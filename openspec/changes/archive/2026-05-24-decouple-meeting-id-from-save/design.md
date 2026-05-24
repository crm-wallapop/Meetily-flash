## Context

The recording lifecycle today threads three pieces of metadata from `start_recording` to `stop_recording`: `meeting_name`, `meeting_folder`, and the transcript segment array. The meeting's *identifier* — the SQLite primary key — is created by a different layer entirely: `TranscriptsRepository::save_transcript` (`frontend/src-tauri/src/database/repositories/transcript.rs:19`) mints `format!("meeting-{}", Uuid::new_v4())` *during* the `INSERT`. As a result, the frontend cannot know the meeting's id until that INSERT commits, which forces the stop hook to:

1. `await storageService.saveMeeting(...)` (a frontend→Rust IPC→SQLite round-trip).
2. Read `meeting_id` from the response.
3. `await enqueueTranscriptionJob(meeting_id, audio_path)`.
4. Navigate to `/meeting-details?id=<meeting_id>`.

Each of these steps blocks the user-visible "saving…" → "saved!" → "navigated" transition on the previous one. When the SQLite write is slow (large transcript array, disk pressure, antivirus scan), the user stares at a frozen UI. Worse, if the save fails partway through, the queue never gets the job and the audio file sits on disk unlinked.

The actual data path is Rust-side SQLite via `api_save_transcript` → `TranscriptsRepository::save_transcript`. The Python `/save-transcript` endpoint at `backend/app/main.py:511` exists and also generates its own id (`meeting-{int(time.time() * 1000)}`), but the desktop app's stop flow doesn't go through it — it's only reachable via Swagger.

The constraint we work under: `RecordingManager` already owns `meeting_name` and `meeting_folder`, generated at `start_recording` time. It also already survives across device-reconnect retries. It is the natural owner of the meeting id. Furthermore, `RecordingSaver` already holds the authoritative transcript segment array in memory — the same data that the frontend currently sends back via IPC to `api_save_transcript`. Moving the SQLite save into `background_shutdown` eliminates this round-trip entirely.

## Goals / Non-Goals

**Goals:**
- `start_recording` returns a `meeting_id` to the frontend as part of its result.
- `stop_recording` returns the same `meeting_id` it gave at start (round-trip invariant).
- The frontend stop hook navigates and enqueues immediately, with zero IPC round-trips for persistence.
- The SQLite save runs in Rust's `background_shutdown` — after audio.mp4 finalization, before the recording gate clears — guaranteeing the row exists when the queue worker runs.
- `save_transcript` accepts a client-supplied `meeting_id` and writes it as the `INSERT INTO meetings (id, ...)` value.
- Duplicate saves with the same `meeting_id` surface a typed error instead of corrupting state.
- Input validation: malformed `meeting_id` strings (empty, wrong shape) are rejected before any DB write.
- `cancel_recording`, recovery, and analytics all operate on the start-time `meeting_id` — one source of truth for the recording's lifetime.

**Non-Goals:**
- Changing the SQLite schema. The `meetings.id` column already accepts arbitrary strings; only the source of the value moves upstream.
- Replacing the `meeting-<UUID>` format with a different identifier scheme. The prefix is load-bearing for log readers and the existing `cancel_recording` GC sweep.
- Moving the Python `/save-transcript` UUID generation logic; we add an optional `meeting_id` parameter for parity but leave the fallback intact so direct API callers don't break.
- Making the save itself faster. We're decoupling the UI from save latency, not eliminating it.
- Eliminating the existing `recording-stopped` event payload. We add `meeting_id` to it for any subscriber that wants it, but the synchronous return value from `stop_recording` is the canonical path.
- Removing `api_save_transcript`. It remains registered for Swagger and future callers, but is no longer on the stop-flow hot path.

## Decisions

### Decision 1: UUID is generated in Rust at `start_recording`, not in the frontend or the backend

Rust already owns the recording lifecycle: the `RecordingManager` constructor names the meeting folder, opens the incremental audio file, and survives across device-change retries. Generating the UUID in Rust keeps a single owner for the recording's identity and metadata.

**Alternatives considered:**
- *Frontend generates UUID* — would split ownership: the folder name and the id would come from different sources, and the frontend would have to inject the id into Rust before recording starts. Adds an extra IPC round-trip and a synchronisation point where today there is none.
- *Backend `pending` row at start* — requires a synchronous round-trip to the Python service before recording begins, which contradicts the local-first principle (recording must work even when the backend is offline) and adds latency to the start-recording UX we're optimising elsewhere.
- *Rust at first `set_meeting_name` call* — `set_meeting_name` can fire later (e.g., from a tray-driven rename) and would race with `stop_recording` returning the id. Generating at `start_recording` is the only call site that guarantees the id exists by the time *any* event fires.

### Decision 2: `meeting_id` lives on `RecordingManager` for the recording's lifetime

The manager is the right home because (a) it is the single owner of recording metadata, (b) its lifetime exactly matches the meeting's recording phase, and (c) `stop_recording` already extracts other fields from it (`get_meeting_folder`, `get_meeting_name`) synchronously before the manager moves into the background shutdown task.

Implementation shape: add a `meeting_id: String` field plus `get_meeting_id(&self) -> &str`. The constructor `RecordingManager::new()` does `format!("meeting-{}", uuid::Uuid::new_v4())` once. The id is immutable after construction — there is no `set_meeting_id` analogue to `set_meeting_name`.

### Decision 3: `StartRecordingResult` and `StopRecordingResult` both carry `meeting_id`

Returning the id from `start_recording` lets the frontend hold it in context for the recording's duration. Returning it again from `stop_recording` is a redundancy on purpose: it's the round-trip pin that lets the stop hook recover from a page reload that lost the in-memory context (the same way `folder_path` and `meeting_name` already do today).

The `recording-started` and `recording-stopped` event payloads also include `meeting_id`. Listeners that don't have access to the command return value (e.g., tray-driven flows, future remote-control plugins) can consume the event.

### Decision 4: SQLite save runs in `background_shutdown`; frontend does not call `api_save_transcript`

The SQLite save is moved into Rust's `background_shutdown` task, which already handles audio.mp4 finalization and recording gate management. The sequence is:

1. `stop_recording` returns `StopRecordingResult { meeting_id, folder_path, meeting_name }` synchronously.
2. Frontend calls `enqueueTranscriptionJob(meeting_id, audio_path)` and `router.push(...)` immediately.
3. Meanwhile, `background_shutdown` runs: (a) `save_recording_only` (audio.mp4 to disk), (b) `TranscriptsRepository::save_transcript` (meeting row + transcript rows to SQLite), (c) emit `recording-saved-to-db { meeting_id }`, (d) `clear_gate_and_resume!()`.
4. By the time the queue worker picks up the job, both the audio file and the SQLite row exist.

This eliminates the frontend→Rust IPC round-trip for the save. The transcript segments are already held in `RecordingSaver::transcript_segments` — the same data the frontend currently sends back via IPC. A `From<recording_saver::TranscriptSegment> for api::TranscriptSegment` conversion bridges the type gap (the `confidence` and `sequence_id` fields are intentionally dropped as they are not persisted in the current schema).

**Why not fire-and-forget from the frontend:**
The earlier design (fire-and-forget `saveMeeting` from the TS side) had a timing gap: `clear_gate_and_resume` fires after `save_recording_only` (audio.mp4 only), but the SQLite row is written by the frontend's async IPC call. There is no ordering guarantee between these two paths, and the gate does not protect the SQLite row. Moving the save into `background_shutdown` makes the gate cover both the audio file and the database row.

**`auto_save = false` path:** when auto_save is disabled, `save_recording_only` returns early (no audio file). The SQLite save still runs — the meeting row and transcript rows are written regardless of whether audio exists. The frontend skips `enqueueTranscriptionJob` when `folder_path` is null (no audio to transcribe).

**Error handling:** if the SQLite save fails, `background_shutdown` logs the error, emits `recording-save-failed`, and still clears the gate (same pattern as audio save failures). The queue worker will find no row and mark the job `Failed`.

### Decision 5: `save_transcript` takes `meeting_id: &str` as a required parameter; the repository no longer generates UUIDs

Signature changes from `save_transcript(pool, title, transcripts, folder_path) -> String` to `save_transcript(pool, meeting_id, title, transcripts, folder_path) -> ()`. The return type drops the id because the caller already knows it. Both `background_shutdown` and `api_save_transcript` call this with the client-supplied id.

The repository validates the id format (`^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`) before the INSERT and returns a typed `InvalidMeetingIdError` for malformed input. SQLite `UNIQUE` violations bubble up as `MeetingAlreadyExistsError` (today they can't happen; with client-supplied ids they become a real failure mode that callers must distinguish from generic DB errors).

### Decision 6: Python `/save-transcript` gets an optional `meeting_id`; fallback preserves today's behaviour

The Pydantic `SaveTranscriptRequest` (line 84 of `backend/app/main.py`) gains `meeting_id: Optional[str] = None`. The endpoint uses the provided id if present; otherwise it falls back to `f"meeting-{int(time.time() * 1000)}"` exactly as today. This keeps Swagger-driven manual saves working unchanged while letting any future direct caller supply a stable id.

We do not migrate the Python fallback to a UUID — that would be a separate behavioural change, and the endpoint is not on the hot path.

### Decision 7: `cancel_recording` already takes `meeting_id`; the frontend must now pass the start-time id

`cancel_recording_impl` (`recording_commands.rs:1060`) already accepts a `meeting_id: String` and uses it to delete the row + folder. Today the frontend passes `""` from `useAutoDetect.ts`. After this change, the frontend reads `activeMeetingId` from context and passes it. `delete_meeting_row_inner` is a no-op when the row hasn't been written yet (the `WHERE id = ?` matches zero rows). A warning log is added on the Rust side when `meeting_id` is empty.

### Decision 8: Crash recovery uses the IndexedDB-persisted `meeting_id`

The post-meeting-transcription change already persists `meeting_id` in IndexedDB as part of the queue snapshot. With this change, IndexedDB is the source of truth across a crash: on restart, recovery enqueues `(stored_meeting_id, audio_path)` directly, and the SQLite row either exists (save committed before crash) or it doesn't (save was mid-flight; the worker's `find_audio_file` on the saved MP4 will fail and the job is marked `Failed`, same as a normal failed save). No new code path required.

### Decision 9: `From<recording_saver::TranscriptSegment> for api::TranscriptSegment` bridges the type gap

Two different `TranscriptSegment` structs exist: `recording_saver::TranscriptSegment` (held in memory during recording) and `api::TranscriptSegment` (used by the repository for DB persistence). The `From` impl maps the common fields: `id`, `text`, `display_time` → `timestamp`, and `audio_start_time`/`audio_end_time`/`duration` (non-optional → `Some`). The `confidence` and `sequence_id` fields are intentionally dropped — they are not persisted in the current `transcripts` table schema and are already discarded by the existing frontend→IPC→repository path.

## Risks / Trade-offs

- **[Risk] SQLite save failure in `background_shutdown` leaves a queued job pointing at a meeting row that never landed.** → Mitigation: the `recording-save-failed` event surfaces to the user; the queue worker handles missing rows by marking jobs `Failed`; manual retry from the meeting-details page works. The failure surface is identical to today — the only difference is the user is already on the meeting-details page instead of the stop screen.
- **[Risk] Two saves with the same client-supplied id (e.g., a stale retry) trigger SQLite `UNIQUE` violations.** → Mitigation: the `MeetingAlreadyExistsError` is treated as idempotent success in `background_shutdown`'s error handler (the row exists; the save was redundant).
- **[Risk] Malformed `meeting_id` from a future caller.** → Mitigation: strict regex validation in `TranscriptsRepository::save_transcript`; rejection is a typed error before any DB interaction.
- **[Trade-off] `start_recording`'s signature changes from `Result<(), String>` to `Result<StartRecordingResult, String>`.** → Internal to the desktop app; IPC surface is not stable. The cost is a one-line update per call site in `recordingService.ts` and `useRecordingStart.ts`.
- **[Trade-off] The sidebar meeting list is stale until `recording-saved-to-db` fires (~2-5 s after stop).** → Acceptable: the user is on the meeting-details page during this window. The sidebar updates asynchronously.
- **[Trade-off] The retranscription processor writes transcript rows keyed by `meeting_id` without verifying the `meetings` row exists.** → Safe today because SQLite FK enforcement is off (`PRAGMA foreign_keys` is not set in the connection setup). With the save now in `background_shutdown` before gate clear, the row will exist by the time the worker runs. The FK constraint in the DDL is a future migration target, not a blocker.

## Migration Plan

This is an internal IPC change with no database migration. Deployment order:

1. Land Rust changes (UUID at start, manager field, `From` impl, repository signature, `background_shutdown` save step, command return types) in one commit, with adversarial tests passing in CI.
2. Land TypeScript changes (typed return, hook simplification, context wiring, event listener) in a second commit.
3. Land Python `SaveTranscriptRequest` field addition in a third commit; backward-compatible (optional field with fallback).

Rollback: revert the three commits in reverse order. No data corruption is possible because the schema is unchanged; any rows written during the brief window where the new code shipped will have ids in the same `meeting-<UUID>` shape as old rows.

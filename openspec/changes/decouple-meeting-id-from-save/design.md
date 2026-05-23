## Context

The recording lifecycle today threads three pieces of metadata from `start_recording` to `stop_recording`: `meeting_name`, `meeting_folder`, and the transcript segment array. The meeting's *identifier* — the SQLite primary key — is created by a different layer entirely: `TranscriptsRepository::save_transcript` (`frontend/src-tauri/src/database/repositories/transcript.rs:19`) mints `format!("meeting-{}", Uuid::new_v4())` *during* the `INSERT`. As a result, the frontend cannot know the meeting's id until that INSERT commits, which forces the stop hook to:

1. `await storageService.saveMeeting(...)` (a Rust→SQLite round-trip).
2. Read `meeting_id` from the response.
3. `await enqueueTranscriptionJob(meeting_id, audio_path)`.
4. Navigate to `/meeting-details?id=<meeting_id>`.

Each of these steps blocks the user-visible "saving…" → "saved!" → "navigated" transition on the previous one. When the SQLite write is slow (large transcript array, disk pressure, antivirus scan), the user stares at a frozen UI. Worse, if the save fails partway through, the queue never gets the job and the audio file sits on disk unlinked.

The actual data path is Rust-side SQLite via `api_save_transcript` → `TranscriptsRepository::save_transcript`. The Python `/save-transcript` endpoint at `backend/app/main.py:511` exists and also generates its own id (`meeting-{int(time.time() * 1000)}`), but the desktop app's stop flow doesn't go through it — it's only reachable via Swagger. This proposal targets the Rust path.

The constraint we work under: `RecordingManager` already owns `meeting_name` and `meeting_folder`, generated at `start_recording` time. It also already survives across device-reconnect retries. It is the natural owner of the meeting id.

## Goals / Non-Goals

**Goals:**
- `start_recording` returns a `meeting_id` to the frontend as part of its result.
- `stop_recording` returns the same `meeting_id` it gave at start (round-trip invariant).
- The stop hook can navigate and enqueue the transcription job *without* awaiting the SQLite save.
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
- Changing the queue's worker semantics. The existing `recording_busy` gate already makes "enqueue before save commits" safe — see Decision 4.

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

### Decision 4: Save is fire-and-forget; enqueue runs before save commits

The transcription queue worker is gated on `recording_busy` (set true in `start_recording`, cleared in `background_shutdown` *after* save completes). This means:

- Frontend calls `enqueueTranscriptionJob(meeting_id, audio_path)` immediately after `stop_recording` returns.
- The job lands in the queue with `status = "pending"` while the SQLite save is still running.
- The worker can't dequeue it because `recording_busy` is still true.
- `background_shutdown` completes the save, then `clear_gate_and_resume!()` flips `recording_busy` to false and resumes the queue.
- By the time the worker picks up the job, the row exists.

No existence check is needed inside `enqueue_transcription_job` (`lib.rs:455-471`) — and there isn't one today, by design (the comment at lib.rs:455-459 says exactly this). The change preserves that property.

The save call is wrapped in a top-level `.catch(toast.error)` so failures surface to the user without blocking navigation. If the save fails, the queue will eventually find no row and mark the job `Failed` — same outcome as today, just with the user already on the meeting-details page.

### Decision 5: `save_transcript` takes `meeting_id: &str` as a required parameter; the repository no longer generates UUIDs

Signature changes from `save_transcript(pool, title, transcripts, folder_path) -> String` to `save_transcript(pool, meeting_id, title, transcripts, folder_path) -> ()`. The return type drops the id because the caller already knows it. `api_save_transcript` is updated symmetrically.

The repository validates the id format (`^meeting-[0-9a-f-]{36}$`) before the INSERT and returns a typed `InvalidMeetingIdError` for malformed input. SQLite `UNIQUE` violations bubble up as `MeetingAlreadyExistsError` (today they can't happen; with client-supplied ids they become a real failure mode that callers must distinguish from generic DB errors).

### Decision 6: Python `/save-transcript` gets an optional `meeting_id`; fallback preserves today's behaviour

The Pydantic `SaveTranscriptRequest` (line 84 of `backend/app/main.py`) gains `meeting_id: Optional[str] = None`. The endpoint uses the provided id if present; otherwise it falls back to `f"meeting-{int(time.time() * 1000)}"` exactly as today. This keeps Swagger-driven manual saves working unchanged while letting any future direct caller supply a stable id.

We do not migrate the Python fallback to a UUID — that would be a separate behavioural change, and the endpoint is not on the hot path.

### Decision 7: `cancel_recording` already takes `meeting_id`; the frontend must now pass the start-time id

`cancel_recording_impl` (`recording_commands.rs:969`) already accepts a `meeting_id: String` and uses it to delete the row + folder. Today the frontend passes whatever id was on the active meeting context (which may or may not have been written yet). After this change, the frontend always passes the start-time `meeting_id` from context, and `delete_meeting_row_inner` becomes a no-op when the row hasn't been written yet (the `WHERE id = ?` matches zero rows — already the case in tests).

### Decision 8: Crash recovery uses the IndexedDB-persisted `meeting_id`

The post-meeting-transcription change already persists `meeting_id` in IndexedDB as part of the queue snapshot. With this change, IndexedDB is the source of truth across a crash: on restart, recovery enqueues `(stored_meeting_id, audio_path)` directly, and the SQLite row either exists (save committed before crash) or it doesn't (save was mid-flight; the worker's `find_audio_file` on the saved MP4 will fail and the job is marked `Failed`, same as a normal failed save). No new code path required.

## Risks / Trade-offs

- **[Risk] Save fails after enqueue + navigation, leaving a queued job pointing at a meeting row that never landed.** → Mitigation: queue worker already handles missing rows by marking jobs `Failed`; toast surfaces save errors to the user; manual retry via re-enqueue from the meeting-details page is unaffected. Net outcome is the same failure surface as today, but the user sees the failure on the meeting-details page (where retry lives) instead of on the dead-end stop screen.
- **[Risk] Two saves with the same client-supplied id (e.g., a retry after a transient error) trigger SQLite `UNIQUE` violations and surface as user-facing errors.** → Mitigation: the new `MeetingAlreadyExistsError` is treated as idempotent success (the row exists; the save was redundant). The frontend's toast logic suppresses this specific error and treats the save as completed.
- **[Risk] Malformed `meeting_id` from a future caller (e.g., a third-party plugin) corrupts logs or routing.** → Mitigation: strict regex validation at the `api_save_transcript` boundary (`^meeting-[0-9a-f-]{36}$`); rejection is a 400-shaped error before any DB or queue interaction. The same regex guards the Python endpoint.
- **[Trade-off] `start_recording`'s signature changes from `Result<(), String>` to `Result<StartRecordingResult, String>`.** → This is a breaking change for any external code calling the command directly. The change is internal to the desktop app and the IPC surface is not stable, so the cost is one-line update to `recordingService.ts` and the tests that mock `start_recording`. We accept it as the smallest viable change.
- **[Trade-off] `recording-started` event payload grows by one field.** → Existing listeners that ignore unknown fields (the standard `serde_json` payload pattern) are unaffected. Listeners that assert on exact payload shape (none today, per audit) would need a one-line update.
- **[Risk] A user clicks "View Meeting" from the success toast before the SQLite row is committed.** → Mitigation: `/meeting-details?id=<meeting_id>` first reads from `api_get_meeting`; we add a small retry (3 attempts × 200 ms, opportunistic) in the meeting-details data hook for the freshly-stopped case. The retry is bounded so a permanently failed save still surfaces an error within ~1 s.

## Migration Plan

This is an internal IPC change with no database migration. Deployment order:

1. Land Rust changes (UUID at start, manager field, repository signature, command return types) in one commit, with adversarial tests passing in CI.
2. Land TypeScript changes (typed return, hook update, context wiring) in a second commit. The TS code is forward-compatible until the Rust change lands because `serde_json` payloads on Tauri commands accept extra fields without error.
3. Land Python `SaveTranscriptRequest` field addition in a third commit; backward-compatible (optional field with fallback).

Rollback: revert the three commits in reverse order. No data corruption is possible because the schema is unchanged; any rows written during the brief window where the new code shipped will have ids in the same `meeting-<UUID>` shape as old rows.

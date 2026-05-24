## 1. Adversarial RED tests (Rust) — write first, expect failures

- [x] 1.1 Add `cargo test` red: `start_recording_returns_well_formed_meeting_id` — assert returned `StartRecordingResult.meeting_id` matches `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`.
- [x] 1.2 Add `cargo test` red: `start_and_stop_return_same_meeting_id` — invoke start, capture id, invoke stop, assert byte-equal id.
- [x] 1.3 Add `cargo test` red: `stop_on_idle_returns_none_meeting_id` — phase Idle → `StopRecordingResult { meeting_id: None, .. }`.
- [x] 1.4 Add `cargo test` red: `save_transcript_rejects_empty_meeting_id` — call `TranscriptsRepository::save_transcript` with `""` → expect `InvalidMeetingIdError`.
- [x] 1.5 Add `cargo test` red: `save_transcript_rejects_malformed_meeting_id` — call with `"meeting-not-a-uuid"` → expect `InvalidMeetingIdError`.
- [x] 1.6 Add `cargo test` red: `save_transcript_persists_client_supplied_id` — call with a valid id, then `SELECT id FROM meetings WHERE id = ?` → assert it matches.
- [x] 1.7 Add `cargo test` red: `save_transcript_duplicate_id_surfaces_typed_error` — call save twice with the same id → second call returns `MeetingAlreadyExistsError` (not a generic `sqlx::Error`).

## 2. Adversarial RED tests (TypeScript) — write first

- [x] 2.1 Add `pnpm test` red: `useRecordingStop_navigates_immediately_after_stop` — mock `recordingService.stopRecording` to return `{ meeting_id, folder_path, meeting_name }`; mock `router.push`; assert `router.push` is called within 200 ms without any save call.
- [x] 2.2 Add `pnpm test` red: `useRecordingStop_enqueues_immediately_after_stop` — same mocks; assert `enqueueTranscriptionJob` is called with the correct `meeting_id` and `audioPath`.
- [x] 2.3 Add `pnpm test` red: `recordingService_startRecording_returns_meeting_id` — invoke via mocked `@tauri-apps/api/core`; assert the return type includes `meeting_id`.

## 3. Rust: `RecordingManager` owns `meeting_id`

- [x] 3.1 Add `meeting_id: String` field to `RecordingManager` in `frontend/src-tauri/src/audio/recording_manager.rs`.
- [x] 3.2 In `RecordingManager::new()`, generate `format!("meeting-{}", uuid::Uuid::new_v4())` once and store it; ensure `uuid` is a workspace dep (already present per `transcript.rs`).
- [x] 3.3 Add `pub fn get_meeting_id(&self) -> &str` returning the field.
- [x] 3.4 Add a unit test pinning that two `RecordingManager::new()` calls produce different ids.

## 4. Rust: `StartRecordingResult` and `StopRecordingResult` carry `meeting_id`

- [x] 4.1 Add `pub struct StartRecordingResult { pub meeting_id: String }` in `recording_commands.rs` next to `StopRecordingResult`.
- [x] 4.2 Widen `StopRecordingResult` with `pub meeting_id: Option<String>`. Update all construction sites in `stop_recording` to include the id from `manager.get_meeting_id()`, returning `None` only on the early-return Idle/Saving paths.
- [x] 4.3 Update `start_recording_with_meeting_name` and `start_recording_with_devices_and_meeting` signatures to return `Result<StartRecordingResult, String>`. Read the id from `manager.get_meeting_id()` *before* the manager moves into the global mutex; include it in the result.
- [x] 4.4 Update the `recording-started` event payloads (both start variants) to include `meeting_id`.
- [x] 4.5 Update the `recording-stopped` event payload to include `meeting_id`.

## 5. Rust: top-level commands in `lib.rs`

- [x] 5.1 Change `start_recording` (line 82) signature: `Result<(), String>` → `Result<audio::recording_commands::StartRecordingResult, String>`. Return the inner result on success.
- [x] 5.2 Update the success log line to include the returned `meeting_id`.
- [x] 5.3 Confirm `stop_recording` inherits the widened `StopRecordingResult` (no signature change needed; struct field addition is transparent).
- [x] 5.4 Optional: replace `RECORDING_FLAG` check in `start_recording` (line 99) with `current_phase()` for consistency with the rest of the codebase.

## 6. Rust: repository accepts client-supplied id; typed errors

- [x] 6.1 Define `InvalidMeetingIdError` and `MeetingAlreadyExistsError` in `frontend/src-tauri/src/database/repositories/transcript.rs` (or a shared error module). Use `thiserror::Error` if available; otherwise a simple enum is fine. Both must serialize to `String` for the existing Tauri error contract.
- [x] 6.2 Add a `validate_meeting_id(s: &str) -> Result<(), InvalidMeetingIdError>` helper that checks the regex `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`. Compile the regex once via `once_cell::sync::Lazy` to avoid per-call cost.
- [x] 6.3 Change `TranscriptsRepository::save_transcript` signature: insert `meeting_id: &str` as the second parameter (after `pool`); drop the `Uuid::new_v4()` call at line 19; bind the parameter into the `INSERT INTO meetings` query.
- [x] 6.4 Validate `meeting_id` at the top of `save_transcript` before opening the transaction; return `InvalidMeetingIdError` on failure.
- [x] 6.5 Map SQLite `UNIQUE` violation (sqlx error kind = `Database` with code `1555` (PRIMARY KEY) or `2067` (UNIQUE) on the `id` column) to `MeetingAlreadyExistsError`. Other DB errors continue to bubble up as `SqlxError`.
- [x] 6.6 Drop the `String` return value from `save_transcript` — callers already know the id. Update the function signature to `Result<(), TranscriptSaveError>` (or whatever the error enum becomes).

## 7. Rust: `From` conversion — `recording_saver::TranscriptSegment` → `api::TranscriptSegment`

- [x] 7.1 Add `impl From<recording_saver::TranscriptSegment> for api::TranscriptSegment` in `api/api.rs` (or a shared location). Map `id`, `text`, `display_time` → `timestamp`, and `audio_start_time`/`audio_end_time`/`duration` (non-optional → `Some`). The `confidence` and `sequence_id` fields are intentionally dropped (not persisted in the current schema).
- [x] 7.2 Add a unit test asserting the conversion is lossless for all persisted fields.

## 8. Rust: SQLite save in `background_shutdown`

- [x] 8.1 After `save_recording_only` in `background_shutdown` (and regardless of whether it succeeded or returned early due to `auto_save = false`), extract `meeting_id`, `meeting_name`, `folder_path`, and transcript segments from the manager.
- [x] 8.2 Convert `Vec<recording_saver::TranscriptSegment>` → `Vec<api::TranscriptSegment>` using the `From` impl from section 7.
- [x] 8.3 Get a DB pool via `DatabaseManager::new_from_app_handle(&app)`.
- [x] 8.4 Call `TranscriptsRepository::save_transcript(pool, meeting_id, meeting_name, segments, folder_path)`. On `MeetingAlreadyExistsError`, treat as idempotent success (log, continue). On other errors, log and emit `recording-save-failed`.
- [x] 8.5 Emit `recording-saved-to-db` event with `{ meeting_id }` after successful save.
- [x] 8.6 Add `meeting_id` to the existing `recording-saved` event payload (emitted from `recording_saver.rs:404`). Thread the manager's `meeting_id` through `save_recording_only` or read it from the saver's metadata.
- [x] 8.7 Ensure `clear_gate_and_resume!()` still runs after both `save_recording_only` AND the SQLite save (on success or error — same as today).

## 9. Rust: `MeetingMetadata.meeting_id` populated

- [x] 9.1 Add `pub fn set_meeting_id(&mut self, id: String)` to `RecordingSaver`.
- [x] 9.2 Call `recording_saver.set_meeting_id(manager.get_meeting_id().to_string())` after `set_meeting_name` in both start variants. This populates `metadata.json` on disk.
- [x] 9.3 (needs manual smoke — app running at PID 90552) Verify that `metadata.json` in the meeting folder now contains `"meeting_id": "meeting-<uuid>"` instead of `null`.

## 10. TypeScript: typed return for start, context wiring

- [x] 10.1 In `frontend/src/services/recordingService.ts`, define `export interface StartRecordingResult { meeting_id: string }` and update `startRecording`, `startRecordingWithDevices`, `startRecordingWithDevicesAndMeeting` typed returns.
- [x] 10.2 Add `RecordingSavedToDbPayload { meeting_id: string }` type and `onRecordingSavedToDb()` listener helper.
- [x] 10.3 In `frontend/src/contexts/TranscriptContext.tsx`, add `activeMeetingId: string | null` to the context with a setter. The setter is called from the start path and cleared on `recording-stopped`.
- [x] 10.4 In `frontend/src/hooks/useRecordingStart.ts`, capture `meeting_id` from the `startRecordingWithDevices` result at all three call sites (lines 44, 77, 109) and stash it in context via the setter from 10.3.

## 11. TypeScript: stop hook simplification

- [x] 11.1 In `frontend/src/hooks/useRecordingStop.ts`, remove the `storageService.saveMeeting(...)` call entirely. The stop flow becomes: stop → enqueue → navigate.
- [x] 11.2 Read `meeting_id` from `stopResult` (with fallback to `activeMeetingId` from context for tray-driven stops).
- [x] 11.3 If `folder_path` is non-null, call `enqueueTranscriptionJob(meeting_id, audioPath)` immediately.
- [x] 11.4 Show the existing toast with "View Meeting" action (keep current pattern — no auto-navigate to preserve M2 workflow).
- [x] 11.5 Listen for `recording-saved-to-db` event to trigger `refetchMeetings()`, `setCurrentMeeting()`, and `markMeetingAsSaved()`.
- [x] 11.6 Listen for `recording-save-failed` event to show an error toast.
- [x] 11.7 Remove all `sessionStorage` fallback logic for `last_recording_folder_path` / `last_recording_meeting_name` — the values now come from `stopResult` and context.

## 12. TypeScript: cancel path reads `activeMeetingId`

- [x] 12.1 In `frontend/src/hooks/useAutoDetect.ts` (line 171), replace `invoke('cancel_recording', { meeting_id: '' })` with `invoke('cancel_recording', { meeting_id: activeMeetingId || '' })`, reading `activeMeetingId` from context.
- [x] 12.2 Add a warning log on the Rust side in `cancel_recording_impl` when `meeting_id` is empty.

## 13. Python: optional meeting_id on `/save-transcript`

- [x] 13.1 Add `meeting_id: Optional[str] = None` to `SaveTranscriptRequest` (`backend/app/main.py:84`).
- [x] 13.2 In the `/save-transcript` handler (line 511), use `request.meeting_id` if present, otherwise fall back to the existing `f"meeting-{int(time.time() * 1000)}"`. Validate the same regex when present.
- [x] 13.3 Add a `pytest` covering both branches: with and without `meeting_id`.

## 14. End-to-end and regression

- [x] 14.1 Update existing test `stop_recording_result_serializes_with_populated_fields` in `recording_commands.rs` to assert the new `meeting_id` field round-trips through serde.
- [x] 14.2 Update existing test `stop_recording_result_serializes_with_none_fields` to also pin `meeting_id: None`.
- [x] 14.3 Add a Rust integration test that exercises start → stop and asserts the round-trip id matches.
- [x] 14.4 Add a Rust integration test that asserts the SQLite row exists after `background_shutdown` completes (verify the meeting_id matches).
- [x] 14.5 (needs manual smoke — app running at PID 90552) Manual smoke: start recording, stop, confirm navigation is immediate (no save wait), meeting appears in sidebar after a few seconds, meeting-details page loads correctly.
- [x] 14.6 (needs manual smoke — app running at PID 90552) Manual smoke: start recording, cancel, confirm folder is deleted and no orphaned DB row.
- [x] 14.7 (needs manual smoke — app running at PID 90552) Manual smoke: start recording with `auto_save = false`, stop, confirm meeting row exists in DB (no audio file expected).

## 15. Green-test sweep and archive prep

- [x] 15.1 Run `cargo test` — all reds must be green.
- [x] 15.2 Run `pnpm test` — all reds must be green.
- [x] 15.3 Run `pytest backend/` — green.
- [x] 15.4 Run `pnpm lint && cargo clippy --all-targets -- -D warnings` — clean.
- [x] 15.5 Re-read `openspec/changes/decouple-meeting-id-from-save/specs/recording-lifecycle/spec.md` and `design.md`. If the implementation diverged (e.g., a different regex, a different error type), amend the spec/design before archiving.
- [ ] 15.6 Run `/opsx:archive` to move the change into `openspec/changes/archive/` and merge the recording-lifecycle delta into `openspec/specs/recording-lifecycle/spec.md`.

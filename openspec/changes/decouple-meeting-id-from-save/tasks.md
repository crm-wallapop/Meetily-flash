## 1. Adversarial RED tests (Rust) — write first, expect failures

- [ ] 1.1 Add `cargo test` red: `start_recording_returns_well_formed_meeting_id` — assert returned `StartRecordingResult.meeting_id` matches `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`.
- [ ] 1.2 Add `cargo test` red: `start_and_stop_return_same_meeting_id` — invoke start, capture id, invoke stop, assert byte-equal id. Use the existing `stop_recording_sync_path_for_test` shape; extend it to expose the manager's id.
- [ ] 1.3 Add `cargo test` red: `stop_on_idle_returns_none_meeting_id` — phase Idle → `StopRecordingResult { meeting_id: None, .. }`.
- [ ] 1.4 Add `cargo test` red: `save_transcript_rejects_empty_meeting_id` — call `TranscriptsRepository::save_transcript` with `""` → expect `InvalidMeetingIdError`.
- [ ] 1.5 Add `cargo test` red: `save_transcript_rejects_malformed_meeting_id` — call with `"meeting-not-a-uuid"` → expect `InvalidMeetingIdError`.
- [ ] 1.6 Add `cargo test` red: `save_transcript_persists_client_supplied_id` — call with a valid id, then `SELECT id FROM meetings WHERE id = ?` → assert it matches.
- [ ] 1.7 Add `cargo test` red: `save_transcript_duplicate_id_surfaces_typed_error` — call save twice with the same id → second call returns `MeetingAlreadyExistsError` (not a generic `sqlx::Error`).

## 2. Adversarial RED tests (TypeScript) — write first

- [ ] 2.1 Add `pnpm test` red: `useRecordingStop_navigates_before_save_resolves` — mock `storageService.saveMeeting` to take 5 s; mock `router.push`; assert `router.push` is called within 200 ms of `recordingService.stopRecording` resolving.
- [ ] 2.2 Add `pnpm test` red: `useRecordingStop_enqueues_before_save_resolves` — same mocks; assert `enqueueTranscriptionJob` is called before `saveMeeting` resolves.
- [ ] 2.3 Add `pnpm test` red: `useRecordingStop_save_failure_does_not_block_navigation` — make `saveMeeting` reject; assert `router.push` still fires and an error toast appears.
- [ ] 2.4 Add `pnpm test` red: `recordingService_startRecording_returns_meeting_id` — invoke via mocked `@tauri-apps/api/core`; assert the return type widens to include `meeting_id`.

## 3. Rust: `RecordingManager` owns `meeting_id`

- [ ] 3.1 Add `meeting_id: String` field to `RecordingManager` in `frontend/src-tauri/src/audio/recording_manager.rs`.
- [ ] 3.2 In `RecordingManager::new()`, generate `format!("meeting-{}", uuid::Uuid::new_v4())` once and store it; ensure `uuid` is a workspace dep (already present per `transcript.rs`).
- [ ] 3.3 Add `pub fn get_meeting_id(&self) -> &str` returning the field.
- [ ] 3.4 Add a unit test pinning that two `RecordingManager::new()` calls produce different ids.

## 4. Rust: `StartRecordingResult` and `StopRecordingResult` carry `meeting_id`

- [ ] 4.1 Add `pub struct StartRecordingResult { pub meeting_id: String }` in `recording_commands.rs` next to `StopRecordingResult`.
- [ ] 4.2 Widen `StopRecordingResult` with `pub meeting_id: Option<String>`. Update all construction sites in `stop_recording` to include the id from `manager.get_meeting_id()`, returning `None` only on the early-return Idle/Saving paths.
- [ ] 4.3 Update `start_recording_with_meeting_name` (line 130) and `start_recording_with_devices_and_meeting` (line 332) signatures to return `Result<StartRecordingResult, String>`. Read the id from `manager.get_meeting_id()` *before* the manager moves into the global mutex; include it in the result.
- [ ] 4.4 Update the `recording-started` event payload at lines 309-312 and 438-444 to include `meeting_id`.
- [ ] 4.5 Update the `recording-stopped` event payload at lines 541-548 to include `meeting_id`.

## 5. Rust: top-level commands in `lib.rs`

- [ ] 5.1 Change `start_recording` (line 82) signature: `Result<(), String>` → `Result<audio::recording_commands::StartRecordingResult, String>`. Return the inner result on success.
- [ ] 5.2 Update the success log line at line 114 to include the returned `meeting_id`.
- [ ] 5.3 Confirm `stop_recording` (line 144) inherits the widened `StopRecordingResult` (no signature change needed; struct field addition is transparent).
- [ ] 5.4 No change to command registration list (lines 916-917) needed — the function names are unchanged.

## 6. Rust: repository accepts client-supplied id; typed errors

- [ ] 6.1 Define `InvalidMeetingIdError` and `MeetingAlreadyExistsError` in `frontend/src-tauri/src/database/repositories/transcript.rs` (or a shared error module). Use `thiserror::Error` if available; otherwise a simple enum is fine. Both must serialize to `String` for the existing Tauri error contract.
- [ ] 6.2 Add a `validate_meeting_id(s: &str) -> Result<(), InvalidMeetingIdError>` helper that checks the regex `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`. Compile the regex once via `once_cell::sync::Lazy` to avoid per-call cost.
- [ ] 6.3 Change `TranscriptsRepository::save_transcript` signature: insert `meeting_id: &str` as the second parameter (after `pool`); drop the `Uuid::new_v4()` call at line 19; bind the parameter into the `INSERT INTO meetings` query at line 30.
- [ ] 6.4 Validate `meeting_id` at the top of `save_transcript` before opening the transaction; return `InvalidMeetingIdError` on failure.
- [ ] 6.5 Map SQLite `UNIQUE` violation (sqlx error kind = `Database` with code `2067` on the `id` column) to `MeetingAlreadyExistsError`. Other DB errors continue to bubble up as `SqlxError`.
- [ ] 6.6 Drop the `String` return value from `save_transcript` — callers already know the id. Update the function signature to `Result<(), TranscriptSaveError>` (or whatever the error enum becomes).

## 7. Rust: `api_save_transcript` accepts and forwards meeting_id

- [ ] 7.1 Add `meeting_id: String` parameter to `api_save_transcript` (line 930 of `api/api.rs`), placed first in the param list for clarity. Update the log line at line 939 to include it.
- [ ] 7.2 Pass `meeting_id` through to `TranscriptsRepository::save_transcript`.
- [ ] 7.3 Map `InvalidMeetingIdError` and `MeetingAlreadyExistsError` to user-facing `String` error messages distinguishable by prefix (e.g., `"invalid meeting_id: …"`, `"meeting already exists: …"`). The frontend will pattern-match on these.
- [ ] 7.4 Update the success response at lines 989-993 to echo back the supplied `meeting_id` for back-compat.

## 8. TypeScript: typed return for start, hook update

- [ ] 8.1 In `frontend/src/services/recordingService.ts`, define `export interface StartRecordingResult { meeting_id: string }` and update `startRecording`, `startRecordingWithDevices`, `startRecordingWithDevicesAndMeeting` typed returns.
- [ ] 8.2 Update `frontend/src/services/storageService.ts` `saveMeeting` signature to require `meetingId: string` as the first argument. Forward it via the `meeting_id` field in the `invoke<SaveMeetingResponse>('api_save_transcript', { meetingId, meetingTitle, transcripts, folderPath })` call.
- [ ] 8.3 In `frontend/src/contexts/TranscriptContext.tsx` (or `RecordingStateContext` if that fits better), add `activeMeetingId: string | null` to the context with a setter. The setter is called from the start path and cleared on `recording-stopped`.
- [ ] 8.4 In `frontend/src/app/page.tsx`, capture the `meeting_id` returned from `start_recording` and stash it in context via the setter from 8.3.
- [ ] 8.5 In `frontend/src/hooks/useRecordingStop.ts`, read `meeting_id` from `stopResult` at line 124. Pass it into `storageService.saveMeeting(meetingId, ...)`. Use it directly for `enqueueTranscriptionJob` and `router.push` without waiting for `saveMeeting` — wrap the save in `.catch((err) => toast.error('Failed to save meeting', { description: errorMessage(err) }))`.
- [ ] 8.6 Move the navigation `setTimeout` (line 256) and the `enqueueTranscriptionJob` block (line 198) above the `await storageService.saveMeeting`. The save still fires; we just don't await it.
- [ ] 8.7 Handle `MeetingAlreadyExistsError` toast suppression: if the save rejection's message contains `"meeting already exists"`, treat it as idempotent success (no error toast).

## 9. TypeScript: meeting-details opportunistic retry

- [ ] 9.1 In the meeting-details data hook (`frontend/src/hooks/meeting-details/useMeetingData.ts`), wrap the initial `api_get_meeting` call in a small retry: 3 attempts × 200 ms when the response is `null`, and a `?source=recording` query param is present. After 3 attempts, fall through to the existing "not found" UX.
- [ ] 9.2 Add a `pnpm test` for the retry: mock `invoke` to return `null` then `null` then the meeting; assert the hook eventually returns the meeting after ~400 ms.

## 10. Python: optional meeting_id on `/save-transcript`

- [ ] 10.1 Add `meeting_id: Optional[str] = None` to `SaveTranscriptRequest` (`backend/app/main.py:84`).
- [ ] 10.2 In the `/save-transcript` handler (line 511), use `request.meeting_id` if present, otherwise fall back to the existing `f"meeting-{int(time.time() * 1000)}"`. Validate the same regex when present.
- [ ] 10.3 Add a `pytest` covering both branches: with and without `meeting_id`.

## 11. End-to-end and regression

- [ ] 11.1 Update existing test `stop_recording_result_serializes_with_populated_fields` in `recording_commands.rs` to assert the new `meeting_id` field round-trips through serde.
- [ ] 11.2 Update existing test `stop_recording_result_serializes_with_none_fields` to also pin `meeting_id: None`.
- [ ] 11.3 Add a Rust integration test that exercises start → stop and asserts the round-trip id matches.
- [ ] 11.4 Manual smoke: start recording, stop, observe navigation to `/meeting-details` within 200 ms even with a 5000-segment transcript. Document the observed delay in the change's archive notes.
- [ ] 11.5 Manual smoke: cancel mid-recording, confirm folder and (if-present) row are removed.

## 12. Green-test sweep and archive prep

- [ ] 12.1 Run `cargo test` — all reds must be green.
- [ ] 12.2 Run `pnpm test` — all reds must be green.
- [ ] 12.3 Run `pytest backend/` — green.
- [ ] 12.4 Run `pnpm lint && cargo clippy --all-targets -- -D warnings` — clean.
- [ ] 12.5 Re-read `openspec/changes/decouple-meeting-id-from-save/specs/recording-lifecycle/spec.md` and `design.md`. If the implementation diverged (e.g., a different regex, a different error type), amend the spec/design before archiving.
- [ ] 12.6 Run `/opsx:archive` to move the change into `openspec/changes/archive/` and merge the recording-lifecycle delta into `openspec/specs/recording-lifecycle/spec.md`.

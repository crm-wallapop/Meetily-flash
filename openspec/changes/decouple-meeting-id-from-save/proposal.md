## Why

The recording stop UI hangs whenever the SQLite save is slow because the meeting UUID — needed for navigation to `/meeting-details` and for `enqueue_transcription_job` — is generated *inside* `TranscriptsRepository::save_transcript` and only returned after the row commits. Today's flow forces the user to wait on a synchronous write before anything post-recording can happen, even though the save is purely a persistence concern that has no reason to block the UI.

Moving UUID generation to `start_recording` makes the meeting ID known throughout the recording's lifetime, lets the stop flow navigate and enqueue immediately, and turns the save into a fire-and-forget background write. This also gives `cancel_recording` and crash-recovery a stable identifier from the moment recording starts, eliminating a category of "meeting saved with mismatched id" bugs.

## What Changes

- **BREAKING (internal IPC):** `start_recording`, `start_recording_with_devices`, and `start_recording_with_devices_and_meeting` SHALL return a `StartRecordingResult { meeting_id: String }` instead of `()`. The `recording-started` event payload SHALL also include `meeting_id`.
- **BREAKING (internal IPC):** `StopRecordingResult` SHALL gain a `meeting_id: String` field (alongside the existing `folder_path` and `meeting_name`). The `recording-stopped` event payload SHALL include `meeting_id`.
- **BREAKING (internal IPC):** `api_save_transcript` SHALL accept a required `meeting_id: String` parameter; `TranscriptsRepository::save_transcript` SHALL accept and use that ID for the `INSERT INTO meetings` row instead of generating its own UUID.
- `RecordingManager` SHALL hold the meeting UUID alongside `meeting_name` and `meeting_folder`, generated at construction time so it survives device-change retries.
- The frontend stop flow (`useRecordingStop`) SHALL read `meeting_id` from `stopResult`, then invoke `enqueueTranscriptionJob(meetingId, audioPath)` and `router.push('/meeting-details?id=' + meetingId)` *without awaiting* `storageService.saveMeeting(...)`. The save runs in the background; failures surface via toast but do not block navigation.
- The Python `/save-transcript` endpoint (`backend/app/main.py`) SHALL accept an optional `meeting_id` field on `SaveTranscriptRequest` for parity. Not on the hot path; included so Swagger-driven manual saves don't drift from the Rust contract.
- Input validation: `api_save_transcript` and the Python endpoint SHALL reject `meeting_id` values that do not match `^meeting-[0-9a-f-]{36}$` (i.e., the existing `meeting-<UUID v4>` shape). Empty or malformed IDs return a 400-style error before any DB write.
- Duplicate-ID protection: `INSERT INTO meetings (id, ...)` SHALL surface the SQLite `UNIQUE` violation as a typed error if the same `meeting_id` is saved twice (e.g., a renegade retry). Today the repository can't hit this because every call mints a fresh UUID; with client-supplied IDs it becomes a real failure mode.

## Capabilities

### New Capabilities
<!-- None. -->

### Modified Capabilities
- `recording-lifecycle`: the recording lifecycle now owns the meeting UUID. `start_recording` becomes the canonical source of `meeting_id`; `stop_recording` returns it; `cancel_recording` consumes it. The lifecycle spec must describe this new invariant and the (no-await) save semantics it enables.

<!-- The in-flight `post-meeting-pipeline` capability (introduced by the unarchived
     `post-meeting-transcription` change) consumes `meeting_id` for enqueue and
     save ordering, but its requirements remain unchanged by this proposal: the
     queue still receives `(meeting_id, audio_path)` exactly as it does today; only
     the source of `meeting_id` shifts upstream. No delta needed for that capability. -->

## Impact

**Rust (`frontend/src-tauri/`)**
- `audio/recording_commands.rs`: generate UUID at the top of `start_recording_with_meeting_name` (line 130) and `start_recording_with_devices_and_meeting` (line 332); thread it onto `RecordingManager`; widen `StopRecordingResult` (line 97) with `meeting_id`; include it in the `recording-stopped` and `recording-started` event payloads (lines 309-312, 541-548).
- `lib.rs`: top-level `start_recording` command (line 82) signature changes from `Result<(), String>` to `Result<StartRecordingResult, String>`; command registration list (lines 916-917) unchanged except for re-export.
- `audio/recording_manager.rs`: add `meeting_id: String` field; constructor generates `format!("meeting-{}", uuid::Uuid::new_v4())`; expose via `get_meeting_id()`.
- `database/repositories/transcript.rs`: `save_transcript` takes `meeting_id: &str` as the first arg; remove the internal `Uuid::new_v4()` call at line 19; surface `UNIQUE` violations as a typed `MeetingAlreadyExistsError`.
- `api/api.rs`: `api_save_transcript` (line 930) accepts `meeting_id: String`; validates the format; forwards to the repository.

**TypeScript (`frontend/src/`)**
- `services/recordingService.ts`: typed return of `startRecording*` becomes `{ meeting_id: string }`.
- `services/storageService.ts`: `saveMeeting` signature gains `meetingId: string`; the response's `meeting_id` is now redundant but preserved for back-compat.
- `services/queueService.ts`: no change; `enqueueTranscriptionJob` already takes `meetingId`.
- `hooks/useRecordingStop.ts`: consume `meeting_id` from `stopResult` (line 124); call `enqueueTranscriptionJob` and `router.push` immediately; fire `storageService.saveMeeting(...)` without awaiting; toast on failure.
- `contexts/TranscriptContext.tsx` and/or `RecordingStateContext`: hold the active `meetingId` for the recording's duration (used by `cancel_recording` and recovery).
- `app/page.tsx`: stash the `meeting_id` returned from `start_recording` into context.

**Python (`backend/`)**
- `app/main.py`: `SaveTranscriptRequest` (line 84) gains optional `meeting_id: Optional[str]`; `/save-transcript` (line 511) uses it if present, otherwise falls back to the existing `meeting-{int(time.time() * 1000)}` shape for unchanged behaviour on direct API callers.
- `app/db.py`: no change; `save_meeting(meeting_id, ...)` already takes the ID as a parameter.

**Tests**
- New adversarial tests for the categories called out in §4 of `CLAUDE.md`: duplicate save, malformed UUID rejected, start↔stop UUID round-trip, enqueue-before-save ordering, and a frontend regression pin that navigation does not await save.

**Migrations / data**
- No DB schema migration. Existing rows keep their IDs; only the upstream source of new IDs moves.

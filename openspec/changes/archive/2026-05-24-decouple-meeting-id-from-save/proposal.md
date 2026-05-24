## Why

The recording stop UI hangs whenever the SQLite save is slow because the meeting UUID — needed for navigation to `/meeting-details` and for `enqueue_transcription_job` — is generated *inside* `TranscriptsRepository::save_transcript` and only returned after the row commits. Today's flow forces the user to wait on a synchronous IPC round-trip (frontend → `api_save_transcript` → SQLite → response) before anything post-recording can happen, even though the save is purely a persistence concern that has no reason to block the UI.

Moving UUID generation to `start_recording` makes the meeting ID known throughout the recording's lifetime, lets the stop flow navigate and enqueue immediately, and moves the SQLite save into Rust's `background_shutdown` task where it runs before the recording gate clears — guaranteeing the row exists when the queue worker picks up the transcription job. This also gives `cancel_recording` and crash-recovery a stable identifier from the moment recording starts, eliminating a category of "meeting saved with mismatched id" bugs.

## What Changes

- **BREAKING (internal IPC):** `start_recording`, `start_recording_with_devices`, and `start_recording_with_devices_and_meeting` SHALL return a `StartRecordingResult { meeting_id: String }` instead of `()`. The `recording-started` event payload SHALL also include `meeting_id`.
- **BREAKING (internal IPC):** `StopRecordingResult` SHALL gain a `meeting_id: String` field (alongside the existing `folder_path` and `meeting_name`). The `recording-stopped` event payload SHALL include `meeting_id`.
- `RecordingManager` SHALL hold the meeting UUID alongside `meeting_name` and `meeting_folder`, generated at construction time so it survives device-change retries.
- The SQLite save (`TranscriptsRepository::save_transcript`) SHALL be called from `background_shutdown` in Rust — after audio.mp4 finalization, before the recording gate clears. This eliminates the frontend→Rust IPC round-trip for the save and guarantees the row exists before the queue worker runs.
- `TranscriptsRepository::save_transcript` SHALL accept a client-supplied `meeting_id: &str` instead of generating its own UUID. A `From<recording_saver::TranscriptSegment> for api::TranscriptSegment` conversion SHALL bridge the saver's segment type to the repository's type.
- The frontend stop flow (`useRecordingStop`) SHALL read `meeting_id` from `stopResult`, then invoke `enqueueTranscriptionJob(meetingId, audioPath)` and `router.push('/meeting-details?id=' + meetingId)` without calling `storageService.saveMeeting()` at all. The Rust `background_shutdown` handles persistence.
- A new `recording-saved-to-db` event SHALL fire from `background_shutdown` after the SQLite save commits, carrying `meeting_id`. The frontend SHALL use this event to trigger `refetchMeetings()` and `markMeetingAsSaved()`.
- `api_save_transcript` SHALL remain unchanged for backward compatibility (Swagger, future callers). It is no longer on the stop-flow hot path.
- The Python `/save-transcript` endpoint (`backend/app/main.py`) SHALL accept an optional `meeting_id` field on `SaveTranscriptRequest` for parity. Not on the hot path; included so Swagger-driven manual saves don't drift from the Rust contract.
- Input validation: `TranscriptsRepository::save_transcript` SHALL reject `meeting_id` values that do not match `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`. Empty or malformed IDs return a typed error before any DB write.
- Duplicate-ID protection: `INSERT INTO meetings (id, ...)` SHALL surface the SQLite `UNIQUE` violation as a typed `MeetingAlreadyExistsError` so the `background_shutdown` error handler can treat it as idempotent.

## Capabilities

### New Capabilities
<!-- None. -->

### Modified Capabilities
- `recording-lifecycle`: the recording lifecycle now owns the meeting UUID. `start_recording` becomes the canonical source of `meeting_id`; `stop_recording` returns it; `cancel_recording` consumes it. The SQLite save moves into Rust's `background_shutdown`, removing the frontend IPC round-trip.

<!-- The in-flight `post-meeting-pipeline` capability (introduced by the unarchived
     `post-meeting-transcription` change) consumes `meeting_id` for enqueue and
     save ordering, but its requirements remain unchanged by this proposal: the
     queue still receives `(meeting_id, audio_path)` exactly as it does today; only
     the source of `meeting_id` shifts upstream. No delta needed for that capability. -->

## Impact

**Rust (`frontend/src-tauri/`)**
- `audio/recording_commands.rs`: generate UUID at the top of `start_recording_with_meeting_name` and `start_recording_with_devices_and_meeting`; thread it onto `RecordingManager`; widen `StopRecordingResult` with `meeting_id`; include it in event payloads; add SQLite save step to `background_shutdown` after `save_recording_only`.
- `lib.rs`: top-level `start_recording` command signature changes from `Result<(), String>` to `Result<StartRecordingResult, String>`; command registration list unchanged except for re-export.
- `audio/recording_manager.rs`: add `meeting_id: String` field; constructor generates `format!("meeting-{}", uuid::Uuid::new_v4())`; expose via `get_meeting_id()`.
- `audio/recording_saver.rs`: populate `MeetingMetadata.meeting_id` via a new `set_meeting_id()` method; include `meeting_id` in the `recording-saved` event payload.
- `database/repositories/transcript.rs`: `save_transcript` takes `meeting_id: &str` as the first arg; remove the internal `Uuid::new_v4()` call; surface `UNIQUE` violations as a typed `MeetingAlreadyExistsError`.
- `api/api.rs`: add `From<recording_saver::TranscriptSegment> for api::TranscriptSegment` conversion. `api_save_transcript` remains unchanged (backward compat).

**TypeScript (`frontend/src/`)**
- `services/recordingService.ts`: typed return of `startRecording*` becomes `{ meeting_id: string }`; add `RecordingSavedToDbPayload` type.
- `services/storageService.ts`: no change (no longer called from stop flow).
- `services/queueService.ts`: no change; `enqueueTranscriptionJob` already takes `meetingId`.
- `hooks/useRecordingStop.ts`: consume `meeting_id` from `stopResult`; call `enqueueTranscriptionJob` and `router.push` immediately; remove `storageService.saveMeeting` call; listen for `recording-saved-to-db` to trigger `refetchMeetings` and `markMeetingAsSaved`.
- `hooks/useAutoDetect.ts`: update cancel path to read `activeMeetingId` from context instead of passing `''`.
- `hooks/useRecordingStart.ts`: capture `meeting_id` from `startRecordingWithDevices` result and stash in context (all 3 call sites).
- `contexts/TranscriptContext.tsx`: add `activeMeetingId: string | null` with setter; cleared on `recording-stopped`.

**Python (`backend/`)**
- `app/main.py`: `SaveTranscriptRequest` gains optional `meeting_id: Optional[str]`; `/save-transcript` uses it if present, otherwise falls back to the existing `meeting-{int(time.time() * 1000)}` shape.
- `app/db.py`: no change; `save_meeting(meeting_id, ...)` already takes the ID as a parameter.

**Tests**
- New adversarial tests for the categories called out in §4 of `CLAUDE.md`: duplicate save, malformed UUID rejected, start↔stop UUID round-trip, SQLite save in background_shutdown, and a frontend regression pin that navigation does not await save.

**Migrations / data**
- No DB schema migration. Existing rows keep their IDs; only the upstream source of new IDs moves.

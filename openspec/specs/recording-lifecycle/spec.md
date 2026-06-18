# Recording Lifecycle — Capability Spec

> Status: **updated 2026-05-26** — delete-meeting-folder-cleanup: `delete_meeting` removes
> on-disk folder after DB transaction commits; decouple-meeting-id-from-save: SQLite save
> moved into `background_shutdown`; frontend stop hook no longer calls `saveMeeting()`.

---

## Purpose

The recording lifecycle governs start, stop, and shutdown of meeting capture. It guarantees the UI reflects recording state changes within strict time bounds while the remaining shutdown work (audio flush, SQLite save, phase reset, scheduler gate release) proceeds asynchronously in `background_shutdown`.
## Requirements
### Requirement: Status bar clears within 1 s of stop command

When the user invokes `stop_recording`, the `RecordingStatusBar` SHALL disappear
(i.e., `isRecording` becomes `false` in the frontend) no later than **1 second**
after the audio streams are released. The remaining shutdown work — MP4 flush and
finalization, SQLite row creation, phase reset, scheduler gate release — runs in the
background (`background_shutdown` task) and does NOT block the UI update.

#### Scenario: UI state is unambiguous immediately after Stop

- **GIVEN** a recording is active
- **WHEN** `stop_recording` is invoked
- **THEN** within 1 second the status bar disappears (or transitions to a clear
  "Saving…" state that does NOT resemble the active-recording state)
- **AND** the Stop button is disabled (already the case via `isStopping`)
- **AND** the disabled Stop button and the status label convey the SAME message:
  recording has ended, background work is in progress

#### Scenario: Stop with a background_shutdown task in flight

- **GIVEN** a recording is active
- **WHEN** `stop_recording` is invoked
- **THEN** the audio streams are released within 1 second
- **AND** the status bar clears within 1 second of stream release
- **AND** `background_shutdown` completes: MP4 flush → SQLite save → PhaseGuard reset (Saving → Idle) → scheduler gate release (`set_recording_gate false`) → `queue.resume_all()`
- **AND** the frontend (`useRecordingStop.ts`) enqueues a transcription job via `enqueue_transcription_job(meetingId, audioPath)` immediately after `stop_recording` returns — the SQLite save runs in `background_shutdown`, not in the frontend

> **Cross-reference:** The transcription job enqueue step is part of the stop lifecycle but is not governed by `RecordingPhase`. See `openspec/specs/post-meeting-pipeline/spec.md` for the queue contract.

---

#### Requirement: Stop command is idempotent

A second `stop_recording` invocation while the first is still in progress SHALL be a no-op. The audio streams, transcription task, and file saver are owned by exactly one shutdown sequence, so a concurrent second call finds them already released.

#### Scenario: User double-presses the Stop button

- **WHEN** the user presses Stop AND immediately presses Stop again before the
  status bar has cleared
- **THEN** the second press is silently ignored (frontend `isStopping` guard OR
  backend `IS_RECORDING` check)
- **AND** the recording is stopped exactly once with no partial cleanup

---

#### Requirement: Audio capture halts within 1 second of stop command

No audio samples recorded after the CPAL streams are released SHALL appear in
the saved file. The incremental saver flushes its in-memory buffer before
finalizing, but the flush boundary is the moment of stream release.

#### Scenario: User speaks immediately after pressing Stop

- **GIVEN** the user presses Stop at time T
- **WHEN** the user speaks at time T + 2 s (after streams are released)
- **THEN** that speech is NOT present in the saved audio file

#### Scenario: User speaks in the 1-second window while streams are closing

- **GIVEN** the user presses Stop at time T
- **WHEN** the user speaks at time T + 0.5 s (streams may still be draining)
- **THEN** whether this audio is captured is implementation-defined, but the
  duration of the capture window SHALL NOT exceed 1 second from the stop command

---

### Requirement: `start_recording` generates and returns the meeting identifier

The recording lifecycle SHALL be the canonical source of the meeting identifier. When the user invokes `start_recording`, `start_recording_with_devices`, or `start_recording_with_devices_and_meeting`, the system SHALL generate a UUID-shaped identifier of the form `meeting-<UUID v4>` BEFORE returning control to the caller, and SHALL include it in the command result as `StartRecordingResult { meeting_id: String }`. The identifier SHALL also appear on the `recording-started` event payload as `meeting_id`. The identifier MUST be non-empty and MUST match the regular expression `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`.

#### Scenario: Successful start returns a well-formed meeting_id

- **GIVEN** no recording is active
- **WHEN** the caller invokes `start_recording`
- **THEN** the command resolves with `StartRecordingResult { meeting_id }` where `meeting_id` matches `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`
- **AND** the `recording-started` event payload carries the same `meeting_id` value

#### Scenario: meeting_id is stable across the recording lifetime

- **GIVEN** `start_recording` returned `meeting_id = M`
- **WHEN** the recording continues for any duration (including device-reconnect retries)
- **THEN** every subsequent inspection of the active recording's identifier (Tauri command, event payload, or `get_recording_state` field) returns the same `M`
- **AND** the identifier does NOT change when the user renames the meeting via `set_active_meeting_name`

#### Scenario: Concurrent start while saving does not mint a second id

- **GIVEN** a previous recording is in the `Saving` phase (background shutdown still finalising)
- **WHEN** the caller invokes `start_recording` before the phase returns to `Idle`
- **THEN** the command rejects with an error
- **AND** no new `meeting_id` is generated or returned

---

### Requirement: `stop_recording` returns the start-time meeting_id

`stop_recording` SHALL include the active recording's `meeting_id` in its `StopRecordingResult`, alongside `folder_path` and `meeting_name`. The identifier returned by `stop_recording` SHALL be byte-equal to the identifier returned by the matching `start_recording` invocation. The `recording-stopped` event payload SHALL include the same `meeting_id`.

#### Scenario: stop returns the same id that start returned

- **GIVEN** `start_recording` returned `meeting_id = M`
- **WHEN** the caller invokes `stop_recording`
- **THEN** the command resolves with `StopRecordingResult { meeting_id, folder_path, meeting_name }` where `meeting_id` is byte-equal to `M`
- **AND** the `recording-stopped` event payload carries the same `M`

#### Scenario: stop on the idle path returns no id

- **GIVEN** no recording is active (phase is `Idle`)
- **WHEN** the caller invokes `stop_recording`
- **THEN** the command resolves with `StopRecordingResult { meeting_id: None, folder_path: None, meeting_name: None }`
- **AND** no error is raised

#### Scenario: stop on the saving path returns no id

- **GIVEN** the phase is `Saving` from a previous stop
- **WHEN** the caller invokes a second `stop_recording`
- **THEN** the command resolves with `StopRecordingResult { meeting_id: None, folder_path: None, meeting_name: None }` (idempotent no-op)
- **AND** no error is raised

---

### Requirement: SQLite save runs in `background_shutdown` before gate clears

The `background_shutdown` task SHALL call `TranscriptsRepository::save_transcript` with the start-time `meeting_id`, meeting name, transcript segments, and folder path AFTER `save_recording_only` (audio.mp4) completes, and BEFORE `clear_gate_and_resume!()`. This guarantees the SQLite row exists when the queue worker dequeues the transcription job.

The save SHALL run regardless of whether `auto_save` is enabled — the meeting row and transcript rows are independent of audio file existence.

On `MeetingAlreadyExistsError`, the save SHALL be treated as idempotent success (log and continue). On other errors, `background_shutdown` SHALL log the error, emit `recording-save-failed`, and still clear the gate.

After successful save, `background_shutdown` SHALL emit `recording-saved-to-db { meeting_id }` so the frontend can update the sidebar and mark the meeting as saved.

#### Scenario: SQLite row exists before queue worker runs

- **GIVEN** `stop_recording` returned `meeting_id = M` and the frontend enqueued a transcription job
- **WHEN** `background_shutdown` completes and `clear_gate_and_resume!()` fires
- **THEN** the `meetings` table has a row with `id = M`
- **AND** the queue worker can dequeue the job and find the row

#### Scenario: SQLite save with auto_save disabled

- **GIVEN** `auto_save` is `false` (no audio file)
- **WHEN** `background_shutdown` runs
- **THEN** `save_recording_only` returns early (no audio)
- **AND** the SQLite save still runs, creating the meeting row and transcript rows
- **AND** no `recording-saved` event fires (no audio.mp4), but `recording-saved-to-db` fires

#### Scenario: Duplicate save is idempotent

- **GIVEN** a row with `id = M` already exists (e.g., from a stale retry)
- **WHEN** `background_shutdown` calls `save_transcript` with `meeting_id = M`
- **THEN** the call returns `MeetingAlreadyExistsError`
- **AND** the error is treated as idempotent success
- **AND** `clear_gate_and_resume!()` proceeds normally

---

### Requirement: Frontend stop hook navigates and enqueues without IPC save

The frontend stop handler SHALL invoke `enqueueTranscriptionJob(meeting_id, audio_path)` (when `folder_path` is non-null) and show a toast with a "View Meeting" action WITHOUT calling `storageService.saveMeeting()`. The SQLite save is handled by Rust's `background_shutdown`.

The frontend SHALL listen for `recording-saved-to-db { meeting_id }` to trigger `refetchMeetings()` and `markMeetingAsSaved()`. The frontend SHALL listen for `recording-save-failed { error }` to show an error toast.

Concretely: the user-visible time from `stop_recording` resolving to the toast appearing SHALL NOT exceed **200 ms** under normal conditions, and SHALL NOT scale with the size of the transcript array.

#### Scenario: Toast appears within 200 ms of stop_recording resolving

- **GIVEN** a recording with N transcript segments (any N from 1 to 10000)
- **WHEN** `stop_recording` resolves at time T
- **THEN** the success toast with "View Meeting" action is shown no later than `T + 200ms`
- **AND** the value of N has no measurable effect on this delay

#### Scenario: Queue enqueue happens immediately after stop

- **GIVEN** a recording has just stopped with a non-null `folder_path`
- **WHEN** the frontend stop handler runs
- **THEN** `enqueueTranscriptionJob(meeting_id, audio_path)` is invoked
- **AND** no `storageService.saveMeeting()` call is made

#### Scenario: recording-save-failed surfaces as toast

- **GIVEN** the SQLite save in `background_shutdown` fails
- **WHEN** the frontend receives `recording-save-failed`
- **THEN** an error toast is shown
- **AND** the user can retry from the meeting-details page

---

### Requirement: `TranscriptsRepository::save_transcript` accepts and validates a client-supplied meeting_id

`TranscriptsRepository::save_transcript` SHALL accept `meeting_id: &str` as the first parameter and use it as the value of `INSERT INTO meetings (id, ...)`. The repository SHALL NOT mint a new UUID. The parameter SHALL be validated against `^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$` before any DB interaction. A SQLite `UNIQUE` violation on the `meetings.id` column SHALL be surfaced as `MeetingAlreadyExistsError`.

#### Scenario: Valid meeting_id is persisted as the row primary key

- **GIVEN** a valid `meeting_id = "meeting-<UUID v4>"`
- **WHEN** `save_transcript` is called with that id, a title, transcripts, and a folder path
- **THEN** the row in the `meetings` table has `id = meeting_id`

#### Scenario: Empty meeting_id is rejected before DB write

- **GIVEN** `meeting_id = ""`
- **WHEN** `save_transcript` is called
- **THEN** the call returns `InvalidMeetingIdError`
- **AND** no row is inserted in the `meetings` table

#### Scenario: Malformed meeting_id is rejected before DB write

- **GIVEN** `meeting_id = "meeting-not-a-uuid"` or any string not matching the regex
- **WHEN** `save_transcript` is called
- **THEN** the call returns `InvalidMeetingIdError`
- **AND** no row is inserted in the `meetings` table

#### Scenario: Duplicate save with the same meeting_id is distinguishable

- **GIVEN** a row with `id = meeting_id` already exists in the `meetings` table
- **WHEN** `save_transcript` is called with the same `meeting_id`
- **THEN** the call returns `MeetingAlreadyExistsError`
- **AND** the existing row is NOT modified
- **AND** callers can treat this error as idempotent success

---

### Requirement: `cancel_recording` deletes the row identified by the start-time meeting_id

When the frontend invokes `cancel_recording(meeting_id)`, the system SHALL use the start-time `meeting_id` from context. `delete_meeting_row_inner` SHALL be a no-op when no row exists (the recording was cancelled before save committed). The recording folder SHALL be deleted regardless of whether a row was written. The Rust side SHALL log a warning when `meeting_id` is empty.

#### Scenario: Cancel after save deletes both row and folder

- **GIVEN** `start_recording` returned `meeting_id = M` AND the save has committed
- **WHEN** the user invokes `cancel_recording(M)`
- **THEN** the row with `id = M` is deleted from the `meetings` table
- **AND** the meeting folder on disk is removed
- **AND** the command resolves with `M`

#### Scenario: Cancel before save still deletes the folder

- **GIVEN** `start_recording` returned `meeting_id = M` AND no save has occurred yet
- **WHEN** the user invokes `cancel_recording(M)`
- **THEN** the meeting folder on disk is removed
- **AND** the DB DELETE matches zero rows (no error)
- **AND** the command resolves with `M`

#### Scenario: Cancel with empty meeting_id logs warning

- **GIVEN** `start_recording` returned `meeting_id = M`
- **WHEN** the frontend invokes `cancel_recording("")`
- **THEN** a warning is logged about empty meeting_id
- **AND** the folder deletion proceeds (if the manager has it)

---

### Requirement: `delete_meeting` removes the on-disk folder after DB transaction commits

When the frontend invokes `api_delete_meeting`, `MeetingsRepository::delete_meeting` SHALL read `folder_path` from the meetings row before the transaction begins. After the transaction commits successfully, the repository SHALL validate that the path contains `meetily-recordings` (preventing path traversal), then call `std::fs::remove_dir_all` on the folder. This ensures the filesystem is cleaned up alongside the DB rows.

If `folder_path` is `None` or the folder does not exist on disk, the deletion succeeds silently. If the path fails the validation check, the folder deletion is skipped and an error is logged. If `remove_dir_all` fails (permission denied, file in use), the DB deletion still succeeds — the error is logged but not surfaced to the user.

#### Scenario: Delete removes both DB rows and folder

- **GIVEN** a meeting with `id = M` and `folder_path = P` exists, and the folder at `P` contains `audio.mp4`
- **WHEN** the frontend invokes `api_delete_meeting(M)`
- **THEN** the meetings, transcripts, transcript_chunks, and summary_processes rows are deleted
- **AND** the folder at `P` and all its contents are removed from disk

#### Scenario: Delete with null folder_path succeeds without filesystem ops

- **GIVEN** a meeting with `id = M` and `folder_path = NULL`
- **WHEN** the frontend invokes `api_delete_meeting(M)`
- **THEN** the DB rows are deleted
- **AND** no filesystem operations are attempted

#### Scenario: Delete with missing folder still succeeds

- **GIVEN** a meeting with `id = M` and `folder_path = P`, but the folder at `P` was already manually deleted
- **WHEN** the frontend invokes `api_delete_meeting(M)`
- **THEN** the DB rows are deleted
- **AND** no error is raised for the missing folder

#### Scenario: Folder deletion failure does not block DB deletion

- **GIVEN** a meeting with `id = M` and `folder_path = P`, and `remove_dir_all(P)` fails (e.g., file in use)
- **WHEN** the frontend invokes `api_delete_meeting(M)`
- **THEN** the DB rows are deleted and the command returns success
- **AND** the folder deletion error is logged
- **AND** the user is NOT shown an error

#### Scenario: Path traversal is rejected

- **GIVEN** a meeting with `id = M` and `folder_path = "../../etc"` (or any path not containing `meetily-recordings`)
- **WHEN** the frontend invokes `api_delete_meeting(M)`
- **THEN** the DB rows are deleted
- **AND** the folder deletion is skipped with a logged warning
- **AND** no filesystem operation is performed outside the recordings root

### Requirement: Diarization queue phase does not delay the recording-stop status bar guarantee

The `Diarizing` queue phase SHALL run as a separate job after `background_shutdown` completes. It SHALL NOT block or delay the existing 1-second status-bar-clear guarantee of `stop_recording`: the `RecordingStatusBar` disappears within 1 second of stream release regardless of whether diarization is enabled or still queued.

#### Scenario: Diarization phase does not delay the 1-second status bar clear

- **GIVEN** a recording is active and speaker diarization is enabled
- **WHEN** `stop_recording` is invoked
- **THEN** the status bar still clears within 1 second of stream release
- **AND** the `Diarizing` queue phase runs later as a separate job, after `background_shutdown` completes, and never blocks the status bar UI

---

### Requirement: TranscriptSegment and TranscriptUpdate carry an optional speaker field

`TranscriptSegment` (Rust) and `TranscriptUpdate` (Tauri event) SHALL include an optional `speaker: Option<String>` field. The field SHALL be `None` during recording (no speaker labels available in offline-only mode) and SHALL be populated after the `Diarizing` queue phase completes.

`TranscriptUpdate` SHALL also include an optional `token_timestamps: Option<String>` field containing a JSON array of `{word: string, start_ms: i64, end_ms: i64}` objects, populated when the transcription provider supports token-level timestamps (Whisper). The field SHALL be `None` for providers that do not support token timestamps (Parakeet).

#### Scenario: TranscriptUpdate during recording has no speaker

- **WHEN** a `TranscriptUpdate` event is emitted during recording
- **THEN** `speaker` is `None`
- **AND** `token_timestamps` is populated if the Whisper provider is active

#### Scenario: TranscriptUpdate speaker populated after diarization

- **WHEN** the `Diarizing` phase completes for a meeting
- **THEN** the transcript rows in the database have `speaker` set to the assigned label
- **AND** the frontend receives a `diarization-complete` event with `{meeting_id, speakers: [{label, name, color}]}`

---

### Requirement: `diarization-complete` event updates frontend speaker state

After the `Diarizing` phase completes, the Rust side SHALL emit a `diarization-complete` Tauri event with the meeting ID and an array of speaker assignments (cluster label, resolved name, color). The frontend SHALL update the transcript view with speaker badges for the meeting.

#### Scenario: Frontend receives diarization-complete

- **WHEN** the `Diarizing` phase completes for meeting `M`
- **THEN** a `diarization-complete` event is emitted with `{meeting_id: "M", speakers: [{label: "Speaker 1", name: "Alice", color: "#0EA5E9"}, {label: "Speaker 2", name: null, color: "#F97316"}]}`
- **AND** the frontend updates the meeting's transcript view with speaker badges


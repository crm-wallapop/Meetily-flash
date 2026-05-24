## ADDED Requirements

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

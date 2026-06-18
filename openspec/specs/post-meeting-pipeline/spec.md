# Post-Meeting Pipeline — Capability Spec

> Status: **proposed** — new capability introduced by `post-meeting-transcription`.

---

## Purpose

Drives the post-recording pipeline: when a recording stops, a transcription job is enqueued on a single-worker FIFO queue, optionally chained to LLM summarisation, gated by a scheduler that pauses on recording/meeting/CPU/RAM/manual signals, and surfaced to the UI via `transcription-queue-changed` snapshots that persist across restarts via IndexedDB.
## Requirements
### Requirement: Transcription is triggered automatically after every recording stops

When `stop_recording` finalises the MP4 of a non-cancelled recording, the system SHALL enqueue a transcription job for that meeting on the transcription queue. Transcription SHALL NOT be triggered if the recording was cancelled via `cancel_recording`.

#### Scenario: Normal stop enqueues a transcription job

- **WHEN** the user stops a recording via `stop_recording` and the MP4 is finalised
- **THEN** a transcription job is enqueued with `status = "pending"` and the meeting's `audio.mp4` path within 1 second of the MP4 being finalised

#### Scenario: Cancelled recording does not enqueue

- **WHEN** the recording is cancelled via `cancel_recording`
- **THEN** no transcription job is enqueued AND no transcription progress events are emitted

---

### Requirement: Summarisation is chained after transcription when an LLM provider is configured

After transcription completes successfully, the system SHALL automatically chain LLM summarisation if and only if a provider is configured in settings. The summary SHALL use the transcript produced by the transcription job. The chained summary job SHALL obey the same scheduling gates as the transcription job.

#### Scenario: Summary fires when provider is configured

- **WHEN** a transcription job completes successfully AND an LLM provider is configured in settings
- **THEN** the same queue entry transitions to `phase = "summarising"` and the summary runs automatically without user action

#### Scenario: No summary when provider is absent

- **WHEN** a transcription job completes successfully AND no LLM provider is configured
- **THEN** the queue entry transitions to `status = "done"` AND no summary is triggered

#### Scenario: Transcription failure does not trigger summarisation

- **WHEN** a transcription job fails with an error
- **THEN** summarisation is NOT triggered, the queue entry transitions to `status = "failed"`, and the error is surfaced in the UI

---

### Requirement: The transcription queue is single-worker and FIFO

The system SHALL maintain a single queue of pending transcription jobs and process them one at a time in the order they were enqueued. Concurrent execution of multiple Whisper jobs is forbidden because Whisper is a singleton GPU resource.

#### Scenario: Multiple recordings stack as pending jobs

- **WHEN** three recordings (M1, M2, M3) are stopped in sequence with M1's transcription still in progress
- **THEN** M2 and M3 are enqueued with `status = "pending"` and are processed in that order after M1 completes
- **AND** `queuePosition` ordinals are deferred to `transcription-scheduler-advanced` (the field is absent from `QueueJob` in this change)

#### Scenario: New recording does not block enqueue

- **WHEN** the user is actively recording M4 AND M1's transcription is still running
- **THEN** stopping M4 enqueues it normally; the queue accepts new jobs regardless of worker state

---

### Requirement: Scheduler gates pause background work when the system is busy

A scheduler SHALL gate transcription and summary job execution against the following signals, all AND-ed. Any single busy signal pauses all background work.

- **Recording active**: `RECORDING_PHASE != Idle`
- **Meeting application detected**: `meeting_detector` reports an active call (Meet/Zoom window detected)
- **CPU above 70 % sustained for 30 s**: rolling 30 s window of CPU samples
- **RAM above 80 % sustained for 30 s**: rolling 30 s window of RAM samples
- **Manual pause**: user has invoked `pause_all_background_work`

When any gate is busy, in-flight jobs SHALL finish their current Whisper chunk and then yield, transitioning to `status = "paused"` with a populated `pauseReason`. The worker SHALL resume the paused job once all gates are clear, using hysteresis on the sustained-duration gates (resume requires the same duration of clear samples that triggered the pause).

The thresholds and durations in this spec are the hardcoded defaults in this change; they become user-configurable in the follow-up change `transcription-scheduler-advanced`.

#### Scenario: Pause when a new recording starts

- **GIVEN** a transcription job is in progress AND the user starts a new recording (phase transitions to `Recording`)
- **WHEN** the current Whisper chunk completes
- **THEN** the job transitions to `status = "paused"`
- **AND** the job resumes once the user stops the new recording AND the other gates remain clear

> `pauseReason` field is deferred to `transcription-scheduler-advanced`.

#### Scenario: Pause when CPU is sustained over 70 % for 30 s

- **GIVEN** a transcription job is in progress
- **WHEN** CPU readings stay above 70 % for 30 consecutive seconds
- **THEN** the job transitions to `status = "paused"` at the next chunk boundary

> `pauseReason = "cpu_high"` field is deferred to `transcription-scheduler-advanced`.

#### Scenario: Resume requires sustained-clear, not single-sample-clear (hysteresis)

- **GIVEN** a job is paused with `pauseReason = "cpu_high"`
- **WHEN** CPU drops below 70 % for a single sample but rises again within 30 s
- **THEN** the job remains paused
- **WHEN** CPU stays below 70 % for 30 consecutive seconds
- **THEN** the job resumes

#### Scenario: Manual pause overrides everything

- **WHEN** the user invokes `pause_all_background_work`
- **THEN** the `SHOULD_YIELD` signal is asserted so any in-flight retranscription exits at its next chunk boundary
- **AND** `scheduler.manual_pause_all` is set so no pending jobs are picked up until `resume_all_background_work` is invoked
- **AND** a `transcription-queue-changed` event with `manual_pause_all = true` is emitted synchronously

> `pauseReason = "manual"` field is deferred to `transcription-scheduler-advanced`.

---

### Requirement: Pause granularity is chunk-boundary

The worker SHALL NOT interrupt a Whisper decode mid-chunk. A pause signal takes effect when the current chunk completes. The maximum pause latency is bounded by the configured chunk duration (typically ~30 s for the retranscription path).

#### Scenario: Pause signal arrives mid-chunk

- **GIVEN** a Whisper chunk is decoding
- **WHEN** the scheduler signals "yield"
- **THEN** the worker continues decoding the current chunk to completion
- **AND** does not start the next chunk
- **AND** the job state transitions to `paused` after the current chunk's output is written

---

### Requirement: Progress events reflect queue and per-job state

The system SHALL emit a `transcription-queue-changed` Tauri event whenever any job's state changes or any scheduler gate transitions. The payload is a full snapshot of the queue.

#### Scenario: State change emits queue snapshot

- **WHEN** a job transitions from `pending` to `in_progress`, or `in_progress` to `paused`/`done`/`failed`, or the scheduler transitions a gate
- **THEN** a `transcription-queue-changed` event is emitted with `{ jobs: [{ meeting_id, audio_path, status, phase }], manual_pause_all: boolean }`

#### Scenario: Manual pause emits an immediate snapshot

- **WHEN** the user invokes `pause_all_background_work` or `resume_all_background_work`
- **THEN** a `transcription-queue-changed` event is emitted synchronously with the updated `manual_pause_all` flag, so the global Pause/Resume toggle reflects the new state without waiting for the next worker-loop transition

#### Scenario: Per-meeting progress percentage is rendered when available

- **GIVEN** a job is in `status = "in_progress"` AND `phase = "Transcribing"`
- **WHEN** the underlying retranscription processor emits a `retranscription-progress` event with `progress_percentage`
- **THEN** the per-meeting badge renders `Transcribing N%` rather than `Transcribing…`

> **Implementation note (drift):** The following payload fields remain deferred to `transcription-scheduler-advanced` and are absent in this change:
> `queuePosition`, `pauseReason`, `startedAt`, `completedAt`, `lastError`, and the top-level `schedulerState.gates` object.
> Progress percentage is sourced from the existing per-meeting `retranscription-progress` event rather than embedded in `QueueJob`. The per-meeting badge therefore renders `Transcribing N%` / `Queued` / `Paused` (no `#N` ordinal, no `— <reason>` qualifier) until `transcription-scheduler-advanced` adds the remaining fields.

---

### Requirement: Jobs are cancellable from the queue

The user SHALL be able to cancel any queued or in-progress job. Cancellation transitions the job to `cancelled` and triggers the existing `RETRANSCRIPTION_CANCELLED` flag if the job is currently running. The MP4 is preserved; the meeting metadata is preserved; only the queue entry is removed.

#### Scenario: Cancel a pending job

- **GIVEN** a job has `status = "pending"`
- **WHEN** the user invokes `cancel_queued_job(meeting_id)`
- **THEN** the job is removed from the queue AND the queue snapshot reflects the new ordering

#### Scenario: Cancel an in-progress job

- **GIVEN** a job is currently being processed
- **WHEN** the user invokes `cancel_queued_job(meeting_id)`
- **THEN** the worker exits at the next chunk boundary AND the job is removed from the queue AND any partial transcript is discarded

---

### Requirement: Queue state persists across app restarts via IndexedDB

The transcription queue SHALL persist its state in IndexedDB so that jobs in `pending` or `in_progress` from a previous app session are detected on next launch. The queue schema in IndexedDB is the authoritative persistence layer.

> **Implementation note (drift):** The Rust `TranscriptionQueue` is in-memory only. IndexedDB persistence is maintained by the frontend: `page.tsx` subscribes to `transcription-queue-changed` events and mirrors each snapshot to IndexedDB via `upsertQueueJob` / `updateJobStatus`. On restart, the recovery flow reads IndexedDB to reconstruct stale jobs.

#### Scenario: Pending job survives app restart

- **GIVEN** a job has `status = "pending"` AND the app is closed
- **WHEN** the app is relaunched
- **THEN** the recovery modal presents the pending job as recoverable
- **AND** accepting the recovery re-enqueues the job
- **AND** dismissing the recovery removes the job from the queue (the user can still manually re-trigger from the meeting view)

#### Scenario: In-progress job from a crashed session is recoverable

- **GIVEN** a job had `status = "in_progress"` when the app crashed
- **WHEN** the app is relaunched
- **THEN** the recovery modal presents the job AND treats it as pending on re-enqueue (the worker re-runs from the start of the MP4; partial transcripts from the crashed session are discarded)

### Requirement: Frontend tolerates malformed queue-state payloads without crashing

The frontend adapter SHALL normalize every `QueueSnapshot` payload received from `get_queue_state` and `transcription-queue-changed` events before it enters React state, coercing any missing or wrong-typed `jobs` to an empty array and any missing or wrong-typed `manual_pause_all` to `false`. No consumer of the queue snapshot SHALL reach a `.find` (or any array operation) on `jobs` without the adapter's guarantee that it is an array.

#### Scenario: Missing jobs array does not crash

- **WHEN** a `transcription-queue-changed` event or `get_queue_state` response arrives with a payload lacking a `jobs` field (e.g. `{ manual_pause_all: true }`)
- **THEN** the frontend stores `{ jobs: [], manual_pause_all: true }` in state and no `Cannot read properties of undefined (reading 'find')` error is thrown

#### Scenario: Non-array jobs does not crash

- **WHEN** a queue-state payload arrives with `jobs` set to a non-array value (e.g. a string or `null`)
- **THEN** the frontend coerces `jobs` to `[]` and continues rendering

#### Scenario: Missing manual_pause_all defaults to false

- **WHEN** a payload arrives without `manual_pause_all`
- **THEN** the frontend treats it as `false`

#### Scenario: Well-formed payload passes through unchanged

- **WHEN** a payload arrives with a valid `jobs` array and a boolean `manual_pause_all`
- **THEN** the normalized snapshot is structurally identical to the input (no fields dropped, no values coerced)

#### Scenario: Sidebar renders meeting items on a malformed payload

- **WHEN** the Meeting Notes sidebar expands to render meeting items AND the queue-state payload is malformed (missing `jobs`)
- **THEN** the meeting items render without a runtime error and the page emits no uncaught error


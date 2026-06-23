## ADDED Requirements

### Requirement: Retranscription checkpoints preserve partial progress

The retranscription processor SHALL persist each transcribed speech segment to a scratch `retranscription_checkpoints` table (keyed by `meeting_id` and `segment_index`, also storing `text`, `start_ms`, `end_ms`, `confidence`) immediately after the segment is transcribed and before the next segment begins. On any invocation — a fresh start, a resume after pause, or a recovery after crash — the processor SHALL load existing checkpoints for the meeting, skip transcription of segments whose checkpoint `(start_ms, end_ms)` matches the re-derived VAD segment at that index, pre-populate the in-memory transcript accumulator from those checkpoints, and resume the transcription loop at the first non-checkpointed segment. The processor SHALL delete all checkpoints for a meeting once the full transcription completes (after the final `transcripts` rows are written) and SHALL delete them on cancellation. Progress events emitted after a resume SHALL reflect the checkpointed fraction rather than restarting from the decode percentage.

#### Scenario: Pause mid-run then resume skips checkpointed segments

- **GIVEN** a retranscription job transcribed N segments (checkpoints persisted) and then paused at a chunk boundary
- **WHEN** the job is resumed
- **THEN** the processor loads the N checkpoints AND does NOT re-transcribe those segments AND resumes transcription at segment N+1 AND the first transcription-progress event reflects the N completed segments (not the decode percentage)

#### Scenario: Crash recovery resumes from the last checkpoint

- **GIVEN** a retranscription job persisted checkpoints for N segments before the app crashed
- **WHEN** the job is re-enqueued on next launch and the processor is invoked
- **THEN** the processor loads the N checkpoints AND resumes transcription at segment N+1 (partial transcripts from before the crash are NOT discarded)

#### Scenario: Checkpoints are deleted on completion

- **WHEN** retranscription completes for a meeting and the final `transcripts` rows are written
- **THEN** all `retranscription_checkpoints` rows for that meeting are deleted (no scratch rows remain)

#### Scenario: Checkpoints are deleted on cancel

- **WHEN** a retranscription job is cancelled
- **THEN** all `retranscription_checkpoints` rows for that meeting are deleted so a later re-transcription starts clean

#### Scenario: VAD boundary mismatch invalidates a checkpoint

- **GIVEN** a checkpoint exists at `segment_index = K` whose `(start_ms, end_ms)` does not match the re-derived VAD segment at index K (e.g. VAD params changed between runs)
- **WHEN** the processor loads checkpoints on invocation
- **THEN** that checkpoint SHALL NOT be trusted AND the segment at index K SHALL be re-transcribed (correctness is preferred over avoiding rework)

#### Scenario: Checkpoint write failure degrades to today's restart behaviour

- **WHEN** a checkpoint `INSERT` fails (e.g. transient disk error)
- **THEN** the processor SHALL log the failure AND continue transcribing the remaining segments in the current run AND the job SHALL NOT be aborted solely because checkpointing failed (on the next resume, segments whose checkpoint write failed are re-transcribed — today's restart behaviour for those segments)

## MODIFIED Requirements

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
- **THEN** the recovery modal presents the job AND treats it as pending on re-enqueue AND the worker resumes transcription from the last persisted `retranscription_checkpoints` row (segments transcribed before the crash are NOT re-transcribed; see the "Retranscription checkpoints preserve partial progress" requirement)

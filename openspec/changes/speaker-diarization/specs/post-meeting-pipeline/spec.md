# Post-Meeting Pipeline — Delta Spec

> Change: `speaker-diarization`
> Modifies: `post-meeting-pipeline`

---

## ADDED Requirements

### Requirement: Diarizing phase chains after Summarising in the queue

The `JobPhase` enum SHALL be extended with a `Diarizing` variant. After the `Summarising` phase completes successfully (or after `Transcribing` if no summary provider is configured), the queue SHALL chain into the `Diarizing` phase via `JobResult::CompletedChain`. The `Diarizing` phase SHALL obey the same scheduler gates as transcription and summarisation.

The queue snapshot (`QueueSnapshot`) SHALL include `phase = "diarizing"` for jobs in this phase. The frontend `QueueJob` type SHALL include `phase: JobPhase` where `JobPhase` is `"Transcribing" | "Summarising" | "Diarizing"`.

#### Scenario: Diarizing chains after Summarising

- **WHEN** a queue job completes `Summarising` successfully
- **THEN** the job transitions to `phase = "diarizing"` via `JobResult::CompletedChain`
- **AND** the `transcription-queue-changed` event reflects the new phase

#### Scenario: Diarizing chains directly after Transcribing when no summary

- **WHEN** a queue job completes `Transcribing` AND no LLM provider is configured AND no summary phase fires
- **THEN** the job transitions to `phase = "diarizing"` via `JobResult::CompletedChain`

#### Scenario: Diarizing phase respects scheduler gates

- **WHEN** the `Diarizing` phase is about to start AND the scheduler reports `recording_busy = true`
- **THEN** the job transitions to `status = "paused"` until the gate clears
- **AND** diarization does not begin until the scheduler permits

---

### Requirement: Diarizing processor decodes audio and runs offline diarization

A `diarization_processor` function (matching the `ProcessorFn` signature) SHALL be registered for the `Diarizing` phase. The processor SHALL:

1. Read the meeting's `folder_path` from the database
2. Decode `audio.mp4` to raw f32 samples at 16 kHz mono using the existing decoder module
3. Run `OfflineSpeakerDiarization::process(samples)` to produce speaker segments
4. Extract average embeddings per speaker cluster using `SpeakerEmbeddingExtractor`
5. Read transcript rows from the `transcripts` table for this meeting (including `token_timestamps`)
6. Align token timestamps with diarization speaker boundaries
7. Update transcript rows with `speaker` labels and `speaker_source = "auto"`
8. Insert rows into `speaker_embeddings` table
9. Match embeddings against the speaker registry for cross-meeting identification
10. Emit `diarization-complete` event

#### Scenario: Full diarization pipeline

- **GIVEN** a meeting with `audio.mp4` and 5 transcript rows with token timestamps
- **WHEN** the `Diarizing` phase runs
- **THEN** the audio is decoded, diarization produces speaker segments, token timestamps are aligned, transcript rows are updated with speaker labels, embeddings are stored, and the `diarization-complete` event is emitted

#### Scenario: Diarization processor handles decode failure gracefully

- **WHEN** the audio file cannot be decoded
- **THEN** the processor returns `JobResult::Failed(error_message)`
- **AND** the job transitions to `status = "failed"`
- **AND** no transcript rows are modified

## MODIFIED Requirements

### Requirement: Diarizing processor decodes audio and runs offline diarization

A `diarization_processor` function (matching the `ProcessorFn` signature) SHALL be registered for the `Diarizing` phase. The processor SHALL:

1. Read the meeting's `folder_path` from the database
2. Decode `audio.mp4` to raw f32 samples at 16 kHz mono using the existing decoder module
3. Run `OfflineSpeakerDiarization::process(samples)` to produce speaker segments
4. Extract average embeddings per speaker cluster using `SpeakerEmbeddingExtractor`
5. Read transcript rows from the `transcripts` table for this meeting (including `token_timestamps`)
6. Align token timestamps with diarization speaker boundaries
7. Persist the aligned per-speaker splits as separate transcript rows (the split-and-persist operation defined by the `speaker-diarization` capability: replace each multi-speaker source row with N rows — one per aligned segment — within a single transaction; update the single-speaker rows in place). This step replaces the prior "Update transcript rows with `speaker` labels" write, which collapsed N splits onto the source row's id (last-writer-wins).
8. Insert rows into `speaker_embeddings` table
9. Match embeddings against the speaker registry for cross-meeting identification
10. Emit `diarization-complete` event

#### Scenario: Full diarization pipeline

- **GIVEN** a meeting with `audio.mp4` and 5 transcript rows with token timestamps
- **WHEN** the `Diarizing` phase runs
- **THEN** the audio is decoded, diarization produces speaker segments, token timestamps are aligned, multi-speaker transcript rows are **replaced by their per-speaker split rows** (single-speaker rows updated in place), embeddings are stored, and the `diarization-complete` event is emitted
- **AND** a transcript row spanning two speakers is persisted as two rows with disjoint text and distinct `speaker_label`, not collapsed to one label

#### Scenario: Diarization processor handles decode failure gracefully

- **WHEN** the audio file cannot be decoded
- **THEN** the processor returns `JobResult::Failed(error_message)`
- **AND** the job transitions to `status = "failed"`
- **AND** no transcript rows are modified

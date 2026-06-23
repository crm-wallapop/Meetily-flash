## MODIFIED Requirements

### Requirement: Transcript-timestamp-driven speaker diarization runs as a post-processing queue phase

After the transcription and summarisation phases complete, the system SHALL run offline speaker diarization on the meeting's `audio.mp4` as a `Diarizing` phase in the transcription queue. The diarization phase SHALL:

1. Decode the audio to 16kHz mono f32 samples via `DecodedAudio::to_whisper_format()`
2. Read transcript timestamps from the `transcripts` table to define speech segments
3. Chunk each segment into pieces sized at the **effective split granularity** = `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)`, where `speech_seconds` is the total transcript-segment duration and `MAX_DIARIZATION_CHUNKS` caps the total chunk count. The granularity thus stays at `SPLIT_TARGET_SECS` for short meetings and coarsens just enough to keep the chunk count at or below `MAX_DIARIZATION_CHUNKS` for long meetings. Each piece remains within [`MIN_SPEECH_SECS`, `MAX_CHUNK_SECS`].
4. Extract a speaker embedding for each chunk via `SpeakerEmbeddingExtractor` (nemo_titanet; see model-selection requirement)
5. Cluster chunks using centroid-based agglomerative clustering with duration-weighted averaging. The clustering SHALL use an **incremental** similarity scheme — a cached pairwise similarity between alive clusters that is recomputed only for the newly-merged cluster on each merge — so that its total cost is O(n²·log n) in the chunk count n, **not** the O(n³) of a full per-merge pairwise rescan. The clustering SHALL run off the async executor (on a blocking thread) so it can never freeze the UI or block other queue work, and SHALL complete in bounded wall-clock time for any meeting length.
6. Merge short-duration speakers into their cosine-nearest larger cluster
7. Align transcript rows with diarization speaker segments

The clustering output (per-chunk labels and duration-weighted centroids) SHALL be identical regardless of the incremental-optimization internals — the optimization changes cost, not results. The diarization phase SHALL be skipped if no `audio.mp4` exists (e.g., `auto_save = false`). The diarization phase SHALL run on imported audio files using the same queue path.

#### Scenario: Diarization runs after summarisation

- **WHEN** a queue job completes the `Summarising` phase successfully
- **THEN** the job transitions to `phase = "diarizing"` and diarization begins on the meeting's `audio.mp4`
- **AND** a `transcription-queue-changed` event is emitted with the updated phase

#### Scenario: Diarization runs directly after transcription when no summary provider

- **WHEN** a queue job completes the `Transcribing` phase AND no LLM provider is configured
- **THEN** the job transitions to `phase = "diarizing"` (skipping `Summarising`)
- **AND** diarization begins on the meeting's `audio.mp4`

#### Scenario: Diarization is skipped when no audio file exists

- **WHEN** a queue job reaches the `Diarizing` phase AND the meeting has no `audio.mp4` (e.g., `auto_save = false`)
- **THEN** the diarization phase is skipped
- **AND** the job transitions to `status = "done"`

#### Scenario: Diarization runs on imported audio

- **WHEN** an audio file is imported as a new meeting AND the import triggers transcription
- **THEN** the queue job includes the `Diarizing` phase after transcription/summarisation
- **AND** diarization produces speaker labels for the imported audio

#### Scenario: Long meeting does not stall in clustering

- **GIVEN** a meeting with ~83 minutes of speech that would produce ~1500 chunks at a fixed 3 s granularity
- **WHEN** diarization runs the clustering step
- **THEN** the effective split granularity is coarsened so the chunk count is at or below `MAX_DIARIZATION_CHUNKS`
- **AND** the clustering step completes in bounded wall-clock time (seconds, not minutes or hours)
- **AND** a `clustering produced N speakers from M chunks` log line is emitted (the prior failure mode where this line never appeared is gone)

#### Scenario: Short meeting is unaffected by the chunk cap

- **GIVEN** a meeting with ~10 minutes of speech
- **WHEN** diarization computes the effective split granularity
- **THEN** the effective granularity equals `SPLIT_TARGET_SECS` (3.0 s) — unchanged from before this change
- **AND** the chunk count is identical to a fixed-3 s chunker

#### Scenario: Incremental clustering is behaviour-identical to the naive rescan

- **GIVEN** the same set of chunk embeddings and the same merge threshold
- **WHEN** clustering runs via the incremental (cached-similarity) implementation
- **THEN** the resulting per-chunk labels and duration-weighted centroids are identical to those produced by a full per-merge pairwise rescan (verified by a property test against a naive oracle kept under `#[cfg(test)]`)

#### Scenario: Clustering does not freeze the UI

- **GIVEN** a diarization run whose clustering step takes several seconds
- **WHEN** clustering executes
- **THEN** the async runtime and UI remain responsive because clustering runs on a blocking thread, not the executor

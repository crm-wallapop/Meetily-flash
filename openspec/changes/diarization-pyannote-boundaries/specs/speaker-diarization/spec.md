## MODIFIED Requirements

### Requirement: Diarization segment granularity resolves speaker turns within Whisper segments

Whisper groups transcript segments by sentence/VAD, not by speaker; on multi-speaker meetings these segments routinely span 15–30s and contain two or more speakers. The diarization output SHALL be granular enough that a speaker turn occurring inside a single Whisper transcript segment produces a diarization segment boundary at or near the turn, so that per-word alignment can attribute the words on each side of the turn to the correct speakers rather than collapsing the whole segment to one speaker.

Speaker change-point boundaries SHALL be sourced from the pyannote-backed `offline_speaker_diarization` pre-splitter (sherpa-onnx `OfflineSpeakerDiarization`), applied to the recording's 16 kHz mono audio. `FastClusteringConfig.threshold` is a cosine-**dissimilarity** (distance) cutoff where smaller → more clusters; to obtain maximally fragmented candidate boundaries the pre-splitter SHALL configure `FastClusteringConfig { num_clusters: -1, threshold: 0.0 }` together with `min_duration_on: 0.0` and `min_duration_off: 0.0` (so sherpa's internal MergeSegments gap-merge and short-segment drop do not collapse the fine boundaries). The resulting boundaries SHALL be intersected with the transcript-segment speech regions to form the embedding chunk layout. The pre-splitter's segment labels SHALL be discarded; only boundary placement SHALL be carried forward, so Meetily's AHC clustering, label-quality refinement, most-isolated-cluster cap, temporal-coherence smoothing, and cross-meeting registry matching remain authoritative for labeling.

This pre-splitter SUPERSEDES the uniform-chunk-grid boundary source mandated by the canonical "Diarizing processor…" requirement's chunking step ("Chunk each segment into pieces sized at the effective split granularity = `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)`"). After this change, `build_chunks` consumes pyannote-bounded chunks and no longer applies its own `effective_split` uniform grid; the `MAX_DIARIZATION_CHUNKS` cap is enforced once, at the pre-splitter's shed step. (A proposal that leaves both the uniform-grid step and the pyannote pre-splitter mandated simultaneously is NON-CONFORMANT — the canonical spec would contradict itself.)

At `threshold: 0.0` the pre-splitter emits dense candidate splits (≈100 ms resolution), most of which are within-speaker embedding variation rather than speaker turns. Individual candidate boundaries are NOT turns; turns are re-derived by Meetily's AHC + temporal-coherence smoothing. The floor for a *persisted* turn is `MIN_SPEECH_SECS` (1.5 s) as enforced by Meetily's chunking/smoothing — NOT pyannote's `min_duration_on`, which (at 0.0) only stops sherpa from dropping segments internally.

When the candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS`, boundaries SHALL be shed uniformly (every k-th by position) down to the cap BEFORE embeddings are extracted, so the embedding cost is paid only on the capped set. Because boundaries are candidate splits (not turns), uniform shedding lowers per-region resolution without destroying turns (turns are recovered downstream by AHC + smoothing). Sub-`MIN_SPEECH_SECS` fragments that survive the shed SHALL then be merged into their time-neighbor.

#### Scenario: Sub-turn interjection is isolated, not swallowed

- **GIVEN** a Whisper transcript segment from 46:58 to 47:21 containing a 2s Ricardo interjection at 46:58–47:00 followed by Cynthia's speech
- **AND** the production diarization previously labeled the entire 46:58–47:30 run as Cynthia
- **WHEN** diarization runs with the pyannote-backed boundary pre-splitter
- **THEN** the diarization output contains a speaker segment boundary near 47:00 separating Ricardo (≈46:50–47:00) from Cynthia (≈47:00 onward), so the interjection's words are attributed to Ricardo
- **AND** the chunk-grid-only baseline over the same window does not produce that boundary

#### Scenario: Back-and-forth between two speakers is not collapsed to one

- **GIVEN** a region where two speakers alternate in 4–8s turns across a 30s window
- **WHEN** diarization runs with the pyannote-backed boundary pre-splitter
- **THEN** the output preserves the alternation as multiple segments rather than merging the window into a single speaker's run

#### Scenario: Single-speaker meeting is not fragmented

- **GIVEN** a meeting with exactly one speaker
- **WHEN** diarization runs with the pyannote-backed boundary pre-splitter
- **THEN** the output is a single speaker (no spurious second cluster introduced by the finer boundary placement)

#### Scenario: Uniform shed-to-cap still recovers alternation turns on long meetings

- **GIVEN** a long (≥45 min) meeting with a rapid two-speaker alternation region and a single-speaker monologue region of comparable length
- **WHEN** the candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS` and uniform shedding runs, followed by Meetily's AHC + temporal-coherence smoothing
- **THEN** the alternation region's turn structure is recovered (a threshold fraction of within-region turns are preserved in the final labeling), because turns are re-derived from the surviving candidate set, not carried by individual shed boundaries
- **AND** the resulting chunk count is at or below `MAX_DIARIZATION_CHUNKS`

#### Scenario: Pyannote segment labels do not leak into the speaker registry

- **GIVEN** a recording diarized with the pre-splitter enabled
- **WHEN** clustering and registry matching complete
- **THEN** no string from the sherpa `offline_speaker_diarization` label namespace is written to `speaker_embeddings`; labels come only from Meetily's own AHC + registry resolution

#### Scenario: Silent or empty audio does not crash the pre-splitter

- **GIVEN** a silent or empty audio fixture
- **WHEN** the pre-splitter runs
- **THEN** the pipeline returns an empty diarization result without panicking

## ADDED Requirements

### Requirement: The pyannote segmentation model is actually consumed, not phantom-loaded

The pyannote-segmentation ONNX model SHALL be passed to a sherpa-onnx configuration object that consumes it (the `OfflineSpeakerDiarization` pre-splitter's `segmentation` config). A code path that accepts `segmentation_model_path`, checks it exists, and then never references it in any sherpa-onnx config SHALL be considered NON-CONFORMANT. This closes the prior phantom-dependency state where `segmentation_model_path` was accepted by the adapter constructor, existence-checked, and discarded.

#### Scenario: Adapter constructs a config that consumes the segmentation model

- **GIVEN** the diarization adapter constructor is called with a `segmentation_model_path` pointing at the on-disk pyannote model
- **WHEN** the adapter builds its sherpa-onnx configuration
- **THEN** at least one sherpa-onnx config object in the resulting pipeline consumes that model path (no load-or-skip that silently drops it)
- **AND** swapping the model file for a dummy changes the diarizer's behavior (presence-of-path alone is not sufficient evidence of consumption)

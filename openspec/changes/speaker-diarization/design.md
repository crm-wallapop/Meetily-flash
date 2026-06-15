## Context

Meetily records multi-participant meetings but produces speaker-anonymous transcripts. The current pipeline is: VAD → Whisper → transcript segments stored in SQLite. There is no speaker attribution at any stage.

The transcription queue already supports phase chaining (`Transcribing` → `Summarising`) via `JobResult::CompletedChain`. The Whisper provider already sets `set_token_timestamps(true)` but discards the per-token timing data.

The `sherpa-onnx` crate (v1.13.2, Apache-2.0) provides official Rust bindings for `SpeakerEmbeddingExtractor` and `SpeakerEmbeddingManager`. Auto-downloads prebuilt static libraries for all target platforms. Thread-safe (`Send + Sync`).

## Goals / Non-Goals

**Goals:**
- Offline speaker diarization as a post-processing queue phase, chained after summarisation
- Token-level timestamp alignment between Whisper segments and diarization speaker boundaries
- Cross-meeting speaker identification via embedding matching
- Retroactive speaker labeling with persistent per-speaker colors
- Single embedding model (nemo_titanet) and pyannote segmentation model
- Diarization for both recorded and imported audio

**Non-Goals:**
- Real-time speaker labels during recording (online diarization)
- Guided enrollment flow (dedicated "record Alice's voice" UI)
- Speaker source separation (overlapping speech is a known limitation)
- Cloud-based speaker identification
- Automatic meeting detection or speaker count from calendar invites

## Decisions

### D1: `sherpa-onnx` crate as the speaker SDK

**Choice**: Official `sherpa-onnx` crate.
**Alternatives**: `sherpa-rs` (deprecated, README redirects to official crate), hand-rolled ONNX bindings.
**Rationale**: Official crate is maintained by the k2-fsa project, provides `SpeakerEmbeddingExtractor` + `SpeakerEmbeddingManager` with full Rust API, thread-safe, static linking, cross-platform.

### D2: Offline-only diarization as a queue phase

**Choice**: Add `Diarizing` phase to the existing queue, after `Summarising` (or directly after `Transcribing` if no summary provider).
**Alternatives**: Online embedding during recording; standalone post-processing outside the queue.
**Rationale**: Queue phase chaining already exists (`JobResult::CompletedChain`). Diarization runs after transcription completes so it can align against stored transcript segments. Zero CPU overhead during recording.

### D3: Transcript-timestamp-driven chunking with centroid clustering

**Choice**: Instead of using `OfflineSpeakerDiarization`, the pipeline uses transcript timestamps to define audio chunks, extracts 3DSpeaker embeddings per chunk, then runs centroid-based agglomerative clustering.
**Alternatives**: `OfflineSpeakerDiarization` (sherpa-onnx built-in), energy-based VAD chunking.
**Rationale**: Transcript timestamps are more reliable than energy-based VAD for finding speech boundaries. Per-chunk embeddings (~2-5s each) produce clean voice prints. Agglomerative clustering with duration-weighted centroids produces accurate speaker grouping. Constants derived from model characteristics: `MIN_SPEECH_SECS=1.5`, `MAX_CHUNK_SECS=10.0`, `SPLIT_TARGET_SECS=3.0`.

### D4: Centroid extraction from clustering (not re-extraction)

**Choice**: Centroids are returned directly from `cluster_by_centroids()` as duration-weighted averages of per-chunk embeddings. `DiarizationOutput` carries both segments and centroids. No separate extraction step.
**Alternatives**: Re-extract embeddings from merged segments (broken — feeds minute-long audio to 3DSpeaker).
**Rationale**: The clustering already maintains accurate duration-weighted centroids. Re-extracting from merged segments produces garbage because 3DSpeaker expects ~2-5s windows, not 150s.

### D5: Short-speaker noise merge

**Choice**: After clustering, speakers with total duration below `MIN_CLUSTER_FRAC × total_audio` (default 2%) are merged into their cosine-nearest larger cluster. The absolute floor is `MIN_SPEECH_SECS` (1.5s) — the model's own minimum embedding input.
**Alternatives**: No merge (leaves noise speakers); fixed absolute threshold.
**Rationale**: Clustering at threshold 0.40 produces noise speakers (1-3s artifacts from boundary effects). These should not appear as separate speakers. The fraction-based threshold scales with meeting length.

### D6: Retroactive labeling with embedding-based cross-meeting matching

**Choice**: Users label "Speaker 0" → "Alice" inline in the transcript view. The centroid embedding from clustering is already stored in `speaker_embeddings`. Future meetings auto-match if cosine similarity exceeds the threshold.
**Alternatives**: Guided enrollment; no cross-meeting matching.
**Rationale**: Zero upfront friction. The system gets smarter as users label more speakers.

### D7: Embedding dimension is model-dependent, not hardcoded

**Choice**: `speaker_embeddings` accepts any dimension in range [64, 1024]. The 3DSpeaker model on the current system outputs 512-dim embeddings, not 256-dim.
**Alternatives**: Hardcode 256-dim (rejected — rejected valid centroids).
**Rationale**: Different model versions and model types produce different embedding dimensions. Storage is self-describing (blob size / 4 = dim).

### D8: Per-speaker persistent colors

**Choice**: Each speaker in the `speakers` table has a `color` field. Assigned from a fixed palette when the speaker is first created. The color is used consistently across all meetings.
**Alternatives**: Fixed palette by position (Speaker 0 always blue).
**Rationale**: Named speakers should look the same in every meeting. Alice is always teal.

### D9: Model selection

**Embedding model (single, hardcoded)**:
- `nemo_titanet` — NeMo Titanet Small EN VoxCeleb (~40 MB, 192-dim) — the only shipped embedding model.

Empirical comparison on meeting 95db (3-speaker ES/EN meeting) showed nemo_titanet small ties nemo_titanet_large on quality (identical speaker counts and acceptance at threshold 0.65 and 1.0) at less than half the model size (40 MB vs 96 MB), and is more robust than eres2net (3 vs 4 speakers at threshold 0.65 without the max_speakers cap). It is the centrist on per-segment agreement: it agrees with both other models more often than they agree with each other. 3dspeaker (the prior default) is trained on Mandarin and was the wrong choice for an ES/EN/CA user.

Speaker counting is handled by the clustering step (AHC with a cosine-similarity threshold), not the embedding model — the model is a feature extractor. The threshold and max_speakers settings are the speaker-count mechanism and must remain configurable. Spectral eigengap / silhouette / BIC auto-count was explored and rejected — see D13.

**Segmentation model (required)**:
- Pyannote segmentation model (~6 MB) — always downloaded during onboarding

### D10: Single-speaker handling

**Choice**: Always run diarization and label, even for 1 speaker. Useful for matching against the speaker registry ("this is Alice"). Single-speaker false positives (split into 2) are visible to the user and can be corrected.

### D11: Merge threshold calibrated at 0.40

**Choice**: Default merge threshold is 0.40 (range [0.40, 0.80]). Empirically calibrated: 0.45 produces 10 speakers, 0.40 produces 3 main + 4 noise (handled by short-speaker merge).
**Rationale**: The original 0.60 default was too high, producing 53 speakers from a 3-speaker meeting.

### D12: max_speakers enforcement via most-isolated-cluster merging

**Choice**: When the cluster count exceeds the `max_speakers` setting, merge the most isolated cluster (lowest nearest-neighbour centroid similarity) into its nearest neighbour — not the highest-similarity pair.

**Rationale**: Two real speakers who sound alike can have higher centroid similarity than a noise/outlier cluster has to any speaker. Merging the highest-similarity pair collapses those two real speakers together, destroying separation. Merging the most isolated cluster absorbs the outlier without touching well-separated speakers. Since the most-similar pair always has a higher nn-sim than any other cluster, the real speakers are guaranteed to survive.

**Empirical validation**: In a 3-speaker Spanish meeting at threshold 0.65, 4 clusters survive short-speaker merge. The two real speakers have centroid sim 0.473 (highest pair); the noise cluster has nn sim 0.327 (lowest). Old strategy merged the two real speakers; new strategy merges the noise cluster, preserving Speaker 1 / Speaker 2 separation.

### D13: Auto-count via spectral eigengap / silhouette / BIC — rejected

**Choice**: Speaker counting stays in the AHC + cosine-threshold + max_speakers pipeline. Spectral eigengap, silhouette, Davies-Bouldin, and BIC auto-count methods were explored (2026-06-15) and rejected.

**Rationale**: On meeting 95db (350 chunks, 192-dim nemo_titanet embeddings), all methods converge on k=4 — the structural truth (3 speakers + 1 short-duration noise cluster), not the desired k=3. Dense eigengap → k=1 (98% of cosine pairs positive, graph fully connected). kNN(10) eigengap → k=4 (stable across k=3..50). Silhouette and Davies-Bouldin both peak at k=4. BIC+PCA-20 picks k=3 but only in a narrow PCA range (fragile). ECAPA-TDNN (SOTA AAM-Softmax verification model) produces the same 4-cluster structure with nearly identical k=3 cluster sizes ([94, 136, 120] vs nemo's [93, 137, 120]) — the 4th cluster is in the audio data, not the embedding model.

The 4→3 reduction is handled by merge_short_speakers (D5, 2% duration floor), which absorbs the short-duration noise cluster. This is the correct mechanism: it is principled (duration is observable), robust (no fragile matrix operations), and transparent (the user sees the merge in speaker settings).

## Risks / Trade-offs

**[Cold-start]** → First N meetings have no auto-matching value. Users must manually label speakers. Mitigated by inline labeling being low-friction (click badge → type name).

**[Cross-meeting accuracy]** → Embedding matching across different microphones, rooms, and days is unproven. False positives are trust-destroying for summarization. Mitigated by configurable threshold. Default threshold is conservative (0.40).

**[Proportional split for Parakeet]** → When Parakeet is the transcription provider, token timestamps are unavailable. Alignment falls back to segment-level proportional split, which can split mid-sentence. Mitigated by documenting as a known limitation and recommending Whisper for best diarization results.

**[Download size]** → Adding ~50 MB (segmentation) + 26–90 MB (embedding) to onboarding downloads. Mitigated by being optional — diarization models can be downloaded separately if user skips during onboarding.

**[Overlapping speech]** → Diarization assigns overlapping speech to one speaker. This is a fundamental limitation of single-channel diarization. Documented as known limitation.

**[Debug-mode performance]** → Debug builds take ~8 min for a 30-min meeting. Release mode estimated at 3-4 min. Mitigated by release-mode production builds.

## Migration Plan

1. **DB migration**: Add `speaker TEXT`, `token_timestamps TEXT`, `speaker_source TEXT` columns to `transcripts` table (all nullable). Create `speakers` and `speaker_embeddings` tables.
2. **Queue migration**: Extend `JobPhase` enum with `Diarizing` variant. Existing jobs with `Transcribing`/`Summarising` phases continue to work — diarization is only triggered for new recordings.
3. **Model download**: Add segmentation + embedding model download to onboarding Step 3. Graceful fallback if download fails — diarization phase is skipped and a warning is logged.
4. **Rollback**: Remove `sherpa-onnx` dependency, revert queue changes, drop new DB columns/tables. Existing recordings are unaffected (speaker columns are nullable).

## Resolved Open Questions

- Default confidence threshold: **0.40** (empirically calibrated, range [0.35, 0.70]).
- Speaker count cap: **global setting**.
- Color palette: **golden-angle HSL** (`hue = index × 137.508 mod 360`), not a fixed 10-color palette.
- Embedding dimension: **model-dependent** (3DSpeaker outputs 512-dim), accepted range [64, 1024].
- Re-diarization embedding cleanup: **delete old embeddings** for the meeting before re-running. Previous runs left stale centroids that polluted cross-meeting matching and prevented un-merging.
- Inline suggestion chips: **merge action**, not rename. Selecting an existing speaker from suggestions merges the cluster into that speaker. To un-merge, re-diarize (which deletes stale embeddings).
- Sherpa-ONNX FFI safety: **validate model paths in Rust** before reaching C++ code. Sherpa-onnx C++ throws uncatchable exceptions on invalid model files, crashing the Rust process. All model paths are validated with `Path::exists()` before constructing the C++ objects.

## Context

Meetily records multi-participant meetings but produces speaker-anonymous transcripts. The current pipeline is: VAD → Whisper → transcript segments stored in SQLite. There is no speaker attribution at any stage. Dead `pyannote` code in `stt.rs` references embedding extraction but was never integrated.

The transcription queue already supports phase chaining (`Transcribing` → `Summarising`) via `JobResult::CompletedChain`. The Whisper provider already sets `set_token_timestamps(true)` but discards the per-token timing data.

The `sherpa-onnx` crate (v1.13.2, Apache-2.0) provides official Rust bindings for `OfflineSpeakerDiarization`, `SpeakerEmbeddingExtractor`, and `SpeakerEmbeddingManager`. Auto-downloads prebuilt static libraries for all target platforms. Thread-safe (`Send + Sync`).

## Goals / Non-Goals

**Goals:**
- Offline speaker diarization as a post-processing queue phase, chained after summarisation
- Token-level timestamp alignment between Whisper segments and diarization speaker boundaries
- Cross-meeting speaker identification via embedding matching with confidence tiers
- Retroactive speaker labeling with persistent per-speaker colors
- User-selectable embedding models (3dspeaker/wespeaker) and required pyannote segmentation model
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
**Rationale**: Official crate is maintained by the k2-fsa project, provides `OfflineSpeakerDiarization` + `SpeakerEmbeddingExtractor` + `SpeakerEmbeddingManager` with full Rust API, thread-safe, static linking, cross-platform.

### D2: Offline-only diarization as a queue phase

**Choice**: Add `Diarizing` phase to the existing queue, after `Summarising` (or directly after `Transcribing` if no summary provider).
**Alternatives**: Online embedding during recording; standalone post-processing outside the queue.
**Rationale**: Queue phase chaining already exists (`JobResult::CompletedChain`). Diarization runs after transcription completes so it can align against stored transcript segments. Zero CPU overhead during recording.

### D3: Token-level timestamps for alignment

**Choice**: Extract and store per-word timestamps from Whisper during transcription. Use them to assign each word to a diarization speaker based on timestamp overlap.
**Alternatives**: Proportional time-split (splits text at arbitrary points); re-transcribe sub-segments (expensive).
**Rationale**: Whisper already generates token timestamps (`set_token_timestamps(true)`). Storing them adds ~200 bytes per segment. Word-level alignment produces natural sentence splits at speaker boundaries.

**Fallback**: For Parakeet (no token timestamps), use segment-level timestamps with proportional text-split as a degraded mode.

### D4: Retroactive labeling with embedding-based cross-meeting matching

**Choice**: Users label "Speaker 1" → "Alice" inline in the transcript view. The system extracts the average embedding for that speaker cluster from the diarization results and stores it in `speaker_embeddings`. Future meetings auto-match if cosine similarity exceeds the threshold.
**Alternatives**: Guided enrollment; no cross-meeting matching.
**Rationale**: Zero upfront friction. The system gets smarter as users label more speakers. False-positive risk mitigated by confidence tiers.

### D5: Confidence tiers for cross-meeting matching

**Choice**: High-confidence matches (> threshold) show the speaker name directly. Lower-confidence matches (0.5–threshold) show "Unknown Speaker (possibly Alice)". Below 0.5: "Unknown Speaker".
**Alternatives**: Binary threshold; always show best match.
**Rationale**: Prevents confident false attribution for summarization. Users can verify suggestions. Threshold is configurable in advanced settings.

### D6: Average embedding per speaker per meeting

**Choice**: Store one average embedding per speaker per meeting in `speaker_embeddings` table. Cross-meeting matching uses `SpeakerEmbeddingManager::search()` against all stored embeddings.
**Alternatives**: Store every per-segment embedding (~50 per meeting).
**Rationale**: Sufficient for matching with the official `SpeakerEmbeddingManager`. Lower storage overhead (~5 KB per meeting vs ~50 KB).

### D7: Embedding-based speaker ID stability across re-diarizations

**Choice**: When re-diarizing, match new speaker clusters against existing labeled speakers by embedding similarity. If a cluster matches "Alice", it keeps the "Alice" label.
**Alternatives**: Numeric labels that reset on re-diarization.
**Rationale**: Prevents user corrections from being lost. Labels are tied to voice identity, not arbitrary numbering.

### D8: Per-speaker persistent colors

**Choice**: Each speaker in the `speakers` table has a `color` field. Assigned from a fixed palette when the speaker is first created. The color is used consistently across all meetings.
**Alternatives**: Fixed palette by position (Speaker 1 always blue).
**Rationale**: Named speakers should look the same in every meeting. Alice is always teal.

### D9: Model selection

**Embedding models (user-selectable)**:
- `3dspeaker_speech_campplus_sv_zh-cn_16k-common` (~26 MB, 256-dim) — default, multilingual
- `wespeaker_zh_cnce_resnet` (~90 MB, 256-dim) — higher accuracy option

**Segmentation model (required)**:
- Pyannote segmentation model (~50 MB) — always downloaded during onboarding

### D10: Single-speaker handling

**Choice**: Always run diarization and label, even for 1 speaker. Useful for matching against the speaker registry ("this is Alice"). Single-speaker false positives (split into 2) are visible to the user and can be corrected.

## Risks / Trade-offs

**[Cold-start]** → First N meetings have no auto-matching value. Users must manually label speakers. Mitigated by inline labeling being low-friction (click badge → type name).

**[Cross-meeting accuracy]** → Embedding matching across different microphones, rooms, and days is unproven. False positives are trust-destroying for summarization. Mitigated by confidence tiers and configurable threshold. Default threshold is conservative (0.6).

**[Proportional split for Parakeet]** → When Parakeet is the transcription provider, token timestamps are unavailable. Alignment falls back to segment-level proportional split, which can split mid-sentence. Mitigated by documenting as a known limitation and recommending Whisper for best diarization results.

**[Download size]** → Adding ~50 MB (segmentation) + 26–90 MB (embedding) to onboarding downloads. Mitigated by being optional — diarization models can be downloaded separately if user skips during onboarding.

**[Overlapping speech]** → Diarization assigns overlapping speech to one speaker. This is a fundamental limitation of single-channel diarization. Documented as known limitation.

## Migration Plan

1. **DB migration**: Add `speaker TEXT`, `token_timestamps TEXT`, `speaker_source TEXT` columns to `transcripts` table (all nullable). Create `speakers` and `speaker_embeddings` tables.
2. **Queue migration**: Extend `JobPhase` enum with `Diarizing` variant. Existing jobs with `Transcribing`/`Summarising` phases continue to work — diarization is only triggered for new recordings.
3. **Model download**: Add segmentation + embedding model download to onboarding Step 3. Graceful fallback if download fails — diarization phase is skipped and a warning is logged.
4. **Rollback**: Remove `sherpa-onnx` dependency, revert queue changes, drop new DB columns/tables. Existing recordings are unaffected (speaker columns are nullable).

## Open Questions

- Default confidence threshold value (tentatively 0.6 — needs empirical tuning).
- Whether to expose speaker count cap as a global setting or per-recording setting (tentatively global).
- Exact color palette for persistent speaker colors (tentatively a 10-color categorical palette).

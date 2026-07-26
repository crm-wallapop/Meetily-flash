## Context

The diarization emits speaker boundaries only where the uniform chunk grid (`SPLIT_TARGET_SECS = 3.0`, `FINE_SPLIT_SECS = 2.0`) changes AHC label. Rapid back-and-forth within one grid cell is collapsed into a single run. On `meeting-cde5c264-…`, the fine diarization for the 5.7–32.5 s window contains a single boundary at 21.36 s despite the transcript reading as multi-turn dialogue.

The boundary source is unused. `sherpa_adapter.rs:103-106` accepts `segmentation_model_path`, existence-checks the file, and never passes it to any sherpa-onnx config. sherpa-onnx 1.13.2 ships `offline_speaker_diarization.rs` — a pyannote-backed diarizer whose segmentation model runs at ~100 ms frame resolution. The canonical `post-meeting-pipeline` spec names `OfflineSpeakerDiarization::process(samples)` at step 3 (`spec.md:290`); the implementation only builds a `SpeakerEmbeddingExtractor`.

API facts (verified against `sherpa-onnx-1.13.2/src/offline_speaker_diarization.rs`):
- `OfflineSpeakerDiarizationConfig { segmentation, embedding, clustering: FastClusteringConfig, min_duration_on, min_duration_off }` (L88-93).
- `process(samples: &[f32]) -> Vec<OfflineSpeakerDiarizationSegment { start, end, speaker }>` (L128-132, L169). The returned segments are **post-clustering** — adjacent same-label regions are coalesced before the label is returned.
- `FastClusteringConfig { num_clusters, threshold }` (L63-65). `threshold` is a cosine-similarity merge cutoff.
- `sample_rate()` (L157) returns the model rate (16 kHz for pyannote-3.0).
- No segmentation-only FFI entry point exists (exhaustive scan of `sherpa-onnx-sys-1.13.2/src/lib.rs` and the FFI layer) — `process()` always runs segmentation + embedding + clustering.

Dependency: this change assumes `diarization-speaker-split-persistence` is archived (the corrected persist path is needed for the extra N>1 splits finer boundaries produce).

## Goals / Non-Goals

**Goals:**
- Source speaker change-point boundaries from pyannote's segmentation model (via sherpa's `OfflineSpeakerDiarization`) so rapid turns produce boundaries the chunk grid cannot.
- Preserve Meetily's label-quality machinery (AHC, cap, temporal-coherence smoothing, `refine_pass2`, registry) — pyannote supplies boundaries only, not labels.
- Close the phantom-dependency state: the segmentation model is genuinely consumed.
- Respect the existing `MAX_DIARIZATION_CHUNKS` perf cap without preferentially stripping turn boundaries on long meetings.

**Non-Goals:**
- A segmentation-only FFI fast path (none exists; the 2× embedding cost is accepted).
- Adopting `pyannote-rs` (rejected — design §Alternatives).
- Sub-`min_duration_on` (0.3 s) segmentation.
- Per-word alignment (handled by the companion `diarization-speaker-split-persistence` change).

## Decisions

**D1 — `OfflineSpeakerDiarization` as a boundary oracle, with `FastClusteringConfig { num_clusters: -1, threshold: 0.0 }` and `min_duration_on: 0.0`, `min_duration_off: 0.0`.** Run sherpa's diarizer on the 16 kHz mono samples; take the `(start, end)` of each returned segment as a boundary; discard the `speaker` field. Meetily's AHC then clusters embeddings on the pyannote-bounded sub-segments.

The threshold/duration settings are load-bearing and their semantics are non-obvious (a panel falsified the design's original `threshold: 1.0`, which had the similarity/dissimilarity direction backwards). In sherpa-onnx, `FastClusteringConfig.threshold` is a cosine-**dissimilarity** (distance) cutoff — `distance = max(0, 1 - cosine_similarity)` — where **smaller threshold → more clusters** (confirmed by the official source: *"a larger threshold leads to few clusters… a smaller threshold leads to more clusters"*). So:
- `threshold: 0.0` → merge only when distance ≤ 0 (cosine_similarity ≥ 1.0, i.e. identical embeddings) → **maximally fragmented**, every embedding-distinct region separate.
- `threshold: 1.0` (the rejected value) → merge when cosine_similarity ≥ 0.0 → **over-merging**, worse than the status quo.
- `num_clusters: -1` lets the threshold govern.

`process()` is NOT raw segmentation — it applies three post-clustering steps regardless of config: (a) ReLabel (per-chunk indices → cluster labels), (b) ComputeResult (contiguous same-cluster runs emitted as single segments), (c) MergeSegments (adjacent same-speaker segments with gap < `min_duration_off` merged). To stop these from collapsing the fine boundaries, `min_duration_off: 0.0` disables the gap-merge and `min_duration_on: 0.0` stops short-segment dropping. With `threshold: 0.0`, nearly every chunk is its own cluster, so the contiguous-same-cluster merge (b) rarely fires. The result is post-ReLabel but maximally fragmented — the closest available approximation to the segmentation model's ~100 ms change-point output, and strictly finer than the 2–3 s chunk grid.

**Consequence (drives D3):** at this setting, pyannote boundaries are **dense candidate splits** (~100 ms resolution), most of which are within-speaker embedding variation, NOT speaker turns. Individual boundaries are not turns; turns are re-derived downstream by Meetily's AHC + temporal-coherence smoothing + the `MIN_SPEECH_SECS` floor (all unchanged). This is why uniform cap-shedding (D3) is acceptable where it would not be if each boundary were a turn.

**D2 — Pre-splitter runs inside `process()`, before `build_chunks`.** The pyannote boundary set is intersected with the transcript-segment speech regions to form the chunk layout, so Meetily computes embeddings on pyannote-bounded sub-segments rather than uniform 3 s windows. sherpa consumes the existing 48 kHz→16 kHz resample buffer (already produced at `commands.rs:343-354` and reused by `refine_pass2` at `sherpa_adapter.rs:401`) — no second decode. `min_duration_on` follows sherpa's 0.3 s default; this is sherpa's own speech-duration filter and is **distinct** from Meetily's `MIN_SPEECH_SECS = 1.5` floor, which D3 enforces. The phantom constructor load-or-skip (`sherpa_adapter.rs:103-106`) is removed — pyannote is now genuinely consumed.

**D3 — Uniform shed-to-cap, BEFORE embedding extraction; replaces `build_chunks`'s grid subdivision.** At D1's settings pyannote emits dense candidate splits (≈100 ms), most of which are within-speaker variation, not turns. When the candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS`, shed uniformly (every k-th boundary by position) down to the cap, **before** embeddings are extracted — so the 2× embedding cost (D1) is paid only on the capped set, not on thousands of raw candidates. Then sub-`MIN_SPEECH_SECS` survivors are merged into their time-neighbor.

Three points resolve the shark-tank's decimation blockers:

- **Why uniform shedding is acceptable (resolves the "self-defeating" B1).** Because boundaries are candidate splits, not turns, shedding a candidate in an alternation region does not destroy a turn — it lowers the resolution there. The turn itself is re-derived by Meetily's AHC + temporal-coherence smoothing from the surviving candidates. The prior concern (that index-shedding deletes B→A transitions and merges speakers) assumed each boundary carried a turn; under D1's dense-candidate model it does not. The cde5c264 oracle (tasks) is the empirical gate: if the AHC+smoothing cannot recover the turns from the shed candidate set, the design hypothesis fails and a smarter (embedding-delta-weighted) shedding policy is the fallback.
- **Reconciling `build_chunks`'s own cap (resolves B4).** `build_chunks` (`sherpa_adapter.rs:331`) today subdivides each transcript segment by `effective_split = max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)`. Part B REPLACES that uniform-grid subdivision with pyannote-boundary subdivision, and the `MAX_DIARIZATION_CHUNKS` cap is enforced ONCE, at the pyannote-boundary layer (D3's shed). `build_chunks` no longer applies its own `effective_split` grid; it consumes the pre-capped pyannote chunks. This avoids a double-decimation that would leave the cap unenforced.
- **Effective turn floor (resolves the floor overclaim).** The floor for a *persisted* turn is `MIN_SPEECH_SECS` (1.5 s) as enforced by Meetily's chunking/smoothing, NOT pyannote's `min_duration_on`. `min_duration_on: 0.0` (D1) only stops sherpa from dropping segments internally; Meetily's own floor still governs what becomes a speaker row.

**D4 — Pyannote labels are discarded; Meetily's AHC + cap + smoothing + `refine_pass2` + registry are authoritative.** No string from sherpa's diarization label namespace is written to `speaker_embeddings`. The pre-splitter changes boundary *placement* only.

## Alternatives (rejected, with evidence)

- **Full swap to `OfflineSpeakerDiarization` for both boundaries AND labels.** Regresses `refine_pass2` label-quality, the most-isolated-cluster cap, temporal-coherence smoothing, and cross-meeting registry matching (all bolted to nemo_titanet via `SpeakerEmbeddingExtractor`). Rejected.
- **Reduce `FINE_SPLIT_SECS` (2 s → finer).** Window-reduction disproven (project memory): between-speaker cosine collapses ~0.6→~0.8 at finer windows, defeating `SMOOTH_SELF_WEIGHT=0.6`. Rejected.
- **`pyannote-rs` crate (crates.io v0.1.2).** Unnecessary dependency — sherpa-onnx (already pinned) ships the same pyannote-backed diarization; v0.1.2 carries no semver guarantees and adds a third-party supply-chain surface. (The "different embedding model breaks the registry" argument does NOT apply here, since Part B discards all diarizer labels regardless — the real reasons are the unnecessary dependency and the semver/supply-chain risk.) Rejected.
- **Acoustic change-point detection (BIC/KL2).** Re-implements, weaker, what pyannote already does. Hold only if the sherpa plumbing proves invasive.

## Risks / Trade-offs

- **[2× embedding extraction]** sherpa's internal pass + Meetily's AHC on the finer chunks. No segmentation-only FFI exists to avoid it. Mitigated by D3 shedding to the cap BEFORE Meetily's embedding pass, so Meetily embeds only the capped set. Gated by a perf test on cde5c264; the cached-AHC bound holds for the clustering half. Open Q1.
- **[Over-segmentation is intended, not a defect]** At `threshold: 0.0` sherpa returns maximally fragmented regions; this is the goal (dense candidate splits). Meetily's AHC + temporal-coherence smoothing re-coalesces same-speaker runs and recovers turns — that is the load-bearing design hypothesis. If the cde5c264 oracle shows the AHC+smoothing CANNOT recover turns from the shed candidate set, the fallback is an embedding-delta-weighted (smarter) shedding policy that keeps high-delta boundaries preferentially. This is the primary residual risk.
- **[Long-meeting resolution loss]** Uniform shed-to-cap lowers per-region resolution on long meetings; acceptable because turns are re-derived downstream, but the long-meeting adversarial test gates that resolution loss does not collapse the alternation region's turn structure.
- **[Cargo caret `1.13` permits 1.13.3]** The `offline_speaker_diarization.rs` API surface is byte-identical between 1.13.2 and 1.13.3 (verified by the panel); no pin tightening required.

## Migration Plan

Code-only: adapter pre-splitter + constructor cleanup + region-fair decimation. No schema migration. Rollback = revert code. Existing meetings keep prior (coarse) boundaries until re-diarized. The spec delta records the pyannote boundary source + the consumption requirement.

## Security Model

Pyannote boundaries are derived from the audio signal, not from any untrusted text field. No new untrusted input surface. The pre-splitter changes boundary placement only; downstream persistence (sqlx `?` binding, opaque text) is unchanged from the companion change.

## Adversarial Tests (§4)

cde5c264 boundary oracle — **two windows**: (a) the Ricardo interjection at ≈46:58 and (b) the actual complaint window 5.7–32.5 s; in each, a boundary the chunk-grid-only baseline misses is present in the pre-splitter output (assert the SPECIFIC boundary exists in pre-splitter output and is absent in baseline, not a window-wide count delta — a count delta could coincidentally align); end-to-end persistence oracle — the pipeline WITH Part B (on top of Part A) persists strictly more speaker-split rows for the complaint window than the chunk-grid baseline; single-speaker meeting not fragmented; **sherpa-label isolation via deterministic equivalence** — run the pipeline with sherpa's `speaker` field populated vs zeroed; assert the resulting `speaker_embeddings` registry is byte-identical (the `speaker` field is an integer, not a string namespace, so a "no string leaks" assertion is non-falsifiable — deterministic equivalence is); chunk count ≤ `MAX_DIARIZATION_CHUNKS` after D3's shed (including at-cap off-by-one: exactly 600 vs 601 candidate boundaries); **uniform-shed turn-recovery** — the load-bearing hypothesis test: a long (≥45 min) two-speaker rapid-alternation fixture sheds to the cap, and Meetily's AHC+smoothing still recovers the alternation turn structure (assert ≥ a threshold fraction of within-region turns are recovered; if this fails, the fallback is embedding-delta-weighted shedding); **adversarial audio** — noise/music that triggers many false change-points at `threshold: 0.0` does not collapse the meeting to one speaker (AHC+smoothing rejects the noise fragments); stress (candidate boundaries ≈10× cap) sheds without OOM; silent/empty audio returns empty diarization without panicking; perf gate on cde5c264 (sub-budget wall-clock, modulo the one-time pyannote decode + 2× embedding); **phantom-model consumption** — swap the segmentation model for a committed dummy fixture and assert a specific, deterministic behavior change (construction error or distinct segmentation output — not a flaky filesystem mutation; mark `#[ignore]` if it needs the real model).

> **Smoke carve-out.** Part B touches zero frontend code (pure Rust adapter change). Per CLAUDE.md §3 smoke is mandated for user-visible *frontend* behavior; more speaker badges are a downstream consequence but flow through Part A's already-smoke-tested persist path. No separate smoke spec for Part B.

## Open Questions

1. **Re-decode avoidance / 2× embedding cost.** Is the 2× embedding cost acceptable on a 60-min meeting, or does `process()` need to be called on a downmixed/subset buffer? Resolve at D1 implementation; the perf gate is the decision point.
2. **Two-pass smart decimation.** Would a second pyannote pass at a meaningful threshold (for labels only) enable within-run-first decimation that preserves turn boundaries better than region-fair? Cost vs. fairness tradeoff; defer unless the region-fair adversarial test reveals unsatisfactory preservation on real long meetings.

## Context

Speaker diarization (`speaker-diarization` capability) runs as a post-transcription queue phase: it carves audio chunks from Whisper segment boundaries, embeds them (nemo_titanet, 192-dim), clusters (greedy duration-weighted AHC), applies temporal-coherence smoothing, coalesces same-label runs, merges short speakers, enforces a speaker cap, then aligns transcript rows to the resulting speaker segments.

Two user-visible label-quality failures were verified by listening to meeting `cde5c264` (3 speakers, 83 min), after the "absorption" thread was closed as a diagnostic artifact:

- **Facet 1 — multi-speaker Whisper windows collapse.** Whisper segments are 22–28s (179/238 rows > 15s) and span multiple speakers. Diarization chunks are carved at `effective_split = max(speech_seconds/600, 3.0)` ≈ 8s for this meeting, then coalesced into ~50s runs, so a multi-speaker segment falls inside one run and gets one label. `[46:58]` Ricardo's 2s interjection is swallowed into a Cynthia run.
- **Facet 2 — short chunks mis-attribute to absent speakers.** `[0:01]` "Hello" (1.4s) is labeled Ricardo, who joins at 17:37.

A compounding defect defeats the spec's existing per-word alignment: `token_timestamps` is NULL for every row. `extract_token_timestamps` (`token_timestamps.rs:5`) is fully implemented but never called, and every `INSERT INTO transcripts` (`import.rs`, `retranscription.rs`, `transcript.rs`) omits the column — so `align_with_tokens` (`alignment.rs:103`) never runs and everything falls back to `align_proportional`, which sees one speaker per ~50s run.

Three read-only spikes (logged in `openspec/exploration/diarization-label-quality.md`) established and then validated the design:

1. **Embedding spike** — finer 2s windows isolate the `[46:58]` interjection cleanly (cos_ric=0.57 at 2s vs cos_cyn=0.61 at 8s). The mixed pre-mixed channel does NOT dominate at 2s.
2. **Full-pipeline spike** — with correct per-chunk labels, smoothing preserves fine labels (only 6/105 chunks flipped; the interjection survives as a 20s run). BUT real AHC @0.65 on 2s chunks **fragments** (56 clusters from 105 chunks) because fine embeddings are noisier and pairwise cosine drops below threshold. Two-pass sidesteps this.
3. **Resolving spike (actual two-pass, end-to-end)** — ran the real two-pass on the full meeting: `[46:42–47:00]` resolves as an 18s Ricardo segment (production swallows `[46:58]`); all three speakers' meeting-derived centroids are separable (Cynthia cos 0.925, Ricardo cos 0.896, Carlos well-separated); lowering the AHC threshold instead still fragments (0.40→11 clusters on 2s). Surfaced that Pass-2 centroids must come from the post-`merge_short_speakers`/post-`enforce_max_speakers_cap` speaker set, not raw AHC (30 raw clusters, 3 real).

## Goals / Non-Goals

**Goals:**
- Resolve facet 1: multi-speaker Whisper segments get per-turn speaker boundaries (verified target: `[46:58]`; `[0:05]` is an expected beneficiary but was outside both spiked regions and is not claimed as verified).
- Resolve facet 2: short chunks can't take labels of temporally-absent speakers (verified target: `[0:01]`).
- Activate the existing token-level alignment requirement by populating `token_timestamps`.

**Non-Goals:**
- Source separation of the pre-mixed remote channel (the meeting platform mixes Cynthia+Ricardo before capture; no diarization decision can un-mix them — accepted).
- Perfect resolution of sub-2s overlaps (e.g. `[47:32]` short exclamations inside a dominant-speaker 2s window). **Accepted limitation** — token-level alignment distributes words *within* a diarization segment and cannot create a speaker split where diarization found no boundary, so wiring `token_timestamps` does not recover `[47:32]` either.
- Changing the AHC threshold or clustering algorithm for the coarse pass (it stays at 0.65; Pass 2 does not re-cluster).
- Any change to `DiarizationPort` or cross-meeting matching.

## Decisions

### D1 — Two-pass coarse→fine re-labeling (not single-pass finer AHC)

Pass 1 is the **existing** `process()` pipeline unchanged (coarse chunks at `effective_split`, threshold 0.65 → cluster → smooth → coalesce → `merge_short_speakers`). `enforce_max_speakers_cap` then runs in `commands.rs` exactly as today, producing the **final speaker set** and its centroids — the only centroid set that is guaranteed capped, so it is what Pass 2 must assign against. Pass 2 is a **new adapter method** `refine_pass2(samples, sample_rate, final_centroids)`, invoked from `commands.rs` immediately after the cap. It re-chunks the audio **uniformly** at `FINE_SPLIT_SECS` (~2s) — independent of Whisper segment boundaries, unlike `build_chunks`'s `MAX_CHUNK_SECS`-respecting step (D2) — embeds each fine chunk, assigns it to its **nearest final centroid** (no second AHC; tie-break by temporal predecessor, D3), then runs the existing smoothing + coalescing + min-segment-floor on the fine labels.

**Placement rationale (resolves the round-2 architectural review).** `enforce_max_speakers_cap` is deliberately outside `process()` (`sherpa_adapter.rs:213`: smoothing "runs inside `process()` so `enforce_max_speakers_cap` judges isolation on these de-contaminated centroids") because the cap needs the per-meeting `max_speakers` resolved via an async DB query the sync adapter cannot perform. Moving the cap into `process()` would require threading that config into the adapter and reversing a deliberate decision. Pass 2 therefore runs **after** the cap in the orchestration layer (`commands.rs`), calling the adapter only for the embedding/assignment heavy lifting. `process()` and `DiarizationPort` are unchanged. `refine_pass2` lives on the concrete `SherpaOnnxDiarizationAdapter` (single implementor; not promoted to the port trait pending the deferred `hexagonal-port-traits` change). The cap is **not** re-run after Pass 2: nearest-centroid assignment can only pick labels from the final centroid set, so the speaker count cannot exceed the capped set (an invariant test pins this).

**Why over alternatives:**
- *Lower the AHC threshold for fine chunks* (e.g. 0.40–0.45): **rejected, now spike-tested.** The resolving spike swept 0.40/0.45/0.50/0.55/0.65 on 2s chunks: even 0.40 yields 11 clusters (vs 2–3 needed), 0.45→15, 0.50→20. The threshold is chunk-size-dependent and no stable fine-grained threshold recovers the speaker count. Two-pass avoids re-clustering entirely.
- *pyannote turn-aware segmentation* (the archived `diarization-segmentation-windows` Path B): **rejected for this metric.** Two-pass already resolves the primary `[46:58]` case; pyannote adds a model pass + the ORT-crate collision that forced Path B's rewrite, for no gain on the verified target.
- *Display-layer split only*: **rejected.** Useful only after diarization segments are already finer than Whisper segments; it doesn't create the boundaries.

Nearest-centroid assignment is validated end-to-end by the resolving spike: the actual two-pass resolves `[46:42–47:00]` as an 18s Ricardo segment, and all three speakers' meeting-derived centroids are separable (Cynthia cos 0.925, Ricardo cos 0.896, Carlos well-separated from both validated references). A second spike assigning to only the top-3 post-cap centroids (the configured design) confirms `[46:42–47:02]` still resolves as Ricardo with a *lower* flip rate (12% vs 20% with raw centroids) and zero noise singletons.

### D2 — Pass 2 granularity 2s, uniform across the full audio

`FINE_SPLIT_SECS = 2.0`. Pass 2 chunks the audio **uniformly** at this interval across the entire recording, independent of Whisper segment boundaries — distinct from the coarse `build_chunks` step, which respects `MAX_CHUNK_SECS` (10s) and leaves segments ≤10s as single unsplittable chunk. A new `build_fine_chunks` helper implements this uniform split; the spike used uniform windows and production must match, or ~25% of segments (those ≤10s) would never be sub-divided and the granularity requirement would silently fail for them. Spike showed 2s isolates the target interjection; 1.5s is marginally crisper at ~33% more compute. 2s is the sweet spot. Exposed as a constant (not user-facing) so it can be tuned per-meeting later if needed.

### D3 — Tie-break fine chunks by temporal predecessor

A fine chunk equidistant between two coarse centroids (or below a confidence margin) takes the label of its temporal predecessor, not an arbitrary centroid. Prevents flicker at ambiguous boundaries; the smoothing pass then cleans residual noise.

### D4 — Token wiring reuses the existing extract function

`extract_token_timestamps(state, num_segments)` is fully implemented (centiseconds→ms, skips special tokens, validates). Wire it into the Whisper result→DB save path and add `token_timestamps` to the three INSERT sites. The column already exists (migration 20260527); existing NULL rows fall back to proportional alignment unchanged. No new infrastructure.

### D5 — Facet 2 temporal-presence constraint, end of `refine_pass2`

Runs as the last step of `refine_pass2`, after smoothing + coalescing + min-segment-floor, on the fine labels. Scan for chunks shorter than `MIN_PRESENCE_SECS` (default 2.0s — ≥ the `[0:01]` "Hello" 1.4s case) whose label has **no other same-label segment within `PRESENCE_WINDOW_SECS` (default 30.0s) on either side** (symmetric ±W, edge-clipped at the recording boundary). Relabel such orphans to the dominant speaker in the window. `merge_short_speakers` is NOT re-run afterward: Pass 2 only assigns to existing final centroids, and relabeling an orphan to a present speaker cannot create a new speaker, so the cap and short-speaker invariants still hold.

Distinct from `enforce_min_segment_floor`: the floor collapses only sub-10s runs whose **two neighbors share a label**; a singleton with a gap on both sides (the `[0:01]` case) escapes it because it has no same-label neighborhood to vote it back. Also distinct from the "genuine interjection preserved" property: a short Ricardo chunk between Cynthia (left) and Carlos (right) is preserved **only if Ricardo has other support within ±W**; if it is Ricardo's only chunk in the window it IS an orphan and IS relabeled (the spec scenario states this nearby-support condition). The window values are un-validated defaults pending the real-audio oracle (task 5.1); the `[0:01]` case requires `MIN_PRESENCE_SECS` ≥ 1.4s.

## Hexagonal boundaries

| Layer | Change |
|---|---|
| `ports/` | None — `DiarizationPort::process` signature unchanged. |
| `domain/` | None. |
| `use_cases/diarization_processor.rs` | None — orchestrates the port; unaffected. |
| `audio/speaker/sherpa_adapter.rs` (adapter) | New `refine_pass2(samples, sr, final_centroids)` method: uniform fine re-chunk at `FINE_SPLIT_SECS` (ignoring `MAX_CHUNK_SECS`) + nearest-centroid labeling + smoothing + coalesce + min-floor + facet-2 temporal-presence scan. `process()` itself is unchanged. |
| `audio/speaker/commands.rs` (command surface) | Orchestrates `process()` → `enforce_max_speakers_cap` → `refine_pass2(post_cap_centroids)`; passes the resulting fine segments to the alignment layer; write-back shape unchanged. |
| `audio/speaker/alignment.rs` (adapter) | None — `align_with_tokens` already implemented; goes live once token data exists. |
| `audio/speaker/token_timestamps.rs` (adapter) | `extract_token_timestamps` wired into the save path (currently dead). |
| Whisper INSERT path (`import.rs`, `retranscription.rs`, `transcript.rs`) | Add `token_timestamps` column to INSERTs. |

## Risks / Trade-offs

- **Pass 2 compute cost (~4× embeddings)** → Task 1 measures wall-clock on a 70-min meeting with an `#[ignore]` real-audio test; if > 60s, scope an 8kHz-downsampled Pass 2 before proceeding (mirrors the segmentation-windows risk treatment). Runs on the existing blocking thread, never the audio callback. **RESOLVED (§1.1):** rayon-parallelised `build_fine_chunks` (chunks are independent; the ONNX embedding session is `&self`/`Sync`) brought Pass 2 from 148s (sequential, `num_threads:2`) to **~29s** on the 83-min cde5c264 meeting (~2487 fine chunks). The 8kHz-downsample path (§1.2) is **not needed** — well under the 60s budget.
- **Two-pass doesn't resolve sub-2s overlaps (e.g. `[47:32]`)** → accepted limitation. Token-level alignment (D4) does NOT recover these — it distributes words within a diarization segment and cannot invent a boundary. `[46:58]` (the primary case) IS resolved.
- **Nearest-centroid mislabels an ambiguous chunk** → D3 tie-break by temporal predecessor + smoothing residual cleanup.
- **Pass 2 fragments a clean single-speaker meeting** → if Pass 1 yields a single centroid, every fine chunk snaps to it by construction; if Pass 1 over-clusters (AHC false positive), smoothing + coalescing clean up the residual. Task 3.6 guards the single-speaker case.
- **Token timestamps absent (some Whisper configs / Parakeet fallback)** → existing `align_proportional` fallback is retained (spec already requires this degraded mode).
- **Prompt injection via transcript text** → unchanged: LLM output is validated at its boundary; diarization never interprets transcript text as instructions.

## Adversarial test plan (§4 categories)

**Two-pass (facet 1):**
- Silence chunk → `extract_embedding` returns None → skipped (no orphan label).
- Equidistant centroid → D3 tie-break by predecessor.
- 1-speaker meeting → single centroid, no fragmentation.
- Oversized meeting (4h) → Pass 2 chunk count bounded; no OOM.
- `[46:58]` real-audio oracle (`#[ignore]`): Ricardo interjection isolated — ≥10s of his label in `[46:42–47:02]`, and no Ricardo label before his 17:37 join. **Ricardo's label is identified by temporal ground truth** (the centroid with minimal pre-17:37 presence — he is the only late-joiner), not a voice-cosine check: a voice reference averaged over `[17:37,19:00]` blends all 3 speakers (the diagnostic dump showed chunks nearest to each of centroids 0, 1, and 2 in that span) and misidentifies the label. A post-join presence sanity check (>300s) guards against a phantom/dead label. The pipeline resolves `[2802s–2820s]` as an 18s segment of his centroid; the prior oracle failure was a test-only reference bug, not a pipeline defect.

**Token wiring (lever 1):**
- Empty `token_timestamps` (Whisper returns none) → falls back to proportional, no crash.
- Oversized transcript chunk (500 kB) → handled.
- Multi-speaker segment with tokens → split at boundary (the spec's existing scenario, now actually exercised).

**Facet 2 (temporal-presence):**
- Absent-speaker singleton at meeting start (`[0:01]` "Hello") → relabeled to present speaker.
- Short run between two different speakers → preserved (not collapsed).
- Isolated singleton with gaps on both sides → relabeled.

## Migration Plan

No data migration: the `token_timestamps` column already exists. Existing rows stay NULL and use proportional alignment. New transcriptions populate the column. Diarization re-runs on existing meetings pick up the finer Pass 2 labels automatically (re-diarization is idempotent and already supported).

Rollback: Pass 2 and the facet-2 constraint are additive stages gated behind their existence in `process()`; reverting the commit restores the coarse-only pipeline. Token wiring reverts to NULL columns (proportional fallback).

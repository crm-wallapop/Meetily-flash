## Context

Speaker diarization (`speaker-diarization` capability) runs as a post-transcription queue phase: it carves audio chunks from Whisper segment boundaries, embeds them (nemo_titanet, 192-dim), clusters (greedy duration-weighted AHC), applies temporal-coherence smoothing, coalesces same-label runs, merges short speakers, enforces a speaker cap, then aligns transcript rows to the resulting speaker segments.

Two user-visible label-quality failures were verified by listening to meeting `cde5c264` (3 speakers, 83 min), after the "absorption" thread was closed as a diagnostic artifact:

- **Facet 1 — multi-speaker Whisper windows collapse.** Whisper segments are 22–28s (179/238 rows > 15s) and span multiple speakers. Diarization chunks are carved at `effective_split = max(speech_seconds/600, 3.0)` ≈ 8s for this meeting, then coalesced into ~50s runs, so a multi-speaker segment falls inside one run and gets one label. `[46:58]` Ricardo's 2s interjection is swallowed into a Cynthia run.
- **Facet 2 — short chunks mis-attribute to absent speakers.** `[0:01]` "Hello" (1.4s) is labeled Ricardo, who joins at 17:37.

A compounding defect defeats the spec's existing per-word alignment: `token_timestamps` is NULL for every row. `extract_token_timestamps` (`token_timestamps.rs:5`) is fully implemented but never called, and every `INSERT INTO transcripts` (`import.rs`, `retranscription.rs`, `transcript.rs`) omits the column — so `align_with_tokens` (`alignment.rs:103`) never runs and everything falls back to `align_proportional`, which sees one speaker per ~50s run.

Two read-only spikes (logged in `openspec/exploration/diarization-label-quality.md`) established the binding constraints:

1. **Embedding spike** — finer 2s windows isolate the `[46:58]` interjection cleanly (cos_ric=0.57 at 2s vs cos_cyn=0.61 at 8s). The mixed pre-mixed channel does NOT dominate at 2s.
2. **Full-pipeline spike** — with correct per-chunk labels, smoothing preserves fine labels (only 6/105 chunks flipped; the interjection survives as a 20s run). BUT real AHC @0.65 on 2s chunks **fragments** (56 clusters from 105 chunks) because fine embeddings are noisier and pairwise cosine drops below threshold. Two-pass sidesteps this.

## Goals / Non-Goals

**Goals:**
- Resolve facet 1: multi-speaker Whisper segments get per-turn speaker boundaries (verified target: `[46:58]`, `[0:05]`).
- Resolve facet 2: short chunks can't take labels of temporally-absent speakers (verified target: `[0:01]`).
- Activate the existing token-level alignment requirement by populating `token_timestamps`.

**Non-Goals:**
- Source separation of the pre-mixed remote channel (the meeting platform mixes Cynthia+Ricardo before capture; no diarization decision can un-mix them — accepted).
- Perfect resolution of sub-2s overlaps (e.g. `[47:32]` short exclamations inside a dominant-speaker 2s window) — these are recovered by token-level word alignment, not finer chunking.
- Changing the AHC threshold or clustering algorithm for the coarse pass (it stays at 0.65; Pass 2 does not re-cluster).
- Any change to `DiarizationPort` or cross-meeting matching.

## Decisions

### D1 — Two-pass coarse→fine re-labeling (not single-pass finer AHC)

Pass 1 runs the **existing** pipeline unchanged at coarse granularity (~8s, threshold 0.65) to produce stable per-speaker centroids. Pass 2 re-chunks the same audio at ~2s (`FINE_SPLIT_SECS`, new constant) and assigns each fine chunk to its **nearest Pass-1 centroid** (no second AHC), then runs the existing smoothing + coalescing on the fine labels.

**Why over alternatives:**
- *Lower the AHC threshold for fine chunks* (e.g. 0.45): **rejected.** The full-pipeline spike showed the threshold is chunk-size-dependent and data-dependent; calibrating a stable fine-grained threshold is fragile. Two-pass avoids re-clustering entirely.
- *pyannote turn-aware segmentation* (the archived `diarization-segmentation-windows` Path B): **rejected for this metric.** Two-pass already resolves the primary `[46:58]` case; pyannote adds a model pass + the ORT-crate collision that forced Path B's rewrite, for no gain on the verified target.
- *Display-layer split only*: **rejected.** Useful only after diarization segments are already finer than Whisper segments; it doesn't create the boundaries.

Nearest-centroid assignment is the same operation the spike's "REF" case validated (clean 6-segment output, interjection preserved). The coarse centroids are already clean (export test: Cynthia cos 0.923 to validated centroid), so Pass 2 inherits stable references.

### D2 — Pass 2 granularity ≈ 2s, configurable

`FINE_SPLIT_SECS = 2.0`. Spike showed 2s isolates the target interjection; 1.5s is marginally crisper at ~33% more compute. 2s is the sweet spot. Exposed as a constant (not user-facing) so it can be tuned per-meeting later if needed.

### D3 — Tie-break fine chunks by temporal predecessor

A fine chunk equidistant between two coarse centroids (or below a confidence margin) takes the label of its temporal predecessor, not an arbitrary centroid. Prevents flicker at ambiguous boundaries; the smoothing pass then cleans residual noise.

### D4 — Token wiring reuses the existing extract function

`extract_token_timestamps(state, num_segments)` is fully implemented (centiseconds→ms, skips special tokens, validates). Wire it into the Whisper result→DB save path and add `token_timestamps` to the three INSERT sites. The column already exists (migration 20260527); existing NULL rows fall back to proportional alignment unchanged. No new infrastructure.

### D5 — Facet 2 temporal-presence constraint, post-smoothing

After smoothing + coalescing, scan for chunks shorter than `MIN_PRESENCE_SECS` (≈2s) whose label has no same-label segment within `PRESENCE_WINDOW_SECS` (≈30s) on either side. Relabel such orphans to the dominant speaker in the window. This is distinct from the existing `enforce_min_segment_floor`: that collapses only sub-10s runs whose **two neighbors share a label**; a singleton with a gap on both sides (the `[0:01]` case) escapes it because it has no same-label neighborhood to vote it back.

## Hexagonal boundaries

| Layer | Change |
|---|---|
| `ports/` | None — `DiarizationPort::process` signature unchanged. |
| `domain/` | None. |
| `use_cases/diarization_processor.rs` | None — orchestrates the port; unaffected. |
| `audio/speaker/sherpa_adapter.rs` (adapter) | Pass 2 fine re-chunk + nearest-centroid labeling + facet-2 constraint, inside `process()` after the existing cluster+smooth. |
| `audio/speaker/alignment.rs` (adapter) | None — `align_with_tokens` already implemented; goes live once token data exists. |
| `audio/speaker/token_timestamps.rs` (adapter) | `extract_token_timestamps` wired into the save path (currently dead). |
| `audio/speaker/commands.rs` (adapter/command surface) | Passes fine segments to alignment; write-back shape unchanged. |
| Whisper INSERT path (`import.rs`, `retranscription.rs`, `transcript.rs`) | Add `token_timestamps` column to INSERTs. |

## Risks / Trade-offs

- **Pass 2 compute cost (~4× embeddings)** → Task 1 measures wall-clock on a 70-min meeting with an `#[ignore]` real-audio test; if > 60s, scope an 8kHz-downsampled Pass 2 before proceeding (mirrors the segmentation-windows risk treatment). Runs on the existing blocking thread, never the audio callback.
- **Two-pass doesn't resolve sub-2s overlaps (e.g. `[47:32]`)** → accepted; token-level word alignment (D4) recovers these. `[46:58]` (the primary case) IS resolved.
- **Nearest-centroid mislabels an ambiguous chunk** → D3 tie-break by temporal predecessor + smoothing residual cleanup.
- **Pass 2 fragments a clean single-speaker meeting** → by construction it cannot: there is one centroid; every fine chunk snaps to it. Covered by an adversarial test.
- **Token timestamps absent (some Whisper configs / Parakeet fallback)** → existing `align_proportional` fallback is retained (spec already requires this degraded mode).
- **Prompt injection via transcript text** → unchanged: LLM output is validated at its boundary; diarization never interprets transcript text as instructions.

## Adversarial test plan (§4 categories)

**Two-pass (facet 1):**
- Silence chunk → `extract_embedding` returns None → skipped (no orphan label).
- Equidistant centroid → D3 tie-break by predecessor.
- 1-speaker meeting → single centroid, no fragmentation.
- Oversized meeting (4h) → Pass 2 chunk count bounded; no OOM.
- `[46:58]` real-audio oracle (`#[ignore]`): Ricardo interjection isolated (the spike's REF result, as a regression guard).

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

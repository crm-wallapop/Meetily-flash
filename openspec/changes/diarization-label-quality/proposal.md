## Why

Diarization produces user-visible **label-quality** failures on multi-speaker meetings — distinct from the disproven "absorption" thread (closed 2026-07-16). Verified by listening to meeting `cde5c264` (3 speakers, 83 min, 2026-06-22):

1. **Multi-speaker Whisper windows collapse to one label (pervasive).** Whisper transcript segments are 22–28s (179 of 238 rows > 15s) and span multiple speakers; diarization coalesces adjacent same-label chunks into ~50s runs, so a multi-speaker segment falls inside one run and gets a single label. At `[46:58]`, Ricardo's 2s "I think she was right" is swallowed into a Cynthia run; at `[0:05]`, a 3-person back-and-forth is labeled one speaker.
2. **Short chunks mis-attribute to absent speakers.** `[0:01]` "Hello" (1.4s) is labeled Ricardo, who does not join until 17:37 — impossible. Cause: chunks are assigned by global embedding proximity with no temporal-presence constraint.

Two root causes, both code-confirmed: (a) coarse chunking (~8s `effective_split`, forced up by the `MAX_DIARIZATION_CHUNKS=600` cap) cannot resolve sub-turn interjections; (b) `token_timestamps` is empty for every row — `extract_token_timestamps` is dead code and every `INSERT INTO transcripts` omits the column — so the per-word alignment the spec already requires (`align_with_tokens`) never runs.

## What Changes

- **Two-pass diarization (facet 1).** Pass 1 clusters at the existing coarse granularity (~8s, threshold 0.65) to obtain stable speaker centroids. Pass 2 re-chunks the audio at ~2s and assigns each fine chunk to its **nearest coarse centroid**, then runs the existing smoothing + coalescing on the fine labels. This sidesteps the binding constraint discovered in the spike: AHC @0.65 *fragments* at 2s (56 clusters from 105 chunks) because fine embeddings are noisier, but nearest-centroid assignment to stable coarse centroids does not. Spike-verified: smoothing preserves the fine labels (6/105 flips) and the `[46:58]` Ricardo turn survives as a distinct 20s run (production swallowed it).
- **Wire `token_timestamps` (lever 1).** Call `extract_token_timestamps` (already implemented, currently dead) in the Whisper save path and add the `token_timestamps` column to every `INSERT INTO transcripts`. This activates the existing "Token-level timestamps align transcript text" requirement, enabling per-word speaker split for the cases two-pass does not fully resolve (e.g. `[47:32]` short exclamations inside a Cynthia-dominated 2s window).
- **Temporal-presence constraint (facet 2).** A short chunk SHALL NOT take a speaker label whose cluster has no temporal support in the surrounding neighborhood — preventing the absent-Ricardo `[0:01]` "Hello" mis-attribution.

No new models or dependencies. No breaking API changes (`DiarizationPort::process` signature unchanged).

## Capabilities

### New Capabilities

(None.)

### Modified Capabilities

- `speaker-diarization`: adds requirements for (a) diarization output resolved at sub-turn granularity via two-pass coarse→fine re-labeling, and (b) a temporal-presence constraint that prevents short chunks from taking labels with no nearby support. The existing "Token-level timestamps align transcript text" requirement is unchanged in intent — this change makes the implementation actually satisfy it by populating `token_timestamps`.

## Impact

- **`sherpa_adapter.rs`** — `process()` gains a Pass-2 fine re-chunk + nearest-coarse-centroid labeling step after the existing cluster + smooth pipeline; the temporal-presence constraint is applied to the fine labels. No change to the `DiarizationPort::process` signature.
- **`token_timestamps.rs`** — `extract_token_timestamps` is wired into the Whisper result-to-DB save path (currently never called).
- **`alignment.rs`** — unchanged; `align_with_tokens` (already implemented) becomes live once token data exists.
- **Transcript INSERT path** — `import.rs`, `retranscription.rs`, `transcript.rs` add the `token_timestamps` column (currently omitted).
- **`commands.rs`** — diarization result write-back unchanged in shape; may pass fine-grained segments to alignment.
- **Performance** — Pass 2 adds a second embedding pass over ~2s chunks (~4× the coarse pass count). Runs on the existing blocking thread; task 1 measures wall-clock and gates behind an 8kHz-downsample path if a 70-min meeting exceeds 60s (mirrors the segmentation-windows risk treatment).
- **No data migration** — `token_timestamps` column already exists in the schema (read path parses it); only the write path omits it. Existing rows stay NULL and fall back to proportional alignment.

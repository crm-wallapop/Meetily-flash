## Why

Diarization produces user-visible **label-quality** failures on multi-speaker meetings — distinct from the disproven "absorption" thread (closed 2026-07-16). Verified by listening to meeting `cde5c264` (3 speakers, 83 min, 2026-06-22):

1. **Multi-speaker Whisper windows collapse to one label (pervasive).** Whisper transcript segments are 22–28s (179 of 238 rows > 15s) and span multiple speakers; diarization coalesces adjacent same-label chunks into ~50s runs, so a multi-speaker segment falls inside one run and gets a single label. At `[46:58]`, Ricardo's 2s "I think she was right" is swallowed into a Cynthia run; at `[0:05]`, a 3-person back-and-forth is labeled one speaker.
2. **Short chunks mis-attribute to absent speakers.** `[0:01]` "Hello" (1.4s) is labeled Ricardo, who does not join until 17:37 — impossible. Cause: chunks are assigned by global embedding proximity with no temporal-presence constraint.

Two root causes, both code-confirmed: (a) coarse chunking (~8s `effective_split`, forced up by the `MAX_DIARIZATION_CHUNKS=600` cap) cannot resolve sub-turn interjections; (b) `token_timestamps` is empty for every row — `extract_token_timestamps` is dead code and every `INSERT INTO transcripts` omits the column — so the per-word alignment the spec already requires (`align_with_tokens`) never runs.

## What Changes

- **Two-pass diarization (facet 1).** Pass 1 is the existing `process()` (coarse ~8s, threshold 0.65 → cluster → smooth → coalesce → `merge_short_speakers`); the existing `enforce_max_speakers_cap` then runs in `commands.rs`, and together they yield the **final** speaker centroids (not the raw AHC output, which carries noise singletons). Pass 2 is a new adapter method `refine_pass2`, invoked from `commands.rs` immediately after the cap, that re-chunks the audio at ~2s and assigns each fine chunk to its **nearest final centroid**, then runs the existing smoothing + coalescing on the fine labels. This sidesteps the binding constraint discovered in the spike: AHC @0.65 *fragments* at 2s (56 clusters from 105 chunks) because fine embeddings are noisier, and lowering the threshold does not fix it (0.40→11 clusters, 0.45→15, still never reaching the 2–3 needed) — but nearest-centroid assignment to stable coarse centroids does not re-cluster. **Spike-verified end-to-end** (logged in `openspec/exploration/diarization-label-quality.md`, "Resolving spike" section): running the actual two-pass on the full meeting resolves `[46:42–47:00]` as an 18s Ricardo segment (production swallows `[46:58]` into Cynthia); all three speakers' meeting-derived centroids are separable (Cynthia cos 0.925, Ricardo cos 0.896, Carlos well-separated from both (cos 0.22 to Cynthia, 0.19 to Ricardo)); the Carlos+Cynthia early region produces clean multi-turn alternation.
- **Wire `token_timestamps` (lever 1).** Call `extract_token_timestamps` (already implemented, currently dead) in the Whisper save path and add the `token_timestamps` column to every `INSERT INTO transcripts`. This is a **compliance fix** for the existing `whisper-model-selection` requirement *"Whisper provider stores token timestamps in the database"* (SHALL extract per-token timestamps, serialize as `{word, start_ms, end_ms}`, store in the `token_timestamps` column, extend `TranscriptResult`/`TranscriptUpdate`) — the codebase violates it today; no spec modification is needed, only the implementation. With fine two-pass segments in place, token timestamps make the per-word attribution at detected turn boundaries **precise** (each word mapped to its actual timestamp) rather than the proportional approximation `align_proportional` currently produces.
- **Temporal-presence constraint (facet 2).** A short chunk SHALL NOT take a speaker label whose cluster has no temporal support in the surrounding neighborhood — preventing the absent-Ricardo `[0:01]` "Hello" mis-attribution.

No new models or dependencies. No breaking API changes (`DiarizationPort::process` signature unchanged).

## Capabilities

### New Capabilities

(None.)

### Modified Capabilities

- `speaker-diarization`: adds requirements for (a) diarization output resolved at sub-turn granularity via two-pass coarse→fine re-labeling, and (b) a temporal-presence constraint that prevents short chunks from taking labels with no nearby support. The existing "Token-level timestamps align transcript text" requirement is unchanged in intent — this change makes the implementation actually satisfy it by populating `token_timestamps`.

## Impact

- **`sherpa_adapter.rs`** — gains a new `refine_pass2(samples, sample_rate, final_centroids)` adapter method (uniform 2s fine re-chunk via a new `build_fine_chunks` helper, nearest-final-centroid labeling, smoothing, coalescing, min-segment-floor, facet-2 temporal-presence scan). `process()` itself is unchanged; `refine_pass2` lives on the concrete adapter, not the `DiarizationPort` trait (promotion deferred to `hexagonal-port-traits`).
- **`token_timestamps.rs`** — `extract_token_timestamps` is wired into the Whisper result-to-DB save path (currently never called).
- **`alignment.rs`** — unchanged; `align_with_tokens` (already implemented) becomes live once token data exists.
- **Transcript INSERT path** — `import.rs`, `retranscription.rs`, `transcript.rs` add the `token_timestamps` column (currently omitted).
- **`commands.rs`** — orchestrates `process()` → `enforce_max_speakers_cap` → `refine_pass2(post_cap_centroids)`; passes fine segments to the alignment layer; write-back shape unchanged.
- **Performance** — Pass 2 adds a second embedding pass over ~2s chunks (~4× the coarse pass count). Runs on the existing blocking thread; task 1 measures wall-clock and gates behind an 8kHz-downsample path if a 70-min meeting exceeds 60s (mirrors the segmentation-windows risk treatment).
- **No data migration** — `token_timestamps` column already exists in the schema (read path parses it); only the write path omits it. Existing rows stay NULL and fall back to proportional alignment.

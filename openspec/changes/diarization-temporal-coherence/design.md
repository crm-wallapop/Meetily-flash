## Context

The diarization pipeline (`audio/speaker/sherpa_adapter.rs` + `commands.rs`) runs:

1. `build_chunks` — splits transcript segments into ~7.86 s diarization chunks
   (`sherpa_adapter.rs:284`; `effective_split = max(speech_secs/600, 3.0)` → 7.86 s for
   this 78.6-min meeting)
2. Per-chunk embedding extraction (nemo_titanet, 192-dim) — happens INSIDE
   `cluster_by_centroids` (`sherpa_adapter.rs:199`); embeddings are NOT currently
   returned to the caller
3. `cluster_by_centroids` — global agglomerative clustering with cached similarity matrix
   (`sherpa_adapter.rs:199`), returns `(labels, cluster_centroids)`. Threshold-gated
   merges, duration-weighted centroids.
4. **Segment coalescing** (`sherpa_adapter.rs:211–245`) — consumes `labels` and turns
   per-chunk labels into `SpeakerSegment` objects. **After this point the per-chunk
   labels and embeddings no longer exist as separate arrays.**
5. `renumber_speakers` + `merge_short_speakers` (`sherpa_adapter.rs:247, 258`) — operate
   on SEGMENTS (2 % duration floor on speaker clusters, not on chunks).
6. `process()` returns `DiarizationOutput { segments, centroids }` (`sherpa_adapter.rs:267`)
7. `enforce_max_speakers_cap` runs in `commands.rs` AFTER `process()` returns
   (`commands.rs:856`)
8. Alignment — `align_proportional` (fallback) or `align_with_tokens`

Step 3 is **global**: all ~600 chunks are clustered in a single AHC pass with no temporal
information. Each chunk is assigned to its cosine-nearest centroid independently of what
label its temporal neighbors received. This is the root of all five failure classes.

### The contamination-then-drift mechanism (data-confirmed)

Production diagnostic on `meeting-00000001-...` (read-only, 2026-06-27):

```
t=1s     Carlos's first chunk → births spurious "Speaker 2" cluster
         (only Carlos+Carol present; a 3rd cluster cannot exist yet)
t≈5min   Bob joins → his chunks pile into the contaminated cluster
         → centroid becomes blurred average of {early-Carlos, Bob}
t≈30min  Blurred Speaker-2 centroid attracts Carol's chunks
         → Carol's cluster stops winning any labels (0 rows, min 30–70)
         → rapid Carlos↔Speaker2 oscillation (44–53% singleton flicker)
         → align_proportional splits at every flip → 5 s text fragments
```

| Signal | Value | Finding |
|---|---|---|
| Chunk granularity | 7.86 s | NOT the problem — turns are captured |
| Transcript-row granularity | 23.2 s median | Whisper segments, not chunks |
| Carol labels, min 0–30 | 37 rows | Correctly labeled while uncontaminated |
| Carol labels, min 30–70 | 0 rows | **Absorbed** — present per user, labels vanished |
| Carlos median row dur, min 30–40 | **5 s** | Alignment fragments from flicker |
| Singleton flicker rate, min 30+ | 44–53 % | Per-chunk labels flip nearly every chunk |
| Refuted: short-turn averaging | Carol 25.6 s > Carlos 23.1 s | Turns were LONGER, not shorter |

### Why granularity is NOT the fix (Thread A deferred)

The system already sub-segments Whisper segments into ~7.86 s chunks. The failures are
wrong cluster *assignments* to these chunks, not missed turns. Pyannote sub-segment
detection would marginally improve boundary precision but would not fix the fundamental
clustering-coherence deficit. Thread A is deferred until boundary precision is the
bottleneck; this change addresses the assignment errors that actually caused the failures.

## Goals

- Eliminate per-chunk flicker (44–53 % → near-zero singleton rate in stable regions)
- Prevent early contamination seeds from propagating (no spurious cluster at t=1)
- Recover absorbed speakers (Carol-class: a speaker who stops winning labels mid-meeting
  due to centroid drift)
- Eliminate downstream text fragmentation (5 s fragments → coherent rows)
- **Guarantee no regression on clean meetings** (well-separated speakers are untouched)

## Non-Goals

- Changing the embedding model, chunk granularity formulas, or threshold defaults
- Pyannote sub-segment turn detection (deferred)
- Per-turn speaker override UI (separate change `per-turn-speaker-override`)
- Real-time/online diarization (remains a post-processing queue phase)
- Fixing the pre-existing async-concurrent-badge-edit race in `useSpeakerRename`
  (separate from this change — it is not introduced here)

## Design

### D1 — Insertion point (revised after adversarial review)

**The smoothing MUST run inside `sherpa_adapter.rs::process()` immediately after
`cluster_by_centroids` (line 199) and BEFORE the segment-coalescing loop (line 211).**

This is forced by the data availability: the smoothing consumes per-chunk `labels`,
per-chunk `embeddings`, per-chunk `timestamps`, and `centroids`. After line 199, `labels`
and `cluster_centroids` exist; `timestamps` are derivable from `chunks[*].start_sample`;
but **per-chunk embeddings are currently computed inside `cluster_by_centroids` and not
returned**. The implementation SHALL therefore refactor `cluster_by_centroids` (or split
out the embedding-extraction step) so the per-chunk embedding `Vec` is available to the
smoothing pass. This is the one structural refactor this change requires.

Revised pipeline (the `← NEW` steps run inside `process()`, before coalescing):

```
cluster_by_centroids  (now also exposes per-chunk embeddings)
  → smooth_labels_temporal     ← NEW (chunk-level: labels, embeddings, timestamps, centroids)
  → recompute_centroids        ← NEW (from cleaned labels)
  → [segment coalescing: lines 211–245]
  → renumber_speakers
  → merge_short_speakers   (segment/cluster-level, 2% floor — unchanged)
process() returns
  → enforce_max_speakers_cap   (in commands.rs — runs AFTER smoothing)
  → align
```

This ordering also resolves the open question below: **smoothing runs BEFORE
`enforce_max_speakers_cap`**, because the cap lives in `commands.rs` after `process()`
returns and cannot see chunk-level data. The cap's most-isolated-cluster metric operates
on the post-smoothing (de-contaminated) centroids; this is desirable — the cap should
judge isolation on cleaned centroids, not contaminated ones. The cap's existing NaN/Inf
guards (`commands.rs` cosine helper) apply unchanged.

### D2 — Neighborhood-voted re-assignment (formula corrected)

> **AMENDMENT (2026-06-29 apply):** the original D2 was internally contradictory
> (the formula summed over `j ≠ i` but the prose said "the chunk's own embedding
> still contributes via the j=i term with full weight"). Both literal readings
> are wrong: **j ≠ i only** (exclude self) recovers absorption but ERASES genuine
> short interjections between two different speakers — the surrounding speakers
> outvote the interjection's excluded voice, reintroducing the user's "merged
> turns" complaint. **Full self** (j=i at weight 1.0) preserves interjections but,
> when a contamination seed's embedding is genuinely tilted toward the contaminant
> centroid, the full self-vote anchors the wrong label and blocks recovery.
>
> **TUNING (2026-06-29 adversarial review):** the first shipped cut used
> `self_weight = 0.5` and `μ = 0.05`. Two findings forced a retune. (a) At
> drift `cos(chunk, wrong_centroid) ≥ 0.95`, the score gap to the true centroid
> is exactly 0.05, so `μ = 0.05` (`>`, not `≥`) never fires — the severe
> volume-absorption case (a real speaker's chunks fully absorbed so the wrong
> centroid drifts to sound like them) is precisely where recovery matters most.
> (b) Lowering μ to recover that case would erase orthogonal-embedding
> interjections (their neighbor gap is ~0.033). Resolution: **raise
> `self_weight` to 0.6 so self anchors an interjection OUTRIGHT** (0.6 exceeds
> the single-side neighbor weight ~0.553 = exp(-1)+exp(-2)+exp(-3), so split
> neighbors cannot outvote self regardless of μ — this also covers edge-of-array
> interjections with neighbors on one side only), **then lower `μ` to 0.03** to
> recover drift up to cos~0.97. Clean meetings stay stable: the self-differential
> `0.6·(1 − cos(A,B)) ≈ 0.24` at between-speaker cos 0.6 is far above μ. Note
> the neighbor peak is `exp(-1) ≈ 0.368`, NOT 1.0 — self (0.6) is the strongest
> single vote, but neighbors collectively (up to ~1.106) outweigh it, which is
> what lets absorption recover while interjections survive. Confirmed by tests
> (`smooth_sustained_absorption_recovered`,
> `smooth_short_interjection_between_different_speakers_preserved`, and the
> new realistic-cosine variants). This is what ships.

For each chunk i with embedding e_i and current label L_i:

1. Gather the ±W temporal neighbors (W default 3 → ±~24 s at 7.86 s granularity ≈ one
   observed flicker cycle of 15–25 s). A window of one cycle captures the local consensus
   without straddling multiple full oscillation periods (which would average both labels
   equally and dilute the vote).
2. For each candidate label k, compute the vote:
   `score(k) = Σ_{j ∈ window(i)} cos(e_j, centroid_k) · w(i,j)`
   where the window includes j=i (self) and its ±W neighbors, and
   `w(i,j) = self_weight` (default 0.6) when j=i, else `exp(-|i−j|)` (peak `exp(-1)≈0.368`
   at the nearest neighbor). **Critical:** the summand MUST be `cos(e_j, centroid_k)`
   (voter j's acoustic fit to centroid k), NOT `cos(e_i, centroid_k)`. The latter is
   constant over j and reduces the vote to `argmax_k cos(e_i, centroid_k)` — i.e.
   nearest-centroid, identical to what AHC already does, which fixes nothing.
3. **Confidence gate (no-regression guard):** reassign `L_i` only if the winning label's
   normalized score exceeds the current label's normalized score by a positive margin `μ`
   (default 0.03). On a clean meeting where existing assignments are high-confidence, the
   self-differential dominates (see TUNING above), so nothing flips — guaranteeing the
   "clean meeting is a near-no-op" property without relying on a post-hoc revert.
4. After all eligible chunks are reassigned, recompute centroids from the new labels
   (`recompute_centroids_from_labels`, duration-weighted).
5. Iterate steps 1–4 up to 2 times (fixed-point, capped to bound cost). Bootstrapping: the
   first pass uses contaminated centroids and recovers the clearest chunks; the recomputed
   centroids are cleaner, so the second pass recovers more.

A Carol chunk at minute 35 that was assigned to Speaker 2 (drifted centroid) gets pulled
back because its **neighbors'** embeddings (the surrounding chunks of her actual voice)
vote more strongly for Carol's centroid (defined from her 37 clean early chunks) than for
the drifted Speaker-2 centroid, once the vote accounts for the neighborhood's acoustic
profile and the confidence gate is met.

NaN/Inf in any `e_j` SHALL contribute 0.0 to the vote (clamped), so a degenerate neighbor
cannot corrupt the outcome. NaN/Inf in any timestamp SHALL be treated as "unknown order"
and exclude that neighbor from the window rather than corrupting the temporal sort.

### D3 — Minimum-duration floor with acoustic guard

> **AMENDMENT (2026-06-29 apply):** the "OR the short run's embedding is closer to
> one neighbor by a clear acoustic margin" merge branch was DROPPED. With the
> damped-self vote (D2 amendment), a short run that is acoustically part of a
> neighbor (a mis-split) is already absorbed by the VOTE before the floor runs
> (its chunks flip to the neighbor's label because the self-fit to its own
> mis-split centroid is low); so the floor's acoustic-margin branch is redundant.
> The floor now has ONE merge rule: a short run whose two adjacent runs share one
> (different) label is a flicker island and is absorbed. A short run sandwiched
> between two different speakers is preserved (by the vote's margin gate; the
> floor must not merge it). This keeps the floor simple and the over-merge risk
> bounded. Test: `smooth_flicker_islands_collapse` + the interjection-preservation
> tests confirm both behaviors.

After D2, enforce that any same-label run shorter than `MIN_SMOOTH_SEGMENT_SECS`
(default ~10 s) is merged into a neighbor — **but only when it is a flicker island**:
merge only if both adjacent runs share the same label as each other (the short run is a
noise spike inside one speaker's region), OR if the short run's embedding is closer to one
neighbor by a clear acoustic margin. **Never merge a short run sandwiched between two
different speakers** — that is a genuine interjection and must be preserved even if brief.

This guard is what makes the floor safe for real short turns (quick confirmations,
back-channels). The earlier "neighbors vote preserves short turns regardless of duration"
rationale was invalid under the original (broken) formula; the corrected formula plus this
structural guard together protect genuine turns.

### D4 — Centroid recomputation (MODIFIES the canonical centroid-storage requirement)

Centroids stored in `speaker_embeddings` (used for cross-meeting matching via
`registry.search`) SHALL be the post-smoothing recomputed centroids, not the pre-smoothing
clustering centroids. This prevents contaminated centroids from polluting the
cross-meeting registry — a stuck wrong centroid would cause every future meeting to
inherit the conflation.

This **modifies** the canonical requirement "Centroid embeddings are stored per speaker
per meeting for cross-meeting matching," which currently states centroids are "computed
during agglomerative clustering." The delta spec uses `## MODIFIED Requirements` for that
requirement (see spec delta) so archive applies the refinement cleanly rather than leaving
the canonical spec internally contradictory.

### D5 — Hexagonal boundary

`smooth_labels_temporal` is a pure function:
`(labels: &[u32], embeddings: &[Vec<f32>], timestamps: &[f64], centroids: &HashMap<u32, Vec<f32>>, params: &SmoothParams) -> Vec<u32>`.
No I/O, no Tauri, no ONNX. It lives in `sherpa_adapter.rs` alongside `cluster_by_centroids`
(the existing pattern — clustering logic is adapter-internal; no port change). The params
(W, MIN_SMOOTH_SEGMENT_SECS, μ, max iterations) are constants, not user settings (YAGNI — no
caller needs to tune them yet).

### D6 — Adversarial tests (§4 mandatory categories)

| Category | Test |
|---|---|
| Contamination seed | A t=0 chunk that births a spurious cluster under global AHC is absorbed into its temporal neighbor under smoothing |
| Sustained absorption | A speaker whose chunks are consistently mis-assigned mid-meeting (centroid drift) is recovered (≥ 80 % of the mis-assigned run) |
| Flicker | 40 % singleton-run input → < 5 % singleton-run output |
| Real turn preserved | A genuine speaker change (strong acoustic shift, ≥ MIN_SMOOTH_SEGMENT_SECS) is NOT smoothed away — including a short turn sandwiched between two different speakers |
| Degenerate embedding | NaN/Inf embeddings in the window contribute 0.0 to the vote (clamp) |
| **Degenerate timestamps (added)** | NaN/Inf timestamp in the chunk array excludes that chunk from the window rather than corrupting the temporal sort or panicking |
| Long meeting | n=600 chunks, smoothing completes < 1 s (O(n·W·K), sub-second) |
| Property | Smoothed output cluster count ≤ input; well-separated speakers (centroid cosine < 0.3) with runs ≥ `MIN_SMOOTH_SEGMENT_SECS` are never merged by smoothing |
| Property (no-regression) | On a clean, high-confidence input, the smoothed output differs from the input on at most a negligible fraction of chunks (the confidence gate prevents flips) |

### D7 — Iteration count (open question resolved)

The 2-iteration fixed-point cap is the **default** (tasks 3.1/3.2 deliver it). The earlier
"resolve empirically" deferral is withdrawn: 2 iterations is the committed design. If
empirical testing against the production meeting shows 1 iteration recovers ≥ 80 % equally
well, a follow-up may reduce the cap — but the default ships at 2.

## Alternatives considered

- **Lower the threshold (0.65 → 0.40):** would merge the t=1 seed into Carlos's cluster at
  the cost of over-merging real speakers elsewhere. Does not address flicker or absorption
  structurally — it is a per-meeting knob, not a pipeline fix.
- **Pyannote sub-segment turns (Thread A):** deferred — 7.86 s chunks already capture turns;
  the failures are assignment errors, not missed turns.
- **Online/streaming clustering:** more invasive (replaces the clustering algorithm, not
  just a post-pass). The post-pass approach is less risky and reversible.
- **Viterbi decode alone:** principled but cannot fix sustained absorption with contaminated
  centroids. Revisit as a refinement after D2/D3 if neighborhood-voting underperforms.
- **Post-hoc revert-if-worse guard:** considered for the no-regression property, rejected in
  favor of the D2 confidence gate. Revert-if-worse measures "fit" against either pre- or
  post-smoothing centroids, both of which make the metric circular; the confidence gate
  guarantees no-flip-on-clean-input structurally instead.

## Risks

- **Over-smoothing real turns:** mitigated by D3's acoustic guard (never merge a short run
  between two different speakers) and the D2 confidence gate (only flip on decisive margin).
- **Window-size tuning:** W too large over-smooths (dilutes the vote across cycles); W too
  small doesn't fix absorption. Default W=3 (~one observed flicker cycle) is grounded in the
  one production data point; explicitly tunable as a constant if other meetings differ.
- **Confidence-margin μ tuning:** too high → absorption not recovered; too low → clean
  meetings perturbed. Tuned empirically against the production meeting; the property test
  (D6 no-regression) locks the clean-meeting behavior once μ is set.
- **Embedding-threading refactor (D1):** the one structural change. `cluster_by_centroids`
  gains a returned embeddings Vec — a local, well-contained change with an existing test
  oracle (the cached-similarity property test) to confirm clustering output is unchanged.

## Open questions

- ~~Run smoothing before or after `enforce_max_speakers_cap`?~~ **Resolved (D1):** before —
  the cap is in `commands.rs`, smoothing is inside `process()`, so the ordering is fixed by
  architecture. Desirable: the cap judges isolation on de-contaminated centroids.
- ~~One D2 iteration or two?~~ **Resolved (D7):** 2 is the default; reducible later if
  empirically warranted.

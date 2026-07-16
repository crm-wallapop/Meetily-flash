# Exploration: diarization label quality (multi-speaker merges + short-segment mis-attribution)

> Opened 2026-07-16. Status: **exploring** (pre-proposal).
> Triggered by live UI verification of `meeting-cde5c264` after the absorption
> thread was closed ([[project_diarization_root_cause]]). The absorption was a
> centroid-measurement artifact and does NOT exist — but listening to the labels
> surfaced **two different, real, user-visible label-quality failures**. This
> exploration defines them precisely and evaluates fix levers.

## Ground truth (from the user, listening to the recording)

- 3 speakers: **Carlos** (user, local mic), **Cynthia** + **Ricardo** (remote,
  pre-mixed on the system channel by the meeting platform).
- Carlos is **silent after ~min 36** ("pretty much Cynthia + Ricardo").
- At [0:01] only Carlos + Cynthia are present; **Ricardo is not on the call**
  (joins ~min 17:37).

## The two facets (one root cause: labels assigned by embedding proximity with no temporal/turn sanity)

### Facet 1 — multi-speaker window collapse (pervasive)

- Whisper transcript segments here are **22–28s** (median 22.3s in the first 90s;
  **179 of 238 rows are >15s**). Whisper groups by sentence/VAD, **not by speaker**.
- Smoking gun: `[0:05–0:32]` (27s) labeled `Speaker 1` contains a 2–3 person
  back-and-forth ("How's it going? … you've aged like five years … I have some updates").
- Every long block is suspect: the meeting is almost entirely 22–28s windows.

### Facet 2 — short-segment mis-attribution to an absent speaker

- `[0:01–0:02]` (1.4s) "Hello." labeled `Speaker 2` — but Speaker 2 = Ricardo, who
  doesn't join until 17:37. Impossible label.
- Cause: diarization clusters **globally** (not in time order); a short,
  vowel-dominated embedding landed nearest Ricardo's centroid. Temporal smoothing
  didn't fix it (isolated singleton + following gap → no same-label neighborhood).

## Identity map (consistent across DB timeline + export test)

| Label | Identity | Evidence |
|---|---|---|
| Speaker 0 | Cynthia | Present throughout; dominant late. Export test: cos 0.923 to validated centroid, 545s early / 1521s late. |
| Speaker 1 | Carlos | Heavy 0–30min, **zero 30–70min**, 42s at end. Matches "I stay silent after ~36m." The cluster that 'vanishes' is the user — correct behavior, not a bug. |
| Speaker 2 | Ricardo | First *real* appearance 17:37 (his join); spurious "Hello" at 0:01 is the only pre-join Speaker 2. |

## Root-cause mechanism (code-confirmed)

Pipeline: `build_chunks` (carve ~8s chunks from Whisper-segment boundaries at
`effective_split`) → `cluster_by_centroids` AHC @0.65 (note: production threshold
is 0.65, not the 0.40 default) → temporal smoothing → **coalesce consecutive
same-label chunks** → `merge_short_speakers` → `enforce_max_speakers_cap(3)` →
`align_transcripts_with_diarization` → `UPDATE transcripts SET speaker_label`.

Two compounding defects produce facet 1:

1. **Coalescing creates ~50s runs.** Final diarization = 100 segments / ~4980s ≈
   50s avg. Adjacent same-label chunks merge into long runs, so a 27s Whisper
   segment falls *inside* one run.
2. **`token_timestamps` is empty for all 238 rows.** So `align_with_tokens`
   (`alignment.rs:103`, the per-word splitter) is **never exercised**; everything
   uses `align_proportional` (`alignment.rs:161`), which splits words
   proportionally across overlapping diarization segments. With 50s runs, a 27s
   Whisper segment overlaps **one** run → all words → one speaker → no split.

So the per-word split capability exists but is defeated by (a) coarse coalesced
runs and (b) missing word timestamps. Facet 2 is independent: global AHC + no
temporal-presence constraint.

> Note: CLAUDE.md / the segmentation-windows design both assume "Token-level
> timestamps" are available. They are not, in practice. That assumption being
> wrong is itself a finding.

## Levers to evaluate (cheapest → most invasive)

1. **Populate `token_timestamps`.** If whisper-rs already yields word timestamps,
   persisting them unlocks `align_with_tokens` (precise per-word split). Cheapest
   *if* the data is already produced. **Does not help alone** while runs are 50s
   (all words in a run map to one speaker) — needs lever 2 or 4.
2. **Reduce coalescing aggressiveness.** Cap run length, or stop merging across
   detected gaps/turns, so diarization segments stay closer to chunk granularity
   (~8s). Then `align_proportional` would split multi-speaker Whisper segments
   across multiple speakers (approximately). Pure pipeline change, no new model.
3. **Temporal-presence constraint (facet 2).** Reject/relable isolated short
   segments whose label has no temporal support nearby (the "Hello" case); minimum
   duration for a singleton label; this is what smoothing was meant to do but the
   singleton+gap case escapes it.
4. **Turn-aware segmentation (segmentation-windows redux, re-justified).** Use
   pyannote native windows so chunks don't span turns → each chunk single-speaker
   → coalescing only merges genuinely-same-speaker windows. **Re-justified for the
   RIGHT metric** (multi-speaker-per-chunk), NOT the disproven "absorption."
   Caveat: the prior §1 gate "22× Cynthia recovery" was itself measured against
   the contaminated centroid, so that result is void — must re-measure on
   multi-speaker-per-chunk rate, not absorption.
5. **Display-layer split.** Render text grouped by diarization segment rather than
   Whisper segment. Only useful after lever 2/4 make diarization segments finer
   than Whisper segments.

## Evidence artifacts

- DB (read-only): `meeting-cde5c264` transcripts — 238 rows, 179 >15s, 0 with
  token_timestamps. Per-speaker 10-min bucket timeline + first/last appearance.
- Export test `test_cde5c264_export_final_pipeline` (commands.rs, this branch):
  authoritative no-absorption guard — Cynthia = Speaker 0, 545s early / 1521s late.
- Live rediarize run (dev log b6x0j4gqm, 2026-07-16 13:37–13:38): 3 speakers /
  100 segments, threshold=0.65, persisted to DB.

## Open questions

- **RESOLVED: token timestamps are produced but not persisted.** `whisper_engine.rs:554/677`
  sets `set_token_timestamps(true)`; `token_timestamps.rs::extract_token_timestamps`
  is fully implemented; the DB read path (`commands.rs:787`) parses them. But
  `extract_token_timestamps` is **never called** (dead code), and every
  `INSERT INTO transcripts` (`import.rs:839`, `retranscription.rs:677`,
  `transcript.rs:88`) omits the `token_timestamps` column. Lever 1 = wire the
  function into the save path + add the column to INSERTs. No new model/deps.
- Is production threshold 0.65 (not the 0.40 default) intentional? It drives
  over-clustering (28→17→3) which then gets coalesced — relevant to lever 2.
- CONFIRMED: facet 1 worsens late (46–48min cases, pure mixed-channel region).

## Converging proposal shape (two paths, cheap-first)

The fixes bundle into facet 1 (multi-speaker merges) + facet 2 (short-segment
mis-attribution). Facet 1 needs finer diarization granularity + token-timestamp
wiring so the fine segments drive per-word labels:

- **Path A (cheap, spike first):** reduce `effective_split`/`SPLIT_TARGET_SECS`
  toward 2–3s (finer fixed chunks) + wire token timestamps (lever 1) + tame
  coalescing (lever 2, cap run length / stop merging across gaps). No new model.
  Spike: measure sub-turn resolution on the 46–48min cases. May suffice if finer
  fixed chunks isolate 2–4s interjections well enough.
- **Path B (principled, if A underdelivers):** pyannote turn-aware segmentation
  (lever 4, = segmentation-windows repurposed) + token timestamps. Detects real
  turn boundaries so a 2s interjection embeds as its own speaker. Heavier
  (model pass + the ORT/`ort`-crate collision that forced the original Path B
  rewrite — see segmentation-windows design.md D1-revised).

Facet 2 (lever 3, temporal-presence constraint) is independent and orthogonal —
fixes the absent-Ricardo "Hello" regardless of path chosen.

## Confirmed late cases (user-verified 2026-07-16, 46–48min region)

Both in the pure Cynthia+Ricardo mixed-channel region (after Carlos goes silent),
confirming facet 1 is worst there. Critical nuance: the diarization gets the
**coarse ~25s turn structure right** (Cynthia/Ricardo alternate correctly at the
run level — see run map below). What it misses is **sub-turn interjections**
(2–4s) buried inside a run.

Run map (46:00–48:44): `S2 46:00–46:25 · S0 46:27–46:30 · S2 46:32–46:58 ·
S0 46:58–47:30 · S2 47:32–48:14 · S0 48:14–48:44` — turns alternate correctly.

- **[46:58–47:21] Speaker 0 (Cynthia), 22.6s** — Ricardo's 2s "I think she was
  right" opens the block, then Cynthia's "our boss is often right… web to app deep
  link." The 8s chunk containing Ricardo's interjection also holds Cynthia's
  follow-on → her voice dominates the embedding → whole run → Speaker 0.
- **[47:32–47:48] Speaker 2 (Ricardo), 16.1s** — a Cynthia/Ricardo back-and-forth
  ("Ming Jong?" / "Ming Jen? Really?" / "I think he's just…") labeled Speaker 2
  despite being mostly Cynthia. Same domination, opposite winner — shows the
  instability: similar content gets different labels depending on which voice
  dominates each chunk's embedding.

### Refined lever ranking (after late cases)

The late cases **rule out the cheap levers as standalone fixes**:

- **Lever 1 (token timestamps) insufficient**: diarization never produced a
  Ricardo segment at 46:58, so `align_with_tokens` has nothing to map his words to.
- **Lever 2 (tame coalescing) insufficient**: runs here are already 16–26s, not
  50s; the failure is *within-run* sub-turn changes, not over-merging.
- **Binding constraint = chunk granularity vs. sub-turn changes.** ~8s
  `effective_split` chunks cannot isolate 2–4s interjections; the louder voice on
  the pre-mixed channel wins each chunk's embedding.

Real options for facet 1:
- **Finer fixed chunking** — drop `effective_split`/`SPLIT_TARGET_SECS` toward 2–3s.
  Cheap, shrinks the domination window, but no turn awareness (may split
  mid-utteration; mixed-channel domination persists at smaller scale).
- **Turn-aware segmentation (lever 4, pyannote)** — principled: detects the actual
  turn boundary so a 2s Ricardo window embeds as Ricardo. Heavier (model pass).
  This is the segmentation-windows direction, **re-justified on the correct metric**
  (sub-turn resolution), not the void absorption metric.

Levers 1+2 still worth bundling (token timestamps + tamed coalescing improve the
cases that finer/turn-aware segmentation *does* catch), but they cannot stand alone.

## Still-open verification

- `[73:08–73:50]` Speaker 1 (Carlos): real Carlos interjection or mis-attribution?
  (Only 2 late Carlos rows in the whole 36–83min region.)
- Confirm Speaker 2 = Ricardo at 17:37 join (strongly indicated by run map).

## Spike result — Path A embedding level (2026-07-16, `spike_finerchunk.py`)

Read-only Python spike (sherpa-onnx 1.13.3, = Rust embeddings). Decoded the
46:00–47:30 region; slid windows at 1.5/2.0/2.5/3.0/4.0/8.0s (hop = win/2);
measured cos to validated `cyn_cen` (Cynthia) and `abs_cen` (Ricardo ref,
confirmed: Ricardo-speaking windows score cos_ric 0.6–0.8).

**DECISIVE POSITIVE at the embedding level.** The 46:58 turn (Ricardo's 2s "I
think she was right" → Cynthia) is separable at ≤2.5s but smeared at 8s:

- 8s  `[46:58–47:06]` cos_cyn=0.61 → CYN  (Ricardo swallowed — **production failure reproduced**)
- 2s  `[46:58–47:00]` cos_cyn=0.06 cos_ric=0.57 → RIC, then `[47:01–47:03]` cos_cyn=0.69 → CYN (**turn detected**)
- 1.5s: crispest. 2.5s: clean. 3s: detectable. 4s: borderline. 8s: lost.

The mixed channel does NOT dominate at 2s — finer fixed chunking isolates
sub-turn interjections. **Path A is viable at the embedding level.** Bonus:
`[46:00–46:25]` labeled Speaker 2 embeds cos_cyn 0.87 (Cynthia) — bidirectional
mis-label, confirms facet 1 swaps either direction.

**Three caveats before Path A is a real fix (all untested, full-pipeline):**
1. Smoothing (±3-neighbour vote, self_weight 0.6) may re-flip isolated 2s Ricardo
   chunks back to Cynthia — finer labels must survive `smooth_to_fixed_point`.
2. Coalescing merges consecutive same-label chunks; fine alternating labels may
   collapse back into long runs unless coalescing is tamed.
3. `MAX_DIARIZATION_CHUNKS=600` cap forces `effective_split` back up when chunk
   count exceeds 600 (~2s over 5000s speech ≈ 2500 chunks). Cap needs rework.

**Next: full-pipeline confirmation** — run finer chunks (2–2.5s) through AHC →
smoothing → coalescing → cap, measure whether the 46:58 / 47:32 interjections
survive to final segments. If they do, Path A is the proposal; if smoothing/
coalescing erase them, Path A needs those tuned or Path B (pyannote) is required.

## Full-pipeline confirmation result (2026-07-16, `spike_fullpipe.py`)

Ran the faithful Python replication (AHC → smooth_to_fixed_point → coalesce →
merge_short, copied from `full_pipeline.py`) on freshly-extracted **2s** chunks
of the 45:00–48:30 region (105 chunks). Two label sources tested.

### Caveat #1 (smoothing survival) — RESOLVED, PASS

With correct per-chunk labels (REF = nearest validated centroid), smoothing
flips only **6/105** chunks and produces clean **6-segment** output:
`[46:42–47:02] RIC (20s) · [47:02–47:58] CYN (56s)`. The Ricardo interjection
**survives** as a 20s run — `enforce_min_segment_floor` (10s floor) does not
touch it. The turn at 47:02 is detected. Production had [46:58–47:30] all-Cynthia
(swallowed Ricardo). **Smoothing/coalesce are NOT the obstacle.** Confirms the
code reading: the floor only collapses sub-10s runs whose two neighbours share a
label; a genuine run ≥10s, or a short run between two *different* speakers, is
preserved.

### NEW BLOCKER — AHC threshold fragmentation

Real greedy AHC @0.65 on the same 2s chunks produced **56 clusters** (vs 2–3
expected). Only cluster 0 (30 chunks, cos_ric=0.96) and cluster 7 (19 chunks,
cos_cyn=0.95) have real membership; the other 54 are singletons whose cosine to
the (drifted) cluster centroid sits at/below 0.65. **At 2s, embeddings are
noisier → pairwise cosine drops → the 0.65 threshold cannot rediscover the
clusters.** "Just lower `SPLIT_TARGET_SECS`" is insufficient: the AHC threshold
is chunk-size-dependent and 0.65 (tuned for 8s) fragments at 2s.

### Design direction — two-pass (coarse centroids → fine re-label)

The REF case IS a two-pass approach and it produced the clean result:
1. **Pass 1 (coarse):** existing pipeline at 8s, threshold 0.65 → stable cluster
   centroids (Cynthia / Ricardo / Carlos).
2. **Pass 2 (fine):** re-chunk at 2s, assign each chunk to nearest *coarse*
   centroid → fine labels. No second AHC, no threshold recalibration.
3. Smoothing + coalesce on the fine labels (confirmed to preserve them).

This sidesteps the AHC-fragmentation blocker entirely — the centroids come from
the stable coarse pass; fine chunks just snap to the nearest one.

### Remaining limitation — 47:32

Even optimistic REF resolves [47:02–47:58] as a single CYN block. The 47:32
back-and-forth ("Ming Jong?" / "Ming Jen?") is short exclamations inside
Cynthia-dominated 2s windows — she wins each window even at 2s. Resolving it
needs either <2s granularity or token-level alignment (lever 1, wire
`token_timestamps`). 46:58 (the primary case) IS resolved by two-pass.

### Updated caveat status

1. ~~Smoothing survival~~ — **PASS** (6/105 flips, interjection preserved).
2. Coalescing — not an obstacle (run-length encoding only; preserves alternation).
3. `MAX_DIARIZATION_CHUNKS=600` cap — still forces `effective_split≈8s`; two-pass
   sidesteps by making fine chunking a *second* pass, not the clustering input.
4. **NEW: AHC threshold (0.65) fragments at 2s** — two-pass sidesteps this too.

## Resolving spike — actual two-pass end-to-end (2026-07-16, `spike_twopass_actual.py`)

Shark-tank round 1 flagged that two-pass was only *inferred* from component spikes
(embedding separability + smoothing survival + AHC fragmentation), never run as an
actual two-pass. This spike runs it for real, plus the AHC threshold sweep the
reviewers asked for. All three Critical findings resolved.

**Pass 1 — coarse AHC on the FULL meeting (8s, threshold 0.65), 621 chunks:**
30 raw clusters, but only **3 dominant** (94% of audio):

| cluster | cos_cyn | cos_abs | chunks | identity |
|---|---|---|---|---|
| 5 | **0.925** | 0.031 | 227 | Cynthia |
| 7 | 0.031 | **0.896** | 214 | Ricardo |
| 3 | 0.217 | 0.191 | 144 | Carlos (low on both refs; dominant 0–30min) |

The other 27 are noise singletons (1–8 chunks) that the existing
`merge_short_speakers` + `enforce_max_speakers_cap` already collapse. **Resolves
shark-tank C2**: all three speakers have clean, separable meeting-derived
voice-prints — not just Cynthia.

**Pass 2 — assign 2s chunks to nearest Pass-1 centroid → smooth → coalesce:**
- **LATE 46:00–49:30 (primary target):** `[46:42–47:00]` resolves as an **18s
  Ricardo segment**. Production currently swallows [46:58] into Cynthia. **Target
  resolved.** Smoothing flipped 21/105 — inflated because this spike fed the 27
  raw noise centroids to Pass 2; the real implementation derives centroids
  post-merge/post-cap (3 clean prints), so flip rate and noise would be lower.
- **EARLY 10:00–13:30 (Carlos + Cynthia):** clean 7-segment alternation, 8% flips.
  Not a 3-speaker region (Ricardo doesn't appear until 17:37, per facet-2
  narrative) — confirms two-pass produces clean multi-turn structure for the
  local-mic speaker vs Cynthia.

**Spike B — AHC threshold sweep on 2s, LATE region (the simpler alternative):**
`0.40→11 clusters · 0.45→15 · 0.50→20 · 0.55→30 · 0.65→57`. Lower thresholds
reduce fragmentation but never reach the 2–3 needed even on a 210s window. **Resolves
shark-tank C5/scope-F2**: lowering the AHC threshold is not a viable simpler
alternative; two-pass (nearest stable centroid) is structurally sounder.

**Design refinement surfaced:** Pass-2 centroids MUST come from the speaker set
*after* `merge_short_speakers` + `enforce_max_speakers_cap` (3 clean prints), not
the raw 30-cluster AHC output. This spike used raw output, so its late-region
labels include noise singletons (cluster18/10/15/24) that the real implementation
would not produce. The spike therefore slightly *under*-represents the real
implementation's quality, yet still resolves the primary target.

**[47:32] reframe confirmed.** Even the actual two-pass resolves [47:02–47:58] as
one Cynthia block — [47:32]'s short exclamations live inside Cynthia-dominated 2s
windows. Token-level alignment does NOT recover this (token alignment distributes
words *within* a diarization segment; it cannot create a speaker split where
diarization found no boundary). [47:32] is an **accepted limitation**, not
token-deferred.

### 3-centroid confirmation (`spike_3centroid.py`, shark-tank R2 I1)

Re-ran Pass 2 on the late region assigning to ONLY the top-3 clusters by duration (the post-`merge_short_speakers`/post-`enforce_max_speakers_cap` equivalent — for cde5c264, max_speakers=3, so post-merge == post-cap == these 3):

- cluster 5: CYN (1816s, cos 0.925), cluster 7: RIC (1712s, cos 0.896), cluster 3: Carlos (1152s).
- **Pass 2 LATE: `[46:42–47:02]` resolves as a 20s RIC segment. TARGET SURVIVES.**
- Smoothing flipped **13/105 (12%)** — *lower* than the raw-30-centroid spike's 21/105 (20%).
- Output: 9 segments (vs 22 with raw centroids), **zero noise singletons** — the late region is clean `CYN 46:00–46:42 · RIC 46:42–47:02 · CYN 47:02–47:58`.

Resolves shark-tank R2 I1: the configured design is not only valid, it is *cleaner* than the raw-centroid spike. The worry that removing singleton "escape valves" would force ambiguous 2s chunks to mis-attribute did not materialize — `[46:58]` still snaps to Ricardo. The raw-30-centroid spike therefore under-represented quality, as claimed.

## Disposition of prior changes (revised)

- `diarization-segmentation-windows`: absorption rationale **disproven**, but its
  turn-aware-segmentation direction is **re-justified for facet 1** (lever 4). Not
  a clean abandon — candidate to **repurpose** with a corrected metric.
- `diarization-f0-correction`: still disproven (no mis-attributed chunks to fix via
  F0; facet 2 needs temporal constraints, not pitch). Clean abandon stands.

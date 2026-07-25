# Exploration: Diarization speaker-split persistence (storage collapse)

> **Status:** Exploration findings (explore mode). Not a proposal yet.
> **Date:** 2026-07-24
> **Capability:** `openspec/specs/speaker-diarization/spec.md` (if present; else nearest)
> **Evidence:** meeting `meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323` (Cynthia + Ricardo)

## TL;DR

`align_transcripts_with_diarization` correctly splits a coarse Whisper segment
that spans multiple speakers into separate per-speaker `AlignedSegment`s — the
split logic exists and is tested. But the storage loop in
`DiarizationProcessor::process` writes each split back via
`update_transcript_speaker(original_id, …)`, an `UPDATE transcripts … WHERE id = ?`.
All splits derived from one source transcript share that transcript's `id`, so N
updates hit the same row and **last-writer-wins**: one speaker label survives,
the split text is discarded entirely. The user sees a 26.8 s block of dialogue
from two+ speakers attributed to a single speaker.

This is a **different bug** from `diarization-label-quality` (just landed). That
change operates *inside* the adapter (`refine_pass2` + temporal-presence orphan
scan) to correct which `speaker_id` a fine diarization segment gets. It does not
touch the storage-mapping collapse described here.

> **Adversarial panel (2026-07-24) — storage collapse CONFIRMED, but granularity
> ceiling exposed.** The fine diarization export for [5.7, 32.5] s contains
> exactly **two** segments: Speaker 0 [5.67–21.36] and Speaker 1 [21.36–55.18].
> `align_proportional` therefore emits two `AlignedSegment`s; the storage loop
> collapses them to Speaker 1 (matches the DB row). So the collapse is real and
> the diagnosis holds. **However:** the diarization emitted only *one* speaker
> boundary across a 27 s window whose transcript reads as rapid multi-turn
> dialogue. Fixing storage persistence yields a **2-way split at 21.36 s**, not
> the per-turn split the text implies. The diarization's own granularity (2 s
> embedding windows + AHC clustering that merges similar consecutive windows) is
> the ceiling — storage/token-timestamp work sharpens *where the text divides at
> an existing boundary* but adds **no new speaker boundaries**. Storage
> persistence is therefore **necessary but not sufficient** for the user-visible
> complaint. See "Granularity ceiling" below. (Export dated 2026-07-16, pre-
> `diarization-label-quality`; structural finding robust to that landing.)

---

## Evidence (cde5c264)

DB: `C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite`

- 238 transcript rows, 0 with `token_timestamps` (recorded before that feature).
- First dialogue rows (ordered by `audio_start_time`):

```
  5.7 -  32.5 (26.8s) Speaker 1  "How's it going? All good, all good. Yeah. Oh look, you've aged like five years. ..."
 32.5 -  40.2 ( 7.7s) Speaker 1  "Yeah. Gotcha. Where is Ricardo? ..."
 41.8 -  55.2 (13.4s) Speaker 1  "Oh there in the meeting room alone ..."
 57.8 -  80.0 (22.3s) Speaker 0  "Okay, we can we can start. He's coming in too. ..."
```

The 5.7–32.5 s row is visibly multi-speaker dialogue ("How's it going?" /
"All good, all good" / "you've aged like five years" are different voices) but
carries one label. Most rows are 20–27 s — Whisper's ≤30 s segment window does
not respect speaker turns.

---

## Root-cause chain (file:line)

1. **Whisper emits coarse ≤30 s segments** that span multiple speakers.
   Inherent to Whisper's segment window; not a Meetily bug.

2. **Diarization adapter produces fine 2–3 s speaker segments.**
   `sherpa_adapter.rs`: Pass-1 `SPLIT_TARGET_SECS = 3.0`, Pass-2 `FINE_SPLIT_SECS = 2.0`
   (`build_fine_chunks`, `refine_pass2`). Correct.

3. **Alignment SPLITS coarse rows at speaker boundaries — correctly.**
   `alignment.rs:65 align_transcripts_with_diarization` dispatches per transcript:
   - token path (`align_with_tokens`, :113) when `token_words` non-empty — splits
     per-word at the speaker whose diarization segment contains the word's `start_ms`;
   - proportional fallback (`align_proportional`, :171) otherwise — splits text by
     time ratio across overlapping diarization segments.
   Both paths return **multiple** `AlignedSegment`s with distinct `speaker` and
   disjoint `text`. Covered by tests `token_alignment_splits_multi_speaker`,
   `proportional_split_fallback`, `speaker_a_b_a_pattern_produces_three_segments`.

4. **BUG — storage collapses the split (last-writer-wins).**
   `diarization_processor.rs:186-192`:
   ```rust
   let aligned = align_transcripts_with_diarization(transcripts, &diarization_segs);
   for seg in &aligned {
       let label = resolve_label(&seg.speaker, &label_map);
       SpeakerRepository::update_transcript_speaker(pool, &seg.original_id, &label, "auto").await?;
   }
   ```
   Every `AlignedSegment` from one source transcript carries the same
   `original_id` (set at `alignment.rs:159/:181/:217`). `update_transcript_speaker`
   (`speaker.rs:244`) is `UPDATE transcripts SET speaker_label=? … WHERE id=?`.
   N updates to the same id → only the final speaker remains; the split `text`,
   `audio_start_ms`/`audio_end_ms` from each `AlignedSegment` are **never written**.

Net effect: the split is computed and thrown away. The coarse Whisper row keeps
one speaker label regardless of how many speakers actually spoke in it.

---

## Granularity ceiling (panel finding, 2026-07-24)

The storage collapse is necessary to fix, but it is **not the whole story** for
the user's complaint. The fine diarization for cde5c264's first dialogue window
is:

```
5.67 – 21.36  Speaker 0   (15.7 s, one run)
21.36 – 55.18 Speaker 1   (33.8 s, one run)
```

The transcript text across [5.7, 32.5] reads as rapid back-and-forth between
voices ("How's it going?" / "All good, all good" / "you've aged like five
years" / "Yeah" / …), implying several short turns. The diarization collapsed
all of that into **two long runs with a single boundary at 21.36 s**. After a
storage fix, the user would see that row split in two — better, but still not
per-turn.

Why: `refine_pass2` re-chunks at `FINE_SPLIT_SECS = 2.0` and assigns each 2 s
window to the nearest post-cap centroid. Consecutive windows with similar
embeddings get the same `speaker_id` and are coalesced into one segment. Rapid
turn-taking between two voices whose embeddings sit close together (or that the
2 s window straddles) is smoothed into a single long run. `pyannote-segmentation`
is loaded but used only for speech-region/VAD-style detection, **not** for its
speaker-change-point output, which could mark finer turn boundaries.

Implications for the proposal:
- Storage persistence (options A/B/C) is the **floor** — without it even the
  boundaries the diarization *does* find are discarded.
- Whether to also pursue finer turn segmentation is a **separate scoping
  question** (new capability vs. bug fix). It may belong in a follow-up change,
  not this one — but the exploration must not promise per-turn splitting from a
  storage-only fix.

---

## Turn-segmentation lever (2026-07-24, post-shark-tank investigation)

The granularity ceiling above is now **in scope** (change bundled per author
decision, 2026-07-24). A focused code investigation found the realistic lever:

### Finding: pyannote-segmentation.onnx is a phantom dependency
`sherpa_adapter.rs:90-121` (`SherpaOnnxDiarizationAdapter::with_shared_threshold`)
takes `segmentation_model_path`, checks the file exists (lines 103-106), and
**never passes it to any sherpa-onnx config**. The only ONNX object constructed
is a `SpeakerEmbeddingExtractor` (nemo_titanet). The model is downloaded
(`model_download.rs:6-11`), existence-checked, and discarded. Every speaker
boundary today comes from the Whisper segment grid, the uniform 3 s / 2 s chunk
grid, or AHC label changes. (This corrects this doc's earlier claim that
pyannote is "loaded but used only for VAD-style detection" — it is not used at
all.)

### Opportunity: sherpa-onnx already ships the missing diarizer
sherpa-onnx 1.13.2 (pinned `Cargo.toml:141`, resolved `Cargo.lock:5905`) ships a
complete `offline_speaker_diarization` module that properly wraps pyannote, with
`min_duration_on` defaulting to **0.3 s** (vs Meetily's 2.0 s effective floor).
Meetily imports none of it — the only `sherpa_onnx::` imports are
`SpeakerEmbeddingExtractor*` and `SpeakerEmbeddingManager`.

### Chosen lever (Part B of the bundled change): OfflineSpeakerDiarization as a boundary pre-splitter (hybrid)
Run sherpa's built-in diarizer on the recording, take its segment boundaries,
feed those sub-segments into Meetily's existing embedding + AHC + cap + registry
pipeline. **Discard sherpa's labels; keep only boundary placement.**

- No new model (pyannote already on disk), no FFI work (binding already pinned).
- Preserves temporal-coherence smoothing, label-quality `refine_pass2`, the
  most-isolated-cluster cap, and cross-meeting registry matching.
- Works **without token_timestamps**: the ceiling is boundary *placement* (from
  the audio signal), not text alignment — cde5c264 benefits fully without
  re-transcription.

### Rejected alternatives (with evidence)
- **Full swap to `OfflineSpeakerDiarization`**: discards the cap / registry /
  temporal-smoothing work — regresses `diarization-label-quality` and breaks
  cross-meeting matching. Rejected.
- **Reduce `FINE_SPLIT_SECS` (2 s → finer)**: same embedding-noise mechanism that
  made temporal-coherence move `SPLIT_TARGET_SECS` to 3.0 — between-speaker
  cosine collapses from ~0.6 toward ~0.8, defeating `SMOOTH_SELF_WEIGHT=0.6`.
  Rejected (window-reduction disproven, per project memory).
- **Token-timestamp per-word embeddings**: inert for cde5c264 (0/238 rows), and
  adds no boundaries even with tokens (alignment sharpens text division at
  existing boundaries, never creates new ones). Rejected.
- **Overlap detection**: addresses simultaneous speech, not cde5c264's sequential
  back-and-forth. Rejected.
- **Acoustic change-point detection (BIC/KL2)**: hold as fallback only if the
  sherpa plumbing proves invasive — re-implements weaker what pyannote does.

---

## Design options (to be chosen at proposal time, NOT committed here)

### A — Child table `aligned_segments`
New table `(transcript_id FK, speaker, text, start_ms, end_ms, source)`. UI
renders from it when rows exist for a meeting; original `transcripts` row is the
source of truth. Rediarize = `DELETE FROM aligned_segments WHERE meeting_id=?`
then re-insert.
- **+** Original transcript IDs stable (summaries, FKs, retranscription refs intact).
- **+** Rediarize is cleanly idempotent.
- **−** New table + join in the read path; UI must prefer aligned rows.

### B — Split-in-place
`DELETE` coarse row, `INSERT` N fine rows with new IDs.
- **+** Simplest read path (no join).
- **−** Changes transcript IDs → must re-point summary/meeting FKs or accept
  orphaned references; rediarize must reconstruct the original coarse row from
  the union of splits (fragile once re-mixed).
- **−** Hardest to make idempotent.

### C — JSON column `aligned_speakers` on `transcripts`
Add `aligned_speakers TEXT` (JSON array of `{speaker,text,start_ms,end_ms}`).
- **+** Minimal schema change; no new table; original row untouched.
- **−** Harder to query/index; UI must parse; rediarize overwrites the column.
- **−** Stores structured data in a blob (anti-pattern vs. relational child).

### D — Fix at the Whisper/VAD layer instead
Reduce Whisper segment granularity (smaller VAD chunks → shorter Whisper
segments → fewer multi-speaker rows). Does not eliminate the problem (Whisper's
≤30 s window still permits multi-speaker segments) and hurts transcription
quality/coherence. **Not a real alternative to A/B/C** — orthogonal, could
*reduce* incidence but cannot *prevent* the collapse.

---

## Token-timestamps factor (independent)

cde5c264 has **0/238 rows** with `token_timestamps`. So even after a storage
fix, alignment runs the **proportional fallback** for this meeting — it splits
text by time ratio, which is imprecise at the boundary. Token-level precision
requires re-transcription to populate `token_timestamps`.

**Important (clarified by panel):** token timestamps sharpen *where the text
divides* at an existing diarization boundary, but they add **no new speaker
boundaries** — `align_with_tokens` still assigns each word to whichever
diarization segment contains its `start_ms`. So the granularity ceiling above
applies to the token path too. Re-transcription is worth doing for boundary
precision, but it is not a path to per-turn splitting on its own.

---

## Open questions for the proposal phase

1. Does the canonical speaker-diarization spec already promise per-speaker
   splitting within a Whisper segment? If yes, this is a regression/bug fix
   against existing spec; if no, it's a capability extension and needs a spec
   delta.
2. **Scope decision (panel):** is this change storage-only (stops the collapse,
   yields boundaries the diarization already finds) or does it also pursue
   finer turn segmentation? If the latter, is `pyannote-segmentation`'s
   speaker-change-point output usable through sherpa-onnx, or does it need a new
   adapter path? Recommend: storage-only now, turn-segmentation as a named
   follow-up — do NOT bundle.
3. Read-path impact: does the frontend/transcript renderer consume
   `transcripts` rows directly, or via an adapter that could transparently
   prefer aligned splits?
4. Rediarize idempotency contract: must a second rediarize reproduce the same
   split, or is non-determinism acceptable?
5. Summary/meeting-note FKs: are transcript IDs referenced anywhere that
   split-in-place (option B) would break?
6. Should re-transcription (to populate `token_timestamps`) be a prerequisite
   deliverable of this change, or a separate follow-up?

---

## What this is NOT

- Not the `diarization-label-quality` fix (adapter-internal label correction).
- Not fixable by re-running rediarize — the collapse is in the write path, not
  the diarization pass.
- Not a VAD/Whisper bug — those layers produce coarse segments by design; the
  job of splitting is, correctly, deferred to alignment. Only the persistence
  of the alignment output is broken.

---

## Split decision + Part B empirical block (2026-07-25)

The bundled change was **split** into two per the shark-tank (two reviewers +
this doc's own recommendation): the user approved the split on 2026-07-24.

- **Part A** = `fix/diarization-speaker-split-persistence` — storage
  conformance fix. **CONVERGED** after two panel rounds (5 + 1 reviewers);
  folded amendments: PRAGMA-driven dynamic column check (catches
  `previous_label`, migration 20260609000000), proptest time-coverage weakened
  to SUBSET (diarization has gaps), fail-after-N transaction-atomicity double.
  Ready for `/opsx:apply`.

- **Part B** = `enhance/diarization-pyannote-boundaries` — pyannote boundary
  pre-splitter. **NOT CONVERGED — empirically blocked.** Three panel rounds
  refuted three successive `D1` formulations:

  1. *"discard labels, keep boundaries"* — REFUTED: `process()` returns
     post-clustering segments; the label and the boundary's survival are
     coupled (same-label coalescing destroys the boundary before the label is
     discarded).
  2. *`FastClusteringConfig.threshold: 1.0`* — REFUTED: `threshold` is a cosine
     **dissimilarity** cutoff (smaller → more clusters), confirmed via the
     official `sherpa-onnx-offline-speaker-diarization.cc` ("a larger threshold
     leads to few clusters"). 1.0 over-merges — worse than status quo. Correct
     fragmentation setting is `0.0`.
  3. *`threshold: 0.0` + "turns re-derived downstream by AHC + smoothing"* —
     REFUTED: AHC (`cluster_by_centroids`) clusters whole embeddings one-per-
     chunk and cannot detect intra-chunk change-points; `smooth_labels_temporal`
     returns a same-length `Vec<u32>` (relabels, never splits);
     `enforce_min_segment_floor` only merges. A turn buried inside one chunk by
     uniform shedding is **permanently lost**. "Re-derived downstream" was a
     category error (labeling ≠ change-point discovery).

### Emerging truth (the 4th formulation, UNVERIFIED)

Chunk **formation** from pyannote candidates needs same/different-speaker info
to avoid merging across turns — exactly the info D1's "discard labels" throws
away. The sound shape is likely: use a **moderate** pyannote threshold (meaning-
ful clustering), keep segment boundaries AND same/different adjacency through
chunk formation (merge sub-`MIN_SPEECH_SECS` segments into same-label
neighbors, preserving label-change boundaries as turns), THEN discard labels
and re-label with Meetily's AHC. But WHICH threshold gives the right
granularity is an empirical question — it cannot be paneled to convergence
without running pyannote at several thresholds on cde5c264 and measuring
boundary quality against the known turns. The value claim also narrows: even
under the best design, long meetings over the `MAX_DIARIZATION_CHUNKS` cap
revert to ~status-quo granularity (the cap + the no-split-smoothing constraint
are a hard ceiling).

### Part B status

The `enhance/diarization-pyannote-boundaries` change directory is left in place
for reference but is **NOT apply-ready** (known errors: `threshold: 1.0`
residuals in proposal/tasks, supersession clause in the wrong canonical
requirement, orphaned scenarios). Re-opening it requires an empirical prototype
first. The phantom-pyannote-dependency finding (sherpa_adapter.rs:103-106) and
the sherpa-onnx 1.13.2 `offline_speaker_diarization` API facts above remain
valid regardless.

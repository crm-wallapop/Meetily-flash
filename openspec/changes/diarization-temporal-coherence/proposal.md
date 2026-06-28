## User-facing summary (plain English)

After grouping speech chunks by voice, the system now checks that consecutive chunks agree on the speaker. This means a person who was correctly identified early in the meeting is no longer "lost" and mis-attributed to someone else later on (the absorption bug), and the rapid back-and-forth mislabeling that used to shred the transcript into tiny fragments is smoothed out. A genuine quick handoff between two different speakers is still preserved. Net effect for the user: fewer wrong-speaker tags, no more "speaker vanished mid-meeting," and coherent transcript paragraphs instead of 5-second fragments.

## Why

Speaker diarization on `Meeting 2026-01-01_00-00-01` (3 speakers: Carlos, Bob,
Carol) produced five distinct failure classes from a single root cause:

1. **Wrong speaker tag** on a paragraph (0:05–0:32 Carlos↔Carol exchange labeled only
   Carol)
2. **Merged turns** — multiple speakers' speech collapsed into one transcript row
3. **Speaker split** — Carlos labeled as two different clusters ("Carlos Ruiz" and
   "Speaker 2") in different parts of the meeting
4. **Speaker absorption** — Carol (present throughout per the user) gets zero labels
   from minute 30 onward; her speech is attributed to Carlos or the contaminated
   "Speaker 2" cluster
5. **Text fragmentation** — transcript rows shredded into 5-second pieces in the
   minute 30–50 oscillation zone

A read-only diagnostic on the production DB (`meeting-00000000-0000-4000-8000-000000000001`,
238 rows / 78.6 min / threshold 0.65 / max_speakers 3) on 2026-06-27 established two facts
that reframe the fix:

- **Chunk granularity is NOT the problem.** The pipeline already sub-segments each ~23 s
  Whisper segment into ~7.86 s diarization chunks
  (`effective_split = max(speech_secs / 600, 3.0)`; `sherpa_adapter.rs:299`). 7.86 s is
  fine enough to capture speaker turns. The 23 s median measured on *transcript rows* is
  the Whisper segment length, not the chunk length — chunks and rows are different objects.
- **Global AHC with no temporal coherence IS the problem.** All ~600 chunks are clustered
  in one pass (`cluster_by_centroids`) with zero temporal information. Each chunk is
  assigned to its cosine-nearest centroid independently of what label its temporal
  neighbors received. This produces:

  - A **contamination seed**: the t = 1 s chunk (Carlos, pre-Bob) births a spurious
    cluster ("Speaker 2") that cannot exist at t = 1.
  - **Centroid drift**: as Bob's chunks pile into the contaminated cluster, its
    centroid becomes a blurred average of {early-Carlos, Bob}. By minute 30 the
    blurred centroid attracts Carol's chunks — she loses all labels.
  - **Per-chunk flicker**: 44–53 % singleton-run rate from minute 30 onward — labels flip
    almost every chunk because no temporal-continuity constraint exists.
  - **Downstream fragmentation**: the proportional alignment splits transcript rows at
    every flicker boundary, producing 5 s fragments (median Carlos-row duration drops
    from 22 s to 5 s in the 30–40 min bucket).

The user's framing — *"conflation at the beginning that carries over throughout"* — is
exactly the contamination-then-drift mechanism the data confirms. A refuted hypothesis is
worth recording: the absorption is **not** short-turn averaging (Carol's labeled segments
at 25.6 s median were *longer* than Carlos's at 23.1 s). The cause is sustained centroid
contamination, not turn length.

## What Changes

Add a **temporal-coherence post-processing pass** to the diarization pipeline, running
inside `process()` immediately after `cluster_by_centroids` and before segment coalescing.
The pass enforces that consecutive chunks prefer the same speaker label, correcting both
the per-chunk flicker and the sustained mis-assignment that global AHC produces. Centroids
are then recomputed from the cleaned labels so the stored embeddings are not contaminated.

- **New pure function `smooth_labels_temporal`** in `audio/speaker/sherpa_adapter.rs`:
  takes per-chunk labels + embeddings + chunk timestamps + centroids from the global AHC,
  returns temporally-smoothed per-chunk labels. Lives alongside `cluster_by_centroids`
  (the existing pattern — clustering logic is in the adapter). No port changes; the
  diarization adapter's internal pipeline gains a step.
- **Neighborhood-voted re-assignment**: each chunk's label is chosen by a weighted vote
  over its ±W temporal neighbors — `score(k)=Σ cos(e_j, centroid_k)·decay(|i−j|)` summed
  over neighbor embeddings `e_j` (not the chunk's own embedding, which would be a no-op
  nearest-centroid rerun). This recovers chunks that were mis-assigned due to centroid
  drift (the Carol-absorption failure).
- **Minimum-duration floor**: any same-label run shorter than `MIN_SMOOTH_SEGMENT_SECS`
  (~10 s) is merged into the longer adjacent neighbor, eliminating residual flicker.
- **Centroid recomputation**: duration-weighted centroids are recomputed from the cleaned
  labels so the stored `speaker_embeddings` (used for cross-meeting matching) are not
  contaminated by pre-smoothing assignments.

Non-goals: per-turn speaker override UI (separate change `per-turn-speaker-override`); the
rename-cancel `onBlur` bug (separate change `fix/speaker-rename-cancel`); pyannote
sub-segment turn detection — 7.86 s chunks are adequate and the model is shipped but
unused (revisit only if boundary precision becomes the bottleneck after this lands);
changing the embedding model (nemo_titanet stays); changing the chunk granularity formulas
(they are working as designed); real-time/online diarization (this remains a post-meeting
queue phase).

## Capabilities

### New Capabilities

_(none)_

### Modified Capabilities

- `speaker-diarization`: a new requirement governs the temporal-coherence smoothing pass
  that runs after clustering and before alignment. The existing clustering and alignment
  requirements are unchanged; the centroid-storage requirement is clarified to store
  post-smoothing centroids.

## Impact

- **Code**:
  - `frontend/src-tauri/src/audio/speaker/sherpa_adapter.rs` — new `smooth_labels_temporal`
    + `recompute_centroids_from_labels` pure functions; `cluster_by_centroids` refactored
    to also return per-chunk embeddings; smoothing called inside `process()` after
    clustering and before coalescing.
  - `frontend/src-tauri/src/audio/speaker/commands.rs` — unchanged: still calls `process()`
    then `enforce_max_speakers_cap`; smoothing precedes the cap because it is internal to
    `process()`.
- **Spec**: `openspec/specs/speaker-diarization/spec.md` — new temporal-coherence
  requirement added.
- **User-visible behavior**: fewer wrong-speaker tags; no more speaker-split (Carlos as
  two clusters); absorbed speakers (Carol-class) no longer vanish mid-meeting;
  transcript rows no longer shredded into 5 s fragments in oscillation zones.
- **Sequencing**: rebases onto the post-`diarization-clustering-perf` code (cached-matrix
  AHC + chunk cap) — no in-flight conflict.
- **No breaking changes** to IPC contracts, storage schema, or frontend event shapes.

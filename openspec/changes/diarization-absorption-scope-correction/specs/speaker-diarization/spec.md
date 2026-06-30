## MODIFIED Requirements

### Requirement: Temporal-coherence smoothing prevents clustering contamination and per-chunk flicker

After global agglomerative clustering assigns per-chunk speaker labels, the system SHALL apply a temporal-coherence smoothing pass to the per-chunk labels INSIDE `sherpa_adapter.rs::process()` immediately after `cluster_by_centroids` and BEFORE per-chunk labels are coalesced into `SpeakerSegment` objects. The smoothing SHALL be a pure function of the chunk labels, chunk embeddings, chunk timestamps, and cluster centroids, with no I/O. The smoothing SHALL NOT increase the cluster count, and SHALL preserve genuine speaker turns whose acoustic shift is strong and whose duration meets the minimum-segment floor. The output SHALL be deterministic.

The smoothing pass SHALL perform neighborhood-voted re-assignment: for each chunk i with current label L_i, the system SHALL compute, for each candidate label k, a vote `score(k) = Σ_{j ∈ window(i)} cos(e_j, centroid_k) · w(i,j)` where the window spans the chunk itself (j = i) and its ±W temporal neighbors (default W = 3), `e_j` is chunk j's embedding, and the weight `w(i,j)` is `self_weight` when j = i (default 0.6) and `exp(-|i−j|)` for neighbors (peak `exp(-1) ≈ 0.368` at the nearest neighbor). The self weight (0.6) is the single strongest vote, but it is low enough that a contaminated chunk's self-fit to its (wrong) centroid is still outvoted by unanimous neighbors (whose combined weight across both sides is up to ~1.106), recovering the chunk (local contamination recovery); and it is high enough that it exceeds the neighbor weight on one side alone (~0.553), so a genuine short interjection's self-vote for its own distinct centroid anchors it against split neighbor votes on either side, and an edge-of-array interjection (neighbors on only one side) is likewise preserved. Using ONLY the chunk's own embedding (no neighbors) would reduce the vote to nearest-centroid and fix nothing; using ONLY neighbors (no self) would erase genuine short interjections between two different speakers, reintroducing the over-merging the pass exists to prevent. The system SHALL reassign the chunk's label only when the winning label's normalized score exceeds the current label's normalized score by a positive confidence margin (default 0.03), so that on a clean, high-confidence input no chunk flips (the pass is a near-no-op); the margin is set low enough to recover centroid drift up to cosine ~0.97 against the true centroid, while a clean meeting's self-differential (~0.24 at a between-speaker cosine of 0.6) is well above it, so clean input stays stable. The winner SHALL be chosen deterministically (highest score, ties broken by smallest label) so the output is independent of HashMap iteration order. The system SHALL then recompute duration-weighted centroids from the cleaned labels and iterate the re-assign/recompute cycle up to a fixed cap (default 2 iterations) so that recovered chunks refine the centroids used in the next pass.

After the iteration, the system SHALL merge a same-label run shorter than `MIN_SMOOTH_SEGMENT_SECS` (default ~10 s) into a neighbor ONLY when both adjacent runs share the same label as each other (a flicker island). The system SHALL NOT merge a short run sandwiched between two different speakers (a genuine interjection); such a run is preserved by the damped-self vote's margin gate, so the floor need not (and must not) merge it.

Non-finite (NaN or Inf) embedding values in the smoothing window SHALL contribute 0.0 to the vote, so a degenerate chunk cannot corrupt the outcome. Non-finite timestamp values SHALL exclude that chunk from the window rather than corrupting the temporal ordering or panicking.

#### Scenario: Early contamination seed is absorbed

- **GIVEN** a meeting where the t=0 chunk is assigned to a spurious cluster but its ±W temporal neighbors are consistently cluster 0
- **WHEN** temporal-coherence smoothing runs
- **THEN** the t=0 chunk is reassigned to cluster 0
- **AND** no spurious cluster persists from the contamination seed

#### Scenario: Local mis-assignment is recovered when neighbors are clean

- **GIVEN** a chunk mis-assigned to cluster B whose ±W temporal neighbors are consistently cluster C (the chunk's own voice)
- **WHEN** temporal-coherence smoothing runs
- **THEN** the chunk is recovered to cluster C
- **AND** recovery requires clean neighbors — a SUSTAINED regional mis-assignment (every neighbor also mis-assigned) is NOT recovered, because the neighborhood vote reinforces the local consensus

> **Out of scope — sustained speaker absorption over a long meeting.** The neighborhood-voted
> smoothing provably cannot recover a SUSTAINED regional mis-assignment: when every temporal
> neighbor of a chunk carries the same (wrong) label, the neighborhood vote reinforces that
> consensus rather than overturning it, so the pass leaves the region unchanged by design. This
> is a structural property of any local smoothing pass, independent of why the region was
> mis-assigned. On `meeting-00000001-…` one of three speakers is absorbed from minute ~30 onward
> under both the production global AHC and a sequential online-centroid-tracking prototype. A
> read-only diagnostic (`test_00000001_embedding_drift_diagnostic`) **ruled out** the
> embedding-drift hypothesis originally suspected — the absorbed speaker's OWN late chunks are
> cos ≈ 0.85 to her early centroid (same-speaker range), NOT the ≈ 0.22 figure cited earlier
> (which was the mean cosine of ALL late chunks to her centroid, low only because most late
> chunks belong to other speakers). The root cause is not yet determined and is filed as a
> separate change; do NOT re-attempt a label-level fix for sustained absorption without first
> establishing the cause.

#### Scenario: Per-chunk flicker is eliminated

- **GIVEN** a clustering output with a 40 % singleton-run rate in an acoustically stable region
- **WHEN** temporal-coherence smoothing runs
- **THEN** the singleton-run rate in that region drops below 5 %
- **AND** genuine speaker turns (strong acoustic shift, duration at or above the minimum-segment floor) are preserved

#### Scenario: Genuine turn is not over-smoothed, including short interjections

- **GIVEN** a genuine speaker change with a strong acoustic shift and duration just above `MIN_SMOOTH_SEGMENT_SECS`
- **WHEN** temporal-coherence smoothing runs
- **THEN** the turn is preserved and is not merged into the neighbor
- **AND** a short interjection (run below the floor) sandwiched between two DIFFERENT speakers is also preserved, not merged

#### Scenario: Degenerate embeddings do not corrupt the vote

- **GIVEN** a chunk whose embedding contains NaN or Inf values
- **WHEN** temporal-coherence smoothing runs
- **THEN** the degenerate embedding contributes 0.0 to the vote
- **AND** the vote outcome is determined by the finite neighbors

#### Scenario: Degenerate timestamps do not corrupt the temporal ordering

- **GIVEN** a chunk array containing a NaN or Inf timestamp (e.g., from garbled Whisper output)
- **WHEN** temporal-coherence smoothing runs
- **THEN** the degenerate-timestamp chunk is excluded from neighbor windows rather than corrupting the sort
- **AND** the smoothing does not panic

#### Scenario: Cluster count never increases

- **GIVEN** a clustering output with K clusters
- **WHEN** temporal-coherence smoothing runs
- **THEN** the smoothed output has at most K clusters
- **AND** a cluster that loses all its chunks under smoothing is dropped rather than preserved as a zero-duration phantom

#### Scenario: Stored centroids are post-smoothing

- **WHEN** diarization completes with temporal-coherence smoothing
- **THEN** the centroids stored in `speaker_embeddings` equal the recomputed post-smoothing centroids
- **AND** cross-meeting matching uses de-contaminated voice profiles

#### Scenario: Long meeting smoothing stays bounded

- **GIVEN** a meeting at the chunk cap (`MAX_DIARIZATION_CHUNKS` = 600)
- **WHEN** temporal-coherence smoothing runs with up to the iteration cap
- **THEN** the smoothing and centroid recompute complete in sub-second wall-clock time, consistent with the O(n·W·K) cost bound

#### Scenario: Clean meeting is a near-no-op

- **GIVEN** a meeting whose clustering output is already temporally coherent (well-separated speakers, no flicker, no contamination)
- **WHEN** temporal-coherence smoothing runs
- **THEN** the output labels are unchanged except for a negligible fraction of chunks
- **AND** the centroids are unchanged
- **AND** well-separated speakers (centroid cosine < 0.3) whose runs meet the minimum-segment floor are never merged

## MODIFIED Requirements

### Requirement: Centroid embeddings are stored per speaker per meeting for cross-meeting matching

The diarization processor SHALL return centroid embeddings. Centroids are duration-weighted averages of per-chunk embeddings, computed during agglomerative clustering **and refined by the temporal-coherence smoothing pass before storage** (see the temporal-coherence requirement below). The stored centroids SHALL be the post-smoothing recomputed values, not the pre-smoothing clustering centroids, so that cross-meeting matching operates on de-contaminated voice profiles. They SHALL be stored in the `speaker_embeddings` table as BLOBs with the cluster label and source meeting ID.

Embedding dimensions are model-dependent (not hardcoded). The storage layer SHALL accept any dimension in range [64, 1024] and validate that all values are finite.

When a user labels a speaker (e.g., "Speaker 0" → "Alice"), the system SHALL create or update a `speakers` table row with the name and persistent color, and link the corresponding `speaker_embeddings` row to the named speaker.

#### Scenario: Centroids stored after temporal-coherence refinement

- **WHEN** diarization identifies 3 speakers in a meeting
- **THEN** 3 rows are inserted into `speaker_embeddings`, each containing the duration-weighted centroid embedding for that cluster AFTER the temporal-coherence pass has recomputed it from cleaned labels, the source meeting ID, and a generated cluster label ("Speaker 0", "Speaker 1", "Speaker 2")

#### Scenario: Labeling a speaker creates a named profile

- **WHEN** the user labels "Speaker 0" as "Alice"
- **THEN** a row is inserted or updated in `speakers` with `name = "Alice"` and a persistent color from the palette
- **AND** the corresponding `speaker_embeddings` row is linked to the named speaker via `speaker_id`
- **AND** all transcript rows with `speaker_label = "Speaker 0"` in that meeting are updated to `speaker_label = "Alice"`

---

## ADDED Requirements

### Requirement: Temporal-coherence smoothing prevents clustering contamination and per-chunk flicker

After global agglomerative clustering assigns per-chunk speaker labels, the system SHALL apply a temporal-coherence smoothing pass to the per-chunk labels INSIDE `sherpa_adapter.rs::process()` immediately after `cluster_by_centroids` and BEFORE per-chunk labels are coalesced into `SpeakerSegment` objects. The smoothing SHALL be a pure function of the chunk labels, chunk embeddings, chunk timestamps, and cluster centroids, with no I/O. The smoothing SHALL NOT increase the cluster count, and SHALL preserve genuine speaker turns whose acoustic shift is strong and whose duration meets the minimum-segment floor. The output SHALL be deterministic.

The smoothing pass SHALL perform neighborhood-voted re-assignment: for each chunk i with current label L_i, the system SHALL compute, for each candidate label k, a vote `score(k) = Σ_{j ∈ window(i)} cos(e_j, centroid_k) · w(i,j)` where the window spans the chunk itself (j = i) and its ±W temporal neighbors (default W = 3), `e_j` is chunk j's embedding, and `w(i,j)` is an exponential decay peaked at the neighbors with the SELF term DAMPED below the neighbor peak (self weight default 0.5 vs neighbor peak 1.0). The self term is damped, not excluded: a contaminated chunk's self-fit to its (wrong) centroid is low, so unanimous neighbors still outvote it and recover the chunk (absorption recovery); a genuine short interjection's self-fit to its own distinct centroid is high, so the split neighbor votes on either side cannot beat it by the confidence margin and the interjection is preserved rather than merged away. Using ONLY the chunk's own embedding (no neighbors) would reduce the vote to nearest-centroid and fix nothing; using ONLY neighbors (no self) would erase genuine short interjections between two different speakers, reintroducing the over-merging the pass exists to prevent. The system SHALL reassign the chunk's label only when the winning label's score exceeds the current label's score by a positive confidence margin, so that on a clean, high-confidence input no chunk flips (the pass is a near-no-op). The winner SHALL be chosen deterministically (highest score, ties broken by smallest label) so the output is independent of HashMap iteration order. The system SHALL then recompute duration-weighted centroids from the cleaned labels and iterate the re-assign/recompute cycle up to a fixed cap (default 2 iterations) so that recovered chunks refine the centroids used in the next pass.

After the iteration, the system SHALL merge a same-label run shorter than `MIN_SMOOTH_SEGMENT_SECS` (default ~10 s) into a neighbor ONLY when both adjacent runs share the same label as each other (a flicker island). The system SHALL NOT merge a short run sandwiched between two different speakers (a genuine interjection); such a run is preserved by the damped-self vote's margin gate, so the floor need not (and must not) merge it.

Non-finite (NaN or Inf) embedding values in the smoothing window SHALL contribute 0.0 to the vote, so a degenerate chunk cannot corrupt the outcome. Non-finite timestamp values SHALL exclude that chunk from the window rather than corrupting the temporal ordering or panicking.

#### Scenario: Early contamination seed is absorbed

- **GIVEN** a meeting where the t=0 chunk is assigned to a spurious cluster but its ±W temporal neighbors are consistently cluster 0
- **WHEN** temporal-coherence smoothing runs
- **THEN** the t=0 chunk is reassigned to cluster 0
- **AND** no spurious cluster persists from the contamination seed

#### Scenario: Absorbed speaker is recovered mid-meeting

- **GIVEN** a 3-speaker meeting where speaker C has a clean early run defining C's centroid, and C's chunks from minute 30 onward are consistently mis-assigned to cluster B due to B's centroid drift
- **WHEN** temporal-coherence smoothing runs
- **THEN** at least 80 % of C's mis-assigned chunks are recovered to C's cluster
- **AND** C's cluster does not vanish in the minute 30+ region

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

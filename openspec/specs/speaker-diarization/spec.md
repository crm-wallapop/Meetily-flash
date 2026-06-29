# speaker-diarization Specification

## Purpose
TBD - created by archiving change speaker-diarization. Update Purpose after archive.
## Requirements
### Requirement: Transcript-timestamp-driven speaker diarization runs as a post-processing queue phase

After the transcription and summarisation phases complete, the system SHALL run offline speaker diarization on the meeting's `audio.mp4` as a `Diarizing` phase in the transcription queue. The diarization phase SHALL:

1. Decode the audio to 16kHz mono f32 samples via `DecodedAudio::to_whisper_format()`
2. Read transcript timestamps from the `transcripts` table to define speech segments
3. Chunk each segment into pieces sized at the **effective split granularity** = `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)`, where `speech_seconds` is the total transcript-segment duration and `MAX_DIARIZATION_CHUNKS` caps the total chunk count. The granularity thus stays at `SPLIT_TARGET_SECS` for short meetings and coarsens just enough to keep the chunk count at or below `MAX_DIARIZATION_CHUNKS` for long meetings. Each piece remains within [`MIN_SPEECH_SECS`, `MAX_CHUNK_SECS`].
4. Extract a speaker embedding for each chunk via `SpeakerEmbeddingExtractor` (nemo_titanet; see model-selection requirement)
5. Cluster chunks using centroid-based agglomerative clustering with duration-weighted averaging. The clustering SHALL use a **cached** pairwise similarity scheme — the similarity between alive clusters is computed once and recomputed only for the newly-merged cluster on each merge, not via a full per-merge pairwise rescan — so that its total cost is bounded and it completes in bounded wall-clock time for any meeting length. The clustering SHALL run off the async executor (on a blocking thread) so it can never freeze the UI or block other queue work.
6. Merge short-duration speakers into their cosine-nearest larger cluster
7. Align transcript rows with diarization speaker segments

The clustering output (per-chunk labels and duration-weighted centroids) SHALL be identical regardless of the cached-similarity optimization internals — the optimization changes cost, not results. The diarization phase SHALL be skipped if no `audio.mp4` exists (e.g., `auto_save = false`). The diarization phase SHALL run on imported audio files using the same queue path.

#### Scenario: Diarization runs after summarisation

- **WHEN** a queue job completes the `Summarising` phase successfully
- **THEN** the job transitions to `phase = "diarizing"` and diarization begins on the meeting's `audio.mp4`
- **AND** a `transcription-queue-changed` event is emitted with the updated phase

#### Scenario: Diarization runs directly after transcription when no summary provider

- **WHEN** a queue job completes the `Transcribing` phase AND no LLM provider is configured
- **THEN** the job transitions to `phase = "diarizing"` (skipping `Summarising`)
- **AND** diarization begins on the meeting's `audio.mp4`

#### Scenario: Diarization is skipped when no audio file exists

- **WHEN** a queue job reaches the `Diarizing` phase AND the meeting has no `audio.mp4` (e.g., `auto_save = false`)
- **THEN** the diarization phase is skipped
- **AND** the job transitions to `status = "done"`

#### Scenario: Diarization runs on imported audio

- **WHEN** an audio file is imported as a new meeting AND the import triggers transcription
- **THEN** the queue job includes the `Diarizing` phase after transcription/summarisation
- **AND** diarization produces speaker labels for the imported audio

#### Scenario: Long meeting does not stall in clustering

- **GIVEN** a meeting with ~83 minutes of speech that would produce ~1500 chunks at a fixed 3 s granularity
- **WHEN** diarization runs the clustering step
- **THEN** the effective split granularity is coarsened so the chunk count is at or below `MAX_DIARIZATION_CHUNKS`
- **AND** the clustering step completes in bounded wall-clock time (seconds, not minutes or hours)
- **AND** a `clustering produced N speakers from M chunks` log line is emitted (the prior failure mode where this line never appeared is gone)

#### Scenario: Short meeting is unaffected by the chunk cap

- **GIVEN** a meeting with ~10 minutes of speech
- **WHEN** diarization computes the effective split granularity
- **THEN** the effective granularity equals `SPLIT_TARGET_SECS` (3.0 s) — unchanged from before this change
- **AND** the chunk count is identical to a fixed-3 s chunker

#### Scenario: Cached clustering is behaviour-identical to the naive rescan

- **GIVEN** the same set of chunk embeddings and the same merge threshold
- **WHEN** clustering runs via the cached-similarity implementation
- **THEN** the resulting per-chunk labels and duration-weighted centroids are identical to those produced by a full per-merge pairwise rescan (verified by a property test against a naive oracle kept under `#[cfg(test)]`)

#### Scenario: Clustering does not freeze the UI

- **GIVEN** a diarization run whose clustering step takes several seconds
- **WHEN** clustering executes
- **THEN** the async runtime and UI remain responsive because clustering runs on a blocking thread, not the executor

---

### Requirement: Short-duration noise speakers are merged into nearest cluster

After clustering, speakers with total speech duration below `MIN_CLUSTER_FRAC × total_audio_secs` (default 2%) SHALL be merged into their cosine-nearest larger cluster. The absolute floor SHALL be `MIN_SPEECH_SECS` (1.5s) — the model's own minimum embedding input.

After merging, adjacent segments with the same speaker SHALL be coalesced, and speaker IDs SHALL be renumbered in temporal first-appearance order.

#### Scenario: Noise speakers merged in 3-speaker meeting

- **GIVEN** clustering produces 7 speakers where 4 have total duration < 3s each and 3 have > 100s each
- **WHEN** the short-speaker merge runs
- **THEN** the 4 short speakers are reassigned to their cosine-nearest large speaker
- **AND** the final output has exactly 3 speakers

---

### Requirement: Token-level timestamps align transcript text with diarization speaker boundaries

The diarization processor SHALL read token timestamps from the `transcripts` table (stored by the Whisper provider) and align each word with the diarization speaker segment whose time range contains the word's timestamp. When a Whisper segment spans multiple speakers, the text SHALL be split at the speaker change boundary, producing separate transcript rows per speaker.

When token timestamps are unavailable (e.g., Parakeet provider), the processor SHALL fall back to segment-level timestamps with proportional text-split as a degraded alignment mode.

#### Scenario: Single-speaker Whisper segment

- **GIVEN** a Whisper segment with `audio_start_time = 5.0`, `audio_end_time = 9.0`, and all token timestamps fall within diarization speaker "Speaker 0" (5.0–9.0)
- **WHEN** the diarization processor aligns the segment
- **THEN** the transcript row is assigned `speaker_label = "Speaker 0"` without splitting

#### Scenario: Multi-speaker Whisper segment split at boundary

- **GIVEN** a Whisper segment with `audio_start_time = 5.0`, `audio_end_time = 9.0`, and token timestamps show words at [5.0, 5.2, 5.4, 7.3, 7.5, 7.7]
- **AND** diarization shows "Speaker 0" at 5.0–7.1 and "Speaker 1" at 7.2–9.0
- **WHEN** the diarization processor aligns the segment
- **THEN** the original transcript row is replaced by two rows:
  - Row 1: text from tokens 5.0–5.4, `speaker_label = "Speaker 0"`, `audio_start_time = 5.0`, `audio_end_time = 7.1`
  - Row 2: text from tokens 7.3–7.7, `speaker_label = "Speaker 1"`, `audio_start_time = 7.2`, `audio_end_time = 9.0`

#### Scenario: Parakeet fallback with proportional split

- **GIVEN** a Whisper segment with no token timestamps (Parakeet provider), `audio_start_time = 5.0`, `audio_end_time = 9.0`
- **AND** diarization shows "Speaker 0" at 5.0–7.2 and "Speaker 1" at 7.2–9.0
- **WHEN** the diarization processor aligns the segment
- **THEN** the text is split proportionally (2.2s / 4.0s = 55% of words to Speaker 0)

---

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

### Requirement: Cross-meeting speaker matching uses embedding similarity

After diarization assigns anonymous speaker labels ("Speaker 0", etc.), the system SHALL compare each speaker cluster's centroid embedding against all named speakers in the `speakers` table using cosine similarity. Matches above the threshold SHALL auto-label the speaker with the matched name.

The threshold SHALL default to 0.40 and SHALL be configurable via advanced settings in range [0.35, 0.70].

#### Scenario: Matching speaker auto-labeled

- **GIVEN** "Alice" exists in the `speakers` table with stored embeddings
- **WHEN** diarization produces a speaker cluster with centroid embedding cosine similarity ≥ 0.60 to Alice
- **THEN** the speaker is labeled "Alice" directly

#### Scenario: No match produces cluster label

- **GIVEN** no speakers in the registry have similarity ≥ threshold
- **WHEN** diarization produces a speaker cluster
- **THEN** the speaker keeps its cluster label ("Speaker 0")

---

### Requirement: Retroactive speaker labeling via inline badges with per-speaker revert

The frontend SHALL render an inline speaker badge next to each transcript segment. The badge SHALL display the current speaker label ("Speaker 0", "Alice", "Unknown Speaker"). Clicking the badge SHALL open an inline input to type a new name or select from existing named speakers.

When the user assigns a name, the frontend SHALL invoke `label_speaker(meeting_id, cluster_label, speaker_name)`, which creates/updates the `speakers` row, links embeddings, and updates all transcript rows for that cluster in the meeting. The original cluster label SHALL be preserved in a `previous_label` column on each transcript row (set only once, on first manual label).

The inline input SHALL show suggestion chips of existing named speakers (excluding auto-generated "Speaker N" labels). Selecting an existing speaker name SHALL merge the current cluster into that speaker — all transcript segments for the cluster are relabeled to the selected name. This is an intentional merge action, not a rename.

Manually-named speaker badges SHALL show a small undo icon (visible on hover) that reverts that speaker to its original auto-generated cluster label. Clicking the icon SHALL invoke `revert_speaker_label(meeting_id, speaker_label)`, which restores all transcript rows for that speaker in the meeting to their `previous_label`, sets `speaker_source` to `NULL`, and unlinks the corresponding embedding. The undo icon SHALL NOT appear on auto-generated labels ("Speaker N") or when `previous_label IS NULL`.

#### Scenario: Label an unknown speaker

- **WHEN** the user clicks the "Speaker 0" badge and types "Alice"
- **THEN** the badge updates to "Alice" with Alice's persistent color
- **AND** all transcript segments from "Speaker 0" in this meeting update to "Alice"

#### Scenario: Merge a cluster into an existing speaker via suggestion chip

- **GIVEN** a meeting where "Speaker 0" was renamed to "Alice" and "Speaker 1" was renamed to "Bob"
- **WHEN** the user clicks the "Speaker 2" badge and selects "Alice" from the suggestion chips
- **THEN** all transcript segments from "Speaker 2" are relabeled to "Alice"
- **AND** "Speaker 2" is effectively merged into "Alice" for this meeting

#### Scenario: Re-label a previously named speaker

- **WHEN** the user clicks the "Alice" badge and types "Bob"
- **THEN** the badge updates to "Bob"
- **AND** the `speakers` row for this cluster is updated to `name = "Bob"`
- **AND** all transcript segments in this meeting update to "Bob"
- **AND** the embedding previously linked to "Alice" is now linked to "Bob" for this meeting only — other meetings with "Alice" are unaffected

#### Scenario: Revert a named speaker to original cluster label

- **GIVEN** a meeting where the user manually renamed "Speaker 0" → "Alice"
- **WHEN** the user hovers over the "Alice" badge and clicks the undo icon
- **THEN** all transcript rows with `speaker_label = "Alice"` in that meeting revert to `speaker_label = "Speaker 0"`
- **AND** `speaker_source` is set to `NULL`
- **AND** `previous_label` is cleared to `NULL`
- **AND** the corresponding embedding is unlinked (`speaker_id = NULL`)

#### Scenario: Revert after merge restores different original labels

- **GIVEN** a meeting where "Speaker 0" was renamed to "Alice" and "Speaker 2" was also renamed to "Alice"
- **WHEN** the user reverts "Alice"
- **THEN** some transcript rows revert to "Speaker 0" and others revert to "Speaker 2" (each row has its own `previous_label`)
- **AND** the two original clusters are restored independently

#### Scenario: Revert disabled for auto-generated labels and legacy manual labels

- **GIVEN** a transcript segment with `speaker_label = "Speaker 0"` (auto-generated) or a manual label from before the `previous_label` migration (where `previous_label IS NULL`)
- **THEN** the undo icon is not shown on the badge

#### Scenario: Full reset clears previous_label

- **WHEN** the user triggers re-diarization (Speakers button)
- **THEN** all `previous_label` values are cleared along with `speaker_label` and `speaker_source`

---

### Requirement: Re-diarization cleans up stale state and resets speaker labels

When the user triggers "re-diarize" (Speakers button) on a meeting, the system SHALL perform a full reset:

1. Clear **all** speaker labels on transcript rows (both `"auto"` and `"manual"`)
2. Delete all embeddings in `speaker_embeddings` for that meeting (stale centroids from previous runs)
3. Delete auto-generated speaker rows (`speaker-auto-{meeting_id}-*`) from the `speakers` table
4. Re-run offline diarization on the full audio
5. Store fresh centroid embeddings from the new clustering
6. Match new speaker clusters against existing named speakers by embedding similarity

The system SHALL emit a `diarization-complete` event with the updated speaker assignments.

#### Scenario: Re-diarize resets all labels to fresh cluster labels

- **GIVEN** a meeting where the user manually corrected "Speaker 1" → "Bob" and "Speaker 0" → "Alice"
- **WHEN** the user triggers re-diarization (Speakers button)
- **THEN** ALL speaker labels are cleared (including "Bob" and "Alice")
- **AND** stale embeddings and auto-generated speaker rows are deleted
- **AND** diarization runs fresh, producing new "Speaker 0", "Speaker 1", etc. labels
- **AND** if embedding similarity matches a cluster to a known speaker (e.g., "Alice" exists in the registry from another meeting), that label is auto-applied

#### Scenario: Re-diarize re-labels auto-assigned rows

- **GIVEN** a meeting where "Speaker 0" was auto-assigned to "Alice" with `speaker_source = "auto"`
- **WHEN** the user triggers re-diarization
- **THEN** all labels are cleared and diarization runs fresh
- **AND** new clusters are labeled based on the fresh embedding match

---

### Requirement: Speaker model selection and download

The system SHALL use a single embedding model: `nemo_titanet` (NeMo Titanet Small EN VoxCeleb, ~40 MB). The pyannote segmentation model (~6 MB) SHALL be a required download. Both models SHALL be downloaded during onboarding Step 3 or on first use if skipped during onboarding.

No user-facing model selector SHALL exist. The embedding model is hardcoded; speaker count is controlled by the merge threshold and max_speakers settings, not by model choice.

Existing databases with a `speaker_embedding_model` column holding a legacy value (e.g., `3dspeaker`) SHALL be migrated to `nemo_titanet` on upgrade. The column is retained for backward compatibility but no longer read by the diarization code.

#### Scenario: Onboarding downloads required models

- **WHEN** the user reaches onboarding Step 3
- **THEN** the pyannote segmentation model and the nemo_titanet embedding model are downloaded alongside Parakeet and Gemma

#### Scenario: Model download failure is graceful

- **WHEN** the speaker model download fails during onboarding
- **THEN** onboarding completes normally
- **AND** the diarization phase is skipped for subsequent recordings until the model is downloaded
- **AND** a warning is logged

#### Scenario: Legacy model value migrated on upgrade

- **GIVEN** an existing database where `settings.speaker_embedding_model = '3dspeaker'`
- **WHEN** the migration runs on upgrade
- **THEN** the value is updated to `nemo_titanet`
- **AND** subsequent diarization jobs load the nemo_titanet model file

---

### Requirement: Per-speaker persistent colors

Each speaker in the `speakers` table SHALL have a `color` field assigned using golden-angle HSL distribution (`hue = index × 137.508 mod 360`, saturation 65%, lightness 55%) when the speaker is first created. The color SHALL be used consistently across all meetings where that speaker appears.

#### Scenario: New speaker gets a color from the palette

- **WHEN** the user labels a speaker for the first time
- **THEN** the speaker is assigned the next available color from the golden-angle palette
- **AND** all transcript segments for that speaker in all meetings display with that color

#### Scenario: Known speaker retains color across meetings

- **GIVEN** "Alice" has `color = "hsl(137, 65%, 55%)"`
- **WHEN** Alice is auto-matched in a new meeting
- **THEN** her badge and transcript segments use the same color

---

### Requirement: Merge threshold configurable in settings

The clustering merge threshold SHALL default to 0.40 and SHALL be configurable via settings in range [0.35, 0.70]. Higher values produce more speakers (more conservative merging). Lower values produce fewer speakers (more aggressive merging). The threshold controls the cosine similarity below which two clusters are merged.

#### Scenario: Default threshold produces correct speaker count

- **GIVEN** a meeting with 3 speakers
- **WHEN** diarization runs with threshold 0.40
- **THEN** 3 speakers are identified after short-speaker merge

#### Scenario: Higher threshold produces more speakers

- **GIVEN** a meeting with 3 speakers
- **WHEN** diarization runs with threshold 0.60
- **THEN** more than 3 speakers are identified (clusters stay separate)

---

### Requirement: max_speakers cap merges most isolated cluster

The effective max_speakers cap for a meeting SHALL be the meeting's per-meeting override (`meetings.max_speakers`) when it is set (NOT NULL), otherwise the global `settings.max_speakers` (default 10, range [2, 20]). When the cluster count after short-speaker merge exceeds the effective cap, the system SHALL reduce the count by repeatedly merging the most isolated cluster — the cluster with the lowest nearest-neighbour centroid cosine similarity — into its nearest neighbour. The cap is an upper bound, not a target: the system SHALL NOT split clusters and SHALL NOT merge clusters when the cluster count is at or below the effective cap. The system SHALL NOT merge the highest-similarity pair, as two real speakers who sound alike can have higher centroid similarity than a noise/outlier cluster, and merging them would destroy separation.

#### Scenario: Excess cluster absorbed without collapsing similar speakers

- **GIVEN** a meeting with 3 speakers where clustering at threshold 0.65 produces 4 clusters
- **AND** two real speakers have centroid sim 0.473 (highest pair)
- **AND** the noise cluster has nearest-neighbour sim 0.327 (lowest)
- **WHEN** the effective max_speakers for the meeting is 3
- **THEN** the noise cluster is merged into its nearest neighbour
- **AND** the two real speakers remain separate

#### Scenario: Per-meeting override takes precedence over global default

- **GIVEN** the global `settings.max_speakers` is 10
- **AND** a meeting has `meetings.max_speakers = 3` (per-meeting override)
- **WHEN** diarization runs on that meeting and produces 5 clusters
- **THEN** clusters are merged down to exactly 3 (the override), not 10 (the global default)

#### Scenario: NULL override falls back to global default

- **GIVEN** the global `settings.max_speakers` is 6
- **AND** a meeting has `meetings.max_speakers IS NULL`
- **WHEN** diarization runs on that meeting and produces 8 clusters
- **THEN** clusters are merged down to 6 (the global default)

#### Scenario: Effective cap above cluster count is a no-op

- **GIVEN** a meeting whose effective max_speakers is 5
- **WHEN** diarization produces 3 clusters
- **THEN** no merging occurs and the 3 clusters are preserved

#### Scenario: Degenerate centroid from garbled output is clamped, not propagated

- **GIVEN** diarization produces a cluster whose duration-weighted centroid is degenerate (contains NaN or Inf values — e.g. from a garbled, non-silent Whisper chunk whose ONNX embedding extraction numerically underflows or overflows; genuinely silent audio is rejected upstream by the embedding extractor's `is_effectively_silent` energy guard before any embedding is produced)
- **AND** the cluster count exceeds the effective max_speakers cap so the most-isolated-cluster merge runs
- **WHEN** the cap enforcement selects and merges clusters
- **THEN** the cosine similarity between a degenerate centroid and any other centroid SHALL be clamped to a finite 0.0 by two conjuncts acting together: the `norm > 0.0` guard (which catches NaN, since a NaN norm makes the `>` comparison false) AND the `dot.is_finite()` guard (which catches Inf, since an Inf centroid has an Inf norm that passes `norm > 0.0` and would otherwise yield Inf/Inf = NaN at the division) — both conjuncts are required, so the degenerate cluster ranks as most-isolated (0.0) rather than corrupting the isolation ranking with a NaN
- **AND** the degenerate cluster SHALL be absorbed into its nearest neighbour with both the survivor's and the absorbed centroid's values clamped to finite (a non-finite value contributes 0.0, not its non-finite geometry, so the survivor's centroid is not corrupted)
- **AND** every surviving centroid SHALL remain finite after the cap completes, so the degeneracy cannot cascade into the remaining clusters on subsequent merges nor reach the `speaker_embeddings` table (whose storage layer requires all values finite)

> **Scope:** This scenario governs the cap-enforcement path only (`cosine_similarity_centroids` in `commands.rs`, whose sole non-test caller is `enforce_max_speakers_cap`). The upstream clustering and short-speaker-merge paths use a separate similarity helper without the `dot.is_finite()` conjunct; their defense against non-finite values is `is_effectively_silent` (which rejects silence but not garbled non-silent audio that can still yield a non-finite ONNX output) plus the `speaker_embeddings` storage finite-check, which rejects non-finite values at persistence time.

### Requirement: Re-transcription clears and re-enqueues diarization

When a user re-transcribes a meeting with a different model, the system SHALL clear all speaker labels from the meeting's transcript rows and re-enqueue a `Diarizing` phase job for that meeting.

#### Scenario: Re-transcription triggers re-diarization

- **WHEN** the user re-transcribes a meeting that has speaker labels
- **THEN** all transcript rows for that meeting have `speaker_label` set to `NULL` and `speaker_source` set to `NULL`
- **AND** a diarization job is enqueued for the meeting

### Requirement: Per-meeting max_speakers override is configurable

Each meeting SHALL carry an optional max_speakers override stored as a nullable `meetings.max_speakers INTEGER` column. The override SHALL be settable and clearable via `set_meeting_max_speakers(meeting_id, cap)`, where `cap` is either an integer in [2, 20] or `None` (which clears the override to NULL). The system SHALL reject values outside [2, 20] and SHALL reject a `meeting_id` that does not exist in the `meetings` table. A `get_meeting_max_speakers(meeting_id)` query SHALL return the override value (or its absence), the effective cap (override if set, else the global default), and the global default, so the UI can render the current state in a single call.

The frontend SHALL surface the override in the meeting's speaker panel as a "Max speakers" control with an explicit "Auto (use default: N)" option that maps to NULL. Setting the override SHALL persist it immediately; the override SHALL take effect on the next diarization or re-diarization run for that meeting. The override control SHALL NOT trigger re-diarization automatically, because re-diarization clears all speaker labels including manual corrections.

#### Scenario: Set a per-meeting override

- **GIVEN** a meeting exists in the `meetings` table
- **WHEN** the user sets the meeting's max speakers to 3
- **THEN** `meetings.max_speakers` is stored as 3 for that meeting
- **AND** the next diarization run for that meeting uses 3 as the effective cap

#### Scenario: Clear the override to use the global default

- **GIVEN** a meeting with `meetings.max_speakers = 3`
- **WHEN** the user selects "Auto (use default)"
- **THEN** `meetings.max_speakers` is set to NULL
- **AND** the next diarization run uses the global `settings.max_speakers`

#### Scenario: Override is applied on re-diarization

- **GIVEN** a meeting already diarized with the global default (10) that produced 5 speakers
- **AND** the user sets the meeting's max speakers override to 3 and triggers re-diarization
- **THEN** re-diarization runs with effective cap 3
- **AND** the result has at most 3 speakers

#### Scenario: Out-of-range override rejected

- **WHEN** `set_meeting_max_speakers` is called with cap = 1 (or 21)
- **THEN** the call returns an error and `meetings.max_speakers` is left unchanged

#### Scenario: Non-existent meeting rejected

- **WHEN** `set_meeting_max_speakers` is called with a `meeting_id` not present in the `meetings` table
- **THEN** the call returns an error

### Requirement: Inline speaker-label input cancels on blur and preserves suggestion-chip submission

The inline `SpeakerLabelInput` SHALL cancel (dismiss without committing) when its text field loses focus, producing the same effect as pressing Escape; this requirement amends the "Retroactive speaker labeling via inline badges with per-speaker revert" requirement, which governs the open/submit/revert flow but is silent on dismiss mechanics. Cancelling on blur SHALL NOT dispatch `label_speaker`. Suggestion-chip buttons inside the input SHALL suppress the default focus shift on activation (via `preventDefault` on `mousedown`) so that selecting a suggested name submits the name via `onSubmit` rather than triggering blur-cancel and unmounting the input before the chip's click is delivered. Pressing Enter with non-empty text SHALL continue to submit, and pressing Escape SHALL continue to cancel. Pressing Tab (or any focus loss, including clicking a second speaker badge while one input is open) SHALL cancel, consistent with the click-outside semantics.

#### Scenario: Click outside cancels without committing

- **GIVEN** a transcript segment whose speaker badge has been clicked and the `SpeakerLabelInput` is open and focused
- **WHEN** the user clicks elsewhere in the document
- **THEN** the input is dismissed (unmounted)
- **AND** no `label_speaker` command is dispatched

#### Scenario: Typed name is discarded on click-outside

- **GIVEN** the `SpeakerLabelInput` is open with the text "Alice" typed into it
- **WHEN** the user clicks outside the input
- **THEN** the input is dismissed
- **AND** `label_speaker` is NOT dispatched (the typed name is discarded, not accidentally committed)

#### Scenario: Suggestion chip still submits after the blur guard

- **GIVEN** the `SpeakerLabelInput` is open, `knownSpeakers` is non-empty, and at least one suggestion chip matching the current typed text is visible
- **WHEN** the user clicks a visible suggestion chip
- **THEN** `label_speaker` IS dispatched with the clicked chip's name as `speakerName`
- **AND** the input is dismissed after the submit

#### Scenario: Keyboard paths are unchanged

- **GIVEN** the `SpeakerLabelInput` is open with non-empty text
- **WHEN** the user presses Enter
- **THEN** `label_speaker` is dispatched (submit) — unchanged from before this change
- **AND WHEN** the user presses Escape instead
- **THEN** the input is dismissed without dispatching `label_speaker` (cancel) — unchanged from before this change

#### Scenario: Tab and second-badge focus loss cancel (documented trade-off)

- **GIVEN** the `SpeakerLabelInput` is open with text typed into it
- **WHEN** the user presses Tab, or clicks a second speaker badge while the first input is open
- **THEN** the first input is dismissed (cancel) without dispatching `label_speaker`
- **AND** this is an intentional, documented trade-off: the input is a transient inline affordance, not a tab-stop in a form flow

### Requirement: Inline speaker-label input supports per-segment override in addition to cluster rename

The inline `SpeakerLabelInput` SHALL offer a scope control that lets the user choose whether a typed name applies to every segment in the current cluster (the existing cluster-rename behavior) or to the single transcript segment whose badge was clicked (a per-segment override); this amends the "Retroactive speaker labeling via inline badges with per-speaker revert" requirement by extending inline labeling from cluster-only to cluster-or-single-segment via the existing `set_segment_speaker` path. The scope control SHALL default to cluster-wide so that the pre-existing rename flow is preserved without regression.

When the user chooses per-segment scope and submits a name, the frontend SHALL invoke `set_segment_speaker(transcript_id, speaker_name)`, which updates exactly one `transcripts` row: it sets `speaker_label` to the submitted name, `speaker_source` to `'manual'`, and `previous_label` to the row's prior `speaker_label` only if `previous_label` was previously `NULL` (set-once). The per-segment override SHALL NOT relabel any other row in the meeting. Suggestion-chip selection SHALL respect the same scope control as typed-name submission.

The submitted name SHALL be persisted via `sqlx` parameterized binding (`?` placeholder), which is the SQL-injection defense; `sanitize_speaker_name` trims, length-checks, and strips HTML but does not itself reject injection strings.

#### Scenario: Default scope is cluster rename (no regression)

- **GIVEN** a transcript segment whose speaker badge has been clicked and the `SpeakerLabelInput` is open
- **WHEN** the user types a name and submits without changing the scope control
- **THEN** `label_speaker` is dispatched with the meeting id, the current cluster label, and the typed name
- **AND** `set_segment_speaker` is NOT dispatched
- **AND** every transcript row in the meeting sharing that cluster label is relabeled

#### Scenario: Per-segment scope overrides exactly one row

- **GIVEN** the `SpeakerLabelInput` is open for a segment whose cluster label is "Speaker 2"
- **WHEN** the user switches the scope control to per-segment and submits the name "Carlos"
- **THEN** `set_segment_speaker` is dispatched with that segment's `transcript_id` and speaker name "Carlos"
- **AND** `label_speaker` is NOT dispatched
- **AND** only that one transcript row is relabeled to "Carlos"; other "Speaker 2" rows in the meeting are unchanged

#### Scenario: Suggestion chip respects per-segment scope

- **GIVEN** the `SpeakerLabelInput` is open with the scope control set to per-segment, `knownSpeakers` is non-empty, and at least one matching suggestion chip is visible
- **WHEN** the user clicks a suggestion chip
- **THEN** `set_segment_speaker` (not `label_speaker`) is dispatched with the chip's name for that segment's `transcript_id`

#### Scenario: Per-segment override sets previous_label exactly once

- **GIVEN** a transcript row with `speaker_label = "Speaker 2"` and `previous_label IS NULL`
- **WHEN** the user applies a per-segment override to "Carlos"
- **THEN** the row's `speaker_label` becomes "Carlos", `speaker_source` becomes `'manual'`, and `previous_label` becomes "Speaker 2"
- **AND WHEN** the user later overrides the same row again to "Bob"
- **THEN** `previous_label` remains "Speaker 2" (set-once), so revert still restores the original cluster label

#### Scenario: Per-segment override is cleared by the re-diarize button (inherited behavior)

- **GIVEN** a transcript row that received a per-segment manual override to "Carlos" (`speaker_source = 'manual'`)
- **WHEN** the user clicks the "Speakers" re-diarize button (which calls `reset_speaker_labels` → `clear_all_speaker_labels`)
- **THEN** the override is cleared along with all other labels (auto and manual), as required by the canonical "Re-diarization cleans up stale state" requirement
- **AND** this change does not alter that behavior; it inherits it

#### Scenario: Per-segment override is revertible via cluster-level revert (for previously-labeled rows)

- **GIVEN** a transcript row overridden per-segment from "Speaker 2" to "Carlos", where the row had a non-null `previous_label`
- **WHEN** the user reverts "Carlos" via the existing badge undo (which calls `revert_speaker_label(meeting_id, "Carlos")`)
- **THEN** that row's `speaker_label` is restored to its own `previous_label` ("Speaker 2")
- **AND** any other rows in the meeting labeled "Carlos" are restored to their own respective `previous_label` values independently

#### Scenario: Known limitation — never-labeled row is not revertible

- **GIVEN** a transcript row with `speaker_label = NULL` and `previous_label IS NULL` (e.g., diarization was skipped)
- **WHEN** the user applies a per-segment override to "Carlos"
- **THEN** `previous_label` is set to the old `speaker_label` which is NULL, so it remains NULL
- **AND** a subsequent `revert_speaker_label` for "Carlos" does NOT restore that row (the `WHERE previous_label IS NOT NULL` guard excludes it), leaving a non-functional undo for that row — a documented limitation

#### Scenario: Hostile speaker name is bound as a parameter, not interpolated

- **WHEN** `set_segment_speaker` is called with a name containing SQL-injection content (e.g., `'; DROP TABLE transcripts; --`)
- **THEN** the name is bound via a `sqlx` `?` placeholder (parameterized query), so it is treated as a literal value
- **AND** no transcript row is modified beyond the targeted id and no table is affected

#### Scenario: Non-existent transcript_id is a safe no-op

- **WHEN** `set_segment_speaker` is called with a `transcript_id` that does not exist in the `transcripts` table
- **THEN** the command returns `Ok(false)` (0 rows affected)
- **AND** no error is raised and no row is mutated

### Requirement: Temporal-coherence smoothing prevents clustering contamination and per-chunk flicker

After global agglomerative clustering assigns per-chunk speaker labels, the system SHALL apply a temporal-coherence smoothing pass to the per-chunk labels INSIDE `sherpa_adapter.rs::process()` immediately after `cluster_by_centroids` and BEFORE per-chunk labels are coalesced into `SpeakerSegment` objects. The smoothing SHALL be a pure function of the chunk labels, chunk embeddings, chunk timestamps, and cluster centroids, with no I/O. The smoothing SHALL NOT increase the cluster count, and SHALL preserve genuine speaker turns whose acoustic shift is strong and whose duration meets the minimum-segment floor. The output SHALL be deterministic.

The smoothing pass SHALL perform neighborhood-voted re-assignment: for each chunk i with current label L_i, the system SHALL compute, for each candidate label k, a vote `score(k) = Σ_{j ∈ window(i)} cos(e_j, centroid_k) · w(i,j)` where the window spans the chunk itself (j = i) and its ±W temporal neighbors (default W = 3), `e_j` is chunk j's embedding, and the weight `w(i,j)` is `self_weight` when j = i (default 0.6) and `exp(-|i−j|)` for neighbors (peak `exp(-1) ≈ 0.368` at the nearest neighbor). The self weight (0.6) is the single strongest vote, but it is low enough that a contaminated chunk's self-fit to its (wrong) centroid is still outvoted by unanimous neighbors (whose combined weight across both sides is up to ~1.106), recovering the chunk (absorption recovery); and it is high enough that it exceeds the neighbor weight on one side alone (~0.553), so a genuine short interjection's self-vote for its own distinct centroid anchors it against split neighbor votes on either side, and an edge-of-array interjection (neighbors on only one side) is likewise preserved. Using ONLY the chunk's own embedding (no neighbors) would reduce the vote to nearest-centroid and fix nothing; using ONLY neighbors (no self) would erase genuine short interjections between two different speakers, reintroducing the over-merging the pass exists to prevent. The system SHALL reassign the chunk's label only when the winning label's normalized score exceeds the current label's normalized score by a positive confidence margin (default 0.03), so that on a clean, high-confidence input no chunk flips (the pass is a near-no-op); the margin is set low enough to recover centroid drift up to cosine ~0.97 against the true centroid, while a clean meeting's self-differential (~0.24 at a between-speaker cosine of 0.6) is well above it, so clean input stays stable. The winner SHALL be chosen deterministically (highest score, ties broken by smallest label) so the output is independent of HashMap iteration order. The system SHALL then recompute duration-weighted centroids from the cleaned labels and iterate the re-assign/recompute cycle up to a fixed cap (default 2 iterations) so that recovered chunks refine the centroids used in the next pass.

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


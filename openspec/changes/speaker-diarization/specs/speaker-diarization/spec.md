# Speaker Diarization — Capability Spec

> Status: **active** — implemented, specs updated to match runtime behavior.

---

## ADDED Requirements

### Requirement: Transcript-timestamp-driven speaker diarization runs as a post-processing queue phase

After the transcription and summarisation phases complete, the system SHALL run offline speaker diarization on the meeting's `audio.mp4` as a `Diarizing` phase in the transcription queue. The diarization phase SHALL:

1. Decode the audio to 16kHz mono f32 samples via `DecodedAudio::to_whisper_format()`
2. Read transcript timestamps from the `transcripts` table to define speech segments
3. Chunk each segment into `SPLIT_TARGET_SECS`-sized pieces (range [`MIN_SPEECH_SECS`, `MAX_CHUNK_SECS`])
4. Extract a 3DSpeaker embedding for each chunk via `SpeakerEmbeddingExtractor`
5. Cluster chunks using centroid-based agglomerative clustering with duration-weighted averaging
6. Merge short-duration speakers into their cosine-nearest larger cluster
7. Align transcript rows with diarization speaker segments

The diarization phase SHALL be skipped if no `audio.mp4` exists (e.g., `auto_save = false`). The diarization phase SHALL run on imported audio files using the same queue path.

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

The diarization processor SHALL return centroid embeddings directly from the clustering step. Centroids are duration-weighted averages of per-chunk embeddings, computed during agglomerative clustering. They SHALL be stored in the `speaker_embeddings` table as BLOBs with the cluster label and source meeting ID.

Embedding dimensions are model-dependent (not hardcoded). The storage layer SHALL accept any dimension in range [64, 1024] and validate that all values are finite.

When a user labels a speaker (e.g., "Speaker 0" → "Alice"), the system SHALL create or update a `speakers` table row with the name and persistent color, and link the corresponding `speaker_embeddings` row to the named speaker.

#### Scenario: Centroids stored during diarization

- **WHEN** diarization identifies 3 speakers in a meeting
- **THEN** 3 rows are inserted into `speaker_embeddings`, each containing the duration-weighted centroid embedding vector for that cluster, the source meeting ID, and a generated cluster label ("Speaker 0", "Speaker 1", "Speaker 2")

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

The system SHALL support four embedding models: `3dspeaker` (default, CAM++ zh-cn, ~39 MB), `nemo_titanet` (NeMo Titanet Small EN VoxCeleb, ~40 MB), `eres2net` (3DSpeaker ERes2Net EN VoxCeleb, ~26 MB), and `wespeaker` (WeSpeaker ResNet34 EN VoxCeleb, ~27 MB). The pyannote segmentation model (~6 MB) SHALL be a required download. All models SHALL be downloaded during onboarding Step 3 or on first use if skipped during onboarding.

The active embedding model SHALL be configurable in settings. Changing the model SHALL NOT trigger re-diarization of existing meetings (user can manually re-diarize).

#### Scenario: Onboarding downloads required models

- **WHEN** the user reaches onboarding Step 3
- **THEN** the pyannote segmentation model and the default embedding model are downloaded alongside Parakeet and Gemma

#### Scenario: Model download failure is graceful

- **WHEN** the speaker model download fails during onboarding
- **THEN** onboarding completes normally
- **AND** the diarization phase is skipped for subsequent recordings until the model is downloaded
- **AND** a warning is logged

#### Scenario: User switches embedding model

- **WHEN** the user selects `wespeaker` in settings
- **THEN** the model is downloaded if not already present
- **AND** subsequent diarization jobs use the new model
- **AND** existing meetings retain their current labels

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

When the cluster count after short-speaker merge exceeds the `max_speakers` setting (default 10, range [2, 20]), the system SHALL reduce the count by repeatedly merging the most isolated cluster — the cluster with the lowest nearest-neighbour centroid cosine similarity — into its nearest neighbour. The system SHALL NOT merge the highest-similarity pair, as two real speakers who sound alike can have higher centroid similarity than a noise/outlier cluster, and merging them would destroy separation.

#### Scenario: Excess cluster absorbed without collapsing similar speakers

- **GIVEN** a meeting with 3 speakers where clustering at threshold 0.65 produces 4 clusters
- **AND** two real speakers have centroid sim 0.473 (highest pair)
- **AND** the noise cluster has nearest-neighbour sim 0.327 (lowest)
- **WHEN** max_speakers is set to 3
- **THEN** the noise cluster is merged into its nearest neighbour
- **AND** the two real speakers remain separate

---

### Requirement: Re-transcription clears and re-enqueues diarization

When a user re-transcribes a meeting with a different model, the system SHALL clear all speaker labels from the meeting's transcript rows and re-enqueue a `Diarizing` phase job for that meeting.

#### Scenario: Re-transcription triggers re-diarization

- **WHEN** the user re-transcribes a meeting that has speaker labels
- **THEN** all transcript rows for that meeting have `speaker_label` set to `NULL` and `speaker_source` set to `NULL`
- **AND** a diarization job is enqueued for the meeting

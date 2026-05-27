# Speaker Diarization — Capability Spec

> Status: **proposed** — new capability introduced by `speaker-diarization`.

---

## ADDED Requirements

### Requirement: Offline speaker diarization runs as a post-processing queue phase

After the transcription and summarisation phases complete, the system SHALL run offline speaker diarization on the meeting's `audio.mp4` as a `Diarizing` phase in the transcription queue. The diarization phase SHALL decode the audio to raw f32 samples, run `OfflineSpeakerDiarization::process`, and produce a list of speaker segments `(start_seconds, end_seconds, speaker_id)`.

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

### Requirement: Token-level timestamps align transcript text with diarization speaker boundaries

The diarization processor SHALL read token timestamps from the `transcripts` table (stored by the Whisper provider) and align each word with the diarization speaker segment whose time range contains the word's timestamp. When a Whisper segment spans multiple speakers, the text SHALL be split at the speaker change boundary, producing separate transcript rows per speaker.

When token timestamps are unavailable (e.g., Parakeet provider), the processor SHALL fall back to segment-level timestamps with proportional text-split as a degraded alignment mode.

#### Scenario: Single-speaker Whisper segment

- **GIVEN** a Whisper segment with `audio_start_time = 5.0`, `audio_end_time = 9.0`, and all token timestamps fall within diarization speaker "Speaker 1" (5.0–9.0)
- **WHEN** the diarization processor aligns the segment
- **THEN** the transcript row is assigned `speaker = "Speaker 1"` without splitting

#### Scenario: Multi-speaker Whisper segment split at boundary

- **GIVEN** a Whisper segment with `audio_start_time = 5.0`, `audio_end_time = 9.0`, and token timestamps show words at [5.0, 5.2, 5.4, 7.3, 7.5, 7.7]
- **AND** diarization shows "Speaker 1" at 5.0–7.1 and "Speaker 2" at 7.2–9.0
- **WHEN** the diarization processor aligns the segment
- **THEN** the original transcript row is replaced by two rows:
  - Row 1: text from tokens 5.0–5.4, `speaker = "Speaker 1"`, `audio_start_time = 5.0`, `audio_end_time = 7.1`
  - Row 2: text from tokens 7.3–7.7, `speaker = "Speaker 2"`, `audio_start_time = 7.2`, `audio_end_time = 9.0`

#### Scenario: Parakeet fallback with proportional split

- **GIVEN** a Whisper segment with no token timestamps (Parakeet provider), `audio_start_time = 5.0`, `audio_end_time = 9.0`
- **AND** diarization shows "Speaker 1" at 5.0–7.2 and "Speaker 2" at 7.2–9.0
- **WHEN** the diarization processor aligns the segment
- **THEN** the text is split proportionally (2.2s / 4.0s = 55% of words to Speaker 1)

---

### Requirement: Speaker embeddings are stored per speaker per meeting for cross-meeting matching

The diarization processor SHALL extract an average embedding for each speaker cluster identified during diarization. The embedding SHALL be stored in the `speaker_embeddings` table with the speaker cluster ID, the embedding vector as a BLOB, and the source meeting ID.

When a user labels a speaker (e.g., "Speaker 1" → "Alice"), the system SHALL create or update a `speakers` table row with the name and persistent color, and link the corresponding `speaker_embeddings` row to the named speaker.

#### Scenario: Embeddings stored during diarization

- **WHEN** diarization identifies 3 speakers in a meeting
- **THEN** 3 rows are inserted into `speaker_embeddings`, each containing the average embedding vector for that speaker cluster, the source meeting ID, and a generated cluster label ("Speaker 1", "Speaker 2", "Speaker 3")

#### Scenario: Labeling a speaker creates a named profile

- **WHEN** the user labels "Speaker 1" as "Alice"
- **THEN** a row is inserted or updated in `speakers` with `name = "Alice"` and a persistent color from the palette
- **AND** the corresponding `speaker_embeddings` row is linked to the named speaker via `speaker_id`
- **AND** all transcript rows with `speaker = "Speaker 1"` in that meeting are updated to `speaker = "Alice"`

---

### Requirement: Cross-meeting speaker matching uses embedding similarity with confidence tiers

After diarization assigns anonymous speaker labels ("Speaker 1", etc.), the system SHALL compare each speaker cluster's average embedding against all named speakers in the `speakers` table using `SpeakerEmbeddingManager::search(embedding, threshold)`. Matches SHALL be classified into tiers:

- **High confidence** (similarity ≥ threshold): speaker is labeled with the matched name directly
- **Medium confidence** (0.5 ≤ similarity < threshold): speaker is labeled as "Unknown Speaker (possibly <name>)"
- **Low confidence** (similarity < 0.5): speaker is labeled as "Unknown Speaker"

The threshold SHALL default to 0.6 and SHALL be configurable via advanced settings.

#### Scenario: High-confidence match auto-labels speaker

- **GIVEN** "Alice" exists in the `speakers` table with stored embeddings
- **WHEN** diarization produces a speaker cluster with embedding similarity = 0.78 to Alice
- **THEN** the speaker is labeled "Alice" directly

#### Scenario: Medium-confidence match shows suggestion

- **GIVEN** "Alice" exists in the `speakers` table
- **WHEN** diarization produces a speaker cluster with embedding similarity = 0.55 to Alice (below threshold 0.6)
- **THEN** the speaker is labeled "Unknown Speaker (possibly Alice)"

#### Scenario: No match produces unknown label

- **GIVEN** no speakers in the registry have similarity ≥ 0.5
- **WHEN** diarization produces a speaker cluster
- **THEN** the speaker is labeled "Unknown Speaker"

---

### Requirement: Retroactive speaker labeling via inline badges

The frontend SHALL render an inline speaker badge next to each transcript segment. The badge SHALL display the current speaker label ("Speaker 1", "Alice", "Unknown Speaker"). Clicking the badge SHALL open an inline input to type a new name or select from existing named speakers.

When the user assigns a name, the frontend SHALL invoke `label_speaker(meeting_id, cluster_label, speaker_name)`, which creates/updates the `speakers` row, links embeddings, and updates all transcript rows for that cluster in the meeting.

#### Scenario: Label an unknown speaker

- **WHEN** the user clicks the "Speaker 1" badge and types "Alice"
- **THEN** the badge updates to "Alice" with Alice's persistent color
- **AND** all transcript segments from "Speaker 1" in this meeting update to "Alice"

#### Scenario: Re-label a previously named speaker

- **WHEN** the user clicks the "Alice" badge and types "Bob"
- **THEN** the badge updates to "Bob"
- **AND** the `speakers` row for this cluster is updated to `name = "Bob"`
- **AND** all transcript segments in this meeting update to "Bob"
- **AND** the embedding previously linked to "Alice" is now linked to "Bob" for this meeting only — other meetings with "Alice" are unaffected

---

### Requirement: Re-diarization preserves manually corrected labels

When the user triggers "re-diarize" on a meeting, the system SHALL re-run offline diarization on the full audio. After re-diarization, the system SHALL match new speaker clusters against existing labeled speakers by embedding similarity. Transcript rows with `speaker_source = "manual"` SHALL NOT be overwritten by the auto-assignment. The system SHALL emit a `diarization-complete` event with the updated speaker assignments.

#### Scenario: Re-diarize preserves manual corrections

- **GIVEN** a meeting where the user manually corrected "Speaker 2" → "Bob"
- **WHEN** the user triggers re-diarization
- **THEN** the re-diarization runs and produces new speaker clusters
- **AND** clusters matching "Bob" by embedding similarity keep the "Bob" label
- **AND** manually corrected rows (`speaker_source = "manual"`) are NOT overwritten

#### Scenario: Re-diarize re-labels auto-assigned rows

- **GIVEN** a meeting where "Speaker 1" was auto-assigned to "Alice" with `speaker_source = "auto"`
- **WHEN** the user triggers re-diarization
- **THEN** "Speaker 1" is re-evaluated against the speaker registry
- **AND** the label is updated based on the new embedding match

---

### Requirement: Speaker model selection and download

The system SHALL support two embedding models: `3dspeaker_speech_campplus_sv_zh-cn_16k-common` (default, ~26 MB) and `wespeaker_zh_cnce_resnet` (~90 MB). The pyannote segmentation model (~50 MB) SHALL be a required download. All models SHALL be downloaded during onboarding Step 3 or on first use if skipped during onboarding.

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

- **WHEN** the user selects `wespeaker_zh_cnce_resnet` in settings
- **THEN** the model is downloaded if not already present
- **AND** subsequent diarization jobs use the new model
- **AND** existing meetings retain their current labels

---

### Requirement: Per-speaker persistent colors

Each speaker in the `speakers` table SHALL have a `color` field assigned from a fixed 10-color categorical palette when the speaker is first created. The color SHALL be used consistently across all meetings where that speaker appears.

#### Scenario: New speaker gets a color from the palette

- **WHEN** the user labels a speaker for the first time
- **THEN** the speaker is assigned the next available color from the palette
- **AND** all transcript segments for that speaker in all meetings display with that color

#### Scenario: Known speaker retains color across meetings

- **GIVEN** "Alice" has `color = "#0EA5E9"` (sky blue)
- **WHEN** Alice is auto-matched in a new meeting
- **THEN** her badge and transcript segments use the same sky blue color

---

### Requirement: Speaker count auto-detection with user cap

`OfflineSpeakerDiarization` SHALL auto-detect the number of speakers. The system SHALL support a user-configurable maximum speaker cap (default: 8, range: 2–20). When auto-detection exceeds the cap, the diarization SHALL be re-run with the cap as the maximum speaker count. The cap SHALL be configurable in advanced settings.

#### Scenario: Auto-detect within cap

- **GIVEN** the user cap is 8
- **WHEN** diarization auto-detects 5 speakers
- **THEN** 5 speakers are identified and labeled

#### Scenario: Auto-detect exceeds cap

- **GIVEN** the user cap is 4
- **WHEN** diarization auto-detects 7 speakers
- **THEN** diarization is re-run with a maximum of 4 speakers (closest clusters are merged)

---

### Requirement: Re-transcription clears and re-enqueues diarization

When a user re-transcribes a meeting with a different model, the system SHALL clear all speaker labels from the meeting's transcript rows and re-enqueue a `Diarizing` phase job for that meeting.

#### Scenario: Re-transcription triggers re-diarization

- **WHEN** the user re-transcribes a meeting that has speaker labels
- **THEN** all transcript rows for that meeting have `speaker` set to `NULL` and `speaker_source` set to `NULL`
- **AND** a diarization job is enqueued for the meeting

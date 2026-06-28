## ADDED Requirements

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

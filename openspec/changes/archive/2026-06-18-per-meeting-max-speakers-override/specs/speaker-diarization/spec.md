## MODIFIED Requirements

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

## ADDED Requirements

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

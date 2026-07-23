## ADDED Requirements

### Requirement: Onboarding completion persists across launches

Once the user completes first-run setup — i.e. the `complete_onboarding` command writes `completed: true` to the onboarding store — subsequent app launches SHALL NOT re-show the onboarding flow unless the store is explicitly reset (via `reset_onboarding_status_cmd`) or deleted. The client-side status-loading path SHALL set `completed` from the persisted store value as soon as that value is read (before any slow model-verification probes), so that the existing completion guard blocks auto-save from overwriting real persisted state with mount-time defaults.

Auto-saving in-progress state (the user's current step, model-download flags) SHALL remain supported after the persisted status has been read.

#### Scenario: Slow model-verification load does not clobber completed status

- **GIVEN** the onboarding store on disk contains `completed: true`
- **WHEN** the app launches and the async status load — including any model-verification probes that take longer than the auto-save debounce window — runs after the persisted `completed` value has already been read into React state
- **THEN** the store on disk SHALL still contain `completed: true` after the load completes
- **AND** the onboarding flow SHALL NOT be shown

#### Scenario: In-progress step changes are persisted after the status read

- **GIVEN** the user is mid-onboarding (persisted status `completed: false`, `current_step: 2`) and the status load has resolved
- **WHEN** the user advances to step 3
- **THEN** the auto-save effect SHALL persist the updated `current_step` to the store within its debounce window
- **AND** a subsequent launch SHALL resume from the persisted step

#### Scenario: Genuine first launch shows onboarding

- **GIVEN** the onboarding store has no `status` key (never written)
- **WHEN** the app launches and the status load resolves, confirming the store is empty
- **THEN** the onboarding flow SHALL be shown

#### Scenario: Store-read failure falls back to defaults

- **GIVEN** the `get_onboarding_status` invoke itself throws (e.g. transient store failure)
- **WHEN** the error is caught and `completed` remains at its mount-time default (`false`)
- **THEN** the auto-save MAY write `completed: false` to the store
- **AND** if the store previously held `completed: true`, that value is lost until the user re-completes setup
- **NOTE** this is an accepted trade-off: without a successful store read, no client-side mechanism can distinguish "no prior completion" from "read failed." The user re-completes once and the store self-heals.

### Requirement: Affected users self-heal on next completion

Users whose stores were clobbered to `completed: false` by the pre-fix race SHALL see the onboarding flow one final time. Upon re-completing, the `complete_onboarding` command writes `completed: true`, and subsequent launches SHALL read it correctly. No explicit migration or store-version bump is required.

#### Scenario: Previously-clobbered store sticks after re-completion

- **GIVEN** a user's store contains `completed: false` as a result of the pre-fix race
- **WHEN** the user completes the onboarding flow again
- **THEN** the store is written to `completed: true`
- **AND** every subsequent launch SHALL read `completed: true` and skip the onboarding flow

## ADDED Requirements

### Requirement: Debug-only detector simulation seam

The system SHALL provide a detector simulation seam, compiled only when the off-by-default `dev-detector` Cargo feature is enabled, that drives the production detection state machine (`spawn_detector` / `step_detector`) with a synthetic `DetectorObservation` so a developer can trigger `meeting-detected` and `meeting-ended` without joining a real Google Meet call. When the `dev-detector` feature is disabled (the default), the seam — the fake adapter, its controller, and the `__dev_simulate_meeting` Tauri command — SHALL NOT be compiled into the binary, and production detection SHALL be identical to a build without this change.

The seam SHALL expose a `__dev_simulate_meeting(state, title?)` Tauri command (registered only under the feature) where `state = "joined"` sets the observation to a fresh in-call signal — a Meet-titled window, `has_meet_connection = true`, `has_browser_capture_session = true`, and `connection_first_seen_at` equal to the current instant so the conservative app-start guard does not suppress it — and `state = "left"` sets the observation to the **full idle state** — all six fields cleared, matching `DetectorObservation::default()` and the real adapter's idle output (`meet_windows = []`, `has_meet_connection = false`, `has_browser_capture_session = false`, `connection_first_seen_at = None`, `default_title = ""`, `is_turn_exit = false`) — after which the real 15 s UDP debounce applies before `meeting-ended` fires. The `title` argument, when provided, SHALL set the resolved default title and the synthetic window title.

#### Scenario: Seam is absent from a default build

- **GIVEN** the `dev-detector` Cargo feature is disabled (the default)
- **WHEN** the app is compiled and started
- **THEN** the `__dev_simulate_meeting` Tauri command is not registered
- **AND** no fake detector adapter is compiled into the binary
- **AND** production detection behaves identically to a build without this change

#### Scenario: Simulating a join fires meeting-detected and auto-starts a real recording

- **GIVEN** the app is built with the `dev-detector` feature and the detector polling loop is running with the fake adapter
- **WHEN** the developer invokes `__dev_simulate_meeting("joined", Some("Weekly sync"))`
- **THEN** within one poll interval the state machine transitions `Idle → InCall` and emits `meeting-detected` with `default_title = "Weekly sync"`
- **AND** the frontend auto-start banner appears and `start_recording` begins capturing real audio exactly as it would for a genuine call

#### Scenario: Simulating a leave fires meeting-ended after the real debounce

- **GIVEN** the fake detector is in the `joined` state and a detector-started recording is active
- **WHEN** the developer invokes `__dev_simulate_meeting("left", None)`
- **THEN** the state machine starts the real 15 s UDP debounce
- **AND** after the debounce elapses it emits `meeting-ended`
- **AND** the frontend shows the stop-prompt banner for the detector-started recording

#### Scenario: The real state-machine semantics are exercised unchanged

- **GIVEN** the app is built with the `dev-detector` feature
- **WHEN** the fake observation is driven through a join → leave script
- **THEN** the cancel-suppression, pre-existing-connection, and debounce behaviours are governed by the unmodified `step_detector`
- **AND** no parallel mock event path bypasses the state machine

#### Scenario: Unknown state value is rejected

- **GIVEN** the app is built with the `dev-detector` feature and the fake detector is running
- **WHEN** the developer invokes `__dev_simulate_meeting("paused", None)` (or any value other than `"joined"` / `"left"`)
- **THEN** the command returns an error before the shared observation is mutated
- **AND** no `meeting-detected` or `meeting-ended` event is emitted

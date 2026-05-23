## MODIFIED Requirements

### Requirement: Scheduler gates pause background work when the system is busy

A scheduler SHALL gate transcription and summary job execution. The gates active and the thresholds used SHALL be determined by the user-configured `scheduling_mode` setting:

- **`aggressive`**: active gates are `recording_active`, `meeting_detected`, `manual_pause`. CPU and RAM gates are disabled.
- **`polite`** (default): all five gates are active: `recording_active`, `meeting_detected`, `cpu_high`, `ram_high`, `manual_pause`. The CPU and RAM thresholds AND sustained durations are read from settings (`cpu_pause_threshold_pct`, `cpu_pause_duration_secs`, `ram_pause_threshold_pct`, `ram_pause_duration_secs`), with documented defaults of 70 %/30 s and 80 %/30 s respectively.
- **`manual`**: jobs are not picked up automatically by the worker. The user invokes `run_transcription_job_now(meeting_id)` to start a specific job. Once started, the job chains through all phases (Transcribing → Summarising → Done) automatically without requiring another "Run now" click. While running, jobs still observe the `recording_active`, `meeting_detected`, and `manual_pause` gates — only auto-resume of a fresh Pending job is disabled.

Hysteresis behaviour (resume requires sustained-clear samples for the same configured duration) is preserved unchanged.

#### Scenario: Aggressive mode ignores CPU and RAM load

- **GIVEN** `scheduling_mode = "aggressive"`
- **WHEN** CPU is sustained above 95 % for 60 seconds
- **THEN** in-progress transcription jobs continue running
- **AND** the queue worker continues picking up pending jobs

#### Scenario: Polite mode honours configured thresholds

- **GIVEN** `scheduling_mode = "polite"` AND `cpu_pause_threshold_pct = 40` AND `cpu_pause_duration_secs = 10`
- **WHEN** CPU is sustained above 40 % for 10 seconds
- **THEN** in-progress jobs transition to `paused` with `pauseReason = "cpu_high"` at the next chunk boundary
- **AND** the per-meeting UI badge reads `Paused — CPU above 40 % for 10 s`

#### Scenario: Manual mode disables auto-resume but preserves auto-pause

- **GIVEN** `scheduling_mode = "manual"` AND a job has `status = "pending"`
- **WHEN** all gates are clear
- **THEN** the worker does NOT auto-pick up the job
- **WHEN** the user invokes `run_transcription_job_now(meeting_id)` AND no recording is active
- **THEN** the worker runs the specified job
- **WHEN** during that run the user starts a new recording
- **THEN** the running job pauses at the next chunk boundary with `pauseReason = "recording_active"` (the `recording_active` gate still applies in manual mode)
- **WHEN** transcription completes and a summary processor is registered
- **THEN** the job automatically chains to the Summarising phase and runs to Done without requiring another "Run now" click

#### Scenario: Scheduler hot-reloads thresholds without restart

- **GIVEN** a transcription job is in progress AND `scheduling_mode = "polite"`
- **WHEN** the user changes `cpu_pause_threshold_pct` from 70 to 30
- **THEN** the scheduler's CPU sample evaluation uses the new threshold within one sampling window (≤5 s)
- **AND** no app restart is required

---

## ADDED Requirements

### Requirement: Per-meeting pauseReason includes the configured thresholds

Per-meeting UI badges showing a job in `paused` status SHALL render the pause reason in human-readable form including the active threshold and duration when applicable.

#### Scenario: CPU pause badge includes the active threshold

- **GIVEN** a job has `status = "paused"` and `pauseReason = "cpu_high"` AND `cpu_pause_threshold_pct = 70` AND `cpu_pause_duration_secs = 30`
- **WHEN** the per-meeting badge renders
- **THEN** the badge text reads `Paused — CPU above 70 % for 30 s`

#### Scenario: Manual pause badge does not include thresholds

- **GIVEN** a job has `status = "paused"` and `pauseReason = "manual"`
- **WHEN** the per-meeting badge renders
- **THEN** the badge text reads `Paused — manually` (no threshold information)

---

### Requirement: Scheduling mode is user-configurable

The system SHALL persist the user's scheduling preferences in the settings store and expose them through the Settings UI under `Advanced > Background processing`.

#### Scenario: Default scheduling mode is polite

- **WHEN** the user first launches the app after upgrade
- **THEN** `scheduling_mode` reads as `"polite"` (the default)
- **AND** threshold settings read as their documented defaults

#### Scenario: Threshold inputs are visible only in polite mode

- **GIVEN** the user is in `Advanced > Background processing`
- **WHEN** the user selects `aggressive` or `manual` mode
- **THEN** the CPU and RAM threshold/duration inputs are hidden
- **WHEN** the user re-selects `polite` mode
- **THEN** the inputs reappear with their previously persisted values

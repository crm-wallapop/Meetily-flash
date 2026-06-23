## ADDED Requirements

### Requirement: TURN-relay latch is scoped and non-blocking
The adapter's per-call `turn_established` latch SHALL be scoped to genuine
in-call observations and SHALL NEVER suppress the entry signal.

- **Entry is unconditional.** The `has_meet_connection` observation returned
  by `current_state()` SHALL equal `has_turn_connection() || (has_meet_connection() && has_browser_capture_session())` regardless of the value
  of `turn_established`. A stale latch SHALL NOT force the entry signal false.
- **Latch set is gated on an in-call discriminator.** `turn_established`
  SHALL be set to `true` only on a poll where a TURN relay is observed AND
  the browser holds an active capture session (`has_turn_connection() && has_browser_capture_session()`). Browser TCP to a Google/GCP IP without an
  active capture session SHALL NOT set the latch.
- **Latch drives only `is_turn_exit`.** The latch exists solely to select the
  4 s TURN exit debounce (`is_turn_exit = !has_turn_connection() && turn_established`) for calls that used a TURN relay. It SHALL be reset to
  `false` by `notify_exit()` on the `InCall → Idle` transition so back-to-back
  calls remain detectable.

#### Scenario: Stale latch does not block detection of a later call
- **WHEN** `turn_established` is `true` (latched by any prior observation)
  AND on a subsequent poll `has_turn_connection()` is `false` BUT
  `has_meet_connection()` and `has_browser_capture_session()` are both `true`
- **THEN** the observation's `has_meet_connection` SHALL be `true` (entry is
  not suppressed) AND the detector can transition `Idle → InCall`

#### Scenario: Background GCP traffic does not set the latch
- **WHEN** `has_turn_connection()` is `true` (browser has a TCP connection to
  a GCP/Google IP) BUT `has_browser_capture_session()` is `false` (no active
  call — e.g. an ordinary Google service in a background tab)
- **THEN** `turn_established` SHALL remain `false` AND a later UDP call's
  exit SHALL use the 15 s UDP debounce (not the 4 s TURN debounce), because
  `is_turn_exit` is `false`

#### Scenario: Genuine TURN call still gets the fast exit debounce
- **WHEN** during a detected call both `has_turn_connection()` and
  `has_browser_capture_session()` are `true` on at least one poll
- **THEN** `turn_established` SHALL be set to `true` AND when the TURN relay
  subsequently drops (`has_turn_connection()` → `false`) `is_turn_exit` SHALL
  be `true` AND the 4 s TURN debounce applies (behaviour preserved)

#### Scenario: notify_exit resets the latch for back-to-back calls
- **WHEN** a TURN call ends AND `notify_exit()` is called on the
  `InCall → Idle` transition
- **THEN** `turn_established` SHALL be reset to `false` AND a subsequent UDP
  call (no TURN relay) SHALL be detected normally with `is_turn_exit = false`

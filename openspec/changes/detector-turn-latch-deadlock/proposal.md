## Why

Auto-detection of Google Meet calls silently stops working after any prior
browser→GCP TCP connection latches an internal flag in the Windows detector
adapter. The user joined a live Meet call and no detection banner appeared;
a fresh app session (which starts with the flag clear) detects fine,
confirming the prior session was "poisoned." Root cause: the sticky
`turn_established` flag is set whenever the browser has *any* TCP connection
to a broad GCP CIDR (the TURN ranges — `34.64.0.0/10`, `35.190.0.0/17`,
`35.191.0.0/16`, `130.211.0.0/22` — also serve ordinary Google services, not
just Meet TURN), then forces the **entry** signal `has_meet_connection=false`
on every subsequent poll. The only reset (`notify_exit()`) runs on the
`InCall → Idle` transition — which can never happen because entry itself is
blocked. It is a self-reinforcing deadlock.

## What Changes

- **Remove the entry-suppression branch.** In
  `WindowsMeetingDetector::current_state()` the `else if turn_established {
  has_conn = false }` branch is deleted. The entry signal becomes
  `turn || (has_meet_connection() && has_browser_capture_session())`
  unconditionally — the latch no longer affects entry. This branch's stated
  rationale ("prevent the exit debounce from ever starting") is stale: since
  the asymmetric-D2 redesign, exit uses `has_browser_capture_session`, never
  `has_meet_connection`, so the branch only ever blocked entry.
- **Gate latch set on an in-call discriminator.** `turn_established` is set
  only when a TURN relay coincides with an active browser capture session
  (`turn && has_browser_capture_session()`), instead of `turn` alone. This
  stops non-Meet GCP traffic from poisoning `is_turn_exit` (which would
  otherwise make real UDP calls exit on the 4 s TURN debounce instead of the
  15 s UDP debounce — a premature-`meeting-ended` regression).
- **Amend the adapter tests that encoded the deadlock as desired behaviour**
  (`otter_ai_persistent_mic_blocked_until_notify_exit`,
  `notify_exit_resets_turn_established_for_next_udp_detection`) and **add
  adversarial adapter tests**: a deadlock-regression test (latched + a real
  UDP call ⇒ entry still fires) and a spurious-latch test (GCP TCP without
  capture ⇒ latch never sets ⇒ `is_turn_exit` stays false).
- The `turn_established` flag itself is **retained** — it still drives
  `is_turn_exit` (the fast 4 s TURN exit debounce). It just no longer blocks
  entry and is no longer poisoned by non-call traffic.

## Capabilities

### New Capabilities
<!-- None -->

### Modified Capabilities
- `meeting-auto-detect`: the entry requirement is strengthened so the entry
  signal can never be permanently suppressed by stale per-call TURN state
  (the deadlock), and the TURN-relay latch is scoped to genuine in-call
  observations so non-Meet GCP traffic cannot corrupt the exit-debounce path.

## Impact

- **Code**: `frontend/src-tauri/src/detection/windows.rs` only — the adapter
  (`current_state`) and its `#[cfg(test)]` block. No change to the
  `MeetingDetectorPort` trait, `DetectorObservation`, the pure
  `step_detector` use case, the dev-detector seam, or any frontend code.
- **Tests**: two existing adapter tests amended; ≥2 adversarial tests added.
- **User-visible**: the detection banner reliably fires across sessions and
  after any background browser traffic. The effect is not webview-assertable
  (it depends on live TCP/WASAPI state), so coverage is the cargo adversarial
  tests plus the existing `dev-detector` simulation seam — no Playwright
  smoke spec is added (rationale recorded in `design.md`).
- **Risk**: low and localized. The only behavioural change for a correctly
  detected call is none; the change only affects the deadlock path and the
  debounce-classification of poisoned latches.

## Why

The adaptive UDP exit debounce shipped in `meeting-auto-detect` is **dead code in production**. The spec promises a 4 s fast exit for stable-mic UDP calls (Scenario "Stable-mic UDP call exits on the short debounce"), but the `stable_capture` flag that selects it is computed on the same detector poll that observes the exit — and the exit *is* a `has_browser_capture_session()` drop, which latches `stable_capture` to `false` on that very poll. So every real UDP exit reads as flaky and waits the full 15 s. The 4 s branch in `step_detector` is never taken for any real call.

The unit and fake-adapter tests pass only because they bypass the real adapter: one polls with capture continuously `true` (never models the exit drop), the other hand-sets `stable_capture = true` on a fake observation. Neither can observe that the real adapter cannot produce `stable_capture = true` on an exit poll.

## What Changes

- **BREAKING (behavioural):** Replace the "any capture drop this call ⟹ flaky" latch with a **recovery-based** latch: a call is marked transient-prone only when a capture drop is followed by a *recovery* (`false → true`). A drop that never recovers is the exit itself and SHALL NOT mark the call flaky. This makes the 4 s stable-UDP exit actually reachable.
- Add a **minimum stable-run guard**: the fast 4 s exit applies only when the capture session was continuously active for at least a confidence window (`STABLE_CONFIDENCE_WINDOW`, ~20 s) immediately before the drop. Short stable runs fall back to 15 s. This covers the case where an early non-recovering drop is actually the leading edge of a transient.
- Latch the exit-confidence decision at the drop so it is **stable across the debounce window** — `step_detector` recomputes the debounce duration on every poll, so the value driving it must not change between the drop poll and the elapsed-threshold poll.
- Remove the now-misleading `stable_capture`-on-exit-poll wiring and the tests that assert the unreachable path; replace with adversarial tests that drive the **real** adapter latch through capture-drop/recovery sequences.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `meeting-auto-detect`: the UDP exit-debounce selection requirement changes — `stable_capture` is redefined from "no capture drop observed this call" to "no *recovered* capture drop this call AND capture was continuously active ≥ `STABLE_CONFIDENCE_WINDOW` before the drop", and the requirement now mandates the value be latched stable across the debounce window.

## Impact

- **Code:** `frontend/src-tauri/src/detection/windows.rs` (`current_state()` — the bc latch + `stable_capture` computation), `frontend/src-tauri/src/use_cases/meeting_detection.rs` (debounce-selection comment/semantics; logic largely unchanged — it already reads `observation.stable_capture`), `frontend/src-tauri/src/detection/fake.rs` and the windows.rs unit tests (replace assertions that encode the unreachable path). The co-committed `device_disconnect_mid_call_latches_unstable_without_confusing_leave` and `rapid_leave_rejoin_within_wasapi_lag_keeps_capture_stable` tests also encode the old drop-edge `bc_drop_observed_this_call` latch this change inverts (Task 1.6); they are rewritten by Task 2.1. The orthogonal `turn_latch_survives_bc_drop_during_exit` (TURN path, unchanged by this proposal) is kept.
- **Spec:** `openspec/specs/meeting-auto-detect/spec.md` delta — the stable-UDP scenarios and the `stable_capture` definition.
- **No change** to: the TURN 4 s path (`is_turn_exit`), the 15 s conservative default, `notify_exit()` reset semantics, any Tauri command, frontend contract, or DB schema.
- **Smoke:** `meeting-auto-detect` is UI-affecting via `meeting-ended` timing; the existing smoke spec's debounce wiring stays valid (it asserts the 15 s default path, which is unchanged for flaky calls).

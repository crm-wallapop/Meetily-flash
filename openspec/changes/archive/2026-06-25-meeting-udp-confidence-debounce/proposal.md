## Why

The adaptive UDP exit debounce shipped in `meeting-auto-detect` is **dead code in production**. The spec promises a 4 s fast exit for stable-mic UDP calls (Scenario "Stable-mic UDP call exits on the short debounce"), but the `stable_capture` flag that selects it is computed on the same detector poll that observes the exit — and the exit *is* a `has_browser_capture_session()` drop, which latches `stable_capture` to `false` on that very poll. So every real UDP exit reads as flaky and waits the full 15 s. The 4 s branch in `step_detector` is never taken for any real call.

The unit and fake-adapter tests pass only because they bypass the real adapter: one polls with capture continuously `true` (never models the exit drop), the other hand-sets `stable_capture = true` on a fake observation. Neither can observe that the real adapter cannot produce `stable_capture = true` on an exit poll.

## What Changes

- **BREAKING (behavioural):** Decide exit stability **once per call, at the first `has_browser_capture_session()` `true → false` drop**, from the length of the unbroken `true` run preceding it: ≥ `STABLE_CONFIDENCE_WINDOW` (~20 s) → `stable_capture = true` (4 s exit); otherwise `false` (15 s). The shipped adapter instead latches on the drop edge itself, so every exit self-marks unstable and the 4 s branch is unreachable — this makes the 4 s stable-UDP exit actually reachable.
- **Lock the decision immutable for the call.** Store it in a per-call `exit_stable_latch: Option<bool>`; once `Some(v)`, hold it unchanged until `notify_exit()` — never cleared by a `false → true` recovery, never recomputed. `step_detector` recomputes the debounce on every poll, so the value MUST NOT flip mid-debounce. (A prior recovery-based draft of this change cleared the latch on a `false → true` edge and recreated the `detector-turn-latch` self-heal trap of commit `693ff90`; immutability closes it. Shark-tank review, 2026-06-25.)
- **`STABLE_CONFIDENCE_WINDOW` is the sole classifier** (a 20 s `const`, above the spec's ~10 s WASAPI transient ceiling). A flaky session drops frequently → short first-drop run → 15 s; a stable session holds capture for minutes → ≥ window → 4 s.
- **Dropped rule (decided 2026-06-25):** the prior "a recovered transient ⟹ 15 s for the rest of the call" semantics is relaxed. Under the locked design, a call that ran stable ≥ window, suffered a recovered transient, then later exits, exits at 4 s (its first drop's run was long). Preserving the old rule would need a new `step_detector → adapter` feedback port — scope-creep this project has deferred (`hexagonal-port-traits`); the locked design is the clean fix. Genuinely flaky calls (short first-drop runs) are unaffected.
- Replace the tests that assert the unreachable path with adversarial tests that drive the **real** adapter latch through drop/recovery/flicker sequences — including a WASAPI-flicker-during-debounce guard (the self-heal-trap regression test), device-disconnect + rapid-leave/rejoin rewrites, boundary-value tests at the window edge, a mid-call-app-start conservative test, a crash-path reconstruction test, and a proptest on the immutability invariant.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `meeting-auto-detect`: the UDP exit-debounce selection requirement changes — `stable_capture` is redefined from "no capture drop observed this call" to "**decided once at the call's first `true → false` drop: `true` iff the preceding unbroken `true` run was ≥ `STABLE_CONFIDENCE_WINDOW`**"; the decision is held **immutable in `exit_stable_latch` until `notify_exit()`** (anti-self-heal); and `STABLE_CONFIDENCE_WINDOW` is the sole flaky/stable classifier.

## Impact

- **Code:** `frontend/src-tauri/src/detection/windows.rs` (`current_state()` — replace `bc_drop_observed_this_call` with `exit_stable_latch` + `bc_true_since`, decided at the first drop and held immutable per design.md D1–D4), `frontend/src-tauri/src/use_cases/meeting_detection.rs` (debounce-selection logic unchanged — it already reads `observation.stable_capture`; update the stale comment + the `DetectorObservation::stable_capture` doc comment), `frontend/src-tauri/src/detection/fake.rs` and the windows.rs unit tests (replace assertions that encode the unreachable path). The co-committed `device_disconnect_mid_call_latches_unstable_without_confusing_leave` and `rapid_leave_rejoin_within_wasapi_lag_keeps_capture_stable` tests encode the old `bc_drop_observed_this_call` latch this change removes; Task 2.1 rewrites them (along with `first_bc_drop_latches_stable_capture_false` and `notify_exit_resets_bc_drop_latch_for_next_call`, which also encode the old latch). The orthogonal `turn_latch_survives_bc_drop_during_exit` (TURN path, unchanged by this proposal) is kept.
- **Spec:** `openspec/specs/meeting-auto-detect/spec.md` delta — the stable-UDP scenarios and the `stable_capture` definition.
- **No change** to: the TURN 4 s path (`is_turn_exit`), the 15 s conservative default, `notify_exit()` reset semantics, any Tauri command, frontend contract, or DB schema.
- **Smoke:** `meeting-auto-detect` is UI-affecting via `meeting-ended` timing; the existing smoke spec's debounce wiring stays valid (it asserts the 15 s default path, which is unchanged for flaky calls).

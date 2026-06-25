# Tasks — meeting-udp-confidence-debounce

Adversarial TDD: each task writes the failing (RED) test first, then the implementation that makes it pass. Run `CL=/FS cargo test --lib detection:: ` (and `meeting_detection::`) from `frontend/src-tauri` to verify.

## 1. Adapter latch redesign (detection/windows.rs)

- [ ] 1.1 RED: `exit_drop_alone_does_not_mark_transient` — poll `bc=true` past the confidence window via a controllable clock, then a single `bc=false` drop (no recovery); assert the observation's `stable_capture == true`. Fails on current code (the drop self-latches `false`).
- [ ] 1.2 RED: `recovered_drop_marks_transient_then_exit_is_15s` — `true`(≥window)→`false`→`true`(recovery)→`true`(≥window)→`false`(exit); assert `stable_capture == false`.
- [ ] 1.3 RED: `short_stable_run_then_drop_is_15s` — `bc=true` for < window, then `bc=false`; assert `stable_capture == false`.
- [ ] 1.4 RED: `stable_capture_latched_stable_across_debounce` — clean stable exit, then several consecutive `bc=false` polls; assert every poll reports the same `stable_capture == true`.
- [ ] 1.5 RED: `notify_exit_resets_stability_state` — trip transient on call A, `notify_exit()`, clean stable call B; assert B reports `stable_capture == true`.
- [ ] 1.6 GREEN: implement per-call fields `bc_true_since`, `transient_observed`, `exit_stable_latch` on `WindowsMeetingDetector`; set `transient_observed` on the `false → true` recovery edge; compute `exit_stable_latch = !transient_observed && run_len >= STABLE_CONFIDENCE_WINDOW` on the `true → false` drop edge; emit `stable_capture = exit_stable_latch.unwrap_or(false)` on `bc == false` polls. Add the `STABLE_CONFIDENCE_WINDOW: Duration` const (20 s).
- [ ] 1.7 GREEN: reset `bc_true_since`, `transient_observed`, `exit_stable_latch` in `notify_exit()`.
- [ ] 1.8 Inject a clock seam for tests — extend `DetectorProbes` (or add a test-only `now` provider) so the continuous-active timer is drivable without real sleeps; production uses `Instant::now()`.

## 2. Remove the unreachable-path tests/assertions

- [ ] 2.1 Replace `detection/windows.rs::stable_call_sets_stable_capture_true` (polls capture continuously true — never models the exit drop) **and** the co-committed `device_disconnect_mid_call_latches_unstable_without_confusing_leave` + `rapid_leave_rejoin_within_wasapi_lag_keeps_capture_stable` tests (which assert the old drop-edge `bc_drop_observed_this_call` monotonic latch that Task 1.6 renames to `transient_observed` and inverts to the recovery edge) with the §1 drop-driven tests. Keep `turn_latch_survives_bc_drop_during_exit` — it exercises the TURN path this proposal does not touch.
- [ ] 2.2 Rewrite `detection/fake.rs::stable_capture_true_drives_short_debounce_to_ended` so it drives the REAL adapter latch through a drop sequence (or move the end-to-end timing assertion to a windows.rs real-clock `#[ignore]` test). Keep `stable_capture_false_holds_ended_through_long_debounce` (the 15 s path is still valid).

## 3. Use-case wiring (use_cases/meeting_detection.rs)

- [ ] 3.1 No logic change required — `step_detector` already reads `observation.stable_capture`. Update the comment at the debounce-selection site to describe the recovery-based + confidence-window semantics (replace the stale "no bc drop observed this call" wording).
- [ ] 3.2 Confirm (test, not code) the per-poll debounce recompute is safe given §1.4's latched-stable guarantee — add `step_detector_stable_capture_drives_4s_when_latched` exercising the InCall branch with `stable_capture=true` across multiple polls.

## 4. End-to-end + smoke

- [ ] 4.1 `#[ignore]` real-clock adapter test: stable exit fires `meeting-ended` within ~5–6 s (4 s debounce + poll slack), not ~16 s. Runs via `cargo test -- --ignored`.
- [ ] 4.2 Confirm the existing `meeting-auto-detect` smoke spec still passes (it asserts the 15 s default path for flaky calls, unchanged). No new smoke spec needed — the change narrows a backend timing path; the event→UI wiring is unchanged. (Per CLAUDE.md §3, verify event wiring is unaffected rather than assuming.)

## 5. Spec sync + archive

- [ ] 5.1 Before archive: re-read `specs/meeting-auto-detect/spec.md` (canonical) and this change's `design.md`; if implementation diverged (e.g. final window value), amend the delta spec and design first.
- [ ] 5.2 `cargo test` green (unit + the new adversarial set); `pnpm test` + `pnpm test:smoke` green.
- [ ] 5.3 `/opsx:archive meeting-udp-confidence-debounce` (syncs the delta into the canonical spec).

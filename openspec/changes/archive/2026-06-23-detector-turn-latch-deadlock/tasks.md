# Tasks — detector-turn-latch-deadlock

> Branch: `fix/detector-turn-latch-deadlock`.
> Adapter-only change in `frontend/src-tauri/src/detection/windows.rs`. Coverage split
> (post-archive correction): the latch/entry-formula LOGIC is Rust-internal (live
> TCP/WASAPI state) and cargo-tested (§1 adversarial tests). The
> `meeting-detected`/`meeting-ended` → AutoDetectBanner WIRING is covered by
> `e2e/smoke/meeting-auto-detect.spec.ts` (a capability-level backfill that also covers
> meeting-udp-media-signal). The original "not webview-assertable" framing was too broad —
> the latch logic isn't webview-assertable, but the detection-result wiring is.

## 1. RED — adversarial adapter tests (must fail on current code)

- [x] 1.1 **Deadlock regression:** `turn_established = true` set directly, then probes return TURN=false, `has_meet_connection=true`, `has_browser_capture_session=true`, a Meet window present. Assert `current_state().has_meet_connection == true`. **RED confirmed** pre-fix: panicked with "a latched turn_established flag must NOT suppress entry when a real UDP call (mc && bc) is active". Test: `latched_turn_flag_does_not_block_subsequent_udp_call_entry`.
- [x] 1.2 **Spurious-latch non-poison:** probes return `has_turn_connection=true` but `has_browser_capture_session=false` (background GCP traffic, no call) for several polls. Assert `turn_established` stays `false` and, after switching to a UDP-call observation (TURN=false), `current_state().is_turn_exit == false`. **RED confirmed** pre-fix: panicked with "turn_established must NOT set on TURN-without-capture". Test: `turn_without_browser_capture_does_not_latch`.
- [x] 1.3 **Invariant (entry formula):** assert `has_meet_connection == turn || (mc && bc)` across the full probe matrix (turn∈{T,F} × mc∈{T,F} × bc∈{T,F}) with `turn_established` true. **RED confirmed** pre-fix: failed at `turn=false mc=true bc=true` (the deadlock cell). Test: `entry_formula_invariant_holds_across_probe_matrix_when_latched`.

## 2. GREEN — implement the fix

- [x] 2.1 Deleted the `else if self.turn_established { … false }` arm. `has_conn = if turn { true } else { mc && bc }` unconditionally. Removed the stale "prevent the exit debounce" rationale; added a WHY comment explaining the arm was a self-reinforcing deadlock (notify_exit only fires on InCall→Idle, which requires entry, which the arm prevented).
- [x] 2.2 Gated the latch set on `turn && bc` (moved after the `bc` computation). Added a WHY comment: `bc` is the in-call discriminator the detector already relies on for UDP entry/exit — non-Meet GCP traffic has no capture session.
- [x] 2.3 `cargo test --lib detection::windows` green: 18/18 (1 CI-only ignored). The §1 RED tests now pass.

## 3. Amend tests that encoded the deadlock as desired behaviour

- [x] 3.1 `otter_ai_persistent_mic_blocked_until_notify_exit`: repurposed. The "must block UDP detection for all polls" assertion is replaced with its post-fix negation ("a latched turn_established must not suppress entry for a real UDP call"). The `notify_exit` reset assertion stays. Added a comment that the Otter/lobby false-positive is the pre-existing known limitation (canonical spec line 107), not latch-suppressed (notify_exit resets the latch at Idle, the same state the lobby scenario starts from).
- [x] 3.2 `notify_exit_resets_turn_established_for_next_udp_detection`: updated the "before notify_exit has_conn=false" assertion to `has_conn == true`. The `notify_exit` → detectable-again assertion stays.
- [x] 3.3 `turn_exit_flag_set_when_turn_drops_after_being_established` passes unchanged — the latch still sets for a genuine TURN call (TURN && bc both true in the test) and `is_turn_exit` still drives the 4 s debounce. **Confirmed GREEN** without any edit.

## 4. Verify

- [x] 4.1 `cargo test --lib` full Tauri crate green: 387/387 (7 ignored). (A first run had 1 flaky failure in `test_4_5_port_panic_does_not_crash_detector` — a pre-existing test-isolation/thread-race issue unrelated to this change; the test passes in isolation and on re-run.)
- [ ] 4.2 Manual QA deferred: a live Meet call after prior browser activity is the ideal verification, but the §1 cargo adversarial tests are the binding proof (they directly assert the post-fix invariants that were broken pre-fix: latched flag no longer suppresses entry, GCP-without-capture no longer poisons the latch, entry formula holds across the probe matrix). Manual QA is optional follow-up; if performed, the deliverable is a captured log line showing `has_meet_connection=true` post-latch during a real Meet call.

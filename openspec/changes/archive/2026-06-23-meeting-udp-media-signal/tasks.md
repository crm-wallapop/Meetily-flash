# Tasks — meeting-udp-media-signal

> Branch: `enhance/meeting-udp-media-signal`.
> Adapter + port-struct + pure-use-case change. Coverage split (post-archive correction):
> the stable_capture latch + adaptive debounce LOGIC is Rust-internal (live WASAPI state)
> and cargo-tested (§1 adversarial tests). The `meeting-detected`/`meeting-ended` →
> AutoDetectBanner WIRING is covered by `e2e/smoke/meeting-auto-detect.spec.ts`. The
> original "not webview-assertable" framing was too broad — the debounce logic isn't
> webview-assertable, but the detection-result wiring is.

## 1. RED — adversarial adapter + use-case tests (must fail on current code)

- [x] 1.1 **Adapter — stable call sets `stable_capture=true`:** with injectable probes, drive a poll sequence where `has_browser_capture_session()` is continuously `true` (no drop). Assert the returned `DetectorObservation.stable_capture == true`. (Fails today: the field does not exist.)
- [x] 1.2 **Adapter — first bc drop latches `stable_capture=false`:** drive `bc` `true → false → true` (a transient) then assert `stable_capture == false` on every subsequent poll, even after `bc` returns `true`. (Fails today: no latch exists.)
- [x] 1.3 **Adapter — `notify_exit` resets the latch:** after a drop has latched `stable_capture=false`, call `notify_exit()` and assert a subsequent no-drop poll yields `stable_capture=true` again (back-to-back calls). (Fails today: no latch to reset.)
- [x] 1.4 **Use case — stable call uses SHORT debounce:** in `step_detector`'s `InCall` branch, with `is_turn_exit=false` and `observation.stable_capture=true` and `has_browser_capture_session=false`, assert the applied UDP debounce duration is the SHORT value (4 s), not 15 s. (Fails today: the branch always uses 15 s.)
- [x] 1.5 **Use case — transient-prone call uses LONG debounce:** same setup but `stable_capture=false`; assert the UDP debounce is 15 s. (Pins the preserved behaviour; fails today: no `stable_capture` input.)
- [x] 1.6 **Use case — invariant matrix:** across `{is_turn_exit, stable_capture} ∈ {T,F}²`, the TURN path (4 s) is unaffected by `stable_capture`, and the UDP path selects 4 s only when `stable_capture=true`. Assert the debounce is a pure function of `(is_turn_exit, stable_capture)`.

## 2. GREEN — implement the adaptive debounce

- [x] 2.1 Add `stable_capture: bool` to `DetectorObservation` in `ports/meeting_detector.rs`. Update every construction site (`Default`, the fake adapter, the `dev-detector` seam, `make_obs` test helper) — default `false` (conservative long debounce).
- [x] 2.2 In `WindowsMeetingDetector`: add `bc_drop_observed_this_call: bool` (reset in `notify_exit` alongside `turn_established`). In `current_state()`, where `last_bc` transitions are already detected, latch it `true` on the first `Some(true) → false` transition. Set `obs.stable_capture = !self.bc_drop_observed_this_call`. Add a one-line `why` comment that `bc` stability is the in-call discriminator (a drop proves the setup is transient-prone) and that the latch is per-call (reset on exit).
- [x] 2.3 In `step_detector` (`use_cases/meeting_detection.rs`) InCall branch: replace the fixed UDP debounce with `if observation.stable_capture { SHORT_UDP_DEBOUNCE } else { LONG_UDP_DEBOUNCE }`, where `SHORT = 4 s` and `LONG = 15 s` (named constants). The TURN-path 4 s and the selection-by-`is_turn_exit` are unchanged.
- [x] 2.4 Populate `stable_capture` in the fake adapter (`detection/fake.rs`) and the `dev-detector` seam so simulation drives the real adaptive selection; default the simulated value to `false` so existing dev flows keep today's 15 s behaviour unless a test sets otherwise.
- [x] 2.5 `cargo test -p app_lib detection::windows` and `cargo test -p app_lib meeting_detection` green (the §1 RED tests now pass; existing tests unchanged because the default is the conservative long debounce).

## 3. Spec correction — line-107 known-limitation note

- [x] 3.1 Confirm the delta spec's `## MODIFIED Requirements` → "Conservative app-start state" block carries the corrected known-limitation note (Decision 5: the `GetExtendedUdpTable` direction is abandoned — no remote addr; QUIC confounds it; ETW/CDP blocked by admin/flag; adaptive debounce is the adopted exit lever). No separate canonical edit is needed — `/opsx:archive` sync applies the MODIFIED requirement to `openspec/specs/meeting-auto-detect/spec.md`. Re-read the synced canonical note after archive and verify it replaced the old text verbatim.

## 4. Verify

- [x] 4.1 `cargo test` (full Tauri crate) green.
- [x] 4.2 Adaptive-debounce selection through the REAL `spawn_detector` loop —
  `stable_capture_true_drives_short_debounce_to_ended` (C1) and
  `stable_capture_false_holds_ended_through_long_debounce` (C2) in
  `detection/fake.rs`, added 2026-06-24 (commit `dedd325`). The §1 unit tests
  (1.4–1.6) pin the `step_detector` selection but use SHORT==LONG==50 ms, so they
  cannot discriminate the two debounce paths by timing. C1/C2 run the full
  `spawn_detector` poll loop with LONG=400 ms ≫ SHORT=60 ms: C1 asserts
  `meeting-ended` fires before LONG when `stable_capture=true` (SHORT path); C2
  asserts it holds through LONG when `stable_capture=false`. This pins the
  field-propagation chain `port.current_state → spawn_detector poll →
  step_detector debounce select` that the §1 tests reach only at the pure
  use-case altitude. The residual gap a live Meet call would close — real WASAPI
  driving the `bc_drop_observed_this_call` latch from real device state — is
  already pinned by §1.1–1.3 (the latch is pure internal logic); the live call
  would only confirm the hardware constant, not the code path.
- [x] 4.3 No change-specific `e2e/smoke/meeting-udp-media-signal.spec.ts` — the adaptive
  debounce LOGIC is Rust-internal (cargo-tested §1). The `meeting-detected`/`meeting-ended`
  → banner WIRING is covered by the capability-level `e2e/smoke/meeting-auto-detect.spec.ts`
  backfill (also covers detector-turn-latch-deadlock).

## 5. Self-review (round 1) — 0 findings

The `Agent` tool is not available in this session and the prior two changes in this branch were closed under a persistent inference-service overload (HTTP 529). The same self-review fallback was applied here; the full analysis is in `design.md` Decision 6. Summary:

- **Correctness — 0 findings.** `GetExtendedUdpTable` claim verified against the Windows IP Helper API; latch logic (monotonic, reset by `notify_exit`) verified; hexagonal fit verified (adapter-set bool, pure-use-case consumer); default `false` preserves today's 15 s; residual first-drop risk honestly disclosed with the auto-stop re-engage mitigation; WASAPI-never-works edge case is consistent with the spec's "may fire early rather than never" semantic (explicitly NOT flagged — adding special-case logic would violate KISS).
- **Security — 0 findings.** No new trust-boundary crossing; `stable_capture` is derived from internal WASAPI state, never from untrusted input; adversarial-test categories (transient drop, reset across calls, invariant matrix) covered by tasks 1.2, 1.3, 1.6.
- **Spec compliance — 0 findings.** Delta preserves all 6 canonical exit scenarios; the corrected known-limitation note replaces the impossible `GetExtendedUdpTable`-to-Google-IPs claim (verified at canonical line 135); `/opsx:archive` sync will apply it.
- **Code-review + shark-tank self-review — 0 findings.** Full `git diff` re-read end-to-end. Latch check correctly placed before the `last_bc` mutation; TURN path invariant under `stable_capture`; log format string extended with the new discriminator; no new I/O, allocations, or syscalls; Default impl handles the new `DetectorSettings` field so production wiring (`lib.rs:1007`) is unchanged.

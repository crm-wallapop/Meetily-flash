# Design — meeting-udp-confidence-debounce

## Context

`meeting-auto-detect` selects the UDP exit-debounce duration (4 s fast vs 15 s safe) from a per-poll `DetectorObservation.stable_capture` bool produced by the Windows adapter (`detection/windows.rs::current_state()`) and consumed by the pure use case (`use_cases/meeting_detection.rs::step_detector`).

The shipped adapter sets a `bc_drop_observed_this_call` latch on the **first `has_browser_capture_session()` `true → false` transition** and emits `stable_capture = !bc_drop_observed_this_call`. The exit of a UDP call *is* that `true → false` transition, so the exit poll always latches the flag and emits `stable_capture = false`. The 4 s branch is therefore unreachable for any real call; every UDP exit takes 15 s.

The tests did not catch this: the windows.rs unit test `stable_call_sets_stable_capture_true` polls with capture continuously `true` (never models the exit drop), and the fake-adapter test `stable_capture_true_drives_short_debounce_to_ended` hand-sets `stable_capture = true` on a fake observation rather than driving the real latch. Both assert the unreachable path.

Hexagonal boundaries (CLAUDE.md §2a): `ports/meeting_detector.rs` holds the pure `DetectorObservation` value object; `detection/windows.rs` is the adapter that computes signals; `use_cases/meeting_detection.rs` is the pure state machine. This change is confined to the adapter's latch logic and the value it stamps into the existing `stable_capture` field — no port-trait, use-case-signature, or frontend-contract change.

## Goals / Non-Goals

**Goals:**
- Make the 4 s stable-UDP exit reachable for real stable-mic calls (the common case), cutting UDP exit latency from ~16 s to ~5–6 s.
- Preserve the 15 s safe path for genuinely flaky setups and short/uncertain calls.
- Replace the tests that assert the unreachable path with ones that drive the **real** adapter latch through capture drop/recovery sequences.

**Non-Goals:**
- No change to the TURN 4 s path (`is_turn_exit`), the `notify_exit()` reset contract, the conservative fail-closed default, or `has_browser_capture_session()` itself.
- No new port trait or use-case signature change. `step_detector` keeps reading `observation.stable_capture`.
- No frontend, Tauri-command, or DB change.

**Superseded prior intent:** the earlier draft of this change promised "a transient ⟹ 15 s for the rest of the call." That rule is **relaxed** by D1 (decided 2026-06-25 after shark-tank review). See D1.

## Decisions

### D1: Decide exit stability ONCE, at the first capture drop, and lock it for the call

The root cause of the dead-code bug: a `true → false` drop is indistinguishable at drop time between "the exit" and "a transient's leading edge", so the shipped adapter's drop-edge `bc_drop_observed_this_call` latch self-marks every exit unstable.

The fix decides the exit's stability **once**, at the **first** `true → false` drop of the call, from a single robust signal: the length of the unbroken `bc == true` run immediately before that drop. If that run is ≥ `STABLE_CONFIDENCE_WINDOW`, the exit is stable (4 s); otherwise unstable (15 s). The decision is stored in `exit_stable_latch: Option<bool>` and **locked for the entire call** — once set to `Some(v)`, it SHALL NOT be cleared or recomputed until `notify_exit()`.

**Why a first-drop run-length signal, not a recovery-based latch.** An earlier draft latched `transient_observed` on the `false → true` recovery edge. Shark-tank review (2026-06-25) proved that design RECREATED the self-heal trap (commit `693ff90`): clearing/altering `exit_stable_latch` on a `false → true` edge meant a single WASAPI-flicker poll mid-debounce flipped a running 4 s exit to 15 s. Holding the latch immutable closes that trap — but immutability also makes any post-first-drop signal (including a recovery) unable to influence the decision, so `transient_observed` becomes dead code and is dropped (YAGNI). Run-length-at-first-drop is the sole discriminator that survives the immutability requirement.

**Why run length discriminates flaky from stable.** A genuinely flaky UDP session drops frequently, so the run preceding its first drop is short (< window) → 15 s. A stable session holds capture for minutes before its real exit → ≥ window → 4 s. The window, not the recovery, is the flaky detector.

**Behavior change vs the prior documented rule (decided 2026-06-25, Option A).** The old "a transient ⟹ 15 s for the rest of the call" rule is RELAXED. Under the locked design, a call that runs stable ≥ window, then suffers a recovered transient, then later really exits, exits at 4 s (the first drop's run was long). This liberalization is acceptable — such a call demonstrated ≥ window of stable capture before its first drop, which is exactly the property the fast exit rewards — and genuinely flaky calls (short first-drop runs) are unaffected (still 15 s). The decision is the safe direction for the cases that matter.

**Alternative considered (rejected).** Preserve "transient ⟹ 15 s" by giving the adapter feedback when a debounce resolves (a new `step_detector → adapter` port) so it could re-latch after a transient without clearing mid-debounce. Rejected — that is port scope-creep this project has explicitly deferred (`hexagonal-port-traits`), and the adapter has no InCall/Idle visibility of its own.

### D2: `STABLE_CONFIDENCE_WINDOW` is the sole classifier (20 s, const)

`STABLE_CONFIDENCE_WINDOW = 20 s`, a `const` (not configurable — YAGNI). It is the sole classifier: a first drop preceded by ≥ 20 s of continuous capture → 4 s; otherwise → 15 s. 20 s sits above the ~10 s WASAPI transient ceiling documented in `openspec/specs/meeting-auto-detect/spec.md`, with margin, so the leading-edge-of-a-slow-transient hazard is closed. A real meeting holds capture for minutes, so the guard is satisfied for essentially all genuine calls; only pathologically short or flaky sessions fall back to 15 s (the safe direction). A 15-second hold-music-only "meeting" would get 15 s — acceptable, conservative.

### D3: Hold the latched decision immutable until `notify_exit()` (anti-self-heal)

`step_detector` recomputes `debounce = if is_turn_exit {4s} else if stable_capture {4s} else {15s}` on **every poll**. Therefore the `stable_capture` value driving the duration MUST NOT change between the drop poll and the threshold poll. `exit_stable_latch`, once `Some(v)`, is **immutable for the rest of the call** — not cleared by a `false → true` recovery, not recomputed on a later drop. Only `notify_exit()` resets it.

**Why this is load-bearing.** The sibling `detector-turn-latch` self-heal (commit `715c810`, reverted `693ff90`) regressed a running 4 s TURN debounce to 15 s by recomputing a flag on the exit poll. Shark-tank review (2026-06-25) proved the recovery-based draft of THIS change recreated the same trap in the opposite direction: a `false → true` flicker mid-debounce cleared the latch and flipped a running 4 s exit to 15 s. Making the latch immutable closes both directions. The MEMORY rule ("the reset condition must NOT overlap the TURN/UDP exit debounce window") is honored: there is no reset condition at all between the drop and `notify_exit()`.

### D4: Reset per-call state in `notify_exit()`; reconstruction covers the crash path

`exit_stable_latch` and `bc_true_since` are per-call and SHALL be reset to `None` in `notify_exit()` so back-to-back calls do not inherit the previous call's stability assessment. (`transient_observed` is **not introduced** — the rejected recovery-based draft would have added it, but D1's immutability makes it dead code, so it is never added.)

**Crash/restart safety net (shark-tank C3).** If the app dies mid-debounce before `notify_exit()` fires, the stale fields die with the process — `WindowsMeetingDetector` is reconstructed fresh on next start (`WindowsMeetingDetector::new` zero-initializes the fields), so no cross-call leak. The `notify_exit()` reset covers the normal path; construction-from-scratch covers the crash path. This is the same property the reverted self-heal relied on for `turn_established`.

## Adapter state (windows.rs)

Per-call fields on `WindowsMeetingDetector`, all reset by `notify_exit()`:
- `bc_true_since: Option<Instant>` — start of the current unbroken `bc == true` run. Set on a `false → true` edge, and on the first poll if `bc == true` (stamped to the detector's construction/first-poll instant — see "First-poll semantics" below). Cleared to `None` on a `true → false` edge **after** the run length is read. Once `exit_stable_latch` is set, `bc_true_since` is no longer consulted (the decision is locked).
- `exit_stable_latch: Option<bool>` — `None` until the first `true → false` drop of the call; on that drop set to `Some(run_len >= STABLE_CONFIDENCE_WINDOW)` where `run_len = now − bc_true_since`. Once `Some(v)`, **immutable until `notify_exit()`** (D3).
- Not introduced: `transient_observed` (the rejected recovery-based draft would have added it; dead under D1/D3, so never added).

`stable_capture` emitted in the observation, on EVERY poll (bc true or false): `exit_stable_latch.unwrap_or(false)`. Before any drop this is `false`; while bc is active and no drop has occurred, `step_detector` does not consume the value (it clears the timer on `bc == true`). Emitting the latched value on `bc == true` polls keeps logs coherent.

**First-poll semantics (shark-tank I2).** If the detector is constructed mid-call (app started while a meeting was already in progress), the first poll sees `bc == true` but the adapter has no history of how long capture was active. `bc_true_since` is stamped to the detector's start instant, so `run_len` is measured from app start, not from true capture onset. A call that exits shortly after a mid-call app start therefore measures a short `run_len` → 15 s. This is the safe (conservative) direction and is accepted; the unknowable pre-start history cannot be recovered. The symmetric first-poll-`false` case (detector starts before the browser opens `getUserMedia`) is handled by the `false → true` edge rule: `bc_true_since` stays `None` through the initial `false` polls and is stamped only when capture first goes `true`, so a later exit's `run_len` is measured from that first real `true` edge, not from detector start (adversarial test 14, shark-tank round-2 I-R1).

`Instant::now()` is the adapter's clock (already used for `connection_first_seen_at`); the pure use case stays clock-free. **The test clock seam (Task 1.8) MUST divert every `Instant::now()` read inside `current_state()` — including `connection_first_seen_at`, the `last_bc` transition block, and `bc_true_since` — not just the new field (shark-tank I3).** A partial seam lets tests pass while production diverges.

## Risks / Trade-offs

- **[Relaxed "transient ⟹ 15 s" rule]** Documented behavior change (D1): a call with a mid-call transient after ≥ window of stable capture now exits at 4 s, not 15 s. Accepted 2026-06-25 — such a call proved stable capture before its first drop. Genuinely flaky calls (short first-drop runs) are unaffected.
- **[Mid-call app start]** See First-poll semantics: `run_len` measured from detector start; conservative. Accepted.
- **[Clock granularity]** The 2 s poll interval means `run_len` and the drop instant are accurate to ~2 s. `STABLE_CONFIDENCE_WINDOW = 20 s` has ample margin so granularity does not flip the decision near the boundary. Boundary tests (Task 1.x) pin the `>=` comparison exactly.
- **[Fail-closed on WASAPI error]** Unchanged: enumeration failure yields `bc == false`, starting the debounce; with no prior stable run the latch is `None`/`false` → 15 s. Conservative.
- **[No mid-debounce reclassification]** Once locked, a call cannot be "upgraded" to 15 s mid-debounce even if capture behaves erratically during the 4 s window. Accepted — the alternative needs port scope-creep (D1 alternative).

## Adversarial tests (driving the REAL adapter latch)

Replace the fake-driven assertions with `current_state()` sequences over `DetectorProbes` + a controllable clock (Task 1.8):

1. **Clean stable exit → 4 s:** `bc=true` for ≥ window, then `bc=false` once ⟹ `stable_capture == true`. (Bug-reproducing RED — fails on current code.)
2. **Flicker-during-debounce stays 4 s (the self-heal-trap guard, shark-tank C1):** `bc=true` (≥window) → `bc=false` (latch `Some(true)`) → `bc=true` (1-poll WASAPI flicker) → `bc=false` again ⟹ `stable_capture == true` on every post-drop poll; the latch was NOT cleared by the flicker.
3. **Short stable run → 15 s:** `bc=true` for < window, then `false` ⟹ `stable_capture == false`.
4. **Recovered transient then later exit (the D1 relaxation):** `bc=true` (≥window) → `false` (first drop, latch `Some(true)`) → `true` (recovery) → `true` (≥window) → `false` (second drop) ⟹ `stable_capture == true` (latch held from first drop).
5. **Latched-stable across debounce:** stable exit then several consecutive `bc=false` polls ⟹ every poll reports the same `stable_capture == true`.
6. **`notify_exit()` resets:** clean stable exit sets latch `Some(true)`; `notify_exit()`; next call's first drop with a short run ⟹ `stable_capture == false` (no inheritance).
7. **Rapid leave/rejoin within WASAPI lag (shark-tank C2):** `bc` reads `true` throughout (rejoin within release lag — no true `true→false` edge) ⟹ no latch set, `bc_true_since` continuous, no spurious drop.
8. **Device disconnect mid-call (shark-tank C1, rewritten):** `bc=true` (≥window) → `false` (device lost; TURN relay still alive so the observation's `is_turn_exit == false`) → `true` (reconnect) → `<window` → `false` (real exit) ⟹ assert the disconnect's first drop did NOT set `is_turn_exit`, and classification follows run-length (≥window disconnect variant → `stable_capture == true`; <window variant → `false`).
9. **Mid-call app start (shark-tank I2):** detector constructed with `bc=true` already active; immediate `bc=false` ⟹ `run_len ≈ 0 < window` ⟹ `stable_capture == false` (conservative).
10. **Boundary values (shark-tank I4):** `run_len` at exactly `STABLE_CONFIDENCE_WINDOW` ⟹ `true`; at window − ε ⟹ `false`; at window + ε ⟹ `true`. Pins the `>=` comparison.
11. **No-`notify_exit` crash path (shark-tank C3):** construct a fresh detector after a simulated mid-debounce crash (no `notify_exit` called); assert `exit_stable_latch`/`bc_true_since` start `None` (reconstruction is the safety net).

End-to-end:
12. **Through `step_detector`/spawn loop — DETERMINISTIC (shark-tank I6):** a real-adapter stable exit drives `meeting-ended` within the 4 s window. Keep this as a FAST deterministic `fake.rs` loop test driving the real latch; do NOT move timing to `#[ignore]` only. A separate `#[ignore]` real-clock test (Task 4.1) confirms wall-clock timing on a live mic.

Property-style (proptest, shark-tank I5 + round-2 I-R2):
13. **Invariant:** for any `bc` poll sequence, once `exit_stable_latch == Some(v)` it stays `Some(v)` until `notify_exit()`; and `exit_stable_latch == Some(v) ⟹ ((run_len at the first drop) >= window) == v`. **Generator strategy (round-2 I-R2 — a naive bool generator almost never yields a ≥ window run at 2 s granularity, making the invariant vacuous):** generate `Vec<(bool, Duration)>` poll sequences with `true`-run lengths biased toward 0–30 s (straddling the 20 s boundary); advance the injected clock by the 2 s poll interval between polls; insert `notify_exit()` at random segment boundaries so multiple calls' first-drops are exercised. Without the run-length bias and the `notify_exit()` segmentation, the property is trivially satisfied by an impl that never latches.

14. **First-poll-`false` then stable run (round-2 I-R1):** the detector's FIRST poll reads `bc == false` (app started before the browser opened `getUserMedia`), then `bc == true` for ≥ window, then `bc == false` ⟹ `stable_capture == true` — the run is measured from the `false → true` edge, NOT from detector start (`bc_true_since` was `None` through the initial `false` polls). Every other test starts `bc == true` on poll 1, so this is the sole cover of the `None → false` first-poll path.

## Open Questions

- **`STABLE_CONFIDENCE_WINDOW` exact value.** Resolved: 20 s (const), above the spec's ~10 s WASAPI transient ceiling. Could be lowered toward ~12–15 s if field data shows transients are reliably < 10 s — revisit empirically.
- ~~Whether to clear `transient_observed` after a long clean run.~~ Moot — `transient_observed` removed by D1.

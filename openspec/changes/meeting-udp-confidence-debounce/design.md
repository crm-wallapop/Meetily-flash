# Design — meeting-udp-confidence-debounce

## Context

`meeting-auto-detect` selects the UDP exit-debounce duration (4 s fast vs 15 s safe) from a per-poll `DetectorObservation.stable_capture` bool produced by the Windows adapter (`detection/windows.rs::current_state()`) and consumed by the pure use case (`use_cases/meeting_detection.rs::step_detector`).

The shipped adapter sets a `bc_drop_observed_this_call` latch on the **first `has_browser_capture_session()` `true → false` transition** and emits `stable_capture = !bc_drop_observed_this_call`. The exit of a UDP call *is* that `true → false` transition, so the exit poll always latches the flag and emits `stable_capture = false`. The 4 s branch is therefore unreachable for any real call; every UDP exit takes 15 s.

The tests did not catch this: the windows.rs unit test `stable_call_sets_stable_capture_true` polls with capture continuously `true` (never models the exit drop), and the fake-adapter tests `stable_capture_true_drives_short_debounce_to_ended` hand-set `g.stable_capture = true` on a fake observation rather than driving the real latch. Both assert the unreachable path.

Hexagonal boundaries (CLAUDE.md §2a): `ports/meeting_detector.rs` holds the pure `DetectorObservation` value object; `detection/windows.rs` is the adapter that computes signals; `use_cases/meeting_detection.rs` is the pure state machine. This change is confined to the adapter's latch logic and the value it stamps into the existing `stable_capture` field — no port-trait, use-case-signature, or frontend-contract change.

## Goals / Non-Goals

**Goals:**
- Make the 4 s stable-UDP exit reachable for real stable-mic calls (the common case), cutting UDP exit latency from ~16 s to ~5–6 s.
- Preserve the 15 s safe path for transient-prone setups and short/uncertain calls.
- Replace the tests that assert the unreachable path with ones that drive the **real** adapter latch through capture drop/recovery sequences.

**Non-Goals:**
- No change to the TURN 4 s path (`is_turn_exit`), the `notify_exit()` reset contract, the conservative fail-closed default, or `has_browser_capture_session()` itself.
- No new port trait or use-case signature change. `step_detector` keeps reading `observation.stable_capture`.
- No frontend, Tauri-command, or DB change.

## Decisions

### D1: Latch transient-prone on the recovery edge, not the drop edge

The root cause is that a `true → false` drop is **indistinguishable at drop time** between "the exit" and "a transient's leading edge". The only signal that proves a drop was a transient is a subsequent `false → true` **recovery**. So the adapter SHALL set `transient_observed = true` on the recovery edge, and a non-recovering drop (the exit) SHALL NOT set it. `stable_capture`'s first clause becomes `!transient_observed`.

**Why:** this is the minimal correct fix to the ordering bug. It preserves the documented "a transient ⟹ 15 s for the rest of the call" behaviour (the recovery still latches), while letting a clean call's single exit drop keep `stable_capture = true`.

**Alternative considered:** keep latching on the drop edge but special-case "the last drop". Rejected — you cannot know a drop is the last one at the time it happens; that is the bug.

### D2: Minimum stable-run guard (`STABLE_CONFIDENCE_WINDOW`)

D1 alone admits a hazard: a call with a very short stable run (e.g. capture active 3 s) then a non-recovering drop would get 4 s — but that drop may be the leading edge of a ~10 s transient that simply hasn't recovered before the 4 s debounce fires `meeting-ended` early. So `stable_capture`'s second clause requires the capture session to have been continuously active for ≥ `STABLE_CONFIDENCE_WINDOW` immediately before the drop. The window is **20 s** — above the ~10 s WASAPI transient ceiling documented in the spec, with margin.

**Why 20 s:** a real meeting holds capture active for minutes, so the guard is satisfied for essentially all genuine calls; only pathologically short or flaky sessions fall back to 15 s, which is the safe direction.

**Alternative considered:** rely on D1 alone (no timer). Rejected — leaves the short-run-then-transient hazard open, which §4 (adversarial) requires handling.

### D3: Latch the `stable_capture` decision stable across the debounce window

`step_detector` recomputes `debounce = if is_turn_exit {4s} else if stable_capture {4s} else {15s}` on **every poll** and compares against `elapsed` since `connection_lost_at`. Therefore the value driving the duration MUST NOT change between the drop poll and the threshold poll. The adapter SHALL compute `stable_capture` once at the drop (`true → false`) poll and hold that latched value on every subsequent `bc == false` poll until capture recovers or `notify_exit()` fires.

**Why this is called out explicitly:** the sibling `detector-turn-latch` work shipped a self-heal reset that recomputed a flag on the `bc == false` exit poll and silently bumped a running 4 s TURN debounce to 15 s (reverted 2026-06-25). The same per-poll-recompute trap applies here in the opposite direction: a flag that flips to `true` mid-debounce could shorten a running 15 s window. Latching the decision at the drop closes both directions.

### D4: Reset all per-call state in `notify_exit()`

`transient_observed`, the continuous-active timer (`bc_true_since`), and the latched `stable_capture` decision are per-call and SHALL be reset in `notify_exit()` so back-to-back calls do not inherit the previous call's stability assessment. This mirrors the existing reset contract for the old latch.

## Adapter state (windows.rs)

Per-call fields on `WindowsMeetingDetector`, all reset by `notify_exit()`:
- `bc_true_since: Option<Instant>` — start of the current unbroken `bc == true` run; set on a `false → true` edge (and on first poll if `bc`), cleared to `None` on a `true → false` edge **after** reading the run length.
- `transient_observed: bool` — set `true` on a `false → true` recovery edge.
- `exit_stable_latch: Option<bool>` — set on the `true → false` drop edge to `!transient_observed && run_len >= STABLE_CONFIDENCE_WINDOW`; read on subsequent `bc == false` polls; cleared on a recovery edge.

`stable_capture` emitted in the observation:
- on a `bc == false` poll: `exit_stable_latch.unwrap_or(false)`
- on a `bc == true` poll: `false` is acceptable (the value is unused while capture is active — `step_detector` clears the timer when `!is_turn_exit && bc`).

`Instant::now()` is the adapter's clock (already used for `connection_first_seen_at`); the pure use case stays clock-free.

## Risks / Trade-offs

- **[Sticky transient flag across a long later stable run]** After a recovered transient, `transient_observed` stays `true` for the rest of the call, so even a subsequent long stable run exits on 15 s. This preserves the documented conservative behaviour; the cost is a slightly slower exit on a call that had one early hiccup but stabilised. Accepted — the safe direction.
- **[Clock granularity]** The 2 s poll interval means `run_len` and the drop instant are accurate to ~2 s. `STABLE_CONFIDENCE_WINDOW = 20 s` has ample margin so granularity does not flip the decision near the boundary in practice.
- **[Fail-closed on WASAPI error]** Unchanged: enumeration failure yields `bc == false`, which starts the debounce; with no prior stable run the latch is `false` → 15 s. Conservative.

## Adversarial tests (driving the REAL adapter latch)

Replace the fake-driven assertions with `current_state()` sequences over `DetectorProbes` (the existing test seam), advancing a controllable clock:

1. **Clean stable exit → 4 s:** `bc=true` for ≥ window, then `bc=false` once (no recovery) ⟹ `stable_capture == true`.
2. **Exit drop does not self-mark:** same as (1), assert `transient_observed`/latch stays `false` and `stable_capture == true` on the drop poll. (The bug-reproducing RED test — fails on current code.)
3. **Recovered transient → 15 s:** `bc=true` (≥window), `false`, `true` (recovery), `true`(≥window), then `false` (exit) ⟹ `stable_capture == false`.
4. **Short stable run → 15 s:** `bc=true` for < window, then `false` ⟹ `stable_capture == false`.
5. **Latched-stable across debounce:** stable exit then several consecutive `bc=false` polls ⟹ every poll reports the same `stable_capture == true`.
6. **`notify_exit()` resets:** trip transient on call A, `notify_exit()`, then a clean stable call B ⟹ B reports `stable_capture == true`.
7. **End-to-end through `step_detector`/spawn loop:** a real-adapter stable exit fires `meeting-ended` within the 4 s window, not 15 s (the fake test C1 is rewritten to drive the real latch, or a windows.rs `#[ignore]` real-clock test covers the timing).

Property-style: for any drop/recovery sequence, `stable_capture == true` ⟹ (no recovery edge occurred) ∧ (last continuous run ≥ window).

## Open Questions

- **`STABLE_CONFIDENCE_WINDOW` exact value (20 s proposed).** Tunable; 20 s is the conservative pick above the ~10 s transient ceiling. Could be lowered toward ~12–15 s if field data shows transients are reliably < 10 s — resolve empirically, default 20 s.
- **Whether to clear `transient_observed` after a long clean run** (non-sticky). Leaning NO (keep sticky, per D-risk above) unless a stable call with one early hiccup exiting at 15 s proves annoying in practice.

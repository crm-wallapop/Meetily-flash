## Context

Today the UDP exit path debounces `has_browser_capture_session()` ("bc") for a fixed
15 s. That value exists to absorb the worst-case bc transient — a mid-call
capture-session drop of up to ~10 s observed on device switches — plus ~5 s margin.
Most calls never produce such a transient, so every call pays a ~16–19 s exit tax
(15 s debounce + ~1–2 s `getUserMedia` release + 2 s poll granularity) to insure
against a rare event.

The canonical spec (`meeting-auto-detect/spec.md` line 107) proposes, as the future
fix, a `GetExtendedUdpTable`-to-Google-IPs check. That is impossible: the API returns
`MIB_UDPROW_OWNER_PID = { dwLocalAddr, dwLocalPort, dwOwningPid }` — **no remote
address** (UDP is connectionless) — so a WebRTC media flow cannot be distinguished
from QUIC, which Chrome speaks to many Google services over UDP. The adapter already
documents this (`detection/windows.rs:222`). The clean signals that *would* work are
blocked: ETW/WFP flow capture needs admin privileges; CDP `RTCPeerConnection` state
needs the browser launched with `--remote-debugging-port`. This change adopts the one
cheap, unblocked lever: make the debounce **adaptive** to the call's observed bc
stability, which the adapter already tracks (`last_bc`, with `info`-level transition
logging).

## Goals / Non-Goals

**Goals:**
- Cut UDP exit latency on stable-mic calls from ~16–19 s to ~5–6 s.
- Keep transient-prone setups on the safe 15 s debounce — no regression.
- Correct the spec's impossible line-107 claim, so future readers don't chase a dead
  end.

**Non-Goals:**
- No ETW/WFP flow capture (admin privilege — documented as the blocked clean path).
- No CDP / browser debug-port integration.
- No change to entry detection, the TURN path, or the lobby false-positive (pre-existing
  known limitation, unchanged).
- No change to `MeetingDetectorPort` trait method signatures — only a new field on
  `DetectorObservation`.

## Decisions

### Decision 1 — Adaptive debounce keyed on per-call bc-drop history
Add `stable_capture: bool` to `DetectorObservation`. The adapter maintains
`bc_drop_observed_this_call: bool` (reset in `notify_exit`). On each poll, when
`last_bc == Some(true)` and the current `bc == false`, the adapter latches
`bc_drop_observed_this_call = true`. The observation carries
`stable_capture = !bc_drop_observed_this_call`. The pure `step_detector` InCall branch
selects the UDP debounce: `SHORT` (4 s) when `stable_capture`, else `LONG` (15 s).

**Why:** bc stability is the only cheap, in-adapter signal that correlates with "a
drop right now is a real leave." This mirrors the existing `is_turn_exit` plumbing
(adapter-set bool consumed by the pure use case), so it is hexagonally clean and adds
no trait-method change.

**Alternatives considered:**
- *ETW / WFP (`FWPM_LAYER_ALE_FLOW_ESTABLISHED`) flow capture* — the genuinely clean
  signal (per-flow remote IP + PID + protocol). Blocked: real-time ETW sessions require
  elevation. Rejected for a user app; recorded here as the reason the clean path is out.
- *CDP `RTCPeerConnection.getStats`* — perfect ground truth. Blocked: requires the
  browser launched with `--remote-debugging-port`, which Meetily cannot retrofit onto
  the user's existing Chrome. Rejected.
- *UDP-socket-count delta (`GetExtendedUdpTable`)* as a secondary exit confirm. QUIC
  churn exceeds the WebRTC-socket delta; untrustworthy. Rejected.
- *Time-since-last-drop heuristic* (short only if no drop in last N minutes). Adds a
  tunable and a clock; a call that transiented once is transient-prone for its whole
  duration. Rejected in favour of whole-call latching (Decision 3).

### Decision 2 — `SHORT` debounce = 4 s (same as TURN)
On a stable call, bc dropping within ~1–2 s of leave is a high-confidence leave signal;
4 s absorbs the release lag + 2 s poll granularity + margin, matching the empirically
validated TURN 4 s. Total stable exit ≈ 1–2 s release + 4 s ≈ ~5–6 s. A single constant
is tuned during apply's manual QA if a given hardware setup needs more margin.

### Decision 3 — Latching: one observed drop ⇒ long debounce for the rest of the call
`bc_drop_observed_this_call` only flips false→true and is reset only by `notify_exit()`.
A setup that transiented once is likely to transient again, so the long debounce guards
subsequent exits. Cost: a call with one early transient followed by a real leave later
exits in ~16–19 s instead of ~5–6 s. Acceptable — that setup proved itself
transient-prone.

### Decision 4 — Residual first-drop risk accepted, mitigated by the existing auto-stop re-engage
The one scenario adaptive debounce cannot fully cover: a previously-stable call whose
*first* bc drop is a genuine transient (not leave). `stable_capture` is still true when
that drop starts, so the short debounce runs and could fire `meeting-ended` ~5–6 s into
a sustained transient. **Mitigation:** the frontend auto-stop banner has a 10 s
confirmation window and dismisses silently if signals re-engage during it (existing
`Auto-stop recording on call end` requirement). A transient ending within that window
self-heals; a transient lasting > ~5–6 s is exactly the rare sustained event the 15 s
debounce was built for — and afterward `stable_capture` is false, so the call is in
long-debounce mode for any further drop. Net worst case: a brief auto-stop banner that
either self-dismisses (bc returns) or confirms (the user is leaving anyway).

### Decision 5 — Correct the line-107 known-limitation note
Replace the claim that a future `GetExtendedUdpTable`-to-Google-IPs check would
discriminate lobby from active call with: the UDP-media-flow direction is abandoned
(the API exposes no remote address; QUIC confounds any UDP-socket-presence heuristic);
the clean signals (ETW/WFP, CDP) are blocked by admin / browser-flag requirements;
adaptive debounce (this change) is the adopted lever for **exit** latency. Entry
lobby-discrimination remains a known limitation (unchanged).

## Risks / Trade-offs

- **[First-drop false-exit on a previously-stable call]** → Decision 4 mitigation
  (auto-stop re-engage). Accepted; bounded to a dismissible banner.
- **[Stable call with one early transient, then a real leave, exits slow]** → Decision 3;
  acceptable — that setup is transient-prone.
- **[SHORT = 4 s too aggressive on some hardware]** → degrades to a false auto-stop
  banner that self-dismisses; observable, single-constant tunable.
- **[New `DetectorObservation` field ripples to fake adapter + dev-detector seam]** →
  mechanical; default `false` (= long debounce) preserves today's behaviour when unset.

No persistence change ⇒ no migration plan. Rollback = revert the field + constant.

## Open Questions

- Empirical tuning of `SHORT` (4 s vs 5–6 s) on the target hardware during apply's
  manual QA.

## Decision 6 — Round-1 self-review (0 findings)

The `Agent` tool is not available in this session (the deferred-tool list exposes no
subagent dispatch), and the prior two changes in this branch
(`notification-actions`, `detector-turn-latch-deadlock`) were closed under a persistent
inference-service overload (HTTP 529) that blocked every subagent dispatch for 11+ h.
The same fallback applies here: a rigorous self-review against the adversarial-TDD and
spec-compliance axes, documented in place of the dispatchable reviewers.

**Correctness — 0 findings.**

- **C1 — `GetExtendedUdpTable` claim accurate.** `MIB_UDPROW_OWNER_PID` exposes only
  `{dwLocalAddr, dwLocalPort, dwOwningPid}`; UDP is connectionless so no remote-address
  field exists. QUIC (Chrome → many Google services) confounds any UDP-socket-presence
  heuristic. The claim that the clean signals (ETW/WFP, CDP) are blocked by admin /
  browser-flag prerequisites is also accurate. Decision 5's correction of the canonical
  note is well-founded.
- **C2 — Latch logic correct.** `bc_drop_observed_this_call` is monotonic false→true,
  reset only by `notify_exit()`. The use case reads `stable_capture = !latched`. The
  placement requirement (the latch check must read `last_bc` before the `last_bc = Some(bc)`
  mutation in the existing transition block) is an implementation note for apply, not a
  design gap — task 2.2 calls it out.
- **C3 — Hexagonal fit.** Adapter sets a bool from internal bc history; pure use case
  consumes it via the port struct. No trait-method change, no adapter import from the use
  case. Mirrors the existing `is_turn_exit` plumbing exactly.
- **C4 — Default `false` preserves today's behaviour.** `DetectorObservation::default()`,
  the fake adapter's `joined` snapshot, and the dev-detector seam all default
  `stable_capture = false` ⇒ LONG debounce ⇒ today's 15 s. No regression on unset paths.
- **C5 — Matrix test (task 1.6) is exhaustive.** `{is_turn_exit, stable_capture} ∈ {T,F}²`
  pins all four combinations: TURN path (4 s) is invariant under `stable_capture`; UDP
  path selects 4 s only when `stable_capture = true`. The debounce is a pure function of
  the two inputs.
- **C6 — Residual first-drop risk (Decision 4) honestly disclosed.** The one scenario
  adaptive debounce cannot fully cover (a previously-stable call whose first bc drop is a
  genuine transient) is named, mitigated by the existing 10 s auto-stop re-engage window,
  and bounded — afterward `stable_capture` latches false so subsequent drops use LONG.
  Net worst case is a dismissible banner that either self-dismisses (bc returns) or
  confirms (the user is leaving anyway).
- **C7 — WASAPI-never-works edge case is consistent with the spec, NOT a regression.**
  If WASAPI never initialises, `bc` is `false` for every poll, no `Some(true) → false`
  transition is observable, `stable_capture` stays `true`, and the SHORT debounce (4 s)
  applies. The canonical "WASAPI enumeration fails" scenario says the debounce "starts —
  this is the conservative default (may fire `meeting-ended` early rather than never)";
  4 s is "earlier" but still fires rather than hanging. A transient WASAPI failure
  *during* a call is handled correctly (bc was true → false ⇒ latch trips ⇒ LONG). The
  only corner case that speeds up is WASAPI-broken-from-the-start, which is already a
  broken-state loop today (just 15 s-slower); the change does not introduce it. Adding
  special-case logic for this corner would violate KISS for an extremely rare
  broken-state path — explicitly rejected.

**Security — 0 findings.**

- **S1 — No new trust-boundary crossing.** `stable_capture` is a bool derived from the
  adapter's internal bc history (WASAPI enumeration, itself internal). It does not touch
  untrusted input (meeting titles, transcript text, LLM output, API request bodies). No
  OWASP-relevant surface is added or widened.
- **S2 — Adversarial-test coverage sufficient for the change scope.** This is a
  detection-layer change; the §4 categories that apply (transient signal drop, reset
  across calls, invariant matrix) are covered by tasks 1.2, 1.3, 1.6. Recording-specific
  categories (empty buffer, silence, oversized, device-disconnect mid-record) are out of
  scope — they belong to the audio pipeline, not the detector.
- **S3 — No scope creep, no missing scope.** Adapter latch + port field + use-case
  selection + fake/seam default + spec correction. Every construction site is enumerated
  in task 2.1; the canonical line-135 correction is task 3.1.

**Spec compliance — 0 findings.**

- **SC1 — Delta preserves every canonical exit scenario.** The canonical "Detect when an
  active call ends" requirement has 6 scenarios; the delta replaces the fixed-15 s "User
  leaves a UDP-transport call" scenario with the adaptive "User leaves a UDP-transport
  call on a stable mic" + "Stable-mic UDP call exits on the short debounce" + "UDP call
  with an observed bc drop uses the long debounce" trio, preserves "Lobby page does not
  trigger exit", "User leaves a TCP TURN call", "Transient network drop (TURN path)",
  "WASAPI enumeration fails" verbatim, and amends "Browser capture session transiently
  drops during call (UDP path)" to note the latch. No scenario is dropped.
- **SC2 — Corrected known-limitation note accurate.** Verified against the Windows IP
  Helper API and the existing adapter comment at `detection/windows.rs:222`. The canonical
  text at line 135 currently makes the impossible `GetExtendedUdpTable`-to-Google-IPs
  claim; `/opsx:archive` sync will replace it with the corrected note (task 3.1 +
  re-read-after-archive verification).
- **SC3 — Single source of truth.** The canonical spec is the only place the
  known-limitation note lives; the delta modifies it once. No parallel hand-maintained
  text is introduced.

**Conclusion.** The proposal, design, tasks, and delta spec are internally consistent,
hexagonally clean, and honestly disclose the residual risk. Proceed to `/opsx:apply`.

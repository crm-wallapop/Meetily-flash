## Context

The Windows meeting detector (`WindowsMeetingDetector` in
`detection/windows.rs`) is an adapter implementing `MeetingDetectorPort`. Each
poll it returns a `DetectorObservation`; the pure use case `step_detector`
(`use_cases/meeting_detection.rs`) consumes it. Two fields drive the state
machine:

- `has_meet_connection` — the **entry** signal. The `Idle → InCall`
  transition fires when `has_title && has_meet_connection && not_preexisting`.
- `has_browser_capture_session` (`bc`) — the **exit** signal for UDP calls.
  The `InCall` branch clears its debounce timer while `bc` is true. This is
  the asymmetric-D2 design (guarded by `test_2_9`): exit does **not** use
  `has_meet_connection`.
- `is_turn_exit` — selects the 4 s TURN debounce vs the 15 s UDP debounce.

The adapter keeps a sticky `turn_established` flag, intended to mean "a TURN
relay was observed for this call." Today it is (a) set whenever
`has_turn_connection()` is true on any poll and (b) used in `current_state()`
to force `has_meet_connection = false` once latched, until `notify_exit()`
resets it on `InCall → Idle`. Two defects compound:

1. **The TURN CIDRs are broad GCP ranges** (`34.64.0.0/10`, `35.190.0.0/17`,
   `35.191.0.0/16`, `130.211.0.0/22`) used by many Google services, so the
   flag latches on non-Meet browser traffic.
2. **Forcing the entry signal false can never be undone** without first
   entering `InCall` — which the suppression prevents. Deadlock.

A captured debug log from the live failure showed the Meet title matched and
`bc` was true, but `has_meet_connection` was stuck `false` ("TURN gone after
call — treating as disconnected").

## Goals / Non-Goals

**Goals:**
- Eliminate the deadlock: a stale `turn_established` latch SHALL NEVER
  prevent detection of a subsequent real call.
- Stop non-Meet GCP traffic from poisoning `is_turn_exit`, which would
  otherwise collapse a real UDP call onto the 4 s TURN debounce and risk a
  premature `meeting-ended`.
- Preserve the legitimate 4 s TURN exit latency for genuine TURN calls.

**Non-Goals:**
- No change to the `MeetingDetectorPort` trait, `DetectorObservation`, or the
  pure `step_detector` (hexagonal: the bug and fix are adapter-local).
- No tightening of the TURN CIDR list (Google does not publish distinct
  TURN IPs; CIDR narrowing is fragile and out of scope).
- No new entry signal (e.g. `GetExtendedUdpTable`) — that is the separate
  known limitation recorded in the canonical spec (line 107).
- No frontend change.

## Decisions

### Decision 1 — Delete the entry-suppression branch
`has_meet_connection` becomes `turn || (has_meet_connection_raw() && bc())`
unconditionally; the `else if turn_established { false }` arm is removed.

**Why:** that arm's documented rationale ("prevent the exit debounce from
ever starting") is stale. Exit has used `bc` (not `has_meet_connection`)
since the asymmetric-D2 redesign — `test_2_9` is the regression guard. So the
arm affects **only** entry, and forcing entry false is pure harm: the
deadlock. Removing it is the minimal change that fixes the reported bug.

**Alternatives considered:**
- *Time-bound the latch (reset after N s without a TURN sighting).* Band-aid:
  still wrongly forces entry false for N seconds and adds a tunable. Rejected.
- *Reset the latch when `mc && bc` are both true.* Restores entry but keeps a
  contradictory branch whose only effect is to be overridden — dead logic.
  Rejected (KISS).

### Decision 2 — Gate latch set on `turn && bc`
`turn_established` is set only on polls where both a TURN relay AND an active
browser capture session are observed, instead of `turn` alone.

**Why:** `bc` is the canonical "in a real call" proxy the detector already
relies on for UDP entry (`mc && bc`) and UDP exit (`bc`). A browser→GCP TCP
connection without mic capture is not a call; a TURN relay with mic capture
is. This is the discriminator that makes `is_turn_exit` trustworthy and stays
entirely inside the adapter (no port-trait change, no state feedback into the
adapter).

**Alternatives considered:**
- *Gate on the use case's `InCall` state.* Requires either feeding state back
  into `current_state()` (port-trait change touching the mock + dev-detector
  seam) or the adapter re-deriving the state machine (DRY violation).
  Rejected per hexagonal / YAGNI.
- *Gate on `has_title` (Meet window present).* Rejected: the focus/tab-switch
  transients that already required the "always check connection signals
  regardless of window state" comment (windows.rs ~628) would cause the latch
  to be missed on those polls. `bc` has no such gap during a call.
- *Narrow the TURN CIDRs.* Google does not publish TURN-only IP ranges;
  today's ranges are shared GCP. Rejected as fragile.

### Decision 3 — Retain `turn_established` and `is_turn_exit`
The flag and the 4 s TURN exit debounce are kept; only their misuse changes.
This preserves the ~5 s TURN exit latency for genuine relay calls.

### Decision 4 — No Playwright smoke spec (smoke-rule carve-out)
The CLAUDE.md §3 smoke-test deliverable applies to "user-visible frontend
behaviour." This change's user-visible effect (the banner reliably firing) is
driven by Rust detection logic that depends on live TCP/WASAPI state — not
observable from the webview. The `dev-detector` simulation seam exercises the
state machine but uses a *fake adapter*, so it cannot reproduce the real
adapter's latch. Coverage is therefore the cargo adversarial tests in this
change. This mirrors the `notification-actions` precedent (task 3.2 deferred
OS-toast rendering to manual QA as not-webview-assertable).

## Risks / Trade-offs

- **[Lobby false-re-entry after a UDP call]** Could removing the suppression
  arm let `mc && bc` from the lobby re-trigger entry? → Mitigated by the
  asymmetric exit signal: after `meeting-ended`, Chrome releases `getUserMedia`
  within ~1–2 s so `bc` drops; the lobby-only case is the pre-existing known
  limitation at canonical-spec line 107, which the latch never protected
  anyway (`notify_exit` resets the latch exactly when we return to `Idle`).
  Net new risk: none.
- **[Very short TURN call where `turn && bc` never co-occur]** → The latch
  never sets; the call exits on the 15 s UDP debounce via `bc`. Safe default;
  no deadlock, only slightly slower exit for that rare call.
- **[Existing tests encode the old behaviour]** → `otter_ai_persistent_mic_-
  blocked_until_notify_exit` asserts the deadlock is desired;
  `notify_exit_resets_turn_established_for_next_udp_detection` asserts the
  suppressed state. Both are amended in `tasks.md`. The Otter false-positive
  concern is re-documented as the pre-existing lobby limitation, not a
  regression introduced here.

## Context

The meeting detector lives in one monolithic Windows adapter
(`detection/windows.rs`) that fuses three vendor-bound signals into the entry gate:
a Meet title regex (`enumerate_meet_windows` + `meet_title_regex`), a Google-CIDR TCP
check (`has_meet_connection`, via `GetExtendedTcpTable`), and a WASAPI browser-capture
check (`has_browser_capture_session`). The pure state machine
(`use_cases/meeting_detection.rs::step_detector`) consumes an observation whose
`has_meet_connection` field already folds the CIDR+TURN+bc signals together, and whose
`meet_windows` field (populated by the title regex) drives a separate `has_title` gate
term:

```rust
// use_cases/meeting_detection.rs:86-94  (current)
let has_title = !observation.meet_windows.is_empty();
let has_conn  = observation.has_meet_connection;          // turn || (mc && bc)
let not_preexisting = /* connection_first_seen_at > detector_start */;
if has_title && has_conn && not_preexisting { /* Idle → InCall */ }
```

Two structural defects motivate this change:

1. **No vendor seam.** `has_meet_connection`, the Meet title regex, and
   `strip_google_meet_suffix` are inlined in the Windows adapter. Adding Zoom or Teams
   means appending to the monolith — new CIDR lists and title patterns interleaved with
   OS enumeration. There is no trait the second vendor implements.
2. **Title parsing is load-bearing for detection.** The Meet PWA emits an **EN dash**
   (U+2013) in `Google Meet - Meet – <code>`; regex branch 3 expects an **EM dash**
   (U+2014), so it matches nothing. Because `has_title` gates entry, the dash typo
   disabled **all** PWA detection — a single fragile pattern took down the core signal.
   Observed in a production sample 2026-06-26; the causal claim below is grounded in the
   WebRTC signaling model, with the sample as confirming evidence (see **Verification
   result** for the sample's scope — one 4 s in-call window).

The directional decision (user, 2026-06-26): **generalize to any conference software**,
with **title auto-detection retained as a second-order, nice-to-have feature — explicitly
not part of activity detection**. That direction forces two questions this design answers:
(a) what does a vendor-neutral *gate* key on, given Windows signal constraints, and
(b) where does vendor-specific knowledge (CIDR lists, title patterns) live so the core
stays neutral.

### The signal reality that constrains the gate

The detector's available Windows signals, by transport:

| Call transport | Detectable signal | Caveat |
|---|---|---|
| TCP TURN relay | `has_turn_connection` (TCP has remote addr) | Only when the call uses TURN — minority path |
| UDP-direct (WebRTC, no TURN) | **none vendor-neutral** | `GetExtendedUdpTable` exposes no remote address (UDP is connectionless) and QUIC confounds any PID-keyed UDP heuristic (`spec.md` Known-limitation) |
| Any browser call | `has_browser_capture_session` (WASAPI Active) | Necessary, not sufficient — any `getUserMedia` |
| Vendor signaling (e.g. Meet HTTPS) | Vendor-CIDR TCP check | Vendor-specific by definition |

The critical fact: WebRTC signaling (HTTPS/WSS over TCP) runs throughout a Meet call —
SDP exchange, ICE candidate trickle, room state — independent of the media transport, so a
Meet **UDP** call keeps TCP connections to Google IPs that `has_meet_connection` catches.
The 2026-06-26 production sample observed exactly this for one call (`mc=true && bc=true`,
`turn=false` over a 4 s in-call window). The WebRTC model says this holds generally; the
caveat is that TURN can be negotiated mid-call under adverse networks, so `turn=false` is
observed-for-this-call, not a universal property of Meet-UDP. The Google-CIDR check is the
load-bearing signal for the primary use case. A pure vendor-neutral gate (`bc + turn`)
would lose it.

ETW / Windows Filtering Platform flow events and the Chrome DevTools Protocol
`RTCPeerConnection` state are the only clean vendor-neutral "media flow active" signals;
both are blocked for a user app (admin privileges; `--remote-debugging-port`). They are
out of scope for v1.

## Goals / Non-Goals

**Goals:**
- Introduce a vendor-neutral seam (`CallSignalingPort`) so a second vendor adapter can
  land without editing the Windows adapter or the state machine.
- Decouple title extraction from detection (`MeetingTitleExtractorPort`) so a title-pattern
  failure can never again disable call detection.
- Preserve Meet-UDP coverage (the primary use case) — the regression risk of
  de-vendoring the gate.
- Ship the EN-dash fix as part of the Meet title-extractor adapter, demoted from
  load-bearing regex to best-effort decorator.
- Document the broadened FP surface (D4) honestly rather than silently.

**Non-Goals:**
- Zoom / Teams / Webex signaling or title adapters — no caller yet (YAGNI; the second
  adapter and the multi-adapter aggregator arrive together when a real second vendor
  lands).
- A `Vec<Box<dyn CallSignalingPort>>` aggregator before the second adapter exists.
- Native-app process-name detection (Zoom.exe / Teams.exe) — a separate detection path.
- The green-room lobby discriminator — deferred to `meeting-lobby-discrimination`, now
  more urgent (D4).
- Any change to the exit / debounce logic (owned by
  `meeting-udp-confidence-debounce`, now archived — out of scope regardless; see D6).
- TURN detection, WASAPI enumeration, window enumeration — these stay in the Windows
  adapter; only the vendor-CIDR and title-pattern code is extracted.

## Decisions

### D1 — Two ports, not one

`CallSignalingPort` (gate) and `MeetingTitleExtractorPort` (decoration) are separate
traits, not one combined `VendorAdapter` port. **Why:** they have different lifecycles
and failure tolerances. Signaling is a hot-path gate (polled every ~2 s, must be fast,
may never block on title work); title extraction is a one-shot post-transition decorator
(runs on `Idle → InCall`, tolerates `None`, can do regex work). Coupling them into one
port forces the gate to depend on title parsing — exactly the fragility the dash bug
exposed. Separate ports preserve independent failure: a title-pattern regression breaks
the recording name, not the recording start.

### D2 — Per-vendor CIDR adapters behind `CallSignalingPort` (chosen strategy)

The port trait is vendor-neutral:
```rust
pub trait CallSignalingPort: Send + Sync {
    fn is_call_signaling_active(&self) -> bool;
}
```
Each vendor ships an adapter carrying its own CIDR list (the Meet CIDR constants already
live in `detection/google_cidrs.rs`; the adapter imports them — they are not moved out of
`windows.rs` because they were never inlined there). v1 wires one adapter
(`MeetSignalingAdapter` = the existing `has_meet_connection` TCP-table scan, extracted with
no logic change). The detector ORs adapter results into `signaling_active`; with one
adapter the OR is trivially that adapter's result. The state machine's entry term becomes
`has_conn = turn || (signaling_active && bc)`.

**Shared helper required by the extraction.** `has_meet_connection`'s scan calls
`is_browser_process(pid)`, which calls `process_name_for_pid` (unsafe Win32 `OpenProcess` /
`QueryFullProcessImageNameW`). That same pair is also used by `has_browser_capture_session`'s
WASAPI check. The extraction promotes `is_browser_process` + `process_name_for_pid` to a
shared `detection/browser_process` helper used by BOTH the signaling adapter and the
remaining `windows.rs` — neither can own a private copy, and the scan cannot move cleanly
without it. The "verbatim move" is of the TCP-scan logic; the browser-process helper is a
new shared module (§2 tasks).

**Why this preserves coverage** (grounded in the WebRTC signaling model, with the single
capture as confirming evidence — see Verification result): WebRTC signaling (HTTPS/WSS over
TCP) runs throughout a call for SDP exchange, candidate trickle, and room state, independent
of media transport — so Meet's UDP calls keep TCP connections to Google IPs and remain
detected as `signaling_active=true && bc=true`. The generalization is architectural (the
seam exists; a Zoom adapter is a new file, not an edit to `windows.rs`), not a weakening of
the signal.

**Rejected alternatives:**

- **Pure vendor-neutral gate (`bc + turn` only).** Regresses the primary use case: Meet-UDP
  calls go undetected. WebRTC signaling over TCP is the catch (observed in the 2026-06-26
  sample; TURN is not guaranteed for UDP calls and can be absent for an entire session).
  Rejected.
- **Hybrid (title extractor feeds the gate for recognized windows).** Re-couples the
  failure modes D1 separates, and contradicts the user's explicit "title is not for
  detection." Rejected.
- **Per-vendor monolith (keep appending CIDR lists / regexes to `windows.rs`).** No seam;
  the generalization problem this change exists to solve persists. Rejected.
- **Block v1 on ETW / CDP.** Both need admin / `--remote-debugging-port`; out of reach for
  a user app. Rejected for v1; remains the only path to a *truly* vendor-neutral
  media-flow signal, worth revisiting if the admin constraint ever lifts.

### D3 — Drop `has_title` from the gate; title becomes pure decoration

Entry loses the `meet_windows.is_empty()` term:
```rust
// after this change
let has_conn = observation.has_meet_connection;   // turn || (signaling_active && bc)
if has_conn && not_preexisting { /* Idle → InCall */ }
```
Title extraction moves to a `MeetingTitleExtractorPort` call made **after** the transition
fires, to resolve `default_title`. `None` falls through to the existing generic-timestamp
default (`Meeting <YYYY-MM-DD HH:MM>`).

**Observation-type refactor (consequence of D3 + the extractor move):** the
`meet_windows: Vec<MeetWindow>` field on `DetectorObservation` — where `MeetWindow` is
documented as "matches the Google Meet title pattern" (`meeting_detector.rs:4`) — becomes a
semantic lie once the Meet filter moves into the extractor. It is renamed
`browser_windows: Vec<BrowserWindow>` (carrying `{hwnd_id, pid, title}`), and
`enumerate_meet_windows` is retargeted to `enumerate_browser_windows`: the `EnumWindows`
callback (today an `unsafe` block that pre-filters by the Meet-title regex) is rewritten to
collect all visible browser-process windows and post-filter nothing by title — the
Meet-title pattern moves entirely into `MeetTitleExtractor::extract_title`. This is a
signature change to the observation struct and its test fixtures (§6 tasks). The field no
longer gates entry; it only feeds the post-detection extractor.

This is the user's explicit design call ("title is not for activity detection"). Its
consequence — a broadened FP surface — is D4.

**Scenario-replacement scope (disclosed).** The delta's MODIFIED entry requirement replaces
the canonical scenarios wholesale — the "Meet tab open but user has not joined", "User joins
muted", "Spotify desktop app", "Spotify in browser + dictation", and "Discord PWA" scenarios
do not survive; the new scenario set (Join, PWA EN-dash, title-less entry,
signaling-false-blocks, Meet home page, green-room join/abandon, cross-vendor FP) covers the
post-`has_title`-removal behavior space. The heading "Detect active Google Meet calls on
Windows" is retained for canonical-spec continuity and renamed when the second vendor adapter
lands. This wholesale replacement is deliberate, not a miss.

### D4 — Accept the broadened FP surface; defer the discriminator

D3 arms two false-positive classes that the title gate previously suppressed:

1. **Green-room FP goes universal — two paths, not one.** Today the title gate accidentally
   suppresses the green-room FP for the PWA (the dash bug makes `has_title=false`).
   Browser-tab Meet already has the FP latent (title matches). Dropping `has_title` arms it
   for **all** Meet configurations. Two paths:
   - **Join path (benign):** the user clicks *Join now*. The proposal's own verification
     (Verification result) shows `getUserMedia` does **not** drop across the green-room →
     in-call transition — so the "FP" is a few-seconds-early start of a real recording, not
     spurious. The user gets an early-but-correct recording. Caveat: "benign" is relative,
     not absolute — the early start captures a few seconds of **pre-join mic + system audio**
     (the user adjusting levels, a side remark) before they click *Join*. Meetily records
     audio only (no camera frames), so the exposure is audio, not video; still
     privacy-weighted, just less severe than the abandon path's open-ended spurious run.
   - **Abandon path (spurious):** the user leaves the green room without joining. `bc`
     drops within seconds → bc-drop exit fires `meeting-ended`. **Caveat (F6 risk):** if
     Meet holds `getUserMedia` open after abandonment (the idle-eviction behavior in
     exploration F6), the bc-drop exit does **not** fire and the spurious recording runs
     until manual stop — an untested failure mode of the "bounded" claim.
   Discriminator deferred to `meeting-lobby-discrimination`; **more urgent under this
   architecture** (the FP surface grew from PWA-only to universal). The highest-leverage
   untested discriminator signal is eRender-in-green-room (exploration F5: does incoming
   audio go Active=0 in the green room while capture stays Active=1?); tasks §8 captures it
   opportunistically during live verification so `meeting-lobby-discrimination` opens with
   data, not inference.
2. **Cross-vendor CIDR+bc imprecision (new, unmeasured).** `mc=true` (Gmail / Drive TCP)
   coincident with `bc=true` (a non-Meet browser call) satisfies the Meet signaling adapter
   and triggers a generic-tagged detection — a coincidence that is common in practice
   (pinned Gmail + any browser WebRTC call), not rare. CIDR+bc cannot prove the capture
   belongs to the CIDR's vendor. The title gate only partially mitigated this (it blocked
   detection only when no Meet-titled window was present; a Meet tab alongside the non-Meet
   call defeated it). v1 ships with the imprecision; **severity is unknown until measured**
   (Open Questions). It does not self-correct until a vendor-confirmation gate ships —
   "narrows as adapters proliferate" is a future cost, not a v1 remedy. Documented as a
   Known limitation in the spec delta.

Both are accepted for v1: the green-room FP's abandon path is bounded by the bc-drop
exit (F6 caveat aside; the join path dissolves into the real recording, so it is not
bounded by an exit — it simply stops being an FP); the cross-vendor FP requires a
coincidental browser call, and its severity is unmeasured — it does not narrow until a
vendor-confirmation gate ships. If either proves severe in practice,
`meeting-lobby-discrimination` is fast-tracked.

### D5 — v1 ships Meet-only adapters; no aggregator yet (YAGNI)

One `CallSignalingPort` adapter (Meet) and one `MeetingTitleExtractorPort` adapter (Meet)
are wired at the composition root. The "OR of signaling adapters" is the single adapter's
result. A `Vec<Box<dyn CallSignalingPort>>` aggregator and a corresponding title-extractor
chain arrive **with the second vendor adapter** — the trigger for that plumbing, not this
change. The port traits exist now (the architectural commitment); the multi-adapter
plumbing does not (no second caller). This keeps v1 to a clean extraction + rewire with
no speculative abstraction.

### D6 — Sequencing relative to `meeting-udp-confidence-debounce` (resolved)

`meeting-udp-confidence-debounce` was archived 2026-06-25; its exit/debounce edits to
`detection/windows.rs` and `use_cases/meeting_detection.rs` have landed in the canonical
spec and code. D6 is therefore a **no-op**: this change rebases directly onto the
post-debounce state. No dual-in-flight reconciliation is needed; the extraction sites
(`has_meet_connection`, `strip_google_meet_suffix`, the title regex,
`enumerate_meet_windows`) are at their post-debounce locations.

### D7 — Adapter file layout

```
detection/
├── windows.rs              ← slimmed: window enum + WASAPI + TURN only
├── signaling/
│   ├── mod.rs              ← (v1: re-export Meet; aggregator lands with 2nd vendor)
│   └── meet.rs             ← MeetSignalingAdapter (Google CIDR, extracted)
└── titles/
    ├── mod.rs              ← (v1: re-export Meet)
    └── meet.rs             ← MeetTitleExtractor (regex + suffix strip, EN-dash fix)
ports/
├── meeting_detector.rs     ← unchanged trait
├── call_signaling.rs       ← NEW trait
└── meeting_title_extractor.rs ← NEW trait
```

The `detection/signaling/mod.rs` and `detection/titles/mod.rs` files exist in v1 as
re-export shims so the second vendor lands as an additive edit (new file + one re-export
line), not a structural change.

### D8 — `step_detector` stays pure; the extractor wires into the detector adapter

`step_detector` (`use_cases/meeting_detection.rs:76`) is a pure free function — no injected
traits, only data (`&DetectorObservation`, `&DetectorSettings`, `&AtomicBool`) — and its
purity is what makes the 25+ state-machine unit tests trivially drivable. Adding
`MeetingTitleExtractorPort` as a generic trait parameter would cascade: every test call
site gains a type parameter, and the priority-ordering logic would enter the pure function.

**Decision — extractor injected into the adapter, not the use case.** `step_detector` AND
`spawn_detector` are both unchanged in signature. The `WindowsMeetingDetector` struct gains
a `title_extractor: Arc<dyn MeetingTitleExtractorPort>` field, injected at the composition
root (`lib.rs`) alongside the existing test-probe seam. The priority chain stays where its
platform inputs are: `resolve_default_title` remains in `windows.rs` (it calls
`foreground_window_title()` and reads `focus_history`, both Win32 / adapter state), but its
Meet-regex calls (`meet_title_regex()`, `strip_google_meet_suffix`) are replaced by
`extractor.extract_title(&[BrowserWindow { .. }])` — first `Some` wins per priority step
(foreground → recent-focus → first-enum → timestamp). The adapter's `current_state()`
populates `observation.default_title` via this call, exactly as today (windows.rs:827);
`step_detector` forwards it into `MeetingDetected.default_title`, unchanged.

**Signature changes (disclosed).** Three mechanical ripples follow from the adapter option;
each is a signature change, not a design change:
- `resolve_default_title` (windows.rs:55) is a **free function**, not a method — it has no
  `&self` to reach a field. It gains an `extractor: &dyn MeetingTitleExtractorPort` parameter;
  its **two** call sites — `current_state` (windows.rs:827, production: passes
  `&self.title_extractor`) and the `resolve_default_title_fallback_is_non_empty` test
  (windows.rs:996, passes a `NoOpTitleExtractor`) — are both updated.
- The foreground and recent-focus steps carry **real** window metadata, not fabricated
  values. `foreground_window_title()` is widened to return a full `BrowserWindow { hwnd_id,
  pid, title }` (field name `hwnd_id` retained from the existing `MeetWindow` for
  platform-neutrality — `HWND` cast to `usize` at the Win32 boundary, as today at
  windows.rs:206; the PID comes from `GetWindowThreadProcessId` — one more Win32 call), and
  `FocusHistory` stores `BrowserWindow`s rather than bare `(String, Instant)` tuples, so the
  recent-focus step iterates real windows. Every priority step now passes a real
  `BrowserWindow` to `extract_title`; every field is a real captured value, never a dummy.
  The load-bearing property is no-fabrication: the alternative (bare title strings in
  `FocusHistory` + dummy `hwnd_id`/`pid` constructed in `resolve_default_title`'s
  recent-focus step) is exactly the invariant violation this avoids. Making the fields
  `Option` or splitting the trait would be YAGNI until an extractor that reads
  `hwnd_id`/`pid` exists.
- `WindowsMeetingDetector::new(focus_history)` (windows.rs:623) gains a
  `title_extractor: Arc<dyn MeetingTitleExtractorPort>` parameter; the sole production call
  site is `lib.rs`. The `#[cfg(test)] with_probes(focus_history, probes)` constructor
  (windows.rs:660) auto-injects a `NoOpTitleExtractor` (returns `None` → timestamp fallback),
  so the 11 existing `with_probes` test call sites are **unchanged**; a 12th test
  (windows.rs:1626, `fresh_detector_after_crash_has_no_inherited_latch`) calls
  `new(empty_history())` directly and switches to `with_probes` to inherit the NoOp
  auto-injection (tasks §7.1). Only the §5.1 test injects a configured stub.
  This is strictly less churn than the spawn-loop
  alternative (which would force `spawn_detector` to gain the extractor AND `DetectorObservation`
  to carry `foreground_title` + a focus-history snapshot — 37 fixture edits).

**Why the spawn-loop override (earlier draft) was rejected.** A prior draft wired the
extractor into `spawn_detector` and overrode `default_title` after `step_detector` returned.
That is incoherent once the regex moves: `resolve_default_title` calls the Meet regex at
each priority step, and that regex moves to the titles adapter — so `resolve_default_title`
cannot stay in `windows.rs` without the extractor, and a single post-transition extractor
call cannot reproduce the per-step priority walk (the extractor sees the whole
`browser_windows` slice, not the foreground-then-recent-focus-then-first-enum ordering).
Wiring the extractor into the adapter keeps the priority chain intact and co-located with
its platform inputs.

**Why not move the priority chain into the use case.** The chain reads
`foreground_window_title()` (Win32) and `focus_history` (adapter state); relocating it to
the use case would force `DetectorObservation` to carry `foreground_title` + a focus-history
snapshot as raw data — bloating the struct and every one of its 37 test fixtures (verified
count). KISS: the chain stays where its platform inputs already live. (This amends the spec delta's earlier
"priority ordering lives in the use case, not the adapter" line, which was wrong for the
same reason — the ordering needs platform access.)

**Hexagonal legality.** An adapter may depend on a port trait (the seam is the trait, not
the concrete `MeetTitleExtractor`); the concrete adapter is selected at the composition root.
`WindowsMeetingDetector` already depends on `MeetingDetectorPort` (the trait it implements);
gaining a second port field is the same kind of seam, wired at the same root.

**Coverage consequence (D9).** `spawn_focus_tracker` and the `connection_first_seen_at` gate
also reference Meet-specific code that moves; `candidate_titles` is built inside
`step_detector` from Meet-windows today. See D9.

### D9 — Code-contract consequences of the `meet_windows` → `browser_windows` rename

The rename (D3) and the regex/suffix move touch the following call sites; each needs an
explicit decision, not a silent find-and-replace:

1. **`connection_first_seen_at` gate (windows.rs:805).** Today:
   `has_conn && !meet_windows.is_empty() && connection_first_seen_at.is_none()`. The
   `!meet_windows.is_empty()` term guarded against Gmail/Drive Google-TCP at startup
   stamping a "new" connection before the user opens Meet. Under the rename it becomes
   `!browser_windows.is_empty()` (true for ANY browser window). **Decision: drop the term.**
   `has_conn` already requires `turn || (mc && bc)`, and `bc`
   (`has_browser_capture_session` — `getUserMedia` active) is false for a Gmail tab with no
   call. The `bc` term subsumes the old window gate; the Gmail-at-startup case cannot stamp
   because `has_conn` is false. The inline comment is updated to say so.

2. **`spawn_focus_tracker` (windows.rs:836-860).** Today it filters foreground titles
   through `meet_title_regex()` before pushing to `focus_history`. After the regex moves,
   the import breaks. **Decision: drop the regex filter.** The tracker records the full
   foreground `BrowserWindow { hwnd_id, pid, title }` (captured via `GetForegroundWindow` +
   `GetWindowThreadProcessId` + `GetWindowText`) whenever the foreground process is a
   browser (the shared `is_browser_process` check — vendor-neutral); `FocusHistory` stores
   `BrowserWindow`s, not bare title strings. The extractor decides at resolution time
   whether a recorded window matches. This widens focus history to all browser windows
   (the extractor filters) — the correct vendor-neutral behavior — and it feeds the
   recent-focus priority step real window metadata (D8).

3. **`candidate_titles` + the `lib.rs` import (meeting_detection.rs:96-100, lib.rs:963).**
   Today `step_detector` builds `candidate_titles` from `observation.meet_windows` titles,
   and `lib.rs` calls `strip_google_meet_suffix` on each at emit time. After the rename +
   the suffix move, both break: `step_detector` would collect ALL browser-window titles
   (noise), and the `lib.rs` import path is stale. **Decision:** the adapter populates
   `observation.candidate_titles` during `current_state()` by iterating `browser_windows`
   and calling `extractor.extract_title(&[w])` per window, collecting the `Some` results
   (each matched window's stripped title) — the same per-window call shape
   `resolve_default_title` uses at each priority step (the port's `&[BrowserWindow]` slice
   is always a single-element view in v1; the "pick best from N" interpretation is unused).
   `step_detector` forwards `observation.candidate_titles.clone()` (one-line change, no
   Meet-specific logic in the pure function); the `lib.rs:963` `strip_google_meet_suffix`
   map is **deleted** (stripping now happens in the adapter via the extractor).
   `DetectorObservation` gains a `candidate_titles: Vec<String>` field; test fixtures add
   `candidate_titles: vec![]`.

4. **`FakeMeetingDetector` + the `Default` impl (fake.rs:57-80, meeting_detector.rs:60-72).**
   The dev-detector fake's `apply()` constructs `DetectorObservation` literally with
   `meet_windows: vec![MeetWindow{..}]` (fake.rs:63) and imports `MeetWindow` (fake.rs:10);
   after the rename it needs `browser_windows: vec![BrowserWindow{..}]` + the new
   `candidate_titles` field, or it will not compile under `--features dev-detector`. The
   `DetectorObservation` `Default` impl (meeting_detector.rs:60-72) likewise needs the new
   field. **Decision:** update both as part of the §6.1a fixture pass; no behavior change
   (the fake's joined snapshot keeps one synthetic window).

### D10 — Frontend detection→UI wiring contract (camelCase keys, global listener, orphan cleanup)

The vendor-neutral backend is inert without the frontend wiring that turns
`meeting-detected` into a titled recording. Three contract fixes — all surfaced
during §8 live verification (2026-06-27) — land in this change because each
silently defeated a user-visible guarantee of the detection work. They are grouped
here (not split into separate changes) because they are the same contract: the
detection→UI→disk pipeline that makes vendor-neutral detection observable.

1. **camelCase invoke keys (Tauri v2).** `recordingService.startRecordingWithDevices`
   and `useAutoDetect`'s `cancel_recording` call were passing snake_case JS keys
   (`meeting_id`, `mic_device_name`, …). Tauri v2 auto-converts camelCase JS keys →
   snake_case Rust params; snake_case JS keys do **not** bind → `Option<T>` params
   silently default to `None`. Live symptom: `start_recording_with_devices_and_meeting`
   logged `Meeting: None, detector_started: None`, so the extracted title never
   reached the recording folder. Fix: `micDeviceName` / `systemDeviceName` /
   `meetingName` / `detectorStarted`, and `meetingId` for `cancel_recording`. **Why
   here:** this IPC hop is the exact pipeline that carries the vendor-neutral title
   to disk; without it the `MeetingTitleExtractorPort` output is dropped at the
   boundary and the EN-dash fix (D3/§3) is unobservable.

2. **Listener hoist to `RecordingControlProvider`.** `useAutoDetect` was mounted only
   in `app/page.tsx`, so the `meeting-detected` / `meeting-ended` listeners unmounted
   on any navigation (e.g. to `/meeting-details`) and silently dropped events. Hoisted
   to a global provider in `layout.tsx` (non-onboarding branch — hooks must not run
   during onboarding), relocating the optimistic `isRecording` pair, the four recording
   hooks, and the `AutoDetectBanner` overlay (the banner is `fixed z-50`, so it works
   as a global overlay). Precedent: `RecordingPostProcessingProvider` already mounts
   global tray/shortcut listeners the same way. The hook is mounted exactly once
   (grep-confirmed: no second mount site), so `useRecordingStart`'s
   sessionStorage/sidebar-window/continue-requested effects do not double-fire.
   **Why here:** detection that only works on one route is not detection of this
   change's contract.

3. **Orphan-listener cleanup (StrictMode race).** `useAutoDetect`'s listener effect is
   async (`await listen()`); cleanup ran before setup resolved, so the returned
   `unlistenDetected?.()` was a no-op on `undefined`, leaving the listener subscribed.
   React StrictMode dev mount→unmount→remount accumulated orphans, fanning one
   `meeting-detected` emit out to N `start_recording` calls (observed: 3× at 16:56:02Z).
   Fix: a `cancelled` flag set in cleanup; `setup` checks it post-resolve and tears down
   the just-registered listeners if cleanup already ran. The smoke spec is tightened to
   assert exactly one `start_recording_with_devices_and_meeting` per emit
   (`callLogCount === 1`) — the assertable contract for this race. The race itself
   predates this change (the hook internals are unchanged by the hoist), but it is fixed
   here per an explicit scope decision because it churns the very recording pipeline the
   change delivers; in production (no StrictMode double-mount) the orphans do not
   accumulate the same way, but the dep-change re-subscription path is still racy without
   the flag.

## Risks / Trade-offs

- **[Green-room FP universal (D4.1)]** → Removing the title gate arms the FP for all Meet
  configs, not just PWA. **Mitigation**: join-path is a benign early start (not spurious);
  abandon-path is bounded by the bc-drop exit **unless** Meet holds `getUserMedia` (F6 risk,
  untested). Documented as a Known limitation; discriminator tracked in
  `meeting-lobby-discrimination`, fast-trackable if severe; the eRender-in-green-room signal
  is captured opportunistically in §8 verification to unblock the discriminator.
- **[Cross-vendor CIDR+bc FP (D4.2) — unmeasured]** → Gmail/Drive TCP + a non-Meet browser
  capture triggers a generic-tagged detection; common coincidence, not rare. **Mitigation**:
  documented as Known limitation; **severity unknown until measured** (Open Questions); the
  extractor returns `None` (generic title) for unrecognized windows so the recording is at
  worst generically named, never mis-attributed to Meet by title. Does not self-correct
  until a vendor-confirmation gate ships.
- **[Extraction refactor risk]** → Moving the TCP-table scan and the title regex out of
  `windows.rs` risks behavioral drift (dual-stack IPv6 unwrap, regex branches, the shared
  `is_browser_process` / `process_name_for_pid` helper — see D2). **Mitigation**: the scan
  and regex logic move with **no logic change**; the shared browser-process helper is
  promoted once and imported by both call sites. The existing unit tests
  (`title_parsing_pwa_format`, the CIDR tests) move with the code and must pass unchanged
  before the EN-dash fixture correction (§3) is applied.
- **[CIDR maintenance burden]** → Vendor CIDRs drift (Google ranges expand; cloud CDNs).
  **Mitigation**: each adapter owns its list; a stale CIDR degrades one vendor's coverage,
  not the architecture. Out of scope for v1 (Meet CIDR is stable).
- **[TURN-less non-Meet browser calls undetected]** → A Zoom web call with no Google TCP
  open is not detected in v1 (no Zoom adapter). **Mitigation**: expected — v1 is Meet-only;
  a Zoom adapter is the canonical second-vendor trigger for D5's plumbing.
- **[Sequencing]** → `meeting-udp-confidence-debounce` archived 2026-06-25; no conflict
  (D6 is a no-op).

## Verification result

Carried over from the pre-rework §1 observation (2026-06-26,
`RUST_LOG=app_lib::detection=debug,app_lib::use_cases=debug`, **unfixed** adapter — regex
branch 3 still EM dash, suffix splitter still EM dash, `has_title` still gating). This data
grounds D2 (why the CIDR adapter is load-bearing for Meet-UDP) and D4 (the FP is a property
of the gate signals, not the regex).

### Green room (pre-join camera preview) — 06:39:52 → 06:40:27 UTC

```
detector poll: windows=[]
detector poll: has_meet_connection=true has_browser_capture_session=true
```
Single `bc transition: false → true` at 06:39:52, then stable for ~35 s. No TURN, no
`meeting-detected` (the dash bug kept `windows=[]`, so `has_title=false` blocked entry).

### In-call (real PWA Meet call, UDP) — 06:49:47 → 06:49:51 UTC

```
detector poll: windows=[]
detector poll: has_meet_connection=true has_browser_capture_session=true
```
No second `bc transition` (getUserMedia never dropped across green-room → in-call). No
TURN, no `meeting-detected` (same dash-bug block).

### What the sample confirms (and its limits)

The sample is one ~35 s green-room window and one 4 s in-call window, same machine, same
session. It is **confirming evidence** for the WebRTC-grounded claims, not their sole basis.

1. **`mc` is load-bearing for Meet-UDP (confirms D2).** Both states read
   `has_meet_connection=true` with `turn=false`. The WebRTC model says signaling-over-TCP
   is present for any Meet call; this sample confirms it for one UDP call. The Google-CIDR
   check — not TURN — is what would detect the UDP call once `has_title` no longer blocks.
   Dropping vendor knowledge from the gate (the rejected pure-neutral alternative) would
   lose this.
2. **The FP is a gate-signal property, not a regex property (confirms D4).** Both states
   are detector-identical on `mc` and `bc`. Under D3 (no `has_title` gate), both would fire
   `meeting-detected` — confirming the green-room FP is armed by dropping the title gate,
   independent of the dash fix.
3. **TURN-at-entry would miss this call.** No TURN relay observed for this UDP call; a
   `has_turn_connection`-at-entry discriminator (exploration F5) would read false here.
   Notes the discriminator is hard; does not prove TURN is absent for all Meet-UDP calls
   (TURN is network-dependent — see D2 caveat).

Note: under the **current** (pre-rework) adapter, `meet_windows=[]` because the EN-dash
title fails branch 3 entirely. Under the reworked design the title isn't consulted for entry,
so `browser_windows` content is irrelevant to detection — it only feeds the post-detection
extractor.

### 2026-06-27 post-fix live verify (`RUST_LOG=app_lib=info,app_lib::detection=debug`)

Two real Meet calls (PWA), CPU/dev build. Local time is RDT (UTC+2); log timestamps
are UTC (`Z`), so `16:xxZ == 18:xx` local — not a clock gap.

- **16:35:47Z** — `bc transition: false → true` (signaling_active=true &&
  has_browser_capture_session=true) → `meeting-detected` → auto-start:
  `🚀 CALLED start_recording_with_devices_and_meeting - Meeting: Some("Meeting
  2026-06-27 18:35"), detector_started: Some(true)`. Recording folder
  `Meeting 2026-01-01_00-00_...` created with the title prefix.
- **16:56:02Z** — second detection while the user was navigating `/meeting-details`
  (off the home page). Identical binding (`Some(title)`, `Some(true)`). **This is the
  D10.2 hoist proof:** the event reached `start_recording` from a route where the old
  page-local listener would have unmounted. (At this instant the orphan race was still
  unfixed → 3× `🚀 CALLED`; reproduced and fixed per D10.3, regression-guarded by the
  tightened smoke assertion `callLogCount('start_recording…') === 1`.)

What this confirms:

1. **The vendor-neutral gate fires (D3):** entry keys on `signaling_active && bc`; the
   title is not consulted for entry.
2. **camelCase keys bind (D10.1):** `Some(title)` + `Some(detector_started=true)` is
   impossible under the old snake_case JS keys (they would both be `None`).
3. **The hoist works (D10.2):** off-page detection reached `start_recording`.

**Known limitation observed — title timing, not regex.** The recording title was the
**fallback** `Meeting <YYYY-MM-DD HH:MM>`, not an extracted Meet code. At both detection
instants the window title was the intermediate `"Google Meet - Meet"` (no EN dash, no
code); the in-call title `"Google Meet - Meet – gae-qfsw-oye"` appeared ~90 s later. The
`MeetTitleExtractor` correctly returns `None` for the intermediate title (the EN-dash
regex itself is unit-proven for the PWA format in §3.1–3.6); `bc` simply transitions
before Meet writes the code into the title. The best-effort decorator behaves as designed
(D3); a post-detection re-extract (re-resolve the title a few seconds after entry, once
Meet has populated the code) is a follow-up enhancement, not a v1 correctness gap.

**Not verified live (deferred — they inform `meeting-lobby-discrimination`, not this
change's correctness):** green-room join/abandon FP paths (§8.2–8.5), eRender-in-green-room
sampling (F5), and the cross-vendor CIDR+bc FP (§8.5). These are empirical data for the
D4 FP analysis; the change's own contract (detect → title binds → off-page listener → no
orphan fan-out) is fully covered by the two live samples above + the cargo/smoke gates.

## Open Questions

- **Cross-vendor FP severity in practice.** D4.2 is reasoned, not measured. Worth a
  follow-up observation run: Gmail tab + a non-Meet browser call, confirm whether the Meet
  adapter fires. If it fires routinely, fast-track `meeting-lobby-discrimination` or add a
  vendor-confirmation gate.
- **Second-vendor trigger.** Which vendor (Zoom web / Teams web / Webex) is the realistic
  first second adapter? That choice determines whether the CIDR-adapter strategy holds
  (Zoom/Teams publish-ish ranges) or whether a non-CIDR signaling signal is needed
  (vendors on shared cloud CDNs). Deferred — answered when a real second vendor is
  requested.
- **QUIC / HTTP3 signaling migration.** The CIDR-TCP check keys on `GetExtendedTcpTable`
  (established TCP sockets). WebRTC signaling-over-TCP holds today, but if Meet migrates
  signaling to QUIC/HTTP3 (transport-over-UDP), the TCP scan silently breaks while the port
  trait stays valid. A future adapter may need a UDP-flow or connection-attribution signal;
  not a v1 defect, but the adapter contract should not assume TCP forever.
- **Does the title extractor need the focus-tracker?** The current title-resolution priority
  (foreground → recent-focus → first-enum → timestamp) lives partly outside the regex.
  Extraction may need to carry the focus-tracker dependency into the adapter, or the
  priority logic stays in the use case and the adapter is purely pattern-matching. Resolved
  in §3 tasks.

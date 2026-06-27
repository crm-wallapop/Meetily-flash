# Tasks

## 1. Introduce the two port traits

- [x] 1.1 `frontend/src-tauri/src/ports/call_signaling.rs`: define
  `pub trait CallSignalingPort: Send + Sync { fn is_call_signaling_active(&self) -> bool; }`.
  Add a unit test with a stub impl asserting it returns a configured value — drives the
  signature. Export from `ports/mod.rs`.
- [x] 1.2 `frontend/src-tauri/src/ports/meeting_title_extractor.rs`: define
  `pub trait MeetingTitleExtractorPort: Send + Sync { fn extract_title(&self, windows: &[BrowserWindow]) -> Option<String>; }`.
  Stub-impl test drives the signature (slice in, `Option<String>` out). Export from
  `ports/mod.rs`.

## 2. Extract `MeetSignalingAdapter` verbatim (no behavior change)

- [x] 2.1 **(red)** `detection/signaling/meet.rs`: test that
  `MeetSignalingAdapter::is_call_signaling_active()` reproduces `has_meet_connection()`
  for a fixture `GetExtendedTcpTable` result — including the dual-stack IPv6-mapped unwrap
  (`::ffff:x.x.x.x` → IPv4 before CIDR match, `spec.md` Implementation note). Fails:
  adapter doesn't exist yet.
- [x] 2.2 **(red → adversarial)** empty TCP table (no connections) → `false`; a connection
  whose remote addr is on the CIDR boundary returns the correct inclusive/exclusive
  result (off-by-one guard).
- [x] 2.3 **(green)** Move the TCP-table scan — `has_meet_connection`,
  `check_tcp4_connections`, `check_tcp6_connections`, and the dual-stack IPv6-unwrap — from
  `detection/windows.rs` into `detection/signaling/meet.rs` as `MeetSignalingAdapter impl
  CallSignalingPort`. The Google CIDR constants already live in `detection/google_cidrs.rs`
  and **stay there** (the adapter imports them — they are not moved). The call site in
  `windows.rs` (the `mc` computation at the `has_conn` fold) now goes through the adapter.
  Move the existing CIDR unit tests with the code — they pass unchanged (the scan logic is
  extracted verbatim). Add `pub mod signaling;` to `detection/mod.rs` and the
  `detection/signaling/mod.rs` re-export shim (D7).
- [x] 2.3a **(green, shared helper)** Promote `is_browser_process` + `process_name_for_pid`
  (unsafe Win32 `OpenProcess` / `QueryFullProcessImageNameW`) out of `windows.rs` into a new
  shared `detection/browser_process.rs`, imported by BOTH the signaling adapter (the TCP
  scan) and the remaining `windows.rs` (the WASAPI browser check). Required because both
  call sites use this pair — the scan cannot move cleanly without it (D2).
- [x] 2.4 Confirm `cargo test -p app_lib detection::signaling` green and that `windows.rs`
  references only the **shared** `browser_process` helper from the moved code (not the
  TCP-scan functions themselves, which now live in the adapter).

## 3. Extract `MeetTitleExtractor` + EN-dash fix

- [x] 3.1 **(red)** `detection/titles/meet.rs`: port the `title_parsing_pwa_format` test
  fixtures from EM dash (`\u{2014}`) to EN dash (`\u{2013}`), matching the format observed
  in the wild (`Google Meet - Meet – opv-augt-jbm`). Fails: the regex still expects EM
  dash.
- [x] 3.2 **(red → adversarial, non-Latin identifier)** `MeetTitleExtractor::extract_title`
  on `Google Meet - Meet – 🎡 Search XP Playground (new)` returns
  `Some("🎡 Search XP Playground (new)")` — emoji, spaces, parentheses (the Meet-Space /
  green-room title shape).
- [x] 3.3 **(red → adversarial, suffix strip)** `strip_google_meet_suffix` on
  `Google Meet - Meet – opv-augt-jbm` → `opv-augt-jbm`. Currently returns the whole string
  (splits on EM dash, no match).
- [x] 3.4 **(red → adversarial, negative assertion)** the EM-dash variant
  `Google Meet - Meet \u{2014} Test` does **not** match — the fix must not silently accept
  both dash types.
- [x] 3.5 **(red → boundary)** empty `windows` slice → `None`; a slice of all-non-Meet
  browser windows (e.g. `[Gmail - Inbox, Zoom Meeting]`) → `None` (the D4.2 cross-vendor
  mitigation — no Meet window in the slice; confirms the port's "None when no wired vendor
  matches" contract for a non-empty slice); oversized title (>1024 chars) → `None` (defensive
  bound per the `MeetingTitleExtractorPort` spec requirement — CLAUDE.md §9 boundary check).
- [x] 3.6 **(green)** Move `meet_title_regex`, `strip_google_meet_suffix`, and the
  Meet-title regex (the branch-3 pattern + the pre-filter inside the `EnumWindows` callback)
  into `detection/titles/meet.rs` as `MeetTitleExtractor impl MeetingTitleExtractorPort`.
  Fix branch 3 and the suffix splitter: `\u{2014}` → `\u{2013}`; update the in-source dash
  comments (EM → EN). All §3 tests pass — including the
  `adversarial_4_1_sql_injection_and_path_traversal` test
  (`Meet - '; DROP TABLE meetings; --` / `Meet - ../../etc/passwd`), which moves with
  `strip_google_meet_suffix` and passes unchanged (downstream `sanitize_filename` +
  parameterized sqlx already mitigate at the boundary; this just preserves the security
  test through the move). Add `pub mod titles;` to `detection/mod.rs` and the
  `detection/titles/mod.rs` re-export shim (D7).

## 4. Drop `has_title` from the gate; rewire entry to the signaling port

- [x] 4.1 **(red)** `step_detector` test: observation with `browser_windows=[]`,
  `has_meet_connection=true`, `not_preexisting=true` → transitions `Idle → InCall`.
  Currently fails (`has_title=false` blocks entry). This is the load-bearing behavior
  change of D3.
- [x] 4.2 **(red → adversarial)** `browser_windows=[]`, `has_meet_connection=false`,
  `not_preexisting=true` → stays `Idle`. Signaling false must still block — there is no
  title gate to fall back on, so the signaling+bc conjunction is the only discriminator.
- [x] 4.3 **(green)** In `use_cases/meeting_detection.rs`: entry becomes
  `has_conn && not_preexisting` (delete the `has_title &&` term). The observation's
  `has_meet_connection` is now computed in the adapter as `turn || (signaling_active && bc)`,
  where `signaling_active` is the wired `CallSignalingPort` result. §4.1–4.2 pass.
- [x] 4.4 **(D6 check)** the exit/debounce tests pass unchanged.
  `meeting-udp-confidence-debounce` was archived 2026-06-25, so this code is already on the
  post-debounce state — no rebase needed (D6 is a no-op).

## 5. Wire title extraction through the detector adapter (D8)

- [x] 5.1 **(red)** Test: a `WindowsMeetingDetector` constructed with a stub
  `MeetingTitleExtractorPort` (returns `Some("opv-augt-jbm")` for a window titled
  `Google Meet - Meet – opv-augt-jbm`, `None` otherwise) produces an
  `observation.default_title == "opv-augt-jbm"` via `resolve_default_title` calling the
  extractor at each priority step (foreground → recent-focus → first-enum → timestamp). With
  a stub that returns `None`, `default_title` falls through to `Meeting <YYYY-MM-DD HH:MM>`.
  Fails: the adapter does not yet hold an extractor, and `resolve_default_title` still calls
  the Meet regex directly.
- [x] 5.2 **(green, per D8)** `step_detector` AND `spawn_detector` are **unchanged in
  signature** — do not add a trait parameter or field to either. Add
  `title_extractor: Arc<dyn MeetingTitleExtractorPort>` to `WindowsMeetingDetector`; inject
  at the composition root (`lib.rs`) via `new(focus_history, title_extractor)` (see §7.1).
  `resolve_default_title` is a **free function** (windows.rs:55) — it gains an
  `extractor: &dyn MeetingTitleExtractorPort` parameter, and its **two** call sites —
  `current_state` (windows.rs:827, production: passes `&self.title_extractor`) and the
  `resolve_default_title_fallback_is_non_empty` test (windows.rs:996, passes a
  `NoOpTitleExtractor`) — are both updated. Replace the `meet_title_regex()` /
  `strip_google_meet_suffix` calls with `extractor.extract_title(&[w])` — first `Some` wins
  per priority step. **No fabricated fields:** widen `foreground_window_title()` to return a
  full `BrowserWindow { hwnd_id, pid, title }` (HWND from `GetForegroundWindow`, PID from
  `GetWindowThreadProcessId`, title from `GetWindowText`), and have `FocusHistory` store
  `BrowserWindow`s (see §6.2b), so the foreground and recent-focus steps pass **real**
  windows; the first-enum step passes `observation.browser_windows.first()` (already real).
  If `GetWindowThreadProcessId` returns `pid == 0` (elevated foreground window / UAC — the
  same edge case `enum_windows_callback` skips at windows.rs:196),
  `foreground_window_title()` returns `None`; the priority chain falls through to
  recent-focus.
  `current_state()` populates `observation.default_title` via this call (as today,
  windows.rs:827); `step_detector` forwards it into `MeetingDetected.default_title`, one
  line unchanged. §5.1 passes.

## 6. Slim `windows.rs`; verify no drift

- [x] 6.1 After extraction, `detection/windows.rs` contains only:
  `enumerate_browser_windows` (renamed from `enumerate_meet_windows` — now returning **all**
  visible browser-process windows, no title pre-filter), `has_browser_capture_session`,
  `has_turn_connection`, the bc-transition / stable-latch / `notify_exit` logic, and
  observation assembly.
- [x] 6.1a **(domain type)** Define `BrowserWindow { hwnd_id, pid, title }` in `domain/` (or
  `ports/`); rename `DetectorObservation.meet_windows: Vec<MeetWindow>` →
  `browser_windows: Vec<BrowserWindow>` (`MeetWindow`'s "matches the Google Meet title
  pattern" doc comment is retired — it is a semantic lie once the filter moves, per D3).
  Add `DetectorObservation.candidate_titles: Vec<String>` (populated by the adapter via the
  extractor — see §6.2c). Rewrite the `EnumWindows` callback to collect `{hwnd_id, pid, title}`
  for visible browser-process windows without the Meet-title pre-filter (the regex moves into
  `MeetTitleExtractor`). Update every test fixture that constructs `MeetWindow`
  (add `candidate_titles: vec![]`), the `DetectorObservation::Default` impl
  (meeting_detector.rs:60-72), and `FakeMeetingDetector::apply` (fake.rs:57-80, under
  `--features dev-detector`).
- [x] 6.2 **(D9 — rename code-contract consequences, four sites):**
  - **(a) `connection_first_seen_at` gate (windows.rs:805).** Drop the
    `!browser_windows.is_empty()` term (formerly `!meet_windows.is_empty()`). `bc` in
    `has_conn` already prevents Gmail/Drive-at-startup from stamping (no `getUserMedia` →
    `has_conn=false`); the window term is redundant. Update the inline comment to say so.
  - **(b) `spawn_focus_tracker` (windows.rs:836-860).** Drop the `meet_title_regex()` filter
    (it moves). Record the full foreground `BrowserWindow { hwnd_id, pid, title }`
    (`GetForegroundWindow` + `GetWindowThreadProcessId` + `GetWindowText`) whenever the
    foreground process is a browser (`is_browser_process`, vendor-neutral); `FocusHistory`
    stores `BrowserWindow`s. The extractor filters at resolution time, and the recent-focus
    priority step receives real window metadata (D8).
  - **(c) `candidate_titles` + `lib.rs` import.** Populate
    `observation.candidate_titles` in the adapter by iterating `browser_windows` and calling
    `extractor.extract_title(&[w])` per window, collecting `Some` results (per D9 item 3);
    `step_detector` forwards `observation.candidate_titles.clone()` instead of building from
    `meet_windows` (meeting_detection.rs:96-100). **Delete** the `lib.rs:963`
    `crate::detection::windows::strip_google_meet_suffix` map — stripping now happens in the
    adapter via the extractor.
  - **(d) `FakeMeetingDetector` + `Default` impl (D9 item 4 — sites needing the NEW
    `candidate_titles` field, not the full rename churn).** `FakeMeetingDetector::apply`
    (fake.rs:57-80) constructs `DetectorObservation` literally with `meet_windows`/
    `MeetWindow`; rename to `browser_windows`/`BrowserWindow` and add `candidate_titles`. The
    `DetectorObservation::Default` impl (meeting_detector.rs:60-72) needs the new field (the
    "six fields" → "seven fields" delta amendment in the spec mirrors this). **Note:** the
    bare `meet_windows` → `browser_windows` rename also breaks ~20 test fixtures in
    `meeting_detection.rs::tests` (`meet_window()` / `idle_obs()` / `detected_obs()` helpers
    + inline literals) and the `probe_windows()` / `make_obs()` helpers in `windows.rs::tests`
    — §6.1a's "Update every test fixture that constructs `MeetWindow`" is the catch-all for
    those; this item (d) lists only the sites needing the new `candidate_titles` field.
- [x] 6.3 `cargo test --manifest-path frontend/src-tauri/Cargo.toml` — full suite green.
  Re-run any `#[ignore]` real-device detection test if present to confirm the extraction
  didn't break the live adapter.

## 7. Composition root

- [x] 7.1 `lib.rs`: construct `MeetSignalingAdapter` and `MeetTitleExtractor`; wire them
  into the detector and the detection use case. `WindowsMeetingDetector::new(focus_history,
  signaling, title_extractor)` (windows.rs:623) gains both adapter parameters — pass the
  `MeetSignalingAdapter` and `MeetTitleExtractor` `Arc`s here (the sole production call site).
  The `#[cfg(test)] with_probes` constructor auto-injects `NoOpSignaling` +
  `NoOpTitleExtractor` so the existing `with_probes` test call sites are unchanged. The one
  test that calls `new(empty_history())` directly (windows.rs:1626,
  `fresh_detector_after_crash_has_no_inherited_latch`) switches to
  `with_probes(empty_history(), <default probes>)` to inherit the NoOp auto-injection —
  net test churn: 1 line. Confirm the app boots, the detector spawns, and
  `meeting-detected` / `meeting-ended` events still reach the frontend.

## 8. Verification (post-fix, live)

- [x] 8.1 Re-run Meetily with `RUST_LOG=app_lib::detection=debug,app_lib::use_cases=debug`
  in a PWA in-call state; confirm `Idle → InCall` and `meeting-detected` fire (the dash bug
  no longer blocks — title isn't consulted for entry) AND that the recording title is the
  extracted Meet title (EN-dash fix end-to-end).
  **Verified 2026-06-27** (see design.md → 2026-06-27 post-fix live verify): `bc transition
  false→true` + `meeting-detected` + auto-start observed on two real PWA calls (16:35:47Z,
  16:56:02Z); title is not consulted for entry (D3 confirmed); `Some(title)` +
  `Some(detector_started=true)` confirms the camelCase binding (§9.1). **Title-end-to-end
  sub-claim not observable live:** the recording got the fallback `Meeting <YYYY-MM-DD
  HH:MM>` because the window title at the detection instant was the intermediate
  `Google Meet - Meet` (no EN dash, no code) — `bc` transitions before Meet writes the
  code. The EN-dash regex itself is unit-proven (§3.1–3.6); the gap is a bc-vs-title timing
  property, documented as a follow-up (post-detection re-extract), not a v1 defect.
- [ ] 8.2 **Green room — join path:** navigate to a green room, then click *Join now*.
  Confirm `meeting-detected` fires (the universal FP predicted by D4.1 — known limitation)
  and that the recording continues seamlessly into the real call (the FP is a benign
  few-seconds-early start on this path, per D4.1). Record in `design.md` → Verification
  result.
- [ ] 8.3 **Green room — abandon path:** navigate to a green room, then leave **without**
  joining. Confirm `meeting-detected` fires, then whether `meeting-ended` fires within
  seconds (bc-drop exit) **or** runs until manual stop (the F6 risk — Meet holding
  `getUserMedia` after abandonment). Either outcome is data; record which.
- [ ] 8.4 **(eRender-in-green-room — unblocks `meeting-lobby-discrimination`)** During a
  green-room session, sample WASAPI eRender Active vs eCapture Active (the exploration's
  highest-leverage untested signal, F5). If eRender reads Active=0 while eCapture reads
  Active=1 in the green room, render-at-entry is a viable discriminator — record the result
  so `meeting-lobby-discrimination` opens with measured data, not inference.
- [ ] 8.5 **(if feasible)** Gmail tab + a non-Meet browser call; observe whether the
  cross-vendor FP (D4.2) fires in practice. Either way, record the result.

  > **§8.2–8.5 deferred** (2026-06-27): these are empirical FP-discriminator data points
  > for the D4 analysis, not correctness gates for *this* change. The user's live-verify
  > scope was "enter/exit meetings" only; the green-room + eRender + cross-vendor
  > observations feed `meeting-lobby-discrimination` (the deferred FP-discriminator
  > change) and are gathered when that change opens. This change's own contract — detect
  > → camelCase title binds → off-page listener → no orphan fan-out — is fully covered by
  > §8.1 + the D10 smoke regression guard + the cargo/Vitest gates.

- [x] 8.6 Before `/opsx:archive`: re-read `specs/meeting-auto-detect/spec.md` and
  `design.md`; amend the delta or design if verification contradicts the FP analysis
  (gates don't catch spec drift — read the spec, not just the diff). Canonical-merge
  coverage now in the delta: the Auto-start requirement ("Meet windows" → "browser windows
  the extractor matches"), the TURN-latch requirement (`has_meet_connection()` →
  `signaling_active`), and the dev-detector-seam requirement (`meet_windows` →
  `browser_windows` + `candidate_titles`) are all MODIFIED. Remaining minor canonical
  staleness NOT worth a delta edit: the "Detect when an active call ends" body and
  "Conservative app-start state" known-limitation still write `has_meet_connection()` —
  these resolve to the deliberately-retained observation field (TURN-latch MODIFIED note),
  not the extracted free function; and the lobby-page scenario's decorative phrase "title
  still showing Meet - xxx" is behaviorally harmless. Sweep these only if touching those
  requirements for another reason.

## Smoke test

Covered by `e2e/smoke/meeting-auto-detect.spec.ts` (D10 amendment). Although the original
proposal scoped the change to Rust adapters + ports + the state machine (no new Tauri
command, no new event name), the post-fix D10 work touched the **event→UI wiring contract**
in three places that a Playwright spec *can* exercise through the mock event bus:
(i) the camelCase invoke-key fix in `recordingService.ts` (and the `cancel_recording`
camelCase key in `useAutoDetect.ts`) — the auto-start path now actually reaches
`start_recording_with_devices_and_meeting`, asserted via `callLogIncludes`;
(ii) the `useAutoDetect` listener hoist into `RecordingControlProvider` (mounted in
`layout.tsx`) — the listener binds on `/` regardless of which page the user is on; and
(iii) the StrictMode orphan-listener cleanup (cancelled-flag in the listener effect) —
**exactly one** `start_recording_with_devices_and_meeting` invoke per `meeting-detected`
emit, asserted via `callLogCount === 1`. The Rust detection logic itself (TURN latch,
UDP-debounce, stable_capture, gate = `has_conn && not_preexisting`) remains covered by the
cargo adversarial tests in §1–§5; a Playwright spec cannot simulate a real Meet PWA call,
only the event→UI contract downstream of it.

## 9. Frontend wiring (D10 — discovered during §8 live verification)

- [x] 9.1 Fix the Tauri v2 camelCase command-arg convention in `recordingService.ts`:
  `start_recording_with_devices_and_meeting` was being invoked with snake_case JS keys
  (`mic_device_name`, `system_device_name`, `meeting_name`), which Tauri v2 does NOT
  auto-bind → `Option<T>` params silently defaulted to `None`. Switch to camelCase
  (`micDeviceName`, `systemDeviceName`, `meetingName`). Confirmed live: log shows
  `Meeting: Some("Meeting 2026-01-01_00-00"), detector_started: Some(true)`.
- [x] 9.2 `useAutoDetect.ts`: switch the `cancel_recording` invoke to camelCase
  (`meetingId`) for the same reason — snake_case `meeting_id` was silently dropping.
- [x] 9.3 Hoist `useAutoDetect` (+ the four recording hooks) out of the home page and into
  `RecordingControlContext` (mounted in `layout.tsx` non-onboarding branch) so the
  `meeting-detected` listener stays subscribed while the user is on any route (was:
  `/meeting-details` unmounting the home page killed the listener mid-call). Confirmed live:
  detection fired while the user was on `/meeting-details`.
- [x] 9.4 Drop the dead `showModal` call in `useAutoDetect.ts` (no modal host in the global
  scope; was an inert pre-existing artifact surfaced by the hoist).
- [x] 9.5 Fix the StrictMode orphan-listener race in the `useAutoDetect` event effect:
  `setup()` is async (`await listen()`), but the effect cleanup can run before setup
  resolves → `unlistenDetected?.()` is a no-op on `undefined` → orphan listener stays
  subscribed. StrictMode dev mount→unmount→remount accumulates orphans, so one
  `meeting-detected` emit fans out to N starts (observed: 3× at 16:56:02Z). Fix: a
  `cancelled` flag checked at the end of `setup()` that tears down freshly-registered
  listeners if cleanup already ran. Tighten `meeting-auto-detect.spec.ts` to assert
  `callLogCount(page, 'start_recording_with_devices_and_meeting') === 1` per emit.

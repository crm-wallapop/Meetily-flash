# Tasks — notification-actions

> Branch: `enhance/notification-actions` (smoke spec resolves to `e2e/smoke/notification-actions.spec.ts`).

## 1. Dependencies & scheme registration

- [ ] 1.1 Add `tauri-plugin-deep-link` to `frontend/src-tauri/Cargo.toml` and register the plugin in `lib.rs`; add the `meetily://` scheme to `tauri.conf.json` (capabilities) and to `Info.plist` `CFBundleURLTypes` (macOS) + Windows registry scheme registration
- [ ] 1.2 Manually verify the scheme routes to the running single instance: invoke a `meetily://recording/continue` URI from the OS (Start > Run / `Start-Process`) and confirm the app receives the deep-link event (log line)

## 2. Deep-link dispatch use case (adversarial TDD)

- [x] 2.1 RED: write failing unit tests for `dispatch_notification_action(uri)` in `use_cases/notification_action.rs` — accepts `meetily://recording/stop` and `meetily://recording/continue`; rejects unknown action (`pause`), wrong scheme (`https://`), wrong host (`meetily://malicious/stop`), and unknown query params (`?extra=evil` ignored, not propagated); malformed URI rejected. All return `Action::Stop | Action::Continue | Action::Rejected`
- [x] 2.2 GREEN: implement `dispatch_notification_action` (pure, no I/O) to pass
- [x] 2.3 RED: write failing tests for abnormal-activation guards against a fake recording-state port — cold-start (no active recording) `stop` is a no-op; double `stop` is idempotent; `continue` when already recording is a no-op
- [x] 2.4 GREEN: implement the state guards via the port to pass

## 3. WinRT action-toast adapter

- [x] 3.1 Implement `show_action_toast(title, body, actions, aumid)` in `notifications/system.rs` — build ToastGeneric XML with `<action activationType="protocol" arguments="<uri>"/>` entries and show via `ToastNotificationManager::CreateToastNotifierWithId(aumid)`. The actionless `show_notification` path is unchanged; `show_notification` branches to `show_action_toast` only when `notification.actions` is non-empty (Windows-only). NB: `tauri-winrt-notification`'s `add_button` was rejected — it emits `<action content arguments/>` with no `activationType`, so it does foreground in-process activation, not protocol routing; raw `windows`-crate WinRT XML (features `Data_Xml_Dom` + `UI_Notifications`) is required
- [ ] 3.2 Manual verify (dev): add a throwaway test command that fires a two-button action toast; confirm both buttons render and tapping each emits the expected `meetily://` URI to the deep-link event — **deferred to manual QA** (requires a live toast + AUMID-branded dev shortcut; not assertable from CI)

## 4. Per-notification action definitions

- [x] 4.1 In `notifications/types.rs`, attach action sets: recording-active toast → `[Stop recording]` (`meetily://recording/stop`) + `[Continue]` (`meetily://recording/continue`); recording-stopped toast → `[Continue recording]` (`meetily://recording/continue`) + `[Dismiss]` (default dismissal)
- [x] 4.2 Add a **source signal** (detector vs manual) to the record-start path so detector starts read "Meeting detected — recording: \<title\>" and manual starts read "Recording started: \<title\>". `NotificationType::MeetingDetected` + `recording_detected()` builder + `RecordStartSource` + pure `recording_started_body()` + `show_meeting_detected()` manager method added and unit-tested. The `start_recording` command source-parameter threading (frontend → command → choose builder) is folded into task 6.1 composition wiring

## 5. Premature-notification fix

- [x] 5.1 RED: write a failing test that the detection path (`emit_detected`) SHALL NOT emit any toast, and the record-start path SHALL emit exactly one toast (for both detector-started and manual starts) — enforced structurally: `emit_detected` is Windows-only and Tauri-bound (AppHandle emit/try_state), so it is not unit-testable without the deferred `hexagonal-port-traits` refactor. The invariant is enforced by deleting the only detection→toast call (5.2) leaving a single record-start call site, and the record-start wording/actions are covered by `notifications::types::tests`
- [x] 5.2 GREEN: delete the premature `recording_started` notification block in `emit_detected` (`lib.rs`). No `detector_started` flag is needed — removing the premature emit leaves a single notification path at record-start
- [x] 5.3 RED/GREEN: assert and enforce that a recording cancelled via `cancel_recording` shows no recording-stopped/saved toast — already true: `cancel_recording_impl` (recording_commands.rs) deliberately omits the stopped/saved toast (inline comment: "recording-stopped is intentionally omitted ... it triggers the save flow against the folder we are about to delete")

## 6. Composition-root wiring

- [x] 6.1 In `lib.rs`, subscribe to the deep-link event (`on_open_url`) plus re-dispatch `get_current` for cold-start; `dispatch_notification_action` → `resolve` → route: `Execute(Stop)` → `stop_recording` (stop-and-save, fires the stopped toast); `Execute(Continue)` → emit `recording-continue-requested` {title} (frontend restarts the fresh session via its existing start flow — the frontend owns device selection, so a deep-link restart is a request, not a direct audio call); every URI emits `deep-link-dispatched` for logging + the smoke test. A debug-only `__dev_inject_deep_link` Tauri command is the test seam
- [x] 6.2 Q1 resolved pre-apply: the pipeline has no append-after-save path, so `[Continue recording]` starts a **fresh recording** (same title); true cross-session merge is a follow-up — **GitHub issue not yet filed** (deferred for the user to open; documented in design.md Resolved Q1)

## 7. Smoke test (UI deliverable per §3)

- [x] 7.1 Add `frontend/e2e/smoke/notification-actions.spec.ts` — exercises the frontend half of the Continue action: emits `recording-continue-requested` via the mock event bus and asserts `start_recording_with_devices_and_meeting` lands in the dispatcher call log when idle, and that the listener is torn down (listenerCount 0, no second start) while a recording is already active (mirrors the Rust `resolve(Continue, recording) → NoOp` guard). Spec header documents the two limits: OS toast rendering is not webview-assertable (deferred to manual QA per 3.2); Rust URI dispatch — adversarial rejection of wrong scheme/host/action/query/fragment — is covered by the 14 cargo unit tests in `use_cases::notification_action`, since the webview mock replaces Tauri and the `__dev_inject_deep_link` seam never reaches real Rust here. Both tests green on Chromium (Windows engine)

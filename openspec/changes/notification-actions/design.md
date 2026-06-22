## Context

The `notifications` capability spec marks the recording-started (on auto-detect) and recording-stopped toasts as **NOT YET IMPLEMENTED**, with open questions asking whether they should carry action buttons. In the running app, `emit_detected` (lib.rs) currently fires a passive `recording_started` toast at *detection* time — before recording actually starts — and the real record-start path fires a second one ~1s later, so the user sees a duplicate, semantically-wrong notification. A `NotificationAction` model already exists in `notifications/types.rs` (`{ id, title, action_type: Button|Reply }` + `add_action()`) but `SystemNotificationHandler::show_notification` (system.rs) renders only title+body via `tauri-plugin-notification`'s `show()` and drops actions entirely. The AUMID `com.meetily.ai` toast registration is working in this dev environment (DisplayName/IconUri applied this session).

This design covers Windows toasts only (the project's current toast surface). macOS notification actions follow a different activation model and are out of scope for v1.

## Goals / Non-Goals

**Goals:**
- Render action buttons on the recording-active and recording-stopped toasts and route a button tap back into the running app to a real command.
- Collapse the premature detect-time toast and the ~1s-later record-start toast into a single, semantically-correct notification.
- Resolve the two NOT-YET-IMPLEMENTED requirements and the action-button open questions in the `notifications` spec.

**Non-Goals:**
- macOS toast action buttons (different activation model; Windows-only for v1).
- True cross-session **merge** of two separately-saved meetings from the stopped toast's [Continue recording]. v1 resumes/continues capture; merge is a follow-up issue (see Open Questions).
- A `Reply` (text-input) toast action — the `NotificationActionType::Reply` variant stays in the model but is not wired.
- Transcription-completion CTAs — covered by the separate open question in the spec, not this change.

## Decisions

### Decision 1: Render action toasts via raw WinRT toast XML, not `tauri-plugin-notification`'s `show()`

`tauri-plugin-notification 2.3.1`'s desktop builder exposes only `.title().body().show()`; it cannot render `<actions>` or surface a button-tap callback. For action-bearing notifications we build the toast XML (ToastGeneric binding + `<actions>`) and show it via `ToastNotificationManager::CreateToastNotifier(AUMID)` (the same WinRT path the diagnostic test toast used). Actionless notifications keep the existing `show()` path unchanged.

- *Alternative considered*: extend/wrap `tauri-plugin-notification` — rejected; it does not expose a button/callback API on desktop.
- *Alternative considered*: `notify-rust` — rejected; same button/callback limitation, and we already need direct WinRT access to control the AUMID.

### Decision 2: Activate buttons via a `meetily://` protocol scheme (`tauri-plugin-deep-link`)

Toast buttons use `activationType="protocol"` with a `launch` URI like `meetily://recording/stop`. The custom scheme is registered with the OS (Windows registry / macOS `CFBundleURLTypes`). `tauri-plugin-deep-link` only registers the scheme and parses URIs — it does **not** forward a warm re-activation to the running instance on Windows (confirmed by plugin-source inspection and an empirical 1→N instance test on 2026-06-22). `tauri-plugin-single-instance` is therefore required and is registered **first** in the builder: a toast-button re-launch passes the `meetily://` URI as argv, single-instance forwards argv to the already-running instance (and exits the launcher so no second window appears), and the composition root re-dispatches the URI through the same `handle_deep_link` path used on cold start. Cold-start still flows through the deep-link plugin's `get_current()`. A pure `extract_meetily_uri(argv)` helper isolates the argv→URI step so it is unit-testable. A use case maps each whitelisted action URI to an existing Tauri command path.

- *Alternative considered*: COM activator (CLSID on the AUMID shortcut) — the canonical "in-app callback" mechanism, but heavier: requires a registered COM server and the Start-Menu-shortcut-with-AUMID registration (which we deliberately avoided for dev). Deferred.
- *Alternative considered*: click-to-foreground only, no buttons — rejected; the user explicitly wants actionable CTAs.

### Decision 3: Collapse the duplicate — remove the premature detect-time toast; record-start is the single actionable toast

The detector's `emit_detected` fires its `recording_started` notification at **detection time**, before recording begins and before the user can cancel via the in-app banner — so a `[Stop recording]` button there would act on a recording that isn't running yet. That premature toast is **removed** entirely (delete the `show_notification` call from `emit_detected` in `lib.rs`); detection surfaces only the in-app banner, as before.

The **record-start** code path (`start_recording` command → `show_recording_started_notification`) becomes the single actionable toast, for both detector-started and manual starts. No `detector_started` flag or suppression logic is needed — with the premature toast gone there is exactly one notification path.

To distinguish wording, the record-start notification carries a **source signal** (`detector` vs `manual`): detector starts read "Meeting detected — recording: \<title\>", manual starts read "Recording started: \<title\>". Both carry `[Stop recording]` / `[Continue]`.

### Decision 4: Boundary validation of deep-link URIs (security)

Deep-link URIs are attacker-controllable external input (§9). The dispatch use case SHALL accept only `scheme == meetily`, `host == recording`, and `action ∈ {stop, continue}`; any other host/action, unknown query params, or malformed URI is rejected (logged, no command invoked). No untrusted string reaches SQL, the filesystem, or the LLM.

### Decision 5: Hexagonal placement

- **Adapter** (`notifications/system.rs`): builds and shows the WinRT action toast (native `windows`-crate calls; no domain logic).
- **Use case** (new, e.g. `use_cases/notification_action.rs`): pure `dispatch_notification_action(uri) -> Action` mapping + validation; takes a port for the recording side-effects. No WinRT, no Tauri.
- **Composition root** (`lib.rs`): registers the deep-link scheme, subscribes to the deep-link event, and calls the use case, which invokes the existing recording commands (`cancel_recording`, `stop_recording`, resume) via the ports already wired there.
- `NotificationAction`/`NotificationActionType` stay in `notifications/types.rs` (pure data; not moved in this change).

### Decision 6: Action → command mapping (v1)

| Toast button | URI | Effect |
|---|---|---|
| `[Stop recording]` (active) | `meetily://recording/stop` | Stop **and save** the recording (the `stop_recording` path). Not discard. |
| `[Continue]` (active) | `meetily://recording/continue` | Dismiss only; recording continues. No-op. |
| `[Continue recording]` (stopped) | `meetily://recording/continue` | Start a **fresh** recording with the same title — the pipeline has no append-after-save path (see Resolved Q1). True cross-session merge is a follow-up issue. |
| `[Dismiss]` (stopped) | (default dismissal) | Accept the stop; meeting stays saved. |

## Risks / Trade-offs

- **[Action toasts invisible if AUMID branding is missing]** (fresh machine / clean dev build) → buttons render only when the AUMID is registered; document the dev-setup branding step (already applied this session) and rely on the installer for production. No crash either way.
- **[Cold-start activation]**: button tapped while the app is not running launches it with the URI, but there is no recording to act on → dispatch SHALL no-op + log (no error surface).
- **[Double-tap / replay]**: a second `stop` when not recording → idempotent no-op; `continue` when already recording → no-op.
- **[Platform coupling]**: raw WinRT XML couples the adapter to Windows; macOS/Linux action paths are a separate future change. Accepted — the toast surface is Windows-only today.
- **[Deep-link scheme collision]**: another app registering `meetily://` is possible but low-risk; the URI carries only a fixed action verb, no sensitive payload.

## Migration Plan

- Add `tauri-plugin-deep-link` and the scheme registration to Tauri config.
- Add `tauri-plugin-single-instance`, registered first in the builder, forwarding any `meetily://` argv to the running instance via `handle_deep_link`.
- No data migration; no schema change. Rollback = revert the code + remove the scheme registration (orphan registry key is inert).

## Resolved During Investigation (pre-apply)

- **[Continue recording] append vs new session** → **Fresh session.** `recording_manager.rs` `stop_recording()` → `recording_saver.stop_and_save()` finalizes the session (writes file + DB row); `start_recording()` → `start_accumulation()` begins a new accumulation. There is no resume-after-save path (`pause_recording`/`resume_recording` are in-flight state toggles, not post-save). v1 starts a fresh recording with the same title; true cross-session merge is a follow-up GitHub issue.
- **Manual starts** → Manual starts fire `show_recording_started_notification` exactly once at record-start (`lib.rs:121`); they get the same actionable toast with manual-appropriate wording. No duplicate to suppress.
- **Single-instance forwarding** → `tauri-plugin-deep-link` v2.4.9 does not itself route a warm re-activation to the running instance on Windows (source has no mutex/named-pipe/COM-ROT; empirical test 2026-06-22: firing `meetily://` grew the instance count 1→3 and the original logged no dispatch). `tauri-plugin-single-instance` is added to satisfy the spec's "running single instance" requirement; its argv callback re-dispatches through `handle_deep_link`. The deep-link plugin's `on_open_url`/`get_current` remain for cold-start.

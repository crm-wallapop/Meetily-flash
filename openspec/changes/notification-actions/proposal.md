## Why

The `notifications` capability spec already marks two requirements **NOT YET IMPLEMENTED** — the recording-started (on auto-detect) and recording-stopped toasts — and explicitly lists "should it include action buttons?" as an open question. Today the code fires a passive, semantically-wrong "Recording started" toast at *detection* time (before recording actually starts), duplicating the real record-start toast ~1s later, and gives the user no way to act on a toast without switching to the app. A `NotificationAction` data model already exists in `types.rs` but is dead code — actions are never rendered or activated. This change makes the toasts actionable and collapses the duplicate, so an out-of-focus user can stop/continue a recording or undo a false stop straight from the notification.

## What Changes

- Wire up the existing dead-code `NotificationAction` model: render toast `<actions>` via raw WinRT toast XML for action-bearing notifications, bypassing `tauri-plugin-notification`'s title/body-only `show()` path (which cannot render buttons or route taps). Actionless notifications keep the simple `show()` path.
- Register a `meetily://` protocol scheme via `tauri-plugin-deep-link` so toast button taps route back into the running app instance. Each button uses `activationType="protocol"` and a whitelisted action URI.
- **Meeting-detected / recording-active toast**: `[Stop recording]` (`meetily://recording/stop`) / `[Continue]` (`meetily://recording/continue` → dismiss, keep recording). Because auto-detect already starts recording, there is no "start" action.
- **Recording-stopped toast**: `[Continue recording]` (`meetily://recording/continue` → undo a false stop, resume capture) / `[Dismiss]` (accept the stop).
- **Fix the premature/duplicate notification**: the detect-time toast becomes the single, correct notification; the redundant second `recording_started` toast that fires ~1s later at actual record-start is suppressed for detector-started recordings.
- Deep-link URIs are untrusted boundary input: the scheme, host, and action are validated and unknown values are rejected.

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `notifications`: implements the two NOT-YET-IMPLEMENTED requirements (recording-started on auto-detect, recording-stopped on save) with concrete action-button decisions, resolving the spec's open questions on action buttons; adds a requirement for action-button activation via a validated protocol scheme; corrects the premature/duplicate recording-started behavior.

## Impact

- **Rust adapters**: `notifications/system.rs` gains a method to build and show an action-bearing WinRT toast (AUMID `com.meetily.ai`); `notifications/types.rs` defines the per-notification action sets; `lib.rs` registers the deep-link scheme, routes action URIs to existing commands (`cancel_recording` / `stop_recording` / resume), and drops the premature notification call in `emit_detected`.
- **Dependency**: add `tauri-plugin-deep-link`; add the `meetily://` scheme to Tauri config (Windows registry + macOS `Info.plist` `CFBundleURLTypes`).
- **Windows toast registration**: dev builds require the AUMID branding already applied this session (`DisplayName`/`IconUri` under `HKCU\Software\Classes\AppUserModelId\com.meetily.ai`); installed builds obtain it from the installer.
- **Frontend**: minimal — button taps are handled Rust-side via deep-link → existing commands, reusing the same code paths as the in-app banners. The in-app banners (`useAutoDetect.ts`) remain the primary in-app surface; toasts are the out-of-focus surface.
- **Security**: deep-link URIs validated at the boundary (scheme + action allowlist, unknown params ignored) — see `design.md`.

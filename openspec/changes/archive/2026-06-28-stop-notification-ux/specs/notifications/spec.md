## ADDED Requirements

### Requirement: Dev-build AUMID branding is populated at startup

On Windows, when running an uninstalled dev build (`tauri dev`), the app SHALL ensure the AUMID registry key (`HKCU\Software\Classes\AppUserModelId\<identifier>`) has both `DisplayName` and `IconUri` values populated before any system toast is shown. The write SHALL be idempotent (a no-op when both values are already present) and non-fatal (registry failures are logged as warnings and do not block startup).

#### Scenario: First dev-build launch brands the AUMID
- **GIVEN** a dev build launches AND the AUMID registry key exists but lacks `DisplayName` or `IconUri`
- **WHEN** startup completes
- **THEN** `DisplayName` and `IconUri` are written to the AUMID registry key
- **AND** subsequent `recording_started` toasts are displayed rather than silently dropped

#### Scenario: Already-branded AUMID is left untouched
- **GIVEN** a dev build launches AND the AUMID registry key already has both `DisplayName` and `IconUri`
- **WHEN** startup completes
- **THEN** the registry values are NOT rewritten

#### Scenario: Registry write failure does not block startup
- **GIVEN** a dev build launches AND the AUMID registry write fails
- **WHEN** startup completes
- **THEN** a warning is logged
- **AND** the app continues to start and run normally

### Requirement: Protocol-scheme reactivation does not allocate a secondary window

The secondary process that forwards a `meetily://` reactivation URI to the running instance SHALL complete without allocating a visible console window or secondary app window, in both debug and release builds. The single-instance forwarding SHALL produce no user-visible window artifact beyond the existing running instance being brought to the foreground.

#### Scenario: Notification action button does not flash a console in dev
- **GIVEN** the app is running via `tauri dev` AND a `recording_started` toast with an action button is shown
- **WHEN** the user clicks the action button, re-launching the app via `meetily://`
- **THEN** the running instance handles the action
- **AND** no console window is allocated or flashed by the forwarding process

#### Scenario: Release-build reactivation remains windowless
- **GIVEN** an installed release build is running AND a notification action button is clicked
- **WHEN** the `meetily://` secondary process forwards the URI
- **THEN** no console or secondary window appears, as a regression guard

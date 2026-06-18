## ADDED Requirements

### Requirement: Cross-platform smoke test execution
The UI smoke test suite SHALL run on Windows, macOS, and Linux on each developer's host machine, with no platform-specific test branches and no platform skips in the UI smoke layer.

#### Scenario: Same spec set on every OS
- **WHEN** a developer runs the smoke suite on Windows, macOS, and Linux
- **THEN** every spec file under `frontend/e2e/smoke/*.spec.ts` is eligible to run on all three OSes, and no spec is gated behind `test.skip` for any single OS

#### Scenario: No macOS-broken tooling in the primary path
- **WHEN** a UI smoke spec runs on macOS
- **THEN** the test does not invoke `tauri-driver`, `webdriverio`, `selenium-webdriver`, or any other tool that requires a desktop WebDriver client

### Requirement: No cloud or hosted test services
The test infrastructure SHALL NOT depend on hosted test-execution services, SHALL NOT depend on a hosted CI runner to gate merges, SHALL NOT ship DOM, source, or test artifacts off-device, and SHALL run entirely on developer machines.

#### Scenario: Absence of cloud test-tool dependencies
- **WHEN** inspecting `frontend/package.json`
- **THEN** none of the following appear in dependencies: `@meticulous.ai/*`, `@stagehand/*`, `@browserbase/*`, `saucelabs`, `browserstack`, `reflect-run`, `@qawolf/*`

#### Scenario: Absence of a CI smoke gate
- **WHEN** inspecting `.github/workflows/`
- **THEN** no workflow gates merges on the smoke suite; the merge gate is the local pre-push hook, not a hosted runner

#### Scenario: No test-issued outbound HTTP to non-localhost hosts
- **WHEN** any smoke spec or mock-dispatcher code path executes during a test run
- **THEN** no outbound HTTP request is issued to a host other than `localhost` (the dev server and backend); the install-time Playwright browser-binary fetch is the only sanctioned outbound call and occurs before any test runs

### Requirement: Local pre-push gate
A checked-in git hook SHALL run the smoke gate before a push reaches the remote, deriving the scoped smoke spec from the branch name so that each OpenSpec change is verified by its own spec without coupling changes. The hook SHALL self-install on `pnpm install` and SHALL be bypassable for WIP/emergency pushes.

#### Scenario: Hook derives the scoped spec from a feature branch
- **WHEN** a developer pushes from branch `fix/<change>` or `enhance/<change>` and `frontend/e2e/smoke/<change>.spec.ts` exists
- **THEN** the hook runs the Vitest unit suite plus `frontend/e2e/smoke/<change>.spec.ts` before the push proceeds

#### Scenario: Hook tolerates an absent scoped spec
- **WHEN** a developer pushes from a branch whose change name maps to `frontend/e2e/smoke/<change>.spec.ts` but no such file exists
- **THEN** the hook runs only Vitest and permits the push

#### Scenario: Hook runs Vitest-only on non-feature branches
- **WHEN** a developer pushes from `main` or any branch without a `fix/`|`enhance/` prefix
- **THEN** the hook runs only Vitest

#### Scenario: Hook is bypassable
- **WHEN** a developer runs `SKIP_SMOKE=1 git push`
- **THEN** the hook exits without running Vitest or Playwright and the push proceeds

#### Scenario: Hook self-installs on install
- **WHEN** a developer runs `pnpm install` in `frontend/`
- **THEN** the `prepare` script sets `git config core.hooksPath .githooks` (and sets the exec bit on the hook where the filesystem supports it), so the hook is active with no manual setup

### Requirement: UI smoke through Playwright with mocked Tauri APIs
The UI smoke layer SHALL drive the Next.js dev server via Playwright, with the Tauri command surface intercepted at the `@tauri-apps/api/core` module (NOT via `window.__TAURI_INTERNALS__` injection) and returning canned fixture responses.

#### Scenario: Tauri commands intercepted at the module seam
- **WHEN** a smoke test triggers a UI action whose handler calls `invoke('start_recording', ...)`
- **THEN** the call is intercepted by the module-seam mock before reaching the Tauri runtime, and no Rust command implementation is invoked, and `window.__TAURI_INTERNALS__` is never accessed by the mock

#### Scenario: Fixture-backed mock responses
- **WHEN** a smoke test exercises a flow that depends on a transcript or summary
- **THEN** the mock returns a fixture loaded from `frontend/e2e/_fixtures/`, identified by scenario name

### Requirement: Fail-closed mock dispatcher
The mock dispatcher SHALL throw when invoked with a command name it has no registered handler for, so that adding a new Rust command without teaching the mock about it surfaces as an immediate test failure rather than silent coverage loss.

#### Scenario: Unknown command name fails the test
- **WHEN** the dispatcher receives `invoke('some_new_command_not_in_registry', ...)`
- **THEN** it throws an error naming the unregistered command, and the calling spec fails with that error

### Requirement: Engine-per-OS Playwright channel with a discriminating test
The Playwright suite SHALL select the browser channel based on the running OS to approximate the WebView engine Tauri uses on that OS, and SHALL include at least one spec that intentionally exercises a rendering difference between Chromium and WebKit so the strategy is provably effective (not merely configured).

#### Scenario: Windows uses Chromium
- **WHEN** Playwright runs on a Windows host
- **THEN** tests execute against the `chromium` channel (matching WebView2's engine family)

#### Scenario: macOS uses WebKit
- **WHEN** Playwright runs on a macOS host
- **THEN** tests execute against the `webkit` channel (matching WKWebView's engine family)

#### Scenario: Linux uses WebKit
- **WHEN** Playwright runs on a Linux host
- **THEN** tests execute against the `webkit` channel (matching WebKitGTK's engine family)

#### Scenario: Discriminating spec catches an engine difference
- **WHEN** the discriminating spec runs on the `chromium` channel
- **THEN** it asserts a rendering property that differs from the assertion made when the same spec runs on the `webkit` channel, proving the channel selection actually changes what is tested

### Requirement: Per-spec smoke test file convention
Each OpenSpec change that modifies UI-reachable behavior SHALL be accompanied by a smoke spec at `frontend/e2e/smoke/<change-name>.spec.ts`, and the change's `tasks.md` SHALL include adding that file as an explicit task.

#### Scenario: Smoke test referenced in tasks
- **WHEN** a new OpenSpec change is proposed that affects UI behavior
- **THEN** its `tasks.md` contains a task whose deliverable is `frontend/e2e/smoke/<change-name>.spec.ts`

### Requirement: Shared, type-guard-validated fixture corpus
The smoke infrastructure SHALL use a shared fixture directory at `frontend/e2e/_fixtures/`, and every fixture SHALL be validated by a runtime type-guard against the existing TypeScript domain types (`TranscriptSegment`, summary block shape) before being handed to a test. When the `frontend-zod-schemas` follow-up lands, the type-guard SHALL be replaced one-for-one with `Schema.parse`.

#### Scenario: Invalid fixture fails fast
- **WHEN** a fixture file fails the type-guard check (e.g. missing a required `meeting_id` field, or a numeric field containing `NaN`)
- **THEN** the test run fails during fixture load with a validation error naming the failed field, before any UI assertion is attempted

#### Scenario: Prototype-pollution payloads rejected
- **WHEN** a fixture JSON contains a `__proto__` or `constructor` key
- **THEN** the loader strips or rejects those keys before handing the object to a test, and the resulting fixture object is frozen

### Requirement: Determinism — no flaky smoke specs
Smoke specs SHALL be deterministic across runs. They SHALL use Playwright's built-in auto-waiting primitives, SHALL NOT use arbitrary `setTimeout` or `sleep`, SHALL NOT assert on time-of-day, and SHALL NOT depend on machine-local state outside the test sandbox.

#### Scenario: Auto-waiting used instead of arbitrary sleeps
- **WHEN** inspecting any file under `frontend/e2e/smoke/*.spec.ts`
- **THEN** no call to `setTimeout`, `page.waitForTimeout`, or `sleep` appears, and all waits use Playwright auto-waiting primitives (`expect(locator).toBe*`, `page.waitForSelector`, `page.waitForResponse`, etc.)

#### Scenario: ESLint enforces the rule
- **WHEN** the linter runs over `frontend/e2e/smoke/`
- **THEN** any use of banned timing APIs is reported as an error

### Requirement: tauri-driver is not adopted
The infrastructure SHALL NOT depend on `tauri-driver`, `webdriverio`, `selenium-webdriver`, or any WebDriver-based desktop automation tool, in any layer of the test suite.

#### Scenario: WebDriver tooling absent from manifests
- **WHEN** inspecting `frontend/package.json` and `frontend/playwright.config.ts`
- **THEN** none of `tauri-driver`, `webdriverio`, `selenium-webdriver`, or equivalent appear in dependencies or configuration

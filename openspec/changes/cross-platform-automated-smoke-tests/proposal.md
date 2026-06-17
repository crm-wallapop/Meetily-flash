## Why

Manual smoke testing is the bottleneck on every OpenSpec change. Today, the only way to know whether a change to recording, transcription, the speaker panel, or summary rendering actually works end-to-end is for a human to launch the app on each OS, capture audio, and click through the flow. This is slow, error-prone, and not repeatable. We need an automated smoke-test suite that **runs identically on Windows, macOS, and Linux**, enforced by a **local pre-push git hook** — not a hosted CI service. This respects the project's hard constraints: **local-first, zero cloud, zero telemetry**, which rules out hosted/self-healing test services (Meticulous, Stagehand, Browserbase, QA Wolf cloud, etc.) *and* rules out depending on a hosted CI runner to gate merges. The gate runs on each developer's machine before a push reaches the remote.

The macOS constraint is architecture-defining: Tauri's WebDriver support has **no macOS desktop client** (verified against current Tauri 2.11.2 docs at [v2.tauri.app/develop/tests/webdriver](https://v2.tauri.app/develop/tests/webdriver/) — *"On desktop, only Windows and Linux are supported due to macOS not having [a native WebDriver server]"*), so `tauri-driver` cannot be the primary path. This forces the UI smoke layer onto Playwright, which runs cross-platform against the Next.js dev server with the Tauri command surface mocked at the JavaScript seam.

## What Changes

**In scope for this change (Playwright UI smoke v1):**

- **New Playwright smoke suite** (`frontend/e2e/smoke/`): drives the Next.js dev server with a JS-seam mock of `@tauri-apps/api/core` (`invoke` + event listeners) that returns canned fixtures. One file per OpenSpec change (`<change-name>.spec.ts`); each change's `tasks.md` includes "add smoke test" as a task.
- **Engine-per-OS channel strategy**: Playwright runs on `chromium` for Windows (WebView2 is Chromium-based), `webkit` for macOS (Tauri uses WKWebView), and `webkit` for Linux (WebKitGTK) — so each developer tests the rendering engine their OS's users actually see. Cross-OS coverage comes from developers on each OS running the suite locally, not from a hosted matrix runner.
- **Fail-closed mock dispatcher**: the JS-seam mock throws on any command name it doesn't know, so adding a new Rust command without registering it in the mock surfaces as an immediate test failure rather than silent coverage loss.
- **Shared, hand-validated fixture corpus** at `frontend/e2e/_fixtures/`: canned transcripts and summaries validated against the existing TypeScript domain types (`TranscriptSegment`, summary shape) via a runtime type-guard. Production Zod schemas are deferred to a follow-up change (see below).
- **Determinism safeguards**: smoke specs SHALL use Playwright auto-waiting, no arbitrary `setTimeout`, no time-of-day assertions, no reliance on machine state.
- **`tauri-driver` is NOT adopted** (verified: its only unique value — driving the real system WebView — is partially covered by the engine-per-OS Playwright channel, and it has a hard macOS gap).
- **Local pre-push gate, no CI.** A checked-in git hook (`.githooks/pre-push`, self-installed on `pnpm install` via a `prepare` script that sets `core.hooksPath`) derives the scoped smoke spec from the branch name (`fix/<change>` | `enhance/<change>` → `e2e/smoke/<change>.spec.ts`), runs Vitest plus that spec before a push, and tolerates the absence of a scoped spec (Vitest-only). `SKIP_SMOKE=1 git push` bypasses for WIP/emergency pushes. There is no GitHub Actions workflow and no hosted-runner dependency — the project is local-first end to end, including its test gate.
- **No cloud, no hosted services, no off-device DOM shipment.** The one sanctioned outbound network call is Playwright's first-run browser-binary download from Playwright's CDN; documented and cached locally.

**Explicitly deferred to follow-up OpenSpec changes:**

The original draft of this change also included a cargo-depth layer (Tauri command tests via `tauri::test::mock_app`) and Zod-validated fixtures. Both depend on prerequisites that do not yet exist in the codebase and are deferred:

- **`hexagonal-port-traits`** — introduce `TranscriberPort`, `LlmPort`, `AudioCapturePort` traits and a `domain/` module. The current code is feature-organized (`audio/`, `summary/`, `whisper_engine/`), not hexagonal; CLAUDE.md §2 describes the hex layout as a target, not the current state. Only `ports/meeting_detector.rs` exists today.
- **`frontend-zod-schemas`** — introduce Zod schemas mirroring the existing hand-written TS types in `frontend/src/types/` and migrate types to `z.output<typeof Schema>` per CLAUDE.md §6. No zod imports exist in `frontend/src` today.
- **`cargo-integration-test-depth`** — once `hexagonal-port-traits` lands, add cargo integration tests via `tauri::test::mock_app()` with a `test-fakes` Cargo feature. Unblocking this is the primary motivation for the ports refactor.

## Capabilities

### New Capabilities
- `automated-smoke-tests`: Cross-platform (Win/macOS/Linux) Playwright UI smoke infrastructure. Defines the per-spec test-file convention, the engine-per-OS Playwright channel mapping, the fail-closed mock-dispatcher contract, the shared hand-validated fixture format, the determinism safeguards, and the local pre-push gate (no CI). Scope is the UI layer only; Rust-side command-depth testing is deferred to `cargo-integration-test-depth`.

### Modified Capabilities
<!-- None — this is pure additive testing infrastructure. No production capability's requirements change. -->

## Impact

- **Frontend (new)**: `frontend/playwright.config.ts`, `frontend/e2e/smoke/*.spec.ts`, `frontend/e2e/_fixtures/*.json` + a hand-written type-guard validator, and a Playwright init script that intercepts `@tauri-apps/api/core`'s `invoke` (NOT `window.__TAURI_INTERNALS__` injection — Tauri explicitly warns against reaching into internals). DevDep addition: `@playwright/test`.
- **Tauri app**: NO CHANGES. This change deliberately avoids touching Rust so it can land without the port-trait refactor. No `test-fakes` feature, no fake ports, no integration tests in `src-tauri/`.
- **Frontend existing**: `frontend/package.json` gains `test:e2e`, `test:smoke`, and a `prepare` script that self-installs the pre-push hook. Existing `vitest` unit tests (e.g. `meeting-max-speakers.test.ts`) are unchanged.
- **Repo root (new)**: `.githooks/pre-push` (the scoped smoke gate) and `frontend/scripts/install-smoke-hook.mjs` (the self-installer invoked by `prepare`). Zero new dependencies — the installer uses Node, which the frontend already requires.
- **No GitHub Actions / CI changes.** A smoke workflow is intentionally NOT added; the gate is the local pre-push hook. (A `.github/workflows/smoke.yml` drafted earlier in this change was removed when the trigger decision moved to a local hook.)
- **No production code paths change.** No behavioral change to recording, transcription, summary, diarization, or any existing capability.
- **Security**: fixtures are static JSON parsed via a safe loader that strips `__proto__`/`constructor` keys (prototype-pollution defense) and freezes the resulting object; the mock dispatcher never reaches the Rust runtime; no test path issues an outbound HTTP request to a non-localhost host.
- **Local-first principle**: preserved — nothing in this change ships data off-device, calls a hosted API, or introduces a telemetry surface. The browser-binary fetch is install-time only and goes to Playwright's CDN (a documented, sanctioned exception). The test gate runs on the developer's machine, not in a hosted CI environment.

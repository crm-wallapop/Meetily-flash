## Context

Today, every OpenSpec change is verified by a human launching the desktop app on each OS and clicking through the affected flow. There is no automated UI coverage: `frontend/vitest.config.ts` runs unit tests under jsdom only (one such test exists — `frontend/src/__tests__/meeting-max-speakers.test.ts`), and there is no Playwright, no e2e directory, and no Tauri-side integration test suite.

Three constraints shape the design:

1. **Cross-platform coverage without a hosted matrix.** Windows, macOS, and Linux must all be able to run the same UI suite. There is no hosted CI gate (the project is local-first end to end, including its merge gate — see D8); cross-OS coverage comes from developers running the suite on their respective host OSes. Tauri's WebDriver support has no macOS desktop client (verified against current Tauri 2.11.2 docs at [v2.tauri.app/develop/tests/webdriver](https://v2.tauri.app/develop/tests/webdriver/)), eliminating `tauri-driver` as an option regardless of where tests run.
2. **Local-first, zero cloud** — rules out the entire 2026 class of hosted/self-healing test services (Meticulous, Stagehand, Browserbase, QA Wolf cloud, Sauce/BrowserStack grids) *and* rules out depending on a hosted CI runner to gate merges.
3. **Codebase is feature-organized, not hexagonal** — CLAUDE.md §2 describes the hexagonal layout as a *target*. The actual code has `audio/`, `summary/`, `whisper_engine/` modules with concrete implementations and only one port trait file (`ports/meeting_detector.rs`). No `TranscriberPort`/`LlmPort`/`AudioCapturePort` traits exist. The frontend has no Zod — `TranscriptSegment` is a hand-written TS type in `frontend/src/types/index.ts`.

Constraint 3 is decisive for scoping: the cargo-depth layer originally drafted for this change depends on traits that don't exist. Rather than expand this change into a 3-in-1 refactor (hex ports + Zod + tests), this change ships the UI smoke layer only and defers the rest (see proposal's "Deferred to follow-up" section).

Existing tooling in `frontend/package.json`: Vitest 3, jsdom, `fake-indexeddb`, `wait-on`. Tauri runtime is 2.6.2 (CLI 2.11.2); Playwright v1.61 current as of 2026-06-15.

## Goals / Non-Goals

**Goals:**
- Replace most manual UI smoke testing with an automated Playwright suite that runs on Windows, macOS, and Linux, with each developer exercising their host OS's engine locally (no central CI matrix — see D8).
- Make smoke coverage a first-class deliverable of every future UI-affecting OpenSpec change, via a per-spec test-file convention.
- Enforce the suite with a local pre-push git hook, not a hosted CI runner.
- Preserve local-first: no off-device data shipment, no hosted services.
- Land without touching Rust — no port-trait refactor, no `test-fakes` feature — so this change is small and independently mergeable.

**Non-Goals (deferred to follow-up changes):**
- **Cargo command-depth testing** (`tauri::test::mock_app` + fake ports). Blocked on `hexagonal-port-traits`. Tracked as `cargo-integration-test-depth`.
- **Zod-validated fixtures.** Blocked on `frontend-zod-schemas`. Until then, fixtures are validated by hand-written type guards against existing TS types.
- **Pixel-exact rendering parity** with the system WebView. Playwright's bundled `chromium`/`webkit` are proxies, not byte-identical mirrors of WebView2/WKWebView/WebKitGTK. Known gaps are listed in Risks.
- **Backfilling smoke tests** for every past OpenSpec change. Backfill is opportunistic, scoped to high-churn capabilities first.
- **AI/self-healing test tools.** Excluded by the local-first constraint.
- **Property-based tests** of the fixture loader. Deferred with Zod (the natural place for `fast-check` is on a real schema).
- **A hosted CI matrix / nightly corpus.** Explicitly out of scope: the gate is local (D8). Adopting CI is a separate decision for a separate change.

## Decisions

### D1: Playwright over Cypress / WebdriverIO for the UI layer
Playwright is chosen because (a) it ships WebKit, Chromium, and Firefox bundles, enabling the engine-per-OS channel strategy (D3); (b) `codegen`, trace viewer, and auto-waiting reduce maintenance; (c) its module-mocking model lets us intercept `@tauri-apps/api/core`'s `invoke` cleanly, matching the existing `vi.mock('@tauri-apps/api/core', ...)` pattern in `meeting-max-speakers.test.ts`. **Cypress was rejected** primarily because its WebDriver-only model and browser-extension architecture don't fit the init-script interception pattern; its experimental WebKit support (Cypress 13+) is too immature to rely on for macOS WebView parity. **WebdriverIO was rejected** because its main strength (WebDriver protocol) is irrelevant once `tauri-driver` is off the table.

### D2: Single mock site — JS seam only
The UI layer mocks `@tauri-apps/api/core` at the JS seam. This is the only mock site in this change (the original draft's second mock site — Rust port traits — is deferred with the cargo-depth layer). The existing pattern in `meeting-max-speakers.test.ts:3` lifts directly to a Playwright init script. **Important:** the interception point is the `@tauri-apps/api/core` module export, NOT `window.__TAURI_INTERNALS__` — Tauri 2 explicitly warns against reaching into internals, and the internal structure is not a stable public API.

### D3: Engine-per-OS Playwright channel
Each OS runs Playwright against the engine family its Tauri build uses: `chromium` on Windows, `webkit` on macOS, `webkit` on Linux. Considered alternative: run all three engines on all three OSes — rejected as 3× the per-run cost for marginal coverage. Without a hosted matrix, this also means each developer only runs their own OS's engine; cross-OS coverage is the union of what developers on each OS run, not a single matrix run.

### D4: Hand-validated JSON fixtures (Zod deferred)
Fixtures live at `frontend/e2e/_fixtures/<scenario>.json` and are validated by a hand-written type-guard function against the existing TS domain types (`TranscriptSegment`, summary block shape). When `frontend-zod-schemas` lands, the validator is replaced with `Schema.parse` and the hand-written guard is deleted — one-for-one swap. Rejected alternatives: (a) inline per-test fixtures — drift across the suite; (b) block this change on Zod — couples two unrelated refactors and delays the smoke suite.

### D5: Per-spec test-file convention under `frontend/e2e/smoke/`
Each OpenSpec change gets `frontend/e2e/smoke/<change-name>.spec.ts`, and that file's existence is a task in the change's `tasks.md`. The **pre-push git hook** (D8) derives the spec filename deterministically from the branch name (`fix/<change>` / `enhance/<change>` → `e2e/smoke/<change>.spec.ts`) and runs only that file (plus Vitest). If no matching spec exists, the hook runs Vitest only and allows the push (the change is too small to need a smoke spec, or the spec is genuinely pending). Considered alternative: monolithic smoke spec — rejected because it couples all changes.

### D6: Fail-closed mock dispatcher
The dispatcher throws on any command name not in its registry. This converts the most common drift failure — adding a Rust command without teaching the mock about it — from silent coverage loss into an immediate test failure. The cost (writing mock entries for new commands) is paid once per command and is itself a forcing function for keeping the mock honest.

### D7: Determinism safeguards
Smoke specs SHALL NOT use arbitrary `setTimeout`/`sleep`, SHALL NOT assert on time-of-day, SHALL NOT depend on machine-local state (filesystem paths outside the test sandbox, user locale, etc.). Flaky tests are worse than no tests; this rule is enforced by an ESLint rule banning `setTimeout` in spec files.

### D8: Local pre-push hook instead of hosted CI
The smoke gate is a checked-in git hook at `.githooks/pre-push`, self-installed on `pnpm install` via a `prepare` script (`frontend/scripts/install-smoke-hook.mjs`) that sets `git config core.hooksPath .githooks` and sets the hook's exec bit where the filesystem supports it. This decision follows the project's local-first principle applied to the gate itself: just as no meeting data leaves the device, no test gate depends on a hosted runner. The hook derives the scoped spec from the branch name (D5), runs Vitest plus that spec, and is bypassable with `SKIP_SMOKE=1 git push` for WIP/emergency pushes.

Considered alternatives:
- **Hosted CI matrix (GitHub Actions, 3-OS).** Rejected: the project deliberately avoids depending on hosted infrastructure for its guarantees; the user explicitly opted out of running smoke on GitHub Actions. (A `smoke.yml` drafted earlier in this change was removed.)
- **Pre-commit hook (per-commit).** Rejected: the cold first-compile of `/meeting-details` (~37s) plus the full suite makes per-commit invocation too heavy for the WIP-commit loop. Pre-push fires once per push — the right granularity.
- **On-demand only (no hook).** Rejected: no enforcement — the suite can rot or be skipped silently.
- **husky / lefthook.** Rejected as unnecessary: the hook count is one, the frontend `package.json` lives in a subdirectory (which makes husky's root-`.husky/` convention fiddly), and a ~20-line Node installer adds zero dependencies (Node is already required). The trade-off accepted with this choice: cross-OS coverage is no longer centrally enforced — it relies on developers on each OS actually pushing through the hook (D3).

## Adversarial test category applicability (CLAUDE.md §4)

CLAUDE.md §4's categories apply to the use cases under test, not to test infrastructure itself. Mapping them to this change:

| §4 Category | Applicable here? | Where addressed |
|---|---|---|
| Missing required fields (Storage) | Yes — fixture parsing | Task 1.2 |
| Empty sections (LLM/Summary) | Yes — fixture parsing | Task 1.3 |
| NaN / type mutation | Yes — fixture parsing | Task 1.4 |
| Prototype pollution (JSON parse) | Yes — loader | Task 1.5 |
| Prompt injection (Transcription/LLM) | Yes — fixture data rendered as text | Task 2.7 |
| Unknown command (mock dispatcher drift) | Yes — fail-closed contract | Task 2.4 |
| Determinism (no arbitrary timeouts) | Yes — smoke specs | Tasks 3.1, 3.2 |
| Cross-platform engine difference | Yes — discriminating spec | Task 4.2 |
| SQL injection, path traversal, oversized request, concurrent saves | **No** — backend API concerns | Backend tests, not UI smoke |
| Device disconnected, permission denied, sample rate mismatch, oversized recording | **No** — Rust audio pipeline concerns | `cargo-integration-test-depth` follow-up + existing Rust unit tests on the real pipeline |
| Empty transcript, garbled output, non-Latin script | **No** — Whisper engine concerns | Whisper unit tests; non-Latin fixture seeded in 1.7 for future use |
| LLM timeout, malformed response, schema mismatch | **No** — LLM client concerns | `cargo-integration-test-depth` follow-up |

The "No" rows are not gaps in this change — they are scope boundaries. Each belongs to a different layer (backend, Rust pipeline, Whisper, LLM client) and is covered by either existing tests or a named follow-up change.

## Prerequisite Follow-up Changes

| Follow-up | Unblocks | Why deferred |
|---|---|---|
| `hexagonal-port-traits` | `cargo-integration-test-depth` | Ports don't exist; refactoring `audio/`, `summary/`, `whisper_engine/` behind traits is its own substantial change. |
| `frontend-zod-schemas` | Zod-validated fixtures, `fast-check` property tests | No zod imports exist in `frontend/src`; migration is its own substantial change. |
| `cargo-integration-test-depth` | IPC-depth coverage | Blocked on `hexagonal-port-traits`. Adds `tauri::test::mock_app()` suite + `test-fakes` Cargo feature. |

## Risks / Trade-offs

- **No central cross-OS enforcement.** Without a hosted matrix, there is no single run that proves all three OSes pass together; coverage is the union of what developers on each OS push through the hook. Mitigation: the engine-discriminator spec (4.2) proves channel selection discriminates on whichever OS runs it; CLAUDE.md §5 documents that cross-OS coverage relies on developers on each OS.
- **Playwright's bundled WebKit ≠ user's WKWebView** (lags by months; macOS users get evergreen WKWebView via OS updates). Some WebKit rendering bugs will surface differently. Mitigation: engine-per-OS keeps the *family* aligned; high-risk visual changes get manual verification on a real Tauri build.
- **Windows WebView2 (evergreen) ≠ Playwright's bundled Chromium** (different version + Microsoft patches). Same class of risk as above. Mitigation: same — manual verification for high-risk visual changes.
- **Linux WebKitGTK version varies by distro**; Playwright ships one WebKit version. A given Linux developer's coverage of WebKitGTK-actual may differ from another's. Mitigation: document this as a known limitation; rely on macOS WebKit coverage as the primary WebKit signal.
- **macOS smoke has a known hole: the ScreenCaptureKit system-audio path.** Real system-audio capture requires screen-recording permission, which a headless run cannot grant. Mitigation: smoke tests do not cover the system-audio capture path on any OS; that path is covered by Rust unit tests on the real pipeline and manual verification. Spec calls this out instead of claiming "runs identically."
- **JS-seam mock drift from Rust command contracts.** A Playwright test could pass while the real Rust command rejects the same input. Mitigation: the fail-closed dispatcher (D6) catches the *missing-command* drift class; for the *wrong-response-shape* drift class, coverage lands with `cargo-integration-test-depth`. Until then, this is an accepted gap.
- **Fixture maintenance burden** until Zod lands. Mitigation: the hand-written type guard fails fast on shape mismatch; once Zod lands, the guard is replaced one-for-one.
- **Playwright's first-run browser-binary fetch is an outbound call** to Playwright's CDN. Mitigation: documented as the sanctioned exception; cached locally (`~/.cache/ms-playwright` Linux, `~/Library/Caches/ms-playwright` macOS, `%USERPROFILE%\AppData\Local\ms-playwright` Windows).
- **Pre-push hook adds latency to every push** (~Vitest + the scoped spec; the cold 37s meeting-details compile is the floor for the summary spec). Mitigation: `SKIP_SMOKE=1 git push` bypasses for WIP pushes; the hook is scoped to one spec, not the full corpus.
- **No macOS code-signing/notarization surface is introduced** by this change (smoke tests don't build the macOS app — they run Next.js dev server + Playwright). If a future change adds macOS signing for smoke builds, secrets management will need addressing then; explicitly out of scope here.

## Migration Plan

Purely additive — no production code path changes, no schema migration, no behavioral shift. Rollout is incremental:

1. Land the harness: Playwright config, fixture loader with type-guard validation, JS-seam `invoke` mock with fail-closed dispatcher, and the local pre-push hook (`.githooks/pre-push` + the `prepare` self-installer).
2. Add a single seed smoke spec (`recording-basic.spec.ts`) end-to-end as the reference template, plus a cross-platform *discriminating* spec that intentionally renders a WebKit-incompatible CSS feature to prove the engine-per-OS strategy actually catches platform differences.
3. Update CLAUDE.md's "Test Commands" section and the OpenSpec apply workflow to require a smoke test task for UI-affecting changes.
4. Existing specs gain smoke coverage opportunistically when next touched; no big-bang backfill.

Rollback: delete `frontend/e2e/`, `.githooks/`, `frontend/scripts/install-smoke-hook.mjs`, revert the package.json `prepare`/`test:*` scripts. No production state to restore.

## Resolved during apply

- **Discriminating CSS feature.** `-webkit-text-security`, `-webkit-overflow-scrolling`, and `-webkit-marquee-style` all converged across engines (probed on webkit v2311 + chromium v1228). The surviving discriminator is `-webkit-hyphens`: Chromium echoes `''`, WebKit echoes `'auto'`. Verified on both engines locally.
- **macOS subset / nightly split.** Moot: with no hosted CI there is no per-OS minute cost to optimize, so there is no "minimized macOS subset" and no nightly corpus. All three OSes run the full suite locally.

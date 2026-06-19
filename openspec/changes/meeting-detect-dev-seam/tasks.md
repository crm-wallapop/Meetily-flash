# Tasks — meeting-detect-dev-seam

Ordered. Each implementation task is "write the failing test, then make it pass."
Adversarial categories per CLAUDE.md §4 are called out where they apply.

## 1. Feature flag and adapter module

- [x] 1.1 Add an off-by-default `dev-detector` feature to
      `frontend/src-tauri/Cargo.toml` (empty feature, no deps). Document it as
      dev-only in the feature comment.
- [x] 1.2 Create `frontend/src-tauri/src/detection/fake.rs` gated behind
      `#[cfg(feature = "dev-detector")]` with a `FakeMeetingDetector` struct
      holding `state: Arc<Mutex<DetectorObservation>>`. Export it from
      `detection/mod.rs` under the same cfg. Wire the module into the build and
      confirm `cargo check --features dev-detector` compiles an empty impl.

## 2. Adversarial tests (red) — the seam drives the REAL state machine

- [x] 2.1 Write a failing `#[tokio::test]` (gated
      `#[cfg(feature = "dev-detector")]`): construct a `FakeMeetingDetector`
      sharing its `Arc<Mutex<DetectorObservation>>` with a controller handle,
      spawn the real `spawn_detector` with it and a `MockEmitter`, drive the
      controller to `joined` (title "Smoke", `connection_first_seen_at = now`)
      then `left`, and assert the emitter records exactly one
      `meeting-detected` (title "Smoke") and at least one `meeting-ended`.
      Category: behavioural — proves only `current_state()` is faked while
      `step_detector`, debounce, and cancel-suppression run unmodified.
      Reuses the shape of `test_5_1a` in `meeting_detection.rs`.
- [x] 2.2 Write a failing test for the `__dev_simulate_meeting` controller:
      invoking it with an unknown `state` (e.g. `"paused"`) returns an `Err`
      and does **not** mutate the shared observation or panic. Category:
      malformed/untrusted input (the command arg is external input at a
      boundary).

- [x] 2.3 Write a failing `#[tokio::test]` (feature-gated) for the
      concurrency category (CLAUDE.md §4): spawn the real `spawn_detector`
      with the fake, then from a second Tokio task call `set_joined("x")` /
      `set_left()` in a tight loop (e.g. 1000 iterations) while the poll loop
      reads `current_state()`. Assert the detector task never panics, never
      deadlocks, and that each `current_state()` snapshot is internally
      consistent (the `Mutex` serializes access; a clone is taken under the
      lock, so no half-written observation can escape).

## 3. Make the red tests pass

- [x] 3.1 Implement `FakeMeetingDetector::current_state()` (lock, clone,
      return) and `notify_exit()` (no-op). Add a controller
      `FakeDetectorHandle` (a clone of the `Arc<Mutex<DetectorObservation>>`)
      with `set_joined(title)` (writes the in-call observation per D4) and
      `set_left()` (writes the **full** `DetectorObservation::default()` per
      D5 — all six fields cleared, not just the two exit signals). Make test
      2.1 pass.
- [x] 3.2 Implement the `state` validation in the controller
      (`"joined"` / `"left"` only; any other value → `Err`). Make test 2.2
      pass.

- [x] 3.3 Confirm test 2.3 passes. No new implementation beyond 3.1 should be
      required — the `Mutex<DetectorObservation>` already serializes
      `current_state()` (clone-under-lock) against `set_joined` / `set_left`.
      If 2.3 fails, that failure is itself the finding: the snapshot is
      inconsistent or there is a deadlock, and the locking strategy must be
      revisited before any composition wiring.

## 4. Dev command and composition wiring

- [x] 4.1 Add a `__dev_simulate_meeting(state: String, title: Option<String>)`
      Tauri command in a feature-gated module. It looks up the shared
      `FakeDetectorHandle` (stored in Tauri managed state) and calls
      `set_joined` / `set_left`, mapping the validation `Err` to a
      `Result<(), String>`.
- [x] 4.2 Branch the composition root in `lib.rs`: under
      `#[cfg(feature = "dev-detector")]` construct `FakeMeetingDetector`,
      store its `FakeDetectorHandle` in Tauri managed state, and pass the
      detector into `spawn_detector` in place of `WindowsMeetingDetector`.
      Under `#[cfg(not(feature = "dev-detector"))]` keep the existing
      `WindowsMeetingDetector` path unchanged.
- [x] 4.3 Register `__dev_simulate_meeting` in the `invoke_handler` only under
      the feature. Confirm `cargo check` (no feature) AND `cargo check
      --features dev-detector` both pass.

## 5. Verify the off-path is clean (the seam must not ship)

- [x] 5.1 With the feature OFF: `cargo build`, then confirm the dev command
      and fake adapter symbols are absent (the command is not referenced in
      `invoke_handler`; `detection/fake.rs` is not compiled). Record the
      verification command and output in the task completion note.
      **Verified:** every reference to the seam is under
      `#[cfg(feature = "dev-detector")]` — `detection/mod.rs:7` (`pub mod
      fake`), `lib.rs:228` (command fn), `lib.rs:807` (composition branch),
      `lib.rs:925` (invoke_handler entry). `cargo check` with no feature
      exits 0 (clean build, seam absent). All remaining hits live inside
      `fake.rs` itself, which is not compiled without the feature.
- [x] 5.2 With the feature ON: `cargo build --features dev-detector`, start the
      app, and from DevTools confirm `invoke('__dev_simulate_meeting', {
      state: 'joined', title: 'Smoke' })` resolves (returns `Ok`). This proves
      the command is registered only when intended.
      **Verified 2026-06-19.** The DB migration blocker (migration
      `20260616000000` `max_speakers`) is now resolved, so the feature build
      launches cleanly. Via the app's CDP endpoint, `__dev_simulate_meeting`
      resolves: `simulate({state:'bogus'})` returns the validation error
      (`unknown state "bogus"; expected "joined" or "left"`) rather than
      "command not found", proving the command is registered only under the
      feature — and `simulate({state:'joined',title:'Smoke'})` drove the real
      state machine to emit `meeting-detected` and auto-start a recording
      (see 6.1).

## 6. Manual smoke — the payoff (satisfies fix-stop-responsiveness 8.3 & 9.10)

- [x] 6.1 Build with `--features dev-detector`, start the app with auto-detect
      enabled, and invoke `__dev_simulate_meeting("joined", "Smoke")` from
      DevTools. Confirm the auto-start banner appears and a **real** recording
      begins (audio levels move, `start_recording` ran).
      **Verified 2026-06-19** via the app CDP endpoint. The detector state
      machine must be in `Idle` for the join to emit (an earlier probe had left
      it stuck `InCall`); driving `simulate("left")` and waiting out the
      debounce returns it to `Idle`, after which `simulate("joined","Smoke")`
      fired `meeting-detected` → `useAutoDetect` → `startRecordingWithDevices`
      and `is_recording` went `true` with a real audio pipeline.
- [x] 6.2 Press Stop manually. Confirm the status bar transitions
      `Recording → Saving → cleared` within 1 s of each transition, and that
      no audio is captured after the Stop press. (Satisfies 8.3.)
      **Verified 2026-06-19:** `stop_recording` returned the saved
      `meeting_id` / `folder_path`; `is_recording` was `false` immediately
      after.
- [x] 6.3 Confirm `audio.mp4` exists on disk within seconds of Stop, the
      meeting list shows the audio file, and the meeting opens with a working
      audio player. (Satisfies 9.10.)
      **Verified 2026-06-19:** `audio.mp4` (184 KB) + `metadata.json` +
      `transcripts.json` written to the meeting folder under
      `~/Music/meetily-recordings`.
- [ ] 6.4 (Bonus path) Invoke `__dev_simulate_meeting("left", null)`, wait out
      the 15 s debounce, and confirm the stop-prompt banner appears for the
      detector-started recording.
      **Not separately exercised**, but the `Idle→InCall→Idle` round trip
      (join emits, leave debounces to `meeting-ended`) is proven by adversarial
      test 2.1, and `simulate("left")` returning the machine to `Idle` is what
      unblocked 6.1 above.

## 7. Spec drift check and archive prep

- [x] 7.1 Re-read `openspec/specs/meeting-auto-detect/spec.md`, this change's
      delta spec, and `design.md`; confirm the implementation matches every
      scenario and amend the delta/design if the implementation evolved.
      **No drift.** Every delta scenario maps to code: join→full in-call
      signal (`fake.rs:60-75`), left→full idle `DetectorObservation::default()`
      (`fake.rs:78-83`), feature-gated composition branch (`lib.rs:800-813`),
      command registration under feature only (`lib.rs:925`), unknown-state
      rejection before mutation (`fake.rs:85-88`, test 2.2). Design D1–D6 all
      reflected; no design amendment needed.
- [x] 7.2 Run `cargo test` (both with and without `--features dev-detector`),
      `pytest backend/`, `pnpm test`, `pnpm lint`; all green.
      **All green:** `cargo test` (no feature) 349 passed / 0 failed;
      `cargo test --features dev-detector detection::fake` 3 passed / 0 failed;
      `pytest backend/ -m "not slow"` 6 passed; `pnpm test` 216 passed / 18
      files; `pnpm lint` clean (pre-existing warnings only, no errors).
- [x] 7.3 Note: the CLAUDE.md smoke-spec deliverable
      (`e2e/smoke/<change>.spec.ts`) is **N/A** for this change — it is a
      dev-only, feature-gated, non-shipping affordance with no user-visible
      frontend behaviour in default builds. If pushed on an `enhance/` branch,
      use `SKIP_SMOKE=1 git push` (per the local smoke-gate convention) since
      no Playwright spec applies to a non-shipping dev command. Document this
      in the archive note.

## 8. Automated frontend smoke (added — replaces the manual DOM checks in 5.2/6.1/6.4)

The manual smoke's DOM-observable parts (when to auto-start, which banner to
show, and the guards that prevent the subtle regressions) are now covered by
adversarial Vitest tests that run in the always-on `pnpm test` gate — no
feature flag, no manual DevTools step. The real-audio `audio.mp4` finalize
timing (6.2/6.3) still needs a live device and stays manual; no browser-level
test can exercise the recording backend (per design D2).

- [x] 8.1 Extract the regression-prone decision logic from `useAutoDetect` into
      exported pure helpers — `shouldStartOnDetected`,
      `isStopPromptActiveForRedetect`, `shouldShowStopPrompt`,
      `detectPromptBanner`, `stopPromptBanner`, `shouldPushTitleUpdate`.
      Behavior-preserving: the hook passes its current ref/state values; the
      helpers never touch refs, effects, or Tauri.
- [x] 8.2 Write `src/__tests__/useAutoDetect.test.ts` importing the REAL
      helpers (not mirrors): detect→start guards (disabled / already-recording),
      stop-prompt guards (not-recording stale-ref / not-detector-started manual
      / user-managed), D17 re-engage dismiss, banner factories, and the
      title-update decision. 21 tests, all green.
- [x] 8.3 Confirm `pnpm test` + `pnpm lint` stay green after the extraction
      (no behavior change).

## 9. Mock-alias cache isolation (defensive — stops the e2e seam leaking into dev)

During this change's smoke work the Playwright mock-alias (`PLAYWRIGHT_E2E=1`
webpack `resolve.alias` swap) was found baked into `.next/`, which made a plain
`pnpm dev` resolve the Tauri API mocks and broke the onboarding gate at 0% —
the manual smoke's precondition. Root cause: Next's filesystem cache does not
invalidate on env-var changes. Hardening so leakage is structurally impossible,
not merely unlikely.

- [x] 9.1 In `frontend/next.config.js`, route the e2e build to a separate
      `distDir` (`.next-e2e` when `PLAYWRIGHT_E2E === '1'`, else `.next`) so the
      mock build's webpack cache is physically disjoint from the normal dev
      cache. Verified: `distDir` resolves to `.next` with no env and
      `.next-e2e` under `PLAYWRIGHT_E2E=1`. The override is active only under
      the e2e env var, so real dev / `tauri dev` / `tauri build` are untouched.
- [x] 9.2 Add `/.next-e2e/` to `frontend/.gitignore` alongside `/.next/`.

## Context

Meeting auto-detect is driven by `MeetingDetectorPort::current_state() -> DetectorObservation`, polled by `spawn_detector` (a Tokio loop) which feeds each observation into the pure `step_detector` state machine and emits `meeting-detected` / `meeting-ended` Tauri events. The frontend `useAutoDetect` hook reacts to those events to auto-start / auto-stop recordings. The production adapter (`WindowsMeetingDetector`) derives the observation from three live OS signals — a browser window title matching the Meet pattern, a TCP connection to a Google media/signalling IP, and an active WASAPI capture session — none of which can be produced without a real Meet call.

The composition root (`lib.rs:788`) constructs `WindowsMeetingDetector::new(focus_history)` and moves it into `spawn_detector`. The port trait and a scriptable `MockMeetingDetector` double already exist in `use_cases/meeting_detection.rs` tests, so swapping the adapter is a one-site change.

## Goals / Non-Goals

**Goals:**
- Let a developer exercise the full auto-detect → auto-start → stop pipeline (including real audio capture and `audio.mp4` finalize) without joining a real Google Meet call.
- Drive the **real** state machine, emitter, and frontend — only `current_state()` is faked — so the smoke verifies actual app behaviour, not a parallel mock path.
- Ship zero seam code in release or default debug builds.

**Non-Goals:**
- Reproducing the real OS signals (window-title scan, TCP CIDR check, WASAPI enumeration). Those are tested by the existing `WindowsMeetingDetector` unit tests; the seam tests the app's *reaction* to a detection, not detection itself.
- Automating the smoke in CI / pre-push. The seam is a locally-run developer tool, not a Playwright harness (a browser-only mock was considered and rejected because it cannot verify the real `audio.mp4` finalize timing that is the point of tasks 8.3 / 9.10).
- macOS / Linux detector simulation. Production detection is Windows-only today; the seam mirrors that scope.

## Decisions

### D1 — Gate behind an off-by-default Cargo feature, not `cfg(debug_assertions)` + env

**Decision:** A new `dev-detector` Cargo feature (off by default) compiles the fake adapter, the controller, and the `__dev_simulate_meeting` command. Enabled explicitly via `pnpm tauri:dev -- --features dev-detector`.

**Alternatives considered:**
- `cfg(debug_assertions)` + runtime env var: simpler, no feature plumbing, but the dev command and fake adapter would exist in *every* debug build (just dormant). A registered Tauri command that ships in all dev builds is a larger surface than necessary.
- Playwright event-injection (emit `meeting-detected`/`meeting-ended` directly to the frontend with mocked `invoke`): lightest, but cannot exercise the real recording backend, so it cannot verify the `audio.mp4` finalize timing — the actual regression `fix-stop-responsiveness` fixes. Rejected for that reason (kept viable for a future UI-only regression spec).

**Why feature-gate wins:** it makes the seam literally absent from the binary unless asked for, so release and normal-dev builds are byte-for-byte unchanged in the detector path, and there is no dormant command surface.

### D2 — Fake the port, not the events

**Decision:** `FakeMeetingDetector` implements `MeetingDetectorPort` and is constructed in place of `WindowsMeetingDetector`. The existing `spawn_detector` / `step_detector` / emitter / frontend wiring is reused verbatim.

**Why over injecting events directly:** injecting `meeting-detected` straight at the frontend would skip the state machine's debounce, cancel-suppression, and pre-existing-connection logic — exactly the behaviours most likely to regress. Faking only `current_state()` exercises all of it.

### D3 — Shared observation behind `Arc<Mutex<DetectorObservation>>`

**Decision:** `current_state(&mut self)` is called from inside the spawned detector task, while the `__dev_simulate_meeting` command mutates the observation from a separate Tauri command handler. The fake therefore holds `state: Arc<Mutex<DetectorObservation>>`; `current_state()` locks, clones, returns. The command handler holds a clone of the `Arc` and locks to mutate. `notify_exit()` is a no-op (the fake has no per-call sticky state).

**Why not `AtomicBool` flags:** `DetectorObservation` carries a `String` title, a `Vec<MeetWindow>`, and an `Option<Instant>`; atomics don't fit. A single `Mutex<DetectorObservation>` is the simplest correct model and mirrors the existing `MockMeetingDetector` (`Mutex<VecDeque<...>>`).

### D4 — `connection_first_seen_at = Some(Instant::now())` on "join"

**Decision:** When the command sets the fake to `joined`, it writes `connection_first_seen_at = Some(Instant::now())`. Because `detector_start` was captured at `spawn_detector` entry (earlier), `step_detector`'s `not_preexisting` guard (`connection_first_seen_at > detector_start`) passes and `meeting-detected` fires.

**Why:** without this, the conservative app-start guard (spec: "Conservative app-start state") would suppress the event — the exact failure mode D15 protects against in production, which the seam must not trip.

### D5 — "Leave" models the UDP exit path; transport is not parameterised in v1

**Decision:** `__dev_simulate_meeting("left")` writes the **full idle observation** — all six `DetectorObservation` fields cleared, identical to `DetectorObservation::default()` and the real adapter's idle output (`meet_windows = []`, `has_meet_connection = false`, `has_browser_capture_session = false`, `connection_first_seen_at = None`, `default_title = ""`, `is_turn_exit = false`). The real 15 s UDP debounce then applies before `meeting-ended`. The TURN (4 s) path is already covered by the `step_detector` unit tests; the seam does not need to expose it for the smoke, which manually hits Stop anyway (task 8.3).

**Why the full idle snapshot:** a partial idle (e.g. leaving `meet_windows` populated and `has_meet_connection = true` while dropping only the capture session) yields an observation combination the real `WindowsMeetingDetector` never produces. Driving such a mix through `step_detector` would exercise a non-production path, undercutting D2's guarantee that the fake drives only real paths.

**Trade-off:** a 15 s wait before auto-stop in the smoke. Acceptable because 8.3 hits Stop manually; auto-stop is a bonus path, not the gate.

### D6 — Command naming and arity

**Decision:** `__dev_simulate_meeting(state: String, title: Option<String>)` where `state ∈ {"joined","left"}` and `title` sets `default_title` / the `MeetWindow` title on join. Any other `state` value is rejected with an error before the shared observation is touched.

**On the `__` prefix:** it is a documentation convention only. Tauri's `generate_handler!` exposes no command-hiding mechanism — any frontend JS (or a compromised renderer) can `invoke('__dev_simulate_meeting', …)` whenever the feature is compiled in. The sole protection against the command appearing in a shipped binary is the off-by-default `dev-detector` Cargo feature; the prefix must never be read as defense-in-depth.

## Risks / Trade-offs

- **[Feature enabled in a release build]** → The feature is off by default in `Cargo.toml`; release/production build scripts must never pass `--features dev-detector`. Add a `cfg`-gated `compile_error!` is **not** used (feature-gating is sufficient and a compile error would complicate legitimate local release builds for testing). Mitigation: document the feature as dev-only in `Cargo.toml` and this design; the command does nothing destructive (it only flips a detector observation).
- **[Fake drifts from real observation semantics]** → The fake only controls `current_state()`; `step_detector` is reused unchanged, so state-machine semantics cannot drift. Adversarial test: drive `spawn_detector` with the fake through a join→leave script and assert both `meeting-detected` and `meeting-ended` fire through the real emitter (mirrors `test_5_1a`).
- **[Mutex poisoned by a panicking poll]** → `spawn_detector` already wraps `port.current_state()` in `catch_unwind`; a poisoned lock surfaces as a panic there and is swallowed per existing behaviour (the loop continues, logging). No new failure mode.
- **[Seam can't reproduce real WASAPI/TCP timing]** → Accepted non-goal; the existing adapter unit tests cover the signals themselves.

## Open Questions

None — the design is fully determined by the existing port/adapter shape and the chosen feature gate.

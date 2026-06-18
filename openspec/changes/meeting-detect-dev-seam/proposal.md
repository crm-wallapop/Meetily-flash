## Why

Verification of meeting auto-detect — the `meeting-detected` → auto-start → banner → `meeting-ended` → stop flow — currently requires joining a real Google Meet call, because the production detector keys off three live OS signals (browser window title, a TCP connection to a Google media IP, and an active WASAPI capture session). None of those can be produced by opening a local page or by replaying an already-recorded meeting. This blocks the two open smoke tasks in `fix-stop-responsiveness` (8.3, 9.10) and will block every future detection change the same way. A debug-only detector seam lets us inject the "a Meet call is active" signal while the rest of the pipeline (state machine, frontend banner, `start_recording`, real audio capture, `stop_recording`, `audio.mp4` finalize) runs for real — removing the live-Meet dependency without weakening what the smoke actually verifies.

## What Changes

- Add a `FakeMeetingDetector` adapter that implements `MeetingDetectorPort` and returns a shared, mutable `DetectorObservation` controlled at runtime.
- Gate the adapter, its controller, and a dev-only Tauri command behind a new off-by-default Cargo feature `dev-detector`, so the seam **does not exist in the binary** unless explicitly compiled in (`cfg(feature = "dev-detector")`). No `debug_assertions` fallback — feature-gate only.
- Add a `__dev_simulate_meeting` Tauri command (registered only under the feature) that flips the fake detector's observation between `joined` (title + connection + capture session, `connection_first_seen_at = now`) and `left` (idle), driving the real state machine through `meeting-detected` and `meeting-ended`.
- Branch the composition root in `lib.rs` so that, when the feature is enabled, `FakeMeetingDetector` is constructed in place of `WindowsMeetingDetector` and the dev command is registered.

## Capabilities

### New Capabilities

_(none)_

### Modified Capabilities

- `meeting-auto-detect`: adds a requirement for a debug-only detector simulation seam, gated behind an off-by-default Cargo feature, that lets a developer trigger `meeting-detected` / `meeting-ended` without a real Meet call. Production detection behaviour is unchanged when the feature is off.

## Impact

- **`frontend/src-tauri/Cargo.toml`** — new `dev-detector` feature (off by default, no dependencies).
- **`frontend/src-tauri/src/detection/`** — new `FakeMeetingDetector` adapter + controller state (compiled only under the feature).
- **`frontend/src-tauri/src/lib.rs`** — composition-root branch selecting the fake detector under the feature; registration of the `__dev_simulate_meeting` command under the feature.
- **No release-binary change** — the feature is off by default; `cargo build`/release builds are byte-for-byte unaffected in the detector path.
- **No new dependency** — the fake is plain Rust over `Arc<Mutex<>>` / `Arc<Atomic*>`, mirroring the existing `MockMeetingDetector` test double.

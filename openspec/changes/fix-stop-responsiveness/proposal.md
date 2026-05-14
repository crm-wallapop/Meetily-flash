## Why

Pressing Stop on a recording today leaves the UI in a confusing split state for
~2 minutes: the Stop button is disabled (so the click was registered) but the
status bar still shows "Recording" with a spinner (so it looks like capture is
ongoing). In reality, the CPAL streams are released within ~1 s — the lag is
entirely cosmetic, caused by `IS_RECORDING` only flipping `false` after the
full shutdown chain (transcription drain → model unload → file save) completes.

Smoke tests on 2026-05-13 reproduced this twice, each with a **2 m 15 s** gap
between stream release and UI clearing. Users can't tell whether the recording
has actually stopped, and they cannot start a new recording during this window.

## What Changes

- Decouple the frontend "is recording" signal from the backend shutdown chain.
  Stream release becomes the authoritative moment `isRecording` flips `false`.
- Run the post-stop shutdown work (transcription drain, model unload, file
  finalize) in a background task that does NOT block the `stop_recording`
  Tauri command's return.
- Add a distinct "Saving…" state in `RecordingStatusBar` for the background
  shutdown window, visually different from "Recording".
- Make `stop_recording` return as soon as streams are released and the
  background shutdown task is spawned — typically < 1 s.
- Preserve idempotency: a second `stop_recording` call during the background
  window is a no-op (uses the existing `IS_RECORDING` guard, which now flips
  early).

## Capabilities

### New Capabilities
<!-- none -->

### Modified Capabilities
- `recording-lifecycle`: tightens stop-responsiveness requirements to 1 s
  (already drafted in `openspec/specs/recording-lifecycle/spec.md` on
  2026-05-13; this change implements that draft).

## Impact

- **Rust**: `frontend/src-tauri/src/audio/recording_commands.rs` — restructure
  `stop_recording` to release streams synchronously, flip `IS_RECORDING` early,
  emit `recording-stopped` (or a new `recording-stream-released` event), then
  spawn a `tokio::spawn` task for the remaining shutdown work.
- **Rust**: Possibly introduce a `SHUTDOWN_IN_PROGRESS` atomic so we can model
  the "saving in background" state distinctly from "recording" and "idle".
- **Frontend**: `RecordingStateContext.tsx` and `RecordingStatusBar` to render
  the new "Saving…" state.
- **No DB / API impact.** No breaking changes to existing Tauri command
  signatures.
- **Test surface**: Existing recording integration tests need updates to assert
  on the new timing contract. Adversarial cases (Whisper hang, file save
  failure mid-background) must not resurrect the lag.

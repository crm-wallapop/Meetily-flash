## 1. Red tests (adversarial first)

- [x] 1.1 Write a failing test `loudness_normalizer_constructs_with_shortterm_mode`: inspect the `EbuR128` instance after `LoudnessNormalizer::new()` and assert short-term mode is enabled (e.g. via a recorded `Mode` value or by feeding ≥ 3s of audio and asserting `loudness_shortterm()` returns `Ok`, not `Err`)
- [x] 1.2 Write a failing test `loudness_normalizer_does_not_clip_speech_after_quiet_phase`: feed 60s of −50 dBFS noise then 1s of −20 dBFS speech; assert output peak does not exceed 0.891 (−1 dBFS TruePeak limit) AND gain never exceeds 3.98× (+12 dB cap)
- [x] 1.3 Write a failing test `loudness_normalizer_noise_floor_not_lifted`: feed 30s of −40 dBFS ambient noise; assert output RMS stays below −28 LUFS
- [x] 1.4 Write a failing test `loudness_normalizer_gain_cap_enforced`: drive a scenario where `loudness_shortterm()` would return −60 LUFS; assert `gain_linear` is clamped to ≤ 3.98
- [x] 1.5 Write a failing test `loudness_normalizer_noise_gate_skips_measurement`: feed a sub-threshold chunk (RMS < −30 dBFS measured over 100ms window); assert `ebur128.add_frames_f32()` is NOT called but output samples still receive the current gain (use a test double or spy for the `ebur128` call)
- [x] 1.6 Write a failing test `loudness_normalizer_shortterm_error_fallback`: call `normalize_loudness()` with fewer than 3 seconds of audio (so `loudness_shortterm()` returns `Err`); assert gain stays at 1.0× [RE-ENABLE as noise-gate test after 2.4/2.5]
- [x] 1.7 Write a failing test `loudness_normalizer_loud_speaker_attenuates`: feed 5s of −5 dBFS speech (loud); assert `gain_linear < 1.0` (attenuation) AND output stays below the TruePeak limit
- [x] 1.8 Write a failing test `loudness_normalizer_preserves_clean_speech`: feed a 5s sine sweep at −20 dBFS; assert output integrated LUFS lands within ±2 LU of −23 LUFS AND the output FFT magnitude profile matches the input (within tolerance) — proves the gate is not stripping quiet onsets

## 2. Implementation

- [x] 2.1 In `LoudnessNormalizer::new()` (`audio_processing.rs`), change the `EbuR128::new` mode flags from `Mode::I | Mode::TRUE_PEAK` to `Mode::I | Mode::S | Mode::TRUE_PEAK`; run test 1.1 green
- [x] 2.2 In `normalize_loudness()`, replace `self.ebur128.loudness_global()` with `self.ebur128.loudness_shortterm()`
- [x] 2.3 Add `const MAX_GAIN_LINEAR: f32 = 3.981_071_7` (+12 dB) and clamp `self.gain_linear` after computing it
- [x] 2.4 Add `const GATE_RMS_WINDOW_SAMPLES: usize = 4800` (100ms @ 48kHz) and `const GATE_RMS_FLOOR_DEFAULT_DBFS: i32 = -30`; accumulate samples into a `gate_window: Vec<f32>` until full, compute RMS in dBFS, then either feed the full window to `ebur128.add_frames_f32()` (if above floor) or discard it (if below). Clear the window after each decision.
- [x] 2.5 Make `LoudnessNormalizer::new()` accept a `gate_floor_dbfs: i32` parameter; convert to linear and store on the struct
- [x] 2.6 Add `noise_gate_floor_dbfs: i32` (default −30) to the settings store (`database/repositories/setting.rs`) and expose via `get_settings` / `save_settings`
- [x] 2.7 Read `noise_gate_floor_dbfs` in the recording start path (`lib.rs` or `recording_manager.rs`) and pass it to `LoudnessNormalizer::new()`; document inline that mid-recording changes do NOT take effect
- [x] 2.8 Run tests 1.1–1.8 green: `cargo test loudness_normalizer`

## 3. UI and diagnostics

- [x] 3.1 In `LoudnessNormalizer`, accumulate `gain_db = 20·log10(gain_linear)` over the trailing 3 s; once per second, emit a `audio-normalizer-gain` Tauri event carrying `{ gain_db: f32 }` where `gain_db` is the **dB-domain mean** (not `20·log10(mean(linear))`). Emit only during active recording; stop on stop/cancel.
- [x] 3.2 In `RecordingSettings.tsx`, add a **noise gate** row: a slider (range −60 to −20 dBFS, step 1) with an adjacent integer `<input>` field that stays in sync; persist the value via `save_settings` on change. When a recording is active, show a subtle hint below the slider: *"Applies to next recording."*
- [x] 3.3 In `RecordingStatusBar.tsx`, register `listen('audio-normalizer-gain', ...)` inside a `useEffect` whose dependency array gates on `isRecording`; render a read-only `Gain: +X.X dB` label next to the duration (show `Gain: —` before the first event). Capture the returned `UnlistenFn` and call it from the effect's cleanup return to deregister on unmount or when recording stops.
- [x] 3.4 In `RecordingSettings.tsx`, add a read-only **sample rate** label sourced from the `list_audio_devices` response for the selected mic device (e.g. `"48 000 Hz"`). No new Tauri command needed.

## 4. Verification

- [ ] 4.1 Run the full Rust test suite: `cargo test --features vulkan`
- [ ] 4.2 Do a 5-minute recording with ambient noise + normal speech; confirm no clipping artifacts in the MP4 (audible test — listen to the file); confirm `RecordingStatusBar` gain readout is visible and updates live
- [ ] 4.3 Open RecordingSettings; confirm noise gate slider and integer field stay in sync; confirm "Applies to next recording" hint appears only while recording; confirm sample rate label shows correct value
- [x] 4.4 Update `openspec/specs/audio-recording-quality/spec.md` to merge in the ADDED and MODIFIED requirements from this change's delta spec

## ADDED Requirements

### Requirement: Microphone loudness normalization uses a bounded short-term window

The `LoudnessNormalizer` SHALL use EBU R128 short-term loudness (3-second sliding window, `loudness_shortterm()`) rather than integrated loudness (`loudness_global()`) when computing the gain to apply to the microphone channel. The underlying `EbuR128` instance SHALL be constructed with `Mode::S` enabled so that `loudness_shortterm()` can return valid measurements. Gain SHALL be capped at +12 dB (linear: ≈ 3.98×) regardless of measured loudness. A noise gate SHALL evaluate per-chunk RMS over a 100ms accumulating window (≈ 4800 samples at 48 kHz) and skip feeding that window to the LUFS measurement when its RMS is below the configured `noise_gate_floor_dbfs` setting (default −30 dBFS, range −60 to −20 dBFS), preventing sub-threshold noise from biasing the short-term window. The gate floor SHALL be user-configurable in RecordingSettings. The gate floor SHALL be read at recording start and held constant for the duration of that recording.

#### Scenario: Short-term measurement mode is enabled at construction

- **WHEN** `LoudnessNormalizer::new()` constructs its `EbuR128` instance
- **THEN** the `Mode` flags include `Mode::S` (short-term) AND `loudness_shortterm()` returns `Ok` once ≥ 3 seconds of audio have been fed

#### Scenario: Speech after a long quiet phase is not clipped

- **WHEN** the microphone captures 60 seconds of ambient noise at −50 dBFS followed by speech at −20 dBFS peak
- **THEN** the normalizer gain does not exceed +12 dB AND the speech samples in the output do not saturate the TruePeakLimiter

#### Scenario: Ambient noise floor is not lifted to speech level

- **WHEN** the microphone captures ambient room noise at −40 LUFS for any duration
- **THEN** the gain applied to that noise segment does not exceed +12 dB, so the noise output level stays below −28 LUFS

#### Scenario: Gain cap is enforced during initial silence

- **WHEN** the first 3 seconds of a recording contain only sub-threshold noise (RMS < −30 dBFS)
- **THEN** `loudness_shortterm()` returns an error (insufficient data) AND the gain falls back to 1.0× (0 dB)

#### Scenario: Noise gate skips measurement but still applies gain to output samples

- **WHEN** a 100ms RMS window has RMS below the configured `noise_gate_floor_dbfs`
- **THEN** the window is NOT fed to `ebur128.add_frames_f32()` AND the current `gain_linear` IS still applied to the output samples

#### Scenario: Loud speaker is attenuated, not clipped

- **WHEN** the microphone captures sustained speech at −5 dBFS (loud relative to target)
- **THEN** `gain_linear` is below 1.0 (attenuation) AND the output stays below the TruePeak limit

#### Scenario: Clean speech is preserved without spectral damage

- **WHEN** a clean speech-band signal at −20 dBFS is processed for ≥ 5 seconds
- **THEN** the output integrated loudness lands within ±2 LU of the −23 LUFS target AND the output magnitude spectrum matches the input within tolerance (no gate-induced onset stripping)

### Requirement: Average normalizer gain is emitted as a Tauri event during recording

The `LoudnessNormalizer` SHALL emit a `audio-normalizer-gain` Tauri event at 1 Hz during active recording, carrying `{ gain_db: f32 }` where `gain_db` is the **dB-domain mean** of the per-chunk gain values from the trailing 3 seconds (`mean(20·log10(gain_linear))`, not `20·log10(mean(gain_linear))`). Emission SHALL stop on recording stop or cancel.

#### Scenario: Event is emitted at 1 Hz during recording

- **WHEN** a recording is active and ≥ 1 second of audio has been processed
- **THEN** the frontend receives an `audio-normalizer-gain` event with a finite `gain_db` value approximately once per second

#### Scenario: Event emission stops on stop/cancel

- **WHEN** the recording is stopped or cancelled
- **THEN** no further `audio-normalizer-gain` events are emitted

### Requirement: Recording status bar displays current normalizer gain

The `RecordingStatusBar` component SHALL render a read-only `Gain: +X.X dB` label that updates from `audio-normalizer-gain` events while a recording is active. The component SHALL show `Gain: —` before the first event arrives and SHALL deregister its event listener on unmount or when recording stops.

#### Scenario: Gain readout updates during recording

- **WHEN** the user is recording and `audio-normalizer-gain` events are flowing
- **THEN** the label reads `Gain: +X.X dB` and updates at the event cadence (≈ 1 Hz)

#### Scenario: Listener is cleaned up to prevent leaks across sessions

- **WHEN** the recording ends and `RecordingStatusBar` unmounts (or its `isRecording`-gated effect tears down)
- **THEN** the `UnlistenFn` returned from `listen('audio-normalizer-gain', …)` is invoked, removing the listener

### Requirement: Sample rate is displayed in RecordingSettings

`RecordingSettings.tsx` SHALL show a read-only label displaying the capture sample rate (in Hz) of the currently selected microphone device, sourced from the existing `list_audio_devices` Tauri command response.

#### Scenario: Sample rate label reflects the selected device

- **WHEN** the user selects a microphone device in RecordingSettings
- **THEN** the label displays that device's reported sample rate (e.g. `48 000 Hz`)

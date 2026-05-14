## Why

The EBU R128 loudness normalizer in `audio_processing.rs` uses cumulative integrated loudness (`loudness_global()`) measured across the entire recording session. During listening phases where the user is not speaking, ambient room noise (keyboard, HVAC, etc.) dominates the measurement at around −40 LUFS, causing the normalizer to apply up to +17 dB of gain. This lifts the noise floor to −23 LUFS — the same target level as speech — clipping actual speech peaks through the `TruePeakLimiter` and making ambient noise as loud as the user's voice in the saved MP4.

## What Changes

- Replace `loudness_global()` (cumulative session loudness) with `loudness_shortterm()` (3-second sliding window) in `LoudnessNormalizer::normalize_loudness()` so gain tracks the current signal rather than session history
- Cap `gain_linear` at a maximum of +12 dB (~4×) to prevent runaway amplification during extended silence periods regardless of measured loudness
- Add a noise gate: skip feeding samples to the LUFS measurement when per-chunk RMS falls below a configurable floor (default −40 dBFS), preventing quiet-noise-only periods from skewing the gain target

## Capabilities

### New Capabilities
- (none)

### Modified Capabilities
- `audio-recording-quality`: This capability currently has NO documented requirement covering microphone loudness normalization — the existing `loudness_global()`-based behavior was implemented in code but never written to spec. This change ADDS a new requirement (bounded short-term normalization with gain cap and noise gate) that documents what the code SHOULD do. The system-audio-at-70%, RNNoise, and flash-attention requirements are unchanged.

## Impact

- `frontend/src-tauri/src/audio/audio_processing.rs` — `LoudnessNormalizer` struct: new `Mode::S` flag, `loudness_shortterm()`, gain cap, 100ms-window noise gate, gain-event emission
- `frontend/src-tauri/src/database/repositories/setting.rs` — new `noise_gate_floor_dbfs: i32` setting
- `frontend/src-tauri/src/lib.rs` or `recording_manager.rs` — read setting at recording start, pass to `LoudnessNormalizer::new()`
- `frontend/src/components/RecordingSettings.tsx` — noise gate slider + integer input, "Applies to next recording" hint, sample rate label
- `frontend/src/components/RecordingStatusBar.tsx` — `Gain: +X.X dB` readout driven by `audio-normalizer-gain` events
- No changes to `pipeline.rs`, `vad.rs`, or the mixer
- The MP4 written by the recording path will have better SNR: speech peaks will not clip, and noise-floor segments will not be artificially lifted to speech level
- Downstream effects: retranscription quality improves automatically because the source MP4 is cleaner

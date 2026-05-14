## Context

`LoudnessNormalizer` in `audio_processing.rs` wraps the `ebur128` crate and calls `loudness_global()` every 512 samples to derive a linear gain applied to the microphone channel. `loudness_global()` returns EBU R128 integrated loudness — a cumulative, gated average over the entire session since the struct was constructed. The EBU R128 absolute gate (−70 LUFS) and relative gate (−10 LU below ungated mean) only exclude near-silence; audible ambient noise at −40 LUFS passes both gates and skews the running average. During listening phases (user not speaking), the normalizer silently ramps gain toward +17 dB or more, then hard-clips speech peaks via `TruePeakLimiter` (per-sample, no attack/release) when the user starts talking.

**Empirical measurements (2026-05-13):** A noisy Meetily-flash recording was compared against a clean reference from the original Meetily (no EBU R128). The noisy recording showed integrated −21.6 LUFS, true peak +2.5 dBFS, and LRA **3.5 LU** — vs. −19.9 LUFS, +0.4 dBFS, and **9.9 LU** for the clean reference. The 6 LU collapse in dynamic range and the near-identical levels for noise floor (−22 to −25 LUFS) and speech confirm that EBU R128 lifts ambient noise to the same loudness as speech. The user's workflow — hardware mic mute for listening, software mic always unmuted — means the mic channel carries continuous white noise during listening phases, which the normalizer treats as the loudness baseline.

## Goals / Non-Goals

**Goals:**
- Gain tracks the current signal rather than session history
- Speech is not clipped when the user starts talking after a quiet phase
- Ambient noise floor is not lifted to −23 LUFS in the MP4
- Maximum gain is bounded regardless of measured loudness

**Non-Goals:**
- Perceptual noise suppression (RNNoise — covered by `tune-vad-rnnoise`)
- Changes to system audio mixing or the `TruePeakLimiter` implementation
- Any transcription path changes

## Decisions

### D1: Short-term LUFS instead of integrated

Replace `self.ebur128.loudness_global()` with `self.ebur128.loudness_shortterm()` (3-second sliding window, defined by EBU R128). Short-term loudness tracks what is actually audible right now — when you are speaking it reflects your voice; when you are silent it reflects the noise floor without accumulating session-long history.

**Implementation note — construct the normalizer with `Mode::S`.** The current `EbuR128::new(..., Mode::I | Mode::TRUE_PEAK)` does NOT enable short-term measurement; calling `loudness_shortterm()` on that instance returns `Err`. With the existing error-fallback path (`gain_linear = 1.0`), failing to set `Mode::S` would silently disable normalization entirely — a regression dressed as a fix. The construction line MUST become `Mode::I | Mode::S | Mode::TRUE_PEAK`, and a unit test asserts this so a future refactor can't strip it.

**Interaction with the noise gate (D3).** Because the gate skips `add_frames_f32()` on sub-threshold chunks, the EBU R128 "3-second short-term window" measures **3 seconds of fed audio**, not 3 seconds of clock time. After a long gated silence, the next speech burst is normalized against speech samples from before the silence — the gain is effectively frozen during gaps. This is the desired behavior (no gain ramp during listening phases), but readers should not assume strict 3s-of-clock semantics.

Alternatives considered:
- **Momentary LUFS (400ms window)**: Too reactive — gain would pump noticeably between words. Short-term (3s) smooths over pauses without accumulating across minutes.
- **Keep integrated + reset periodically**: Adds state complexity; reset boundaries are arbitrary.
- **Separate gain measurement from recording path**: Architecturally cleaner but over-engineered for this fix.

### D2: Hard gain cap at +12 dB

Add `self.gain_linear = self.gain_linear.min(MAX_GAIN_LINEAR)` where `MAX_GAIN_LINEAR = 10_f32.powf(12.0 / 20.0) ≈ 3.98`. This is a safety rail for edge cases where even the 3-second window is dominated by silence (e.g., very first 3 seconds of a recording before the user speaks).

+12 dB (+4×) is enough to boost a genuinely quiet speaker; beyond that the signal is more likely noise than a whisper.

### D3: Noise gate threshold is user-configurable

The gate floor is stored in settings as `noise_gate_floor_dbfs: i32` (integer dBFS, default −30). It is read at recording start and passed to `LoudnessNormalizer`. Rationale: the correct threshold depends on the user's environment and hardware. A HyperX mic in a quiet room has a different hardware-mute noise floor than a laptop mic in a coffee shop. −30 dBFS is an aggressive starting point suited to hardware-muted white noise; empirical calibration per setup is the right long-term answer — exposing the knob makes that possible without a code change.

The gate excludes sub-threshold chunks from `ebur128.add_frames_f32()` but still applies the current `gain_linear` to output samples — the gate affects measurement only, not output.

**RMS window.** Per-chunk RMS at the existing 512-sample (≈10.7ms) cadence is too noisy: a single keypress transient can momentarily exceed threshold and contaminate the measurement. The gate computes RMS over a **100ms accumulating window (≈4800 samples at 48kHz)**. Sub-threshold accumulation simply discards those samples; above-threshold accumulation feeds the full window into `add_frames_f32()`. This smooths over single-chunk transients without losing reactivity.

**Mid-recording configuration changes.** The gate floor is read from settings at recording start and held constant for the duration of that recording. Mid-recording slider drags do NOT take effect on the active session. The settings UI shows a subtle hint near the slider when a recording is in progress: *"Applies to next recording."* Rationale: live-updating the gate threshold mid-recording risks audible gain pumping at the moment of change, and the diagnostic-grade nature of this setting doesn't warrant the extra wiring.

The settings UI exposes this as a **slider (−60 to −20 dBFS) with an adjacent integer input field** so users can both drag and type an exact value. Located in `RecordingSettings.tsx`.

### D4: No `TruePeakLimiter` changes

The limiter acts as a hard ceiling and is correct as a final safety net. With the gain cap in place, the limiter will rarely trigger for speech. Changing its attack/release would require buffering (lookahead) and is out of scope for this fix.

### D5: Average gain level displayed in recording HUD

During recording, `LoudnessNormalizer` accumulates per-chunk gain samples and emits a `audio-normalizer-gain` Tauri event once per second carrying `{ gain_db: f32 }` — the **mean of `20·log10(gain_linear)` over the last 3 seconds** (i.e., dB-domain average, *not* `20·log10(mean(gain_linear))`). dB-domain averaging matches what a human reading "Gain: +4.2 dB" expects; the linear-mean alternative is dominated by peaks and over-reports.

The readout is rendered in `RecordingStatusBar.tsx` (the slim status bar shown during active recording, next to the duration). Format: `Gain: +X.X dB` while events are arriving; `Gain: —` before the first event. The event is emitted only during active recording and stops on stop/cancel. The frontend listener registered via `listen('audio-normalizer-gain', ...)` MUST capture the returned `UnlistenFn` and call it from the `useEffect` cleanup to avoid leaking across recording sessions.

The 1 Hz throttle matches the short-term window update cadence and avoids flooding the IPC channel.

### D6: Sample rate shown in RecordingSettings

A read-only label in `RecordingSettings.tsx` displays the capture sample rate reported by the active mic device. Sourced from the existing `list_audio_devices` Tauri command response (or a dedicated `get_audio_device_info` call). No new Rust state needed.

## Risks / Trade-offs

- [Risk: Short-term window pumps on rapid speech/silence alternation] → 3-second window smooths over typical inter-word gaps (200–500ms). Pumping is only audible if the speaker alternates speaking/silence at multi-second cadences, which is uncommon in meeting speech. Monitor with `chunk_id % 200` log that is already in place.
- [Risk: Gain cap prevents normalizing a genuinely very quiet speaker] → +12 dB (+4×) handles speakers 12 dB below target; a speaker at −35 LUFS would still be brought to −23 LUFS within the cap. If a speaker is quieter than that, the cap prevents distortion at the cost of a quieter recording — acceptable.
- [Risk: `loudness_shortterm()` returns `Err` for the first 3 seconds] → The `ebur128` crate returns an error if fewer than 3 seconds of audio have been processed. The existing code already falls back to `gain_linear = 1.0` on error; no change needed there.
- [Risk: Noise gate default −30 dBFS too aggressive for very quiet mics] → A very quiet mic's speech floor could sit near −30 dBFS, causing the gate to occasionally exclude soft-speech chunks from measurement. The UI slider lets the user lower the threshold. The settings tooltip documents how to calibrate: record a short silence, check the level in the HUD, then set the gate 5–10 dB above that floor.
- [Risk: Interaction with `tune-vad-rnnoise`] → When `RNNOISE_APPLY_ENABLED = true` (covered by the `tune-vad-rnnoise` proposal), the signal feeding `LoudnessNormalizer` is 10–15 dB quieter than the raw mic input. The −30 dBFS default may then over-gate, excluding even soft speech. Revisit the default (likely lower to ≈ −45 dBFS) as a follow-up task in `tune-vad-rnnoise` after RNNoise is enabled and measured.

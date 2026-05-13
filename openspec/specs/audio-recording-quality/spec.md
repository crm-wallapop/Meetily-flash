# Audio Recording Quality — Capability Spec

> Status: **updated 2026-05-13** — added EBU R128 short-term normalization, noise gate,
> gain cap, and diagnostic event requirements from `fix-audio-normalization`.
> Covers the professional audio mixer, loudness normalizer, and recording pipeline.

---

## Requirement: System audio is mixed at 70 % of microphone level

In the `ProfessionalAudioMixer`, system audio (WASAPI loopback) SHALL be scaled to
0.7 before being summed with the microphone signal. This prevents system audio
(notifications, music, other participants) from drowning out the local microphone.

### Scenario: System audio is present at full scale
- **WHEN** system audio samples arrive at amplitude 1.0 AND microphone is silent
- **THEN** the mixed output amplitude is ≤ 0.7

### Scenario: Mix does not clip
- **WHEN** both mic and system audio arrive at amplitude 1.0 simultaneously
- **THEN** the mixed output is clamped and does not exceed ±1.0
  (the mixer applies soft clip / normalisation before writing to disk)

---

## Requirement: RNNoise suppression is applied to the microphone channel

> **Status: implementation flag `RNNOISE_APPLY_ENABLED` is currently `false` in
> `ffmpeg_mixer.rs`. This requirement documents the intended behaviour when enabled.**

When enabled, the mic channel SHALL pass through RNNoise before being mixed, reducing
steady-state background noise (fan, HVAC) in recordings.

### Scenario: Suppression is enabled
- **WHEN** `RNNOISE_APPLY_ENABLED = true`
- **THEN** the mic channel processed by RNNoise before mixing has lower noise floor
  than the raw mic channel for steady-state background noise

### Scenario: Suppression is disabled (current default)
- **WHEN** `RNNOISE_APPLY_ENABLED = false`
- **THEN** the mic signal is passed through unchanged; no RNNoise processing occurs

---

## Requirement: EBU R128 short-term normalization targets −23 LUFS

The `LoudnessNormalizer` in `audio_processing.rs` uses EBU R128 **short-term mode** (3-second
window) to measure loudness and drive gain toward −23 LUFS. Integrated (cumulative) mode is NOT
used for the normalization gain decision; it is measured only for reference.

### Scenario: Short-term mode is active
- **WHEN** `LoudnessNormalizer::new()` is called
- **THEN** the internal `EbuR128` instance is constructed with `Mode::S | Mode::TRUE_PEAK`
  so that `loudness_shortterm()` returns `Ok` once ≥ 3 s of audio has been fed

### Scenario: Quiet phase does not cause clipping on loud speech
- **GIVEN** 60 s of −50 dBFS noise followed by 1 s of −20 dBFS speech
- **WHEN** the normalizer processes this sequence
- **THEN** output peak does not exceed 0.891 (−1 dBFS true-peak limit) AND gain never exceeds 3.98× (+12 dB cap)

### Scenario: Clean speech output matches target LUFS
- **WHEN** a 5 s sine sweep at −20 dBFS is processed
- **THEN** output integrated LUFS is within ±2 LU of −23 LUFS AND the FFT magnitude profile
  matches the input within tolerance (the gate does not strip quiet onsets)

---

## Requirement: Gain cap of +12 dB prevents excessive amplification

After computing the normalization gain from `loudness_shortterm()`, `gain_linear` is clamped
to ≤ 3.981 (= +12 dB). There is no minimum clamp — attenuation is unlimited.

### Scenario: Gain cap is enforced
- **WHEN** `loudness_shortterm()` returns a value ≤ −60 LUFS (e.g., near-silence)
- **THEN** `gain_linear` is clamped to 3.981_071_7, never exceeds it

---

## Requirement: Noise gate suppresses sub-threshold loudness measurement

A 100ms RMS window (4 800 samples at 48 kHz) is accumulated before each `ebur128.add_frames_f32()`
call. If the window RMS is below `gate_floor_dbfs`, the EBU R128 measurement is skipped for that
window; gain from the previous measured window is applied to the output samples unchanged. The
gate floor defaults to −30 dBFS and is configurable per-recording.

### Scenario: Sub-threshold window skips measurement but still applies gain
- **WHEN** a 100ms chunk whose RMS is below the gate floor is processed
- **THEN** `ebur128.add_frames_f32()` is NOT called AND output samples still receive the current `gain_linear`

### Scenario: Noise floor is not lifted
- **WHEN** 30 s of −40 dBFS ambient noise is processed
- **THEN** output RMS stays below −28 LUFS (noise gate suppresses measurement, gain stays at 1.0×)

### Scenario: Loud speaker is attenuated
- **WHEN** 5 s of −5 dBFS speech is processed
- **THEN** `gain_linear < 1.0` (attenuates) AND output stays below the TruePeak limit

---

## Requirement: Gate floor is configurable and persisted

The user can set `noise_gate_floor_dbfs` (range −60 to −20 dBFS) in Recording Settings. The
value is persisted in `recording_preferences.json` via `set_recording_preferences` / `get_recording_preferences`.
A change takes effect on the **next recording start**; it does not affect a recording already in progress.

---

## Requirement: Normalizer emits real-time gain diagnostics

During an active recording, the `LoudnessNormalizer` emits a Tauri `audio-normalizer-gain` event
once per second carrying `{ gain_db: f32 }`, where `gain_db` is the dB-domain mean of the last
≤ 30 gain observations (3 s at one observation per 100ms gate window). The channel is torn down
on `stop_recording()`; no events are emitted outside of an active recording.

### Scenario: Gain readout updates live in status bar
- **WHEN** a recording is active
- **THEN** `RecordingStatusBar` shows `Gain: +X.X dB` (or `Gain: —` before the first event)
  and the value updates approximately once per second

---

## Requirement: GPU backends use flash attention; CPU and OpenCL do not

> **Status: RESOLVED 2026-05-13** — Vulkan flash attention re-enabled after diagnosis confirmed
> the 2026-05-12 regression was a VAD mis-fire, not a flash_attn kernel issue.
>
> **Root cause diagnosis (2026-05-13):** `test_flash_attn_noise_inputs` ran three audio inputs
> (digital silence, -40 dBFS noise floor, -6 dBFS loud white noise — louder than speech) against
> both flash_attn=false and flash_attn=true on Intel Arc Vulkan. Results:
>
> - All three inputs: **both settings produce identical hallucinated output** (e.g., -6 dBFS →
>   `"(water splashing)"` with flash_attn=false AND flash_attn=true).
> - 2 s JFK speech clip: both settings produce identical coherent output.
> - 11 s JFK full segment: both settings produce the correct transcript.
>
> Hypothesis 1 confirmed: the garbled live transcription was Whisper hallucinating on loud
> environmental noise forwarded by VAD — not a Vulkan-specific flash_attn regression. The VAD
> noise threshold needs a separate fix to avoid forwarding near-full-scale noise chunks.

Flash attention is enabled for Metal, CUDA, and Vulkan. It is disabled for CPU and OpenCL,
which lack the fp16 shader infrastructure required.

### Scenario: Metal, CUDA, and Vulkan use flash attention
- **WHEN** the backend is Metal, CUDA, or Vulkan
- **THEN** `WhisperContextParameters { flash_attn: true }` is used

### Scenario: CPU and OpenCL do not use flash attention
- **WHEN** no GPU is detected or the backend is OpenCL
- **THEN** `WhisperContextParameters { flash_attn: false }` is used

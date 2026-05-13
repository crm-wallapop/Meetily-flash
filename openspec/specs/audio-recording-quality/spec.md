# Audio Recording Quality — Capability Spec

> Status: **initial** — documents the current mixing invariants.
> Covers the professional audio mixer in the recording pipeline.

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

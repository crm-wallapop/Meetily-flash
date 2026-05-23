## MODIFIED Requirements

### Requirement: RNNoise suppression is applied to the microphone channel

RNNoise SHALL be enabled by default (`RNNOISE_APPLY_ENABLED = true`). The mic channel SHALL pass through RNNoise before EBU R128 normalization, reducing steady-state background noise (fan, HVAC, keyboard) in recordings and improving the signal quality seen by the normalizer.

#### Scenario: Suppression is enabled (new default)

- **WHEN** `RNNOISE_APPLY_ENABLED = true` (default)
- **THEN** the mic channel is processed by RNNoise before being passed to `LoudnessNormalizer` AND the output noise floor for steady-state noise (HVAC, fan) is measurably lower than the raw mic signal

#### Scenario: Suppression disabled path still compiles and can be toggled

- **WHEN** `RNNOISE_APPLY_ENABLED = false`
- **THEN** the mic signal bypasses RNNoise and is passed directly to `LoudnessNormalizer` unchanged

---

## ADDED Requirements

### Requirement: Silero VAD positive threshold is calibrated to reject ambient noise

> **Forward reference:** `ContinuousVadProcessor` (the live-transcription VAD path that originally
> received this calibration) is being removed by `post-meeting-transcription` task 1.2 — that task is
> still in progress as of 2026-05-18. Once removed, this requirement applies exclusively to the VAD
> path in `retranscription.rs` (post-meeting batch retranscription).

The `positive_speech_threshold` in the Silero VAD configuration used by `retranscription.rs` SHALL be set to a value empirically determined to not fire on recorded ambient noise at the expected post-normalization level. The target range is 0.60–0.65. The exact value SHALL be determined by running the Silero model against a real captured noise-only segment from a Meetily recording.

#### Scenario: VAD does not trigger on ambient noise only

- **WHEN** the VAD processes a 5-second segment containing only keyboard and HVAC noise (no speech)
- **THEN** `positive_speech_threshold` is set such that the VAD does NOT produce a positive speech detection for that segment

#### Scenario: VAD still triggers on soft speech

- **WHEN** the VAD processes a segment containing soft but intelligible speech at −30 dBFS
- **THEN** the VAD produces a positive speech detection within the redemption window

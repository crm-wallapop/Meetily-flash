## Context

After `fix-audio-normalization` and `post-meeting-transcription` are applied, two residual noise paths remain: (1) The MP4 may still contain broadband steady-state noise (HVAC, fan) that the short-term LUFS normalizer no longer amplifies but also does not suppress. (2) The retranscription VAD (`get_speech_chunks_with_progress` in `retranscription.rs`) uses the same Silero model with the same thresholds as the live path; if noise in the MP4 scores above 0.50 it still reaches Whisper.

RNNoise is already compiled and linked — `RNNOISE_APPLY_ENABLED = false` is a single constant in `ffmpeg_mixer.rs`. Silero threshold constants live in `vad.rs`. Both can be tuned without structural changes.

This change is intentionally held until post-proposal-2 recordings can be evaluated empirically.

## Goals / Non-Goals

**Goals:**
- RNNoise runs on the mic channel in the recording path, reducing steady-state noise in the MP4
- Silero `positive_speech_threshold` is raised to a calibrated value that does not fire on ambient noise at the expected post-normalization level
- A regression test asserts the new threshold does not forward a noise-only chunk

**Non-Goals:**
- DNN-based noise suppression at inference time (that is RNNoise's job, already in binary)
- Changes to VAD redemption time or min speech duration
- Any UI changes

## Decisions

### D1: Enable RNNoise before EBU R128

The processing order in `pipeline.rs` is: HPF → RNNoise (disabled) → EBU R128. Enabling RNNoise here means the normalizer sees a cleaner signal, which also improves its LUFS measurement (speech-only loudness, not noise-inclusive). This is the designed intent per the `audio-recording-quality` spec.

### D2: Threshold target 0.60–0.65, determined empirically

The correct threshold depends on the actual noise floor in recordings produced after proposal 1. The implementation task includes running the Silero model against a captured noise-only segment from a real recording and finding the highest threshold at which it does not fire. 0.60 is the starting point; 0.65 is the upper bound before the risk of missing soft speech becomes unacceptable.

### D3: Unit test uses a recorded noise sample, not synthetic white noise

Synthetic white noise does not match the spectral profile of keyboard/HVAC noise. The test fixture should be a short (2–5s) WAV captured from a real Meetily recording that contains only ambient noise. This makes the threshold calibration meaningful.

## Risks / Trade-offs

- [Risk: RNNoise introduces latency or CPU overhead] → RNNoise processes 10ms frames at 48kHz; overhead is negligible (<1% CPU on modern hardware). Already verified by the existing conditional path.
- [Risk: RNNoise over-suppresses soft speech] → RNNoise is a noise suppressor, not a gate; it attenuates noise-frequency components proportionally. Soft speech is preserved but may be slightly reduced. Monitor by listening to recordings from a quiet speaker with it enabled.
- [Risk: Threshold 0.65 misses soft or accented speech] → The retranscription VAD uses a 2-second redemption window — a single below-threshold frame does not drop a speech chunk. Risk is low for speech that starts above 0.65 even if it occasionally dips.
- [Risk: This change may be unnecessary] → By design. If proposals 1 and 2 resolve the transcription quality issues, this change stays unimplemented. Evaluate before applying.

## 1. Calibrate Silero threshold

- [ ] 1.1 Capture a 5-second WAV snippet from a real Meetily recording containing only ambient noise (keyboard, HVAC — no speech); save as `frontend/src-tauri/tests/fixtures/ambient_noise_only.wav`
- [ ] 1.2 Write a test `vad_does_not_fire_on_ambient_noise`: load the fixture, run it through `ContinuousVadProcessor` at the current threshold (0.50); record the result (expected to fail — confirms the problem exists before the fix)
- [ ] 1.3 Increment `positive_speech_threshold` in steps (0.55, 0.60, 0.65) and re-run 1.2 at each value; record the lowest threshold at which the VAD does not fire on the fixture
- [ ] 1.4 Write a second test `vad_fires_on_soft_speech`: use a short soft-speech fixture (can be synthesised or sampled); assert the VAD fires at the chosen threshold
- [ ] 1.5 Set `positive_speech_threshold` to the calibrated value in `vad.rs`; run both tests green

## 2. Enable RNNoise

- [ ] 2.1 Set `RNNOISE_APPLY_ENABLED = true` in `ffmpeg_mixer.rs`
- [ ] 2.2 Run the existing RNNoise-related tests to confirm no regression: `cargo test rnnoise`
- [ ] 2.3 Do a 3-minute recording with keyboard and HVAC noise; confirm the MP4 has lower noise floor than before (audible comparison)

## 3. Verification

- [ ] 3.1 Run full test suite: `cargo test --features vulkan`
- [ ] 3.2 Trigger post-meeting retranscription on a recording with ambient noise; confirm no hallucinated segments from noise-only chunks
- [ ] 3.3 Update `openspec/specs/audio-recording-quality/spec.md` to merge in the MODIFIED and ADDED requirements from this change's delta spec

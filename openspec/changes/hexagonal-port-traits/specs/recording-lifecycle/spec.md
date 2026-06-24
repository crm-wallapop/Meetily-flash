## ADDED Requirements

### Requirement: Stop path consumes a swappable capture port

The `stop_recording` use case SHALL depend on an `AudioCapturePort` trait
(`ports/audio_capture.rs`), not a concrete cpal adapter. The lifecycle seam —
releasing the capture streams and force-flushing the incremental saver — SHALL
be invoked through the port so that a fake capture can be injected in cargo
tests. The concrete `RecordingManager` (cpal adapter) and the test fake SHALL
both implement the trait.

This requirement exists so that the "status bar clears within 1 s" and "audio
capture halts within 1 s" behavioral requirements of this capability are
verifiable without a live microphone: the phase-machine timing and the
no-chunks-after-stop invariant SHALL be assertable against a fake port that
returns instantly and counts delivered chunks.

#### Scenario: Stop invokes the port, not a concrete adapter

- **WHEN** the `stop_recording` use case runs with an `AudioCapturePort` fake
  whose `stop_streams_and_flush` records that it was called
- **THEN** the fake's `stop_streams_and_flush` is invoked exactly once
- **AND** the phase transitions to `Saving` after the port call returns
- **AND** the use case does not reference `RecordingManager` (the concrete cpal
  adapter) directly

#### Scenario: Phase flips fast regardless of adapter speed

- **WHEN** an `AudioCapturePort` fake whose `stop_streams_and_flush` returns
  instantly is injected
- **AND** `stop_recording` is called
- **THEN** the use case returns and the phase is `Saving` within 1 second
- **AND** the timing assertion does not depend on any real cpal stream teardown
  (the fake performs no device I/O)

#### Scenario: No audio chunks are delivered after stop

- **WHEN** an `AudioCapturePort` fake that counts chunks delivered through its
  receiver is injected
- **AND** `stop_recording` is called and `stop_streams_and_flush` returns
- **THEN** no further chunks appear on the receiver after the stop call returns
- **AND** the chunk count observed after stop is zero over a post-stop sampling
  window

#### Scenario: Real cpal adapter satisfies the same contract

- **WHEN** `RecordingManager` (the concrete cpal adapter) is wired as the
  `AudioCapturePort` in the composition root
- **THEN** the production stop path behaves exactly as before this change: the
  1 s stream-release bound, the `Saving` phase transition, the synchronous
  `recording-stopped` emit carrying `folder_path`, and idempotent re-stop
  during `Saving` all hold unchanged

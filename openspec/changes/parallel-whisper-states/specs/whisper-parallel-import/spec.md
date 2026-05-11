## ADDED Requirements

### Requirement: Vulkan builds process at most 2 whisper segments concurrently
During file import, when the active GPU backend is Vulkan, the system SHALL process
at most 2 VAD segments concurrently by sharing one loaded `WhisperContext` across two
concurrent `WhisperState` tasks.

#### Scenario: Vulkan concurrency is 2
- **WHEN** `whisper_concurrency` is called with `GpuType::Vulkan`
- **THEN** it SHALL return `2`

#### Scenario: Non-Vulkan backends remain sequential
- **WHEN** `whisper_concurrency` is called with any GPU type other than `GpuType::Vulkan`
- **THEN** it SHALL return `1`

### Requirement: Transcript output order is preserved under parallel processing
The system SHALL produce transcript segments in the same order as the input VAD segments
regardless of which segment finishes transcription first.

#### Scenario: Out-of-order completion yields ordered output
- **WHEN** segment N+1 completes transcription before segment N
- **THEN** segment N SHALL appear before segment N+1 in the final transcript

### Requirement: Import cancellation is honoured inside concurrent futures
The system SHALL check `IMPORT_CANCELLED` inside each segment future before invoking
the whisper engine and return `Err("Import cancelled")` immediately if the flag is set.

#### Scenario: Cancellation stops pending segments
- **WHEN** `IMPORT_CANCELLED` is set to `true` while segments are queued
- **THEN** futures that have not yet started transcription SHALL return an error without calling the engine

### Requirement: Short segments are skipped without transcription
The system SHALL skip any segment with fewer than 1600 samples (100 ms at 16 kHz) and
treat it as producing no transcript text, regardless of concurrency mode.

#### Scenario: Sub-100ms segment produces no output
- **WHEN** a segment has fewer than 1600 samples
- **THEN** the system SHALL return `None` for that segment slot without invoking the engine

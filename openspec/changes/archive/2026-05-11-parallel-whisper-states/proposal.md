## Why

File import transcription processes VAD segments sequentially, leaving the CPU idle during GPU inference and the GPU idle during mel-spectrogram computation. On a 28-minute Spanish meeting (33 segments) with large-v3-turbo-q5_0 on Intel Arc iGPU, this yields 1.47x realtime — roughly 19 minutes of transcription time for 28 minutes of audio. Overlapping mel computation for segment N+1 with GPU inference for segment N is expected to reduce total transcription time by 20–30%.

## What Changes

- Replace the sequential `for` loop over VAD segments in `audio/import.rs` with `futures::stream::iter(...).buffer_unordered(2)`, allowing at most 2 segments to be in-flight concurrently.
- Each segment spawns a `tokio::spawn_blocking` task that: (1) creates a fresh `WhisperState` from the shared `Arc<WhisperContext>`, (2) computes the mel spectrogram, and (3) runs GPU inference.
- Output ordering is preserved by collecting `(index, result)` pairs and sorting before writing to the transcript.

## Capabilities

### New Capabilities

- `whisper-parallel-import`: Parallel segment transcription during file import — at most 2 concurrent whisper states sharing one loaded context, with ordered output.

### Modified Capabilities

_(none — no existing spec requirements change)_

## Impact

- **`frontend/src-tauri/src/audio/import.rs`**: The segment transcription loop (approximately lines 551–610) is replaced.
- **`frontend/src-tauri/Cargo.toml`**: `futures` and `futures-util` are already declared; no new dependency.
- **`frontend/src-tauri/tests/pipeline_perf.rs`**: Existing perf test covers sequential baseline; a parallel variant will verify the speedup.
- **Memory**: Two concurrent `WhisperState` objects on top of the shared `WhisperContext`. On large-v3-turbo-q5_0, each state is ~100 MB additional VRAM/RAM; acceptable on systems with ≥8 GB.
- **No behavioral change**: Same transcript output, same language detection, same confidence scores — only wall-clock time changes.

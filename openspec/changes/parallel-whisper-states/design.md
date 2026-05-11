## Context

File import in `audio/import.rs` previously ran a sequential `for` loop over VAD segments, calling `transcribe_audio_with_confidence` one at a time. This kept the GPU idle during mel-spectrogram computation (CPU-bound) and the CPU idle during attention inference (GPU-bound).

Confirmed baseline (debug build, Vulkan + flash attn, large-v3-turbo-q5_0, 28-min Spanish meeting):
- 33 VAD segments, 1,154,582 ms transcription (≈19 min), 1.47× realtime.
- Large segments dominate: seg 1 (259 s audio → 160 s), seg 15 (188 s → 128 s).

`WhisperEngine` holds an `Arc<WhisperContext>` internally. `WhisperContext::create_state()` is `&self` — multiple tasks may call it concurrently on a shared context. The ggml Vulkan backend serialises GPU queue submissions internally, making two concurrent `WhisperState` objects safe on Vulkan. No analogous guarantee exists in the current whisper.cpp for Metal, CUDA, OpenCL, or CPU backends.

## Goals / Non-Goals

**Goals:**
- Overlap CPU mel computation for segment N+1 with GPU attention inference for segment N on Vulkan builds.
- Preserve output ordering: transcript segments must arrive in the same order regardless of completion order.
- Preserve cancellation semantics: `IMPORT_CANCELLED` flag checked inside each future.
- Zero change for non-Vulkan backends.

**Non-Goals:**
- Concurrency > 2 (diminishing returns; VRAM pressure from extra states).
- Parallelism for Metal, CUDA, or CPU-only paths (no documented multi-state thread-safety).
- Changes to live recording transcription (different code path).
- Changes to Parakeet transcription path (remains sequential by design).

## Decisions

### 1. `futures::stream::iter(...).buffer_unordered(N)` over `tokio::task::spawn`

`buffer_unordered(N)` keeps the producer-consumer relationship simple: the stream drives scheduling, ordering is handled by collecting `(index, result)` pairs into a pre-allocated `Vec<Option<_>>`, and the concurrency bound is trivially configured. `spawn` would require a `JoinSet` or explicit channel with the same complexity but more boilerplate.

Alternatives considered: `rayon` (blocking, incompatible with async whisper engine), `tokio::task::JoinSet` (more code for same outcome).

### 2. Concurrency gate: `whisper_concurrency(gpu_type) -> usize`

A single function from `GpuType` to `usize` makes the policy testable in isolation without touching the stream logic. Vulkan → 2; everything else → 1 (sequential, same as old for loop). This lets future backends opt in by changing one match arm.

### 3. Pre-extract owned segment data before the async closure

`processable_segments` is a `Vec<SpeechChunk>` holding `&`-referenced data. Async closures passed to `stream::iter().map()` require `'static` or `move` semantics. Collecting `(i, samples.clone(), start_ms, end_ms)` tuples up front avoids lifetime errors without unsafe code.

### 4. Ordered output via index-keyed result slots

Segment results land in a `Vec<Option<_>>` at position `i`. After the stream drains, a single `for entry in segment_results` loop reconstructs the ordered transcript. This is O(n) and allocation-free beyond the initial vec.

## Risks / Trade-offs

| Risk | Mitigation |
|---|---|
| Second `WhisperState` per concurrent task adds ~100 MB VRAM/RAM | Concurrency capped at 2; acceptable on ≥8 GB systems targeted by large-v3-turbo |
| ggml Vulkan serialisation claim is empirical, not formally documented | Unit test `test_whisper_concurrency_gate` asserts Vulkan=2, all others=1; easy to revert if instability is observed |
| Ordering bugs if index logic is wrong | Integration test `test_cancellation_inside_closure_path` + perf test exercise the full stream |
| CUDA Windows detection was broken (was returning `None`) | Fixed in `ebee1d2`: CUDA detection now walks `PATH` for `nvcc` on Windows |

## Migration Plan

No user-facing migration needed. The change is internal to `import.rs`. All existing callers of `import_audio_file` receive the same `Result<MeetingId>` interface. The concurrency is transparent to the Tauri frontend.

Rollback: revert to sequential by changing `whisper_concurrency` to always return `1`.

## Open Questions

_(none — implementation is complete)_

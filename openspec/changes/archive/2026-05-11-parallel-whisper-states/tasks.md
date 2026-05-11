## 1. Concurrency gate

- [x] 1.1 Add `whisper_concurrency(gpu_type: &GpuType) -> usize` function in `import.rs` returning 2 for Vulkan, 1 for all other backends
- [x] 1.2 Write `test_whisper_concurrency_gate` unit test asserting Vulkan=2, Metal/CUDA/OpenCL/None=1

## 2. Parallel stream implementation

- [x] 2.1 Pre-extract owned segment data `(i, samples, start_ms, end_ms)` to satisfy `'static` closure requirement
- [x] 2.2 Replace sequential `for` loop with `stream::iter(...).map(...).buffer_unordered(whisper_concurrency(...))`
- [x] 2.3 Collect results into index-keyed `Vec<Option<(String, f64, f64, f32)>>` to preserve ordering
- [x] 2.4 Check `IMPORT_CANCELLED` inside each future before invoking the engine
- [x] 2.5 Reconstruct ordered transcript from result slots after stream drains

## 3. Tests

- [x] 3.1 Write `test_cancellation_inside_closure_path` — verify `IMPORT_CANCELLED=true` causes futures to return `Err` without calling the engine
- [x] 3.2 Verify `test_whisper_concurrency_gate` passes with `cargo test`
- [x] 3.3 Run `bench_transcription_pipeline` perf test on the 28-min Spanish meeting to confirm speedup

## 4. Fix-ups from review

- [x] 4.1 Fix CUDA detection on Windows (was returning `None`; should walk `PATH` for `nvcc`)
- [x] 4.2 Add defensive `IMPORT_CANCELLED.store(false)` reset in test teardown to prevent cross-test pollution
- [x] 4.3 Add doc comment on `CANCEL_FLAG_LOCK` explaining the per-test mutex rationale

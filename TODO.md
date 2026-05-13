# TODO

<!-- All items below are resolved in fix/code-quality-debt (merged). -->

## fix(lint): remove remaining `as unknown as T` casts in frontend/src

CLAUDE.md §6 prohibits `as unknown as T` outside test doubles. These survive
`pnpm lint` because `@typescript-eslint/no-explicit-any` does not flag them.

Known locations:
- `frontend/src/contexts/TranscriptContext.tsx:383`
- `frontend/src/hooks/meeting-details/useSummaryGeneration.ts:253`
- `frontend/src/components/AISummary/Block.tsx:241,243`
- `frontend/src/components/AISummary/BlockNoteSummaryView.tsx:148`

For each site, replace the cast with a proper type guard, a Zod/schema-derived
type, or a narrowed union.

---

## fix(hardware): detect CUDA via driver DLL, not developer SDK env vars

`hardware_detector.rs::has_cuda_support()` checks `CUDA_PATH` / `CUDA_HOME`,
which are only set by the NVIDIA CUDA developer toolkit — not a standard GPU
driver. An end-user machine with an NVIDIA GPU but no SDK installed reports
`has_gpu_acceleration = false` and falls back to CPU Whisper inference.

Same class of problem fixed for Vulkan in `enhance/quality-and-perf`.

Fix — check the driver-installed DLL/library instead:

```rust
fn has_cuda_support() -> bool {
    if std::env::var("CUDA_PATH").is_ok() || std::env::var("CUDA_HOME").is_ok() {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        // nvcuda.dll is placed in System32 by the NVIDIA display driver
        if std::path::Path::new(r"C:\Windows\System32\nvcuda.dll").exists() {
            return true;
        }
    }
    #[cfg(target_os = "linux")]
    {
        // libcuda.so.1 is installed by the NVIDIA driver (not the CUDA SDK)
        if std::path::Path::new("/usr/lib/x86_64-linux-gnu/libcuda.so.1").exists()
            || std::path::Path::new("/usr/lib/libcuda.so.1").exists()
        {
            return true;
        }
    }
    false
}
```

Also remove the `/usr/local/cuda` path check (SDK directory, not driver).

Note: the CUDA Cargo feature (`--features cuda`) must still be enabled at
compile time for GPU inference to work. The detector only affects configuration
decisions (beam size, threads, chunk size).

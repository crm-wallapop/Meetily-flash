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

---

## tune(audio): raise loudness target from −23 LUFS to −18 or −14 LUFS

The `LoudnessNormalizer` targets −23 LUFS (EBU R128 broadcast standard).
Broadcast loudness is intentionally quiet compared to personal/podcast listening
levels. Smoke test 2026-05-13 confirmed background noise is suppressed correctly
but voice is perceived as soft.

Candidate targets:
- **−18 LUFS** — podcast/YouTube standard; noticeable improvement without
  sounding "hot"
- **−14 LUFS** — streaming services (Spotify/Apple Music) default; loudest
  commonly-accepted ceiling

Change site: `audio_processing.rs` — the constant passed as the target LUFS to
`LoudnessNormalizer::new()`. Update the spec scenario in
`openspec/specs/audio-recording-quality/spec.md` when changing.

---

## ux(auto-detect): prompt for manually-started recordings when Meet call ends

Currently `meeting-ended` only triggers the stop-prompt banner for
detector-started recordings (`isDetectorStartedRef.current` guard in
`useAutoDetect.ts`). A user who starts recording manually before joining Meet
gets no prompt when they leave the call.

**Deferred** — the detection timing is not yet trustworthy enough to expand
the blast radius. The `meeting-ended` event fires after a 10-second debounce,
and in practice the auto-stop fires too late for the feature to feel reliable.
Resolve the detection latency issue first, then consider lifting the guard.

Tracking note: this was observed in the 2026-05-13 smoke test (auto-detect
started recording; stop-prompt fired but may not have registered correctly —
needs controlled repro with verbally-announced button presses).

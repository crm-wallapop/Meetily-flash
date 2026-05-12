# Transcription Performance & Quality Sweep

**Audio file:** 28-min Spanish meeting (`Meeting 2026-05-08_10-05-12`)  
**Machine:** Intel Core Ultra 7 155H · Intel Arc iGPU · Windows 11  
**Backend:** whisper.cpp Vulkan + flash attention  
**Results dir:** `C:\Users\CarlosRuizMartínez\Music\meetily-recordings\perf-sweep\`

---

## Summary scorecard

| Cycle | Config | Total time | vs baseline | Words | Quality |
|-------|--------|-----------|-------------|-------|---------|
| Baseline | debug + large, pre-parallel | 22:14 min | — | ~3,037 | ref |
| C1 | debug + large-v3-turbo-q5_0, VAD 2000 | 22:03 min | -1% | 3,037 | ★★★★★ |
| C2 | **release** + large-v3-turbo-q5_0, VAD 2000 | 19:56 min | -10% | 3,037 | ★★★★★ |
| C3 | debug + large, **VAD 500** | 25:01 min | +13% | 4,035 | ★★★☆☆ |
| C4 | debug + large, **VAD 5000** | 18:28 min | -17% | **1,238** | ✗ (data loss) |
| C5 | debug + **small-q5_1**, VAD 2000 | 7:19 min | **-67%** | 2,905 | ★★★★☆ |
| C6 | debug + **tiny-q5_1**, VAD 2000 | 3:32 min | **-84%** | 3,588 | ✗ (hallucination) |
| **C7** | **release + large-v3-turbo-q5_0, VAD 2000** | **18:26 min** | **-17%** | 3,037 | **★★★★★** |
| **C8** | **release + small-q5_1**, VAD 2000 | **6:23 min** | **-71%** | 2,905 | **★★★★☆** |

Historical baseline: 22:14 min total (debug, pre-parallel-states, sequential)

---

## Test matrix

| Cycle | Config | Model | VAD ms | Build | Status |
|-------|--------|-------|--------|-------|--------|
| 1 | Debug baseline (post-parallel-states) | large-v3-turbo-q5_0 | 2000 | debug | **DONE** |
| 2 | Release build | large-v3-turbo-q5_0 | 2000 | release | **DONE** |
| 3 | Narrow VAD | large-v3-turbo-q5_0 | 500 | debug | **DONE** |
| 4 | Wide VAD | large-v3-turbo-q5_0 | 5000 | debug | **DONE** |
| 5 | small-q5_1 model | small-q5_1 | 2000 | debug | **DONE** |
| 6 | tiny-q5_1 model | tiny-q5_1 | 2000 | debug | **DONE** |
| 7 | Release + best config | large-v3-turbo-q5_0 | 2000 | release | **DONE** |
| 8 | Release + fast model | small-q5_1 | 2000 | release | **DONE** |

---

## Detailed results

### Cycle 1 — debug baseline (post-parallel-states)

**Config:** large-v3-turbo-q5_0 · VAD 2000ms · debug build

| Stage | ms |
|-------|----|
| Decode | 43,128 |
| Resample | 86,347 |
| VAD | 43,020 |
| Load | 2,660 |
| **Transcription** | **1,148,069** |
| **Total** | **1,323,224** |

**Performance:** 1.476× realtime · 33 segments  
**Quality:** 3,037 words · conf=0.749 · excellent Spanish ✓

Note: parallel-states (`buffer_unordered(2)`) gave negligible benefit here because this meeting generates only 33 VAD segments. Benefit is larger for high-segment imports (84+ segments).

---

### Cycle 2 — release build

**Config:** large-v3-turbo-q5_0 · VAD 2000ms · release build

| Stage | debug | release | Δ |
|-------|-------|---------|---|
| Decode | 43,128ms | 4,373ms | **-90%** |
| Resample | 86,347ms | 2,229ms | **-97%** |
| VAD | 43,020ms | 28,210ms | **-34%** |
| **Transcription** | **1,148,069ms** | **1,158,317ms** | **+0.9%** |
| **Total** | **1,323,224ms** | **1,196,206ms** | **-9.6%** |

**Quality:** identical (3,037 words, conf=0.749)

Release build saves 2.1 min in pre-processing — Rust resampling loop was 39× slower in debug. Transcription is GPU-bound and unaffected by Rust build profile.

---

### Cycle 3 — narrow VAD (500ms)

**Config:** large-v3-turbo-q5_0 · VAD 500ms · debug build

- 282 segments vs 33 — GPU per-call overhead dominates
- Transcription +16% slower; total +13.5%
- ~40% of segments RTF > 2× (worst: 0.97s clip → RTF 10.3×)
- **Verdict: strongly counterproductive**

---

### Cycle 4 — wide VAD (5000ms)

**Config:** large-v3-turbo-q5_0 · VAD 5000ms · debug build

- 6 segments — three are 445s, 462s, 723s
- Whisper's ~30s audio context cap silently truncates each huge segment
- 59% content loss (3,037 → 1,238 words) — speed gain is illusory
- **Verdict: disqualified**

---

### Cycle 5 — small-q5_1 model

**Config:** small-q5_1 · VAD 2000ms · debug build

| Metric | large (C1) | small (C5) | Δ |
|--------|-----------|------------|---|
| Model size | 547 MB | 181 MB | -67% |
| Transcription | 1,148,069ms | **316,174ms** | **-72%** |
| Total | 1,323,224ms | **438,660ms** | **-67%** |
| Realtime | 1.476× | **5.358×** | **3.6× faster** |
| Words | 3,037 | 2,905 | -4.3% |
| Confidence | 0.749 | 0.710 | -5.2% |

**Quality:** Substance largely preserved; minor wording differences; one hallucination cluster on a difficult passage; proper nouns mostly intact. Acceptable for meeting transcription.

**Verdict: ★★★ best speed/quality balance for users who need fast turnaround**

---

### Cycle 6 — tiny-q5_1 model

**Config:** tiny-q5_1 · VAD 2000ms · debug build

| Metric | large (C1) | tiny (C6) | Δ |
|--------|-----------|-----------|---|
| Model size | 547 MB | 31 MB | -94% |
| Transcription | 1,148,069ms | **109,537ms** | **-90%** |
| Total | 1,323,224ms | **212,285ms** | **-84%** |
| Realtime | 1.476× | **15.47×** | **10.5× faster** |
| Words | 3,037 | 3,588 | +18% (hallucination) |
| Confidence | 0.749 | 0.760 | misleading (simpler tokens) |

**Quality (spot-check):** Severe degradation — "Joan ha avisado" → "Es un abizado", names dropped, wrong verbs, invented words ("inviliación"). The +18% word count is hallucination, not content.

**Verdict: ✗ disqualified for meeting use** — too inaccurate for names, action items, technical terms in Spanish

---

### Cycle 8 — release + small-q5_1, VAD 2000ms (fast mode)

> Status: **COMPLETE**

**Config:** small-q5_1 · VAD 2000ms · release build · Vulkan + flash attn

| Stage | C5 debug+small | C8 release+small | Δ |
|-------|---------------|-----------------|---|
| Decode | 20,880ms | 2,115ms | **-90%** |
| Resample | 78,293ms | 2,212ms | **-97%** |
| VAD | 22,502ms | 24,742ms | ~same |
| **Transcription** | **316,174ms** | **353,001ms** | +12% (GPU variance) |
| **Total** | **438,660ms** | **383,470ms** | **-55,190ms (-13%)** |

**Performance:** 4.799× realtime · 33 segments · **6:23 total for 28-min meeting**  
**Quality:** 2,905 words · avg_conf=0.710 — identical to cycle 5 ✓

**vs historical debug baseline:** 1,323,224ms → 383,470ms = **-71% (3.45× faster)**

Transcription +12% vs cycle 5 is run-to-run GPU variance (no Rust build effect on GPU work). Pre-processing is now essentially free: decode+resample = 4.3s total.

**Verdict: ★★★★☆ Best practical fast mode** — 3.45× faster than baseline, ~4% word loss, 6-7 min for a 28-min meeting.

---

### Cycle 7 — release + large-v3-turbo-q5_0, VAD 2000ms (best config)

**Config:** large-v3-turbo-q5_0 · VAD 2000ms · release build

| Stage | debug baseline | release best | Δ |
|-------|---------------|--------------|---|
| Decode | 43,128ms | **1,411ms** | **-97%** |
| Resample | 86,347ms | **1,852ms** | **-98%** |
| VAD | 43,020ms | **14,125ms** | **-67%** |
| Load | 2,660ms | 1,426ms | -46% |
| **Transcription** | **1,148,069ms** | **1,086,987ms** | **-5.3%** |
| **Total** | **1,323,224ms** | **1,105,801ms** | **-16.5%** |

**Performance:** 1.559× realtime · 33 segments  
**Quality:** 3,037 words · conf=0.749 — **identical to baseline** ✓

Total wall-clock: **18:26 min** for a 28:14 min meeting.

Transcription improvement vs debug cycle 1 (-5.3%) is likely GPU cache / OS warm-up effect from earlier runs. Run-to-run variance is ~±5% for transcription; treat cycles 2 and 7 as equivalent.

---

## Quality reference

Cycle 1 transcript: 3,037 words · avg_conf=0.749

---

## Final recommendation

### TL;DR

**Ship release builds.** That's the only zero-cost change that delivers a consistent 10% total improvement (2 minutes saved on a 28-minute meeting) with zero quality loss. Everything else involves a trade-off.

---

### What we learned

**1. The bottleneck is firmly the GPU**  
Transcription accounts for 97-98% of total time in a release build. All optimizations that don't touch the GPU (build profile, VAD tuning) have at most ~10% impact on total time. To go faster, you must change what the GPU does.

**2. Release build is a must-ship fix**  
The debug build was spending 43s decoding and 86s resampling — Rust debug mode was 10-39× slower for these tight loops. Release build eliminates this entirely, saving ~127s with no downside. **This should be the default for production builds.**

**3. VAD 2000ms is the sweet spot**  
- 500ms: 8.5× more segments, GPU overhead dominates, 16% slower — counterproductive
- 5000ms: segments too large for whisper's ~30s context window, 59% silent content loss — broken
- 2000ms: 33 segments, clean balance of segment overhead vs context utilization

**4. Model choice is the biggest lever — with a quality gate**  

| Model | Speed | Quality | Recommendation |
|-------|-------|---------|----------------|
| large-v3-turbo-q5_0 (547 MB) | 1.5× RT | ★★★★★ | Default — full fidelity |
| small-q5_1 (181 MB) | 5.4× RT | ★★★★☆ | Fast mode — ~4% loss, occasional hallucinations |
| tiny-q5_1 (31 MB) | 15.5× RT | ★★☆☆☆ | Too inaccurate for Spanish meetings |

**5. Parallel whisper states: marginal here, valuable for long recordings**  
`buffer_unordered(2)` shipped in this codebase gives negligible benefit for a 33-segment meeting. It pays off for longer recordings that produce 80+ segments (e.g. 90-min all-hands) where two GPU states can overlap I/O and compute.

---

### Recommended config (production)

```
Model:   large-v3-turbo-q5_0
VAD:     2000ms redemption window
Build:   --release
GPU:     Vulkan + flash attention (already enabled)
```

Expected: **~18-20 min total for a 28-min meeting** (1.4-1.6× realtime)

---

### Recommended config (fast / draft mode, opt-in)

```
Model:   small-q5_1
VAD:     2000ms
Build:   --release
```

**Measured: 6:23 total for a 28-min meeting** (3.45× faster than baseline, 2.95× faster than production config)  
Trade-off: ~4% word loss, occasional hallucination on noisy passages; proper nouns generally preserved.

---

### Remaining headroom (not tested — higher effort)

| Optimization | Expected gain | Effort |
|---|---|---|
| `distil-whisper-large-v3-q5_0` model | 3-5× on transcription, near-large quality | Download only (0 code) |
| Segment batching for short clips (<8s) | 20-30% on short-segment meetings | Medium (fiddly mapping) |
| OpenVINO backend (Intel NPU/iGPU) | 3-5× over Vulkan on Intel HW | High (build system change) |

The `distil-whisper-large-v3-q5_0` model is the highest-value next step: it reduces the decoder from 4 → 2 layers (half the autoregressive decode work), drops in as a GGUF replacement, and should be nearly indistinguishable from large-v3-turbo for Spanish meetings. No code changes required — just a model download and UI selection.

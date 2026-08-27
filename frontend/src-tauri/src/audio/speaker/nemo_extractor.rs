//! nemo_titanet embedding extraction via the `ort` crate (replaces sherpa-onnx's
//! `SpeakerEmbeddingExtractor`).
//!
//! WHY this exists: sherpa-onnx-sys 1.13.4 statically bundles ONNX Runtime 1.17.1
//! (C-API ≤17) while the project's `ort = "2.0.0-rc.10"` dep (needed for nemo AND
//! pyannote) brings C-API 27 — the two runtimes cannot coexist in one process
//! (STATUS_ACCESS_VIOLATION on the global C-API symbol table; verified by the
//! `pyannote_sherpa_load_crux` probe). Part B therefore ports nemo_titanet
//! extraction to `ort` and removes sherpa-onnx entirely: one runtime for the
//! whole app.
//!
//! FIDELITY: the fbank/CMVN pipeline below is LIFTED VERBATIM from
//! `embed-probe-ort/src/main.rs`, which was diffed constant-by-constant against
//! sherpa-onnx v1.13.4 + kaldi-native-fbank v1.22.3 C++ source and validated by
//! the subprocess cosine gate: cosine(emb_sherpa, emb_port) = 0.9946–0.9989 on
//! production-relevant clips (≥1.5s, non-silent) after the `f32::EPSILON` log-floor
//! fix. Do NOT re-tune the constants. See
//! `openspec/exploration/diarization-pyannote-boundaries-ort-probe.md`
//! §"ARCHITECTURE LOOP CLOSED" for the validation record.
//!
//! Concurrency (design D1): `ort::Session::run` takes `&mut self` in
//! 2.0.0-rc.10, so the session is wrapped in a `Mutex`. The fbank/CMVN work
//! (most of the wall-clock) happens OUTSIDE the lock and stays rayon-parallel;
//! only the model forward is serialized. Task 6.2 re-benchmarks
//! `build_fine_chunks`/`refine_pass2` post-port; the documented fallback is a
//! pool of N cloned sessions if Pass-2 busts its budget.

use anyhow::{anyhow, Result};
use ndarray::{Array1, Array3};
use ort::execution_providers::CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::PathBuf;
use std::sync::Mutex;

use super::embedding::SpeakerEmbeddingPort;
use super::types::EmbeddingVector;

/// Mean-square energy below this → treat as silence, no embedding.
/// (Verbatim port of the production gate at sherpa_adapter.rs `is_effectively_silent`.)
const SILENCE_MS_ENERGY: f32 = 1e-10;

/// nemo_titanet output dimension (verified model contract: `embs float32[N,192]`).
pub const NEMO_EMBEDDING_DIM: usize = 192;

// ============================================================================
// nemo_titanet preprocessing pipeline — LIFTED VERBATIM from embed-probe-ort
// (validated against sherpa-onnx v1.13.4 / kaldi-native-fbank v1.22.3 source).
// Do NOT modify the constants; they were verified against sherpa/knf source.
// ============================================================================

/// Parameters of the nemo_titanet preprocessing pipeline, derived from the
/// sherpa/knf source analysis. Grouped as a struct so the fbank functions are
/// parameterized and the constants testable.
#[derive(Clone, Debug)]
pub(crate) struct NemoFbankParams {
    sample_rate: usize,
    feat_dim: usize,
    frame_length_ms: f32,
    frame_shift_ms: f32,
    window_size: usize, // samples = int(sr*0.001*frame_length_ms)
    window_shift: usize, // samples = int(sr*0.001*frame_shift_ms)
    fft_size: usize,    // next_pow2(window_size)
    preemph_coeff: f32,
    low_freq: f32,
    high_freq: f32, // raw config value; effective = nyquist + high_freq if <=0
    use_power: bool,
    use_log_fbank: bool,
}

impl Default for NemoFbankParams {
    fn default() -> Self {
        // Verified against nemo-titanet-embedding.onnx metadata + sherpa defaults.
        let sample_rate = 16000usize;
        let frame_length_ms = 25.0f32;
        let frame_shift_ms = 10.0f32;
        let window_size = (sample_rate as f32 * 0.001 * frame_length_ms) as usize; // 400
        let window_shift = (sample_rate as f32 * 0.001 * frame_shift_ms) as usize; // 160
        // RoundUpToNearestPowerOfTwo(400) = 512.
        let fft_size = window_size.next_power_of_two(); // 512
        Self {
            sample_rate,
            feat_dim: 80,
            frame_length_ms,
            frame_shift_ms,
            window_size,
            window_shift,
            fft_size,
            preemph_coeff: 0.97,
            low_freq: 0.0,
            high_freq: -400.0,
            use_power: true,
            use_log_fbank: true,
        }
    }
}

/// Slaney Hz → mel. Matches knf MelScaleSlaney exactly (mel-computations.h:118).
#[inline]
fn mel_scale_slaney(freq: f32) -> f32 {
    if freq <= 1000.0 {
        freq * 3.0 / 200.0
    } else {
        15.0 + 14.545078505785561 * (freq / 1000.0).ln()
    }
}

/// Slaney mel → Hz. Matches knf InverseMelScaleSlaney (mel-computations.h:105).
#[inline]
fn inverse_mel_scale_slaney(mel: f32) -> f32 {
    if mel <= 15.0 {
        200.0 / 3.0 * mel
    } else {
        1000.0 * ((mel - 15.0) * 0.06875177742094911f32).exp()
    }
}

/// Build the librosa/slaney mel filterbank: sparse per-bin (first_index, weights)
/// pairs, matching knf MelBanks::InitLibrosaMelBanks exactly.
fn build_librosa_mel_filterbank(params: &NemoFbankParams) -> Vec<(usize, Vec<f32>)> {
    let num_bins = params.feat_dim;
    let window_length_padded = params.fft_size; // 512
    let num_fft_bins = window_length_padded / 2; // 256
    let sample_freq = params.sample_rate as f32;
    let nyquist = 0.5 * sample_freq;

    let low_freq = params.low_freq;
    let high_freq = if params.high_freq > 0.0 {
        params.high_freq
    } else {
        nyquist + params.high_freq
    };
    assert!(
        low_freq >= 0.0 && low_freq < nyquist && high_freq > 0.0 && high_freq <= nyquist,
        "bad mel range: low={} high={} nyquist={}",
        low_freq,
        high_freq,
        nyquist
    );

    let fft_bin_width = sample_freq / window_length_padded as f32; // 31.25
    let mel_low = mel_scale_slaney(low_freq);
    let mel_high = mel_scale_slaney(high_freq);
    let mel_freq_delta = (mel_high - mel_low) / (num_bins as f32 + 1.0);

    let mut bins: Vec<(usize, Vec<f32>)> = Vec::with_capacity(num_bins);
    for bin in 0..num_bins {
        let left_mel = mel_low + bin as f32 * mel_freq_delta;
        let center_mel = mel_low + (bin + 1) as f32 * mel_freq_delta;
        let right_mel = mel_low + (bin + 2) as f32 * mel_freq_delta;

        let left_hz = inverse_mel_scale_slaney(left_mel);
        let center_hz = inverse_mel_scale_slaney(center_mel);
        let right_hz = inverse_mel_scale_slaney(right_mel);

        let mut this_bin = vec![0.0f32; num_fft_bins + 1];
        let mut first_index: i32 = -1;
        let mut last_index: i32 = -1;
        for i in 0..(num_fft_bins + 1) {
            let hz = fft_bin_width * i as f32;
            if hz > left_hz && hz < right_hz {
                let weight = if hz <= center_hz {
                    (hz - left_hz) / (center_hz - left_hz)
                } else {
                    (right_hz - hz) / (right_hz - center_hz)
                };
                let weight = weight * 2.0 / (right_hz - left_hz);
                this_bin[i] = weight;
                if first_index == -1 {
                    first_index = i as i32;
                }
                last_index = i as i32;
            }
        }
        assert!(
            first_index != -1 && last_index >= first_index,
            "mel bin {} empty — num_bins too large?",
            bin
        );
        let first = first_index as usize;
        let last = last_index as usize;
        bins.push((first, this_bin[first..last + 1].to_vec()));
    }
    bins
}

/// Periodic Hann window (pytorch convention), matching knf's "hann" branch.
fn hann_window(frame_length: usize) -> Vec<f32> {
    let a = 2.0f32 * std::f32::consts::PI / frame_length as f32;
    (0..frame_length)
        .map(|i| 0.5 - 0.5 * (a * i as f32).cos())
        .collect()
}

/// Preemphasize in place, matching knf Preemphasize (descending sweep).
fn preemphasize(d: &mut [f32], preemph_coeff: f32) {
    if preemph_coeff == 0.0 {
        return;
    }
    for i in (1..d.len()).rev() {
        d[i] -= preemph_coeff * d[i - 1];
    }
    d[0] -= preemph_coeff * d[0];
}

/// Number of frames for snip_edges=true framing. Returns 0 if num_samples <
/// window_size (matches knf NumFrames).
fn num_frames_snip(num_samples: usize, window_size: usize, window_shift: usize) -> usize {
    if num_samples < window_size {
        0
    } else {
        1 + (num_samples - window_size) / window_shift
    }
}

/// Compute the [T, feat_dim] log-mel filterbank matrix for `samples` using the
/// nemo pipeline. Returns None if T==0 (too few samples for >=1 frame).
fn nemo_log_mel_fbank(samples: &[f32], params: &NemoFbankParams) -> Option<Vec<f32>> {
    use realfft::{num_complex::Complex, RealFftPlanner};

    let window_size = params.window_size;
    let window_shift = params.window_shift;
    let fft_size = params.fft_size;
    let feat_dim = params.feat_dim;
    let num_frames = num_frames_snip(samples.len(), window_size, window_shift);
    if num_frames == 0 {
        return None;
    }

    let window = hann_window(window_size);
    let mel_bins = build_librosa_mel_filterbank(params);
    let mut fft_planner = RealFftPlanner::<f32>::new();
    let r2c = fft_planner.plan_fft_forward(fft_size);
    let mut fft_out: Vec<Complex<f32>> = r2c.make_output_vec();
    let mut fft_scratch: Vec<Complex<f32>> = r2c.make_scratch_vec();

    let num_fft_bins = fft_size / 2; // 256
    let mut features = vec![0.0f32; num_frames * feat_dim];
    // Match knf FbankComputer::Compute (feature-fbank.cc): the log floor is
    // std::numeric_limits<float>::epsilon() (≈1.192e-7), NOT FLT_MIN. This
    // matters on near-zero-energy mel bins: knf floors them to log(1.192e-7)
    // ≈ -15.93, while a FLT_MIN floor (≈1.175e-38) would give ≈ -87.1. On
    // silence every bin hits this floor (causing a large systematic divergence);
    // on speech the quiet high-frequency bins do. f32::EPSILON ==
    // std::numeric_limits<float>::epsilon() exactly.
    let eps: f32 = f32::EPSILON;

    let mut frame_buf = vec![0.0f32; fft_size];
    for f in 0..num_frames {
        let start = f * window_shift;
        frame_buf[..window_size].copy_from_slice(&samples[start..start + window_size]);
        preemphasize(&mut frame_buf[..window_size], params.preemph_coeff);
        for i in 0..window_size {
            frame_buf[i] *= window[i];
        }
        r2c.process_with_scratch(&mut frame_buf, &mut fft_out, &mut fft_scratch)
            .expect("fft");
        let mut power = [0.0f32; 257];
        debug_assert_eq!(num_fft_bins + 1, 257);
        for k in 0..=num_fft_bins {
            let re = fft_out[k].re;
            let im = fft_out[k].im;
            power[k] = re * re + im * im;
        }
        let mel_row = &mut features[f * feat_dim..(f + 1) * feat_dim];
        for (bin, (first, weights)) in mel_bins.iter().enumerate() {
            let mut energy = 0.0f32;
            for (w_idx, &w) in weights.iter().enumerate() {
                energy += w * power[first + w_idx];
            }
            mel_row[bin] = if params.use_log_fbank {
                energy.max(eps).ln()
            } else {
                energy
            };
        }
        for v in frame_buf.iter_mut() {
            *v = 0.0;
        }
    }
    Some(features)
}

/// Per-feature CMVN (NormalizePerFeature) over the [T, feat_dim] row-major
/// matrix, matching sherpa nemo-impl.h NormalizePerFeature exactly.
fn normalize_per_feature(features: &mut [f32], num_frames: usize, feat_dim: usize) {
    let mut ex = vec![0.0f32; feat_dim];
    let mut ex2 = vec![0.0f32; feat_dim];
    for f in 0..num_frames {
        let row = &features[f * feat_dim..(f + 1) * feat_dim];
        for j in 0..feat_dim {
            ex[j] += row[j];
            ex2[j] += row[j] * row[j];
        }
    }
    let n = num_frames as f32;
    for j in 0..feat_dim {
        ex[j] /= n;
        ex2[j] /= n;
    }
    let mut denom = vec![0.0f32; feat_dim];
    for j in 0..feat_dim {
        let variance = (ex2[j] - ex[j] * ex[j]).max(1e-5);
        denom[j] = variance.sqrt() + 1e-5;
    }
    for f in 0..num_frames {
        let row = &mut features[f * feat_dim..(f + 1) * feat_dim];
        for j in 0..feat_dim {
            row[j] = (row[j] - ex[j]) / denom[j];
        }
    }
}

/// Build the ort input tensors for nemo_titanet from raw samples:
///   audio_signal : float32[1, 80, T_padded]
///   length       : int64[1]
/// Returns (audio_signal_flat in [80, T_padded] row-major, T_padded, T_unpadded, length).
/// Returns None if the clip is too short to yield even one frame.
fn nemo_build_model_inputs(
    samples: &[f32],
    params: &NemoFbankParams,
) -> Option<(Vec<f32>, usize, usize, i64)> {
    let feat_dim = params.feat_dim;
    let mut features = nemo_log_mel_fbank(samples, params)?;
    let num_frames = features.len() / feat_dim;
    normalize_per_feature(&mut features, num_frames, feat_dim);
    let pad = if num_frames % 16 != 0 {
        16 - (num_frames % 16)
    } else {
        0
    };
    let num_frames_padded = num_frames + pad;
    features.resize(num_frames_padded * feat_dim, 0.0);
    let mut transposed = vec![0.0f32; feat_dim * num_frames_padded];
    for t in 0..num_frames_padded {
        for j in 0..feat_dim {
            transposed[j * num_frames_padded + t] = features[t * feat_dim + j];
        }
    }
    Some((transposed, num_frames_padded, num_frames, num_frames as i64))
}

// ============================================================================
// END verbatim preprocessing pipeline.
// ============================================================================

/// Silence gate — verbatim port of the production `is_effectively_silent`
/// (mean-square energy < 1e-10). Kept here so the extractor is self-contained;
/// the adapter re-exports/reuses it.
pub(crate) fn is_effectively_silent(audio: &[f32]) -> bool {
    if audio.is_empty() {
        return true;
    }
    let sum_sq: f32 = audio.iter().map(|&s| s * s).sum();
    (sum_sq / audio.len() as f32) < SILENCE_MS_ENERGY
}

/// nemo_titanet embedding extractor backed by an `ort::Session`.
///
/// Replaces sherpa-onnx's `SpeakerEmbeddingExtractor`. The model file is the
/// SAME on-disk `nemo-titanet-embedding.onnx` sherpa used — only the loader and
/// preprocessing runtime changed (both validated to 0.9946+ cosine).
pub struct NemoEmbeddingExtractor {
    /// `Session::run` takes `&mut self` in ort 2.0.0-rc.10, so sessions sit
    /// behind Mutexes. A POOL of N independent sessions (round-robin checkout)
    /// restores rayon-parallel inference for `build_fine_chunks`/`refine_pass2`
    /// — a single Mutex serialized Pass-2 to 95.9s against its 60s budget
    /// (task 6.2's documented fallback; ~40 MB per nemo_titanet session).
    /// Fbank/CMVN work happens outside the locks.
    sessions: Vec<Mutex<Session>>,
    next: std::sync::atomic::AtomicUsize,
    audio_input_name: String,
    length_input_name: String,
    emb_output_name: String,
    dim: usize,
    params: NemoFbankParams,
}

impl NemoEmbeddingExtractor {
    /// Build the extractor from the on-disk nemo_titanet model.
    ///
    /// Session config: Level3, CPU, intra_threads=2. DEVIATION from design
    /// D1's "1 intra thread" (which mirrored the probe): with the session
    /// pool feeding rayon's par_iter, 2 intra threads per session measured
    /// within the Pass-2 budget (40.19s vs 60s on cde5c264) while keeping
    /// total ORT threads bounded at pool_size × 2.
    pub fn new(model_path: &str) -> Result<Self> {
        let path = PathBuf::from(model_path);
        if !path.exists() {
            return Err(anyhow!("embedding model not found: {}", model_path));
        }

        let build_session = || -> Result<Session> {
            let providers = vec![CPUExecutionProvider::default().build()];
            Session::builder()
                .map_err(|e| anyhow!("session builder: {}", e))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow!("opt level: {}", e))?
                .with_execution_providers(providers.clone())
                .map_err(|e| anyhow!("providers: {}", e))?
                .with_intra_threads(2)
                .map_err(|e| anyhow!("intra threads: {}", e))?
                .commit_from_file(&path)
                .map_err(|e| anyhow!("commit nemo_titanet session: {}", e))
        };
        // Pool size: min(8, hardware threads) — enough to keep rayon's
        // par_iter fed during refine_pass2 without unbounded memory.
        let pool_size = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .clamp(1, 8);
        // First session doubles as the I/O-name introspection source.
        let first = build_session()?;
        let audio_input_name = first
            .inputs
            .iter()
            .find(|i| i.name == "audio_signal")
            .ok_or_else(|| anyhow!("model has no 'audio_signal' input"))?
            .name
            .to_string();
        let length_input_name = first
            .inputs
            .iter()
            .find(|i| i.name == "length")
            .ok_or_else(|| anyhow!("model has no 'length' input"))?
            .name
            .to_string();
        let emb_output_name = first
            .outputs
            .iter()
            .find(|o| o.name == "embs")
            .or_else(|| first.outputs.iter().find(|o| o.name == "embeddings"))
            .ok_or_else(|| anyhow!("model has no 'embs'/'embeddings' output"))?
            .name
            .to_string();
        let mut sessions = Vec::with_capacity(pool_size);
        sessions.push(Mutex::new(first));
        for _ in 1..pool_size {
            sessions.push(Mutex::new(build_session()?));
        }

        Ok(Self {
            sessions,
            next: std::sync::atomic::AtomicUsize::new(0),
            audio_input_name,
            length_input_name,
            emb_output_name,
            // nemo-titanet-embedding.onnx output is verified float32[N, 192]
            // (matches sherpa's extractor.dim() == 192).
            dim: NEMO_EMBEDDING_DIM,
            params: NemoFbankParams::default(),
        })
    }

    /// Embedding dimension (192).
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Raw extraction — the private-method equivalent the diarization pipeline
    /// calls (the adapter's `extract_embedding` delegates here).
    /// Returns None on silence or sub-minimum-frame audio, mirroring the
    /// sherpa gates: `is_effectively_silent` → None; < 1 mel frame
    /// (< 400 samples @16kHz, the frame window) → None.
    pub fn extract_embedding(&self, audio: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        if is_effectively_silent(audio) {
            return None;
        }
        // The fbank constants are calibrated for 16kHz (window 400/shift 160).
        // All production callers pass DIARIZATION_SAMPLE_RATE = 16000; sherpa
        // resampled internally for other rates, but no production path does.
        if sample_rate != 16000 {
            log::warn!(
                "nemo extractor: non-16kHz input ({}Hz) — fbank assumes 16kHz; \
                 sherpa previously resampled internally. Skipping.",
                sample_rate
            );
            return None;
        }

        let (audio_flat, t_padded, _t_unpadded, length_val) =
            nemo_build_model_inputs(audio, &self.params)?;

        let audio_3d: Array3<f32> = Array1::from(audio_flat)
            .into_shape_with_order([1, self.params.feat_dim, t_padded])
            .ok()?;
        let length_arr: Array1<i64> = ndarray::arr1(&[length_val]);
        let audio_ref = TensorRef::from_array_view(audio_3d.view()).ok()?;
        let length_ref = TensorRef::from_array_view(length_arr.view()).ok()?;

        let inputs = ort::inputs![
            self.audio_input_name.as_str() => audio_ref,
            self.length_input_name.as_str() => length_ref,
        ];

        // SessionOutputs borrows the session — extract the owned embedding
        // inside the lock scope, then release.
        // Round-robin over the session pool: concurrent callers (rayon
        // par_iter) land on distinct sessions and run truly in parallel.
        let idx = self
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            % self.sessions.len();
        // Poison recovery (into_inner): a panic in another thread while
        // holding the lock must not permanently kill this session — the ORT
        // session itself has no corruptable invariants. A silent permanent
        // `None` here would degrade every later extraction.
        let mut session = match self.sessions[idx].lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        let outputs = session.run(inputs).ok()?;
        let out = outputs.get(self.emb_output_name.as_str())?;
        let arr = out.try_extract_array::<f32>().ok()?;
        let slice: &[f32] = arr
            .as_slice()
            .unwrap_or_else(|| arr.to_slice().unwrap());
        if slice.len() < self.dim {
            return None;
        }
        Some(slice[..self.dim].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Task 1.4 — I/O contract: `nemo_build_model_inputs` feeds the model its
    /// verified contract. `audio_signal` is [1, 80, T_padded] where
    /// T_padded = ceil(T/16)*16, `length` = unpadded frame count, and the
    /// pad-16 region is exactly zero (post-CMVN, matching sherpa — design
    /// Open Question 2).
    #[test]
    fn nemo_model_inputs_io_contract() {
        let params = NemoFbankParams::default();

        // 2.0s @ 16kHz = 32000 samples → frames = 1 + (32000-400)/160 = 198
        // (integer division). 198 % 16 = 6 → pad = 10 → T_padded = 208.
        let samples: Vec<f32> = (0..32000)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / 16000.0).sin() * 0.1)
            .collect();
        let (flat, t_padded, t_unpadded, length) =
            nemo_build_model_inputs(&samples, &params).expect("2s of speech yields frames");
        assert_eq!(t_unpadded, 198, "frame count for 2.0s");
        assert_eq!(t_padded, 208, "padded to multiple of 16");
        assert_eq!(length, 198, "length tensor = unpadded frames");
        assert_eq!(flat.len(), params.feat_dim * t_padded);

        // Pad region is exactly zero: frame t in [t_unpadded, t_padded) has
        // all 80 features == 0.0 (zero-filled AFTER CMVN, matching sherpa's
        // post-normalize resize).
        for t in t_unpadded..t_padded {
            for j in 0..params.feat_dim {
                let v = flat[j * t_padded + t];
                assert_eq!(v, 0.0, "pad frame {} feat {} must be zero", t, j);
            }
        }

        // Non-pad region is non-degenerate (CMVN output: mean≈0 per feature
        // over the UNPADDED frames).
        for j in 0..params.feat_dim {
            let mut sum = 0.0f32;
            for t in 0..t_unpadded {
                sum += flat[j * t_padded + t];
            }
            let mean = sum / t_unpadded as f32;
            assert!(
                mean.abs() < 1e-3,
                "CMVN centers each feature (feat {} mean {})",
                j,
                mean
            );
        }
    }

    /// Task 1.4 (cont.) — sub-frame audio (< 400 samples = < 1 mel frame)
    /// yields None (the `is_ready`-equivalent minimum gate).
    #[test]
    fn nemo_model_inputs_none_below_one_frame() {
        let params = NemoFbankParams::default();
        assert!(nemo_build_model_inputs(&vec![0.1f32; 399], &params).is_none());
        assert!(nemo_build_model_inputs(&vec![0.1f32; 400], &params).is_some());
    }

    /// Task 1.2 (filter parity, unit level) — the silence gate: zero-energy
    /// audio is silent, non-silent passes. (The full sherpa-parity sweep on
    /// real clips is the cosine-gate fixture test in tests/.)
    #[test]
    fn nemo_extractor_silence_gate() {
        assert!(is_effectively_silent(&vec![0.0f32; 16000]));
        assert!(is_effectively_silent(&[]));
        assert!(!is_effectively_silent(&vec![0.5f32; 16000]));
        // Just-below-threshold energy stays silent (mean-square < 1e-10).
        let quiet = vec![1e-6f32; 16000];
        let mean_sq: f32 = quiet.iter().map(|&s| s * s).sum::<f32>() / quiet.len() as f32;
        assert_eq!(is_effectively_silent(&quiet), mean_sq < 1e-10);
    }

    /// Fbank param sanity (guards against accidental constant drift — the
    /// constants were verified against sherpa/knf source; do not re-tune).
    #[test]
    fn nemo_fbank_params_match_verified_constants() {
        let p = NemoFbankParams::default();
        assert_eq!(p.sample_rate, 16000);
        assert_eq!(p.feat_dim, 80);
        assert_eq!(p.window_size, 400, "25ms @ 16kHz");
        assert_eq!(p.window_shift, 160, "10ms @ 16kHz");
        assert_eq!(p.fft_size, 512, "next_pow2(400)");
        assert!((p.preemph_coeff - 0.97).abs() < f32::EPSILON);
        assert_eq!(p.low_freq, 0.0);
        assert_eq!(p.high_freq, -400.0, "effective 7600Hz (nyquist - 400)");
        assert!(p.use_log_fbank);
        assert!(p.use_power);
    }

    /// Task 1.3 — is_ready parity sweep (25 ms → 2 s).
    ///
    /// SOURCE FINDING (speaker-embedding-extractor-nemo-impl.h, v1.13.4):
    /// `IsReady(OnlineStream *s)` is exactly
    ///   `return s->GetNumProcessedFrames() < s->NumFramesReady();`
    /// For a fresh stream (0 processed frames) this reduces to "at least ONE
    /// complete frame is available" — there is NO hidden minimum-frame or
    /// minimum-sample constant. knf NumFramesReady (snip_edges=true) counts
    /// complete windows: `1 + (N - window_size)/window_shift` for N ≥ 400,
    /// else 0 — identical to the port's `num_frames_snip`. Therefore the
    /// port's `< 400 samples → None` gate IS sherpa-parity by construction;
    /// this sweep locks the boundary in across the 25ms→2s range.
    #[test]
    fn nemo_is_ready_parity_sweep_25ms_to_2s() {
        let params = NemoFbankParams::default();

        // Sweep sample counts spanning 25ms→2s plus the exact boundaries.
        // 25ms=400, 50ms=800, 100ms=1600, 250ms=4000, 500ms=8000,
        // 700ms=11200 (short-1/2 fixtures), 1s=16000, 1.5s=24000, 2s=32000,
        // and the off-by-ones around the frame boundary.
        for &n in &[
            1usize,
            399,
            400,
            401,
            800,
            1600,
            4000,
            8000,
            11200,
            15999,
            16000,
            23999,
            24000,
            31999,
            32000,
        ] {
            let samples: Vec<f32> = (0..n)
                .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / 16000.0).sin() * 0.2)
                .collect();
            let expected_frames =
                num_frames_snip(n, params.window_size, params.window_shift);
            let got = nemo_build_model_inputs(&samples, &params);
            if expected_frames == 0 {
                assert!(
                    got.is_none(),
                    "{} samples (<1 frame): port must DROP (sherpa IsReady=false)",
                    n
                );
            } else {
                let (flat, t_padded, t_unpadded, length) =
                    got.unwrap_or_else(|| panic!("{} samples: port must EMBED", n));
                assert_eq!(t_unpadded, expected_frames, "{} samples: frame count", n);
                assert_eq!(length as usize, expected_frames);
                assert_eq!(
                    t_padded,
                    (expected_frames + 15) / 16 * 16,
                    "{} samples: pad-to-16",
                    n
                );
                assert_eq!(flat.len(), params.feat_dim * t_padded);
            }
        }

        // Documented equivalence (the parity claim, stated as an assertion so
        // it cannot rot silently):
        //   sherpa IsReady(fresh stream, N samples)
        //     ≡ NumFramesReady(N) > 0
        //     ≡ num_frames_snip(N, 400, 160) > 0
        //     ≡ N >= 400
        assert_eq!(
            num_frames_snip(400, params.window_size, params.window_shift),
            1
        );
        assert_eq!(
            num_frames_snip(399, params.window_size, params.window_shift),
            0
        );
    }
}

impl SpeakerEmbeddingPort for NemoEmbeddingExtractor {
    fn extract(&self, audio: &[f32], sample_rate: u32) -> Result<EmbeddingVector> {
        let min_samples = (sample_rate as usize) / 2;
        if audio.len() < min_samples {
            return Err(anyhow!(
                "audio too short: {} samples (minimum ~{} for 0.5s at {}Hz)",
                audio.len(),
                min_samples,
                sample_rate
            ));
        }
        if is_effectively_silent(audio) {
            return Err(anyhow!(
                "audio is silent (near-zero energy); cannot extract a meaningful speaker embedding"
            ));
        }

        let embedding = self
            .extract_embedding(audio, sample_rate)
            .ok_or_else(|| anyhow!("not enough audio to extract embedding"))?;

        EmbeddingVector::from_slice(&embedding, self.dim)
            .map_err(|e| anyhow!("embedding validation failed: {}", e))
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

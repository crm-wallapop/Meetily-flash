//! Candidate (ort) path of the cosine-equivalence subprocess harness.
//!
//! WHY THIS EXISTS
//! ---------------
//! sherpa-onnx 1.13.x statically bundles ONNX Runtime 1.17.1 (C-API ≤17), while
//! the project's `ort = 2.0.0-rc.10` crate ships a much newer ORT (C-API 27).
//! Loading both into one process triggers a STATUS_ACCESS_VIOLATION on Windows.
//! The `meetily-flash` crate at the time linked BOTH (sherpa for diarization,
//! ort for its ONNX models), so it could not be the parent process. This crate — and its sibling
//! `embed-probe-sherpa` — are standalone binaries with NO dependency on
//! `app_lib`, so each loads exactly one ORT. A third process (shell script /
//! Python) calls both binaries, then runs `compare_embeddings.py`.
//!
//! CLI CONTRACT (shared verbatim with embed-probe-sherpa)
//! ------------------------------------------------------
//!   argv[1] = path to a manifest JSON file: an array of
//!             { "id": "<str>", "path": "<audio file abs path>",
//!               "start_seconds": <f64>, "end_seconds": <f64> }
//!             The seconds keys are sample-rate-independent; the optional
//!             start_sample/end_sample keys (legacy, @16kHz) are ignored.
//!
//!   stdout  = a single JSON object:
//!             { "results": [
//!                 { "id": "...", "embedding": [f32,...], "skipped": false },
//!                 { "id": "...", "embedding": [],        "skipped": true }, ...
//!               ] }
//!             `skipped: true` means the clip was too short to yield >=1 mel
//!             frame (nemo window_size=400 samples), matching sherpa's
//!             is_ready()=false semantics.
//!
//!   env     = EMBED_PROBE_MODEL (optional). Defaults to
//!             ~/.meetily-models/nemo-titanet-embedding.onnx.
//!
//! PREPROCESSING
//! -------------
//! All preprocessing constants + functions below are COPIED VERBATIM from
//! frontend/src-tauri/tests/pyannote_ort_probe.rs (lines ~1507-1839) — the
//! `nemo_titanet_ort_cosine_equivalence` test's helper module. They were
//! verified against sherpa-onnx v1.13.4 source (knf fbank) and the model's
//! metadata; do NOT re-derive them. The model I/O contract is:
//!   input  audio_signal : float32[N, 80, T_padded]   (T_padded = ceil(T/16)*16)
//!   input  length        : int64[N]                   (T, unpadded frame count)
//!   output embs          : float32[N, 192]
//!
//! The ort session-builder pattern is copied from the same test (~line 1948)
//! and from lines 48-59.

use anyhow::{anyhow, Context, Result};
use ndarray::{Array1, Array3};
use ort::execution_providers::CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

// ============================================================================
// PREPROCESSING PIPELINE — copied verbatim from
// frontend/src-tauri/tests/pyannote_ort_probe.rs:1507-1839.
// Do NOT modify the constants; they were verified against sherpa-onnx v1.13.4
// source (knf fbank). The only edits are: `struct NemoFbankParams` lost its
// `#[allow(dead_code)]` (every field IS read here), and the helper fns became
// module-private instead of test-local.
// ============================================================================

/// Parameters of the nemo_titanet preprocessing pipeline, derived from the
/// sherpa/knf source analysis documented in the test file. Grouped as a struct
/// so the fbank functions below are parameterized and the constants testable.
#[allow(dead_code)]
struct NemoFbankParams {
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

struct Clip {
    id: String,
    path: PathBuf,
    start_seconds: f64,
    end_seconds: f64,
}

fn main() -> Result<()> {
    // ---- Parse argv ----
    let manifest_path = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow!("usage: embed-probe-ort <manifest.json>"))?;
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path))?;
    let manifest_val: Value = serde_json::from_slice(&manifest_bytes)
        .with_context(|| format!("parse manifest {}", manifest_path))?;
    let arr = manifest_val
        .as_array()
        .ok_or_else(|| anyhow!("manifest must be a JSON array"))?;

    let mut clips: Vec<Clip> = Vec::with_capacity(arr.len());
    for (i, entry) in arr.iter().enumerate() {
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("manifest[{}].id missing/not a string", i))?
            .to_string();
        let path = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("manifest[{}].path missing/not a string", i))?;
        // Prefer the sample-rate-independent seconds keys. Fall back to the
        // legacy start_sample/end_sample (@16kHz) → seconds only if absent.
        let (start_seconds, end_seconds) = match (
            entry.get("start_seconds").and_then(Value::as_f64),
            entry.get("end_seconds").and_then(Value::as_f64),
        ) {
            (Some(s), Some(e)) => (s, e),
            _ => {
                let s16 = entry
                    .get("start_sample")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("manifest[{}] needs start_seconds/end_seconds (or start_sample/end_sample @16kHz)", i))?
                    as f64;
                let e16 = entry
                    .get("end_sample")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| anyhow!("manifest[{}] needs end_seconds (or end_sample @16kHz)", i))?
                    as f64;
                (s16 / 16000.0, e16 / 16000.0)
            }
        };
        clips.push(Clip {
            id,
            path: PathBuf::from(path),
            start_seconds,
            end_seconds,
        });
    }

    // ---- Resolve the model path ----
    let model_path = match std::env::var("EMBED_PROBE_MODEL") {
        Ok(p) => PathBuf::from(p),
        Err(_) => dirs_home().join(".meetily-models").join("nemo-titanet-embedding.onnx"),
    };
    if !model_path.exists() {
        return Err(anyhow!(
            "embedding model not found at {} (set EMBED_PROBE_MODEL to override)",
            model_path.display()
        ));
    }

    // ---- Build the ort session (project's ort 2.0.0-rc.10) ----
    // Mirrors the test (~line 1948) and pyannote probe (lines 48-59).
    let providers = vec![CPUExecutionProvider::default().build()];
    let session = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .with_execution_providers(providers)?
        .with_intra_threads(1)?
        .commit_from_file(&model_path)
        .with_context(|| format!("ort load nemo-titanet {}", model_path.display()))?;

    eprintln!("embed-probe-ort: session inputs:");
    for (i, inp) in session.inputs.iter().enumerate() {
        eprintln!("  [{}] {} = {:?}", i, inp.name, inp.input_type);
    }
    eprintln!("embed-probe-ort: session outputs:");
    for (i, out) in session.outputs.iter().enumerate() {
        eprintln!("  [{}] {} = {:?}", i, out.name, out.output_type);
    }

    // Locate I/O by documented name. nemo_titanet: audio_signal (f32) + length
    // (int64) → embs (f32[N,192]). Fall back to "embeddings" for older models.
    let audio_input_name = session
        .inputs
        .iter()
        .find(|i| i.name == "audio_signal")
        .ok_or_else(|| anyhow!("model has no 'audio_signal' input"))?
        .name
        .to_string();
    let length_input_name = session
        .inputs
        .iter()
        .find(|i| i.name == "length")
        .ok_or_else(|| anyhow!("model has no 'length' input"))?
        .name
        .to_string();
    let emb_output_name = session
        .outputs
        .iter()
        .find(|o| o.name == "embs")
        .or_else(|| session.outputs.iter().find(|o| o.name == "embeddings"))
        .ok_or_else(|| anyhow!("model has no 'embs'/'embeddings' output"))?
        .name
        .to_string();
    eprintln!(
        "embed-probe-ort: I/O names — in[{}/{}] out[{}]",
        audio_input_name, length_input_name, emb_output_name
    );

    // nemo-titanet-embedding.onnx output is verified to be float32[N, 192].
    // The test cross-checks this against sherpa's extractor.dim() (== 192); we
    // don't have sherpa in this process, so we hardcode the verified value.
    // The slice.len() >= emb_dim assert below guards against surprises.
    let emb_dim: usize = 192;

    let mut session = session; // run() needs &mut self

    // ---- Per-clip decode + extract ----
    let mut decode_cache: std::collections::HashMap<String, (u32, Vec<f32>)> =
        std::collections::HashMap::new();
    let params = NemoFbankParams::default();

    let mut results = Vec::with_capacity(clips.len());
    for clip in &clips {
        // ---- Decode (cached) at native sample rate ----
        let cache_key = clip.path.to_string_lossy().into_owned();
        let (native_sr, all_samples): (u32, Vec<f32>) = match decode_cache.get(&cache_key) {
            Some(cached) => cached.clone(),
            None => {
                let (sr, samples) = decode_mono_native(&clip.path)
                    .with_context(|| format!("decode {}", clip.path.display()))?;
                decode_cache.insert(cache_key.clone(), (sr, samples.clone()));
                (sr, samples)
            }
        };

        // ---- Slice the requested [start_seconds, end_seconds) window at the
        // file's NATIVE sample rate. seconds are sample-rate-independent, so
        // this lands at the correct wall-clock position regardless of 16/48kHz.
        let native_sr_f = native_sr as f64;
        let s = ((clip.start_seconds * native_sr_f).round() as isize).max(0) as usize;
        let e = ((clip.end_seconds * native_sr_f).round() as isize).max(0) as usize;
        let s = s.min(all_samples.len());
        let e = e.min(all_samples.len());
        if e <= s {
            eprintln!(
                "embed-probe-ort: [{}] empty range [{:.3}s..{:.3}s) @{}Hz → [{}..{}) — skipping",
                clip.id, clip.start_seconds, clip.end_seconds, native_sr, s, e
            );
            results.push(json!({
                "id": clip.id,
                "embedding": [],
                "skipped": true,
            }));
            continue;
        }
        let native_clip = &all_samples[s..e];

        // ---- Resample the native-rate slice to 16000Hz mono so the sherpa and
        // ort paths see byte-identical input (this is the whole point of the
        // gate — both binaries MUST feed nemo the same 16kHz mono slice).
        let clip_samples: Vec<f32> = resample_to_16k(native_clip, native_sr)
            .with_context(|| format!("resample [{}] {}Hz→16k", clip.id, native_sr))?;
        let clip_samples: &[f32] = &clip_samples;

        // ---- Build ort inputs + forward ----
        // Mirrors the test's ort_embedding closure (~line 1998).
        let (audio_flat, t_padded, _t_unpadded, length_val) =
            match nemo_build_model_inputs(clip_samples, &params) {
                Some(x) => x,
                None => {
                    // Too short for >=1 mel frame (matches sherpa is_ready()=false).
                    eprintln!(
                        "embed-probe-ort: [{}] nemo_build_model_inputs=None ({} samples < 1 frame) — skipping",
                        clip.id,
                        clip_samples.len()
                    );
                    results.push(json!({
                        "id": clip.id,
                        "embedding": [],
                        "skipped": true,
                    }));
                    continue;
                }
            };

        let audio_3d: Array3<f32> = Array1::from(audio_flat)
            .into_shape_with_order([1, params.feat_dim, t_padded])
            .map_err(|e| anyhow!("audio shape: {}", e))?;
        let length_arr: Array1<i64> = ndarray::arr1(&[length_val]);
        let audio_ref = TensorRef::from_array_view(audio_3d.view())
            .map_err(|e| anyhow!("audio tensor ref: {}", e))?;
        let length_ref = TensorRef::from_array_view(length_arr.view())
            .map_err(|e| anyhow!("length tensor ref: {}", e))?;
        let inputs = ort::inputs![
            audio_input_name.as_str() => audio_ref,
            length_input_name.as_str() => length_ref,
        ];
        let outputs = session
            .run(inputs)
            .map_err(|e| anyhow!("ort forward [{}]: {}", clip.id, e))?;
        let out = outputs
            .get(emb_output_name.as_str())
            .ok_or_else(|| anyhow!("output '{}' not in ort outputs", emb_output_name))?;
        let arr = out
            .try_extract_array::<f32>()
            .map_err(|e| anyhow!("extract embs [{}]: {}", clip.id, e))?;
        let slice = arr
            .as_slice()
            .unwrap_or_else(|| arr.to_slice().unwrap());
        if slice.len() < emb_dim {
            return Err(anyhow!(
                "embedding output too short: {} (need {})",
                slice.len(),
                emb_dim
            ));
        }
        let emb: Vec<f32> = slice[..emb_dim].to_vec();

        eprintln!(
            "embed-probe-ort: [{}] extracted {}-dim embedding ({} native samp @{}Hz → {} 16k samp, T_pad={})",
            clip.id,
            emb.len(),
            native_clip.len(),
            native_sr,
            clip_samples.len(),
            t_padded
        );
        results.push(json!({
            "id": clip.id,
            "embedding": emb,
            "skipped": false,
        }));
    }

    let out = json!({ "results": results });
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

/// Decode any audio file symphonia supports to mono f32 samples at its NATIVE
/// sample rate (whatever the container reports — typically 48000Hz for m4a/mp4
/// meeting recordings). Does NOT resample; the caller slices by seconds × this
/// native rate and then hands the slice to `resample_to_16k`. The native rate is
/// returned alongside the samples. Mirrors
/// frontend/src-tauri/src/audio/decoder.rs:397 decode_audio_file_with_progress
/// (no progress callback, no ffmpeg pre-conversion — probe files are
/// mp4/wav/m4a/mp3, all symphonia-native).
fn decode_mono_native(path: &Path) -> Result<(u32, Vec<f32>)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path)
        .with_context(|| format!("open audio file {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .with_context(|| format!("probe audio format {}", path.display()))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow!("no audio track in {}", path.display()))?;
    let track_id = track.id;

    let sample_rate = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate in {}", path.display()))?;

    eprintln!(
        "embed-probe-ort: decoded {} at native {}Hz (slicing by seconds × {}, then resampling to 16kHz)",
        path.display(),
        sample_rate,
        sample_rate
    );

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow!("make decoder for {}: {}", path.display(), e))?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut channels: u16 = track
        .codec_params
        .channels
        .map(|c| c.count() as u16)
        .unwrap_or(1);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                eprintln!("embed-probe-ort: packet read error in {}: {}", path.display(), e);
                break;
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                if sample_buf.is_none() {
                    let spec = *decoded.spec();
                    let actual = spec.channels.count() as u16;
                    if actual != channels {
                        eprintln!(
                            "embed-probe-ort: channel count corrected metadata={} actual={} (using actual)",
                            channels, actual
                        );
                        channels = actual;
                    }
                    let duration = decoded.capacity() as u64;
                    sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
                }
                if let Some(ref mut buf) = sample_buf {
                    buf.copy_interleaved_ref(decoded);
                    interleaved.extend_from_slice(buf.samples());
                }
            }
            Err(e) => {
                eprintln!("embed-probe-ort: decode error in {}: {}", path.display(), e);
                continue;
            }
        }
    }

    if interleaved.is_empty() {
        return Err(anyhow!("no samples decoded from {}", path.display()));
    }

    let mono = if channels > 1 {
        audio_to_mono(&interleaved, channels)
    } else {
        interleaved
    };

    let max_abs = mono
        .iter()
        .filter(|s| s.is_finite())
        .map(|s| s.abs())
        .fold(0.0f32, |a, b| a.max(b));
    let mono = if max_abs > 1.0 {
        let scale = 1.0 / max_abs;
        mono.into_iter().map(|s| s * scale).collect()
    } else {
        mono
    };

    Ok((sample_rate, mono))
}

/// Resample a mono f32 slice from `from_sample_rate` to exactly 16000Hz mono.
/// No-op fast path when already 16kHz. Uses rubato `SincFixedIn` with the same
/// adaptive parameters as frontend/src-tauri/src/audio/audio_processing.rs:548
/// `resample` (the project's verified resampler), so both probe crates and the
/// production decoder agree bit-for-bit on the 48k→16k downsample. Mono in/out.
///
/// `FixedIn` requires the whole input up front (we have it — the clip slice),
/// and `process` returns the full resampled buffer in one call.
fn resample_to_16k(input: &[f32], from_sample_rate: u32) -> Result<Vec<f32>> {
    use rubato::{
        Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
    };

    const TARGET_SR: u32 = 16000;
    if input.is_empty() {
        return Ok(Vec::new());
    }
    if from_sample_rate == TARGET_SR {
        return Ok(input.to_vec());
    }

    let ratio = TARGET_SR as f64 / from_sample_rate as f64;
    // Downsample (ratio < 1) needs strong anti-aliasing; match the project's
    // 48k→16k branch (audio_processing.rs:589-598): sinc_len=512, Cubic, 512.
    let (sinc_len, interpolation_type, oversampling) = if ratio >= 2.0 {
        (512, SincInterpolationType::Cubic, 512)
    } else if ratio >= 1.5 {
        (384, SincInterpolationType::Cubic, 384)
    } else if ratio > 1.0 {
        (256, SincInterpolationType::Linear, 256)
    } else if ratio <= 0.5 {
        (512, SincInterpolationType::Cubic, 512)
    } else {
        (384, SincInterpolationType::Linear, 384)
    };
    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff: 0.95,
        interpolation: interpolation_type,
        oversampling_factor: oversampling,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut resampler = SincFixedIn::<f32>::new(ratio, 2.0, params, input.len(), 1)
        .map_err(|e| anyhow!("rubato SincFixedIn::new({}Hz→{}Hz): {}", from_sample_rate, TARGET_SR, e))?;
    let waves_in = vec![input.to_vec()];
    let waves_out = resampler
        .process(&waves_in, None)
        .map_err(|e| anyhow!("rubato process({}Hz→{}Hz, {} samp): {}", from_sample_rate, TARGET_SR, input.len(), e))?;
    Ok(waves_out.into_iter().next().unwrap())
}

/// Mirrors audio/audio_processing.rs:516 audio_to_mono.
fn audio_to_mono(audio: &[f32], channels: u16) -> Vec<f32> {
    let effective = if channels > 2 { 2 } else { channels };
    let mut out = Vec::with_capacity(audio.len() / channels as usize);
    for chunk in audio.chunks(channels as usize) {
        let sum: f32 = chunk.iter().take(effective as usize).sum();
        out.push(sum / effective as f32);
    }
    out
}

/// Cross-platform home dir without pulling a dep. (Same impl as embed-probe-sherpa.)
fn dirs_home() -> PathBuf {
    if let Ok(h) = std::env::var("USERPROFILE") {
        return PathBuf::from(h);
    }
    let drive = std::env::var("HOMEDRIVE").unwrap_or_default();
    let pathpart = std::env::var("HOMEPATH").unwrap_or_default();
    if !drive.is_empty() || !pathpart.is_empty() {
        return PathBuf::from(format!("{}{}", drive, pathpart));
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h);
    }
    PathBuf::from(".")
}

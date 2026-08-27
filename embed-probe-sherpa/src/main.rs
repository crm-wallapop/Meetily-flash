//! Reference (sherpa-onnx) path of the cosine-equivalence subprocess harness.
//!
//! WHY THIS EXISTS
//! ---------------
//! sherpa-onnx 1.13.x statically bundles ONNX Runtime 1.17.1 (C-API ≤17), while
//! the project's `ort = 2.0.0-rc.10` crate ships a much newer ORT (C-API 27).
//! Loading both into one process triggers a STATUS_ACCESS_VIOLATION on Windows.
//! The `meetily-flash` crate at the time linked BOTH (sherpa for diarization,
//! ort for its ONNX models), so it could not be the parent process. This crate — and its sibling
//! `embed-probe-ort` — are standalone binaries with NO dependency on `app_lib`,
//! so each loads exactly one ORT. A third process (shell script / Python) calls
//! both binaries, then runs `compare_embeddings.py` on their JSON outputs.
//!
//! CLI CONTRACT (shared verbatim with embed-probe-ort)
//! ---------------------------------------------------
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
//!             `skipped: true` means the clip was too short/silent for sherpa
//!             (its internal framing produced 0 frames, i.e. is_ready()=false).
//!
//!   env     = EMBED_PROBE_MODEL (optional). Defaults to
//!             ~/.meetily-models/nemo-titanet-embedding.onnx.
//!
//! Extraction pattern copied verbatim from sherpa_adapter.rs:304 `extract_embedding`
//! and the nemo_titanet_ort_cosine_equivalence test's sherpa branch (~line 1920):
//!   SpeakerEmbeddingExtractor::create(&cfg)
//!   create_stream() -> accept_waveform(16000, samples) -> is_ready() -> compute()

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use std::path::{Path, PathBuf};

/// Manifest entry. The clip window is taken from the audio file's
/// `[start_seconds, end_seconds)` range (sample-rate-independent). The legacy
/// `start_sample`/`end_sample` keys (@16kHz) are parsed only to fall back when
/// the seconds keys are absent.
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
        .ok_or_else(|| anyhow!("usage: embed-probe-sherpa <manifest.json>"))?;
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
    // Same default as frontend/src-tauri/src/audio/speaker/model_download.rs
    // (embedding_filename() returns "nemo-titanet-embedding.onnx").
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

    // ---- Build sherpa extractor (bundled ORT 1.17.1) ----
    // Mirrors sherpa_adapter.rs:29-37 and the test's sherpa_cfg (~line 1921).
    let sherpa_cfg = SpeakerEmbeddingExtractorConfig {
        model: Some(
            model_path
                .to_str()
                .ok_or_else(|| anyhow!("model path not utf8: {}", model_path.display()))?
                .to_string(),
        ),
        num_threads: 1,
        debug: false,
        provider: Some("cpu".to_string()),
    };
    let extractor = SpeakerEmbeddingExtractor::create(&sherpa_cfg)
        .ok_or_else(|| anyhow!("sherpa SpeakerEmbeddingExtractor::create failed"))?;
    let emb_dim = extractor.dim() as usize;
    eprintln!(
        "embed-probe-sherpa: extractor ready (dim={}, model={})",
        emb_dim,
        model_path.display()
    );

    // ---- Per-clip decode + extract ----
    // Decode caching: the manifest may list the same audio file many times
    // (10 clips from one meeting). Cache decoded samples per-path to avoid
    // re-decoding. Keyed by canonicalized path string. Stores NATIVE-rate mono
    // samples; each clip slices by seconds × native_rate then resamples to 16k.
    let mut decode_cache: std::collections::HashMap<String, (u32, Vec<f32>)> =
        std::collections::HashMap::new();

    let mut results = Vec::with_capacity(clips.len());
    for clip in &clips {
        // ---- Decode (cached) at native sample rate ----
        let cache_key = clip
            .path
            .to_string_lossy()
            .into_owned();
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
                "embed-probe-sherpa: [{}] empty range [{:.3}s..{:.3}s) @{}Hz → [{}..{}) — skipping",
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
        // ort paths see byte-identical input (sherpa's internal resampler is
        // removed as a variable — see the gate's design doc).
        let clip_samples: Vec<f32> = resample_to_16k(native_clip, native_sr)
            .with_context(|| format!("resample [{}] {}Hz→16k", clip.id, native_sr))?;
        let clip_samples_ref: &[f32] = &clip_samples;

        // ---- Extract via sherpa (now fed 16000Hz mono, matching the ort path) ----
        // Mirrors sherpa_adapter.rs:304 extract_embedding MINUS the project's
        // is_effectively_silent energy gate, so silence clips still flow through
        // sherpa's raw pipeline (consistent with the ort path). The is_ready()
        // gate below is sherpa's own framing gate (>=1 mel frame required).
        let stream = extractor.create_stream();
        let stream = match stream {
            Some(st) => st,
            None => {
                eprintln!(
                    "embed-probe-sherpa: [{}] create_stream() returned None — skipping",
                    clip.id
                );
                results.push(json!({
                    "id": clip.id,
                    "embedding": [],
                    "skipped": true,
                }));
                continue;
            }
        };
        stream.accept_waveform(16000, clip_samples_ref);
        if !extractor.is_ready(&stream) {
            // Too short for sherpa to produce a frame (nemo window_size=400 samp).
            eprintln!(
                "embed-probe-sherpa: [{}] is_ready()=false ({} samples < 1 frame) — skipping",
                clip.id,
                clip_samples_ref.len()
            );
            results.push(json!({
                "id": clip.id,
                "embedding": [],
                "skipped": true,
            }));
            continue;
        }
        let emb = match extractor.compute(&stream) {
            Some(e) => e,
            None => {
                eprintln!(
                    "embed-probe-sherpa: [{}] compute() returned None — skipping",
                    clip.id
                );
                results.push(json!({
                    "id": clip.id,
                    "embedding": [],
                    "skipped": true,
                }));
                continue;
            }
        };

        eprintln!(
            "embed-probe-sherpa: [{}] extracted {}-dim embedding ({} native samp @{}Hz → {} 16k samp)",
            clip.id,
            emb.len(),
            native_clip.len(),
            native_sr,
            clip_samples_ref.len()
        );
        results.push(json!({
            "id": clip.id,
            "embedding": emb,
            "skipped": false,
        }));
    }

    // ---- Single JSON blob to stdout ----
    let out = json!({ "results": results });
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

/// Decode any audio file symphonia supports to mono f32 samples at its NATIVE
/// sample rate (whatever the container reports — typically 48000Hz for m4a/mp4
/// meeting recordings). Does NOT resample; the caller slices by seconds × this
/// native rate and then hands the slice to `resample_to_16k`. The native rate is
/// returned alongside the samples.
///
/// Downmix mirrors audio/audio_processing.rs:516 audio_to_mono (mean of first
/// ≤2 channels — mic arrays use only the first 2 to avoid anti-phase cancellation).
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
        "embed-probe-sherpa: decoded {} at native {}Hz (slicing by seconds × {}, then resampling to 16kHz)",
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
                eprintln!("embed-probe-sherpa: packet read error in {}: {}", path.display(), e);
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
                            "embed-probe-sherpa: channel count corrected metadata={} actual={} (using actual)",
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
                eprintln!("embed-probe-sherpa: decode error in {}: {}", path.display(), e);
                continue;
            }
        }
    }

    if interleaved.is_empty() {
        return Err(anyhow!("no samples decoded from {}", path.display()));
    }

    // Downmix to mono (mean of first ≤2 channels).
    let mono = if channels > 1 {
        audio_to_mono(&interleaved, channels)
    } else {
        interleaved
    };

    // Clamp/normalize to [-1, 1] (mirrors decoder.rs normalize_audio_samples).
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

/// Mirrors audio/audio_processing.rs:516 audio_to_mono: for >2 channels use only
/// the first 2 (mic arrays carry anti-phase aux channels); mean the rest.
fn audio_to_mono(audio: &[f32], channels: u16) -> Vec<f32> {
    let effective = if channels > 2 { 2 } else { channels };
    let mut out = Vec::with_capacity(audio.len() / channels as usize);
    for chunk in audio.chunks(channels as usize) {
        let sum: f32 = chunk.iter().take(effective as usize).sum();
        out.push(sum / effective as f32);
    }
    out
}

/// Cross-platform home dir without pulling a dep. Honors $HOME (POSIX) and
/// %USERPROFILE% / %HOMEDRIVE%%HOMEPATH% (Windows). Falls back to argv if unset.
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


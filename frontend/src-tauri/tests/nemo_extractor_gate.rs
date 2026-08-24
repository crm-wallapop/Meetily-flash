//! nemo_titanet port fidelity gate (tasks 1.1 + 1.2) — in-process, now that
//! sherpa-onnx is removed from `meetily-flash` (one ORT runtime).
//!
//! The sherpa REFERENCE embeddings cannot be produced in-process (the two ORT
//! runtimes coexisted in one process only before Part B's removal — and crashed).
//! They were captured out-of-process via the `embed-probe-sherpa` binary and are
//! COMMITTED as a fixture: `tests/fixtures/nemo_c5_reference_embeddings.json`
//! (both the original 10-clip set and the C5 extension set, with per-clip
//! windows and sherpa's skip decisions).
//!
//! GATE (margin-derived tiered threshold — panel verdict 2026-07-27):
//!   - cosine ≥ 0.99 for clips ≥ 2.0s
//!   - cosine ≥ 0.98 for clips in [1.5, 2.0)s
//!   - floors derived from the AHC separation margin (merge 0.40;
//!     inter-speaker cosine 0.6–0.8; residual worst-case 0.0131 is ~46× below
//!     the 0.60 floor). Revisable ONLY if the margin changes — never in
//!     response to a failing measurement.
//!   - clips < 1.5s and silence-window clips are NOT gated on cosine (they are
//!     dropped before clustering: MIN_SPEECH_SECS / is_effectively_silent) —
//!     they ARE gated on filter parity (the port drops what sherpa+Meetily
//!     gates dropped).
//!   - per-clip cosine is REPORTED (visible with --nocapture) so drift in any
//!     regime is observable, not just pass/fail.
//!
//! Run:
//!   cargo test --release --test nemo_extractor_gate -- --ignored --nocapture

#![cfg(test)]

use app_lib::audio::speaker::nemo_extractor::NemoEmbeddingExtractor;

// ============================================================================
// EXACT reference input pipeline — copied verbatim from embed-probe-sherpa
// (decode_mono_native + resample_to_16k). The committed fixture embeddings
// were produced through THIS pipeline; the gate must feed the port
// byte-identical 16kHz mono input or boundary/normalize differences would
// pollute the cosine measurement.
// ============================================================================

fn decode_mono_native(path: &std::path::Path) -> anyhow::Result<(u32, Vec<f32>)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| anyhow::anyhow!("no audio track"))?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.ok_or_else(|| anyhow::anyhow!("unknown sample rate"))?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| anyhow::anyhow!("make decoder: {}", e))?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut channels: u16 = track.codec_params.channels.map(|c| c.count() as u16).unwrap_or(1);
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        };
        if packet.track_id() != track_id {
            continue;
        }
        if let Ok(decoded) = decoder.decode(&packet) {
            if sample_buf.is_none() {
                let spec = *decoded.spec();
                channels = spec.channels.count() as u16;
                let duration = decoded.capacity() as u64;
                sample_buf = Some(SampleBuffer::<f32>::new(duration, spec));
            }
            if let Some(ref mut buf) = sample_buf {
                buf.copy_interleaved_ref(decoded);
                interleaved.extend_from_slice(buf.samples());
            }
        }
    }

    if interleaved.is_empty() {
        return Err(anyhow::anyhow!("no samples decoded"));
    }

    // Downmix to mono (mean of first ≤2 channels) — probe parity.
    let mono = if channels > 1 {
        let ch = channels as usize;
        interleaved
            .chunks(ch)
            .map(|frame| {
                let n = ch.min(2);
                frame[..n].iter().sum::<f32>() / n as f32
            })
            .collect()
    } else {
        interleaved
    };

    // Normalize only if samples exceed [-1, 1] — probe parity.
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

fn resample_to_16k(input: &[f32], from_sample_rate: u32) -> anyhow::Result<Vec<f32>> {
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
        .map_err(|e| anyhow::anyhow!("rubato: {}", e))?;
    let waves_in = vec![input.to_vec()];
    let waves_out = resampler
        .process(&waves_in, None)
        .map_err(|e| anyhow::anyhow!("rubato process: {}", e))?;
    Ok(waves_out[0].clone())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na <= f32::EPSILON || nb <= f32::EPSILON {
        0.0
    } else {
        dot / (na * nb)
    }
}

#[derive(serde::Deserialize)]
struct FixtureClip {
    id: String,
    #[serde(default)]
    embedding: Vec<f32>,
    #[serde(default)]
    skipped: bool,
    start_seconds: f64,
    end_seconds: f64,
    #[serde(default = "default_true")]
    speech_region: bool,
}

fn default_true() -> bool {
    true
}

#[derive(serde::Deserialize)]
struct Fixture {
    #[serde(rename = "audio_meeting_id")]
    #[allow(dead_code)]
    audio_meeting_id: String,
    sets: std::collections::BTreeMap<String, Vec<FixtureClip>>,
}

/// Resolve the cde5c264 audio file path from the local DB (read-only).
async fn resolve_audio() -> Option<std::path::PathBuf> {
    let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
    if !std::path::Path::new(db_path).exists() {
        return None;
    }
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
        .await
        .ok()?;
    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind("meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323")
        .fetch_optional(&pool)
        .await
        .ok()??;
    let folder: Option<String> = sqlx::Row::get(&row, "folder_path");
    drop(pool);
    let folder = folder?;
    let dir = std::path::PathBuf::from(folder);
    ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.exists())
}

#[tokio::test]
#[ignore] // requires the on-disk cde5c264 recording + model (real-audio harness)
async fn nemo_extractor_cosine_gate() {
    let fixture_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/nemo_c5_reference_embeddings.json");
    let fixture: Fixture =
        serde_json::from_str(&std::fs::read_to_string(&fixture_path).expect("fixture"))
            .expect("fixture json");

    let models = dirs::home_dir()
        .expect("home")
        .join(".meetily-models")
        .join(app_lib::audio::speaker::model_download::embedding_filename());
    assert!(models.exists(), "nemo model missing at {}", models.display());

    let audio = resolve_audio().await;
    if audio.is_none() {
        eprintln!("GATE: cde5c264 recording unavailable — skipping (#[ignore] semantics)");
        return;
    }
    let audio = audio.unwrap();
    eprintln!("GATE: audio = {}", audio.display());

    let extractor =
        NemoEmbeddingExtractor::new(models.to_str().unwrap()).expect("build extractor");

    // EXACT reference input pipeline: decode native ONCE, then per clip
    // slice-at-native + SincFixedIn resample (byte-identical to what
    // embed-probe-sherpa fed the reference).
    let (native_sr, native_samples) = decode_mono_native(&audio).expect("decode native");
    eprintln!("GATE: decoded {} samples @ {}Hz", native_samples.len(), native_sr);

    let mut failures: Vec<String> = Vec::new();
    let mut report = String::from("clip                      dur(s)  cosine   tier      verdict\n");
    report.push_str(&"-".repeat(72));
    report.push('\n');

    for (set_name, clips) in &fixture.sets {
        for clip in clips {
            let dur = clip.end_seconds - clip.start_seconds;
            let id = format!("{}/{}", set_name, clip.id);

            // Reference pipeline: slice at native rate, then resample slice.
            let s = ((clip.start_seconds * native_sr as f64).round() as usize).min(native_samples.len());
            let e = ((clip.end_seconds * native_sr as f64).round() as usize).min(native_samples.len());
            let clip_16k: Vec<f32> = if e <= s {
                Vec::new()
            } else {
                resample_to_16k(&native_samples[s..e], native_sr).expect("resample")
            };
            let port_emb = extractor.extract_embedding(&clip_16k, 16000);

            // FILTER CLASSIFICATION (task 1.2): clips drawn from transcript
            // gaps (speech_region:false) are structurally never chunk inputs
            // (build_chunks consumes only Whisper speech regions) — recorded,
            // not cosine-gated. short-1/2 (<1.5s) likewise: MIN_SPEECH_SECS drops.
            // FILTER PARITY (strict): sherpa skipped == port must skip.
            if clip.skipped {
                let ok = port_emb.is_none();
                report.push_str(&format!(
                    "{:<26}{:>5.2}   skipped  parity    {}\n",
                    id,
                    dur,
                    if ok { "OK" } else { "FAIL(port embedded a dropped clip)" }
                ));
                if !ok {
                    failures.push(format!("{}: sherpa skipped, port embedded", id));
                }
                continue;
            }

            let (Some(port), ref_ref) = (port_emb.as_deref(), clip.embedding.as_slice()) else {
                failures.push(format!("{}: port returned None, sherpa embedded", id));
                report.push_str(&format!(
                    "{:<26}{:>5.2}   embedded FAIL(port None)\n",
                    id, dur
                ));
                continue;
            };
            if ref_ref.is_empty() {
                failures.push(format!("{}: fixture embedding empty but not skipped", id));
                continue;
            }

            let cos = cosine(port, ref_ref);
            // Margin-derived tiered threshold (panel verdict 2026-07-27),
            // applied to clips that structurally reach clustering:
            // speech-region AND >=1.5s. Gap-window clips (silence-1/2) are
            // filter-classified: never chunk inputs (not Whisper segments).
            let (floor, tier) = if !clip.speech_region {
                (None, "gap-window(not a chunk input)")
            } else if dur < 1.5 {
                (None, "no-cosine(dropped: <MIN_SPEECH_SECS)")
            } else if dur < 2.0 {
                (Some(0.98), "[1.5,2.0)s>=0.98")
            } else {
                (Some(0.99), ">=2.0s>=0.99")
            };
            match floor {
                None => report.push_str(&format!(
                    "{:<26}{:>5.2}   {:+.4}  {}\n",
                    id, dur, cos, tier
                )),
                Some(f) => {
                    let ok = cos >= f;
                    report.push_str(&format!(
                        "{:<26}{:>5.2}   {:+.4}  {:<16}{}\n",
                        id,
                        dur,
                        cos,
                        tier,
                        if ok { "PASS" } else { "FAIL" }
                    ));
                    if !ok {
                        failures.push(format!("{}: cosine {:.4} < {:.2}", id, cos, f));
                    }
                }
            }
        }
    }

    eprintln!("\n{}", report);
    let out = std::env::temp_dir().join("nemo_extractor_gate_report.txt");
    std::fs::write(&out, &report).expect("write report");
    eprintln!("GATE: report at {}", out.display());

    assert!(
        failures.is_empty(),
        "nemo extractor gate FAILED ({}):\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
    eprintln!("GATE: PASS — margin-derived tiered threshold met on all clips");
}

/// Task 2.6 — cde5c264 boundary oracle THROUGH THE PRODUCTION MODULE.
///
/// Runs `PyannoteSegmentation::boundary_segments` (the exact code path the
/// diarization command uses) over two known-turn windows and asserts SPECIFIC
/// boundaries exist that the chunk-grid baseline cannot produce:
///   (a) the banter window 5.7–32.5s — chunk grid yields ONE boundary at
///       21.36s; pyannote+smoothing yields ~24 turns;
///   (b) the Ricardo interjection at ≈46:58 — collapsed to one run today.
/// Also asserts cap-shedding leaves the count ≤ MAX_DIARIZATION_CHUNKS.
///
/// Run:
///   cargo test --release --test nemo_extractor_gate -- --ignored --nocapture pyannote_boundary_oracle_cde5c264
#[tokio::test]
#[ignore]
async fn pyannote_boundary_oracle_cde5c264() {
    use app_lib::audio::speaker::pyannote_segmentation::PyannoteSegmentation;

    let models = dirs::home_dir()
        .expect("home")
        .join(".meetily-models")
        .join("pyannote-segmentation.onnx");
    assert!(models.exists(), "pyannote model missing");

    let audio = resolve_audio().await.expect("cde5c264 recording");
    let (native_sr, native_samples) = decode_mono_native(&audio).expect("decode");
    // Production feeds to_whisper_format output (16kHz mono whole-file resample).
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio).expect("decode prod");
    let samples = decoded.to_whisper_format();
    let duration = samples.len() as f64 / 16000.0;
    eprintln!(
        "ORACLE: {} samples ({:.1}s @16k); native {}Hz",
        samples.len(), duration, native_sr
    );
    drop(native_samples);

    let seg = PyannoteSegmentation::new(models.to_str().unwrap()).expect("pyannote session");
    let t0 = std::time::Instant::now();
    let cps = seg.change_points(&samples).expect("change_points");
    eprintln!(
        "ORACLE: {} smoothed change-points in {:.1}s",
        cps.len(),
        t0.elapsed().as_secs_f64()
    );

    // Whisper-style speech regions for the two windows under test: synthesize
    // one region spanning each window (the oracle isolates BOUNDARY PLACEMENT,
    // not VAD behavior — the intersect seam is unit-tested separately).
    let windows: &[(f64, f64, &str)] = &[
        (5.7, 32.5, "banter (rapid multi-turn)"),
        (46.0 * 60.0 + 42.0, 47.0 * 60.0 + 2.0, "Ricardo interjection ≈46:58"),
    ];
    let mut report = String::new();
    let mut failed = Vec::new();

    for &(ws, we, label) in windows {
        let in_window: Vec<f64> = cps
            .iter()
            .filter(|&&t| t >= ws && t <= we)
            .copied()
            .collect();
        eprintln!(
            "\nORACLE {}: {} pyannote change-points (chunk-grid baseline: 1)",
            label,
            in_window.len()
        );
        report.push_str(&format!(
            "## {} [{:.1}-{:.1}s]: {} change-points\n",
            label, ws, we,
            in_window.len()
        ));
        for &t in &in_window {
            report.push_str(&format!("- {:.3}s\n", t));
        }

        // Chunk-grid baseline produces exactly ONE boundary per window (the
        // uniform grid changes label once). The oracle requires strictly more.
        if in_window.len() <= 1 {
            failed.push(format!("{}: only {} boundaries (need >1)", label, in_window.len()));
        }
        // Known-anchor hit: Ricardo join/interjection timestamps must have a
        // boundary within ±2s.
        let anchor = if label.contains("join") { 17.0 * 60.0 + 37.0 } else { 46.0 * 60.0 + 58.0 };
        let _ = anchor; // anchors checked via the window presence below
    }

    // Anchor precision: the interjection at 2818s ±2s must have ≥1 boundary.
    let anchor_hits = cps.iter().filter(|&&t| (t - 2818.0).abs() <= 2.0).count();
    eprintln!("ORACLE: interjection anchor 2818s±2s hits: {}", anchor_hits);
    report.push_str(&format!("\nanchor 2818s±2s: {} hits\n", anchor_hits));
    if anchor_hits == 0 {
        failed.push("interjection anchor 2818s has no boundary within ±2s".into());
    }
    // Ricardo join anchor 1057s ±2s.
    let join_hits = cps.iter().filter(|&&t| (t - 1057.0).abs() <= 2.0).count();
    eprintln!("ORACLE: Ricardo-join anchor 1057s±2s hits: {}", join_hits);
    if join_hits == 0 {
        failed.push("Ricardo join anchor 1057s has no boundary within ±2s".into());
    }

    // Cap shedding sanity through the public API.
    let whisper_regions: Vec<(f64, f64)> = vec![(5.7, 32.5), (1057.0, 1087.0), (2810.0, 2830.0)];
    let bounded = seg
        .boundary_segments(&samples, &whisper_regions, app_lib::audio::speaker::sherpa_adapter::max_diarization_chunks())
        .expect("boundary_segments");
    eprintln!("ORACLE: boundary_segments → {} segments (cap 600)", bounded.len());
    assert!(
        bounded.len() <= app_lib::audio::speaker::sherpa_adapter::max_diarization_chunks(),
        "cap exceeded"
    );

    let out = std::env::temp_dir().join("pyannote_boundary_oracle_report.txt");
    std::fs::write(&out, &report).expect("write report");
    eprintln!("ORACLE: report at {}", out.display());

    assert!(
        failed.is_empty(),
        "boundary oracle FAILED:\n  {}",
        failed.join("\n  ")
    );
    eprintln!("ORACLE: PASS — pyannote boundaries resolve turns the chunk grid collapses");
}

//! Empirical probe: can the project's `ort` 2.0.0-rc.10 dep load and run
//! `pyannote-segmentation-3.0.onnx` that sherpa-onnx 1.13.x CANNOT (its
//! bundled ORT 1.17.1 STATUS_ACCESS_VIOLATIONs on this model)?
//!
//! WHY a separate path from sherpa-onnx: sherpa-onnx Rust 1.13.x statically
//! bundles ORT 1.17.1 (C-API ≤17); pyannote-segmentation-3.0 requires C-API
//! 24-27. The project's own `ort = 2.0.0-rc.10` (used for Parakeet) ships a
//! much newer ORT. If this probe succeeds, Part B (finer speaker boundaries)
//! becomes feasible via a hand-rolled pyannote pipeline on `ort`, sidestepping
//! sherpa-onnx's stale ORT entirely. See memory
//! `project_sherpa_onnx_ort_pyannote_block.md`.
//!
//! I/O contract (from sherpa-onnx `offline-speaker-segmentation-pyannote-model.h`
//! and pengzhendong/pyannote-onnx):
//!   input  "input"   : float32[1, 1, 160000]  (batch, channel=mono, samples @ 16kHz)
//!   output "output"  : float32[1, num_frames, 7]  (per-frame powerset-class logits)
//!   num_frames ≈ (160000 - 721) / 270 + 1 = 591  (read dynamically — do NOT hardcode)
//!   7 classes encode the powerset of up to 3 speakers:
//!     0=no speech, 1=spk1, 2=spk2, 3=spk3, 4=spk1+2, 5=spk1+3, 6=spk2+3
//!   receptive field: size=721 samples (~45ms), shift=270 samples (~16.9ms)
//!
//! Run:
//!   cargo test --test pyannote_ort_probe -- --ignored --nocapture

#![cfg(test)]

use ndarray::{Array1, Array3, Axis};
use ort::execution_providers::CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;

#[tokio::test]
#[ignore]
async fn pyannote_loads_and_runs_via_ort_rc10() {
    let _ = env_logger::builder().is_test(true).try_init();

    let model_path = dirs::home_dir()
        .expect("home dir")
        .join(".meetily-models")
        .join("pyannote-segmentation.onnx");
    assert!(
        model_path.exists(),
        "pyannote model missing at {}",
        model_path.display()
    );

    // Mirror parakeet_engine/model.rs:91,114-125 session config.
    // ort 2.0.0-rc.10 uses commit_from_file (not with_model_from_file).
    let providers = vec![CPUExecutionProvider::default().build()];
    let session_result = Session::builder()
        .expect("builder")
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .expect("opt level")
        .with_execution_providers(providers)
        .expect("providers")
        .with_intra_threads(1)
        .expect("intra threads")
        .commit_from_file(&model_path);

    let session = match session_result {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "PROBE FAIL: ort 2.0.0-rc.10 cannot load pyannote-segmentation-3.0: {:?}",
                e
            );
            panic!("ORT load failed — Part B block not liftable via project's ort dep");
        }
    };

    eprintln!("PROBE OK: pyannote loaded via ort 2.0.0-rc.10");
    eprintln!("  inputs:");
    for (i, inp) in session.inputs.iter().enumerate() {
        eprintln!("    [{}] {} = {:?}", i, inp.name, inp.input_type);
    }
    eprintln!("  outputs:");
    for (i, out) in session.outputs.iter().enumerate() {
        eprintln!("    [{}] {} = {:?}", i, out.name, out.output_type);
    }

    // Real 10s @ 16kHz synthetic forward pass.
    // WHY a real forward, not just load: the sherpa-onnx crash happened during
    // create() which includes a graph-init forward. Load-success alone is
    // insufficient evidence — verify inference also doesn't crash.
    //
    // Input contract: float32[1, 1, 160000] (batch, channel=mono, samples).
    // Shape verified against sherpa `offline-speaker-diarization-pyannote-impl.h:346-350`
    // and pengzhendong/pyannote-onnx (segmentation-3.0: duration=10s).
    const SAMPLE_RATE: usize = 16000;
    const WINDOW_SECONDS: usize = 10;
    const WINDOW_SAMPLES: usize = SAMPLE_RATE * WINDOW_SECONDS; // 160000

    let waveform: Array1<f32> =
        Array1::from_shape_fn(WINDOW_SAMPLES, |i| (i as f32 * 2.0 * 3.14159 / 440.0).sin());
    // [1,1,160000]: batch=1, channel=1 (mono), samples
    let input_view = waveform
        .view()
        .insert_axis(Axis(0))
        .insert_axis(Axis(0));
    let input_3d: Array3<f32> = input_view.into_owned();

    eprintln!(
        "PROBE: running {}s synthetic forward ({} samples, input shape {:?})",
        WINDOW_SECONDS, WINDOW_SAMPLES, input_3d.shape()
    );

    let input_name = session.inputs[0].name.to_string();
    let output_name = session.outputs[0].name.to_string();
    let tensor_ref = TensorRef::from_array_view(input_3d.view()).expect("tensor ref");
    let inputs = ort::inputs![input_name.as_str() => tensor_ref];
    // ort 2.0.0-rc.10 Session::run takes SessionInputs by value and needs &mut self.
    let mut session = session;
    let outputs = match session.run(inputs) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("PROBE FAIL: forward pass crashed: {:?}", e);
            panic!("ORT forward failed");
        }
    };

    eprintln!("PROBE OK: forward pass succeeded, {} outputs", outputs.len());
    let output = outputs
        .get(output_name.as_str())
        .unwrap_or_else(|| panic!("output '{}' not found", output_name));
    let output_array = output
        .try_extract_array::<f32>()
        .expect("extract output array");
    eprintln!(
        "  output '{}' shape: {:?} (expected ~[1, 591, 7])",
        output_name,
        output_array.shape()
    );

    // Sanity-check the powerset dimension is 7 classes (3-speaker powerset).
    let shape = output_array.shape();
    assert!(
        shape.len() == 3,
        "expected 3-D output [batch, frames, classes], got {}-D",
        shape.len()
    );
    assert_eq!(
        shape[0], 1,
        "expected batch=1, got {}",
        shape[0]
    );
    assert_eq!(
        shape[2], 7,
        "expected 7 powerset classes (3-speaker powerset), got {} — \
         if this is wrong, the model may be a different pyannote version",
        shape[2]
    );
    let num_frames = shape[1];
    // Receptive-field formula: (160000 - 721) / 270 + 1 = 591. Allow slack for
    // padding-mode variance; the sherpa file should produce ~591.
    let expected_frames = (WINDOW_SAMPLES as i64 - 721) / 270 + 1;
    assert!(
        (num_frames as i64 - expected_frames).abs() <= 5,
        "num_frames {} far from expected {} — receptive-field formula mismatch",
        num_frames, expected_frames
    );

    eprintln!(
        "PROBE PASS: pyannote-segmentation-3.0 loads and runs via ort 2.0.0-rc.10. \
         Output: {} frames @ ~16.9ms resolution = {:.1}s of segmentable audio per window. \
         Part B block is LIFTABLE via unblock lever (b).",
        num_frames,
        num_frames as f32 * 0.0169
    );
}

// ============================================================================
// PHASE 2: cde5c264 threshold-sweep prototype (empirical boundary data).
//
// WHY this exists: the Part B shark-tank concluded the design cannot converge
// to a sound D1 without empirical data on which onset threshold resolves turns
// on real audio. This test gathers that data: runs pyannote segmentation via
// `ort` over the real cde5c264 recording at three onset thresholds, computes
// change-points, and dumps boundaries near three known windows for inspection.
//
// Known turns (ground truth from prior oracle work):
//   - Banter 5.7–32.5s: rapid multi-turn dialogue (currently 1 boundary at 21.36s)
//   - Ricardo join 17:37 (1057s): a new speaker enters
//   - Ricardo interjection 46:58 (2818s): brief insertion in Cynthia's run
//
// NO ASSERTIONS — this is data-gathering, not a pass/fail test. Output goes to
// eprintln (visible with --nocapture) and to temp-dir files for inspection.
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_threshold_sweep
// ============================================================================

/// Powerset class → 3-speaker multilabel (per pyannote-audio/utils/powerset.py).
/// Index 0 = no speech; 1-3 = single speakers; 4-6 = overlap pairs.
fn powerset_to_multilabel(class: usize) -> [bool; 3] {
    match class {
        0 => [false, false, false],
        1 => [true, false, false],
        2 => [false, true, false],
        3 => [false, false, true],
        4 => [true, true, false],
        5 => [true, false, true],
        6 => [false, true, true],
        _ => [false, false, false],
    }
}

/// Decode per-frame powerset logits → binary speaker activity via hysteresis.
/// Returns Vec<[bool; 3]> (one per frame) using pyannote's onset/offset convention.
fn decode_multilabel_with_hysteresis(
    logits: &[f32], // shape [num_frames, 7], row-major
    num_frames: usize,
    onset: f32,
    offset: f32,
) -> Vec<[bool; 3]> {
    let num_classes = 7;
    let mut active = [false; 3];
    let mut out = Vec::with_capacity(num_frames);

    for frame in 0..num_frames {
        let row = &logits[frame * num_classes..(frame + 1) * num_classes];
        // Expand powerset → 3 independent speaker probabilities (sum overlap
        // classes into their constituent speakers, per pengzhendong/pyannote-onnx).
        let mut probs = [0.0f32; 3];
        for (class, &p) in row.iter().enumerate() {
            let ml = powerset_to_multilabel(class);
            for spk in 0..3 {
                if ml[spk] {
                    probs[spk] += p.exp(); // pengzhendong uses plain exp, not softmax
                }
            }
        }
        // Hysteresis: turn on above onset, turn off below offset.
        for spk in 0..3 {
            if active[spk] {
                if probs[spk] < offset {
                    active[spk] = false;
                }
            } else if probs[spk] > onset {
                active[spk] = true;
            }
        }
        out.push(active);
    }
    out
}

/// Compute change-points: frames where the active-speaker set changes.
/// Returns timestamps in seconds.
fn change_points(activity: &[[bool; 3]], frame_shift_secs: f64, receptive_offset_secs: f64) -> Vec<f64> {
    let mut points = Vec::new();
    for i in 1..activity.len() {
        if activity[i] != activity[i - 1] {
            let t = receptive_offset_secs + (i as f64) * frame_shift_secs;
            points.push(t);
        }
    }
    points
}

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_threshold_sweep() {
    let _ = env_logger::builder().is_test(true).try_init();

    // --- Load cde5c264 audio (mirrors test_cde5c264_two_pass_oracle pattern) ---
    let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
        .await
        .expect("DB connect (read-only)");
    let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(&pool)
        .await
        .expect("fetch meeting");
    let folder = row
        .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
        .expect("cde5c264 folder_path missing");
    drop(pool); // release DB connection early

    // find_audio_in_folder is in speaker::commands (test module); replicate the lookup.
    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| audio_dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    eprintln!("SWEEP: audio at {}", audio_path.display());

    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path)
        .expect("decode audio");
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds.max(0.001);
    eprintln!("SWEEP: {} samples ({:.1}s @ 16kHz mono)", samples.len(), audio_duration);

    // --- Load pyannote via ort (same as Phase 1 probe) ---
    let model_path = dirs::home_dir()
        .expect("home dir")
        .join(".meetily-models")
        .join("pyannote-segmentation.onnx");
    assert!(model_path.exists(), "pyannote model missing");

    let providers = vec![CPUExecutionProvider::default().build()];
    let session = Session::builder()
        .expect("builder")
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .expect("opt level")
        .with_execution_providers(providers)
        .expect("providers")
        .with_intra_threads(1)
        .expect("intra threads")
        .commit_from_file(&model_path)
        .expect("load pyannote");

    let input_name = session.inputs[0].name.to_string();
    let output_name = session.outputs[0].name.to_string();
    // ort 2.0.0-rc.10 Session::run takes SessionInputs by value and needs &mut self.
    let mut session = session;

    // --- Sliding window segmentation ---
    // sherpa default: window_shift_ratio=0.1 → step=1s (90% overlap).
    // WHY heavy overlap: pyannote's receptive field is window-local; without
    // overlap, a turn at a window seam can be missed or doubled. sherpa's 0.1
    // ratio is the documented default for quality.
    const SAMPLE_RATE: usize = 16000;
    const WINDOW_SAMPLES: usize = 160000; // 10s
    const STEP_SAMPLES: usize = 16000; // 1s (window_shift_ratio = 0.1)
    const FRAME_SHIFT_SECS: f64 = 270.0 / 16000.0; // ~16.9ms
    const RECEPTIVE_OFFSET_SECS: f64 = 721.0 / 16000.0; // ~45ms (first valid frame)

    // RESTRICT TO REGIONS OF INTEREST — full 83-min recording × 4963 windows
    // × ~270ms inference per window = 22 min per threshold. The goal is
    // boundary-quality assessment near known turns, so process only windows
    // overlapping those regions (~90 windows total, ~25s of inference).
    // Each region gets a ±WINDOW buffer so a turn at the seam is still caught.
    let known_windows: &[(f64, f64, &str)] = &[
        (5.7, 32.5, "banter (rapid multi-turn)"),
        (17.0 * 60.0 + 37.0, 18.0 * 60.0 + 7.0, "Ricardo join 17:37"),
        (46.0 * 60.0 + 50.0, 47.0 * 60.0 + 10.0, "Ricardo interjection 46:58"),
    ];
    const ROI_BUFFER_SECS: f64 = 12.0; // > WINDOW so seam-turns are caught

    let in_roi = |win_start_secs: f64| -> bool {
        let win_end_secs = win_start_secs + (WINDOW_SAMPLES as f64 / SAMPLE_RATE as f64);
        known_windows.iter().any(|&(ws, we, _)| {
            win_end_secs >= (ws - ROI_BUFFER_SECS) && win_start_secs <= (we + ROI_BUFFER_SECS)
        })
    };

    let total_windows_full = if samples.len() > WINDOW_SAMPLES {
        (samples.len() - WINDOW_SAMPLES) / STEP_SAMPLES + 1
    } else {
        1
    };

    // CACHE LOGITS — model output is threshold-independent. Decode once per
    // window, apply all thresholds to the same data. Each window → (start_secs,
    // logits). Stored as Vec<(f64, Vec<f32>)>.
    let t0 = std::time::Instant::now();
    let mut cached: Vec<(f64, Vec<f32>)> = Vec::new();
    let mut windows_processed = 0usize;
    for win_idx in 0..total_windows_full {
        let start = win_idx * STEP_SAMPLES;
        let win_start_secs = start as f64 / SAMPLE_RATE as f64;
        if !in_roi(win_start_secs) {
            continue;
        }
        let end = (start + WINDOW_SAMPLES).min(samples.len());
        let win_len = end - start;
        if win_len < 16000 {
            break;
        }
        let mut window = vec![0.0f32; WINDOW_SAMPLES];
        window[..win_len].copy_from_slice(&samples[start..end]);

        let input_3d: Array3<f32> = Array1::from(window)
            .into_shape_with_order([1, 1, WINDOW_SAMPLES])
            .unwrap();
        let tensor_ref = TensorRef::from_array_view(input_3d.view()).expect("tensor ref");
        let inputs = ort::inputs![input_name.as_str() => tensor_ref];
        let outputs = session.run(inputs).expect("forward");
        let output = outputs.get(output_name.as_str()).expect("output");
        let output_array = output.try_extract_array::<f32>().expect("extract");
        let shape = output_array.shape();
        let num_frames = shape[1];
        let num_classes = shape[2];
        let logits_slice = output_array
            .as_slice()
            .unwrap_or_else(|| output_array.to_slice().unwrap());
        cached.push((win_start_secs, logits_slice[..num_frames * num_classes].to_vec()));
        windows_processed += 1;
    }
    let inference_elapsed = t0.elapsed().as_secs_f64();
    eprintln!(
        "SWEEP: {} ROI windows inferred (of {} full) in {:.1}s — {:.0}ms/window",
        windows_processed, total_windows_full, inference_elapsed,
        if windows_processed > 0 { inference_elapsed * 1000.0 / windows_processed as f64 } else { 0.0 }
    );

    // For each threshold, decode the cached windows and compute change-points.
    let thresholds = [0.3f32, 0.5f32, 0.7f32];
    let mut all_change_points: Vec<Vec<f64>> = Vec::with_capacity(thresholds.len());

    for &onset in &thresholds {
        let offset = onset;
        let mut per_frame_activity: Vec<[bool; 3]> = Vec::new();
        for &(win_start_secs, ref logits) in &cached {
            let num_classes = 7;
            let num_frames = logits.len() / num_classes;
            let window_activity =
                decode_multilabel_with_hysteresis(logits, num_frames, onset, offset);
            for (i, &act) in window_activity.iter().enumerate() {
                let abs_secs = win_start_secs + RECEPTIVE_OFFSET_SECS + (i as f64) * FRAME_SHIFT_SECS;
                let frame_idx = (abs_secs / FRAME_SHIFT_SECS).round() as usize;
                while per_frame_activity.len() <= frame_idx {
                    per_frame_activity.push([false; 3]);
                }
                per_frame_activity[frame_idx] = act;
            }
        }
        eprintln!(
            "SWEEP: onset {:.1} — {} frames decoded (cached inference reused)",
            onset, per_frame_activity.len()
        );
        let cps = change_points(&per_frame_activity, FRAME_SHIFT_SECS, 0.0);
        all_change_points.push(cps);
    }

    let mut report = String::new();
    report.push_str(&format!(
        "# cde5c264 pyannote threshold sweep (ROI-only)\n# audio: {:.1}s, {} windows (of {} full), inference: {:.1}s\n\n",
        audio_duration, windows_processed, total_windows_full, inference_elapsed
    ));

    for (thresh_idx, &onset) in thresholds.iter().enumerate() {
        let cps = &all_change_points[thresh_idx];
        eprintln!("\n===== onset {:.1}: {} total change-points =====", onset, cps.len());
        report.push_str(&format!("\n## onset {:.1}: {} change-points\n\n", onset, cps.len()));

        for &(ws, we, label) in known_windows {
            let nearby: Vec<f64> = cps
                .iter()
                .filter(|&&t| t >= ws - 2.0 && t <= we + 2.0)
                .copied()
                .collect();
            eprintln!("  {} [{:.1}–{:.1}s]: {} boundaries", label, ws, we, nearby.len());
            for &t in &nearby {
                eprintln!("    {:.3}s", t);
            }
            report.push_str(&format!(
                "### {} [{:.1}–{:.1}s]: {} boundaries\n",
                label, ws, we, nearby.len()
            ));
            for &t in &nearby {
                report.push_str(&format!("- {:.3}s\n", t));
            }
            report.push('\n');
        }
    }

    let out = std::env::temp_dir().join("cde5c264_pyannote_threshold_sweep.txt");
    std::fs::write(&out, &report).expect("write report");
    eprintln!("\nSWEEP: full report at {}", out.display());
    eprintln!("SWEEP: DONE — inspect output for boundary density near known turns.");
}


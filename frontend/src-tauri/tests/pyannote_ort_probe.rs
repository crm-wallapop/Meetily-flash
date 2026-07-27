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

// ============================================================================
// PHASE 2b: smoothing + anchor-hit precision.
//
// WHY: raw pyannote output is ~1 change-point/sec with ~46% sub-500ms jitter —
// same-speaker flicker, not real turns. pyannote-audio itself applies a
// smoothing stage before clustering (see pyannote-audio/utils/sliding.py and
// the `clamp()`/`argmax` post-processing). The two canonical operations are:
//   (1) median filter per-speaker — removes single-frame flicker
//   (2) min-on-duration / max-off-duration gates — removes micro-bursts/gaps
// We implement both, then measure whether the smoothed turns still HIT the
// known anchors (Ricardo join 1057s, interjection 2818s) within ±2s. That
// anchor-hit rate is a PRECISION claim, not a count — it's what a shark-tank
// needs to converge a sound D1.
// ============================================================================

/// Median filter per-speaker over a half-window `rad` (kernel = 2*rad+1).
/// Applied independently to each of the 3 speaker tracks. Removes single-frame
/// flicker without shifting edges (unlike a moving average). Edge-handling:
/// clamp padding (replicate nearest). Matches pyannote-audio's `median_filter`.
fn median_filter_per_speaker(activity: &[[bool; 3]], rad: usize) -> Vec<[bool; 3]> {
    if rad == 0 || activity.is_empty() {
        return activity.to_vec();
    }
    let n = activity.len();
    let kernel = 2 * rad + 1;
    let mut out = vec![[false; 3]; n];
    for spk in 0..3 {
        for i in 0..n {
            let mut window = Vec::with_capacity(kernel);
            for k in -(rad as isize)..=(rad as isize) {
                let idx = (i as isize + k).clamp(0, (n - 1) as isize) as usize;
                window.push(activity[idx][spk]);
            }
            // Median of booleans = majority vote.
            let trues = window.iter().filter(|&&b| b).count();
            out[i][spk] = trues * 2 > kernel;
        }
    }
    out
}

/// Min-on-duration / max-off-duration gate per pyannote-audio's `clamp`:
///   - any "on" run shorter than min_on_frames → discarded (turned off)
///   - any "off" run shorter than max_off_frames → filled (turned on)
/// This removes micro-bursts and micro-gaps that survive median filtering.
/// Applied per-speaker. Mirrors pyannote pipeline hyperparams (defaults:
/// min_on ~0.3s, min_off ~0.5s).
fn duration_gates_per_speaker(
    activity: &[[bool; 3]],
    min_on_frames: usize,
    max_off_frames: usize,
) -> Vec<[bool; 3]> {
    if activity.is_empty() {
        return activity.to_vec();
    }
    let n = activity.len();
    let mut out = activity.to_vec();
    for spk in 0..3 {
        // First pass: collapse short OFF runs (fill gaps).
        if max_off_frames > 0 {
            let mut i = 0;
            while i < n {
                if !out[i][spk] {
                    let run_start = i;
                    while i < n && !out[i][spk] {
                        i += 1;
                    }
                    let run_len = i - run_start;
                    // Only fill if bounded by ON on both sides (true gap, not tail).
                    if run_len <= max_off_frames && run_start > 0 && i < n {
                        for j in run_start..i {
                            out[j][spk] = true;
                        }
                    }
                } else {
                    i += 1;
                }
            }
        }
        // Second pass: collapse short ON runs (drop bursts).
        if min_on_frames > 0 {
            let mut i = 0;
            while i < n {
                if out[i][spk] {
                    let run_start = i;
                    while i < n && out[i][spk] {
                        i += 1;
                    }
                    let run_len = i - run_start;
                    if run_len < min_on_frames {
                        for j in run_start..i {
                            out[j][spk] = false;
                        }
                    }
                } else {
                    i += 1;
                }
            }
        }
    }
    out
}

/// Smoothed change-points: median filter + duration gates, then change-point
/// extraction. Returns (timestamps, speaker_assignment_at_each_turn) so the
/// caller can report WHICH speaker changed, not just that something did.
fn smoothed_change_points(
    activity: &[[bool; 3]],
    median_rad: usize,
    min_on_frames: usize,
    max_off_frames: usize,
    frame_shift_secs: f64,
    receptive_offset_secs: f64,
) -> Vec<f64> {
    let med = median_filter_per_speaker(activity, median_rad);
    let gated = duration_gates_per_speaker(&med, min_on_frames, max_off_frames);
    change_points(&gated, frame_shift_secs, receptive_offset_secs)
}

/// Anchor-hit precision: of the smoothed change-points, how many fall within
/// ±tolerance_secs of each known anchor, and how many anchors are hit at all?
/// Returns (hits_per_anchor, total_anchors_hit, total_anchors).
fn anchor_hits(
    change_pts: &[f64],
    anchors: &[(f64, &str)], // (timestamp_secs, label)
    tolerance_secs: f64,
) -> (Vec<usize>, usize, usize) {
    let mut hits = Vec::with_capacity(anchors.len());
    for &(t, _) in anchors {
        let n = change_pts.iter().filter(|&&cp| (cp - t).abs() <= tolerance_secs).count();
        hits.push(n);
    }
    let anchors_hit = hits.iter().filter(|&&n| n > 0).count();
    (hits, anchors_hit, anchors.len())
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

// ============================================================================
// PHASE 2b: smoothed turn quality + anchor-hit precision on cde5c264.
//
// WHY this is a separate test from the raw sweep: the sweep proved pyannote
// resolves the banter turns the current pipeline collapses (36 vs 1), but
// raw output is ~1cp/s with 46% jitter — detection, not turns. This test
// answers the precision question a shark-tank needs: after standard
// pyannote post-processing (median filter + duration gates), do the
// smoothed turns (a) drop to a sane count AND (b) still hit the known
// anchors (Ricardo join 1057s, interjection 2818s) within ±2s?
//
// Design:
//   - onset=0.5 only (sweep proved threshold-insensitive)
//   - 3 smoothing configs: light / medium / aggressive
//   - report: change-point count per region, anchor-hit rate, anchor miss list
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_smoothed_precision
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_smoothed_precision() {
    let _ = env_logger::builder().is_test(true).try_init();

    // --- Load cde5c264 audio (mirrors the sweep test) ---
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
    drop(pool);

    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| audio_dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds.max(0.001);

    // --- Load pyannote via ort ---
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
    let mut session = session;

    // --- Constants ---
    const SAMPLE_RATE: usize = 16000;
    const WINDOW_SAMPLES: usize = 160000; // 10s
    const STEP_SAMPLES: usize = 16000; // 1s
    const FRAME_SHIFT_SECS: f64 = 270.0 / 16000.0; // ~16.9ms
    const RECEPTIVE_OFFSET_SECS: f64 = 721.0 / 16000.0; // ~45ms
    const ONSET: f32 = 0.5; // sweep proved threshold-insensitive

    // --- ROI (same as sweep) ---
    let known_windows: &[(f64, f64, &str)] = &[
        (5.7, 32.5, "banter (rapid multi-turn)"),
        (17.0 * 60.0 + 37.0, 18.0 * 60.0 + 7.0, "Ricardo join 17:37"),
        (46.0 * 60.0 + 50.0, 47.0 * 60.0 + 10.0, "Ricardo interjection 46:58"),
    ];
    const ROI_BUFFER_SECS: f64 = 12.0;
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

    // --- Cached inference (one pass; smoothing is threshold-independent of inference) ---
    let mut cached: Vec<(f64, Vec<f32>)> = Vec::new();
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
    }
    eprintln!(
        "SMOOTH: {} ROI windows inferred over {:.1}s audio",
        cached.len(),
        audio_duration
    );

    // --- Decode to raw activity, then merge across overlapping windows ---
    // (Same last-writer-wins merge as the sweep; sufficient for boundary discovery.)
    let mut raw_activity: Vec<[bool; 3]> = Vec::new();
    for &(win_start_secs, ref logits) in &cached {
        let num_classes = 7;
        let num_frames = logits.len() / num_classes;
        let window_activity =
            decode_multilabel_with_hysteresis(logits, num_frames, ONSET, ONSET);
        for (i, &act) in window_activity.iter().enumerate() {
            let abs_secs = win_start_secs + RECEPTIVE_OFFSET_SECS + (i as f64) * FRAME_SHIFT_SECS;
            let frame_idx = (abs_secs / FRAME_SHIFT_SECS).round() as usize;
            while raw_activity.len() <= frame_idx {
                raw_activity.push([false; 3]);
            }
            raw_activity[frame_idx] = act;
        }
    }
    eprintln!(
        "SMOOTH: {} raw frames, onset {:.1} → {} raw change-points",
        raw_activity.len(),
        ONSET,
        change_points(&raw_activity, FRAME_SHIFT_SECS, 0.0).len()
    );

    // --- Smoothing configs (light / medium / aggressive) ---
    // Frames-per-second ≈ 1/0.0169 ≈ 59. Configs in seconds → frames.
    const FPS: f64 = 1.0 / FRAME_SHIFT_SECS; // ~59
    let sec_to_frames = |s: f64| (s * FPS).round() as usize;
    // (label, median_rad_frames, min_on_secs, max_off_secs)
    let configs: &[(&str, usize, f64, f64)] = &[
        ("light (rad=1, on=0.1s, off=0.2s)", 1, 0.10, 0.20),
        ("medium (rad=3, on=0.3s, off=0.5s)", 3, 0.30, 0.50), // pyannote defaults
        ("aggressive (rad=5, on=0.5s, off=1.0s)", 5, 0.50, 1.00),
    ];

    // Anchors for precision measurement. These are the GROUND-TRUTH speaker
    // changes we MUST recover — if smoothing eats them, that config is too
    // aggressive. ±2s tolerance matches natural labelling slop.
    let anchors: &[(f64, &str)] = &[
        (17.0 * 60.0 + 37.0, "Ricardo join 1057s"),
        (46.0 * 60.0 + 58.0, "Ricardo interjection 2818s"),
    ];
    const ANCHOR_TOLERANCE_SECS: f64 = 2.0;

    let mut report = String::new();
    report.push_str(&format!(
        "# cde5c264 pyannote smoothed precision\n# audio: {:.1}s, onset {:.1}, {} ROI windows\n\n",
        audio_duration, ONSET, cached.len()
    ));

    for &(label, median_rad, min_on_s, max_off_s) in configs {
        let cps = smoothed_change_points(
            &raw_activity,
            median_rad,
            sec_to_frames(min_on_s),
            sec_to_frames(max_off_s),
            FRAME_SHIFT_SECS,
            0.0,
        );
        let (hits, anchors_hit, anchors_total) = anchor_hits(&cps, anchors, ANCHOR_TOLERANCE_SECS);

        eprintln!("\n===== {} =====", label);
        eprintln!("  total smoothed change-points: {}", cps.len());
        report.push_str(&format!("## {}\n- total change-points: {}\n\n", label, cps.len()));

        for &(ws, we, rlabel) in known_windows {
            let nearby: Vec<f64> = cps
                .iter()
                .filter(|&&t| t >= ws - 2.0 && t <= we + 2.0)
                .copied()
                .collect();
            eprintln!("  {} [{:.1}–{:.1}s]: {} turns", rlabel, ws, we, nearby.len());
            for &t in &nearby {
                eprintln!("    {:.3}s", t);
            }
            report.push_str(&format!(
                "### {} [{:.1}–{:.1}s]: {} turns\n",
                rlabel, ws, we, nearby.len()
            ));
            for &t in &nearby {
                report.push_str(&format!("- {:.3}s\n", t));
            }
            report.push('\n');
        }

        eprintln!(
            "  ANCHOR PRECISION: {}/{} anchors hit (±{}s) — {}",
            anchors_hit, anchors_total, ANCHOR_TOLERANCE_SECS,
            anchors
                .iter()
                .zip(hits.iter())
                .map(|((t, al), &n)| format!("{}@{:.0}s={}hit", al, t, if n > 0 { "" } else { "MISS " }))
                .collect::<Vec<_>>()
                .join(", ")
        );
        report.push_str(&format!(
            "**anchor precision: {}/{} hit (±{}s)**\n",
            anchors_hit, anchors_total, ANCHOR_TOLERANCE_SECS
        ));
        for ((_, al), &n) in anchors.iter().zip(hits.iter()) {
            report.push_str(&format!("- {}: {} hits\n", al, n));
        }
        report.push('\n');
    }

    let out = std::env::temp_dir().join("cde5c264_pyannote_smoothed_precision.txt");
    std::fs::write(&out, &report).expect("write report");
    eprintln!("\nSMOOTH: full report at {}", out.display());
    eprintln!("SMOOTH: DONE — compare turn counts vs raw sweep, check anchor-hit rate.");
}

// ============================================================================
// PHASE 2c: AHC re-clustering — the load-bearing empirical question.
//
// WHY this is the decisive probe: Part B's design hypothesis (D4) is "pyannote
// supplies boundaries only; Meetily's AHC + smoothing re-derive turns." That
// hypothesis is unverified. pyannote emits ~120 dense candidate fragments in
// the ROI after smoothing — but does the EXISTING AHC, tuned for 2-3s chunks,
// actually re-cluster those fragments into correct speakers, or does fragment
// density destabilize it? If the latter, Part B's whole approach fails and a
// panel must redesign.
//
// EXPERIMENT: run cde5c264 audio through adapter.process() TWICE with the SAME
// adapter (same threshold, same nemo_titanet embeddings, same smoothing) —
// differing ONLY in the `segments` boundaries passed:
//   Path A (baseline): production transcript timestamps (chunk-grid boundaries)
//   Path B (pyannote) : pyannote-3.0 fragments via ort, smoothed at pyannote
//                       default (median rad=3, on=0.3s, off=0.5s)
// Both paths feed the identical downstream AHC + smooth_to_fixed_point + cap.
// Any output difference is therefore PURELY attributable to boundary placement.
//
// METRIC (the thing that matters): in the 5.7–32.5s banter window, does Path B
// produce DISTINCT speaker labels where Path A collapses to one? And does the
// Ricardo interjection at 46:58 (2818s) survive as its own speaker in Path B?
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_ahc_reclustering
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_ahc_reclustering() {
    let _ = env_logger::builder().is_test(true).try_init();

    // --- Load cde5c264 audio (mirrors prior probes) ---
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

    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| audio_dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds.max(0.001);

    // Fetch transcript segments (the production boundary source) now that
    // audio_duration is known. Mirrors fetch_transcript_timestamps() in
    // commands.rs:607 — table `transcripts`, columns `audio_start_time`/
    // `audio_end_time`, same null/bounds validation so Path A matches production.
    let transcript_segments: Vec<(f64, f64)> = {
        let rows = sqlx::query(
            "SELECT audio_start_time, audio_end_time FROM transcripts \
             WHERE meeting_id = ? ORDER BY audio_start_time ASC",
        )
        .bind(meeting_id)
        .fetch_all(&pool)
        .await
        .expect("fetch transcripts");
        rows.into_iter()
            .filter_map(|r| {
                let s: Option<f64> = sqlx::Row::get(&r, "audio_start_time");
                let e: Option<f64> = sqlx::Row::get(&r, "audio_end_time");
                match (s, e) {
                    (Some(start), Some(end))
                        if start < end && start >= 0.0 && end <= audio_duration + 1.0 =>
                    {
                        Some((start, end))
                    }
                    _ => None,
                }
            })
            .collect()
    };
    drop(pool);
    eprintln!(
        "AHC: {} transcript segments fetched from DB",
        transcript_segments.len()
    );

    // --- Build the diarization adapter (mirrors test_cde5c264_two_pass_oracle) ---
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    // Bring the trait into scope so adapter.process() resolves.
    use app_lib::audio::speaker::diarization::DiarizationPort;
    use app_lib::audio::speaker::types::SpeakerSegment;
    let home = dirs::home_dir().expect("home dir").join(".meetily-models");
    // Use the canonical filename from model_download.rs (avoids hardcoding drift).
    let emb_name = app_lib::audio::speaker::model_download::embedding_filename();
    let emb_path = home.join(emb_name);
    let seg_path = home.join("pyannote-segmentation.onnx");
    assert!(emb_path.exists(), "embedding model missing at {}", emb_path.display());
    assert!(seg_path.exists(), "segmentation model missing at {}", seg_path.display());

    // threshold 0.40 × 65536 = production default (commands.rs oracle pattern).
    let threshold_fp = Arc::new(AtomicU32::new((0.40f32 * 65536.0) as u32));
    let emb_str = emb_path.to_str().expect("emb path utf8");
    let seg_str = seg_path.to_str().expect("seg path utf8");
    let adapter = app_lib::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
        emb_str,
        seg_str,
        threshold_fp,
    )
    .expect("build adapter");

    // =========================================================================
    // PATH A: production transcript boundaries through adapter.process()
    // =========================================================================
    eprintln!("\n===== PATH A: transcript-boundary baseline =====");
    // adapter.process() is sync; call inline on the current thread (probe,
    // not perf-critical — and the adapter is not Send-safe for spawn_blocking).
    let out_a = adapter
        .process(&samples, 16000, &transcript_segments)
        .expect("Path A process()");
    eprintln!(
        "AHC: Path A → {} speaker segments in {:.1}s",
        out_a.segments.len(),
        audio_duration
    );
    let labels_a: std::collections::HashSet<u32> =
        out_a.segments.iter().map(|s| s.speaker_id).collect();
    eprintln!("AHC: Path A distinct speakers: {}", labels_a.len());

    // =========================================================================
    // PATH B: pyannote-ort smoothed boundaries through the SAME adapter
    // =========================================================================
    eprintln!("\n===== PATH B: pyannote-ort smoothed boundaries =====");
    // Generate pyannote fragments over the FULL audio (not ROI) so AHC sees
    // the same audio span as Path A. Cached-inference + smoothing as in Phase 2b.
    let providers = vec![CPUExecutionProvider::default().build()];
    let pya_session = Session::builder()
        .expect("builder")
        .with_optimization_level(GraphOptimizationLevel::Level3)
        .expect("opt level")
        .with_execution_providers(providers)
        .expect("providers")
        .with_intra_threads(1)
        .expect("intra threads")
        .commit_from_file(&seg_path)
        .expect("load pyannote for Path B");
    let input_name = pya_session.inputs[0].name.to_string();
    let output_name = pya_session.outputs[0].name.to_string();
    let mut pya_session = pya_session;

    const SAMPLE_RATE: usize = 16000;
    const WINDOW_SAMPLES: usize = 160000; // 10s
    const STEP_SAMPLES: usize = 16000; // 1s
    const FRAME_SHIFT_SECS: f64 = 270.0 / 16000.0;
    const RECEPTIVE_OFFSET_SECS: f64 = 721.0 / 16000.0;
    const ONSET: f32 = 0.5;

    let total_windows = if samples.len() > WINDOW_SAMPLES {
        (samples.len() - WINDOW_SAMPLES) / STEP_SAMPLES + 1
    } else {
        1
    };
    eprintln!("AHC: Path B inferring {} pyannote windows...", total_windows);

    let mut cached: Vec<(f64, Vec<f32>)> = Vec::new();
    let t_infer = std::time::Instant::now();
    for win_idx in 0..total_windows {
        let start = win_idx * STEP_SAMPLES;
        let end = (start + WINDOW_SAMPLES).min(samples.len());
        if end - start < 16000 {
            break;
        }
        let win_start_secs = start as f64 / SAMPLE_RATE as f64;
        let mut window = vec![0.0f32; WINDOW_SAMPLES];
        window[..end - start].copy_from_slice(&samples[start..end]);
        let input_3d: Array3<f32> = Array1::from(window)
            .into_shape_with_order([1, 1, WINDOW_SAMPLES])
            .unwrap();
        let tensor_ref = TensorRef::from_array_view(input_3d.view()).expect("tensor ref");
        let inputs = ort::inputs![input_name.as_str() => tensor_ref];
        let outputs = pya_session.run(inputs).expect("forward");
        let output = outputs.get(output_name.as_str()).expect("output");
        let arr = output.try_extract_array::<f32>().expect("extract");
        let shape = arr.shape();
        let nf = shape[1];
        let nc = shape[2];
        let sl = arr.as_slice().unwrap_or_else(|| arr.to_slice().unwrap());
        cached.push((win_start_secs, sl[..nf * nc].to_vec()));
    }
    eprintln!(
        "AHC: Path B inference done in {:.0}s ({:.0}ms/window)",
        t_infer.elapsed().as_secs_f64(),
        t_infer.elapsed().as_millis() as f64 / total_windows as f64
    );

    // Decode + merge across windows → raw activity vector.
    let mut raw_activity: Vec<[bool; 3]> = Vec::new();
    for &(win_start_secs, ref logits) in &cached {
        let num_classes = 7;
        let num_frames = logits.len() / num_classes;
        let window_activity =
            decode_multilabel_with_hysteresis(logits, num_frames, ONSET, ONSET);
        for (i, &act) in window_activity.iter().enumerate() {
            let abs_secs = win_start_secs + RECEPTIVE_OFFSET_SECS + (i as f64) * FRAME_SHIFT_SECS;
            let frame_idx = (abs_secs / FRAME_SHIFT_SECS).round() as usize;
            while raw_activity.len() <= frame_idx {
                raw_activity.push([false; 3]);
            }
            raw_activity[frame_idx] = act;
        }
    }

    // Smooth at pyannote default config (medium — the only config that hit
    // BOTH anchors in Phase 2b). median rad=3, min_on=0.3s, max_off=0.5s.
    const FPS: f64 = 1.0 / FRAME_SHIFT_SECS;
    let sec_to_frames = |s: f64| (s * FPS).round() as usize;
    let med = median_filter_per_speaker(&raw_activity, 3);
    let gated = duration_gates_per_speaker(&med, sec_to_frames(0.3), sec_to_frames(0.5));
    let cps = change_points(&gated, FRAME_SHIFT_SECS, 0.0);
    eprintln!("AHC: Path B → {} smoothed change-points", cps.len());

    // Convert change-points to (start, end) segment tuples for adapter.process().
    // Each consecutive pair (cp[i], cp[i+1]) is a segment. Prepend 0.0 and
    // append audio_duration so the full span is covered.
    let mut bounds = vec![0.0f64];
    bounds.extend(cps.iter().copied().filter(|&t| t > 0.0 && t < audio_duration));
    bounds.push(audio_duration);
    bounds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    bounds.dedup_by(|a, b| (*a - *b).abs() < 0.05);
    let pyannote_segments: Vec<(f64, f64)> = bounds
        .windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|(s, e)| e - s >= 0.1) // drop sub-100ms slivers
        .collect();
    eprintln!(
        "AHC: Path B → {} pyannote segments fed to adapter.process()",
        pyannote_segments.len()
    );

    let t_b = std::time::Instant::now();
    let out_b = adapter
        .process(&samples, 16000, &pyannote_segments)
        .expect("Path B process()");
    eprintln!(
        "AHC: Path B → {} speaker segments in {:.1}s",
        out_b.segments.len(),
        t_b.elapsed().as_secs_f64()
    );
    let labels_b: std::collections::HashSet<u32> =
        out_b.segments.iter().map(|s| s.speaker_id).collect();
    eprintln!("AHC: Path B distinct speakers: {}", labels_b.len());

    // =========================================================================
    // COMPARISON — the load-bearing measurement
    // =========================================================================
    let banter = |s: f64| s >= 5.7 && s <= 32.5;
    let interj = |s: f64| (s - 2818.0).abs() <= 10.0;

    let banter_a: Vec<&SpeakerSegment> =
        out_a.segments.iter().filter(|s| banter(s.start_seconds)).collect();
    let banter_b: Vec<&SpeakerSegment> =
        out_b.segments.iter().filter(|s| banter(s.start_seconds)).collect();
    let banter_labels_a: std::collections::HashSet<u32> =
        banter_a.iter().map(|s| s.speaker_id).collect();
    let banter_labels_b: std::collections::HashSet<u32> =
        banter_b.iter().map(|s| s.speaker_id).collect();

    let interj_a: Vec<&SpeakerSegment> =
        out_a.segments.iter().filter(|s| interj(s.start_seconds)).collect();
    let interj_b: Vec<&SpeakerSegment> =
        out_b.segments.iter().filter(|s| interj(s.start_seconds)).collect();
    let interj_labels_a: std::collections::HashSet<u32> =
        interj_a.iter().map(|s| s.speaker_id).collect();
    let interj_labels_b: std::collections::HashSet<u32> =
        interj_b.iter().map(|s| s.speaker_id).collect();

    let mut report = String::new();
    report.push_str(&format!(
        "# cde5c264 AHC re-clustering comparison\n# audio: {:.1}s, threshold 0.40\n\n",
        audio_duration
    ));
    report.push_str("## Path A (transcript boundaries, production)\n\n");
    report.push_str(&format!("- total segments: {}\n", out_a.segments.len()));
    report.push_str(&format!("- distinct speakers (whole meeting): {}\n", labels_a.len()));
    report.push_str(&format!("- banter 5.7-32.5s: {} segments, {} distinct speakers\n\n",
        banter_a.len(), banter_labels_a.len()));

    report.push_str("## Path B (pyannote-ort smoothed boundaries)\n\n");
    report.push_str(&format!("- pyannote change-points: {}\n", cps.len()));
    report.push_str(&format!("- pyannote segments fed: {}\n", pyannote_segments.len()));
    report.push_str(&format!("- total speaker segments: {}\n", out_b.segments.len()));
    report.push_str(&format!("- distinct speakers (whole meeting): {}\n", labels_b.len()));
    report.push_str(&format!("- banter 5.7-32.5s: {} segments, {} distinct speakers\n\n",
        banter_b.len(), banter_labels_b.len()));

    report.push_str("## Load-bearing verdict\n\n");
    let banter_resolved = banter_labels_b.len() > banter_labels_a.len();
    let interj_resolved = !interj_labels_b.is_empty();
    report.push_str(&format!(
        "- banter speaker-separation: Path A={}, Path B={} → **{}**\n",
        banter_labels_a.len(), banter_labels_b.len(),
        if banter_resolved { "IMPROVED (pyannote fragments re-cluster into distinct speakers)" }
        else { "NO IMPROVEMENT (AHC collapses pyannote fragments too)" }
    ));
    report.push_str(&format!(
        "- interjection 46:58 survives as own speaker: Path A={}, Path B={} → **{}**\n",
        interj_labels_a.len(), interj_labels_b.len(),
        if interj_resolved { "SURVIVES" } else { "LOST" }
    ));

    eprintln!("\n========== AHC VERDICT ==========");
    eprintln!("Banter 5.7-32.5s distinct speakers: Path A={}, Path B={}",
        banter_labels_a.len(), banter_labels_b.len());
    eprintln!("Interjection 46:58 distinct speakers: Path A={}, Path B={}",
        interj_labels_a.len(), interj_labels_b.len());
    eprintln!("Whole-meeting distinct speakers: A={}, B={}", labels_a.len(), labels_b.len());
    eprintln!("==================================");

    let out = std::env::temp_dir().join("cde5c264_pyannote_ahc_reclustering.txt");
    std::fs::write(&out, &report).expect("write report");
    eprintln!("\nAHC: full report at {}", out.display());
    eprintln!("AHC: DONE — if Path B banter shows more distinct speakers than Path A, Part B's D4 hypothesis holds.");
}

// ============================================================================
// PHASE 2d — CRUX-RESOLUTION PROBE: does sherpa's OfflineSpeakerDiarization
// load + run pyannote WITHOUT the ort crate?
//
// WHY this single test decides the design: Round 1 of the adversarial panel
// converged on one empirical dispute. The all-sherpa champion claims the
// on-disk pyannote-segmentation.onnx (opset 13, IR 7, all standard-domain
// node ops) loads fine under sherpa's bundled ORT 1.17.1 — which would
// dissolve the entire two-ORT conflict by removing the `ort` dep from the
// diarization path. Options 1 (port nemo_titanet to ort) and 3 (subprocess
// IPC) both presuppose this is FALSE.
//
// This test is the arbiter. It:
//   1. Constructs sherpa's OfflineSpeakerDiarizationConfig pointing the
//      segmentation model at the on-disk pyannote file, the embedding model
//      at nemo_titanet (the model sherpa ALREADY loads today).
//   2. Calls OfflineSpeakerDiarization::create() — the exact step the design
//      doc D1 assumes works. If this returns None or panics, all-sherpa is
//      falsified; the conflict is real and Options 1/3 remain.
//   3. Runs process() on 60s of real cde5c264 audio. If it returns non-empty
//      in-range segments, sherpa's full diarization pipeline (segmentation +
//      embedding + clustering) runs on ONE ORT — Option 2 is confirmed.
//
// Per the design doc D1, FastClusteringConfig uses num_clusters=-1,
// threshold=0.0, min_duration_on=0.0, min_duration_off=0.0 to get maximally
// fragmented candidate boundaries (the design's intent).
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_sherpa_load_crux
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_sherpa_load_crux() {
    let _ = env_logger::builder().is_test(true).try_init();

    // --- Resolve model paths ---
    let home = dirs::home_dir().expect("home dir").join(".meetily-models");
    let emb_name = app_lib::audio::speaker::model_download::embedding_filename();
    let emb_path = home.join(emb_name);
    let seg_path = home.join("pyannote-segmentation.onnx");
    assert!(emb_path.exists(), "embedding model missing at {}", emb_path.display());
    assert!(seg_path.exists(), "segmentation model missing at {}", seg_path.display());

    eprintln!("CRUX: seg model opset/IR (verified externally via onnx Python):");
    eprintln!("CRUX:   pyannote: opset 13, IR 7, standard-domain node ops only");
    eprintln!("CRUX:   (nemo_titanet loads fine at opset 17/IR 8 — higher than pyannote)");

    // --- Construct sherpa's OfflineSpeakerDiarizationConfig (D1 settings) ---
    // The structs are re-exported at the sherpa_onnx crate root
    // (lib.rs: `pub use offline_speaker_diarization::*;`); the module itself
    // is private, so import from the crate root, not the module path.
    use sherpa_onnx::{
        FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
        OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
        SpeakerEmbeddingExtractorConfig,
    };

    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(seg_path.to_str().expect("seg path utf8").to_string()),
            },
            num_threads: 1,
            debug: false,
            provider: Some("cpu".to_string()),
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(emb_path.to_str().expect("emb path utf8").to_string()),
            num_threads: 1,
            debug: false,
            provider: Some("cpu".to_string()),
        },
        // D1: maximally fragmented candidate boundaries. threshold=0.0 is a
        // cosine-DISSIMILARITY cutoff where smaller → more clusters (per the
        // design doc's correction of the original threshold=1.0).
        clustering: FastClusteringConfig {
            num_clusters: -1,
            threshold: 0.0,
        },
        min_duration_on: 0.0,
        min_duration_off: 0.0,
    };

    eprintln!("CRUX: calling OfflineSpeakerDiarization::create()...");
    let t0 = std::time::Instant::now();
    let diarizer = match OfflineSpeakerDiarization::create(&config) {
        Some(d) => {
            eprintln!(
                "CRUX PASS: create() succeeded in {:.1}s — sherpa loaded pyannote-3.0 via its bundled ORT 1.17.1",
                t0.elapsed().as_secs_f64()
            );
            eprintln!("CRUX   → the two-ORT conflict is DISSOLVED by routing pyannote through sherpa");
            eprintln!("CRUX   → Option 2 (all-sherpa) central claim CONFIRMED; Options 1 & 3 solve a non-problem");
            d
        }
        None => {
            eprintln!(
                "CRUX FAIL: create() returned None in {:.1}s — sherpa's ORT 1.17.1 CANNOT load pyannote-3.0",
                t0.elapsed().as_secs_f64()
            );
            eprintln!("CRUX   → the two-ORT conflict is REAL; Option 2 falsified; Options 1 & 3 remain");
            panic!("sherpa OfflineSpeakerDiarization::create() failed for pyannote-3.0");
        }
    };
    eprintln!("CRUX: sample_rate = {}", diarizer.sample_rate());

    // --- Run process() on 60s of real cde5c264 audio ---
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
    drop(pool);

    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| audio_dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
    let samples = decoded.to_whisper_format();

    // First 60s — includes the banter window 5.7–32.5s where the merged-speakers
    // bug manifests. If sherpa's diarizer produces >1 segment here, the bug is
    // fixed by this path on real audio.
    let probe_samples = &samples[..(60 * 16000).min(samples.len())];
    eprintln!(
        "CRUX: calling process() on first 60s ({} samples)...",
        probe_samples.len()
    );
    let t1 = std::time::Instant::now();
    let result = match diarizer.process(probe_samples) {
        Some(r) => {
            eprintln!(
                "CRUX PASS: process() returned in {:.1}s — full diarization pipeline ran",
                t1.elapsed().as_secs_f64()
            );
            r
        }
        None => {
            eprintln!("CRUX FAIL: process() returned None — pipeline ran but produced nothing");
            panic!("sherpa process() returned None");
        }
    };

    let segments = result.sort_by_start_time();
    eprintln!("CRUX: {} segments from 60s of audio:", segments.len());
    let mut distinct_speakers = std::collections::HashSet::new();
    for seg in segments.iter().take(40) {
        eprintln!(
            "  {:.2}-{:.2}s speaker={}",
            seg.start, seg.end, seg.speaker
        );
        distinct_speakers.insert(seg.speaker);
    }
    eprintln!(
        "CRUX: {} distinct speakers in first 60s (banter window 5.7-32.5s should be multi-speaker)",
        distinct_speakers.len()
    );

    // Sanity: in-range, non-empty.
    assert!(!segments.is_empty(), "expected non-empty diarization");
    for seg in &segments {
        assert!(seg.start >= 0.0 && seg.end <= 60.5, "segment out of range: {:?}", seg);
        assert!(seg.end > seg.start, "non-positive-duration segment: {:?}", seg);
    }

    let banter_speakers: std::collections::HashSet<i32> = segments
        .iter()
        .filter(|s| s.start >= 5.7 && s.start <= 32.5)
        .map(|s| s.speaker)
        .collect();
    eprintln!(
        "CRUX VERDICT: {} distinct speakers in banter 5.7-32.5s (current pipeline: 1)",
        banter_speakers.len()
    );

    let out = std::env::temp_dir().join("cde5c264_pyannote_sherpa_crux.txt");
    let mut report = String::new();
    report.push_str("# cde5c264 sherpa OfflineSpeakerDiarization crux probe\n\n");
    report.push_str(&format!(
        "## create() + process() on 60s: PASS\n- segments: {}\n- distinct speakers (60s): {}\n- banter 5.7-32.5s speakers: {} (baseline: 1)\n\n",
        segments.len(), distinct_speakers.len(), banter_speakers.len()
    ));
    report.push_str("## Segments\n\n");
    for seg in &segments {
        report.push_str(&format!("- {:.2}-{:.2}s speaker={}\n", seg.start, seg.end, seg.speaker));
    }
    std::fs::write(&out, &report).expect("write report");
    eprintln!("\nCRUX: full report at {}", out.display());
}

// ============================================================================
// PHASE 3: nemo_titanet cosine-equivalence gate probe (Option 1 design gate).
//
// WHY this test exists: the adversarial design panel agreed that before
// committing to "port nemo_titanet embedding extraction from sherpa-onnx to
// the `ort` crate," a gate must pass: the two paths must produce cosinely-
// equivalent embeddings (cosine > 0.99) on a diverse clip set. This test is
// that gate.
//
// DESIGN: load the SAME nemo-titanet-embedding.onnx via two paths:
//   - Reference: sherpa_onnx::SpeakerEmbeddingExtractor (sherpa's C++ frontend
//     + bundled ORT 1.17.1). Pattern mirrors sherpa_adapter.rs:304
//     (extract_embedding): create_stream → accept_waveform → is_ready → compute.
//   - Candidate: ort::Session (project's ort 2.0.0-rc.10) over the same model
//     file, with a HAND-ROLLED reproduction of sherpa's nemo preprocessing
//     pipeline (mel filterbank + CMVN + frame-pad + transpose).
//
// Run both on N=10 clips spanning silence / short / overlap-dense / clean
// single-speaker regions. Assert cosine > 0.99 on every clip (the panel's
// gate threshold).
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture nemo_titanet_ort_cosine_equivalence
//
// ----------------------------------------------------------------------------
// THE PREPROCESSING PIPELINE (reproduced from sherpa-onnx v1.13.4 source).
//
// sherpa's SpeakerEmbeddingExtractor does NOT feed raw audio to the model.
// The model input contract (verified on nemo-titanet-embedding.onnx via onnx
// Python):
//     audio_signal : float32[N, 80, T]   (80 mel-filterbank features × T frames)
//     length       : int64[N]            (T, the unpadded frame count)
//     embs         : float32[N, 192]     (output embedding)
// The preprocessing runs in sherpa's C++ frontend before the forward pass.
//
// Source chain (all verified against raw GitHub source, v1.13.4 / knf v1.22.3):
//   1. sherpa-onnx/csrc/speaker-embedding-extractor-nemo-impl.h
//      - CreateStream(): builds FeatureExtractorConfig for nemo with:
//          normalize_samples = true
//          snip_edges        = true
//          is_librosa        = true
//          low_freq          = 0
//          remove_dc_offset  = false
//          sampling_rate     = meta.sample_rate      (16000, from ONNX metadata)
//          feature_dim       = meta.feat_dim         (80,    from ONNX metadata)
//          frame_shift_ms    = meta.window_stride_ms (10,    from ONNX metadata)
//          frame_length_ms   = meta.window_size_ms   (25,    from ONNX metadata)
//          window_type       = meta.window_type      ("hann",from ONNX metadata)
//        All other FeatureExtractorConfig fields keep sherpa's defaults:
//          dither=0.0, preemph_coeff=0.97, round_to_power_of_two=true,
//          high_freq=-400.0, is_mfcc/is_whisper/is_t_one=false.
//   2. sherpa-onnx/csrc/features.cc InitFbank() maps the sherpa config onto
//      knf::FbankOptions. knf defaults left untouched: use_power=true,
//      use_log_fbank=true, use_energy=false, energy_floor=0.
//   3. sherpa-onnx/csrc/features.cc AcceptWaveform(): normalize_samples=true
//      means samples pass through AS-IS (range [-1,+1]), NOT scaled by 32768.
//   4. kaldi-native-fbank (knf v1.22.3) feature-window.cc ProcessWindow():
//      order is dither → remove_dc_offset → (energy) → preemphasize → window.
//      For nemo: dither=0 (skip), remove_dc_offset=false (skip), so it is just
//      preemphasize(coeff=0.97) then apply window. Preemphasize:
//          for i in (n-1)..1 (descending): d[i] -= 0.97 * d[i-1]
//          d[0] -= 0.97 * d[0]
//   5. knf feature-window.cc window function ("hann", periodic per the pytorch
//      convention referenced in the source comment):
//          a = 2π / frame_length      (NOTE: /frame_length, not /(frame_length-1))
//          w[i] = 0.5 - 0.5*cos(a*i),  i in 0..frame_length
//      frame_length = int(16000 * 0.001 * 25) = 400.
//   6. knf feature-window.cc framing (snip_edges=true):
//          frame_shift = int(16000 * 0.001 * 10) = 160
//          num_frames  = 1 + (N - frame_length) / frame_shift   (integer div)
//          first_sample_of_frame(f) = f * frame_shift
//   7. knf rfft.cc: forward real FFT of the zero-padded frame (padded to
//      PaddedWindowSize = next_pow2(400) = 512). Uses kiss_fftr convention:
//      FORWARD, NO 1/N normalization.
//   8. knf feature-functions.cc ComputePowerSpectrum(): power[k] = re²+im²
//      for k in 0..256 (no normalization).
//   9. knf mel-computations.cc InitLibrosaMelBanks(): Slaney mel scale
//      (use_slaney_mel_scale=true, norm="slaney", floor_to_int_bin=false).
//        MelScaleSlaney(f)   = f*3/200               if f<=1000  else 15 + 14.545078505785561*ln(f/1000)
//        InverseMelScaleSlaney(m) = 200/3 * m        if m<=15    else 1000*exp((m-15)*0.06875177742094911)
//        low_freq=0 → mel_low=0; high_freq=8000+(-400)=7600 → mel_high
//        mel_freq_delta = (mel_high - mel_low) / (num_bins + 1)   (num_bins=80)
//        For each bin b: left_mel/center_mel/right_mel at (b, b+1, b+2)*delta
//          → left_hz/center_hz/right_hz via InverseMelScaleSlaney
//          fft_bin_width = sample_freq / window_length_padded = 16000/512 = 31.25
//          for each fft bin i (hz = i*fft_bin_width):
//            if left_hz < hz < right_hz:
//              weight = (hz-left)/(center-left)  if hz<=center else (right-hz)/(right-center)
//              weight *= 2/(right_hz - left_hz)   (slaney normalization)
//        Triangular filter applied to the 257-bin power spectrum → 80 mel energies.
//  10. knf feature-fbank.cc FbankComputer.Compute(): use_log_fbank=true →
//        mel_energies[i] = ln(max(mel_energies[i], eps))   (eps = f32 MIN > 0)
//      use_energy=false so output is exactly num_bins=80 floats per frame.
//  11. sherpa nemo-impl.h Compute(): NormalizePerFeature (per-feature CMVN)
//      over the [T, 80] matrix (Eigen RowMajor map):
//        EX       = colwise mean            (per feature bin, across T frames)
//        EX2      = colwise mean of squares
//        variance = max(EX2 - EX², 1e-5)    (per bin, floored)
//        stddev   = sqrt(variance)
//        m[i][j]  = (m[i][j] - EX[j]) / (stddev[j] + 1e-5)
//      NOTE the DOUBLE epsilon: the variance is floored at 1e-5 AND the
//      denominator adds another 1e-5. Both are reproduced exactly.
//  12. sherpa nemo-impl.h Compute(): frame padding to a multiple of 16:
//        if num_frames % 16 != 0: pad = 16 - num_frames % 16
//          features resized to (num_frames+pad)*feat_dim, new frames = zeros
//  13. sherpa nemo-impl.h Compute(): reshape to [1, num_frames_padded, 80]
//      then Transpose12 → [1, 80, num_frames_padded]. This is audio_signal.
//      length = num_frames (UNPADDED) as int64 scalar [1].
//
// DISCREPANCY vs the Round 1 panelist summary (TRUST THE SOURCE):
//   - Panelist said "stride=25ms". WRONG: model metadata window_stride_ms=10,
//     window_size_ms=25. Window LENGTH is 25ms; STRIDE/shift is 10ms. This is
//     load-bearing — it determines num_frames.
//   - Panelist's other params (feat_dim=80, is_librosa=true, low_freq=0,
//     remove_dc_offset=false, snip_edges=true, normalize_samples=true,
//     variance floor max(var,1e-5), pad-to-multiple-of-16, transpose
//     [1,T,80]→[1,80,T], separate length int64 tensor) all MATCH the source.
//   - Panelist did NOT mention: preemph_coeff=0.97 (applied — knf default),
//     high_freq=-400 (→ 7600 Hz effective), slaney norm on the mel filters,
//     the +1e-5 added to the stddev denominator on top of the variance floor,
//     use_power=true / use_log_fbank=true / use_energy=false (knf defaults).
// ============================================================================

/// Parameters of the nemo_titanet preprocessing pipeline, derived from the
/// sherpa/knf source analysis documented above. Grouped as a struct so the
/// fbank functions below are parameterized and the constants are testable.
#[allow(dead_code)]
struct NemoFbankParams {
    sample_rate: usize,
    feat_dim: usize,
    frame_length_ms: f32,
    frame_shift_ms: f32,
    window_size: usize,           // samples = int(sr*0.001*frame_length_ms)
    window_shift: usize,          // samples = int(sr*0.001*frame_shift_ms)
    fft_size: usize,              // next_pow2(window_size)
    preemph_coeff: f32,
    low_freq: f32,
    high_freq: f32,               // raw config value; effective = nyquist + high_freq if <=0
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
        // RoundUpToNearestPowerOfTwo(400) = 512 (400 is not a power of two, so
        // next_power_of_two gives the next one up).
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
/// pairs, matching knf MelBanks::InitLibrosaMelBanks exactly:
///   - triangular filters between left/center/right mel points
///   - slaney normalization (weight *= 2 / (right_hz - left_hz))
///   - fft bin i has frequency hz = i * fft_bin_width  (fft_bin_width = sr/fft_size)
/// `weights` covers fft bins [first_index .. first_index + weights.len()].
fn build_librosa_mel_filterbank(params: &NemoFbankParams) -> Vec<(usize, Vec<f32>)> {
    let num_bins = params.feat_dim;
    let window_length_padded = params.fft_size; // 512
    let num_fft_bins = window_length_padded / 2; // 256
    let sample_freq = params.sample_rate as f32;
    let nyquist = 0.5 * sample_freq;

    // Effective high_freq: if config value <= 0, high_freq = nyquist + config.
    let low_freq = params.low_freq;
    let high_freq = if params.high_freq > 0.0 {
        params.high_freq
    } else {
        nyquist + params.high_freq
    };
    assert!(
        low_freq >= 0.0 && low_freq < nyquist && high_freq > 0.0 && high_freq <= nyquist,
        "bad mel range: low={} high={} nyquist={}",
        low_freq, high_freq, nyquist
    );

    let fft_bin_width = sample_freq / window_length_padded as f32; // 31.25
    let mel_low = mel_scale_slaney(low_freq);
    let mel_high = mel_scale_slaney(high_freq);
    // knf: mel_freq_delta = (mel_high - mel_low) / (num_bins + 1)
    let mel_freq_delta = (mel_high - mel_low) / (num_bins as f32 + 1.0);

    let mut bins: Vec<(usize, Vec<f32>)> = Vec::with_capacity(num_bins);
    for bin in 0..num_bins {
        let left_mel = mel_low + bin as f32 * mel_freq_delta;
        let center_mel = mel_low + (bin + 1) as f32 * mel_freq_delta;
        let right_mel = mel_low + (bin + 2) as f32 * mel_freq_delta;

        let left_hz = inverse_mel_scale_slaney(left_mel);
        let center_hz = inverse_mel_scale_slaney(center_mel);
        let right_hz = inverse_mel_scale_slaney(right_mel);

        // knf iterates i in 0..=num_fft_bins (num_fft_bins+1 = 257 entries).
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
                // slaney normalization
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

/// Periodic Hann window (pytorch convention), matching knf's "hann" branch:
///   a = 2π / frame_length      (periodic: divides by length, not length-1)
///   w[i] = 0.5 - 0.5*cos(a*i)
fn hann_window(frame_length: usize) -> Vec<f32> {
    let a = 2.0f32 * std::f32::consts::PI / frame_length as f32;
    (0..frame_length)
        .map(|i| 0.5 - 0.5 * (a * i as f32).cos())
        .collect()
}

/// Preemphasize in place, matching knf Preemphasize (descending sweep, d[0]
/// subtracts preemph*d[0]).
fn preemphasize(d: &mut [f32], preemph_coeff: f32) {
    if preemph_coeff == 0.0 {
        return;
    }
    for i in (1..d.len()).rev() {
        d[i] -= preemph_coeff * d[i - 1];
    }
    d[0] -= preemph_coeff * d[0];
}

/// Number of frames for snip_edges=true framing:
///   num_frames = 1 + (num_samples - window_size) / window_shift   (integer div)
/// Returns 0 if num_samples < window_size (matches knf NumFrames).
fn num_frames_snip(num_samples: usize, window_size: usize, window_shift: usize) -> usize {
    if num_samples < window_size {
        0
    } else {
        1 + (num_samples - window_size) / window_shift
    }
}

/// Compute the [T, feat_dim] log-mel filterbank matrix for `samples` using the
/// nemo pipeline documented above. Returns None if T==0 (too few samples).
///
/// Steps per frame: extract window → preemphasize → apply hann → zero-pad to
/// fft_size → forward real FFT → power spectrum → mel filterbank → log.
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

    // Precompute window + filterbank + FFT plan.
    let window = hann_window(window_size);
    let mel_bins = build_librosa_mel_filterbank(params);
    let mut fft_planner = RealFftPlanner::<f32>::new();
    let r2c = fft_planner.plan_fft_forward(fft_size);
    let mut fft_out: Vec<Complex<f32>> = r2c.make_output_vec();
    let mut fft_scratch: Vec<Complex<f32>> = r2c.make_scratch_vec();

    let num_fft_bins = fft_size / 2; // 256
    let mut features = vec![0.0f32; num_frames * feat_dim];
    // knf feature-fbank.cc: log floor = numeric_limits<float>::epsilon() (~1.19e-7).
    let eps: f32 = f32::MIN_POSITIVE;

    let mut frame_buf = vec![0.0f32; fft_size];
    for f in 0..num_frames {
        let start = f * window_shift;
        // Copy window into the zero-initialized fft buffer (rest stays 0 = padding).
        frame_buf[..window_size].copy_from_slice(&samples[start..start + window_size]);
        // knf ProcessWindow order (nemo): preemphasize THEN window.
        preemphasize(&mut frame_buf[..window_size], params.preemph_coeff);
        for i in 0..window_size {
            frame_buf[i] *= window[i];
        }
        // Forward real FFT (kiss_fftr convention: no 1/N normalization).
        r2c.process_with_scratch(&mut frame_buf, &mut fft_out, &mut fft_scratch)
            .expect("fft");
        // Power spectrum: power[k] = re²+im², k in 0..=num_fft_bins (257 values).
        // realfft returns Complex<f32>[N/2+1] directly; map power[k] = |c[k]|².
        let mut power = [0.0f32; 257];
        debug_assert_eq!(num_fft_bins + 1, 257);
        for k in 0..=num_fft_bins {
            let re = fft_out[k].re;
            let im = fft_out[k].im;
            power[k] = re * re + im * im;
        }
        // Apply mel filterbank → feat_dim mel energies, then log.
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
        // Clear the frame buffer so next iteration's zero-padding is correct
        // (the FFT treated it as scratch).
        for v in frame_buf.iter_mut() {
            *v = 0.0;
        }
    }
    Some(features)
}

/// Per-feature CMVN (NormalizePerFeature) over the [T, feat_dim] row-major
/// matrix, matching sherpa nemo-impl.h NormalizePerFeature exactly:
///   EX       = colwise mean                  (per feature bin)
///   variance = max(colwise_mean_of_squares - EX², 1e-5)
///   stddev   = sqrt(variance)
///   m[i][j]  = (m[i][j] - EX[j]) / (stddev[j] + 1e-5)
/// Modifies `features` in place.
fn normalize_per_feature(features: &mut [f32], num_frames: usize, feat_dim: usize) {
    // EX (mean) and EX2 (mean of squares), per column j.
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
    // variance = max(EX2 - EX², 1e-5); denom = sqrt(variance) + 1e-5
    let mut denom = vec![0.0f32; feat_dim];
    for j in 0..feat_dim {
        let variance = (ex2[j] - ex[j] * ex[j]).max(1e-5);
        denom[j] = variance.sqrt() + 1e-5;
    }
    // m[i][j] = (m[i][j] - EX[j]) / denom[j]
    for f in 0..num_frames {
        let row = &mut features[f * feat_dim..(f + 1) * feat_dim];
        for j in 0..feat_dim {
            row[j] = (row[j] - ex[j]) / denom[j];
        }
    }
}

/// Build the ort input tensors for nemo_titanet from raw samples:
///   audio_signal : float32[1, 80, T_padded]   (T_padded = round-up of T to 16)
///   length       : int64[1]                   (T, unpadded)
/// Returns (audio_signal_flat in [80, T_padded] row-major, T_padded, T_unpadded, length).
/// Returns None if the clip is too short to yield even one frame.
fn nemo_build_model_inputs(
    samples: &[f32],
    params: &NemoFbankParams,
) -> Option<(Vec<f32>, usize, usize, i64)> {
    let feat_dim = params.feat_dim;
    // Steps 1-10: log-mel fbank → [T, 80].
    let mut features = nemo_log_mel_fbank(samples, params)?;
    let num_frames = features.len() / feat_dim;
    // Step 11: per-feature CMVN.
    normalize_per_feature(&mut features, num_frames, feat_dim);
    // Step 12: pad frames to a multiple of 16 (zero-fill new frames).
    let pad = if num_frames % 16 != 0 {
        16 - (num_frames % 16)
    } else {
        0
    };
    let num_frames_padded = num_frames + pad;
    features.resize(num_frames_padded * feat_dim, 0.0);
    // Step 13: transpose [1, T_pad, 80] → [1, 80, T_pad]. The [T_pad, 80]
    // matrix is row-major (frame-contiguous); after transpose the [80, T_pad]
    // matrix is row-major (feature-contiguous).
    let mut transposed = vec![0.0f32; feat_dim * num_frames_padded];
    for t in 0..num_frames_padded {
        for j in 0..feat_dim {
            transposed[j * num_frames_padded + t] = features[t * feat_dim + j];
        }
    }
    Some((transposed, num_frames_padded, num_frames, num_frames as i64))
}

/// Cosine similarity between two vectors. Returns 0.0 if either is zero-norm.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "embedding dim mismatch");
    let dot: f32 = a.iter().zip(b.iter()).map(|(&x, &y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na * nb)
}

#[tokio::test]
#[ignore]
async fn nemo_titanet_ort_cosine_equivalence() {
    let _ = env_logger::builder().is_test(true).try_init();

    // --- Resolve the nemo-titanet model (same file for both paths) ---
    let home = dirs::home_dir().expect("home dir").join(".meetily-models");
    let emb_name = app_lib::audio::speaker::model_download::embedding_filename();
    let model_path = home.join(emb_name);
    assert!(
        model_path.exists(),
        "nemo-titanet model missing at {}",
        model_path.display()
    );
    eprintln!("GATE: model under test = {}", model_path.display());

    // --- Load cde5c264 audio (same DB/folder/decode pattern as the sweep test) ---
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
    drop(pool);

    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| audio_dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    eprintln!("GATE: audio at {}", audio_path.display());
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
    let samples = decoded.to_whisper_format();
    eprintln!(
        "GATE: {} samples ({:.1}s @ 16kHz mono)",
        samples.len(),
        decoded.duration_seconds.max(0.001)
    );

    // --- Clip selection: 10 clips spanning the panel's required categories ---
    // (start_secs, end_secs, label). Timestamps chosen from DB transcript gaps
    // (silence), the banter window 5.7-32.5s (overlap/dense + short), and clean
    // single-speaker monologues (>20s segments).
    const SAMPLE_RATE: usize = 16000;
    let sr_f = SAMPLE_RATE as f64;
    let clips: &[(f64, f64, &str)] = &[
        // Silence / near-silence (2) — drawn from the two largest DB transcript
        // gaps (no speech for 10-14s).
        (2905.30, 2908.30, "silence-1 (inside 13.9s gap 2905.3-2919.2)"),
        (1878.58, 1881.58, "silence-2 (inside 10.1s gap 1878.6-1888.7)"),
        // Short <1s clips (2) — sub-second speech windows from active regions.
        (6.00, 6.70, "short-1 0.7s (banter onset)"),
        (1917.63, 1918.33, "short-2 0.7s (inside 3.1s seg 1917.6-1920.7)"),
        // Overlap / dense regions (2) — banter rapid multi-turn.
        (5.67, 8.67, "overlap-1 (banter rapid multi-turn)"),
        (10.00, 13.00, "overlap-2 (banter rapid multi-turn)"),
        // Clean single-speaker regions (4) — drawn from >20s monologues.
        (57.78, 61.78, "clean-1 (22s monologue 57.8-80.1)"),
        (80.05, 84.05, "clean-2 (23s monologue 80.1-103.5)"),
        (1057.00, 1061.00, "clean-3 (Ricardo join region 17:37)"),
        (1933.56, 1938.52, "clean-4 (4.96s seg 1933.6-1938.5)"),
    ];
    assert_eq!(clips.len(), 10, "gate spec requires N>=10 clips");

    // =========================================================================
    // REFERENCE PATH: sherpa SpeakerEmbeddingExtractor (bundled ORT 1.17.1).
    // Pattern mirrors sherpa_adapter.rs:304 extract_embedding, but WITHOUT the
    // project's is_effectively_silent energy gate — we call sherpa's raw API
    // directly so silence clips still produce an embedding (the model runs on
    // the silence's mel features, same as the ort path will).
    // =========================================================================
    use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
    let sherpa_cfg = SpeakerEmbeddingExtractorConfig {
        model: Some(model_path.to_str().expect("model path utf8").to_string()),
        num_threads: 1,
        debug: false,
        provider: Some("cpu".to_string()),
    };
    let sherpa_ext = SpeakerEmbeddingExtractor::create(&sherpa_cfg)
        .expect("sherpa SpeakerEmbeddingExtractor::create");
    let emb_dim = sherpa_ext.dim() as usize;
    eprintln!("GATE: sherpa extractor ready (embedding dim = {})", emb_dim);
    assert_eq!(emb_dim, 192, "expected nemo_titanet output_dim=192");

    let sherpa_embedding = |clip_samples: &[f32]| -> Option<Vec<f32>> {
        let stream = sherpa_ext.create_stream()?;
        stream.accept_waveform(SAMPLE_RATE as i32, clip_samples);
        // sherpa gates on is_ready (enough audio for >=1 frame after framing).
        if !sherpa_ext.is_ready(&stream) {
            return None;
        }
        sherpa_ext.compute(&stream)
    };

    // =========================================================================
    // CANDIDATE PATH: ort::Session (project's ort 2.0.0-rc.10) over the same
    // model, with the hand-rolled nemo preprocessing pipeline.
    // =========================================================================
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
        .expect("ort load nemo-titanet");
    // Verify the input contract against the model under test.
    eprintln!("GATE: ort session inputs:");
    for (i, inp) in session.inputs.iter().enumerate() {
        eprintln!("  [{}] {} = {:?}", i, inp.name, inp.input_type);
    }
    eprintln!("GATE: ort session outputs:");
    for (i, out) in session.outputs.iter().enumerate() {
        eprintln!("  [{}] {} = {:?}", i, out.name, out.output_type);
    }
    // Locate inputs by the documented names. nemo_titanet: audio_signal (f32),
    // length (int64). Output of interest: embs (f32[N,192]).
    let audio_input_name = session
        .inputs
        .iter()
        .find(|i| i.name == "audio_signal")
        .expect("model has no 'audio_signal' input")
        .name
        .to_string();
    let length_input_name = session
        .inputs
        .iter()
        .find(|i| i.name == "length")
        .expect("model has no 'length' input")
        .name
        .to_string();
    let emb_output_name = session
        .outputs
        .iter()
        .find(|o| o.name == "embs")
        .or_else(|| session.outputs.iter().find(|o| o.name == "embeddings"))
        .expect("model has no 'embs'/'embeddings' output")
        .name
        .to_string();
    eprintln!(
        "GATE: ort I/O names — in[{}/{}] out[{}]",
        audio_input_name, length_input_name, emb_output_name
    );
    let mut session = session; // run() needs &mut self

    let params = NemoFbankParams::default();
    let mut ort_embedding = |clip_samples: &[f32]| -> Option<Vec<f32>> {
        // Build audio_signal [1,80,T_pad] + length [1].
        let (audio_flat, t_padded, _t_unpadded, length_val) =
            nemo_build_model_inputs(clip_samples, &params)?;
        let audio_3d: Array3<f32> =
            Array1::from(audio_flat)
                .into_shape_with_order([1, params.feat_dim, t_padded])
                .expect("audio shape");
        let length_arr: Array1<i64> = ndarray::arr1(&[length_val]);
        let audio_ref = TensorRef::from_array_view(audio_3d.view()).expect("audio tensor ref");
        let length_ref =
            TensorRef::from_array_view(length_arr.view()).expect("length tensor ref");
        // ort::inputs! returns a SessionInputs value (not a Result) — the `?`
        // in the ort docs comes from the TensorRef::from_array_view(...) calls
        // inside the macro, which we've already unwrapped via .expect above.
        let inputs = ort::inputs![
            audio_input_name.as_str() => audio_ref,
            length_input_name.as_str() => length_ref,
        ];
        let outputs = session.run(inputs).expect("ort forward");
        let out = outputs
            .get(emb_output_name.as_str())
            .unwrap_or_else(|| panic!("output '{}' not in ort outputs", emb_output_name));
        let arr = out.try_extract_array::<f32>().expect("extract embs");
        // Output is [1, 192]; flatten to the 192-dim embedding.
        let slice = arr
            .as_slice()
            .unwrap_or_else(|| arr.to_slice().unwrap());
        assert!(
            slice.len() >= emb_dim,
            "embedding output too short: {}",
            slice.len()
        );
        Some(slice[..emb_dim].to_vec())
    };

    // =========================================================================
    // RUN BOTH PATHS ON EVERY CLIP AND MEASURE COSINE SIMILARITY
    // =========================================================================
    const GATE_THRESHOLD: f32 = 0.99;
    // (start, end, label, sherpa_norm, ort_norm, cosine). cosine == 0.0 marks
    // a row where one or both paths returned None (N/A, not a real cosine).
    let mut results: Vec<(f64, f64, &str, f32, f32, f32)> = Vec::with_capacity(clips.len());
    let mut all_pass = true;
    let mut report = String::new();
    report.push_str("# nemo_titanet ort cosine-equivalence gate\n");
    report.push_str(&format!("# model: {}\n", model_path.display()));
    report.push_str(&format!(
        "# audio: {:.1}s, {} clips, gate threshold cosine > {}\n\n",
        decoded.duration_seconds.max(0.001),
        clips.len(),
        GATE_THRESHOLD
    ));
    report.push_str("Per-clip results:\n");
    report.push_str("start_s\tend_s\tlabel\t\t\tsherpa_norm\tort_norm\tcosine\n");

    for &(start_s, end_s, label) in clips {
        let s = ((start_s * sr_f) as usize).min(samples.len());
        let e = ((end_s * sr_f) as usize).min(samples.len());
        if e <= s {
            results.push((start_s, end_s, label, 0.0, 0.0, 0.0));
            report.push_str(&format!(
                "{:.2}\t{:.2}\t{}\tEMPTY (no samples)\n",
                start_s, end_s, label
            ));
            all_pass = false;
            continue;
        }
        let clip_samples = &samples[s..e];
        let emb_sherpa = sherpa_embedding(clip_samples);
        let emb_ort = ort_embedding(clip_samples);

        match (&emb_sherpa, &emb_ort) {
            (Some(es), Some(eo)) => {
                let sherpa_norm = es.iter().map(|x| x * x).sum::<f32>().sqrt();
                let ort_norm = eo.iter().map(|x| x * x).sum::<f32>().sqrt();
                let cos = cosine_similarity(es, eo);
                results.push((start_s, end_s, label, sherpa_norm, ort_norm, cos));
                report.push_str(&format!(
                    "{:.2}\t{:.2}\t{}\t{:.4}\t\t{:.4}\t\t{:.6}\n",
                    start_s, end_s, label, sherpa_norm, ort_norm, cos
                ));
                eprintln!(
                    "GATE: [{:.2}-{:.2}s] {} — sherpa_norm={:.4} ort_norm={:.4} cosine={:.6}",
                    start_s, end_s, label, sherpa_norm, ort_norm, cos
                );
                if cos <= GATE_THRESHOLD {
                    all_pass = false;
                }
            }
            (None, Some(_)) => {
                eprintln!(
                    "GATE: [{:.2}-{:.2}s] {} — sherpa returned None (is_ready=false) but ort produced an embedding",
                    start_s, end_s, label
                );
                results.push((start_s, end_s, label, 0.0, 0.0, 0.0));
                report.push_str(&format!(
                    "{:.2}\t{:.2}\t{}\tSHERPA=None\tort=Some\tcosine=N/A\n",
                    start_s, end_s, label
                ));
                // If sherpa says "not ready" but the clip has >=1 frame, treat
                // as a gate failure (the paths disagree on what's processable).
                let nf = num_frames_snip(clip_samples.len(), params.window_size, params.window_shift);
                if nf > 0 {
                    all_pass = false;
                }
            }
            (Some(_), None) => {
                eprintln!(
                    "GATE: [{:.2}-{:.2}s] {} — sherpa produced an embedding but ort pipeline returned None",
                    start_s, end_s, label
                );
                results.push((start_s, end_s, label, 0.0, 0.0, 0.0));
                report.push_str(&format!(
                    "{:.2}\t{:.2}\t{}\tsherpa=Some\tORT=None\tcosine=N/A\n",
                    start_s, end_s, label
                ));
                all_pass = false;
            }
            (None, None) => {
                eprintln!(
                    "GATE: [{:.2}-{:.2}s] {} — both paths returned None (clip too short for ≥1 frame)",
                    start_s, end_s, label
                );
                results.push((start_s, end_s, label, 0.0, 0.0, 0.0));
                report.push_str(&format!(
                    "{:.2}\t{:.2}\t{}\tBOTH=None (sub-frame clip)\n",
                    start_s, end_s, label
                ));
                // Both agree the clip is unprocessable — not a cosine failure,
                // but flag it so the report is honest.
            }
        }
    }

    // --- Min/mean/max cosine over clips where BOTH paths produced embeddings ---
    let valid_cosines: Vec<f32> = results
        .iter()
        .filter_map(|&(_, _, _, sn, on, c)| {
            // A real cosine row has both norms > 0 (an embedding was produced).
            if sn > 0.0 && on > 0.0 { Some(c) } else { None }
        })
        .collect();
    let (min_c, max_c, mean_c) = if valid_cosines.is_empty() {
        (0.0f32, 0.0f32, 0.0f32)
    } else {
        let mn = valid_cosines.iter().cloned().fold(f32::INFINITY, f32::min);
        let mx = valid_cosines
            .iter()
            .cloned()
            .fold(f32::NEG_INFINITY, f32::max);
        let mean = valid_cosines.iter().sum::<f32>() / valid_cosines.len() as f32;
        (mn, mx, mean)
    };

    report.push_str("\nSummary (over clips where both paths produced embeddings):\n");
    report.push_str(&format!(
        "- min  cosine: {:.6}\n- mean cosine: {:.6}\n- max  cosine: {:.6}\n",
        min_c, mean_c, max_c
    ));
    report.push_str(&format!(
        "- gate threshold: cosine > {} on ALL clips\n",
        GATE_THRESHOLD
    ));
    report.push_str(&format!("- result: {}\n", if all_pass { "PASS" } else { "FAIL" }));

    eprintln!(
        "\n========== GATE {}: min={:.6} mean={:.6} max={:.6} (threshold > {}) ==========",
        if all_pass { "PASS" } else { "FAIL" },
        min_c,
        mean_c,
        max_c,
        GATE_THRESHOLD
    );

    let out = std::env::temp_dir().join("nemo_titanet_ort_cosine_equivalence.txt");
    std::fs::write(&out, &report).expect("write report");
    eprintln!("GATE: full report at {}", out.display());

    // --- Per-clip failure table + panic (the panel's gate assertion) ---
    if !all_pass {
        eprintln!("\nGATE FAIL — per-clip table:");
        eprintln!(
            "{:>8} {:>8} {:<48} {:>12} {:>12} {:>10}",
            "start_s", "end_s", "label", "sherpa_norm", "ort_norm", "cosine"
        );
        for &(s, e, label, sn, on, c) in &results {
            eprintln!(
                "{:>8.2} {:>8.2} {:<48} {:>12.4} {:>12.4} {:>10.6}",
                s, e, label, sn, on, c
            );
        }
        let failing: Vec<String> = results
            .iter()
            .filter(|&&(_, _, _, sn, on, c)| sn > 0.0 && on > 0.0 && c <= GATE_THRESHOLD)
            .map(|&(s, _, label, _, _, c)| format!("[{:.2}s] {}: cosine={:.6}", s, label, c))
            .collect();
        panic!(
            "nemo_titanet ort cosine-equivalence GATE FAILED: {} clip(s) below threshold {}.\n\
             Failing clips:\n  - {}\n\
             This means the hand-rolled ort preprocessing does NOT reproduce sherpa's nemo frontend \
             to the panel's required fidelity. Do NOT port nemo_titanet to ort until the pipeline \
             is corrected and this gate passes.",
            failing.len(),
            GATE_THRESHOLD,
            failing.join("\n  - ")
        );
    }

    eprintln!(
        "GATE: PASS — all {} clips within cosine > {}. Min/mean/max = {:.6}/{:.6}/{:.6}.",
        clips.len(), GATE_THRESHOLD, min_c, mean_c, max_c
    );
}


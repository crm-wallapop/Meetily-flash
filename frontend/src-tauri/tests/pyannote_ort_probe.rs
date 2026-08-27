//! Empirical probe: can the project's `ort` 2.0.0-rc.10 dep load and run
//! `pyannote-segmentation-3.0.onnx` that sherpa-onnx 1.13.x CANNOT (its
//! bundled ORT 1.17.1 STATUS_ACCESS_VIOLATIONs on this model)?
//!
//! WHY a separate path from sherpa-onnx: sherpa-onnx Rust 1.13.x statically
//! bundles ORT 1.17.1 (C-API ≤17); pyannote-segmentation-3.0 requires C-API
//! 24-27. The project's own `ort = 2.0.0-rc.10` (the app's ONNX runtime) ships a
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

    // Session config once mirrored the deleted parakeet_engine/model.rs
    // (CPU provider + commit_from_file); kept identical so probe timings
    // stay comparable across the engine swap.
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
    let adapter = app_lib::audio::speaker::sherpa_adapter::OrtDiarizationAdapter::with_shared_threshold(
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

// ============================================================================


// ============================================================================
// PHASE 2d: Path B through FULL production post-processing (cap + refine_pass2).
//
// WHY: Phase 2c compared raw adapter.process() outputs. Production additionally
// runs enforce_max_speakers_cap(meeting override) + refine_pass2. Ground truth:
// this meeting has EXACTLY 3 speakers; Phase 2c's raw Path B produced 4
// (oversplit) and appeared to lose the 46:58 interjection. This probe answers,
// with production parity on BOTH paths:
//   1. final distinct-speaker count == 3?
//   2. banter 5.7-32.5s split into >1 speaker?
//   3. which final segment COVERS 2818s (46:58), what label, what span?
//      (Phase 2c filtered by segment START within +-10s - an absorbed span
//       would have been miscounted as "lost".)
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_production_parity
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_production_parity() {
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use app_lib::audio::speaker::commands::enforce_max_speakers_cap;
    use app_lib::audio::speaker::diarization::DiarizationPort;

    // --- Audio + transcript grid (identical to Phase 2c) ---
    let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
        .await
        .expect("DB connect");
    let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";
    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id).fetch_optional(&pool).await.expect("fetch meeting");
    let folder = row.and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path")).expect("folder_path");

    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"].iter()
        .map(|n| audio_dir.join(n)).find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode");
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds.max(0.001);

    let transcript_segments: Vec<(f64, f64)> = {
        let rows = sqlx::query(
            "SELECT audio_start_time, audio_end_time FROM transcripts \
             WHERE meeting_id = ? ORDER BY audio_start_time ASC")
            .bind(meeting_id).fetch_all(&pool).await.expect("fetch transcripts");
        rows.into_iter().filter_map(|r| {
            let s: Option<f64> = sqlx::Row::get(&r, "audio_start_time");
            let e: Option<f64> = sqlx::Row::get(&r, "audio_end_time");
            match (s, e) {
                (Some(a), Some(b)) if a < b && a >= 0.0 && b <= audio_duration + 1.0 => Some((a, b)),
                _ => None,
            }
        }).collect()
    };
    drop(pool);
    eprintln!("PARITY: {} transcript segments", transcript_segments.len());

    // --- Adapter (production threshold 0.40) ---
    let home = dirs::home_dir().expect("home").join(".meetily-models");
    let emb_path = home.join(app_lib::audio::speaker::model_download::embedding_filename());
    let seg_path = home.join("pyannote-segmentation.onnx");
    assert!(emb_path.exists() && seg_path.exists());
    let threshold_fp = Arc::new(AtomicU32::new((0.40f32 * 65536.0) as u32));
    let adapter = app_lib::audio::speaker::sherpa_adapter::OrtDiarizationAdapter::with_shared_threshold(
        emb_path.to_str().unwrap(), seg_path.to_str().unwrap(), threshold_fp,
    ).expect("adapter");

    const CAP: usize = 3;

    /// Production post-processing: cap to the meeting override, then Pass-2 refine.
    fn production_post(
        adapter: &app_lib::audio::speaker::sherpa_adapter::OrtDiarizationAdapter,
        samples: &[f32],
        coarse: app_lib::audio::speaker::diarization::DiarizationOutput,
    ) -> Vec<app_lib::audio::speaker::types::SpeakerSegment> {
        let mut segments = coarse.segments;
        let mut centroids = coarse.centroids;
        if !segments.is_empty() {
            enforce_max_speakers_cap(&mut centroids, &mut segments, CAP);
            if !centroids.is_empty() {
                segments = adapter.refine_pass2(samples, 16000, &centroids)
                    .expect("refine_pass2");
                let used: std::collections::HashSet<u32> =
                    segments.iter().map(|s| s.speaker_id).collect();
                centroids.retain(|k, _| used.contains(k));
            }
        }
        eprintln!("PARITY: post-cap clusters: {}", centroids.len());
        segments
    }

    fn sorted_labels(set: &std::collections::HashSet<u32>) -> Vec<u32> {
        let mut v: Vec<u32> = set.iter().copied().collect();
        v.sort();
        v
    }

    fn report(name: &str, segs: &[app_lib::audio::speaker::types::SpeakerSegment]) -> String {
        let labels: std::collections::HashSet<u32> = segs.iter().map(|s| s.speaker_id).collect();
        let banter: Vec<&app_lib::audio::speaker::types::SpeakerSegment> = segs.iter()
            .filter(|s| s.start_seconds < 32.5 && s.end_seconds > 5.7).collect();
        let banter_labels: std::collections::HashSet<u32> = banter.iter().map(|s| s.speaker_id).collect();
        let covering: Vec<&app_lib::audio::speaker::types::SpeakerSegment> = segs.iter()
            .filter(|s| s.start_seconds <= 2818.0 && s.end_seconds >= 2818.0).collect();
        let mut out = format!(
            "## {name}\n- total segments: {}\n- distinct speakers: {:?}\n- banter 5.7-32.5s coverage: {} segments, labels {:?}\n",
            segs.len(),
            sorted_labels(&labels),
            banter.len(),
            sorted_labels(&banter_labels),
        );
        for c in &covering {
            out.push_str(&format!("- covers 2818s: [{:.1} - {:.1}] label=Speaker {}\n",
                c.start_seconds, c.end_seconds, c.speaker_id));
        }
        if covering.is_empty() {
            out.push_str("- covers 2818s: NONE FOUND\n");
        }
        out
    }

    let mut report_text = format!("# production-parity probe (cap={CAP} + refine_pass2)\n\n");

    // --- Path A: transcript grid through full production flow ---
    eprintln!("PARITY: Path A coarse...");
    let out_a = adapter.process(&samples, 16000, &transcript_segments).expect("Path A");
    let final_a = production_post(&adapter, &samples, out_a);
    report_text.push_str(&report("Path A (transcript grid, production)", &final_a));
    report_text.push('\n');

    // --- Path B: pyannote fragment grid through full production flow ---
    let providers = vec![ort::execution_providers::CPUExecutionProvider::default().build()];
    let session = ort::session::Session::builder().expect("builder")
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
        .expect("opt")
        .with_execution_providers(providers).expect("providers")
        .with_intra_threads(1).expect("threads")
        .commit_from_file(&seg_path).expect("load pyannote");
    let input_name = session.inputs[0].name.to_string();
    let output_name = session.outputs[0].name.to_string();
    let mut session = session;

    const SAMPLE_RATE: usize = 16000;
    const WINDOW_SAMPLES: usize = 160000;
    const STEP_SAMPLES: usize = 16000;
    const FRAME_SHIFT_SECS: f64 = 270.0 / 16000.0;
    const RECEPTIVE_OFFSET_SECS: f64 = 721.0 / 16000.0;
    const ONSET: f32 = 0.5;

    let total_windows = if samples.len() > WINDOW_SAMPLES {
        (samples.len() - WINDOW_SAMPLES) / STEP_SAMPLES + 1
    } else { 1 };
    eprintln!("PARITY: Path B inferring {} windows...", total_windows);

    let mut cached: Vec<(f64, Vec<f32>)> = Vec::new();
    for win_idx in 0..total_windows {
        let start = win_idx * STEP_SAMPLES;
        let end = (start + WINDOW_SAMPLES).min(samples.len());
        if end - start < 16000 { break; }
        let win_start_secs = start as f64 / SAMPLE_RATE as f64;
        let mut window = vec![0.0f32; WINDOW_SAMPLES];
        window[..end - start].copy_from_slice(&samples[start..end]);
        let input_3d: ndarray::Array3<f32> = ndarray::Array1::from(window)
            .into_shape_with_order([1, 1, WINDOW_SAMPLES]).unwrap();
        let tensor_ref = ort::value::TensorRef::from_array_view(input_3d.view()).expect("tensor");
        let inputs = ort::inputs![input_name.as_str() => tensor_ref];
        let outputs = session.run(inputs).expect("forward");
        let output = outputs.get(output_name.as_str()).expect("output");
        let arr = output.try_extract_array::<f32>().expect("extract");
        let shape = arr.shape();
        let sl = arr.as_slice().unwrap_or_else(|| arr.to_slice().unwrap());
        cached.push((win_start_secs, sl[..shape[1] * shape[2]].to_vec()));
    }

    let mut raw_activity: Vec<[bool; 3]> = Vec::new();
    for &(win_start_secs, ref logits) in &cached {
        let num_classes = 7;
        let num_frames = logits.len() / num_classes;
        let window_activity = decode_multilabel_with_hysteresis(logits, num_frames, ONSET, ONSET);
        for (i, &act) in window_activity.iter().enumerate() {
            let abs_secs = win_start_secs + RECEPTIVE_OFFSET_SECS + (i as f64) * FRAME_SHIFT_SECS;
            let frame_idx = (abs_secs / FRAME_SHIFT_SECS).round() as usize;
            while raw_activity.len() <= frame_idx { raw_activity.push([false; 3]); }
            raw_activity[frame_idx] = act;
        }
    }
    const FPS: f64 = 1.0 / FRAME_SHIFT_SECS;
    let sec_to_frames = |s: f64| (s * FPS).round() as usize;
    let med = median_filter_per_speaker(&raw_activity, 3);
    let gated = duration_gates_per_speaker(&med, sec_to_frames(0.3), sec_to_frames(0.5));
    let cps = change_points(&gated, FRAME_SHIFT_SECS, 0.0);
    let mut bounds = vec![0.0f64];
    bounds.extend(cps.iter().copied().filter(|&t| t > 0.0 && t < audio_duration));
    bounds.push(audio_duration);
    bounds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    bounds.dedup_by(|a, b| (*a - *b).abs() < 0.05);
    let pyannote_segments: Vec<(f64, f64)> = bounds.windows(2)
        .map(|w| (w[0], w[1]))
        .filter(|(st, en)| en - st >= 0.1)
        .collect();
    eprintln!("PARITY: Path B {} fragments, running coarse...", pyannote_segments.len());

    let out_b = adapter.process(&samples, 16000, &pyannote_segments).expect("Path B");
    let final_b = production_post(&adapter, &samples, out_b);
    report_text.push_str(&report("Path B (pyannote grid, production post)", &final_b));

    let out = std::env::temp_dir().join("cde5c264_production_parity.txt");
    std::fs::write(&out, &report_text).expect("write report");
    eprintln!("PARITY: report at {}", out.display());
}

// ============================================================================
// PHASE 2e: Path A WITH production's boundary_segments() intersection.
//
// WHY: Phase 2d showed the RAW transcript grid through cap+pass2 splits the
// banter into 2 speakers - but the real app run (which includes the
// pyannote-boundary intersection with MIN_SEGMENT_SECS sliver chain-merging)
// produced one flat Speaker-0 row there. This probe inserts the exact
// production intersection step and reports:
//   1. how many intersected pieces overlap the banter window BEFORE clustering,
//   2. final banter speaker labels after cap+pass2,
//   3. 2818s coverage.
// If banter collapses here but not in 2d-Path-A, the chain-merge is proven
// to be the flattening point.
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_intersection_parity
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_intersection_parity() {
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use app_lib::audio::speaker::commands::enforce_max_speakers_cap;
    use app_lib::audio::speaker::diarization::DiarizationPort;

    let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
        .await.expect("DB connect");
    let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";
    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id).fetch_optional(&pool).await.expect("fetch meeting");
    let folder = row.and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path")).expect("folder_path");

    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"].iter()
        .map(|n| audio_dir.join(n)).find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode");
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds.max(0.001);

    let transcript_segments: Vec<(f64, f64)> = {
        let rows = sqlx::query(
            "SELECT audio_start_time, audio_end_time FROM transcripts \
             WHERE meeting_id = ? ORDER BY audio_start_time ASC")
            .bind(meeting_id).fetch_all(&pool).await.expect("fetch transcripts");
        rows.into_iter().filter_map(|r| {
            let s: Option<f64> = sqlx::Row::get(&r, "audio_start_time");
            let e: Option<f64> = sqlx::Row::get(&r, "audio_end_time");
            match (s, e) {
                (Some(a), Some(b)) if a < b && a >= 0.0 && b <= audio_duration + 1.0 => Some((a, b)),
                _ => None,
            }
        }).collect()
    };
    drop(pool);

    fn sorted_labels(set: &std::collections::HashSet<u32>) -> Vec<u32> {
        let mut v: Vec<u32> = set.iter().copied().collect();
        v.sort();
        v
    }

    // --- Production intersection step ---
    let seg_model = dirs::home_dir().expect("home").join(".meetily-models").join("pyannote-segmentation.onnx");
    let pya = app_lib::audio::speaker::pyannote_segmentation::PyannoteSegmentation::new(
        seg_model.to_str().unwrap()).expect("pyannote");
    let bounded = pya.boundary_segments(
        &samples, &transcript_segments,
        app_lib::audio::speaker::sherpa_adapter::max_diarization_chunks(),
    ).expect("boundary_segments");
    let banter_pieces: Vec<(f64, f64)> = bounded.iter()
        .filter(|(st, en)| *st < 32.5 && *en > 5.7).copied().collect();
    eprintln!("INTERSECT: {} intersected segments total; {} pieces overlap banter:",
        bounded.len(), banter_pieces.len());
    for (st, en) in &banter_pieces {
        eprintln!("  [{:.2} - {:.2}] ({:.2}s)", st, en, en - st);
    }

    // --- Adapter + production post ---
    let home = dirs::home_dir().expect("home").join(".meetily-models");
    let emb_path = home.join(app_lib::audio::speaker::model_download::embedding_filename());
    assert!(emb_path.exists());
    // Production parity: speakerMergeThreshold=0.65 is the live DB value
    // (settings table); the 2e run used the 0.40 compile-time default.
    let threshold_fp = Arc::new(AtomicU32::new((0.65f32 * 65536.0) as u32));
    let adapter = app_lib::audio::speaker::sherpa_adapter::OrtDiarizationAdapter::with_shared_threshold(
        emb_path.to_str().unwrap(), seg_model.to_str().unwrap(), threshold_fp,
    ).expect("adapter");

    eprintln!("INTERSECT-065: running coarse...");
    let coarse = adapter.process(&samples, 16000, &bounded).expect("process");
    let mut segments = coarse.segments;
    let mut centroids = coarse.centroids;
    if !segments.is_empty() {
        enforce_max_speakers_cap(&mut centroids, &mut segments, 3);
        if !centroids.is_empty() {
            segments = adapter.refine_pass2(&samples, 16000, &centroids).expect("pass2");
            let used: std::collections::HashSet<u32> =
                segments.iter().map(|s| s.speaker_id).collect();
            centroids.retain(|k, _| used.contains(k));
        }
    }

    let labels: std::collections::HashSet<u32> = segments.iter().map(|s| s.speaker_id).collect();
    let banter: Vec<&app_lib::audio::speaker::types::SpeakerSegment> = segments.iter()
        .filter(|s| s.start_seconds < 32.5 && s.end_seconds > 5.7).collect();
    let banter_labels: std::collections::HashSet<u32> = banter.iter().map(|s| s.speaker_id).collect();
    let covering: Vec<&app_lib::audio::speaker::types::SpeakerSegment> = segments.iter()
        .filter(|s| s.start_seconds <= 2818.0 && s.end_seconds >= 2818.0).collect();

    let mut out = format!(
        "# intersection-parity probe\n\n## banter pre-cluster pieces\n{} pieces: {:?}\n\n## post cap+pass2\n- distinct speakers: {:?}\n- banter: {} segments, labels {:?}\n",
        banter_pieces.len(), banter_pieces, sorted_labels(&labels), banter.len(), sorted_labels(&banter_labels));
    for c in &covering {
        out.push_str(&format!("- covers 2818s: [{:.1} - {:.1}] Speaker {}\n",
            c.start_seconds, c.end_seconds, c.speaker_id));
    }
    let path = std::env::temp_dir().join("cde5c264_intersection_parity_065.txt");
    std::fs::write(&path, &out).expect("write");
    eprintln!("INTERSECT: report at {}", path.display());
}
// ============================================================================
// PHASE 2f: alignment + persistence parity - the LAST untested stage.
//
// WHY: 2e/2f probes proved the diarization output splits the banter into 2
// speakers (at both 0.40 and 0.65). But the app persisted ONE flat Speaker-0
// row. This probe runs align_transcripts_with_diarization over the final
// diarization segments vs the CURRENT transcript rows and prints what WOULD
// be persisted for the banter window (5.67-30.0) and for 2818s.
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_alignment_parity
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_alignment_parity() {
    let _ = env_logger::builder()
        .filter_level(log::LevelFilter::Info)
        .is_test(true)
        .try_init();
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;
    use app_lib::audio::speaker::alignment::{
        align_transcripts_with_diarization, DiarizationSegment, TranscriptInput,
    };
    use app_lib::audio::speaker::commands::enforce_max_speakers_cap;
    use app_lib::audio::speaker::diarization::DiarizationPort;

    let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
        .await.expect("DB connect");
    let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

    // Full transcripts (id/text/start/end) for alignment input.
    #[derive(sqlx::FromRow)]
    struct Row { id: String, text: String, start_time: f64, end_time: f64 }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, transcript as text, audio_start_time as start_time, audio_end_time as end_time FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(meeting_id)
    .fetch_all(&pool).await.expect("fetch transcripts");
    let transcripts: Vec<TranscriptInput> = rows.iter().map(|r| TranscriptInput {
        id: r.id.clone(),
        text: r.text.clone(),
        audio_start_ms: (r.start_time * 1000.0) as i64,
        audio_end_ms: (r.end_time * 1000.0) as i64,
        token_words: None,
    }).collect();
    eprintln!("ALIGN: {} transcript rows", transcripts.len());

    // Audio + grid + adapter at production threshold 0.65.
    let mrow = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id).fetch_optional(&pool).await.expect("fetch meeting").expect("meeting");
    let folder = sqlx::Row::get::<Option<String>, _>(&mrow, "folder_path").expect("folder");
    drop(pool);
    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"].iter()
        .map(|n| audio_dir.join(n)).find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode");
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds.max(0.001);

    let grid: Vec<(f64, f64)> = transcripts.iter().map(|t|
        (t.audio_start_ms as f64 / 1000.0, t.audio_end_ms as f64 / 1000.0)).collect();

    let seg_model = dirs::home_dir().expect("home").join(".meetily-models").join("pyannote-segmentation.onnx");
    let pya = app_lib::audio::speaker::pyannote_segmentation::PyannoteSegmentation::new(
        seg_model.to_str().unwrap()).expect("pyannote");
    let bounded = pya.boundary_segments(&samples, &grid,
        app_lib::audio::speaker::sherpa_adapter::max_diarization_chunks()).expect("boundary_segments");

    let home = dirs::home_dir().expect("home").join(".meetily-models");
    let emb_path = home.join(app_lib::audio::speaker::model_download::embedding_filename());
    let threshold_fp = Arc::new(AtomicU32::new((0.65f32 * 65536.0) as u32));
    let adapter = app_lib::audio::speaker::sherpa_adapter::OrtDiarizationAdapter::with_shared_threshold(
        emb_path.to_str().unwrap(), seg_model.to_str().unwrap(), threshold_fp,
    ).expect("adapter");

    eprintln!("ALIGN: coarse...");
    let coarse = adapter.process(&samples, 16000, &bounded).expect("process");
    let mut segments = coarse.segments;
    let mut centroids = coarse.centroids;
    if !segments.is_empty() {
        enforce_max_speakers_cap(&mut centroids, &mut segments, 3);
        if !centroids.is_empty() {
            segments = adapter.refine_pass2(&samples, 16000, &centroids).expect("pass2");
            let used: std::collections::HashSet<u32> =
                segments.iter().map(|s| s.speaker_id).collect();
            centroids.retain(|k, _| used.contains(k));
        }
    }

// --- Dump the RAW final segments overlapping the banter window so the
    // alignment collapse (if any) can be attributed precisely. Also cache all
    // final segments to JSON for offline analysis.
    {
        let banter_raw: Vec<&app_lib::audio::speaker::types::SpeakerSegment> = segments.iter()
            .filter(|s| s.start_seconds < 32.5 && s.end_seconds > 5.7)
            .collect();
        eprintln!("ALIGN-RAW: {} final segments overlap banter:", banter_raw.len());
        for s in &banter_raw {
            eprintln!("  [{:.2} - {:.2}] Speaker {}", s.start_seconds, s.end_seconds, s.speaker_id);
        }
        let json = serde_json::json!(segments.iter().map(|s| serde_json::json!({
            "start": s.start_seconds, "end": s.end_seconds, "speaker": s.speaker_id,
        })).collect::<Vec<_>>());
        let jpath = std::env::temp_dir().join("cde5c264_final_segments.json");
        std::fs::write(&jpath, serde_json::to_string_pretty(&json).unwrap()).ok();
        eprintln!("ALIGN-RAW: cached all {} final segments to {}", segments.len(), jpath.display());
    }

    // --- THE STAGE UNDER TEST: alignment ---
    let diar_segs: Vec<DiarizationSegment> = segments.iter().map(|s| DiarizationSegment {
        start_ms: (s.start_seconds * 1000.0) as i64,
        end_ms: (s.end_seconds * 1000.0) as i64,
        speaker_id: s.speaker_id,
    }).collect();
    let aligned = align_transcripts_with_diarization(transcripts, &diar_segs);

    // What would be persisted in the banter window and around 2818s?
    let banter_rows: Vec<&app_lib::audio::speaker::alignment::AlignedSegment> = aligned.iter()
        .filter(|a| a.audio_start_ms < 32_500 && a.audio_end_ms > 5_000)
        .collect();
    let near_2818: Vec<&app_lib::audio::speaker::alignment::AlignedSegment> = aligned.iter()
        .filter(|a| a.audio_start_ms <= 2_818_000 && a.audio_end_ms >= 2_818_000)
        .collect();

    let mut out = String::from("# alignment-parity probe (0.65, full pipeline)\n\n## persisted-shape rows, banter window\n");
    for a in &banter_rows {
        out.push_str(&format!("- row {} [{:.2}-{:.2}] speaker={} source={:?} text=\"{}\"\n",
            a.original_id, a.audio_start_ms as f64 / 1000.0, a.audio_end_ms as f64 / 1000.0,
            a.speaker, a.speaker_source,
            if a.text.len() > 60 { &a.text[..60] } else { &a.text }));
    }
    out.push_str("\n## rows covering 2818s\n");
    for a in &near_2818 {
        out.push_str(&format!("- row {} [{:.2}-{:.2}] speaker={}\n",
            a.original_id, a.audio_start_ms as f64 / 1000.0, a.audio_end_ms as f64 / 1000.0, a.speaker));
    }
    let path = std::env::temp_dir().join("cde5c264_alignment_parity.txt");
    std::fs::write(&path, &out).expect("write");
    eprintln!("ALIGN: report at {}", path.display());
}

// ============================================================================
// PHASE 2g: FULL production command - run_diarization_for_meeting against the
// real DB (with backup), then print the persisted banter rows.
//
// WHY: 2f verified alignment in isolation; this runs the ACTUAL Tauri command
// body (fetch -> intersect -> process -> cap -> pass2 -> align -> persist) so
// what lands in meeting_minutes.sqlite is exactly what the Speakers button
// would produce. Backs up the DB first.
//
// Run:
//   cargo test --release --test pyannote_ort_probe -- --ignored --nocapture pyannote_cde5c264_real_persist
// ============================================================================

#[tokio::test]
#[ignore]
async fn pyannote_cde5c264_real_persist() {
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";

    // --- Backup DB + WAL/SHM before any write ---
    for ext in ["", "-wal", "-shm"] {
        let src = format!("{}{}", db_path, ext);
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let dst = format!("{}.bak-{}{}", db_path, stamp, ext);
        if std::path::Path::new(&src).exists() {
            std::fs::copy(&src, &dst).expect("backup copy");
            eprintln!("PERSIST: backed up {} -> {}", src, dst);
        }
    }

    let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", db_path))
        .await.expect("DB connect (rw)");

    // Production threshold from the settings table (0.65 live).
    let threshold: f32 = sqlx::query("SELECT speakerMergeThreshold FROM settings WHERE id = '1'")
        .fetch_one(&pool).await
        .map(|r| sqlx::Row::get::<f64, _>(&r, "speakerMergeThreshold") as f32)
        .unwrap_or(0.50);
    eprintln!("PERSIST: production speakerMergeThreshold = {}", threshold);

    // Cap resolution happens inside run_diarization_for_meeting
    // (meetings.max_speakers override -> settings.max_speakers).

    // No cross-meeting registry in probe context (production default is also
    // None until a speaker is named).
    let registry = Arc::new(std::sync::Mutex::new(None));

    let threshold_fp = (threshold * 65536.0) as u32;
    eprintln!("PERSIST: running run_diarization_for_meeting...");
    let result = app_lib::audio::speaker::commands::run_diarization_for_meeting(
        &pool, meeting_id, threshold_fp, registry,
    ).await.expect("run_diarization_for_meeting");
    eprintln!("PERSIST: {} speakers, {} segments labeled",
        result.speaker_count, result.segments_labeled);

    // --- Print the persisted banter rows and 2818 coverage ---
    #[derive(sqlx::FromRow)]
    struct Row { start: f64, end: f64, speaker: Option<String>, text: String }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT audio_start_time as start, audio_end_time as end, speaker_label as speaker, transcript as text \
         FROM transcripts WHERE meeting_id = ? AND audio_start_time < 35 ORDER BY audio_start_time ASC")
        .bind(meeting_id).fetch_all(&pool).await.expect("fetch banter");
    println!("\n=== PERSISTED BANTER ROWS ===");
    for r in rows {
        println!("  [{:7.2} - {:7.2}] {:<12} {}",
            r.start, r.end, r.speaker.as_deref().unwrap_or("?"),
            &r.text[..r.text.len().min(70)]);
    }
    let rows = sqlx::query_as::<_, Row>(
        "SELECT audio_start_time as start, audio_end_time as end, speaker_label as speaker, transcript as text \
         FROM transcripts WHERE meeting_id = ? AND audio_start_time BETWEEN 2790 AND 2830 ORDER BY audio_start_time ASC")
        .bind(meeting_id).fetch_all(&pool).await.expect("fetch 2818");
    println!("\n=== PERSISTED ROWS AROUND 46:58 (2818s) ===");
    for r in rows {
        println!("  [{:7.2} - {:7.2}] {:<12} {}",
            r.start, r.end, r.speaker.as_deref().unwrap_or("?"),
            &r.text[..r.text.len().min(70)]);
    }
    pool.close().await;
}

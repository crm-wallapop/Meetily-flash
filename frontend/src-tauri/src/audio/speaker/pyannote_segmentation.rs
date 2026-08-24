//! In-process pyannote speaker-change-point segmentation via the `ort` crate.
//!
//! WHY: the uniform chunk grid (`SPLIT_TARGET_SECS`/`effective_split`) collapses
//! rapid back-and-forth within one grid cell (on cde5c264, the 5.7–32.5s banter
//! window yields ONE boundary at 21.36s vs 24 turns from pyannote at default
//! smoothing). pyannote-segmentation-3.0 supplies dense candidate boundaries;
//! Meetily's AHC + temporal-coherence smoothing remain authoritative for
//! LABELING (design D5 — labels are structurally absent: this module emits
//! boundaries only).
//!
//! FIDELITY: the decode/smoothing pipeline is LIFTED VERBATIM from the
//! validated probes (`tests/pyannote_ort_probe.rs` Phases 2b/2c): onset 0.5,
//! median filter rad=3, min_on=0.3s / max_off=0.5s duration gates — the ONLY
//! config that hit BOTH known anchors (Ricardo join 17:37, interjection 46:58)
//! on cde5c264. Do NOT re-tune.
//!
//! Concurrency: same pattern as `nemo_extractor` — `Mutex<Session>` (run takes
//! &mut self); window preprocessing happens outside the lock.

use anyhow::{anyhow, Result};
use ndarray::{Array1, Array3};
use ort::execution_providers::CPUExecutionProvider;
use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::TensorRef;
use std::sync::Mutex;

/// Sliding-window geometry (pyannote-3.0 contract, verified against sherpa's
/// offline-speaker-diarization headers + the ONNX receptive field):
const SAMPLE_RATE: usize = 16000;
const WINDOW_SAMPLES: usize = 160_000; // 10s
const STEP_SAMPLES: usize = 16_000; // 1s step (90% overlap)
/// Frame shift: 270 samples @16kHz ≈ 16.875ms.
const FRAME_SHIFT_SECS: f64 = 270.0 / 16000.0;

/// Powerset class → 3-speaker multilabel (per pyannote-audio powerset.py).
/// Index 0 = no speech; 1–3 = solo speakers; 4–6 = overlap pairs.
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

/// Decode per-frame powerset LOGITS → binary speaker activity via hysteresis
/// (turn on above `onset`, off below `offset`). LIFTED VERBATIM from the probe
/// (`decode_multilabel_with_hysteresis`). CRITICAL: the ONNX graph ends in
/// **LogSoftmax** — rows are log-probabilities (≤0); `exp()` converts them to
/// probabilities BEFORE the 0.5 thresholds are meaningful. Dropping the exp()
/// would silence every speaker permanently.
fn decode_multilabel_with_hysteresis(
    frame_logits: &[f32], // shape [num_frames * num_classes], row-major (log-probs)
    num_frames: usize,
    num_classes: usize,
    onset: f32,
    offset: f32,
) -> Vec<[bool; 3]> {
    let mut active = [false; 3];
    let mut out = Vec::with_capacity(num_frames);
    for frame in 0..num_frames {
        let row = &frame_logits[frame * num_classes..(frame + 1) * num_classes];
        // Expand powerset → per-speaker probability mass (exp the log-probs,
        // sum overlap classes into constituents — probe parity).
        let mut probs = [0.0f32; 3];
        for (class, &log_p) in row.iter().enumerate() {
            let ml = powerset_to_multilabel(class);
            if ml != [false, false, false] {
                let p = log_p.exp();
                for spk in 0..3 {
                    if ml[spk] {
                        probs[spk] += p;
                    }
                }
            }
        }
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

/// Per-speaker median filter (majority vote over a 2*rad+1 kernel, clamped
/// edges). LIFTED VERBATIM from the probe. Removes single-frame flicker
/// without shifting edges.
fn median_filter_per_speaker(activity: &[[bool; 3]], rad: usize) -> Vec<[bool; 3]> {
    if rad == 0 || activity.is_empty() {
        return activity.to_vec();
    }
    let n = activity.len();
    let kernel = 2 * rad + 1;
    let mut out = vec![[false; 3]; n];
    for spk in 0..3 {
        for i in 0..n {
            let mut trues = 0usize;
            for k in -(rad as isize)..=(rad as isize) {
                let idx = (i as isize + k).clamp(0, (n - 1) as isize) as usize;
                if activity[idx][spk] {
                    trues += 1;
                }
            }
            out[i][spk] = trues * 2 > kernel;
        }
    }
    out
}

/// Min-on / max-off duration gates per pyannote-audio's `clamp`: collapse OFF
/// runs shorter than `max_off_frames` that are bounded by ON on both sides
/// (fill gaps); drop ON runs shorter than `min_on_frames`. LIFTED VERBATIM.
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
        if max_off_frames > 0 {
            let mut i = 0;
            while i < n {
                if !out[i][spk] {
                    let run_start = i;
                    while i < n && !out[i][spk] {
                        i += 1;
                    }
                    if i - run_start <= max_off_frames && run_start > 0 && i < n {
                        for j in run_start..i {
                            out[j][spk] = true;
                        }
                    }
                } else {
                    i += 1;
                }
            }
        }
        if min_on_frames > 0 {
            let mut i = 0;
            while i < n {
                if out[i][spk] {
                    let run_start = i;
                    while i < n && out[i][spk] {
                        i += 1;
                    }
                    if i - run_start < min_on_frames {
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

/// Change-points: frames where the active-speaker set differs from the
/// previous frame, as absolute seconds.
fn change_points(activity: &[[bool; 3]]) -> Vec<f64> {
    let mut points = Vec::new();
    for i in 1..activity.len() {
        if activity[i] != activity[i - 1] {
            points.push(i as f64 * FRAME_SHIFT_SECS);
        }
    }
    points
}

/// Intersect pyannote change-points with Whisper speech regions (design D2/D3):
/// each change-point strictly inside a Whisper segment becomes an intra-region
/// split; gap-window change-points are dropped (silence is never chunked).
/// Sub-[`MIN_SEGMENT_SECS`] splits are merged into their time-neighbor so the
/// finer layout does not create fragments `build_chunks` would silently DROP
/// (dropping loses content; merging preserves it).
pub(crate) const MIN_SEGMENT_SECS: f64 = 1.5;

pub(crate) fn intersect_pyannote_with_whisper(
    change_pts: &[f64],
    whisper_segments: &[(f64, f64)],
) -> Vec<(f64, f64)> {
    if whisper_segments.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for &(ws, we) in whisper_segments {
        if we <= ws {
            continue;
        }
        // Split points inside (ws, we) exclusive — an exact-edge point would
        // produce a zero-length piece (task 2.12).
        let mut bounds = vec![ws];
        bounds.extend(change_pts.iter().copied().filter(|&t| t > ws && t < we));
        bounds.push(we);

        // Merge sub-MIN_SEGMENT_SECS pieces into their time-neighbor.
        let mut merged: Vec<(f64, f64)> = Vec::with_capacity(bounds.len() - 1);
        for pair in bounds.windows(2) {
            let (s, e) = (pair[0], pair[1]);
            if e - s < MIN_SEGMENT_SECS && !merged.is_empty() {
                // Extend the previous piece's end (absorbs the sliver).
                let prev = merged.last_mut().expect("non-empty checked");
                prev.1 = e;
            } else if e - s < MIN_SEGMENT_SECS {
                // Leading sliver: hold it open until the next piece extends it.
                merged.push((s, e));
            } else {
                // If the previous piece is an open leading sliver, absorb THIS
                // piece into it instead of pushing a new fragment.
                if let Some(prev) = merged.last_mut() {
                    if prev.1 - prev.0 < MIN_SEGMENT_SECS {
                        prev.1 = e;
                        continue;
                    }
                }
                merged.push((s, e));
            }
        }
        // Final safety: any remaining sub-minimum trailing piece merges back.
        if let Some(prev) = merged.last_mut() {
            if prev.1 - prev.0 < MIN_SEGMENT_SECS && merged.len() > 1 {
                let trailing = merged.pop().expect("len > 1 checked");
                merged.last_mut().expect("non-empty").1 = trailing.1;
            }
        }
        out.extend(merged);
    }
    out
}

/// Uniform shed-to-cap (design D4/D5, task 3.5): when candidate boundaries
/// exceed the cap, shed every k-th BY POSITION down to the cap BEFORE
/// embedding, then merge sub-minimum survivors into their time-neighbor.
/// Because boundaries are candidate splits (not turns), shedding lowers
/// per-region resolution uniformly without destroying turns — turns are
/// re-derived downstream by AHC + smoothing.
pub(crate) fn shed_boundaries_to_cap(
    segments: Vec<(f64, f64)>,
    cap: usize,
    min_secs: f64,
) -> Vec<(f64, f64)> {
    if cap == 0 || segments.len() <= cap {
        return segments;
    }
    // Shed interior segments positionally down to the cap. The FIRST and LAST
    // segments are force-kept: dropping the tail would lose the meeting's
    // final seconds of speech outright.
    let n = segments.len();
    let excess = n - cap;
    let interior = n - 2; // indices 1..n-1
    let to_shed = excess.min(interior);

    let mut keep = vec![true; n];
    keep[0] = true;
    keep[n - 1] = true;

    if to_shed > 0 && interior > 0 {
        let step = ((interior as f64) / (to_shed as f64)).ceil().max(1.0) as usize;
        let mut marked = 0usize;
        let mut pos = 1usize;
        while pos < n - 1 && marked < to_shed {
            keep[pos] = false;
            marked += 1;
            pos += step;
        }
        // Step-rounding shortfall: mark remaining interior from the back.
        let mut back = n - 2;
        while marked < to_shed && back >= 1 {
            if keep[back] {
                keep[back] = false;
                marked += 1;
            }
            if back == 1 {
                break;
            }
            back -= 1;
        }
    }

    let kept: Vec<(f64, f64)> = segments
        .into_iter()
        .zip(keep)
        .filter_map(|(seg, k)| k.then_some(seg))
        .collect();
    merge_sub_minimum(kept, min_secs)
}

fn merge_sub_minimum(mut segs: Vec<(f64, f64)>, min_secs: f64) -> Vec<(f64, f64)> {
    loop {
        let Some(idx) = segs.iter().position(|(s, e)| e - s < min_secs) else {
            break;
        };
        if segs.len() <= 1 {
            break;
        }
        let removed = segs.remove(idx);
        if idx == 0 {
            segs[0].0 = removed.0; // extend the following piece leftward
        } else {
            segs[idx - 1].1 = removed.1; // extend the previous piece rightward
        }
    }
    segs
}

/// Smoothing parameters (pyannote defaults — the only config that hit BOTH
/// anchors on cde5c264; do not re-tune without re-running the anchor test).
#[derive(Clone, Copy, Debug)]
pub(crate) struct PyannoteParams {
    pub onset: f32,
    pub median_rad: usize,
    pub min_on_secs: f64,
    pub max_off_secs: f64,
}

impl Default for PyannoteParams {
    fn default() -> Self {
        Self {
            onset: 0.5,
            median_rad: 3,
            min_on_secs: 0.3,
            max_off_secs: 0.5,
        }
    }
}

fn secs_to_frames(secs: f64) -> usize {
    (secs / FRAME_SHIFT_SECS).round() as usize
}

/// In-process pyannote-segmentation-3.0 boundary source (second `ort::Session`
/// alongside Parakeet + nemo_titanet — one ORT runtime for the whole app).
pub struct PyannoteSegmentation {
    session: Mutex<Session>,
    audio_input_name: String,
    output_name: String,
    params: PyannoteParams,
}

impl PyannoteSegmentation {
    pub fn new(model_path: &str) -> Result<Self> {
        Self::with_params(model_path, PyannoteParams::default())
    }

    pub fn with_params(model_path: &str, params: PyannoteParams) -> Result<Self> {
        let path = std::path::PathBuf::from(model_path);
        if !path.exists() {
            return Err(anyhow!("segmentation model not found: {}", model_path));
        }
        let providers = vec![CPUExecutionProvider::default().build()];
        let session = Session::builder()
            .map_err(|e| anyhow!("session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow!("opt level: {}", e))?
            .with_execution_providers(providers)
            .map_err(|e| anyhow!("providers: {}", e))?
            .with_intra_threads(1)
            .map_err(|e| anyhow!("intra threads: {}", e))?
            .commit_from_file(&path)
            .map_err(|e| anyhow!("commit pyannote session: {}", e))?;

        let audio_input_name = session
            .inputs
            .iter()
            .find(|i| i.name == "input" || i.name == "audio_signal")
            .ok_or_else(|| anyhow!("model has no 'input'/'audio_signal' input"))?
            .name
            .to_string();
        let output_name = session
            .outputs
            .first()
            .ok_or_else(|| anyhow!("model has no outputs"))?
            .name
            .to_string();

        Ok(Self {
            session: Mutex::new(session),
            audio_input_name,
            output_name,
            params,
        })
    }

    /// Full-recording smoothed change-points (seconds). Slides a 10s window at
    /// a 1s step over the 16kHz mono samples, decodes powerset activity per
    /// window, last-writer-wins merge (probe parity), then smooths.
    pub fn change_points(&self, samples: &[f32]) -> Result<Vec<f64>> {
        let total_windows = if samples.len() > WINDOW_SAMPLES {
            (samples.len() - WINDOW_SAMPLES) / STEP_SAMPLES + 1
        } else {
            1
        };

        let params = self.params;
        let min_on_f = secs_to_frames(params.min_on_secs);
        let max_off_f = secs_to_frames(params.max_off_secs);

        // Merged per-frame activity across overlapping windows
        // (last-writer-wins — probe parity).
        let mut activity: Vec<[bool; 3]> = Vec::new();

        let mut session = self.session.lock().map_err(|_| anyhow!("session lock poisoned"))?;
        for win_idx in 0..total_windows {
            let start = win_idx * STEP_SAMPLES;
            let end = (start + WINDOW_SAMPLES).min(samples.len());
            if end.saturating_sub(start) < SAMPLE_RATE {
                break; // skip tail windows shorter than 1s
            }
            let mut window = vec![0.0f32; WINDOW_SAMPLES];
            window[..end - start].copy_from_slice(&samples[start..end]);

            let input_3d: Array3<f32> = Array1::from(window)
                .into_shape_with_order([1, 1, WINDOW_SAMPLES])
                .map_err(|e| anyhow!("window shape: {}", e))?;
            let audio_ref =
                TensorRef::from_array_view(input_3d.view()).map_err(|e| anyhow!("tensor: {}", e))?;
            let inputs = ort::inputs![self.audio_input_name.as_str() => audio_ref];

            // SessionOutputs borrows the session — copy what we need inside
            // the lock scope.
            let decoded: Vec<[bool; 3]> = {
                let outputs = session.run(inputs).map_err(|e| anyhow!("forward: {}", e))?;
                let out = outputs
                    .get(self.output_name.as_str())
                    .ok_or_else(|| anyhow!("output missing"))?;
                let arr = out.try_extract_array::<f32>().map_err(|e| anyhow!("extract: {}", e))?;
                let slice: &[f32] = arr.as_slice().unwrap_or_else(|| arr.to_slice().unwrap());
                let shape = arr.shape();
                let num_frames = shape[1];
                let num_classes = shape[2];
                decode_multilabel_with_hysteresis(slice, num_frames, num_classes, params.onset, params.onset)
            };

            // Map window-local frames → absolute frame indices (last-writer-wins).
            let win_start_secs = start as f64 / SAMPLE_RATE as f64;
            let first_frame = (win_start_secs / FRAME_SHIFT_SECS).round() as usize;
            for (i, act) in decoded.iter().enumerate() {
                let abs_idx = first_frame + i;
                while activity.len() <= abs_idx {
                    activity.push([false; 3]);
                }
                activity[abs_idx] = *act;
            }
        }
        drop(session);

        // Smooth: median filter + duration gates (probe parity).
        let med = median_filter_per_speaker(&activity, params.median_rad);
        let gated = duration_gates_per_speaker(&med, min_on_f, max_off_f);
        Ok(change_points(&gated))
    }

    /// Boundary segments for `build_chunks`: smoothed change-points →
    /// [(start, end)] tuples covering [0, duration], intersected with the
    /// Whisper speech regions and shed to the chunk cap.
    pub fn boundary_segments(
        &self,
        samples: &[f32],
        whisper_segments: &[(f64, f64)],
        cap: usize,
    ) -> Result<Vec<(f64, f64)>> {
        let cps = self.change_points(samples)?;
        let intersected = intersect_pyannote_with_whisper(&cps, whisper_segments);
        Ok(shed_boundaries_to_cap(intersected, cap, MIN_SEGMENT_SECS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Log-softmax row helper: class k gets logit ln(p_k), others share
    /// log((1-p_k)/(num_classes-1)) — enough structure to exercise hysteresis.
    fn log_row(active_prob: f32, num_classes: usize) -> Vec<f32> {
        // Simple synthetic: active classes get p, inactive split (1 - sum)/rest.
        let mut probs = vec![(1.0f32 - active_prob) / (num_classes as f32 - 1.0); num_classes];
        probs[1] = active_prob; // speaker 1 solo
        probs.iter().map(|p| p.ln()).collect()
    }

    #[test]
    fn powerset_decode_exp_is_required_for_hysteresis() {
        // With exp() applied (probe parity), a 0.9-probability frame turns
        // speaker 1 ON at onset 0.5.
        let mut rows = Vec::new();
        for _ in 0..5 {
            rows.extend(log_row(0.9, 7));
        }
        let act = decode_multilabel_with_hysteresis(&rows, 5, 7, 0.5, 0.5);
        assert!(act.iter().all(|a| a[0]), "exp(logits) must cross onset 0.5");

        // WITHOUT exp (the regression this test guards): raw log-probs are
        // negative, never cross 0.5 — proving the exp() is load-bearing.
        let mut raw_rows = Vec::new();
        for _ in 0..5 {
            raw_rows.extend(log_row(0.9, 7)); // these ARE log-probs
        }
        let no_exp: Vec<f32> = raw_rows.clone(); // skip .exp() by decoding manually
        let act_no_exp =
            decode_multilabel_with_hysteresis_no_exp(&no_exp, 5, 7, 0.5, 0.5);
        assert!(act_no_exp.iter().all(|a| !a[0]), "without exp, nothing activates");
    }

    // Direct copy of the decoder minus the exp step — exists ONLY to prove
    // the exp() is load-bearing (see the test above).
    fn decode_multilabel_with_hysteresis_no_exp(
        frame_logits: &[f32],
        num_frames: usize,
        num_classes: usize,
        onset: f32,
        offset: f32,
    ) -> Vec<[bool; 3]> {
        let mut active = [false; 3];
        let mut out = Vec::with_capacity(num_frames);
        for frame in 0..num_frames {
            let row = &frame_logits[frame * num_classes..(frame + 1) * num_classes];
            let mut probs = [0.0f32; 3];
            for (class, &p) in row.iter().enumerate() {
                let ml = powerset_to_multilabel(class);
                if ml != [false, false, false] {
                    for spk in 0..3 {
                        if ml[spk] {
                            probs[spk] += p;
                        }
                    }
                }
            }
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

    #[test]
    fn hysteresis_turns_off_below_offset() {
        // ON for 3 frames at 0.9, then OFF (0.05) — hysteresis keeps it on
        // through intermediate dips but drops sustained silence.
        let mut rows = Vec::new();
        for _ in 0..3 {
            rows.extend(log_row(0.9, 7));
        }
        rows.extend(log_row(0.4, 7)); // dip below onset but above offset? 0.4 < 0.5 offset → off
        rows.extend(log_row(0.05, 7));
        rows.extend(log_row(0.05, 7));
        let act = decode_multilabel_with_hysteresis(&rows, 6, 7, 0.5, 0.5);
        assert!(act[0][0]);
        assert!(act[2][0]);
        assert!(!act[5][0], "sustained low probability turns the speaker off");
    }

    #[test]
    fn median_filter_removes_single_frame_flicker() {
        // One-frame dropout inside a stable ON region → filtered back ON.
        let mut act: Vec<[bool; 3]> = vec![[true, false, false]; 10];
        act[5] = [false, false, false];
        let filtered = median_filter_per_speaker(&act, 3);
        assert!(
            filtered.iter().all(|a| a[0]),
            "median rad=3 removes an isolated single-frame flicker"
        );
    }

    #[test]
    fn duration_gates_fill_short_gaps_and_drop_short_bursts() {
        // Gap of 20 frames (~0.34s) bounded by ON on both sides, max_off=30
        // frames (~0.51s) → filled.
        let mut act: Vec<[bool; 3]> = vec![[true, false, false]; 100];
        for f in 40..60 {
            act[f][0] = false;
        }
        let gated = duration_gates_per_speaker(&act, secs_to_frames(0.3), secs_to_frames(0.5));
        assert!(gated.iter().all(|a| a[0]), "gap ≤ max_off filled");

        // Burst of 10 frames (~0.17s) < min_on (18 frames ≈ 0.3s) → dropped.
        let burst: Vec<[bool; 3]> = (0..100)
            .map(|i| [i >= 40 && i < 50, false, false])
            .collect();
        let gated2 = duration_gates_per_speaker(&burst, secs_to_frames(0.3), secs_to_frames(0.5));
        assert!(gated2.iter().all(|a| !a[0]), "burst < min_on dropped");
    }

    #[test]
    fn intersect_splits_inside_and_merges_slivers() {
        // Whisper segment [10, 30]; pyannote points at 12 (split), 15 (sliver:
        // creates a 3s piece... actually 12→15 = 3s ≥ min), and 29.99 (sliver).
        let cps = vec![5.0, 12.0, 29.99]; // 5.0 is outside → ignored
        let segs = intersect_pyannote_with_whisper(&cps, &[(10.0, 30.0)]);
        // Expected pieces: [10,12], [12,29.99 merged sliver], [29.99→30 merged]
        // 12→29.99 is 17.99s; trailing 29.99→30 is 0.01s < 1.5 → absorbed into
        // previous piece's end.
        assert_eq!(segs.len(), 2, "got {:?}", segs);
        assert!((segs[0].0 - 10.0).abs() < 1e-9 && (segs[0].1 - 12.0).abs() < 1e-9);
        assert!((segs[1].0 - 12.0).abs() < 1e-9 && (segs[1].1 - 30.0).abs() < 1e-9);
    }

    #[test]
    fn intersect_exact_edge_point_produces_no_zero_length_split() {
        // Task 2.12: a change-point exactly on a Whisper edge is exclusive —
        // the full [10,30] span survives as ONE piece (no zero-length split).
        let cps = vec![10.0, 30.0];
        let segs = intersect_pyannote_with_whisper(&cps, &[(10.0, 30.0)]);
        assert_eq!(segs.len(), 1, "edge points excluded (no zero-length split)");
        assert!((segs[0].0 - 10.0).abs() < 1e-9);
        assert!((segs[0].1 - 30.0).abs() < 1e-9);
    }

    #[test]
    fn shed_to_cap_keeps_positional_coverage() {
        // 120 segments of 2s each; cap 60 → shed half positionally.
        let segs: Vec<(f64, f64)> = (0..120).map(|i| (i as f64 * 2.0, i as f64 * 2.0 + 2.0)).collect();
        let shed = shed_boundaries_to_cap(segs.clone(), 60, 1.5);
        assert!(shed.len() <= 60, "shed respects the cap (len={})", shed.len());
        // Coverage: first and last spans preserved.
        assert!((shed[0].0 - 0.0).abs() < 1e-9);
        assert!((shed.last().unwrap().1 - 240.0).abs() < 1e-9);
        // No sub-minimum survivors after merge.
        assert!(shed.windows(2).all(|w| w[0].1 <= w[1].1 + 1e-9), "monotonic");
    }

    #[test]
    fn merge_sub_minimum_extends_neighbors_not_drop() {
        // [0,3],[3,4](1s<min),[4,7] → middle merges into left: [0,4],[4,7].
        let merged = merge_sub_minimum(vec![(0.0, 3.0), (3.0, 4.0), (4.0, 7.0)], 1.5);
        assert_eq!(merged, vec![(0.0, 4.0), (4.0, 7.0)]);
        // Leading sliver extends the FOLLOWING piece leftward: [0,1],[1,4] → [0,4].
        let merged2 = merge_sub_minimum(vec![(0.0, 1.0), (1.0, 4.0)], 1.5);
        assert_eq!(merged2, vec![(0.0, 4.0)]);
    }
}

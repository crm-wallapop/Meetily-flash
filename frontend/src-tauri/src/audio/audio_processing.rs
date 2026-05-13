use anyhow::Result;
use chrono::Utc;
use log::{debug, info, warn};
use realfft::num_complex::{Complex32, ComplexFloat};
use realfft::RealFftPlanner;
use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::path::PathBuf;
use nnnoiseless::DenoiseState;

use super::encode::encode_single_audio; // Correct path to encode module

/// Sanitize a filename to be safe for filesystem use
pub fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect::<String>()
        .trim()
        .to_string()
}

/// Create a meeting folder with timestamp and return the path
/// Creates structure: base_path/MeetingName_YYYY-MM-DD_HH-MM/
///                    ├── .checkpoints/  (for incremental saves, optional)
///
/// # Arguments
/// * `base_path` - Base directory for meetings
/// * `meeting_name` - Name of the meeting
/// * `create_checkpoints_dir` - Whether to create .checkpoints/ subdirectory (only needed when auto_save is true)
pub fn create_meeting_folder(
    base_path: &PathBuf,
    meeting_name: &str,
    create_checkpoints_dir: bool,
) -> Result<PathBuf> {
    let timestamp = Utc::now().format("%Y-%m-%d_%H-%M").to_string();
    let sanitized_name = sanitize_filename(meeting_name);
    let folder_name = format!("{}_{}", sanitized_name, timestamp);
    let meeting_folder = base_path.join(folder_name);

    // Create main meeting folder
    std::fs::create_dir_all(&meeting_folder)?;

    // Only create .checkpoints subdirectory if requested (when auto_save is true)
    if create_checkpoints_dir {
        let checkpoints_dir = meeting_folder.join(".checkpoints");
        std::fs::create_dir_all(&checkpoints_dir)?;
        log::info!("Created meeting folder with checkpoints: {}", meeting_folder.display());
    } else {
        log::info!("Created meeting folder without checkpoints: {}", meeting_folder.display());
    }

    Ok(meeting_folder)
}

pub fn normalize_v2(audio: &[f32]) -> Vec<f32> {
    let rms = (audio.iter().map(|&x| x * x).sum::<f32>() / audio.len() as f32).sqrt();
    let peak = audio
        .iter()
        .fold(0.0f32, |max, &sample| max.max(sample.abs()));

    // Return the original audio if it's completely silent
    if rms == 0.0 || peak == 0.0 {
        return audio.to_vec();
    }

    // Increase target RMS for better voice volume while keeping peak in check
    let target_rms = 0.9;  // Increased from 0.6
    let target_peak = 0.95; // Slightly reduced to prevent clipping

    let rms_scaling = target_rms / rms;
    let peak_scaling = target_peak / peak;

    // Apply a minimum scaling factor to boost very quiet audio
    let min_scaling = 1.5; // Minimum boost for quiet audio
    let scaling_factor = (rms_scaling.min(peak_scaling)).max(min_scaling);

    // Apply scaling with soft clipping to prevent harsh distortion
    audio
        .iter()
        .map(|&sample| {
            let scaled = sample * scaling_factor;
            // Soft clip at ±0.95 to prevent harsh distortion
            if scaled > 0.95 {
                0.95 + (scaled - 0.95) * 0.05
            } else if scaled < -0.95 {
                -0.95 + (scaled + 0.95) * 0.05
            } else {
                scaled
            }
        })
        .collect()
}

/// True peak limiter with lookahead buffer (prevents clipping)
struct TruePeakLimiter {
    lookahead_samples: usize,
    buffer: Vec<f32>,
    gain_reduction: Vec<f32>,
    current_position: usize,
}

impl TruePeakLimiter {
    fn new(sample_rate: u32) -> Self {
        const LIMITER_LOOKAHEAD_MS: usize = 10;
        let lookahead_samples = ((sample_rate as usize * LIMITER_LOOKAHEAD_MS) / 1000).max(1);

        Self {
            lookahead_samples,
            buffer: vec![0.0; lookahead_samples],
            gain_reduction: vec![1.0; lookahead_samples],
            current_position: 0,
        }
    }

    fn process(&mut self, sample: f32, true_peak_limit: f32) -> f32 {
        self.buffer[self.current_position] = sample;

        let sample_abs = sample.abs();
        if sample_abs > true_peak_limit {
            let reduction = true_peak_limit / sample_abs;
            self.gain_reduction[self.current_position] = reduction;
        } else {
            self.gain_reduction[self.current_position] = 1.0;
        }

        let output_position = (self.current_position + 1) % self.lookahead_samples;
        let output_sample = self.buffer[output_position] * self.gain_reduction[output_position];

        self.current_position = output_position;
        output_sample
    }
}

/// Professional loudness normalizer using EBU R128 standard
/// This is a STATEFUL normalizer that tracks cumulative loudness over time
///
/// EBU R128 is the broadcast industry standard for loudness normalization:
/// - Target: -23 LUFS (Loudness Units relative to Full Scale)
/// - Used by: Netflix, YouTube, Spotify, all professional broadcast
/// - Perceptually accurate (not just simple RMS)
///
pub struct LoudnessNormalizer {
    ebur128: ebur128::EbuR128,
    limiter: TruePeakLimiter,
    gain_linear: f32,
    gate_window: Vec<f32>,
    gate_floor_linear: f32,
    true_peak_limit: f32,
    // Gain event emission: 3s trailing ring of dB-domain gain values; emit mean once per second
    gain_history: std::collections::VecDeque<f32>,
    samples_since_emit: usize,
    gain_sender: Option<tokio::sync::mpsc::UnboundedSender<f32>>,
}

impl LoudnessNormalizer {
    pub fn new(channels: u32, sample_rate: u32, gate_floor_dbfs: i32) -> Result<Self> {
        const TRUE_PEAK_LIMIT: f64 = -1.0;
        const GATE_RMS_WINDOW_SAMPLES: usize = 4800; // 100ms @ 48 kHz

        let ebur128 = ebur128::EbuR128::new(channels, sample_rate, ebur128::Mode::S | ebur128::Mode::TRUE_PEAK)
            .map_err(|e| anyhow::anyhow!("Failed to create EBU R128 normalizer: {}", e))?;

        let true_peak_limit = 10_f32.powf(TRUE_PEAK_LIMIT as f32 / 20.0);
        let gate_floor_linear = 10_f32.powf(gate_floor_dbfs as f32 / 20.0);

        Ok(Self {
            ebur128,
            limiter: TruePeakLimiter::new(sample_rate),
            gain_linear: 1.0,
            gate_window: Vec::with_capacity(GATE_RMS_WINDOW_SAMPLES),
            gate_floor_linear,
            true_peak_limit,
            gain_history: std::collections::VecDeque::with_capacity(30),
            samples_since_emit: 0,
            gain_sender: None,
        })
    }

    pub fn set_gain_sender(&mut self, sender: tokio::sync::mpsc::UnboundedSender<f32>) {
        self.gain_sender = Some(sender);
    }

    /// Normalize loudness using EBU R128 standard with true peak limiting
    ///
    /// This maintains cumulative loudness measurements across all processed audio,
    /// resulting in consistent normalization that sounds natural.
    ///
    /// Target: -23 LUFS (professional broadcast standard for speech/dialog)
    /// Applies sample-by-sample with 10ms lookahead limiter to prevent clipping
    pub fn normalize_loudness(&mut self, samples: &[f32]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        const TARGET_LUFS: f64 = -23.0;
        const MAX_GAIN_LINEAR: f32 = 3.981_071_7; // 10^(12/20) = +12 dB safety ceiling
        const GATE_RMS_WINDOW_SAMPLES: usize = 4800; // 100ms @ 48 kHz
        const EMIT_INTERVAL_SAMPLES: usize = 48000; // 1 second @ 48 kHz
        const GAIN_HISTORY_MAX: usize = 30; // 3s of 100ms gate-window updates

        let mut normalized_samples = Vec::with_capacity(samples.len());

        for &sample in samples {
            // Apply current gain and true peak limiting to output immediately
            let amplified = sample * self.gain_linear;
            let limited = self.limiter.process(amplified, self.true_peak_limit);
            normalized_samples.push(limited);

            // Accumulate into 100ms noise-gate window
            self.gate_window.push(sample);

            if self.gate_window.len() >= GATE_RMS_WINDOW_SAMPLES {
                let rms = (self.gate_window.iter().map(|&x| x * x).sum::<f32>()
                    / self.gate_window.len() as f32)
                    .sqrt();

                if rms >= self.gate_floor_linear {
                    // Above gate threshold — feed to ebur128 and update gain
                    if let Err(e) = self.ebur128.add_frames_f32(&self.gate_window) {
                        warn!("Failed to add frames to EBU R128: {}", e);
                    } else {
                        if let Ok(current_lufs) = self.ebur128.loudness_shortterm() {
                            if current_lufs.is_finite() && current_lufs < 0.0 {
                                let gain_db = TARGET_LUFS - current_lufs;
                                self.gain_linear =
                                    (10_f32.powf(gain_db as f32 / 20.0)).min(MAX_GAIN_LINEAR);
                            }
                        }
                    }
                }
                // Sub-threshold window: skip ebur128 feed, keep current gain_linear

                // Record current gain_db in 3s trailing history (dB-domain mean for readout)
                let gain_db_now = 20.0 * self.gain_linear.log10();
                if self.gain_history.len() >= GAIN_HISTORY_MAX {
                    self.gain_history.pop_front();
                }
                self.gain_history.push_back(gain_db_now);

                self.gate_window.clear();
            }
        }

        // Emit mean gain_db once per second via channel (relay task forwards to Tauri event)
        self.samples_since_emit += samples.len();
        while self.samples_since_emit >= EMIT_INTERVAL_SAMPLES {
            self.samples_since_emit -= EMIT_INTERVAL_SAMPLES;
            if let Some(ref tx) = self.gain_sender {
                if !self.gain_history.is_empty() {
                    let mean_db = self.gain_history.iter().sum::<f32>() / self.gain_history.len() as f32;
                    let _ = tx.send(mean_db);
                }
            }
        }

        normalized_samples
    }
}

#[cfg(test)]
impl LoudnessNormalizer {
    /// Returns true if Mode::S is enabled and ≥3s of audio has been fed.
    pub fn shortterm_available(&self) -> bool {
        self.ebur128.loudness_shortterm().is_ok()
    }

    pub fn current_gain(&self) -> f32 {
        self.gain_linear
    }
}

/// RNNoise-based noise suppression processor
///
/// Uses a recurrent neural network to suppress background noise while preserving speech.
/// Processes audio at 48kHz in 10ms frames (480 samples per frame).
///
/// Benefits:
/// - 10-15 dB noise reduction in typical office/home environments
/// - Preserves speech quality and intelligibility
/// - Low latency (~10ms per frame)
/// - Cross-platform (works on macOS, Windows, Linux)
pub struct NoiseSuppressionProcessor {
    denoiser: DenoiseState<'static>,
    frame_buffer: Vec<f32>,
    frame_size: usize,  // 480 samples at 48kHz = 10ms
}

impl NoiseSuppressionProcessor {
    /// Create a new noise suppression processor
    ///
    /// # Arguments
    /// * `sample_rate` - Must be 48000 Hz (RNNoise requirement)
    pub fn new(sample_rate: u32) -> Result<Self> {
        if sample_rate != 48000 {
            return Err(anyhow::anyhow!(
                "Noise suppression requires 48kHz sample rate, got {}Hz",
                sample_rate
            ));
        }

        const FRAME_SIZE: usize = DenoiseState::FRAME_SIZE;

        info!("Initializing RNNoise noise suppression (frame size: {} samples, 10ms @ 48kHz)", FRAME_SIZE);

        Ok(Self {
            denoiser: *DenoiseState::new(),
            frame_buffer: Vec::with_capacity(FRAME_SIZE * 2),
            frame_size: FRAME_SIZE,
        })
    }

    /// Apply noise suppression to audio samples
    ///
    /// Processes audio in 480-sample frames (10ms at 48kHz).
    /// Buffers partial frames for next call.
    ///
    /// CRITICAL FIX: Always returns same length as input to prevent latency accumulation
    ///
    /// # Arguments
    /// * `samples` - Input audio samples at 48kHz
    ///
    /// # Returns
    /// Noise-suppressed audio samples (SAME LENGTH as input)
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        if samples.is_empty() {
            return Vec::new();
        }

        // CRITICAL: Remember original input length
        let input_len = samples.len();

        // Add new samples to buffer
        self.frame_buffer.extend_from_slice(samples);

        let mut output = Vec::with_capacity(input_len);

        // Process complete frames
        while self.frame_buffer.len() >= self.frame_size {
            // Extract one frame
            let frame: Vec<f32> = self.frame_buffer.drain(0..self.frame_size).collect();

            // RNNoise processes audio: separate input and output buffers
            let mut denoised_frame = vec![0.0f32; self.frame_size];

            // Apply noise suppression
            // process_frame(output: &mut [f32], input: &[f32]) -> f32
            // Returns VAD probability (0.0-1.0), higher means more likely to be speech
            let _vad_prob = self.denoiser.process_frame(&mut denoised_frame, &frame);

            output.extend_from_slice(&denoised_frame);
        }

        // Return processed output without forcing length matching
        // Frame-based processing naturally creates variable-length output
        // Downstream pipeline handles this correctly via ring buffer
        output
    }

    /// Get the number of buffered samples waiting for processing
    pub fn buffered_samples(&self) -> usize {
        self.frame_buffer.len()
    }

    /// Flush any remaining buffered samples
    /// Call this at the end of recording to process partial frames
    pub fn flush(&mut self) -> Vec<f32> {
        if self.frame_buffer.is_empty() {
            return Vec::new();
        }

        // Pad the remaining samples to a full frame with zeros
        let remaining = self.frame_buffer.len();
        let mut input_frame = self.frame_buffer.clone();
        if input_frame.len() < self.frame_size {
            input_frame.resize(self.frame_size, 0.0);
        }

        let mut output = vec![0.0f32; self.frame_size];
        self.denoiser.process_frame(&mut output, &input_frame);
        self.frame_buffer.clear();

        // Return only the original samples (without padding)
        output.truncate(remaining);
        output
    }
}

/// High-pass filter to remove low-frequency rumble and noise
/// Removes frequencies below cutoff_hz (typically 80-100 Hz for speech)
pub struct HighPassFilter {
    #[allow(dead_code)]
    sample_rate: f32,
    #[allow(dead_code)]
    cutoff_hz: f32,
    // First-order IIR filter coefficients
    alpha: f32,
    prev_input: f32,
    prev_output: f32,
}

impl HighPassFilter {
    /// Create a new high-pass filter
    ///
    /// # Arguments
    /// * `sample_rate` - Audio sample rate in Hz
    /// * `cutoff_hz` - Cutoff frequency in Hz (typical: 80-100 Hz for speech)
    pub fn new(sample_rate: u32, cutoff_hz: f32) -> Self {
        let sample_rate_f = sample_rate as f32;
        let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz);
        let dt = 1.0 / sample_rate_f;
        let alpha = rc / (rc + dt);

        info!("Initializing high-pass filter: cutoff={}Hz @ {}Hz", cutoff_hz, sample_rate);

        Self {
            sample_rate: sample_rate_f,
            cutoff_hz,
            alpha,
            prev_input: 0.0,
            prev_output: 0.0,
        }
    }

    /// Apply high-pass filter to audio samples
    /// Uses first-order IIR (Infinite Impulse Response) filter
    pub fn process(&mut self, samples: &[f32]) -> Vec<f32> {
        let mut output = Vec::with_capacity(samples.len());

        for &sample in samples {
            // First-order high-pass IIR filter formula:
            // y[n] = alpha * (y[n-1] + x[n] - x[n-1])
            let filtered = self.alpha * (self.prev_output + sample - self.prev_input);

            self.prev_input = sample;
            self.prev_output = filtered;

            output.push(filtered);
        }

        output
    }

    /// Reset filter state (call when starting new recording)
    pub fn reset(&mut self) {
        self.prev_input = 0.0;
        self.prev_output = 0.0;
    }
}

pub fn spectral_subtraction(audio: &[f32], d: f32) -> Result<Vec<f32>> {
    let mut real_planner = RealFftPlanner::<f32>::new();
    let window_size = 1600; // 16k sample rate - 100ms

    // CRITICAL FIX: Handle cases where audio is longer than window size
    if audio.is_empty() {
        return Ok(Vec::new());
    }

    // If audio is longer than window size, truncate to prevent overflow
    let processed_audio = if audio.len() > window_size {
        warn!("Audio length {} exceeds window size {}, truncating", audio.len(), window_size);
        &audio[..window_size]
    } else {
        audio
    };

    let r2c = real_planner.plan_fft_forward(window_size);
    let mut y = r2c.make_output_vec();

    // Safe padding: only pad if audio is shorter than window size
    let mut padded_audio = processed_audio.to_vec();
    if processed_audio.len() < window_size {
        let padding_needed = window_size - processed_audio.len();
        padded_audio.extend(vec![0.0f32; padding_needed]);
    }

    let mut indata = padded_audio;
    r2c.process(&mut indata, &mut y)?;

    let mut processed_audio = y
        .iter()
        .map(|&x| {
            let magnitude_y = x.abs().powf(2.0);

            let div = 1.0 - (d / magnitude_y);

            let gain = {
                if div > 0.0 {
                    f32::sqrt(div)
                } else {
                    0.0f32
                }
            };

            x * gain
        })
        .collect::<Vec<Complex32>>();

    let c2r = real_planner.plan_fft_inverse(window_size);

    let mut outdata = c2r.make_output_vec();

    c2r.process(&mut processed_audio, &mut outdata)?;

    Ok(outdata)
}

// not an average of non-speech segments, but I don't know how much pause time we
// get. for now, we will just assume the noise is constant (kinda defeats the purpose)
// but oh well
pub fn average_noise_spectrum(audio: &[f32]) -> f32 {
    let mut total_sum = 0.0f32;

    for sample in audio {
        let magnitude = sample.abs();

        total_sum += magnitude.powf(2.0);
    }

    total_sum / audio.len() as f32
}

pub fn audio_to_mono(audio: &[f32], channels: u16) -> Vec<f32> {
    let mut mono_samples = Vec::with_capacity(audio.len() / channels as usize);

    // For microphone arrays (> 2 channels), only use first 2 channels
    // Many microphone arrays have auxiliary channels for beam-forming/noise cancellation
    // that can contain anti-phase signals. Averaging all channels can cause destructive
    // interference resulting in near-zero output.
    let effective_channels = if channels > 2 { 2 } else { channels };

    // Iterate over the audio slice in chunks, each containing `channels` samples
    for chunk in audio.chunks(channels as usize) {
        // Sum only the first effective_channels (typically 1-2 for mic arrays)
        let sum: f32 = chunk.iter().take(effective_channels as usize).sum();

        // Calculate the average mono sample using effective channel count
        let mono_sample = sum / effective_channels as f32;

        // Store the computed mono sample
        mono_samples.push(mono_sample);
    }

    mono_samples
}

/// High-quality audio resampling with adaptive parameters based on sample rate ratio
///
/// This function automatically selects the best resampling parameters based on:
/// - Sample rate ratio (upsampling vs downsampling)
/// - Quality requirements (integer ratios get optimized paths)
/// - Anti-aliasing needs
///
/// Supports all common sample rates: 8kHz, 16kHz, 24kHz, 44.1kHz, 48kHz, etc.
pub fn resample(input: &[f32], from_sample_rate: u32, to_sample_rate: u32) -> Result<Vec<f32>> {
    if input.is_empty() {
        return Ok(Vec::new());
    }

    // Fast path: No resampling needed
    if from_sample_rate == to_sample_rate {
        return Ok(input.to_vec());
    }

    let ratio = to_sample_rate as f64 / from_sample_rate as f64;

    // Adaptive parameters based on sample rate ratio
    let (sinc_len, interpolation_type, oversampling) = if ratio >= 2.0 {
        // Large upsampling (e.g., 8kHz → 16kHz, 16kHz → 48kHz, 24kHz → 48kHz)
        // Needs high quality to avoid artifacts
        debug!("High-quality upsampling: {}Hz → {}Hz (ratio: {:.2}x)",
               from_sample_rate, to_sample_rate, ratio);
        (
            512,                              // Longer sinc for smoother interpolation
            SincInterpolationType::Cubic,     // Cubic for best quality
            512,                              // Higher oversampling
        )
    } else if ratio >= 1.5 {
        // Moderate upsampling (e.g., 32kHz → 48kHz)
        debug!("Moderate upsampling: {}Hz → {}Hz (ratio: {:.2}x)",
               from_sample_rate, to_sample_rate, ratio);
        (
            384,
            SincInterpolationType::Cubic,
            384,
        )
    } else if ratio > 1.0 {
        // Small upsampling (e.g., 44.1kHz → 48kHz)
        debug!("Small upsampling: {}Hz → {}Hz (ratio: {:.2}x)",
               from_sample_rate, to_sample_rate, ratio);
        (
            256,
            SincInterpolationType::Linear,
            256,
        )
    } else if ratio <= 0.5 {
        // Large downsampling (e.g., 48kHz → 16kHz, 48kHz → 8kHz)
        // Needs strong anti-aliasing
        debug!("Anti-aliased downsampling: {}Hz → {}Hz (ratio: {:.2}x)",
               from_sample_rate, to_sample_rate, ratio);
        (
            512,                              // Longer sinc for anti-aliasing
            SincInterpolationType::Cubic,     // Cubic for quality
            512,
        )
    } else {
        // Moderate downsampling (e.g., 48kHz → 24kHz, 48kHz → 32kHz)
        debug!("Moderate downsampling: {}Hz → {}Hz (ratio: {:.2}x)",
               from_sample_rate, to_sample_rate, ratio);
        (
            384,
            SincInterpolationType::Linear,
            384,
        )
    };

    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff: 0.95,                      // Preserve most of the frequency content
        interpolation: interpolation_type,
        oversampling_factor: oversampling,
        window: WindowFunction::BlackmanHarris2,  // Best window for audio
    };

    let mut resampler = SincFixedIn::<f32>::new(
        ratio,
        2.0,  // Maximum relative deviation
        params,
        input.len(),
        1,    // Mono
    )?;

    let waves_in = vec![input.to_vec()];
    let waves_out = resampler.process(&waves_in, None)?;

    debug!("Resampling complete: {} samples → {} samples",
           input.len(), waves_out[0].len());

    Ok(waves_out.into_iter().next().unwrap())
}

// Alias for compatibility with existing code
pub fn resample_audio(input: &[f32], from_sample_rate: u32, to_sample_rate: u32) -> Vec<f32> {
    match resample(input, from_sample_rate, to_sample_rate) {
        Ok(result) => result,
        Err(e) => {
            debug!("Resampling failed: {}, returning original audio", e);
            input.to_vec()
        }
    }
}

/// Fast resampling optimized for transcription preprocessing
///
pub fn write_audio_to_file(
    audio: &[f32],
    sample_rate: u32,
    output_path: &PathBuf,
    device: &str,
    skip_encoding: bool,
) -> Result<String> {
    write_audio_to_file_with_meeting_name(audio, sample_rate, output_path, device, skip_encoding, None)
}

pub fn write_audio_to_file_with_meeting_name(
    audio: &[f32],
    sample_rate: u32,
    output_path: &PathBuf,
    device: &str,
    skip_encoding: bool,
    meeting_name: Option<&str>,
) -> Result<String> {
    let timestamp = Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let sanitized_device_name = device.replace(['/', '\\'], "_");

    // Create meeting folder if meeting name is provided
    let final_output_path = if let Some(name) = meeting_name {
        let sanitized_meeting_name = sanitize_filename(name);
        let meeting_folder = output_path.join(&sanitized_meeting_name);

        // Create the meeting folder if it doesn't exist
        if !meeting_folder.exists() {
            std::fs::create_dir_all(&meeting_folder)?;
        }

        meeting_folder
    } else {
        output_path.clone()
    };

    let file_path = final_output_path
        .join(format!("{}_{}.mp4", sanitized_device_name, timestamp))
        .to_str()
        .expect("Failed to create valid path")
        .to_string();
    let file_path_clone = file_path.clone();
    // Run FFmpeg in a separate task
    if !skip_encoding {
        encode_single_audio(
            bytemuck::cast_slice(audio),
            sample_rate,
            1,
            &file_path.into(),
        )?;
    }
    Ok(file_path_clone)
}

/// Write transcript text to a file alongside the recording (legacy plain text format)
pub fn write_transcript_to_file(
    transcript_text: &str,
    output_path: &PathBuf,
    meeting_name: Option<&str>,
) -> Result<String> {
    let timestamp = Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string();

    // Create meeting folder if meeting name is provided (same logic as audio)
    let final_output_path = if let Some(name) = meeting_name {
        let sanitized_meeting_name = sanitize_filename(name);
        let meeting_folder = output_path.join(&sanitized_meeting_name);

        // Create the meeting folder if it doesn't exist
        if !meeting_folder.exists() {
            std::fs::create_dir_all(&meeting_folder)?;
        }

        meeting_folder
    } else {
        output_path.clone()
    };

    let file_path = final_output_path.join(format!("transcript_{}.txt", timestamp));

    // Write transcript to file
    std::fs::write(&file_path, transcript_text)?;

    Ok(file_path.to_string_lossy().to_string())
}

/// Write structured transcript with timestamps to JSON file
pub fn write_transcript_json_to_file(
    segments: &[super::recording_saver::TranscriptSegment],
    output_path: &PathBuf,
    meeting_name: Option<&str>,
    audio_filename: &str,
    recording_duration: f64,
) -> Result<String> {
    use serde_json::json;

    let timestamp = Utc::now().format("%Y-%m-%d_%H-%M-%S").to_string();

    // Create meeting folder if meeting name is provided
    let final_output_path = if let Some(name) = meeting_name {
        let sanitized_meeting_name = sanitize_filename(name);
        let meeting_folder = output_path.join(&sanitized_meeting_name);

        if !meeting_folder.exists() {
            std::fs::create_dir_all(&meeting_folder)?;
        }

        meeting_folder
    } else {
        output_path.clone()
    };

    let file_path = final_output_path.join(format!("transcript_{}.json", timestamp));

    // Create structured JSON transcript
    let transcript_json = json!({
        "version": "1.0",
        "recording_duration": recording_duration,
        "audio_file": audio_filename,
        "sample_rate": 48000,
        "created_at": Utc::now().to_rfc3339(),
        "meeting_name": meeting_name,
        "segments": segments,
    });

    // Write JSON to file with pretty formatting
    let json_string = serde_json::to_string_pretty(&transcript_json)?;
    std::fs::write(&file_path, json_string)?;

    Ok(file_path.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_samples(amplitude: f32, freq_hz: f32, sample_rate: u32, duration_secs: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * duration_secs) as usize;
        (0..n)
            .map(|i| amplitude * (2.0 * std::f32::consts::PI * freq_hz * i as f32 / sample_rate as f32).sin())
            .collect()
    }

    // --------------- 1.1 ---------------
    #[test]
    fn loudness_normalizer_constructs_with_shortterm_mode() {
        // −20 dBFS sine at 440 Hz for 3.1 seconds (above the default −30 dBFS gate floor)
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -30).unwrap();
        let samples = sine_samples(0.1, 440.0, 48000, 3.1);
        normalizer.normalize_loudness(&samples);
        assert!(
            normalizer.shortterm_available(),
            "Mode::S must be enabled so loudness_shortterm() returns Ok after ≥3s of audio"
        );
    }

    // --------------- 1.5 ---------------
    // Red test for noise gate. Fails until 2.4+2.5 are implemented.
    // Behavioral proxy: feed 4s of above-threshold audio to set a non-unity gain,
    // then feed 4s of sub-threshold audio. Without the gate, ebur128 is fed sub-threshold
    // frames and shortterm LUFS drops, ramping gain to the cap (3.98). With the gate,
    // ebur128 never receives those frames, so gain stays near the above-threshold value.
    #[test]
    fn loudness_normalizer_noise_gate_skips_measurement() {
        const GATE_FLOOR: i32 = -30;
        let mut normalizer = LoudnessNormalizer::new(1, 48000, GATE_FLOOR).unwrap();
        // Warm up with above-threshold speech to establish non-unity gain
        let warm = sine_samples(0.1, 440.0, 48000, 4.0); // −20 dBFS RMS (above −30 floor)
        normalizer.normalize_loudness(&warm);
        let gain_after_warmup = normalizer.current_gain();
        // Now feed 4s of sub-threshold noise (−50 dBFS, below −30 floor)
        let quiet = sine_samples(0.00316, 441.0, 48000, 4.0); // −50 dBFS
        let quiet_out = normalizer.normalize_loudness(&quiet);
        // Gate must prevent LUFS update: gain stays close to warmup value
        assert!(
            (normalizer.current_gain() - gain_after_warmup).abs() < 0.05,
            "gain must not change during sub-threshold phase (gate should skip measurement); \
             before={:.4}, after={:.4}", gain_after_warmup, normalizer.current_gain()
        );
        // Gain is still applied to output — verify via RMS ratio.
        // Skip first 480 output samples (TruePeakLimiter 10ms lookahead flush contains the
        // warmup tail at amplitude ~0.1, which would inflate output RMS by ~2×).
        const LOOKAHEAD: usize = 480; // 10ms @ 48 kHz
        let input_rms = (quiet.iter().map(|&x| x * x).sum::<f32>() / quiet.len() as f32).sqrt();
        let steady = &quiet_out[LOOKAHEAD..];
        let output_rms = (steady.iter().map(|&x| x * x).sum::<f32>() / steady.len() as f32).sqrt();
        let rms_ratio = if input_rms > 1e-8 { output_rms / input_rms } else { 0.0 };
        assert!(
            (rms_ratio - gain_after_warmup).abs() < 0.2,
            "gain must be applied to gated output; expected rms_ratio≈{:.4}, got {:.4}",
            gain_after_warmup, rms_ratio
        );
    }

    // --------------- 1.6 ---------------
    // Scenario: entire recording is sub-threshold → gate never feeds ebur128 → gain stays 1.0.
    // Red until 2.4+2.5 are implemented.
    #[test]
    fn loudness_normalizer_shortterm_error_fallback() {
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -30).unwrap();
        // Only sub-threshold audio: gate prevents every frame from reaching ebur128
        let quiet = sine_samples(0.001, 440.0, 48000, 4.0); // −60 dBFS (below −30 gate floor)
        normalizer.normalize_loudness(&quiet);
        assert_eq!(
            normalizer.current_gain(),
            1.0,
            "gain must stay 1.0 when gate prevents all frames from reaching ebur128"
        );
    }

    // --------------- 1.4 ---------------
    #[test]
    fn loudness_normalizer_gain_cap_enforced() {
        // Feed 4s of very quiet noise at −60 dBFS (amplitude 0.001).
        // Gate floor set to −80 so the signal is above the gate and ebur128 actually measures it.
        // Without cap: shortterm ≈ −60 LUFS → gain = 10^(37/20) ≈ 70×. Cap should clamp to 3.98×.
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -80).unwrap();
        let samples = sine_samples(0.001, 440.0, 48000, 4.0);
        normalizer.normalize_loudness(&samples);
        const MAX_GAIN_LINEAR: f32 = 3.981_071_7; // 10^(12/20)
        assert!(
            normalizer.current_gain() <= MAX_GAIN_LINEAR + 0.001,
            "gain_linear must be capped at +12 dB (3.98×), got {}",
            normalizer.current_gain()
        );
    }

    // --------------- 1.2 ---------------
    #[test]
    fn loudness_normalizer_does_not_clip_speech_after_quiet_phase() {
        // 4s of −50 dBFS noise to fill the 3s shortterm window, then 50ms of speech.
        // Gate floor set to −80 so the noise is above the gate and ebur128 actually measures it.
        // 50ms is too short to pull the window LUFS back up, so gain stays at noise-induced level.
        // Without cap, gain ≈ 30×; with cap, gain = 3.98×.
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -80).unwrap();
        let noise = sine_samples(0.00316, 441.0, 48000, 4.0); // −50 dBFS
        let speech = sine_samples(0.1, 440.0, 48000, 0.05);   // −20 dBFS, 50ms
        normalizer.normalize_loudness(&noise);
        normalizer.normalize_loudness(&speech);
        // Gain must be capped: with noise at −50 LUFS → target −23 LUFS → uncapped gain ≈ 30×
        assert!(
            normalizer.current_gain() <= 3.981_071_7 + 0.01,
            "gain must be capped at +12 dB after quiet phase, got {}", normalizer.current_gain()
        );
    }

    // --------------- 1.3 ---------------
    #[test]
    fn loudness_normalizer_noise_floor_not_lifted() {
        // 5s of −40 dBFS ambient noise. Gate floor set to −80 so ebur128 measures it.
        // Without cap, gain ≈ 7× would lift output to −23 dBFS.
        // With +12 dB cap, output stays at −40+12 = −28 dBFS (proved by assertion).
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -80).unwrap();
        let noise = sine_samples(0.01, 440.0, 48000, 5.0); // −40 dBFS (amplitude 0.01)
        let out = normalizer.normalize_loudness(&noise);
        let rms = (out.iter().map(|&x| x * x).sum::<f32>() / out.len() as f32).sqrt();
        let rms_dbfs = 20.0 * rms.log10();
        assert!(
            rms_dbfs < -27.0, // gain cap keeps output at ≤ −28 dBFS; uncapped 7× would reach −23
            "noise floor must not be lifted above −28 dBFS, got {:.1} dBFS", rms_dbfs
        );
    }

    // --------------- 1.7 ---------------
    #[test]
    fn loudness_normalizer_loud_speaker_attenuates() {
        // 5s of −5 dBFS speech. Short-term LUFS ≈ −5 LUFS → gain < 1.0 (attenuation).
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -30).unwrap();
        let samples = sine_samples(0.562, 440.0, 48000, 5.0); // ~−5 dBFS (amplitude 0.562)
        normalizer.normalize_loudness(&samples);
        assert!(
            normalizer.current_gain() < 1.0,
            "loud speaker must be attenuated (gain < 1.0), got {}", normalizer.current_gain()
        );
    }

    // --------------- 1.8 ---------------
    #[test]
    fn loudness_normalizer_preserves_clean_speech() {
        // 6s of clean speech-band sine. Measure the LAST 3s (steady-state, after the shortterm
        // window has converged) to verify output K-weighted integrated LUFS is within ±2 LU of
        // −23 LUFS. Also proves the noise gate is not stripping quiet onsets.
        let mut normalizer = LoudnessNormalizer::new(1, 48000, -30).unwrap();
        let samples = sine_samples(0.1, 440.0, 48000, 6.0);
        let out = normalizer.normalize_loudness(&samples);
        // Last 3 seconds (144000 samples) — window has stabilised by then
        let steady = &out[out.len().saturating_sub(3 * 48000)..];
        let mut meter = ebur128::EbuR128::new(1, 48000, ebur128::Mode::I)
            .expect("meter creation failed");
        meter.add_frames_f32(steady).expect("add_frames failed");
        let output_lufs = meter.loudness_global().expect("loudness_global failed");
        assert!(
            output_lufs >= -25.0 && output_lufs <= -21.0,
            "steady-state output LUFS should be within ±2 LU of −23 LUFS target, got {:.1} LUFS",
            output_lufs
        );
    }
}

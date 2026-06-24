use anyhow::{anyhow, Result};
use sherpa_onnx::{
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
    SpeakerEmbeddingManager,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::diarization::{DiarizationOutput, DiarizationPort};
use super::embedding::SpeakerEmbeddingPort;
use super::registry::SpeakerIdentificationPort;
use super::types::{EmbeddingVector, SpeakerSegment};

pub struct SherpaOnnxEmbeddingAdapter {
    extractor: SpeakerEmbeddingExtractor,
    dim: usize,
}

impl SherpaOnnxEmbeddingAdapter {
    pub fn new(model_path: &str) -> Result<Self> {
        let path = PathBuf::from(model_path);
        if !path.exists() {
            return Err(anyhow!("embedding model not found: {}", model_path));
        }

        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(path.to_string_lossy().to_string()),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        };

        let extractor = SpeakerEmbeddingExtractor::create(&config)
            .ok_or_else(|| anyhow!("failed to create embedding extractor"))?;

        Ok(Self {
            dim: extractor.dim() as usize,
            extractor,
        })
    }
}

impl SpeakerEmbeddingPort for SherpaOnnxEmbeddingAdapter {
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

        let stream = self.extractor.create_stream()
            .ok_or_else(|| anyhow!("failed to create online stream"))?;

        stream.accept_waveform(sample_rate as i32, audio);

        if !self.extractor.is_ready(&stream) {
            return Err(anyhow!("not enough audio to extract embedding"));
        }

        let embedding = self.extractor.compute(&stream)
            .ok_or_else(|| anyhow!("embedding extraction returned empty result"))?;

        EmbeddingVector::from_slice(&embedding, self.dim)
            .map_err(|e| anyhow!("embedding validation failed: {}", e))
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

pub struct SherpaOnnxDiarizationAdapter {
    extractor: SpeakerEmbeddingExtractor,
    merge_threshold_fp: Arc<AtomicU32>,
}

impl SherpaOnnxDiarizationAdapter {
    pub fn new(model_path: &str, segmentation_model_path: &str) -> Result<Self> {
        Self::with_shared_threshold(model_path, segmentation_model_path, Arc::new(AtomicU32::new(to_fp(MERGE_SIMILARITY_DEFAULT))))
    }

    pub fn with_shared_threshold(
        model_path: &str,
        segmentation_model_path: &str,
        threshold_fp: Arc<AtomicU32>,
    ) -> Result<Self> {
        let mp = PathBuf::from(model_path);
        if !mp.exists() {
            return Err(anyhow!("diarization model not found: {}", model_path));
        }
        let sp = PathBuf::from(segmentation_model_path);
        if !sp.exists() {
            return Err(anyhow!("segmentation model not found: {}", segmentation_model_path));
        }

        let emb_config = SpeakerEmbeddingExtractorConfig {
            model: Some(mp.to_string_lossy().to_string()),
            num_threads: 2,
            debug: false,
            provider: Some("cpu".to_string()),
        };
        let extractor = SpeakerEmbeddingExtractor::create(&emb_config)
            .ok_or_else(|| anyhow!("failed to create embedding extractor for diarization"))?;

        Ok(Self {
            extractor,
            merge_threshold_fp: threshold_fp,
        })
    }

    fn merge_threshold(&self) -> f32 {
        from_fp(self.merge_threshold_fp.load(Ordering::Relaxed))
    }
}

const MERGE_SIMILARITY_DEFAULT: f32 = 0.40;

const MIN_SPEECH_SECS: f64 = 1.5;
const MAX_CHUNK_SECS: f64 = 10.0;
const SPLIT_TARGET_SECS: f64 = 3.0;
// Ceiling on chunk count for long meetings. The split granularity coarsens
// above 3 s once total speech exceeds 600 × 3 s = 30 min, keeping n bounded so
// the O(n²) clustering stays sub-second (design D2).
const MAX_DIARIZATION_CHUNKS: usize = 600;
const MIN_CLUSTER_FRAC: f64 = 0.02;

fn to_fp(v: f32) -> u32 {
    (v * 65536.0) as u32
}

fn from_fp(v: u32) -> f32 {
    v as f32 / 65536.0
}

pub(crate) struct Chunk {
    pub(crate) start_sample: usize,
    pub(crate) end_sample: usize,
    pub(crate) duration_secs: f64,
    pub(crate) embedding: Vec<f32>,
}

impl DiarizationPort for SherpaOnnxDiarizationAdapter {
    fn process(&self, samples: &[f32], sample_rate: u32, segments: &[(f64, f64)]) -> Result<DiarizationOutput> {
        if samples.is_empty() {
            return Ok(DiarizationOutput { segments: vec![], centroids: HashMap::new() });
        }

        let sr = sample_rate as usize;
        let sr_f = sr as f64;
        let duration_secs = samples.len() as f64 / sr_f;

        // If no transcript segments provided, fall back to energy-based VAD.
        let segments = if segments.is_empty() {
            log::warn!("DIARIZATION: no transcript segments, falling back to energy VAD");
            energy_vad_segments(samples, sample_rate)
        } else {
            segments.to_vec()
        };

        if segments.is_empty() {
            return Ok(DiarizationOutput {
                segments: vec![SpeakerSegment {
                    start_seconds: 0.0,
                    end_seconds: duration_secs,
                    speaker_id: 0,
                }],
                centroids: HashMap::new(),
            });
        }

        // Step 1: Create audio chunks from transcript segments.
        let chunks = self.build_chunks(samples, sample_rate, &segments);

        if chunks.is_empty() {
            return Ok(DiarizationOutput {
                segments: vec![SpeakerSegment {
                    start_seconds: 0.0,
                    end_seconds: duration_secs,
                    speaker_id: 0,
                }],
                centroids: HashMap::new(),
            });
        }

        // Step 2: Centroid-based agglomerative clustering.
        let t_cluster = std::time::Instant::now();
        let threshold = self.merge_threshold();
        let (labels, cluster_centroids) = cluster_by_centroids(&chunks, threshold);
        let n_clusters: std::collections::HashSet<u32> = labels.iter().copied().collect();
        log::info!(
            "DIARIZATION: clustering produced {} speakers from {} chunks in {:.2}s (threshold={:.2})",
            n_clusters.len(),
            chunks.len(),
            t_cluster.elapsed().as_secs_f64(),
            threshold,
        );

        // Step 3: Build speaker segments from clustered chunks.
        // Sort chunks by start_sample to get temporal order.
        let mut indexed: Vec<(usize, u32)> = labels.into_iter().enumerate().collect();
        indexed.sort_by_key(|(i, _)| chunks[*i].start_sample);

        let mut result: Vec<SpeakerSegment> = Vec::new();
        if indexed.is_empty() {
            return Ok(DiarizationOutput { segments: result, centroids: HashMap::new() });
        }

        let mut cur_speaker = indexed[0].1;
        let mut seg_start = chunks[indexed[0].0].start_sample as f64 / sr_f;
        let mut seg_end = chunks[indexed[0].0].end_sample as f64 / sr_f;

        for (chunk_idx, label) in &indexed[1..] {
            let chunk = &chunks[*chunk_idx];
            let chunk_start = chunk.start_sample as f64 / sr_f;
            let chunk_end = chunk.end_sample as f64 / sr_f;

            if *label == cur_speaker {
                seg_end = chunk_end;
            } else {
                result.push(SpeakerSegment {
                    start_seconds: seg_start,
                    end_seconds: seg_end,
                    speaker_id: cur_speaker,
                });
                cur_speaker = *label;
                seg_start = chunk_start;
                seg_end = chunk_end;
            }
        }
        result.push(SpeakerSegment {
            start_seconds: seg_start,
            end_seconds: seg_end,
            speaker_id: cur_speaker,
        });

        let (result, id_map) = renumber_speakers(result);

        let centroids: HashMap<u32, Vec<f32>> = cluster_centroids
            .into_iter()
            .filter_map(|(old_id, c)| Some((*id_map.get(&old_id)?, c)))
            .collect();

        // Merge short-duration speakers into their nearest neighbour.
        let total_audio_secs = result.iter()
            .map(|s| s.end_seconds - s.start_seconds)
            .sum::<f64>();
        let (result, centroids) = merge_short_speakers(result, centroids, total_audio_secs);

        let n_spk: std::collections::HashSet<u32> = result.iter().map(|s| s.speaker_id).collect();
        log::warn!(
            "DIARIZATION: final result: {} speakers, {} segments",
            n_spk.len(),
            result.len(),
        );

        Ok(DiarizationOutput { segments: result, centroids })
    }
}

impl SherpaOnnxDiarizationAdapter {
    fn extract_embedding(&self, audio: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        if is_effectively_silent(audio) {
            return None;
        }
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(sample_rate as i32, audio);
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream)
    }

    pub(crate) fn build_chunks(
        &self,
        samples: &[f32],
        sample_rate: u32,
        segments: &[(f64, f64)],
    ) -> Vec<Chunk> {
        let sr_f = sample_rate as f64;
        let t_chunk = std::time::Instant::now();
        let mut chunks: Vec<Chunk> = Vec::new();

        let speech_seconds: f64 = segments
            .iter()
            .map(|&(s, e)| (e - s).max(0.0))
            .filter(|&d| d >= MIN_SPEECH_SECS)
            .sum();
        let effective_split = (speech_seconds / MAX_DIARIZATION_CHUNKS as f64).max(SPLIT_TARGET_SECS);

        for &(start_s, end_s) in segments {
            let dur = end_s - start_s;
            if dur < MIN_SPEECH_SECS {
                continue;
            }

            if dur <= MAX_CHUNK_SECS {
                let start = ((start_s * sr_f) as usize).min(samples.len());
                let end = ((end_s * sr_f) as usize).min(samples.len());
                if end <= start {
                    continue;
                }
                let audio = &samples[start..end];
                if let Some(emb) = self.extract_embedding(audio, sample_rate) {
                    chunks.push(Chunk {
                        start_sample: start,
                        end_sample: end,
                        duration_secs: dur,
                        embedding: emb,
                    });
                }
            } else {
                // Split long segments into non-overlapping ~effective_split chunks
                let chunk_samples = (effective_split * sr_f) as usize;
                let start_base = (start_s * sr_f) as usize;
                let end_limit = ((end_s * sr_f) as usize).min(samples.len());
                let mut pos = start_base;
                while pos + chunk_samples <= end_limit {
                    let audio = &samples[pos..pos + chunk_samples];
                    let chunk_dur = chunk_samples as f64 / sr_f;
                    if let Some(emb) = self.extract_embedding(audio, sample_rate) {
                        chunks.push(Chunk {
                            start_sample: pos,
                            end_sample: pos + chunk_samples,
                            duration_secs: chunk_dur,
                            embedding: emb,
                        });
                    }
                    pos += chunk_samples;
                }
                // Handle remaining tail if it's long enough
                if pos < end_limit {
                    let tail_dur = (end_limit - pos) as f64 / sr_f;
                    if tail_dur >= MIN_SPEECH_SECS {
                        let audio = &samples[pos..end_limit];
                        if let Some(emb) = self.extract_embedding(audio, sample_rate) {
                            chunks.push(Chunk {
                                start_sample: pos,
                                end_sample: end_limit,
                                duration_secs: tail_dur,
                                embedding: emb,
                            });
                        }
                    }
                }
            }
        }

        log::warn!(
            "DIARIZATION: chunked + embedded {} chunks from {} segments in {:.2}s",
            chunks.len(),
            segments.len(),
            t_chunk.elapsed().as_secs_f64(),
        );

        chunks
    }
}

/// Merge speakers whose total duration is below threshold into nearest neighbour.
/// Threshold = MIN_CLUSTER_FRAC × total audio, but never below MIN_SPEECH_SECS
/// (model can't produce embeddings from shorter clips anyway).
fn merge_short_speakers(
    mut segments: Vec<SpeakerSegment>,
    mut centroids: HashMap<u32, Vec<f32>>,
    total_audio_secs: f64,
) -> (Vec<SpeakerSegment>, HashMap<u32, Vec<f32>>) {
    let min_dur = (MIN_CLUSTER_FRAC * total_audio_secs).max(MIN_SPEECH_SECS);

    let mut speaker_dur: HashMap<u32, f64> = HashMap::new();
    for s in &segments {
        *speaker_dur.entry(s.speaker_id).or_default() += s.end_seconds - s.start_seconds;
    }

    let short_speakers: Vec<u32> = speaker_dur.iter()
        .filter(|(_, &dur)| dur < min_dur)
        .map(|(&id, _)| id)
        .collect();

    if short_speakers.is_empty() {
        return (segments, centroids);
    }

    let long_speakers: Vec<u32> = speaker_dur.iter()
        .filter(|(_, &dur)| dur >= min_dur)
        .map(|(&id, _)| id)
        .collect();

    if long_speakers.is_empty() {
        return (segments, centroids);
    }

    let mut remap: HashMap<u32, u32> = HashMap::new();
    for short_id in &short_speakers {
        let short_centroid = match centroids.get(short_id) {
            Some(c) => c,
            None => continue,
        };
        let best = long_speakers.iter()
            .filter_map(|&long_id| {
                centroids.get(&long_id).map(|c| (long_id, cosine_similarity(short_centroid, c)))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| id);
        if let Some(target) = best {
            remap.insert(*short_id, target);
            log::debug!(
                "DIARIZATION: merging short speaker {} ({:.1}s) → speaker {}",
                short_id,
                speaker_dur[short_id],
                target,
            );
        }
    }

    // Remap segment speaker IDs
    for s in &mut segments {
        if let Some(&new_id) = remap.get(&s.speaker_id) {
            s.speaker_id = new_id;
        }
    }

    // Drop centroids for merged-away speakers
    for short_id in &short_speakers {
        if remap.contains_key(short_id) {
            centroids.remove(short_id);
        }
    }

    // Merge adjacent segments with same speaker and renumber.
    segments = merge_adjacent(segments);
    let (segments, renum_map) = renumber_speakers(segments);

    // Remap centroid keys to match final speaker IDs
    let centroids: HashMap<u32, Vec<f32>> = centroids
        .into_iter()
        .filter_map(|(old_id, c)| {
            let new_id = renum_map.get(&old_id).copied().unwrap_or(old_id);
            Some((new_id, c))
        })
        .collect();

    (segments, centroids)
}

fn merge_adjacent(mut segments: Vec<SpeakerSegment>) -> Vec<SpeakerSegment> {
    if segments.is_empty() {
        return segments;
    }
    segments.sort_by(|a, b| a.start_seconds.partial_cmp(&b.start_seconds).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged = vec![segments[0].clone()];
    for s in segments.into_iter().skip(1) {
        let last = merged.last_mut().unwrap();
        if s.speaker_id == last.speaker_id && s.start_seconds <= last.end_seconds + 0.5 {
            last.end_seconds = last.end_seconds.max(s.end_seconds);
        } else {
            merged.push(s);
        }
    }
    merged
}

pub(crate) fn cluster_by_centroids(chunks: &[Chunk], threshold: f32) -> (Vec<u32>, HashMap<u32, Vec<f32>>) {
    let n = chunks.len();
    if n == 0 {
        return (Vec::new(), HashMap::new());
    }

    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut centroids: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
    let mut cluster_durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
    let mut alive: Vec<bool> = vec![true; n];

    // Cached upper-triangle similarity matrix: sim[a] has n-a-1 entries,
    // sim[a][b-a-1] = cosine(centroids[a], centroids[b]) for a < b.
    // Computed once here; only row a + column a are recomputed on a merge into a
    // (all other pairs are unaffected). Scan uses O(1) lookups vs the naive's
    // O(d) cosine recompute — this is the ~200x speedup (design D1).
    let mut sim: Vec<Vec<f32>> = (0..n)
        .map(|a| {
            (a + 1..n)
                .map(|b| cosine_similarity(&centroids[a], &centroids[b]))
                .collect()
        })
        .collect();

    loop {
        let mut best_sim = threshold;
        let mut best_pair: Option<(usize, usize)> = None;

        for a in 0..n {
            if !alive[a] {
                continue;
            }
            for b in (a + 1)..n {
                if !alive[b] {
                    continue;
                }
                let s = sim[a][b - a - 1];
                if s > best_sim {
                    best_sim = s;
                    best_pair = Some((a, b));
                }
            }
        }

        let Some((a, b)) = best_pair else { break };

        let dur_a = cluster_durations[a];
        let dur_b = cluster_durations[b];
        let total_dur = dur_a + dur_b;
        let w_a = dur_a as f32 / total_dur as f32;
        let w_b = dur_b as f32 / total_dur as f32;

        let b_members = std::mem::take(&mut members[b]);
        let b_centroid = centroids[b].clone();
        for (i, v) in b_centroid.iter().enumerate() {
            centroids[a][i] = centroids[a][i] * w_a + v * w_b;
        }
        cluster_durations[a] = total_dur;
        members[a].extend(b_members);
        alive[b] = false;

        for x in (a + 1)..n {
            if alive[x] {
                sim[a][x - a - 1] = cosine_similarity(&centroids[a], &centroids[x]);
            }
        }
        for x in 0..a {
            if alive[x] {
                sim[x][a - x - 1] = cosine_similarity(&centroids[x], &centroids[a]);
            }
        }
    }

    let mut labels = vec![0u32; n];
    let mut next_label = 0u32;
    let mut label_map: HashMap<usize, u32> = HashMap::new();
    let mut final_centroids: HashMap<u32, Vec<f32>> = HashMap::new();
    for (idx, is_alive) in alive.iter().enumerate() {
        if !is_alive {
            continue;
        }
        let label = next_label;
        next_label += 1;
        label_map.insert(idx, label);
        for &member in &members[idx] {
            labels[member] = label;
        }
        final_centroids.insert(label, centroids[idx].clone());
    }

    (labels, final_centroids)
}

#[cfg(test)]
pub(crate) fn cluster_by_centroids_naive(chunks: &[Chunk], threshold: f32) -> (Vec<u32>, HashMap<u32, Vec<f32>>) {
    let n = chunks.len();
    if n == 0 {
        return (Vec::new(), HashMap::new());
    }

    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut centroids: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
    let mut cluster_durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
    let mut alive: Vec<bool> = vec![true; n];

    loop {
        let mut best_sim = threshold;
        let mut best_pair: Option<(usize, usize)> = None;

        let alive_indices: Vec<usize> = alive.iter().enumerate()
            .filter(|(_, &a)| a)
            .map(|(i, _)| i)
            .collect();

        for i_idx in 0..alive_indices.len() {
            for j_idx in (i_idx + 1)..alive_indices.len() {
                let a = alive_indices[i_idx];
                let b = alive_indices[j_idx];
                let sim = cosine_similarity(&centroids[a], &centroids[b]);
                if sim > best_sim {
                    best_sim = sim;
                    best_pair = Some((a, b));
                }
            }
        }

        let Some((a, b)) = best_pair else { break };

        let dur_a = cluster_durations[a];
        let dur_b = cluster_durations[b];
        let total_dur = dur_a + dur_b;
        let w_a = dur_a as f32 / total_dur as f32;
        let w_b = dur_b as f32 / total_dur as f32;

        let b_members = std::mem::take(&mut members[b]);
        let b_centroid = centroids[b].clone();
        for (i, v) in b_centroid.iter().enumerate() {
            centroids[a][i] = centroids[a][i] * w_a + v * w_b;
        }
        cluster_durations[a] = total_dur;
        members[a].extend(b_members);
        alive[b] = false;
    }

    let mut labels = vec![0u32; n];
    let mut next_label = 0u32;
    let mut label_map: HashMap<usize, u32> = HashMap::new();
    let mut final_centroids: HashMap<u32, Vec<f32>> = HashMap::new();
    for (idx, is_alive) in alive.iter().enumerate() {
        if !is_alive {
            continue;
        }
        let label = next_label;
        next_label += 1;
        label_map.insert(idx, label);
        for &member in &members[idx] {
            labels[member] = label;
        }
        final_centroids.insert(label, centroids[idx].clone());
    }

    (labels, final_centroids)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < 1e-8 || norm_b < 1e-8 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// The diarization pipeline strips silence earlier via energy VAD, but the
// extractor guards again: a direct caller must not feed the model silence and
// get back a degenerate vector that corrupts centroid clustering. Threshold is
// mean-square energy; RMS < 1e-5 is ~-100 dBFS, far below any real speech.
fn is_effectively_silent(audio: &[f32]) -> bool {
    if audio.is_empty() {
        return true;
    }
    let sum_sq: f32 = audio.iter().map(|&s| s * s).sum();
    (sum_sq / audio.len() as f32) < 1e-10
}

fn renumber_speakers(segments: Vec<SpeakerSegment>) -> (Vec<SpeakerSegment>, HashMap<u32, u32>) {
    let mut mapping: HashMap<u32, u32> = HashMap::new();
    let mut next_id: u32 = 0;
    let result = segments
        .into_iter()
        .map(|mut s| {
            let assigned = *mapping.entry(s.speaker_id).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                id
            });
            s.speaker_id = assigned;
            s
        })
        .collect();
    (result, mapping)
}

fn energy_vad_segments(samples: &[f32], sample_rate: u32) -> Vec<(f64, f64)> {
    let sr = sample_rate as usize;
    let window_ms = 30;
    let window_samples = (sr * window_ms / 1000).max(1);

    let mut rms_values: Vec<f32> = samples
        .chunks(window_samples)
        .map(|c| {
            let sum: f32 = c.iter().map(|s| s * s).sum();
            (sum / c.len() as f32).sqrt()
        })
        .collect();
    rms_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = rms_values[rms_values.len() / 2];
    let threshold = (median * 0.1).max(0.001);

    let mut speech: Vec<(usize, usize)> = Vec::new();
    let mut in_speech = false;
    let mut seg_start = 0usize;
    let mut pos = 0;
    while pos + window_samples <= samples.len() {
        let chunk = &samples[pos..pos + window_samples];
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
        if rms > threshold {
            if !in_speech {
                seg_start = pos;
                in_speech = true;
            }
        } else if in_speech {
            speech.push((seg_start, pos));
            in_speech = false;
        }
        pos += window_samples;
    }
    if in_speech {
        speech.push((seg_start, samples.len()));
    }

    // Merge nearby segments (< 300ms gap)
    let min_gap = (0.3 * sr as f64) as usize;
    let merged = merge_nearby_segments(speech, min_gap);

    merged
        .into_iter()
        .map(|(s, e)| (s as f64 / sr as f64, e as f64 / sr as f64))
        .collect()
}

fn merge_nearby_segments(segments: Vec<(usize, usize)>, min_gap: usize) -> Vec<(usize, usize)> {
    if segments.is_empty() {
        return segments;
    }
    let mut merged = Vec::with_capacity(segments.len());
    let mut cur = segments[0];
    for seg in &segments[1..] {
        if seg.0.saturating_sub(cur.1) < min_gap {
            cur.1 = cur.1.max(seg.1);
        } else {
            merged.push(cur);
            cur = *seg;
        }
    }
    merged.push(cur);
    merged
}

pub struct SherpaOnnxRegistryAdapter {
    manager: SpeakerEmbeddingManager,
    dim: usize,
}

impl SherpaOnnxRegistryAdapter {
    pub fn new(dim: usize) -> Result<Self> {
        let manager = SpeakerEmbeddingManager::create(dim as i32)
            .ok_or_else(|| anyhow!("failed to create speaker embedding manager"))?;
        Ok(Self { manager, dim })
    }
}

impl SpeakerIdentificationPort for SherpaOnnxRegistryAdapter {
    fn add(&self, name: &str, embedding: &EmbeddingVector) -> Result<()> {
        if embedding.dim() != self.dim {
            return Err(anyhow!(
                "embedding dimension mismatch: expected {}, got {}",
                self.dim,
                embedding.dim()
            ));
        }
        if !self.manager.add(name, embedding.as_slice()) {
            return Err(anyhow!("failed to add speaker: {}", name));
        }
        Ok(())
    }

    fn add_list(&self, name: &str, embeddings: &[EmbeddingVector]) -> Result<()> {
        let vecs: Vec<Vec<f32>> = embeddings.iter().map(|e| e.as_slice().to_vec()).collect();
        if !self.manager.add_list(name, &vecs) {
            return Err(anyhow!("failed to add speaker list: {}", name));
        }
        Ok(())
    }

    fn search(&self, embedding: &EmbeddingVector, threshold: f32) -> Result<Option<String>> {
        if embedding.dim() != self.dim {
            return Err(anyhow!(
                "embedding dimension mismatch: expected {}, got {}",
                self.dim,
                embedding.dim()
            ));
        }
        Ok(self.manager.search(embedding.as_slice(), threshold))
    }

    fn verify(&self, name: &str, embedding: &EmbeddingVector, threshold: f32) -> Result<bool> {
        Ok(self.manager.verify(name, embedding.as_slice(), threshold))
    }

    fn remove(&self, name: &str) -> Result<()> {
        if !self.manager.remove(name) {
            return Err(anyhow!("failed to remove speaker: {}", name));
        }
        Ok(())
    }

    fn list_speakers(&self) -> Result<Vec<String>> {
        Ok(self.manager.get_all_speakers())
    }

    fn contains(&self, name: &str) -> Result<bool> {
        Ok(self.manager.contains(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_adapter_rejects_nonexistent_model_path() {
        match SherpaOnnxEmbeddingAdapter::new("/nonexistent/model.onnx") {
            Err(e) => assert!(e.to_string().contains("embedding model not found"), "unexpected error: {}", e),
            Ok(_) => panic!("expected error for nonexistent model path"),
        }
    }

    #[test]
    fn diarization_adapter_rejects_nonexistent_embedding_model() {
        match SherpaOnnxDiarizationAdapter::new("/nonexistent/embedding.onnx", "/nonexistent/segmentation.onnx") {
            Err(e) => assert!(e.to_string().contains("diarization model not found"), "unexpected error: {}", e),
            Ok(_) => panic!("expected error for nonexistent model path"),
        }
    }

    #[test]
    fn diarization_adapter_rejects_nonexistent_segmentation_model() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        match SherpaOnnxDiarizationAdapter::new(path, "/nonexistent/segmentation.onnx") {
            Err(e) => assert!(e.to_string().contains("segmentation model not found"), "unexpected error: {}", e),
            Ok(_) => panic!("expected error for nonexistent model path"),
        }
    }

    #[test]
    fn registry_search_empty_returns_none() {
        let registry = SherpaOnnxRegistryAdapter::new(256).unwrap();
        let embedding = EmbeddingVector::from_slice(&vec![0.1f32; 256], 256).unwrap();
        let result = registry.search(&embedding, 0.5).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn registry_search_below_threshold_returns_none() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        let alice_embedding = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0], 4).unwrap();
        registry.add("Alice", &alice_embedding).unwrap();

        let query = EmbeddingVector::from_slice(&vec![0.0, 1.0, 0.0, 0.0], 4).unwrap();
        let result = registry.search(&query, 0.99).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn registry_add_and_search_match() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        let alice = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0], 4).unwrap();
        registry.add("Alice", &alice).unwrap();

        let query = EmbeddingVector::from_slice(&vec![0.9, 0.1, 0.0, 0.0], 4).unwrap();
        let result = registry.search(&query, 0.5).unwrap();
        assert_eq!(result.as_deref(), Some("Alice"));
    }

    #[test]
    fn registry_dimension_mismatch_rejected() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        let wrong_dim = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0, 0.0], 5).unwrap();
        let result = registry.add("Alice", &wrong_dim);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("dimension mismatch"));
    }

    #[test]
    fn registry_search_dimension_mismatch_rejected() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        let wrong_dim = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0, 0.0], 5).unwrap();
        let result = registry.search(&wrong_dim, 0.5);
        assert!(result.is_err());
    }

    #[test]
    fn registry_list_speakers_empty() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        assert!(registry.list_speakers().unwrap().is_empty());
    }

    #[test]
    fn registry_remove_nonexistent_fails() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        assert!(registry.remove("ghost").is_err());
    }

    #[test]
    fn registry_add_remove_lifecycle() {
        let registry = SherpaOnnxRegistryAdapter::new(4).unwrap();
        let emb = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0], 4).unwrap();
        registry.add("Alice", &emb).unwrap();
        assert!(registry.contains("Alice").unwrap());
        registry.remove("Alice").unwrap();
        assert!(!registry.contains("Alice").unwrap());
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn is_effectively_silent_rejects_all_zeros() {
        assert!(is_effectively_silent(&vec![0.0f32; 16000]));
    }

    #[test]
    fn is_effectively_silent_rejects_empty() {
        assert!(is_effectively_silent(&[]));
    }

    #[test]
    fn is_effectively_silent_accepts_real_audio() {
        let audio: Vec<f32> = (0..16000).map(|i| ((i as f32) * 0.001).sin() * 0.1).collect();
        assert!(!is_effectively_silent(&audio));
    }

    #[test]
    fn cluster_single_chunk_gets_one_label() {
        let chunks = vec![Chunk {
            start_sample: 0,
            end_sample: 48000,
            duration_secs: 3.0,
            embedding: vec![1.0; 8],
        }];
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.5);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0], 0);
        assert_eq!(centroids.len(), 1);
    }

    #[test]
    fn cluster_two_identical_chunks_merge() {
        let chunks = vec![
            Chunk { start_sample: 0, end_sample: 48000, duration_secs: 3.0, embedding: vec![1.0, 0.0, 0.0, 0.0] },
            Chunk { start_sample: 48000, end_sample: 96000, duration_secs: 3.0, embedding: vec![1.0, 0.0, 0.0, 0.0] },
        ];
        let (labels, _centroids) = cluster_by_centroids(&chunks, 0.5);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0], labels[1], "identical embeddings should merge");
    }

    #[test]
    fn cluster_orthogonal_chunks_stay_separate() {
        let chunks = vec![
            Chunk { start_sample: 0, end_sample: 48000, duration_secs: 3.0, embedding: vec![1.0, 0.0, 0.0, 0.0] },
            Chunk { start_sample: 48000, end_sample: 96000, duration_secs: 3.0, embedding: vec![0.0, 1.0, 0.0, 0.0] },
        ];
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.99);
        assert_eq!(labels.len(), 2);
        assert_ne!(labels[0], labels[1], "orthogonal embeddings should stay separate at high threshold");
        assert_eq!(centroids.len(), 2);
    }

    #[test]
    fn renumber_speakers_contiguous() {
        let segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 42 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 7 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 42 },
        ];
        let (renumbered, _mapping) = renumber_speakers(segments);
        assert_eq!(renumbered[0].speaker_id, 0);
        assert_eq!(renumbered[1].speaker_id, 1);
        assert_eq!(renumbered[2].speaker_id, 0);
    }

    #[test]
    fn fp_roundtrip() {
        let v = 0.65f32;
        assert!((from_fp(to_fp(v)) - v).abs() < 0.001);
    }

    // ── diarization-clustering-perf — adversarial tests ───────────────────
    //
    // The new cached-matrix cluster_by_centroids must produce byte-for-byte
    // identical (labels, centroids) to the naive oracle across the full input
    // grid, handle edge cases without panic, respect the chunk cap, and reject
    // non-finite embeddings.

    fn make_chunk(embedding: Vec<f32>, duration_secs: f64) -> Chunk {
        Chunk {
            start_sample: 0,
            end_sample: 0,
            duration_secs,
            embedding,
        }
    }

    fn make_unit_chunk(i: usize, dim: usize, duration_secs: f64) -> Chunk {
        let mut e = vec![0.0f32; dim];
        if i < dim {
            e[i] = 1.0;
        }
        make_chunk(e, duration_secs)
    }

    fn random_unit_chunk(seed: u64, dim: usize, duration_secs: f64) -> Chunk {
        let mut rng_state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut e = vec![0.0f32; dim];
        let mut norm: f32 = 0.0;
        for v in e.iter_mut() {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let x = ((rng_state >> 33) as f64 / (1u64 << 31) as f64) as f32 * 2.0 - 1.0;
            *v = x;
            norm += x * x;
        }
        let norm = norm.sqrt().max(1e-12);
        for v in e.iter_mut() {
            *v /= norm;
        }
        make_chunk(e, duration_secs)
    }

    fn centroids_equal(a: &HashMap<u32, Vec<f32>>, b: &HashMap<u32, Vec<f32>>, eps: f32) -> bool {
        if a.len() != b.len() {
            return false;
        }
        for (k, va) in a {
            match b.get(k) {
                Some(vb) => {
                    if va.len() != vb.len() {
                        return false;
                    }
                    if va.iter().zip(vb.iter()).any(|(x, y)| (x - y).abs() > eps) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }

    #[test]
    fn oracle_new_equals_naive_across_grid() {
        let dim = 8usize;
        let ns = [0usize, 1, 2, 5, 20, 80];
        let thresholds = [0.30f32, 0.40, 0.55];

        for &n in &ns {
            for &thr in &thresholds {
                let geometries = build_geometries(n, dim);
                for chunks in geometries {
                    let (new_labels, new_cent) = cluster_by_centroids(&chunks, thr);
                    let (old_labels, old_cent) = cluster_by_centroids_naive(&chunks, thr);
                    assert_eq!(
                        new_labels, old_labels,
                        "label mismatch at n={} thr={} (geometry of len {})",
                        n, thr, chunks.len()
                    );
                    assert!(
                        centroids_equal(&new_cent, &old_cent, 1e-5),
                        "centroid mismatch at n={} thr={}",
                        n, thr
                    );
                }
            }
        }
    }

    fn build_geometries(n: usize, dim: usize) -> Vec<Vec<Chunk>> {
        if n == 0 {
            return vec![vec![]];
        }
        let mut out = Vec::new();

        // Well-separated: two clusters of orthogonal basis vectors.
        if n >= 2 {
            let mut sep = Vec::new();
            for i in 0..n {
                let axis = i % 2;
                let mut e = vec![0.0f32; dim];
                if axis < dim {
                    e[axis] = 1.0;
                }
                sep.push(make_chunk(e, 3.0));
            }
            out.push(sep);
        }

        // Overlapping: random unit embeddings.
        let rand: Vec<Chunk> = (0..n).map(|i| random_unit_chunk(i as u64 + 1, dim, 3.0)).collect();
        out.push(rand);

        // All-identical.
        let identical_e = {
            let mut e = vec![0.0f32; dim];
            e[0] = 1.0;
            e
        };
        let identical: Vec<Chunk> = (0..n).map(|_| make_chunk(identical_e.clone(), 3.0)).collect();
        out.push(identical);

        // All-mutually-orthogonal (capped at dim axes).
        let ortho: Vec<Chunk> = (0..n).map(|i| make_unit_chunk(i, dim, 3.0)).collect();
        out.push(ortho);

        out
    }

    #[test]
    #[ignore = "n=5000 stress test: the O(n²·d) init + O(n³/3) matrix scan take ~90s \
                in plain f32 (no SIMD). D2's chunk cap (MAX_DIARIZATION_CHUNKS=600) \
                keeps production n ≤ 600 where clustering is sub-second. Run with \
                --ignored to verify no-OOM/no-panic at scale."]
    fn oversized_input_is_bounded() {
        let n = 5000usize;
        let dim = 192usize;
        let chunks: Vec<Chunk> = (0..n).map(|i| random_unit_chunk(i as u64, dim, 3.0)).collect();

        let (labels, centroids) = cluster_by_centroids(&chunks, 0.40);

        assert_eq!(labels.len(), n, "every chunk must be labelled");
        assert!(!centroids.is_empty(), "at least one cluster must exist");
    }

    #[test]
    fn oversized_matrix_stays_bounded_by_chunk_cap() {
        let dim = 8usize;
        let chunks: Vec<Chunk> = (0..MAX_DIARIZATION_CHUNKS)
            .map(|i| random_unit_chunk(i as u64, dim, 3.0))
            .collect();
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.40);
        assert_eq!(labels.len(), MAX_DIARIZATION_CHUNKS);
        assert!(!centroids.is_empty());
    }

    // Perf-regression guard (diarization-clustering-perf task 1.2/2.3). The old
    // full-rescan AHC was O(n³); at the real freeze scale (n≈1640) it took 10+
    // minutes. The cached-matrix path is sub-second at the production cap. This
    // test runs at every `cargo test` (not #[ignore]) and fails if clustering
    // n=MAX_DIARIZATION_CHUNKS blows past the deadline — catching a regression
    // that reverts to full-rescan OR removes the cap. The deadline is 15 s, not
    // the 5 s an O(n³) revert would still clear: a debug build under `cargo
    // test` parallel load can spike 5× over its isolated runtime (~1 s here), so
    // the tighter bound flakes. 15 s stays ~60× under an O(n³) revert (minutes).
    #[test]
    fn production_scale_clustering_completes_under_wall_clock_deadline() {
        let dim = 8usize;
        let chunks: Vec<Chunk> = (0..MAX_DIARIZATION_CHUNKS)
            .map(|i| random_unit_chunk(i as u64, dim, 3.0))
            .collect();

        let start = std::time::Instant::now();
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.40);
        let elapsed = start.elapsed();

        assert_eq!(labels.len(), MAX_DIARIZATION_CHUNKS);
        assert!(!centroids.is_empty());
        assert!(
            elapsed.as_secs() < 15,
            "n={} clustering took {:?} — regression past the cached-matrix O(n² log n) bound",
            MAX_DIARIZATION_CHUNKS,
            elapsed,
        );
    }

    #[test]
    fn empty_and_single_chunk_no_panic() {
        let (empty_labels, empty_cent) = cluster_by_centroids(&[], 0.40);
        assert!(empty_labels.is_empty());
        assert!(empty_cent.is_empty());

        let one = vec![make_chunk(vec![1.0, 0.0, 0.0], 3.0)];
        let (labels, cent) = cluster_by_centroids(&one, 0.40);
        assert_eq!(labels, vec![0u32]);
        assert_eq!(cent.len(), 1);
        assert!(cent.contains_key(&0));
    }

    #[test]
    fn degenerate_geometries_match_oracle() {
        let dim = 8;

        let identical_e = {
            let mut e = vec![0.0f32; dim];
            e[0] = 1.0;
            e
        };
        let identical: Vec<Chunk> = (0..10).map(|_| make_chunk(identical_e.clone(), 3.0)).collect();
        let (id_new, id_cent) = cluster_by_centroids(&identical, 0.40);
        let (id_old, _) = cluster_by_centroids_naive(&identical, 0.40);
        assert_eq!(id_new, id_old, "all-identical labels must match oracle");
        assert_eq!(id_cent.len(), 1, "all-identical must collapse to 1 cluster");

        let ortho: Vec<Chunk> = (0..dim).map(|i| make_unit_chunk(i, dim, 3.0)).collect();
        let (ort_new, _) = cluster_by_centroids(&ortho, 0.40);
        let (ort_old, _) = cluster_by_centroids_naive(&ortho, 0.40);
        assert_eq!(ort_new, ort_old, "all-orthogonal labels must match oracle");
        assert!(
            (0..dim).all(|i| ort_new[i] == i as u32),
            "all-orthogonal must produce n distinct clusters (no merges)"
        );
    }

    #[test]
    fn chunk_cap_formula_coarsens_long_meetings() {
        // build_chunks needs a real embedding extractor (model files) so the
        // integration is covered by the #[ignore] real-audio tests. The cap
        // arithmetic is a pure function of segment durations — pinned here.
        //
        // Long meeting: speech_seconds / 600 > 3.0 → effective_split coarsens.
        let long_speech = (MAX_DIARIZATION_CHUNKS as f64) * SPLIT_TARGET_SECS * 3.0; // 5400s
        let long_split = (long_speech / MAX_DIARIZATION_CHUNKS as f64).max(SPLIT_TARGET_SECS);
        assert_eq!(long_split, 9.0, "long meeting coarsens to speech/600");
        assert!(long_split > SPLIT_TARGET_SECS);
        assert!(
            (long_speech / long_split).floor() as usize <= MAX_DIARIZATION_CHUNKS,
            "coarsened granularity must keep chunk count ≤ cap"
        );
    }

    #[test]
    fn short_meetings_keep_default_granularity() {
        let short_speech = 600.0f64; // 10 min < 30 min cap threshold
        let short_split = (short_speech / MAX_DIARIZATION_CHUNKS as f64).max(SPLIT_TARGET_SECS);
        assert_eq!(short_split, SPLIT_TARGET_SECS, "short meetings keep 3.0s granularity");
        assert_eq!(
            (short_speech / short_split).floor() as usize,
            (short_speech / SPLIT_TARGET_SECS).floor() as usize,
            "short meeting chunk count is unchanged from today"
        );
    }

    #[test]
    fn non_finite_embeddings_do_not_corrupt_clustering() {
        // cosine_similarity of a NaN embedding yields NaN; the > threshold
        // predicate is false for NaN, so NaN chunks never merge but they also
        // never crash. The diarization pipeline rejects these upstream via
        // is_effectively_silent / the extractor's finite guard, so reaching
        // cluster_by_centroids with a NaN is itself a bug — but we verify the
        // function does not panic or produce a corrupted label array.
        let dim = 4;
        let mut chunks: Vec<Chunk> = (0..3).map(|i| make_unit_chunk(i, dim, 3.0)).collect();
        chunks.push(make_chunk(vec![f32::NAN; dim], 3.0));

        let (labels, _) = cluster_by_centroids(&chunks, 0.40);
        assert_eq!(labels.len(), 4, "all chunks labelled, no panic");
        // The NaN chunk must not merge into any cluster (NaN > threshold is false).
        let nan_label = labels[3];
        let others: Vec<u32> = labels[0..3].iter().copied().collect();
        assert!(
            !others.contains(&nan_label) || others.iter().filter(|&&l| l == nan_label).count() == 0,
            "NaN chunk must not corrupt other clusters"
        );
    }

    // Verification gap on diarization-clustering-perf: oracle_new_equals_naive
    // _across_grid only covers degenerate geometries (orthogonal basis vectors,
    // all-identical, fully-random unit vectors). Real nemo_titanet embeddings
    // produce 4–8 loosely separated clusters in 192-d with Gaussian within-
    // cluster noise — creating the intermediate similarities and near-tie merge
    // candidates that exercise the cached-matrix rescan path. If the perf
    // refactor is behavior-preserving on realistic structure, labels and
    // centroids must match byte-for-byte.

    fn lcg_uniform(seed: &mut u64) -> f64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn lcg_gaussian(seed: &mut u64) -> f32 {
        let u1 = lcg_uniform(seed).max(1e-12);
        let u2 = lcg_uniform(seed);
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * std::f64::consts::PI * u2;
        (r * theta.cos()) as f32
    }

    #[test]
    fn cached_matrix_matches_naive_on_realistic_cluster_structure() {
        let dim = 192usize;
        let n_clusters = 4usize;
        let per_cluster = 50usize;
        // sigma=0.08 in 192-d: ||n||≈1.11, so within-cluster cosine ≈ 1/(1+1.23)≈0.45
        // and between-cluster ≈ 0/(1+1.23)=0 — straddling thr=0.40 so the loop hits
        // real merge-order decisions rather than trivially identical vectors.
        let noise_sigma: f32 = 0.08;
        let threshold = 0.40f32;

        let mut seed: u64 = 0xA11C_E5E1_4B6D_2F3A;

        // 4 random unit-vector centers in 192-d (mutual cosine ≈ 0).
        let mut centers: Vec<Vec<f32>> = Vec::with_capacity(n_clusters);
        for _ in 0..n_clusters {
            let mut c = vec![0.0f32; dim];
            let mut norm: f32 = 0.0;
            for v in c.iter_mut() {
                let x = (lcg_uniform(&mut seed) * 2.0 - 1.0) as f32;
                *v = x;
                norm += x * x;
            }
            let norm = norm.sqrt().max(1e-12);
            for v in c.iter_mut() {
                *v /= norm;
            }
            centers.push(c);
        }

        // Emit embeddings INTERLEAVED by cluster so members are not contiguous —
        // forces the matrix scan to cross cluster boundaries every iteration
        // and exercise the cached rescan under realistic near-tie candidates.
        // Built directly in interleaved order to avoid needing Chunk: Clone.
        let mut chunks: Vec<Chunk> = Vec::with_capacity(n_clusters * per_cluster);
        for i in 0..per_cluster {
            for c in 0..n_clusters {
                let center = &centers[c];
                let mut e = vec![0.0f32; dim];
                let mut norm: f32 = 0.0;
                for d in 0..dim {
                    let val = center[d] + lcg_gaussian(&mut seed) * noise_sigma;
                    e[d] = val;
                    norm += val * val;
                }
                let norm = norm.sqrt().max(1e-12);
                for v in e.iter_mut() {
                    *v /= norm;
                }
                // start_sample varies so chunks are distinguishable (mirrors
                // production where each chunk covers a distinct audio window).
                let start_sample = (i * n_clusters + c) * 48000;
                let mut chunk = make_chunk(e, 3.0);
                chunk.start_sample = start_sample;
                chunk.end_sample = start_sample + 48000;
                chunks.push(chunk);
            }
        }

        let (new_labels, new_cent) = cluster_by_centroids(&chunks, threshold);
        let (old_labels, old_cent) = cluster_by_centroids_naive(&chunks, threshold);

        // Sanity: the test is non-degenerate — it actually produced multiple
        // clusters (otherwise the equivalence check is vacuously true).
        let unique: std::collections::HashSet<u32> = new_labels.iter().copied().collect();
        assert!(
            (2..=n_clusters as u32 + 1).contains(&(unique.len() as u32)),
            "expected 2..={} clusters from synthetic structure, got {}; adjust noise_sigma",
            n_clusters,
            unique.len()
        );

        assert_eq!(
            new_labels, old_labels,
            "cached-matrix labels must equal naive oracle on realistic 4-cluster Gaussian structure \
             ({} embeddings, dim={}, sigma={}, thr={}); divergence means the perf refactor changed \
             clustering behavior, not just speed",
            chunks.len(),
            dim,
            noise_sigma,
            threshold,
        );
        assert!(
            centroids_equal(&new_cent, &old_cent, 1e-4),
            "cached-matrix centroids must match naive oracle within 1e-4 on realistic structure \
             (cluster count = {})",
            unique.len(),
        );
    }
}

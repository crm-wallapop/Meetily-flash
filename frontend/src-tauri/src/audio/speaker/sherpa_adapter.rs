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
                // Split long segments into non-overlapping ~SPLIT_TARGET_SECS chunks
                let chunk_samples = (SPLIT_TARGET_SECS * sr_f) as usize;
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

fn cluster_by_centroids(chunks: &[Chunk], threshold: f32) -> (Vec<u32>, HashMap<u32, Vec<f32>>) {
    let n = chunks.len();
    if n == 0 {
        return (Vec::new(), HashMap::new());
    }

    // Each chunk starts as its own cluster.
    // cluster_members[i] = set of original indices in cluster i
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

        // Merge b into a: duration-weighted centroid average.
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

    // Build label array and collect final centroids per cluster.
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
}

use anyhow::{anyhow, Result};
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
    SpeakerEmbeddingManager,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::diarization::DiarizationPort;
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
    model_path: PathBuf,
    segmentation_model_path: PathBuf,
    embedding_extractor: SpeakerEmbeddingExtractor,
    embedding_dim: usize,
    // Shared with AppState — lock-free updates from the settings UI.
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
        let sp = PathBuf::from(segmentation_model_path);
        if !mp.exists() {
            return Err(anyhow!("diarization model not found: {}", model_path));
        }
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
        let dim = extractor.dim() as usize;

        Ok(Self {
            model_path: mp,
            segmentation_model_path: sp,
            embedding_extractor: extractor,
            embedding_dim: dim,
            merge_threshold_fp: threshold_fp,
        })
    }

    fn merge_threshold(&self) -> f32 {
        from_fp(self.merge_threshold_fp.load(Ordering::Relaxed))
    }

    fn create_diarization(&self) -> Result<OfflineSpeakerDiarization> {
        let config = OfflineSpeakerDiarizationConfig {
            segmentation: OfflineSpeakerSegmentationModelConfig {
                pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: Some(self.segmentation_model_path.to_string_lossy().to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
            embedding: SpeakerEmbeddingExtractorConfig {
                model: Some(self.model_path.to_string_lossy().to_string()),
                num_threads: 2,
                debug: false,
                provider: Some("cpu".to_string()),
            },
            clustering: FastClusteringConfig {
                num_clusters: -1,
                threshold: 0.80,
            },
            ..Default::default()
        };

        OfflineSpeakerDiarization::create(&config)
            .ok_or_else(|| anyhow!("failed to create diarization"))
    }

    /// Extract a single embedding from an audio chunk at `sample_rate`.
    fn extract_embedding(&self, audio: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        let stream = self.embedding_extractor.create_stream()?;
        stream.accept_waveform(sample_rate as i32, audio);
        if !self.embedding_extractor.is_ready(&stream) {
            return None;
        }
        self.embedding_extractor.compute(&stream)
    }
}

/// Speakers whose total airtime is below this fraction of the audio duration
/// are considered noise and merged into the temporally nearest dominant speaker.
const MIN_SPEAKER_FRACTION: f64 = 0.03;

/// Cosine similarity above which two speaker clusters are merged in the
/// second-pass centroid clustering.  This is the default; the user can
/// override it via the settings slider.
const MERGE_SIMILARITY_DEFAULT: f32 = 0.50;

fn to_fp(v: f32) -> u32 {
    (v * 65536.0) as u32
}

fn from_fp(v: u32) -> f32 {
    v as f32 / 65536.0
}

impl DiarizationPort for SherpaOnnxDiarizationAdapter {
    fn process(&self, samples: &[f32], _sample_rate: u32) -> Result<Vec<SpeakerSegment>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        let sd = self.create_diarization()?;

        let mut segments: Vec<SpeakerSegment> = match sd.process(samples) {
            Some(result) => result
                .sort_by_start_time()
                .into_iter()
                .map(|s| SpeakerSegment {
                    start_seconds: s.start as f64,
                    end_seconds: s.end as f64,
                    speaker_id: s.speaker as u32,
                })
                .collect(),
            None => return Ok(Vec::new()),
        };

        if segments.is_empty() {
            return Ok(Vec::new());
        }

        // Pass 1: absorb tiny noise clusters into nearest dominant speaker.
        let audio_duration = segments
            .iter()
            .map(|s| s.end_seconds - s.start_seconds)
            .sum::<f64>();
        let min_speaker_duration = audio_duration * MIN_SPEAKER_FRACTION;
        segments = merge_small_speakers(segments, min_speaker_duration);

        // Pass 2: re-cluster speaker centroids by extracting embeddings from
        // each speaker's audio and merging those whose cosine similarity exceeds
        // the threshold.  This catches over-segmentation that threshold tuning
        // alone cannot fix.
        segments = merge_similar_speakers(
            &self.embedding_extractor,
            self.embedding_dim,
            segments,
            samples,
            16000,
            self.merge_threshold(),
        );

        segments = renumber_speakers(segments);
        Ok(segments)
    }
}

/// Reassign segments from speakers with total duration < `min_duration`
/// to the temporally nearest dominant speaker.
fn merge_small_speakers(
    segments: Vec<SpeakerSegment>,
    min_duration: f64,
) -> Vec<SpeakerSegment> {
    use std::collections::HashMap;

    let mut duration_per_speaker: HashMap<u32, f64> = HashMap::new();
    for s in &segments {
        *duration_per_speaker
            .entry(s.speaker_id)
            .or_insert(0.0) += s.end_seconds - s.start_seconds;
    }

    let dominant: std::collections::HashSet<u32> = duration_per_speaker
        .iter()
        .filter(|(_, &dur)| dur >= min_duration)
        .map(|(&id, _)| id)
        .collect();

    if dominant.is_empty() || dominant.len() == duration_per_speaker.len() {
        return segments;
    }

    // Pre-compute dominant midpoints for nearest-neighbor lookup.
    let dominant_midpoints: Vec<(u32, f64)> = segments
        .iter()
        .filter(|s| dominant.contains(&s.speaker_id))
        .map(|s| (s.speaker_id, (s.start_seconds + s.end_seconds) / 2.0))
        .collect();

    segments
        .into_iter()
        .map(|mut s| {
            if dominant.contains(&s.speaker_id) {
                return s;
            }
            let mid = (s.start_seconds + s.end_seconds) / 2.0;
            let mut best_id = s.speaker_id;
            let mut best_dist = f64::MAX;
            for &(id, o_mid) in &dominant_midpoints {
                let dist = (mid - o_mid).abs();
                if dist < best_dist {
                    best_dist = dist;
                    best_id = id;
                }
            }
            s.speaker_id = best_id;
            s
        })
        .collect()
}

/// Second-pass clustering: extract an average embedding per speaker from the
/// original audio, then agglomeratively merge speakers whose centroid cosine
/// similarity is above `threshold`.
fn merge_similar_speakers(
    extractor: &SpeakerEmbeddingExtractor,
    dim: usize,
    segments: Vec<SpeakerSegment>,
    samples: &[f32],
    sample_rate: u32,
    threshold: f32,
) -> Vec<SpeakerSegment> {
    use std::collections::HashMap;

    let speaker_ids: Vec<u32> = segments
        .iter()
        .map(|s| s.speaker_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    if speaker_ids.len() <= 1 {
        return segments;
    }

    // Build speaker → list of (start_sample, end_sample) from segments.
    let sr = sample_rate as f64;
    let mut chunks: HashMap<u32, Vec<(usize, usize)>> = HashMap::new();
    for s in &segments {
        let start = ((s.start_seconds * sr) as usize).min(samples.len());
        let end = ((s.end_seconds * sr) as usize).min(samples.len());
        if end > start {
            chunks.entry(s.speaker_id).or_default().push((start, end));
        }
    }

    // Extract average embedding per speaker.
    let mut centroids: HashMap<u32, Vec<f32>> = HashMap::new();
    for &sid in &speaker_ids {
        let Some(speaker_chunks) = chunks.get(&sid) else {
            continue;
        };
        let mut sum = vec![0.0f32; dim];
        let mut count = 0usize;
        for &(start, end) in speaker_chunks {
            let audio = &samples[start..end];
            let stream = match extractor.create_stream() {
                Some(s) => s,
                None => continue,
            };
            stream.accept_waveform(sample_rate as i32, audio);
            if !extractor.is_ready(&stream) {
                continue;
            }
            let emb = match extractor.compute(&stream) {
                Some(e) => e,
                None => continue,
            };
            for (i, v) in emb.iter().enumerate() {
                sum[i] += v;
            }
            count += 1;
        }
        if count > 0 {
            for v in &mut sum {
                *v /= count as f32;
            }
            centroids.insert(sid, sum);
        }
    }

    if centroids.len() <= 1 {
        return segments;
    }

    // Agglomerative merge: repeatedly find the most similar pair above threshold.
    // `mapping[sid]` = the canonical speaker ID after merges.
    let mut mapping: HashMap<u32, u32> = HashMap::new();
    for &sid in &speaker_ids {
        mapping.insert(sid, sid);
    }
    let mut merged_centroids: HashMap<u32, Vec<f32>> = centroids.clone();

    loop {
        let mut best_sim = threshold;
        let mut best_pair: Option<(u32, u32)> = None;

        let ids: Vec<u32> = mapping.values().copied().collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                let a = ids[i];
                let b = ids[j];
                let ca = merged_centroids.get(&a);
                let cb = merged_centroids.get(&b);
                if let (Some(ca), Some(cb)) = (ca, cb) {
                    let sim = cosine_similarity(ca, cb);
                    if sim > best_sim {
                        best_sim = sim;
                        best_pair = Some((a, b));
                    }
                }
            }
        }

        let Some((a, b)) = best_pair else { break };

        // Merge b into a: average their centroids weighted by chunk count.
        let new_a = if let (Some(ca), Some(cb)) =
            (merged_centroids.get(&a).cloned(), merged_centroids.get(&b).cloned())
        {
            ca.iter().zip(cb.iter()).map(|(x, y)| (x + y) / 2.0).collect()
        } else {
            continue;
        };
        merged_centroids.insert(a, new_a);
        merged_centroids.remove(&b);

        for (_, canonical) in mapping.iter_mut() {
            if *canonical == b {
                *canonical = a;
            }
        }
    }

    segments
        .into_iter()
        .map(|mut s| {
            s.speaker_id = *mapping.get(&s.speaker_id).unwrap_or(&s.speaker_id);
            s
        })
        .collect()
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

/// Renumber speaker IDs contiguously from 0, preserving order of first appearance.
fn renumber_speakers(segments: Vec<SpeakerSegment>) -> Vec<SpeakerSegment> {
    use std::collections::HashMap;

    let mut mapping: HashMap<u32, u32> = HashMap::new();
    let mut next_id: u32 = 0;
    segments
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
        .collect()
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
            Ok(_) => panic!("expected error for nonexistent segmentation model path"),
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
    fn merge_small_speakers_absorbs_noise() {
        // Speaker 0: 60s total (dominant).  Speaker 1: 0.5s (noise).
        let segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 60.0, speaker_id: 0 },
            SpeakerSegment { start_seconds: 30.0, end_seconds: 30.5, speaker_id: 1 },
        ];
        // min_duration = 2.0s → speaker 1 is noise, gets merged to speaker 0.
        let merged = merge_small_speakers(segments, 2.0);
        assert!(merged.iter().all(|s| s.speaker_id == 0));
    }

    #[test]
    fn merge_small_speakers_keeps_dominant() {
        // Both speakers above threshold → no change.
        let segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 10.0, speaker_id: 0 },
            SpeakerSegment { start_seconds: 10.0, end_seconds: 20.0, speaker_id: 1 },
        ];
        let merged = merge_small_speakers(segments, 1.0);
        assert_eq!(merged[0].speaker_id, 0);
        assert_eq!(merged[1].speaker_id, 1);
    }

    #[test]
    fn renumber_speakers_contiguous() {
        let segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 42 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 7 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 42 },
        ];
        let renumbered = renumber_speakers(segments);
        assert_eq!(renumbered[0].speaker_id, 0); // 42 → 0
        assert_eq!(renumbered[1].speaker_id, 1); // 7 → 1
        assert_eq!(renumbered[2].speaker_id, 0); // 42 → 0
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
}

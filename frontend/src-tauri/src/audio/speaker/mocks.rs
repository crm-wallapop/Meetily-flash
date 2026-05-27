use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};

use super::diarization::DiarizationPort;
use super::embedding::SpeakerEmbeddingPort;
use super::registry::SpeakerIdentificationPort;
use super::types::{EmbeddingVector, SpeakerSegment};

const DEFAULT_EMBEDDING_DIM: usize = 256;
const MIN_AUDIO_SAMPLES: usize = 48000; // ~1s at 48kHz

/// Mock embedding extractor for testing.
pub struct MockEmbeddingPort {
    dim: usize,
    min_samples: usize,
}

impl MockEmbeddingPort {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            min_samples: MIN_AUDIO_SAMPLES,
        }
    }

    pub fn with_min_samples(mut self, n: usize) -> Self {
        self.min_samples = n;
        self
    }
}

impl SpeakerEmbeddingPort for MockEmbeddingPort {
    fn extract(&self, audio: &[f32], _sample_rate: u32) -> Result<EmbeddingVector> {
        if audio.len() < self.min_samples {
            return Err(anyhow!(
                "audio too short: {} samples (minimum {})",
                audio.len(),
                self.min_samples
            ));
        }
        // Generate a deterministic embedding based on audio energy
        let energy: f32 = audio.iter().map(|s| s * s).sum::<f32>() / audio.len() as f32;
        let base = (energy * 100.0).fract() as f32 * 0.5 + 0.1;
        let values = vec![base; self.dim];
        EmbeddingVector::from_slice(&values, self.dim).map_err(|e| anyhow!(e.to_string()))
    }

    fn dim(&self) -> usize {
        self.dim
    }
}

/// Mock speaker registry for testing.
pub struct MockIdentificationPort {
    inner: Arc<Mutex<MockRegistryInner>>,
}

struct MockRegistryInner {
    speakers: HashMap<String, Vec<EmbeddingVector>>,
    threshold: f32,
}

impl MockIdentificationPort {
    pub fn new(threshold: f32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MockRegistryInner {
                speakers: HashMap::new(),
                threshold,
            })),
        }
    }
}

impl SpeakerIdentificationPort for MockIdentificationPort {
    fn add(&self, name: &str, embedding: &EmbeddingVector) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner
            .speakers
            .entry(name.to_string())
            .or_default()
            .push(embedding.clone());
        Ok(())
    }

    fn add_list(&self, name: &str, embeddings: &[EmbeddingVector]) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let entry = inner.speakers.entry(name.to_string()).or_default();
        for e in embeddings {
            entry.push(e.clone());
        }
        Ok(())
    }

    fn search(&self, embedding: &EmbeddingVector, threshold: f32) -> Result<Option<String>> {
        let inner = self.inner.lock().unwrap();
        let mut best: Option<(String, f32)> = None;
        for (name, stored) in &inner.speakers {
            for stored_emb in stored {
                let sim = cosine_similarity(embedding.as_slice(), stored_emb.as_slice());
                if sim >= threshold {
                    if best.as_ref().map(|(_, s)| sim > *s).unwrap_or(true) {
                        best = Some((name.clone(), sim));
                    }
                }
            }
        }
        Ok(best.map(|(name, _)| name))
    }

    fn verify(&self, name: &str, embedding: &EmbeddingVector, threshold: f32) -> Result<bool> {
        let inner = self.inner.lock().unwrap();
        if let Some(stored) = inner.speakers.get(name) {
            for stored_emb in stored {
                let sim = cosine_similarity(embedding.as_slice(), stored_emb.as_slice());
                if sim >= threshold {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn remove(&self, name: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.speakers.remove(name);
        Ok(())
    }

    fn list_speakers(&self) -> Result<Vec<String>> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.speakers.keys().cloned().collect())
    }

    fn contains(&self, name: &str) -> Result<bool> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.speakers.contains_key(name))
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Mock diarization port for testing.
pub struct MockDiarizationPort {
    segments: Vec<SpeakerSegment>,
}

impl MockDiarizationPort {
    pub fn new(segments: Vec<SpeakerSegment>) -> Self {
        Self { segments }
    }

    pub fn single_speaker(duration_s: f64) -> Self {
        Self {
            segments: vec![SpeakerSegment {
                start_seconds: 0.0,
                end_seconds: duration_s,
                speaker_id: 1,
            }],
        }
    }

    pub fn silence() -> Self {
        Self { segments: vec![] }
    }
}

impl DiarizationPort for MockDiarizationPort {
    fn process(&self, _samples: &[f32], _sample_rate: u32) -> Result<Vec<SpeakerSegment>> {
        Ok(self.segments.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_DIM: usize = 4;

    fn test_embedding(values: &[f32]) -> EmbeddingVector {
        EmbeddingVector::from_slice(values, TEST_DIM).unwrap()
    }

    // ── 3.4: MockEmbeddingPort returns error when audio < minimum ─────

    #[test]
    fn mock_embedding_rejects_short_audio() {
        let port = MockEmbeddingPort::new(TEST_DIM).with_min_samples(100);
        let short_audio = vec![0.1f32; 50];
        let result = port.extract(&short_audio, 16000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("too short"));
    }

    // ── 3.5: Minimum audio duration enforced ──────────────────────────

    #[test]
    fn mock_embedding_accepts_sufficient_audio() {
        let port = MockEmbeddingPort::new(TEST_DIM).with_min_samples(100);
        let audio = vec![0.5f32; 200];
        let result = port.extract(&audio, 16000);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().dim(), TEST_DIM);
    }

    #[test]
    fn mock_registry_add_and_search() {
        let registry = MockIdentificationPort::new(0.5);
        let emb = test_embedding(&[0.1, 0.2, 0.3, 0.4]);

        registry.add("Alice", &emb).unwrap();

        let same = test_embedding(&[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(registry.search(&same, 0.5).unwrap(), Some("Alice".to_string()));

        // Orthogonal vector: cosine similarity should be ~0
        let diff = test_embedding(&[-0.4, 0.3, -0.2, 0.1]);
        assert_eq!(registry.search(&diff, 0.99).unwrap(), None);
    }

    #[test]
    fn mock_registry_empty_search_returns_none() {
        let registry = MockIdentificationPort::new(0.5);
        let emb = test_embedding(&[0.1, 0.2, 0.3, 0.4]);
        assert_eq!(registry.search(&emb, 0.5).unwrap(), None);
    }

    #[test]
    fn mock_registry_remove() {
        let registry = MockIdentificationPort::new(0.5);
        let emb = test_embedding(&[0.1, 0.2, 0.3, 0.4]);
        registry.add("Alice", &emb).unwrap();
        registry.remove("Alice").unwrap();
        assert!(!registry.contains("Alice").unwrap());
    }

    #[test]
    fn mock_diarization_returns_configured_segments() {
        let port = MockDiarizationPort::new(vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 5.0, speaker_id: 1 },
            SpeakerSegment { start_seconds: 5.0, end_seconds: 10.0, speaker_id: 2 },
        ]);
        let result = port.process(&[0.0; 1000], 16000).unwrap();
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn mock_diarization_silence() {
        let port = MockDiarizationPort::silence();
        let result = port.process(&[0.0; 1000], 16000).unwrap();
        assert!(result.is_empty());
    }
}

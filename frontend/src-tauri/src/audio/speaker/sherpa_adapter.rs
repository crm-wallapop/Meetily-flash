use anyhow::{anyhow, Result};
use sherpa_onnx::{
    OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig,
    SpeakerEmbeddingManager,
};
use std::path::PathBuf;

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
}

impl SherpaOnnxDiarizationAdapter {
    pub fn new(model_path: &str, segmentation_model_path: &str) -> Result<Self> {
        let mp = PathBuf::from(model_path);
        let sp = PathBuf::from(segmentation_model_path);
        if !mp.exists() {
            return Err(anyhow!("diarization model not found: {}", model_path));
        }
        if !sp.exists() {
            return Err(anyhow!("segmentation model not found: {}", segmentation_model_path));
        }
        Ok(Self {
            model_path: mp,
            segmentation_model_path: sp,
        })
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
            ..Default::default()
        };

        OfflineSpeakerDiarization::create(&config)
            .ok_or_else(|| anyhow!("failed to create diarization"))
    }
}

impl DiarizationPort for SherpaOnnxDiarizationAdapter {
    fn process(&self, samples: &[f32], _sample_rate: u32) -> Result<Vec<SpeakerSegment>> {
        if samples.is_empty() {
            return Ok(Vec::new());
        }

        let sd = self.create_diarization()?;

        match sd.process(samples) {
            Some(result) => {
                let mapped = result.sort_by_start_time()
                    .into_iter()
                    .map(|s| SpeakerSegment {
                        start_seconds: s.start as f64,
                        end_seconds: s.end as f64,
                        speaker_id: s.speaker as u32,
                    })
                    .collect();
                Ok(mapped)
            }
            None => Ok(Vec::new()),
        }
    }
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
}

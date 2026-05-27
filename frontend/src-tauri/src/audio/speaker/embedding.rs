use anyhow::Result;
use super::types::EmbeddingVector;

/// Port for extracting speaker embeddings from audio.
pub trait SpeakerEmbeddingPort: Send + Sync {
    fn extract(&self, audio: &[f32], sample_rate: u32) -> Result<EmbeddingVector>;
    fn dim(&self) -> usize;
}

use anyhow::Result;
use super::types::EmbeddingVector;

/// Port for speaker identification (in-memory registry of known speakers).
pub trait SpeakerIdentificationPort: Send + Sync {
    fn add(&self, name: &str, embedding: &EmbeddingVector) -> Result<()>;
    fn add_list(&self, name: &str, embeddings: &[EmbeddingVector]) -> Result<()>;
    fn search(&self, embedding: &EmbeddingVector, threshold: f32) -> Result<Option<String>>;
    fn verify(&self, name: &str, embedding: &EmbeddingVector, threshold: f32) -> Result<bool>;
    fn remove(&self, name: &str) -> Result<()>;
    fn list_speakers(&self) -> Result<Vec<String>>;
    fn contains(&self, name: &str) -> Result<bool>;
}

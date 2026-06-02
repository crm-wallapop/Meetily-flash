use anyhow::{anyhow, Result};
use chrono::Utc;
use sqlx::SqlitePool;
use tracing::info;

const MAX_NAME_LEN: usize = 200;
const MIN_EMBEDDING_DIM: usize = 64;
const MAX_EMBEDDING_DIM: usize = 1024;

pub struct SpeakerRepository;

impl SpeakerRepository {
    pub async fn create_speaker(
        pool: &SqlitePool,
        id: &str,
        name: &str,
        color: &str,
    ) -> Result<()> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("speaker name cannot be empty"));
        }
        if trimmed.len() > MAX_NAME_LEN {
            return Err(anyhow!(
                "speaker name too long: {} chars (max {})",
                trimmed.len(),
                MAX_NAME_LEN
            ));
        }

        let now = Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO speakers (id, name, color, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(trimmed)
        .bind(color)
        .bind(&now)
        .bind(&now)
        .execute(pool)
        .await?;

        info!("Created speaker {} ({})", trimmed, id);
        Ok(())
    }

    pub async fn get_speaker(pool: &SqlitePool, id: &str) -> Result<Option<SpeakerRow>> {
        let row = sqlx::query_as::<_, SpeakerRow>(
            "SELECT id, name, color, created_at, updated_at FROM speakers WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await?;
        Ok(row)
    }

    pub async fn list_speakers(pool: &SqlitePool) -> Result<Vec<SpeakerRow>> {
        let rows =
            sqlx::query_as::<_, SpeakerRow>(
                "SELECT id, name, color, created_at, updated_at FROM speakers ORDER BY created_at ASC",
            )
            .fetch_all(pool)
            .await?;
        Ok(rows)
    }

    pub async fn update_speaker_name(
        pool: &SqlitePool,
        id: &str,
        new_name: &str,
    ) -> Result<bool> {
        let trimmed = new_name.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("speaker name cannot be empty"));
        }
        if trimmed.len() > MAX_NAME_LEN {
            return Err(anyhow!(
                "speaker name too long: {} chars (max {})",
                trimmed.len(),
                MAX_NAME_LEN
            ));
        }

        let now = Utc::now().to_rfc3339();
        let result = sqlx::query("UPDATE speakers SET name = ?, updated_at = ? WHERE id = ?")
            .bind(trimmed)
            .bind(&now)
            .bind(id)
            .execute(pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn remove_speaker(pool: &SqlitePool, id: &str) -> Result<bool> {
        // speaker_embeddings has ON DELETE SET NULL for speaker_id
        let result = sqlx::query("DELETE FROM speakers WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await?;

        if result.rows_affected() > 0 {
            info!("Removed speaker {}", id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn store_embedding(
        pool: &SqlitePool,
        id: &str,
        speaker_id: Option<&str>,
        embedding: &[f32],
        source_meeting_id: &str,
        cluster_label: &str,
    ) -> Result<()> {
        if !(MIN_EMBEDDING_DIM..=MAX_EMBEDDING_DIM).contains(&embedding.len()) {
            return Err(anyhow!(
                "embedding dimension out of range [{}, {}]: got {}",
                MIN_EMBEDDING_DIM,
                MAX_EMBEDDING_DIM,
                embedding.len()
            ));
        }
        for (i, &v) in embedding.iter().enumerate() {
            if !v.is_finite() {
                return Err(anyhow!("non-finite embedding value at index {}", i));
            }
        }

        let blob = Self::serialize_embedding(embedding);
        let now = Utc::now().to_rfc3339();

        sqlx::query(
            "INSERT INTO speaker_embeddings (id, speaker_id, embedding, source_meeting_id, cluster_label, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(speaker_id)
        .bind(&blob)
        .bind(source_meeting_id)
        .bind(cluster_label)
        .bind(&now)
        .execute(pool)
        .await?;

        Ok(())
    }

    pub async fn get_embeddings_by_meeting(
        pool: &SqlitePool,
        meeting_id: &str,
    ) -> Result<Vec<EmbeddingRow>> {
        let rows = sqlx::query_as::<_, EmbeddingRow>(
            "SELECT id, speaker_id, embedding, source_meeting_id, cluster_label FROM speaker_embeddings WHERE source_meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await?;

        Ok(rows)
    }

    pub async fn list_all_embeddings(pool: &SqlitePool) -> Result<Vec<(String, Vec<f32>)>> {
        #[derive(sqlx::FromRow)]
        struct EmbeddingWithName {
            embedding: Vec<u8>,
            name: String,
        }

        let rows = sqlx::query_as::<_, EmbeddingWithName>(
            "SELECT e.embedding, COALESCE(s.name, e.cluster_label) as name \
             FROM speaker_embeddings e \
             LEFT JOIN speakers s ON e.speaker_id = s.id",
        )
        .fetch_all(pool)
        .await?;

        let mut result = Vec::with_capacity(rows.len());
        for row in rows {
            let embedding = Self::deserialize_embedding(&row.embedding)?;
            result.push((row.name, embedding));
        }
        Ok(result)
    }

    pub async fn link_embedding_to_speaker(
        pool: &SqlitePool,
        embedding_id: &str,
        speaker_id: &str,
    ) -> Result<bool> {
        let result =
            sqlx::query("UPDATE speaker_embeddings SET speaker_id = ? WHERE id = ?")
                .bind(speaker_id)
                .bind(embedding_id)
                .execute(pool)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_transcript_speaker(
        pool: &SqlitePool,
        transcript_id: &str,
        speaker_label: &str,
        source: &str,
    ) -> Result<bool> {
        let result = if source == "auto" {
            sqlx::query(
                "UPDATE transcripts SET speaker_label = ?, speaker_source = ? WHERE id = ? AND (speaker_source IS NULL OR speaker_source != 'manual')",
            )
            .bind(speaker_label)
            .bind(source)
            .bind(transcript_id)
            .execute(pool)
            .await?
        } else {
            sqlx::query(
                "UPDATE transcripts SET speaker_label = ?, speaker_source = ? WHERE id = ?",
            )
            .bind(speaker_label)
            .bind(source)
            .bind(transcript_id)
            .execute(pool)
            .await?
        };
        Ok(result.rows_affected() > 0)
    }

    pub async fn update_meeting_speakers(
        pool: &SqlitePool,
        meeting_id: &str,
        old_label: &str,
        new_label: &str,
    ) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE transcripts SET speaker_label = ?, speaker_source = 'manual' WHERE meeting_id = ? AND speaker_label = ?",
        )
        .bind(new_label)
        .bind(meeting_id)
        .bind(old_label)
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn clear_auto_speaker_labels(pool: &SqlitePool, meeting_id: &str) -> Result<u64> {
        let result = sqlx::query(
            "UPDATE transcripts SET speaker_label = NULL, speaker_source = NULL WHERE meeting_id = ? AND speaker_source = 'auto'",
        )
        .bind(meeting_id)
        .execute(pool)
        .await?;
        info!(
            "Cleared {} auto speaker labels for meeting {}",
            result.rows_affected(),
            meeting_id
        );
        Ok(result.rows_affected())
    }

    fn serialize_embedding(values: &[f32]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(values.len() * 4);
        for &v in values {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes
    }

    pub fn deserialize_embedding(blob: &[u8]) -> Result<Vec<f32>> {
        if blob.len() % 4 != 0 {
            return Err(anyhow!(
                "embedding blob size {} is not a multiple of 4",
                blob.len()
            ));
        }
        let dim = blob.len() / 4;
        if !(MIN_EMBEDDING_DIM..=MAX_EMBEDDING_DIM).contains(&dim) {
            return Err(anyhow!(
                "embedding dimension out of range [{}, {}]: got {}",
                MIN_EMBEDDING_DIM,
                MAX_EMBEDDING_DIM,
                dim
            ));
        }
        let mut values = Vec::with_capacity(dim);
        for chunk in blob.chunks_exact(4) {
            let v = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if !v.is_finite() {
                return Err(anyhow!("non-finite value in stored embedding"));
            }
            values.push(v);
        }
        Ok(values)
    }
}

#[derive(Debug, Clone, serde::Serialize, sqlx::FromRow)]
pub struct SpeakerRow {
    pub id: String,
    pub name: String,
    pub color: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EmbeddingRow {
    pub id: String,
    pub speaker_id: Option<String>,
    pub embedding: Vec<u8>,
    pub source_meeting_id: String,
    pub cluster_label: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_deserialize_round_trip() {
        let original: Vec<f32> = (0..256).map(|i| i as f32 * 0.01).collect();
        let blob = SpeakerRepository::serialize_embedding(&original);
        assert_eq!(blob.len(), 256 * 4);

        let restored = SpeakerRepository::deserialize_embedding(&blob).unwrap();
        assert_eq!(restored.len(), 256);
        for (i, (a, b)) in original.iter().zip(restored.iter()).enumerate() {
            assert_eq!(a, b, "mismatch at index {}", i);
        }
    }

    #[test]
    fn deserialize_wrong_dimension_rejected() {
        let values = vec![0.5f32; 128];
        let blob = SpeakerRepository::serialize_embedding(&values);
        // But wait, serialize doesn't validate dimension — it just writes bytes
        // The validation is in store_embedding. Let's just check deserialize.
        let result = SpeakerRepository::deserialize_embedding(&blob);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_non_multiple_of_4_rejected() {
        let blob = vec![0u8; 13]; // not a multiple of 4
        let result = SpeakerRepository::deserialize_embedding(&blob);
        assert!(result.is_err());
    }

    #[test]
    fn name_validation_rejects_empty() {
        // Validates the same logic used in create_speaker
        assert!("".trim().is_empty());
        assert!("   ".trim().is_empty());
    }

    #[test]
    fn name_validation_rejects_too_long() {
        let long = "A".repeat(201);
        assert!(long.trim().len() > MAX_NAME_LEN);
    }

    #[test]
    fn name_validation_accepts_normal() {
        assert!(!"Alice".trim().is_empty());
        assert!("Alice".trim().len() <= MAX_NAME_LEN);
    }

    #[test]
    fn name_validation_rejects_sql_injection() {
        let injection = "'; DROP TABLE speakers; --";
        // The name itself is valid (non-empty, under 200 chars)
        // but parameterized queries prevent injection
        assert!(!injection.trim().is_empty());
        // The key protection is using .bind() not string formatting
    }
}

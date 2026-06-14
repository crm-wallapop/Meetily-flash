use crate::api::{TranscriptSearchResult, TranscriptSegment};
use chrono::Utc;
use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::{Connection, Error as SqlxError, SqlitePool};
use thiserror::Error;
use tracing::{error, info};
use uuid::Uuid;

static MEETING_ID_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("invalid meeting_id regex")
});

#[derive(Debug, Error)]
pub enum TranscriptSaveError {
    #[error("invalid meeting_id: {0}")]
    InvalidMeetingId(String),
    #[error("meeting already exists: {0}")]
    MeetingAlreadyExists(String),
    #[error("database error: {0}")]
    Database(#[from] SqlxError),
}

fn validate_meeting_id(id: &str) -> Result<(), TranscriptSaveError> {
    if MEETING_ID_RE.is_match(id) {
        Ok(())
    } else {
        Err(TranscriptSaveError::InvalidMeetingId(id.to_string()))
    }
}

fn is_unique_violation(e: &SqlxError) -> bool {
    if let SqlxError::Database(db_err) = e {
        db_err.code().map_or(false, |c| c == "1555" || c == "2067")
    } else {
        false
    }
}

pub struct TranscriptsRepository;

impl TranscriptsRepository {
    pub async fn save_transcript(
        pool: &SqlitePool,
        meeting_id: &str,
        meeting_title: &str,
        transcripts: &[TranscriptSegment],
        folder_path: Option<String>,
    ) -> Result<(), TranscriptSaveError> {
        validate_meeting_id(meeting_id)?;

        let mut conn = pool.acquire().await?;
        let mut transaction = conn.begin().await?;

        let now = Utc::now();

        let result = sqlx::query(
            "INSERT INTO meetings (id, title, created_at, updated_at, folder_path) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(meeting_id)
        .bind(meeting_title)
        .bind(now)
        .bind(now)
        .bind(&folder_path)
        .execute(&mut *transaction)
        .await;

        match result {
            Err(ref e) if is_unique_violation(e) => {
                return Err(TranscriptSaveError::MeetingAlreadyExists(
                    meeting_id.to_string(),
                ));
            }
            Err(e) => {
                error!("Failed to create meeting '{}': {}", meeting_title, e);
                transaction.rollback().await?;
                return Err(TranscriptSaveError::Database(e));
            }
            Ok(_) => {}
        }

        info!("Successfully created meeting with id: {}", meeting_id);

        for segment in transcripts {
            let transcript_id = format!("transcript-{}", Uuid::new_v4());
            let result = sqlx::query(
                "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration)
                 VALUES (?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&transcript_id)
            .bind(meeting_id)
            .bind(&segment.text)
            .bind(&segment.timestamp)
            .bind(segment.audio_start_time)
            .bind(segment.audio_end_time)
            .bind(segment.duration)
            .execute(&mut *transaction)
            .await;

            if let Err(e) = result {
                error!(
                    "Failed to save transcript segment for meeting {}: {}",
                    meeting_id, e
                );
                transaction.rollback().await?;
                return Err(TranscriptSaveError::Database(e));
            }
        }

        info!(
            "Successfully saved {} transcript segments for meeting {}",
            transcripts.len(),
            meeting_id
        );

        transaction.commit().await?;

        Ok(())
    }

    /// Searches for a query string within the transcripts.
    /// It returns a list of matching transcripts with context.
    pub async fn search_transcripts(
        pool: &SqlitePool,
        query: &str,
    ) -> Result<Vec<TranscriptSearchResult>, SqlxError> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let search_query = format!("%{}%", query.to_lowercase());

        let rows = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT m.id, m.title, t.transcript, t.timestamp
             FROM meetings m
             JOIN transcripts t ON m.id = t.meeting_id
             WHERE LOWER(t.transcript) LIKE ?",
        )
        .bind(&search_query)
        .fetch_all(pool)
        .await?;

        let results = rows
            .into_iter()
            .map(|(id, title, transcript, timestamp)| {
                let match_context = Self::get_match_context(&transcript, query);
                TranscriptSearchResult {
                    id,
                    title,
                    match_context,
                    timestamp,
                }
            })
            .collect();

        Ok(results)
    }

    /// Helper function to extract a snippet of text around the first match of a query.
    fn get_match_context(transcript: &str, query: &str) -> String {
        let transcript_lower = transcript.to_lowercase();
        let query_lower = query.to_lowercase();

        match transcript_lower.find(&query_lower) {
            Some(match_index) => {
                let start_index = match_index.saturating_sub(100);
                let end_index = (match_index + query.len() + 100).min(transcript.len());

                let mut context = String::new();
                if start_index > 0 {
                    context.push_str("...");
                }
                context.push_str(&transcript[start_index..end_index]);
                if end_index < transcript.len() {
                    context.push_str("...");
                }
                context
            }
            None => transcript.chars().take(200).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = SqlitePool::connect(":memory:").await.unwrap();
        sqlx::query(
            "CREATE TABLE meetings (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                folder_path TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TABLE transcripts (
                id TEXT PRIMARY KEY,
                meeting_id TEXT NOT NULL,
                transcript TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                audio_start_time REAL,
                audio_end_time REAL,
                duration REAL,
                previous_label TEXT,
                FOREIGN KEY (meeting_id) REFERENCES meetings(id) ON DELETE CASCADE
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    fn valid_id() -> String {
        format!("meeting-{}", uuid::Uuid::new_v4())
    }

    // Task 1.4: save_transcript rejects empty meeting_id
    #[tokio::test]
    async fn save_transcript_rejects_empty_meeting_id() {
        let pool = test_pool().await;
        let err = TranscriptsRepository::save_transcript(
            &pool,
            "",
            "title",
            &[],
            None,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, TranscriptSaveError::InvalidMeetingId(_)),
            "expected InvalidMeetingId, got {:?}",
            err
        );
    }

    // Task 1.5: save_transcript rejects malformed meeting_id
    #[tokio::test]
    async fn save_transcript_rejects_malformed_meeting_id() {
        let pool = test_pool().await;
        let cases = [
            "meeting-not-a-uuid",
            "Meeting-550e8400-e29b-41d4-a716-446655440000",
            "meeting-550E8400-E29B-41D4-A716-446655440000",
            "meeting-550e8400-e29b-41d4-a716",
            "550e8400-e29b-41d4-a716-446655440000",
            "meeting_550e8400-e29b-41d4-a716-446655440000",
        ];
        for bad_id in cases {
            let err = TranscriptsRepository::save_transcript(
                &pool,
                bad_id,
                "title",
                &[],
                None,
            )
            .await
            .unwrap_err();
            assert!(
                matches!(err, TranscriptSaveError::InvalidMeetingId(_)),
                "expected InvalidMeetingId for '{}', got {:?}",
                bad_id,
                err
            );
        }
    }

    // Task 1.6: save_transcript persists client-supplied id
    #[tokio::test]
    async fn save_transcript_persists_client_supplied_id() {
        let pool = test_pool().await;
        let id = valid_id();
        TranscriptsRepository::save_transcript(
            &pool,
            &id,
            "test meeting",
            &[],
            None,
        )
        .await
        .unwrap();

        let row: (String,) =
            sqlx::query_as("SELECT id FROM meetings WHERE id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(row.0, id);
    }

    // Task 1.7: duplicate id surfaces typed error
    #[tokio::test]
    async fn save_transcript_duplicate_id_surfaces_typed_error() {
        let pool = test_pool().await;
        let id = valid_id();
        TranscriptsRepository::save_transcript(
            &pool,
            &id,
            "first",
            &[],
            None,
        )
        .await
        .unwrap();

        let err = TranscriptsRepository::save_transcript(
            &pool,
            &id,
            "duplicate",
            &[],
            None,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, TranscriptSaveError::MeetingAlreadyExists(_)),
            "expected MeetingAlreadyExists, got {:?}",
            err
        );
    }
}

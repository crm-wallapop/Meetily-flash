//! Task 2.7 — persistence_oracle_cde5c264: end-to-end Part B on top of
//! Part A. Runs the PRODUCTION diarization twice on the anchor recording —
//! once with the effective_split grid (Part A baseline), once with in-process
//! pyannote boundaries (this change) — aligns each output to the real
//! transcript rows covering the complaint window (Ricardo interjection,
//! ≈46:58), persists both runs through `SpeakerRepository::persist_aligned_groups`
//! into throwaway in-memory databases, and asserts the pyannote run persists
//! STRICTLY MORE rows for that window than the chunk-grid baseline.
//!
//! Run:
//!   cargo test --release --test persistence_oracle -- --ignored --nocapture

#![cfg(test)]

use app_lib::audio::speaker::alignment::{
    align_transcripts_with_diarization, DiarizationSegment, TranscriptInput,
};
use app_lib::audio::speaker::diarization::DiarizationPort;

const MEETING_ID: &str = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";
// The complaint window: Ricardo's interjection at ≈46:58 inside Cynthia's run.
const WINDOW: (f64, f64) = (2810.0, 2830.0);
const DB_PATH: &str = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";

async fn fetch_window_transcripts() -> Vec<TranscriptInput> {
    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{DB_PATH}?mode=ro"))
        .await
        .expect("connect local DB");
    let rows = sqlx::query(
        "SELECT id, transcript, audio_start_time, audio_end_time FROM transcripts \
         WHERE meeting_id = ? AND audio_start_time IS NOT NULL AND audio_end_time IS NOT NULL \
         AND audio_end_time >= ? AND audio_start_time <= ? ORDER BY audio_start_time ASC",
    )
    .bind(MEETING_ID)
    .bind(WINDOW.0)
    .bind(WINDOW.1)
    .fetch_all(&pool)
    .await
    .expect("fetch window transcripts");
    drop(pool);
    assert!(!rows.is_empty(), "no transcript rows in the complaint window");
    rows.into_iter()
        .map(|r| {
            let id: String = sqlx::Row::get(&r, "id");
            let text: String = sqlx::Row::get(&r, "transcript");
            let s: f64 = sqlx::Row::get(&r, "audio_start_time");
            let e: f64 = sqlx::Row::get(&r, "audio_end_time");
            eprintln!(
                "PERSIST-ORACLE: source row {} {:.2}s-{:.2}s ({} chars)",
                id,
                s,
                e,
                text.len()
            );
            TranscriptInput {
                id,
                text,
                audio_start_ms: (s * 1000.0) as i64,
                audio_end_ms: (e * 1000.0) as i64,
                token_words: None, // proportional alignment path (matches production default)
            }
        })
        .collect()
}

async fn make_temp_pool() -> sqlx::SqlitePool {
    let pool = sqlx::SqlitePool::connect(":memory:").await.unwrap();
    // Schema mirrors the production `transcripts` table (same DDL the
    // split-persistence unit tests use).
    sqlx::query(
        "CREATE TABLE transcripts (
            id TEXT PRIMARY KEY,
            meeting_id TEXT NOT NULL,
            transcript TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            summary TEXT, action_items TEXT, key_points TEXT,
            speaker TEXT,
            audio_start_time REAL, audio_end_time REAL, duration REAL,
            speaker_label TEXT, speaker_source TEXT,
            token_timestamps TEXT, previous_label TEXT
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    pool
}

async fn insert_sources(pool: &sqlx::SqlitePool, transcripts: &[TranscriptInput]) {
    for t in transcripts {
        sqlx::query(
            "INSERT INTO transcripts \
             (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker_source) \
             VALUES (?, ?, ?, '2026-08-24T00:00:00Z', ?, ?, ?, NULL)",
        )
        .bind(&t.id)
        .bind(MEETING_ID)
        .bind(&t.text)
        .bind(t.audio_start_ms as f64 / 1000.0)
        .bind(t.audio_end_ms as f64 / 1000.0)
        .bind((t.audio_end_ms - t.audio_start_ms) as f64 / 1000.0)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// Rows persisted inside the complaint window after a persistence pass.
async fn count_window_rows(pool: &sqlx::SqlitePool) -> usize {
    let rows = sqlx::query(
        "SELECT audio_start_time, audio_end_time FROM transcripts \
         WHERE meeting_id = ? AND speaker_source = 'auto' \
         AND audio_start_time IS NOT NULL AND audio_end_time IS NOT NULL \
         AND audio_end_time >= ? AND audio_start_time <= ?",
    )
    .bind(MEETING_ID)
    .bind(WINDOW.0)
    .bind(WINDOW.1)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.len()
}

async fn persist_one_path(
    label: &str,
    segments: &[DiarizationSegment],
    transcripts: &[TranscriptInput],
) -> usize {
    let pool = make_temp_pool().await;
    insert_sources(&pool, transcripts).await;
    let aligned = align_transcripts_with_diarization(transcripts.to_vec(), segments);
    eprintln!(
        "PERSIST-ORACLE [{label}]: {} diarization segments → {} aligned splits",
        segments.len(),
        aligned.len()
    );
    let written = app_lib::database::repositories::speaker::SpeakerRepository::persist_aligned_groups(&pool, aligned)
        .await
        .expect("persist_aligned_groups");
    let window_rows = count_window_rows(&pool).await;
    eprintln!(
        "PERSIST-ORACLE [{label}]: wrote {written} rows total, {window_rows} in the complaint window"
    );
    window_rows
}

#[tokio::test]
#[ignore] // requires the on-disk cde5c264 recording + models + local DB
async fn persistence_oracle_cde5c264() {
    use std::sync::atomic::AtomicU32;

    let home = dirs::home_dir().expect("home").join(".meetily-models");
    let adapter =
        app_lib::audio::speaker::sherpa_adapter::OrtDiarizationAdapter::with_shared_threshold(
            home.join(app_lib::audio::speaker::model_download::embedding_filename())
                .to_str()
                .unwrap(),
            home.join("pyannote-segmentation.onnx").to_str().unwrap(),
            std::sync::Arc::new(AtomicU32::new((0.40f32 * 65536.0) as u32)),
        )
        .expect("build adapter");

    let transcripts = fetch_window_transcripts().await;

    // Full-recording samples via the production decoder (16 kHz mono).
    let folder: String = {
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{DB_PATH}?mode=ro"))
            .await
            .expect("connect");
        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(MEETING_ID)
            .fetch_one(&pool)
            .await
            .expect("fetch meeting");
        drop(pool);
        sqlx::Row::get::<Option<String>, _>(&row, "folder_path").expect("folder_path")
    };
    let audio_dir = std::path::Path::new(&folder);
    let audio_path = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]
        .iter()
        .map(|n| audio_dir.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| panic!("no audio file in {}", folder));
    let decoded = app_lib::audio::decoder::decode_audio_file(&audio_path).expect("decode");
    let samples = decoded.to_whisper_format();

    // Path A: Part A baseline — effective_split grid over the transcript rows.
    let out_a = adapter.process(&samples, 16000, &transcripts_vec_windows(&transcripts)).expect("path A");
    // Path B: Part B — pyannote boundaries intersected with the same rows.
    let pya = app_lib::audio::speaker::pyannote_segmentation::PyannoteSegmentation::new(
        home.join("pyannote-segmentation.onnx").to_str().unwrap(),
    )
    .expect("pyannote session");
    let bounded = pya
        .boundary_segments(
            &samples,
            &transcripts_vec_windows(&transcripts),
            app_lib::audio::speaker::sherpa_adapter::max_diarization_chunks(),
        )
        .expect("boundary_segments");
    let out_b = adapter.process(&samples, 16000, &bounded).expect("path B");

    let to_diarseg = |out: &app_lib::audio::speaker::diarization::DiarizationOutput| -> Vec<DiarizationSegment> {
        out.segments
            .iter()
            .map(|s| DiarizationSegment {
                start_ms: (s.start_seconds * 1000.0) as i64,
                end_ms: (s.end_seconds * 1000.0) as i64,
                speaker_id: s.speaker_id,
            })
            .collect()
    };

    let rows_a = persist_one_path("grid-baseline", &to_diarseg(&out_a), &transcripts).await;
    let rows_b = persist_one_path("pyannote", &to_diarseg(&out_b), &transcripts).await;
    eprintln!(
        "PERSIST-ORACLE: complaint-window rows — grid {rows_a} vs pyannote {rows_b}"
    );
    assert!(
        rows_b > rows_a,
        "persistence oracle FAILED: pyannote path ({rows_b}) did not beat the chunk-grid baseline ({rows_a})"
    );
    eprintln!("PERSIST-ORACLE: PASS — strictly more speaker-split rows persisted for the complaint window");
}

/// The adapter takes `&[(f64, f64)]` segment windows.
fn transcripts_vec_windows(transcripts: &[TranscriptInput]) -> Vec<(f64, f64)> {
    transcripts
        .iter()
        .map(|t| (t.audio_start_ms as f64 / 1000.0, t.audio_end_ms as f64 / 1000.0))
        .collect()
}

//! Live Enhance → Speakers harness for meeting cde5c264 (terminal-driven —
//! no UI, per the "terminal or nothing" constraint).
//!
//! Deliberately Tauri-free (same linkage shape as `pipeline_perf`, which is
//! proven to launch under the project's Vulkan env): the tauri-mock variant
//! of this harness hit a loader failure (0xc0000139) with identical import
//! tables to a working exe — avoided by construction here.
//!
//! Production parity:
//! - decode → VAD → silence-split → Whisper (turbo-q5_0, token timestamps)
//!   exactly as `run_retranscription`'s transcription stage;
//! - persistence via the same `create_transcript_segments` +
//!   DELETE-and-insert-in-a-transaction + `write_transcripts_json` helpers
//!   the production save stage calls;
//! - diarization via the real `run_diarization_for_meeting` (the stage-95
//!   handoff), with the live speaker-merge threshold.
//!
//! Backs up the DB (+wal/shm) before connecting.
//!
//! Run (from src-tauri, under the run_vulkan_windows.bat env):
//!   cargo test --features vulkan --test enhance_speakers_live -- --ignored --nocapture

use sqlx::Row;
use std::path::Path;

const DB_PATH: &str = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
const MODELS_DIR: &str = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\models";
const MEETING_ID: &str = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";
const MODEL: &str = "large-v3-turbo-q5_0";
// 25s at 16kHz — same MAX_SEGMENT_SAMPLES constant as the production pipeline.
const MAX_SEGMENT_SAMPLES: usize = 25 * 16000;

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn enhance_then_speakers_cde5c264_live() {
    // --- Backup DB + WAL/SHM before anything opens it read/write ---
    for ext in ["", "-wal", "-shm"] {
        let src = format!("{}{}", DB_PATH, ext);
        let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");
        let dst = format!("{}.bak-enh-{}{}", DB_PATH, stamp, ext);
        if Path::new(&src).exists() {
            std::fs::copy(&src, &dst).expect("backup copy");
            eprintln!("HARNESS: backed up {}", dst);
        }
    }

    let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", DB_PATH))
        .await
        .expect("DB connect (rw)");

    // Pending migrations (notably the parakeet-config rewrite) are applied by
    // the app at startup; apply them here via the same migrate! the manager
    // uses so the transcript_settings row is production-shaped.
    sqlx::migrate!("./migrations").run(&pool).await.expect("migrations");

    let folder: String = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(MEETING_ID)
        .fetch_one(&pool)
        .await
        .expect("meeting row")
        .get("folder_path");
    let audio_path = Path::new(&folder).join("audio.mp4");
    assert!(audio_path.exists(), "audio missing at {}", audio_path.display());
    eprintln!("HARNESS: audio = {}", audio_path.display());

    let threshold: f32 = sqlx::query("SELECT speakerMergeThreshold FROM settings WHERE id = '1'")
        .fetch_one(&pool)
        .await
        .map(|r| r.get::<f64, _>("speakerMergeThreshold") as f32)
        .unwrap_or(0.40);
    eprintln!("HARNESS: speakerMergeThreshold = {}", threshold);

    // --- Decode → 16k mono → VAD → silence-split (production stage order) ---
    let decoded = app_lib::audio::decode_audio_file(&audio_path).expect("decode");
    let duration_seconds = decoded.duration_seconds;
    let audio_samples = decoded.to_whisper_format();
    const VAD_REDEMPTION_MS: u32 = 2000; // production default
    let speech = app_lib::audio::vad::get_speech_chunks(&audio_samples, VAD_REDEMPTION_MS)
        .expect("vad");
    eprintln!("HARNESS: {} speech segments from VAD", speech.len());

    let mut processable = Vec::new();
    for seg in &speech {
        if seg.samples.len() > MAX_SEGMENT_SAMPLES {
            processable.extend(app_lib::audio::common::split_segment_at_silence(seg, MAX_SEGMENT_SAMPLES));
        } else {
            processable.push(seg.clone());
        }
    }
    eprintln!("HARNESS: {} segments after silence-split", processable.len());

    // --- Whisper with token timestamps ---
    let engine = std::sync::Arc::new(
        app_lib::whisper_engine::WhisperEngine::new_with_models_dir(Some(std::path::PathBuf::from(
            MODELS_DIR,
        )))
        .expect("engine ctor"),
    );
    engine.discover_models().await.expect("discover models");
    engine.load_model(MODEL).await.expect("load turbo-q5_0");

    let mut all_transcripts: Vec<(String, f64, f64, Option<String>)> = Vec::new();
    let total = processable.len();

    // Production Vulkan path overlaps 2 segments (whisper_concurrency=2);
    // mirror it so wall time matches the app. Results land in indexed slots
    // to preserve segment order regardless of completion order.
    let concurrency = 2usize;
    let engine_for_tasks = engine.clone();
    use futures::stream::{self, StreamExt};
    let jobs = stream::iter(processable.iter().enumerate().map(|(i, seg)| {
        let engine = engine_for_tasks.clone();
        let samples = seg.samples.clone();
        async move {
            if samples.len() < 1600 {
                return (i, None);
            }
            match engine
                .transcribe_audio_with_confidence(samples, None, seg.start_timestamp_ms as i64)
                .await
            {
                Ok((text, _, _, token_ts)) if !text.trim().is_empty() => {
                    (i, Some((text, seg.start_timestamp_ms, seg.end_timestamp_ms, token_ts)))
                }
                Ok(_) => (i, None),
                Err(e) => (i, Some(("__WHISPER_ERR__".to_string(), seg.start_timestamp_ms, seg.end_timestamp_ms, Some(format!("{e}"))))),
            }
        }
    }))
    .buffer_unordered(concurrency);

    let mut slots: Vec<Option<(String, f64, f64, Option<String>)>> = vec![None; total];
    let mut done = 0usize;
    futures::pin_mut!(jobs);
    while let Some((i, r)) = jobs.next().await {
        slots[i] = r;
        done += 1;
        if done % 20 == 0 || done == total {
            eprintln!("HARNESS: transcribed {}/{}", done, total);
        }
    }
    for s in slots.into_iter().flatten() {
        all_transcripts.push(s);
    }
    let whisper_errors = all_transcripts.iter().filter(|(t, ..)| t == "__WHISPER_ERR__").count();
    if whisper_errors > 0 {
        panic!("{} whisper segments failed", whisper_errors);
    }

    // --- Production save stage: segments → transactional replace + JSON ---
    let segments = app_lib::audio::common::create_transcript_segments(&all_transcripts);
    let mut conn = pool.acquire().await.expect("conn");
    let mut tx = sqlx::Connection::begin(&mut *conn).await.expect("tx");
    sqlx::query("DELETE FROM transcripts WHERE meeting_id = ?")
        .bind(MEETING_ID)
        .execute(&mut *tx)
        .await
        .expect("delete old");
    for s in &segments {
        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, token_timestamps)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&s.id)
        .bind(MEETING_ID)
        .bind(&s.text)
        .bind(&s.timestamp)
        .bind(s.audio_start_time)
        .bind(s.audio_end_time)
        .bind(s.duration)
        .bind(&s.token_timestamps)
        .execute(&mut *tx)
        .await
        .expect("insert");
    }
    tx.commit().await.expect("commit");
    let _ = app_lib::audio::common::write_transcripts_json(Path::new(&folder), &segments);
    eprintln!(
        "HARNESS: persisted {} segments ({:.1}s audio)",
        segments.len(),
        duration_seconds
    );

    // Dump the coarse grid so stage-by-stage diarization debugging can run
    // against the exact input without re-transcribing.
    let grid: Vec<serde_json::Value> = segments
        .iter()
        .map(|s| {
            serde_json::json!({
                "start": s.audio_start_time,
                "end": s.audio_end_time,
                "tokens": s.token_timestamps.is_some(),
                "len": s.text.len(),
            })
        })
        .collect();
    let grid_path = std::env::temp_dir().join("cde5c264_coarse_grid.json");
    std::fs::write(
        &grid_path,
        serde_json::to_string_pretty(&grid).expect("grid json"),
    )
    .expect("write grid");
    eprintln!("HARNESS: coarse grid dumped to {}", grid_path.display());

    // Diarization is opt-in so the 2-hour transcription result stays reusable
    // for stage-by-stage debugging (run the 2g probe or set =1 to diarize here).
    if std::env::var("MEETIFY_HARNESS_DIARIZE").as_deref() != Ok("1") {
        eprintln!("HARNESS: MEETIFY_HARNESS_DIARIZE not set — skipping diarization");
        pool.close().await;
        return;
    }

    // --- Speakers (stage-95 handoff): the real diarization command ---
    let registry = std::sync::Arc::new(std::sync::Mutex::new(None));
    let threshold_fp = (threshold * 65536.0) as u32;
    let d = app_lib::audio::speaker::commands::run_diarization_for_meeting(
        &pool,
        MEETING_ID,
        threshold_fp,
        registry,
    )
    .await
    .expect("diarization");
    eprintln!(
        "HARNESS: diarization — {} speakers, {} segments labeled",
        d.speaker_count, d.segments_labeled
    );

    // --- Post-state report ---
    #[derive(sqlx::FromRow)]
    struct Row {
        start: f64,
        end: f64,
        speaker: Option<String>,
        text: String,
    }

    let labels: Vec<(Option<String>,)> = sqlx::query_as(
        "SELECT DISTINCT speaker_label FROM transcripts WHERE meeting_id = ? ORDER BY 1",
    )
    .bind(MEETING_ID)
    .fetch_all(&pool)
    .await
    .expect("labels");
    println!(
        "\n=== SPEAKERS ({}) ===\n{}",
        labels.len(),
        labels
            .iter()
            .map(|(l,)| l.clone().unwrap_or_else(|| "?".into()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    for (title, lo, hi) in [
        ("BANTER WINDOW [0..40s]", 0.0f64, 40.0),
        ("INTERJECTION WINDOW [2790..2840s]", 2790.0, 2840.0),
    ] {
        let rows = sqlx::query_as::<_, Row>(
            "SELECT audio_start_time as start, audio_end_time as end, speaker_label as speaker, transcript as text \
             FROM transcripts WHERE meeting_id = ? AND audio_start_time >= ? AND audio_start_time < ? \
             ORDER BY audio_start_time ASC",
        )
        .bind(MEETING_ID)
        .bind(lo)
        .bind(hi)
        .fetch_all(&pool)
        .await
        .expect("window rows");
        println!("\n=== {} ===", title);
        for r in &rows {
            let text: String = r.text.chars().take(72).collect();
            println!(
                "  [{:7.2} - {:7.2}] {:<10} {}",
                r.start,
                r.end,
                r.speaker.as_deref().unwrap_or("?"),
                text
            );
        }
    }

    pool.close().await;
}

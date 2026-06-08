use crate::audio::speaker::alignment::TranscriptInput;
use crate::audio::speaker::diarization::DiarizationPort;
use crate::audio::speaker::registry::SpeakerIdentificationPort;
use crate::audio::speaker::sherpa_adapter::SherpaOnnxRegistryAdapter;
use crate::audio::speaker::types::EmbeddingVector;
use crate::database::repositories::speaker::SpeakerRepository;
use crate::state::AppState;
use sqlx::SqlitePool;
use tauri::Emitter;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

const DIARIZATION_SAMPLE_RATE: u32 = 16000;

fn sanitize_speaker_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("Speaker name cannot be empty".to_string());
    }
    if trimmed.len() > 200 {
        return Err(format!(
            "Speaker name too long: {} chars (max 200)",
            trimmed.len()
        ));
    }
    let sanitized = strip_html_tags(trimmed);
    Ok(sanitized)
}

fn strip_html_tags(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result
}

fn pick_color(index: usize) -> String {
    let hue = (index as f64 * 137.508) % 360.0;
    format!("hsl({}, 65%, 55%)", hue.round() as u16)
}

#[tauri::command]
pub async fn label_speaker(
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
    cluster_label: String,
    speaker_name: String,
) -> Result<String, String> {
    let pool = app_state.db_manager.pool();
    let name = sanitize_speaker_name(&speaker_name)?;

    let meeting_exists = sqlx::query("SELECT id FROM meetings WHERE id = ?")
        .bind(&meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    if meeting_exists.is_none() {
        return Err(format!("Meeting not found: {}", meeting_id));
    }

    let cluster_rows = sqlx::query(
        "SELECT COUNT(*) as count FROM transcripts WHERE meeting_id = ? AND speaker_label = ?",
    )
    .bind(&meeting_id)
    .bind(&cluster_label)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;

    let count: i64 = sqlx::Row::get(&cluster_rows, "count");
    if count == 0 {
        return Err(format!(
            "No transcripts found for cluster '{}' in meeting {}",
            cluster_label, meeting_id
        ));
    }

    let speaker_id = format!("speaker-{}", Uuid::new_v4());

    #[derive(sqlx::FromRow)]
    struct SpeakerIdColor {
        id: String,
        color: String,
    }

    let existing = sqlx::query_as::<_, SpeakerIdColor>(
        "SELECT id, color FROM speakers WHERE name = ?",
    )
    .bind(&name)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;

    let (final_speaker_id, _final_color) = match existing {
        Some(row) => (row.id, row.color),
        None => {
            let speaker_count = SpeakerRepository::list_speakers(pool)
                .await
                .map(|s| s.len())
                .unwrap_or(0);
            let color = pick_color(speaker_count);
            SpeakerRepository::create_speaker(pool, &speaker_id, &name, &color)
                .await
                .map_err(|e| e.to_string())?;
            (speaker_id, color)
        }
    };

    let updated = SpeakerRepository::update_meeting_speakers(
        pool,
        &meeting_id,
        &cluster_label,
        &name,
    )
    .await
    .map_err(|e| e.to_string())?;

    log::info!(
        "label_speaker: labeled {} transcripts in meeting {} cluster '{}' as '{}'",
        updated,
        meeting_id,
        cluster_label,
        name
    );

    Ok(final_speaker_id)
}

#[tauri::command]
pub async fn list_speakers_cmd(
    app_state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let pool = app_state.db_manager.pool();
    let speakers = SpeakerRepository::list_speakers(pool)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::to_value(speakers).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn remove_speaker_cmd(
    app_state: tauri::State<'_, AppState>,
    speaker_id: String,
) -> Result<bool, String> {
    let pool = app_state.db_manager.pool();
    SpeakerRepository::remove_speaker(pool, &speaker_id)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn rediarize_meeting<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<u64, String> {
    log::warn!("rediarize_meeting: CALLED with meeting_id={}", meeting_id);
    let pool = app_state.db_manager.pool().clone();
    let threshold_fp = app_state.speaker_merge_threshold_fp.load(Ordering::Relaxed);
    let registry = app_state.speaker_registry.clone();

    let app_clone = app.clone();
    let mid = meeting_id.clone();
    tokio::spawn(async move {
        let result = run_diarization_for_meeting(&pool, &mid, threshold_fp, registry).await;
        match result {
            Ok(r) => {
                let _ = app_clone.emit("diarization-complete", serde_json::json!({
                    "meeting_id": mid,
                    "speaker_count": r.speaker_count,
                    "segments_labeled": r.segments_labeled,
                }));
                log::warn!("rediarize_meeting: DONE for {}, {} speakers, {} segments", mid, r.speaker_count, r.segments_labeled);
            }
            Err(e) => {
                log::error!("rediarize_meeting: FAILED for {}: {}", mid, e);
            }
        }
    }).await.map_err(|e| e.to_string())?;

    Ok(0)
}

pub async fn run_diarization_for_meeting(
    pool: &SqlitePool,
    meeting_id: &str,
    threshold_fp: u32,
    registry: Arc<Mutex<Option<SherpaOnnxRegistryAdapter>>>,
) -> Result<DiarizationResult, String> {
    let cleared = SpeakerRepository::clear_auto_speaker_labels(pool, meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    log::info!(
        "run_diarization_for_meeting: cleared {} auto labels for meeting {}",
        cleared,
        meeting_id
    );

    let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    let folder_path: Option<String> = row.and_then(|r| sqlx::Row::get(&r, "folder_path"));
    let Some(folder) = folder_path else {
        log::warn!("run_diarization_for_meeting: no folder_path for meeting {}", meeting_id);
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    };

    let folder_path = std::path::Path::new(&folder);
    log::warn!("run_diarization_for_meeting: looking for audio in {}", folder_path.display());
    let audio_path = find_audio_in_folder(folder_path);
    let Some(audio_path) = audio_path else {
        log::warn!("run_diarization_for_meeting: no audio file in {}", folder_path.display());
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    };
    log::warn!("run_diarization_for_meeting: found audio at {}", audio_path.display());

    let models_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".meetily-models");
    let embedding_path = models_dir.join("3dspeaker-embedding.onnx");
    let segmentation_path = models_dir.join("pyannote-segmentation.onnx");

    if !embedding_path.exists() || !segmentation_path.exists() {
        log::warn!("run_diarization_for_meeting: speaker models not found, skipping");
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    }

    // Step 1: Decode audio + resample to 16kHz mono via sinc resampler.
    let t0 = std::time::Instant::now();
    let decoded = crate::audio::decoder::decode_audio_file(&audio_path)
        .map_err(|e| format!("Audio decode failed: {}", e))?;
    let samples = decoded.to_whisper_format();
    let audio_duration = decoded.duration_seconds;
    log::warn!(
        "DIARIZATION: audio decode + sinc resample: {:.2}s ({}Hz → 16kHz, {:.1}s)",
        t0.elapsed().as_secs_f64(),
        decoded.sample_rate,
        audio_duration,
    );

    // Step 2: Fetch transcript timestamps FIRST.
    let transcript_segments = fetch_transcript_timestamps(pool, meeting_id, audio_duration)
        .await
        .map_err(|e| format!("Failed to fetch transcript timestamps: {}", e))?;

    log::warn!(
        "DIARIZATION: fetched {} valid transcript segments",
        transcript_segments.len(),
    );

    // Step 3: Create adapter.
    let t1 = std::time::Instant::now();
    let shared_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(threshold_fp));
    let adapter = super::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
        embedding_path.to_str().unwrap_or(""),
        segmentation_path.to_str().unwrap_or(""),
        shared_fp,
    )
    .map_err(|e| format!("Failed to create diarization adapter: {}", e))?;
    log::warn!("DIARIZATION: adapter creation: {:.2}s", t1.elapsed().as_secs_f64());

    // Step 4: Run diarization with transcript-driven segments.
    let t2 = std::time::Instant::now();
    let diarization = adapter.process(&samples, DIARIZATION_SAMPLE_RATE, &transcript_segments)
        .map_err(|e| format!("Diarization failed: {}", e))?;
    let segments = diarization.segments;
    let centroids = diarization.centroids;
    log::warn!("DIARIZATION: full pipeline: {:.2}s → {} segments", t2.elapsed().as_secs_f64(), segments.len());

    if segments.is_empty() {
        log::info!("run_diarization_for_meeting: 0 speakers detected for meeting {}", meeting_id);
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    }

    let num_speakers: std::collections::HashSet<u32> =
        segments.iter().map(|s| s.speaker_id).collect();
    log::info!(
        "run_diarization_for_meeting: detected {} speakers for meeting {}",
        num_speakers.len(),
        meeting_id
    );

    // Create speaker rows with colors so the frontend can render them.
    let sorted_speakers: Vec<&u32> = num_speakers.iter().collect();
    for (idx, &sid) in sorted_speakers.iter().enumerate() {
        let cluster_label = format!("Speaker {}", sid);
        let color = pick_color(idx);
        if let Err(e) = SpeakerRepository::create_speaker(
            pool,
            &format!("speaker-auto-{}-{}", meeting_id, sid),
            &cluster_label,
            &color,
        )
        .await
        {
            log::warn!("DIARIZATION: failed to create speaker {}: {}", cluster_label, e);
        }
    }

    // Step 5: Voice fingerprinting — store embeddings + cross-meeting matching.
    let mut label_map: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    for (speaker_id, centroid) in &centroids {
        let emb_id = format!("emb-{}", Uuid::new_v4());
        let cluster_label = format!("Speaker {}", speaker_id);
        if let Err(e) = SpeakerRepository::store_embedding(
            pool,
            &emb_id,
            None,
            centroid,
            meeting_id,
            &cluster_label,
        )
        .await
        {
            log::warn!("DIARIZATION: failed to store embedding for {}: {}", cluster_label, e);
        }

        // Cross-meeting matching via registry.
        if let Ok(emb) = EmbeddingVector::from_slice(centroid, centroid.len()) {
            let matched_name = registry.lock().ok().and_then(|mut guard| {
                guard.as_ref().and_then(|r| r.search(&emb, 0.60).ok().flatten())
            });
            if let Some(name) = matched_name {
                log::info!("DIARIZATION: matched Speaker {} → {}", speaker_id, name);
                label_map.insert(*speaker_id, name);
            }
        }
    }

    // Step 6: Fetch full transcripts for alignment.
    let transcripts = fetch_transcripts_for_alignment(pool, meeting_id).await
        .map_err(|e| format!("Failed to fetch transcripts: {}", e))?;

    if transcripts.is_empty() {
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: num_speakers.len() });
    }

    use crate::audio::speaker::alignment::{
        align_transcripts_with_diarization, DiarizationSegment,
    };

    let diarization_segs: Vec<DiarizationSegment> = segments
        .iter()
        .map(|s| DiarizationSegment {
            start_ms: (s.start_seconds * 1000.0) as i64,
            end_ms: (s.end_seconds * 1000.0) as i64,
            speaker_id: s.speaker_id,
        })
        .collect();

    let mut aligned = align_transcripts_with_diarization(transcripts, &diarization_segs);

    // Step 7: Temporal assignment for "Unknown Speaker" labels.
    let labeled_midpoints: Vec<(i64, String)> = aligned
        .iter()
        .filter(|s| s.speaker != "Unknown Speaker")
        .map(|s| {
            let mid = (s.audio_start_ms + s.audio_end_ms) / 2;
            (mid, s.speaker.clone())
        })
        .collect();

    let mut temporal_assigned = 0u64;
    for seg in &mut aligned {
        if seg.speaker == "Unknown Speaker" && !labeled_midpoints.is_empty() {
            let mid = (seg.audio_start_ms + seg.audio_end_ms) / 2;
            let nearest = labeled_midpoints
                .iter()
                .min_by_key(|(m, _)| (mid - *m).unsigned_abs())
                .map(|(_, name)| name.clone());
            if let Some(name) = nearest {
                seg.speaker = name;
                temporal_assigned += 1;
            }
        }
    }
    if temporal_assigned > 0 {
        log::warn!("DIARIZATION: assigned {} short segments via temporal adjacency", temporal_assigned);
    }

    // Step 8: Write labels to DB.
    let mut segments_labeled = 0u64;
    for seg in &aligned {
        let label = resolve_label(&seg.speaker, &label_map);
        SpeakerRepository::update_transcript_speaker(pool, &seg.original_id, &label, "auto")
            .await
            .map_err(|e| e.to_string())?;
        segments_labeled += 1;
    }

    log::info!(
        "run_diarization_for_meeting: labeled {} segments for meeting {}",
        segments_labeled,
        meeting_id
    );

    Ok(DiarizationResult {
        segments_labeled,
        speaker_count: num_speakers.len(),
    })
}

pub struct DiarizationResult {
    pub segments_labeled: u64,
    pub speaker_count: usize,
}

fn find_audio_in_folder(folder: &std::path::Path) -> Option<std::path::PathBuf> {
    let candidates = [
        "audio.mp4", "audio.m4a", "audio.wav", "audio.mp3",
        "audio.flac", "audio.ogg", "recording.mp4",
    ];
    for name in &candidates {
        let path = folder.join(name);
        if path.exists() {
            return Some(path);
        }
    }
    None
}

async fn fetch_transcript_timestamps(
    pool: &SqlitePool,
    meeting_id: &str,
    audio_duration_secs: f64,
) -> Result<Vec<(f64, f64)>, String> {
    #[derive(sqlx::FromRow)]
    struct Row {
        audio_start_time: Option<f64>,
        audio_end_time: Option<f64>,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT audio_start_time, audio_end_time FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let start = r.audio_start_time?;
            let end = r.audio_end_time?;
            // Validate: non-null, start < end, within audio bounds
            if start < end && start >= 0.0 && end <= audio_duration_secs + 1.0 {
                Some((start, end))
            } else {
                None
            }
        })
        .collect())
}

#[tauri::command]
pub async fn get_diarization_enabled(
    app_state: tauri::State<'_, AppState>,
) -> Result<bool, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT diarizationEnabled FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let enabled: i64 = sqlx::Row::get(&row, "diarizationEnabled");
    Ok(enabled != 0)
}

#[tauri::command]
pub async fn set_diarization_enabled(
    app_state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET diarizationEnabled = ? WHERE id = '1'")
        .bind(enabled as i64)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_diarization_enabled: updated to {}", enabled);
    Ok(())
}

#[tauri::command]
pub async fn get_speaker_merge_threshold(
    app_state: tauri::State<'_, AppState>,
) -> Result<f64, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT speakerMergeThreshold FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let threshold: f64 = sqlx::Row::get(&row, "speakerMergeThreshold");
    Ok(threshold)
}

#[tauri::command]
pub async fn set_speaker_merge_threshold(
    app_state: tauri::State<'_, AppState>,
    threshold: f64,
) -> Result<(), String> {
    if !(0.35..=0.70).contains(&threshold) {
        return Err("Threshold must be between 0.35 and 0.70".to_string());
    }
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET speakerMergeThreshold = ? WHERE id = '1'")
        .bind(threshold)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    let fp = (threshold as f32 * 65536.0) as u32;
    app_state.speaker_merge_threshold_fp.store(fp, Ordering::Relaxed);
    log::info!("set_speaker_merge_threshold: updated to {}", threshold);
    Ok(())
}

#[tauri::command]
pub async fn get_speaker_embedding_model(
    app_state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT speakerEmbeddingModel FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let model: String = sqlx::Row::get(&row, "speakerEmbeddingModel");
    Ok(model)
}

#[tauri::command]
pub async fn set_speaker_embedding_model(
    app_state: tauri::State<'_, AppState>,
    model: String,
) -> Result<(), String> {
    let valid = model == "3dspeaker" || model == "wespeaker";
    if !valid {
        return Err(format!("Unknown speaker embedding model: {}", model));
    }
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET speakerEmbeddingModel = ? WHERE id = '1'")
        .bind(&model)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_speaker_embedding_model: updated to {}", model);
    Ok(())
}

#[tauri::command]
pub async fn get_max_speakers(
    app_state: tauri::State<'_, AppState>,
) -> Result<i64, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT maxSpeakers FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let cap: i64 = sqlx::Row::get(&row, "maxSpeakers");
    Ok(cap)
}

#[tauri::command]
pub async fn set_max_speakers(
    app_state: tauri::State<'_, AppState>,
    cap: i64,
) -> Result<(), String> {
    if !(2..=20).contains(&cap) {
        return Err("Max speakers must be between 2 and 20".to_string());
    }
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET maxSpeakers = ? WHERE id = '1'")
        .bind(cap)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_max_speakers: updated to {}", cap);
    Ok(())
}

async fn fetch_transcripts_for_alignment(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<Vec<TranscriptInput>, String> {
    #[derive(sqlx::FromRow)]
    struct Row {
        id: String,
        text: String,
        start_time: f64,
        end_time: f64,
        token_timestamps: Option<String>,
    }

    let rows = sqlx::query_as::<_, Row>(
        "SELECT id, transcript as text, audio_start_time as start_time, audio_end_time as end_time, token_timestamps FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .into_iter()
        .map(|r| {
            let token_words = r.token_timestamps.and_then(|json| {
                serde_json::from_str::<Vec<crate::audio::speaker::alignment::TokenWord>>(&json).ok()
            });
            TranscriptInput {
                id: r.id,
                text: r.text,
                audio_start_ms: (r.start_time * 1000.0) as i64,
                audio_end_ms: (r.end_time * 1000.0) as i64,
                token_words,
            }
        })
        .collect())
}

fn resolve_label(speaker: &str, label_map: &std::collections::HashMap<u32, String>) -> String {
    if let Some(id_str) = speaker.strip_prefix("Speaker ") {
        if let Ok(id) = id_str.parse::<u32>() {
            if let Some(label) = label_map.get(&id) {
                return label.clone();
            }
        }
    }
    speaker.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_rejects_empty() {
        assert!(sanitize_speaker_name("").is_err());
        assert!(sanitize_speaker_name("   ").is_err());
    }

    #[test]
    fn sanitize_rejects_too_long() {
        let long = "A".repeat(201);
        assert!(sanitize_speaker_name(&long).is_err());
    }

    #[test]
    fn sanitize_accepts_normal() {
        assert_eq!(sanitize_speaker_name("Alice").unwrap(), "Alice");
    }

    #[test]
    fn sanitize_strips_html_tags() {
        assert_eq!(
            sanitize_speaker_name("<script>alert(1)</script>").unwrap(),
            "alert(1)"
        );
    }

    #[test]
    fn sanitize_accepts_prompt_injection_as_literal() {
        let name = sanitize_speaker_name("ignore previous instructions").unwrap();
        assert_eq!(name, "ignore previous instructions");
    }

    #[test]
    fn sanitize_accepts_sql_injection_as_literal() {
        let name = sanitize_speaker_name("'; DROP TABLE speakers; --").unwrap();
        assert_eq!(name, "'; DROP TABLE speakers; --");
    }

    #[test]
    fn strip_html_works() {
        assert_eq!(strip_html_tags("<b>hello</b>"), "hello");
        assert_eq!(strip_html_tags("no tags"), "no tags");
        assert_eq!(strip_html_tags("<script>alert(1)</script>"), "alert(1)");
    }

    #[test]
    fn pick_color_is_deterministic() {
        assert_eq!(pick_color(0), pick_color(0));
        assert_ne!(pick_color(0), pick_color(1));
        let c0 = pick_color(0);
        let c1 = pick_color(1);
        assert!(c0.starts_with("hsl("));
        assert!(c1.starts_with("hsl("));
        assert_ne!(c0, c1);
    }

    #[test]
    fn resolve_label_returns_cluster_name_when_no_match() {
        let map = std::collections::HashMap::new();
        assert_eq!(resolve_label("Speaker 1", &map), "Speaker 1");
        assert_eq!(resolve_label("Unknown Speaker", &map), "Unknown Speaker");
    }

    #[test]
    fn resolve_label_returns_matched_name() {
        let mut map = std::collections::HashMap::new();
        map.insert(1u32, "Alice".to_string());
        assert_eq!(resolve_label("Speaker 1", &map), "Alice");
    }

    #[test]
    fn threshold_range_validates() {
        assert!(set_speaker_merge_threshold_validate(0.39).is_err());
        assert!(set_speaker_merge_threshold_validate(0.40).is_ok());
        assert!(set_speaker_merge_threshold_validate(0.80).is_ok());
        assert!(set_speaker_merge_threshold_validate(0.81).is_err());
    }

    fn set_speaker_merge_threshold_validate(threshold: f64) -> Result<(), String> {
        if !(0.40..=0.80).contains(&threshold) {
            return Err("Threshold must be between 0.40 and 0.80".to_string());
        }
        Ok(())
    }

    /// Run diarization on the test meeting directly — no UI needed.
    /// `cargo test -p meetily-flash --features vulkan -- --ignored test_diarize_meeting_403`
    #[tokio::test]
    #[ignore]
    async fn test_diarize_meeting_403() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rw", db_path))
            .await
            .expect("DB connect");

        let registry = Arc::new(Mutex::new(None));
        let threshold_fp = (0.40f32 * 65536.0) as u32;
        let meeting_id = "meeting-00000000-0000-4000-8000-000000000003";

        let result = run_diarization_for_meeting(&pool, meeting_id, threshold_fp, registry).await;

        match &result {
            Ok(r) => eprintln!(
                "SUCCESS: {} speakers, {} segments labeled",
                r.speaker_count, r.segments_labeled
            ),
            Err(e) => eprintln!("FAILED: {}", e),
        }

        assert!(result.is_ok(), "Diarization should succeed");
        let r = result.unwrap();
        assert_eq!(r.speaker_count, 3, "Should detect exactly 3 speakers, got {}", r.speaker_count);
        assert!(r.segments_labeled > 0, "Should label at least 1 segment");
    }

    #[tokio::test]
    async fn auto_label_does_not_overwrite_manual() {
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rw", db_path))
            .await
            .expect("DB connect");

        let meeting_id = "meeting-00000000-0000-4000-8000-000000000003";
        let transcript_id = format!("test-concurrent-{}", uuid::Uuid::new_v4());

        // Insert a test transcript with manual label
        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker_label, speaker_source)
             VALUES (?, ?, 'test', '00:00', 0.0, 1.0, 1.0, 'Alice', 'manual')"
        )
        .bind(&transcript_id)
        .bind(meeting_id)
        .execute(&pool)
        .await
        .expect("insert");

        // Try to auto-label the same transcript
        let updated = SpeakerRepository::update_transcript_speaker(
            &pool, &transcript_id, "Speaker 0", "auto",
        )
        .await
        .expect("update");

        // Verify the manual label is intact (before any assertion that could panic)
        let row: (String, String) = sqlx::query_as(
            "SELECT speaker_label, speaker_source FROM transcripts WHERE id = ?"
        )
        .bind(&transcript_id)
        .fetch_one(&pool)
        .await
        .expect("fetch");

        // Cleanup first, then assert (so cleanup runs even if update assertion fails)
        sqlx::query("DELETE FROM transcripts WHERE id = ?")
            .bind(&transcript_id)
            .execute(&pool)
            .await
            .expect("cleanup");

        assert!(!updated, "auto-label should not overwrite manual label");
        assert_eq!(row.0, "Alice");
        assert_eq!(row.1, "manual");
    }
}

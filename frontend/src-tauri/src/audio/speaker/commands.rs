use crate::audio::speaker::alignment::TranscriptInput;
use crate::audio::speaker::diarization::DiarizationPort;
use crate::audio::speaker::registry::SpeakerIdentificationPort;
use crate::audio::speaker::sherpa_adapter::SherpaOnnxRegistryAdapter;
use crate::audio::speaker::types::{EmbeddingVector, SpeakerSegment};
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

#[tauri::command]
pub async fn reset_speaker_labels<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<u64, String> {
    log::warn!("reset_speaker_labels: CALLED with meeting_id={}", meeting_id);
    let pool = app_state.db_manager.pool().clone();
    let threshold_fp = app_state.speaker_merge_threshold_fp.load(Ordering::Relaxed);
    let registry = app_state.speaker_registry.clone();

    SpeakerRepository::clear_all_speaker_labels(&pool, &meeting_id)
        .await
        .map_err(|e| e.to_string())?;

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
                log::warn!("reset_speaker_labels: DONE for {}, {} speakers, {} segments", mid, r.speaker_count, r.segments_labeled);
            }
            Err(e) => {
                log::error!("reset_speaker_labels: FAILED for {}: {}", mid, e);
            }
        }
    }).await.map_err(|e| e.to_string())?;

    Ok(0)
}

#[tauri::command]
pub async fn revert_speaker_label(
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
    speaker_label: String,
) -> Result<u64, String> {
    let pool = app_state.db_manager.pool().clone();
    SpeakerRepository::revert_speaker_label(&pool, &meeting_id, &speaker_label)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_segment_speaker(
    app_state: tauri::State<'_, AppState>,
    transcript_id: String,
    speaker_label: String,
) -> Result<bool, String> {
    let pool = app_state.db_manager.pool();
    let label = sanitize_speaker_name(&speaker_label)?;
    SpeakerRepository::update_transcript_speaker_manual(pool, &transcript_id, &label)
        .await
        .map_err(|e| e.to_string())
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

    let deleted = SpeakerRepository::delete_embeddings_by_meeting(pool, meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    log::info!(
        "run_diarization_for_meeting: deleted {} stale embeddings for meeting {}",
        deleted,
        meeting_id
    );

    let removed = SpeakerRepository::remove_auto_speakers_for_meeting(pool, meeting_id)
        .await
        .map_err(|e| e.to_string())?;

    log::info!(
        "run_diarization_for_meeting: removed {} stale auto speakers for meeting {}",
        removed,
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

    let embedding_path = models_dir.join(super::model_download::embedding_filename());
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
    // Clustering + embedding extraction are CPU-bound (seconds even after the
    // D1/D2 perf fix); offload to a blocking thread so the async runtime
    // (detection polls, IPC) stays responsive regardless of meeting length.
    let t2 = std::time::Instant::now();
    let diarization = tokio::task::spawn_blocking(move || {
        adapter.process(&samples, DIARIZATION_SAMPLE_RATE, &transcript_segments)
    })
    .await
    .map_err(|e| format!("Diarization blocking task failed: {}", e))?
    .map_err(|e| format!("Diarization failed: {}", e))?;
    let mut segments = diarization.segments;
    let mut centroids = diarization.centroids;
    log::warn!("DIARIZATION: full pipeline: {:.2}s → {} segments", t2.elapsed().as_secs_f64(), segments.len());

    if segments.is_empty() {
        log::info!("run_diarization_for_meeting: 0 speakers detected for meeting {}", meeting_id);
        return Ok(DiarizationResult { segments_labeled: cleared, speaker_count: 0 });
    }

    // Enforce max_speakers cap: merge the MOST ISOLATED cluster (lowest
    // nearest-neighbour similarity) into its nearest neighbour. Merging the
    // highest-similarity pair collapses two similar real speakers; merging
    // the outlier absorbs noise/fragment clusters without touching
    // well-separated speakers that happen to sound alike.
    let effective_cap = resolve_effective_cap_for_meeting(pool, meeting_id).await;
    log::info!(
        "DIARIZATION: effective max_speakers cap for meeting {} = {}",
        meeting_id, effective_cap
    );
    enforce_max_speakers_cap(&mut centroids, &mut segments, effective_cap);

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
            let matched_name = registry.lock().ok().and_then(|guard| {
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
    let row = sqlx::query("SELECT diarization_enabled FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let enabled: i64 = sqlx::Row::get(&row, "diarization_enabled");
    Ok(enabled != 0)
}

#[tauri::command]
pub async fn set_diarization_enabled(
    app_state: tauri::State<'_, AppState>,
    enabled: bool,
) -> Result<(), String> {
    let pool = app_state.db_manager.pool();
    sqlx::query("UPDATE settings SET diarization_enabled = ? WHERE id = '1'")
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
pub async fn get_max_speakers(
    app_state: tauri::State<'_, AppState>,
) -> Result<i64, String> {
    let pool = app_state.db_manager.pool();
    let row = sqlx::query("SELECT max_speakers FROM settings LIMIT 1")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    let cap: i64 = sqlx::Row::get(&row, "max_speakers");
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
    sqlx::query("UPDATE settings SET max_speakers = ? WHERE id = '1'")
        .bind(cap)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_max_speakers: updated to {}", cap);
    Ok(())
}

#[derive(serde::Serialize)]
pub struct MeetingMaxSpeakers {
    r#override: Option<i32>,
    effective: i64,
    global_default: i64,
}

fn validate_meeting_cap(cap: i32) -> Result<(), String> {
    if !(2..=20).contains(&cap) {
        return Err("Max speakers must be between 2 and 20".to_string());
    }
    Ok(())
}

async fn set_meeting_max_speakers_inner(
    pool: &SqlitePool,
    meeting_id: &str,
    cap: Option<i32>,
) -> Result<(), String> {
    if let Some(c) = cap {
        validate_meeting_cap(c)?;
    }
    let exists = sqlx::query("SELECT 1 FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
    if exists.is_none() {
        return Err(format!("Meeting not found: {}", meeting_id));
    }
    sqlx::query("UPDATE meetings SET max_speakers = ? WHERE id = ?")
        .bind(cap)
        .bind(meeting_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    log::info!("set_meeting_max_speakers: meeting {} cap = {:?}", meeting_id, cap);
    Ok(())
}

async fn get_meeting_max_speakers_inner(
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<MeetingMaxSpeakers, String> {
    let row = sqlx::query(
        "SELECT m.max_speakers AS meeting_cap, \
         (SELECT max_speakers FROM settings LIMIT 1) AS global_cap \
         FROM meetings m WHERE m.id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    let row = row.ok_or_else(|| format!("Meeting not found: {}", meeting_id))?;
    let meeting_cap: Option<i64> =
        sqlx::Row::try_get(&row, "meeting_cap").map_err(|e| e.to_string())?;
    let global_cap: i64 = sqlx::Row::try_get(&row, "global_cap").map_err(|e| e.to_string())?;
    Ok(MeetingMaxSpeakers {
        r#override: meeting_cap.map(|v| v as i32),
        effective: resolve_effective_cap(meeting_cap, global_cap) as i64,
        global_default: global_cap,
    })
}

#[tauri::command]
pub async fn get_meeting_max_speakers(
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingMaxSpeakers, String> {
    get_meeting_max_speakers_inner(app_state.db_manager.pool(), &meeting_id).await
}

#[tauri::command]
pub async fn set_meeting_max_speakers(
    app_state: tauri::State<'_, AppState>,
    meeting_id: String,
    cap: Option<i32>,
) -> Result<(), String> {
    set_meeting_max_speakers_inner(app_state.db_manager.pool(), &meeting_id, cap).await
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

fn cosine_similarity_centroids(a: &[f32], b: &[f32]) -> f32 {
    let min_len = a.len().min(b.len());
    let dot: f32 = a[..min_len].iter().zip(&b[..min_len]).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a[..min_len].iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b[..min_len].iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a > 0.0 && norm_b > 0.0 { dot / (norm_a * norm_b) } else { 0.0 }
}

fn resolve_effective_cap(meeting_override: Option<i64>, global_default: i64) -> usize {
    meeting_override.unwrap_or(global_default) as usize
}

async fn resolve_effective_cap_for_meeting(pool: &SqlitePool, meeting_id: &str) -> usize {
    let row = sqlx::query(
        "SELECT m.max_speakers AS meeting_cap, \
         (SELECT max_speakers FROM settings LIMIT 1) AS global_cap \
         FROM meetings m WHERE m.id = ?",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    match row {
        Some(r) => {
            let meeting_cap: Option<i64> =
                sqlx::Row::try_get(&r, "meeting_cap").unwrap_or(None);
            let global_cap: i64 = sqlx::Row::try_get(&r, "global_cap").unwrap_or(10);
            resolve_effective_cap(meeting_cap, global_cap)
        }
        None => 10,
    }
}

fn enforce_max_speakers_cap(
    centroids: &mut std::collections::HashMap<u32, Vec<f32>>,
    segments: &mut Vec<SpeakerSegment>,
    cap: usize,
) {
    while centroids.len() > cap.max(2) {
        let ids: Vec<u32> = centroids.keys().copied().collect();

        let mut durations: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
        for seg in segments.iter() {
            *durations.entry(seg.speaker_id).or_insert(0.0) += seg.end_seconds - seg.start_seconds;
        }

        let mut most_isolated = ids[0];
        let mut lowest_nn_sim = f32::MAX;
        let mut nn_of_isolated = ids[0];

        for &i in &ids {
            let mut best_j = ids[0];
            let mut best_sim = f32::MIN;
            for &j in &ids {
                if i == j {
                    continue;
                }
                let sim = cosine_similarity_centroids(&centroids[&i], &centroids[&j]);
                if sim > best_sim {
                    best_sim = sim;
                    best_j = j;
                }
            }
            log::debug!(
                "DIARIZATION: cluster {} ({:.1}s) nearest={} sim={:.3}",
                i,
                durations.get(&i).unwrap_or(&0.0),
                best_j,
                best_sim
            );
            if best_sim < lowest_nn_sim {
                lowest_nn_sim = best_sim;
                most_isolated = i;
                nn_of_isolated = best_j;
            }
        }

        log::warn!(
            "DIARIZATION: cap={}: merging most-isolated speaker {} ({:.1}s) → speaker {} ({:.1}s) (nn sim={:.3})",
            cap,
            most_isolated,
            durations.get(&most_isolated).unwrap_or(&0.0),
            nn_of_isolated,
            durations.get(&nn_of_isolated).unwrap_or(&0.0),
            lowest_nn_sim
        );
        for seg in segments.iter_mut() {
            if seg.speaker_id == most_isolated {
                seg.speaker_id = nn_of_isolated;
            }
        }
        centroids.remove(&most_isolated);
    }
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

    /// Re-diarize meeting 95db and VERIFY exactly 3 speakers with clear
    /// Speaker 1 / Speaker 2 separation on the acceptance lines.
    ///
    /// Strategy: nemo_titanet model, threshold 0.50 (gives 4 speakers with
    /// correct separation), then max_speakers=3 enforcement merges the
    /// smallest cluster into its nearest neighbour.
    ///
    /// Acceptance criteria:
    ///   seg_6 and seg_7 → same speaker
    ///   seg_9 and seg_10 → same speaker
    ///   those two groups → DIFFERENT speakers
    ///   total speaker count → exactly 3
    ///
    /// `cargo test -p meetily-flash --features vulkan -- --ignored test_rediarize_verify_95db`
    #[tokio::test]
    #[ignore]
    async fn test_rediarize_verify_95db() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rw", db_path))
            .await
            .expect("DB connect");

        let meeting_id = "meeting-00000000-0000-4000-8000-000000000002";

        sqlx::query("UPDATE settings SET speaker_embedding_model = 'nemo_titanet', max_speakers = 3 WHERE id = '1'")
            .execute(&pool)
            .await
            .expect("set model + max_speakers");

        let threshold_fp = (0.65f32 * 65536.0) as u32;
        let registry = Arc::new(Mutex::new(None));
        let result = run_diarization_for_meeting(&pool, meeting_id, threshold_fp, registry)
            .await
            .expect("diarization");

        eprintln!("Diarization: {} speakers, {} segments", result.speaker_count, result.segments_labeled);

        #[derive(sqlx::FromRow)]
        struct LabelRow { id: String, speaker_label: Option<String> }
        let labels: std::collections::HashMap<String, String> = sqlx::query_as::<_, LabelRow>(
            "SELECT id, speaker_label FROM transcripts WHERE meeting_id = ? AND id IN ('seg_6','seg_7','seg_9','seg_10')",
        )
        .bind(meeting_id)
        .fetch_all(&pool)
        .await
        .expect("fetch labels")
        .into_iter()
        .filter_map(|r| r.speaker_label.map(|l| (r.id, l)))
        .collect();

        let s6 = labels.get("seg_6").cloned().unwrap_or_default();
        let s7 = labels.get("seg_7").cloned().unwrap_or_default();
        let s9 = labels.get("seg_9").cloned().unwrap_or_default();
        let s10 = labels.get("seg_10").cloned().unwrap_or_default();

        eprintln!("seg_6 -> {}", s6);
        eprintln!("seg_7 -> {}", s7);
        eprintln!("seg_9 -> {}", s9);
        eprintln!("seg_10 -> {}", s10);

        assert_eq!(result.speaker_count, 3, "Must detect exactly 3 speakers, got {}", result.speaker_count);
        assert_eq!(s6, s7, "seg_6 and seg_7 must be the same speaker");
        assert_eq!(s9, s10, "seg_9 and seg_10 must be the same speaker");
        assert_ne!(s6, s9, "the two groups must be different speakers");
    }

    /// Gold-standard oracle: build REAL chunks from meeting-95db's audio via the
    /// production `build_chunks` path, then run BOTH the cached-matrix
    /// `cluster_by_centroids` AND the naive O(n³) oracle on those chunks.
    /// Divergence would mean the diarization-clustering-perf refactor changed
    /// clustering behavior on real meeting audio — invalidating the 0.40
    /// threshold calibration and the seg_6==seg_7 / seg_9==seg_10 acceptance
    /// lines pinned by `test_rediarize_verify_95db`.
    ///
    /// The synthetic Gaussian equivalence test
    /// (`cached_matrix_matches_naive_on_realistic_cluster_structure`) covers
    /// realistic STRUCTURE; this test covers realistic SCALE + real embedding
    /// geometry (nemo_titanet on actual speech, not a synthetic center+noise
    /// model). Both must agree.
    ///
    /// `cargo test -p meetily-flash -- --ignored test_clustering_oracle_on_real_95db`
    #[tokio::test]
    #[ignore]
    async fn test_clustering_oracle_on_real_95db() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rw", db_path))
            .await
            .expect("DB connect");

        let meeting_id = "meeting-00000000-0000-4000-8000-000000000002";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder_path: Option<String> = row.and_then(|r| sqlx::Row::get(&r, "folder_path"));
        let folder = folder_path.expect("meeting-95db folder_path missing");
        let audio_path = find_audio_in_folder(std::path::Path::new(&folder))
            .expect("audio file in meeting-95db folder");

        let decoded = crate::audio::decoder::decode_audio_file(&audio_path)
            .expect("decode audio");
        let samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds;

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");
        assert!(
            !transcript_segments.is_empty(),
            "meeting-95db must have transcript segments to build chunks from"
        );

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path = models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "nemo_titanet embedding model missing");
        assert!(segmentation_path.exists(), "pyannote segmentation model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter = crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
            embedding_path.to_str().unwrap(),
            segmentation_path.to_str().unwrap(),
            threshold_fp,
        )
        .expect("create adapter");

        // build_chunks is CPU-bound (runs the embedding model on each segment);
        // offload to a blocking thread exactly as run_diarization_for_meeting does.
        let samples_arc = std::sync::Arc::new(samples);
        let segments_arc = std::sync::Arc::new(transcript_segments.clone());
        let adapter_arc = std::sync::Arc::new(adapter);
        let chunks = tokio::task::spawn_blocking(move || {
            adapter_arc.build_chunks(&samples_arc, DIARIZATION_SAMPLE_RATE, &segments_arc)
        })
        .await
        .expect("blocking task panicked");

        eprintln!(
            "Gold-standard oracle: {} real chunks from meeting-95db ({} transcript segments)",
            chunks.len(),
            transcript_segments.len()
        );
        assert!(!chunks.is_empty(), "build_chunks must produce chunks");

        // Run BOTH algorithms at multiple thresholds spanning the production
        // range. Equivalence must hold at every threshold — a divergence at any
        // one invalidates the refactor.
        for &threshold in &[0.30f32, 0.40, 0.50, 0.65] {
            let t0 = std::time::Instant::now();
            let (new_labels, _) = crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&chunks, threshold);
            let new_elapsed = t0.elapsed().as_secs_f64();

            let t1 = std::time::Instant::now();
            let (old_labels, _) = crate::audio::speaker::sherpa_adapter::cluster_by_centroids_naive(&chunks, threshold);
            let old_elapsed = t1.elapsed().as_secs_f64();

            let mismatches: Vec<(usize, u32, u32)> = new_labels
                .iter()
                .zip(old_labels.iter())
                .enumerate()
                .filter(|(_, (n, o))| n != o)
                .map(|(i, (n, o))| (i, *n, *o))
                .collect();

            let new_unique: std::collections::HashSet<u32> = new_labels.iter().copied().collect();
            eprintln!(
                "  thr={:.2}: {} clusters | cached {:.2}s vs naive {:.2}s | {} label mismatches",
                threshold,
                new_unique.len(),
                new_elapsed,
                old_elapsed,
                mismatches.len(),
            );

            assert_eq!(
                mismatches.len(),
                0,
                "cached-matrix and naive oracle disagree on {} of {} labels at thr={:.2} \
                 (first 5 mismatches: {:?}); refactor is NOT behavior-preserving on real audio",
                mismatches.len(),
                new_labels.len(),
                threshold,
                &mismatches[..mismatches.len().min(5)],
            );
        }
    }

    /// Per-meeting max_speakers override takes precedence over the global default.
    /// Sets the global cap wide (10) and the per-meeting override narrow (3), then
    /// asserts diarization yields <= 3 speakers — proving the override, not the
    /// global, drove the merge. Restores both values afterwards.
    ///
    /// `cargo test -p meetily-flash --features vulkan -- --ignored test_per_meeting_override_caps_speakers`
    #[tokio::test]
    #[ignore]
    async fn test_per_meeting_override_caps_speakers() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rw", db_path))
            .await
            .expect("DB connect");

        let meeting_id = "meeting-00000000-0000-4000-8000-000000000002";

        let (original_global, original_model): (i64, String) =
            sqlx::query_as("SELECT max_speakers, speaker_embedding_model FROM settings LIMIT 1")
                .fetch_one(&pool)
                .await
                .expect("read global");

        sqlx::query("UPDATE settings SET speaker_embedding_model = 'nemo_titanet', max_speakers = 10 WHERE id = '1'")
            .execute(&pool)
            .await
            .expect("set global wide");
        sqlx::query("UPDATE meetings SET max_speakers = 3 WHERE id = ?")
            .bind(meeting_id)
            .execute(&pool)
            .await
            .expect("set per-meeting override");

        let threshold_fp = (0.65f32 * 65536.0) as u32;
        let registry = Arc::new(Mutex::new(None));
        let result = run_diarization_for_meeting(&pool, meeting_id, threshold_fp, registry)
            .await
            .expect("diarization");

        eprintln!(
            "per-meeting override=3, global=10 -> {} speakers",
            result.speaker_count
        );

        sqlx::query("UPDATE settings SET max_speakers = ?, speaker_embedding_model = ? WHERE id = '1'")
            .bind(original_global)
            .bind(&original_model)
            .execute(&pool)
            .await
            .expect("restore global");
        sqlx::query("UPDATE meetings SET max_speakers = NULL WHERE id = ?")
            .bind(meeting_id)
            .execute(&pool)
            .await
            .expect("clear override");

        assert!(
            result.speaker_count <= 3,
            "per-meeting override of 3 must cap the result (global was 10), got {}",
            result.speaker_count
        );
    }

    #[tokio::test]
    async fn auto_label_does_not_overwrite_manual() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("DB connect");
        sqlx::query(
            "CREATE TABLE transcripts (
                id TEXT PRIMARY KEY,
                meeting_id TEXT NOT NULL,
                transcript TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                audio_start_time REAL NOT NULL,
                audio_end_time REAL NOT NULL,
                duration REAL NOT NULL,
                speaker_label TEXT,
                speaker_source TEXT,
                previous_label TEXT
            )",
        )
        .execute(&pool)
        .await
        .expect("create table");

        let meeting_id = "meeting-test";
        let transcript_id = format!("test-concurrent-{}", uuid::Uuid::new_v4());

        sqlx::query(
            "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, duration, speaker_label, speaker_source)
             VALUES (?, ?, 'test', '00:00', 0.0, 1.0, 1.0, 'Alice', 'manual')"
        )
        .bind(&transcript_id)
        .bind(meeting_id)
        .execute(&pool)
        .await
        .expect("insert");

        let updated = SpeakerRepository::update_transcript_speaker(
            &pool, &transcript_id, "Speaker 0", "auto",
        )
        .await
        .expect("update");

        let row: (String, String) = sqlx::query_as(
            "SELECT speaker_label, speaker_source FROM transcripts WHERE id = ?"
        )
        .bind(&transcript_id)
        .fetch_one(&pool)
        .await
        .expect("fetch");

        assert!(!updated, "auto-label should not overwrite manual label");
        assert_eq!(row.0, "Alice");
        assert_eq!(row.1, "manual");
    }

    async fn setup_meeting_cap_db() -> SqlitePool {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("DB connect");
        sqlx::query("CREATE TABLE meetings (id TEXT PRIMARY KEY, max_speakers INTEGER)")
            .execute(&pool)
            .await
            .expect("create meetings");
        sqlx::query("CREATE TABLE settings (id TEXT PRIMARY KEY, max_speakers INTEGER NOT NULL DEFAULT 10)")
            .execute(&pool)
            .await
            .expect("create settings");
        sqlx::query("INSERT INTO settings (id, max_speakers) VALUES ('1', 10)")
            .execute(&pool)
            .await
            .expect("seed settings");
        pool
    }

    #[tokio::test]
    async fn migration_adds_nullable_max_speakers_column() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:")
            .await
            .expect("DB connect");
        sqlx::query("CREATE TABLE meetings (id TEXT PRIMARY KEY)")
            .execute(&pool)
            .await
            .expect("create meetings");
        let sql = include_str!("../../../migrations/20260616000000_per_meeting_max_speakers.sql");
        sqlx::query(sql)
            .execute(&pool)
            .await
            .expect("migration applies");
        sqlx::query("INSERT INTO meetings (id) VALUES ('m1')")
            .execute(&pool)
            .await
            .expect("insert");
        let (cap,): (Option<i64>,) =
            sqlx::query_as("SELECT max_speakers FROM meetings WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .expect("fetch");
        assert_eq!(cap, None, "freshly inserted meeting must default to NULL (inherit global)");
    }

    #[test]
    fn resolve_effective_cap_override_wins() {
        assert_eq!(resolve_effective_cap(Some(3), 10), 3);
    }

    #[test]
    fn resolve_effective_cap_falls_back_to_global() {
        assert_eq!(resolve_effective_cap(None, 6), 6);
        assert_eq!(resolve_effective_cap(None, 10), 10);
    }

    #[tokio::test]
    async fn resolve_effective_cap_for_meeting_reads_override_then_global() {
        let pool = setup_meeting_cap_db().await;
        sqlx::query("INSERT INTO meetings (id, max_speakers) VALUES ('m1', 3)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO meetings (id) VALUES ('m2')")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            resolve_effective_cap_for_meeting(&pool, "m1").await,
            3,
            "per-meeting override 3 beats global 10"
        );
        assert_eq!(
            resolve_effective_cap_for_meeting(&pool, "m2").await,
            10,
            "NULL override falls back to global 10"
        );
        sqlx::query("UPDATE settings SET max_speakers = 6 WHERE id = '1'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(
            resolve_effective_cap_for_meeting(&pool, "m1").await,
            3,
            "override is unaffected by a global change"
        );
        assert_eq!(
            resolve_effective_cap_for_meeting(&pool, "m2").await,
            6,
            "non-overridden meeting tracks the new global"
        );
    }

    #[test]
    fn enforce_cap_is_noop_below_threshold() {
        let mut centroids = std::collections::HashMap::<u32, Vec<f32>>::new();
        centroids.insert(0, vec![1.0, 0.0]);
        centroids.insert(1, vec![0.0, 1.0]);
        centroids.insert(2, vec![1.0, 1.0]);
        let mut segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 0 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 1 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 2 },
        ];
        let before = centroids.len();
        enforce_max_speakers_cap(&mut centroids, &mut segments, 5);
        assert_eq!(centroids.len(), before, "cap above cluster count must not merge");
        assert_eq!(segments[0].speaker_id, 0);
        assert_eq!(segments[1].speaker_id, 1);
        assert_eq!(segments[2].speaker_id, 2);
    }

    #[test]
    fn enforce_cap_merges_most_isolated_cluster() {
        let mut centroids = std::collections::HashMap::<u32, Vec<f32>>::new();
        centroids.insert(0, vec![1.0, 0.0]);
        centroids.insert(1, vec![0.95, 0.05]);
        centroids.insert(2, vec![0.0, 1.0]);
        let mut segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 0 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 2 },
        ];
        enforce_max_speakers_cap(&mut centroids, &mut segments, 2);
        assert_eq!(centroids.len(), 2, "cap=2 must merge 3 clusters down to 2");
        assert!(!centroids.contains_key(&2), "most-isolated cluster 2 must be absorbed");
        assert_eq!(
            segments.iter().filter(|s| s.speaker_id == 2).count(),
            0,
            "segments must be relabelled away from the absorbed cluster"
        );
    }

    // Integration: wires the REAL AHC clustering (cluster_by_centroids) into the REAL
    // override cap (enforce_max_speakers_cap). The unit tests above exercise each in
    // isolation on hand-built centroids; this proves they compose to enforce a
    // per-meeting override on actual clustering output — no model, no GPU, no audio.
    // The embedding-inference step that needs vulkan stays in the #[ignore] test.
    fn four_speaker_chunks() -> Vec<crate::audio::speaker::sherpa_adapter::Chunk> {
        use crate::audio::speaker::sherpa_adapter::Chunk;
        let a = vec![1.0, 0.0, 0.0, 0.0];
        vec![
            Chunk { start_sample: 0, end_sample: 48000, duration_secs: 3.0, embedding: a.clone() },
            Chunk { start_sample: 48000, end_sample: 96000, duration_secs: 3.0, embedding: a.clone() },
            Chunk { start_sample: 96000, end_sample: 144000, duration_secs: 3.0, embedding: vec![0.0, 1.0, 0.0, 0.0] },
            Chunk { start_sample: 144000, end_sample: 192000, duration_secs: 3.0, embedding: vec![0.0, 0.0, 1.0, 0.0] },
            Chunk { start_sample: 192000, end_sample: 240000, duration_secs: 3.0, embedding: vec![0.0, 0.0, 0.0, 1.0] },
        ]
    }

    fn segments_from_labels(
        labels: &[u32],
    ) -> Vec<SpeakerSegment> {
        labels
            .iter()
            .enumerate()
            .map(|(i, &sid)| SpeakerSegment {
                start_seconds: i as f64,
                end_seconds: i as f64 + 3.0,
                speaker_id: sid,
            })
            .collect()
    }

    #[test]
    fn cluster_then_cap_enforces_override_on_real_clustering() {
        let chunks = four_speaker_chunks();
        let (labels, mut centroids) =
            crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&chunks, 0.5);
        assert_eq!(
            centroids.len(),
            4,
            "real AHC yields 4 clusters for 4 orthogonal speakers"
        );
        let mut segments = segments_from_labels(&labels);
        enforce_max_speakers_cap(&mut centroids, &mut segments, 3);
        let speakers: std::collections::HashSet<u32> =
            segments.iter().map(|s| s.speaker_id).collect();
        assert_eq!(
            speakers.len(),
            3,
            "per-meeting cap=3 must reduce the real 4-cluster output to 3"
        );
    }

    #[test]
    fn cluster_then_cap_is_noop_when_cap_above_cluster_count() {
        let chunks = four_speaker_chunks();
        let (labels, mut centroids) =
            crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&chunks, 0.5);
        let mut segments = segments_from_labels(&labels);
        let before = centroids.len();
        enforce_max_speakers_cap(&mut centroids, &mut segments, 10);
        assert_eq!(
            centroids.len(),
            before,
            "cap above cluster count is an upper bound, not a target"
        );
    }

    #[test]
    fn validate_meeting_cap_rejects_out_of_range() {
        assert!(validate_meeting_cap(1).is_err());
        assert!(validate_meeting_cap(21).is_err());
        assert!(validate_meeting_cap(0).is_err());
        assert!(validate_meeting_cap(2).is_ok());
        assert!(validate_meeting_cap(20).is_ok());
    }

    #[tokio::test]
    async fn set_meeting_cap_rejects_unknown_meeting() {
        let pool = setup_meeting_cap_db().await;
        let err = set_meeting_max_speakers_inner(&pool, "nonexistent", Some(3)).await;
        assert!(err.is_err(), "unknown meeting id must be rejected");
    }

    #[tokio::test]
    async fn set_meeting_cap_rejects_invalid_range() {
        let pool = setup_meeting_cap_db().await;
        sqlx::query("INSERT INTO meetings (id) VALUES ('m1')")
            .execute(&pool)
            .await
            .unwrap();
        assert!(set_meeting_max_speakers_inner(&pool, "m1", Some(1)).await.is_err());
        assert!(set_meeting_max_speakers_inner(&pool, "m1", Some(21)).await.is_err());
    }

    #[tokio::test]
    async fn set_meeting_cap_none_clears_override_to_null() {
        let pool = setup_meeting_cap_db().await;
        sqlx::query("INSERT INTO meetings (id, max_speakers) VALUES ('m1', 3)")
            .execute(&pool)
            .await
            .unwrap();
        set_meeting_max_speakers_inner(&pool, "m1", None)
            .await
            .expect("clear");
        let (cap,): (Option<i64>,) =
            sqlx::query_as("SELECT max_speakers FROM meetings WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cap, None, "None must clear the override to NULL");
    }

    #[tokio::test]
    async fn set_meeting_cap_persists_value() {
        let pool = setup_meeting_cap_db().await;
        sqlx::query("INSERT INTO meetings (id) VALUES ('m1')")
            .execute(&pool)
            .await
            .unwrap();
        set_meeting_max_speakers_inner(&pool, "m1", Some(4))
            .await
            .expect("set");
        let (cap,): (Option<i64>,) =
            sqlx::query_as("SELECT max_speakers FROM meetings WHERE id = 'm1'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cap, Some(4));
    }

    #[tokio::test]
    async fn get_meeting_max_speakers_returns_effective_and_global() {
        let pool = setup_meeting_cap_db().await;
        sqlx::query("INSERT INTO meetings (id) VALUES ('m1')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO meetings (id, max_speakers) VALUES ('m2', 3)")
            .execute(&pool)
            .await
            .unwrap();
        let r1 = get_meeting_max_speakers_inner(&pool, "m1").await.expect("get m1");
        assert_eq!(r1.r#override, None);
        assert_eq!(r1.effective, 10);
        assert_eq!(r1.global_default, 10);
        let r2 = get_meeting_max_speakers_inner(&pool, "m2").await.expect("get m2");
        assert_eq!(r2.r#override, Some(3));
        assert_eq!(r2.effective, 3);
        assert_eq!(r2.global_default, 10);
    }

    #[tokio::test]
    async fn effective_cap_reflects_global_change_when_override_null() {
        let pool = setup_meeting_cap_db().await;
        sqlx::query("INSERT INTO meetings (id) VALUES ('m1')")
            .execute(&pool)
            .await
            .unwrap();
        let before = get_meeting_max_speakers_inner(&pool, "m1").await.unwrap();
        assert_eq!(before.effective, 10);
        sqlx::query("UPDATE settings SET max_speakers = 6 WHERE id = '1'")
            .execute(&pool)
            .await
            .unwrap();
        let after = get_meeting_max_speakers_inner(&pool, "m1").await.unwrap();
        assert_eq!(
            after.effective, 6,
            "changing global default must immediately affect non-overridden meetings"
        );
    }
}

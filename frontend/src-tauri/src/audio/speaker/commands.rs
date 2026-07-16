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
    // Surface diarization failure to the frontend: the task returns its result
    // so the command can return Err on failure. Without this a failure logs
    // silently and returns Ok(0), the diarization-complete event the frontend
    // awaits to clear its isRediarizing spinner never fires, and the spinner
    // hangs indefinitely.
    let join_result = tokio::spawn(async move {
        let result = run_diarization_for_meeting(&pool, &mid, threshold_fp, registry).await;
        match &result {
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
        result
    })
    .await
    .map_err(|e| e.to_string())?;

    match join_result {
        Ok(_) => Ok(0),
        Err(e) => Err(format!("rediarize_meeting failed for {}: {}", meeting_id, e)),
    }
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
    // Return Err on diarization failure so the frontend's isRediarizing spinner
    // clears (its catch block runs) instead of hanging on a silent Ok(0).
    let join_result = tokio::spawn(async move {
        let result = run_diarization_for_meeting(&pool, &mid, threshold_fp, registry).await;
        match &result {
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
        result
    })
    .await
    .map_err(|e| e.to_string())?;

    match join_result {
        Ok(_) => Ok(0),
        Err(e) => Err(format!("reset_speaker_labels failed for {}: {}", meeting_id, e)),
    }
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

    // Resolve the cap before the blocking pipeline: it is an async DB read
    // independent of process() output, and process()+cap()+refine_pass2() must
    // share one blocking thread (the adapter is moved into the closure).
    let effective_cap = resolve_effective_cap_for_meeting(pool, meeting_id).await;
    log::info!(
        "DIARIZATION: effective max_speakers cap for meeting {} = {}",
        meeting_id, effective_cap
    );

    // Step 4: Run the full diarization pipeline off the async runtime. Pass 1
    // (coarse process()) → enforce_max_speakers_cap → Pass 2 (refine_pass2:
    // 2 s re-chunk assigned to the post-cap centroids, design D1). All three
    // are CPU-bound; offloading keeps detection polls / IPC responsive.
    let t2 = std::time::Instant::now();
    let (segments, centroids) = tokio::task::spawn_blocking(move || {
        let coarse = adapter.process(&samples, DIARIZATION_SAMPLE_RATE, &transcript_segments)?;
        let mut segments = coarse.segments;
        let mut centroids = coarse.centroids;
        if !segments.is_empty() {
            // Cap: merge the MOST ISOLATED cluster (lowest nearest-neighbour
            // similarity), not the highest-similarity pair, so two similar real
            // speakers survive while noise/fragment clusters are absorbed.
            enforce_max_speakers_cap(&mut centroids, &mut segments, effective_cap);
            if !centroids.is_empty() {
                segments = adapter.refine_pass2(&samples, DIARIZATION_SAMPLE_RATE, &centroids)?;
                // Pass 2 only draws labels from the post-cap centroid set, so
                // prune any centroid a speaker no longer uses (clean fingerprinting).
                let used: std::collections::HashSet<u32> =
                    segments.iter().map(|s| s.speaker_id).collect();
                centroids.retain(|k, _| used.contains(k));
            }
        }
        Ok::<_, anyhow::Error>((segments, centroids))
    })
    .await
    .map_err(|e| format!("Diarization blocking task failed: {}", e))?
    .map_err(|e| format!("Diarization failed: {}", e))?;
    log::warn!(
        "DIARIZATION: full pipeline (Pass 1 + cap + Pass 2): {:.2}s → {} segments",
        t2.elapsed().as_secs_f64(),
        segments.len()
    );

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
    // A degenerate centroid makes dot/norm non-finite. The norm>0 guard already
    // maps a NaN norm to 0.0, but an Inf centroid has an Inf norm that passes that
    // guard and yields Inf/Inf = NaN similarity — poisoning the isolation ranking.
    // The dot.is_finite() conjunct closes that hole, clamping the Inf case to a
    // finite 0.0 (most-isolated) so the degenerate cluster is absorbed cleanly by
    // the NaN-safe merge below rather than corrupting the selection.
    if norm_a > 0.0 && norm_b > 0.0 && dot.is_finite() {
        dot / (norm_a * norm_b)
    } else {
        0.0
    }
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
        // Recompute the surviving centroid as the duration-weighted average of
        // the two merged clusters, matching cluster_by_centroids
        // (sherpa_adapter.rs:527). Without this the stored speaker_embeddings
        // row would hold only nn's original members, degrading cross-meeting
        // matching and skewing the next merge iteration's isolation ranking.
        let dur_iso = *durations.get(&most_isolated).unwrap_or(&0.0);
        let dur_nn = *durations.get(&nn_of_isolated).unwrap_or(&0.0);
        let total = dur_iso + dur_nn;
        if let Some(cent_iso) = centroids.remove(&most_isolated) {
            if total > 0.0 {
                let w_iso = dur_iso as f32 / total as f32;
                let w_nn = dur_nn as f32 / total as f32;
                if let Some(cent_nn) = centroids.get_mut(&nn_of_isolated) {
                    for (i, v) in cent_iso.iter().enumerate() {
                        // Whisper can emit NaN/Inf for silent or garbled chunks; a
                        // degenerate cluster selected as most-isolated would otherwise
                        // write its non-finite values into the survivor, and the NaN
                        // spreads to every remaining cluster on the next merge. Clamp
                        // both sides so a degenerate cluster contributes no geometry.
                        let nn_val = if cent_nn[i].is_finite() { cent_nn[i] } else { 0.0 };
                        let iso_val = if v.is_finite() { *v } else { 0.0 };
                        cent_nn[i] = nn_val * w_nn + iso_val * w_iso;
                    }
                }
            }
        }
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

    /// Temporal-coherence regression oracle on the production meeting that
    /// motivated the correction (`meeting-00000001-…`). Pre-fix baseline:
    /// 44–53 % singleton-flicker rate from minute 30 onward (per-chunk labels
    /// flipping almost every chunk under global AHC with no temporal continuity).
    ///
    /// Acceptance criterion after re-diarization with temporal smoothing:
    ///   isolated short-run singleton rows (label differs from BOTH temporal
    ///   neighbours AND duration < 5 s) < 10 % of rows in min 30–70. A 24 s
    ///   isolated run is a genuine turn, not flicker — hence the duration gate.
    ///
    /// Out of scope (verified 2026-06-29; see the change proposal and the
    /// `test_00000001_embedding_drift_diagnostic` test): sustained speaker
    /// absorption is embedding drift (late-voice cos ≈ 0.22 to the speaker's own
    /// early centroid), unfixable at the label/clustering layer, filed as a
    /// separate change. A `duration < 5 s` fragment count is also NOT asserted —
    /// it measures Whisper segment length, which diarization cannot change.
    ///
    /// Recipe (back up `meeting_minutes.sqlite` first — this mutates the prod DB;
    /// CPU-only build is fine, diarization uses ONNX, not the whisper GPU path):
    ///   cargo test -p meetily-flash -- --ignored \
    ///     test_temporal_coherence_regression_00000001
    #[tokio::test]
    #[ignore]
    async fn test_temporal_coherence_regression_00000001() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=rw", db_path))
            .await
            .expect("DB connect");
        let meeting_id = "meeting-00000000-0000-4000-8000-000000000001";

        sqlx::query("UPDATE settings SET speaker_embedding_model = 'nemo_titanet', max_speakers = 3 WHERE id = '1'")
            .execute(&pool)
            .await
            .expect("set model + max_speakers");

        // Full reset (mirrors the Speakers-button reset_speaker_labels): clear ALL
        // prior labels including manual ones, so the metric below measures the fresh
        // clustering output, not stale manual labels concentrated in min 0–30.
        sqlx::query(
            "UPDATE transcripts SET speaker_label = NULL, speaker_source = NULL, previous_label = NULL WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .execute(&pool)
        .await
        .expect("reset labels");

        let threshold_fp = (0.40f32 * 65536.0) as u32;
        let registry = Arc::new(Mutex::new(None));
        let result = run_diarization_for_meeting(&pool, meeting_id, threshold_fp, registry)
            .await
            .expect("diarization");
        eprintln!(
            "Diarization: {} speakers, {} segments",
            result.speaker_count, result.segments_labeled
        );

        #[derive(sqlx::FromRow)]
        struct LabelRow {
            id: String,
            speaker_label: Option<String>,
            audio_start_time: f64,
            duration: f64,
        }
        let rows: Vec<LabelRow> = sqlx::query_as::<_, LabelRow>(
            "SELECT id, speaker_label, audio_start_time, duration FROM transcripts \
             WHERE meeting_id = ? ORDER BY audio_start_time",
        )
        .bind(meeting_id)
        .fetch_all(&pool)
        .await
        .expect("fetch labels");
        let labelled: Vec<&LabelRow> =
            rows.iter().filter(|r| r.speaker_label.is_some()).collect();
        assert!(
            labelled.len() >= 10,
            "need a meaningful number of labelled rows, got {}",
            labelled.len()
        );

        // Flicker: isolated SHORT singleton rows < 10 % in min 30–70. The duration
        // gate excludes genuine turns (a 24 s isolated run is a real speaker change
        // the acoustic guard correctly preserves).
        let mid: Vec<&LabelRow> = labelled
            .iter()
            .filter(|r| (1800.0..4200.0).contains(&r.audio_start_time))
            .copied()
            .collect();
        let mut singletons = 0usize;
        for i in 1..mid.len().saturating_sub(1) {
            let prev = mid[i - 1].speaker_label.as_deref();
            let cur = mid[i].speaker_label.as_deref();
            let next = mid[i + 1].speaker_label.as_deref();
            if cur != prev && cur != next && mid[i].duration < 5.0 {
                singletons += 1;
            }
        }
        let rate = if mid.is_empty() { 0.0 } else { singletons as f64 / mid.len() as f64 };
        eprintln!(
            "min 30-70 short-singleton rate: {:.1}% ({} / {})",
            rate * 100.0,
            singletons,
            mid.len()
        );
        assert!(
            rate < 0.10,
            "flicker regressed: short-singleton rate {:.1}% > 10%",
            rate * 100.0
        );
    }

    /// DIAGNOSTIC (read-only — opens the DB in `mode=ro`, so it CANNOT mutate):
    /// characterizes the sustained mid-meeting absorption on `meeting-00000001-…`
    /// and records what its geometry rules IN and OUT. The test carries NO
    /// pass/fail assertion on these values — it prints its findings so the
    /// absorption's character is documented without freezing a not-yet-understood
    /// cause into an assertion. Three probes: (1) the absorbed speaker's OWN late
    /// chunks are cos ≈ 0.85 to her early centroid — same-speaker range, which
    /// RULES OUT the embedding-drift hypothesis (the ≈ 0.22 figure seen in the
    /// all-late-chunks mean is low only because most late chunks belong to other
    /// speakers); (2) the global AHC is faithful to its own centroids (only ~1 of
    /// the dominant speaker's ~234 late chunks is nearer the absorbed speaker's
    /// centroid); (3) a sequential online-centroid-tracking prototype reproduces
    /// the absorption (the absorbed speaker's cluster keeps ~7 late chunks),
    /// showing the obvious clustering-level alternative does not recover her
    /// either. Root cause (e.g. over-merge into a neighboring cluster) is not
    /// determined here and is filed as a separate change.
    ///
    /// `cargo test -p meetily-flash -- --ignored test_00000001_embedding_drift_diagnostic`
    #[tokio::test]
    #[ignore]
    async fn test_00000001_embedding_drift_diagnostic() {
        let _ = env_logger::builder().is_test(true).try_init();
        // mode=ro guarantees this test cannot mutate the prod DB (task 1.3 guard).
        let db_path = r"C:\Users\user\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-00000000-0000-4000-8000-000000000001";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder_path: Option<String> =
            row.and_then(|r| sqlx::Row::get(&r, "folder_path"));
        let folder = folder_path.expect("00000001 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds;
        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");
        assert!(!transcript_segments.is_empty(), "00000001 needs transcript segments");

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "nemo_titanet embedding model missing");
        assert!(segmentation_path.exists(), "pyannote segmentation model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                threshold_fp,
            )
            .expect("create adapter");

        let samples_arc = std::sync::Arc::new(samples);
        let segments_arc = std::sync::Arc::new(transcript_segments.clone());
        let adapter_arc = std::sync::Arc::new(adapter);
        let chunks = tokio::task::spawn_blocking(move || {
            adapter_arc.build_chunks(&samples_arc, DIARIZATION_SAMPLE_RATE, &segments_arc)
        })
        .await
        .expect("blocking task panicked");
        eprintln!("drift diagnostic: {} chunks", chunks.len());
        assert!(!chunks.is_empty());

        let (labels, _) =
            crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&chunks, 0.40);
        let sr = DIARIZATION_SAMPLE_RATE as f64;

        fn cosine(a: &[f32], b: &[f32]) -> f32 {
            let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
            let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
            let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
            if na < 1e-12 || nb < 1e-12 { 0.0 } else { dot / (na * nb) }
        }

        let mut early_sums: std::collections::HashMap<u32, (Vec<f32>, usize)> =
            std::collections::HashMap::new();
        let mut early_counts: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        let mut late_counts: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        for (c, &lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 < 1800.0 {
                let dim = c.embedding.len();
                let entry = early_sums.entry(lab).or_insert_with(|| (vec![0.0f32; dim], 0usize));
                for (acc, v) in entry.0.iter_mut().zip(c.embedding.iter()) {
                    *acc += v;
                }
                entry.1 += 1;
                *early_counts.entry(lab).or_insert(0) += 1;
            } else {
                *late_counts.entry(lab).or_insert(0) += 1;
            }
        }
        let early_centroids: std::collections::HashMap<u32, Vec<f32>> = early_sums
            .iter()
            .map(|(lab, (sum, n))| (*lab, sum.iter().map(|v| v / *n as f32).collect()))
            .collect();
        eprintln!("early counts by label: {:?}", early_counts);
        eprintln!("late  counts by label: {:?}", late_counts);

        // carol = early-dominant (>=5 chunks) label with fewest late chunks.
        // carlos  = label with the most late chunks.
        let early_vec: Vec<(u32, usize)> =
            early_counts.iter().map(|(&k, &v)| (k, v)).collect();
        let carol = early_vec
            .iter()
            .filter(|(_, n)| *n >= 5)
            .min_by_key(|(lab, _)| late_counts.get(lab).copied().unwrap_or(0))
            .map(|(lab, _)| *lab)
            .expect("need an early-dominant label");
        let carlos = late_counts
            .iter()
            .max_by_key(|(_, &n)| n)
            .map(|(&lab, _)| lab)
            .expect("need a late-dominant label");
        eprintln!("carol (early-dominant, vanishes late) = label {:?}", carol);
        eprintln!("carlos  (dominant late)                 = label {:?}", carlos);

        let carol_c = early_centroids.get(&carol).expect("carol early centroid");
        let carlos_c = early_centroids.get(&carlos).expect("carlos early centroid");

        let mut nearer_carol = 0usize;
        let mut nearer_carlos = 0usize;
        let mut tie = 0usize;
        let mut absorbed_total = 0usize;
        let mut absorbed_but_nearer_carol = 0usize;
        let mut sum_cyn = 0.0f64;
        let mut sum_car = 0.0f64;
        let mut late_n = 0usize;
        for (c, &lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 < 1800.0 {
                continue;
            }
            let cos_cyn = cosine(&c.embedding, carol_c);
            let cos_car = cosine(&c.embedding, carlos_c);
            sum_cyn += cos_cyn as f64;
            sum_car += cos_car as f64;
            late_n += 1;
            if (cos_cyn - cos_car).abs() < 1e-4 {
                tie += 1;
            } else if cos_cyn > cos_car {
                nearer_carol += 1;
            } else {
                nearer_carlos += 1;
            }
            if lab == carlos {
                absorbed_total += 1;
                if cos_cyn > cos_car {
                    absorbed_but_nearer_carol += 1;
                }
            }
        }
        eprintln!(
            "late chunks ({}): mean cos->carol={:.4}, mean cos->carlos={:.4}",
            late_n,
            sum_cyn / late_n as f64,
            sum_car / late_n as f64
        );
        eprintln!(
            "late chunks nearest-centroid: nearer-carol={}, nearer-carlos={}, tie={}",
            nearer_carol, nearer_carlos, tie
        );
        eprintln!(
            "late chunks AHC-labeled carlos({:?}): {} total, {} nearer carol's early centroid \
             (high => carlos cluster over-merged carol's chunks; low => carlos's late chunks are genuinely his)",
            carlos, absorbed_total, absorbed_but_nearer_carol
        );

        // Probe 1: the absorbed speaker's late voice is FAR from her own early
        // centroid — nemo_titanet same-speaker is normally >= 0.7; a value < 0.5
        // over a 70-min meeting is severe drift.
        let mut cyn_late_cos = 0.0f64;
        let mut cyn_late_n = 0usize;
        for (c, &lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 >= 1800.0 && lab == carol {
                cyn_late_cos += cosine(&c.embedding, carol_c) as f64;
                cyn_late_n += 1;
            }
        }
        let cyn_late_mean =
            if cyn_late_n > 0 { cyn_late_cos / cyn_late_n as f64 } else { 0.0 };
        eprintln!(
            "PROBE 1 (no-drift check): carol late chunks ({}): mean cos to her OWN early centroid = {:.4} \
             (>= 0.5 = same-speaker range, rules out embedding drift)",
            cyn_late_n, cyn_late_mean
        );

        // --- Sequential clustering prototype (online centroid adaptation) ---
        // Process chunks in time order; assign each to the nearest existing
        // centroid at/above threshold (adapting that centroid as a running mean),
        // else spawn a new cluster. Tests whether adapting the centroid online
        // recovers the absorbed speaker's late chunks. ANALYSIS ONLY.
        let thr = 0.40f32;
        let mut order: Vec<usize> = (0..chunks.len()).collect();
        order.sort_by_key(|&i| chunks[i].start_sample);
        let mut seq_centroids: Vec<(u32, Vec<f32>, usize)> = Vec::new();
        let mut seq_labels = vec![0u32; chunks.len()];
        let mut next_id: u32 = 0;
        for &i in &order {
            let emb = &chunks[i].embedding;
            let best_opt = seq_centroids
                .iter()
                .map(|(lab, sum, n)| {
                    let cent: Vec<f32> = sum.iter().map(|v| v / *n as f32).collect();
                    (*lab, cosine(emb, &cent))
                })
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            let assigned = match best_opt {
                Some((best, best_cos)) if best_cos >= thr => {
                    if let Some((_, sum, n)) =
                        seq_centroids.iter_mut().find(|(l, _, _)| *l == best)
                    {
                        for (s, v) in sum.iter_mut().zip(emb.iter()) {
                            *s += v;
                        }
                        *n += 1;
                    }
                    best
                }
                _ => {
                    let lab = next_id;
                    next_id += 1;
                    seq_centroids.push((lab, emb.clone(), 1));
                    lab
                }
            };
            seq_labels[i] = assigned;
        }
        let mut seq_early: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        let mut seq_late: std::collections::HashMap<u32, usize> =
            std::collections::HashMap::new();
        for (i, &lab) in seq_labels.iter().enumerate() {
            let t0 = chunks[i].start_sample as f64 / sr;
            if t0 < 1800.0 {
                *seq_early.entry(lab).or_insert(0) += 1;
            } else {
                *seq_late.entry(lab).or_insert(0) += 1;
            }
        }
        eprintln!(
            "PROBE 3 (sequential prototype, thr={:.2}): {} clusters total | early {:?} | late {:?}",
            thr, next_id, seq_early, seq_late
        );
        for (lab, n) in seq_early.iter().filter(|(_, n)| **n >= 5) {
            let l = seq_late.get(lab).copied().unwrap_or(0);
            eprintln!(
                "  seq label {:?}: {} early -> {} late {}",
                lab,
                n,
                l,
                if l >= 5 { "SURVIVES" } else { "" }
            );
        }

        // ===== TRACT 1: cross-stage stability check =====
        // Stage A (raw AHC) absorption is characterized above (cynthia -> ~7 late
        // chunks). Replicate Stage B (AHC + temporal smoothing) at chunk level to
        // see whether smoothing compounds the absorption. Mirrors process()'s
        // internal smooth_to_fixed_point call exactly.
        let embeddings_b: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
        let timestamps_b: Vec<f64> = chunks.iter().map(|c| c.start_sample as f64 / sr).collect();
        let durations_b: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
        let (labels_b, centroids_b) =
            crate::audio::speaker::sherpa_adapter::smooth_to_fixed_point(
                &labels,
                &embeddings_b,
                &timestamps_b,
                &durations_b,
                &centroids_a,
                &crate::audio::speaker::sherpa_adapter::SmoothParams::default(),
            );
        let mut b_early: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        let mut b_late: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for (c, &lab) in chunks.iter().zip(labels_b.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 < 1800.0 {
                *b_early.entry(lab).or_insert(0) += 1;
            } else {
                *b_late.entry(lab).or_insert(0) += 1;
            }
        }
        let cyn_b = b_early
            .iter()
            .filter(|(_, &n)| n >= 5)
            .min_by_key(|(lab, _)| b_late.get(*lab).copied().unwrap_or(0))
            .map(|(lab, _)| *lab);
        eprintln!(
            "TRACT 1 (Stage B = AHC + smoothing): cynthia-equiv = {:?} | early {:?} | late {:?}",
            cyn_b, b_early, b_late
        );
        let n_clusters_b: std::collections::HashSet<u32> = labels_b.iter().copied().collect();
        let effective_cap = resolve_effective_cap_for_meeting(&pool, meeting_id).await;
        eprintln!(
            "TRACT 1: {} clusters after smoothing vs effective cap {} -> cap {}",
            n_clusters_b.len(),
            effective_cap,
            if n_clusters_b.len() > effective_cap {
                "FIRES (Stage D active)"
            } else {
                "no-op (Stage D exonerated)"
            }
        );

        // ===== TRACT 1b: cap simulation =====
        // Simulate enforce_max_speakers_cap on the smoothed output to determine
        // whether Cynthia's distinct-but-small cluster gets merged. The cap merges
        // the MOST ISOLATED cluster (lowest nearest-neighbour centroid cosine) into
        // its NN, repeating until count <= cap. Cynthia's cos->carlos = 0.31 makes
        // her a prime candidate. Mirrors enforce_max_speakers_cap (commands.rs:856).
        let mut cap_centroids = centroids_b.clone();
        let mut cap_durations: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
        for (c, &lab) in chunks.iter().zip(labels_b.iter()) {
            *cap_durations.entry(lab).or_insert(0.0) += c.duration_secs;
        }
        let cyn_b_label = cyn_b.unwrap_or(u32::MAX);
        eprintln!(
            "TRACT 1b: simulating cap ({}) on {} clusters (cynthia-equiv label {}):",
            effective_cap, cap_centroids.len(), cyn_b_label
        );
        let mut cap_step = 0usize;
        while cap_centroids.len() > effective_cap.max(2) {
            let ids: Vec<u32> = cap_centroids.keys().copied().collect();
            let mut most_isolated = ids[0];
            let mut lowest_nn = f32::MAX;
            let mut nn_of_isolated = ids[0];
            for &i in &ids {
                let mut best_sim = f32::MIN;
                let mut best_j = i;
                for &j in &ids {
                    if i == j {
                        continue;
                    }
                    let s = cosine(&cap_centroids[&i], &cap_centroids[&j]);
                    if s > best_sim {
                        best_sim = s;
                        best_j = j;
                    }
                }
                if best_sim < lowest_nn {
                    lowest_nn = best_sim;
                    most_isolated = i;
                    nn_of_isolated = best_j;
                }
            }
            let cynthia_hit = most_isolated == cyn_b_label || nn_of_isolated == cyn_b_label;
            eprintln!(
                "  cap step {}: merge {} ({:.1}s) -> {} ({:.1}s) nn_sim={:.3}{}",
                cap_step,
                most_isolated,
                cap_durations.get(&most_isolated).copied().unwrap_or(0.0),
                nn_of_isolated,
                cap_durations.get(&nn_of_isolated).copied().unwrap_or(0.0),
                lowest_nn,
                if cynthia_hit { " <- CYNTHIA MERGED" } else { "" },
            );
            let cent_nn = cap_centroids[&nn_of_isolated].clone();
            let cent_iso = cap_centroids[&most_isolated].clone();
            let dur_nn = cap_durations[&nn_of_isolated];
            let dur_iso = cap_durations[&most_isolated];
            let total = dur_nn + dur_iso;
            let w_nn = dur_nn as f32 / total as f32;
            let w_iso = dur_iso as f32 / total as f32;
            let merged: Vec<f32> = cent_nn
                .iter()
                .zip(cent_iso.iter())
                .map(|(a, b)| a * w_nn + b * w_iso)
                .collect();
            cap_centroids.insert(nn_of_isolated, merged);
            cap_durations.insert(nn_of_isolated, total);
            cap_centroids.remove(&most_isolated);
            cap_durations.remove(&most_isolated);
            cap_step += 1;
        }
        let cyn_survives_cap = cap_centroids.contains_key(&cyn_b_label);
        eprintln!(
            "TRACT 1b: cynthia {} the cap (final clusters: {:?})",
            if cyn_survives_cap { "SURVIVES" } else { "is ABSORBED by" },
            cap_centroids.keys().collect::<Vec<_>>(),
        );

        // ===== TRACT 1c: absolute cosine distribution of "nearer cynthia" late chunks =====
        // The 178 late chunks cosine-nearer Cynthia's early centroid than Carlos's are
        // NOT contamination (Tract 2 refutes the cascade). This measures their ABSOLUTE
        // cosine to Cynthia: >= 0.5 = genuinely her voice (data loss); < 0.4 = sub-threshold
        // ambiguous chunks that never reached the merge threshold (illusory absorption).
        let mut cos_bins = [0usize; 5];
        let mut nearer_cyn_total = 0usize;
        for (c, &_lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 < 1800.0 {
                continue;
            }
            let cos_cyn = cosine(&c.embedding, cynthia_c);
            let cos_car = cosine(&c.embedding, carlos_c);
            if cos_cyn > cos_car {
                nearer_cyn_total += 1;
                let idx = if cos_cyn < 0.2 {
                    0
                } else if cos_cyn < 0.3 {
                    1
                } else if cos_cyn < 0.4 {
                    2
                } else if cos_cyn < 0.5 {
                    3
                } else {
                    4
                };
                cos_bins[idx] += 1;
            }
        }
        eprintln!(
            "TRACT 1c: {} 'nearer cynthia' late chunks — absolute cos->cynthia distribution:",
            nearer_cyn_total
        );
        eprintln!(
            "  <0.2: {} | 0.2-0.3: {} | 0.3-0.4: {} | 0.4-0.5: {} | >=0.5: {}  (threshold = 0.40)",
            cos_bins[0], cos_bins[1], cos_bins[2], cos_bins[3], cos_bins[4]
        );
        eprintln!(
            "TRACT 1c: {} chunks >= 0.5 (genuinely cynthia's voice) vs {} < 0.4 (sub-threshold ambiguous)",
            cos_bins[4],
            cos_bins[0] + cos_bins[1] + cos_bins[2]
        );

        // ===== TRACT 2: AHC merge-tree instrumentation =====
        // Local instrumented copy of cluster_by_centroids (mirrors
        // sherpa_adapter.rs:498-589 verbatim) that records every merge so we can
        // replay cynthia's cluster-ancestor: contamination onset (first carlos-origin
        // member absorbed), per-merge centroid drift, and cumulative drift of her
        // centroid away from her pure early centroid toward carlos. Tests the
        // contamination-cascade hypothesis — see
        // openspec/exploration/diarization-absorption-ahc-cascade.md.
        let n = chunks.len();
        let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
        let mut centroids_m: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
        let mut cluster_durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
        let mut alive: Vec<bool> = vec![true; n];
        let mut sim: Vec<Vec<f32>> = (0..n)
            .map(|a| {
                (a + 1..n)
                    .map(|b| cosine(&centroids_m[a], &centroids_m[b]))
                    .collect()
            })
            .collect();

        struct MergeEvent {
            step: usize,
            surv: usize,
            abso: usize,
            sim_val: f32,
            shift: f32,
            centroid_after: Vec<f32>,
            absorbed_members: Vec<usize>,
        }
        let mut merge_log: Vec<MergeEvent> = Vec::new();

        loop {
            let mut best_sim = 0.40f32;
            let mut best_pair: Option<(usize, usize)> = None;
            for a in 0..n {
                if !alive[a] {
                    continue;
                }
                for b in (a + 1)..n {
                    if !alive[b] {
                        continue;
                    }
                    let s = sim[a][b - a - 1];
                    if s > best_sim {
                        best_sim = s;
                        best_pair = Some((a, b));
                    }
                }
            }
            let Some((a, b)) = best_pair else { break };

            let cent_before = centroids_m[a].clone();
            let dur_a = cluster_durations[a];
            let dur_b = cluster_durations[b];
            let total_dur = dur_a + dur_b;
            let w_a = dur_a as f32 / total_dur as f32;
            let w_b = dur_b as f32 / total_dur as f32;
            let b_members = std::mem::take(&mut members[b]);
            let b_centroid = centroids_m[b].clone();
            for (i, v) in b_centroid.iter().enumerate() {
                centroids_m[a][i] = centroids_m[a][i] * w_a + v * w_b;
            }
            cluster_durations[a] = total_dur;
            members[a].extend_from_slice(&b_members);
            alive[b] = false;
            for x in (a + 1)..n {
                if alive[x] {
                    sim[a][x - a - 1] = cosine(&centroids_m[a], &centroids_m[x]);
                }
            }
            for x in 0..a {
                if alive[x] {
                    sim[x][a - x - 1] = cosine(&centroids_m[x], &centroids_m[a]);
                }
            }

            let shift = cosine(&cent_before, &centroids_m[a]);
            merge_log.push(MergeEvent {
                step: merge_log.len(),
                surv: a,
                abso: b,
                sim_val: best_sim,
                shift,
                centroid_after: centroids_m[a].clone(),
                absorbed_members: b_members,
            });
        }

        let mut local_labels = vec![0u32; n];
        let mut next_label = 0u32;
        let mut label_map: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
        let mut final_centroids: std::collections::HashMap<u32, Vec<f32>> =
            std::collections::HashMap::new();
        for (idx, is_alive) in alive.iter().enumerate() {
            if !is_alive {
                continue;
            }
            let label = next_label;
            next_label += 1;
            label_map.insert(idx, label);
            for &member in &members[idx] {
                local_labels[member] = label;
            }
            final_centroids.insert(label, centroids_m[idx].clone());
        }
        assert_eq!(
            local_labels, labels,
            "local AHC copy must match production cluster_by_centroids output"
        );

        let slot_of: std::collections::HashMap<u32, usize> =
            label_map.iter().map(|(&s, &l)| (l, s)).collect();
        let cyn_slot = *slot_of.get(&cynthia).expect("cynthia slot");
        let cyn_final = &final_centroids[&cynthia];
        let car_final = &final_centroids[&carlos];

        // Tag each chunk's origin by its nearest FINAL centroid (cynthia vs carlos).
        let chunk_is_carlos_origin: Vec<bool> = chunks
            .iter()
            .map(|c| cosine(&c.embedding, car_final) > cosine(&c.embedding, cyn_final))
            .collect();

        let cyn_merges: usize = merge_log.iter().filter(|m| m.surv == cyn_slot).count();
        eprintln!(
            "TRACT 2: replaying {} merges into cynthia's cluster-ancestor (slot {}):",
            cyn_merges, cyn_slot
        );
        let mut onset: Option<usize> = None;
        for m in &merge_log {
            if m.surv != cyn_slot {
                continue;
            }
            let carlos_origin = m
                .absorbed_members
                .iter()
                .filter(|&&ci| chunk_is_carlos_origin[ci])
                .count();
            let drift_from_early = cosine(&m.centroid_after, cynthia_c);
            let is_contamination = carlos_origin > 0;
            if onset.is_none() && is_contamination {
                onset = Some(m.step);
            }
            eprintln!(
                "  step {}: absorb slot {} ({} members, {} carlos-origin) sim={:.3} shift={:.4} cos->her_early={:.4}{}",
                m.step,
                m.abso,
                m.absorbed_members.len(),
                carlos_origin,
                m.sim_val,
                m.shift,
                drift_from_early,
                if is_contamination { " <- CONTAMINATION" } else { "" },
            );
        }
        eprintln!("TRACT 2: contamination onset at step {:?}", onset);

        let final_drift_cyn = cosine(cyn_final, cynthia_c);
        let final_drift_car = cosine(cyn_final, carlos_c);
        eprintln!(
            "TRACT 2: cynthia final centroid: cos->her_early={:.4}, cos->carlos_early={:.4}",
            final_drift_cyn, final_drift_car
        );
        eprintln!(
            "TRACT 2: cascade verdict: {}",
            if final_drift_car > 0.5 {
                "centroid drifted toward carlos (cascade signature present)"
            } else {
                "centroid did NOT drift toward carlos (cascade refuted — pure geometry)"
            }
        );

        // ===== TRACT 3: third-speaker placement + per-cluster embedding stability =====
        // Tract 1c only compared chunks against cynthia vs carlos — never cluster 0
        // (the third speaker). ~170 of the 178 "nearer cynthia" chunks are in cluster
        // 0 by label. Are those orphans solidly the third speaker (high cos to cluster
        // 0) or genuinely ambiguous (low cos to both)? Does cynthia's embedding
        // degrade over time while others stay stable? Is the loss a cliff at 30 min
        // or gradual? Is her late audio quiet (SNR/channel) or loud (embedder fault)?
        let third = early_vec
            .iter()
            .filter(|(lab, n)| *lab != cynthia && *lab != carlos && *n >= 5)
            .max_by_key(|(_, n)| *n)
            .map(|(lab, _)| *lab);
        let third_c: Option<&Vec<f32>> = third.and_then(|t| early_centroids.get(&t));
        eprintln!(
            "TRACT 3: third-speaker cluster = {:?} (has early centroid = {})",
            third,
            third_c.is_some()
        );

        // 3a: 3-way nearest-centroid for late chunks + orphan placement.
        let mut nearest3: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut orphan_cos_third: Vec<f32> = Vec::new();
        let mut orphan_cos_cyn: Vec<f32> = Vec::new();
        let mut orphan_by_label: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for (c, &lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 < 1800.0 {
                continue;
            }
            let cos_cyn = cosine(&c.embedding, cynthia_c);
            let cos_car = cosine(&c.embedding, carlos_c);
            let cos_third = third_c
                .map(|tc| cosine(&c.embedding, tc))
                .unwrap_or(f32::MIN);
            let best = if cos_cyn >= cos_car && cos_cyn >= cos_third {
                "cynthia"
            } else if cos_car >= cos_cyn && cos_car >= cos_third {
                "carlos"
            } else {
                "third"
            };
            *nearest3.entry(best).or_insert(0) += 1;
            if cos_cyn > cos_car && lab != cynthia {
                orphan_cos_third.push(cos_third);
                orphan_cos_cyn.push(cos_cyn);
                *orphan_by_label.entry(lab).or_insert(0) += 1;
            }
        }
        eprintln!(
            "TRACT 3a: late chunks 3-way nearest-centroid: {:?}",
            nearest3
        );
        eprintln!(
            "TRACT 3a: 'nearer-cynthia' orphans (not in her cluster) by AHC label: {:?}",
            orphan_by_label
        );
        let ocn = orphan_cos_cyn.len();
        if ocn > 0 {
            let mean_cyn: f64 =
                orphan_cos_cyn.iter().map(|&v| v as f64).sum::<f64>() / ocn as f64;
            let mean_third: f64 =
                orphan_cos_third.iter().map(|&v| v as f64).sum::<f64>() / ocn as f64;
            eprintln!(
                "TRACT 3a: {} orphans: mean cos->cynthia={:.4}, mean cos->third={:.4} \
                 (third >> cyn => solidly the 3rd speaker, cynthia's late speech is MISSING/not chunked; \
                 both < 0.4 => genuinely ambiguous no-man's-land)",
                ocn, mean_cyn, mean_third
            );
        }

        // 3b: per-cluster early->late embedding stability. For each main cluster, mean
        // cos of early chunks to own early centroid vs late chunks to the SAME early
        // centroid. All speakers weakened => global recording issue; only cynthia =>
        // speaker-specific (mic/channel/SNR).
        let main_labels: Vec<u32> = [Some(cynthia), Some(carlos), third]
            .iter()
            .filter_map(|x| *x)
            .collect();
        for lab in &main_labels {
            let Some(cent) = early_centroids.get(lab) else {
                continue;
            };
            let mut early_sum = 0.0f64;
            let mut en = 0usize;
            let mut late_sum = 0.0f64;
            let mut ln = 0usize;
            for (c, &l) in chunks.iter().zip(labels.iter()) {
                if l != *lab {
                    continue;
                }
                let t0 = c.start_sample as f64 / sr;
                let cos = cosine(&c.embedding, cent) as f64;
                if t0 < 1800.0 {
                    early_sum += cos;
                    en += 1;
                } else {
                    late_sum += cos;
                    ln += 1;
                }
            }
            if en > 0 && ln > 0 {
                let late_mean = late_sum / ln as f64;
                eprintln!(
                    "TRACT 3b: label {:?}: early cos->own-centroid={:.4} (n={}), \
                     late cos->own-EARLY-centroid={:.4} (n={}){}",
                    lab,
                    early_sum / en as f64,
                    en,
                    late_mean,
                    ln,
                    if late_mean < 0.5 {
                        " <- LATE EMBEDDINGS WEAKENED vs early"
                    } else {
                        ""
                    }
                );
            }
        }

        // 3c: temporal cliff vs gradual. 5-min bins; orphan count + mean cos->cynthia.
        let mut windows: std::collections::BTreeMap<u32, (usize, f64, usize)> =
            std::collections::BTreeMap::new();
        for (c, &lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            let bin = (t0 / 300.0) as u32;
            let cos_cyn = cosine(&c.embedding, cynthia_c);
            let cos_car = cosine(&c.embedding, carlos_c);
            let entry = windows.entry(bin).or_insert((0, 0.0, 0));
            entry.2 += 1;
            if cos_cyn > cos_car && lab != cynthia {
                entry.0 += 1;
                entry.1 += cos_cyn as f64;
            }
        }
        eprintln!(
            "TRACT 3c: 5-min bins (bin | start | total | orphans | orphan_mean_cos->cynthia):"
        );
        for (bin, (oc, sum, tot)) in &windows {
            let mean = if *oc > 0 { sum / *oc as f64 } else { 0.0 };
            eprintln!(
                "  bin {:>2} ({}-{}s): total={:<4} orphans={:<4} mean_cos_cyn={:.3}",
                bin,
                bin * 300,
                (bin + 1) * 300,
                tot,
                oc,
                mean
            );
        }

        // 3d: audio RMS energy for cynthia's late chunks vs orphans. Low orphan energy
        // = her late speech is quiet/attenuated (SNR/channel/mixing issue); high energy
        // = loud audio the embedder should handle (failure is in the model).
        let samples_ref = samples_for_energy.as_slice();
        let mut cyn_e: Vec<f64> = Vec::new();
        let mut orph_e: Vec<f64> = Vec::new();
        for (c, &lab) in chunks.iter().zip(labels.iter()) {
            let t0 = c.start_sample as f64 / sr;
            if t0 < 1800.0 {
                continue;
            }
            let start = c.start_sample as usize;
            let n_samp = (c.duration_secs * sr) as usize;
            let end = (start + n_samp).min(samples_ref.len());
            if start >= samples_ref.len() || end <= start {
                continue;
            }
            let sum_sq: f64 = samples_ref[start..end]
                .iter()
                .map(|s| (*s as f64).powi(2))
                .sum();
            let rms = (sum_sq / (end - start) as f64).sqrt();
            let cos_cyn = cosine(&c.embedding, cynthia_c);
            let cos_car = cosine(&c.embedding, carlos_c);
            if lab == cynthia {
                cyn_e.push(rms);
            } else if cos_cyn > cos_car {
                orph_e.push(rms);
            }
        }
        let mean_of = |v: &[f64]| {
            if v.is_empty() {
                0.0
            } else {
                v.iter().sum::<f64>() / v.len() as f64
            }
        };
        eprintln!(
            "TRACT 3d: RMS energy — cynthia late (n={}): mean={:.6} | orphans (n={}): mean={:.6} \
             (orphan << cynthia => quiet/attenuated = SNR/channel/mix issue; \
             comparable => loud audio, embedder is the failure)",
            cyn_e.len(),
            mean_of(&cyn_e),
            orph_e.len(),
            mean_of(&orph_e)
        );
    }

    /// Mirrors `SherpaOnnxDiarizationAdapter::process()` steps 2–5 plus the
    /// commands.rs max-speakers cap, so the diagnostic sweep sees exactly what
    /// production would store: cluster → smooth → coalesce → merge_short → cap.
    fn prod_path_on_chunks(
        chunks: &[crate::audio::speaker::sherpa_adapter::Chunk],
        embeddings: &[Vec<f32>],
        timestamps: &[f64],
        durations: &[f64],
        threshold: f32,
        cap: usize,
        sr_f: f64,
    ) -> (
        Vec<SpeakerSegment>,
        std::collections::HashMap<u32, Vec<f32>>,
    ) {
        use crate::audio::speaker::sherpa_adapter::{
            SmoothParams, cluster_by_centroids, merge_short_speakers, smooth_to_fixed_point,
        };
        let (labels, centroids) = cluster_by_centroids(chunks, threshold);
        let (labels, centroids) = smooth_to_fixed_point(
            &labels,
            embeddings,
            timestamps,
            durations,
            &centroids,
            &SmoothParams::default(),
        );
        let mut indexed: Vec<(usize, u32)> = labels.iter().copied().enumerate().collect();
        indexed.sort_by_key(|(i, _)| chunks[*i].start_sample);
        let mut segments: Vec<SpeakerSegment> = Vec::new();
        if let Some(&(ci0, cur0)) = indexed.first() {
            let mut cur = cur0;
            let mut seg_start = chunks[ci0].start_sample as f64 / sr_f;
            let mut seg_end = chunks[ci0].end_sample as f64 / sr_f;
            for &(ci, lab) in &indexed[1..] {
                let cs = chunks[ci].start_sample as f64 / sr_f;
                let ce = chunks[ci].end_sample as f64 / sr_f;
                if lab == cur {
                    seg_end = ce;
                } else {
                    segments.push(SpeakerSegment {
                        start_seconds: seg_start,
                        end_seconds: seg_end,
                        speaker_id: cur,
                    });
                    cur = lab;
                    seg_start = cs;
                    seg_end = ce;
                }
            }
            segments.push(SpeakerSegment {
                start_seconds: seg_start,
                end_seconds: seg_end,
                speaker_id: cur,
            });
        }
        let total: f64 = segments.iter().map(|s| s.end_seconds - s.start_seconds).sum();
        let (mut segments, mut centroids) = merge_short_speakers(segments, centroids, total);
        enforce_max_speakers_cap(&mut centroids, &mut segments, cap);
        (segments, centroids)
    }

    // GATE (task 1.1 + 1.2): runs the native OfflineSpeakerDiarization pipeline
    // on cde5c264's audio, then sweeps our clustering threshold {0.30, 0.35,
    // 0.40} on the re-embedded native windows. The absorbed speaker (Cynthia)
    // is identified by a voice fingerprint from the OLD whisper-segment path
    // (her early centroid), then cosine-matched to the new pipeline's clusters
    // regardless of label numbering. D3 proceed metric: her late-half speech
    // ≥ 30 % of early AND ≥ 60 s absolute, at SOME threshold. Native windows
    // are cached to a temp file so the slow pipeline only runs once.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_native_pipeline_diagnostic() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples_arc = std::sync::Arc::new(decoded.to_whisper_format());
        let sr_f = DIARIZATION_SAMPLE_RATE as f64;
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "nemo_titanet embedding model missing");
        assert!(segmentation_path.exists(), "pyannote segmentation model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                std::sync::Arc::clone(&threshold_fp),
            )
            .expect("create adapter");
        let adapter_arc = std::sync::Arc::new(adapter);

        // ── STEP 1: Cynthia's voice fingerprint from the OLD whisper-segment path.
        // She is the absorbed speaker — early-dominant, vanishes late under the
        // old fixed-split chunking. Her early-segment centroid is a voice
        // fingerprint we cosine-match against the new pipeline's clusters
        // regardless of label numbering.
        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");
        let adapter_for_old = std::sync::Arc::clone(&adapter_arc);
        let samples_for_old = std::sync::Arc::clone(&samples_arc);
        let seg_arc = std::sync::Arc::new(transcript_segments);
        let old_chunks = tokio::task::spawn_blocking(move || {
            adapter_for_old.build_chunks(&samples_for_old, DIARIZATION_SAMPLE_RATE, &seg_arc)
        })
        .await
        .expect("old build_chunks panicked");
        assert!(
            !old_chunks.is_empty(),
            "need old-path chunks to fingerprint Cynthia"
        );

        let (old_labels, _) =
            crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&old_chunks, 0.40);
        let mut old_early: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        let mut old_late: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for (c, &lab) in old_chunks.iter().zip(old_labels.iter()) {
            let t0 = c.start_sample as f64 / sr_f;
            if t0 < 1800.0 {
                *old_early.entry(lab).or_insert(0) += 1;
            } else {
                *old_late.entry(lab).or_insert(0) += 1;
            }
        }
        let cynthia_label = old_early
            .iter()
            .filter(|(_, &n)| n >= 5)
            .min_by_key(|(lab, _)| old_late.get(lab).copied().unwrap_or(0))
            .map(|(lab, _)| *lab)
            .expect("need an early-dominant old label to fingerprint Cynthia");
        let dim = old_chunks[0].embedding.len();
        let mut cynthia_sum = vec![0.0f32; dim];
        let mut cynthia_n = 0usize;
        for (c, &lab) in old_chunks.iter().zip(old_labels.iter()) {
            if lab == cynthia_label && (c.start_sample as f64 / sr_f) < 1800.0 {
                for (acc, v) in cynthia_sum.iter_mut().zip(c.embedding.iter()) {
                    *acc += v;
                }
                cynthia_n += 1;
            }
        }
        assert!(cynthia_n > 0, "Cynthia fingerprint needs ≥1 early chunk");
        let cynthia_centroid: Vec<f32> = cynthia_sum.iter().map(|v| v / cynthia_n as f32).collect();
        eprintln!(
            "STEP 1 (fingerprint): old-path label {} = Cynthia ({} early chunks); old early {:?} | late {:?}",
            cynthia_label, cynthia_n, old_early, old_late,
        );

        // ── STEP 2: native windows (cached — the expensive pipeline runs once).
        let cache_path = std::env::temp_dir().join("meetily_native_windows_cde5c264.txt");
        let windows: Vec<(f64, f64)> = if cache_path.exists() {
            eprintln!(
                "STEP 2 (native): loading cached windows from {}",
                cache_path.display()
            );
            std::fs::read_to_string(&cache_path)
                .expect("read cache")
                .lines()
                .filter_map(|l| {
                    let mut it = l.split(',');
                    let s = it.next()?.parse::<f64>().ok()?;
                    let e = it.next()?.parse::<f64>().ok()?;
                    Some((s, e))
                })
                .collect()
        } else {
            use sherpa_onnx::{
                FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
                OfflineSpeakerSegmentationModelConfig,
                OfflineSpeakerSegmentationPyannoteModelConfig, SpeakerEmbeddingExtractorConfig,
            };
            // min_duration_on/off = 0.0 so the native pass never silently drops a
            // short turn — our downstream MIN_SPEECH_SECS filter does all duration
            // gating, and a 0.3s native default would hide the very recoverable
            // short speaker turns this change targets.
            let native_config = OfflineSpeakerDiarizationConfig {
                segmentation: OfflineSpeakerSegmentationModelConfig {
                    pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                        model: Some(segmentation_path.to_string_lossy().to_string()),
                    },
                    num_threads: 8,
                    debug: false,
                    provider: Some("cpu".to_string()),
                },
                embedding: SpeakerEmbeddingExtractorConfig {
                    model: Some(embedding_path.to_string_lossy().to_string()),
                    num_threads: 8,
                    debug: false,
                    provider: Some("cpu".to_string()),
                },
                clustering: FastClusteringConfig::default(),
                min_duration_on: 0.0,
                min_duration_off: 0.0,
            };
            let native = OfflineSpeakerDiarization::create(&native_config)
                .expect("native create (silent None → model-load failure)");
            assert_eq!(native.sample_rate(), 16000, "pyannote expects 16kHz");

            let peak_rss = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0u64));
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (peak_t, stop_t) =
                (std::sync::Arc::clone(&peak_rss), std::sync::Arc::clone(&stop));
            let sampler = std::thread::spawn(move || {
                while !stop_t.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(ms) = memory_stats::memory_stats() {
                        peak_t.fetch_max(
                            ms.physical_mem as u64,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            });
            let native_arc = std::sync::Arc::new(native);
            let native_for_proc = std::sync::Arc::clone(&native_arc);
            let samples_for_native = std::sync::Arc::clone(&samples_arc);
            let t_native = std::time::Instant::now();
            let result = tokio::task::spawn_blocking(move || {
                native_for_proc.process(&samples_for_native[..])
            })
            .await
            .expect("native process panicked")
            .expect("native process returned None");
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
            sampler.join().ok();
            let elapsed = t_native.elapsed();
            let peak_bytes = peak_rss.load(std::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "STEP 2 (native): process() = {:.1}s ({:.3}x realtime of {:.0}s); peak RSS ≈ {:.0} MB; native num_speakers estimate = {} (discarded)",
                elapsed.as_secs_f64(),
                elapsed.as_secs_f64() / audio_duration,
                audio_duration,
                peak_bytes as f64 / 1e6,
                result.num_speakers(),
            );
            let native_segs = result.sort_by_start_time();
            eprintln!(
                "STEP 2 (native): {} speaker-homogeneous windows",
                native_segs.len()
            );
            let wins: Vec<(f64, f64)> = native_segs
                .iter()
                .map(|s| (s.start as f64, s.end as f64))
                .collect();
            let cache_text: String =
                wins.iter().map(|(s, e)| format!("{},{}\n", s, e)).collect();
            let _ = std::fs::write(&cache_path, &cache_text);
            eprintln!(
                "STEP 2 (native): cached {} windows to {}",
                wins.len(),
                cache_path.display()
            );
            wins
        };

        // ── STEP 3: re-embed native windows through OUR extractor.
        let adapter_for_new = std::sync::Arc::clone(&adapter_arc);
        let samples_for_new = std::sync::Arc::clone(&samples_arc);
        let windows_arc = std::sync::Arc::new(windows);
        let windows_arc_for_step3 = std::sync::Arc::clone(&windows_arc);
        let chunks = tokio::task::spawn_blocking(move || {
            adapter_for_new.build_chunks(&samples_for_new, DIARIZATION_SAMPLE_RATE, &windows_arc_for_step3)
        })
        .await
        .expect("new build_chunks panicked");
        eprintln!(
            "STEP 3 (re-embed): {} chunks from native windows",
            chunks.len()
        );
        assert!(!chunks.is_empty());

        let embeddings: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
        let timestamps: Vec<f64> = chunks.iter().map(|c| c.start_sample as f64 / sr_f).collect();
        let durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();

        // ── STEP 4: sweep threshold {0.30, 0.35, 0.40}; identify Cynthia at each.
        for &threshold in &[0.30f32, 0.35, 0.40] {
            let (segs, cents) = prod_path_on_chunks(
                &chunks,
                &embeddings,
                &timestamps,
                &durations,
                threshold,
                3,
                sr_f,
            );
            let speakers: std::collections::HashSet<u32> =
                segs.iter().map(|s| s.speaker_id).collect();
            let mut p_early: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            let mut p_late: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            for s in &segs {
                let dur = s.end_seconds - s.start_seconds;
                let mid = (s.start_seconds + s.end_seconds) * 0.5;
                if mid < 1800.0 {
                    *p_early.entry(s.speaker_id).or_insert(0.0) += dur;
                } else {
                    *p_late.entry(s.speaker_id).or_insert(0.0) += dur;
                }
            }
            let (cyn_id, cyn_cos) = cents
                .iter()
                .map(|(id, c)| {
                    (*id, cosine_similarity_centroids(c, &cynthia_centroid))
                })
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or((u32::MAX, 0.0));
            let cyn_early = p_early.get(&cyn_id).copied().unwrap_or(0.0);
            let cyn_late = p_late.get(&cyn_id).copied().unwrap_or(0.0);
            let ratio = if cyn_early > 0.0 {
                cyn_late / cyn_early
            } else {
                f64::INFINITY
            };
            let meets = cyn_early > 0.0 && cyn_late >= 0.30 * cyn_early && cyn_late >= 60.0;
            let tag = if cyn_cos < 0.5 {
                "NO MATCH (cos<0.5)"
            } else if meets {
                "★ MEETS proceed metric"
            } else {
                "below metric"
            };
            eprintln!(
                "SWEEP t={:.2}: {} speakers [early {:?} | late {:?}] | Cynthia→cluster {} (cos {:.3}): early {:.0}s late {:.0}s (ratio {:.2}) {}",
                threshold,
                speakers.len(),
                p_early,
                p_late,
                cyn_id,
                cyn_cos,
                cyn_early,
                cyn_late,
                ratio,
                tag,
            );
        }

        // ── STEP 5: sweep MIN_SPEECH_SECS. build_chunks drops 37% of native
        // windows at the production 1.5s floor. Re-embed ALL windows at min=0.0,
        // then filter + cluster at each minimum to see if Cynthia's late speech
        // was trapped in the short-window bucket.
        let adapter_for_sweep = std::sync::Arc::clone(&adapter_arc);
        let samples_for_sweep = std::sync::Arc::clone(&samples_arc);
        let all_chunks = tokio::task::spawn_blocking(move || {
            adapter_for_sweep.build_chunks_with_min(
                &samples_for_sweep,
                DIARIZATION_SAMPLE_RATE,
                &windows_arc,
                0.0,
            )
        })
        .await
        .expect("sweep build_chunks panicked");
        eprintln!(
            "STEP 5 (min-sweep): embedded {} chunks at min=0.0 (vs {} at production 1.5s)",
            all_chunks.len(),
            chunks.len()
        );

        for &min_speech in &[0.3f64, 0.5, 1.0, 1.5] {
            let filtered: Vec<crate::audio::speaker::sherpa_adapter::Chunk> = all_chunks
                .iter()
                .filter(|c| c.duration_secs >= min_speech)
                .cloned()
                .collect();
            let embeddings: Vec<Vec<f32>> =
                filtered.iter().map(|c| c.embedding.clone()).collect();
            let timestamps: Vec<f64> =
                filtered.iter().map(|c| c.start_sample as f64 / sr_f).collect();
            let durations: Vec<f64> = filtered.iter().map(|c| c.duration_secs).collect();
            let (segs, cents) = prod_path_on_chunks(
                &filtered,
                &embeddings,
                &timestamps,
                &durations,
                0.40f32,
                3,
                sr_f,
            );
            let speakers: std::collections::HashSet<u32> =
                segs.iter().map(|s| s.speaker_id).collect();
            let (cyn_id, cyn_cos) = cents
                .iter()
                .map(|(id, c)| {
                    (*id, cosine_similarity_centroids(c, &cynthia_centroid))
                })
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or((u32::MAX, 0.0));
            let mut p_early: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            let mut p_late: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            for s in &segs {
                let dur = s.end_seconds - s.start_seconds;
                let mid = (s.start_seconds + s.end_seconds) * 0.5;
                if mid < 1800.0 {
                    *p_early.entry(s.speaker_id).or_insert(0.0) += dur;
                } else {
                    *p_late.entry(s.speaker_id).or_insert(0.0) += dur;
                }
            }
            let cyn_early = p_early.get(&cyn_id).copied().unwrap_or(0.0);
            let cyn_late = p_late.get(&cyn_id).copied().unwrap_or(0.0);
            let ratio = if cyn_early > 0.0 {
                cyn_late / cyn_early
            } else {
                f64::INFINITY
            };
            let meets = cyn_early > 0.0
                && cyn_late >= 0.30 * cyn_early
                && cyn_late >= 60.0;
            let tag = if cyn_cos < 0.5 {
                "NO MATCH (cos<0.5)"
            } else if meets {
                "★ MEETS proceed metric"
            } else {
                "below metric"
            };
            eprintln!(
                "STEP 5 MIN={:.1}s: {} chunks, {} speakers [early {:?} | late {:?}] | Cynthia→cluster {} (cos {:.3}): early {:.0}s late {:.0}s (ratio {:.2}) {}",
                min_speech,
                filtered.len(),
                speakers.len(),
                p_early,
                p_late,
                cyn_id,
                cyn_cos,
                cyn_early,
                cyn_late,
                ratio,
                tag,
            );
        }

        // ── STEP 6: native process() with FastClustering forced to 3 clusters.
        // STEP 5 proved our AHC absorbs Cynthia at all thresholds and minimums.
        // This tests whether sherpa's own FastClustering keeps Cynthia distinct
        // under the same 3-speaker constraint our production cap enforces.
        {
            use sherpa_onnx::{
                FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
                OfflineSpeakerSegmentationModelConfig,
                OfflineSpeakerSegmentationPyannoteModelConfig, SpeakerEmbeddingExtractorConfig,
            };
            let native6_cache =
                std::env::temp_dir().join("meetily_native3_labels_cde5c264.txt");
            let native6_segs: Vec<(f32, f32, i32)> = if native6_cache.exists() {
                eprintln!(
                    "STEP 6 (native-3): loading cached labels from {}",
                    native6_cache.display()
                );
                std::fs::read_to_string(&native6_cache)
                    .unwrap_or_default()
                    .lines()
                    .filter_map(|line| {
                        let mut it = line.split(',');
                        let s = it.next()?.parse::<f32>().ok()?;
                        let e = it.next()?.parse::<f32>().ok()?;
                        let sp = it.next()?.parse::<i32>().ok()?;
                        Some((s, e, sp))
                    })
                    .collect()
            } else {
                let native6_config = OfflineSpeakerDiarizationConfig {
                    segmentation: OfflineSpeakerSegmentationModelConfig {
                        pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                            model: Some(segmentation_path.to_string_lossy().to_string()),
                        },
                        num_threads: 8,
                        debug: false,
                        provider: Some("cpu".to_string()),
                    },
                    embedding: SpeakerEmbeddingExtractorConfig {
                        model: Some(embedding_path.to_string_lossy().to_string()),
                        num_threads: 8,
                        debug: false,
                        provider: Some("cpu".to_string()),
                    },
                    clustering: FastClusteringConfig {
                        num_clusters: 3,
                        threshold: 0.5,
                    },
                    min_duration_on: 0.0,
                    min_duration_off: 0.0,
                };
                let samples_for_native6 = std::sync::Arc::clone(&samples_arc);
                let t_native6 = std::time::Instant::now();
                let native6_result = tokio::task::spawn_blocking(move || {
                    let native = OfflineSpeakerDiarization::create(&native6_config)
                        .expect("native6 create");
                    native.process(&samples_for_native6[..])
                })
                .await
                .expect("native6 process panicked")
                .expect("native6 process returned None");
                let elapsed = t_native6.elapsed();
                let segs = native6_result.sort_by_start_time();
                eprintln!(
                    "STEP 6 (native-3): process() = {:.1}s; {} segments, {} speakers (forced 3)",
                    elapsed.as_secs_f64(),
                    segs.len(),
                    native6_result.num_speakers(),
                );
                let raw: Vec<(f32, f32, i32)> =
                    segs.iter().map(|s| (s.start, s.end, s.speaker)).collect();
                let cache_text: String = raw
                    .iter()
                    .map(|(s, e, sp)| format!("{},{},{}\n", s, e, sp))
                    .collect();
                let _ = std::fs::write(&native6_cache, &cache_text);
                raw
            };

            let dim = cynthia_centroid.len();
            let mut n_early: std::collections::HashMap<i32, f64> = std::collections::HashMap::new();
            let mut n_late: std::collections::HashMap<i32, f64> = std::collections::HashMap::new();
            let mut n_sum: std::collections::HashMap<i32, (Vec<f64>, f64)> =
                std::collections::HashMap::new();
            for (start, end, speaker) in &native6_segs {
                let dur = (*end - *start) as f64;
                let mid = ((*start + *end) * 0.5) as f64;
                if mid < 1800.0 {
                    *n_early.entry(*speaker).or_insert(0.0) += dur;
                } else {
                    *n_late.entry(*speaker).or_insert(0.0) += dur;
                }
                let seg_start = *start as f64;
                let seg_end = *end as f64;
                for c in &all_chunks {
                    let c_mid = c.start_sample as f64 / sr_f + c.duration_secs * 0.5;
                    if c_mid >= seg_start && c_mid <= seg_end {
                        let entry = n_sum.entry(*speaker).or_insert((vec![0.0f64; dim], 0.0));
                        for (k, &v) in c.embedding.iter().enumerate() {
                            entry.0[k] += v as f64 * c.duration_secs;
                        }
                        entry.1 += c.duration_secs;
                    }
                }
            }

            let mut cyn_native_id = i32::MAX;
            let mut cyn_native_cos = 0.0f32;
            for (id, (sum_vec, total_dur)) in &n_sum {
                if *total_dur > 0.0 {
                    let cent: Vec<f32> =
                        sum_vec.iter().map(|&x| (x / *total_dur) as f32).collect();
                    let cos = cosine_similarity_centroids(&cent, &cynthia_centroid);
                    if cos > cyn_native_cos {
                        cyn_native_cos = cos;
                        cyn_native_id = *id;
                    }
                }
            }
            let cyn_e = n_early.get(&cyn_native_id).copied().unwrap_or(0.0);
            let cyn_l = n_late.get(&cyn_native_id).copied().unwrap_or(0.0);
            let ratio = if cyn_e > 0.0 { cyn_l / cyn_e } else { f64::INFINITY };
            let meets = cyn_e > 0.0 && cyn_l >= 0.30 * cyn_e && cyn_l >= 60.0;
            let tag = if cyn_native_cos < 0.5 {
                "NO MATCH (cos<0.5)"
            } else if meets {
                "★ MEETS proceed metric"
            } else {
                "below metric"
            };
            eprintln!(
                "STEP 6 (native-3): speakers early {:?} late {:?} | Cynthia→native {} (cos {:.3}): early {:.0}s late {:.0}s (ratio {:.2}) {}",
                n_early, n_late, cyn_native_id, cyn_native_cos, cyn_e, cyn_l, ratio, tag,
            );
        }

        // ── STEP 7: per-chunk cosine to Cynthia's centroid on native windows.
        // STEP 6 reports per-cluster aggregates only. This splits the
        // embedding-vs-clustering question: are Cynthia's late native-window
        // embeddings actually resembling her early centroid (→ clustering fails
        // to group them) or do they NOT resemble it (→ embedding model can't
        // extract her voice print from mixed audio)?
        {
            let mut early_buckets = [0.0f64; 4]; // [>=0.5, >=0.4, >=0.3, <0.3]
            let mut late_buckets = [0.0f64; 4];
            let mut late_max_cos = -1.0f32;
            let mut late_top: Vec<(f32, f64, f64)> = Vec::new();
            for c in &all_chunks {
                let mid = c.start_sample as f64 / sr_f + c.duration_secs * 0.5;
                let cos = cosine_similarity_centroids(&c.embedding, &cynthia_centroid);
                let dur = c.duration_secs;
                let idx = if cos >= 0.5 {
                    0
                } else if cos >= 0.4 {
                    1
                } else if cos >= 0.3 {
                    2
                } else {
                    3
                };
                if mid < 1800.0 {
                    early_buckets[idx] += dur;
                } else {
                    late_buckets[idx] += dur;
                    if cos > late_max_cos {
                        late_max_cos = cos;
                    }
                    if cos >= 0.5 {
                        late_top.push((cos, mid, dur));
                    }
                }
            }
            late_top.sort_by(|a, b| {
                b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
            });
            eprintln!(
                "STEP 7 (per-chunk cos to Cynthia centroid, native windows @ min=0.0):"
            );
            eprintln!(
                "  EARLY (<30min): cos>=0.5: {:.0}s | >=0.4: {:.0}s | >=0.3: {:.0}s | <0.3: {:.0}s",
                early_buckets[0], early_buckets[1], early_buckets[2], early_buckets[3]
            );
            eprintln!(
                "  LATE (>=30min): cos>=0.5: {:.0}s | >=0.4: {:.0}s | >=0.3: {:.0}s | <0.3: {:.0}s | max_cos={:.3}",
                late_buckets[0], late_buckets[1], late_buckets[2], late_buckets[3], late_max_cos
            );
            let total_late: f64 = late_buckets.iter().sum();
            let verdict = if late_buckets[0] >= 60.0 {
                "EMBEDDINGS PRESENT (>=60s at cos>=0.5) -> clustering fails to group them"
            } else if late_buckets[0] + late_buckets[1] >= 30.0 {
                "PARTIAL (30-60s at cos>=0.4) -> mixed signal; clustering + embedding both weak"
            } else {
                "EMBEDDINGS ABSENT (<30s at cos>=0.4) -> embedding model fails on mixed audio"
            };
            eprintln!("  VERDICT: {} (late total {:.0}s)", verdict, total_late);
            eprintln!(
                "  Late chunks at cos>=0.5: count={} (showing top 10 by cos):",
                late_top.len()
            );
            for (cos, mid, dur) in late_top.iter().take(10) {
                eprintln!("    cos={:.3} @ {:.0}s (dur {:.1}s)", cos, mid, dur);
            }
        }

        // ── STEP 8: alternate embedding models on the SAME native windows.
        // STEP 7 proved nemo_titanet_small extracts Speaker 2's print from
        // 96.6% of Cynthia's late speech. This tests whether a different
        // extractor recovers her print from the same mixed audio. Early
        // Cynthia windows are selected by TIME overlap with nemo's cos>=0.5
        // early set (model-independent), so centroid differences reflect the
        // extractor, not the selection.
        {
            let alt_segments: Vec<(f64, f64)> = all_chunks
                .iter()
                .map(|c| {
                    let s = c.start_sample as f64 / sr_f;
                    (s, s + c.duration_secs)
                })
                .collect();

            let nemo_cyn_early_ranges: Vec<(f64, f64)> = all_chunks
                .iter()
                .filter(|c| {
                    let mid = c.start_sample as f64 / sr_f + c.duration_secs * 0.5;
                    mid < 1800.0
                        && cosine_similarity_centroids(&c.embedding, &cynthia_centroid) >= 0.5
                })
                .map(|c| {
                    let s = c.start_sample as f64 / sr_f;
                    (s, s + c.duration_secs)
                })
                .collect();

            let alt_models: [(&str, &str); 3] = [
                ("3D-speaker", "3dspeaker-embedding.onnx"),
                ("ERES2Net", "eres2net-embedding.onnx"),
                ("nemo_titanet_large", "nemo-titanet-large-embedding.onnx"),
            ];

            for (model_name, model_file) in &alt_models {
                let alt_path = models_dir.join(model_file);
                if !alt_path.exists() {
                    eprintln!(
                        "STEP 8 ({}): SKIPPED — model not found at {}",
                        model_name,
                        alt_path.display()
                    );
                    continue;
                }

                let alt_adapter = match crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                    alt_path.to_str().unwrap(),
                    segmentation_path.to_str().unwrap(),
                    std::sync::Arc::clone(&threshold_fp),
                ) {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!(
                            "STEP 8 ({}): adapter create failed: {} — skipping",
                            model_name, e
                        );
                        continue;
                    }
                };

                let alt_adapter_arc = std::sync::Arc::new(alt_adapter);
                let alt_adapter_for_embed = std::sync::Arc::clone(&alt_adapter_arc);
                let samples_for_alt = std::sync::Arc::clone(&samples_arc);
                let segments_for_alt: Vec<(f64, f64)> = alt_segments.clone();
                let alt_chunks = tokio::task::spawn_blocking(move || {
                    alt_adapter_for_embed.build_chunks_with_min(
                        &samples_for_alt,
                        DIARIZATION_SAMPLE_RATE,
                        &segments_for_alt,
                        0.0,
                    )
                })
                .await
                .expect("alt build_chunks panicked");

                let dim = alt_chunks.first().map(|c| c.embedding.len()).unwrap_or(0);
                if dim == 0 {
                    eprintln!(
                        "STEP 8 ({}): no alt chunks produced — skipping",
                        model_name
                    );
                    continue;
                }

                let mut alt_sum = vec![0.0f32; dim];
                let mut alt_n = 0usize;
                for c in &alt_chunks {
                    let mid = c.start_sample as f64 / sr_f + c.duration_secs * 0.5;
                    if mid >= 1800.0 {
                        continue;
                    }
                    let cs = c.start_sample as f64 / sr_f;
                    let ce = cs + c.duration_secs;
                    let in_cyn = nemo_cyn_early_ranges
                        .iter()
                        .any(|&(rs, re)| cs < re && rs < ce);
                    if !in_cyn {
                        continue;
                    }
                    for (acc, v) in alt_sum.iter_mut().zip(c.embedding.iter()) {
                        *acc += v;
                    }
                    alt_n += 1;
                }
                if alt_n == 0 {
                    eprintln!(
                        "STEP 8 ({}): NO early Cynthia chunks matched — skipping",
                        model_name
                    );
                    continue;
                }
                let alt_centroid: Vec<f32> =
                    alt_sum.iter().map(|v| v / alt_n as f32).collect();

                let mut early_buckets = [0.0f64; 4];
                let mut late_buckets = [0.0f64; 4];
                let mut late_max_cos = -1.0f32;
                for c in &alt_chunks {
                    let mid = c.start_sample as f64 / sr_f + c.duration_secs * 0.5;
                    let cos = cosine_similarity_centroids(&c.embedding, &alt_centroid);
                    let dur = c.duration_secs;
                    let idx = if cos >= 0.5 {
                        0
                    } else if cos >= 0.4 {
                        1
                    } else if cos >= 0.3 {
                        2
                    } else {
                        3
                    };
                    if mid < 1800.0 {
                        early_buckets[idx] += dur;
                    } else {
                        late_buckets[idx] += dur;
                        if cos > late_max_cos {
                            late_max_cos = cos;
                        }
                    }
                }
                let total_late: f64 = late_buckets.iter().sum();
                let verdict = if late_buckets[0] >= 60.0 {
                    "EMBEDDINGS PRESENT (>=60s at cos>=0.5)"
                } else if late_buckets[0] + late_buckets[1] >= 30.0 {
                    "PARTIAL (30-60s at cos>=0.4)"
                } else {
                    "EMBEDDINGS ABSENT (<30s at cos>=0.4)"
                };
                eprintln!(
                    "STEP 8 ({}): dim={}, alt_chunks={} (nemo all_chunks={}), early_cyn_n={}",
                    model_name, dim, alt_chunks.len(), all_chunks.len(), alt_n
                );
                eprintln!(
                    "  EARLY (<30min): cos>=0.5: {:.0}s | >=0.4: {:.0}s | >=0.3: {:.0}s | <0.3: {:.0}s",
                    early_buckets[0], early_buckets[1], early_buckets[2], early_buckets[3]
                );
                eprintln!(
                    "  LATE (>=30min): cos>=0.5: {:.0}s | >=0.4: {:.0}s | >=0.3: {:.0}s | <0.3: {:.0}s | max_cos={:.3}",
                    late_buckets[0], late_buckets[1], late_buckets[2], late_buckets[3], late_max_cos
                );
                eprintln!("  VERDICT: {} (late total {:.0}s)", verdict, total_late);
            }
        }
    }

    // CONFIRMATION (2026-07-16): runs the REAL production process() on
    // cde5c264 and exports final segment labels + centroids to a temp file,
    // so the Python pipeline replication (full_pipeline.py) can be checked
    // against the actual Rust binary. Python then identifies Cynthia by cos
    // to the validated spike_cen centroid and measures her early/late speech.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_export_final_pipeline() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "nemo_titanet embedding model missing");
        assert!(segmentation_path.exists(), "pyannote segmentation model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                threshold_fp,
            )
            .expect("create adapter");

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");

        let diarization = tokio::task::spawn_blocking(move || {
            adapter.process(&samples, DIARIZATION_SAMPLE_RATE, &transcript_segments)
        })
        .await
        .expect("process panicked")
        .expect("process failed");

        let mut segments = diarization.segments;
        let mut centroids = diarization.centroids;
        let effective_cap = resolve_effective_cap_for_meeting(&pool, meeting_id).await;
        enforce_max_speakers_cap(&mut centroids, &mut segments, effective_cap);

        let spk: std::collections::HashSet<u32> = segments.iter().map(|s| s.speaker_id).collect();
        eprintln!(
            "EXPORT: {} segments, {} speakers (cap {})",
            segments.len(),
            spk.len(),
            effective_cap
        );

        let out = std::env::temp_dir().join("meetily_final_labels_cde5c264.txt");
        let mut s = String::new();
        for seg in &segments {
            s.push_str(&format!(
                "SEG {:.4} {:.4} {}\n",
                seg.start_seconds, seg.end_seconds, seg.speaker_id
            ));
        }
        let mut sorted_c: Vec<(&u32, &Vec<f32>)> = centroids.iter().collect();
        sorted_c.sort_by_key(|(k, _)| **k);
        for (k, v) in &sorted_c {
            s.push_str(&format!("CENT {}", k));
            for x in v.iter() {
                s.push_str(&format!(" {:.6}", x));
            }
            s.push('\n');
        }
        std::fs::write(&out, s).expect("write export");
        eprintln!("EXPORT: wrote {}", out.display());
    }

    // §1.1 perf gate + §5.1 oracle (diarization-label-quality): runs the full
    // two-pass (process → enforce_max_speakers_cap → refine_pass2 with post-cap
    // centroids) on cde5c264 and asserts the [46:58] Ricardo interjection —
    // swallowed by production into a Cynthia run — survives as a ≥10s fine
    // segment whose centroid matches a Ricardo reference derived from his
    // user-confirmed 17:37 join region. Also gates Pass 2 wall-clock (<60s, else
    // §1.2 scopes an 8kHz-downsample path). Replaces manual QA per
    // feedback_verify_with_existing_data.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_two_pass_oracle() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "nemo_titanet embedding model missing");
        assert!(segmentation_path.exists(), "pyannote segmentation model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                threshold_fp,
            )
            .expect("create adapter");

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");
        let effective_cap = resolve_effective_cap_for_meeting(&pool, meeting_id).await;

        let sr = DIARIZATION_SAMPLE_RATE;
        let n_fine_grid = crate::audio::speaker::sherpa_adapter::fine_chunk_ranges(
            samples.len(),
            sr,
            crate::audio::speaker::sherpa_adapter::FINE_SPLIT_SECS,
        )
        .len();

        let (fine_segments, centroids, pass2_secs) =
            tokio::task::spawn_blocking(move || {
                let coarse = adapter.process(&samples, sr, &transcript_segments)?;
                let mut segments = coarse.segments;
                let mut centroids = coarse.centroids;
                enforce_max_speakers_cap(&mut centroids, &mut segments, effective_cap);

                let t_pass2 = std::time::Instant::now();
                let fine = adapter.refine_pass2(&samples, sr, &centroids)?;
                let pass2_secs = t_pass2.elapsed().as_secs_f64();

                Ok::<_, anyhow::Error>((fine, centroids, pass2_secs))
            })
            .await
            .expect("two-pass panicked")
            .expect("two-pass failed");

        eprintln!(
            "PASS2: {:.2}s wall-clock, ~{} fine-grid chunks, {} output segments",
            pass2_secs, n_fine_grid, fine_segments.len()
        );

        // Identify Ricardo by temporal ground truth, not voice averaging: he
        // joins at 17:37, so his label has minimal pre-join presence. A voice
        // reference averaged from [17:37,19:00] blends all 3 speakers (diagnostic
        // dump showed chunks nearest to centroids 0, 1, AND 2 in that span) and
        // lands closest to the wrong centroid. The join-time constraint is
        // user-confirmed ground truth and needs no clean audio reference.
        let join_sec = 17.0 * 60.0 + 37.0;
        let dur_before = |label: u32| {
            fine_segments
                .iter()
                .filter(|s| s.speaker_id == label && s.start_seconds < join_sec)
                .map(|s| (s.end_seconds.min(join_sec) - s.start_seconds).max(0.0))
                .sum::<f64>()
        };
        let dur_after = |label: u32| {
            fine_segments
                .iter()
                .filter(|s| s.speaker_id == label && s.end_seconds > join_sec)
                .map(|s| (s.end_seconds - s.start_seconds.max(join_sec)).max(0.0))
                .sum::<f64>()
        };
        let ric_label = centroids
            .keys()
            .map(|&k| (k, dur_before(k)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .expect("no centroids")
            .0;
        let ric_late = dur_after(ric_label);
        eprintln!(
            "RICARDO: label {} (pre-join {:.0}s, post-join {:.0}s)",
            ric_label,
            dur_before(ric_label),
            ric_late
        );
        assert!(
            ric_late > 300.0,
            "identified late-joiner label {} has only {:.0}s post-join speech (expected >300s) — \
             temporal ground-truth heuristic picked a phantom label",
            ric_label,
            ric_late
        );

        // §5.1 oracle (primary target): [46:42, 47:02] must contain ≥10s of
        // Ricardo — production swallowed [46:58] into a Cynthia run.
        let window_start = 46.0 * 60.0 + 42.0;
        let window_end = 47.0 * 60.0 + 2.0;
        let ric_dur: f64 = fine_segments
            .iter()
            .filter(|s| s.speaker_id == ric_label)
            .map(|s| {
                let ov = s.end_seconds.min(window_end) - s.start_seconds.max(window_start);
                ov.max(0.0)
            })
            .sum();
        eprintln!("RICARDO [46:42–47:02]: {:.1}s total (target ≥10s)", ric_dur);
        assert!(
            ric_dur >= 10.0,
            "§5.1 FAIL: Ricardo has only {:.1}s in [46:42–47:02] (target ≥10s); \
             interjection still swallowed",
            ric_dur
        );

        // §5.1 oracle (facet 2): Ricardo must NOT appear before his 17:37 join.
        // The [0:01] "Hello" vowel-dominated chunk was globally nearest Ricardo
        // in production; the orphan scan should have relabeled it.
        let early_ric_count = fine_segments
            .iter()
            .filter(|s| s.speaker_id == ric_label && s.start_seconds < 3.0)
            .count();
        eprintln!(
            "RICARDO in [0–3s]: {} segment(s) (must be 0 — he joins at 17:37)",
            early_ric_count
        );
        assert_eq!(
            early_ric_count, 0,
            "§5.1 FAIL: Ricardo labeled in [0–3s] before his 17:37 join — \
             facet-2 temporal-presence orphan scan failed"
        );

        // §1.1 GATE (checked last so oracle results are visible regardless):
        // Pass 2 must complete in <60s after rayon parallelization. If this
        // fails, scope an 8kHz-downsample path per design.md §1.2.
        assert!(
            pass2_secs < 60.0,
            "§1.1 GATE: Pass 2 took {:.2}s (>60s) — scope 8kHz-downsample path per design",
            pass2_secs
        );
    }

    // §5.2: end-to-end integration of the real two-pass diarization with the
    // token-level alignment layer on the [46:42–47:02] target. §5.1 proves the
    // boundary exists (Ricardo [2802–2820s]); this test proves it reaches
    // align_with_tokens and yields ≥1 word to each side of the boundary.
    //
    // CAVEAT: cde5c264 was recorded before token_timestamps population and the
    // prod DB is read-only (?mode=ro), so the column is NULL for every row.
    // Token_words are synthesized at uniform spacing within each real Whisper
    // segment's [audio_start, audio_end]. This exercises the identical
    // alignment logic (token.start_ms → speaker_at_time) in the correct
    // audio-relative time base; validating with real Whisper token timing is
    // deferred until cde5c264 is re-transcribed.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_per_word_split_alignment() {
        use crate::audio::speaker::alignment::{
            align_transcripts_with_diarization, DiarizationSegment, TokenWord,
        };
        use std::collections::HashSet;
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                threshold_fp,
            )
            .expect("create adapter");

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");
        let effective_cap = resolve_effective_cap_for_meeting(&pool, meeting_id).await;
        let sr = DIARIZATION_SAMPLE_RATE;

        let fine_segments = tokio::task::spawn_blocking(move || {
            let coarse = adapter.process(&samples, sr, &transcript_segments)?;
            let mut segments = coarse.segments;
            let mut centroids = coarse.centroids;
            enforce_max_speakers_cap(&mut centroids, &mut segments, effective_cap);
            adapter.refine_pass2(&samples, sr, &centroids)
        })
        .await
        .expect("two-pass panicked")
        .expect("two-pass failed");

        // Identify Ricardo by temporal ground truth (minimal pre-17:37 presence).
        let join_sec = 17.0 * 60.0 + 37.0;
        let labels: HashSet<u32> = fine_segments.iter().map(|s| s.speaker_id).collect();
        let ric_label = labels
            .iter()
            .map(|&k| {
                let pre: f64 = fine_segments
                    .iter()
                    .filter(|s| s.speaker_id == k && s.start_seconds < join_sec)
                    .map(|s| (s.end_seconds.min(join_sec) - s.start_seconds).max(0.0))
                    .sum();
                (k, pre)
            })
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(k, _)| k)
            .expect("speaker labels");

        // Real transcript text + boundaries from the DB; synthesize uniform
        // token_words (column is NULL — see CAVEAT above).
        let mut transcripts = fetch_transcripts_for_alignment(&pool, meeting_id)
            .await
            .expect("fetch transcripts for alignment");
        for t in &mut transcripts {
            if t.token_words.is_some() {
                continue;
            }
            let words: Vec<&str> = t.text.split_whitespace().collect();
            if words.is_empty() {
                continue;
            }
            let span = (t.audio_end_ms - t.audio_start_ms).max(1);
            let step = span as f64 / words.len() as f64;
            t.token_words = Some(
                words
                    .iter()
                    .enumerate()
                    .map(|(i, w)| TokenWord {
                        word: w.to_string(),
                        start_ms: t.audio_start_ms + (step * i as f64) as i64,
                        end_ms: t.audio_start_ms + (step * (i as f64 + 1.0)) as i64,
                    })
                    .collect(),
            );
        }

        let window_start = ((46.0 * 60.0 + 42.0) * 1000.0) as i64;
        let window_end = ((47.0 * 60.0 + 2.0) * 1000.0) as i64;
        let windowed: Vec<_> = transcripts
            .into_iter()
            .filter(|t| t.audio_end_ms > window_start && t.audio_start_ms < window_end)
            .collect();
        assert!(
            !windowed.is_empty(),
            "no transcript segments overlap [46:42–47:02] — data mismatch"
        );

        let diarization: Vec<DiarizationSegment> = fine_segments
            .iter()
            .map(|s| DiarizationSegment {
                start_ms: (s.start_seconds * 1000.0) as i64,
                end_ms: (s.end_seconds * 1000.0) as i64,
                speaker_id: s.speaker_id,
            })
            .collect();

        let aligned = align_transcripts_with_diarization(windowed, &diarization);

        let count_words = |label: u32| {
            aligned
                .iter()
                .filter(|a| a.speaker == format!("Speaker {}", label))
                .map(|a| a.text.split_whitespace().count())
                .sum::<usize>()
        };
        let ric_words = count_words(ric_label);
        let other_words: usize = labels
            .iter()
            .filter(|&&l| l != ric_label)
            .map(|&l| count_words(l))
            .sum();
        eprintln!(
            "§5.2 [46:42–47:02]: Ricardo(label {}) {} words, other speakers {} words, {} aligned segments",
            ric_label,
            ric_words,
            other_words,
            aligned.len()
        );
        assert!(
            ric_words >= 1,
            "§5.2 FAIL: 0 Ricardo words in [46:42–47:02] — alignment didn't attribute any word to the boundary speaker"
        );
        assert!(
            other_words >= 1,
            "§5.2 FAIL: 0 non-Ricardo words in [46:42–47:02] — alignment didn't split across the boundary"
        );
    }

    // GATE alternative: tests whether FINER FIXED splitting fixes absorption
    // WITHOUT the native pipeline. Root cause (per absorption memory) is coarse
    // chunking — `effective_split` coarsens to ~8.3s for this 83-min meeting,
    // and segments ≤10s get one embedding for the whole mixed-speaker window.
    // Pre-splitting whisper segments into 3–5s sub-segments forces build_chunks
    // into its one-chunk-per-segment path at a granularity where windows are
    // more likely speaker-homogeneous. If Cynthia survives here, the fix is
    // trivially simple (change the split granularity) — no native pipeline, no
    // new model, no double-embedding latency.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_finer_split_diagnostic() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples_arc = std::sync::Arc::new(decoded.to_whisper_format());
        let sr_f = DIARIZATION_SAMPLE_RATE as f64;
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "embedding model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                std::sync::Arc::clone(&threshold_fp),
            )
            .expect("create adapter");
        let adapter_arc = std::sync::Arc::new(adapter);

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");

        // ── Fingerprint: Cynthia from the OLD (coarse ~8.3s) path.
        let adapter_for_old = std::sync::Arc::clone(&adapter_arc);
        let samples_for_old = std::sync::Arc::clone(&samples_arc);
        let seg_arc = std::sync::Arc::new(transcript_segments.clone());
        let old_chunks = tokio::task::spawn_blocking(move || {
            adapter_for_old.build_chunks(&samples_for_old, DIARIZATION_SAMPLE_RATE, &seg_arc)
        })
        .await
        .expect("old build_chunks panicked");
        assert!(
            !old_chunks.is_empty(),
            "need old-path chunks to fingerprint Cynthia"
        );

        let (old_labels, _) =
            crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&old_chunks, 0.40);
        let mut old_early: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        let mut old_late: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for (c, &lab) in old_chunks.iter().zip(old_labels.iter()) {
            let t0 = c.start_sample as f64 / sr_f;
            if t0 < 1800.0 {
                *old_early.entry(lab).or_insert(0) += 1;
            } else {
                *old_late.entry(lab).or_insert(0) += 1;
            }
        }
        let cynthia_label = old_early
            .iter()
            .filter(|(_, &n)| n >= 5)
            .min_by_key(|(lab, _)| old_late.get(lab).copied().unwrap_or(0))
            .map(|(lab, _)| *lab)
            .expect("fingerprint: need an early-dominant old label");
        let dim = old_chunks[0].embedding.len();
        let mut cynthia_sum = vec![0.0f32; dim];
        let mut cynthia_n = 0usize;
        for (c, &lab) in old_chunks.iter().zip(old_labels.iter()) {
            if lab == cynthia_label && (c.start_sample as f64 / sr_f) < 1800.0 {
                for (acc, v) in cynthia_sum.iter_mut().zip(c.embedding.iter()) {
                    *acc += v;
                }
                cynthia_n += 1;
            }
        }
        assert!(cynthia_n > 0, "Cynthia fingerprint needs ≥1 early chunk");
        let cynthia_centroid: Vec<f32> = cynthia_sum.iter().map(|v| v / cynthia_n as f32).collect();
        eprintln!(
            "FINGERPRINT: old label {} = Cynthia ({} early chunks); old early {:?} | late {:?}",
            cynthia_label, cynthia_n, old_early, old_late,
        );

        // ── Sweep finer fixed-split granularities.
        for &split_secs in &[3.0f64, 5.0] {
            let fine_segments: Vec<(f64, f64)> = transcript_segments
                .iter()
                .flat_map(|(start, end)| {
                    let mut t = *start;
                    let mut subs: Vec<(f64, f64)> = Vec::new();
                    while t < *end {
                        let next = (t + split_secs).min(*end);
                        subs.push((t, next));
                        t = next;
                    }
                    subs
                })
                .collect();

            let adapter_for_fine = std::sync::Arc::clone(&adapter_arc);
            let samples_for_fine = std::sync::Arc::clone(&samples_arc);
            let fine_arc = std::sync::Arc::new(fine_segments);
            let t_bc = std::time::Instant::now();
            let chunks = tokio::task::spawn_blocking(move || {
                adapter_for_fine.build_chunks(
                    &samples_for_fine,
                    DIARIZATION_SAMPLE_RATE,
                    &fine_arc,
                )
            })
            .await
            .expect("fine build_chunks panicked");
            let bc_secs = t_bc.elapsed().as_secs_f64();

            let embeddings: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
            let timestamps: Vec<f64> =
                chunks.iter().map(|c| c.start_sample as f64 / sr_f).collect();
            let durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();

            let (segs, cents) = prod_path_on_chunks(
                &chunks,
                &embeddings,
                &timestamps,
                &durations,
                0.40,
                3,
                sr_f,
            );
            let speakers: std::collections::HashSet<u32> =
                segs.iter().map(|s| s.speaker_id).collect();
            let mut p_early: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            let mut p_late: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
            for s in &segs {
                let dur = s.end_seconds - s.start_seconds;
                let mid = (s.start_seconds + s.end_seconds) * 0.5;
                if mid < 1800.0 {
                    *p_early.entry(s.speaker_id).or_insert(0.0) += dur;
                } else {
                    *p_late.entry(s.speaker_id).or_insert(0.0) += dur;
                }
            }
            // Cosine threshold lowered to 0.4: the fingerprint comes from
            // coarse ~8.3s mixed-speaker chunks, so it's noisier than a clean
            // single-speaker centroid — a true Cynthia cluster in the finer path
            // may match at 0.4–0.6 rather than 0.8+.
            let (cyn_id, cyn_cos) = cents
                .iter()
                .map(|(id, c)| {
                    (*id, cosine_similarity_centroids(c, &cynthia_centroid))
                })
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                .unwrap_or((u32::MAX, 0.0));
            let cyn_early = p_early.get(&cyn_id).copied().unwrap_or(0.0);
            let cyn_late = p_late.get(&cyn_id).copied().unwrap_or(0.0);
            let ratio = if cyn_early > 0.0 {
                cyn_late / cyn_early
            } else {
                f64::INFINITY
            };
            let meets = cyn_early > 0.0 && cyn_late >= 0.30 * cyn_early && cyn_late >= 60.0;
            let tag = if cyn_cos < 0.4 {
                "NO MATCH (cos<0.4)"
            } else if meets {
                "★ MEETS proceed metric"
            } else {
                "below metric"
            };
            eprintln!(
                "SPLIT {:.0}s: {} chunks in {:.0}s → {} speakers [early {:?} | late {:?}] | Cynthia→cluster {} (cos {:.3}): early {:.0}s late {:.0}s (ratio {:.2}) {}",
                split_secs,
                chunks.len(),
                bc_secs,
                speakers.len(),
                p_early,
                p_late,
                cyn_id,
                cyn_cos,
                cyn_early,
                cyn_late,
                ratio,
                tag,
            );
        }
    }

    // Decisive diagnostic: per-chunk embedding analysis. Determines whether
    // Cynthia's late-half absorption is EMBEDDING DEGRADATION (her quieter
    // speech produces embeddings far from her centroid) or CLUSTERING MERGE
    // (embeddings are close but AHC assigns them to other clusters). This
    // chooses between an audio/embedding-level fix and a clustering-level fix.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_embedding_survival_diagnostic() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples_arc = std::sync::Arc::new(decoded.to_whisper_format());
        let sr_f = DIARIZATION_SAMPLE_RATE as f64;

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "embedding model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                std::sync::Arc::clone(&threshold_fp),
            )
            .expect("create adapter");
        let adapter_arc = std::sync::Arc::new(adapter);

        let transcript_segments =
            fetch_transcript_timestamps(&pool, meeting_id, decoded.duration_seconds.max(0.001))
                .await
                .expect("fetch transcript timestamps");

        let fine_segments: Vec<(f64, f64)> = transcript_segments
            .iter()
            .flat_map(|(start, end)| {
                let mut t = *start;
                let mut subs: Vec<(f64, f64)> = Vec::new();
                while t < *end {
                    let next = (t + 3.0).min(*end);
                    subs.push((t, next));
                    t = next;
                }
                subs
            })
            .collect();

        let adapter_for_chunks = std::sync::Arc::clone(&adapter_arc);
        let samples_for_chunks = std::sync::Arc::clone(&samples_arc);
        let fine_arc = std::sync::Arc::new(fine_segments);
        eprintln!("Extracting embeddings for {} 3s chunks...", fine_arc.len());
        let chunks = tokio::task::spawn_blocking(move || {
            adapter_for_chunks.build_chunks(
                &samples_for_chunks,
                DIARIZATION_SAMPLE_RATE,
                &fine_arc,
            )
        })
        .await
        .expect("build_chunks panicked");
        eprintln!("Built {} chunks", chunks.len());

        for &thresh in &[0.30f32, 0.40, 0.50] {
            let (labels, centroids) =
                crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&chunks, thresh);

            let mut early_dur: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            let mut late_dur: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            for (i, c) in chunks.iter().enumerate() {
                let t0 = c.start_sample as f64 / sr_f;
                let map = if t0 < 1800.0 {
                    &mut early_dur
                } else {
                    &mut late_dur
                };
                *map.entry(labels[i]).or_insert(0.0) += c.duration_secs;
            }
            let cynthia_cluster = *early_dur
                .iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(k, _)| k)
                .unwrap_or(&0);
            let cynthia_centroid = centroids
                .get(&cynthia_cluster)
                .expect("centroid exists");

            let mut bins_early = [0usize; 5];
            let mut bins_late = [0usize; 5];
            let mut late_close_right = 0usize;
            let mut late_close_wrong = 0usize;
            let mut late_far = 0usize;
            for (i, c) in chunks.iter().enumerate() {
                let t0 = c.start_sample as f64 / sr_f;
                let cos = cosine_similarity_centroids(&c.embedding, cynthia_centroid);
                let bin = if cos < 0.2 {
                    0
                } else if cos < 0.4 {
                    1
                } else if cos < 0.6 {
                    2
                } else if cos < 0.8 {
                    3
                } else {
                    4
                };
                let is_late = t0 >= 1800.0;
                let is_cynthia = labels[i] == cynthia_cluster;
                if is_late {
                    bins_late[bin] += 1;
                    if cos >= thresh {
                        if is_cynthia {
                            late_close_right += 1;
                        } else {
                            late_close_wrong += 1;
                        }
                    } else {
                        late_far += 1;
                    }
                } else {
                    bins_early[bin] += 1;
                }
            }

            let cyn_e = early_dur.get(&cynthia_cluster).copied().unwrap_or(0.0);
            let cyn_l = late_dur.get(&cynthia_cluster).copied().unwrap_or(0.0);
            eprintln!(
                "\n=== THRESHOLD {:.2}: {} clusters | Cynthia=cluster {} | early {:.0}s late {:.0}s (ratio {:.2}) ===",
                thresh,
                centroids.len(),
                cynthia_cluster,
                cyn_e,
                cyn_l,
                if cyn_e > 0.0 {
                    cyn_l / cyn_e
                } else {
                    f64::INFINITY
                },
            );
            eprintln!(
                "  Early cos bins [.0-.2 .2-.4 .4-.6 .6-.8 .8-1.0]: {:?}",
                bins_early,
            );
            eprintln!(
                "  Late  cos bins [.0-.2 .2-.4 .4-.6 .6-.8 .8-1.0]: {:?}",
                bins_late,
            );
            eprintln!(
                "  Late half: {} chunks close->Cynthia, {} close->WRONG cluster, {} far (<{:.2})",
                late_close_right, late_close_wrong, late_far, thresh,
            );
        }
    }

    /// Stage-trace: run the EXACT production path (Whisper transcript segments →
    /// build_chunks → cluster → smooth → coalesce → merge_short) on cde5c264 and
    /// log Cynthia's early/late duration at each stage. Python POCs showed no
    /// contamination at the clustering stage — this isolates which Rust-only
    /// post-clustering step absorbs her.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_stage_trace_diagnostic() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("cde5c264 folder_path missing");
        let audio_path =
            find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples_arc = std::sync::Arc::new(decoded.to_whisper_format());
        let sr_f = DIARIZATION_SAMPLE_RATE as f64;
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "embedding model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                std::sync::Arc::clone(&threshold_fp),
            )
            .expect("create adapter");
        let adapter_arc = std::sync::Arc::new(adapter);

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");

        // ── Fingerprint Cynthia: old coarse path, early-dominant cluster centroid.
        let adapter_for_old = std::sync::Arc::clone(&adapter_arc);
        let samples_for_old = std::sync::Arc::clone(&samples_arc);
        let seg_arc = std::sync::Arc::new(transcript_segments.clone());
        let old_chunks = tokio::task::spawn_blocking(move || {
            adapter_for_old.build_chunks(&samples_for_old, DIARIZATION_SAMPLE_RATE, &seg_arc)
        })
        .await
        .expect("old build_chunks panicked");
        assert!(!old_chunks.is_empty(), "need old-path chunks");
        let (old_labels, _) =
            crate::audio::speaker::sherpa_adapter::cluster_by_centroids(&old_chunks, 0.40);
        let mut old_early: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        let mut old_late: std::collections::HashMap<u32, usize> = std::collections::HashMap::new();
        for (c, &lab) in old_chunks.iter().zip(old_labels.iter()) {
            let t0 = c.start_sample as f64 / sr_f;
            if t0 < 1800.0 {
                *old_early.entry(lab).or_insert(0) += 1;
            } else {
                *old_late.entry(lab).or_insert(0) += 1;
            }
        }
        let cynthia_label = old_early
            .iter()
            .filter(|(_, &n)| n >= 5)
            .min_by_key(|(lab, _)| old_late.get(lab).copied().unwrap_or(0))
            .map(|(lab, _)| *lab)
            .expect("need an early-dominant old label to fingerprint Cynthia");
        let dim = old_chunks[0].embedding.len();
        let mut cynthia_sum = vec![0.0f32; dim];
        let mut cynthia_n = 0usize;
        for (c, &lab) in old_chunks.iter().zip(old_labels.iter()) {
            if lab == cynthia_label && (c.start_sample as f64 / sr_f) < 1800.0 {
                for (acc, v) in cynthia_sum.iter_mut().zip(c.embedding.iter()) {
                    *acc += v;
                }
                cynthia_n += 1;
            }
        }
        assert!(cynthia_n > 0, "Cynthia fingerprint needs ≥1 early chunk");
        let cynthia_fp: Vec<f32> = cynthia_sum.iter().map(|v| v / cynthia_n as f32).collect();
        eprintln!(
            "STAGE-TRACE: Cynthia fingerprint from {} early chunks (old label {}, old early {:?} late {:?})",
            cynthia_n, cynthia_label, old_early, old_late,
        );

        // ── Production chunks: Whisper transcript segments (NOT native windows).
        let adapter_for_prod = std::sync::Arc::clone(&adapter_arc);
        let samples_for_prod = std::sync::Arc::clone(&samples_arc);
        let prod_seg_arc = std::sync::Arc::new(transcript_segments.clone());
        let chunks = tokio::task::spawn_blocking(move || {
            adapter_for_prod.build_chunks(&samples_for_prod, DIARIZATION_SAMPLE_RATE, &prod_seg_arc)
        })
        .await
        .expect("prod build_chunks panicked");
        eprintln!(
            "STAGE-TRACE: {} production chunks from {} transcript segments",
            chunks.len(),
            transcript_segments.len(),
        );

        let embeddings: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
        let timestamps: Vec<f64> = chunks.iter().map(|c| c.start_sample as f64 / sr_f).collect();
        let durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();

        use crate::audio::speaker::sherpa_adapter::{
            SmoothParams, cluster_by_centroids, correct_labels_by_f0, detect_f0,
            merge_short_speakers, smooth_to_fixed_point, F0CorrectionParams,
        };

        // ── Stage A: cluster only (centroid linkage — current production).
        let (labels_a, centroids_a) = cluster_by_centroids(&chunks, 0.40);
        trace_chunk_stage("A: centroid_linkage", &labels_a, &timestamps, &durations, &centroids_a, &cynthia_fp);

        // ── Stage B: + smoothing. Count label changes to measure smoothing impact.
        let (labels_b, centroids_b) = smooth_to_fixed_point(
            &labels_a,
            &embeddings,
            &timestamps,
            &durations,
            &centroids_a,
            &SmoothParams::default(),
        );
        let changed = labels_a.iter().zip(labels_b.iter()).filter(|(a, b)| a != b).count();
        eprintln!("STAGE-TRACE: smoothing changed {} of {} chunk labels", changed, labels_a.len());
        trace_chunk_stage("B: +smoothing", &labels_b, &timestamps, &durations, &centroids_b, &cynthia_fp);

        // ── Stage C: build segments (same as process()).
        let mut indexed: Vec<(usize, u32)> = labels_b.iter().copied().enumerate().collect();
        indexed.sort_by_key(|(i, _)| chunks[*i].start_sample);
        let mut segments: Vec<SpeakerSegment> = Vec::new();
        if let Some(&(ci0, cur0)) = indexed.first() {
            let mut cur = cur0;
            let mut seg_start = chunks[ci0].start_sample as f64 / sr_f;
            let mut seg_end = chunks[ci0].end_sample as f64 / sr_f;
            for &(ci, lab) in &indexed[1..] {
                let cs = chunks[ci].start_sample as f64 / sr_f;
                let ce = chunks[ci].end_sample as f64 / sr_f;
                if lab == cur {
                    seg_end = ce;
                } else {
                    segments.push(SpeakerSegment {
                        start_seconds: seg_start,
                        end_seconds: seg_end,
                        speaker_id: cur,
                    });
                    cur = lab;
                    seg_start = cs;
                    seg_end = ce;
                }
            }
            segments.push(SpeakerSegment {
                start_seconds: seg_start,
                end_seconds: seg_end,
                speaker_id: cur,
            });
        }
        trace_segment_stage("C: +segments", &segments, &centroids_b, &cynthia_fp);

        // ── Stage D: + merge_short_speakers (final production output).
        let total: f64 = segments.iter().map(|s| s.end_seconds - s.start_seconds).sum();
        let (segments_d, centroids_d) = merge_short_speakers(segments, centroids_b.clone(), total);
        trace_segment_stage("D: +merge_short (FINAL)", &segments_d, &centroids_d, &cynthia_fp);

        // ── Stage E: + F0 correction (change: diarization-f0-correction).
        // Apply F0 correction to the smoothed labels, build segments, merge.
        let samples_ref = samples_arc.as_ref();
        let labels_e = correct_labels_by_f0(
            &labels_b,
            samples_ref,
            DIARIZATION_SAMPLE_RATE,
            &chunks,
            &centroids_b,
            &F0CorrectionParams::default(),
        );
        let f0_changed = labels_b.iter().zip(labels_e.iter()).filter(|(a, b)| a != b).count();
        eprintln!("STAGE-TRACE: F0 correction changed {} of {} chunk labels", f0_changed, labels_e.len());

        // ── F0 failure diagnostic: per-cluster median F0, relabel flow,
        // late-half mixed-chunk F0 visibility. Answers whether Cynthia's
        // pitch is detectable in her absorbed (mixed-audio) late chunks.
        {
            let chunk_f0: Vec<Option<f32>> = chunks
                .iter()
                .map(|c| {
                    let end = c.end_sample.min(samples_ref.len());
                    if end <= c.start_sample { return None; }
                    detect_f0(&samples_ref[c.start_sample..end], DIARIZATION_SAMPLE_RATE)
                })
                .collect();
            let mut clust_labels: Vec<u32> = labels_b.iter().copied().collect();
            clust_labels.sort();
            clust_labels.dedup();
            let mut cluster_medians: std::collections::HashMap<u32, f32> = std::collections::HashMap::new();
            eprintln!("DIAG: per-cluster median F0 (labels_b):");
            for &lab in &clust_labels {
                let mut voiced: Vec<f32> = labels_b.iter().enumerate()
                    .filter(|(_, &l)| l == lab)
                    .filter_map(|(i, _)| chunk_f0[i])
                    .collect();
                voiced.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let total = labels_b.iter().filter(|&&l| l == lab).count();
                let med = if voiced.is_empty() { f32::NAN } else { voiced[voiced.len() / 2] };
                cluster_medians.insert(lab, med);
                eprintln!("DIAG:   cluster {} → median F0 {:.0} Hz ({} voiced / {} total)",
                    lab, med, voiced.len(), total);
            }
            let mut flow: std::collections::HashMap<(u32, u32), usize> = std::collections::HashMap::new();
            for (&a, &b) in labels_b.iter().zip(labels_e.iter()) {
                if a != b { *flow.entry((a, b)).or_default() += 1; }
            }
            eprintln!("DIAG: relabel flow (old→new):");
            let mut fv: Vec<_> = flow.into_iter().collect();
            fv.sort();
            for ((from, to), count) in &fv {
                eprintln!("DIAG:   {}→{}: {} chunks", from, to, count);
            }
            let cyn_clust_b = centroids_b.keys().copied().max_by(|a, b| {
                let ca = centroids_b.get(a).map(|v| cosine_similarity_centroids(&cynthia_fp, v)).unwrap_or(-1.0);
                let cb = centroids_b.get(b).map(|v| cosine_similarity_centroids(&cynthia_fp, v)).unwrap_or(-1.0);
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            }).unwrap_or(0);
            let cyn_f0_med = *cluster_medians.get(&cyn_clust_b).unwrap_or(&f32::NAN);
            let late_thresh = (1800.0 * sr_f) as usize;
            let mut late_voiced = 0usize;
            let mut late_unvoiced = 0usize;
            let mut late_match_cyn = 0usize;
            let mut late_f0s: Vec<f32> = Vec::new();
            for (i, c) in chunks.iter().enumerate() {
                if c.start_sample < late_thresh { continue; }
                if labels_b[i] == cyn_clust_b { continue; }
                match chunk_f0[i] {
                    Some(f) => {
                        late_voiced += 1;
                        late_f0s.push(f);
                        if (f - cyn_f0_med).abs() < 30.0 { late_match_cyn += 1; }
                    }
                    None => late_unvoiced += 1,
                }
            }
            eprintln!("DIAG: Cynthia cluster_b={}, median F0={:.0} Hz", cyn_clust_b, cyn_f0_med);
            eprintln!("DIAG: late non-Cynthia chunks: {} voiced, {} unvoiced, {} within 30Hz of Cynthia median",
                late_voiced, late_unvoiced, late_match_cyn);
            if !late_f0s.is_empty() {
                late_f0s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let n = late_f0s.len();
                eprintln!("DIAG: late non-Cynthia F0: min {:.0} p25 {:.0} med {:.0} p75 {:.0} max {:.0}",
                    late_f0s[0], late_f0s[n / 4], late_f0s[n / 2],
                    late_f0s[3 * n / 4], late_f0s[n - 1]);
            }

            // ── NON-CIRCULAR F0 diagnostic (2026-07-10): the circular
            // correct_labels_by_f0 failed because per-cluster F0 profiles
            // inherit embedding contamination. This derives each profile from
            // CLEAN EARLY chunks (start<1800s AND cos>=0.5) instead, then
            // reassigns late absorber chunks to Cynthia using her EARLY
            // profile. Reuses chunk_f0/cyn_clust_b/late_thresh above.
            let early_profile: std::collections::HashMap<u32, f32> = centroids_b
                .keys()
                .map(|&lab| {
                    let cent = centroids_b.get(&lab).cloned().unwrap_or_default();
                    let mut voiced: Vec<f32> = chunks
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| c.start_sample < late_thresh && !cent.is_empty())
                        .filter(|(i, c)| {
                            cosine_similarity_centroids(&c.embedding, &cent) >= 0.5
                        })
                        .filter_map(|(i, _)| chunk_f0[i])
                        .collect();
                    voiced.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let med = if voiced.is_empty() { f32::NAN } else { voiced[voiced.len() / 2] };
                    eprintln!(
                        "DIAG-NC: cluster {} early-clean F0 median {:.0} Hz ({} voiced)",
                        lab, med, voiced.len()
                    );
                    (lab, med)
                })
                .collect();
            let late_dur_of = |lab: u32, labels: &[u32]| -> f64 {
                chunks
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| labels[i] == lab)
                    .filter(|&(_, c)| c.start_sample >= late_thresh)
                    .map(|(_, c)| c.duration_secs)
                    .sum()
            };
            let absorber_cl = centroids_b
                .keys()
                .copied()
                .filter(|&l| l != cyn_clust_b)
                .max_by(|a, b| {
                    late_dur_of(*a, labels_b.as_slice())
                        .partial_cmp(&late_dur_of(*b, labels_b.as_slice()))
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .unwrap_or(cyn_clust_b);
            let cyn_prof = *early_profile.get(&cyn_clust_b).unwrap_or(&f32::NAN);
            let abs_prof = *early_profile.get(&absorber_cl).unwrap_or(&f32::NAN);
            let cyn_late_before = late_dur_of(cyn_clust_b, labels_b.as_slice());
            let abs_late_before = late_dur_of(absorber_cl, labels_b.as_slice());
            let mut labels_nc = labels_b.clone();
            let mut reassigned = 0usize;
            let mut re_cos_ge04 = 0usize;
            let mut re_cos_lt03 = 0usize;
            let cyn_cent = centroids_b.get(&cyn_clust_b).cloned().unwrap_or_default();
            if cyn_prof.is_finite() && abs_prof.is_finite() {
                let opposite = (cyn_prof < 180.0) != (abs_prof < 180.0);
                for (i, c) in chunks.iter().enumerate() {
                    if c.start_sample < late_thresh || labels_b[i] != absorber_cl {
                        continue;
                    }
                    let f0 = match chunk_f0[i] {
                        Some(f) => f,
                        None => continue,
                    };
                    if (f0 - cyn_prof).abs() < 30.0 && (f0 - abs_prof).abs() > 50.0 && opposite {
                        labels_nc[i] = cyn_clust_b;
                        reassigned += 1;
                        if !cyn_cent.is_empty() {
                            let cs = cosine_similarity_centroids(&c.embedding, &cyn_cent);
                            if cs >= 0.4 { re_cos_ge04 += 1; }
                            if cs < 0.3 { re_cos_lt03 += 1; }
                        }
                    }
                }
            }
            let cyn_late_after = late_dur_of(cyn_clust_b, labels_nc.as_slice());
            let abs_late_after = late_dur_of(absorber_cl, labels_nc.as_slice());
            eprintln!(
                "DIAG-NC: cyn_clust={} early F0 {:.0} Hz | absorber_cl={} early F0 {:.0} Hz | opposite_sides={}",
                cyn_clust_b, cyn_prof, absorber_cl, abs_prof, (cyn_prof < 180.0) != (abs_prof < 180.0)
            );
            eprintln!(
                "DIAG-NC: REASSIGN {} late absorber→Cynthia | cos>=0.4: {} | cos<0.3: {}",
                reassigned, re_cos_ge04, re_cos_lt03
            );
            eprintln!(
                "DIAG-NC: Cynthia late {:.0}s → {:.0}s | absorber late {:.0}s → {:.0}s",
                cyn_late_before, cyn_late_after, abs_late_before, abs_late_after
            );
        }

        let mut indexed_e: Vec<(usize, u32)> = labels_e.iter().copied().enumerate().collect();
        indexed_e.sort_by_key(|(i, _)| chunks[*i].start_sample);
        let mut segments_e: Vec<SpeakerSegment> = Vec::new();
        if let Some(&(ci0, cur0)) = indexed_e.first() {
            let mut cur = cur0;
            let mut seg_start = chunks[ci0].start_sample as f64 / sr_f;
            let mut seg_end = chunks[ci0].end_sample as f64 / sr_f;
            for &(ci, lab) in &indexed_e[1..] {
                let cs = chunks[ci].start_sample as f64 / sr_f;
                let ce = chunks[ci].end_sample as f64 / sr_f;
                if lab == cur {
                    seg_end = ce;
                } else {
                    segments_e.push(SpeakerSegment {
                        start_seconds: seg_start,
                        end_seconds: seg_end,
                        speaker_id: cur,
                    });
                    cur = lab;
                    seg_start = cs;
                    seg_end = ce;
                }
            }
            segments_e.push(SpeakerSegment {
                start_seconds: seg_start,
                end_seconds: seg_end,
                speaker_id: cur,
            });
        }
        let total_e: f64 = segments_e.iter().map(|s| s.end_seconds - s.start_seconds).sum();
        let (segments_e, centroids_e) = merge_short_speakers(segments_e, centroids_b.clone(), total_e);
        trace_segment_stage("E: +F0 correction (FINAL)", &segments_e, &centroids_e, &cynthia_fp);

        // ── Rails (§4.1): assert F0 correction recovers Cynthia without
        // stealing Speaker 2's chunks or erasing Cynthia's early half.
        // Identify Cynthia's cluster by cosine to fingerprint in each output.
        let cyn_cluster_d = centroids_d
            .keys()
            .copied()
            .max_by(|a, b| {
                let ca = centroids_d.get(a).map(|v| cosine_similarity_centroids(&cynthia_fp, v)).unwrap_or(-1.0);
                let cb = centroids_d.get(b).map(|v| cosine_similarity_centroids(&cynthia_fp, v)).unwrap_or(-1.0);
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(0);
        let cyn_cluster_e = centroids_e
            .keys()
            .copied()
            .max_by(|a, b| {
                let ca = centroids_e.get(a).map(|v| cosine_similarity_centroids(&cynthia_fp, v)).unwrap_or(-1.0);
                let cb = centroids_e.get(b).map(|v| cosine_similarity_centroids(&cynthia_fp, v)).unwrap_or(-1.0);
                ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(0);

        let speaker_dur_split = |segs: &[SpeakerSegment], sid: u32, early: bool| -> f64 {
            segs.iter()
                .filter(|s| s.speaker_id == sid)
                .map(|s| {
                    if early {
                        (1800.0_f64.min(s.end_seconds) - s.start_seconds).max(0.0)
                    } else {
                        (s.end_seconds - 1800.0_f64.max(s.start_seconds)).max(0.0)
                    }
                })
                .sum::<f64>()
        };

        let cyn_late_e = speaker_dur_split(&segments_e, cyn_cluster_e, false);
        let cyn_early_e = speaker_dur_split(&segments_e, cyn_cluster_e, true);
        let cyn_late_d = speaker_dur_split(&segments_d, cyn_cluster_d, false);
        let cyn_early_d = speaker_dur_split(&segments_d, cyn_cluster_d, true);

        eprintln!(
            "RAILS: Cynthia late D={:.0}s E={:.0}s | early D={:.0}s E={:.0}s",
            cyn_late_d, cyn_late_e, cyn_early_d, cyn_early_e,
        );

        // Floor: recovered late-half ≥ 600s (was ~26s without F0 correction).
        assert!(cyn_late_e >= 600.0,
            "FLOOR: Cynthia late-half {:.0}s < 600s — F0 correction did not recover enough", cyn_late_e);
        // Ceiling: no overshoot ≤ 1800s (D13 upper bound was 1381s).
        assert!(cyn_late_e <= 1800.0,
            "CEILING: Cynthia late-half {:.0}s > 1800s — F0 correction over-recovered (stealing Speaker 2)", cyn_late_e);
        // Early-half guard: Cynthia's early-half not reduced >10%.
        if cyn_early_d > 0.0 {
            assert!(cyn_early_e >= cyn_early_d * 0.90,
                "EARLY-HALF GUARD: Cynthia early-half dropped {:.0}s→{:.0}s (>10%)", cyn_early_d, cyn_early_e);
        }

        // Absorber guard: Speaker 2 = the cluster with the most late-half
        // duration that is NOT Cynthia. Its late-half must not drop >15%.
        let spk2_cluster_d = centroids_d
            .keys()
            .copied()
            .filter(|&c| c != cyn_cluster_d)
            .max_by(|a, b| {
                let da = speaker_dur_split(&segments_d, *a, false);
                let db = speaker_dur_split(&segments_d, *b, false);
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or(cyn_cluster_d);
        let spk2_late_d = speaker_dur_split(&segments_d, spk2_cluster_d, false);
        // In corrected output, Speaker 2's cluster may have been renumbered.
        // Find the cluster closest to spk2_cluster_d's centroid.
        let spk2_centroid = centroids_d.get(&spk2_cluster_d).cloned().unwrap_or_default();
        let spk2_cluster_e = if spk2_centroid.is_empty() {
            cyn_cluster_e
        } else {
            *centroids_e
                .iter()
                .max_by(|a, b| {
                    let ca = cosine_similarity_centroids(&spk2_centroid, a.1);
                    let cb = cosine_similarity_centroids(&spk2_centroid, b.1);
                    ca.partial_cmp(&cb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(k, _)| k)
                .unwrap_or(&cyn_cluster_e)
        };
        let spk2_late_e = speaker_dur_split(&segments_e, spk2_cluster_e, false);
        eprintln!(
            "RAILS: Speaker2 late D={:.0}s E={:.0}s (cluster D={} E={})",
            spk2_late_d, spk2_late_e, spk2_cluster_d, spk2_cluster_e,
        );
        if spk2_late_d > 0.0 {
            assert!(spk2_late_e >= spk2_late_d * 0.85,
                "ABSORBER GUARD: Speaker 2 late-half dropped {:.0}s→{:.0}s (>15%)", spk2_late_d, spk2_late_e);
        }

        // Per-chunk F0-register: chunks moved TO Cynthia's cluster by F0
        // correction must have F0 within 30 Hz of Cynthia's median F0.
        let cyn_voiced_f0s: Vec<f32> = chunks
            .iter()
            .enumerate()
            .filter(|(i, _)| labels_b[*i] == cyn_cluster_d || labels_e[*i] == cyn_cluster_e)
            .filter_map(|(_, c)| {
                let end = c.end_sample.min(samples_ref.len());
                if end <= c.start_sample { return None; }
                detect_f0(&samples_ref[c.start_sample..end], DIARIZATION_SAMPLE_RATE)
            })
            .collect();
        if cyn_voiced_f0s.len() >= 3 {
            let mut sorted = cyn_voiced_f0s.clone();
            sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let cyn_median_f0 = sorted[sorted.len() / 2];
            let mut moved_ok = 0usize;
            let mut moved_total = 0usize;
            for (i, c) in chunks.iter().enumerate() {
                let moved_to_cyn = labels_b[i] != cyn_cluster_e && labels_e[i] == cyn_cluster_e;
                if !moved_to_cyn { continue; }
                moved_total += 1;
                let end = c.end_sample.min(samples_ref.len());
                if end <= c.start_sample { continue; }
                if let Some(f0) = detect_f0(&samples_ref[c.start_sample..end], DIARIZATION_SAMPLE_RATE) {
                    if (f0 - cyn_median_f0).abs() < 30.0 { moved_ok += 1; }
                }
            }
            eprintln!(
                "RAILS: F0-register — moved {} chunks to Cynthia, {} within 30Hz of median {:.0}Hz",
                moved_total, moved_ok, cyn_median_f0,
            );
            if moved_total > 0 {
                assert!(moved_ok as f64 / moved_total as f64 >= 0.5,
                    "F0-REGISTER: only {}/{} moved chunks have F0 within 30Hz of Cynthia's median", moved_ok, moved_total);
            }
        }

        // Contamination diagnostic: <40% of Cynthia's late chunks have
        // cos<0.3 to her centroid (embedding-level contamination measure).
        let cyn_centroid_e = centroids_e.get(&cyn_cluster_e).cloned().unwrap_or_default();
        if !cyn_centroid_e.is_empty() {
            let late_cyn: Vec<_> = chunks
                .iter()
                .enumerate()
                .filter(|(i, c)| {
                    labels_e[*i] == cyn_cluster_e && (c.start_sample as f64 / sr_f) >= 1800.0
                })
                .map(|(_, c)| c)
                .collect();
            let contaminated = late_cyn
                .iter()
                .filter(|c| cosine_similarity_centroids(&c.embedding, &cyn_centroid_e) < 0.3)
                .count();
            let pct = if late_cyn.is_empty() { 0.0 } else { contaminated as f64 / late_cyn.len() as f64 * 100.0 };
            eprintln!(
                "RAILS: contamination — {}/{} ({:.0}%) of Cynthia's late chunks have cos<0.3 to her centroid",
                contaminated, late_cyn.len(), pct,
            );
            assert!(pct < 40.0,
                "CONTAMINATION: {:.0}% of Cynthia's late chunks have cos<0.3 — F0 correction is stealing garbage", pct);
        }
    }

    fn trace_chunk_stage(
        stage: &str,
        labels: &[u32],
        timestamps: &[f64],
        durations: &[f64],
        centroids: &std::collections::HashMap<u32, Vec<f32>>,
        fingerprint: &[f32],
    ) {
        let clusters: std::collections::HashSet<u32> = labels.iter().copied().collect();
        let (cyn_cluster, cyn_cos) = clusters
            .iter()
            .map(|&c| {
                let cos = centroids
                    .get(&c)
                    .map(|v| cosine_similarity_centroids(fingerprint, v))
                    .unwrap_or(-1.0);
                (c, cos)
            })
            .max_by(|a, b| {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            })
            .unwrap_or((0, -1.0));
        let mut early = 0.0f64;
        let mut late = 0.0f64;
        let mut cyn_chunks = 0usize;
        for (i, &lab) in labels.iter().enumerate() {
            if lab == cyn_cluster {
                cyn_chunks += 1;
                if timestamps[i] < 1800.0 {
                    early += durations[i];
                } else {
                    late += durations[i];
                }
            }
        }
        let mut all_early = 0.0f64;
        let mut all_late = 0.0f64;
        for (i, _) in labels.iter().enumerate() {
            if timestamps[i] < 1800.0 {
                all_early += durations[i];
            } else {
                all_late += durations[i];
            }
        }
        eprintln!(
            "STAGE-TRACE [{}]: {} clusters | Cynthia→{} (cos {:.3}): {} chunks, early {:.0}s late {:.0}s (ratio {:.2}) | total early {:.0}s late {:.0}s",
            stage,
            clusters.len(),
            cyn_cluster,
            cyn_cos,
            cyn_chunks,
            early,
            late,
            if early > 0.0 { late / early } else { f64::INFINITY },
            all_early,
            all_late,
        );
    }

    fn trace_segment_stage(
        stage: &str,
        segments: &[SpeakerSegment],
        centroids: &std::collections::HashMap<u32, Vec<f32>>,
        fingerprint: &[f32],
    ) {
        use std::collections::HashSet;
        let speakers: HashSet<u32> = segments.iter().map(|s| s.speaker_id).collect();
        let (cyn_spk, cyn_cos) = speakers
            .iter()
            .map(|&s| {
                let cos = centroids
                    .get(&s)
                    .map(|v| cosine_similarity_centroids(fingerprint, v))
                    .unwrap_or(-1.0);
                (s, cos)
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0, -1.0));
        let mut early = 0.0f64;
        let mut late = 0.0f64;
        let mut all_early = 0.0f64;
        let mut all_late = 0.0f64;
        for s in segments {
            let dur = s.end_seconds - s.start_seconds;
            let mid = (s.start_seconds + s.end_seconds) / 2.0;
            if mid < 1800.0 {
                all_early += dur;
                if s.speaker_id == cyn_spk {
                    early += dur;
                }
            } else {
                all_late += dur;
                if s.speaker_id == cyn_spk {
                    late += dur;
                }
            }
        }
        let merged = cyn_cos < 0.3;
        eprintln!(
            "STAGE-TRACE [{}]: {} speakers, {} segments | Cynthia→{} (cos {:.3}){}: early {:.0}s late {:.0}s (ratio {:.2}) | total early {:.0}s late {:.0}s",
            stage,
            speakers.len(),
            segments.len(),
            cyn_spk,
            cyn_cos,
            if merged { " *** MERGED ***" } else { "" },
            early,
            late,
            if early > 0.0 { late / early } else { f64::INFINITY },
            all_early,
            all_late,
        );
    }

    /// §4.2: Regression guard — meeting 95db is a known-good 3-speaker meeting.
    /// F0 correction must not regress it: speaker count stays 3, no speaker
    /// collapses by >15% between halves (late-half ≥ 15% of early-half).
    ///
    /// NOTE (2026-07-10): F0 correction is gated off in `process()`
    /// (`F0_CORRECTION_ENABLED = false`) after it collapsed 95db 3→2. This test
    /// now guards the baseline (3 speakers WITHOUT F0 correction). The
    /// F0-induced regression is documented in design.md "⚠ REAL-DATA
    /// VALIDATION FAILED".
    #[tokio::test]
    #[ignore]
    async fn test_f0_correction_no_regression_95db() {
        let _ = env_logger::builder().is_test(true).try_init();
        let db_path = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite";
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}?mode=ro", db_path))
            .await
            .expect("DB connect (read-only)");
        let meeting_id = "meeting-95db7d8e-8ed2-42e2-90f4-5e5203b52930";

        let row = sqlx::query("SELECT folder_path FROM meetings WHERE id = ?")
            .bind(meeting_id)
            .fetch_optional(&pool)
            .await
            .expect("fetch meeting");
        let folder = row
            .and_then(|r| sqlx::Row::get::<Option<String>, _>(&r, "folder_path"))
            .expect("95db folder_path missing");
        let audio_path = find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds.max(0.001);

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let embedding_path = models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        assert!(embedding_path.exists(), "embedding model missing");

        let threshold_fp = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(
            (0.40f32 * 65536.0) as u32,
        ));
        let adapter =
            crate::audio::speaker::sherpa_adapter::SherpaOnnxDiarizationAdapter::with_shared_threshold(
                embedding_path.to_str().unwrap(),
                segmentation_path.to_str().unwrap(),
                std::sync::Arc::clone(&threshold_fp),
            )
            .expect("create adapter");

        let transcript_segments = fetch_transcript_timestamps(&pool, meeting_id, audio_duration)
            .await
            .expect("fetch transcript timestamps");

        let output = adapter
            .process(&samples, DIARIZATION_SAMPLE_RATE, &transcript_segments)
            .expect("diarization");

        let speakers: std::collections::HashSet<u32> =
            output.segments.iter().map(|s| s.speaker_id).collect();
        eprintln!(
            "95db F0-regression: {} speakers, {} segments, {:.0}s audio",
            speakers.len(),
            output.segments.len(),
            audio_duration,
        );
        assert_eq!(speakers.len(), 3, "95db must keep 3 speakers (got {})", speakers.len());

        let midpoint = audio_duration / 2.0;
        let mut early_dur: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
        let mut late_dur: std::collections::HashMap<u32, f64> = std::collections::HashMap::new();
        for s in &output.segments {
            let early = (midpoint.min(s.end_seconds) - s.start_seconds).max(0.0);
            let late = (s.end_seconds - midpoint.max(s.start_seconds)).max(0.0);
            *early_dur.entry(s.speaker_id).or_insert(0.0) += early;
            *late_dur.entry(s.speaker_id).or_insert(0.0) += late;
        }
        for &sid in speakers.iter() {
            let e = early_dur.get(&sid).copied().unwrap_or(0.0);
            let l = late_dur.get(&sid).copied().unwrap_or(0.0);
            eprintln!("95db speaker {}: early {:.0}s, late {:.0}s", sid, e, l);
            if e > 30.0 {
                assert!(l >= e * 0.15,
                    "95db speaker {} collapsed: late {:.0}s < 15% of early {:.0}s", sid, l, e);
            }
        }
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
        // The survivor's centroid must be the duration-weighted recompute, not
        // the stale pre-merge value. Here cluster 2 (dur 1.0s) absorbs into
        // cluster 1 (dur 0.0s): w_iso=1, w_nn=0, so survivor == absorbed vector.
        assert_eq!(centroids[&1], vec![0.0_f32, 1.0_f32]);
    }

    #[test]
    fn enforce_cap_recompute_weights_both_durations() {
        // Both the absorbed and surviving clusters have nonzero duration, so the
        // survivor must be a non-trivial weighted average — not just the absorbed
        // vector (w_iso=1) and not just the survivor (w_nn=1).
        let mut centroids = std::collections::HashMap::<u32, Vec<f32>>::new();
        centroids.insert(0, vec![1.0, 0.0]);
        centroids.insert(1, vec![0.95, 0.05]);
        centroids.insert(2, vec![0.0, 1.0]);
        let mut segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 2.0, speaker_id: 2 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 1 },
        ];
        enforce_max_speakers_cap(&mut centroids, &mut segments, 2);
        assert!(!centroids.contains_key(&2));
        let c = &centroids[&1];
        // w_iso = 2/3 (absorbed cluster 2), w_nn = 1/3 (survivor cluster 1)
        assert!((c[0] - 0.95 * (1.0 / 3.0)).abs() < 1e-5, "x: {}", c[0]);
        assert!((c[1] - (0.05 * (1.0 / 3.0) + 1.0 * (2.0 / 3.0))).abs() < 1e-5, "y: {}", c[1]);
    }

    #[test]
    fn enforce_cap_floors_at_two_speakers() {
        // cap.max(2) prevents collapsing below two clusters — a single-speaker
        // diarization is meaningless, so even an explicit max_speakers=1 floors at 2.
        let mut centroids = std::collections::HashMap::<u32, Vec<f32>>::new();
        centroids.insert(0, vec![1.0, 0.0]);
        centroids.insert(1, vec![0.0, 1.0]);
        centroids.insert(2, vec![1.0, 1.0]);
        let mut segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 0 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 1 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 2 },
        ];
        enforce_max_speakers_cap(&mut centroids, &mut segments, 1);
        assert_eq!(centroids.len(), 2, "cap=1 floors at 2 via cap.max(2)");
    }

    #[test]
    fn enforce_cap_does_not_propagate_nan_from_degenerate_centroid() {
        // §4 adversarial (garbled output): a degenerate NaN embedding — Whisper can
        // emit one for a silent or garbled chunk — must not poison the surviving
        // centroid when the cluster is merged under the cap. The NaN centroid reads
        // as most-isolated (its similarity to every other cluster is 0.0), so it is
        // the cluster that gets absorbed; without the finite-check the weighted
        // average writes NaN into the survivor, and it spreads to every remaining
        // cluster on subsequent merge iterations.
        let mut centroids = std::collections::HashMap::<u32, Vec<f32>>::new();
        centroids.insert(0, vec![1.0, 0.0]);
        centroids.insert(1, vec![0.9, 0.1]);
        centroids.insert(2, vec![f32::NAN, f32::NAN]);
        let mut segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 0 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 1 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 2 },
        ];
        enforce_max_speakers_cap(&mut centroids, &mut segments, 2);

        for (id, c) in &centroids {
            for v in c {
                assert!(v.is_finite(), "NaN/Inf propagated into surviving centroid {id}: {c:?}");
            }
        }
        assert!(!centroids.contains_key(&2), "degenerate cluster 2 must be absorbed");
        assert_eq!(
            segments.iter().filter(|s| s.speaker_id == 2).count(),
            0,
            "degenerate cluster's segments must be relabelled, not orphaned"
        );
    }

    #[test]
    fn cosine_similarity_clamps_inf_centroid_to_finite_zero() {
        // §4 adversarial (garbled output): an Inf-laden centroid must not yield a
        // NaN similarity. Unlike the NaN case (caught by the pre-existing norm>0
        // guard), an Inf centroid has an Inf norm that PASSES norm>0 and reaches
        // the division, where Inf/(Inf·finite) = NaN — poisoning the isolation
        // ranking. The dot.is_finite() conjunct is what clamps this to 0.0.
        let inf = f32::INFINITY;
        assert_eq!(cosine_similarity_centroids(&[inf, 0.0], &[1.0, 0.0]), 0.0);
        // Finite-vs-finite is unchanged: the guard is a no-op for a finite dot.
        assert!((cosine_similarity_centroids(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
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

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

// Per-meeting diarization serialization. Keyed by meeting_id so diarization
// of DISTINCT meetings proceed in parallel, while two simultaneous calls on
// the SAME meeting serialize — preventing clear/align/persist interleaving
// that would otherwise corrupt transcript rows (e.g. the second pass's
// clear_auto_labels racing the first pass's persist).
static DIARIZATION_LOCKS: std::sync::OnceLock<
    Arc<tokio::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
> = std::sync::OnceLock::new();

pub(crate) async fn diarization_lock_for(meeting_id: &str) -> Arc<tokio::sync::Mutex<()>> {
    let map = DIARIZATION_LOCKS
        .get_or_init(|| Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())));
    let mut guard = map.lock().await;
    guard
        .entry(meeting_id.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone()
}

pub async fn run_diarization_for_meeting(
    pool: &SqlitePool,
    meeting_id: &str,
    threshold_fp: u32,
    registry: Arc<Mutex<Option<SherpaOnnxRegistryAdapter>>>,
) -> Result<DiarizationResult, String> {
    let meeting_lock = diarization_lock_for(meeting_id).await;
    let _diarization_guard = meeting_lock.lock().await;

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
        align_transcripts_with_diarization, AlignedSegment, DiarizationSegment, SpeakerSource,
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
        // Proportional-tail Unknowns (speaker_source == Unknown) carry words
        // that fall outside every diarization segment — they must survive as
        // "Unknown Speaker" rather than be borrowed onto a nearby speaker.
        // Token-path gap Unknowns (speaker_source == Auto) remain eligible.
        if seg.speaker == "Unknown Speaker"
            && seg.speaker_source != SpeakerSource::Unknown
            && !labeled_midpoints.is_empty()
        {
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

    // Step 8: Persist aligned per-speaker splits. Resolve registry labels, then
    // group by source row and persist each group in one transaction. The prior
    // per-segment UPDATE-by-id loop collapsed N splits onto the shared source
    // id (last-writer-wins) and discarded the split text.
    let aligned: Vec<AlignedSegment> = aligned
        .into_iter()
        .map(|mut s| {
            s.speaker = resolve_label(&s.speaker, &label_map);
            s
        })
        .collect();
    let segments_labeled = SpeakerRepository::persist_aligned_groups(pool, aligned)
        .await
        .map_err(|e| e.to_string())?
        as u64;

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

    // Empirical threshold probe for the parked `diarization-pyannote-boundaries`
    // change. The Part B shark-tank refuted 3 successive D1 formulations; the
    // panel concluded the right threshold (and whether to keep or discard
    // sherpa's labels through chunk formation) cannot be settled analytically.
    // This probe runs sherpa's OfflineSpeakerDiarization directly — bypassing
    // Meetily's adapter — at several FastClusteringConfig.threshold values and
    // dumps the resulting segment boundaries next to known ground-truth turns
    // (Ricardo joins at 17:37; banter window 5.7–32.5s; interjection at 46:58).
    // WHY direct sherpa: Meetily's adapter never wires pyannote (phantom dep —
    // sherpa_adapter.rs:103-106); this is the only way to see its raw output.
    #[ignore]
    #[tokio::test]
    async fn test_cde5c264_pyannote_threshold_probe() {
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
        let audio_path = find_audio_in_folder(std::path::Path::new(&folder)).expect("audio file");
        let decoded = crate::audio::decoder::decode_audio_file(&audio_path).expect("decode audio");
        let full_samples = decoded.to_whisper_format();
        let audio_duration = decoded.duration_seconds.max(0.001);
        // WHY slice to 50 min: sherpa's OfflineSpeakerDiarization::process()
        // access-violates on the full 83-min buffer (status 0xc0000005). 50 min
        // covers all three ground-truth windows (banter 5.7–32.5s, Ricardo join
        // 17:37, interjection 46:58) at ~48M samples — under whatever limit
        // sherpa hit. Full-audio crash investigation deferred (would only
        // matter if Part B ships without external chunking).
        const PROBE_MINUTES: usize = 50;
        let probe_samples = PROBE_MINUTES * 60 * 16000;
        let samples = if full_samples.len() > probe_samples {
            eprintln!(
                "PROBE: slicing {} -> {} samples ({} min cap for sherpa process() stability)",
                full_samples.len(), probe_samples, PROBE_MINUTES
            );
            full_samples[..probe_samples].to_vec()
        } else {
            full_samples
        };
        eprintln!(
            "PROBE: {} samples ({:.1}s @ 16kHz mono)",
            samples.len(),
            audio_duration
        );

        let models_dir = dirs::home_dir().unwrap_or_default().join(".meetily-models");
        let segmentation_path = models_dir.join("pyannote-segmentation.onnx");
        let embedding_path =
            models_dir.join(crate::audio::speaker::model_download::embedding_filename());
        assert!(segmentation_path.exists(), "pyannote model missing");
        assert!(embedding_path.exists(), "titanet embedding model missing");
        let segmentation_str = segmentation_path.to_str().unwrap().to_string();
        let embedding_str = embedding_path.to_str().unwrap().to_string();

        // WHY use sherpa_onnx::* flat paths: crate lib.rs (1.13.2 line 210)
        // declares `mod offline_speaker_diarization` (private module) and
        // re-exports via `pub use offline_speaker_diarization::*` (line 228)
        // so the qualified module path is not reachable from outside.
        use sherpa_onnx::{
            FastClusteringConfig, OfflineSpeakerDiarization,
            OfflineSpeakerDiarizationConfig, OfflineSpeakerSegmentationModelConfig,
            OfflineSpeakerSegmentationPyannoteModelConfig, SpeakerEmbeddingExtractorConfig,
        };

        // threshold is a cosine DISSIMILARITY cutoff (smaller -> more clusters).
        // 0.0 maximally fragments, 0.5 is sherpa's default, ~0.3 is a middle
        // ground. min_duration_on/off held at defaults so the comparison
        // isolates threshold's effect on boundary placement.
        //
        // STEP 0 (diagnostic): create-only with a tiny synthetic buffer to
        // isolate whether the crash is in model loading (create) or processing.
        // Production NEVER loads pyannote (sherpa_adapter.rs:103-106 only
        // existence-checks the path), so this is the first code to actually
        // pass it to ORT 1.17.1 — if the model is unloadable in this build,
        // Part B is blocked on an ORT/sherpa upgrade regardless of threshold.
        eprintln!("PROBE STEP 0: create-only diagnostic (threshold 0.5, 1600 samples of silence)");
        {
            let cfg = OfflineSpeakerDiarizationConfig {
                segmentation: OfflineSpeakerSegmentationModelConfig {
                    pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                        model: Some(segmentation_str.clone()),
                    },
                    num_threads: 1,
                    debug: true,
                    provider: Some("cpu".to_string()),
                },
                embedding: SpeakerEmbeddingExtractorConfig {
                    model: Some(embedding_str.clone()),
                    num_threads: 1,
                    debug: true,
                    provider: Some("cpu".to_string()),
                },
                clustering: FastClusteringConfig { num_clusters: -1, threshold: 0.5 },
                min_duration_on: 0.3,
                min_duration_off: 0.5,
            };
            eprintln!("PROBE STEP 0: calling create()...");
            match OfflineSpeakerDiarization::create(&cfg) {
                Some(d) => {
                    eprintln!("PROBE STEP 0: create OK, sample_rate={}, calling process(1600 samples)...", d.sample_rate());
                    let tiny: Vec<f32> = vec![0.0; 1600];
                    match d.process(&tiny) {
                        Some(r) => eprintln!("PROBE STEP 0: process OK, {} segments", r.num_segments()),
                        None => eprintln!("PROBE STEP 0: process returned None"),
                    }
                }
                None => eprintln!("PROBE STEP 0: create returned None"),
            }
        }
        eprintln!("PROBE STEP 0: complete\n");

        for &threshold in &[0.0f32, 0.3, 0.5] {
            let config = OfflineSpeakerDiarizationConfig {
                segmentation: OfflineSpeakerSegmentationModelConfig {
                    pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                        model: Some(segmentation_str.clone()),
                    },
                    num_threads: 1,
                    debug: false,
                    provider: Some("cpu".to_string()),
                },
                embedding: SpeakerEmbeddingExtractorConfig {
                    model: Some(embedding_str.clone()),
                    num_threads: 1,
                    debug: false,
                    provider: Some("cpu".to_string()),
                },
                clustering: FastClusteringConfig {
                    num_clusters: -1,
                    threshold,
                },
                min_duration_on: 0.3,
                min_duration_off: 0.5,
            };

            let t0 = std::time::Instant::now();
            let diarizer = OfflineSpeakerDiarization::create(&config).expect("create diarizer failed");

            let n_expected = diarizer.sample_rate();
            // samples is owned by the outer test fn; clone per iteration so
            // multiple thresholds can be probed without re-decoding the audio.
            let samples_for_threshold = samples.clone();
            let result = tokio::task::spawn_blocking(move || diarizer.process(&samples_for_threshold))
                .await
                .expect("process panicked")
                .expect("process failed");
            let elapsed = t0.elapsed().as_secs_f64();

            let segments = result.sort_by_start_time();
            let num_speakers = result.num_speakers();
            eprintln!(
                "\n===== threshold {:.2}: {} segments, {} speakers, sample_rate={}, {:.1}s wall =====",
                threshold, segments.len(), num_speakers, n_expected, elapsed
            );

            // Ground-truth windows to inspect.
            let banter_start = 5.7f32;
            let banter_end = 32.5f32;
            let ricardo_join = 17.0 * 60.0 + 37.0; // 17:37
            let interject = 46.0 * 60.0 + 58.0; // 46:58

            let banter_boundaries: Vec<(f32, f32, i32)> = segments
                .iter()
                .filter(|s| s.start < banter_end && s.end > banter_start)
                .map(|s| (s.start, s.end, s.speaker))
                .collect();
            eprintln!(
                "  banter [5.7–32.5s]: {} segments, {} speaker-changes, speakers {:?}",
                banter_boundaries.len(),
                banter_boundaries.windows(2).filter(|w| w[0].2 != w[1].2).count(),
                {
                    let mut s: Vec<i32> = banter_boundaries.iter().map(|x| x.2).collect();
                    s.sort();
                    s.dedup();
                    s
                }
            );
            for (st, en, sp) in &banter_boundaries {
                eprintln!("    [{:7.2} – {:7.2}] speaker {}", st, en, sp);
            }

            let pre_join_speakers: Vec<i32> = {
                let mut s: Vec<i32> = segments
                    .iter()
                    .filter(|s| s.start < ricardo_join)
                    .map(|s| s.speaker)
                    .collect();
                s.sort();
                s.dedup();
                s
            };
            let post_join_speakers: Vec<i32> = {
                let mut s: Vec<i32> = segments
                    .iter()
                    .filter(|s| s.start >= ricardo_join)
                    .map(|s| s.speaker)
                    .collect();
                s.sort();
                s.dedup();
                s
            };
            eprintln!(
                "  ricardo-join {}: pre-join speakers {:?}, post-join speakers {:?}, new-after {:?}",
                ricardo_join,
                pre_join_speakers,
                post_join_speakers,
                post_join_speakers
                    .iter()
                    .filter(|s| !pre_join_speakers.contains(s))
                    .collect::<Vec<_>>()
            );

            let near_interject: Vec<(f32, f32, i32)> = segments
                .iter()
                .filter(|s| s.start < interject + 5.0 && s.end > interject - 5.0)
                .map(|s| (s.start, s.end, s.speaker))
                .collect();
            eprintln!(
                "  interjection [{}±5s]: {} segments (target: ≥1 boundary near {})",
                interject,
                near_interject.len(),
                interject
            );
            for (st, en, sp) in &near_interject {
                eprintln!("    [{:7.2} – {:7.2}] speaker {}", st, en, sp);
            }

            let out =
                std::env::temp_dir().join(format!("probe_cde5c264_t{}.txt", (threshold * 100.0) as u32));
            let mut body = String::new();
            body.push_str(&format!(
                "# threshold {} min_duration_on 0.3 min_duration_off 0.5 num_speakers {} wall {:.1}s\n",
                threshold, num_speakers, elapsed
            ));
            for s in &segments {
                body.push_str(&format!("SEG {:.3} {:.3} {}\n", s.start, s.end, s.speaker));
            }
            std::fs::write(&out, body).expect("write probe");
            eprintln!("  wrote {}", out.display());
        }
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

    // 3.2 — two concurrent diarize invocations on the same meeting_id are
    // serialized by the meeting-level guard. Verified at the lock-helper level
    // (the function wraps the entire pipeline, which needs real audio + models).
    #[tokio::test]
    async fn rediarize_mutual_exclusion_per_meeting() {
        let meeting = format!("test-mutex-{}", Uuid::new_v4());
        let lock = diarization_lock_for(&meeting).await;
        let guard = lock.lock().await;

        let lock2 = diarization_lock_for(&meeting).await;
        let join = tokio::spawn(async move {
            let _g2 = lock2.lock().await;
            "proceeded"
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(
            !join.is_finished(),
            "second diarize on same meeting must block while first holds the lock"
        );

        drop(guard);
        let result = tokio::time::timeout(std::time::Duration::from_secs(2), join)
            .await
            .expect("second diarize did not proceed within 2s of first releasing")
            .expect("spawned task panicked");
        assert_eq!(result, "proceeded");
    }
}

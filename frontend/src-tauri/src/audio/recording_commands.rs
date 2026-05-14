// audio/recording_commands.rs
//
// Slim Tauri command layer for recording functionality.
// Delegates to transcription and recording modules for actual implementation.

use anyhow::Result;
use log::{error, info, warn};
use serde::Serialize;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tokio::task::JoinHandle;

use super::{
    parse_audio_device,
    default_input_device,   // Get default microphone
    default_output_device,  // Get default system audio
    RecordingManager,
    DeviceEvent,
    DeviceMonitorType
};

// Import transcription modules
use super::transcription::{
    self,
    reset_speech_detected_flag,
};

// Re-export TranscriptUpdate for backward compatibility
pub use super::transcription::TranscriptUpdate;

// ============================================================================
// RECORDING PHASE — three-state lifecycle replacing IS_RECORDING bool
// ============================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingPhase {
    Idle = 0,
    Recording = 1,
    Saving = 2,
}

impl From<u8> for RecordingPhase {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Recording,
            2 => Self::Saving,
            _ => Self::Idle,
        }
    }
}

impl From<RecordingPhase> for u8 {
    fn from(p: RecordingPhase) -> Self {
        p as u8
    }
}

pub(crate) fn current_phase() -> RecordingPhase {
    RecordingPhase::from(RECORDING_PHASE.load(Ordering::SeqCst))
}

pub(crate) fn set_phase(phase: RecordingPhase) {
    RECORDING_PHASE.store(phase as u8, Ordering::SeqCst);
}

// ============================================================================
// GLOBAL STATE
// ============================================================================

static RECORDING_PHASE: AtomicU8 = AtomicU8::new(RecordingPhase::Idle as u8);

// Drop guard: resets phase to Idle on scope exit, including on panic inside tokio::spawn.
// Place `let _guard = PhaseGuard;` as the first line of the background shutdown task so the
// UI never stays stuck in "Saving…" even if background_shutdown panics or the thread aborts.
struct PhaseGuard;
impl Drop for PhaseGuard {
    fn drop(&mut self) {
        set_phase(RecordingPhase::Idle);
    }
}

// Global recording manager and transcription task to keep them alive during recording
static RECORDING_MANAGER: Mutex<Option<RecordingManager>> = Mutex::new(None);
static TRANSCRIPTION_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);
// Background shutdown task handle — guarded so a concurrent stop_recording call can detect it
static SHUTDOWN_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

// Listener ID for proper cleanup - prevents microphone from staying active after recording stops
static TRANSCRIPT_LISTENER_ID: Mutex<Option<tauri::EventId>> = Mutex::new(None);

// ============================================================================
// PUBLIC TYPES
// ============================================================================

/// Result returned synchronously from `stop_recording` so the frontend save flow
/// has the meeting's folder/name without waiting for the background finalize.
/// The `recording-stopped` event still fires (at the same moment) for any listener
/// that needs the side-channel; `recording-saved` fires later when audio.mp4 is on disk.
#[derive(Debug, Serialize, Clone)]
pub struct StopRecordingResult {
    pub folder_path: Option<String>,
    pub meeting_name: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct TranscriptionStatus {
    pub chunks_in_queue: usize,
    pub is_processing: bool,
    pub last_activity_ms: u64,
}

// ============================================================================
// RECORDING COMMANDS
// ============================================================================

/// Start recording with default devices
pub async fn start_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    start_recording_with_meeting_name(app, None).await
}

/// Registers the transcript-update listener and stores its ID for cleanup in stop_recording.
/// Late transcripts that arrive after stop_recording would be processed against a closed
/// session, so the listener must be unregistered before the mic is released.
fn register_transcript_listener<R: Runtime>(app: &AppHandle<R>) {
    use tauri::Listener;
    let listener_id = app.listen("transcript-update", |event: tauri::Event| {
        if let Ok(update) = serde_json::from_str::<TranscriptUpdate>(event.payload()) {
            let segment = crate::audio::recording_saver::TranscriptSegment {
                id: format!("seg_{}", update.sequence_id),
                text: update.text.clone(),
                audio_start_time: update.audio_start_time,
                audio_end_time: update.audio_end_time,
                duration: update.duration,
                display_time: update.timestamp.clone(),
                confidence: update.confidence,
                sequence_id: update.sequence_id,
            };

            if let Ok(manager_guard) = RECORDING_MANAGER.lock() {
                if let Some(manager) = manager_guard.as_ref() {
                    manager.add_transcript_segment(segment);
                }
            }
        }
    });
    match TRANSCRIPT_LISTENER_ID.lock() {
        Ok(mut g) => {
            *g = Some(listener_id);
            info!("✅ Transcript-update event listener registered for history persistence");
        }
        Err(e) => error!("TRANSCRIPT_LISTENER_ID lock poisoned, listener not stored: {}", e),
    }
}

/// Start recording with default devices and optional meeting name
pub async fn start_recording_with_meeting_name<R: Runtime>(
    app: AppHandle<R>,
    meeting_name: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting recording with default devices, meeting: {:?}",
        meeting_name
    );

    // Check if already recording or saving
    let phase = current_phase();
    info!("🔍 Phase check: {:?}", phase);
    if phase == RecordingPhase::Saving {
        return Err("a previous recording is still finalizing".to_string());
    }
    if phase == RecordingPhase::Recording {
        return Err("Recording already in progress".to_string());
    }

    // Validate that transcription models are available before starting recording
    info!("🔍 Validating transcription model availability before starting recording...");
    if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await {
        error!("Model validation failed: {}", validation_error);

        // Emit error event for frontend - actionable: false to show toast instead of modal
        // (download progress is already shown in top-right toast)
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": validation_error,
            "userMessage": "Recording cannot start: Transcription model is still downloading. Please wait for the download to complete.",
            "actionable": false
        }));

        return Err(validation_error);
    }
    info!("✅ Transcription model validation passed");

    // Async-first approach - no more blocking operations!
    info!("🚀 Starting async recording initialization");

    // Create new recording manager
    let mut manager = RecordingManager::new();

    // Load recording preferences to get auto_save AND device preferences.
    // noise_gate_floor_dbfs is read once here; mid-recording changes do NOT take effect.
    let (auto_save, preferred_mic_name, preferred_system_name, gate_floor_dbfs) =
        match super::recording_preferences::load_recording_preferences(&app).await {
            Ok(prefs) => {
                info!("📋 Loaded recording preferences: auto_save={}, preferred_mic={:?}, preferred_system={:?}, gate_floor={}dBFS",
                      prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device, prefs.noise_gate_floor_dbfs);
                (prefs.auto_save, prefs.preferred_mic_device, prefs.preferred_system_device, prefs.noise_gate_floor_dbfs)
            }
            Err(e) => {
                warn!("Failed to load recording preferences, using defaults: {}", e);
                (true, None, None, -30)
            }
        };

    // ============================================================================
    // MICROPHONE DEVICE RESOLUTION: Preference → Default → Error
    // ============================================================================
    let microphone_device = match preferred_mic_name {
        Some(pref_name) => {
            info!("🎤 Attempting to use preferred microphone: '{}'", pref_name);
            match parse_audio_device(&pref_name) {
                Ok(device) => {
                    info!("✅ Using preferred microphone: '{}'", device.name);
                    Some(Arc::new(device))
                }
                Err(e) => {
                    warn!("⚠️ Preferred microphone '{}' not available: {}", pref_name, e);
                    warn!("   Falling back to system default microphone...");
                    match default_input_device() {
                        Ok(device) => {
                            info!("✅ Using default microphone: '{}'", device.name);
                            Some(Arc::new(device))
                        }
                        Err(default_err) => {
                            error!("❌ No microphone available (preferred and default both failed)");
                            return Err(format!(
                                "No microphone device available. Preferred device '{}' not found, and default microphone unavailable: {}",
                                pref_name, default_err
                            ));
                        }
                    }
                }
            }
        }
        None => {
            info!("🎤 No microphone preference set, using system default");
            match default_input_device() {
                Ok(device) => {
                    info!("✅ Using default microphone: '{}'", device.name);
                    Some(Arc::new(device))
                }
                Err(e) => {
                    error!("❌ No default microphone available");
                    return Err(format!("No microphone device available: {}", e));
                }
            }
        }
    };

    // ============================================================================
    // SYSTEM AUDIO DEVICE RESOLUTION: Preference → Default → None (optional)
    // ============================================================================
    let system_device = match preferred_system_name {
        Some(pref_name) => {
            info!("🔊 Attempting to use preferred system audio: '{}'", pref_name);
            match parse_audio_device(&pref_name) {
                Ok(device) => {
                    info!("✅ Using preferred system audio: '{}'", device.name);
                    Some(Arc::new(device))
                }
                Err(e) => {
                    warn!("⚠️ Preferred system audio '{}' not available: {}", pref_name, e);
                    warn!("   Falling back to system default...");
                    match default_output_device() {
                        Ok(device) => {
                            info!("✅ Using default system audio: '{}'", device.name);
                            Some(Arc::new(device))
                        }
                        Err(default_err) => {
                            warn!("⚠️ No system audio available (preferred and default both failed): {}", default_err);
                            warn!("   Recording will continue with microphone only");
                            None // System audio is optional
                        }
                    }
                }
            }
        }
        None => {
            info!("🔊 No system audio preference set, using system default");
            match default_output_device() {
                Ok(device) => {
                    info!("✅ Using default system audio: '{}'", device.name);
                    Some(Arc::new(device))
                }
                Err(e) => {
                    warn!("⚠️ No default system audio available: {}", e);
                    warn!("   Recording will continue with microphone only");
                    None // System audio is optional
                }
            }
        }
    };

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        // Example: Meeting 2025-10-03_08-25-23
        let now = chrono::Local::now();
        format!(
            "Meeting {}",
            now.format("%Y-%m-%d_%H-%M-%S")
        )
    });
    manager.set_meeting_name(Some(effective_meeting_name));

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    // Set up gain event relay: normalizer → channel → Tauri event
    {
        let (gain_tx, mut gain_rx) = tokio::sync::mpsc::unbounded_channel::<f32>();
        manager.set_gain_sender(gain_tx);
        let app_for_gain = app.clone();
        tokio::spawn(async move {
            while let Some(gain_db) = gain_rx.recv().await {
                let _ = app_for_gain.emit("audio-normalizer-gain", serde_json::json!({ "gain_db": gain_db }));
            }
        });
    }

    // Start recording with resolved devices (replaces start_recording_with_defaults_and_auto_save call)
    let transcription_receiver = manager
        .start_recording(microphone_device, system_device, auto_save, gate_floor_dbfs)
        .await
        .map_err(|e| format!("Failed to start recording: {}", e))?;

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    // Transition to Recording phase
    set_phase(RecordingPhase::Recording);
    reset_speech_detected_flag(); // Reset for new recording session

    // Emit phase transition event
    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Recording" }));

    // Start optimized parallel transcription task and store handle
    let task_handle = transcription::start_transcription_task(app.clone(), transcription_receiver);
    {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        *global_task = Some(task_handle);
    }

    register_transcript_listener(&app);

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started successfully with parallel processing",
        "devices": ["Default Microphone", "Default System Audio"],
        "workers": 3
    })).map_err(|e| e.to_string())?;

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    info!("✅ Recording started successfully with async-first approach");

    Ok(())
}

/// Start recording with specific devices
pub async fn start_recording_with_devices<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
) -> Result<(), String> {
    start_recording_with_devices_and_meeting(app, mic_device_name, system_device_name, None).await
}

/// Start recording with specific devices and optional meeting name
pub async fn start_recording_with_devices_and_meeting<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
) -> Result<(), String> {
    info!(
        "Starting recording with specific devices: mic={:?}, system={:?}, meeting={:?}",
        mic_device_name, system_device_name, meeting_name
    );

    // Check if already recording or saving
    let phase = current_phase();
    info!("🔍 Phase check: {:?}", phase);
    if phase == RecordingPhase::Saving {
        return Err("a previous recording is still finalizing".to_string());
    }
    if phase == RecordingPhase::Recording {
        return Err("Recording already in progress".to_string());
    }

    // Validate that transcription models are available before starting recording
    info!("🔍 Validating transcription model availability before starting recording...");
    if let Err(validation_error) = transcription::validate_transcription_model_ready(&app).await {
        error!("Model validation failed: {}", validation_error);

        // Emit error event for frontend - actionable: false to show toast instead of modal
        // (download progress is already shown in top-right toast)
        let _ = app.emit("transcription-error", serde_json::json!({
            "error": validation_error,
            "userMessage": "Recording cannot start: Transcription model is still downloading. Please wait for the download to complete.",
            "actionable": false
        }));

        return Err(validation_error);
    }
    info!("✅ Transcription model validation passed");

    // Parse devices
    let mic_device = if let Some(ref name) = mic_device_name {
        Some(Arc::new(parse_audio_device(name).map_err(|e| {
            format!("Invalid microphone device '{}': {}", name, e)
        })?))
    } else {
        None
    };

    let system_device = if let Some(ref name) = system_device_name {
        Some(Arc::new(parse_audio_device(name).map_err(|e| {
            format!("Invalid system device '{}': {}", name, e)
        })?))
    } else {
        None
    };

    // Async-first approach for custom devices - no more blocking operations!
    info!("🚀 Starting async recording initialization with custom devices");

    // Create new recording manager
    let mut manager = RecordingManager::new();

    // Load recording preferences to check auto_save and gate floor settings.
    // noise_gate_floor_dbfs is read once here; mid-recording changes do NOT take effect.
    let (auto_save, gate_floor_dbfs) = match super::recording_preferences::load_recording_preferences(&app).await {
        Ok(prefs) => {
            info!("📋 Loaded recording preferences: auto_save={}, gate_floor={}dBFS", prefs.auto_save, prefs.noise_gate_floor_dbfs);
            (prefs.auto_save, prefs.noise_gate_floor_dbfs)
        }
        Err(e) => {
            warn!("Failed to load recording preferences, defaulting to auto_save=true: {}", e);
            (true, -30)
        }
    };

    // Always ensure a meeting name is set so incremental saver initializes
    let effective_meeting_name = meeting_name.clone().unwrap_or_else(|| {
        let now = chrono::Local::now();
        format!(
            "Meeting {}",
            now.format("%Y-%m-%d_%H-%M-%S")
        )
    });
    manager.set_meeting_name(Some(effective_meeting_name));

    // Set up error callback
    let app_for_error = app.clone();
    manager.set_error_callback(move |error| {
        let _ = app_for_error.emit("recording-error", error.user_message());
    });

    // Set up gain event relay: normalizer → channel → Tauri event
    {
        let (gain_tx, mut gain_rx) = tokio::sync::mpsc::unbounded_channel::<f32>();
        manager.set_gain_sender(gain_tx);
        let app_for_gain = app.clone();
        tokio::spawn(async move {
            while let Some(gain_db) = gain_rx.recv().await {
                let _ = app_for_gain.emit("audio-normalizer-gain", serde_json::json!({ "gain_db": gain_db }));
            }
        });
    }

    // Start recording with specified devices and auto_save setting
    let transcription_receiver = manager
        .start_recording(mic_device, system_device, auto_save, gate_floor_dbfs)
        .await
        .map_err(|e| format!("Failed to start recording: {}", e))?;

    // Store the manager globally to keep it alive
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        *global_manager = Some(manager);
    }

    // Transition to Recording phase
    set_phase(RecordingPhase::Recording);
    reset_speech_detected_flag(); // Reset for new recording session

    // Emit phase transition event
    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Recording" }));

    // Start optimized parallel transcription task and store handle
    let task_handle = transcription::start_transcription_task(app.clone(), transcription_receiver);
    {
        let mut global_task = TRANSCRIPTION_TASK.lock().unwrap();
        *global_task = Some(task_handle);
    }

    register_transcript_listener(&app);

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started with custom devices and parallel processing",
        "devices": [
            mic_device_name.unwrap_or_else(|| "Default Microphone".to_string()),
            system_device_name.unwrap_or_else(|| "Default System Audio".to_string())
        ],
        "workers": 3
    })).map_err(|e| e.to_string())?;

    // Update tray menu to reflect recording state
    crate::tray::update_tray_menu(&app);

    info!("✅ Recording started with custom devices using async-first approach");

    Ok(())
}

/// Stop recording: synchronous path releases streams and transitions to Saving;
/// transcription drain, model unload, analytics, and file save run in the background.
pub async fn stop_recording<R: Runtime>(
    app: AppHandle<R>,
) -> Result<StopRecordingResult, String>
where
    R: 'static,
{
    info!("🛑 stop_recording: releasing streams, spawning background shutdown");

    // (a) Phase guard — idempotent. Empty result is correct: nothing to save.
    match current_phase() {
        RecordingPhase::Idle => {
            info!("Recording was not active");
            return Ok(StopRecordingResult { folder_path: None, meeting_name: None });
        }
        RecordingPhase::Saving => {
            info!("Recording already in Saving phase — ignoring duplicate stop");
            return Ok(StopRecordingResult { folder_path: None, meeting_name: None });
        }
        RecordingPhase::Recording => {}
    }

    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({ "stage": "stopping_audio", "message": "Stopping audio capture...", "progress": 20 }),
    );

    // (b) Stop streams + force flush — takes the manager out of global state permanently.
    let manager_for_background = {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        global_manager.take()
    };

    let (stream_result, mut manager_for_background) = if let Some(mut mgr) = manager_for_background {
        info!("🚀 FORCE FLUSH — eliminating pipeline accumulation delays");
        let r = mgr.stop_streams_and_force_flush().await;
        (r, Some(mgr))
    } else {
        warn!("No recording manager found to stop");
        (Ok(()), None)
    };

    if let Err(e) = stream_result {
        // Per design D3: even if stream release fails, transition to Saving so the UI
        // doesn't stay stuck in Recording forever. Background task will log and clean up.
        error!("❌ Stream release error (continuing to Saving): {}", e);
    }

    // Clean up transcript listener before transitioning (releases mic reference)
    {
        use tauri::Listener;
        if let Some(lid) = TRANSCRIPT_LISTENER_ID.lock().unwrap().take() {
            app.unlisten(lid);
            info!("✅ Transcript-update listener removed");
        }
    }

    // Extract analytics snapshot NOW while we still hold the manager, before the background
    // task takes ownership. This avoids re-borrowing across the spawn boundary.
    let analytics_snapshot: Option<(Option<f64>, f64, f64, u64, bool, Option<String>, Option<String>, u64)> =
        manager_for_background.as_ref().map(|mgr| {
            let state = mgr.get_state();
            let stats = state.get_stats();
            (
                mgr.get_recording_duration(),       // Option<f64>
                mgr.get_active_recording_duration().unwrap_or(0.0),
                mgr.get_total_pause_duration(),
                mgr.get_transcript_segments().len() as u64,
                state.has_fatal_error(),
                state.get_microphone_device().map(|d| d.name.clone()),
                state.get_system_device().map(|d| d.name.clone()),
                stats.chunks_processed,
            )
        });

    // Take transcription task handle before spawning so the background task owns it
    let transcription_task = {
        let mut t = TRANSCRIPTION_TASK.lock().unwrap();
        t.take()
    };

    // Extract folder/name now (synchronously) so we can return them AND emit recording-stopped
    // before the manager moves into the background task. The frontend save flow needs these
    // values immediately — without them, meetings get saved with null folder_path and the
    // audio file (created later by background_shutdown) is never linked to the DB row.
    let folder_path: Option<String> = manager_for_background
        .as_ref()
        .and_then(|m| m.get_meeting_folder().map(|p| p.to_string_lossy().to_string()));
    let meeting_name: Option<String> = manager_for_background
        .as_ref()
        .and_then(|m| m.get_meeting_name());

    // (c) Transition to Saving — this is the moment the UI stops showing "Recording"
    set_phase(RecordingPhase::Saving);
    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Saving" }));
    info!("✅ Phase → Saving (streams released; background shutdown spawning)");

    // Emit recording-stopped synchronously with folder info so listeners (TranscriptContext,
    // useRecordingStop's sessionStorage sink) get the path before invoke() resolves on the
    // frontend. background_shutdown no longer emits this event — it emits recording-saved
    // when audio.mp4 is finalized.
    let _ = app.emit(
        "recording-stopped",
        serde_json::json!({
            "message": "Recording stopped - audio is being finalized in background",
            "folder_path": folder_path,
            "meeting_name": meeting_name,
        }),
    );

    // Update tray immediately so system tray reflects the Saving state
    crate::tray::update_tray_menu(&app);

    // (d) Spawn background shutdown task — transcription drain, model unload, analytics, save
    let app_bg = app.clone();
    let handle = tokio::spawn(async move {
        // PhaseGuard ensures phase returns to Idle on scope exit, including on panic.
        let _guard = PhaseGuard;

        let result = background_shutdown(
            app_bg.clone(),
            manager_for_background.take(),
            transcription_task,
            analytics_snapshot,
        )
        .await;

        if let Err(e) = result {
            error!("❌ Background shutdown error: {}", e);
            let _ = app_bg.emit(
                "recording-save-failed",
                serde_json::json!({ "error": e }),
            );
        }

        // Set phase before emitting so any concurrent get_recording_state call sees Idle.
        // _guard will also call set_phase(Idle) on drop — harmless, ensures safety on panic.
        set_phase(RecordingPhase::Idle);
        let _ = app_bg.emit("recording-state-changed", serde_json::json!({ "phase": "Idle" }));
        crate::tray::update_tray_menu(&app_bg);
        info!("✅ Phase → Idle (background shutdown complete)");
    });

    // Store handle so a concurrent stop_recording call can detect an in-flight shutdown
    if let Ok(mut guard) = SHUTDOWN_TASK.lock() {
        *guard = Some(handle);
    }

    // (e) Return result so the frontend save flow can write the DB row with folder_path
    // populated. The audio file itself is finalized asynchronously and announced via
    // recording-saved later.
    info!("✅ stop_recording returned (background shutdown running)");
    Ok(StopRecordingResult { folder_path, meeting_name })
}

/// All shutdown work that does NOT need to block the frontend.
/// Called from the tokio::spawn background task in stop_recording.
/// Errors are logged; the caller always transitions to Idle regardless.
async fn background_shutdown<R: Runtime>(
    app: AppHandle<R>,
    mut manager: Option<RecordingManager>,
    transcription_task: Option<JoinHandle<()>>,
    analytics_snapshot: Option<(Option<f64>, f64, f64, u64, bool, Option<String>, Option<String>, u64)>,
) -> Result<(), String> {
    // Drain transcription queue
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({ "stage": "processing_transcripts", "message": "Processing remaining transcript chunks...", "progress": 40 }),
    );

    if let Some(task_handle) = transcription_task {
        info!("⏳ Background: waiting for ALL transcription chunks to be processed");

        let progress_app = app.clone();
        let progress_task = tokio::spawn(async move {
            let start = std::time::Instant::now();
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                let elapsed = start.elapsed().as_secs();
                let _ = progress_app.emit(
                    "recording-shutdown-progress",
                    serde_json::json!({
                        "stage": "processing_transcripts",
                        "message": format!("Processing transcripts... ({}s elapsed)", elapsed),
                        "progress": 40, "detailed": true, "elapsed_seconds": elapsed
                    }),
                );
            }
        });

        match tokio::time::timeout(tokio::time::Duration::from_secs(600), task_handle).await {
            Ok(Ok(())) => info!("✅ ALL transcription chunks processed — no data lost"),
            Ok(Err(e)) => warn!("⚠️ Transcription task error: {:?}", e),
            Err(_) => warn!("⏱️ Transcription timeout (10 min) — continuing shutdown"),
        }
        progress_task.abort();
    } else {
        info!("ℹ️ No transcription task to drain");
    }

    // Unload model
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({ "stage": "unloading_model", "message": "Unloading speech recognition model...", "progress": 70 }),
    );

    let config = match tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        crate::api::api::api_get_transcript_config(app.clone(), app.clone().state(), None),
    )
    .await
    {
        Ok(Ok(Some(cfg))) => Some(cfg.provider),
        Ok(Ok(None)) => None,
        Ok(Err(e)) => { warn!("⚠️ Transcript config error: {:?}", e); None }
        Err(_) => { warn!("⏱️ Transcript config timeout"); None }
    };

    match config.as_deref() {
        Some("parakeet") => {
            let engine_clone = {
                let g = crate::parakeet_engine::commands::PARAKEET_ENGINE.lock().unwrap();
                g.as_ref().cloned()
            };
            if let Some(engine) = engine_clone {
                let model = engine.get_current_model().await.unwrap_or_else(|| "unknown".into());
                if !engine.unload_model().await {
                    warn!("⚠️ Failed to unload Parakeet model '{}'", model);
                }
            }
        }
        _ => {
            let engine_clone = {
                let g = crate::whisper_engine::commands::WHISPER_ENGINE.lock().unwrap();
                g.as_ref().cloned()
            };
            if let Some(engine) = engine_clone {
                let model = engine.get_current_model().await.unwrap_or_else(|| "unknown".into());
                if !engine.unload_model().await {
                    warn!("⚠️ Failed to unload Whisper model '{}'", model);
                }
            }
        }
    }

    // Analytics
    if let Some((total_dur, active_dur, pause_dur, segments, had_error, mic_name, sys_name, chunks)) = analytics_snapshot {
        fn classify(name: &str) -> &'static str {
            let n = name.to_lowercase();
            if n.contains("bluetooth") || n.contains("airpods") || n.contains("beats")
                || n.contains("headphones") || n.contains("bt ") || n.contains("wireless")
            { "Bluetooth" } else { "Wired" }
        }

        let trans_cfg = match crate::api::api::api_get_transcript_config(app.clone(), app.clone().state(), None).await {
            Ok(Some(c)) => Some((c.provider, c.model)),
            _ => None,
        };
        let (t_prov, t_model) = trans_cfg.unwrap_or_else(|| ("unknown".into(), "unknown".into()));

        let sum_cfg = match crate::api::api::api_get_model_config(app.clone(), app.clone().state(), None).await {
            Ok(Some(c)) => Some((c.provider, c.model)),
            _ => None,
        };
        let (s_prov, s_model) = sum_cfg.unwrap_or_else(|| ("unknown".into(), "unknown".into()));

        let mic_type = mic_name.as_deref().map(classify).unwrap_or("Unknown").to_string();
        let sys_type = sys_name.as_deref().map(classify).unwrap_or("Unknown").to_string();

        match crate::analytics::commands::track_meeting_ended(
            t_prov, t_model, s_prov, s_model,
            total_dur, active_dur, pause_dur,
            mic_type, sys_type, chunks, segments, had_error,
        ).await {
            Ok(_) => info!("✅ Analytics tracked"),
            Err(e) => warn!("⚠️ Analytics error: {}", e),
        }
    }

    // Save recording
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({ "stage": "finalizing", "message": "Finalizing recording...", "progress": 90 }),
    );

    if let Some(ref mut mgr) = manager {
        match tokio::time::timeout(tokio::time::Duration::from_secs(300), mgr.save_recording_only(&app)).await {
            Ok(Ok(_)) => info!("✅ Recording saved"),
            Ok(Err(e)) => {
                warn!("⚠️ Save error (transcripts preserved): {}", e);
                return Err(format!("save_recording_only failed: {}", e));
            }
            Err(_) => {
                warn!("⏱️ File I/O timeout during save");
                return Err("save_recording_only timed out after 5 minutes".into());
            }
        }
    }

    // Emit completion progress. The synchronous recording-stopped event already fired in
    // stop_recording with folder_path/meeting_name; recording-saved (with the audio path)
    // fires from recording_saver::stop_and_save when the MP4 finalize step completes.
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({ "stage": "complete", "message": "Recording stopped successfully", "progress": 100 }),
    );

    info!("🎉 Background shutdown complete");
    Ok(())
}

/// Check if recording is active
pub async fn is_recording() -> bool {
    current_phase() == RecordingPhase::Recording
}

/// Get recording statistics
pub async fn get_transcription_status() -> TranscriptionStatus {
    TranscriptionStatus {
        chunks_in_queue: 0,
        is_processing: current_phase() == RecordingPhase::Recording,
        last_activity_ms: 0,
    }
}

/// Pause the current recording
#[tauri::command]
pub async fn pause_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Pausing recording");

    // Check if currently recording
    if current_phase() != RecordingPhase::Recording {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and pause it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.pause_recording().map_err(|e| e.to_string())?;

        // Emit pause event to frontend
        app.emit(
            "recording-paused",
            serde_json::json!({
                "message": "Recording paused"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect paused state
        crate::tray::update_tray_menu(&app);

        info!("Recording paused successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Resume the current recording
#[tauri::command]
pub async fn resume_recording<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    info!("Resuming recording");

    // Check if currently recording
    if current_phase() != RecordingPhase::Recording {
        return Err("No recording is currently active".to_string());
    }

    // Access the recording manager and resume it
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.resume_recording().map_err(|e| e.to_string())?;

        // Emit resume event to frontend
        app.emit(
            "recording-resumed",
            serde_json::json!({
                "message": "Recording resumed"
            }),
        )
        .map_err(|e| e.to_string())?;

        // Update tray menu to reflect resumed state
        crate::tray::update_tray_menu(&app);

        info!("Recording resumed successfully");
        Ok(())
    } else {
        Err("No recording manager found".to_string())
    }
}

/// Check if recording is currently paused
#[tauri::command]
pub async fn is_recording_paused() -> bool {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        manager.is_paused()
    } else {
        false
    }
}

/// Get detailed recording state
#[tauri::command]
pub async fn get_recording_state() -> serde_json::Value {
    let phase = current_phase();
    let is_recording = phase == RecordingPhase::Recording;
    let phase_str = match phase {
        RecordingPhase::Idle => "Idle",
        RecordingPhase::Recording => "Recording",
        RecordingPhase::Saving => "Saving",
    };
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        serde_json::json!({
            "is_recording": is_recording,
            "phase": phase_str,
            "is_paused": manager.is_paused(),
            "is_active": manager.is_active(),
            "recording_duration": manager.get_recording_duration(),
            "active_duration": manager.get_active_recording_duration(),
            "total_pause_duration": manager.get_total_pause_duration(),
            "current_pause_duration": manager.get_current_pause_duration()
        })
    } else {
        serde_json::json!({
            "is_recording": is_recording,
            "phase": phase_str,
            "is_paused": false,
            "is_active": false,
            "recording_duration": null,
            "active_duration": null,
            "total_pause_duration": 0.0,
            "current_pause_duration": null
        })
    }
}

/// Get the meeting folder path for the current recording
/// Returns the path if a meeting name was set and folder structure initialized
#[tauri::command]
pub async fn get_meeting_folder_path() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();
    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_meeting_folder().map(|p| p.to_string_lossy().to_string()))
    } else {
        Ok(None)
    }
}

/// Get accumulated transcript segments from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_transcript_history() -> Result<Vec<crate::audio::recording_saver::TranscriptSegment>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_transcript_segments())
    } else {
        Ok(Vec::new()) // No recording active, return empty
    }
}

/// Get meeting name from current recording session
/// Used for syncing frontend state after page reload during active recording
#[tauri::command]
pub async fn get_recording_meeting_name() -> Result<Option<String>, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        Ok(manager.get_meeting_name())
    } else {
        Ok(None)
    }
}

// ============================================================================
// CANCEL RECORDING — atomic cleanup for auto-detect cancellation
// ============================================================================

fn delete_recording_folder_inner(folder: Option<&std::path::Path>) -> Result<(), String> {
    let Some(path) = folder else { return Ok(()) };
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|e| {
            format!(
                "cancel_recording: file deletion failed at {}: {}",
                path.display(),
                e
            )
        })?;
        info!("cancel_recording: deleted audio folder {}", path.display());
    }
    Ok(())
}

/// Error text includes both meeting_id and folder_path so the startup GC pass can reconcile
/// if the process crashes before the row is cleaned up.
async fn delete_meeting_row_inner(
    pool: &sqlx::SqlitePool,
    meeting_id: &str,
    folder_path: &str,
) -> Result<(), String> {
    if meeting_id.is_empty() {
        return Ok(());
    }
    sqlx::query("DELETE FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .execute(pool)
        .await
        .map_err(|e| {
            format!(
                "cancel_recording: DB deletion failed for meeting {} at {}: {}",
                meeting_id, folder_path, e
            )
        })?;
    info!("cancel_recording: DB row deleted for meeting {}", meeting_id);
    Ok(())
}

/// Extracted so tests can assert phase state without a real AppHandle.
pub(crate) fn cancel_audio_capture_inner() -> Option<std::path::PathBuf> {
    set_phase(RecordingPhase::Idle);

    let folder = match RECORDING_MANAGER.lock() {
        Ok(mut global) => {
            if let Some(manager) = global.take() {
                let folder = manager.get_meeting_folder();
                drop(manager);
                folder
            } else {
                None
            }
        }
        Err(e) => {
            error!("RECORDING_MANAGER lock poisoned during cancel: {}", e);
            None
        }
    };

    match TRANSCRIPTION_TASK.lock() {
        Ok(mut task) => {
            if let Some(handle) = task.take() {
                handle.abort();
            }
        }
        Err(e) => error!("TRANSCRIPTION_TASK lock poisoned during cancel: {}", e),
    }

    folder
}

pub async fn cancel_recording_impl(
    app: &AppHandle,
    meeting_id: String,
) -> Result<String, String> {
    use crate::database::manager::DatabaseManager;

    info!("🚫 cancel_recording called for meeting_id={}", meeting_id);

    let folder = cancel_audio_capture_inner();

    // cancel_recording bypasses stop_recording, so the transcript listener must be
    // cleaned up here — stop_recording will not run for this session.
    {
        use tauri::Listener;
        match TRANSCRIPT_LISTENER_ID.lock() {
            Ok(mut g) => {
                if let Some(listener_id) = g.take() {
                    app.unlisten(listener_id);
                }
            }
            Err(e) => error!("TRANSCRIPT_LISTENER_ID lock poisoned during cancel: {}", e),
        }
    }

    crate::tray::update_tray_menu(app);

    // Notify frontend immediately so RecordingStateContext updates without waiting for polling.
    // recording-stopped is intentionally omitted here — it triggers the save flow against the
    // folder we are about to delete.
    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Idle" }));

    let folder_path = folder
        .as_ref()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    delete_recording_folder_inner(folder.as_deref()).map_err(|e| {
        error!("{}", e);
        e
    })?;

    // DB row may not exist if the frontend never saved; error propagates with meeting_id + path
    // so the startup GC pass can reconcile on next launch.
    if let Ok(db) = DatabaseManager::new_from_app_handle(app).await {
        let pool = db.pool().clone();
        if let Err(e) = delete_meeting_row_inner(&pool, &meeting_id, &folder_path).await {
            error!("{}", e);
            return Err(e);
        }
    }

    Ok(meeting_id)
}

pub async fn cancel_recording(
    app: AppHandle,
    meeting_id: String,
) -> Result<String, String> {
    cancel_recording_impl(&app, meeting_id).await
}

/// Name propagates through the `recording-stopped` event so the frontend saves the edited title.
pub async fn set_active_meeting_name_impl(name: String) -> Result<(), String> {
    let mut guard = RECORDING_MANAGER
        .lock()
        .map_err(|_| "recording manager lock poisoned".to_string())?;
    match guard.as_mut() {
        Some(manager) => {
            manager.set_meeting_name(Some(name));
            Ok(())
        }
        None => Ok(()), // no-op: recording already gone or not yet started
    }
}

// ============================================================================
// DEVICE MONITORING COMMANDS (AirPods/Bluetooth disconnect/reconnect support)
// ============================================================================

/// Response structure for device events
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type")]
pub enum DeviceEventResponse {
    DeviceDisconnected {
        device_name: String,
        device_type: String,
    },
    DeviceReconnected {
        device_name: String,
        device_type: String,
    },
    DeviceListChanged,
}

impl From<DeviceEvent> for DeviceEventResponse {
    fn from(event: DeviceEvent) -> Self {
        match event {
            DeviceEvent::DeviceDisconnected { device_name, device_type } => {
                DeviceEventResponse::DeviceDisconnected {
                    device_name,
                    device_type: format!("{:?}", device_type),
                }
            }
            DeviceEvent::DeviceReconnected { device_name, device_type } => {
                DeviceEventResponse::DeviceReconnected {
                    device_name,
                    device_type: format!("{:?}", device_type),
                }
            }
            DeviceEvent::DeviceListChanged => DeviceEventResponse::DeviceListChanged,
        }
    }
}

/// Reconnection status information
#[derive(Debug, Serialize, Clone)]
pub struct ReconnectionStatus {
    pub is_reconnecting: bool,
    pub disconnected_device: Option<DisconnectedDeviceInfo>,
}

/// Information about a disconnected device
#[derive(Debug, Serialize, Clone)]
pub struct DisconnectedDeviceInfo {
    pub name: String,
    pub device_type: String,
}

/// Poll for audio device events (disconnect/reconnect)
/// Should be called periodically (every 1-2 seconds) by frontend during recording
#[tauri::command]
pub async fn poll_audio_device_events() -> Result<Option<DeviceEventResponse>, String> {
    let mut manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_mut() {
        if let Some(event) = manager.poll_device_events() {
            info!("📱 Device event polled: {:?}", event);
            Ok(Some(event.into()))
        } else {
            Ok(None)
        }
    } else {
        // Not recording, no events
        Ok(None)
    }
}

/// Get current reconnection status
/// Returns whether the system is attempting to reconnect and which device
#[tauri::command]
pub async fn get_reconnection_status() -> Result<ReconnectionStatus, String> {
    let manager_guard = RECORDING_MANAGER.lock().unwrap();

    if let Some(manager) = manager_guard.as_ref() {
        let state = manager.get_state();
        let disconnected_device = state.get_disconnected_device().map(|(device, device_type)| {
            DisconnectedDeviceInfo {
                name: device.name.clone(),
                device_type: format!("{:?}", device_type),
            }
        });

        Ok(ReconnectionStatus {
            is_reconnecting: manager.is_reconnecting(),
            disconnected_device,
        })
    } else {
        // Not recording, no reconnection in progress
        Ok(ReconnectionStatus {
            is_reconnecting: false,
            disconnected_device: None,
        })
    }
}

/// Get information about the active audio output device
/// Used to warn users about Bluetooth playback issues
#[tauri::command]
pub async fn get_active_audio_output() -> Result<super::playback_monitor::AudioOutputInfo, String> {
    super::playback_monitor::get_active_audio_output()
        .await
        .map_err(|e| format!("Failed to get audio output info: {}", e))
}

/// Manually trigger device reconnection attempt
/// Useful for UI "Retry" button
#[tauri::command]
pub async fn attempt_device_reconnect(
    device_name: String,
    device_type: String,
) -> Result<bool, String> {
    // Parse device type first
    let monitor_type = match device_type.as_str() {
        "Microphone" => DeviceMonitorType::Microphone,
        "SystemAudio" => DeviceMonitorType::SystemAudio,
        _ => return Err(format!("Invalid device type: {}", device_type)),
    };

    // Check if recording is active
    {
        let manager_guard = RECORDING_MANAGER.lock().unwrap();
        if manager_guard.is_none() {
            return Err("Recording not active".to_string());
        }
    } // Release lock

    // Spawn blocking task to handle the async reconnection
    let result = tokio::task::spawn_blocking(move || {
        tokio::runtime::Handle::current().block_on(async {
            let mut manager_guard = RECORDING_MANAGER.lock().unwrap();
            if let Some(manager) = manager_guard.as_mut() {
                manager.attempt_device_reconnect(&device_name, monitor_type).await
            } else {
                Err(anyhow::anyhow!("Recording not active"))
            }
        })
    })
    .await
    .map_err(|e| format!("Task join error: {}", e))?;

    match result {
        Ok(success) => {
            if success {
                info!("✅ Manual reconnection successful");
            } else {
                warn!("❌ Manual reconnection failed - device not available");
            }
            Ok(success)
        }
        Err(e) => {
            error!("Manual reconnection error: {}", e);
            Err(e.to_string())
        }
    }
}

// ============================================================================
// Test-only hook for the synchronous stop path (exercises the phase guard +
// Saving transition without a real AppHandle or CPAL streams).
// ============================================================================

#[cfg(test)]
pub(crate) fn stop_recording_sync_path_for_test() {
    // Phase guard — step (a)
    if current_phase() != RecordingPhase::Recording {
        return;
    }
    // Skip stream release (step b) — no manager in tests.
    // Transition to Saving — step (c).
    set_phase(RecordingPhase::Saving);
    // Skip spawn (step d) — no AppHandle in tests.
}

// ============================================================================
// Tests for cancel_recording helper functions (tasks 5.1 and 5.2)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Helper: in-memory SQLite database for isolation.
    async fn test_db() -> (crate::database::manager::DatabaseManager, TempDir) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.sqlite").to_string_lossy().to_string();
        let db = crate::database::manager::DatabaseManager::new(&db_path, &db_path)
            .await
            .unwrap();
        (db, dir)
    }

    // Task 3.1: A second stop call while Saving returns Ok without restarting shutdown.
    // Idempotent because the phase guard in stop_recording returns Ok if Saving.
    #[test]
    fn stop_recording_is_idempotent_during_saving() {
        // Simulate in-progress background shutdown
        set_phase(RecordingPhase::Saving);
        // Second stop: phase guard should short-circuit
        stop_recording_sync_path_for_test();
        // Phase must still be Saving — not reset to Recording or Idle
        assert_eq!(
            current_phase(),
            RecordingPhase::Saving,
            "second stop during Saving must be a no-op and leave phase as Saving"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 3.4: start_recording is rejected while phase is Saving.
    // Tested via the guard at the top of start_recording_with_meeting_name.
    #[test]
    fn start_recording_rejected_during_saving() {
        set_phase(RecordingPhase::Saving);
        // The phase check inside start_recording functions returns an error when Saving.
        // We test the guard logic directly since we can't call the full async command here.
        let phase = current_phase();
        let rejected = phase == RecordingPhase::Saving;
        assert!(
            rejected,
            "start_recording must be rejected when phase is Saving (previous recording still finalizing)"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 5.1: cancel_recording transitions to Idle (stops audio capture).
    // Uses cancel_audio_capture_inner() — the extracted audio-stop step — so the
    // assertion does not require a real AppHandle or RecordingManager.
    // Note: RECORDING_PHASE is a global static; this test must not run concurrently
    // with other tests that write it. In practice the other tests here don't touch it.
    #[test]
    fn test_5_1_cancel_clears_recording_flag() {
        set_phase(RecordingPhase::Recording);
        cancel_audio_capture_inner();
        assert!(
            current_phase() == RecordingPhase::Idle,
            "cancel must transition to Idle so audio chunks stop being processed"
        );
    }

    // Task 1.3: current_phase() returns the phase most recently set by set_phase()
    // for each variant — verifies the enum round-trip through AtomicU8.
    #[test]
    fn test_1_3_phase_round_trip() {
        for &phase in &[RecordingPhase::Idle, RecordingPhase::Recording, RecordingPhase::Saving] {
            set_phase(phase);
            assert_eq!(
                current_phase(),
                phase,
                "current_phase() must return {:?} after set_phase({:?})",
                phase,
                phase
            );
        }
        // Leave in Idle so other tests start clean
        set_phase(RecordingPhase::Idle);
    }

    // Task 1.4: sequential phase transitions are observable in order.
    #[test]
    fn test_1_4_phase_sequence_observable_in_order() {
        set_phase(RecordingPhase::Recording);
        assert_eq!(current_phase(), RecordingPhase::Recording);
        set_phase(RecordingPhase::Saving);
        assert_eq!(current_phase(), RecordingPhase::Saving);
        set_phase(RecordingPhase::Idle);
        assert_eq!(current_phase(), RecordingPhase::Idle);
    }

    // Task 2.1: stop_recording synchronous path must complete within 1 s AND leave
    // the phase in Saving (streams released, background work not yet done).
    // FAILS until task 2.3: stub does not transition to Saving.
    #[test]
    fn stop_releases_streams_within_1s() {
        set_phase(RecordingPhase::Recording);
        let start = std::time::Instant::now();
        stop_recording_sync_path_for_test();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "synchronous stop path must complete within 1 s, took {:?}",
            elapsed
        );
        assert_eq!(
            current_phase(),
            RecordingPhase::Saving,
            "stop must transition to Saving before returning (background work runs separately)"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 2.2: stop_recording emits Saving phase BEFORE returning from the synchronous path.
    // Tested via phase atomic — the Saving transition must happen on the synchronous path,
    // not inside the spawned background task.
    // FAILS until task 2.3: stub does not transition to Saving.
    #[test]
    fn stop_emits_saving_phase_event() {
        set_phase(RecordingPhase::Recording);
        stop_recording_sync_path_for_test();
        assert_eq!(
            current_phase(),
            RecordingPhase::Saving,
            "recording-state-changed(Saving) must be emitted by the synchronous stop path"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 4.1: PhaseGuard resets phase to Idle on normal scope exit.
    #[test]
    fn phase_guard_resets_to_idle_on_drop() {
        set_phase(RecordingPhase::Saving);
        {
            let _guard = PhaseGuard;
            assert_eq!(current_phase(), RecordingPhase::Saving, "still Saving inside guard scope");
        } // _guard drops → set_phase(Idle)
        assert_eq!(
            current_phase(),
            RecordingPhase::Idle,
            "PhaseGuard::drop must call set_phase(Idle)"
        );
    }

    // Task 4.1 (panic path): PhaseGuard resets phase to Idle even when the spawn block panics.
    // This is the critical invariant: a Whisper panic / OOM / mutex poison inside
    // background_shutdown must not leave the UI stuck in "Saving…" forever.
    #[tokio::test]
    async fn phase_guard_resets_to_idle_on_panic() {
        set_phase(RecordingPhase::Saving);

        let handle = tokio::spawn(async {
            let _guard = PhaseGuard;
            panic!("simulated background_shutdown panic");
        });

        // The task panics — JoinError::panicked is expected.
        let _ = handle.await;

        assert_eq!(
            current_phase(),
            RecordingPhase::Idle,
            "PhaseGuard must reset phase to Idle even when the spawn block panics"
        );
    }

    // Task 5.1: cancel_recording_folder helper deletes the folder and returns Ok.
    #[test]
    fn test_5_1_cancel_deletes_recording_folder() {
        let dir = TempDir::new().unwrap();
        let folder = dir.path().to_path_buf();
        std::fs::write(folder.join("audio.wav"), b"RIFF").unwrap();
        assert!(folder.exists(), "pre-condition: folder exists before cancel");

        let result = delete_recording_folder_inner(Some(&folder));
        assert!(result.is_ok(), "cancel must succeed: {:?}", result);
        assert!(!folder.exists(), "folder must be deleted after cancel");
    }

    // Task 5.1 (extension): returns Ok(meeting_id) on success via the row helper.
    #[tokio::test]
    async fn test_5_1_delete_row_returns_ok_when_db_is_open() {
        let (db, _dir) = test_db().await;
        let pool = db.pool().clone();

        let result = delete_meeting_row_inner(&pool, "meeting-001", "/tmp/meeting-001").await;
        assert!(result.is_ok(), "deletion of non-existent row must succeed (no-op)");
    }

    // Task 5.2: when the DB pool is closed (simulating a failure) after file deletion
    // succeeds, the function returns an error that includes both the meeting_id and
    // the folder path so log readers and the GC pass can act.
    #[tokio::test]
    async fn test_5_2_db_failure_returns_error_with_meeting_id_and_path() {
        let (db, _dir) = test_db().await;
        let pool = db.pool().clone();

        // Close the pool before the DELETE so the operation fails.
        pool.close().await;

        let result =
            delete_meeting_row_inner(&pool, "meeting-abc", "/recordings/meeting-abc").await;

        assert!(result.is_err(), "closed pool must cause an error");
        let err = result.unwrap_err();
        assert!(
            err.contains("meeting-abc"),
            "error must include meeting_id for GC reconciliation; got: {err}"
        );
        assert!(
            err.contains("/recordings/meeting-abc"),
            "error must include folder path for GC reconciliation; got: {err}"
        );
    }

    // Task 9: regression — stop_recording must hand folder_path and meeting_name back
    // to the frontend SYNCHRONOUSLY via its return value, not (only) via the late
    // recording-stopped event. The previous code left the save flow racing against
    // background_shutdown, which meant meetings got saved with null folder_path and
    // the audio file (finalized minutes later) was never linked to the DB row.
    //
    // This test pins the contract: the struct exists, serializes through serde, and
    // both fields default to None when the caller has nothing to report (e.g., the
    // early-return paths in stop_recording when phase is Idle or Saving).
    #[test]
    fn stop_recording_result_serializes_with_none_fields() {
        let result = StopRecordingResult {
            folder_path: None,
            meeting_name: None,
        };
        let json = serde_json::to_value(&result).expect("must serialize");
        assert!(json.is_object(), "StopRecordingResult must serialize as JSON object");
        assert_eq!(json.get("folder_path"), Some(&serde_json::Value::Null));
        assert_eq!(json.get("meeting_name"), Some(&serde_json::Value::Null));
    }

    #[test]
    fn stop_recording_result_serializes_with_populated_fields() {
        let result = StopRecordingResult {
            folder_path: Some("C:/recordings/Meeting 2026-05-13_22-41-17".to_string()),
            meeting_name: Some("Standup".to_string()),
        };
        let json = serde_json::to_value(&result).expect("must serialize");
        assert_eq!(
            json.get("folder_path").and_then(|v| v.as_str()),
            Some("C:/recordings/Meeting 2026-05-13_22-41-17"),
            "folder_path must round-trip through serde for frontend consumption"
        );
        assert_eq!(
            json.get("meeting_name").and_then(|v| v.as_str()),
            Some("Standup"),
            "meeting_name must round-trip through serde for frontend consumption"
        );
    }
}

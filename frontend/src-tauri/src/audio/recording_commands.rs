// audio/recording_commands.rs
//
// Slim Tauri command layer for recording functionality.
// Delegates to transcription and recording modules for actual implementation.

use anyhow::Result;
use futures::FutureExt as _;
use log::{error, info, warn};
use serde::Serialize;
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc, Mutex,
};
use tauri::{AppHandle, Emitter, Manager, Runtime};

use super::{
    parse_audio_device,
    default_input_device,   // Get default microphone
    default_output_device,  // Get default system audio
    RecordingManager,
    DeviceEvent,
    DeviceMonitorType
};

// Import transcription modules
use super::transcription::reset_speech_detected_flag;

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
// Uses compare_exchange(Saving→Idle) so a concurrent M2 start (phase=Recording) is never
// clobbered — the exchange only fires if we're still in Saving.
struct PhaseGuard;
impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let _ = RECORDING_PHASE.compare_exchange(
            RecordingPhase::Saving as u8,
            RecordingPhase::Idle as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

// Global recording manager to keep it alive during recording
static RECORDING_MANAGER: Mutex<Option<RecordingManager>> = Mutex::new(None);


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

/// Set the transcription-scheduler recording gate and signal any active job to yield.
/// Called by every start_recording variant; inverted by stop and cancel paths.
fn set_recording_gate<R: Runtime>(app: &AppHandle<R>, busy: bool) {
    let queue = app.state::<crate::TranscriptionQueueState>();
    queue.scheduler.recording_busy.store(busy, Ordering::SeqCst);
    if busy {
        crate::use_cases::transcription_queue::SHOULD_YIELD.store(true, Ordering::SeqCst);
    }
}

/// Returns `Err` if `phase` prevents starting a new recording.
/// Extracted so tests can call it without an `AppHandle`.
pub(crate) fn can_start_recording(phase: RecordingPhase) -> Result<(), String> {
    if phase == RecordingPhase::Recording {
        return Err("Recording already in progress".to_string());
    }
    Ok(())
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

    // Check if already recording
    let phase = current_phase();
    info!("🔍 Phase check: {:?}", phase);
    can_start_recording(phase)?;

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
    let _transcription_receiver = manager
        .start_recording(microphone_device, system_device, auto_save, gate_floor_dbfs)
        .await
        .map_err(|e| format!("Failed to start recording: {}", e))?;

    // Store the manager globally, atomically claiming Recording phase.
    // fetch_update rejects only if another start_recording already won the race
    // (phase == Recording). It accepts Idle and Saving — the latter is the M2 back-to-back
    // case. The stale `phase` snapshot is intentionally not used as the expected value: by
    // the time async device init finishes, M1's save may have already transitioned
    // Saving→Idle, and we must not reject M2 just because Saving→Idle raced us.
    // Whichever concurrent start loses the fetch_update has its manager dropped here,
    // closing any cpal streams it opened.
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        if RECORDING_PHASE
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                if cur == RecordingPhase::Recording as u8 {
                    None // already Recording — reject
                } else {
                    Some(RecordingPhase::Recording as u8)
                }
            })
            .is_err()
        {
            return Err("Recording already in progress".to_string());
        }
        *global_manager = Some(manager);
    }

    // Phase already set to Recording by the fetch_update above.
    reset_speech_detected_flag(); // Reset for new recording session
    set_recording_gate(&app, true);

    // Emit phase transition event
    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Recording" }));

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started successfully",
        "devices": ["Default Microphone", "Default System Audio"],
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

    // Check if already recording
    let phase = current_phase();
    info!("🔍 Phase check: {:?}", phase);
    can_start_recording(phase)?;

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
    let _transcription_receiver = manager
        .start_recording(mic_device, system_device, auto_save, gate_floor_dbfs)
        .await
        .map_err(|e| format!("Failed to start recording: {}", e))?;

    // Store the manager globally, atomically claiming Recording phase.
    // fetch_update rejects only if another start_recording already won the race
    // (phase == Recording). Accepts Idle and Saving (M2 back-to-back case). See the
    // comment in start_recording_with_meeting_name for the full rationale.
    // Whichever concurrent start loses has its manager dropped here, closing cpal streams.
    {
        let mut global_manager = RECORDING_MANAGER.lock().unwrap();
        if RECORDING_PHASE
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |cur| {
                if cur == RecordingPhase::Recording as u8 {
                    None // already Recording — reject
                } else {
                    Some(RecordingPhase::Recording as u8)
                }
            })
            .is_err()
        {
            return Err("Recording already in progress".to_string());
        }
        *global_manager = Some(manager);
    }

    // Phase already set to Recording by the fetch_update above.
    reset_speech_detected_flag(); // Reset for new recording session
    set_recording_gate(&app, true);

    // Emit phase transition event
    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Recording" }));

    // Emit success event
    app.emit("recording-started", serde_json::json!({
        "message": "Recording started with custom devices",
        "devices": [
            mic_device_name.unwrap_or_else(|| "Default Microphone".to_string()),
            system_device_name.unwrap_or_else(|| "Default System Audio".to_string())
        ],
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
        // Atomically claim Saving inside the lock. This closes two races simultaneously:
        // 1. cancel_audio_capture_inner(): its CAS(Recording→Idle) fails the moment we
        //    transition to Saving, so it can no longer steal the manager.
        // 2. Concurrent stop_recording (e.g., UI button + tray menu): the second caller's
        //    CAS fails here and returns early — it never spawns background_shutdown and
        //    therefore never calls clear_gate_and_resume!, which would otherwise prematurely
        //    clear the recording gate while the winner's MP4 write is still in flight.
        if RECORDING_PHASE
            .compare_exchange(
                RecordingPhase::Recording as u8,
                RecordingPhase::Saving as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_err()
        {
            // Another concurrent stop already won (phase ≠ Recording). Nothing to do.
            return Ok(StopRecordingResult { folder_path: None, meeting_name: None });
        }
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

    // (c) Emit the Saving event — phase was already set to Saving in the manager lock above.
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

    // (d) Spawn background shutdown task — save recording, then analytics
    let app_bg = app.clone();
    // Local flag: background_shutdown sets this to true after the save step completes
    // (regardless of outcome). The panic arm reads it to distinguish a save panic
    // from a post-save analytics panic, without relying on RECORDING_PHASE which
    // M2's concurrent set_phase(Recording) can overwrite.
    let save_attempted = Arc::new(AtomicBool::new(false));
    tokio::spawn(async move {
        // PhaseGuard: belt-and-suspenders phase reset in case catch_unwind itself fails.
        let _guard = PhaseGuard;

        // catch_unwind ensures cleanup below runs even when background_shutdown panics,
        // preventing recording_busy from being permanently stuck for the session.
        let result = std::panic::AssertUnwindSafe(background_shutdown(
            app_bg.clone(),
            manager_for_background.take(),
            analytics_snapshot,
            save_attempted.clone(),
        ))
        .catch_unwind()
        .await;

        match result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                error!("❌ Background shutdown error: {}", e);
                let _ = app_bg.emit(
                    "recording-save-failed",
                    serde_json::json!({ "error": e }),
                );
            }
            Err(_panic) => {
                // save_attempted is set before clear_gate_and_resume! on every save outcome.
                // If false, the panic happened mid-save — emit the error. If true, the panic
                // was in post-save analytics; the MP4 is on disk, so don't alarm the user.
                if save_attempted.load(Ordering::SeqCst) {
                    error!("❌ background_shutdown panicked in post-save analytics (recording was saved)");
                } else {
                    error!("❌ background_shutdown panicked during save — gate and UI will be cleaned up");
                    let _ = app_bg.emit(
                        "recording-save-failed",
                        serde_json::json!({ "error": "internal error during recording save" }),
                    );
                }
            }
        }

        // clear_gate_and_resume! already ran on the happy/error paths (phase=Idle already).
        // On the panic path, compare_exchange succeeds and runs the full cleanup.
        if RECORDING_PHASE.compare_exchange(
            RecordingPhase::Saving as u8,
            RecordingPhase::Idle as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ).is_ok() {
            set_recording_gate(&app_bg, false);
            let queue_bg = app_bg.state::<crate::TranscriptionQueueState>();
            if !queue_bg.scheduler.manual_pause_all.load(Ordering::SeqCst) {
                queue_bg.resume_all().await;
            }
            // Guard: M2 may have called set_phase(Recording) during resume_all().await.
            // Skip "Idle" emit to avoid clobbering M2's "Recording" event on the frontend.
            if current_phase() == RecordingPhase::Idle {
                let _ = app_bg.emit("recording-state-changed", serde_json::json!({ "phase": "Idle" }));
                crate::tray::update_tray_menu(&app_bg);
            }
        }
        info!("✅ Phase → Idle (background shutdown complete)");
    });

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
    analytics_snapshot: Option<(Option<f64>, f64, f64, u64, bool, Option<String>, Option<String>, u64)>,
    save_attempted: Arc<AtomicBool>,
) -> Result<(), String> {
    // Save first: gate clears AFTER save so the worker never races against an in-flight MP4
    // write (worker could call find_audio_file() on an incomplete MP4 and mark the job Failed).
    // Phase transitions to Idle immediately after save so M2 can start a new recording without
    // waiting for the analytics block. compare_exchange guards against overwriting M2's phase.
    let _ = app.emit(
        "recording-shutdown-progress",
        serde_json::json!({ "stage": "finalizing", "message": "Finalizing recording...", "progress": 90 }),
    );

    let queue = app.state::<crate::TranscriptionQueueState>();

    macro_rules! clear_gate_and_resume {
        () => {
            // compare_exchange runs first: if M2 has fully started (phase=Recording) the
            // exchange fails and nothing below runs — gate stays set, queue stays paused.
            // RACE: M2 can pass can_start_recording(Saving) and be in its async startup before
            // reaching set_phase(Recording). In that window compare_exchange succeeds, gate
            // clears, and the worker may briefly run. M2 re-sets gate when it sets
            // phase=Recording; the worker yields at the next chunk boundary. Benign since live
            // transcription was removed (no engine contention during recording startup).
            // EMIT GUARD: resume_all().await is a yield point. M2 may call set_phase(Recording)
            // and emit "Recording" during that yield. Re-check phase before emitting "Idle" so
            // M1's late emit cannot clobber M2's "Recording" event on the frontend.
            if RECORDING_PHASE.compare_exchange(
                RecordingPhase::Saving as u8,
                RecordingPhase::Idle as u8,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ).is_ok() {
                set_recording_gate(&app, false);
                if !queue.scheduler.manual_pause_all.load(Ordering::SeqCst) {
                    queue.resume_all().await;
                }
                if current_phase() == RecordingPhase::Idle {
                    let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Idle" }));
                    crate::tray::update_tray_menu(&app);
                }
            }
        };
    }

    if let Some(ref mut mgr) = manager {
        match tokio::time::timeout(tokio::time::Duration::from_secs(300), mgr.save_recording_only(&app)).await {
            Ok(Ok(_)) => {
                info!("✅ Recording saved");
                save_attempted.store(true, Ordering::SeqCst);
                clear_gate_and_resume!();
            }
            Ok(Err(e)) => {
                warn!("⚠️ Save error (transcripts preserved): {}", e);
                save_attempted.store(true, Ordering::SeqCst);
                clear_gate_and_resume!();
                return Err(format!("save_recording_only failed: {}", e));
            }
            Err(_) => {
                warn!("⏱️ File I/O timeout during save");
                save_attempted.store(true, Ordering::SeqCst);
                clear_gate_and_resume!();
                return Err("save_recording_only timed out after 5 minutes".into());
            }
        }
    } else {
        save_attempted.store(true, Ordering::SeqCst);
        clear_gate_and_resume!();
    }

    // Analytics runs after phase=Idle so M2 can start recording while this completes.
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

    // recording-stopped fired synchronously in stop_recording with folder_path/meeting_name;
    // recording-saved fires from recording_saver::stop_and_save when the MP4 finalizes.
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
///
/// Only proceeds when the caller successfully claims the Recording→Idle phase transition.
/// If phase is Saving (background_shutdown running) or Idle (no live session), returns None
/// without touching the manager — prevents a stale cancel from destroying state it doesn't own.
///
/// RESIDUAL RISK (back-to-back): compare_exchange(Recording→Idle) succeeds for any Recording
/// phase, including M2's. A stale M1 cancel arriving while M2 is Recording will transition M2's
/// phase and take M2's manager. Closing this gap requires session-scoped identity tokens;
/// defer to when a cancel button is added to the UI (task 12.3).
pub(crate) fn cancel_audio_capture_inner() -> Option<std::path::PathBuf> {
    // Gate: only proceed if we claimed the Recording→Idle transition. Fails for Saving/Idle.
    if RECORDING_PHASE.compare_exchange(
        RecordingPhase::Recording as u8,
        RecordingPhase::Idle as u8,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ).is_err() {
        return None;
    }

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

    folder
}

pub async fn cancel_recording_impl(
    app: &AppHandle,
    meeting_id: String,
) -> Result<String, String> {
    use crate::database::manager::DatabaseManager;

    info!("🚫 cancel_recording called for meeting_id={}", meeting_id);

    let folder = cancel_audio_capture_inner();

    // Only clear the gate if cancel_audio_capture_inner() actually claimed Recording→Idle.
    // If it returned None (phase was Saving or Idle), background_shutdown owns the gate and
    // will clear it via clear_gate_and_resume! once the MP4 write is complete. Clearing early
    // would let the transcription worker pick up a file that is still being written.
    if folder.is_some() {
        set_recording_gate(app, false);
        let queue = app.state::<crate::TranscriptionQueueState>();
        if !queue.scheduler.manual_pause_all.load(Ordering::SeqCst) {
            queue.resume_all().await;
        }
        // Guard: M2 may have called set_phase(Recording) during resume_all().await.
        // Skip "Idle" emit and tray update to avoid clobbering M2's "Recording" event.
        // recording-stopped is intentionally omitted regardless — it triggers the save flow
        // against the folder we are about to delete.
        if current_phase() == RecordingPhase::Idle {
            let _ = app.emit("recording-state-changed", serde_json::json!({ "phase": "Idle" }));
            crate::tray::update_tray_menu(app);
        }
    }

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

    // Serializes tests that read/write RECORDING_PHASE (process-global AtomicU8).
    // Without this, parallel test threads can interleave set_phase calls and produce
    // flaky assertion failures (e.g. cancel_during_saving sees Recording set by another test).
    static PHASE_TEST_LOCK: Mutex<()> = Mutex::new(());
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
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

    // can_start_recording: Saving → Ok (M2 back-to-back allowed); Recording → Err (blocked).
    #[test]
    fn can_start_recording_allows_saving_blocks_recording() {
        assert!(
            can_start_recording(RecordingPhase::Saving).is_ok(),
            "can_start_recording must return Ok when phase is Saving (M2 back-to-back)"
        );
        assert!(
            can_start_recording(RecordingPhase::Recording).is_err(),
            "can_start_recording must return Err when phase is Recording"
        );
        assert!(
            can_start_recording(RecordingPhase::Idle).is_ok(),
            "can_start_recording must return Ok when phase is Idle"
        );
    }

    // Task 5.1: cancel_recording transitions to Idle (stops audio capture).
    // Uses cancel_audio_capture_inner() — the extracted audio-stop step — so the
    // assertion does not require a real AppHandle or RecordingManager.
    // Note: RECORDING_PHASE is a global static; this test must not run concurrently
    // with other tests that write it. In practice the other tests here don't touch it.
    #[test]
    fn test_5_1_cancel_clears_recording_flag() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Recording);
        cancel_audio_capture_inner();
        assert!(
            current_phase() == RecordingPhase::Idle,
            "cancel must transition to Idle so audio chunks stop being processed"
        );
    }

    // cancel_audio_capture_inner must return None and leave phase unchanged when phase=Saving.
    // A stale cancel for M1 fired while M1's background_shutdown holds Saving must be a no-op
    // so stop_recording's phase guard and any concurrent M2 manager remain intact.
    #[test]
    fn cancel_during_saving_leaves_phase_unchanged() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Saving);
        let result = cancel_audio_capture_inner();
        assert!(result.is_none(), "stale cancel during Saving must return None (early exit)");
        assert_eq!(
            current_phase(),
            RecordingPhase::Saving,
            "cancel must not overwrite Saving phase (compare_exchange(Recording→Idle) must fail)"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 1.3: current_phase() returns the phase most recently set by set_phase()
    // for each variant — verifies the enum round-trip through AtomicU8.
    #[test]
    fn test_1_3_phase_round_trip() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
    fn stop_sync_path_transitions_phase_to_saving_and_returns_fast() {
        // Note: this stub exercises only the phase-state machine, not real stream teardown.
        // The 1s bound is a sanity check on the stub; the real stop path is not measured here.
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Recording);
        let start = std::time::Instant::now();
        stop_recording_sync_path_for_test();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "stub must return instantly, took {:?}",
            elapsed
        );
        assert_eq!(
            current_phase(),
            RecordingPhase::Saving,
            "stop must transition to Saving before returning (background work runs separately)"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 2.2: phase is Saving (not Recording) after the synchronous stop path returns.
    // Validates only the phase-state machine — stream teardown is not tested here since
    // the stub has no real streams. The Saving transition must happen synchronously, before
    // the spawned background task runs.
    #[test]
    fn stop_sync_path_transitions_phase_to_saving() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Recording);
        stop_recording_sync_path_for_test();
        assert_eq!(
            current_phase(),
            RecordingPhase::Saving,
            "phase must be Saving on return from the synchronous stop path"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 4.1: PhaseGuard resets phase to Idle on normal scope exit.
    #[test]
    fn phase_guard_resets_to_idle_on_drop() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Saving);
        {
            let _guard = PhaseGuard;
            assert_eq!(current_phase(), RecordingPhase::Saving, "still Saving inside guard scope");
        } // _guard drops → compare_exchange(Saving→Idle)
        assert_eq!(
            current_phase(),
            RecordingPhase::Idle,
            "PhaseGuard::drop must transition Saving→Idle"
        );
    }

    // PhaseGuard must not clobber M2's Recording phase (back-to-back recording scenario).
    // M1's guard fires after M2 has already started — compare_exchange(Saving→Idle) must
    // be a no-op when current phase is Recording.
    #[test]
    fn phase_guard_does_not_clobber_recording_phase() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Saving);
        {
            let _guard = PhaseGuard;
            // Simulate M2 starting mid-save (the window this fix targets)
            set_phase(RecordingPhase::Recording);
        } // guard drops — compare_exchange must be a no-op because phase ≠ Saving
        assert_eq!(
            current_phase(),
            RecordingPhase::Recording,
            "PhaseGuard::drop must not overwrite M2's Recording phase"
        );
        set_phase(RecordingPhase::Idle); // cleanup
    }

    // Task 4.1 (panic path): PhaseGuard resets phase to Idle on direct task panic.
    // The real spawn block wraps background_shutdown in catch_unwind so panics there are
    // handled with full cleanup; this test covers the fallback if catch_unwind itself fails.
    #[tokio::test]
    async fn phase_guard_resets_to_idle_on_panic() {
        let _lock = PHASE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_phase(RecordingPhase::Saving);

        let handle = tokio::spawn(async {
            let _guard = PhaseGuard;
            panic!("simulated background_shutdown panic");
        });

        // The task panics — JoinError::panicked is expected.
        assert!(handle.await.is_err(), "spawn task must report panic as JoinError");

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

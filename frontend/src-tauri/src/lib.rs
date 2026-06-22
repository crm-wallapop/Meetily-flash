use serde::Serialize;
use std::sync::Mutex as StdMutex;

// Performance optimization: Conditional logging macros for hot paths
#[cfg(debug_assertions)]
macro_rules! perf_debug {
    ($($arg:tt)*) => {
        log::debug!($($arg)*)
    };
}

#[cfg(not(debug_assertions))]
macro_rules! perf_debug {
    ($($arg:tt)*) => {};
}

#[cfg(debug_assertions)]
macro_rules! perf_trace {
    ($($arg:tt)*) => {
        log::trace!($($arg)*)
    };
}

#[cfg(not(debug_assertions))]
macro_rules! perf_trace {
    ($($arg:tt)*) => {};
}

// perf_debug! and perf_trace! are macro_rules! at crate root — accessible everywhere directly.

// Re-export async logging macros for external use (removed due to macro conflicts)

// Declare audio module
pub mod analytics;
pub mod api;
pub mod audio;
pub mod config;
pub mod console_utils;
pub mod database;
pub mod detection;
pub mod notifications;
pub mod ollama;
pub mod onboarding;
pub mod openai;
pub mod anthropic;
pub mod groq;
pub mod openrouter;
pub mod parakeet_engine;
pub mod ports;
pub mod state;
pub mod summary;
pub mod tray;
pub mod use_cases;
pub mod utils;
pub mod whisper_engine;

use audio::{list_audio_devices, AudioDevice, trigger_audio_permission};
use log::{error as log_error, info as log_info};
use notifications::commands::NotificationManagerState;
use std::sync::Arc;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::RwLock;
use use_cases::transcription_queue::TranscriptionQueue;
use use_cases::scheduler_settings::SchedulerLiveConfig;

pub type TranscriptionQueueState = Arc<TranscriptionQueue>;
pub type SchedulerConfigState = Arc<SchedulerLiveConfig>;

// Remembers the last saved meeting title so the stopped toast's [Continue recording]
// can restart a fresh session with the same name — the meetily:// URI carries no title.
static LAST_MEETING_TITLE: StdMutex<Option<String>> = StdMutex::new(None);

// Global language preference storage (default to "auto-translate" for automatic translation to English)
static LANGUAGE_PREFERENCE: std::sync::LazyLock<StdMutex<String>> =
    std::sync::LazyLock::new(|| StdMutex::new("auto-translate".to_string()));

#[derive(Debug, Serialize, Clone)]
struct TranscriptionStatus {
    chunks_in_queue: usize,
    is_processing: bool,
    last_activity_ms: u64,
}

#[tauri::command]
async fn start_recording<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
) -> Result<audio::recording_commands::StartRecordingResult, String> {
    log_info!("🔥 CALLED start_recording with meeting: {:?}", meeting_name);
    log_info!(
        "📋 Backend received parameters - mic: {:?}, system: {:?}, meeting: {:?}",
        mic_device_name,
        system_device_name,
        meeting_name
    );

    if is_recording().await {
        return Err("Recording already in progress".to_string());
    }

    // Call the actual audio recording system with meeting name
    match audio::recording_commands::start_recording_with_devices_and_meeting(
        app.clone(),
        mic_device_name,
        system_device_name,
        meeting_name.clone(),
    )
    .await
    {
        Ok(result) => {
            tray::update_tray_menu(&app);

            log_info!("Recording started successfully with meeting_id: {}", result.meeting_id);

            // Show recording started notification through NotificationManager
            // This respects user's notification preferences. The legacy no-devices
            // start path is always manual.
            let notification_manager_state = app.state::<NotificationManagerState<R>>();
            if let Err(e) = notifications::commands::show_recording_started_notification(
                &app,
                &notification_manager_state,
                meeting_name.clone(),
                notifications::types::RecordStartSource::Manual,
            )
            .await
            {
                log_error!(
                    "Failed to show recording started notification: {}",
                    e
                );
            } else {
                log_info!("Successfully showed recording started notification");
            }

            Ok(result)
        }
        Err(e) => {
            log_error!("Failed to start audio recording: {}", e);
            Err(format!("Failed to start recording: {}", e))
        }
    }
}

#[tauri::command]
async fn stop_recording<R: Runtime>(
    app: AppHandle<R>,
) -> Result<audio::recording_commands::StopRecordingResult, String> {
    log_info!("Attempting to stop recording...");

    if !audio::recording_commands::is_recording().await {
        log_info!("Recording is already stopped");
        return Ok(audio::recording_commands::StopRecordingResult {
            meeting_id: None,
            folder_path: None,
            meeting_name: None,
        });
    }

    match audio::recording_commands::stop_recording(app.clone()).await
    {
        Ok(result) => {
            tray::update_tray_menu(&app);

            if let Some(name) = result.meeting_name.as_ref() {
                if let Ok(mut g) = LAST_MEETING_TITLE.lock() {
                    *g = Some(name.clone());
                }
            }

            let notification_manager_state = app.state::<NotificationManagerState<R>>();
            if let Err(e) = notifications::commands::show_recording_stopped_notification(
                &app,
                &notification_manager_state,
                result.meeting_name.clone(),
            )
            .await
            {
                log_error!("Failed to show recording stopped notification: {}", e);
            }

            Ok(result)
        }
        Err(e) => {
            log_error!("Failed to stop audio recording: {}", e);
            tray::update_tray_menu(&app);
            Err(format!("Failed to stop recording: {}", e))
        }
    }
}

/// Read-only recording state backing the deep-link dispatch guards. Reads the
/// authoritative `RecordingPhase` from the audio layer — NOT a separate flag — so the
/// dispatch guards stay in sync with the real recording lifecycle across every start
/// path (the production `start_recording_with_devices_and_meeting` command does not
/// touch any standalone flag, only the phase).
struct LiveRecordingState;
impl use_cases::notification_action::RecordingStatePort for LiveRecordingState {
    fn is_recording(&self) -> bool {
        audio::recording_commands::current_phase()
            == audio::recording_commands::RecordingPhase::Recording
    }
}

/// Validate a `meetily://` URI, guard it against the live recording state, and route the
/// outcome to a recording command. Emits `deep-link-dispatched` for every URI (logging +
/// test surface); `recording-continue-requested` when a fresh capture should start. The
/// URI is treated as untrusted boundary input throughout — only the resolved `Action`
/// (carrying no payload) reaches the command paths.
fn handle_deep_link(app: &tauri::AppHandle, uri: &str) {
    use tauri::Emitter;
    use use_cases::notification_action::{dispatch_notification_action, resolve, Action, Resolved};

    let action = dispatch_notification_action(uri);
    let outcome = resolve(action, &LiveRecordingState);

    log::info!("deep-link dispatch: uri={uri} action={action:?} outcome={outcome:?}");
    let _ = app.emit(
        "deep-link-dispatched",
        serde_json::json!({
            "uri": uri,
            "action": format!("{action:?}"),
            "outcome": format!("{outcome:?}"),
        }),
    );

    match outcome {
        Resolved::Execute(Action::Stop) => {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = stop_recording(app).await {
                    log::error!("deep-link stop failed: {e}");
                }
            });
        }
        Resolved::Execute(Action::Continue) => {
            let title = LAST_MEETING_TITLE.lock().ok().and_then(|g| g.clone());
            let _ = app.emit(
                "recording-continue-requested",
                serde_json::json!({ "title": title }),
            );
        }
        Resolved::Execute(Action::Rejected) | Resolved::NoOp => {}
    }
}

/// Dev/test seam: dispatch a `meetily://` URI through the same path a real toast-button
/// activation takes, so the smoke spec can exercise routing from the webview. Debug-only;
/// never registered in release builds.
#[cfg(debug_assertions)]
#[tauri::command]
fn __dev_inject_deep_link(app: tauri::AppHandle, uri: String) -> Result<String, String> {
    handle_deep_link(&app, &uri);
    Ok(format!("dispatched: {uri}"))
}

#[tauri::command]
async fn cancel_recording(app: AppHandle, meeting_id: String) -> Result<String, String> {
    audio::recording_commands::cancel_recording(app, meeting_id).await
}

#[tauri::command]
async fn is_recording() -> bool {
    audio::recording_commands::is_recording().await
}

/// Name propagates through `recording-stopped` so the frontend saves the user-edited title.
#[tauri::command]
async fn set_active_meeting_name(name: String) -> Result<(), String> {
    audio::recording_commands::set_active_meeting_name_impl(name).await
}

/// Newtype wrapper so Tauri's TypeId-keyed state map doesn't collide with any
/// other `Arc<AtomicBool>` registered by future code or plugins.
#[cfg(target_os = "windows")]
struct SuppressDetectionSignal(std::sync::Arc<std::sync::atomic::AtomicBool>);

/// Prevents re-detection after the user dismisses the auto-start banner (D16).
#[cfg(target_os = "windows")]
#[tauri::command]
fn signal_cancel_detection(
    suppress: tauri::State<'_, SuppressDetectionSignal>,
) {
    suppress.0.store(true, std::sync::atomic::Ordering::SeqCst);
    log::info!("cancel-suppression signal set by frontend");
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
fn signal_cancel_detection() {}

/// Dev/test-only (compiled under `dev-detector` only): inject a synthetic
/// meeting-detection observation so the auto-detect flow runs without a real
/// Google Meet call. `state` is `"joined"` or `"left"`; any other value is
/// rejected before the shared observation is touched. The `__` prefix is a
/// documentation convention only — `generate_handler!` exposes no command-hiding,
/// so the off-by-default Cargo feature is the sole compile-time gate.
#[cfg(feature = "dev-detector")]
#[tauri::command]
fn __dev_simulate_meeting(
    state: String,
    title: Option<String>,
    handle: tauri::State<'_, detection::fake::FakeDetectorHandle>,
) -> Result<(), String> {
    handle.apply(&state, title.as_deref())
}

#[tauri::command]
fn get_transcription_status() -> TranscriptionStatus {
    TranscriptionStatus {
        chunks_in_queue: 0,
        is_processing: false,
        last_activity_ms: 0,
    }
}

#[tauri::command]
fn read_audio_file(file_path: String) -> Result<Vec<u8>, String> {
    match std::fs::read(&file_path) {
        Ok(data) => Ok(data),
        Err(e) => Err(format!("Failed to read audio file: {}", e)),
    }
}

#[tauri::command]
async fn save_transcript(file_path: String, content: String) -> Result<(), String> {
    log_info!("Saving transcript to: {}", file_path);

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&file_path).parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }
    }

    // Write content to file
    std::fs::write(&file_path, content)
        .map_err(|e| format!("Failed to write transcript: {}", e))?;

    log_info!("Transcript saved successfully");
    Ok(())
}

// Audio level monitoring commands
#[tauri::command]
async fn start_audio_level_monitoring<R: Runtime>(
    app: AppHandle<R>,
    device_names: Vec<String>,
) -> Result<(), String> {
    log_info!(
        "Starting audio level monitoring for devices: {:?}",
        device_names
    );

    audio::simple_level_monitor::start_monitoring(app, device_names)
        .await
        .map_err(|e| format!("Failed to start audio level monitoring: {}", e))
}

#[tauri::command]
async fn stop_audio_level_monitoring() -> Result<(), String> {
    log_info!("Stopping audio level monitoring");

    audio::simple_level_monitor::stop_monitoring()
        .await
        .map_err(|e| format!("Failed to stop audio level monitoring: {}", e))
}

#[tauri::command]
async fn is_audio_level_monitoring() -> bool {
    audio::simple_level_monitor::is_monitoring()
}

// Analytics commands are now handled by analytics::commands module

// Whisper commands are now handled by whisper_engine::commands module

#[tauri::command]
async fn get_audio_devices() -> Result<Vec<AudioDevice>, String> {
    list_audio_devices()
        .await
        .map_err(|e| format!("Failed to list audio devices: {}", e))
}

#[tauri::command]
async fn trigger_microphone_permission() -> Result<bool, String> {
    trigger_audio_permission()
        .map_err(|e| format!("Failed to trigger microphone permission: {}", e))
}

#[tauri::command]
async fn start_recording_with_devices<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
) -> Result<audio::recording_commands::StartRecordingResult, String> {
    start_recording_with_devices_and_meeting(app, mic_device_name, system_device_name, None, None).await
}

#[tauri::command]
async fn start_recording_with_devices_and_meeting<R: Runtime>(
    app: AppHandle<R>,
    mic_device_name: Option<String>,
    system_device_name: Option<String>,
    meeting_name: Option<String>,
    // `Some(true)` marks a detector-started recording so the record-start toast reads
    // "Meeting detected — recording: <title>" instead of the manual wording. Threaded
    // from the frontend auto-detect hook (useAutoDetect) through the recording service.
    detector_started: Option<bool>,
) -> Result<audio::recording_commands::StartRecordingResult, String> {
    log_info!("🚀 CALLED start_recording_with_devices_and_meeting - Mic: {:?}, System: {:?}, Meeting: {:?}, detector_started: {:?}",
             mic_device_name, system_device_name, meeting_name, detector_started);

    // Clone meeting_name for notification use later
    let meeting_name_for_notification = meeting_name.clone();

    // Call the recording module functions that support meeting names
    let recording_result = match (mic_device_name.clone(), system_device_name.clone()) {
        (None, None) => {
            log_info!(
                "No devices specified, starting with defaults and meeting: {:?}",
                meeting_name
            );
            audio::recording_commands::start_recording_with_meeting_name(app.clone(), meeting_name)
                .await
        }
        _ => {
            log_info!(
                "Starting with specified devices: mic={:?}, system={:?}, meeting={:?}",
                mic_device_name,
                system_device_name,
                meeting_name
            );
            audio::recording_commands::start_recording_with_devices_and_meeting(
                app.clone(),
                mic_device_name,
                system_device_name,
                meeting_name,
            )
            .await
        }
    };

    let source = if detector_started == Some(true) {
        notifications::types::RecordStartSource::Detector
    } else {
        notifications::types::RecordStartSource::Manual
    };

    match recording_result {
        Ok(result) => {
            log_info!("Recording started successfully via tauri command, meeting_id: {}", result.meeting_id);

            // Show recording started notification through NotificationManager
            // This respects user's notification preferences
            let notification_manager_state = app.state::<NotificationManagerState<R>>();
            if let Err(e) = notifications::commands::show_recording_started_notification(
                &app,
                &notification_manager_state,
                meeting_name_for_notification.clone(),
                source,
            )
            .await
            {
                log_error!(
                    "Failed to show recording started notification: {}",
                    e
                );
            }

            Ok(result)
        }
        Err(e) => {
            log_error!("Failed to start recording via tauri command: {}", e);
            Err(e)
        }
    }
}

#[tauri::command]
async fn set_language_preference(language: String) -> Result<(), String> {
    let mut lang_pref = LANGUAGE_PREFERENCE
        .lock()
        .map_err(|e| format!("Failed to set language preference: {}", e))?;
    log_info!("Setting language preference to: {}", language);
    *lang_pref = language;
    Ok(())
}

// Internal helper function to get language preference (for use within Rust code)
pub fn get_language_preference_internal() -> Option<String> {
    LANGUAGE_PREFERENCE.lock().ok().map(|lang| lang.clone())
}

/// Read all transcript text from a `transcripts.json` file produced by `write_transcripts_json`.
async fn read_transcript_text(path: &std::path::Path) -> anyhow::Result<String> {
    let content = tokio::fs::read_to_string(path).await?;
    let root: serde_json::Value = serde_json::from_str(&content)?;
    let segments = root
        .get("segments")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("missing segments array"))?;
    let text = segments
        .iter()
        .filter_map(|s| s.get("text").and_then(|t| t.as_str()))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(text)
}

// ── Queue Tauri commands (task 8.1) ───────────────────────────────────────────

#[tauri::command]
async fn pause_all_background_work<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, TranscriptionQueueState>,
) -> Result<(), String> {
    use tauri::Emitter;
    state.pause_all().await;
    // Emit a fresh snapshot so the UI reflects the paused state immediately;
    // the worker-loop notifier only fires on job-status transitions and won't
    // cover the manual-pause case where no job is mid-flight.
    let snapshot = state.get_state().await;
    let _ = app.emit("transcription-queue-changed", &snapshot);
    Ok(())
}

#[tauri::command]
async fn resume_all_background_work<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, TranscriptionQueueState>,
) -> Result<(), String> {
    use tauri::Emitter;
    state.resume_all().await;
    let snapshot = state.get_state().await;
    let _ = app.emit("transcription-queue-changed", &snapshot);
    Ok(())
}

#[tauri::command]
async fn get_queue_state(
    state: tauri::State<'_, TranscriptionQueueState>,
) -> Result<use_cases::transcription_queue::QueueSnapshot, String> {
    Ok(state.get_state().await)
}

#[tauri::command]
async fn cancel_queued_job<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, TranscriptionQueueState>,
    meeting_id: String,
) -> Result<(), String> {
    use tauri::Emitter;
    state.cancel(&meeting_id).await;
    // Emit the updated snapshot so the frontend badge disappears immediately.
    let snapshot = state.get_state().await;
    let _ = app.emit("transcription-queue-changed", &snapshot);
    Ok(())
}

/// Enqueue a transcription job. Called by the frontend after saveMeeting returns the UUID,
/// and by the recovery modal on restart. No existence check: when called after a fresh
/// recording, the MP4 is being finalised concurrently; the recording gate (cleared by
/// background_shutdown after save completes) prevents the worker from starting until the
/// file is on disk.
#[tauri::command]
async fn enqueue_transcription_job<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, TranscriptionQueueState>,
    meeting_id: String,
    audio_path: String,
) -> Result<(), String> {
    use tauri::Emitter;
    log::info!("enqueue_transcription_job: meeting_id={} audio_path={}", meeting_id, audio_path);
    let path = std::path::PathBuf::from(&audio_path);
    state.enqueue(meeting_id, path).await;
    let snapshot = state.get_state().await;
    let _ = app.emit("transcription-queue-changed", &snapshot);
    log::info!("enqueue_transcription_job: job enqueued successfully");
    Ok(())
}

/// Force-start a specific transcription job, bypassing the manual-mode
/// auto-resume gate.  The job still respects recording_active,
/// meeting_detected, and manual_pause gates.
#[tauri::command]
async fn run_transcription_job_now<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: tauri::State<'_, TranscriptionQueueState>,
    meeting_id: String,
) -> Result<bool, String> {
    use tauri::Emitter;
    let started = state.run_job_now(&meeting_id).await;
    if started {
        let snapshot = state.get_state().await;
        let _ = app.emit("transcription-queue-changed", &snapshot);
    }
    Ok(started)
}

pub fn run() {
    // Max level is set by env_logger (via RUST_LOG) in main.rs — don't override it here.

    tauri::Builder::default()
        // Registered first on purpose: this plugin decides whether this process is the
        // authoritative instance or a re-activation that must hand its argv to the
        // running instance and exit. Toast action buttons re-launch the app via
        // meetily://; without this, every tap spawns a fresh instance that cannot see
        // the live recording. The callback re-dispatches any meetily:// URI through the
        // same handle_deep_link path the deep-link plugin uses on cold start.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if let Some(uri) = use_cases::notification_action::extract_meetily_uri(&argv) {
                handle_deep_link(app, uri);
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(whisper_engine::parallel_commands::ParallelProcessorState::new())
        .manage(Arc::new(RwLock::new(
            None::<notifications::manager::NotificationManager<tauri::Wry>>,
        )) as NotificationManagerState<tauri::Wry>)
        .manage(audio::init_system_audio_state())
        .manage(summary::summary_engine::ModelManagerState(Arc::new(tokio::sync::Mutex::new(None))))
        .setup(|_app| {
            log::info!("Application setup complete");

            // Register the meetily:// scheme with the OS so toast action buttons can
            // re-activate the running instance. Idempotent; a registry write failure is
            // non-fatal (toasts still render, just without working buttons). Cold-start
            // detection (handle_cli_arguments) already ran during plugin init.
            #[cfg(target_os = "windows")]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                if let Err(e) = _app.deep_link().register_all() {
                    log::warn!("deep-link scheme registration failed: {e:?}");
                }
            }

            // Route meetily:// activations through the dispatch use case. on_open_url
            // covers warm activations (button tap while running); get_current covers a URI
            // that cold-started the app (the plugin parsed it during its own init, so we
            // re-dispatch here — the guard turns a no-recording stop/continue into a no-op).
            #[cfg(target_os = "windows")]
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                let handle = _app.handle().clone();
                _app.deep_link().on_open_url(move |event| {
                    for url in event.urls() {
                        handle_deep_link(&handle, url.as_str());
                    }
                });
                if let Ok(Some(urls)) = _app.deep_link().get_current() {
                    let handle = _app.handle().clone();
                    for url in urls {
                        handle_deep_link(&handle, url.as_str());
                    }
                }
            }

            // Wire the transcription queue with production processors.
            {
                use audio::retranscription::YIELD_SENTINEL;
                use use_cases::scheduler_settings::{SchedulerLiveConfig, load_scheduler_settings};
                use use_cases::transcription_queue::{JobResult, ProcessorFn, StateChangeNotifier, TranscriptionQueue};

                // Load scheduler settings from store (async, so block_on).
                let app_handle = _app.handle().clone();
                let settings = tauri::async_runtime::block_on(async {
                    load_scheduler_settings(&app_handle).await
                });
                let live_config = Arc::new(SchedulerLiveConfig::from_settings(&settings));
                _app.manage(live_config.clone() as SchedulerConfigState);

                let retranscription_processor: ProcessorFn = {
                    let app = _app.handle().clone();
                    Arc::new(move |meeting_id: String, audio_path: std::path::PathBuf| {
                        let app = app.clone();
                        Box::pin(async move {
                            let folder = match audio_path.parent() {
                                Some(p) => p.to_string_lossy().to_string(),
                                None => return JobResult::Failed("invalid audio path".to_string()),
                            };
                            match audio::retranscription::start_retranscription(
                                app.clone(), meeting_id, folder, None, None, None,
                            )
                            .await
                            {
                                Ok(_) => {
                                    // Chain to summary only if an LLM provider is configured.
                                    use crate::database::repositories::setting::SettingsRepository;
                                    let has_provider = if let Some(s) = app.try_state::<state::AppState>() {
                                        let pool = s.db_manager.pool().clone();
                                        matches!(
                                            SettingsRepository::get_model_config(&pool).await,
                                            Ok(Some(c)) if !c.provider.is_empty()
                                        )
                                    } else {
                                        false
                                    };
                                    if has_provider {
                                        JobResult::CompletedChain
                                    } else {
                                        JobResult::Completed
                                    }
                                }
                                Err(e) if e.to_string().contains(YIELD_SENTINEL) => {
                                    JobResult::Yielded
                                }
                                Err(e) => JobResult::Failed(e.to_string()),
                            }
                        })
                    })
                };

                let summary_processor: ProcessorFn = {
                    let app = _app.handle().clone();
                    Arc::new(move |meeting_id: String, audio_path: std::path::PathBuf| {
                        let app = app.clone();
                        Box::pin(async move {
                            use crate::database::repositories::setting::SettingsRepository;
                            use tauri::Emitter;

                            let _ = app.emit("retranscription-progress", serde_json::json!({
                                "meeting_id": meeting_id,
                                "stage": "summarising",
                                "progress_percentage": 0,
                                "message": "Generating summary…"
                            }));

                            let (pool, config) = {
                                let app_state = match app.try_state::<state::AppState>() {
                                    Some(s) => s,
                                    None => return JobResult::Completed,
                                };
                                let pool = app_state.db_manager.pool().clone();
                                let config = match SettingsRepository::get_model_config(&pool).await {
                                    Ok(Some(c)) if !c.provider.is_empty() => c,
                                    _ => return JobResult::Completed,
                                };
                                (pool, config)
                            };

                            let folder = match audio_path.parent() {
                                Some(p) => p.to_path_buf(),
                                None => return JobResult::Completed,
                            };
                            let transcript_path = folder.join("transcripts.json");
                            let text = match read_transcript_text(&transcript_path).await {
                                Ok(t) if !t.is_empty() => t,
                                _ => return JobResult::Completed,
                            };

                            summary::SummaryService::process_transcript_background(
                                app,
                                pool,
                                meeting_id,
                                text,
                                config.provider,
                                config.model,
                                String::new(),
                                "daily_standup".to_string(),
                            )
                            .await;
                            JobResult::Completed
                        })
                    })
                };

                let queue = Arc::new(TranscriptionQueue::with_processors_and_config(
                    retranscription_processor,
                    Some(summary_processor),
                    live_config,
                ));
                _app.manage(queue.clone() as TranscriptionQueueState);

                let app_for_notifier = _app.handle().clone();
                let notifier: StateChangeNotifier = Arc::new(move |snapshot| {
                    use tauri::Emitter;
                    let _ = app_for_notifier.emit("transcription-queue-changed", &snapshot);
                });
                let _worker_handle = queue.spawn_worker_with_notifier(Some(notifier));
                log::info!("Transcription queue worker started");

                // Sysinfo CPU/RAM polling — feeds the scheduler hysteresis gates every 5 s.
                // The first sysinfo sample after a refresh_cpu_usage() call is always 0.0;
                // a 200 ms warm-up sleep is done before entering the loop so the first
                // real sample is accurate.  An initial 0.0 sample maps to "clear" which
                // is safe (the threshold is 70 % CPU / 80 % RAM).
                let scheduler = Arc::clone(&queue.scheduler);
                tauri::async_runtime::spawn(async move {
                    use sysinfo::System;
                    let mut sys = System::new_all();
                    sys.refresh_cpu_usage();
                    tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
                    loop {
                        sys.refresh_cpu_usage();
                        sys.refresh_memory();
                        let cpu_pct = sys.global_cpu_usage() as f64;
                        let ram_pct = if sys.total_memory() > 0 {
                            sys.used_memory() as f64 / sys.total_memory() as f64 * 100.0
                        } else {
                            0.0
                        };
                        scheduler.feed_cpu_sample(cpu_pct);
                        scheduler.feed_ram_sample(ram_pct);
                        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    }
                });
                log::info!("Sysinfo CPU/RAM scheduler polling started");
            }

            // Initialize system tray
            if let Err(e) = tray::create_tray(_app.handle()) {
                log::error!("Failed to create system tray: {}", e);
            }

            // Initialize notification system with proper defaults
            log::info!("Initializing notification system...");
            let app_for_notif = _app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let notif_state = app_for_notif.state::<NotificationManagerState<tauri::Wry>>();
                match notifications::commands::initialize_notification_manager(app_for_notif.clone()).await {
                    Ok(manager) => {
                        // Set default consent and permissions on first launch
                        if let Err(e) = manager.set_consent(true).await {
                            log::error!("Failed to set initial consent: {}", e);
                        }
                        if let Err(e) = manager.request_permission().await {
                            log::error!("Failed to request initial permission: {}", e);
                        }

                        // Store the initialized manager
                        let mut state_lock = notif_state.write().await;
                        *state_lock = Some(manager);
                        log::info!("Notification system initialized with default permissions");
                    }
                    Err(e) => {
                        log::error!("Failed to initialize notification manager: {}", e);
                    }
                }
            });

            // Set models directory to use app_data_dir (unified storage location)
            whisper_engine::commands::set_models_directory(&_app.handle());

            // Initialize Whisper engine on startup
            tauri::async_runtime::spawn(async {
                if let Err(e) = whisper_engine::commands::whisper_init().await {
                    log::error!("Failed to initialize Whisper engine on startup: {}", e);
                }
            });

            // Set Parakeet models directory
            parakeet_engine::commands::set_models_directory(&_app.handle());

            // Initialize Parakeet engine on startup
            tauri::async_runtime::spawn(async {
                if let Err(e) = parakeet_engine::commands::parakeet_init().await {
                    log::error!("Failed to initialize Parakeet engine on startup: {}", e);
                }
            });

            // Initialize ModelManager for summary engine (async, non-blocking)
            let app_handle_for_model_manager = _app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match summary::summary_engine::commands::init_model_manager_at_startup(&app_handle_for_model_manager).await {
                    Ok(_) => log::info!("ModelManager initialized successfully at startup"),
                    Err(e) => {
                        log::warn!("Failed to initialize ModelManager at startup: {}", e);
                        log::warn!("ModelManager will be lazy-initialized on first use");
                    }
                }
            });

            let app_for_gc_and_detector = _app.handle().clone();
            tauri::async_runtime::spawn(async move {
                // ── GC pass ────────────────────────────────────────────────
                match database::manager::DatabaseManager::new_from_app_handle(&app_for_gc_and_detector).await {
                    Ok(db) => {
                        if let Ok(data_dir) = app_for_gc_and_detector.path().app_data_dir() {
                            let recordings_dir = data_dir.join("recordings");
                            let report = use_cases::recording_gc::run_startup_gc(&db, &recordings_dir).await;
                            log::info!(
                                "startup gc: orphan_rows={} orphan_files={} errors={}",
                                report.orphan_rows_deleted,
                                report.orphan_files_deleted,
                                report.errors.len()
                            );
                            for err in &report.errors {
                                log::warn!("startup gc error: {}", err);
                            }
                        }
                    }
                    Err(e) => log::warn!("startup gc: failed to open db: {}", e),
                }

                // ── Meeting detector ───────────────────────────────────────
                #[cfg(target_os = "windows")]
                {
                    use std::sync::atomic::AtomicBool;
                    use std::sync::Arc;
                    use std::time::Duration;
                    use use_cases::meeting_detection::{DetectorSettings, spawn_detector, DetectorEventEmitter};
                    use tauri::Emitter;

                    let auto_detect = {
                        use tauri_plugin_store::StoreExt;
                        app_for_gc_and_detector
                            .store("settings.json")
                            .ok()
                            .and_then(|s| s.get("autoDetectMeetings").and_then(|v| v.as_bool()))
                            .unwrap_or(true)
                    };

                    // Always register the suppress signal so signal_cancel_detection command
                    // can find it in app state regardless of whether auto-detect is enabled.
                    let suppress_signal = Arc::new(AtomicBool::new(false));
                    app_for_gc_and_detector.manage(SuppressDetectionSignal(suppress_signal.clone()));

                    if auto_detect {
                        #[cfg(not(feature = "dev-detector"))]
                        let detector = {
                            use detection::windows::{FocusHistory, WindowsMeetingDetector, spawn_focus_tracker};
                            let focus_history: FocusHistory = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
                            let _focus_task = spawn_focus_tracker(focus_history.clone());
                            WindowsMeetingDetector::new(focus_history.clone())
                        };
                        #[cfg(feature = "dev-detector")]
                        let detector = {
                            let fake = detection::fake::FakeMeetingDetector::new();
                            app_for_gc_and_detector
                                .manage(detection::fake::FakeDetectorHandle(fake.handle()));
                            fake
                        };

                        struct AppEmitter<R: tauri::Runtime>(tauri::AppHandle<R>);
                        impl<R: tauri::Runtime> DetectorEventEmitter for AppEmitter<R> {
                            fn emit_detected(&self, default_title: String, candidate_titles: Vec<String>) {
                                use std::sync::atomic::Ordering;
                                use use_cases::transcription_queue::SHOULD_YIELD;

                                // Pause background transcription while a meeting is active.
                                if let Some(q) = self.0.try_state::<TranscriptionQueueState>() {
                                    q.scheduler.meeting_busy.store(true, Ordering::SeqCst);
                                    SHOULD_YIELD.store(true, Ordering::SeqCst);
                                }

                                // Detection surfaces only the in-app banner (the meeting-detected
                                // event below). The actionable toast fires once at record-start,
                                // not here — firing at detection time produced a premature,
                                // semantically-wrong notification (see notification-actions spec).
                                let stripped: Vec<String> = candidate_titles
                                    .into_iter()
                                    .map(|t| crate::detection::windows::strip_google_meet_suffix(&t))
                                    .collect();
                                let _ = self.0.emit("meeting-detected", serde_json::json!({
                                    "default_title": default_title,
                                    "candidate_titles": stripped,
                                }));
                            }
                            fn emit_ended(&self) {
                                use std::sync::atomic::Ordering;

                                // Clear the meeting gate. Only resume the worker if the user
                                // has not deliberately paused all background work — otherwise
                                // resume_all() would silently clear manual_pause_all.
                                if let Some(q) = self.0.try_state::<TranscriptionQueueState>() {
                                    q.scheduler.meeting_busy.store(false, Ordering::SeqCst);
                                    if !q.scheduler.manual_pause_all.load(Ordering::SeqCst) {
                                        let q_arc = q.inner().clone();
                                        tauri::async_runtime::spawn(async move {
                                            q_arc.resume_all().await;
                                        });
                                    }
                                }

                                let handler = crate::notifications::system::SystemNotificationHandler::new(self.0.clone());
                                let notif = crate::notifications::types::Notification::new(
                                    "Meetily — Meeting ended",
                                    "Recording will stop in 10 seconds. Switch to Meetily to keep it running.",
                                    crate::notifications::types::NotificationType::RecordingStopped,
                                );
                                tauri::async_runtime::spawn(async move {
                                    if let Err(e) = handler.show_notification(notif).await {
                                        log::warn!("recording-ended notification failed: {}", e);
                                    }
                                });
                                let _ = self.0.emit("meeting-ended", ());
                            }
                        }

                        let emitter = AppEmitter(app_for_gc_and_detector.clone());
                        let _detector_handle = spawn_detector(
                            detector,
                            emitter,
                            Duration::from_secs(2),
                            DetectorSettings::default(),
                            suppress_signal,
                        );

                        log::info!("meeting detector started");
                    } else {
                        log::info!("auto-detect disabled by settings");
                    }
                }
            });

            // Trigger system audio permission request on startup (similar to microphone permission)
            // #[cfg(target_os = "macos")]
            // {
            //     tauri::async_runtime::spawn(async {
            //         if let Err(e) = audio::permissions::trigger_system_audio_permission() {
            //             log::warn!("Failed to trigger system audio permission: {}", e);
            //         }
            //     });
            // }

            // Initialize database (handles first launch detection and conditional setup)
            tauri::async_runtime::block_on(async {
                database::setup::initialize_database_on_startup(&_app.handle()).await
            })
            .expect("Failed to initialize database");

            // Initialize bundled templates directory for dynamic template discovery
            log::info!("Initializing bundled templates directory...");
            if let Ok(resource_path) = _app.handle().path().resource_dir() {
                let templates_dir = resource_path.join("templates");
                log::info!("Setting bundled templates directory to: {:?}", templates_dir);
                summary::templates::set_bundled_templates_dir(templates_dir);
            } else {
                log::warn!("Failed to resolve resource directory for templates");
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_recording,
            stop_recording,
            cancel_recording,
            set_active_meeting_name,
            signal_cancel_detection,
            #[cfg(feature = "dev-detector")]
            __dev_simulate_meeting,
            #[cfg(debug_assertions)]
            __dev_inject_deep_link,
            is_recording,
            get_transcription_status,
            read_audio_file,
            save_transcript,
            analytics::commands::init_analytics,
            analytics::commands::disable_analytics,
            analytics::commands::track_event,
            analytics::commands::identify_user,
            analytics::commands::track_meeting_started,
            analytics::commands::track_recording_started,
            analytics::commands::track_recording_stopped,
            analytics::commands::track_meeting_deleted,
            analytics::commands::track_settings_changed,
            analytics::commands::track_feature_used,
            analytics::commands::is_analytics_enabled,
            analytics::commands::start_analytics_session,
            analytics::commands::end_analytics_session,
            analytics::commands::track_daily_active_user,
            analytics::commands::track_user_first_launch,
            analytics::commands::is_analytics_session_active,
            analytics::commands::track_summary_generation_started,
            analytics::commands::track_summary_generation_completed,
            analytics::commands::track_summary_regenerated,
            analytics::commands::track_model_changed,
            analytics::commands::track_custom_prompt_used,
            analytics::commands::track_meeting_ended,
            analytics::commands::track_analytics_enabled,
            analytics::commands::track_analytics_disabled,
            analytics::commands::track_analytics_transparency_viewed,
            whisper_engine::commands::whisper_init,
            whisper_engine::commands::whisper_get_available_models,
            whisper_engine::commands::whisper_load_model,
            whisper_engine::commands::whisper_get_current_model,
            whisper_engine::commands::whisper_is_model_loaded,
            whisper_engine::commands::whisper_has_available_models,
            whisper_engine::commands::whisper_validate_model_ready,
            whisper_engine::commands::whisper_transcribe_audio,
            whisper_engine::commands::whisper_get_models_directory,
            whisper_engine::commands::whisper_download_model,
            whisper_engine::commands::whisper_cancel_download,
            whisper_engine::commands::whisper_delete_corrupted_model,
            // Parakeet engine commands
            parakeet_engine::commands::parakeet_init,
            parakeet_engine::commands::parakeet_get_available_models,
            parakeet_engine::commands::parakeet_load_model,
            parakeet_engine::commands::parakeet_get_current_model,
            parakeet_engine::commands::parakeet_is_model_loaded,
            parakeet_engine::commands::parakeet_has_available_models,
            parakeet_engine::commands::parakeet_validate_model_ready,
            parakeet_engine::commands::parakeet_transcribe_audio,
            parakeet_engine::commands::parakeet_get_models_directory,
            parakeet_engine::commands::parakeet_download_model,
            parakeet_engine::commands::parakeet_retry_download,
            parakeet_engine::commands::parakeet_cancel_download,
            parakeet_engine::commands::parakeet_delete_corrupted_model,
            parakeet_engine::commands::open_parakeet_models_folder,
            // Parallel processing commands
            whisper_engine::parallel_commands::initialize_parallel_processor,
            whisper_engine::parallel_commands::start_parallel_processing,
            whisper_engine::parallel_commands::pause_parallel_processing,
            whisper_engine::parallel_commands::resume_parallel_processing,
            whisper_engine::parallel_commands::stop_parallel_processing,
            whisper_engine::parallel_commands::get_parallel_processing_status,
            whisper_engine::parallel_commands::get_system_resources,
            whisper_engine::parallel_commands::check_resource_constraints,
            whisper_engine::parallel_commands::calculate_optimal_workers,
            whisper_engine::parallel_commands::prepare_audio_chunks,
            whisper_engine::parallel_commands::test_parallel_processing_setup,
            get_audio_devices,
            trigger_microphone_permission,
            start_recording_with_devices,
            start_recording_with_devices_and_meeting,
            start_audio_level_monitoring,
            stop_audio_level_monitoring,
            is_audio_level_monitoring,
            // Recording pause/resume commands
            audio::recording_commands::pause_recording,
            audio::recording_commands::resume_recording,
            audio::recording_commands::is_recording_paused,
            audio::recording_commands::get_recording_state,
            audio::recording_commands::get_meeting_folder_path,
            // Reload sync commands (retrieve transcript history and meeting name)
            audio::recording_commands::get_transcript_history,
            audio::recording_commands::get_recording_meeting_name,
            // Device monitoring commands (AirPods/Bluetooth disconnect/reconnect)
            audio::recording_commands::poll_audio_device_events,
            audio::recording_commands::get_reconnection_status,
            audio::recording_commands::attempt_device_reconnect,
            // Playback device detection (Bluetooth warning)
            audio::recording_commands::get_active_audio_output,
            console_utils::show_console,
            console_utils::hide_console,
            console_utils::toggle_console,
            ollama::get_ollama_models,
            ollama::pull_ollama_model,
            ollama::delete_ollama_model,
            ollama::get_ollama_model_context,
            openai::openai::get_openai_models,
            anthropic::anthropic::get_anthropic_models,
            groq::groq::get_groq_models,
            api::api_get_meetings,
            api::api_search_transcripts,
            api::api_get_profile,
            api::api_save_profile,
            api::api_update_profile,
            api::api_get_model_config,
            api::api_save_model_config,
            api::api_get_api_key,
            // api::api_get_auto_generate_setting,
            // api::api_save_auto_generate_setting,
            api::api_get_transcript_config,
            api::api_save_transcript_config,
            api::api_get_transcript_api_key,
            api::api_delete_meeting,
            api::api_get_meeting,
            api::api_get_meeting_metadata,
            api::api_get_meeting_transcripts,
            api::api_save_meeting_title,
            api::api_save_transcript,
            api::open_meeting_folder,
            api::test_backend_connection,
            api::debug_backend_connection,
            api::open_external_url,
            // Custom OpenAI commands
            api::api_save_custom_openai_config,
            api::api_get_custom_openai_config,
            api::api_test_custom_openai_connection,
            // Summary commands
            summary::api_process_transcript,
            summary::api_get_summary,
            summary::api_save_meeting_summary,
            summary::api_cancel_summary,
            // Template commands
            summary::api_list_templates,
            summary::api_get_template_details,
            summary::api_validate_template,
            // Built-in AI commands
            summary::summary_engine::builtin_ai_list_models,
            summary::summary_engine::builtin_ai_get_model_info,
            summary::summary_engine::builtin_ai_download_model,
            summary::summary_engine::builtin_ai_cancel_download,
            summary::summary_engine::builtin_ai_delete_model,
            summary::summary_engine::builtin_ai_is_model_ready,
            summary::summary_engine::builtin_ai_get_available_summary_model,
            summary::summary_engine::builtin_ai_get_recommended_model,
            openrouter::get_openrouter_models,
            audio::recording_preferences::get_recording_preferences,
            audio::recording_preferences::set_recording_preferences,
            audio::recording_preferences::get_default_recordings_folder_path,
            audio::recording_preferences::open_recordings_folder,
            audio::recording_preferences::select_recording_folder,
            audio::recording_preferences::get_available_audio_backends,
            audio::recording_preferences::get_current_audio_backend,
            audio::recording_preferences::set_audio_backend,
            audio::recording_preferences::get_audio_backend_info,
            // Language preference commands
            set_language_preference,
            // Notification system commands
            notifications::commands::get_notification_settings,
            notifications::commands::set_notification_settings,
            notifications::commands::request_notification_permission,
            notifications::commands::show_notification,
            notifications::commands::show_test_notification,
            notifications::commands::is_dnd_active,
            notifications::commands::get_system_dnd_status,
            notifications::commands::set_manual_dnd,
            notifications::commands::set_notification_consent,
            notifications::commands::clear_notifications,
            notifications::commands::is_notification_system_ready,
            notifications::commands::initialize_notification_manager_manual,
            notifications::commands::test_notification_with_auto_consent,
            notifications::commands::get_notification_stats,
            // System audio capture commands
            audio::system_audio_commands::start_system_audio_capture_command,
            audio::system_audio_commands::list_system_audio_devices_command,
            audio::system_audio_commands::check_system_audio_permissions_command,
            audio::system_audio_commands::start_system_audio_monitoring,
            audio::system_audio_commands::stop_system_audio_monitoring,
            audio::system_audio_commands::get_system_audio_monitoring_status,
            // Screen Recording permission commands
            audio::permissions::check_screen_recording_permission_command,
            audio::permissions::request_screen_recording_permission_command,
            audio::permissions::trigger_system_audio_permission_command,
            // Database import commands
            database::commands::check_first_launch,
            database::commands::select_legacy_database_path,
            database::commands::detect_legacy_database,
            database::commands::check_default_legacy_database,
            database::commands::check_homebrew_database,
            database::commands::import_and_initialize_database,
            database::commands::initialize_fresh_database,
            // Database and Models path commands
            database::commands::get_database_directory,
            database::commands::open_database_folder,
            whisper_engine::commands::open_models_folder,
            // Onboarding commands
            onboarding::get_onboarding_status,
            onboarding::save_onboarding_status_cmd,
            onboarding::reset_onboarding_status_cmd,
            onboarding::complete_onboarding,
            // System settings commands
            #[cfg(target_os = "macos")]
            utils::open_system_settings,
            // Transcription queue commands (tasks 8.1, 9.3)
            pause_all_background_work,
            resume_all_background_work,
            get_queue_state,
            cancel_queued_job,
            enqueue_transcription_job,
            run_transcription_job_now,
            // Scheduler settings commands
            use_cases::scheduler_settings::get_scheduler_settings,
            use_cases::scheduler_settings::save_scheduler_settings_cmd,
            // Retranscription commands
            audio::retranscription::start_retranscription_command,
            audio::retranscription::cancel_retranscription_command,
            audio::retranscription::is_retranscription_in_progress_command,
            // Import audio commands
            audio::import::select_and_validate_audio_command,
            audio::import::validate_audio_file_command,
            audio::import::start_import_audio_command,
            audio::import::cancel_import_command,
            audio::import::is_import_in_progress_command,
            // Speaker labeling commands
            audio::speaker::commands::label_speaker,
            audio::speaker::commands::list_speakers_cmd,
            audio::speaker::commands::remove_speaker_cmd,
            audio::speaker::commands::rediarize_meeting,
            audio::speaker::commands::reset_speaker_labels,
            audio::speaker::commands::revert_speaker_label,
            audio::speaker::commands::set_segment_speaker,
            audio::speaker::commands::get_speaker_merge_threshold,
            audio::speaker::commands::set_speaker_merge_threshold,
            audio::speaker::commands::get_max_speakers,
            audio::speaker::commands::set_max_speakers,
            audio::speaker::commands::get_meeting_max_speakers,
            audio::speaker::commands::set_meeting_max_speakers,
            audio::speaker::model_download::download_speaker_models,
            audio::speaker::model_download::check_speaker_models_available,
            audio::speaker::commands::get_diarization_enabled,
            audio::speaker::commands::set_diarization_enabled,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|_app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                log::info!("Application exiting, cleaning up resources...");
                tauri::async_runtime::block_on(async {
                    // Clean up database connection and checkpoint WAL
                    if let Some(app_state) = _app_handle.try_state::<state::AppState>() {
                        log::info!("Starting database cleanup...");
                        if let Err(e) = app_state.db_manager.cleanup().await {
                            log::error!("Failed to cleanup database: {}", e);
                        } else {
                            log::info!("Database cleanup completed successfully");
                        }
                    } else {
                        log::warn!("AppState not available for database cleanup (likely first launch)");
                    }

                    // Clean up sidecar
                    log::info!("Cleaning up sidecar...");
                    if let Err(e) = summary::summary_engine::force_shutdown_sidecar().await {
                        log::error!("Failed to force shutdown sidecar: {}", e);
                    }
                });
                log::info!("Application cleanup complete");
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::recording_commands::{set_phase, RecordingPhase};
    use use_cases::notification_action::RecordingStatePort;

    // Restores RecordingPhase to Idle on scope exit (including panic) so one test's
    // phase mutation cannot leak into another. The global static makes fully-isolated
    // tests impossible without serial_test, but this guard keeps the window minimal.
    struct PhaseGuard;
    impl Drop for PhaseGuard {
        fn drop(&mut self) {
            set_phase(RecordingPhase::Idle);
        }
    }

    // This test would have caught C1-correctness: LiveRecordingState must reflect the
    // authoritative RecordingPhase, not a stale flag set by only one start path.
    #[test]
    fn live_recording_state_reflects_authoritative_phase() {
        let _guard = PhaseGuard;

        set_phase(RecordingPhase::Recording);
        assert!(
            LiveRecordingState.is_recording(),
            "LiveRecordingState must report recording when RecordingPhase == Recording"
        );

        set_phase(RecordingPhase::Idle);
        assert!(
            !LiveRecordingState.is_recording(),
            "LiveRecordingState must report idle when RecordingPhase == Idle"
        );

        set_phase(RecordingPhase::Saving);
        assert!(
            !LiveRecordingState.is_recording(),
            "LiveRecordingState must report idle during Saving (not Recording)"
        );
    }
}

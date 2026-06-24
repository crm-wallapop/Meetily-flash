// Init script that registers fixture-backed handlers for every Tauri command
// the app calls during page load + the recording lifecycle. Added AFTER
// TAURI_MOCK_INIT_SCRIPT so the dispatcher exists. Without these, the fail-closed
// dispatcher throws on every load command and the app falls back to the
// onboarding screen (the default when get_onboarding_status rejects).
//
// Stateful: __smokeMeetings starts empty and stop_recording appends the
// just-recorded meeting so the sidebar assertion can see it appear.

export const SMOKE_MEETING_ID = 'meet-smoke-001';
export const SMOKE_MEETING_TITLE = 'Smoke Test Meeting';

export const SMOKE_DEFAULTS_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;

  window.__smokeMeetings = [];
  // Recording-active flag. The real Rust backend drives RecordingStateContext
  // via the recording-state-changed event (phase: Recording | Saving | Idle);
  // the context derives isRecording from phase, NOT from any invoke return.
  // Without emitting these events, RecordingControls never sees isRecording=true
  // and the stop button never mounts — even though the start invoke resolved.
  window.__smokeRecording = false;
  var bus = window.__tauriMockEventBus;

  d.register('get_onboarding_status', function () {
    return { completed: true, current_step: 4 };
  });
  d.register('check_first_launch', function () { return false; });
  d.register('is_recording', function () { return !!window.__smokeRecording; });
  d.register('get_recording_state', function () {
    return window.__smokeRecording
      ? { phase: 'Recording', is_recording: true, is_paused: false, is_active: true }
      : { phase: 'Idle', is_recording: false, is_paused: false, is_active: false };
  });
  d.register('get_audio_devices', function () { return []; });
  d.register('get_recording_preferences', function () {
    return { preferred_mic_device: '', preferred_system_device: '' };
  });
  d.register('get_queue_state', function () {
    return { jobs: [], manual_pause_all: false };
  });
  d.register('api_get_meetings', function () { return window.__smokeMeetings; });
  d.register('api_get_model_config', function () {
    return { provider: 'ollama', model: '', endpoint: '' };
  });
  d.register('api_get_transcript_config', function () {
    return { provider: 'whisper', model: 'base' };
  });
  d.register('api_get_api_key', function () { return null; });
  d.register('builtin_ai_get_recommended_model', function () { return null; });
  d.register('get_ollama_models', function () { return []; });
  d.register('list_speakers_cmd', function () { return []; });
  d.register('set_language_preference', function () { return null; });
  d.register('plugin:store|load', function () { return null; });

  d.register('start_recording_with_devices_and_meeting', function () {
    window.__smokeRecording = true;
    if (bus) {
      bus.emit('recording-started', {});
      bus.emit('recording-state-changed', { phase: 'Recording' });
    }
    return { meeting_id: '${SMOKE_MEETING_ID}' };
  });
  d.register('start_recording', function () {
    window.__smokeRecording = true;
    if (bus) {
      bus.emit('recording-started', {});
      bus.emit('recording-state-changed', { phase: 'Recording' });
    }
    return { meeting_id: '${SMOKE_MEETING_ID}' };
  });
  d.register('stop_recording', function () {
    window.__smokeRecording = false;
    if (bus) {
      // Saving emits synchronously (matches the real backend: streams release,
      // phase flips to Saving — all before stop_recording returns). The
      // SQLite-save signal fires next so the sidebar refetch sees the row.
      bus.emit('recording-state-changed', { phase: 'Saving' });
      // recording-saved-to-db is what useRecordingStop listens for to call
      // refetchMeetings(); without it the sidebar never re-fetches and the
      // just-recorded meeting stays invisible even though stop_recording resolved.
      bus.emit('recording-saved-to-db', { meeting_id: '${SMOKE_MEETING_ID}' });
      // Idle + recording-stopped fire on a later macrotask. In the real backend
      // this gap is background_shutdown (MP4 flush + SQLite save + phase reset);
      // here it also breaks the React 18 batch so the Saving paint is observable.
      // Default 0 defers by one macrotask (Saving still paints one frame); a spec
      // sets __smokeSavingPhaseMs to hold Saving long enough to assert it.
      var idleDelay = window.__smokeSavingPhaseMs || 0;
      setTimeout(function () {
        bus.emit('recording-state-changed', { phase: 'Idle' });
        bus.emit('recording-stopped', { folder_path: '/tmp/smoke' });
      }, idleDelay);
    }
    window.__smokeMeetings.push({
      id: '${SMOKE_MEETING_ID}',
      title: '${SMOKE_MEETING_TITLE}',
    });
    return { meeting_id: '${SMOKE_MEETING_ID}', folder_path: '/tmp/smoke' };
  });
  d.register('pause_recording', function () { return null; });
  d.register('resume_recording', function () { return null; });
  d.register('start_audio_level_monitoring', function () { return null; });
  d.register('stop_audio_level_monitoring', function () { return null; });
  d.register('enqueue_transcription_job', function () { return null; });
  d.register('pause_all_background_work', function () { return null; });
  d.register('resume_all_background_work', function () { return null; });

  // Notification plugin — showRecordingNotification fires on start; let it no-op.
  d.register('plugin:notification|request_permission', function () {
    return { display: 'granted' };
  });
  d.register('plugin:notification|send_notification', function () { return null; });

  // Analytics track_* calls — fire-and-forget, must not throw.
  d.registerMany({
    track_recording_started: function () { return null; },
    track_recording_stopped: function () { return null; },
    track_meeting_started: function () { return null; },
    track_meeting_deleted: function () { return null; },
    track_event: function () { return null; },
    track_feature_used: function () { return null; },
    track_model_changed: function () { return null; },
    track_daily_active_user: function () { return null; },
    track_user_first_launch: function () { return null; },
  });
})();
`;

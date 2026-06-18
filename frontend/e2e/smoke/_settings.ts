// Init script for the /settings page. Registers the SpeakerSettings commands
// (merge threshold, max speakers, diarization enabled) with call recording so the
// threshold-slider smoke can assert set_speaker_merge_threshold dispatch. Added
// AFTER TAURI_MOCK_INIT_SCRIPT and SMOKE_DEFAULTS_INIT_SCRIPT (the latter covers the
// app-load commands the settings page also fires). NOTE: no backticks inside — this
// is a template literal evaluated verbatim in the page context.

export const SMOKE_SETTINGS_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;

  window.__smokeSettings = window.__smokeSettings || {
    merge_threshold: 0.40,
    max_speakers: 10,
    diarization_enabled: true
  };
  window.__smokeSettingsCalls = window.__smokeSettingsCalls || [];

  d.register('get_speaker_merge_threshold', function () {
    return window.__smokeSettings.merge_threshold;
  });
  d.register('set_speaker_merge_threshold', function (args) {
    window.__smokeSettings.merge_threshold = args.threshold;
    window.__smokeSettingsCalls.push({ cmd: 'set_speaker_merge_threshold', threshold: args.threshold });
    return null;
  });
  d.register('get_max_speakers', function () { return window.__smokeSettings.max_speakers; });
  d.register('set_max_speakers', function (args) {
    window.__smokeSettingsCalls.push({ cmd: 'set_max_speakers', cap: args.cap });
    return null;
  });
  d.register('get_diarization_enabled', function () { return window.__smokeSettings.diarization_enabled; });
  d.register('set_diarization_enabled', function (args) {
    window.__smokeSettings.diarization_enabled = args.enabled;
    window.__smokeSettingsCalls.push({ cmd: 'set_diarization_enabled', enabled: args.enabled });
    return null;
  });
})();
`;

// Init script registering fixture-backed handlers for the meeting-details page
// load + summary fetch. Added AFTER TAURI_MOCK_INIT_SCRIPT and SMOKE_DEFAULTS_INIT_SCRIPT.
//
// The summary data shape targets the page.tsx LEGACY format path (sections with
// title + blocks), which is where the explicit empty-blocks / invalid-blocks
// defensive handling lives (page.tsx ~line 282). That handling IS the regression
// class locked in by task 1.3. The test swaps window.__smokeSummaryData before
// each navigation to exercise multi-block vs empty-blocks.

export const SMOKE_MEETING_DETAILS_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;

  // MeetingMetadata shape consumed by usePaginatedTranscripts.loadMetadata. If this is
  // missing the hook sets transcriptError and the page falls back to "Failed to load
  // meeting details" — which masks any summary-render assertion (the summary never runs).
  // folder_path must be set: TranscriptButtonGroup only renders the Enhance
  // button (and mounts RetranscribeDialog) when meetingFolderPath is truthy,
  // and page.tsx reads it straight off this metadata object.
  window.__smokeMeetingMetadata = {
    id: 'meet-summary-001',
    title: 'Summary Smoke Meeting',
    created_at: '2026-06-17T10:00:00Z',
    updated_at: '2026-06-17T10:30:00Z',
    folder_path: '/smoke/meetings/meet-summary-001'
  };

  // Default summary fixture: multi-block legacy format. Tests override
  // window.__smokeSummaryData via page.addInitScript before navigation.
  window.__smokeSummaryData = {
    _section_order: ['decisions', 'action_items'],
    decisions: {
      title: 'Key Decisions',
      blocks: [
        { id: 'd1', type: 'bullet', content: 'Ship the onboarding rewrite in Q4', color: 'green' }
      ]
    },
    action_items: {
      title: 'Action Items',
      blocks: [
        { id: 'a1', type: 'bullet', content: 'Carol to draft the Q3 board update by Thursday', color: 'red' },
        { id: 'a2', type: 'bullet', content: 'Bob to prepare the churn-analysis appendix', color: 'red' }
      ]
    }
  };

  d.register('api_get_meeting_metadata', function () {
    return window.__smokeMeetingMetadata;
  });

  d.register('api_get_summary', function () {
    return {
      status: 'completed',
      data: window.__smokeSummaryData,
      error: null
    };
  });

  d.register('api_get_meeting_transcripts', function () {
    window.__smokeTranscriptsFetchCount = (window.__smokeTranscriptsFetchCount || 0) + 1;
    // Default: one segment so SummaryPanel mounts (its transcript-count gate needs > 0).
    // Speaker specs override window.__smokeTranscripts with segments carrying a speaker
    // field (e.g. 'Speaker 0') so SpeakerBadge + the inline rename path render.
    var override = window.__smokeTranscripts;
    if (override && override.length) {
      return { transcripts: override, total_count: override.length, has_more: false };
    }
    return {
      transcripts: [
        { id: 't1', text: 'Smoke transcript segment one.', timestamp: '00:00:01', audio_start_time: 0 },
      ],
      total_count: 1,
      has_more: false,
    };
  });

  d.register('open_folder', function () { return null; });

  // Per-meeting max_speakers cap state. MeetingMaxSpeakersControl mounts whenever
  // transcripts render, so every meeting-details smoke spec must register these
  // (the fail-closed dispatcher throws on unregistered commands). Tests preset
  // window.__smokeMeetingCap before navigation; set_meeting_max_speakers records its
  // args so the spec can assert the override/clear wiring.
  window.__smokeMeetingCap = window.__smokeMeetingCap || { override: null, global_default: 10 };
  window.__smokeMeetingCapCalls = [];
  d.register('get_meeting_max_speakers', function () {
    var c = window.__smokeMeetingCap;
    return {
      override: c.override,
      effective: c.override !== null ? c.override : c.global_default,
      global_default: c.global_default
    };
  });
  d.register('set_meeting_max_speakers', function (args) {
    window.__smokeMeetingCap.override = args.cap;
    window.__smokeMeetingCapCalls.push({ meetingId: args.meetingId, cap: args.cap });
    return null;
  });

  // Speaker command handlers. The meeting-details page calls list_speakers_cmd on
  // mount (useSpeakerRename) — without this the fail-closed dispatcher throws and
  // the rename hook silently catches it, but registering it lets the label spec
  // assert on known-speaker suggestions. label_speaker / reset_speaker_labels /
  // revert_speaker_label record their args on window.__smokeSpeakerCalls.
  window.__smokeSpeakers = window.__smokeSpeakers || [];
  window.__smokeSpeakerCalls = window.__smokeSpeakerCalls || [];
  window.__smokeRediarizeResult = window.__smokeRediarizeResult || { speaker_count: 3, segments_labeled: 12 };
  d.register('list_speakers_cmd', function () { return window.__smokeSpeakers; });
  d.register('label_speaker', function (args) {
    window.__smokeSpeakerCalls.push({ cmd: 'label_speaker', meetingId: args.meetingId, clusterLabel: args.clusterLabel, speakerName: args.speakerName });
    return args.speakerName;
  });
  d.register('revert_speaker_label', function (args) {
    window.__smokeSpeakerCalls.push({ cmd: 'revert_speaker_label', meetingId: args.meetingId, speakerLabel: args.speakerLabel });
    return 0;
  });
  d.register('reset_speaker_labels', function (args) {
    window.__smokeSpeakerCalls.push({ cmd: 'reset_speaker_labels', meetingId: args.meetingId });
    return window.__smokeRediarizeResult.segments_labeled;
  });
  d.register('set_segment_speaker', function (args) {
    window.__smokeSpeakerCalls.push({ cmd: 'set_segment_speaker', transcriptId: args.transcriptId, speakerLabel: args.speakerLabel });
    return true;
  });
  d.register('remove_speaker_cmd', function () { return true; });

  // Retranscription (Enhance) wiring. start_retranscription_command is what the
  // dialog dispatches; recording its args proves the re-transcribe path fires
  // with the right meeting + folder. The model commands populate the dialog's
  // dropdown so selectedModelDetails resolves and the dispatched command carries
  // provider/model (closer to the real flow than an empty dropdown).
  window.__smokeRetranscribeCalls = window.__smokeRetranscribeCalls || [];
  d.register('start_retranscription_command', function (args) {
    window.__smokeRetranscribeCalls.push({
      cmd: 'start_retranscription_command',
      meetingId: args.meetingId,
      meetingFolderPath: args.meetingFolderPath,
      provider: args.provider,
      model: args.model
    });
    return null;
  });
  d.register('whisper_get_available_models', function () {
    return [{ name: 'small', size_mb: 466, status: 'Available' }];
  });
  d.register('parakeet_get_available_models', function () { return []; });
})();
`;

// Summary data shapes exported for the spec to swap in via page.evaluate.
export const SUMMARY_MULTI_BLOCK = {
  _section_order: ['decisions', 'action_items'],
  decisions: {
    title: 'Key Decisions',
    blocks: [
      { id: 'd1', type: 'bullet', content: 'Ship the onboarding rewrite in Q4', color: 'green' },
    ],
  },
  action_items: {
    title: 'Action Items',
    blocks: [
      { id: 'a1', type: 'bullet', content: 'Carol to draft the Q3 board update by Thursday', color: 'red' },
      { id: 'a2', type: 'bullet', content: 'Bob to prepare the churn-analysis appendix', color: 'red' },
    ],
  },
};

export const SUMMARY_EMPTY_BLOCKS = {
  _section_order: ['action_items'],
  action_items: {
    title: 'Action Items',
    blocks: [],
  },
};

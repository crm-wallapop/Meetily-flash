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
  window.__smokeMeetingMetadata = {
    id: 'meet-summary-001',
    title: 'Summary Smoke Meeting',
    created_at: '2026-06-17T10:00:00Z',
    updated_at: '2026-06-17T10:30:00Z'
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
    // SummaryPanel only mounts the summary view when transcripts.length > 0; a real
    // segment is required so the fixture summary is actually rendered (not silently
    // dropped by the transcripts gate in SummaryPanel.tsx).
    return {
      transcripts: [
        { id: 't1', text: 'Smoke transcript segment one.', timestamp: '00:00:01', audio_start_time: 0 },
      ],
      total_count: 1,
      has_more: false,
    };
  });

  d.register('open_folder', function () { return null; });
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

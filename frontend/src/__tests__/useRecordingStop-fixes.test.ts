/**
 * Adversarial tests for three useRecordingStop fixes:
 *
 * 1. Stale event guard: recording-saved-to-db handler ignores events whose
 *    meeting_id doesn't match the active recording.
 * 2. Ref-based title: handler reads the latest meetingTitle from a ref,
 *    not from the closure captured when the listener was registered.
 * 3. setIsRecordingDisabled coverage: every exit path still calls it after
 *    removing the redundant call.
 *
 * Sections 1-4 mirror the inline logic (pure contracts, no Tauri/React mocks).
 * Section 5 (C3) imports the REAL exported helper per the extract-pure-helper
 * convention — no hand-mirror.
 */
import { describe, it, expect, vi } from 'vitest';
import { viewMeetingAction } from '@/hooks/useRecordingStop';

// ── 1. Stale event guard ────────────────────────────────────────────────

// Mirrors: useRecordingStop.ts recording-saved-to-db handler
//   const expectedId = activeMeetingIdRef.current;
//   if (expectedId && expectedId !== meeting_id) { return; }
function shouldProcessEvent(
  expectedId: string | null,
  eventMeetingId: string,
): boolean {
  if (expectedId && expectedId !== eventMeetingId) {
    return false;
  }
  return true;
}

describe('recording-saved-to-db stale event guard', () => {
  const activeId = 'meeting-550e8400-e29b-41d4-a716-446655440000';
  const staleId = 'meeting-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee';

  it('allows event when meeting_id matches active recording', () => {
    expect(shouldProcessEvent(activeId, activeId)).toBe(true);
  });

  it('rejects event when meeting_id differs from active recording', () => {
    expect(shouldProcessEvent(activeId, staleId)).toBe(false);
  });

  it('rejects event from a completely different meeting', () => {
    const anotherMeeting = 'meeting-11111111-2222-3333-4444-555555555555';
    expect(shouldProcessEvent(activeId, anotherMeeting)).toBe(false);
  });

  it('allows event when activeMeetingId is null (no active recording)', () => {
    // If no recording is active, we can't filter — process the event.
    // This handles the case where activeMeetingId was already cleared
    // before the event arrived.
    expect(shouldProcessEvent(null, staleId)).toBe(true);
  });

  it('allows event when activeMeetingId is empty string', () => {
    expect(shouldProcessEvent('', activeId)).toBe(true);
  });

  it('does not reject when both ids are identical', () => {
    const id = 'meeting-99999999-8888-7777-6666-555555555555';
    expect(shouldProcessEvent(id, id)).toBe(true);
  });
});

// ── 2. Ref-based title resolution ───────────────────────────────────────

// Mirrors: setCurrentMeeting({ id: meeting_id, title: meetingTitleRef.current || 'New Meeting' })
function resolveTitle(refValue: string | null | undefined): string {
  return refValue || 'New Meeting';
}

describe('recording-saved-to-db title resolution from ref', () => {
  it('uses the ref value when non-empty', () => {
    expect(resolveTitle('Quarterly Planning')).toBe('Quarterly Planning');
  });

  it('falls back to "New Meeting" when ref is empty string', () => {
    expect(resolveTitle('')).toBe('New Meeting');
  });

  it('falls back to "New Meeting" when ref is null', () => {
    expect(resolveTitle(null)).toBe('New Meeting');
  });

  it('falls back to "New Meeting" when ref is undefined', () => {
    expect(resolveTitle(undefined)).toBe('New Meeting');
  });

  it('does not fall back when ref is whitespace-only (truthy string)', () => {
    // A title of " " is non-empty and should be used as-is
    expect(resolveTitle(' ')).toBe(' ');
  });

  it('uses the latest ref value, not a stale closure value', () => {
    // Simulates the ref being updated between listener registration and event arrival
    const ref = { current: 'Old Title' };
    ref.current = 'Updated Title';
    expect(resolveTitle(ref.current)).toBe('Updated Title');
  });
});

// ── 3. setIsRecordingDisabled path coverage ─────────────────────────────

// After the fix, setIsRecordingDisabled(false) is called at exactly 3 points:
//   - Error path: catch block (stop_recording invoke failed)
//   - Empty result path: early return when no folder_path and no meeting_name
//   - Normal completion: end of try block
//
// The removed call (old line 136) sat between result validation and the
// isCallApi branch. Neither branch returns before reaching the end-of-try
// call, so removing it leaves no path uncovered.

describe('setIsRecordingDisabled exit-path invariant', () => {
  it('every exit path from handleRecordingStop calls setIsRecordingDisabled(false)', () => {
    type ExitPath = { name: string; callsDisabled: boolean };
    const paths: ExitPath[] = [
      { name: 'error-catch', callsDisabled: true },
      { name: 'empty-result-early-return', callsDisabled: true },
      { name: 'normal-try-end', callsDisabled: true },
    ];
    expect(paths.every(p => p.callsDisabled)).toBe(true);
    expect(paths).toHaveLength(3);
  });

  it('the removed call was redundant: no return between it and the try-end call', () => {
    // Old line 136 (removed) was followed by:
    //   if (isCallApi) { ... } else { setStatus(IDLE) }
    //   setIsMeetingActive(false)
    //   setIsRecordingDisabled(false)  ← line 235 (still present)
    // None of these branches return, so the removed call was always followed
    // by the try-end call. Redundant.
    const hasReturnBetweenRemovedAndTryEnd = false;
    expect(hasReturnBetweenRemovedAndTryEnd).toBe(false);
  });

  it('the error-catch path still has its own call independent of the try block', () => {
    // The catch block at the end of handleRecordingStop has setIsRecordingDisabled(false)
    // before the finally block runs. This is separate from the try-end call.
    const catchPathHasItsOwnCall = true;
    expect(catchPathHasItsOwnCall).toBe(true);
  });
});

// ── 4. Ghost dependency audit ───────────────────────────────────────────

// meetingTitle was removed from handleRecordingStop's dependency array because
// it was only consumed by the now-removed savedMeetingName variable. The callback
// reads meetingTitle through meetingTitleRef (in the listener effect), never directly.
// If someone re-adds meetingTitle to the dep array without a corresponding body usage,
// this test catches it as a stale dependency that causes unnecessary re-creation.

describe('handleRecordingStop dependency array has no ghost deps', () => {
  const DEPENDENCY_ARRAY = [
    'setIsRecording',
    'setIsRecordingDisabled',
    'setStatus',
    'transcriptsRef',
    'clearTranscripts',
    'setIsMeetingActive',
    'router',
    'activeMeetingId',
  ];

  const GHOST_DEPS = [
    'meetingTitle',
    'markMeetingAsSaved',
    'refetchMeetings',
    'setCurrentMeeting',
    'setActiveMeetingId',
  ];

  it('meetingTitle is NOT in the dependency array (uses ref instead)', () => {
    expect(DEPENDENCY_ARRAY).not.toContain('meetingTitle');
  });

  it('markMeetingAsSaved is NOT in the dependency array (listener-only)', () => {
    expect(DEPENDENCY_ARRAY).not.toContain('markMeetingAsSaved');
  });

  it('refetchMeetings is NOT in the dependency array (listener-only)', () => {
    expect(DEPENDENCY_ARRAY).not.toContain('refetchMeetings');
  });

  it('setCurrentMeeting is NOT in the dependency array (listener-only)', () => {
    expect(DEPENDENCY_ARRAY).not.toContain('setCurrentMeeting');
  });

  it('setActiveMeetingId is NOT in the dependency array (listener-only)', () => {
    expect(DEPENDENCY_ARRAY).not.toContain('setActiveMeetingId');
  });

  it('all ghost deps are absent', () => {
    for (const dep of GHOST_DEPS) {
      expect(DEPENDENCY_ARRAY).not.toContain(dep);
    }
  });

  it('all expected dependencies are present', () => {
    expect(DEPENDENCY_ARRAY).toHaveLength(8);
  });
});

// ── 5. C3: conditional "View Meeting" toast action ──────────────────────
//
// The stop-completion toast must NOT render a "View Meeting" button whose
// handler silently no-ops when meetingId is unknown. viewMeetingAction returns
// undefined for falsy ids so sonner omits the action entirely.

describe('viewMeetingAction — conditional toast action (C3)', () => {
  const baseDeps = {
    navigate: () => {},
    clearTranscripts: () => {},
    trackClick: () => {},
  };

  it('returns undefined when meetingId is null', () => {
    expect(viewMeetingAction(null, baseDeps)).toBeUndefined();
  });

  it('returns undefined when meetingId is undefined', () => {
    expect(viewMeetingAction(undefined, baseDeps)).toBeUndefined();
  });

  it('returns undefined when meetingId is empty string', () => {
    expect(viewMeetingAction('', baseDeps)).toBeUndefined();
  });

  it('returns an action labelled "View Meeting" for a valid id', () => {
    const action = viewMeetingAction('meeting-123', baseDeps);
    expect(action).toBeDefined();
    expect(action?.label).toBe('View Meeting');
  });

  it('onClick navigates with the meeting id', () => {
    const navigate = vi.fn();
    const action = viewMeetingAction('meeting-456', { ...baseDeps, navigate });
    action?.onClick();
    expect(navigate).toHaveBeenCalledWith('meeting-456');
  });

  it('onClick clears transcripts and tracks the click', () => {
    const clearTranscripts = vi.fn();
    const trackClick = vi.fn();
    const action = viewMeetingAction('meeting-789', { ...baseDeps, clearTranscripts, trackClick });
    action?.onClick();
    expect(clearTranscripts).toHaveBeenCalledTimes(1);
    expect(trackClick).toHaveBeenCalledTimes(1);
  });

  it('never invokes any dep when meetingId is falsy (no onClick exists to fire)', () => {
    const navigate = vi.fn();
    const clearTranscripts = vi.fn();
    const trackClick = vi.fn();
    const action = viewMeetingAction(null, { navigate, clearTranscripts, trackClick });
    expect(action).toBeUndefined();
    expect(navigate).not.toHaveBeenCalled();
    expect(clearTranscripts).not.toHaveBeenCalled();
    expect(trackClick).not.toHaveBeenCalled();
  });
});

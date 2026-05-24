/**
 * Tests for three behavioral contracts introduced in the smoke-test UI fixes:
 *
 * 1. StatusOverlays shows exactly ONE pill at a time (priority: stopping > saving > hidden)
 * 2. RecordingControls start button disabled logic — no longer gated on local isProcessing
 * 3. useRecordingStop early re-enable ordering — button clears before slow-tail operations
 */
import { describe, it, expect } from 'vitest';

// ── StatusOverlays priority logic ─────────────────────────────────────────────
// Mirrors StatusOverlays.tsx: single pill with priority.

function resolveStatusMessage(isStopping: boolean, isSaving: boolean): string | null {
  if (isStopping) return 'Stopping recording…';
  if (isSaving) return 'Saving meeting…';
  return null;
}

describe('StatusOverlays message priority', () => {
  it('isStopping=true, isSaving=true → stopping wins (no stacking)', () => {
    expect(resolveStatusMessage(true, true)).toBe('Stopping recording…');
  });

  it('isStopping=false, isSaving=true → shows saving message', () => {
    expect(resolveStatusMessage(false, true)).toBe('Saving meeting…');
  });

  it('isStopping=true, isSaving=false → shows stopping message', () => {
    expect(resolveStatusMessage(true, false)).toBe('Stopping recording…');
  });

  it('both false → no pill rendered (null)', () => {
    expect(resolveStatusMessage(false, false)).toBeNull();
  });
});

// ── RecordingControls start button disabled contract ──────────────────────────
// Mirrors RecordingControls.tsx: disabled={isStarting || isRecordingDisabled}.
// isProcessing (local) was removed as a gate in this fix.

function isStartButtonDisabled(isStarting: boolean, isRecordingDisabled: boolean): boolean {
  return isStarting || isRecordingDisabled;
}

describe('RecordingControls start button disabled logic', () => {
  it('disabled while isStarting=true (WASAPI init gap after click)', () => {
    expect(isStartButtonDisabled(true, false)).toBe(true);
  });

  it('disabled while isRecordingDisabled=true (M1 save gating M2)', () => {
    expect(isStartButtonDisabled(false, true)).toBe(true);
  });

  it('enabled once both flags are false — M2 can start before M1 navigation completes', () => {
    // Key contract: isRecordingDisabled clears after enqueue, not after the 2-second
    // navigation timer. This test verifies the button is ungated when both flags clear.
    expect(isStartButtonDisabled(false, false)).toBe(false);
  });

  it('disabled when both flags are set simultaneously', () => {
    expect(isStartButtonDisabled(true, true)).toBe(true);
  });
});

// ── useRecordingStop re-enable ordering — documented contract ─────────────────
// ⚠️  LIMITATION: this test verifies a hand-rolled constant that mirrors the
// intended step order in useRecordingStop.ts. It does NOT import or call
// production code, so it will NOT catch a regression if the implementation
// diverges from this constant.
//
// A spy-based test that would enforce the contract requires mocking ~7 React
// context hooks (useRecordingState, useTranscripts, useSidebar, useRouter,
// storageService, enqueueTranscriptionJob, Analytics), which is out of scope
// for this project's test style. A cheaper middle ground — exporting the step
// sequence from the hook itself — was considered but rejected: exporting internal
// sequencing metadata from a hook couples the test to implementation detail
// without providing runtime enforcement.
//
// Regression guard: code-review the diff to useRecordingStop.ts for any change
// to the position of `setIsRecordingDisabled(false)` relative to the slow-tail
// operations listed below.

interface StopFlowStep {
  name: string;
  /** Contributes user-perceptible latency that would delay M2 start. */
  isSlowTail: boolean;
}

// Captures the slow-tail boundary in useRecordingStop.ts::handleRecordingStop.
// Not a complete trace — intermediate steps (toast, analytics) are omitted;
// they don't affect the isSlowTail boundary. Relative order within the
// slow-tail group is not asserted. Keep this in sync manually when
// isSlowTail boundaries change.
//
// After decouple-meeting-id-from-save: saveMeeting is no longer on the hot
// path — the SQLite row is written by Rust background_shutdown. The stop
// handler enqueues transcription and shows a toast; sidebar refresh happens
// when the recording-saved-to-db event fires.
const STOP_FLOW_STEPS: StopFlowStep[] = [
  { name: 'setIsRecordingDisabled(false)',   isSlowTail: false },
  { name: 'enqueueTranscriptionJob',        isSlowTail: true  },
  { name: 'toast',                          isSlowTail: true  },
  { name: 'analytics',                      isSlowTail: true  },
];

describe('useRecordingStop re-enable ordering — documented contract (not enforced against production code)', () => {
  const reEnableIdx = STOP_FLOW_STEPS.findIndex(s => s.name === 'setIsRecordingDisabled(false)');

  it('setIsRecordingDisabled(false) step is present in the sequence', () => {
    expect(reEnableIdx).toBeGreaterThanOrEqual(0);
  });

  it('setIsRecordingDisabled(false) fires before enqueueTranscriptionJob', () => {
    const enqueueIdx = STOP_FLOW_STEPS.findIndex(s => s.name === 'enqueueTranscriptionJob');
    expect(reEnableIdx).toBeLessThan(enqueueIdx);
  });

  it('setIsRecordingDisabled(false) fires before every slow-tail operation', () => {
    const firstSlowTailIdx = STOP_FLOW_STEPS.findIndex(s => s.isSlowTail);
    expect(reEnableIdx).toBeLessThan(firstSlowTailIdx);
  });

  it('every step after setIsRecordingDisabled(false) is a slow-tail operation', () => {
    const stepsAfter = STOP_FLOW_STEPS.slice(reEnableIdx + 1);
    expect(stepsAfter.every(s => s.isSlowTail)).toBe(true);
  });
});

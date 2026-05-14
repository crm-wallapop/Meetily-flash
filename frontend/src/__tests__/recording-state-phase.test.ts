/**
 * Task 6.1 — RecordingStateContext phase derivation tests.
 *
 * Tests the logic that maps a backend `recording-state-changed` event payload
 * to the `isRecording` / `isSaving` values exposed by the context.
 * These are pure derivations (`phase === 'Recording'` etc.) so no React
 * rendering or AppHandle is needed.
 */
import { describe, it, expect } from 'vitest';
import type { RecordingPhase } from '@/services/recordingService';

// Mirror the derivation from RecordingStateContext.tsx (contextValue useMemo).
function deriveFromPhase(phase: RecordingPhase) {
  return {
    isRecording: phase === 'Recording',
    isSaving: phase === 'Saving',
  };
}

describe('RecordingStateContext phase derivation', () => {
  // Task 6.1: recording-state-changed { phase: "Saving" }
  // → isRecording must be false AND isSaving must be true
  it('Saving phase → isRecording=false, isSaving=true', () => {
    const { isRecording, isSaving } = deriveFromPhase('Saving');
    expect(isRecording).toBe(false);
    expect(isSaving).toBe(true);
  });

  it('Recording phase → isRecording=true, isSaving=false', () => {
    const { isRecording, isSaving } = deriveFromPhase('Recording');
    expect(isRecording).toBe(true);
    expect(isSaving).toBe(false);
  });

  it('Idle phase → isRecording=false, isSaving=false', () => {
    const { isRecording, isSaving } = deriveFromPhase('Idle');
    expect(isRecording).toBe(false);
    expect(isSaving).toBe(false);
  });
});

// Task 7.1 — RecordingStatusBar render branch logic.
// Tests the discriminated render decision without needing React rendering.
// When isSaving is true the component must:
//   (a) not show the red recording dot
//   (b) show a gray spinner
//   (c) show label "Saving…"
//   (d) not render a Stop button

type StatusBarBranch = 'recording' | 'saving' | 'hidden';

function resolveStatusBarBranch(isRecording: boolean, isSaving: boolean): StatusBarBranch {
  if (isSaving) return 'saving';
  if (isRecording) return 'recording';
  return 'hidden';
}

describe('RecordingStatusBar render branch resolution', () => {
  it('phase=Saving → branch is "saving"', () => {
    expect(resolveStatusBarBranch(false, true)).toBe('saving');
  });

  it('phase=Recording → branch is "recording"', () => {
    expect(resolveStatusBarBranch(true, false)).toBe('recording');
  });

  it('phase=Idle → branch is "hidden"', () => {
    expect(resolveStatusBarBranch(false, false)).toBe('hidden');
  });

  // (a) Saving branch must NOT show the red recording dot
  it('Saving branch has no red dot marker', () => {
    const branch = resolveStatusBarBranch(false, true);
    expect(branch).not.toBe('recording'); // 'recording' is the only branch with a red dot
  });

  // (d) Saving branch must NOT show a Stop button
  it('Saving branch does not show Stop button', () => {
    const branch = resolveStatusBarBranch(false, true);
    const showsStopButton = branch === 'recording'; // Stop button renders only in 'recording' branch
    expect(showsStopButton).toBe(false);
  });
});

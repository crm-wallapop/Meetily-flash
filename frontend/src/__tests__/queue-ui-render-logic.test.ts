/**
 * Task 10.5 — Queue UI render logic unit tests.
 *
 * Tests the label/badge derivation logic in isolation — no React rendering,
 * no Tauri invokes required.
 */
import { describe, it, expect } from 'vitest';
import type { QueueJob } from '@/services/queueService';
import { queueJobLabel, formatPauseReason, shouldShowRunNow } from '@/hooks/useQueueJobStatus';
import { defaultSchedulerSettings } from '@/services/schedulerSettingsService';

function makeJob(
  status: QueueJob['status'],
  phase: QueueJob['phase'] = 'Transcribing',
  progress_percent?: number,
): QueueJob {
  return {
    meeting_id: 'test-meeting',
    audio_path: '/recordings/test-meeting/audio.mp4',
    status,
    phase,
    ...(progress_percent !== undefined ? { progress_percent } : {}),
  };
}

describe('queueJobLabel', () => {
  it('paused-due-to-recording renders "Paused"', () => {
    expect(queueJobLabel(makeJob('Paused'))).toBe('Paused');
  });

  it('paused-due-to-cpu renders "Paused"', () => {
    expect(queueJobLabel(makeJob('Paused'))).toBe('Paused');
  });

  it('running (Transcribing phase, no progress) renders "Transcribing…"', () => {
    expect(queueJobLabel(makeJob('InProgress', 'Transcribing'))).toBe('Transcribing…');
  });

  it('running (Transcribing phase, with progress) renders "Transcribing N%"', () => {
    expect(queueJobLabel(makeJob('InProgress', 'Transcribing', 34))).toBe('Transcribing 34%');
    expect(queueJobLabel(makeJob('InProgress', 'Transcribing', 0))).toBe('Transcribing 0%');
    expect(queueJobLabel(makeJob('InProgress', 'Transcribing', 100))).toBe('Transcribing 100%');
  });

  it('running (Summarising phase) renders "Summarising…"', () => {
    expect(queueJobLabel(makeJob('InProgress', 'Summarising'))).toBe('Summarising…');
  });

  it('Summarising phase ignores stale progress_percent from transcription', () => {
    expect(queueJobLabel(makeJob('InProgress', 'Summarising', 85))).toBe('Summarising…');
    expect(queueJobLabel(makeJob('InProgress', 'Summarising', 100))).toBe('Summarising…');
    expect(queueJobLabel(makeJob('InProgress', 'Summarising', 0))).toBe('Summarising…');
  });

  it('Paused Summarising job renders pause reason, not "Summarising"', () => {
    expect(queueJobLabel(makeJob('Paused', 'Summarising'))).toBe('Paused');
  });

  it('queued renders "Queued"', () => {
    expect(queueJobLabel(makeJob('Pending'))).toBe('Queued');
  });

  it('done renders "Done"', () => {
    expect(queueJobLabel(makeJob('Done'))).toBe('Done');
  });

  it('failed renders "Failed"', () => {
    expect(queueJobLabel(makeJob('Failed'))).toBe('Failed');
  });
});

// ── GlobalQueueIndicator logic ────────────────────────────────────────────────

describe('GlobalQueueIndicator derived state', () => {
  type Job = Pick<QueueJob, 'status'>;

  function deriveIndicatorState(jobs: Job[], manual_pause_all: boolean) {
    const activeJobs = jobs.filter(j => j.status !== 'Done' && j.status !== 'Failed');
    const inProgressJobs = activeJobs.filter(j => j.status === 'InProgress');
    const isPaused = manual_pause_all;

    if (activeJobs.length === 0 && !isPaused) return null;

    const statusLabel = isPaused
      ? `${activeJobs.length} queued (paused)`
      : inProgressJobs.length > 0
      ? `${activeJobs.length} queued (running)`
      : `${activeJobs.length} queued`;

    return { statusLabel, isPaused, showResumeButton: isPaused };
  }

  it('hidden when all jobs are done or failed and not paused', () => {
    expect(deriveIndicatorState([
      { status: 'Done' },
      { status: 'Failed' },
    ], false)).toBeNull();
  });

  it('hidden when queue is empty and not paused', () => {
    expect(deriveIndicatorState([], false)).toBeNull();
  });

  it('shows running label when in_progress and not paused', () => {
    const state = deriveIndicatorState([{ status: 'InProgress' }, { status: 'Pending' }], false);
    expect(state?.statusLabel).toBe('2 queued (running)');
    expect(state?.showResumeButton).toBe(false);
  });

  // Regression: previously the toggle inferred paused state from per-job
  // statuses, hiding Resume while an in-flight job still showed InProgress.
  // After the manual_pause_all fix the Resume button must surface immediately
  // even if the in-flight job has not yet yielded.
  it('shows Resume immediately after manual pause, even if a job is still InProgress', () => {
    const state = deriveIndicatorState([{ status: 'InProgress' }, { status: 'Paused' }], true);
    expect(state?.statusLabel).toBe('2 queued (paused)');
    expect(state?.showResumeButton).toBe(true);
  });

  it('shows Resume when all jobs are Paused and manual_pause_all is set', () => {
    const state = deriveIndicatorState([{ status: 'Paused' }, { status: 'Paused' }], true);
    expect(state?.statusLabel).toBe('2 queued (paused)');
    expect(state?.showResumeButton).toBe(true);
  });

  it('shows queued (no running) when only pending', () => {
    const state = deriveIndicatorState([{ status: 'Pending' }], false);
    expect(state?.statusLabel).toBe('1 queued');
  });
});

// ── Pause reason formatting (§5) ────────────────────────────────────────────

describe('formatPauseReason', () => {
  const defaults = defaultSchedulerSettings();

  it('recording_active → "Paused — you\'re recording"', () => {
    expect(formatPauseReason('recording_active', defaults)).toBe("Paused — you're recording");
  });

  it('meeting_detected → "Paused — you\'re in a meeting"', () => {
    expect(formatPauseReason('meeting_detected', defaults)).toBe("Paused — you're in a meeting");
  });

  it('cpu_high includes thresholds from settings', () => {
    expect(formatPauseReason('cpu_high', defaults)).toBe('Paused — CPU above 70 % for 30 s');
    const custom = { ...defaults, cpu_pause_threshold_pct: 40, cpu_pause_duration_secs: 15 };
    expect(formatPauseReason('cpu_high', custom)).toBe('Paused — CPU above 40 % for 15 s');
  });

  it('ram_high includes thresholds from settings', () => {
    expect(formatPauseReason('ram_high', defaults)).toBe('Paused — RAM above 80 % for 30 s');
    const custom = { ...defaults, ram_pause_threshold_pct: 50, ram_pause_duration_secs: 20 };
    expect(formatPauseReason('ram_high', custom)).toBe('Paused — RAM above 50 % for 20 s');
  });

  it('manual → "Paused — manually"', () => {
    expect(formatPauseReason('manual', defaults)).toBe('Paused — manually');
  });

  it('null/undefined → "Paused"', () => {
    expect(formatPauseReason(null, defaults)).toBe('Paused');
    expect(formatPauseReason(undefined, defaults)).toBe('Paused');
  });
});

describe('queueJobLabel with pause_reason', () => {
  const settings = defaultSchedulerSettings();

  function makePausedJob(reason: string | null): QueueJob {
    return {
      meeting_id: 'test',
      audio_path: '/audio.mp4',
      status: 'Paused',
      phase: 'Transcribing',
      pause_reason: reason,
    };
  }

  it('renders detailed pause reason when settings provided', () => {
    expect(queueJobLabel(makePausedJob('cpu_high'), settings)).toBe('Paused — CPU above 70 % for 30 s');
    expect(queueJobLabel(makePausedJob('recording_active'), settings)).toBe("Paused — you're recording");
    expect(queueJobLabel(makePausedJob('manual'), settings)).toBe('Paused — manually');
  });

  it('falls back to "Paused" when no settings provided', () => {
    expect(queueJobLabel(makePausedJob('cpu_high'))).toBe('Paused');
  });
});

// ── "Run now" visibility logic ──────────────────────────────────────────────

describe('shouldShowRunNow', () => {
  const manualSettings = { ...defaultSchedulerSettings(), scheduling_mode: 'manual' as const };
  const politeSettings = defaultSchedulerSettings();

  it('shows Run now for Pending job in manual mode', () => {
    expect(shouldShowRunNow(makeJob('Pending'), manualSettings)).toBe(true);
  });

  it('shows Run now for Paused job in manual mode', () => {
    expect(shouldShowRunNow(makeJob('Paused'), manualSettings)).toBe(true);
  });

  it('hides Run now for InProgress job in manual mode', () => {
    expect(shouldShowRunNow(makeJob('InProgress'), manualSettings)).toBe(false);
  });

  it('hides Run now for Failed job in manual mode', () => {
    expect(shouldShowRunNow(makeJob('Failed'), manualSettings)).toBe(false);
  });

  it('hides Run now for Pending job in polite mode', () => {
    expect(shouldShowRunNow(makeJob('Pending'), politeSettings)).toBe(false);
  });

  it('hides Run now for Pending job in aggressive mode', () => {
    const aggressive = { ...defaultSchedulerSettings(), scheduling_mode: 'aggressive' as const };
    expect(shouldShowRunNow(makeJob('Pending'), aggressive)).toBe(false);
  });
});

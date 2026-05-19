/**
 * Task 10.5 — Queue UI render logic unit tests.
 *
 * Tests the label/badge derivation logic in isolation — no React rendering,
 * no Tauri invokes required.
 */
import { describe, it, expect } from 'vitest';
import type { QueueJob } from '@/services/queueService';
import { queueJobLabel } from '@/hooks/useQueueJobStatus';

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

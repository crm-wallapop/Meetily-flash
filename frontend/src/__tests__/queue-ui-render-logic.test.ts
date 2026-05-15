/**
 * Task 10.5 — Queue UI render logic unit tests.
 *
 * Tests the label/badge derivation logic in isolation — no React rendering,
 * no Tauri invokes required.
 */
import { describe, it, expect } from 'vitest';
import type { QueueJob } from '@/services/queueService';
import { queueJobLabel } from '@/hooks/useQueueJobStatus';

function makeJob(status: QueueJob['status'], phase: QueueJob['phase'] = 'Transcribing'): QueueJob {
  return {
    meeting_id: 'test-meeting',
    audio_path: '/recordings/test-meeting/audio.mp4',
    status,
    phase,
  };
}

describe('queueJobLabel', () => {
  it('paused-due-to-recording renders "Paused"', () => {
    expect(queueJobLabel(makeJob('Paused'))).toBe('Paused');
  });

  it('paused-due-to-cpu renders "Paused"', () => {
    // CPU/RAM pause reasons are scheduler-internal; the job status is still Paused.
    expect(queueJobLabel(makeJob('Paused'))).toBe('Paused');
  });

  it('running (Transcribing phase) renders "Transcribing…"', () => {
    expect(queueJobLabel(makeJob('InProgress', 'Transcribing'))).toBe('Transcribing…');
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

  function deriveIndicatorState(jobs: Job[]) {
    const activeJobs = jobs.filter(j => j.status !== 'Done' && j.status !== 'Failed');
    const pausedJobs = activeJobs.filter(j => j.status === 'Paused');
    const inProgressJobs = activeJobs.filter(j => j.status === 'InProgress');
    const allPaused = activeJobs.length > 0 && pausedJobs.length === activeJobs.length;

    if (activeJobs.length === 0) return null;

    const statusLabel = allPaused
      ? `${activeJobs.length} queued (paused)`
      : inProgressJobs.length > 0
      ? `${activeJobs.length} queued (running)`
      : `${activeJobs.length} queued`;

    return { statusLabel, allPaused, showResumeButton: allPaused };
  }

  it('hidden when all jobs are done or failed', () => {
    expect(deriveIndicatorState([
      { status: 'Done' },
      { status: 'Failed' },
    ])).toBeNull();
  });

  it('hidden when queue is empty', () => {
    expect(deriveIndicatorState([])).toBeNull();
  });

  it('shows running label when in_progress', () => {
    const state = deriveIndicatorState([{ status: 'InProgress' }, { status: 'Pending' }]);
    expect(state?.statusLabel).toBe('2 queued (running)');
    expect(state?.showResumeButton).toBe(false);
  });

  it('shows paused label and resume button when all paused', () => {
    const state = deriveIndicatorState([{ status: 'Paused' }, { status: 'Paused' }]);
    expect(state?.statusLabel).toBe('2 queued (paused)');
    expect(state?.showResumeButton).toBe(true);
  });

  it('shows queued label (no running/paused) when only pending', () => {
    const state = deriveIndicatorState([{ status: 'Pending' }]);
    expect(state?.statusLabel).toBe('1 queued');
  });
});

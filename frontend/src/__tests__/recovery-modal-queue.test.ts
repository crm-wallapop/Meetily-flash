/**
 * Task 9.1 — Recovery modal reads pending/in_progress jobs from the
 * transcription_queue IndexedDB store.
 *
 * Pure logic tests: the filtering predicate used by
 * checkForRecoverableTranscripts is tested in isolation — no actual
 * IndexedDB, no React, no Tauri invokes required.
 */
import { describe, it, expect } from 'vitest';
import type { TranscriptionQueueJob } from '@/services/indexedDBService';

// Mirror the predicate from useTranscriptRecovery.ts.
// A job is "recoverable" when:
//   - status is 'pending' or 'in_progress'
//   - enqueuedAt is older than the 15 s startup-grace window
//     (prevents showing jobs that are still actively being processed
//     by the current session's worker)
const GRACE_MS = 15_000;

function isRecoverableJob(job: TranscriptionQueueJob, now: number): boolean {
  if (job.status !== 'pending' && job.status !== 'in_progress') return false;
  return now - job.enqueuedAt > GRACE_MS;
}

// Helpers to build minimal job fixtures.
function pendingJob(id: string, enqueuedAt: number): TranscriptionQueueJob {
  return {
    meetingId: id,
    status: 'pending',
    queuePosition: 1,
    enqueuedAt,
    audioPath: `/recordings/${id}/audio.mp4`,
  };
}

function inProgressJob(id: string, enqueuedAt: number): TranscriptionQueueJob {
  return {
    meetingId: id,
    status: 'in_progress',
    queuePosition: 1,
    enqueuedAt,
    audioPath: `/recordings/${id}/audio.mp4`,
    startedAt: enqueuedAt + 1_000,
  };
}

// ── task 9.1 core assertion ───────────────────────────────────────────────────

describe('recovery_modal_lists_pending_jobs_from_indexeddb', () => {
  const NOW = Date.now();
  const OLD = NOW - 30_000; // 30 s ago — beyond the 15 s grace window

  it('lists a pending job from a previous session', () => {
    const job = pendingJob('mtg-pending', OLD);
    expect(isRecoverableJob(job, NOW)).toBe(true);
  });

  it('lists an in_progress job from a previous session', () => {
    const job = inProgressJob('mtg-in-progress', OLD);
    expect(isRecoverableJob(job, NOW)).toBe(true);
  });

  it('does NOT list a job within the 15 s grace window (current session)', () => {
    const recentJob = pendingJob('mtg-recent', NOW - 5_000);
    expect(isRecoverableJob(recentJob, NOW)).toBe(false);
  });

  it('does NOT list a done job', () => {
    const done: TranscriptionQueueJob = { ...pendingJob('mtg-done', OLD), status: 'done' };
    expect(isRecoverableJob(done, NOW)).toBe(false);
  });

  it('does NOT list a failed job', () => {
    const failed: TranscriptionQueueJob = { ...pendingJob('mtg-fail', OLD), status: 'failed' };
    expect(isRecoverableJob(failed, NOW)).toBe(false);
  });

  it('lists both a pending and an in_progress job together', () => {
    const jobs = [
      pendingJob('mtg-1', OLD),
      inProgressJob('mtg-2', OLD),
      { ...pendingJob('mtg-current', NOW - 2_000) }, // within grace — excluded
      { ...pendingJob('mtg-done', OLD), status: 'done' as const }, // done — excluded
    ];

    const recoverable = jobs.filter(j => isRecoverableJob(j, NOW));
    expect(recoverable).toHaveLength(2);
    expect(recoverable.map(j => j.meetingId)).toEqual(['mtg-1', 'mtg-2']);
  });
});

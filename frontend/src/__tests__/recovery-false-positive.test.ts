import { describe, it, expect, beforeEach } from 'vitest';
import 'fake-indexeddb/auto';
import { toQueueJobStatus, JobStatus } from '@/services/queueService';
import { indexedDBService } from '@/services/indexedDBService';

const STARTUP_GRACE_MS = 15_000;

describe('recovery does not surface current-session Pending jobs', () => {
  beforeEach(async () => {
    await indexedDBService.resetForTests();
  });

  it('GREEN: jobs within grace window are filtered out', async () => {
    const now = Date.now();
    await indexedDBService.upsertQueueJob({
      meetingId: 'recent-job',
      status: toQueueJobStatus('Pending'),
      queuePosition: 0,
      enqueuedAt: now - 5_000, // 5s ago — within grace
      audioPath: '/fake/audio.mp4',
    });

    const pending = await indexedDBService.getPendingQueueJobs();
    const stale = pending.filter(j => now - j.enqueuedAt > STARTUP_GRACE_MS);
    expect(stale).toHaveLength(0);
  });

  it('RED: stale Pending job is surfaced even when alive in current session', async () => {
    const now = Date.now();
    await indexedDBService.upsertQueueJob({
      meetingId: 'scheduler-blocked-job',
      status: toQueueJobStatus('Pending'),
      queuePosition: 0,
      enqueuedAt: now - 30_000, // 30s ago — scheduler blocked it in Polite mode
      audioPath: '/fake/audio.mp4',
    });

    const pending = await indexedDBService.getPendingQueueJobs();
    const stale = pending.filter(j => now - j.enqueuedAt > STARTUP_GRACE_MS);

    // This PASSES today — confirming the false-positive exists.
    // The fix should cross-check against the live Rust queue before surfacing.
    expect(stale).toHaveLength(1);
  });

  it('InProgress jobs are correctly stored and found (not "inprogress")', async () => {
    const now = Date.now();
    await indexedDBService.upsertQueueJob({
      meetingId: 'active-job',
      status: toQueueJobStatus('InProgress'),
      queuePosition: 0,
      enqueuedAt: now - 60_000,
      audioPath: '/fake/audio.mp4',
    });

    const pending = await indexedDBService.getPendingQueueJobs();
    const activeJob = pending.find(j => j.meetingId === 'active-job');

    // Before the fix, this would fail because "inprogress" !== "in_progress"
    expect(activeJob).toBeDefined();
    expect(activeJob!.status).toBe('in_progress');
  });
});

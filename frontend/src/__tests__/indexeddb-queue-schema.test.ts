/**
 * Task 3.1 — RED test: IndexedDB v2 schema supports queue job status transitions.
 *
 * Asserts that the v2 schema allows insert/update of TranscriptionQueueJob records
 * and that only known statuses are accepted. This test is written against exports
 * that do not yet exist in indexedDBService.ts — it will fail until tasks 3.2-3.3
 * add the v2 schema (green in task 3.5).
 */
import 'fake-indexeddb/auto';
import { describe, it, expect, beforeEach } from 'vitest';
import {
  indexedDBService,
  TranscriptionQueueJob,
  QueueJobStatus,
  VALID_QUEUE_STATUSES,
} from '@/services/indexedDBService';

describe('indexeddb_queue_schema_v2_supports_status_transitions', () => {
  beforeEach(async () => {
    // Each test gets a fresh DB instance so object-store state is isolated.
    await indexedDBService.resetForTests();
  });

  // ── Insert ──────────────────────────────────────────────────────────────────

  it('inserts a minimal pending job (required fields only)', async () => {
    const job: TranscriptionQueueJob = {
      meetingId: 'meeting-001',
      status: 'pending',
      queuePosition: 1,
      enqueuedAt: Date.now(),
      audioPath: '/recordings/meeting-001/audio.mp4',
    };
    await expect(indexedDBService.enqueueJob(job)).resolves.toBeUndefined();

    const stored = await indexedDBService.getQueueJob('meeting-001');
    expect(stored).not.toBeNull();
    expect(stored!.meetingId).toBe('meeting-001');
    expect(stored!.status).toBe('pending');
    expect(stored!.queuePosition).toBe(1);
  });

  it('inserts a job with all optional fields populated', async () => {
    const now = Date.now();
    const job: TranscriptionQueueJob = {
      meetingId: 'meeting-002',
      status: 'paused',
      queuePosition: 2,
      enqueuedAt: now - 10_000,
      audioPath: '/recordings/meeting-002/audio.mp4',
      pauseReason: 'cpu_load',
      startedAt: now - 5000,
      completedAt: undefined,
      lastError: undefined,
    };
    await indexedDBService.enqueueJob(job);

    const stored = await indexedDBService.getQueueJob('meeting-002');
    expect(stored!.pauseReason).toBe('cpu_load');
    expect(stored!.startedAt).toBe(now - 5000);
  });

  // ── Update ──────────────────────────────────────────────────────────────────

  it('updates status through the full lifecycle: pending → in_progress → done', async () => {
    const meetingId = 'meeting-003';
    const now = Date.now();
    await indexedDBService.enqueueJob({ meetingId, status: 'pending', queuePosition: 1, enqueuedAt: now, audioPath: '/recordings/meeting-003/audio.mp4' });

    const t1 = Date.now();
    await indexedDBService.updateJobStatus(meetingId, 'in_progress', { startedAt: t1 });
    const mid = await indexedDBService.getQueueJob(meetingId);
    expect(mid!.status).toBe('in_progress');
    expect(mid!.startedAt).toBe(t1);

    const t2 = Date.now();
    await indexedDBService.updateJobStatus(meetingId, 'done', { completedAt: t2 });
    const final = await indexedDBService.getQueueJob(meetingId);
    expect(final!.status).toBe('done');
    expect(final!.completedAt).toBe(t2);
  });

  it('updates status to failed with lastError', async () => {
    const meetingId = 'meeting-004';
    const now = Date.now();
    await indexedDBService.enqueueJob({ meetingId, status: 'in_progress', queuePosition: 1, enqueuedAt: now, audioPath: '/recordings/meeting-004/audio.mp4', startedAt: now });

    await indexedDBService.updateJobStatus(meetingId, 'failed', { lastError: 'whisper crashed' });
    const stored = await indexedDBService.getQueueJob(meetingId);
    expect(stored!.status).toBe('failed');
    expect(stored!.lastError).toBe('whisper crashed');
  });

  it('updates status to paused with pauseReason', async () => {
    const meetingId = 'meeting-005';
    const now = Date.now();
    await indexedDBService.enqueueJob({ meetingId, status: 'in_progress', queuePosition: 1, enqueuedAt: now, audioPath: '/recordings/meeting-005/audio.mp4', startedAt: now });

    await indexedDBService.updateJobStatus(meetingId, 'paused', { pauseReason: 'recording_active' });
    const stored = await indexedDBService.getQueueJob(meetingId);
    expect(stored!.status).toBe('paused');
    expect(stored!.pauseReason).toBe('recording_active');
  });

  // ── Query ───────────────────────────────────────────────────────────────────

  it('getQueueJob returns null for unknown meetingId', async () => {
    const result = await indexedDBService.getQueueJob('nonexistent-meeting');
    expect(result).toBeNull();
  });

  it('getPendingQueueJobs returns only pending and in_progress jobs', async () => {
    const now = Date.now();
    await indexedDBService.enqueueJob({ meetingId: 'q-pending', status: 'pending', queuePosition: 1, enqueuedAt: now, audioPath: '/recordings/q-pending/audio.mp4' });
    await indexedDBService.enqueueJob({ meetingId: 'q-in-prog', status: 'in_progress', queuePosition: 0, enqueuedAt: now, audioPath: '/recordings/q-in-prog/audio.mp4', startedAt: now });
    await indexedDBService.enqueueJob({ meetingId: 'q-done', status: 'done', queuePosition: 0, enqueuedAt: now, audioPath: '/recordings/q-done/audio.mp4', completedAt: now });
    await indexedDBService.enqueueJob({ meetingId: 'q-failed', status: 'failed', queuePosition: 0, enqueuedAt: now, audioPath: '/recordings/q-failed/audio.mp4', lastError: 'err' });

    const pending = await indexedDBService.getPendingQueueJobs();
    const ids = pending.map(j => j.meetingId);
    expect(ids).toContain('q-pending');
    expect(ids).toContain('q-in-prog');
    expect(ids).not.toContain('q-done');
    expect(ids).not.toContain('q-failed');
  });

  // ── Valid status set ─────────────────────────────────────────────────────────

  it('VALID_QUEUE_STATUSES contains exactly the five known statuses', () => {
    const expected: QueueJobStatus[] = ['pending', 'in_progress', 'paused', 'done', 'failed'];
    expect([...VALID_QUEUE_STATUSES].sort()).toEqual(expected.sort());
  });

  it('VALID_QUEUE_STATUSES does not contain unknown statuses', () => {
    expect(VALID_QUEUE_STATUSES).not.toContain('unknown');
    expect(VALID_QUEUE_STATUSES).not.toContain('cancelled');
    expect(VALID_QUEUE_STATUSES).not.toContain('queued');
    expect(VALID_QUEUE_STATUSES).not.toContain('');
  });
});

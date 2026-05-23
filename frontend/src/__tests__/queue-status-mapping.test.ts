import { describe, it, expect } from 'vitest';
import { toQueueJobStatus, JobStatus, QueueJobStatus } from '@/services/queueService';

describe('toQueueJobStatus maps Rust PascalCase to IndexedDB snake_case', () => {
  it('maps every Rust JobStatus variant correctly', () => {
    const cases: [JobStatus, QueueJobStatus][] = [
      ['Pending', 'pending'],
      ['InProgress', 'in_progress'],
      ['Paused', 'paused'],
      ['Done', 'done'],
      ['Failed', 'failed'],
    ];

    for (const [rust, expected] of cases) {
      expect(toQueueJobStatus(rust)).toBe(expected);
    }
  });

  it('never produces a status with the wrong casing', () => {
    const wrong = ['inprogress', 'IN_PROGRESS', 'In_Progress'];
    const all: QueueJobStatus[] = (['Pending', 'InProgress', 'Paused', 'Done', 'Failed'] as JobStatus[])
      .map(toQueueJobStatus);

    for (const w of wrong) {
      expect(all).not.toContain(w as QueueJobStatus);
    }
  });
});

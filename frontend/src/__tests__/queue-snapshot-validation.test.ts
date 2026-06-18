import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));

import { invoke } from '@tauri-apps/api/core';
import {
  normalizeQueueSnapshot,
  getQueueState,
  type QueueSnapshot,
  type QueueJob,
} from '@/services/queueService';

const mockInvoke = invoke as unknown as ReturnType<typeof vi.fn>;

const VALID_JOB: QueueJob = {
  meeting_id: 'm1',
  audio_path: '/a/audio.mp4',
  status: 'Pending',
  phase: 'Transcribing',
};

describe('normalizeQueueSnapshot', () => {
  it('coerces a payload missing jobs to an empty array (task 1.1)', () => {
    expect(normalizeQueueSnapshot({ manual_pause_all: true })).toEqual({
      jobs: [],
      manual_pause_all: true,
    });
  });

  it('coerces an absent payload to the empty snapshot (task 1.2)', () => {
    const empty: QueueSnapshot = { jobs: [], manual_pause_all: false };
    expect(normalizeQueueSnapshot(undefined)).toEqual(empty);
    expect(normalizeQueueSnapshot(null)).toEqual(empty);
  });

  it('coerces wrong-typed fields to defaults (task 1.3)', () => {
    expect(
      normalizeQueueSnapshot({ jobs: 'nope', manual_pause_all: 'yes' }),
    ).toEqual({ jobs: [], manual_pause_all: false });
  });

  it('passes a well-formed payload through unchanged (task 1.4)', () => {
    const valid: QueueSnapshot = { jobs: [VALID_JOB], manual_pause_all: false };
    expect(normalizeQueueSnapshot(valid)).toEqual(valid);
  });
});

describe('getQueueState applies the normalizer at the boundary (task 1.6)', () => {
  beforeEach(() => mockInvoke.mockReset());

  it('returns a normalized snapshot when invoke omits jobs', async () => {
    mockInvoke.mockResolvedValue({ manual_pause_all: true });
    const result = await getQueueState();
    expect(result).toEqual({ jobs: [], manual_pause_all: true });
  });
});

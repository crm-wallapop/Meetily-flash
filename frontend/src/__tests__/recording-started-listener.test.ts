/**
 * recording-started listener — pure derivation logic (task 3.1).
 *
 * The listener at TranscriptContext.tsx (setupRecordingListeners) extracts a
 * meeting_id from the Rust recording-started event and derives a fallback title
 * when the backend has no meeting name yet. This test verifies the two pure
 * helpers the listener delegates to, covering adversarial payloads per
 * CLAUDE.md §4 (missing fields, SQL-injection-like meeting_id).
 */
import { describe, it, expect } from 'vitest';
import { deriveRecordingStartedIds, recordingStartedFallbackTitle } from '@/contexts/TranscriptContext';

const FIXED_NOW = 1700000000000;

describe('deriveRecordingStartedIds', () => {
  it('uses meeting_id from the Rust payload when present', () => {
    const result = deriveRecordingStartedIds({ meeting_id: 'meeting-abc-123' }, FIXED_NOW);
    expect(result.meetingId).toBe('meeting-abc-123');
    expect(result.activeMeetingId).toBe('meeting-abc-123');
  });

  it('falls back to meeting-{timestamp} when Rust omits meeting_id (payload undefined)', () => {
    const result = deriveRecordingStartedIds(undefined, FIXED_NOW);
    expect(result.meetingId).toBe(`meeting-${FIXED_NOW}`);
    expect(result.activeMeetingId).toBeNull();
  });

  it('falls back when payload is present but has no meeting_id key', () => {
    const result = deriveRecordingStartedIds({ other: 'value' }, FIXED_NOW);
    expect(result.meetingId).toBe(`meeting-${FIXED_NOW}`);
    expect(result.activeMeetingId).toBeNull();
  });

  it('passes a SQL-injection-like meeting_id through unchanged (frontend stores; Rust parameterizes)', () => {
    const hostile = "'; DROP TABLE meetings; --";
    const result = deriveRecordingStartedIds({ meeting_id: hostile }, FIXED_NOW);
    expect(result.meetingId).toBe(hostile);
    expect(result.activeMeetingId).toBe(hostile);
  });

  it('empty-string meeting_id is treated as absent (falls back)', () => {
    const result = deriveRecordingStartedIds({ meeting_id: '' }, FIXED_NOW);
    expect(result.meetingId).toBe(`meeting-${FIXED_NOW}`);
    expect(result.activeMeetingId).toBeNull();
  });
});

describe('recordingStartedFallbackTitle', () => {
  it('formats as Meeting-{iso} with T→_ and :→-', () => {
    const title = recordingStartedFallbackTitle(FIXED_NOW);
    const expected = `Meeting ${new Date(FIXED_NOW).toISOString().slice(0, 19).replace('T', '_').replace(/:/g, '-')}`;
    expect(title).toBe(expected);
  });
});

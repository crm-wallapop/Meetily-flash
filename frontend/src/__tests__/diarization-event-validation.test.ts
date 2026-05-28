import { describe, it, expect } from 'vitest';
import { validateDiarizationPayload } from '@/services/diarizationEvents';

describe('validateDiarizationPayload', () => {
  it('accepts a valid payload', () => {
    const result = validateDiarizationPayload({
      meeting_id: 'abc-123',
      speaker_count: 2,
      segments_labeled: 15,
    });
    expect(result).toEqual({
      meeting_id: 'abc-123',
      speaker_count: 2,
      segments_labeled: 15,
    });
  });

  it('rejects null', () => {
    expect(validateDiarizationPayload(null)).toBeNull();
  });

  it('rejects undefined', () => {
    expect(validateDiarizationPayload(undefined)).toBeNull();
  });

  it('rejects a string', () => {
    expect(validateDiarizationPayload('not an object')).toBeNull();
  });

  it('rejects payload missing meeting_id', () => {
    expect(
      validateDiarizationPayload({ speaker_count: 2, segments_labeled: 5 }),
    ).toBeNull();
  });

  it('rejects payload with non-string meeting_id', () => {
    expect(
      validateDiarizationPayload({
        meeting_id: 42,
        speaker_count: 2,
        segments_labeled: 5,
      }),
    ).toBeNull();
  });

  it('rejects payload with empty meeting_id', () => {
    expect(
      validateDiarizationPayload({
        meeting_id: '',
        speaker_count: 2,
        segments_labeled: 5,
      }),
    ).toBeNull();
  });

  it('rejects payload with non-numeric speaker_count', () => {
    expect(
      validateDiarizationPayload({
        meeting_id: 'abc-123',
        speaker_count: 'two',
        segments_labeled: 5,
      }),
    ).toBeNull();
  });

  it('rejects payload with negative speaker_count', () => {
    expect(
      validateDiarizationPayload({
        meeting_id: 'abc-123',
        speaker_count: -1,
        segments_labeled: 5,
      }),
    ).toBeNull();
  });

  it('rejects payload missing segments_labeled', () => {
    expect(
      validateDiarizationPayload({
        meeting_id: 'abc-123',
        speaker_count: 2,
      }),
    ).toBeNull();
  });

  it('rejects payload with non-numeric segments_labeled', () => {
    expect(
      validateDiarizationPayload({
        meeting_id: 'abc-123',
        speaker_count: 2,
        segments_labeled: 'five',
      }),
    ).toBeNull();
  });

  it('tolerates extra fields (forward-compatible)', () => {
    const result = validateDiarizationPayload({
      meeting_id: 'abc-123',
      speaker_count: 2,
      segments_labeled: 5,
      extra_field: true,
    });
    expect(result?.meeting_id).toBe('abc-123');
  });

  it('handles payload from JSON.parse of a stringified event', () => {
    const raw = JSON.parse(
      '{"meeting_id":"x","speaker_count":3,"segments_labeled":10}',
    );
    const result = validateDiarizationPayload(raw);
    expect(result?.meeting_id).toBe('x');
    expect(result?.speaker_count).toBe(3);
  });
});

import { describe, it, expect } from 'vitest';

function validateMaxSpeakers(value: unknown): number | null {
  if (typeof value !== 'number' || !Number.isInteger(value)) return null;
  if (value < 2 || value > 20) return null;
  return value;
}

describe('max speakers cap validation', () => {
  it('rejects value below 2', () => {
    expect(validateMaxSpeakers(1)).toBeNull();
    expect(validateMaxSpeakers(0)).toBeNull();
    expect(validateMaxSpeakers(-1)).toBeNull();
  });

  it('rejects value above 20', () => {
    expect(validateMaxSpeakers(21)).toBeNull();
    expect(validateMaxSpeakers(100)).toBeNull();
  });

  it('accepts value at lower bound (2)', () => {
    expect(validateMaxSpeakers(2)).toBe(2);
  });

  it('accepts value at upper bound (20)', () => {
    expect(validateMaxSpeakers(20)).toBe(20);
  });

  it('accepts value in range (10)', () => {
    expect(validateMaxSpeakers(10)).toBe(10);
  });

  it('rejects non-integer', () => {
    expect(validateMaxSpeakers(5.5)).toBeNull();
  });

  it('rejects string', () => {
    expect(validateMaxSpeakers('10')).toBeNull();
  });

  it('rejects null', () => {
    expect(validateMaxSpeakers(null)).toBeNull();
  });
});

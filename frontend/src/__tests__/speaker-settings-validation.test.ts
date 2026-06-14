import { describe, it, expect } from 'vitest';
import {
  SPEAKER_EMBEDDING_MODELS,
  type SpeakerEmbeddingModel,
} from '@/services/speakerService';

function validateMaxSpeakers(value: unknown): number | null {
  if (typeof value !== 'number' || !Number.isInteger(value)) return null;
  if (value < 2 || value > 20) return null;
  return value;
}

function validateEmbeddingModel(value: unknown): SpeakerEmbeddingModel | null {
  if (typeof value !== 'string') return null;
  if (value === '3dspeaker' || value === 'wespeaker' || value === 'nemo_titanet' || value === 'eres2net') return value;
  return null;
}

describe('speaker model dropdown', () => {
  it('renders all embedding model options', () => {
    expect(SPEAKER_EMBEDDING_MODELS).toHaveLength(4);
    const values = SPEAKER_EMBEDDING_MODELS.map((m) => m.value);
    expect(values).toContain('3dspeaker');
    expect(values).toContain('wespeaker');
    expect(values).toContain('nemo_titanet');
    expect(values).toContain('eres2net');
  });

  it('each option has a non-empty label', () => {
    for (const m of SPEAKER_EMBEDDING_MODELS) {
      expect(m.label.length).toBeGreaterThan(0);
    }
  });
});

describe('validateEmbeddingModel', () => {
  it('accepts 3dspeaker', () => {
    expect(validateEmbeddingModel('3dspeaker')).toBe('3dspeaker');
  });

  it('accepts wespeaker', () => {
    expect(validateEmbeddingModel('wespeaker')).toBe('wespeaker');
  });

  it('accepts nemo_titanet', () => {
    expect(validateEmbeddingModel('nemo_titanet')).toBe('nemo_titanet');
  });

  it('accepts eres2net', () => {
    expect(validateEmbeddingModel('eres2net')).toBe('eres2net');
  });

  it('rejects unknown string', () => {
    expect(validateEmbeddingModel('random-model')).toBeNull();
  });

  it('rejects null', () => {
    expect(validateEmbeddingModel(null)).toBeNull();
  });

  it('rejects number', () => {
    expect(validateEmbeddingModel(42)).toBeNull();
  });
});

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

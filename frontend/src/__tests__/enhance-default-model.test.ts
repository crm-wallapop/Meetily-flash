import { describe, it, expect } from 'vitest';
import { pickDefaultModel } from '../hooks/modelSelection';

const m = (name: string) => ({ provider: 'whisper' as const, name, displayName: `🏠 Whisper: ${name}`, size_mb: 1 });

describe('pickDefaultModel (enhance dialog default + fallback notice)', () => {
  it('configured whisper model available: selects it, no notice', () => {
    const listed = [m('small'), m('large-v3')];
    const r = pickDefaultModel(listed, 'localWhisper', 'large-v3');
    expect(r.key).toBe('whisper:large-v3');
    expect(r.fallbackNotice).toBeNull();
  });

  it('configured model missing: selects best available by quality order, with notice', () => {
    const listed = [m('small'), m('large-v3-turbo-q5_0')];
    const r = pickDefaultModel(listed, 'localWhisper', 'large-v3');
    // turbo-q5_0 outranks small regardless of listing order
    expect(r.key).toBe('whisper:large-v3-turbo-q5_0');
    expect(r.fallbackNotice).toContain('large-v3');
    expect(r.fallbackNotice).toContain('large-v3-turbo-q5_0');
  });

  it('configured parakeet: selects whisper WITH an engine-substitution notice', () => {
    const listed = [m('small'), m('large-v3-turbo-q5_0')];
    const r = pickDefaultModel(listed, 'parakeet', 'parakeet-tdt-0.6b-v3-int8');
    expect(r.key).toBe('whisper:large-v3-turbo-q5_0');
    expect(r.fallbackNotice).toContain('Parakeet');
    expect(r.fallbackNotice).toContain('Whisper');
    expect(r.fallbackNotice).toContain('timestamp');
  });

  it('empty model list: nothing selectable, no notice', () => {
    const r = pickDefaultModel([], 'localWhisper', 'large-v3');
    expect(r.key).toBeNull();
    expect(r.fallbackNotice).toBeNull();
  });
});

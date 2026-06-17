import { describe, it, expect, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { loadFixture, validateFixture, FixtureValidationError, type Fixture } from './loader';

const fixturesDir = path.join(process.cwd(), 'e2e', '_fixtures');
const readFixture = (name: string): string =>
  readFileSync(path.join(fixturesDir, name), 'utf8');

const validSegment = (overrides: Record<string, unknown> = {}) => ({
  id: 'seg-1',
  text: 'Hello world',
  audio_start_time: 0,
  audio_end_time: 1500,
  duration: 1500,
  display_time: '00:00',
  confidence: 0.95,
  sequence_id: 0,
  ...overrides,
});

const validBlock = (overrides: Record<string, unknown> = {}) => ({
  id: 'blk-1',
  type: 'action_item',
  content: 'Follow up with Alice',
  color: 'default',
  ...overrides,
});

afterEach(() => {
  delete (Object.prototype as Record<string, unknown>).isAdmin;
  delete (Object.prototype as Record<string, unknown>).polluted;
});

describe('fixture loader — missing required fields (adversarial)', () => {
  it('rejects a transcript fixture missing meeting_id with a field-named error', () => {
    const payload = JSON.stringify({ segments: [validSegment()] });
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('meeting_id');
      expect((e as Error).message).toContain('meeting_id');
    }
  });

  it('rejects a summary fixture missing meeting_id with a field-named error', () => {
    const payload = JSON.stringify({ blocks: [validBlock()] });
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('meeting_id');
    }
  });
});

describe('fixture loader — empty sections (adversarial)', () => {
  it('rejects a summary fixture whose blocks array is empty', () => {
    const payload = JSON.stringify({ meeting_id: 'm1', blocks: [] });
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('blocks');
    }
  });
});

describe('fixture loader — type mutation (adversarial)', () => {
  it('rejects a string in a numeric slot via the type guard', () => {
    const payload = JSON.stringify({
      meeting_id: 'm1',
      segments: [validSegment({ audio_start_time: 'zero' })],
    });
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toContain('audio_start_time');
    }
  });

  it('rejects NaN in a numeric field via the type guard (post-parse, direct)', () => {
    const fixture = {
      meeting_id: 'm1',
      segments: [validSegment({ audio_start_time: NaN })],
    };
    expect(() => validateFixture(fixture)).toThrow(FixtureValidationError);
    try {
      validateFixture(fixture);
    } catch (e) {
      expect((e as FixtureValidationError).field).toContain('audio_start_time');
    }
  });

  it('rejects Infinity in a numeric field via the type guard (post-parse, direct)', () => {
    const fixture = {
      meeting_id: 'm1',
      segments: [validSegment({ audio_end_time: Infinity })],
    };
    expect(() => validateFixture(fixture)).toThrow(FixtureValidationError);
    try {
      validateFixture(fixture);
    } catch (e) {
      expect((e as FixtureValidationError).field).toContain('audio_end_time');
    }
  });

  it('rejects malformed JSON at parse time — bare NaN token surfaces as field=json', () => {
    const payload = `{"meeting_id":"m1","segments":[{"id":"s1","text":"hi","audio_start_time":NaN,"audio_end_time":1,"duration":1,"display_time":"00:00","confidence":0.9,"sequence_id":0}]}`;
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('json');
    }
  });

  it('rejects malformed JSON at parse time — bare Infinity token surfaces as field=json', () => {
    const payload = `{"meeting_id":"m1","segments":[{"id":"s1","text":"hi","audio_start_time":Infinity,"audio_end_time":1,"duration":1,"display_time":"00:00","confidence":0.9,"sequence_id":0}]}`;
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('json');
    }
  });
});

describe('fixture loader — prototype pollution (adversarial)', () => {
  it('rejects a bare __proto__ payload at root with field=root and no pollution', () => {
    const payload = `{"__proto__":{"isAdmin":true}}`;
    expect(() => loadFixture(payload)).toThrow(FixtureValidationError);
    try {
      loadFixture(payload);
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('root');
    }
    expect(Object.prototype.isAdmin).toBeUndefined();
  });

  it('loads a transcript fixture with __proto__ without polluting Object.prototype and freezes result', () => {
    const payload = `{"meeting_id":"m1","segments":[],"__proto__":{"isAdmin":true}}`;
    const result = loadFixture(payload);
    expect(Object.prototype.isAdmin).toBeUndefined();
    expect(Object.isFrozen(result)).toBe(true);
    expect(Object.keys(result)).not.toContain('__proto__');
  });

  it('strips constructor.prototype payloads', () => {
    const payload = `{"meeting_id":"m1","blocks":[{"id":"b","type":"t","content":"c","color":"x"}],"constructor":{"prototype":{"polluted":true}}}`;
    const result = loadFixture(payload);
    expect(Object.prototype.polluted).toBeUndefined();
    expect(Object.isFrozen(result)).toBe(true);
    expect(Object.keys(result)).not.toContain('constructor');
  });

  it('strips nested __proto__ keys inside segment objects', () => {
    const payload = `{"meeting_id":"m1","segments":[{"id":"s1","text":"hi","audio_start_time":0,"audio_end_time":1,"duration":1,"display_time":"00:00","confidence":0.9,"sequence_id":0,"__proto__":{"isAdmin":true}}]}`;
    const result = loadFixture(payload) as Extract<Fixture, { kind: 'transcript' }>;
    expect(Object.prototype.isAdmin).toBeUndefined();
    expect(Object.isFrozen(result)).toBe(true);
    expect(Object.keys(result.segments[0])).not.toContain('__proto__');
  });
});

describe('fixture loader — reference fixture files', () => {
  it('loads transcript-30s-multi-speaker.json as a multi-segment transcript', () => {
    const result = loadFixture(readFixture('transcript-30s-multi-speaker.json')) as Extract<
      Fixture,
      { kind: 'transcript' }
    >;
    expect(result.kind).toBe('transcript');
    expect(result.meeting_id).toBe('meet-30s-multi');
    expect(result.segments.length).toBeGreaterThanOrEqual(3);
    expect(Object.isFrozen(result)).toBe(true);
  });

  it('loads summary-action-items.json as a multi-block summary', () => {
    const result = loadFixture(readFixture('summary-action-items.json')) as Extract<
      Fixture,
      { kind: 'summary' }
    >;
    expect(result.kind).toBe('summary');
    expect(result.blocks.length).toBeGreaterThanOrEqual(2);
    expect(result.blocks.some((b) => b.type === 'action_item')).toBe(true);
    expect(Object.isFrozen(result)).toBe(true);
  });

  it('loads transcript-non-latin.json with ES/CA/mixed content', () => {
    const result = loadFixture(readFixture('transcript-non-latin.json')) as Extract<
      Fixture,
      { kind: 'transcript' }
    >;
    expect(result.kind).toBe('transcript');
    const combined = result.segments.map((s) => s.text).join(' ');
    expect(combined).toMatch(/días|Bon dia|pressupostos/);
  });

  it('rejects summary-empty-blocks.json with field=blocks', () => {
    expect(() => loadFixture(readFixture('summary-empty-blocks.json'))).toThrow(
      FixtureValidationError,
    );
    try {
      loadFixture(readFixture('summary-empty-blocks.json'));
    } catch (e) {
      expect((e as FixtureValidationError).field).toBe('blocks');
    }
  });
});

describe('fixture loader — happy path', () => {
  it('loads a valid transcript fixture and freezes it', () => {
    const payload = JSON.stringify({
      meeting_id: 'm1',
      segments: [validSegment()],
    });
    const result = loadFixture(payload) as Extract<Fixture, { kind: 'transcript' }>;
    expect(result.kind).toBe('transcript');
    expect(result.meeting_id).toBe('m1');
    expect(result.segments).toHaveLength(1);
    expect(result.segments[0].text).toBe('Hello world');
    expect(Object.isFrozen(result)).toBe(true);
    expect(Object.isFrozen(result.segments)).toBe(true);
    expect(Object.isFrozen(result.segments[0])).toBe(true);
  });

  it('loads a valid summary fixture and freezes it', () => {
    const payload = JSON.stringify({
      meeting_id: 'm1',
      blocks: [validBlock()],
    });
    const result = loadFixture(payload) as Extract<Fixture, { kind: 'summary' }>;
    expect(result.kind).toBe('summary');
    expect(result.blocks).toHaveLength(1);
    expect(Object.isFrozen(result)).toBe(true);
  });
});

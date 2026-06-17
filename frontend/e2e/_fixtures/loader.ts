import type { TranscriptHistorySegment, Block } from '@/types';

export interface TranscriptFixture {
  // `kind` is synthetic — assigned by the loader, not present in fixture files —
  // so downstream consumers (the Playwright mock dispatcher) can exhaustively
  // switch on the Fixture union without sniffing keys.
  kind: 'transcript';
  meeting_id: string;
  segments: TranscriptHistorySegment[];
}

export interface SummaryFixture {
  kind: 'summary';
  meeting_id: string;
  blocks: Block[];
}

export type Fixture = TranscriptFixture | SummaryFixture;

export class FixtureValidationError extends Error {
  constructor(
    public readonly field: string,
    message: string,
  ) {
    super(`fixture validation failed: ${field} — ${message}`);
    this.name = 'FixtureValidationError';
  }
}

const FORBIDDEN_KEYS = new Set(['__proto__', 'constructor', 'prototype']);

function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function asString(v: unknown, field: string): string {
  if (typeof v !== 'string' || v.length === 0) {
    throw new FixtureValidationError(field, 'expected non-empty string');
  }
  return v;
}

function asFiniteNumber(v: unknown, field: string): number {
  if (typeof v !== 'number' || !Number.isFinite(v)) {
    throw new FixtureValidationError(field, 'expected finite number');
  }
  return v;
}

// Strip __proto__/constructor/prototype keys before validation. Validation
// reconstructs objects from known fields, so this is defense-in-depth: it
// neutralizes the root-level payload (which reaches the discriminator branch
// before reconstruction) and guards against future code paths that might
// forward the raw parsed object without revalidation.
function sanitize(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sanitize);
  if (isPlainObject(value)) {
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(value)) {
      if (FORBIDDEN_KEYS.has(key)) continue;
      out[key] = sanitize(value[key]);
    }
    return out;
  }
  return value;
}

function validateTranscript(raw: Record<string, unknown>): TranscriptFixture {
  const meeting_id = asString(raw.meeting_id, 'meeting_id');
  const segs = raw.segments;
  if (!Array.isArray(segs)) {
    throw new FixtureValidationError('segments', 'expected array');
  }
  const segments = segs.map((s, i) => {
    if (!isPlainObject(s)) {
      throw new FixtureValidationError(`segments[${i}]`, 'expected object');
    }
    return {
      id: asString(s.id, `segments[${i}].id`),
      text: asString(s.text, `segments[${i}].text`),
      audio_start_time: asFiniteNumber(s.audio_start_time, `segments[${i}].audio_start_time`),
      audio_end_time: asFiniteNumber(s.audio_end_time, `segments[${i}].audio_end_time`),
      duration: asFiniteNumber(s.duration, `segments[${i}].duration`),
      display_time: asString(s.display_time, `segments[${i}].display_time`),
      confidence: asFiniteNumber(s.confidence, `segments[${i}].confidence`),
      sequence_id: asFiniteNumber(s.sequence_id, `segments[${i}].sequence_id`),
    } satisfies TranscriptHistorySegment;
  });
  return { kind: 'transcript', meeting_id, segments };
}

function validateSummary(raw: Record<string, unknown>): SummaryFixture {
  const meeting_id = asString(raw.meeting_id, 'meeting_id');
  const blocksRaw = raw.blocks;
  if (!Array.isArray(blocksRaw) || blocksRaw.length === 0) {
    throw new FixtureValidationError('blocks', 'expected non-empty array');
  }
  const blocks = blocksRaw.map((b, i) => {
    if (!isPlainObject(b)) {
      throw new FixtureValidationError(`blocks[${i}]`, 'expected object');
    }
    return {
      id: asString(b.id, `blocks[${i}].id`),
      type: asString(b.type, `blocks[${i}].type`),
      content: asString(b.content, `blocks[${i}].content`),
      color: asString(b.color, `blocks[${i}].color`),
    } satisfies Block;
  });
  return { kind: 'summary', meeting_id, blocks };
}

function deepFreeze<T>(value: T): T {
  if (Array.isArray(value)) {
    value.forEach(deepFreeze);
  } else if (isPlainObject(value)) {
    for (const key of Object.keys(value)) deepFreeze(value[key]);
  }
  return Object.freeze(value);
}

export function validateFixture(raw: unknown): Fixture {
  const cleaned = sanitize(raw);
  if (!isPlainObject(cleaned)) {
    throw new FixtureValidationError('root', 'expected object at top level');
  }
  let fixture: Fixture;
  if ('segments' in cleaned) {
    fixture = validateTranscript(cleaned);
  } else if ('blocks' in cleaned) {
    fixture = validateSummary(cleaned);
  } else {
    throw new FixtureValidationError(
      'root',
      'unrecognized fixture shape — expected "segments" or "blocks"',
    );
  }
  return deepFreeze(fixture);
}

export function loadFixture(json: string): Fixture {
  let parsed: unknown;
  try {
    parsed = JSON.parse(json);
  } catch (e) {
    throw new FixtureValidationError('json', `invalid JSON: ${(e as Error).message}`);
  }
  return validateFixture(parsed);
}

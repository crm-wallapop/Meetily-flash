import { describe, it, expect } from 'vitest';
import { isOllamaNotInstalledError } from '../lib/utils';
import {
  getModelIcon,
  getStatusColor as getParakeetStatusColor,
  formatFileSize,
  isQuantizedModel,
  getModelPerformanceBadge,
  getRecommendedModel,
  type ModelStatus,
} from '../lib/parakeet';
import {
  isModelAvailable,
  isModelDownloading,
  isModelNotDownloaded,
  isModelCorrupted,
  isModelError,
  getStatusColor as getBuiltInStatusColor,
  getStatusLabel,
  type BuiltInModelStatus,
} from '../lib/builtin-ai';
import { loadBetaFeatures, DEFAULT_BETA_FEATURES } from '../types/betaFeatures';

// ---------------------------------------------------------------------------
// isOllamaNotInstalledError
// ---------------------------------------------------------------------------
describe('isOllamaNotInstalledError', () => {
  it.each([
    'Cannot connect to Ollama server',
    'connection refused',
    'ECONNREFUSED 127.0.0.1:11434',
    'Ollama CLI not found',
    'ollama not in path',
    'Please check if the server is running',
    'Please check if the Ollama server is running',
  ])('returns true for: %s', (msg) => {
    expect(isOllamaNotInstalledError(msg)).toBe(true);
  });

  it.each([
    'Model not found',
    'Timeout exceeded',
    'Invalid API key',
    '',
  ])('returns false for: %s', (msg) => {
    expect(isOllamaNotInstalledError(msg)).toBe(false);
  });

  it('is case-insensitive', () => {
    expect(isOllamaNotInstalledError('CONNECTION REFUSED')).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// parakeet helpers
// ---------------------------------------------------------------------------
describe('getModelIcon', () => {
  it('returns fire emoji for High accuracy', () => expect(getModelIcon('High')).toBe('🔥'));
  it('returns bolt emoji for Good accuracy', () => expect(getModelIcon('Good')).toBe('⚡'));
  it('returns rocket emoji for Decent accuracy', () => expect(getModelIcon('Decent')).toBe('🚀'));
});

describe('getParakeetStatusColor', () => {
  it('returns green for Available', () => {
    expect(getParakeetStatusColor('Available')).toBe('green');
  });

  it('returns gray for Missing', () => {
    expect(getParakeetStatusColor('Missing')).toBe('gray');
  });

  it('returns blue for Downloading object', () => {
    const status: ModelStatus = { Downloading: 50 };
    expect(getParakeetStatusColor(status)).toBe('blue');
  });

  it('returns red for Error object', () => {
    const status: ModelStatus = { Error: 'disk full' };
    expect(getParakeetStatusColor(status)).toBe('red');
  });
});

describe('formatFileSize', () => {
  it('formats MB below 1000', () => expect(formatFileSize(512)).toBe('512MB'));
  it('formats exactly 1000 as GB', () => expect(formatFileSize(1000)).toBe('1.0GB'));
  it('formats 1500 as 1.5GB', () => expect(formatFileSize(1500)).toBe('1.5GB'));
  it('formats 999 as MB (boundary)', () => expect(formatFileSize(999)).toBe('999MB'));
});

describe('isQuantizedModel', () => {
  it('detects int8 in name', () => expect(isQuantizedModel('parakeet-tdt-0.6b-v3-int8')).toBe(true));
  it('returns false for FP32 model', () => expect(isQuantizedModel('parakeet-tdt-0.6b-v3')).toBe(false));
});

describe('getModelPerformanceBadge', () => {
  it('FP32 returns Full Precision / blue', () => {
    expect(getModelPerformanceBadge('FP32')).toEqual({ label: 'Full Precision', color: 'blue' });
  });
  it('Int8 returns Int8 Quantized / green', () => {
    expect(getModelPerformanceBadge('Int8')).toEqual({ label: 'Int8 Quantized', color: 'green' });
  });
});

describe('getRecommendedModel', () => {
  it('returns int8 model by default', () => {
    expect(getRecommendedModel()).toContain('int8');
  });
  it('returns int8 model regardless of specs', () => {
    expect(getRecommendedModel({ ram: 64, cores: 16 })).toContain('int8');
  });
});

// ---------------------------------------------------------------------------
// builtin-ai status helpers
// ---------------------------------------------------------------------------
describe('BuiltInModelStatus guards', () => {
  const available: BuiltInModelStatus = { type: 'available' };
  const downloading: BuiltInModelStatus = { type: 'downloading', progress: 42 };
  const notDownloaded: BuiltInModelStatus = { type: 'not_downloaded' };
  const corrupted: BuiltInModelStatus = { type: 'corrupted', file_size: 100, expected_min_size: 500 };
  const errored: BuiltInModelStatus = { type: 'error', Error: 'network failure' };

  it.each([
    ['isModelAvailable', isModelAvailable, available, true],
    ['isModelAvailable', isModelAvailable, downloading, false],
    ['isModelDownloading', isModelDownloading, downloading, true],
    ['isModelDownloading', isModelDownloading, available, false],
    ['isModelNotDownloaded', isModelNotDownloaded, notDownloaded, true],
    ['isModelNotDownloaded', isModelNotDownloaded, available, false],
    ['isModelCorrupted', isModelCorrupted, corrupted, true],
    ['isModelCorrupted', isModelCorrupted, available, false],
    ['isModelError', isModelError, errored, true],
    ['isModelError', isModelError, available, false],
  ])('%s(%s) === %s', (_fn, fn, status, expected) => {
    expect(fn(status as BuiltInModelStatus)).toBe(expected);
  });
});

describe('getBuiltInStatusColor', () => {
  it.each([
    [{ type: 'available' } as BuiltInModelStatus, 'green'],
    [{ type: 'downloading', progress: 10 } as BuiltInModelStatus, 'blue'],
    [{ type: 'not_downloaded' } as BuiltInModelStatus, 'gray'],
    [{ type: 'corrupted', file_size: 0, expected_min_size: 100 } as BuiltInModelStatus, 'red'],
    [{ type: 'error', Error: 'x' } as BuiltInModelStatus, 'red'],
  ])('status %j → %s', (status, color) => {
    expect(getBuiltInStatusColor(status)).toBe(color);
  });
});

describe('getStatusLabel', () => {
  it('shows progress percentage while downloading', () => {
    expect(getStatusLabel({ type: 'downloading', progress: 73 })).toBe('Downloading 73%');
  });
  it('labels available correctly', () => {
    expect(getStatusLabel({ type: 'available' })).toBe('Available');
  });
});

// ---------------------------------------------------------------------------
// loadBetaFeatures — tests server-side path (window === undefined)
// ---------------------------------------------------------------------------
describe('loadBetaFeatures (SSR path)', () => {
  it('returns defaults when window is undefined', () => {
    // jsdom sets window; simulate Node/SSR by temporarily removing it
    const original = globalThis.window;
    // @ts-expect-error intentional deletion for SSR test
    delete globalThis.window;
    try {
      expect(loadBetaFeatures()).toEqual(DEFAULT_BETA_FEATURES);
    } finally {
      globalThis.window = original;
    }
  });
});

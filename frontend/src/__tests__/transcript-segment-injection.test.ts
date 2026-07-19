import { describe, it, expect } from 'vitest';
import { computeDisplayText } from '@/components/VirtualizedTranscriptView';

// §4 adversarial category: prompt injection / XSS via transcript text.
//
// TranscriptSegment renders `text` inside <p>{displayText}</p>. React escapes
// text content by default, so adversarial HTML in the transcript is inert as
// long as (a) the processing layer passes text through without transforming it
// and (b) no dangerouslySetInnerHTML appears in the render path (guarded by
// the separate dangerouslySetInnerHTML lint/test in task 8.4).
//
// This test exercises the real `computeDisplayText` (the exact function
// TranscriptSegment calls) with §4 adversarial payloads to prove property (a):
// the processing layer is pass-through for HTML — it only strips filler words,
// never sanitizes or restructures markup. XSS protection is React's job.
describe('computeDisplayText — adversarial payloads pass through unchanged (§4)', () => {
  const adversarial: Array<{ label: string; payload: string }> = [
    { label: '<script> tag', payload: '<script>alert(1)</script>' },
    { label: '<img onerror>', payload: '<img src=x onerror="alert(1)">' },
    { label: '<svg onload>', payload: '<svg onload="alert(1)">' },
    { label: '<iframe>', payload: '<iframe src="javascript:alert(1)"></iframe>' },
    { label: 'javascript: URL', payload: 'javascript:alert(1)' },
    { label: 'template injection ${...}', payload: '${7*7}' },
    { label: 'mixed-case <SCRIPT>', payload: '<SCRIPT>alert(1)</SCRIPT>' },
    { label: 'SQL-style injection in prose', payload: "'; DROP TABLE meetings; --" },
  ];

  for (const { label, payload } of adversarial) {
    it(`does not sanitize or restructure ${label} — passes through for React to escape`, () => {
      // No filler words in any payload → cleanStopWords is a no-op → text is
      // returned verbatim (modulo whitespace collapse/trim). This is the
      // property that matters: the processing layer never touches HTML.
      expect(computeDisplayText(payload)).toBe(payload.replace(/\s+/g, ' ').trim());
    });
  }

  it('collapses to [Silence] for empty/whitespace-only text', () => {
    expect(computeDisplayText('')).toBe('[Silence]');
    expect(computeDisplayText('   ')).toBe('[Silence]');
  });

  it('strips filler words from normal speech but keeps the substance', () => {
    expect(computeDisplayText('uh hello world um')).toBe('hello world');
    expect(computeDisplayText('uh, let me think')).toBe('let me think');
  });

  it('preserves adversarial payload even when surrounded by filler words', () => {
    // Filler words are stripped, the adversarial fragment survives intact.
    expect(computeDisplayText('uh <script>alert(1)</script> um')).toBe('<script>alert(1)</script>');
  });
});

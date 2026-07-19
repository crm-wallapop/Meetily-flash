import { describe, test, expect } from 'vitest';
import * as fs from 'node:fs';
import * as path from 'node:path';

// §9 boundary-security guard. dangerouslySetInnerHTML bypasses React's
// automatic text escaping — the exact property that makes transcript text safe
// to render (see transcript-segment-injection.test.ts). The only sanctioned use
// is the notes page, which renders trusted user-authored markdown. Any new
// usage in a transcript-rendering component would re-open the XSS surface this
// change closed; this guard fails the suite if one appears.
const SRC_ROOT = path.join(process.cwd(), 'src');
const BASELINE_FILE = 'app/notes/[id]/page.tsx';

function walkSource(dir: string, acc: string[] = []): string[] {
  for (const entry of fs.readdirSync(dir)) {
    if (entry === 'node_modules' || entry === '.next' || entry === '__tests__') continue;
    const full = path.join(dir, entry);
    if (fs.statSync(full).isDirectory()) {
      walkSource(full, acc);
    } else if (/\.(ts|tsx)$/.test(entry) && !/\.test\.(ts|tsx)$/.test(entry)) {
      acc.push(full);
    }
  }
  return acc;
}

describe('dangerouslySetInnerHTML guard (§9 boundary security)', () => {
  test(`the only usage in frontend/src is the baseline ${BASELINE_FILE}`, () => {
    const hits: string[] = [];
    for (const f of walkSource(SRC_ROOT)) {
      const lines = fs.readFileSync(f, 'utf8').split('\n');
      lines.forEach((line, i) => {
        const trimmed = line.trimStart();
        if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) return;
        if (line.includes('dangerouslySetInnerHTML')) {
          hits.push(`${path.relative(SRC_ROOT, f).replace(/\\/g, '/')}:${i + 1}`);
        }
      });
    }
    const offenderFiles = [...new Set(hits.map((h) => h.replace(/:\d+$/, '')))];
    expect(offenderFiles, `raw hits: ${hits.join(', ')}`).toEqual([BASELINE_FILE]);
  });
});

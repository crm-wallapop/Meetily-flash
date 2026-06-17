import { describe, test, expect, afterEach } from 'vitest';
import { ESLint } from 'eslint';
import * as fs from 'node:fs';
import * as path from 'node:path';

// Task 3.2 — meta-test proving the no-restricted-syntax override in
// .eslintrc.json bans fixed-sleep primitives in e2e specs. Fixed sleeps
// (page.waitForTimeout, setTimeout, sleep) are the #1 cause of flaky UI tests;
// the rule forces authors to use auto-waiting primitives instead. This test
// lints a deliberately-flaky spec using the REAL .eslintrc.json (not a re-
// declared config) so it catches rule regressions or accidental removal.

const META_DIR = path.join(process.cwd(), 'e2e', '__meta_determinism__');
const FLAKY_SPEC = path.join(META_DIR, 'flaky.spec.ts');

const FLAKY_SOURCE = [
  "import { test } from '@playwright/test';",
  "test('flaky', async ({ page }) => {",
  '  await page.waitForTimeout(5000);',
  '  await page.click("#btn");',
  '});',
  '',
].join('\n');

afterEach(() => {
  if (fs.existsSync(FLAKY_SPEC)) fs.unlinkSync(FLAKY_SPEC);
  if (fs.existsSync(META_DIR)) fs.rmSync(META_DIR, { recursive: true, force: true });
});

describe('determinism ESLint rule (3.2)', () => {
  // Each test spins up a real ESLint instance with useEslintrc. The FIRST invocation in
  // the process pays the cold @typescript-eslint parser + config-cascade load, which on
  // Windows exceeds vitest's default 5s budget (~8-12s observed). Subsequent tests reuse
  // the warmed modules and finish in tens of ms.
  const ESLINT_TEST_TIMEOUT = 30_000;

  test('page.waitForTimeout in an e2e spec triggers the ban rule from .eslintrc.json', async () => {
    fs.mkdirSync(META_DIR, { recursive: true });
    fs.writeFileSync(FLAKY_SPEC, FLAKY_SOURCE);

    const eslint = new ESLint({ useEslintrc: true, cwd: process.cwd() });
    const results = await eslint.lintFiles([FLAKY_SPEC]);
    const messages = results[0]?.messages ?? [];
    const banned = messages.filter((m) => m.ruleId === 'no-restricted-syntax');

    expect(banned.length, 'waitForTimeout must be flagged').toBeGreaterThan(0);
    expect(banned[0]?.message).toMatch(/auto-wait/i);
  }, ESLINT_TEST_TIMEOUT);

  test('setTimeout in an e2e spec triggers the ban rule', async () => {
    fs.mkdirSync(META_DIR, { recursive: true });
    fs.writeFileSync(
      FLAKY_SPEC,
      [
        "import { test } from '@playwright/test';",
        "test('flaky-sleep', async ({ page }) => {",
        '  await new Promise((r) => setTimeout(r, 3000));',
        '  await page.click("#btn");',
        '});',
        '',
      ].join('\n'),
    );

    const eslint = new ESLint({ useEslintrc: true, cwd: process.cwd() });
    const results = await eslint.lintFiles([FLAKY_SPEC]);
    const messages = results[0]?.messages ?? [];
    const banned = messages.filter((m) => m.ruleId === 'no-restricted-syntax');

    expect(banned.length, 'setTimeout must be flagged').toBeGreaterThan(0);
  }, ESLINT_TEST_TIMEOUT);
});

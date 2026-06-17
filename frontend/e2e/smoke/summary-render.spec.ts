import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import {
  SMOKE_MEETING_DETAILS_INIT_SCRIPT,
  SUMMARY_MULTI_BLOCK,
  SUMMARY_EMPTY_BLOCKS,
} from './_meeting-details';

// Task 4.3 — locks in the regression class from 1.3 (LLM/Summary: empty sections
// must not crash the renderer). Drives the meeting-details page with two summary
// fixtures via the legacy-format path (page.tsx ~line 282), which is where the
// explicit empty-blocks / invalid-blocks defensive handling lives:
//   - multi-block: section blocks render as visible text
//   - empty-blocks: the page renders without crashing (the defensive path sets
//     blocks: [] and the renderer must tolerate it — no exception, no blank screen)
//
// The test sets window.__smokeSummaryData before each navigation so the
// api_get_summary handler serves the right fixture.

const MEETING_URL = '/meeting-details?id=meet-summary-001';

async function setSummaryData(page: import('@playwright/test').Page, data: unknown): Promise<void> {
  await page.addInitScript((d) => {
    (window as unknown as { __smokeSummaryData?: unknown }).__smokeSummaryData = d;
  }, data);
}

test.describe('summary-render smoke (4.3)', () => {
  test.beforeEach(() => {
    // Cold first-compile of /meeting-details (BlockNote editor + ~6k modules) takes ~37s
    // on a warm dev server; the default 30s test budget aborts page.goto's "load" wait
    // mid-compile before the page ever renders.
    test.setTimeout(90_000);
  });

  test('multi-block summary renders section content', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);
    await setSummaryData(page, SUMMARY_MULTI_BLOCK);

    await page.goto(MEETING_URL);
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // A block's content text from the fixture must appear in the rendered DOM.
    await expect(page.getByText('Carol to draft the Q3 board update by Thursday').first()).toBeVisible({
      timeout: 20_000,
    });
  });

  test('empty-blocks summary does not crash the renderer', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    page.on('dialog', (d) => d.dismiss());

    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);
    await setSummaryData(page, SUMMARY_EMPTY_BLOCKS);

    await page.goto(MEETING_URL);
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Positive signal the meeting loaded past the metadata gate and transcripts
    // rendered — without this the "no crash" assertions below could pass vacuously
    // on a blank or "Failed to load" page (which is exactly what happened before the
    // api_get_meeting_metadata mock was wired).
    await expect(page.getByText('Smoke transcript segment one.').first()).toBeVisible({
      timeout: 20_000,
    });

    // The regression class (task 1.3): the defensive legacy path sets blocks: [] per
    // section; the renderer must tolerate it. Assert no uncaught pageerror fired and
    // the page body remains interactive (not a React error-boundary blank).
    await expect(page.locator('body')).toBeVisible();
    expect(pageErrors, 'empty-blocks summary must not throw a pageerror').toEqual([]);
  });
});

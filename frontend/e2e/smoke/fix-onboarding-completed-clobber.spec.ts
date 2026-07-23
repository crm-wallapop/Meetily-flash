import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';

// fix-onboarding-completed-clobber — the onboarding-persistence contract.
//
// What this spec DOES assert:
//   OnboardingContext's auto-save effect (lines 183-197) debounces at 1s. The
//   fix moves setCompleted to BEFORE the slow verifyModelStatus await so the
//   existing guard (if completed) blocks the auto-save before the debounce can
//   fire with the mount-time default. This spec forces the race window open by
//   delaying parakeet_init past the 1s debounce, then asserts the clobbering
//   command never reaches the invoke call log.
//
// What this spec CANNOT assert (covered elsewhere):
//   - The Rust store layer (onboarding.rs) — cargo tests.
//   - layout.tsx:84's independent get_onboarding_status read — that gate has no
//     race (single fast invoke, no auto-save); it's transitively fixed once the
//     store stops being clobbered.

// Delay parakeet_init past the 1s auto-save debounce to force the race window
// open. Without this, verifyModelStatus resolves instantly (unregistered probes
// throw + get caught) and the test would pass even on the buggy code.
const SLOW_PROBES_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;
  d.register('parakeet_init', function () {
    return new Promise(function (resolve) {
      setTimeout(function () {
        window.__smokeParakeetInitDone = true;
        resolve();
      }, 3000);
    });
  });
  d.register('parakeet_has_available_models', function () { return true; });
  d.register('builtin_ai_get_available_summary_model', function () { return 'test-model'; });
})();
`;

const NULL_STATUS_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;
  d.register('get_onboarding_status', function () { return null; });
})();
`;

async function callLogIncludes(
  page: import('@playwright/test').Page,
  cmd: string,
): Promise<boolean> {
  return page.evaluate(
    (c) =>
      (
        window as unknown as {
          __tauriMockDispatcher: { callLog: () => string[] };
        }
      ).__tauriMockDispatcher.callLog().includes(c),
    cmd,
  );
}

test.describe('onboarding persistence smoke (fix-onboarding-completed-clobber)', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('slow model verification does not fire a clobbering save', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SLOW_PROBES_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Wait for the slow parakeet_init to resolve. The 1s auto-save debounce
    // fires well before this point (at t=1s vs init resolving at t=2s), so if
    // the bug were present the clobbering invoke would already be in the log.
    await expect
      .poll(
        () =>
          page.evaluate(
            () => (window as unknown as { __smokeParakeetInitDone?: boolean }).__smokeParakeetInitDone === true,
          ),
        { timeout: 15_000 },
      )
      .toBe(true);

    // The clobber command must not appear in the call log. The dispatcher
    // pushes every command before checking handlers (init-script.ts), so even
    // an unregistered save is captured — this proves the auto-save effect's
    // guard blocked the write, not merely that the invoke failed.
    expect(await callLogIncludes(page, 'save_onboarding_status_cmd')).toBe(false);
  });

  test('first launch (null status) shows onboarding', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(NULL_STATUS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    await expect(page.getByText('Welcome to Meetily')).toBeVisible({ timeout: 10_000 });
  });
});

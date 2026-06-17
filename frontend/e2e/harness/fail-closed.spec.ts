import { test, expect } from '@playwright/test';
import {
  TAURI_MOCK_INIT_SCRIPT,
  TAURI_INTERNALS_SPY_INIT_SCRIPT,
} from '../mocks/init-script';

// Task 2.4 — adversarial: dispatcher drift. Proves the fail-closed contract:
// an invoke for a command no test registered must reject with an error that
// names the offending command. If a future change makes the dispatcher swallow
// unknown commands (return undefined, log-and-fallback, etc.), this spec fails.
// The contract is what prevents a silently-broken smoke test from reporting
// green when a real Rust command has been renamed or removed.

test.describe('fail-closed dispatcher (2.4)', () => {
  test.beforeEach(async ({ page }) => {
    await page.addInitScript(TAURI_INTERNALS_SPY_INIT_SCRIPT);
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );
  });

  test('invoke on an unregistered command rejects with an error naming that command', async ({ page }) => {
    // Catch inside evaluate so the rejection surfaces as a string we can assert
    // on, rather than a Playwright-level Error that obscures the message.
    const outcome = await page.evaluate(() =>
      (window as unknown as {
        __tauriMockInvoke: (cmd: string, args?: unknown) => Promise<unknown>;
      }).__tauriMockInvoke('brand_new_command_not_yet_registered', {}).then(
        () => 'RESOLVED-UNEXPECTEDLY',
        (e: Error) => `REJECTED:${e.message}`,
      ),
    );

    expect(outcome, 'dispatcher must reject, not silently resolve').not.toBe(
      'RESOLVED-UNEXPECTEDLY',
    );
    expect(outcome).toContain('brand_new_command_not_yet_registered');
  });
});

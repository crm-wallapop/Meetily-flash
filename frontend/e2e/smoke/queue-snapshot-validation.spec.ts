import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';

// Regression for the sidebar queue crash: `Cannot read properties of
// undefined (reading 'filter'/'find')`. The queue adapter trusted the IPC
// payload via a bare cast, so a `transcription-queue-changed` event (or
// `get_queue_state` response) missing a `jobs` array made the
// useRetranscriptionProgress listener call `snapshot.jobs.filter(...)` on
// undefined and throw. The home page is gated behind a no-microphone
// permissions alert in the smoke env, so meeting items don't render on `/`;
// the reliable crash trigger is therefore the event payload, emitted directly
// through the mock event bus. The malformed `get_queue_state` override also
// exercises the seed path.

const MALFORMED_QUEUE_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;
  // Override the defaults' well-formed handler with a payload MISSING jobs.
  d.register('get_queue_state', function () {
    return { manual_pause_all: true };
  });
})();
`;

async function callLogIncludes(page: import('@playwright/test').Page, cmd: string): Promise<boolean> {
  return page.evaluate(
    (c) => (window as unknown as { __tauriMockDispatcher: { callLog: () => string[] } })
      .__tauriMockDispatcher.callLog().includes(c),
    cmd,
  );
}

test.describe('queue-snapshot-validation smoke (2.1)', () => {
  test('a malformed transcription-queue-changed event does not crash the sidebar', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(e.message));
    page.on('dialog', (d) => d.dismiss());

    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(MALFORMED_QUEUE_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // useQueueSnapshot calls get_queue_state and subscribes useRetranscriptionProgress
    // to transcription-queue-changed in the same synchronous effect tick, so this
    // guarantees the listener is registered before we emit.
    await expect.poll(() => callLogIncludes(page, 'get_queue_state'), {
      timeout: 15_000,
    }).toBe(true);

    // Emit a malformed queue event (no jobs). Before the fix, the
    // useRetranscriptionProgress listener called snapshot.jobs.filter(...) on
    // undefined and threw inside this emit; the try/catch captures that throw.
    const emitError = await page.evaluate(() => {
      const bus = (window as unknown as {
        __tauriMockEventBus: { emit: (event: string, payload: unknown) => void };
      }).__tauriMockEventBus;
      try {
        bus.emit('transcription-queue-changed', { manual_pause_all: true });
        return null;
      } catch (e) {
        return e instanceof Error ? e.message : String(e);
      }
    });
    expect(emitError).toBeNull();

    // No uncaught error escaped to the page (covers the seed/render paths).
    expect(pageErrors).toEqual([]);
  });
});

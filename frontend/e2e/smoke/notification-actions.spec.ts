import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT, SMOKE_MEETING_TITLE } from './_defaults';

// notification-actions §3 / task 7.1 — smoke test for the actionable-toast flow.
//
// What this spec DOES assert (the frontend half of the Continue action):
//   The Rust deep-link handler emits `recording-continue-requested` { title } when the
//   user taps [Continue recording] on the stopped toast. useRecordingStart subscribes to
//   that event and, when idle, reuses the normal start path → invoke('start_recording_-
//   with_devices_and_meeting'). We emit the event via the mock bus and assert the start
//   command lands in the dispatcher call log, and that the listener is NOT subscribed
//   while a recording is already active (the frontend mirrors the Rust resolve() guard).
//
// What this spec CANNOT assert (covered elsewhere or deferred):
//   - OS toast rendering (buttons render, tap fires the meetily:// URI) — not observable
//     from the webview; deferred to manual QA per task 3.2.
//   - Rust URI dispatch (meetily://recording/{stop,continue} parsing, adversarial
//     rejection of wrong scheme/host/action/query/fragment) — the pure use case is
//     covered by 14 unit tests in use_cases::notification_action (cargo test). The
//     webview mock replaces Tauri, so the `__dev_inject_deep_link` seam never reaches
//     real Rust here.
//   - The Stop action (meetily://recording/stop) routes to stop_recording — same reason;
//     covered structurally by the dispatch unit tests + the composition-root handler.

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

async function listenerCount(
  page: import('@playwright/test').Page,
  event: string,
): Promise<number> {
  return page.evaluate(
    (e) =>
      (
        window as unknown as {
          __tauriMockEventBus: { listenerCount: (e: string) => number };
        }
      ).__tauriMockEventBus.listenerCount(e),
    event,
  );
}

test.describe('notification-actions smoke (7.1)', () => {
  test('[Continue recording] event starts a fresh recording when idle', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');

    await page.waitForFunction(
      () =>
        (window as unknown as { __tauriCoreMockActive?: boolean })
          .__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // The Continue listener is wired through useRecordingStart, which only mounts once
    // the app has bypassed onboarding and rendered the home page. Poll for subscription
    // rather than emitting blind — an emit before subscribe is silently dropped.
    await expect
      .poll(() => listenerCount(page, 'recording-continue-requested'), {
        timeout: 15_000,
      })
      .toBeGreaterThanOrEqual(1);

    // Idle pre-condition: no start has fired yet.
    expect(await callLogIncludes(page, 'start_recording_with_devices_and_meeting')).toBe(false);

    // Simulate the Rust deep-link handler receiving meetily://recording/continue and
    // re-emitting into the webview.
    await page.evaluate(
      (title) =>
        (
          window as unknown as {
            __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
          }
        ).__tauriMockEventBus.emit('recording-continue-requested', { title }),
      SMOKE_MEETING_TITLE,
    );

    await expect
      .poll(() => callLogIncludes(page, 'start_recording_with_devices_and_meeting'), {
        timeout: 15_000,
      })
      .toBe(true);
  });

  test('[Continue recording] is a no-op while a recording is already active', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');

    await page.waitForFunction(
      () =>
        (window as unknown as { __tauriCoreMockActive?: boolean })
          .__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Start a recording via the sidebar Mic button (the recording-basic reference path).
    const sidebarMic = page.locator('button.bg-red-500').filter({
      has: page.locator('svg.lucide-mic'),
    });
    await expect(sidebarMic).toBeVisible({ timeout: 15_000 });
    await sidebarMic.click();

    await expect
      .poll(() => callLogIncludes(page, 'start_recording_with_devices_and_meeting'), {
        timeout: 15_000,
      })
      .toBe(true);

    // While recording, the Continue listener is torn down — the frontend guard mirrors
    // the Rust resolve(Continue, recording) → NoOp guard. So an emit must not produce a
    // second start.
    await expect
      .poll(() => listenerCount(page, 'recording-continue-requested'), {
        timeout: 15_000,
      })
      .toBe(0);

    const startsBefore = await page.evaluate(
      () =>
        (
          window as unknown as {
            __tauriMockDispatcher: { callLog: () => string[] };
          }
        ).__tauriMockDispatcher.callLog().filter(
          (c: string) => c === 'start_recording_with_devices_and_meeting',
        ).length,
    );

    await page.evaluate(
      () =>
        (
          window as unknown as {
            __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
          }
        ).__tauriMockEventBus.emit('recording-continue-requested', { title: 'ignored' }),
    );

    // The mock bus delivers emits synchronously (forEach over subscribers) before
    // returning, so if a handler had run it would already be in the log. listenerCount
    // 0 above means there is no subscriber to run — the count cannot change.
    const startsAfter = await page.evaluate(
      () =>
        (
          window as unknown as {
            __tauriMockDispatcher: { callLog: () => string[] };
          }
        ).__tauriMockDispatcher.callLog().filter(
          (c: string) => c === 'start_recording_with_devices_and_meeting',
        ).length,
    );
    expect(startsAfter).toBe(startsBefore);
  });
});

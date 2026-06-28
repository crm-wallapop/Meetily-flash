import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT, SMOKE_MEETING_ID } from './_defaults';

// stop-notification-ux §5.1 — pins the C3 wiring: the stop-completion toast's
// "View Meeting" action is rendered conditionally via viewMeetingAction(meetingId),
// not unconditionally with a dead no-op onClick.
//
// Test 1 (known id): the default mock's stop_recording returns SMOKE_MEETING_ID, so
// the action must render inside [data-sonner-toaster]; clicking it navigates to the
// meeting-details route carrying the id query param.
// Test 2 (null id): start + stop are overridden so meeting_id is null at every source
// (the start invoke returns null → activeMeetingId stays null; stop returns null).
// folder_path/meeting_name stay non-null so handleRecordingStop's early-skip guard
// (useRecordingStop.ts:163) does NOT suppress the toast — the success toast still
// paints, just WITHOUT the dead action button. This is the exact dead-button case C3
// fixes; the pure helper's null/undefined/empty branches are already pinned by the
// Vitest suite (useRecordingStop-fixes.test.ts), so this spec covers only the wiring
// that only the full webview can prove (per memory feedback_smoke_carveout.md).

async function callLogIncludes(page: import('@playwright/test').Page, cmd: string): Promise<boolean> {
  return page.evaluate(
    (c) => (window as unknown as { __tauriMockDispatcher: { callLog: () => string[] } })
      .__tauriMockDispatcher.callLog().includes(c),
    cmd,
  );
}

// Sidebar Mic click → stop button → click stop → wait for the stop_recording invoke.
async function startThenStop(page: import('@playwright/test').Page): Promise<void> {
  const sidebarMic = page.locator('button.bg-red-500').filter({
    has: page.locator('svg.lucide-mic'),
  });
  await expect(sidebarMic).toBeVisible({ timeout: 15_000 });
  await sidebarMic.click();

  const stopButton = page.locator('button:not([disabled])').filter({
    has: page.locator('svg.lucide-square'),
  });
  await expect(stopButton).toBeVisible({ timeout: 15_000 });
  await stopButton.click();

  await expect.poll(() => callLogIncludes(page, 'stop_recording'), {
    timeout: 15_000,
  }).toBe(true);
}

test.describe('stop-notification-ux smoke (5.1)', () => {
  test.beforeEach(async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );
  });

  test('View Meeting action renders and navigates when stop returns a known meeting_id', async ({ page }) => {
    await startThenStop(page);

    const action = page.locator('[data-sonner-toaster]').getByRole('button', { name: 'View Meeting' });
    await expect(action).toBeVisible({ timeout: 10_000 });

    await action.click();
    await expect.poll(() => page.url(), { timeout: 10_000 }).toContain(`meeting-details?id=${SMOKE_MEETING_ID}`);
  });

  test('View Meeting action is omitted when stop returns no meeting_id', async ({ page }) => {
    // Override start + stop so meeting_id is null at every source the stop handler
    // reads: start returns null (activeMeetingId stays null per useRecordingStart:51),
    // stop returns null with folder_path/meeting_name non-null to clear the early-skip
    // guard. The success toast must still paint — just without the dead action.
    await page.evaluate(() => {
      const w = window as unknown as {
        __tauriMockDispatcher: { register: (cmd: string, fn: (args: unknown) => unknown) => void };
        __tauriMockEventBus?: { emit: (event: string, payload: unknown) => void };
        __smokeRecording?: boolean;
        __smokeSavingPhaseMs?: number;
      };
      const d = w.__tauriMockDispatcher;
      const bus = w.__tauriMockEventBus;
      d.register('start_recording_with_devices_and_meeting', () => {
        w.__smokeRecording = true;
        bus?.emit('recording-started', {});
        bus?.emit('recording-state-changed', { phase: 'Recording' });
        return { meeting_id: null };
      });
      d.register('stop_recording', () => {
        w.__smokeRecording = false;
        bus?.emit('recording-state-changed', { phase: 'Saving' });
        const idleDelay = w.__smokeSavingPhaseMs || 0;
        setTimeout(() => {
          bus?.emit('recording-state-changed', { phase: 'Idle' });
          bus?.emit('recording-stopped', { folder_path: '/tmp/smoke' });
        }, idleDelay);
        return { meeting_id: null, folder_path: '/tmp/smoke', meeting_name: 'Smoke Test Meeting' };
      });
    });

    await startThenStop(page);

    // The success toast paints — proves the stop handler ran past the early-skip guard,
    // so the only reason the action is absent is viewMeetingAction(null) → undefined.
    await expect(
      page.locator('[data-sonner-toaster]').getByText('Recording saved successfully!'),
    ).toBeVisible({ timeout: 10_000 });
    // The action button does not render (sonner paints the action in the same commit as
    // the toast text, so once the text is visible the absence is meaningful).
    await expect(
      page.locator('[data-sonner-toaster]').getByRole('button', { name: 'View Meeting' }),
    ).toHaveCount(0);
  });
});

import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT, SMOKE_MEETING_TITLE } from './_defaults';

// Task 4.1 — the reference smoke spec. Drives the recording lifecycle through
// the module-seam mock: sidebar Mic click dispatches start-recording-from-sidebar
// → useRecordingStart.handleDirectStart → invoke('start_recording_with_devices_and_meeting')
// → mock returns { meeting_id } AND emits recording-state-changed(phase: Recording) so
// RecordingStateContext flips isRecording true (it derives from phase, not the invoke
// return). RecordingControls mounts (gated on hasMicrophone || isRecording), the stop
// button appears, stop fires invoke('stop_recording') which emits recording-saved-to-db
// → useRecordingStop listener calls refetchMeetings() → api_get_meetings returns the
// fixture list with the new meeting.
//
// The "meeting appears" assertion is data-layer, not DOM: the expanded sidebar tree
// has a pre-existing crash (Cannot read properties of undefined reading 'find') that is
// orthogonal to the recording lifecycle and tracked separately. Here we prove the full
// save→refresh path: stop_recording pushes the meeting into the fixture list the mock
// serves to api_get_meetings, AND api_get_meetings is re-invoked after stop (proving
// refetchMeetings ran). Every assertion uses auto-waiting — no fixed sleeps (§3 rule).

async function callLogIncludes(page: import('@playwright/test').Page, cmd: string): Promise<boolean> {
  return page.evaluate(
    (c) => (window as unknown as { __tauriMockDispatcher: { callLog: () => string[] } })
      .__tauriMockDispatcher.callLog().includes(c),
    cmd,
  );
}

// True when `after` appears at least once AFTER the last occurrence of `before` in the
// call log. Used to prove refetchMeetings (api_get_meetings) ran in response to stop,
// not just during the initial page load.
async function callLogAfter(
  page: import('@playwright/test').Page,
  before: string,
  after: string,
): Promise<boolean> {
  return page.evaluate(
    ({ before, after }) => {
      const log = (window as unknown as { __tauriMockDispatcher: { callLog: () => string[] } })
        .__tauriMockDispatcher.callLog();
      const lastBefore = log.lastIndexOf(before);
      return lastBefore !== -1 && log.indexOf(after, lastBefore + 1) !== -1;
    },
    { before, after },
  );
}

async function smokeMeetingsHas(page: import('@playwright/test').Page, title: string): Promise<boolean> {
  return page.evaluate(
    (t) => (window as unknown as { __smokeMeetings?: Array<{ title: string }> })
      .__smokeMeetings?.some((m) => m.title === t) ?? false,
    title,
  );
}

test.describe('recording-basic smoke (4.1)', () => {
  test('start → recording state → stop → meeting appears in sidebar', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Sidebar Mic button = start trigger. Visible once the main app renders
    // (after the onboarding-bypass reload settles).
    const sidebarMic = page.locator('button.bg-red-500').filter({
      has: page.locator('svg.lucide-mic'),
    });
    await expect(sidebarMic).toBeVisible({ timeout: 15_000 });

    // Start recording.
    await sidebarMic.click();

    // The mock dispatcher received the Rust start command.
    await expect.poll(() => callLogIncludes(page, 'start_recording_with_devices_and_meeting'), {
      timeout: 15_000,
    }).toBe(true);

    // isRecording flipped true → RecordingControls mounted → stop button visible.
    // Scope to an enabled button: the sidebar's own Square is disabled while recording.
    const stopButton = page.locator('button:not([disabled])').filter({
      has: page.locator('svg.lucide-square'),
    });
    await expect(stopButton).toBeVisible({ timeout: 15_000 });

    // Stop recording.
    await stopButton.click();

    await expect.poll(() => callLogIncludes(page, 'stop_recording'), {
      timeout: 15_000,
    }).toBe(true);

    // stop_recording pushed the meeting into the fixture list the mock serves to
    // api_get_meetings, and the recording-saved-to-db emit made useRecordingStop call
    // refetchMeetings(). Prove both: the meeting is in __smokeMeetings, AND
    // api_get_meetings was re-invoked AFTER stop_recording (not just during page load).
    await expect.poll(() => smokeMeetingsHas(page, SMOKE_MEETING_TITLE), {
      timeout: 15_000,
    }).toBe(true);
    await expect.poll(() => callLogAfter(page, 'stop_recording', 'api_get_meetings'), {
      timeout: 15_000,
    }).toBe(true);
  });
});

import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';

// detector-turn-latch-deadlock + meeting-udp-media-signal — the UI half of the
// detection contract.
//
// What this spec DOES assert:
//   useAutoDetect subscribes to `meeting-detected` and `meeting-ended` Tauri
//   events (useAutoDetect.ts:236-241). On `meeting-detected` (when idle +
//   autoDetectMeetings on, which defaults true), it auto-starts a recording and
//   shows the detect-prompt banner. On `meeting-ended` (while a detector-started
//   recording is active), it shows the stop-prompt banner. This spec emits those
//   events via the mock bus and asserts the banners render + the auto-start
//   command fires — proving the event-name + payload-field contract that makes
//   detection results user-visible.
//
// What this spec CANNOT assert (covered elsewhere):
//   - The Rust latch / UDP-debounce / stable_capture LOGIC that decides WHEN to
//     emit meeting-detected/meeting-ended — cargo adversarial tests in
//     detection::windows (18/18) and meeting_detection (the step_detector
//     invariant matrix). The latch deadlock fix, the stable_capture latch, and
//     the adaptive debounce are all pure-Rust state-machine properties.
//   - A live Meet call triggering the real WindowsMeetingDetector — the
//     `#[ignore]` detector_smoke.rs live trace.

const DETECTED_TITLE = 'Sprint Planning';

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

async function callLogCount(
  page: import('@playwright/test').Page,
  cmd: string,
): Promise<number> {
  return page.evaluate(
    (c) =>
      (
        window as unknown as {
          __tauriMockDispatcher: { callLog: () => string[] };
        }
      ).__tauriMockDispatcher.callLog().filter((x) => x === c).length,
    cmd,
  );
}

test.describe('meeting-auto-detect smoke (detection-result wiring)', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('meeting-detected shows the detect-prompt banner and auto-starts a recording', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // useAutoDetect wires its listeners once the home page mounts. Poll for
    // subscription before emitting (emit-before-subscribe drops silently).
    await expect
      .poll(() => listenerCount(page, 'meeting-detected'), { timeout: 15_000 })
      .toBeGreaterThanOrEqual(1);

    expect(await callLogIncludes(page, 'start_recording_with_devices_and_meeting')).toBe(false);

    // Simulate the Rust detector emitting a detection result.
    await page.evaluate(
      (title) =>
        (
          window as unknown as {
            __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
          }
        ).__tauriMockEventBus.emit('meeting-detected', {
          default_title: title,
          candidate_titles: [title, 'Daily Standup'],
        }),
      DETECTED_TITLE,
    );

    // The detect-prompt banner renders with the title in its editable input.
    await expect(page.getByText('Google Meet detected — start recording?')).toBeVisible({ timeout: 10_000 });
    // The default_title from the payload populates the banner's editable input.
    const titleInput = page.getByPlaceholder('Meeting title');
    await expect(titleInput).toBeVisible({ timeout: 5_000 });
    await expect(titleInput).toHaveValue(DETECTED_TITLE);

    // shouldStartOnDetected auto-starts the recording via the normal start path.
    // Exactly one invoke per emit — guards the useAutoDetect async-listener
    // cleanup (StrictMode mount→unmount→remount must not leave an orphan that
    // fans one emit out to N starts).
    await expect
      .poll(() => callLogCount(page, 'start_recording_with_devices_and_meeting'), {
        timeout: 10_000,
      })
      .toBe(1);
  });

  test('meeting-ended shows the stop-prompt banner for a detector-started recording', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    await expect
      .poll(() => listenerCount(page, 'meeting-detected'), { timeout: 15_000 })
      .toBeGreaterThanOrEqual(1);

    // Trigger a detector-started recording first (isDetectorStartedRef must be
    // true for shouldShowStopPrompt to fire).
    await page.evaluate(() => {
      (
        window as unknown as {
          __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
        }
      ).__tauriMockEventBus.emit('meeting-detected', {
        default_title: 'Call',
        candidate_titles: ['Call'],
      });
    });
    await expect
      .poll(() => callLogIncludes(page, 'start_recording_with_devices_and_meeting'), {
        timeout: 10_000,
      })
      .toBe(true);

    // Dismiss the detect-prompt so only the stop-prompt is visible post-emit.
    // Clicking Start Recording runs handleBannerConfirm which clears the banner
    // but leaves isDetectorStartedRef true and the recording active.
    await page.getByRole('button', { name: 'Start Recording' }).click();
    await expect(page.getByText('Google Meet detected — start recording?')).toBeHidden({ timeout: 5_000 });

    await expect
      .poll(() => listenerCount(page, 'meeting-ended'), { timeout: 15_000 })
      .toBeGreaterThanOrEqual(1);

    await page.evaluate(() => {
      (
        window as unknown as {
          __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
        }
      ).__tauriMockEventBus.emit('meeting-ended', undefined);
    });

    await expect(page.getByText('Google Meet call ended — stop recording?')).toBeVisible({ timeout: 10_000 });
  });

  // fix-stop-responsiveness §9.5 — the consolidated stop call site. Both the
  // manual Stop button and the stop-prompt banner's confirm button must route
  // through handleRecordingStop → recordingService.stopRecording() →
  // invoke('stop_recording'). The first two tests prove the banners render; this
  // one proves the stop-prompt confirm actually reaches the Rust command and
  // drives the Saving phase (the regression from §9: the banner-confirm path
  // used to call handleRecordingStop without ever invoking stop_recording).
  test('stop-prompt confirm drives stop_recording and the Saving phase', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    await expect
      .poll(() => listenerCount(page, 'meeting-detected'), { timeout: 15_000 })
      .toBeGreaterThanOrEqual(1);

    // Hold Saving so the branch paints (same reason as recording-basic).
    await page.evaluate(() => {
      (window as unknown as { __smokeSavingPhaseMs?: number }).__smokeSavingPhaseMs = 1500;
    });

    // Detector-started recording (sets isDetectorStartedRef for the stop-prompt).
    await page.evaluate(() => {
      (
        window as unknown as {
          __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
        }
      ).__tauriMockEventBus.emit('meeting-detected', {
        default_title: 'Call',
        candidate_titles: ['Call'],
      });
    });
    await expect
      .poll(() => callLogIncludes(page, 'start_recording_with_devices_and_meeting'), {
        timeout: 10_000,
      })
      .toBe(true);
    await page.getByRole('button', { name: 'Start Recording' }).click();
    await expect(page.getByText('Google Meet detected — start recording?')).toBeHidden({ timeout: 5_000 });

    await expect
      .poll(() => listenerCount(page, 'meeting-ended'), { timeout: 15_000 })
      .toBeGreaterThanOrEqual(1);
    await page.evaluate(() => {
      (
        window as unknown as {
          __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
        }
      ).__tauriMockEventBus.emit('meeting-ended', undefined);
    });
    await expect(page.getByText('Google Meet call ended — stop recording?')).toBeVisible({ timeout: 10_000 });

    expect(await callLogIncludes(page, 'stop_recording')).toBe(false);

    // The stop-prompt confirm button routes through the consolidated stop path.
    await page.getByRole('button', { name: 'Stop Recording' }).click();

    await expect
      .poll(() => callLogIncludes(page, 'stop_recording'), { timeout: 10_000 })
      .toBe(true);

    // The shared stop_recording handler emits Saving — the banner-confirm path
    // reached the same phase transition as the manual Stop button.
    await expect(page.getByTestId('saving-status-bar')).toBeVisible({ timeout: 5_000 });
    await expect(page.getByTestId('saving-label')).toHaveText('Saving…');
  });
});

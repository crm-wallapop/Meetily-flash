import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import { SMOKE_MEETING_DETAILS_INIT_SCRIPT } from './_meeting-details';

// Task 5.4 (automated) — UI wiring smoke for the per-meeting max_speakers override.
// The adapter unit tests (4.x) prove the invoke call shapes; this spec proves the live
// DOM wiring end-to-end through the module-seam mock: the control renders reflecting
// the persisted cap state, entering an override dispatches
// set_meeting_max_speakers {meetingId, cap:number}, and the Auto toggle dispatches
// {meetingId, cap:null} to clear it. Actual speaker-count reduction is covered by the
// #[ignore] cargo integration test (the real Rust diarization pipeline) — a browser
// smoke cannot exercise the audio path.

const MEETING_URL = '/meeting-details?id=meet-summary-001';

interface CapCall {
  meetingId: string;
  cap: number | null;
}
interface CapState {
  override: number | null;
  global_default: number;
}

async function setCapState(
  page: import('@playwright/test').Page,
  state: CapState,
): Promise<void> {
  await page.addInitScript((s) => {
    (window as unknown as { __smokeMeetingCap?: CapState }).__smokeMeetingCap = s;
  }, state);
}

async function lastCapCall(page: import('@playwright/test').Page): Promise<CapCall | null> {
  return page.evaluate(() => {
    const calls =
      (window as unknown as { __smokeMeetingCapCalls?: CapCall[] }).__smokeMeetingCapCalls ?? [];
    return calls.length ? calls[calls.length - 1]! : null;
  });
}

async function bootstrap(
  page: import('@playwright/test').Page,
  state: CapState,
): Promise<void> {
  page.on('dialog', (d) => d.dismiss());
  await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
  await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
  await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);
  // Runs after the meeting-details init, so it overrides the default cap state.
  await setCapState(page, state);
  await page.goto(MEETING_URL);
  await page.waitForFunction(
    () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
    { timeout: 15_000 },
  );
}

test.describe('per-meeting-max-speakers-override smoke (5.4)', () => {
  test.beforeEach(() => {
    // Cold first-compile of /meeting-details takes ~37s on a warm dev server (see
    // summary-render smoke); the default 30s budget aborts page.goto mid-compile.
    test.setTimeout(90_000);
  });

  test('renders in Auto mode reflecting the global default', async ({ page }) => {
    await bootstrap(page, { override: null, global_default: 10 });

    const control = page.getByTestId('meeting-max-speakers-control');
    await expect(control).toBeVisible({ timeout: 20_000 });

    const toggle = control.getByRole('button');
    const input = control.getByRole('spinbutton');
    await expect(toggle).toHaveText('Auto (10)');
    await expect(input).toBeDisabled();
    await expect(input).toHaveValue('10');
  });

  test('entering an override persists via set_meeting_max_speakers {cap:number}', async ({ page }) => {
    await bootstrap(page, { override: null, global_default: 10 });

    const control = page.getByTestId('meeting-max-speakers-control');
    await expect(control).toBeVisible({ timeout: 20_000 });
    const toggle = control.getByRole('button');
    const input = control.getByRole('spinbutton');

    // Auto → Override enables the input (no commit on this click).
    await toggle.click();
    await expect(input).toBeEnabled();

    await input.fill('3');
    await input.blur(); // onBlur → commit(v)

    await expect.poll(() => lastCapCall(page), { timeout: 10_000 }).toEqual({
      meetingId: 'meet-summary-001',
      cap: 3,
    });
    // Commit refreshes from the mock (override now 3) → toggle flips to Override.
    await expect(toggle).toHaveText('Override');
  });

  test('Auto toggle clears the override via set_meeting_max_speakers {cap:null}', async ({ page }) => {
    await bootstrap(page, { override: 3, global_default: 10 });

    const control = page.getByTestId('meeting-max-speakers-control');
    await expect(control).toBeVisible({ timeout: 20_000 });
    const toggle = control.getByRole('button');
    await expect(toggle).toHaveText('Override'); // loads in override mode

    await toggle.click(); // override → auto, commits null

    await expect.poll(() => lastCapCall(page), { timeout: 10_000 }).toEqual({
      meetingId: 'meet-summary-001',
      cap: null,
    });
    await expect(toggle).toHaveText('Auto (10)');
  });
});

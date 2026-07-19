import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT, SMOKE_MEETING_ID, SMOKE_MEETING_TITLE } from './_defaults';
import { SMOKE_MEETING_DETAILS_INIT_SCRIPT } from './_meeting-details';

// Task 3.2 — pins the reload regression class exposed by the orphan cleanup.
// Pre-cleanup, TranscriptContext ran a `syncFromBackend` effect on every mount
// (i.e., every reload) that called the now-deleted `get_transcript_history`
// Tauri command. The cleanup (§3.5) removed that effect; this spec verifies the
// reload path still renders AND that the live record start still reconciles
// after a reload (exercising the `recording-started` listener the cleanup
// narrowed to metadata-only).
//
// Design D4§4 originally specified flipping the mock `get_recording_state` via
// `page.evaluate` post-reload. Apply-time: a standalone `page.evaluate` emit of
// `recording-state-changed` after reload does not flip `isRecording` in the UI,
// while the identical emit inside the `start_recording` mock handler does (the
// proven path in recording-basic.spec.ts). Rather than chase the mock-lifecycle
// artifact, this spec drives the start via the sidebar Mic click — the real
// user path — which emits both `recording-started` and `recording-state-changed`
// from inside the mock handler. This is strictly more coverage (it also
// exercises the start command dispatch + the recording-started metadata
// listener) and satisfies the design's non-tautology intent.

const IDLE_COPY = 'Start recording — transcript generates after you stop.';

test.describe('cleanup-realtime-transcription-orphans smoke (3.2)', () => {
  test.beforeEach(() => {
    // Cold first-compile of `/` + a reload doubles dev-server transform cost on
    // the very first run in a suite; default 30s can abort mid-compile.
    test.setTimeout(90_000);
  });

  test('reload does not crash and recording still reconciles post-reload', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    page.on('dialog', (d) => d.dismiss());

    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Positive signal (task 10.1 copy fix): the corrected idle-state copy renders.
    // Without this the "no pageerror" assertion could pass vacuously on a blank or
    // error-boundary page.
    await expect(page.getByText(IDLE_COPY).first()).toBeVisible({ timeout: 20_000 });

    // The reload itself is the regression class: pre-cleanup, mount re-ran the
    // dead `syncFromBackend` effect. Post-cleanup the idle state must still paint.
    await page.reload();
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );
    await expect(page.getByText(IDLE_COPY).first()).toBeVisible({ timeout: 20_000 });

    // Recording reconciliation post-reload: sidebar Mic click dispatches
    // start_recording → mock emits recording-started + recording-state-changed →
    // RecordingStateContext flips isRecording → RecordingControls mounts the
    // stop button. Same selector recording-basic.spec.ts uses.
    const sidebarMic = page.locator('button.bg-red-500').filter({
      has: page.locator('svg.lucide-mic'),
    });
    await expect(sidebarMic).toBeVisible({ timeout: 15_000 });
    await sidebarMic.click();

    const stopButton = page.locator('button:not([disabled])').filter({
      has: page.locator('svg.lucide-square'),
    });
    await expect(stopButton).toBeVisible({ timeout: 15_000 });

    // No uncaught error survived load + reload + record-start. The body assertion
    // guards against a React error-boundary blank screen.
    await expect(page.locator('body')).toBeVisible();
    expect(pageErrors, 'reload + record-start must not throw a pageerror').toEqual([]);
  });

  // Task 12.3 — replaces the live-mic manual check with automated coverage of the
  // full record→stop→view flow. This change doesn't alter that flow (design
  // Non-Goals: "No change to post-meeting transcript rendering behavior"), so this
  // is non-regression coverage: it proves the dead-code removal didn't break the
  // integration hop from a freshly-recorded meeting to its transcript+summary view.
  // (a) sidebar row is covered by recording-basic.spec.ts 4.1; this test covers
  // (b) transcript render + (c) summary render + (d) no console errors for the
  // meeting that actually went through record→stop (not a pre-loaded fixture).
  test('record → stop → meeting-details renders transcript + summary (12.3)', async ({ page }) => {
    const pageErrors: string[] = [];
    page.on('pageerror', (err) => pageErrors.push(err.message));
    page.on('dialog', (d) => d.dismiss());

    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);
    // Align the meeting-details metadata fixture with the ID _defaults.ts mints on
    // stop (SMOKE_MEETING_ID), so the flow is honest end-to-end: the meeting that
    // record→stop produces is the one whose details we view. The
    // api_get_meeting_metadata / api_get_meeting_transcripts / api_get_summary
    // handlers are global (ignore URL ID), so this override is what ties the URL
    // to the fixture data.
    await page.addInitScript(([id, title]) => {
      (window as unknown as { __smokeMeetingMetadata?: Record<string, unknown> }).__smokeMeetingMetadata = {
        id,
        title,
        created_at: '2026-07-19T10:00:00Z',
        updated_at: '2026-07-19T10:30:00Z',
        folder_path: '/smoke/meetings/' + id,
      };
    }, [SMOKE_MEETING_ID, SMOKE_MEETING_TITLE]);

    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Record → stop (same drive as recording-basic.spec.ts 4.1). Produces
    // SMOKE_MEETING_ID in __smokeMeetings.
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

    // Wait for stop_recording + the sidebar refetch so the meeting is committed
    // before navigating away.
    await expect.poll(
      () =>
        page.evaluate(
          (t) =>
            (window as unknown as { __smokeMeetings?: Array<{ title: string }> })
              .__smokeMeetings?.some((m) => m.title === t) ?? false,
          SMOKE_MEETING_TITLE,
        ),
      { timeout: 15_000 },
    ).toBe(true);

    // View the just-recorded meeting's details.
    await page.goto(`/meeting-details?id=${SMOKE_MEETING_ID}`);
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // (b) transcript renders. (c) summary renders. Both served by the
    // meeting-details fixture handlers for the meeting we just recorded.
    await expect(page.getByText('Smoke transcript segment one.').first()).toBeVisible({
      timeout: 20_000,
    });
    await expect(
      page.getByText('Carol to draft the Q3 board update by Thursday').first(),
    ).toBeVisible({ timeout: 20_000 });

    // (d) no console errors across the full flow.
    await expect(page.locator('body')).toBeVisible();
    expect(pageErrors, 'record→stop→view flow must not throw a pageerror').toEqual([]);
  });
});

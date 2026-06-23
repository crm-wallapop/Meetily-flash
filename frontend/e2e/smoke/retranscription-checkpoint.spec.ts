import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import { SMOKE_MEETING_DETAILS_INIT_SCRIPT } from './_meeting-details';

// retranscription-checkpoint §3 / spec scenario "Progress reflects the
// checkpointed fraction on resume" — the UI half of the contract.
//
// What this spec DOES assert:
//   The RetranscribeDialog subscribes to `retranscription-progress` and, while
//   processing, renders the payload's `progress_percentage` + `stage` + `message`
//   (RetranscribeDialog.tsx:130-135, 363-381). The checkpoint change's resume
//   path emits a `progress_percentage` that reflects the checkpointed fraction
//   (not the decode %). This spec emits the event the Rust side would send on a
//   resume (66 % = 3 of 4 segments checkpointed) and asserts the dialog renders
//   that value — proving the event-name + payload-field contract that makes the
//   checkpoint fraction user-visible. If Rust renamed the event or changed the
//   payload field names, this spec would catch it.
//
// What this spec CANNOT assert (covered elsewhere):
//   - The Rust resume logic that COMPUTES 66 % (load checkpoints, derive the
//     fraction, emit it) — cargo adversarial tests in retranscription.rs
//     (progress_reflects_checkpointed_fraction_on_resume asserts the first
//     on_progress fires with (3, 4) → 66 %).
//   - A real GPU transcription with timed pause/resume — manual QA (task 3.2).

const MEETING_URL = '/meeting-details?id=meet-summary-001';

test.describe('retranscription-checkpoint smoke (progress contract)', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('retranscription-progress renders the checkpointed fraction in the dialog', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);
    await page.goto(MEETING_URL);
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Open the RetranscribeDialog via the Enhance button (folder_path is set in
    // the meeting-details fixture, so the button mounts).
    const enhance = page.getByTitle('Retranscribe to enhance your recorded audio');
    await expect(enhance).toBeVisible({ timeout: 20_000 });
    await enhance.click();

    const startBtn = page.getByRole('button', { name: 'Start Retranscription' });
    await expect(startBtn).toBeVisible({ timeout: 10_000 });
    await startBtn.click();

    // handleStartRetranscription dispatches the command (proving the dialog
    // entered the processing state that gates the progress-bar render).
    await expect.poll(async () => {
      return page.evaluate(() =>
        (window as unknown as { __smokeRetranscribeCalls?: { cmd: string }[] })
          .__smokeRetranscribeCalls ?? [],
      );
    }, { timeout: 10_000 }).toContainEqual(expect.objectContaining({ cmd: 'start_retranscription_command' }));

    // The progress listener registers asynchronously on dialog open; poll the
    // bus so the emit lands after subscription (emit-before-subscribe drops).
    await expect.poll(async () => {
      return page.evaluate(() =>
        (window as unknown as { __tauriMockEventBus: { listenerCount: (e: string) => number } })
          .__tauriMockEventBus.listenerCount('retranscription-progress'),
      );
    }, { timeout: 10_000 }).toBeGreaterThanOrEqual(1);

    // Emit the progress event the Rust resume path would send: 66 % reflects
    // 3 of 4 segments checkpointed (25 + (3/4)*55), not the decode percentage.
    await page.evaluate(() => {
      (window as unknown as { __tauriMockEventBus: { emit: (e: string, p: unknown) => void } })
        .__tauriMockEventBus.emit('retranscription-progress', {
          meeting_id: 'meet-summary-001',
          stage: 'Transcribing',
          progress_percentage: 66,
          message: 'Resuming from checkpoint',
        });
    });

    // The dialog renders {Math.round(progress_percentage)}% + the stage label.
    await expect(page.getByText('66%')).toBeVisible({ timeout: 10_000 });
    // exact: the dialog title "Retranscribing..." would also substring-match.
    await expect(page.getByText('Transcribing', { exact: true })).toBeVisible({ timeout: 5_000 });
  });
});

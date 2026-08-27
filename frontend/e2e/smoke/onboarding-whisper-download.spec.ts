// Onboarding transcription-model download must run on the Whisper engine.
//
// Pins the post-Parakeet wiring of DownloadProgressStep + OnboardingContext
// (2026-08-27 sweep):
//   - step 3 issues `whisper_download_model` (NOT any engine-specific legacy
//     command) for DEFAULT_WHISPER_MODEL ('large-v3-turbo', see
//     src/constants/modelDefaults.ts — keep in sync);
//   - progress/completion state advances on the generic `model-download-*`
//     events emitted by the whisper engine;
//   - the Continue button stays gated on the transcription model finishing.
// A regression here bricks fresh-install onboarding (no transcribable engine).

import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';

const WHISPER_MODEL = 'large-v3-turbo'; // mirrors DEFAULT_WHISPER_MODEL

const ONBOARDING_FIRST_LAUNCH_INIT_SCRIPT = `
(function () {
  'use strict';
  var d = window.__tauriMockDispatcher;
  if (!d) return;
  d.register('get_onboarding_status', function () { return null; });
  d.register('whisper_get_available_models', function () { return []; });
})();
`;

test.describe('onboarding whisper download smoke', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('step 3 downloads DEFAULT_WHISPER_MODEL via model-download events and gates Continue', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(ONBOARDING_FIRST_LAUNCH_INIT_SCRIPT);
    await page.goto('/');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    // Walk to step 3: Welcome -> Setup Overview -> Download Progress.
    await expect(page.getByText('Welcome to Meetily')).toBeVisible({ timeout: 10_000 });
    await page.getByRole('button', { name: 'Get Started' }).click();
    const letsGo = page.getByRole('button', { name: "Let's Go" });
    await expect(letsGo).toBeVisible({ timeout: 10_000 });
    await letsGo.click();

    await expect(page.getByText('Getting things ready')).toBeVisible({ timeout: 10_000 });

    // The download command must be the whisper one. On pre-sweep code this
    // never appears (the old context invoked a different command), so this
    // expectation is the RED discriminator for the rewiring.
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __tauriMockDispatcher: { callLog: () => string[] };
                }
              ).__tauriMockDispatcher.callLog().includes('whisper_download_model'),
          ),
        { timeout: 15_000 },
      )
      .toBe(true);

    // Wait for the step's listener subscription before emitting, or the
    // event is silently dropped (same race as the retranscription dialog).
    await expect
      .poll(
        () =>
          page.evaluate(
            () =>
              (
                window as unknown as {
                  __tauriMockEventBus: { listenerCount: (e: string) => number };
                }
              ).__tauriMockEventBus.listenerCount('model-download-progress'),
          ),
        { timeout: 15_000 },
      )
      .toBeGreaterThan(0);

    const transcriptionCard = page
      .locator('div.bg-white')
      .filter({ has: page.locator('h3:text("Transcription Engine")') })
      .first();
    await expect(transcriptionCard).toBeVisible();

    await page.evaluate(
      (m) =>
        (
          window as unknown as {
            __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
          }
        ).__tauriMockEventBus.emit('model-download-progress', { modelName: m, progress: 40 }),
      WHISPER_MODEL,
    );
    await expect(transcriptionCard.getByText('40%', { exact: true })).toBeVisible({ timeout: 10_000 });

    await page.evaluate(
      (m) =>
        (
          window as unknown as {
            __tauriMockEventBus: { emit: (e: string, p: unknown) => void };
          }
        ).__tauriMockEventBus.emit('model-download-complete', { modelName: m }),
      WHISPER_MODEL,
    );
    await expect(transcriptionCard.getByText('100%', { exact: true })).toBeVisible({ timeout: 10_000 });

    // Continue unlocks once the transcription model reports downloaded.
    await expect(page.getByRole('button', { name: 'Continue' })).toBeEnabled({ timeout: 10_000 });

    // No legacy engine commands may fire at any point in the flow.
    expect(
      await page.evaluate(() =>
        (
          window as unknown as { __tauriMockDispatcher: { callLog: () => string[] } }
        ).__tauriMockDispatcher
          .callLog()
          .some((c) => c.toLowerCase().includes('parakeet')),
      ),
    ).toBe(false);
  });
});

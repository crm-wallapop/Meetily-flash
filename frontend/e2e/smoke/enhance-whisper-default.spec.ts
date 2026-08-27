// Enhance dialog must default to a Whisper model even when the live
// transcription config is Parakeet and a Parakeet model is available.
//
// WHY: Enhance is the diarization boundary source. Parakeet rows carry no
// token timestamps, so alignment falls back to proportional word-count
// slicing and cuts sentences mid-word at speaker changes (cde5c264 banter,
// 2026-08-27). Whisper rows carry token timestamps and split word-exactly
// via align_with_tokens. User decision 2026-08-27: Enhance uses only
// Whisper; Parakeet stays listed only when no Whisper model exists locally.

import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import { SMOKE_MEETING_DETAILS_INIT_SCRIPT } from './_meeting-details';

const MEETING_URL = 'http://localhost:3118/meeting-details?id=meet-summary-001';

test.describe('enhance model default (whisper-only)', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('defaults to Whisper even when configured provider is Parakeet', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_MEETING_DETAILS_INIT_SCRIPT);

    // Override AFTER the defaults fixture: the live machine's shape —
    // configured provider is Parakeet AND a Parakeet model is downloaded.
    await page.addInitScript(`
      (function () {
        var d = window.__tauriMockDispatcher;
        if (!d) return;
        d.register('api_get_transcript_config', function () {
          return { provider: 'parakeet', model: 'parakeet-tdt-0.6b-v3-int8' };
        });
        d.register('whisper_get_available_models', function () {
          return [
            { name: 'small', size_mb: 466, status: 'Available' },
            { name: 'large-v3', size_mb: 2900, status: 'Available' },
          ];
        });
        d.register('parakeet_get_available_models', function () {
          return [{ name: 'parakeet-tdt-0.6b-v3-int8', size_mb: 670, status: 'Available' }];
        });
      })();
    `);

    await page.goto(MEETING_URL);
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    const enhance = page.getByTitle('Retranscribe to enhance your recorded audio');
    await expect(enhance).toBeVisible({ timeout: 20_000 });
    await enhance.click();

    // The model combobox (scoped: the dialog also renders a language one).
    const combo = page.getByRole('combobox').filter({ hasText: /Whisper:|Parakeet:/ });
    await expect(combo).toBeVisible({ timeout: 10_000 });
    await expect(combo).toContainText(/Whisper:/, { timeout: 10_000 });
    await expect(combo).not.toContainText(/Parakeet:/);
  });
});

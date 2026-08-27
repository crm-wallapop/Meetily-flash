// Transcript settings provider list is Whisper-only.
//
// Pins the 2026-08-27 Parakeet removal at its config surface: the provider
// dropdown on /settings → "Transcription" offers exactly one engine
// (Local Whisper). A Parakeet entry reappearing anywhere in this dropdown —
// e.g., a revert of cc0db59/0c486a6 — fails this spec.

import { test, expect } from '@playwright/test';
import { TAURI_MOCK_INIT_SCRIPT } from '../mocks/init-script';
import { SMOKE_DEFAULTS_INIT_SCRIPT } from './_defaults';
import { SMOKE_SETTINGS_INIT_SCRIPT } from './_settings';

test.describe('transcript settings whisper-only smoke', () => {
  test.beforeEach(() => {
    test.setTimeout(120_000);
  });

  test('provider dropdown offers Local Whisper only (no Parakeet)', async ({ page }) => {
    page.on('dialog', (d) => d.dismiss());
    await page.addInitScript(TAURI_MOCK_INIT_SCRIPT);
    await page.addInitScript(SMOKE_DEFAULTS_INIT_SCRIPT);
    await page.addInitScript(SMOKE_SETTINGS_INIT_SCRIPT);
    await page.goto('/settings');
    await page.waitForFunction(
      () => (window as unknown as { __tauriCoreMockActive?: boolean }).__tauriCoreMockActive === true,
      { timeout: 15_000 },
    );

    await page.getByRole('tab', { name: 'Transcription' }).click();

    const panel = page.locator('[role="tabpanel"]').filter({ hasText: 'Transcript Model' });
    await expect(panel).toBeVisible({ timeout: 20_000 });

    // The provider Select is the first combobox inside the tab panel; it may
    // show a placeholder when the fixture's provider value isn't a real item.
    const providerSelect = panel.getByRole('combobox').first();
    await expect(providerSelect).toBeVisible({ timeout: 10_000 });
    await providerSelect.click();

    await expect(page.getByRole('option', { name: /Local Whisper/ })).toHaveCount(1);
    // Exactly one option overall: no second engine may be offered.
    await expect(page.getByRole('option')).toHaveCount(1);
    await expect(page.getByRole('option')).not.toContainText(/Parakeet/i);

    await page.keyboard.press('Escape');
  });
});

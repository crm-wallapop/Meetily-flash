import { defineConfig } from '@playwright/test';

// D3: engine-per-OS. Each platform runs the browser engine its Tauri build
// ships — chromium on Windows (WebView2), webkit on macOS (WKWebView) and
// Linux (WebKitGTK). CI installs the matching browser per runner.
const browserName: 'chromium' | 'webkit' =
  process.platform === 'win32' ? 'chromium' : 'webkit';

export default defineConfig({
  testDir: './e2e',
  testMatch: '**/*.spec.ts',
  workers: 1,
  fullyParallel: false,
  retries: process.env.CI ? 1 : 0,
  forbidOnly: !!process.env.CI,
  reporter: process.env.CI ? [['github'], ['list']] : 'list',
  use: {
    baseURL: 'http://localhost:3118',
    trace: 'on-first-retry',
    actionTimeout: 10_000,
  },
  projects: [
    {
      name: browserName,
      use: { browserName },
    },
  ],
  webServer: {
    // PLAYWRIGHT_E2E gates the webpack alias in next.config.js that swaps
    // @tauri-apps/api/{core,event} for fixture-backed mocks. If a server is
    // already running on :3118 WITHOUT this env, kill it before running specs.
    command: 'pnpm run dev',
    env: { PLAYWRIGHT_E2E: '1' },
    url: 'http://localhost:3118',
    reuseExistingServer: !process.env.CI,
    timeout: 180_000,
    stdout: 'pipe',
    stderr: 'pipe',
  },
});

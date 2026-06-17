const path = require('path');

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: false, // Disabled for BlockNote compatibility
  output: 'export',
  images: {
    unoptimized: true,
  },
  // Add basePath configuration
  basePath: '',
  assetPrefix: '/',

  // Add webpack configuration for Tauri
  webpack: (config, { isServer }) => {
    if (!isServer) {
      config.resolve.fallback = {
        ...config.resolve.fallback,
        fs: false,
        path: false,
        os: false,
      };
    }
    // Playwright smoke-test seam: swap the Tauri API modules for fixture-backed
    // mocks so the Next.js dev server runs in a plain browser without the Tauri
    // runtime. An ES-module export cannot be replaced at runtime by an init
    // script, so the interception happens here at webpack resolve time. The
    // alias is gated so production builds and normal `pnpm dev` are untouched.
    if (process.env.PLAYWRIGHT_E2E === '1') {
      config.resolve.alias = {
        ...config.resolve.alias,
        '@tauri-apps/api/core': path.resolve(__dirname, 'e2e/mocks/tauri-core-mock.ts'),
        '@tauri-apps/api/event': path.resolve(__dirname, 'e2e/mocks/tauri-event-mock.ts'),
        '@tauri-apps/plugin-notification': path.resolve(__dirname, 'e2e/mocks/tauri-notification-mock.ts'),
      };
    }
    return config;
  },
}

module.exports = nextConfig

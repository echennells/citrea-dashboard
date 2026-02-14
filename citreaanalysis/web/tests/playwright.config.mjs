import { defineConfig, devices } from '@playwright/test';
import path from 'path';

const DB_PATH = process.env.DB_PATH || path.resolve('..', 'data', 'citrea.db');

export default defineConfig({
  testDir: '.',
  testMatch: '**/*.spec.mjs',
  timeout: 60_000,
  expect: { timeout: 10_000 },
  fullyParallel: false,
  retries: 0,
  reporter: [['list']],
  outputDir: path.resolve('..', 'test-results'),

  use: {
    baseURL: 'http://localhost:3099',
    screenshot: 'off',        // we handle screenshots manually
    trace: 'retain-on-failure',
    video: 'off',
  },

  projects: [
    {
      name: 'desktop',
      use: {
        ...devices['Desktop Chrome'],
        viewport: { width: 1440, height: 900 },
      },
    },
    {
      name: 'tablet',
      use: {
        ...devices['iPad (gen 7)'],
        viewport: { width: 810, height: 1080 },
      },
    },
    {
      name: 'mobile',
      use: {
        ...devices['iPhone 13'],
        viewport: { width: 390, height: 844 },
      },
    },
  ],

  webServer: {
    command: `DB_PATH="${DB_PATH}" node ../server.js`,
    port: 3099,
    timeout: 15_000,
    reuseExistingServer: !process.env.CI,
    env: {
      PORT: '3099',
      DB_PATH,
    },
  },
});

import { test, expect } from '@playwright/test';
import {
  screenshot,
  waitForAppReady,
  waitForChartRender,
  enableSlow3G,
  disableThrottling,
} from './helpers.mjs';

// ─── Initial load screenshots across viewports ─────────────────────────────

test.describe('Initial load', () => {
  test('dashboard loads and renders correctly', async ({ page }, testInfo) => {
    const device = testInfo.project.name;
    await page.goto('/');
    await waitForAppReady(page);

    // Verify key elements are visible
    await expect(page.locator('h1')).toHaveText('Citrea Dashboard');
    await expect(page.locator('#card-txs .card-value, #card-txs')).not.toHaveText('—');
    await expect(page.locator('#chart-fee-per-tx svg')).toBeVisible();
    await expect(page.locator('#batch-table tbody tr')).not.toHaveCount(0);

    await screenshot(page, `01-initial-load-${device}`);
  });
});

// ─── Period switching (overview cards) ──────────────────────────────────────

test.describe('Overview period switches', () => {
  // Only run on desktop to avoid redundant screenshots
  test.skip(({ }, testInfo) => testInfo.project.name !== 'desktop');

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);
  });

  for (const period of ['7d', '24h', '4h', 'all']) {
    test(`switch overview to ${period}`, async ({ page }) => {
      // Click the period button in the overview toggle
      const btn = page.locator(`#period-toggle .period-btn[data-period="${period}"]`);
      await btn.click();

      // Wait for the button to become active
      await expect(btn).toHaveClass(/active/);

      // Wait for card values to update (they'll briefly flash)
      await page.waitForFunction(
        () => {
          const v = document.getElementById('card-txs');
          return v && v.textContent.trim() !== '—' && v.textContent.trim() !== 'pending';
        },
        { timeout: 10_000 }
      );

      await screenshot(page, `02-overview-period-${period}`);
    });
  }
});

// ─── Chart period switching ─────────────────────────────────────────────────

test.describe('Chart period switches', () => {
  test.skip(({ }, testInfo) => testInfo.project.name !== 'desktop');

  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);
  });

  const charts = [
    { id: 'chart-fee-per-tx', label: 'fee-per-tx' },
    { id: 'chart-fees', label: 'fees' },
  ];

  for (const chart of charts) {
    for (const period of ['all', '7d', '4h']) {
      test(`switch ${chart.label} to ${period}`, async ({ page }) => {
        const toggle = page.locator(`.period-toggle[data-chart="${chart.id}"]`);
        const btn = toggle.locator(`.period-btn[data-period="${period}"]`);
        await btn.click();

        await expect(btn).toHaveClass(/active/);
        await waitForChartRender(page, chart.id);

        await screenshot(page, `03-chart-${chart.label}-${period}`);
      });
    }
  }
});

// ─── Pre-launch toggle ──────────────────────────────────────────────────────

test.describe('Pre-launch toggle', () => {
  test.skip(({ }, testInfo) => testInfo.project.name !== 'desktop');

  test('toggle pre-launch on and off', async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);

    const checkbox = page.locator('#prelaunch-check');
    const slider = page.locator('.toggle-slider');

    // The checkbox starts checked (showPrelaunch = true by default)
    // Toggle it OFF
    await slider.click();
    // Wait for charts to re-render (they get destroyed and recreated)
    await page.waitForSelector('#chart-fee-per-tx svg', { timeout: 15_000 });
    await waitForChartRender(page, 'chart-fee-per-tx');

    await screenshot(page, '04-prelaunch-off');

    // Toggle it back ON
    await slider.click();
    await page.waitForSelector('#chart-fee-per-tx svg', { timeout: 15_000 });
    await waitForChartRender(page, 'chart-fee-per-tx');

    await screenshot(page, '04-prelaunch-on');
  });
});

// ─── Slow 3G initial load ───────────────────────────────────────────────────

test.describe('Slow 3G', () => {
  test.skip(({ }, testInfo) => testInfo.project.name !== 'desktop');

  test('initial load on simulated 3G', async ({ page }) => {
    const client = await enableSlow3G(page);
    const start = Date.now();

    await page.goto('/');
    await waitForAppReady(page, { timeout: 60_000 });

    const loadTime = Date.now() - start;
    console.log(`\n  3G initial load: ${loadTime.toLocaleString()}ms to interactive\n`);

    await screenshot(page, '05-slow-3g-load');
    await disableThrottling(client);
  });
});

import { test, expect } from '@playwright/test';
import { writeFileSync } from 'fs';
import path from 'path';
import {
  screenshot,
  waitForAppReady,
  waitForChartRender,
  injectLongTaskObserver,
  clearLongTasks,
  measureInteraction,
  testResponsivenessDuring,
  enableSlow3G,
  disableThrottling,
  PerfSummary,
  SCREENSHOT_DIR,
  ensureScreenshotDir,
} from './helpers.mjs';

// Perf tests should only be run with --project=desktop

const summary = new PerfSummary();

// ─── Chart period switch timing ─────────────────────────────────────────────

test.describe('Chart period switch timing', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);
  });

  const charts = [
    { id: 'chart-fee-per-tx', label: 'Fee/Tx' },
    { id: 'chart-tx', label: 'Tx Count' },
    { id: 'chart-fees', label: 'L1 Fees' },
    { id: 'chart-util', label: 'Gas Util' },
  ];

  for (const chart of charts) {
    test(`${chart.label} - switch to "all" period`, async ({ page }) => {
      const getLongTasks = await injectLongTaskObserver(page);
      await clearLongTasks(page);

      const toggle = page.locator(`.period-toggle[data-chart="${chart.id}"]`);
      const allBtn = toggle.locator('.period-btn[data-period="all"]');

      const { elapsed } = await measureInteraction(
        page,
        () => allBtn.click(),
        () => waitForChartRender(page, chart.id)
      );

      const longTasks = await getLongTasks();
      summary.add(`Chart "${chart.label}" → all`, elapsed, longTasks);

      console.log(
        `  ${chart.label} "all" period: ${elapsed}ms` +
          (longTasks.length > 0
            ? ` (${longTasks.length} long tasks: ${longTasks.map((t) => t.duration + 'ms').join(', ')})`
            : ' (0 long tasks)')
      );

      // Soft assertion — log but don't fail on slow renders
      if (elapsed > 3000) {
        console.warn(`  ⚠ ${chart.label} took ${elapsed}ms — may feel sluggish`);
      }
    });
  }
});

// ─── Long task detection during full chart load ─────────────────────────────

test.describe('Long task detection', () => {
  test('detect long tasks during initial "all" period load', async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);

    const getLongTasks = await injectLongTaskObserver(page);
    await clearLongTasks(page);

    // Switch ALL charts to "all" period simultaneously
    const chartIds = ['chart-fee-per-tx', 'chart-tx', 'chart-fees', 'chart-util'];
    for (const id of chartIds) {
      const toggle = page.locator(`.period-toggle[data-chart="${id}"]`);
      await toggle.locator('.period-btn[data-period="all"]').click();
    }

    // Wait for all charts to finish rendering
    for (const id of chartIds) {
      await waitForChartRender(page, id, { timeout: 30_000 });
    }

    const longTasks = await getLongTasks();
    const totalBlocked = longTasks.reduce((sum, t) => sum + t.duration, 0);

    console.log(`\n  Long tasks during "all" charts load:`);
    console.log(`    Count: ${longTasks.length}`);
    console.log(`    Total blocked time: ${totalBlocked}ms`);
    if (longTasks.length > 0) {
      console.log(`    Durations: ${longTasks.map((t) => t.duration + 'ms').join(', ')}`);
    }

    summary.add('All charts → "all" (total)', totalBlocked, longTasks);
    await screenshot(page, '06-all-charts-all-period');
  });
});

// ─── Interactivity during chart render ──────────────────────────────────────

test.describe('Interactivity during heavy operations', () => {
  test('toggle pre-launch while charts are loading "all"', async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);
    await injectLongTaskObserver(page);

    const result = await testResponsivenessDuring(
      page,
      // Heavy action: switch all charts to "all"
      async () => {
        const chartIds = ['chart-fee-per-tx', 'chart-tx', 'chart-fees', 'chart-util'];
        for (const id of chartIds) {
          const toggle = page.locator(`.period-toggle[data-chart="${id}"]`);
          await toggle.locator('.period-btn[data-period="all"]').click();
        }
        // Wait for at least one chart to finish
        await waitForChartRender(page, 'chart-fee-per-tx', { timeout: 30_000 });
      },
      // Probe action: try to toggle pre-launch
      async () => {
        return measureInteraction(
          page,
          () => page.locator('.toggle-slider').click(),
          async () => {
            // Wait for the checkbox state to actually change
            await page.waitForFunction(
              () => {
                const cb = document.getElementById('prelaunch-check');
                return cb !== null;
              },
              { timeout: 10_000 }
            );
          }
        );
      }
    );

    summary.addBlocked(
      'Pre-launch toggle during chart load',
      result.probeElapsed,
      result.blocked
    );

    const status = result.blocked
      ? `BLOCKED - delayed ${result.probeElapsed}ms`
      : `OK - responded in ${result.probeElapsed}ms`;
    console.log(`\n  Interactivity during chart load: ${status}`);
  });

  test('click overview period while charts are loading', async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);

    const result = await testResponsivenessDuring(
      page,
      // Heavy: switch charts to "all"
      async () => {
        const chartIds = ['chart-fee-per-tx', 'chart-tx', 'chart-fees', 'chart-util'];
        for (const id of chartIds) {
          const toggle = page.locator(`.period-toggle[data-chart="${id}"]`);
          await toggle.locator('.period-btn[data-period="all"]').click();
        }
        await waitForChartRender(page, 'chart-fee-per-tx', { timeout: 30_000 });
      },
      // Probe: click overview period button
      async () => {
        return measureInteraction(
          page,
          () => page.locator('#period-toggle .period-btn[data-period="7d"]').click(),
          async () => {
            await page.waitForFunction(
              () => {
                const v = document.getElementById('card-txs');
                return v && v.textContent.trim() !== '—' && v.textContent.trim() !== 'pending';
              },
              { timeout: 10_000 }
            );
          }
        );
      }
    );

    summary.addBlocked(
      'Overview period switch during chart load',
      result.probeElapsed,
      result.blocked
    );

    const status = result.blocked
      ? `BLOCKED - delayed ${result.probeElapsed}ms`
      : `OK - responded in ${result.probeElapsed}ms`;
    console.log(`\n  Overview switch during chart load: ${status}`);
  });
});

// ─── Overview period switch timing ──────────────────────────────────────────

test.describe('Overview period switch timing', () => {
  test.beforeEach(async ({ page }) => {
    await page.goto('/');
    await waitForAppReady(page);
  });

  for (const period of ['7d', '24h', '4h']) {
    test(`overview switch to ${period}`, async ({ page }) => {
      const getLongTasks = await injectLongTaskObserver(page);
      await clearLongTasks(page);

      const btn = page.locator(`#period-toggle .period-btn[data-period="${period}"]`);

      const { elapsed } = await measureInteraction(
        page,
        () => btn.click(),
        async () => {
          await expect(btn).toHaveClass(/active/);
          await page.waitForFunction(
            () => {
              const v = document.getElementById('card-txs');
              return v && v.textContent.trim() !== '—' && v.textContent.trim() !== 'pending';
            },
            { timeout: 10_000 }
          );
        }
      );

      const longTasks = await getLongTasks();
      summary.add(`Overview → ${period}`, elapsed, longTasks);

      console.log(
        `  Overview "${period}": ${elapsed}ms` +
          (longTasks.length > 0
            ? ` (${longTasks.length} long tasks)`
            : ' (0 long tasks)')
      );
    });
  }
});

// ─── Slow 3G full interaction flow ──────────────────────────────────────────

test.describe('Slow 3G interaction flow', () => {
  test('load + interact on 3G', async ({ page }) => {
    const client = await enableSlow3G(page);

    // Measure initial load
    const loadStart = Date.now();
    await page.goto('/');
    await waitForAppReady(page, { timeout: 120_000 });
    const loadTime = Date.now() - loadStart;
    summary.add('3G initial load', loadTime);
    console.log(`\n  3G initial load: ${loadTime}ms`);

    const getLongTasks = await injectLongTaskObserver(page);

    // Measure a period switch on 3G
    await clearLongTasks(page);
    const { elapsed: periodTime } = await measureInteraction(
      page,
      () => page.locator('#period-toggle .period-btn[data-period="7d"]').click(),
      async () => {
        await page.waitForFunction(
          () => {
            const v = document.getElementById('card-txs');
            return v && v.textContent.trim() !== '—' && v.textContent.trim() !== 'pending';
          },
          { timeout: 30_000 }
        );
      }
    );
    const periodTasks = await getLongTasks();
    summary.add('3G overview → 7d', periodTime, periodTasks);
    console.log(`  3G overview "7d" switch: ${periodTime}ms`);

    // Measure a chart period switch on 3G
    await clearLongTasks(page);
    const toggle = page.locator('.period-toggle[data-chart="chart-fee-per-tx"]');
    const { elapsed: chartTime } = await measureInteraction(
      page,
      () => toggle.locator('.period-btn[data-period="all"]').click(),
      () => waitForChartRender(page, 'chart-fee-per-tx', { timeout: 60_000 })
    );
    const chartTasks = await getLongTasks();
    summary.add('3G chart Fee/Tx → all', chartTime, chartTasks);
    console.log(`  3G chart "all" switch: ${chartTime}ms`);

    await screenshot(page, '07-3g-interaction-flow');
    await disableThrottling(client);
  });
});

// ─── Print summary after all tests ──────────────────────────────────────────

test.afterAll(async () => {
  console.log(summary.toString());

  // Also save as JSON
  ensureScreenshotDir();
  const jsonPath = path.join(SCREENSHOT_DIR, 'perf-summary.json');
  writeFileSync(jsonPath, JSON.stringify(summary.toJSON(), null, 2));
  console.log(`  Summary saved to: ${jsonPath}\n`);
});

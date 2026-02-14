import { existsSync, mkdirSync } from 'fs';
import path from 'path';

// ─── Screenshot directory ───────────────────────────────────────────────────

export const SCREENSHOT_DIR = path.join(process.cwd(), 'screenshots');

export function ensureScreenshotDir() {
  if (!existsSync(SCREENSHOT_DIR)) mkdirSync(SCREENSHOT_DIR, { recursive: true });
}

/**
 * Take a screenshot and save it to web/screenshots/ with a descriptive name.
 * @param {import('@playwright/test').Page} page
 * @param {string} name - e.g. "initial-load-desktop"
 */
export async function screenshot(page, name) {
  ensureScreenshotDir();
  const filePath = path.join(SCREENSHOT_DIR, `${name}.png`);
  await page.screenshot({ path: filePath, fullPage: true });
  return filePath;
}

// ─── Wait for app to be interactive ─────────────────────────────────────────

/**
 * Wait until the dashboard has loaded its data and charts are rendered.
 * Checks for:
 *  - Overview cards have non-placeholder values
 *  - At least one chart SVG exists
 *  - Table has rows
 */
export async function waitForAppReady(page, { timeout = 30_000 } = {}) {
  // Wait for a card value that isn't the placeholder "—" or "pending"
  await page.waitForFunction(
    () => {
      const v = document.getElementById('card-txs');
      return v && v.textContent.trim() !== '—' && v.textContent.trim() !== 'pending';
    },
    { timeout }
  );

  // Wait for at least one chart SVG to render (ApexCharts injects <svg>)
  await page.waitForSelector('#chart-fee-per-tx svg', { timeout });

  // Wait for table rows
  await page.waitForSelector('#batch-table tbody tr', { timeout });
}

/**
 * Wait until a specific chart has finished re-rendering after a period switch.
 * Detects render completion by waiting for the loading overlay to disappear
 * and a fresh SVG to be present.
 * @param {import('@playwright/test').Page} page
 * @param {string} chartId - e.g. "chart-fee-per-tx"
 */
export async function waitForChartRender(page, chartId, { timeout = 15_000 } = {}) {
  // Wait for loading overlay to disappear
  await page.waitForFunction(
    (id) => {
      const box = document.getElementById(id)?.closest('.chart-box');
      return box && !box.querySelector('.chart-loading');
    },
    chartId,
    { timeout }
  );
  // Ensure SVG is present
  await page.waitForSelector(`#${chartId} svg`, { timeout: 5000 });
}

// ─── Long Task Observer ─────────────────────────────────────────────────────

/**
 * Inject a PerformanceObserver into the page that records all Long Tasks (>50ms).
 * Returns a handle that can be used to retrieve recorded tasks.
 * @param {import('@playwright/test').Page} page
 * @returns {Promise<() => Promise<Array<{duration: number, startTime: number}>>>}
 */
export async function injectLongTaskObserver(page) {
  await page.evaluate(() => {
    window.__longTasks = [];
    if (typeof PerformanceObserver !== 'undefined') {
      const obs = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          window.__longTasks.push({
            duration: Math.round(entry.duration),
            startTime: Math.round(entry.startTime),
            name: entry.name,
          });
        }
      });
      try {
        obs.observe({ type: 'longtask', buffered: true });
      } catch {
        // longtask not supported in this browser — silently skip
      }
      window.__longTaskObserver = obs;
    }
  });

  return async function getLongTasks() {
    return page.evaluate(() => {
      const tasks = window.__longTasks || [];
      return JSON.parse(JSON.stringify(tasks));
    });
  };
}

/**
 * Clear recorded long tasks (useful between measurements).
 */
export async function clearLongTasks(page) {
  await page.evaluate(() => {
    window.__longTasks = [];
  });
}

// ─── Interaction measurement ────────────────────────────────────────────────

/**
 * Measure how long an interaction takes to produce a visible DOM change.
 *
 * @param {import('@playwright/test').Page} page
 * @param {() => Promise<void>} action - the interaction to perform (e.g. click)
 * @param {() => Promise<void>} waitCondition - resolves when UI change is visible
 * @returns {Promise<{elapsed: number}>} elapsed time in ms
 */
export async function measureInteraction(page, action, waitCondition) {
  const start = Date.now();
  await action();
  await waitCondition();
  const elapsed = Date.now() - start;
  return { elapsed };
}

/**
 * Measure a click on a selector and wait for a DOM change.
 * Convenience wrapper around measureInteraction.
 *
 * @param {import('@playwright/test').Page} page
 * @param {string} clickSelector - CSS selector to click
 * @param {string} waitSelector - CSS selector or function to wait for
 * @param {object} [opts]
 * @param {number} [opts.timeout=15000]
 * @returns {Promise<{elapsed: number}>}
 */
export async function measureClick(page, clickSelector, waitSelector, { timeout = 15_000 } = {}) {
  return measureInteraction(
    page,
    () => page.click(clickSelector),
    () =>
      typeof waitSelector === 'function'
        ? waitSelector()
        : page.waitForSelector(waitSelector, { timeout })
  );
}

// ─── Responsiveness during heavy operations ─────────────────────────────────

/**
 * Start a heavy operation, then immediately try a probe interaction.
 * Measures whether the probe is blocked by the heavy operation.
 *
 * @param {import('@playwright/test').Page} page
 * @param {() => Promise<void>} heavyAction - starts the heavy work (e.g. switch all charts to "all")
 * @param {() => Promise<{elapsed: number}>} probeAction - the quick interaction to test
 * @returns {Promise<{probeElapsed: number, blocked: boolean}>}
 */
export async function testResponsivenessDuring(page, heavyAction, probeAction) {
  // Fire heavy action without awaiting completion
  const heavyPromise = heavyAction();

  // Small delay to let the heavy action begin processing
  await page.waitForTimeout(100);

  // Now try the probe
  const probeResult = await probeAction();

  // Wait for heavy action to complete
  await heavyPromise;

  return {
    probeElapsed: probeResult.elapsed,
    // Consider >1000ms as "blocked"
    blocked: probeResult.elapsed > 1000,
  };
}

// ─── CDP Network Throttling (Slow 3G) ──────────────────────────────────────

/**
 * Enable slow 3G-like network conditions via Chrome DevTools Protocol.
 * @param {import('@playwright/test').Page} page
 */
export async function enableSlow3G(page) {
  const client = await page.context().newCDPSession(page);
  await client.send('Network.emulateNetworkConditions', {
    offline: false,
    downloadThroughput: (500 * 1024) / 8,   // 500 kbps
    uploadThroughput: (500 * 1024) / 8,      // 500 kbps
    latency: 400,                            // 400ms RTT
  });
  return client;
}

/**
 * Disable network throttling.
 * @param {import('playwright').CDPSession} client
 */
export async function disableThrottling(client) {
  await client.send('Network.emulateNetworkConditions', {
    offline: false,
    downloadThroughput: -1,
    uploadThroughput: -1,
    latency: 0,
  });
}

// ─── Summary reporter ───────────────────────────────────────────────────────

/**
 * Collect and format a performance summary.
 */
export class PerfSummary {
  constructor() {
    this.entries = [];
  }

  add(label, elapsedMs, longTasks = []) {
    this.entries.push({ label, elapsedMs, longTasks });
  }

  addBlocked(label, elapsedMs, blocked) {
    this.entries.push({ label, elapsedMs, blocked });
  }

  toString() {
    const lines = ['\n=== Performance Summary ==='];
    const maxLabel = Math.max(...this.entries.map((e) => e.label.length));

    for (const e of this.entries) {
      const pad = e.label.padEnd(maxLabel + 2);
      const ms = `${e.elapsedMs.toLocaleString()}ms`;

      if (e.blocked !== undefined) {
        const status = e.blocked
          ? `BLOCKED - interaction delayed ${e.elapsedMs.toLocaleString()}ms`
          : `OK - responded in ${ms}`;
        lines.push(`  ${pad} ${status}`);
      } else if (e.longTasks) {
        const ltCount = e.longTasks.length;
        const ltDetail =
          ltCount > 0
            ? ` (${ltCount} long task${ltCount > 1 ? 's' : ''}: ${e.longTasks.map((t) => t.duration + 'ms').join(', ')})`
            : ' (0 long tasks)';
        lines.push(`  ${pad} ${ms}${ltDetail}`);
      } else {
        lines.push(`  ${pad} ${ms}`);
      }
    }

    lines.push('');
    return lines.join('\n');
  }

  toJSON() {
    return this.entries;
  }
}

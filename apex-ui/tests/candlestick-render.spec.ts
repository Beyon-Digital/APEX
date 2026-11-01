import { expect, test } from '@playwright/test';

// Inspect the actual chart canvas rather than merely checking that a canvas
// element exists: an unpainted chart previously passed that weaker check.
async function candlePixels(page: import('@playwright/test').Page): Promise<number> {
  return page.getByTestId('candle-chart').locator('canvas').evaluateAll((canvases) => {
    let count = 0;
    for (const canvas of canvases) {
      const ctx = (canvas as HTMLCanvasElement).getContext('2d');
      if (!ctx) continue;
      const pixels = ctx.getImageData(0, 0, canvas.width, canvas.height).data;
      for (let i = 0; i < pixels.length; i += 16) {
        const [r, g, b, a] = [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]];
        if (a > 150 && ((r < 45 && g > 100 && b < 140) || (r > 150 && g < 75 && b < 125))) count++;
      }
    }
    return count;
  });
}

test('daily candles are painted after the chart is reopened', async ({ page }) => {
  await page.goto('/');
  await page.getByTestId('tab-market').click();
  await page.getByTestId('tab-chart').click();
  await expect(page.getByTestId('candle-chart')).toContainText(/\d+ bars/);
  await expect.poll(() => candlePixels(page)).toBeGreaterThan(10);

  await page.evaluate(() => window.dispatchEvent(new Event('focus')));
  await expect.poll(() => candlePixels(page)).toBeGreaterThan(10);
});

test('forced repaint preserves the chart’s zoom and scroll range', async ({ page }) => {
  await page.goto('/');
  const ranges = await page.evaluate(async () => {
    const libraryPath = '/node_modules/.vite/deps/lightweight-charts.js';
    const paintPath = '/src/lib/chartPaint.ts';
    const { createChart } = await import(libraryPath);
    const { repaintChart } = await import(paintPath);
    const container = document.createElement('div');
    container.style.width = '600px';
    container.style.height = '240px';
    document.body.appendChild(container);
    const chart = createChart(container, { width: 600, height: 240 });
    const series = chart.addLineSeries();
    series.setData(Array.from({ length: 100 }, (_, index) => ({ time: 1700000000 + index * 86400, value: index })));
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    chart.timeScale().setVisibleLogicalRange({ from: 20, to: 60 });
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const before = chart.timeScale().getVisibleLogicalRange();
    repaintChart(chart, container);
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const after = chart.timeScale().getVisibleLogicalRange();
    chart.remove();
    container.remove();
    return { before, after };
  });
  expect(ranges.before).not.toBeNull();
  expect(ranges.after.from).toBeCloseTo(ranges.before.from, 4);
  expect(ranges.after.to).toBeCloseTo(ranges.before.to, 4);
});

import type { IChartApi } from 'lightweight-charts';

/** Force a WKWebView canvas repaint without leaving the chart at a new size. */
export function repaintChart(chart: IChartApi, container: HTMLDivElement | null, height?: number) {
  if (!container) return;
  const width = container.clientWidth;
  const chartHeight = height ?? container.clientHeight;
  if (width <= 0 || chartHeight <= 0) return;
  const range = chart.timeScale().getVisibleLogicalRange();
  // Lightweight Charts skips a resize when the size is unchanged. Changing it
  // by two pixels ensures a different even pixel size before restoring it.
  chart.resize(width + 2, chartHeight, true);
  chart.resize(width, chartHeight, true);
  if (range) {
    // Restore at the final width; the library schedules a follow-up paint.
    chart.timeScale().setVisibleLogicalRange(range);
  }
}

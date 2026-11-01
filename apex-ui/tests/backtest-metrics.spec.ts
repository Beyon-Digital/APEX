import { test, expect } from '@playwright/test';
import { formatProfitFactor } from '../src/lib/format';

test('profit factor survives JSON infinity for an all-winning backtest', () => {
  expect(formatProfitFactor(null, 1, 0)).toBe('∞');
  expect(formatProfitFactor(1.925, 2, 1)).toBe('1.93');
  expect(formatProfitFactor(0, 0, 0)).toBe('0.00');
  expect(formatProfitFactor(null, 0, 0)).toBe('—');
});

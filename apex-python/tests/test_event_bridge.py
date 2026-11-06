"""Regression checks for executing source rather than inferring an indicator model."""
import math
from pathlib import Path

import pytest

from runtime.event_bridge import Worker


def init_worker(tmp_path: Path, source: str):
    path = tmp_path / "strategy.py"
    path.write_text(source)
    worker = Worker()
    worker.request({"op": "init", "path": str(path)})
    return worker


def bar(worker, price, index=0):
    return worker.request({"op": "bar", "bar": {
        "symbol": "AAPL", "timeframe": "1m", "open": price, "high": price,
        "low": price, "close": price, "volume": 1, "timestamp_ns": index*60_000_000_000}})


def test_authored_logic_and_state(tmp_path):
    worker = init_worker(tmp_path, '''from apex_sdk import Strategy, Signal, Timeframe
class Actual(Strategy):
    def on_init(self, params):
        self.subscribe(["AAPL"], Timeframe.M1)
        self.count = 0
    def on_bar(self, symbol, bar):
        self.count += 1
        if self.count == 2:
            self.emit(Signal(symbol, "long", 1.0))
        if self.count == 3:
            self.emit(Signal(symbol, "flat", 1.0))
''')
    assert bar(worker, 10)["signals"] == []
    assert bar(worker, 11)["signals"][0]["direction"] == "long"
    assert bar(worker, 12)["signals"][0]["direction"] == "flat"
    assert bar(worker, 13)["signals"] == []


def test_indicators_have_no_future_data(tmp_path):
    worker = init_worker(tmp_path, '''from apex_sdk import Strategy
class Actual(Strategy):
    def on_bar(self, symbol, bar): pass
''')
    bar(worker, 10)
    assert math.isnan(worker.strategy.indicator("sma", "AAPL", 2))
    bar(worker, 20)
    assert worker.strategy.indicator("sma", "AAPL", 2) == 15
    bar(worker, 30)
    assert worker.strategy.indicator("sma", "AAPL", 2) == 25
    assert worker.strategy.indicator("rsi", "AAPL", 2) == 100


def test_callback_failure_is_not_success(tmp_path):
    worker = init_worker(tmp_path, '''from apex_sdk import Strategy
class Actual(Strategy):
    def on_bar(self, symbol, bar): raise RuntimeError("broken strategy")
''')
    with pytest.raises(RuntimeError, match="broken strategy"):
        bar(worker, 10)


def test_unsupported_short_fails_explicitly(tmp_path):
    worker = init_worker(tmp_path, '''from apex_sdk import Strategy, Signal
class Actual(Strategy):
    def on_bar(self, symbol, bar): self.emit(Signal(symbol, "short", 1.0))
''')
    with pytest.raises(ValueError, match="long and flat"):
        bar(worker, 10)


def test_prefix_sum_indicators_match_the_direct_formulas(tmp_path):
    """The O(1) prefix-sum SMA/RSI must equal the straightforward slice maths."""
    import random

    worker = init_worker(tmp_path, '''from apex_sdk import Strategy
class Actual(Strategy):
    def on_bar(self, symbol, bar): pass
''')
    random.seed(7)
    price, closes = 100.0, []
    for index in range(130):
        price = max(1.0, price + random.uniform(-2, 2))
        closes.append(price)
        bar(worker, price, index)
        for period in (2, 5, 14, 50):
            got_sma = worker.strategy.indicator("sma", "AAPL", period)
            if len(closes) >= period:
                assert got_sma == pytest.approx(sum(closes[-period:]) / period, rel=1e-9)
            else:
                assert math.isnan(got_sma)
            if len(closes) > period:
                changes = [b - a for a, b in zip(closes[-period - 1:-1], closes[-period:])]
                gain = sum(max(x, 0) for x in changes) / period
                loss = sum(max(-x, 0) for x in changes) / period
                expected = 100 - 100 / (1 + gain / loss) if loss else (100.0 if gain else 50.0)
                assert worker.strategy.indicator("rsi", "AAPL", period) == pytest.approx(expected, rel=1e-9)


def test_many_bars_stay_fast(tmp_path):
    import time

    worker = init_worker(tmp_path, '''from apex_sdk import Strategy
class Actual(Strategy):
    def on_bar(self, symbol, bar): pass
''')
    started = time.perf_counter()
    for index in range(1500):
        bar(worker, 100 + (index % 7), index)
    assert time.perf_counter() - started < 5.0

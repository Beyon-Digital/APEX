"""Line-oriented strategy worker shared by historical replay and paper execution.

stdout is reserved for protocol replies. User prints are captured as log lines.
A failed callback fails the request rather than silently substituting a model.
"""
from __future__ import annotations

import contextlib
import io
import json
import math
import sys

from apex_sdk.types import Bar, Timeframe
from runtime.strategy_runner import _load_strategy_module, _find_strategy_class


class Transport:
    def __init__(self):
        self.signals = []
        self.logs = []
        self.error = None

    def send(self, method, params):
        if method == "emit_signal":
            signal = params["signal"]
            if signal["direction"] not in ("long", "flat"):
                self.error = "Paper/replay worker supports long and flat signals only"
                return
            self.signals.append(signal)
        elif method == "strategy_log":
            self.logs.append(params["message"])


class Worker:
    def __init__(self):
        self.transport = Transport()
        self.strategy = None
        self.closes = {}

    def request(self, request):
        self.transport.signals.clear()
        self.transport.logs.clear()
        self.transport.error = None
        op = request["op"]
        if op == "init":
            cls = _find_strategy_class(_load_strategy_module(request["path"]))
            self.strategy = cls("event-worker", self.transport)
            self.strategy.on_init(request.get("params", {}))
        elif op == "bar":
            raw = request["bar"]
            symbol = raw["symbol"]
            # Prefix sums make every period O(1) per bar (they used to be re-summed,
            # O(n^2) per bar, which made warm-up and backtests slow). Past and
            # current bars only, so there is no look-ahead.
            acc = self.closes.setdefault(symbol, {"c": [0.0], "g": [0.0], "l": [0.0], "last": None})
            close = raw["close"]
            change = 0.0 if acc["last"] is None else close - acc["last"]
            acc["c"].append(acc["c"][-1] + close)
            acc["g"].append(acc["g"][-1] + max(change, 0.0))
            acc["l"].append(acc["l"][-1] + max(-change, 0.0))
            acc["last"] = close
            n = len(acc["c"]) - 1  # number of closes seen
            cache = self.strategy._indicator_cache
            for period in range(2, 401):
                sma = (acc["c"][n] - acc["c"][n - period]) / period if n >= period else math.nan
                cache[("sma", symbol, (period,))] = sma
                if n > period:
                    # The last `period` price changes: indices n-period .. n-1 of the change series.
                    gain = (acc["g"][n] - acc["g"][n - period]) / period
                    loss = (acc["l"][n] - acc["l"][n - period]) / period
                    cache[("rsi", symbol, (period,))] = (
                        100 - 100 / (1 + gain / loss) if loss else (100.0 if gain else 50.0)
                    )
            self.strategy.on_bar(symbol, Bar(
                symbol=symbol, timeframe=Timeframe(raw["timeframe"]),
                open=raw["open"], high=raw["high"], low=raw["low"],
                close=raw["close"], volume=raw["volume"], timestamp_ns=raw["timestamp_ns"],
            ))
        elif op == "stop":
            self.strategy.on_stop()
        else:
            raise ValueError(f"Unknown operation: {op}")
        if self.transport.error:
            raise ValueError(self.transport.error)
        return {"signals": list(self.transport.signals), "logs": list(self.transport.logs),
                "subscriptions": self.strategy._subscriptions}


def main():
    worker = Worker()
    for line in sys.stdin:
        output = io.StringIO()
        try:
            with contextlib.redirect_stdout(output):
                result = worker.request(json.loads(line))
            result["logs"].extend(output.getvalue().splitlines())
        except Exception as exc:
            result = {"error": f"{type(exc).__name__}: {exc}"}
        print(json.dumps(result, allow_nan=False), flush=True)


if __name__ == "__main__":
    main()

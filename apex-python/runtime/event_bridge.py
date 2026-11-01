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
            closes = self.closes.setdefault(symbol, [])
            closes.append(raw["close"])
            # Populate the SDK cache before the callback, using past/current bars only.
            for period in range(2, 401):
                sma = sum(closes[-period:]) / period if len(closes) >= period else math.nan
                self.strategy._indicator_cache[("sma", symbol, (period,))] = sma
                if len(closes) > period:
                    changes = [b-a for a, b in zip(closes[-period-1:-1], closes[-period:])]
                    gain = sum(max(x, 0) for x in changes) / period
                    loss = sum(max(-x, 0) for x in changes) / period
                    rsi = 100 - 100 / (1 + gain/loss) if loss else (100.0 if gain else 50.0)
                    self.strategy._indicator_cache[("rsi", symbol, (period,))] = rsi
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

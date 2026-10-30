# Strategy API Reference — APEX Python SDK

Complete reference for the `apex_sdk` Python package used to build trading
strategies that run inside the APEX terminal.

## Installation

The SDK is bundled with the APEX Python sidecar. For development:

```bash
cd apex-python
python3 -m pip install -e ".[dev]"
```

Recommended runtime: **Python ≥ 3.11** for the full sidecar and desktop app.
The SDK's shared dataclasses now fall back cleanly on older local interpreters
that do not support slotted dataclasses, but the packaged sidecar target
remains Python 3.11+.

## Development modes

- **Browser/Vite mode** (used by Playwright and browser-only UI work) returns deterministic mocked Tauri responses for strategy file listing, run output, and backtest results.
- **Desktop/Tauri mode** invokes the real Tauri commands and the Python sidecar/runtime behind them.
- If you are validating strategy flows end-to-end, prefer the desktop launcher: `bun run dev` from the repo root, or `cd apex-ui && bun run tauri:dev`.

---

## Native paper sessions and authored backtests

The native IDE's **Run Paper** control starts a Python worker and dispatches
quote-sampled bars into the authored `on_bar` callback. It does not dispatch
`on_tick`. Stop controls terminate the backend session and close its paper
position. Active sessions remain visible across workspace tabs and can be
stopped after a webview reload.

- Live sessions require exactly one symbol subscription using `M1`, `M5`, or
  `M15`. Execution is hard-wired to the `paper` adapter.
- `long` enters a fixed-size position; `flat` exits it. Repeated entry signals
  are ignored while held. Short signals fail explicitly. The native run
  command accepts `params_json`; `quantity` defaults to 1.
- Live bars use sampled quotes, with volume unavailable. Duplicate, backward,
  stale, and implausibly future timestamps are rejected. A session stops after
  90 seconds without fresh data, or after 30 minutes in total.
- Backtests execute the actual Python callbacks, rather than inferring a model
  from source text. They support one long/flat signal per bar and use the
  requested fixed trade size. Signals fill at that bar's close with configured
  slippage and commission; liquidation costs are included in final equity.
- The worker populates `sma` and simple rolling-mean `rsi` indicator caches
  before each bar callback. Other indicators can be implemented in strategy
  code. Indicator warmup returns `NaN`.
- Each callback has a 15-second timeout. Callback and protocol errors fail the
  run and are shown in the IDE.

---

## Module: `apex_sdk`

### Exports

```python
from apex_sdk import Strategy, Bar, Tick, Signal, Timeframe
```

---

## Class: `Strategy`

Base class for all user-authored strategies. Subclass it and override the
event hooks to implement your trading logic.

```python
from apex_sdk import Strategy, Bar, Signal, Timeframe


class MyStrategy(Strategy):
    def on_init(self, params: dict) -> None: ...
    def on_bar(self, symbol: str, bar: Bar) -> None: ...
    def on_tick(self, symbol: str, tick: Tick) -> None: ...
    def on_stop(self) -> None: ...
```

### Constructor

The constructor is called by the APEX runtime — **do not override `__init__`**.

| Parameter      | Type        | Description                              |
| -------------- | ----------- | ---------------------------------------- |
| `strategy_id`  | `str`       | Unique ID assigned by the runtime        |
| `ipc_client`   | `IPCClient` | Internal IPC handle (do not use directly)|

### Methods

#### `subscribe(symbols, timeframe)`

Register interest in market data for the given symbols at the given timeframe.
Call this in `on_init`.

```python
def on_init(self, params: dict) -> None:
    self.subscribe(["RELIANCE.NS", "TCS.NS"], Timeframe.M5)
```

| Parameter   | Type            | Description                        |
| ----------- | --------------- | ---------------------------------- |
| `symbols`   | `list[str]`     | List of symbol identifiers         |
| `timeframe` | `Timeframe`     | Bar timeframe (e.g., `M1`, `H1`)  |

#### `indicator(name, symbol, *params) → float`

Retrieve a cached indicator value for the current bar. Returns `math.nan` if
the indicator has not been computed yet.

```python
sma_20 = self.indicator("sma", "RELIANCE.NS", 20)
rsi_14 = self.indicator("rsi", "RELIANCE.NS", 14)
```

| Parameter | Type     | Description                               |
| --------- | -------- | ----------------------------------------- |
| `name`    | `str`    | Indicator name (`"sma"`, `"ema"`, `"rsi"`, `"macd"`, `"bbands"`) |
| `symbol`  | `str`    | Symbol to look up                         |
| `*params` | `object` | Indicator-specific parameters (e.g., period) |

**Returns:** `float` — indicator value, or `math.nan`

#### `emit(signal)`

Send a trading signal to the Rust Order Trade Manager (OTM). The OTM
translates signals into orders based on risk rules.

```python
self.emit(Signal(
    symbol="RELIANCE.NS",
    direction="long",
    strength=0.85,
    metadata={"reason": "breakout"},
))
```

| Parameter | Type     | Description        |
| --------- | -------- | ------------------ |
| `signal`  | `Signal` | Signal to emit     |

#### `log(message)`

Send a log line to the APEX logging system via IPC.

```python
self.log(f"SMA crossed above price for {symbol}")
```

| Parameter | Type  | Description     |
| --------- | ----- | --------------- |
| `message` | `str` | Log message     |

### Event Hooks

Override these in your subclass. **`on_bar` and `on_tick` must complete in
< 1 ms** — the runtime emits a warning if exceeded.

#### `on_init(params)`

Called once when the strategy subprocess starts. Use this to set up
subscriptions and initialize state.

```python
def on_init(self, params: dict) -> None:
    self.subscribe(["AAPL", "MSFT"], Timeframe.M1)
    self.lookback = params.get("lookback", 20)
```

| Parameter | Type   | Description                                 |
| --------- | ------ | ------------------------------------------- |
| `params`  | `dict` | Key-value params from strategy configuration|

#### `on_bar(symbol, bar)`

Called on every new bar for subscribed symbols.

```python
def on_bar(self, symbol: str, bar: Bar) -> None:
    if bar.close > bar.open:
        self.emit(Signal(symbol=symbol, direction="long", strength=0.6))
```

| Parameter | Type  | Description              |
| --------- | ----- | ------------------------ |
| `symbol`  | `str` | Symbol that produced bar |
| `bar`     | `Bar` | OHLCV bar data           |

#### `on_tick(symbol, tick)`

Called on every tick for subscribed symbols. Use sparingly — ticks arrive at
high frequency.

| Parameter | Type   | Description               |
| --------- | ------ | ------------------------- |
| `symbol`  | `str`  | Symbol that produced tick |
| `tick`    | `Tick` | Tick data                 |

#### `on_stop()`

Called when the strategy is being shut down. Clean up resources here.

---

## Data Types

### `Timeframe`

Supported bar timeframes. String enum.

| Value | Description  |
| ----- | ------------ |
| `S1`  | 1 second     |
| `M1`  | 1 minute     |
| `M5`  | 5 minutes    |
| `M15` | 15 minutes   |
| `H1`  | 1 hour       |
| `H4`  | 4 hours      |
| `D1`  | 1 day        |
| `W1`  | 1 week       |

```python
from apex_sdk import Timeframe

tf = Timeframe.M5  # "5m"
```

### `Bar`

An OHLCV bar. Frozen dataclass with slots.

| Field          | Type        | Description                    |
| -------------- | ----------- | ------------------------------ |
| `symbol`       | `str`       | Symbol identifier              |
| `timeframe`    | `Timeframe` | Bar timeframe                  |
| `open`         | `float`     | Opening price                  |
| `high`         | `float`     | Highest price                  |
| `low`          | `float`     | Lowest price                   |
| `close`        | `float`     | Closing price                  |
| `volume`       | `float`     | Volume traded                  |
| `timestamp_ns` | `int`       | Nanosecond Unix timestamp      |

### `Tick`

A single price tick. Frozen dataclass with slots.

| Field          | Type    | Description               |
| -------------- | ------- | ------------------------- |
| `symbol`       | `str`   | Symbol identifier         |
| `price`        | `float` | Trade price               |
| `size`         | `float` | Trade size                |
| `timestamp_ns` | `int`   | Nanosecond Unix timestamp |

### `Signal`

A trading signal emitted by a strategy to the OTM.

| Field       | Type              | Description                      |
| ----------- | ----------------- | -------------------------------- |
| `symbol`    | `str`             | Target symbol                    |
| `direction` | `str`             | `"long"`, `"short"`, or `"flat"` |
| `strength`  | `float`           | Signal strength, 0.0 – 1.0      |
| `metadata`  | `dict[str, object]` | Arbitrary key-value metadata   |

#### `Signal.to_dict() → dict`

Serialize to a plain dict for msgpack transport.

---

## Complete Strategy Example

```python
"""Dual moving average crossover strategy."""

import math
from apex_sdk import Strategy, Bar, Signal, Timeframe


class DualMACrossover(Strategy):
    def on_init(self, params: dict) -> None:
        symbols = params.get("symbols", ["RELIANCE.NS"])
        self.fast_period = params.get("fast_period", 10)
        self.slow_period = params.get("slow_period", 30)

        self.subscribe(symbols, Timeframe.M5)
        self.prev_signal: dict[str, str] = {}
        self.log(f"DualMA initialized: fast={self.fast_period}, slow={self.slow_period}")

    def on_bar(self, symbol: str, bar: Bar) -> None:
        fast_sma = self.indicator("sma", symbol, self.fast_period)
        slow_sma = self.indicator("sma", symbol, self.slow_period)

        # Skip until indicators are warm
        if math.isnan(fast_sma) or math.isnan(slow_sma):
            return

        if fast_sma > slow_sma:
            direction = "long"
        elif fast_sma < slow_sma:
            direction = "short"
        else:
            return

        # Only emit on change
        prev = self.prev_signal.get(symbol)
        if direction != prev:
            strength = abs(fast_sma - slow_sma) / slow_sma
            self.emit(Signal(
                symbol=symbol,
                direction=direction,
                strength=min(strength * 10, 1.0),
                metadata={
                    "fast_sma": fast_sma,
                    "slow_sma": slow_sma,
                    "reason": "ma_crossover",
                },
            ))
            self.prev_signal[symbol] = direction
            self.log(f"{symbol}: {direction} signal (strength={strength:.3f})")

    def on_stop(self) -> None:
        self.log("DualMA strategy stopped")
```

---

## Runtime Configuration

Strategies are configured in `config/apex.toml`:

```toml
[strategy]
python_path = "python3"
strategies_dir = "strategies"
ipc_socket = "/tmp/apex_strategy.sock"
max_concurrent = 4
max_restarts = 3
latency_warn_threshold = "1ms"
```

The local launch scripts also respect the `.env` runtime overrides used by the
desktop app:

```bash
APEX_STRATEGY_PYTHON_PATH=python3
APEX_SIDECAR_SOCKET=/tmp/apex_strategy.sock
```

Use `.env.example` as the scaffold for these values when bootstrapping a fresh
checkout.

## IPC Protocol

Communication between the Python sidecar and Rust core uses **msgpack** over
Unix domain sockets. The protocol is request-response:

| Direction        | Message Type    | Payload                              |
| ---------------- | --------------- | ------------------------------------ |
| Rust → Python    | `bar_update`    | `{symbol, timeframe, ohlcv}`        |
| Rust → Python    | `tick_update`   | `{symbol, price, size, timestamp}`   |
| Python → Rust    | `emit_signal`   | `{strategy_id, signal}`             |
| Python → Rust    | `strategy_log`  | `{strategy_id, message}`            |
| Python → Rust    | `subscribe`     | `{symbols, timeframe}`              |
| Rust → Python    | `indicator_cache` | `{cache: {key: value, ...}}`      |

## Error Handling

- **Exceptions in `on_bar`/`on_tick`** are caught by the runner, logged, and
  do not crash the strategy process.
- **Unhandled exceptions in `on_init`** cause the strategy to enter `FAILED`
  state after `max_restarts` attempts.
- **Latency warnings** are emitted (not errors) when hooks exceed the
  configured threshold.

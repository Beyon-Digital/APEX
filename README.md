# ⚡ APEX — Adaptive Performance EXecution Terminal

> A Bloomberg-grade, locally-hosted trading terminal engineered for speed, fault-tolerance, and complete autonomy.  
> Built for traders who demand institutional-level tooling without institutional lock-in.

-----

## Table of Contents

1. [Project Vision](#1-project-vision)
1. [Core Design Philosophy](#2-core-design-philosophy)
1. [Architecture Overview](#3-architecture-overview)
1. [Ports & Adapters Pattern](#4-ports--adapters-pattern)
1. [Technology Stack](#5-technology-stack)
1. [System Components](#6-system-components)
1. [Feature Set](#7-feature-set)
1. [Data Flow & Pipeline](#8-data-flow--pipeline)
1. [Directory Structure](#9-directory-structure)
1. [Database & Storage Strategy](#10-database--storage-strategy)
1. [ML & Algorithmic Trading Engine](#11-ml--algorithmic-trading-engine)
1. [Broker Adapter Specifications](#12-broker-adapter-specifications)
1. [UI / Design Language](#13-ui--design-language)
1. [Performance Targets](#14-performance-targets)
1. [Fault Tolerance & Resilience](#15-fault-tolerance--resilience)
1. [Security Model](#16-security-model)
1. [Development Roadmap](#17-development-roadmap)
1. [Getting Started](#18-getting-started)

-----

## 1. Project Vision

**APEX** is a locally-hosted, full-stack trading terminal that fuses the data breadth of a Bloomberg Terminal with the low-latency execution philosophy of HFT infrastructure. It runs entirely on your machine — no cloud dependencies, no subscription fees, no data leakage.

It is built for:

- **Active traders & quants** who monitor multiple instruments across asset classes in real time
- **Algo developers** who want to script, backtest, and deploy automated strategies
- **Data scientists** who want to build, train, and deploy ML models directly within the trading loop
- **Risk-conscious operators** who demand full control over execution, data, and logic

Key differentiators vs. existing open-source tools:

- Hexagonal (Ports & Adapters) architecture makes adding any new broker or data source a single adapter file
- Sub-millisecond internal message bus via Rust core; Python scripting layer for strategy authoring
- Fully local data storage — TimescaleDB for tick/OHLCV, DuckDB for fast analytical queries, Redis for sub-ms hot state
- Graph-layer for custom vector/relationship modelling (petgraph + Neo4j-compatible export)
- Multi-panel, command-driven UI inspired by Bloomberg’s workflow model but built on modern web stack

-----

## 2. Core Design Philosophy

### Speed First, Ergonomics Second

Every architectural decision starts with latency impact analysis. The hot path (market data ingestion → signal computation → order dispatch) must never be blocked by UI rendering, logging, or DB writes. These happen on isolated threads/processes via lock-free channels.

### Nothing Leaves Your Machine

All market data, trade history, strategy code, model weights, credentials, and portfolio state live exclusively in your local environment. No telemetry, no cloud sync unless you explicitly configure it.

### Fail Loudly, Recover Silently

Circuit breakers at every broker adapter boundary. Automatic reconnection with exponential backoff. All state is journaled — the system can resume from crash without loss of position or in-flight order state.

### The Adapter is the Extension Point

Any new broker, data feed, ML framework, or notification system is a port implementation. Core business logic never changes when you add Zerodha, Interactive Brokers, or a WebSocket crypto feed. You write one adapter file and register it.

### Scripts Are First-Class Citizens

Strategy scripts are not plugins — they are first-class runtime entities with their own sandboxed execution context, live hot-reload, performance profiling, and kill-switch controls from the UI.

-----

## 3. Architecture Overview

```
┌──────────────────────────────────────────────────────────────────────────────────┐
│                            APEX TERMINAL (Local Process)                         │
│                                                                                  │
│  ┌────────────────────────────────────────────────────────────────────────────┐  │
│  │                         UI Layer (Tauri + React)                           │  │
│  │  [ Multi-Panel Workspace ] [ Charts ] [ Order Book ] [ Strategy IDE ]      │  │
│  │  [ News Feed ] [ Custom Vectors ] [ ML Dashboard ] [ Alert Console ]       │  │
│  └──────────────────────────────────┬─────────────────────────────────────────┘  │
│                                     │ IPC (Tauri Commands / Events)              │
│  ┌──────────────────────────────────▼─────────────────────────────────────────┐  │
│  │                      Application Core (Rust)                               │  │
│  │                                                                            │  │
│  │   ┌─────────────────┐  ┌───────────────┐  ┌──────────────────────────┐     │  │
│  │   │  Market Data    │  │  Order/Trade  │  │   Strategy Engine        │     │  │
│  │   │  Aggregator     │  │  Manager      │  │   (Script Orchestrator)  │     │  │
│  │   └────────┬────────┘  └───────┬───────┘  └────────────┬─────────────┘     │  │
│  │            │                   │                       │                   │  │
│  │   ┌────────▼───────────────────▼────────────────────────▼──────────────┐   │  │
│  │   │               Internal Message Bus (Tokio MPSC / crossbeam)        │   │  │
│  │   │               Lock-free SPSC queues on hot path                    │   │  │
│  │   └────────────────────────────────────────────────────────────────────┘   │  │
│  │                                                                            │  │
│  │   ┌─────────────────────────────────────────────────────────────────────┐  │  │
│  │   │                   Port Interfaces (Rust Traits)                     │  │  │
│  │   │  MarketDataPort | ExecutionPort | NewsPort | StoragePort            │  │  │
│  │   └─────────────────────────────────────────────────────────────────────┘  │  │
│  └──────────────────────────────────┬─────────────────────────────────────────┘  │
│                                     │                                            │
│  ┌──────────────────────────────────▼─────────────────────────────────────────┐  │
│  │                       Adapter Layer                                        │  │
│  │                                                                            │  │
│  │  [Zerodha Kite] [Interactive Brokers] [Alpaca] [Binance] [Yahoo Finance]   │  │
│  │  [NSE Feed]     [Reuters/Refinitiv]   [Alpha Vantage] [Custom WS Feed]     │  │
│  └──────────────────────────────────┬─────────────────────────────────────────┘  │
│                                     │                                            │
│  ┌──────────────────────────────────▼─────────────────────────────────────────┐  │
│  │                      Storage Layer                                         │  │
│  │                                                                            │  │
│  │  [TimescaleDB — tick/OHLCV]  [Redis — hot state]  [DuckDB — analytics]     │  │
│  │  [SQLite — config/trades]    [Local filesystem — models/scripts/logs]      │  │
│  └────────────────────────────────────────────────────────────────────────────┘  │
│                                                                                  │
│  ┌────────────────────────────────────────────────────────────────────────────┐  │
│  │              Python Sidecar (ML & Strategy Runtime)                        │  │
│  │  [scikit-learn] [PyTorch] [TA-Lib] [Backtrader] [Custom Script Sandbox]    │  │
│  └────────────────────────────────────────────────────────────────────────────┘  │
└──────────────────────────────────────────────────────────────────────────────────┘
```

The system is structured into five distinct horizontal layers, each communicating only via well-defined interfaces:

|Layer               |Responsibility                                              |Tech                              |
|--------------------|------------------------------------------------------------|----------------------------------|
|**UI**              |Rendering, user interaction, workspace management           |Tauri 2 + React 18 + TypeScript   |
|**Application Core**|Business logic, orchestration, state management             |Rust (Tokio async runtime)        |
|**Port Interfaces** |Contracts that decouple business logic from I/O             |Rust Traits                       |
|**Adapters**        |Concrete implementations of each port for specific providers|Rust + optional Python bridge     |
|**Storage**         |Local-first data persistence and retrieval                  |TimescaleDB, Redis, DuckDB, SQLite|

-----

## 4. Ports & Adapters Pattern

APEX uses Hexagonal Architecture (Ports & Adapters) as its structural backbone. This pattern cleanly isolates the trading domain from external concerns (APIs, databases, UI frameworks).

### Core Ports (Rust Traits)

```rust
// Market Data Port — any real-time feed must implement this
pub trait MarketDataPort: Send + Sync {
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<MarketDataStream>;
    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()>;
    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote>;
    fn adapter_id(&self) -> &'static str;
    fn health(&self) -> AdapterHealth;
}

// Execution Port — any broker must implement this
pub trait ExecutionPort: Send + Sync {
    async fn place_order(&self, order: &Order) -> Result<OrderId>;
    async fn cancel_order(&self, order_id: &OrderId) -> Result<()>;
    async fn modify_order(&self, order_id: &OrderId, params: &ModifyParams) -> Result<()>;
    async fn get_positions(&self) -> Result<Vec<Position>>;
    async fn get_order_status(&self, order_id: &OrderId) -> Result<OrderStatus>;
    fn broker_id(&self) -> &'static str;
    fn supported_order_types(&self) -> Vec<OrderType>;
}

// News Port — any news source must implement this
pub trait NewsPort: Send + Sync {
    async fn subscribe_headlines(&self, filters: &NewsFilter) -> Result<NewsStream>;
    async fn search(&self, query: &str, since: DateTime<Utc>) -> Result<Vec<NewsItem>>;
}

// Storage Port — abstract persistence
pub trait StoragePort: Send + Sync {
    async fn write_tick(&self, tick: &Tick) -> Result<()>;
    async fn write_ohlcv(&self, bar: &OHLCV) -> Result<()>;
    async fn query_ohlcv(&self, params: &OHLCVQuery) -> Result<Vec<OHLCV>>;
    async fn write_trade(&self, trade: &Trade) -> Result<()>;
    async fn query_trades(&self, params: &TradeQuery) -> Result<Vec<Trade>>;
}

// Strategy Port — ML and algo sidecar contract
pub trait StrategyPort: Send + Sync {
    async fn load_script(&self, path: &Path) -> Result<StrategyId>;
    async fn start(&self, id: &StrategyId, params: &Value) -> Result<()>;
    async fn stop(&self, id: &StrategyId) -> Result<()>;
    async fn get_metrics(&self, id: &StrategyId) -> Result<StrategyMetrics>;
    fn emit_signal(&self, signal: &TradingSignal);
}
```

### Adapter Registration

Adapters are registered at boot via a dependency injection registry:

```rust
// apex-core/src/bootstrap.rs
let mut registry = AdapterRegistry::new();

// Market Data Adapters
registry.register_market_data("zerodha", ZerodhaKiteAdapter::new(&config.zerodha)?);
registry.register_market_data("yahoo",   YahooFinanceAdapter::new());
registry.register_market_data("nse_ws",  NseFeedAdapter::new(&config.nse)?);

// Execution Adapters
registry.register_execution("zerodha_ex", ZerodhaExecutionAdapter::new(&config.zerodha)?);
registry.register_execution("ibkr",       IBKRAdapter::new(&config.ibkr)?);
registry.register_execution("paper",      PaperTradingAdapter::new()); // Always available

// News Adapters
registry.register_news("rss_aggregator", RSSNewsAdapter::new(&config.rss_feeds));

// Storage
registry.register_storage("timescale", TimescaleAdapter::new(&config.db_url)?);
registry.register_storage("duckdb",    DuckDBAdapter::new(&config.duckdb_path)?);
```

Adding a new broker requires only implementing `ExecutionPort` and `MarketDataPort` for that broker — zero changes to any other system component.

-----

## 5. Technology Stack

### Core Engine

|Component             |Technology                                                   |Rationale                                                                     |
|----------------------|-------------------------------------------------------------|------------------------------------------------------------------------------|
|Language              |**Rust 1.78+**                                               |Zero-cost abstractions, no GC pauses, memory safety, sub-microsecond latencies|
|Async Runtime         |**Tokio**                                                    |Production-grade async I/O, task scheduling, timer precision                  |
|Concurrency Primitives|**crossbeam** (SPSC/MPMC queues)                             |Lock-free data structures for hot path                                        |
|Serialization         |**serde + bincode** (internal), **serde_json** (API boundary)|Minimal allocation on hot path                                                |
|HTTP Client           |**reqwest** (async)                                          |Broker REST API calls                                                         |
|WebSocket             |**tokio-tungstenite**                                        |Real-time market feeds                                                        |
|IPC to UI             |**Tauri 2 Commands + Events**                                |Zero-copy where possible, structured command dispatch                         |
|Graph Engine          |**petgraph**                                                 |Instrument relationship graph, vector similarity                              |
|FIX Protocol          |Custom parser (no QuickFIX dep)                              |Lightweight FIX 4.x/5.0 decoder                                               |

### Frontend / UI

|Component                 |Technology                                           |Rationale                                                           |
|--------------------------|-----------------------------------------------------|--------------------------------------------------------------------|
|Desktop Shell             |**Tauri 2**                                          |Rust-native, ~10 MB binary, full OS access, no Electron RAM overhead|
|UI Framework              |**React 18** + **TypeScript 5**                      |Component model suits multi-panel workspace                         |
|State Management          |**Zustand** (global) + **React Query** (server state)|Minimal re-renders, simple mental model                             |
|Charting                  |**Lightweight Charts (TradingView lib)** + **D3.js** |Sub-16ms render on 50k candles; D3 for custom vector graphs         |
|Styling                   |**Tailwind CSS** + **CSS Variables**                 |Utility-first; terminal dark theme with configurable accent colours |
|Data Tables               |**TanStack Table v8**                                |Virtual scrolling, sub-ms DOM updates on tick data                  |
|Order Book Visualisation  |**Custom Canvas renderer (WebGL via regl)**          |DOM cannot handle 10k+ order book updates/sec                       |
|Layout Engine             |**React-Grid-Layout**                                |Drag-and-drop Bloomberg-style panel system                          |
|Code Editor (Strategy IDE)|**Monaco Editor**                                    |Full LSP, Python syntax, inline type hints                          |

### Python ML & Strategy Sidecar

|Component              |Technology                                                 |Rationale                                    |
|-----------------------|-----------------------------------------------------------|---------------------------------------------|
|Runtime                |**Python 3.11+** (sidecar subprocess, IPC over Unix socket)|ML ecosystem breadth                         |
|ML Frameworks          |**scikit-learn**, **PyTorch**, **XGBoost**, **LightGBM**   |Full spectrum from classical to deep learning|
|Technical Analysis     |**TA-Lib**, **pandas-ta**, **vectorbt**                    |Comprehensive indicator library              |
|Backtesting            |**Backtrader** + custom event-driven engine                |Realistic fill simulation                    |
|Data Manipulation      |**pandas**, **polars**                                     |Polars for large dataset transforms          |
|Communication with Rust|**Unix socket IPC** + **msgpack** framing                  |Low-overhead bidirectional messaging         |
|Notebook (optional)    |**Jupyter** (spawned on-demand)                            |Interactive analysis                         |

### Storage

|Store             |Technology                                          |Use Case                                                             |
|------------------|----------------------------------------------------|---------------------------------------------------------------------|
|Time-series       |**TimescaleDB** (PostgreSQL extension)              |Tick data, OHLCV bars — hypertable compression, continuous aggregates|
|Analytical queries|**DuckDB** (embedded)                               |Fast OLAP on parquet exports, backtesting slices                     |
|Hot state         |**Redis** (local)                                   |Current quotes, positions, open orders — sub-ms reads                |
|Relational config |**SQLite**                                          |Strategy configs, alert rules, watchlists, user preferences          |
|Vector / Graph    |**petgraph** (in-memory) + optional **Neo4j** export|Instrument relationships, sector correlations                        |
|Model artifacts   |**Local filesystem** (structured)                   |Trained model weights, feature pipelines                             |
|Raw archive       |**Parquet files** (via Arrow/DuckDB)                |Cold tick data archival                                              |

### Infrastructure & Tooling

|Component   |Technology                                               |
|------------|---------------------------------------------------------|
|Build system|Cargo (Rust) + Bun/Vite (frontend) + Hatchling/pip (Python)|
|Logging     |**tracing** (Rust, structured JSON) + **loguru** (Python)|
|Metrics     |**Prometheus** (local scrape) + optional Grafana         |
|Config      |**TOML** files; broker/API keys in an encrypted in-app vault|
|Testing     |**cargo test** + **pytest** + **Playwright** (E2E)       |
|Packaging   |**Tauri bundler** (.dmg / .AppImage / .msi)              |

-----

## 6. System Components

### 6.1 Market Data Aggregator (MDA)

The MDA is the system’s sensory cortex. It:

- Maintains WebSocket connections to all configured market data adapters simultaneously
- Normalises all incoming data into canonical `Tick` and `Quote` types regardless of source
- Broadcasts to internal subscribers via a topic-based pub/sub bus
- Applies symbol mapping (exchange suffixes, ISIN → ticker resolution)
- Detects feed outages and switches to fallback adapters transparently
- Writes every tick to the TimescaleDB write buffer (batched, non-blocking)
- Maintains a rolling in-memory ring buffer of recent ticks per symbol for instant chart loads

**Supported Feed Protocols:** WebSocket (JSON / binary), FIX 4.x/5.0, REST polling with adaptive interval, Server-Sent Events.

### 6.2 Order & Trade Manager (OTM)

The OTM owns all order lifecycle management:

- Accepts order intents from UI, strategy scripts, or automated signals
- Validates orders against pre-trade risk rules before dispatch
- Routes to the correct broker adapter based on account/instrument routing config
- Persists all order state to SQLite with full audit trail
- Handles partial fills, amendments, and cancellations
- Reconciles positions against broker responses every 30 seconds
- Exposes position and P&L snapshots to the UI in real-time

**Order Types:** Market, Limit, Stop, Stop-Limit, Trailing Stop, Bracket, Cover Order, After-Market Order.

**Pre-Trade Risk Checks:** Max order value, max position size (absolute + % of portfolio), max daily loss limit (hard stop), duplicate order detection window, market hours validation.

### 6.3 Strategy Engine & Script Orchestrator

- Loads Python strategy scripts from `strategies/` directory
- Each strategy runs in an isolated subprocess with defined I/O interfaces
- Strategies receive market data events and emit `TradingSignal` objects
- Signals pass to the OTM which applies final risk checks before order placement
- Live hot-reload: file changes detected, strategy restarts gracefully
- Full metrics per strategy: signal rate, fill rate, P&L attribution, execution latency
- Paper trading mode per strategy (simulated fills against live quotes)

**Strategy Script Interface (Python):**

```python
from apex_sdk import Strategy, Signal, SignalType, Timeframe

class MomentumCrossover(Strategy):

    def on_init(self, params: dict):
        self.fast = params.get("fast_period", 9)
        self.slow = params.get("slow_period", 21)
        self.subscribe(["RELIANCE", "HDFCBANK"], Timeframe.M5)

    def on_bar(self, symbol: str, bar: Bar):
        sma_fast = self.indicator("SMA", symbol, self.fast)
        sma_slow = self.indicator("SMA", symbol, self.slow)

        if sma_fast > sma_slow and self.prev("sma_fast") <= self.prev("sma_slow"):
            self.emit(Signal(symbol, SignalType.BUY, quantity=10, reason="EMA crossover"))

    def on_stop(self):
        self.log("Strategy stopped cleanly")
```

### 6.4 Custom Vector & Relationship Engine

Beyond standard correlations, APEX models custom relationships between instruments, sectors, and macro variables as a graph:

- Define nodes (instruments, sectors, events, custom variables)
- Define typed edges: `CORRELATED_WITH(r=0.82, window="30D")`, `BELONGS_TO(sector="Energy")`, `LEADS_BY(lag=2h)`, `PRICED_IN(currency="USD")`
- Visualise as a force-directed D3 layout in the UI
- Query: “Show all instruments with correlation > 0.7 to USDINR in last 30 days”
- Auto-compute correlation edges from historical price data
- Export to Neo4j Cypher or GraphML format
- Vector similarity search (cosine) to find instruments with similar return profiles

### 6.5 News & Sentiment Engine

- Aggregates RSS/Atom feeds from configurable sources (Economic Times, Reuters, NSE announcements, etc.)
- Parses and de-duplicates items into a unified `NewsItem` model
- NLP tagging: extracts ticker symbols, sectors, and named entities
- Sentiment scoring via lightweight local model (FinBERT-distilled or rule-based scorer)
- Alerts on high-relevance news for watched symbols
- News items linked to price chart timestamps for correlation inspection
- Full-text search with relevance ranking

### 6.6 ML Training & Inference Engine

**Training Pipeline:**

1. Data extraction via DuckDB (OHLCV + custom features)
1. Feature engineering pipeline (configurable, serialisable)
1. Model training (any sklearn/PyTorch model)
1. Backtesting validation against held-out period
1. Model registration to `models/` with full metadata

**Inference Pipeline:**

1. Registered models loaded at strategy startup
1. Predictions run in Python sidecar → sent to Rust core via IPC
1. Appear as custom indicators on charts or as strategy signals
1. Model drift detection runs nightly

**Supported Model Types:** Price direction classifiers (RF, XGBoost, LSTM), volatility forecasters (GARCH, feedforward NN), regime detectors (HMM, k-means), RL execution agents (PPO via stable-baselines3), custom PyTorch models.

### 6.7 Alerting & Notification System

- Price level alerts (above/below, % move, VWAP cross)
- Technical indicator alerts (RSI threshold, MACD cross, volume spike)
- Position P&L alerts (profit target, stop loss proximity)
- Strategy alerts (signal generated, order filled, error)
- News keyword alerts (custom regex on news feed)
- Delivery: in-app toast, sound, desktop OS notification, optional Telegram webhook

-----

## 7. Feature Set

### 7.1 Workspace & Layout

- Multi-panel workspace with drag-and-drop, resize, float, and snap-to-grid
- Save / load workspace layouts (named profiles)
- Bloomberg-style command bar: type `RELIANCE:CHART` to open chart, `:ORDERS` for order blotter
- Full keyboard shortcut coverage for every action
- Multi-monitor support (Tauri native window management)
- Dark terminal theme + configurable accent palette

### 7.2 Charting

- Candlestick, OHLC, Line, Area, Heikin-Ashi, Renko charts
- Timeframes: 1s, 5s, 15s, 30s, 1m, 3m, 5m, 15m, 30m, 1h, 4h, 1D, 1W, 1M
- 80+ built-in technical indicators (MA family, oscillators, volume, volatility, trend)
- Custom indicator scripting (Python, rendered as overlays)
- Multi-instrument overlay (normalised returns comparison)
- Chart drawing tools (trend lines, Fibonacci, channels, Gann)
- Volume profile (VWAP, VPOC, VA high/low)
- Session separators, economic event markers, news event pins
- Chart linking: symbol click in one panel updates all linked panels

### 7.3 Market Data & Watchlists

- Unlimited watchlists with grouping and sorting
- Real-time streaming quotes: bid/ask, last, volume, day change %, VWAP
- Sector/index heat maps
- Market depth / Level 2 order book (where broker provides)
- Time and Sales (tape) window per instrument
- Options chain viewer with Greeks
- Futures chain with basis calculation
- Currency / FX rate matrix
- Custom market scanner with filter builder (momentum, volume breakout, RSI range, etc.)

### 7.4 Order Entry & Execution

- One-click / hotkey order entry from chart
- Advanced order entry form with bracket/cover order support
- Order staging (stage orders, send on confirmation)
- Live order blotter with status updates
- Position dashboard with real-time P&L, beta, Greeks
- Trade history with full audit trail and CSV export
- Portfolio analytics: exposure by sector, currency, asset class
- Paper trading mode toggle (per account or system-wide)

### 7.5 Strategy IDE & Automation

- Built-in Monaco editor with Python syntax + APEX SDK autocomplete
- Strategy file browser with run / stop / restart controls
- Live strategy performance dashboard: trades, win rate, Sharpe, drawdown
- Backtest runner with parameter sweep (grid search over strategy params)
- Backtest result visualiser: equity curve, drawdown chart, trade scatter
- Strategy scheduling: market hours, timer, event triggers
- Signal log: full audit of every emitted signal and its fill outcome

### 7.6 ML Workbench

- Dataset builder: select symbol, date range, features, target variable
- Feature engineering UI: TA indicators, lag features, rolling statistics
- Model training wizard: algorithm picker, hyperparameter config, cross-validation
- Training progress with live loss curves
- Model evaluation report: classification/regression metrics + equity curve on backtest
- Model registry: version-tracked models with metadata and performance history
- One-click model deployment as live signal source

### 7.7 Custom Vectors & Graph

- Graph node editor: instrument, sector, macro variable nodes
- Relationship editor: typed, weighted edges between nodes
- Automatic correlation edge computation from historical data
- Force-directed graph visualisation (zoom / pan / node focus)
- Similarity search: “find instruments most similar to this return profile”
- Graph query interface: filter by relationship type, strength, direction
- Export as Cypher (Neo4j) or GraphML

### 7.8 News & Research

- Multi-source news feed with source priority and filtering
- Per-symbol news timeline linked to chart timestamps
- Sentiment score badge on news items
- Custom keyword alert rules
- News search with date range, symbol, sentiment filters
- Earnings calendar and economic events calendar (auto-fetched)
- Corporate action tracker (dividends, splits, rights issues)

### 7.9 Data Management

- Historical data downloader (OHLCV from configured sources)
- Data quality inspector: gap detection, outlier flagging
- Parquet export for any symbol/date range
- Storage dashboard: disk usage per dataset, retention policy config
- Import from CSV / Excel / broker export

### 7.10 System & Configuration

- Adapter configuration UI: add/remove/test broker and feed connections
- Risk parameter editor: per-account and global limits
- Alert rule manager
- Performance dashboard: CPU/RAM/network/latency metrics
- Log viewer with live tail and filter

-----

## 8. Data Flow & Pipeline

### Real-Time Tick Path (Hot Path — Latency Critical)

```
External Feed (WS/FIX)
        │
        ▼
Adapter (decode bytes → canonical Tick struct)       ~5–50μs decode
        │
        ▼
Market Data Aggregator (normalise, symbol resolve)
        │
        ├──→ Redis HMSET (hot quote state, fire-and-forget)
        │
        ├──→ SPSC channel → Strategy Engine subscribers
        │         └──→ Strategy on_tick() → Signal → OTM → Broker
        │
        ├──→ MPSC channel → UI event bridge → React
        │         └──→ Chart update, watchlist row update
        │
        └──→ TimescaleDB async write buffer (batched 100ms)
```

**Total hot path target: < 1ms** from feed receipt to strategy signal emission (software latency, excluding network round-trip to broker).

### Order Execution Path

```
Signal / Manual Order Intent
        │
        ▼
Pre-Trade Risk Engine (sync checks, ~10μs)
        │  PASS / REJECT
        ▼ (PASS)
Execution Adapter (serialise → broker REST/WS)
        │
        ├──→ Order acknowledgement → OTM state update → UI
        └──→ Fill event → position update → P&L recalc → UI
```

### Historical / Analytical Path (Throughput Optimised)

```
DuckDB query (OHLCV from Parquet / TimescaleDB)
        │  Polars DataFrame in Python sidecar
        ▼
Feature Engineering Pipeline
        ▼
ML model training / backtest engine
        ▼
Results → msgpack → Rust IPC → UI
```

-----

## 9. Directory Structure

```
apex/
├── Cargo.toml                         # Rust workspace root
├── Cargo.lock
├── tauri.conf.json
├── package.json                       # Frontend workspace root
│
├── apex-core/                         # Rust: Core business logic
│   ├── src/
│   │   ├── lib.rs
│   │   ├── bootstrap.rs               # DI registry init
│   │   ├── domain/
│   │   │   ├── models.rs              # Tick, Quote, Order, Trade, Bar, Position
│   │   │   ├── events.rs              # Domain events
│   │   │   └── errors.rs
│   │   ├── ports/
│   │   │   ├── market_data.rs         # MarketDataPort trait
│   │   │   ├── execution.rs           # ExecutionPort trait
│   │   │   ├── news.rs                # NewsPort trait
│   │   │   ├── storage.rs             # StoragePort trait
│   │   │   └── strategy.rs            # StrategyPort trait
│   │   ├── application/
│   │   │   ├── market_data_aggregator.rs
│   │   │   ├── order_trade_manager.rs
│   │   │   ├── strategy_orchestrator.rs
│   │   │   ├── news_engine.rs
│   │   │   ├── alert_engine.rs
│   │   │   └── risk_engine.rs
│   │   └── bus/
│   │       ├── message_bus.rs         # Internal pub/sub
│   │       └── topics.rs
│   └── Cargo.toml
│
├── apex-adapters/                     # Rust: All adapter implementations
│   ├── src/
│   │   ├── market_data/
│   │   │   ├── zerodha_kite.rs
│   │   │   ├── yahoo_finance.rs
│   │   │   ├── nse_feed.rs
│   │   │   ├── alpaca.rs
│   │   │   └── binance.rs
│   │   ├── execution/
│   │   │   ├── zerodha_execution.rs
│   │   │   ├── ibkr.rs
│   │   │   ├── alpaca_execution.rs
│   │   │   └── paper_trading.rs
│   │   └── storage/
│   │       ├── timescale.rs
│   │       ├── duckdb.rs
│   │       ├── redis_state.rs
│   │       └── sqlite_config.rs
│   └── Cargo.toml
│
├── apex-tauri/                        # Tauri desktop shell
│   ├── src/
│   │   ├── main.rs
│   │   ├── commands/
│   │   │   ├── market.rs
│   │   │   ├── orders.rs
│   │   │   ├── strategies.rs
│   │   │   └── data.rs
│   │   └── state.rs
│   └── Cargo.toml
│
├── apex-ui/                           # React frontend
│   ├── src/
│   │   ├── main.tsx
│   │   ├── App.tsx
│   │   ├── store/
│   │   │   ├── workspaceStore.ts
│   │   │   ├── marketStore.ts
│   │   │   ├── orderStore.ts
│   │   │   └── strategyStore.ts
│   │   ├── components/
│   │   │   ├── workspace/
│   │   │   │   ├── PanelGrid.tsx      # Drag-drop layout
│   │   │   │   ├── Panel.tsx
│   │   │   │   └── CommandBar.tsx     # Bloomberg-style command input
│   │   │   ├── charts/
│   │   │   │   ├── CandleChart.tsx    # TradingView Lightweight Charts
│   │   │   │   ├── OrderBookHeatmap.tsx  # WebGL canvas
│   │   │   │   ├── VectorGraph.tsx    # D3 force-directed
│   │   │   │   └── IndicatorPanel.tsx
│   │   │   ├── trading/
│   │   │   │   ├── OrderEntry.tsx
│   │   │   │   ├── OrderBlotter.tsx
│   │   │   │   ├── PositionTable.tsx
│   │   │   │   └── RiskGauge.tsx
│   │   │   ├── strategy/
│   │   │   │   ├── StrategyIDE.tsx    # Monaco editor
│   │   │   │   ├── StrategyCard.tsx
│   │   │   │   ├── BacktestResults.tsx
│   │   │   │   └── MLWorkbench.tsx
│   │   │   ├── news/
│   │   │   │   ├── NewsFeed.tsx
│   │   │   │   └── SentimentBadge.tsx
│   │   │   └── system/
│   │   │       ├── AlertConsole.tsx
│   │   │       └── HealthMonitor.tsx
│   │   ├── hooks/
│   │   │   ├── useMarketData.ts
│   │   │   ├── useOrders.ts
│   │   │   └── useStrategy.ts
│   │   └── theme/
│   │       ├── tokens.css
│   │       └── terminal.css
│   ├── index.html
│   ├── vite.config.ts
│   └── package.json
│
├── apex-python/                       # Python ML & strategy sidecar
│   ├── apex_sdk/
│   │   ├── __init__.py
│   │   ├── strategy.py                # Base Strategy class
│   │   ├── indicators.py              # TA-Lib wrappers
│   │   ├── models.py                  # Pydantic domain models
│   │   └── ipc.py                     # Unix socket IPC client
│   ├── ml/
│   │   ├── pipeline.py
│   │   ├── trainer.py
│   │   ├── evaluator.py
│   │   └── registry.py
│   ├── runtime/
│   │   ├── sidecar.py                 # Main sidecar entrypoint
│   │   └── sandbox.py                 # Strategy subprocess isolation
│   └── pyproject.toml
│
├── strategies/                        # User strategy scripts (hot-loaded)
│   ├── examples/
│   │   ├── momentum_crossover.py
│   │   ├── mean_reversion.py
│   │   └── ml_classifier_signal.py
│   └── README.md
│
├── models/                            # Trained ML model artifacts
│   └── registry.json
│
├── config/
│   ├── apex.toml                      # Main config
│   ├── risk.toml                      # Risk parameter overrides
│   └── feeds.toml                     # Feed/news source definitions
│
├── data/
│   ├── parquet/                       # Cold archived tick data
│   ├── sqlite/                        # Config DB
│   └── logs/
│
└── docs/
    ├── architecture.md
    ├── adapter-guide.md
    ├── strategy-api.md
    └── ml-guide.md
```

-----

## 10. Database & Storage Strategy

### TimescaleDB (Primary Time-Series Store)

```sql
-- Tick data — hypertable partitioned by time, compressed after 7 days
CREATE TABLE ticks (
    time        TIMESTAMPTZ NOT NULL,
    symbol      TEXT NOT NULL,
    bid         NUMERIC(18,6),
    ask         NUMERIC(18,6),
    last        NUMERIC(18,6),
    volume      BIGINT,
    source      TEXT
);
SELECT create_hypertable('ticks', 'time');
SELECT add_compression_policy('ticks', INTERVAL '7 days');

-- Continuous aggregate: 1-minute OHLCV (auto-refreshed)
CREATE MATERIALIZED VIEW ohlcv_1m
WITH (timescaledb.continuous) AS
SELECT
    time_bucket('1 minute', time) AS bucket,
    symbol,
    first(last, time)  AS open,
    max(last)          AS high,
    min(last)          AS low,
    last(last, time)   AS close,
    sum(volume)        AS volume
FROM ticks
GROUP BY bucket, symbol;
```

### DuckDB (Analytical Queries)

```sql
-- 30-day rolling correlation between two instruments
SELECT
    date_trunc('day', time) AS day,
    corr(r_RELIANCE, r_HDFCBANK) OVER (
        ORDER BY date_trunc('day', time)
        ROWS BETWEEN 29 PRECEDING AND CURRENT ROW
    ) AS rolling_corr
FROM returns_view
ORDER BY day;
```

### Redis Schema (Hot State)

```
QUOTE:{SYMBOL}              → Hash { bid, ask, last, volume, updated_at }
POSITION:{ACCOUNT}:{SYMBOL} → Hash { qty, avg_price, pnl, side }
ORDER:{ORDER_ID}            → Hash { status, symbol, qty, price, ... }
STRATEGY:{ID}:state         → Hash { status, last_signal, pnl, ... }
```

### Data Retention Policy

|Data Type|Hot (Redis)   |Warm (TimescaleDB)           |Cold (Parquet)|
|---------|--------------|-----------------------------|--------------|
|Tick data|Session       |30 days (compressed after 7d)|30d+          |
|OHLCV 1m |Last 2h       |1 year                       |1y+           |
|OHLCV 1D |All time      |All time                     |Backup only   |
|Trades   |Open positions|All time                     |All time      |
|News     |Last 24h      |90 days                      |90d+          |

-----

## 11. ML & Algorithmic Trading Engine

### Model Training Workflow

```
1. Define Dataset
   └── Symbol(s), date range, features, target, train/test split

2. Feature Engineering
   ├── OHLCV features (returns, log-returns, range)
   ├── Technical indicators (RSI, MACD, ATR, VWAP deviation)
   ├── Volume features (OBV, volume z-score)
   ├── Calendar features (hour-of-day, day-of-week, days-to-expiry)
   ├── Cross-asset features (correlation with BNF, USDINR, VIX)
   └── Lag features (t-1, t-2, t-5, t-10 returns)

3. Model Selection
   └── RandomForest / XGBoost / LightGBM / LSTM / Transformer

4. Training & Validation
   └── TimeSeriesSplit cross-validation (no lookahead bias)

5. Evaluation
   ├── Classification: Precision, Recall, F1, ROC-AUC
   ├── Regression: MAE, RMSE, Information Coefficient (IC)
   └── Trading: Sharpe, Calmar, Max Drawdown, Win Rate

6. Registration
   └── Model saved: weights + feature pipeline + training metadata

7. Deployment
   └── Model loaded as live signal source → strategy → OTM
```

### Backtesting Engine

Event-driven, designed to avoid lookahead bias:

- Processes bars strictly in time order
- Fills use next-bar open price (conservative) or configurable slippage model
- Accounts for commission (configurable per adapter)
- Parameter sweeps with walk-forward optimisation
- Outputs: equity curve, underwater plot, trade log, statistics report

### Reinforcement Learning (Advanced)

- Environment: custom `gym.Env` wrapping the backtesting engine
- Observation space: recent OHLCV + indicators + position state
- Action space: Buy / Sell / Hold (or continuous position sizing)
- Reward: Sharpe-adjusted P&L per step
- Algorithms: PPO, SAC (via stable-baselines3)

-----

## 12. Broker Adapter Specifications

### Interface Commitment

Every broker adapter must implement:

1. `MarketDataPort` — streaming quote subscription
1. `ExecutionPort` — order CRUD and position query
1. Health check with structured status
1. Automatic reconnection (exponential backoff, max 5 retries, cap 60s)
1. Rate limit awareness
1. Auth token refresh before expiry

### Planned Adapters

|Broker / Feed             |Type                    |Protocol        |Priority        |
|--------------------------|------------------------|----------------|----------------|
|**Zerodha Kite**          |Indian equity/F&O + data|REST + WS       |v1.0            |
|**Interactive Brokers**   |Multi-asset global      |TWS API (socket)|v1.0            |
|**Alpaca**                |US equity               |REST + WS       |v1.0            |
|**Binance**               |Crypto                  |REST + WS       |v1.0            |
|**Yahoo Finance**         |Data only               |REST polling    |v1.0            |
|**NSE India (unofficial)**|Data only               |REST + WS       |v1.0            |
|**Paper Trading**         |Simulation              |Internal        |v1.0 (always on)|
|**Alpha Vantage**         |Data + news             |REST            |v1.1            |
|**Fyers**                 |Indian equity/F&O       |REST + WS       |v1.1            |
|**Angel One SmartAPI**    |Indian equity/F&O       |REST + WS       |v1.1            |

### Adding a New Adapter (Summary)

1. Create `apex-adapters/src/execution/{broker}.rs`
1. `impl ExecutionPort for {Broker}Adapter { … }`
1. Add auth config to `config/apex.toml` schema
1. Register in `bootstrap.rs`
1. Write integration tests against a mock server
1. Document in `docs/adapter-guide.md`

Zero changes to core business logic required.

-----

## 13. UI / Design Language

### Terminal Aesthetic

APEX uses a **high-density, dark terminal aesthetic** — colour is functional, not decorative. Every pixel earns its place.

**Principles:**

- **Density over whitespace**: Information packing is a feature, not a bug
- **Colour as signal**: Green/red for direction, amber for warning, cyan for interactive. No decorative colour
- **Monospace data, proportional labels**: Prices in fixed-width (JetBrains Mono); UI labels in clean grotesque (IBM Plex Sans)
- **Subdued chrome, sharp data**: UI scaffolding is desaturated; prices, P&L, and signals are vivid
- **Keyboard-first**: Every critical action has a hotkey

### Colour Palette

```css
:root {
  /* Backgrounds */
  --bg-void:          #080b0f;
  --bg-base:          #0d1117;
  --bg-elevated:      #161b22;
  --bg-overlay:       #1c2333;
  --bg-hover:         #21262d;

  /* Borders */
  --border-subtle:    #21262d;
  --border-default:   #30363d;
  --border-focus:     #388bfd;

  /* Market direction */
  --color-bull:       #3fb950;
  --color-bear:       #f85149;
  --color-neutral:    #8b949e;

  /* Interactive accents */
  --accent-primary:   #388bfd;
  --accent-warning:   #e3b341;
  --accent-danger:    #f85149;

  /* Text */
  --text-primary:     #e6edf3;
  --text-secondary:   #8b949e;
  --text-disabled:    #484f58;
  --text-code:        #79c0ff;

  /* Chart */
  --chart-bull-body:  rgba(63, 185, 80, 0.85);
  --chart-bear-body:  rgba(248, 81, 73, 0.85);
  --chart-grid:       rgba(48, 54, 61, 0.5);
  --chart-crosshair:  rgba(56, 139, 253, 0.6);
}
```

### Typography

```css
--font-mono: 'JetBrains Mono', 'Fira Code', monospace;
font-feature-settings: "tnum" 1, "zero" 1;  /* tabular numbers */

--font-ui:   'IBM Plex Sans', 'SF Pro Text', system-ui, sans-serif;

--text-xs:   10px;   /* dense watchlist rows */
--text-sm:   12px;   /* standard data */
--text-md:   13px;   /* default */
--text-lg:   16px;   /* panel headers */
--text-xl:   20px;   /* major metrics */
--text-hero: 28px;   /* account P&L hero */
```

### Command Bar Reference

|Command                     |Action                        |
|----------------------------|------------------------------|
|`RELIANCE`                  |Open default view for symbol  |
|`RELIANCE:CHART`            |Open candlestick chart        |
|`RELIANCE:DEPTH`            |Open order book               |
|`RELIANCE:NEWS`             |Open news timeline            |
|`:ORDERS`                   |Open order blotter            |
|`:POS`                      |Open positions dashboard      |
|`:SCAN`                     |Open market scanner           |
|`:ML`                       |Open ML workbench             |
|`:STRAT`                    |Open strategy IDE             |
|`:GRAPH`                    |Open vector/relationship graph|
|`BUY RELIANCE 10`           |Stage a market buy order      |
|`SELL HDFCBANK 5 LIMIT 1600`|Stage a limit sell order      |

-----

## 14. Performance Targets

|Metric                             |Target                      |Measurement        |
|-----------------------------------|----------------------------|-------------------|
|Tick-to-strategy-signal latency    |< 1ms                       |Internal trace span|
|Order dispatch (signal → API call) |< 5ms                       |Trace span         |
|UI quote update rate               |60fps sustained, 10k symbols|Browser perf API   |
|Historical data load (1yr 1m OHLCV)|< 200ms                     |DuckDB query time  |
|App cold start                     |< 3 seconds                 |Tauri startup trace|
|Memory footprint (idle, 50 symbols)|< 300MB                     |OS process monitor |
|Backtest (10yr daily data)         |< 5 seconds                 |Benchmark suite    |
|TimescaleDB write throughput       |> 100k ticks/sec            |pgbench            |
|Concurrent strategy scripts        |20                          |Load test          |

-----

## 15. Fault Tolerance & Resilience

### Circuit Breakers (Per Adapter)

```toml
# config/apex.toml
[adapter.zerodha.circuit_breaker]
failure_threshold = 5        # failures before OPEN
success_threshold = 2        # successes in HALF-OPEN before CLOSE
timeout_seconds   = 30       # time in OPEN before attempting HALF-OPEN
```

States: **Closed** (normal) → **Open** (failing, all calls rejected, fallback activated) → **Half-Open** (test recovery).

### State Journaling

The OTM journals every state transition (intent + outcome) to SQLite WAL before executing. On crash recovery, the journal is replayed and reconciled against the broker.

### Position Reconciliation

Every 30s, OTM fetches live positions from broker and reconciles against local state. Discrepancies trigger an alert. Broker state is authoritative.

### Maximum Loss Circuit Breaker

If session P&L drops below `max_daily_loss`, **all execution adapters are disabled immediately**. No strategy script can override this. Only manual UI reset re-enables execution.

### Feed Failover

Adapter outage → UNHEALTHY marking → fallback adapter activated → background reconnection with exponential backoff (1s, 2s, 4s, 8s, 16s, 32s, cap 60s) → alert emitted.

-----

## 16. Security Model

### Credential Storage

- Broker API keys encrypted at rest via OS keychain (macOS Keychain / Windows DPAPI / libsecret)
- In memory: `SecretString` types zeroed on drop
- Never logged, never serialised outside the keychain

### Network Security

- All external calls: HTTPS/WSS with TLS certificate verification enforced
- Local services (TimescaleDB, Redis) bind to `127.0.0.1` only
- Tauri IPC scope-controlled: frontend can only invoke allowlisted commands

### Strategy Sandbox

- Scripts run in subprocess with restricted builtins
- No direct filesystem or network access; all I/O via SDK API
- Resource limits: CPU %, max memory, max execution time per bar

### Audit Trail

All order placements, modifications, and cancellations logged with nanosecond timestamp, source (manual / strategy ID), pre/post state, and broker response.

-----

## 17. Development Roadmap

### Phase 1 — Foundation (✅ Complete)

- [x] Cargo workspace + Tauri 2 shell + Vite/React setup
- [x] Core domain models and port trait definitions
- [x] Internal message bus (topic-based pub/sub with 9 topics)
- [x] SQLite adapter (config + audit trail persistence)
- [x] TimescaleDB adapter (hypertable schema, query builder with Send safety)
- [x] Redis adapter (hot state caching with typed query support)
- [x] Yahoo Finance market data adapter (polling with 3s intervals)
- [x] Paper trading execution adapter (slippage + commission modeling)
- [x] Basic watchlist UI with real-time price updates
- [x] Basic candlestick chart (TradingView Lightweight Charts)

### Phase 2 — Trading Core (✅ 100% Complete)

- [x] Zerodha Kite adapter (market data + execution via REST/polling)
- [x] Angel One SmartAPI adapter (execution + market data with JWT auth, polling subscription)
- [x] Groww adapter (execution + market data with REST API, polling subscription)
- [x] Robinhood adapter (execution + market data with OAuth bearer, cancel-replace pattern)
- [x] Order entry UI with data-testid attrs + order blotter
- [x] Position dashboard with P&L (UI components with testids, integration complete)
- [x] Pre-trade risk engine (max order value, position %, daily loss limits)
- [x] Alert system (engine + UI integration with create/remove/list)
- [x] News feed engine (RSS aggregator with sentiment scoring)
- [x] Multi-panel workspace layout with tab switching (Chart / Strategy IDE)

### Phase 3 — Analytics & Automation (✅ 100% Complete)

- [x] Python sidecar IPC (Unix socket with msgpack framing)
- [x] Strategy IDE (Monaco editor integrated with file management, run/stop/save)
- [x] Strategy SDK base classes (on_init, on_bar, on_tick, on_stop hooks)
- [x] Backtest engine with full simulation loop (buy/sell/close signals, equity curve, Sharpe ratio, max drawdown, slippage & commission modeling)
- [x] DuckDB analytical engine (OLAP queries, Parquet I/O, rolling correlations, statistics)
- [x] Technical indicator library (SMA, EMA, RSI, MACD, Bollinger Bands, ATR, VWAP, Stochastic, StdDev, ROC)
- [x] Market scanner (price/volume/indicator-based criteria screening)
- [x] Historical data downloader (Yahoo Finance bulk download with CSV storage)

### Phase 4 — Intelligence (✅ ~95% Complete)

- [x] ML trainer pipeline structure (trainer.py with time-series CV)
- [x] Graph engine (petgraph-based vector/relationship modeling with correlation analysis)
- [x] D3 graph visualization (VectorGraph component with force layout)
- [x] ML workbench UI integration (dataset builder, training configuration, evaluation display, model registry)
- [x] Model registry with IPC commands (list, train, delete models via Tauri)
- [x] Walk-forward backtest engine (rolling train/test windows, overfitting ratio)
- [x] News sentiment analysis (financial-lexicon VADER-style scorer integrated into NewsEngine)
- [x] AI Copilot panel (OpenRouter-backed chat with live positions/quotes/risk context)

### Phase 5 — Production Hardening (✅ ~95% Complete)

- [x] Circuit breakers on adapters (reusable generic `CircuitBreaker<T>` with state machine)
- [x] Risk engine with hard stops (< 10μs latency target, atomic P&L tracking)
- [x] Crash recovery on startup (reconcile stale Pending/Open orders with brokers)
- [x] Position reconciliation loop (30s periodic polling for all registered brokers)
- [x] E2E test framework (Playwright: 67 browser-mode tests, all passing)
- [x] Unit test coverage (Rust workspace: 194 passing tests; Python SDK: 21 passing tests)
- [x] Tauri IPC commands (20 commands: market, orders, alerts, risk, data, ML, health)
- [x] Real-time event push system (7 event types via message bus → Tauri emitter)
- [x] Health monitoring dashboard (adapter status, system metrics, real-time polling)
- [x] Hot path tracing instrumentation (market data aggregator, OTM, risk engine)
- [x] Security audit (IPC input validation, secret-free code paths, dependency review)
- [ ] Performance profiling and optimisation

### Phase 8 — Performance & Hardening (✅ ~80% Complete)

- [x] Hot path profiling (tracing::instrument on critical paths)
- [x] Circuit breaker implementation (generic CircuitBreaker<T> with state machine)
- [x] Crash recovery (reconcile stale orders on startup)
- [x] Testing strategy (164 Rust + 65 Playwright + 21 Python tests)
- [ ] Jaeger trace export (optional)

### Phase 9 — Configuration & Packaging (✅ ~90% Complete)

- [x] Configuration schema (config/apex.example.toml, fully commented)
- [x] Setup & migration scripts (scripts/setup_db.sh, scripts/dev.sh)
- [x] Database migrations (migrations/ with 3 SQL files)
- [x] Tauri bundle configuration (io.apex.terminal, 1920x1080, CSP, Python sidecar)
- [x] Icon generation from source SVG

### Phase 10 — Documentation (✅ Complete)

- [x] Adapter guide (docs/adapter-guide.md — 350 lines)
- [x] Strategy API reference (docs/strategy-api.md — 328 lines)
- [x] ML workbench guide (docs/ml-guide.md — 300 lines)

### Phase UI — Frontend (✅ ~95% Complete)

- [x] Design system & theming (CSS variables, dark theme)
- [x] Tauri event bridge (useQuoteStream, useOrderStream, usePositionStream, useHealthStream hooks)
- [x] Workspace layout (13-tab center panel, 3-column responsive grid)
- [x] Market overview page (index cards, breadth meter, top movers, headlines)
- [x] Ticker tape marquee (major indices + FX + commodities)
- [x] Keyboard HUD (`?` overlay) and sortable/flashing watchlist
- [x] Command bar (order placement, symbol navigation, tab switching with feedback)
- [x] Candlestick chart (lightweight-charts integration)
- [x] Order book heatmap (OrderBookHeatmap component + live Book panel)
- [x] Order entry form (symbol, side, type, quantity, price with validation)
- [x] Strategy IDE (Monaco editor with file management)
- [x] News panel, order blotter, scanner, correlation graph, copilot, market overview panels (13-tab center workspace)
- [x] News + fired-alert Tauri event streams (useNewsStream, useAlertStream)

### Current Status Summary (March 2026)

**Working Features:**
- ✅ Full-stack foundation (Rust core, React UI, Python SDK)
- ✅ Paper trading with realistic order execution (slippage + commission)
- ✅ Root workflow wrappers via `bun run dev`, `bun run dev:web-only`, `bun run build`, and `bun run verify`
- ✅ Yahoo Finance real-time quotes (3s polling)
- ✅ Zerodha Kite adapter (market data + execution via REST API)
- ✅ Risk engine with pre-trade validation and daily loss circuit breaker
- ✅ Full UI: watchlist, candlestick charts, order entry, positions, alerts
- ✅ Strategy IDE with Monaco editor, file management, and run/stop controls
- ✅ Python strategy framework with IPC bridge and subprocess isolation
- ✅ Circuit breaker pattern for adapter resilience (generic `CircuitBreaker<T>`)
- ✅ Backtest engine with equity curve, Sharpe ratio, max drawdown metrics
- ✅ Walk-forward backtest with overfitting detection
- ✅ Crash recovery and periodic position reconciliation (30s loop)
- ✅ News engine with RSS aggregation and sentiment scoring
- ✅ Graph engine with petgraph + D3 force-directed visualization
- ✅ ML Workbench UI with training, feature selection, and model registry
- ✅ Health monitoring dashboard with adapter status
- ✅ Command bar with order placement and workspace navigation
- ✅ Tauri event bridge hooks for real-time data streaming
- ✅ Browser-mode development fallback with mocked Tauri IPC plus polling for Playwright/UI work
- ✅ Desktop/Tauri startup path verified through `scripts/dev.sh` and `cd apex-ui && bun run tauri:dev`
- ✅ Hot path tracing instrumentation for performance analysis
- ✅ Database migration files for production deployment
- ✅ Production bundle configuration with Python sidecar

**Remaining Gaps:**
- ⚠️ **Performance profiling** — hot-path tracing is in place; systematic latency profiling pending
- ⚠️ **Jaeger trace export** — optional NDJSON trace export exists; Jaeger OTLP sink pending

**Next Milestones:**
1. Integrate **NLP sentiment pipeline** with news engine
2. Complete **security audit**
3. Generate **application icons** from SVG source

-----

## 18. Getting Started

### Prerequisites

```bash
# Rust (stable 1.78+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Bun (latest stable)
curl -fsSL https://bun.sh/install | bash

# Python 3.11 recommended for the full sidecar/runtime
# (the SDK tests also degrade cleanly on older local interpreters)

# Optional local services for extended storage experiments
# - TimescaleDB
# - Redis
```

### Setup

```bash
git clone https://github.com/yourorg/apex && cd apex
cp config/apex.example.toml config/apex.toml   # if it does not already exist
cp .env.example .env                           # if it does not already exist

cd apex-ui && bun install && cd ..
cd apex-python && python3 -m pip install -e ".[dev]" && cd ..

# Desktop app (starts Vite on :3000 and launches the native window)
bun run dev

# Browser-only UI (used by Playwright and mock-IPC UI development)
bun run dev:web-only

# Full verification wrapper from the repo root
bun run verify
```

### Verified local workflows (March 2026)

The following paths were exercised successfully in this repository state:

```bash
# Rust workspace tests
cargo test --workspace

# Python sidecar / SDK tests
cd apex-python && python3 -m pytest tests

# Frontend static verification
cd apex-ui && bun run lint && bun run build

# Browser-mode end-to-end tests (mocked IPC + polling)
cd apex-ui && bun run test

# Root wrapper covering DB setup + cargo check + UI lint/build + Python tests
bun run verify
```

Observed results from this session:

- `cargo test --workspace` → **194 passed**
- `apex-python` pytest suite → **21 passed**
- Playwright suite → **67 passed**
- `bun run verify` → **passed**
- Desktop smoke → native app launched successfully through both `scripts/dev.sh --skip-db` and `cd apex-ui && bun run tauri:dev`

### Development modes

- **Browser mode** (`bun run dev:web-only` or Playwright’s built-in dev server on port `1420`) uses mocked Tauri IPC plus polling so the UI can be exercised without the native shell.
- **Desktop mode** (`bun run dev` or `cd apex-ui && bun run tauri:dev`) launches Vite and then starts the real `apex-tauri` binary.
- The local SQLite path used by the desktop app is `data/apex.db`, which matches the setup script and current runtime wiring.

### Configuration (`config/apex.toml`)

```toml
[database]
timescale_url = "postgresql://apex:apex@localhost:5432/apex_market"
duckdb_path   = "data/analytics.duckdb"
redis_url     = "redis://127.0.0.1:6379"
sqlite_path   = "data/apex.db"

[adapters.zerodha]
enabled    = false
# Keys stored in OS keychain, referenced by name here

[adapters.paper_trading]
enabled         = true
initial_capital = 1000000.0
currency        = "INR"

[risk]
max_order_value     = 500000
max_position_pct    = 0.20
max_daily_loss      = 50000
duplicate_window_ms = 500

[python_sidecar]
enabled = true
venv    = "apex-python/.venv"
socket  = "/tmp/apex_python.sock"
```

-----

## Appendix: Key References & Inspirations

|Project                           |What We Learn                                         |
|----------------------------------|------------------------------------------------------|
|**NautilusTrader**                |Event-driven architecture, Rust/Python bridge via PyO3|
|**Backtrader**                    |Event-driven backtest engine design                   |
|**hftbacktest**                   |Realistic fill simulation, queue-position modelling   |
|**QuantLib-Rust**                 |Financial instrument modelling in Rust                |
|**TradingView Lightweight Charts**|Sub-16ms chart rendering techniques                   |
|**LMAX Disruptor**                |Lock-free ring buffer design for hot path             |
|**TimescaleDB**                   |Hypertable + continuous aggregate design patterns     |
|**petgraph**                      |Graph data structure for relationship engine          |

-----

*APEX — Built for traders who think in microseconds and act in conviction.*

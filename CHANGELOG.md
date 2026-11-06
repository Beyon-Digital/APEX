# Changelog

All notable changes to this project will be documented in this file.

The format is based on "Keep a Changelog" and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added (algo trading, copilot, ChatGPT sign-in, UX)
- **Sign in with ChatGPT**: the Copilot can run on a ChatGPT Plus/Pro/Go plan allowance using OpenAI's documented open-source flow (OAuth + PKCE with dynamic client registration, loopback callback, verified ID token, rotating refresh tokens kept encrypted in the vault). Plan-backed requests use the Responses endpoint with the documented constraints and map OpenAI's usage-limit errors to plain messages.
- **Strategies can trade live**: sessions take a broker, size and duration; live runs need the config opt-in, a connected account and armed live trading, and every order passes the live guard. Indicators are warmed up on history, logs stream as they happen, and session state (position, bars, orders, errors) is visible. Positions are tracked per broker.
- **Safer automations**: fresh daily data with a staleness check, long/flat position awareness (no pyramiding, no shorting), quote subscription for paper orders, connected-broker precheck, startup grace, and the kill switch pauses them.
- **Copilot**: new tools (status, brokers, balances, holdings, symbol search, watchlist, strategy sessions start/stop, modify order, kill switch), a richer system prompt, approval cards that show side/quantity/price/broker, single-use per-turn approvals, and no silent file overwrite.
- **Per-ticker data feed**: a SRC badge in the Watchlist shows which feed serves each ticker and lets you pin another; unavailable feeds say why.
- **Settings** is now five short tabs (Accounts, Market data, AI Copilot, Trading, General) with collapsible adapter rows; dead controls removed. Ticker fields in the order ticket, alerts, backtests and automations suggest symbols as you type.

### Added
- **Encrypted credential vault**: broker and AI API keys are entered in the app (Settings > Adapters & API keys) and stored in `vault.db`, encrypted with a passphrase-derived key (Argon2id + XChaCha20-Poly1305). Adapters are built from the vault and hot-added, replaced or removed at runtime with no restart; locking the vault drops every in-memory credential. A one-time "Import .env" command migrates old env files. Env-var credentials are no longer read.
- **Live Zerodha Kite**: form-encoded orders with correct SL/SL-M mapping, product/exchange/variety, order history and status mapping, positions, holdings, margins and P&L; instruments master; KiteTicker WebSocket (binary packets, order postbacks) with batched REST fallback; token-based historical candles; rate limiting, retries and `TokenException` handling; daily login flow with token persisted until 06:00 IST.
- **Live-trading safeguards**: explicit arm toggle (disarms on restart), single-use order confirmation bound to the exact order, per-day order and per-order quantity caps (`[live]` config), kill switch (Ctrl+Shift+K) that halts trading and cancels working live orders. Automations and the AI copilot go through the same guard.
- **Live order sync**: broker-pushed and polled order updates drive fills, positions and live realized P&L into the risk engine; live market orders are valued from the quote cache and rejected without a quote.
- **Terminal UI**: amber-on-black terminal theme (new default, dense), tiled draggable multi-panel workspace with Trading/Research/Monitor presets, symbol search with autocomplete (broker instruments + Yahoo), command-bar `ADD`/`RM`, history, suggestions and Ctrl+K.

### Changed
- Watchlist symbols added after launch are now subscribed (and removed ones released); index, FX and futures-style symbols (`^GSPC`, `EURUSD=X`, `GC=F`) pass validation.
- When several feeds stream the same symbol, the higher-priority (broker) feed wins over delayed Yahoo data.

- **AI Copilot**: OpenRouter-backed assistant panel with live terminal context (positions, watchlist quotes, session P&L injected into the system prompt). `copilot_chat` IPC command, `OPEN_ROUTER` / `OPENROUTER_API_KEY` env var, `[copilot]` config section, default model `openrouter/free`
- **News Panel**: Center-tab news feed backed by the previously dormant NewsEngine — RSS polling wired into `AppState`, `news-item` Tauri event stream, `get_news` / `search_news` / `list_news_feeds` commands, symbol filter + keyword search UI
- **Order Blotter**: `get_orders` command merging persisted order history with in-memory open orders; blotter tab with cancel support for working orders
- **Order Book Panel**: `get_order_book` command — real Binance L2 depth for crypto pairs, deterministic synthetic book estimated from cached quotes for equities; OrderBookHeatmap wired live
- **Correlation Graph Panel**: `compute_correlations` command — daily-return Pearson correlations across the watchlist upserted into the GraphEngine; VectorGraph force layout rendered live with coefficient edge labels; `get_graph` snapshot command
- **Market Scanner Panel**: `run_scan` command over watchlist or ad-hoc universes — price/volume/%change/RSI/SMA criteria builder UI with results table and chart navigation
- **Alert Engine Live Evaluation**: `AlertEngine::start` subscribes to the quote wildcard topic and evaluates rules continuously; PositionUpdate-driven `evaluate_pnl` task for DailyPnl rules; `alert-fired` events surface in the news panel
- **Message Bus Wildcard Fan-out**: Parameterized topics (`Quote("AAPL")`) now also deliver to `Topic::*("*")` wildcard subscribers — fixes real-time quote/order/signal push events never reaching the frontend
- **Config Sections**: `[news]` (feeds, poll interval) and `[copilot]` (model, base URL, max tokens) in `config/apex.toml`
- **Market Overview Tab**: Index cards (S&P, NASDAQ, DOW, NIFTY, BTC, ETH, gold, WTI, EURUSD, USDINR), watchlist breadth meter, top gainers/losers, and latest headlines — a Bloomberg MOST-page equivalent
- **Ticker Tape**: Scrolling marquee of major index quotes under the command bar (pauses on hover)
- **Keyboard HUD**: `?` key overlay listing shortcuts and command-bar syntax
- **Watchlist Upgrades**: Price flash on tick changes (green/red), sortable Symbol/Last/Chg% columns
- **Status Bar**: Feed liveness indicator (LIVE/STALE by last quote arrival) and subscribed-symbol count
- **Chart Timeframes**: 5m/15m/1H/1D selector on the candle chart with live ticks bucketed into the active timeframe
- **Quant Analytics Tab**: OpenBB-style analytics — `compute_indicator` (10 indicators, chart overlays + synced oscillator pane), `get_quant_stats` (returns/skew/kurtosis/Jarque-Bera/Sharpe/Sortino/Omega/max-DD/ACF + rolling vol & Sharpe), `get_regression` (date-aligned cross-market OLS with scatter/fit/residuals); `apex-core::application::quant` pure-Rust stats module
- **Light/Dark Theme + Compact Density**: `[appearance]` config section, full light palette + compact spacing/density overrides in `tokens.css`, live-applied via `html[data-theme]`/`data-density]`, persisted through settings
- **Backtest Tab**: dedicated UX — strategy file picker, params (symbol/timeframe/date range/quantity/capital), equity-curve + drawdown charts, metrics grid, full trade log; `:BACKTEST` command
- **Agentic Copilot**: tool-call loop (≤10 rounds) over live terminal state — quotes, OHLCV, quant stats, regression, scans, news, strategy read/write, backtests, CSV export, ML model list/train — so the model can build→test→optimise iteratively; tool trace rendered inline in the chat
- **LLM Provider Registry**: `[[llm.providers]]` config + Settings CRUD — OpenAI-compatible endpoints with `api_kind` chat (`/chat/completions`), responses (`/responses`), or acp; keys referenced by env-var name only; active-provider picker with key-configured badges
- **ACP Connector**: newline JSON-RPC stdio bridge to external agents/IDEs (initialize → session/new → session/prompt, streamed `agent_message_chunk` collection); `[acp]` command config, `api_kind: acp` routes copilot to it
- **Zerodha Kite Login**: `zerodha_login` command exchanges the Kite `request_token` for a daily `access_token` (sha256 `api_key+request_token+api_secret` checksum) and installs it on both Zerodha adapters; Settings broker card gets a Kite login row

### Fixed
- **Blank Candle Chart**: `CandleChart` never fetched OHLCV data and `get_historical_data` only read sqlite — the backend now falls back to the market data adapter on a storage miss and persists fetched bars
- **Silent Mock Mode in Desktop App**: `tauri.conf.json` lacked `withGlobalTauri`, so `window.__TAURI__` was never injected and the desktop build silently ran browser-mode mock data (every symbol showed 150.20). CSP `connect-src` now also allows `ipc:` / `ipc.localhost` for Tauri IPC
- **Settings save panic**: `ensure_table` used immutable `doc[section]` indexing which panics on missing sections — probes with `doc.get()` first so `[appearance]`/`[llm]`/`[acp]` are created on first save
- `PolymarketAdapter::fetch_markets` and `PolymarketMarket` made public (test compile error on `main`)
- Live-network crypto adapter integration tests marked `#[ignore]` — they hit real exchange APIs and broke offline `cargo test`
- **VADER-Style NLP Sentiment Pipeline**: Financial-domain lexicon-based sentiment analysis with 80+ terms, negation handling, degree modifiers, capitalisation boost, and VADER-normalised compound scoring (−1.0 to +1.0)
- **IPC Input Validation**: Comprehensive security validation for all 20 Tauri IPC commands — symbol format, quantity bounds, price validation, broker ID whitelist, algorithm whitelist, path traversal prevention, JSON payload size limits
- **Structured JSON Trace Exporter**: Optional NDJSON trace output (`APEX_JSON_TRACE=1`) with dual-layer tracing (console + file), suitable for Jaeger/Grafana/Datadog ingestion
- **Application Icon**: Source SVG with lightning bolt + candlestick chart aesthetic, plus `scripts/generate_icons.sh` for automated multi-size PNG/ICO/ICNS generation
- **CommandBar Execution Wiring**: Full command execution — order placement (BUY/SELL), symbol navigation, panel switching (:ML, :HEALTH, :STRATEGY, :CHART) — with success/error feedback display
- **Tauri Event Bridge Hooks**: `useQuoteStream`, `useOrderStream`, `usePositionStream`, `useHealthStream` real-time event hooks with automatic Tauri/browser fallback
- **Hot Path Profiling**: `#[tracing::instrument]` spans on `MarketDataAggregator::start`, `OrderTradeManager::submit_order/cancel_order/handle_fill/reconcile_positions`, `RiskEngine::check` with selective field recording
- **Database Migrations**: 3 SQL migration files — `001_core_tables.sql` (orders, trades, ohlcv), `002_application_tables.sql` (alert_rules, strategy_runs, ml_models), `003_timescaledb_extensions.sql` (hypertable, compression, retention)
- **Production Bundle Config**: Tauri bundle updated with Python sidecar resources, migration files, and Python3 system dependency for Linux
- **Playwright E2E Tests**: 9 new command bar tests (65 total, all passing)
- **ML Workbench UI**: Full training dashboard with algorithm selection (Random Forest, Gradient Boosting, Logistic Regression, XGBoost), feature selection chips, CV split configuration, lag period settings, and model registry with metrics display
- **ML Zustand Store**: Centralized state management for ML models, training status, and error handling
- **Health Monitor UI**: System health dashboard with adapter status indicators, uptime/memory/subscription metrics, and real-time health polling (5s intervals)
- **Health Zustand Store**: Centralized state management for system health data
- **Walk-Forward Backtest Engine**: Rolling train/test window validation with configurable n_windows and train_pct, overfitting ratio calculation, and aggregate test metrics
- **WalkForwardConfig/WalkForwardResult**: Full configuration and result types for walk-forward analysis
- **BacktestMetrics Default**: Default implementation for zero-initialized backtest metrics
- **Tauri IPC Commands**: `list_ml_models`, `train_ml_model`, `delete_ml_model`, `get_system_health` (4 new commands, 20 total)
- **ML DTOs**: MLModelDto, MLTrainingRequestDto, MLTrainingResultDto for frontend–backend data transfer
- **Health DTOs**: AdapterHealthDto, SystemHealthDto for health monitoring data transfer
- **ModelRegistry State**: In-memory ML model registry managed by Tauri for IPC access
- **Workspace Tabs**: Added ML Workbench and Health Monitor as new center-column tabs alongside Chart and Strategy IDE
- **Playwright Tests**: 29 new E2E tests — 13 ML Workbench + 7 Health Monitor + 9 CommandBar (65 total)
- **Rust Tests**: 30 new tests — 9 sentiment + 11 validation + 6 walk-forward + 1 tracing + 3 DTO (184 total, all passing)
- **Tauri IPC Commands**: `get_ohlcv`, `get_account_balance`, `modify_order`, `get_alert_rules`, `get_historical_data`, `get_watchlist_symbols` (6 prior new commands)
- **Real-time Event Push**: Message bus → Tauri emitter for 7 event types (quotes, orders, positions, news, alerts, strategy signals, adapter health)
- **Paper Trading Registration**: Paper trading adapter auto-registered as default execution adapter on startup
- **Data Command Module**: New `apex-tauri/src/commands/data.rs` for historical data and watchlist queries
- **Strategy IDE Tab**: Added Chart/Strategy IDE tab switching to workspace layout
- **UI Test IDs**: Added `data-testid` attributes to OrderEntry, PositionsPanel, and StrategyIDE components for Playwright testing
- **StrategyIDE Enhancements**: File creation dialog with name input, save button with confirmation, pipeline status indicator
- **Angel One Adapters**: Full `ExecutionPort` + `MarketDataPort` implementations against SmartAPI (`apiconnect.angelone.in`) with JWT auth, symbol/order mappings, 3s polling subscription
- **Groww Adapters**: Full `ExecutionPort` + `MarketDataPort` implementations against Groww REST API with order lifecycle, position/balance queries, polling subscription
- **Robinhood Adapters**: Full `ExecutionPort` + `MarketDataPort` implementations against Robinhood API with OAuth bearer auth, order lifecycle (cancel-replace pattern for modifications), polling subscription
- **Backtest Engine**: Event-driven bar replay across multiple symbols with merged timeline, configurable slippage/commission (bps), full metrics suite (Sharpe ratio, max drawdown, profit factor, win rate, equity curve, consecutive win/loss tracking)
- **Technical Indicator Library**: Pure Rust implementations of SMA, EMA, RSI, MACD, Bollinger Bands, ATR, VWAP, Stochastic Oscillator, Standard Deviation, Rate of Change — all with comprehensive unit tests
- **Market Scanner**: Real-time symbol screening engine with price/volume/indicator-based criteria (RSI, SMA crossover), AND logic across filters
- **Historical Data Downloader**: Bulk OHLCV data download from Yahoo Finance with CSV storage and reload capability, rate limiting, progress tracking
- **Crash Recovery**: `reconcile_on_startup()` called during Tauri app bootstrap
- **Position Reconciliation Loop**: 30s periodic position reconciliation across all registered brokers

### Changed
- **Workspace Layout**: Center column now supports tab switching between Chart+OrderEntry and StrategyIDE views
- **Playwright Tests**: Rewrote order-placement, ml-pipeline, and account-switching test suites to match actual UI (36 tests total, all passing)
- **Playwright Config**: Set `reuseExistingServer: true` for faster local test runs

### Fixed
- **apex-core Cargo.toml**: Added missing `reqwest` workspace dependency
- **apex-adapters/redis_state.rs**: Fixed `flush_all()` never-type fallback error with explicit `query_async::<()>()` type annotation
- **apex-adapters/timescale.rs**: Fixed `query_orders()` future Send safety by adding `Send` bound to `ToSql` trait objects
- **apex-tauri/commands/data.rs**: Added missing `Symbol` import
- **apex-tauri/commands/orders.rs**: Fixed unused variable warnings with underscore prefixes

## [0.1.0] - 2026-03-17

### Added
- Initial release.

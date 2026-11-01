use std::collections::HashMap;

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::domain::models::*;

/// Configuration for a backtest run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestConfig {
    /// Unique identifier for this backtest run
    pub run_id: String,
    /// Symbols to include in the backtest
    pub symbols: Vec<Symbol>,
    /// Start date of the backtest period
    pub start: DateTime<Utc>,
    /// End date of the backtest period
    pub end: DateTime<Utc>,
    /// Initial capital
    pub initial_capital: f64,
    /// Currency
    pub currency: String,
    /// Commission in basis points (e.g. 3 = 0.03%)
    pub commission_bps: f64,
    /// Slippage in basis points (e.g. 2 = 0.02%)
    pub slippage_bps: f64,
}

impl Default for BacktestConfig {
    fn default() -> Self {
        Self {
            run_id: Uuid::new_v4().to_string(),
            symbols: vec![],
            start: Utc::now(),
            end: Utc::now(),
            initial_capital: 1_000_000.0,
            currency: "INR".into(),
            commission_bps: 3.0,
            slippage_bps: 2.0,
        }
    }
}

/// Result of a backtest run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    pub run_id: String,
    pub config: BacktestConfig,
    pub metrics: BacktestMetrics,
    pub trades: Vec<BacktestTrade>,
    pub equity_curve: Vec<EquityPoint>,
}

/// Performance metrics from a backtest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestMetrics {
    pub total_return: f64,
    pub total_return_pct: f64,
    pub annualized_return_pct: f64,
    pub sharpe_ratio: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub total_trades: u64,
    pub winning_trades: u64,
    pub losing_trades: u64,
    pub win_rate: f64,
    pub profit_factor: f64,
    pub avg_trade_pnl: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub max_consecutive_wins: u32,
    pub max_consecutive_losses: u32,
    pub final_equity: f64,
}

/// A single trade in the backtest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestTrade {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub entry_time: DateTime<Utc>,
    pub entry_price: f64,
    pub exit_time: Option<DateTime<Utc>>,
    pub exit_price: Option<f64>,
    pub quantity: f64,
    pub pnl: f64,
    pub commission: f64,
}

/// A point on the equity curve
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquityPoint {
    pub time: DateTime<Utc>,
    pub equity: f64,
    pub drawdown: f64,
}

/// Internal position state during backtest
#[derive(Debug, Clone)]
pub struct SimPosition {
    symbol: Symbol,
    quantity: f64,
    avg_price: f64,
    entry_time: DateTime<Utc>,
    entry_commission: f64,
}

/// Backtest engine — replays historical OHLCV bars through a signal
/// generator and simulates order execution with slippage & commission.
pub struct BacktestEngine {
    config: BacktestConfig,
    positions: HashMap<String, SimPosition>,
    cash: f64,
    trades: Vec<BacktestTrade>,
    equity_curve: Vec<EquityPoint>,
    peak_equity: f64,
}

impl BacktestEngine {
    /// Create a new backtest engine with the given configuration.
    pub fn new(config: BacktestConfig) -> Self {
        let initial = config.initial_capital;
        Self {
            config,
            positions: HashMap::new(),
            cash: initial,
            trades: Vec::new(),
            equity_curve: Vec::new(),
            peak_equity: initial,
        }
    }

    /// Run a backtest over the provided OHLCV data and signals.
    ///
    /// `data` is a map of symbol → sorted OHLCV bars.
    /// `signal_fn` is a callback that receives the current bar for each
    /// symbol and returns an optional signal.
    pub fn run<F>(
        &mut self,
        data: &HashMap<String, Vec<OHLCV>>,
        mut signal_fn: F,
    ) -> Result<BacktestResult>
    where
        F: FnMut(&str, &OHLCV, &HashMap<String, SimPosition>, f64) -> Option<BacktestSignal>,
    {
        info!(
            run_id = %self.config.run_id,
            symbols = ?self.config.symbols.iter().map(|s| &s.0).collect::<Vec<_>>(),
            "Starting backtest"
        );

        // Build a merged timeline of all bars sorted by time
        let mut all_bars: Vec<(&str, &OHLCV)> = Vec::new();
        for (symbol, bars) in data {
            for bar in bars {
                all_bars.push((symbol.as_str(), bar));
            }
        }
        all_bars.sort_by_key(|(_, b)| b.time);

        if all_bars.is_empty() {
            return Err(anyhow!("No data provided for backtest"));
        }

        // Replay bars
        for (symbol, bar) in &all_bars {
            // Generate signal
            if let Some(signal) = signal_fn(symbol, bar, &self.positions, self.cash) {
                self.process_signal(signal, bar)?;
            }

            // Record equity curve point
            let equity = self.calculate_equity(bar);
            if equity > self.peak_equity {
                self.peak_equity = equity;
            }
            let drawdown = if self.peak_equity > 0.0 {
                (self.peak_equity - equity) / self.peak_equity
            } else {
                0.0
            };
            self.equity_curve.push(EquityPoint {
                time: bar.time,
                equity,
                drawdown,
            });
        }

        // Close any remaining open positions at last available price
        let last_bars: HashMap<String, &OHLCV> = data
            .iter()
            .filter_map(|(sym, bars)| bars.last().map(|b| (sym.clone(), b)))
            .collect();
        self.close_all_positions(&last_bars);
        // The terminal point includes liquidation costs, matching final_equity.
        if let Some(last) = self.equity_curve.last_mut() {
            last.equity = self.cash;
            last.drawdown = if self.peak_equity > 0.0 {
                (self.peak_equity - self.cash).max(0.0) / self.peak_equity
            } else {
                0.0
            };
        }

        // Calculate final metrics
        let metrics = self.calculate_metrics();

        let result = BacktestResult {
            run_id: self.config.run_id.clone(),
            config: self.config.clone(),
            metrics,
            trades: self.trades.clone(),
            equity_curve: self.equity_curve.clone(),
        };

        info!(
            run_id = %result.run_id,
            total_trades = result.metrics.total_trades,
            total_return_pct = format!("{:.2}%", result.metrics.total_return_pct),
            sharpe = format!("{:.3}", result.metrics.sharpe_ratio),
            max_dd = format!("{:.2}%", result.metrics.max_drawdown_pct),
            "Backtest complete"
        );

        Ok(result)
    }

    fn process_signal(&mut self, signal: BacktestSignal, bar: &OHLCV) -> Result<()> {
        match signal {
            BacktestSignal::Buy { symbol, quantity } => {
                let fill_price = bar.close * (1.0 + self.config.slippage_bps / 10_000.0);
                let commission = fill_price * quantity * self.config.commission_bps / 10_000.0;
                let cost = fill_price * quantity + commission;

                if cost > self.cash {
                    debug!(
                        symbol = %symbol.0,
                        cost = cost,
                        cash = self.cash,
                        "Insufficient cash for buy order"
                    );
                    return Ok(());
                }

                self.cash -= cost;

                // Merge with existing position or create new
                if let Some(pos) = self.positions.get_mut(&symbol.0) {
                    let new_qty = pos.quantity + quantity;
                    pos.avg_price =
                        (pos.avg_price * pos.quantity + fill_price * quantity) / new_qty;
                    pos.quantity = new_qty;
                    pos.entry_commission += commission;
                } else {
                    self.positions.insert(
                        symbol.0.clone(),
                        SimPosition {
                            symbol: symbol.clone(),
                            quantity,
                            avg_price: fill_price,
                            entry_time: bar.time,
                            entry_commission: commission,
                        },
                    );
                }

                self.trades.push(BacktestTrade {
                    symbol,
                    side: OrderSide::Buy,
                    entry_time: bar.time,
                    entry_price: fill_price,
                    exit_time: None,
                    exit_price: None,
                    quantity,
                    pnl: 0.0,
                    commission,
                });
            }
            BacktestSignal::Sell { symbol, quantity } => {
                let fill_price = bar.close * (1.0 - self.config.slippage_bps / 10_000.0);
                if let Some(pos) = self.positions.get_mut(&symbol.0) {
                    let sell_qty = quantity.min(pos.quantity);
                    let commission = fill_price * sell_qty * self.config.commission_bps / 10_000.0;
                    let entry_fee = pos.entry_commission * sell_qty / pos.quantity;
                    pos.entry_commission -= entry_fee;
                    let pnl = (fill_price - pos.avg_price) * sell_qty - commission - entry_fee;

                    self.cash += fill_price * sell_qty - commission;
                    pos.quantity -= sell_qty;

                    self.trades.push(BacktestTrade {
                        symbol: symbol.clone(),
                        side: OrderSide::Sell,
                        entry_time: pos.entry_time,
                        entry_price: pos.avg_price,
                        exit_time: Some(bar.time),
                        exit_price: Some(fill_price),
                        quantity: sell_qty,
                        pnl,
                        commission,
                    });

                    if pos.quantity <= 0.001 {
                        self.positions.remove(&symbol.0);
                    }
                } else {
                    debug!(symbol = %symbol.0, "No position to sell");
                }
            }
            BacktestSignal::Close { symbol } => {
                if let Some(pos) = self.positions.remove(&symbol.0) {
                    let fill_price = bar.close * (1.0 - self.config.slippage_bps / 10_000.0);
                    let commission =
                        fill_price * pos.quantity * self.config.commission_bps / 10_000.0;
                    let pnl = (fill_price - pos.avg_price) * pos.quantity
                        - commission
                        - pos.entry_commission;
                    self.cash += fill_price * pos.quantity - commission;

                    self.trades.push(BacktestTrade {
                        symbol,
                        side: OrderSide::Sell,
                        entry_time: pos.entry_time,
                        entry_price: pos.avg_price,
                        exit_time: Some(bar.time),
                        exit_price: Some(fill_price),
                        quantity: pos.quantity,
                        pnl,
                        commission,
                    });
                }
            }
        }
        Ok(())
    }

    fn close_all_positions(&mut self, last_bars: &HashMap<String, &OHLCV>) {
        let symbols: Vec<String> = self.positions.keys().cloned().collect();
        for symbol in symbols {
            if let Some(bar) = last_bars.get(&symbol) {
                if let Some(pos) = self.positions.remove(&symbol) {
                    let fill_price = bar.close * (1.0 - self.config.slippage_bps / 10_000.0);
                    let commission =
                        fill_price * pos.quantity * self.config.commission_bps / 10_000.0;
                    let pnl = (fill_price - pos.avg_price) * pos.quantity
                        - commission
                        - pos.entry_commission;
                    self.cash += fill_price * pos.quantity - commission;

                    self.trades.push(BacktestTrade {
                        symbol: pos.symbol,
                        side: OrderSide::Sell,
                        entry_time: pos.entry_time,
                        entry_price: pos.avg_price,
                        exit_time: Some(bar.time),
                        exit_price: Some(fill_price),
                        quantity: pos.quantity,
                        pnl,
                        commission,
                    });
                }
            } else {
                warn!(symbol = %symbol, "No last bar for open position at end of backtest");
            }
        }
    }

    fn calculate_equity(&self, current_bar: &OHLCV) -> f64 {
        let positions_value: f64 = self
            .positions
            .values()
            .map(|pos| {
                if pos.symbol.0 == current_bar.symbol.0 {
                    pos.quantity * current_bar.close
                } else {
                    pos.quantity * pos.avg_price // fallback
                }
            })
            .sum();
        self.cash + positions_value
    }

    fn calculate_metrics(&self) -> BacktestMetrics {
        let initial = self.config.initial_capital;
        let final_equity = self.cash;
        let total_return = final_equity - initial;
        let total_return_pct = if initial > 0.0 {
            (total_return / initial) * 100.0
        } else {
            0.0
        };

        // Count wins/losses from closed trades
        let closed: Vec<&BacktestTrade> = self
            .trades
            .iter()
            .filter(|t| t.exit_time.is_some())
            .collect();
        let total_trades = closed.len() as u64;
        let winning: Vec<&&BacktestTrade> = closed.iter().filter(|t| t.pnl > 0.0).collect();
        let losing: Vec<&&BacktestTrade> = closed.iter().filter(|t| t.pnl <= 0.0).collect();
        let winning_trades = winning.len() as u64;
        let losing_trades = losing.len() as u64;

        let win_rate = if total_trades > 0 {
            (winning_trades as f64 / total_trades as f64) * 100.0
        } else {
            0.0
        };

        let total_wins: f64 = winning.iter().map(|t| t.pnl).sum();
        let total_losses: f64 = losing.iter().map(|t| t.pnl.abs()).sum();

        let profit_factor = if total_losses > 0.0 {
            total_wins / total_losses
        } else if total_wins > 0.0 {
            f64::INFINITY
        } else {
            0.0
        };

        let avg_trade_pnl = if total_trades > 0 {
            closed.iter().map(|t| t.pnl).sum::<f64>() / total_trades as f64
        } else {
            0.0
        };

        let avg_win = if winning_trades > 0 {
            total_wins / winning_trades as f64
        } else {
            0.0
        };

        let avg_loss = if losing_trades > 0 {
            total_losses / losing_trades as f64
        } else {
            0.0
        };

        // Consecutive wins/losses
        let (max_cons_wins, max_cons_losses) = Self::max_consecutive(&closed);

        // Sharpe ratio from equity curve
        let sharpe = self.calculate_sharpe();

        // Max drawdown
        let (max_dd, max_dd_pct) = self.max_drawdown();

        // Annualized return
        let days = if !self.equity_curve.is_empty() {
            let first = self.equity_curve.first().unwrap().time;
            let last = self.equity_curve.last().unwrap().time;
            (last - first).num_days().max(1) as f64
        } else {
            365.0
        };
        let years = days / 365.0;
        let annualized = if years > 0.0 {
            ((final_equity / initial).powf(1.0 / years) - 1.0) * 100.0
        } else {
            total_return_pct
        };

        BacktestMetrics {
            total_return,
            total_return_pct,
            annualized_return_pct: annualized,
            sharpe_ratio: sharpe,
            max_drawdown: max_dd,
            max_drawdown_pct: max_dd_pct,
            total_trades,
            winning_trades,
            losing_trades,
            win_rate,
            profit_factor,
            avg_trade_pnl,
            avg_win,
            avg_loss,
            max_consecutive_wins: max_cons_wins,
            max_consecutive_losses: max_cons_losses,
            final_equity,
        }
    }

    fn calculate_sharpe(&self) -> f64 {
        if self.equity_curve.len() < 2 {
            return 0.0;
        }

        let returns: Vec<f64> = self
            .equity_curve
            .windows(2)
            .map(|w| {
                if w[0].equity > 0.0 {
                    (w[1].equity - w[0].equity) / w[0].equity
                } else {
                    0.0
                }
            })
            .collect();

        if returns.is_empty() {
            return 0.0;
        }

        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance =
            returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        let std_dev = variance.sqrt();

        if std_dev > 0.0 {
            // Annualize: assume 252 trading days
            (mean / std_dev) * (252.0_f64).sqrt()
        } else {
            0.0
        }
    }

    fn max_drawdown(&self) -> (f64, f64) {
        let mut peak = self.config.initial_capital;
        let mut max_dd = 0.0_f64;
        let mut max_dd_pct = 0.0_f64;

        for pt in &self.equity_curve {
            if pt.equity > peak {
                peak = pt.equity;
            }
            let dd = peak - pt.equity;
            let dd_pct = if peak > 0.0 { (dd / peak) * 100.0 } else { 0.0 };
            max_dd = max_dd.max(dd);
            max_dd_pct = max_dd_pct.max(dd_pct);
        }

        (max_dd, max_dd_pct)
    }

    fn max_consecutive(trades: &[&BacktestTrade]) -> (u32, u32) {
        let mut max_wins = 0_u32;
        let mut max_losses = 0_u32;
        let mut curr_wins = 0_u32;
        let mut curr_losses = 0_u32;

        for trade in trades {
            if trade.pnl > 0.0 {
                curr_wins += 1;
                curr_losses = 0;
                max_wins = max_wins.max(curr_wins);
            } else {
                curr_losses += 1;
                curr_wins = 0;
                max_losses = max_losses.max(curr_losses);
            }
        }

        (max_wins, max_losses)
    }
}

/// Signal type for backtest signal generation
#[derive(Debug, Clone)]
pub enum BacktestSignal {
    Buy { symbol: Symbol, quantity: f64 },
    Sell { symbol: Symbol, quantity: f64 },
    Close { symbol: Symbol },
}

// ---------------------------------------------------------------------------
// Walk-Forward Backtest
// ---------------------------------------------------------------------------

/// Configuration for a walk-forward backtest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardConfig {
    /// Base backtest configuration
    pub base_config: BacktestConfig,
    /// Number of walk-forward windows
    pub n_windows: usize,
    /// Fraction of each window used for training (in-sample)
    pub train_pct: f64,
}

impl Default for WalkForwardConfig {
    fn default() -> Self {
        Self {
            base_config: BacktestConfig::default(),
            n_windows: 5,
            train_pct: 0.7,
        }
    }
}

/// Result of a single walk-forward window
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardWindow {
    pub window_index: usize,
    pub train_start: DateTime<Utc>,
    pub train_end: DateTime<Utc>,
    pub test_start: DateTime<Utc>,
    pub test_end: DateTime<Utc>,
    pub train_metrics: BacktestMetrics,
    pub test_metrics: BacktestMetrics,
}

/// Result of a full walk-forward backtest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkForwardResult {
    pub config: WalkForwardConfig,
    pub windows: Vec<WalkForwardWindow>,
    pub aggregate_metrics: BacktestMetrics,
    pub overfitting_ratio: f64,
}

/// Walk-forward backtest engine — splits data into rolling train/test windows
/// and runs the backtest on each, aggregating results to detect overfitting.
pub struct WalkForwardEngine;

impl WalkForwardEngine {
    /// Run a walk-forward backtest.
    ///
    /// `data` is a map of symbol → sorted OHLCV bars.
    /// `signal_fn` receives bars, positions, and cash and returns optional
    /// signals — identical to `BacktestEngine::run`.
    pub fn run<F>(
        config: &WalkForwardConfig,
        data: &HashMap<String, Vec<OHLCV>>,
        signal_fn: F,
    ) -> Result<WalkForwardResult>
    where
        F: Fn(&str, &OHLCV, &HashMap<String, SimPosition>, f64) -> Option<BacktestSignal> + Clone,
    {
        if config.n_windows == 0 {
            return Err(anyhow!("walk-forward n_windows must be > 0"));
        }
        if !(0.1..=0.9).contains(&config.train_pct) {
            return Err(anyhow!(
                "walk-forward train_pct must be between 0.1 and 0.9"
            ));
        }

        // Determine overall time range from data
        let (global_start, global_end) = Self::time_range(data)?;
        let total_secs = (global_end - global_start).num_seconds() as f64;
        let window_secs = total_secs / config.n_windows as f64;

        info!(
            n_windows = config.n_windows,
            train_pct = config.train_pct,
            total_days = total_secs / 86400.0,
            "Starting walk-forward backtest"
        );

        let mut windows: Vec<WalkForwardWindow> = Vec::with_capacity(config.n_windows);

        for i in 0..config.n_windows {
            let win_start =
                global_start + chrono::Duration::seconds((i as f64 * window_secs) as i64);
            let win_end = if i == config.n_windows - 1 {
                global_end
            } else {
                global_start + chrono::Duration::seconds(((i + 1) as f64 * window_secs) as i64)
            };

            let split = win_start
                + chrono::Duration::seconds(
                    ((win_end - win_start).num_seconds() as f64 * config.train_pct) as i64,
                );

            // Slice data for train and test windows
            let train_data = Self::slice_data(data, win_start, split);
            let test_data = Self::slice_data(data, split, win_end);

            // Run train window
            let train_cfg = BacktestConfig {
                run_id: format!("{}_train_{}", config.base_config.run_id, i),
                start: win_start,
                end: split,
                ..config.base_config.clone()
            };
            let mut train_engine = BacktestEngine::new(train_cfg);
            let train_result = train_engine.run(&train_data, signal_fn.clone());
            let train_metrics = match train_result {
                Ok(r) => r.metrics,
                Err(_) => BacktestMetrics::default(),
            };

            // Run test window
            let test_cfg = BacktestConfig {
                run_id: format!("{}_test_{}", config.base_config.run_id, i),
                start: split,
                end: win_end,
                ..config.base_config.clone()
            };
            let mut test_engine = BacktestEngine::new(test_cfg);
            let test_result = test_engine.run(&test_data, signal_fn.clone());
            let test_metrics = match test_result {
                Ok(r) => r.metrics,
                Err(_) => BacktestMetrics::default(),
            };

            windows.push(WalkForwardWindow {
                window_index: i,
                train_start: win_start,
                train_end: split,
                test_start: split,
                test_end: win_end,
                train_metrics,
                test_metrics,
            });
        }

        // Calculate aggregate test metrics
        let aggregate_metrics = Self::aggregate_metrics(&windows);

        // Overfitting ratio = avg train return / avg test return
        // A ratio near 1.0 indicates no overfitting; significantly above 1.0 indicates
        // the strategy performs much better in-sample than out-of-sample.
        // We use absolute values so the ratio is always positive.
        let avg_train_return: f64 = windows
            .iter()
            .map(|w| w.train_metrics.total_return_pct)
            .sum::<f64>()
            / windows.len() as f64;
        let avg_test_return: f64 = windows
            .iter()
            .map(|w| w.test_metrics.total_return_pct)
            .sum::<f64>()
            / windows.len() as f64;
        let overfitting_ratio = if avg_test_return.abs() > 0.001 {
            (avg_train_return / avg_test_return).abs()
        } else if avg_train_return.abs() > 0.001 {
            f64::INFINITY
        } else {
            1.0
        };

        info!(
            windows = windows.len(),
            overfitting_ratio = format!("{:.2}", overfitting_ratio),
            avg_test_return_pct = format!("{:.2}%", avg_test_return),
            "Walk-forward backtest complete"
        );

        Ok(WalkForwardResult {
            config: config.clone(),
            windows,
            aggregate_metrics,
            overfitting_ratio,
        })
    }

    /// Determine overall time range from all bars across all symbols.
    fn time_range(data: &HashMap<String, Vec<OHLCV>>) -> Result<(DateTime<Utc>, DateTime<Utc>)> {
        let mut min_time: Option<DateTime<Utc>> = None;
        let mut max_time: Option<DateTime<Utc>> = None;

        for bars in data.values() {
            for bar in bars {
                min_time = Some(match min_time {
                    Some(t) if t < bar.time => t,
                    _ => bar.time,
                });
                max_time = Some(match max_time {
                    Some(t) if t > bar.time => t,
                    _ => bar.time,
                });
            }
        }

        match (min_time, max_time) {
            (Some(start), Some(end)) => Ok((start, end)),
            _ => Err(anyhow!("No data to determine time range")),
        }
    }

    /// Slice data for a given time window [start, end).
    fn slice_data(
        data: &HashMap<String, Vec<OHLCV>>,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> HashMap<String, Vec<OHLCV>> {
        data.iter()
            .map(|(symbol, bars)| {
                let sliced: Vec<OHLCV> = bars
                    .iter()
                    .filter(|b| b.time >= start && b.time < end)
                    .cloned()
                    .collect();
                (symbol.clone(), sliced)
            })
            .filter(|(_, bars)| !bars.is_empty())
            .collect()
    }

    /// Aggregate test metrics across all walk-forward windows.
    fn aggregate_metrics(windows: &[WalkForwardWindow]) -> BacktestMetrics {
        if windows.is_empty() {
            return BacktestMetrics::default();
        }

        let n = windows.len() as f64;
        let test_metrics: Vec<&BacktestMetrics> = windows.iter().map(|w| &w.test_metrics).collect();

        BacktestMetrics {
            total_return: test_metrics.iter().map(|m| m.total_return).sum(),
            total_return_pct: test_metrics.iter().map(|m| m.total_return_pct).sum::<f64>() / n,
            annualized_return_pct: test_metrics
                .iter()
                .map(|m| m.annualized_return_pct)
                .sum::<f64>()
                / n,
            sharpe_ratio: test_metrics.iter().map(|m| m.sharpe_ratio).sum::<f64>() / n,
            max_drawdown: test_metrics
                .iter()
                .map(|m| m.max_drawdown)
                .fold(0.0_f64, f64::max),
            max_drawdown_pct: test_metrics
                .iter()
                .map(|m| m.max_drawdown_pct)
                .fold(0.0_f64, f64::max),
            total_trades: test_metrics.iter().map(|m| m.total_trades).sum(),
            winning_trades: test_metrics.iter().map(|m| m.winning_trades).sum(),
            losing_trades: test_metrics.iter().map(|m| m.losing_trades).sum(),
            win_rate: {
                let total: u64 = test_metrics.iter().map(|m| m.total_trades).sum();
                let wins: u64 = test_metrics.iter().map(|m| m.winning_trades).sum();
                if total > 0 {
                    (wins as f64 / total as f64) * 100.0
                } else {
                    0.0
                }
            },
            profit_factor: {
                let total_wins: f64 = test_metrics
                    .iter()
                    .map(|m| m.avg_win * m.winning_trades as f64)
                    .sum();
                let total_losses: f64 = test_metrics
                    .iter()
                    .map(|m| m.avg_loss * m.losing_trades as f64)
                    .sum();
                if total_losses > 0.0 {
                    total_wins / total_losses
                } else {
                    0.0
                }
            },
            avg_trade_pnl: test_metrics.iter().map(|m| m.avg_trade_pnl).sum::<f64>() / n,
            avg_win: test_metrics.iter().map(|m| m.avg_win).sum::<f64>() / n,
            avg_loss: test_metrics.iter().map(|m| m.avg_loss).sum::<f64>() / n,
            max_consecutive_wins: test_metrics
                .iter()
                .map(|m| m.max_consecutive_wins)
                .max()
                .unwrap_or(0),
            max_consecutive_losses: test_metrics
                .iter()
                .map(|m| m.max_consecutive_losses)
                .max()
                .unwrap_or(0),
            final_equity: test_metrics.iter().map(|m| m.final_equity).sum::<f64>() / n,
        }
    }
}

impl Default for BacktestMetrics {
    fn default() -> Self {
        Self {
            total_return: 0.0,
            total_return_pct: 0.0,
            annualized_return_pct: 0.0,
            sharpe_ratio: 0.0,
            max_drawdown: 0.0,
            max_drawdown_pct: 0.0,
            total_trades: 0,
            winning_trades: 0,
            losing_trades: 0,
            win_rate: 0.0,
            profit_factor: 0.0,
            avg_trade_pnl: 0.0,
            avg_win: 0.0,
            avg_loss: 0.0,
            max_consecutive_wins: 0,
            max_consecutive_losses: 0,
            final_equity: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn make_bars(symbol: &str, prices: &[(i64, f64)]) -> Vec<OHLCV> {
        prices
            .iter()
            .map(|(day, close)| {
                let time = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()
                    + chrono::Duration::days(*day);
                OHLCV {
                    time,
                    symbol: Symbol(symbol.into()),
                    timeframe: Timeframe::D1,
                    open: close - 1.0,
                    high: close + 2.0,
                    low: close - 2.0,
                    close: *close,
                    volume: 100_000,
                }
            })
            .collect()
    }

    #[test]
    fn terminal_equity_includes_liquidation_costs() {
        let mut engine = BacktestEngine::new(BacktestConfig {
            initial_capital: 1000.0,
            commission_bps: 10.0,
            slippage_bps: 10.0,
            ..BacktestConfig::default()
        });
        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_bars("AAPL", &[(0, 100.0), (1, 110.0)]));
        let result = engine
            .run(&data, |symbol, bar, _, _| {
                if bar.close == 100.0 {
                    Some(BacktestSignal::Buy {
                        symbol: Symbol(symbol.into()),
                        quantity: 1.0,
                    })
                } else {
                    None
                }
            })
            .unwrap();
        let expected = 1000.0 - 100.1 * 1.001 + 109.89 * 0.999;
        assert!((result.metrics.final_equity - expected).abs() < 1e-8);
        assert!(
            (result
                .trades
                .iter()
                .filter(|t| t.exit_time.is_some())
                .map(|t| t.pnl)
                .sum::<f64>()
                - result.metrics.total_return)
                .abs()
                < 1e-8
        );
        assert_eq!(
            result.equity_curve.last().unwrap().equity,
            result.metrics.final_equity
        );
    }

    #[test]
    fn test_backtest_no_data() {
        let config = BacktestConfig::default();
        let mut engine = BacktestEngine::new(config);
        let data = HashMap::new();
        let result = engine.run(&data, |_, _, _, _| None);
        assert!(result.is_err());
    }

    #[test]
    fn test_backtest_no_signals() {
        let config = BacktestConfig {
            initial_capital: 100_000.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert(
            "AAPL".into(),
            make_bars("AAPL", &[(0, 100.0), (1, 101.0), (2, 102.0)]),
        );

        let result = engine.run(&data, |_, _, _, _| None).unwrap();
        assert_eq!(result.metrics.total_trades, 0);
        assert!((result.metrics.final_equity - 100_000.0).abs() < 0.01);
    }

    #[test]
    fn test_backtest_buy_and_sell() {
        let config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert(
            "AAPL".into(),
            make_bars("AAPL", &[(0, 100.0), (1, 110.0), (2, 120.0)]),
        );

        let result = engine
            .run(&data, |symbol, bar, positions, _cash| {
                if bar.close == 100.0 {
                    // Buy on first bar
                    Some(BacktestSignal::Buy {
                        symbol: Symbol(symbol.into()),
                        quantity: 100.0,
                    })
                } else if bar.close == 120.0 && positions.contains_key(symbol) {
                    // Sell on third bar
                    Some(BacktestSignal::Sell {
                        symbol: Symbol(symbol.into()),
                        quantity: 100.0,
                    })
                } else {
                    None
                }
            })
            .unwrap();

        // Bought at 100, sold at 120, 100 shares = $2000 profit
        // 1 buy entry + 1 sell close = 2 trades total, 1 with exit_time (the sell)
        assert_eq!(result.metrics.total_trades, 1); // only closed trades count
        assert!(result.metrics.total_return > 0.0);
        assert_eq!(result.metrics.winning_trades, 1);
        assert!(result.metrics.win_rate > 0.0);
    }

    #[test]
    fn test_backtest_insufficient_cash() {
        let config = BacktestConfig {
            initial_capital: 500.0, // Very low capital
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_bars("AAPL", &[(0, 100.0), (1, 110.0)]));

        let result = engine
            .run(&data, |symbol, bar, _positions, _cash| {
                if bar.close == 100.0 {
                    // Try to buy 100 shares at $100 = $10,000 but only have $500
                    Some(BacktestSignal::Buy {
                        symbol: Symbol(symbol.into()),
                        quantity: 100.0,
                    })
                } else {
                    None
                }
            })
            .unwrap();

        // Order should be skipped due to insufficient cash
        assert_eq!(result.metrics.total_trades, 0);
        assert!((result.metrics.final_equity - 500.0).abs() < 0.01);
    }

    #[test]
    fn test_backtest_with_commission_and_slippage() {
        let config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 10.0, // 0.1%
            slippage_bps: 5.0,    // 0.05%
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_bars("AAPL", &[(0, 100.0), (1, 110.0)]));

        let result = engine
            .run(&data, |symbol, bar, positions, _cash| {
                if bar.close == 100.0 {
                    Some(BacktestSignal::Buy {
                        symbol: Symbol(symbol.into()),
                        quantity: 10.0,
                    })
                } else if positions.contains_key(symbol) {
                    Some(BacktestSignal::Close {
                        symbol: Symbol(symbol.into()),
                    })
                } else {
                    None
                }
            })
            .unwrap();

        // With commission and slippage, profit should be less than pure 10*10=$100
        assert!(result.metrics.final_equity < 100_100.0);
        assert!(result.metrics.final_equity > 100_000.0); // Should still be profitable
    }

    #[test]
    fn test_backtest_close_signal() {
        let config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_bars("AAPL", &[(0, 100.0), (1, 90.0)]));

        let result = engine
            .run(&data, |symbol, bar, positions, _cash| {
                if bar.close == 100.0 {
                    Some(BacktestSignal::Buy {
                        symbol: Symbol(symbol.into()),
                        quantity: 10.0,
                    })
                } else if positions.contains_key(symbol) {
                    Some(BacktestSignal::Close {
                        symbol: Symbol(symbol.into()),
                    })
                } else {
                    None
                }
            })
            .unwrap();

        // Bought at 100, closed at 90, 10 shares = -$100 loss
        assert!(result.metrics.total_return < 0.0);
        assert_eq!(result.metrics.losing_trades, 1);
    }

    #[test]
    fn test_backtest_equity_curve() {
        let config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert(
            "AAPL".into(),
            make_bars("AAPL", &[(0, 100.0), (1, 105.0), (2, 110.0)]),
        );

        let result = engine.run(&data, |_, _, _, _| None).unwrap();

        // Should have 3 equity points
        assert_eq!(result.equity_curve.len(), 3);
        // All should equal initial capital (no trades)
        for pt in &result.equity_curve {
            assert!((pt.equity - 100_000.0).abs() < 0.01);
        }
    }

    #[test]
    fn test_backtest_max_drawdown() {
        let config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        // Price goes up then down
        data.insert(
            "AAPL".into(),
            make_bars("AAPL", &[(0, 100.0), (1, 120.0), (2, 90.0), (3, 110.0)]),
        );

        let result = engine
            .run(&data, |symbol, bar, positions, _cash| {
                if bar.close == 100.0 {
                    Some(BacktestSignal::Buy {
                        symbol: Symbol(symbol.into()),
                        quantity: 100.0,
                    })
                } else if bar.close == 110.0 && positions.contains_key(symbol) {
                    Some(BacktestSignal::Close {
                        symbol: Symbol(symbol.into()),
                    })
                } else {
                    None
                }
            })
            .unwrap();

        // Should have recorded a drawdown when price went from 120 to 90
        assert!(result.metrics.max_drawdown > 0.0);
        assert!(result.metrics.max_drawdown_pct > 0.0);
    }

    #[test]
    fn test_backtest_consecutive_wins_losses() {
        let config = BacktestConfig {
            initial_capital: 1_000_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let mut engine = BacktestEngine::new(config);
        let mut data = HashMap::new();
        data.insert(
            "AAPL".into(),
            make_bars(
                "AAPL",
                &[
                    (0, 100.0),
                    (1, 110.0), // win
                    (2, 120.0),
                    (3, 130.0), // win
                    (4, 140.0),
                    (5, 130.0), // loss
                ],
            ),
        );

        let mut trade_num = 0;
        let result = engine
            .run(&data, |symbol, bar, positions, _cash| {
                if !positions.contains_key(symbol) && trade_num < 3 {
                    // Buy on even bars
                    if [100.0, 120.0, 140.0].contains(&bar.close) {
                        trade_num += 1;
                        return Some(BacktestSignal::Buy {
                            symbol: Symbol(symbol.into()),
                            quantity: 10.0,
                        });
                    }
                } else if positions.contains_key(symbol) {
                    // Sell on odd bars
                    if [110.0, 130.0, 130.0].contains(&bar.close) {
                        return Some(BacktestSignal::Close {
                            symbol: Symbol(symbol.into()),
                        });
                    }
                }
                None
            })
            .unwrap();

        // Should have some consecutive stats
        assert!(result.metrics.total_trades > 0);
    }

    #[test]
    fn test_backtest_config_default() {
        let config = BacktestConfig::default();
        assert_eq!(config.initial_capital, 1_000_000.0);
        assert_eq!(config.commission_bps, 3.0);
        assert_eq!(config.slippage_bps, 2.0);
        assert_eq!(config.currency, "INR");
    }

    // -----------------------------------------------------------------------
    // Walk-forward backtest tests
    // -----------------------------------------------------------------------

    fn make_long_series(symbol: &str, n_days: usize, base_price: f64) -> Vec<OHLCV> {
        (0..n_days)
            .map(|i| {
                let time = Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap()
                    + chrono::Duration::days(i as i64);
                // Simple oscillating price: base ± small variation
                let close = base_price + (i as f64 * 0.5).sin() * 5.0;
                OHLCV {
                    time,
                    symbol: Symbol(symbol.into()),
                    timeframe: Timeframe::D1,
                    open: close - 0.5,
                    high: close + 1.0,
                    low: close - 1.0,
                    close,
                    volume: 100_000,
                }
            })
            .collect()
    }

    #[test]
    fn test_walk_forward_basic() {
        let base_config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let wf_config = WalkForwardConfig {
            base_config,
            n_windows: 3,
            train_pct: 0.7,
        };

        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_long_series("AAPL", 90, 100.0));

        let result = WalkForwardEngine::run(&wf_config, &data, |_, _, _, _| None).unwrap();

        assert_eq!(result.windows.len(), 3);
        assert!(result.overfitting_ratio.is_finite() || result.overfitting_ratio == 1.0);
    }

    #[test]
    fn test_walk_forward_with_signals() {
        let base_config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let wf_config = WalkForwardConfig {
            base_config,
            n_windows: 2,
            train_pct: 0.6,
        };

        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_long_series("AAPL", 60, 100.0));

        let result = WalkForwardEngine::run(&wf_config, &data, |symbol, _bar, positions, cash| {
            if !positions.contains_key(symbol) && cash > 1000.0 {
                Some(BacktestSignal::Buy {
                    symbol: Symbol(symbol.into()),
                    quantity: 10.0,
                })
            } else if positions.contains_key(symbol) {
                Some(BacktestSignal::Close {
                    symbol: Symbol(symbol.into()),
                })
            } else {
                None
            }
        })
        .unwrap();

        assert_eq!(result.windows.len(), 2);
        // At least one window should have exercised some trading signals
        let total_trades: u64 = result
            .windows
            .iter()
            .map(|w| w.train_metrics.total_trades + w.test_metrics.total_trades)
            .sum();
        assert!(
            total_trades > 0,
            "walk-forward should produce at least one trade across all windows"
        );
    }

    #[test]
    fn test_walk_forward_invalid_config() {
        let base_config = BacktestConfig::default();

        // Zero windows
        let wf_config = WalkForwardConfig {
            base_config: base_config.clone(),
            n_windows: 0,
            train_pct: 0.7,
        };
        let data = HashMap::new();
        assert!(WalkForwardEngine::run(&wf_config, &data, |_, _, _, _| None).is_err());

        // Invalid train_pct
        let wf_config = WalkForwardConfig {
            base_config,
            n_windows: 3,
            train_pct: 0.95,
        };
        assert!(WalkForwardEngine::run(&wf_config, &data, |_, _, _, _| None).is_err());
    }

    #[test]
    fn test_walk_forward_overfitting_ratio() {
        let base_config = BacktestConfig {
            initial_capital: 100_000.0,
            commission_bps: 0.0,
            slippage_bps: 0.0,
            ..BacktestConfig::default()
        };
        let wf_config = WalkForwardConfig {
            base_config,
            n_windows: 2,
            train_pct: 0.7,
        };

        let mut data = HashMap::new();
        data.insert("AAPL".into(), make_long_series("AAPL", 60, 100.0));

        let result = WalkForwardEngine::run(&wf_config, &data, |_, _, _, _| None).unwrap();

        // With no signals, overfitting ratio should be ~1.0 (train and test are both ~0%)
        assert!(result.overfitting_ratio.is_finite());
    }

    #[test]
    fn test_walk_forward_default_config() {
        let config = WalkForwardConfig::default();
        assert_eq!(config.n_windows, 5);
        assert!((config.train_pct - 0.7).abs() < 0.001);
    }

    #[test]
    fn test_backtest_metrics_default() {
        let metrics = BacktestMetrics::default();
        assert_eq!(metrics.total_trades, 0);
        assert!((metrics.total_return - 0.0).abs() < 0.001);
        assert!((metrics.sharpe_ratio - 0.0).abs() < 0.001);
    }
}

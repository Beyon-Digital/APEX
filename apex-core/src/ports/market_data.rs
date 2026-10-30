use crate::domain::models::*;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Health status of an adapter
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum AdapterHealth {
    Healthy,
    Degraded(String),
    Unhealthy(String),
}

/// Stream of ticks from a market data adapter
pub type TickStream = mpsc::Receiver<Tick>;

/// Port trait for market data feeds
#[async_trait]
pub trait MarketDataPort: Send + Sync {
    /// Whether this feed can subscribe now (for example, after authentication).
    fn subscription_ready(&self) -> bool {
        true
    }
    /// Whether a symbol belongs to this feed's instrument namespace.
    fn supports_symbol(&self, _symbol: &Symbol) -> bool {
        true
    }
    /// Source priority when several feeds stream the same symbol: a higher value
    /// wins while it is delivering ticks. Free fallback feeds use 0.
    fn priority(&self) -> u8 {
        1
    }
    /// Subscribe to real-time tick data for the given symbols
    async fn subscribe(&self, symbols: &[Symbol]) -> Result<TickStream>;
    /// Unsubscribe from tick data for the given symbols
    async fn unsubscribe(&self, symbols: &[Symbol]) -> Result<()>;
    /// Get a snapshot quote for a symbol
    async fn get_snapshot(&self, symbol: &Symbol) -> Result<Quote>;
    /// Get historical OHLCV data
    async fn get_historical_ohlcv(
        &self,
        symbol: &Symbol,
        timeframe: Timeframe,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<OHLCV>>;
    /// Unique adapter identifier
    fn adapter_id(&self) -> &'static str;
    /// Current health status
    fn health(&self) -> AdapterHealth;
}

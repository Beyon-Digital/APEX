use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Canonical symbol representation (e.g. "RELIANCE", "AAPL", "BTC/USDT")
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct Symbol(pub String);

/// A single price tick from a market data feed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tick {
    pub time: DateTime<Utc>,
    pub symbol: Symbol,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    pub volume: u64,
    pub source: String,
    /// Day open when the feed provides it (REST-poll adapters like Yahoo).
    #[serde(default)]
    pub open: Option<f64>,
    /// Percent change vs previous close when the feed provides it.
    #[serde(default)]
    pub change_pct: Option<f64>,
}

/// A full quote snapshot for a symbol
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quote {
    pub symbol: Symbol,
    pub bid: f64,
    pub ask: f64,
    pub last: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: u64,
    pub change_pct: f64,
    pub vwap: f64,
    pub updated_at: DateTime<Utc>,
}

/// OHLCV bar (candlestick)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OHLCV {
    pub time: DateTime<Utc>,
    pub symbol: Symbol,
    #[serde(default = "default_ohlcv_timeframe")]
    pub timeframe: Timeframe,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: u64,
}

fn default_ohlcv_timeframe() -> Timeframe {
    Timeframe::D1
}

/// Order side
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    TrailingStop,
}

/// Order lifecycle status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum OrderStatus {
    Pending,
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// Unique order identifier
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct OrderId(pub String);

/// A full order record
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: OrderId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub status: OrderStatus,
    pub filled_qty: f64,
    pub avg_price: f64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub broker_id: String,
    pub source: String,
    /// Human-readable cause when an order is rejected.
    #[serde(default)]
    pub rejection_reason: Option<String>,
}

/// A trading position
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub symbol: Symbol,
    pub quantity: f64,
    pub avg_price: f64,
    pub side: OrderSide,
    pub pnl: f64,
    pub pnl_pct: f64,
    pub broker_id: String,
}

/// A news item from any feed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsItem {
    pub id: Uuid,
    pub headline: String,
    pub summary: String,
    pub source: String,
    pub url: String,
    pub published: DateTime<Utc>,
    pub symbols: Vec<Symbol>,
    pub sentiment: Option<f32>,
}

/// A trading signal emitted by a strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingSignal {
    pub id: Uuid,
    pub strategy_id: String,
    pub symbol: Symbol,
    pub action: SignalAction,
    pub quantity: f64,
    pub price: Option<f64>,
    pub confidence: f32,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

/// Signal action type
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum SignalAction {
    Buy,
    Sell,
    Close,
}

/// Request to create a new order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewOrderRequest {
    pub symbol: Symbol,
    pub side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
    pub tag: Option<String>,
    /// Broker-specific order parameters (product, exchange, ...). Adapters
    /// that don't use them ignore the field.
    #[serde(default)]
    pub broker_params: Option<BrokerOrderParams>,
}

/// Optional broker-specific order attributes, e.g. Zerodha's product/variety.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrokerOrderParams {
    /// Margin product, e.g. "CNC", "MIS", "NRML".
    #[serde(default)]
    pub product: Option<String>,
    /// Exchange segment, e.g. "NSE", "BSE", "NFO".
    #[serde(default)]
    pub exchange: Option<String>,
    /// Validity, e.g. "DAY", "IOC".
    #[serde(default)]
    pub validity: Option<String>,
    /// Order variety, e.g. "regular", "amo".
    #[serde(default)]
    pub variety: Option<String>,
}

/// Parameters for modifying an existing order
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifyParams {
    pub quantity: Option<f64>,
    pub price: Option<f64>,
    pub stop_price: Option<f64>,
}

/// Account balance information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountBalance {
    pub total_value: f64,
    pub cash: f64,
    pub margin_used: f64,
    pub margin_available: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub currency: String,
}

/// Parameters for querying OHLCV data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OHLCVQuery {
    pub symbol: Symbol,
    pub timeframe: Timeframe,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub limit: Option<usize>,
}

/// Parameters for querying orders
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderQuery {
    pub symbol: Option<Symbol>,
    pub status: Option<OrderStatus>,
    pub broker_id: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

/// Timeframe for OHLCV data
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum Timeframe {
    S1,
    S5,
    S15,
    M1,
    M3,
    M5,
    M15,
    M30,
    H1,
    H4,
    D1,
    W1,
}

impl Timeframe {
    pub fn as_str(&self) -> &'static str {
        match self {
            Timeframe::S1 => "S1",
            Timeframe::S5 => "S5",
            Timeframe::S15 => "S15",
            Timeframe::M1 => "M1",
            Timeframe::M3 => "M3",
            Timeframe::M5 => "M5",
            Timeframe::M15 => "M15",
            Timeframe::M30 => "M30",
            Timeframe::H1 => "H1",
            Timeframe::H4 => "H4",
            Timeframe::D1 => "D1",
            Timeframe::W1 => "W1",
        }
    }

    pub fn from_str(s: &str) -> Timeframe {
        match s {
            "S1" => Timeframe::S1,
            "S5" => Timeframe::S5,
            "S15" => Timeframe::S15,
            "M1" => Timeframe::M1,
            "M3" => Timeframe::M3,
            "M5" => Timeframe::M5,
            "M15" => Timeframe::M15,
            "M30" => Timeframe::M30,
            "H1" => Timeframe::H1,
            "H4" => Timeframe::H4,
            "W1" => Timeframe::W1,
            _ => Timeframe::D1,
        }
    }
}

/// News filter for subscribing to news feeds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsFilter {
    pub symbols: Option<Vec<Symbol>>,
    pub sources: Option<Vec<String>>,
    pub keywords: Option<Vec<String>>,
}

/// Fill event from broker
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillEvent {
    pub order_id: OrderId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub quantity: f64,
    pub price: f64,
    pub commission: f64,
    pub filled_at: DateTime<Utc>,
    pub broker_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_creation() {
        let s = Symbol("AAPL".into());
        assert_eq!(s.0, "AAPL");
    }

    #[test]
    fn test_order_status_equality() {
        assert_eq!(OrderStatus::Pending, OrderStatus::Pending);
        assert_ne!(OrderStatus::Pending, OrderStatus::Filled);
    }

    #[test]
    fn test_tick_serialization_roundtrip() {
        let tick = Tick {
            time: Utc::now(),
            symbol: Symbol("RELIANCE".into()),
            bid: 2500.50,
            ask: 2501.00,
            last: 2500.75,
            volume: 1000,
            source: "test".into(),
            open: None,
            change_pct: None,
        };
        let json = serde_json::to_string(&tick).unwrap();
        let deserialized: Tick = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.symbol, tick.symbol);
        assert_eq!(deserialized.bid, tick.bid);
    }

    #[test]
    fn test_order_serialization_roundtrip() {
        let order = Order {
            id: OrderId("ord-001".into()),
            symbol: Symbol("AAPL".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: 10.0,
            price: Some(150.0),
            stop_price: None,
            status: OrderStatus::Pending,
            filled_qty: 0.0,
            avg_price: 0.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            broker_id: "paper".into(),
            source: "manual".into(),
            rejection_reason: None,
        };
        let json = serde_json::to_string(&order).unwrap();
        let deserialized: Order = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, order.id);
        assert_eq!(deserialized.side, OrderSide::Buy);
    }

    #[test]
    fn test_new_order_request_serialization() {
        let req = NewOrderRequest {
            symbol: Symbol("BTC/USDT".into()),
            side: OrderSide::Sell,
            order_type: OrderType::Market,
            quantity: 0.5,
            price: None,
            stop_price: None,
            tag: Some("strategy_1".into()),
            broker_params: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: NewOrderRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.symbol, req.symbol);
    }

    #[test]
    fn test_timeframe_variants() {
        let tf = Timeframe::M5;
        let json = serde_json::to_string(&tf).unwrap();
        assert_eq!(json, "\"M5\"");
    }
}

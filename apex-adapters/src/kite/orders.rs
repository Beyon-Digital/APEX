//! Conversion between Kite order JSON and APEX orders. REST responses, order
//! history and KiteTicker order postbacks all share this shape.

use apex_core::domain::models::*;
use chrono::Utc;
use serde_json::Value;

use super::{parse_kite_time, to_apex_symbol};

/// Map a Kite order status (plus fill counts) to an APEX status.
pub fn map_status(status: &str, filled: f64, quantity: f64) -> OrderStatus {
    match status.trim().to_uppercase().as_str() {
        "COMPLETE" => OrderStatus::Filled,
        "REJECTED" => OrderStatus::Rejected,
        "CANCELLED" | "CANCELED" => OrderStatus::Cancelled,
        "OPEN" | "TRIGGER PENDING" | "MODIFIED" | "UPDATE" | "AMO REQ RECEIVED" => {
            if filled > 0.0 && filled < quantity {
                OrderStatus::PartiallyFilled
            } else {
                OrderStatus::Open
            }
        }
        // PUT ORDER REQ RECEIVED, VALIDATION PENDING, MODIFY PENDING, CANCEL PENDING, ...
        _ => OrderStatus::Pending,
    }
}

pub fn map_order_type(kite: &str) -> OrderType {
    match kite {
        "LIMIT" => OrderType::Limit,
        "SL-M" => OrderType::Stop,
        "SL" => OrderType::StopLimit,
        _ => OrderType::Market,
    }
}

/// Kite order type for an APEX order type. `SL-M` is stop-market (trigger
/// only); `SL` is stop-limit (trigger plus limit price).
pub fn kite_order_type(order_type: &OrderType) -> Option<&'static str> {
    match order_type {
        OrderType::Market => Some("MARKET"),
        OrderType::Limit => Some("LIMIT"),
        OrderType::Stop => Some("SL-M"),
        OrderType::StopLimit => Some("SL"),
        OrderType::TrailingStop => None,
    }
}

fn num(v: &Value, key: &str) -> f64 {
    v.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0)
}

fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(|x| x.as_str()).unwrap_or_default().to_string()
}

/// Build an APEX order from one Kite order object.
pub fn order_from_json(v: &Value) -> Option<Order> {
    let id = v.get("order_id").and_then(|x| x.as_str())?.to_string();
    let quantity = num(v, "quantity");
    let filled = num(v, "filled_quantity");
    let status_text = text(v, "status");
    let status = map_status(&status_text, filled, quantity);
    let price = num(v, "price");
    let trigger = num(v, "trigger_price");
    let created_at = parse_kite_time(&text(v, "order_timestamp")).unwrap_or_else(Utc::now);
    let updated_at = parse_kite_time(&text(v, "exchange_update_timestamp")).unwrap_or_else(Utc::now);
    let rejection_reason = (status == OrderStatus::Rejected).then(|| {
        let message = text(v, "status_message");
        if message.is_empty() {
            text(v, "status_message_raw")
        } else {
            message
        }
    });
    let tag = text(v, "tag");

    Some(Order {
        id: OrderId(id),
        symbol: Symbol(to_apex_symbol(&text(v, "exchange"), &text(v, "tradingsymbol"))),
        side: if text(v, "transaction_type") == "SELL" { OrderSide::Sell } else { OrderSide::Buy },
        order_type: map_order_type(&text(v, "order_type")),
        quantity,
        price: (price > 0.0).then_some(price),
        stop_price: (trigger > 0.0).then_some(trigger),
        status,
        filled_qty: filled,
        avg_price: num(v, "average_price"),
        created_at,
        updated_at,
        broker_id: "zerodha".to_string(),
        source: if tag.is_empty() { "api".to_string() } else { tag },
        rejection_reason: rejection_reason.filter(|r| !r.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_mapping_covers_kite_lifecycle() {
        assert_eq!(map_status("COMPLETE", 10.0, 10.0), OrderStatus::Filled);
        assert_eq!(map_status("OPEN", 0.0, 10.0), OrderStatus::Open);
        assert_eq!(map_status("OPEN", 4.0, 10.0), OrderStatus::PartiallyFilled);
        assert_eq!(map_status("TRIGGER PENDING", 0.0, 10.0), OrderStatus::Open);
        assert_eq!(map_status("VALIDATION PENDING", 0.0, 10.0), OrderStatus::Pending);
        assert_eq!(map_status("PUT ORDER REQ RECEIVED", 0.0, 10.0), OrderStatus::Pending);
        assert_eq!(map_status("CANCELLED", 0.0, 10.0), OrderStatus::Cancelled);
        assert_eq!(map_status("REJECTED", 0.0, 10.0), OrderStatus::Rejected);
    }

    #[test]
    fn stop_orders_map_to_correct_kite_types() {
        assert_eq!(kite_order_type(&OrderType::Stop), Some("SL-M"));
        assert_eq!(kite_order_type(&OrderType::StopLimit), Some("SL"));
        assert_eq!(kite_order_type(&OrderType::TrailingStop), None);
        assert_eq!(map_order_type("SL-M"), OrderType::Stop);
        assert_eq!(map_order_type("SL"), OrderType::StopLimit);
    }

    #[test]
    fn parses_rejected_order() {
        let order = order_from_json(&json!({
            "order_id": "220101000012345",
            "exchange": "NSE",
            "tradingsymbol": "RELIANCE",
            "transaction_type": "BUY",
            "order_type": "LIMIT",
            "quantity": 1,
            "price": 1200.5,
            "trigger_price": 0,
            "filled_quantity": 0,
            "average_price": 0,
            "status": "REJECTED",
            "status_message": "Insufficient funds",
            "order_timestamp": "2024-01-15 09:15:00",
            "tag": "apex"
        }))
        .unwrap();
        assert_eq!(order.symbol.0, "RELIANCE.NS");
        assert_eq!(order.status, OrderStatus::Rejected);
        assert_eq!(order.rejection_reason.as_deref(), Some("Insufficient funds"));
        assert_eq!(order.price, Some(1200.5));
        assert_eq!(order.stop_price, None);
        assert_eq!(order.source, "apex");
    }
}

//! Safeguards for orders sent to live (non-paper) brokers.
//!
//! * Live trading must be armed explicitly; it disarms on restart.
//! * Manual live orders need a confirmation id from `preview_order`, bound to
//!   the exact order, so the confirm dialog cannot be bypassed from the UI.
//! * Per-order quantity and per-day order-count caps apply to manual,
//!   automated and AI-placed live orders alike.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use apex_core::domain::models::{NewOrderRequest, OrderSide, OrderType};
use chrono::{NaiveDate, Utc};

use crate::config::LiveConfig;

const CONFIRMATION_TTL: Duration = Duration::from_secs(60);

struct DayCounter {
    date: NaiveDate,
    orders: u32,
}

pub struct LiveTradingGuard {
    armed: AtomicBool,
    limits: LiveConfig,
    counter: Mutex<DayCounter>,
    confirmations: Mutex<HashMap<String, (String, Instant)>>,
}

/// Holds one unit of the daily order budget; call `refund` if the order never
/// reached the broker.
pub struct OrderSlot<'a> {
    guard: &'a LiveTradingGuard,
    date: NaiveDate,
}

impl OrderSlot<'_> {
    pub fn refund(self) {
        let mut counter = self.guard.counter.lock().unwrap();
        if counter.date == self.date {
            counter.orders = counter.orders.saturating_sub(1);
        }
    }
}

impl LiveTradingGuard {
    pub fn new(limits: LiveConfig) -> Self {
        Self {
            armed: AtomicBool::new(false),
            limits,
            counter: Mutex::new(DayCounter { date: Utc::now().date_naive(), orders: 0 }),
            confirmations: Mutex::new(HashMap::new()),
        }
    }

    pub fn is_armed(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    pub fn set_armed(&self, armed: bool) {
        self.armed.store(armed, Ordering::SeqCst);
        if !armed {
            self.confirmations.lock().unwrap().clear();
        }
    }

    pub fn orders_today(&self) -> u32 {
        let mut counter = self.counter.lock().unwrap();
        Self::roll(&mut counter);
        counter.orders
    }

    pub fn max_orders_per_day(&self) -> u32 {
        self.limits.max_orders_per_day
    }

    pub fn max_qty_per_order(&self) -> f64 {
        self.limits.max_qty_per_order
    }

    fn roll(counter: &mut DayCounter) {
        let today = Utc::now().date_naive();
        if counter.date != today {
            counter.date = today;
            counter.orders = 0;
        }
    }

    /// Armed state and quantity cap, without touching the daily budget.
    pub fn check_order(&self, order: &NewOrderRequest) -> Result<(), String> {
        if !self.is_armed() {
            return Err(
                "Live trading is not armed. Arm it in the Live panel before sending live orders."
                    .to_string(),
            );
        }
        if order.quantity > self.limits.max_qty_per_order {
            return Err(format!(
                "Quantity {} exceeds the live per-order limit of {}",
                order.quantity, self.limits.max_qty_per_order
            ));
        }
        Ok(())
    }

    /// Modifying a live order is gated like placing one: armed, and the new
    /// quantity within the per-order cap.
    pub fn check_modify(&self, new_quantity: Option<f64>) -> Result<(), String> {
        if !self.is_armed() {
            return Err("Live trading is not armed. Arm it before changing live orders.".to_string());
        }
        if let Some(quantity) = new_quantity {
            if quantity > self.limits.max_qty_per_order {
                return Err(format!(
                    "Quantity {quantity} exceeds the live per-order limit of {}",
                    self.limits.max_qty_per_order
                ));
            }
        }
        Ok(())
    }

    /// Take one unit of today's order budget.
    pub fn reserve(&self) -> Result<OrderSlot<'_>, String> {
        let mut counter = self.counter.lock().unwrap();
        Self::roll(&mut counter);
        if counter.orders >= self.limits.max_orders_per_day {
            return Err(format!(
                "Daily live order limit reached ({})",
                self.limits.max_orders_per_day
            ));
        }
        counter.orders += 1;
        Ok(OrderSlot { guard: self, date: counter.date })
    }

    /// Identifies exactly what the user confirmed.
    pub fn fingerprint(broker_id: &str, order: &NewOrderRequest) -> String {
        let side = match order.side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        };
        let kind = match order.order_type {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
            OrderType::Stop => "stop",
            OrderType::StopLimit => "stoplimit",
            OrderType::TrailingStop => "trailing",
        };
        let product = order
            .broker_params
            .as_ref()
            .and_then(|p| p.product.as_deref())
            .unwrap_or("");
        format!(
            "{broker_id}|{}|{side}|{kind}|{}|{:?}|{:?}|{product}",
            order.symbol.0, order.quantity, order.price, order.stop_price
        )
    }

    pub fn issue_confirmation(&self, broker_id: &str, order: &NewOrderRequest) -> (String, u64) {
        let id = uuid::Uuid::new_v4().to_string();
        let mut confirmations = self.confirmations.lock().unwrap();
        confirmations.retain(|_, (_, at)| at.elapsed() < CONFIRMATION_TTL);
        confirmations.insert(id.clone(), (Self::fingerprint(broker_id, order), Instant::now()));
        (id, CONFIRMATION_TTL.as_secs())
    }

    /// Single-use: a confirmation is consumed whether or not it matches.
    pub fn consume_confirmation(
        &self,
        id: &str,
        broker_id: &str,
        order: &NewOrderRequest,
    ) -> Result<(), String> {
        let entry = self.confirmations.lock().unwrap().remove(id);
        match entry {
            None => Err("Order was not confirmed. Review and confirm it first.".to_string()),
            Some((_, at)) if at.elapsed() >= CONFIRMATION_TTL => {
                Err("Order confirmation expired. Review and confirm again.".to_string())
            }
            Some((fingerprint, _)) if fingerprint != Self::fingerprint(broker_id, order) => {
                Err("Order changed after it was confirmed. Review and confirm again.".to_string())
            }
            Some(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apex_core::domain::models::Symbol;

    fn order(qty: f64) -> NewOrderRequest {
        NewOrderRequest {
            symbol: Symbol("RELIANCE.NS".into()),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            quantity: qty,
            price: Some(100.0),
            stop_price: None,
            tag: None,
            broker_params: None,
        }
    }

    fn guard(max_orders: u32, max_qty: f64) -> LiveTradingGuard {
        LiveTradingGuard::new(LiveConfig { max_orders_per_day: max_orders, max_qty_per_order: max_qty })
    }

    #[test]
    fn disarmed_by_default_and_enforces_quantity() {
        let g = guard(5, 10.0);
        assert!(g.check_order(&order(1.0)).is_err());
        g.set_armed(true);
        assert!(g.check_order(&order(10.0)).is_ok());
        assert!(g.check_order(&order(11.0)).is_err());
    }

    #[test]
    fn daily_budget_and_refund() {
        let g = guard(2, 10.0);
        let a = g.reserve().unwrap();
        let _b = g.reserve().unwrap();
        assert!(g.reserve().is_err());
        a.refund();
        assert_eq!(g.orders_today(), 1);
        assert!(g.reserve().is_ok());
    }

    #[test]
    fn confirmation_is_single_use_and_bound_to_the_order() {
        let g = guard(5, 10.0);
        let (id, _) = g.issue_confirmation("zerodha", &order(1.0));
        assert!(g.consume_confirmation(&id, "zerodha", &order(2.0)).is_err());
        // Consumed by the failed attempt.
        assert!(g.consume_confirmation(&id, "zerodha", &order(1.0)).is_err());

        let (id, _) = g.issue_confirmation("zerodha", &order(1.0));
        assert!(g.consume_confirmation(&id, "zerodha", &order(1.0)).is_ok());
        assert!(g.consume_confirmation(&id, "zerodha", &order(1.0)).is_err());
        assert!(g.consume_confirmation("bogus", "zerodha", &order(1.0)).is_err());
    }

    #[test]
    fn disarming_invalidates_pending_confirmations() {
        let g = guard(5, 10.0);
        g.set_armed(true);
        let (id, _) = g.issue_confirmation("zerodha", &order(1.0));
        g.set_armed(false);
        assert!(g.consume_confirmation(&id, "zerodha", &order(1.0)).is_err());
    }

    #[test]
    fn modifying_needs_arming_and_respects_the_quantity_cap() {
        let g = guard(5, 10.0);
        assert!(g.check_modify(Some(5.0)).is_err(), "disarmed");
        g.set_armed(true);
        assert!(g.check_modify(Some(10.0)).is_ok());
        assert!(g.check_modify(None).is_ok());
        assert!(g.check_modify(Some(1000.0)).is_err(), "cannot inflate a small order past the cap");
    }
}

//! Exercise the same order manager and paper adapter used by live Python signals.
use apex_adapters::execution::paper_trading::PaperTradingAdapter;
use apex_core::{
    application::{
        order_trade_manager::{cancel_orphaned_paper_orders, OrderTradeManager},
        risk_engine::{RiskConfig, RiskEngine},
    },
    bus::message_bus::{BusMessage, MessageBus, Topic},
    domain::models::*,
    ports::{execution::ExecutionPort, storage::StoragePort},
};
use std::sync::Arc;

fn quote(price: f64) -> Quote {
    Quote {
        symbol: Symbol("AUDIT".into()),
        bid: price,
        ask: price,
        last: price,
        open: price,
        high: price,
        low: price,
        volume: 1,
        change_pct: 0.0,
        vwap: price,
        updated_at: chrono::Utc::now(),
    }
}
fn request(side: OrderSide, quantity: f64) -> NewOrderRequest {
    NewOrderRequest {
        symbol: Symbol("AUDIT".into()),
        side,
        order_type: OrderType::Market,
        quantity,
        price: Some(100.0),
        stop_price: None,
        tag: Some("strategy:regression".into()),
        broker_params: None,
    }
}

#[tokio::test]
async fn filled_entry_and_exit_reconcile_positions_and_emit_close() {
    let bus = Arc::new(MessageBus::new());
    let risk = Arc::new(RiskEngine::new(RiskConfig::default()));
    let paper = Arc::new(PaperTradingAdapter::new());
    paper.update_quote(&quote(100.0));
    let mut otm = OrderTradeManager::new(risk.clone(), bus.clone());
    otm.register_execution("paper".into(), paper.clone());
    let mut updates = bus.subscribe(Topic::PositionUpdate);
    let entry = otm
        .submit_order(request(OrderSide::Buy, 1.0), "paper")
        .await
        .unwrap();
    assert_eq!(
        paper.get_order_status(&entry).await.unwrap().status,
        OrderStatus::Filled
    );
    assert_eq!(otm.get_positions().len(), 1);
    paper.update_quote(&quote(110.0));
    assert!((paper.get_positions().await.unwrap()[0].pnl - 9.98).abs() < 1e-8);
    let marked = paper.get_account_balance().await.unwrap();
    assert!((marked.unrealized_pnl - 9.98).abs() < 1e-8);

    otm.submit_order(request(OrderSide::Sell, 1.0), "paper")
        .await
        .unwrap();
    assert!(otm.get_positions().is_empty());
    assert!(paper.get_positions().await.unwrap().is_empty());
    let balance = paper.get_account_balance().await.unwrap();
    assert!((balance.total_value - balance.cash).abs() < 1e-8);
    assert!((balance.realized_pnl - (balance.cash - 1_000_000.0)).abs() < 1e-8);
    assert!((risk.session_pnl() - balance.realized_pnl).abs() < 0.011);

    let mut closed = false;
    while let Ok(BusMessage::PositionData(pos)) = updates.try_recv() {
        closed |= pos.quantity == 0.0;
    }
    assert!(
        closed,
        "UI must receive a removal event for the closed position"
    );
}

#[tokio::test]
async fn risk_rejection_never_reaches_paper_adapter() {
    let bus = Arc::new(MessageBus::new());
    let risk = Arc::new(RiskEngine::new(RiskConfig {
        max_order_value: 50.0,
        ..RiskConfig::default()
    }));
    let paper = Arc::new(PaperTradingAdapter::new());
    paper.update_quote(&quote(100.0));
    let mut otm = OrderTradeManager::new(risk, bus);
    otm.register_execution("paper".into(), paper.clone());
    assert!(otm
        .submit_order(request(OrderSide::Buy, 1.0), "paper")
        .await
        .unwrap_err()
        .to_string()
        .contains("Risk"));
    assert!(paper.get_positions().await.unwrap().is_empty());
}

#[tokio::test]
async fn paper_realized_loss_triggers_the_daily_halt() {
    let risk = Arc::new(RiskEngine::new(RiskConfig {
        max_daily_loss: 50.0,
        ..RiskConfig::default()
    }));
    let paper = Arc::new(PaperTradingAdapter::new());
    paper.update_quote(&quote(100.0));
    let mut otm = OrderTradeManager::new(risk.clone(), Arc::new(MessageBus::new()));
    otm.register_execution("paper".into(), paper.clone());
    otm.submit_order(request(OrderSide::Buy, 1.0), "paper")
        .await
        .unwrap();
    paper.update_quote(&quote(10.0));
    otm.submit_order(request(OrderSide::Sell, 1.0), "paper")
        .await
        .unwrap();
    assert!(risk.is_halted());
    assert!(risk.session_pnl() < -90.0);
}

fn resting(
    side: OrderSide,
    kind: OrderType,
    limit: Option<f64>,
    stop: Option<f64>,
) -> NewOrderRequest {
    NewOrderRequest {
        symbol: Symbol("AUDIT".into()),
        side,
        order_type: kind,
        quantity: 1.0,
        price: limit,
        stop_price: stop,
        tag: None,
        broker_params: None,
    }
}

#[tokio::test]
async fn limit_and_stop_orders_trigger_once_and_publish_real_position_updates() {
    let bus = Arc::new(MessageBus::new());
    let risk = Arc::new(RiskEngine::new(RiskConfig {
        duplicate_window_ms: 0,
        ..RiskConfig::default()
    }));
    let paper = Arc::new(PaperTradingAdapter::new());
    let mut otm = OrderTradeManager::new(risk, bus.clone());
    otm.register_execution("paper".into(), paper.clone());
    let mut events = bus.subscribe(Topic::OrderUpdate("*".into()));
    otm.process_paper_quote(&quote(100.0)).await.unwrap();
    let buy = otm
        .submit_order(
            resting(OrderSide::Buy, OrderType::Limit, Some(90.0), None),
            "paper",
        )
        .await
        .unwrap();
    assert_eq!(
        paper.get_order_status(&buy).await.unwrap().status,
        OrderStatus::Open
    );
    let mut stale = quote(89.0);
    stale.updated_at -= chrono::Duration::minutes(5);
    otm.process_paper_quote(&stale).await.unwrap();
    assert_eq!(
        paper.get_order_status(&buy).await.unwrap().status,
        OrderStatus::Open
    );
    otm.process_paper_quote(&quote(90.0)).await.unwrap();
    let filled = paper.get_order_status(&buy).await.unwrap();
    assert_eq!(filled.status, OrderStatus::Filled);
    assert!(
        filled.avg_price <= 90.0,
        "slippage cannot violate a buy limit"
    );
    assert_eq!(otm.get_positions()[0].quantity, 1.0);
    otm.process_paper_quote(&quote(89.0)).await.unwrap();
    assert_eq!(otm.get_positions()[0].quantity, 1.0, "one fill only");
    let sell = otm
        .submit_order(
            resting(OrderSide::Sell, OrderType::Stop, None, Some(85.0)),
            "paper",
        )
        .await
        .unwrap();
    assert_eq!(
        paper.get_order_status(&sell).await.unwrap().status,
        OrderStatus::Open
    );
    otm.process_paper_quote(&quote(84.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&sell).await.unwrap().status,
        OrderStatus::Filled
    );
    assert!(otm.get_positions().is_empty());
    let mut status = Vec::new();
    while let Ok(BusMessage::OrderData(o)) = events.try_recv() {
        if o.id == buy {
            status.push(o.status);
        }
    }
    assert_eq!(status, vec![OrderStatus::Open, OrderStatus::Filled]);
}

#[tokio::test]
async fn stop_limit_latches_trigger_and_then_waits_for_limit() {
    let paper = Arc::new(PaperTradingAdapter::new());
    let mut otm = OrderTradeManager::new(
        Arc::new(RiskEngine::new(RiskConfig {
            duplicate_window_ms: 0,
            ..RiskConfig::default()
        })),
        Arc::new(MessageBus::new()),
    );
    otm.register_execution("paper".into(), paper.clone());
    otm.process_paper_quote(&quote(100.0)).await.unwrap();
    let id = otm
        .submit_order(
            resting(
                OrderSide::Buy,
                OrderType::StopLimit,
                Some(105.0),
                Some(110.0),
            ),
            "paper",
        )
        .await
        .unwrap();
    otm.process_paper_quote(&quote(111.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&id).await.unwrap().status,
        OrderStatus::Open
    );
    otm.process_paper_quote(&quote(104.0)).await.unwrap();
    let order = paper.get_order_status(&id).await.unwrap();
    assert_eq!(order.status, OrderStatus::Filled);
    assert!(order.avg_price <= 105.0);
}

#[tokio::test]
async fn cancelled_or_risk_rejected_resting_orders_never_fill() {
    let paper = Arc::new(PaperTradingAdapter::new());
    let risk = Arc::new(RiskEngine::new(RiskConfig {
        max_order_value: 150.0,
        duplicate_window_ms: 0,
        ..RiskConfig::default()
    }));
    let mut otm = OrderTradeManager::new(risk, Arc::new(MessageBus::new()));
    otm.register_execution("paper".into(), paper.clone());
    otm.process_paper_quote(&quote(100.0)).await.unwrap();
    let cancel = otm
        .submit_order(
            resting(OrderSide::Buy, OrderType::Limit, Some(90.0), None),
            "paper",
        )
        .await
        .unwrap();
    otm.cancel_order(&cancel, "paper").await.unwrap();
    otm.process_paper_quote(&quote(89.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&cancel).await.unwrap().status,
        OrderStatus::Cancelled
    );
    let mut stop = resting(OrderSide::Buy, OrderType::Stop, None, Some(120.0));
    stop.quantity = 1.0;
    let rejected = otm.submit_order(stop, "paper").await.unwrap();
    otm.process_paper_quote(&quote(160.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&rejected).await.unwrap().status,
        OrderStatus::Rejected
    );
    let rejected_order = paper.get_order_status(&rejected).await.unwrap();
    assert!(rejected_order
        .rejection_reason
        .as_deref()
        .unwrap()
        .contains("Order value"));
    let storage = apex_adapters::storage::sqlite_storage::SqliteStorage::new(":memory:").unwrap();
    storage.init_schema().await.unwrap();
    storage.write_order(&rejected_order).await.unwrap();
    let history = storage
        .query_orders(OrderQuery {
            symbol: None,
            status: Some(OrderStatus::Rejected),
            broker_id: Some("paper".into()),
            from: None,
            to: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(history[0].rejection_reason, rejected_order.rejection_reason);
    assert!(otm.get_positions().is_empty());
}

#[tokio::test]
async fn previous_session_paper_open_orders_are_cancelled_in_storage() {
    let storage = apex_adapters::storage::sqlite_storage::SqliteStorage::new(":memory:").unwrap();
    storage.init_schema().await.unwrap();
    let paper = Arc::new(PaperTradingAdapter::new());
    let id = paper
        .place_order(&resting(OrderSide::Buy, OrderType::Limit, Some(80.0), None))
        .await
        .unwrap();
    storage
        .write_order(&paper.get_order_status(&id).await.unwrap())
        .await
        .unwrap();
    assert_eq!(cancel_orphaned_paper_orders(&storage).await.unwrap(), 1);
    let found = storage
        .query_orders(OrderQuery {
            symbol: None,
            status: Some(OrderStatus::Cancelled),
            broker_id: Some("paper".into()),
            from: None,
            to: None,
            limit: None,
        })
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, id);
}

#[tokio::test]
async fn buy_stop_and_sell_limit_use_correct_direction_and_preserve_limit() {
    let paper = Arc::new(PaperTradingAdapter::new());
    let mut otm = OrderTradeManager::new(
        Arc::new(RiskEngine::new(RiskConfig {
            duplicate_window_ms: 0,
            ..RiskConfig::default()
        })),
        Arc::new(MessageBus::new()),
    );
    otm.register_execution("paper".into(), paper.clone());
    otm.process_paper_quote(&quote(100.0)).await.unwrap();
    let buy = otm
        .submit_order(
            resting(OrderSide::Buy, OrderType::Stop, None, Some(110.0)),
            "paper",
        )
        .await
        .unwrap();
    otm.process_paper_quote(&quote(109.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&buy).await.unwrap().status,
        OrderStatus::Open
    );
    otm.process_paper_quote(&quote(111.0)).await.unwrap();
    assert!(paper.get_order_status(&buy).await.unwrap().avg_price > 111.0);
    let sell = otm
        .submit_order(
            resting(OrderSide::Sell, OrderType::Limit, Some(120.0), None),
            "paper",
        )
        .await
        .unwrap();
    otm.process_paper_quote(&quote(119.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&sell).await.unwrap().status,
        OrderStatus::Open
    );
    otm.process_paper_quote(&quote(120.0)).await.unwrap();
    assert_eq!(
        paper.get_order_status(&sell).await.unwrap().status,
        OrderStatus::Filled
    );
    assert!(paper.get_order_status(&sell).await.unwrap().avg_price >= 120.0);
    assert!(otm.get_positions().is_empty());
}

#[tokio::test]
async fn market_risk_uses_current_quote_not_caller_supplied_price() {
    let paper = Arc::new(PaperTradingAdapter::new());
    let risk = Arc::new(RiskEngine::new(RiskConfig {
        max_order_value: 50.0,
        duplicate_window_ms: 0,
        ..RiskConfig::default()
    }));
    let mut otm = OrderTradeManager::new(risk, Arc::new(MessageBus::new()));
    otm.register_execution("paper".into(), paper.clone());
    otm.process_paper_quote(&quote(100.0)).await.unwrap();
    let mut order = request(OrderSide::Buy, 1.0);
    order.price = Some(1.0);
    assert!(otm
        .submit_order(order, "paper")
        .await
        .unwrap_err()
        .to_string()
        .contains("Risk"));
    assert!(paper.get_positions().await.unwrap().is_empty());
}

#[tokio::test]
async fn resting_buy_rejection_explains_insufficient_funds() {
    let paper = Arc::new(PaperTradingAdapter::with_config(
        120.0,
        "USD".into(),
        2.0,
        3.0,
    ));
    let risk = Arc::new(RiskEngine::new(RiskConfig {
        max_position_pct: 10.0,
        duplicate_window_ms: 0,
        ..RiskConfig::default()
    }));
    let mut otm = OrderTradeManager::new(risk, Arc::new(MessageBus::new()));
    otm.register_execution("paper".into(), paper.clone());
    otm.process_paper_quote(&quote(100.0)).await.unwrap();
    let pending = otm
        .submit_order(
            resting(OrderSide::Buy, OrderType::Limit, Some(90.0), None),
            "paper",
        )
        .await
        .unwrap();
    otm.submit_order(request(OrderSide::Buy, 1.0), "paper")
        .await
        .unwrap();
    otm.process_paper_quote(&quote(89.0)).await.unwrap();
    let result = paper.get_order_status(&pending).await.unwrap();
    assert_eq!(result.status, OrderStatus::Rejected);
    assert!(result
        .rejection_reason
        .as_deref()
        .unwrap()
        .contains("Insufficient funds"));
}

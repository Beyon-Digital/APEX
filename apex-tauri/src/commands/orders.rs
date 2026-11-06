use crate::dto::{AccountBalanceDto, NewOrderRequestDto, OrderDto, OrderPreviewDto, PositionDto};
use crate::state::{AppState, LiveAuth};
use crate::validation;
use apex_core::domain::models::*;
use tauri::State;

/// Validate a UI order request and convert it to the domain type.
fn parse_order(request: &NewOrderRequestDto) -> Result<NewOrderRequest, String> {
    validation::validate_symbol(&request.symbol)?;
    validation::validate_quantity(request.quantity)?;
    validation::validate_price(request.price)?;
    validation::validate_price(request.stop_price)?;
    validation::validate_broker_id(&request.broker_id)?;
    if let Some(ref tag) = request.tag {
        validation::validate_string_length(tag, "tag")?;
    }
    if let Some(ref product) = request.product {
        validation::validate_string_length(product, "product")?;
    }

    let side = match request.side.to_lowercase().as_str() {
        "buy" => OrderSide::Buy,
        "sell" => OrderSide::Sell,
        _ => return Err(format!("Invalid side: {}", request.side)),
    };

    let order_type = match request.order_type.to_lowercase().as_str() {
        "market" => OrderType::Market,
        "limit" => OrderType::Limit,
        "stop" => OrderType::Stop,
        "stoplimit" | "stop_limit" => OrderType::StopLimit,
        "trailingstop" | "trailing_stop" => OrderType::TrailingStop,
        _ => return Err(format!("Invalid order type: {}", request.order_type)),
    };

    Ok(NewOrderRequest {
        symbol: Symbol(request.symbol.clone()),
        side,
        order_type,
        quantity: request.quantity,
        price: request.price,
        stop_price: request.stop_price,
        tag: request.tag.clone(),
        broker_params: request.product.clone().map(|product| BrokerOrderParams {
            product: Some(product.to_uppercase()),
            ..Default::default()
        }),
    })
}

/// Place a new order. Live orders need `confirmation_id` from `preview_order`.
#[tauri::command]
pub async fn place_order(
    request: NewOrderRequestDto,
    confirmation_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let new_order = parse_order(&request)?;

    if request.broker_id == "paper" && new_order.order_type != OrderType::Market {
        state
            .aggregator
            .start(&[new_order.symbol.clone()])
            .await
            .map_err(|e| format!("Could not subscribe to market data for paper order: {e}"))?;
    }

    state
        .guarded_submit(
            new_order,
            &request.broker_id,
            LiveAuth::Manual(confirmation_id.as_deref()),
        )
        .await
        .map(|id| id.0)
}

/// Describe what an order would do and, for live brokers, issue the
/// confirmation id that `place_order` requires.
#[tauri::command]
pub async fn preview_order(
    request: NewOrderRequestDto,
    state: State<'_, AppState>,
) -> Result<OrderPreviewDto, String> {
    let order = parse_order(&request)?;
    let live = request.broker_id != "paper";
    let reference = order
        .price
        .or(order.stop_price)
        .or_else(|| state.aggregator.get_cached_quote(&order.symbol.0).map(|q| q.last));
    let estimated_value = reference.map(|price| price * order.quantity);

    let mut warnings = Vec::new();
    if live {
        if !state.live_guard.is_armed() {
            warnings.push("Live trading is not armed.".to_string());
        }
        if order.quantity > state.live_guard.max_qty_per_order() {
            warnings.push(format!(
                "Quantity exceeds the live per-order limit of {}.",
                state.live_guard.max_qty_per_order()
            ));
        }
        if state.live_guard.orders_today() >= state.live_guard.max_orders_per_day() {
            warnings.push("Daily live order limit reached.".to_string());
        }
        if state.risk.is_halted() {
            warnings.push("Trading is halted by the risk engine.".to_string());
        }
        if estimated_value.is_none() {
            warnings.push("No live quote; order value cannot be estimated.".to_string());
        }
    }

    let (confirmation_id, expires_in_secs) = if live && warnings.iter().all(|w| w.starts_with("No live quote")) {
        let (id, ttl) = state.live_guard.issue_confirmation(&request.broker_id, &order);
        (Some(id), ttl)
    } else {
        (None, 0)
    };

    Ok(OrderPreviewDto {
        broker_id: request.broker_id,
        live,
        estimated_value,
        confirmation_id,
        expires_in_secs,
        warnings,
    })
}

/// Cancel an order.
#[tauri::command]
pub async fn cancel_order(
    order_id: String,
    broker_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validation::validate_string_length(&order_id, "order_id")?;
    validation::validate_broker_id(&broker_id)?;

    state
        .otm
        .cancel_order(&OrderId(order_id), &broker_id)
        .await
        .map_err(|e| e.to_string())
}

/// Modify an existing order.
#[tauri::command]
pub async fn modify_order(
    order_id: String,
    new_quantity: Option<f64>,
    new_price: Option<f64>,
    broker_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    validation::validate_string_length(&order_id, "order_id")?;
    validation::validate_broker_id(&broker_id)?;
    if let Some(q) = new_quantity {
        validation::validate_quantity(q)?;
    }
    validation::validate_price(new_price)?;

    let params = ModifyParams {
        quantity: new_quantity,
        price: new_price,
        stop_price: None,
    };

    state
        .guarded_modify(&OrderId(order_id), &broker_id, &params)
        .await
        .map_err(|e| format!("Failed to modify order: {}", e))
}

/// Get all positions.
#[tauri::command]
pub async fn get_positions(state: State<'_, AppState>) -> Result<Vec<PositionDto>, String> {
    Ok(state
        .otm
        .get_positions()
        .iter()
        .map(PositionDto::from)
        .collect())
}

/// Long-term holdings reported by a broker (empty for brokers without them).
#[tauri::command]
pub async fn get_holdings(
    broker_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<PositionDto>, String> {
    validation::validate_broker_id(&broker_id)?;
    state
        .otm
        .get_holdings(&broker_id)
        .await
        .map(|holdings| holdings.iter().map(PositionDto::from).collect())
        .map_err(|e| e.to_string())
}

/// Get all open orders.
#[tauri::command]
pub async fn get_open_orders(state: State<'_, AppState>) -> Result<Vec<OrderDto>, String> {
    Ok(state.otm.open_orders().iter().map(OrderDto::from).collect())
}

/// Get order history (all statuses, newest first) — backs the order blotter.
/// Falls back to the in-memory open-order map when storage has no rows yet,
/// so a fresh session still shows resting orders.
#[tauri::command]
pub async fn get_orders(
    symbol: Option<String>,
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<OrderDto>, String> {
    if let Some(ref s) = symbol {
        validation::validate_symbol(s)?;
    }

    let mut orders = state
        .storage
        .query_orders(OrderQuery {
            symbol: symbol.clone().map(Symbol),
            status: None,
            broker_id: None,
            from: None,
            to: None,
            limit: Some(limit.unwrap_or(200).min(1000)),
        })
        .await
        .map_err(|e| format!("Failed to query orders: {}", e))?;

    // Prefer the newer in-memory lifecycle while the journal catches up.
    for open in state.otm.open_orders() {
        if let Some(existing) = orders.iter_mut().find(|o| o.id == open.id) {
            if open.updated_at >= existing.updated_at {
                *existing = open;
            }
        } else {
            if symbol.as_ref().map(|s| open.symbol.0 == *s).unwrap_or(true) {
                orders.push(open);
            }
        }
    }
    orders.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    orders.truncate(limit.unwrap_or(200).min(1000));

    Ok(orders.iter().map(OrderDto::from).collect())
}

/// Get account balance for a broker.
#[tauri::command]
pub async fn get_account_balance(
    broker_id: String,
    state: State<'_, AppState>,
) -> Result<AccountBalanceDto, String> {
    validation::validate_broker_id(&broker_id)?;

    let balance = state
        .otm
        .get_account_balance(&broker_id)
        .await
        .map_err(|e| format!("Failed to get account balance: {}", e))?;

    Ok(AccountBalanceDto::from(&balance))
}

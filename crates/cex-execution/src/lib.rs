use anyhow::{Context, Result, anyhow, ensure};
use async_trait::async_trait;
use shared::{config::CexConfig, types::MarketSide};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::env;
use tokio::time::Duration;
use tracing::{debug, info, warn};

use binance_sdk::{
    config::ConfigurationRestApi,
    spot::{
        rest_api::{
            self,
            DeleteOpenOrdersParams,
            DeleteOrderParams,
            GetAccountParams,
            NewOrderNewOrderRespTypeEnum,
            NewOrderParams,
            NewOrderSideEnum,
            NewOrderTimeInForceEnum,
            NewOrderTypeEnum,
        },
        SpotRestApi,
    },
};
use dotenvy::dotenv;
use rust_decimal::{Decimal, prelude::FromPrimitive};

#[derive(Debug, Clone)]
pub struct CexOrderRequest {
    pub pair: String,
    pub side: MarketSide,
    pub size: f64,
    pub price: Option<f64>,
    pub time_in_force: Option<Duration>,
}

#[derive(Debug, Clone)]
pub struct CexExecutionReport {
    pub order_id: String,
    pub pair: String,
    pub side: MarketSide,
    pub requested_size: f64,
    pub filled_size: f64,
    pub avg_fill_price: f64,
    pub remaining_size: f64,
    pub slippage_bps: f64,
}

#[derive(Debug, Clone, Default)]
pub struct ExchangePosition {
    pub pair: String,
    pub net_position: f64,
    pub avg_entry_price: f64,
}

#[async_trait]
pub trait CexExecutor: Send + Sync {
    async fn place_order(&self, request: CexOrderRequest) -> Result<CexExecutionReport>;
    async fn cancel_order(&self, order_id: &str) -> Result<()>;
    async fn cancel_all(&self, pair: Option<&str>) -> Result<usize>;
    async fn fetch_open_position(&self, pair: &str) -> Result<Option<ExchangePosition>>;
}

#[derive(Debug)]
pub struct BinancePerpExecutor {
    id_counter: AtomicU64,
    fill_ratio: f64,
    slippage_bps: f64,
    min_price: f64,
}

impl BinancePerpExecutor {
    pub fn new() -> Self {
        Self {
            id_counter: AtomicU64::new(0),
            fill_ratio: 1.0,
            slippage_bps: 2.5,
            min_price: 0.000_000_1,
        }
    }

    pub fn with_fill_ratio(mut self, ratio: f64) -> Self {
        self.fill_ratio = ratio.clamp(0.0, 1.0);
        self
    }

    pub fn with_slippage(mut self, slippage_bps: f64) -> Self {
        self.slippage_bps = slippage_bps.max(0.0);
        self
    }

    fn next_order_id(&self) -> String {
        let id = self.id_counter.fetch_add(1, Ordering::Relaxed) + 1;
        format!("binance-perp-{}", id)
    }

    fn adjusted_price(&self, side: &MarketSide, price: f64) -> f64 {
        if price <= 0.0 {
            return self.min_price;
        }

        let slip = self.slippage_bps / 10_000.0;
        let factor = match side {
            MarketSide::Bid => 1.0 + slip,
            MarketSide::Ask => 1.0 - slip,
        };

        (price * factor).max(self.min_price)
    }
}

impl Default for BinancePerpExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CexExecutor for BinancePerpExecutor {
    async fn place_order(&self, request: CexOrderRequest) -> Result<CexExecutionReport> {
        ensure!(request.size > 0.0, "order size must be positive");
        let order_id = self.next_order_id();
        let requested_size = request.size;
        let fill_ratio = self.fill_ratio.clamp(0.0, 1.0);
        let filled_size = (requested_size * fill_ratio).min(requested_size);
        let remaining_size = (requested_size - filled_size).max(0.0);

        let reference_price = request.price.unwrap_or(1.0_f64);
        let avg_fill_price = self.adjusted_price(&request.side, reference_price);

        info!(
            order_id = %order_id,
            pair = %request.pair,
            side = ?request.side,
            requested = requested_size,
            filled = filled_size,
            remaining = remaining_size,
            slippage_bps = self.slippage_bps,
            "executed binance perp order (stub)"
        );

        Ok(CexExecutionReport {
            order_id,
            pair: request.pair,
            side: request.side,
            requested_size,
            filled_size,
            avg_fill_price,
            remaining_size,
            slippage_bps: self.slippage_bps,
        })
    }

    async fn cancel_order(&self, order_id: &str) -> Result<()> {
        info!(order_id, "cancel order (stub)");
        // TODO: 调用 Binance 永续撤单接口
        Ok(())
    }

    async fn cancel_all(&self, pair: Option<&str>) -> Result<usize> {
        info!(?pair, "cancel all orders for pair (stub)");
        // TODO: 批量撤销 Binance 永续订单
        Ok(0)
    }

    async fn fetch_open_position(&self, pair: &str) -> Result<Option<ExchangePosition>> {
        debug!(pair, "fetch open position (stub)");
        // TODO: 查询 Binance 永续当前持仓
        Ok(None)
    }
}

#[derive(Debug, Clone, Default)]
pub struct PositionState {
    pub net_position: f64,
    pub avg_entry_price: f64,
    pub realized_pnl: f64,
}

const ORDER_ID_DELIMITER: char = ':';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinanceEnvironment {
    Production,
    Testnet,
}

impl BinanceEnvironment {
    fn from_flag(use_testnet: bool) -> Self {
        if use_testnet {
            BinanceEnvironment::Testnet
        } else {
            BinanceEnvironment::Production
        }
    }

    fn label(self) -> &'static str {
        match self {
            BinanceEnvironment::Production => "production",
            BinanceEnvironment::Testnet => "testnet",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BinanceSpotExecutor {
    client: rest_api::RestApi,
    recv_window: Option<Decimal>,
    environment: BinanceEnvironment,
}

impl BinanceSpotExecutor {
    pub fn new(
        api_key: String,
        api_secret: String,
        environment: BinanceEnvironment,
        recv_window_ms: Option<u64>,
    ) -> Result<Self> {
        let mut builder = ConfigurationRestApi::builder()
            .api_key(api_key)
            .api_secret(api_secret);

        let recv_window = recv_window_ms
            .map(decimal_from_u64)
            .transpose()
            .context("invalid recv_window_ms value")?;

        let rest_conf = builder
            .build()
            .context("failed to build Binance REST configuration")?;

        let client = match environment {
            BinanceEnvironment::Production => SpotRestApi::production(rest_conf),
            BinanceEnvironment::Testnet => SpotRestApi::testnet(rest_conf),
        };

        Ok(Self {
            client,
            recv_window,
            environment,
        })
    }

    pub fn from_config(config: &CexConfig) -> Result<Self> {
        let _ = dotenv();

        let api_key = env::var(&config.api_key_env)
            .with_context(|| format!("环境变量 {} 未设置", config.api_key_env))?;
        let api_secret = env::var(&config.api_secret_env)
            .with_context(|| format!("环境变量 {} 未设置", config.api_secret_env))?;

        let executor = Self::new(
            api_key,
            api_secret,
            BinanceEnvironment::from_flag(config.use_testnet),
            config.recv_window_ms,
        )?;

        info!(
            environment = executor.environment.label(),
            "initialized Binance spot executor"
        );

        Ok(executor)
    }

    fn symbol_from_pair(pair: &str) -> Result<String> {
        let symbol = pair.replace('/', "").to_uppercase();
        ensure!(!symbol.is_empty(), "pair cannot be empty");
        Ok(symbol)
    }

    fn base_asset(pair: &str) -> Result<String> {
        pair.split_once('/')
            .map(|(base, _)| base.to_uppercase())
            .ok_or_else(|| anyhow!("pair {pair} 必须包含 / 分隔 base/quote"))
    }

    fn map_side(side: &MarketSide) -> NewOrderSideEnum {
        match side {
            MarketSide::Bid => NewOrderSideEnum::Buy,
            MarketSide::Ask => NewOrderSideEnum::Sell,
        }
    }

    fn map_time_in_force(tif: Option<Duration>) -> NewOrderTimeInForceEnum {
        match tif {
            Some(duration) if duration.is_zero() => NewOrderTimeInForceEnum::Ioc,
            Some(duration) if duration <= Duration::from_secs(1) => NewOrderTimeInForceEnum::Ioc,
            Some(_) => NewOrderTimeInForceEnum::Gtc,
            None => NewOrderTimeInForceEnum::Gtc,
        }
    }

    fn parse_decimal(value: Option<&String>) -> Result<f64> {
        let parsed = value
            .map(|s| s.parse::<f64>().with_context(|| format!("无法解析数值 {}", s)))
            .transpose()?
            .unwrap_or(0.0);
        Ok(parsed)
    }

    fn split_order_id(&self, order_id: &str) -> Result<(String, String)> {
        if let Some((symbol, native)) = order_id.split_once(ORDER_ID_DELIMITER) {
            Ok((symbol.to_uppercase(), native.to_string()))
        } else {
            warn!(
                %order_id,
                "order id 未包含符号分隔，默认使用上次下单交易对进行撤单"
            );
            Err(anyhow!("order id {order_id} 缺少 symbol 前缀，无法撤单"))
        }
    }
}

#[async_trait]
impl CexExecutor for BinanceSpotExecutor {
    async fn place_order(&self, request: CexOrderRequest) -> Result<CexExecutionReport> {
        ensure!(request.size > 0.0, "order size must be positive");

        let CexOrderRequest {
            pair,
            side,
            size: requested_size,
            price: maybe_price,
            time_in_force,
        } = request;

        let symbol = Self::symbol_from_pair(&pair)?;
        let order_side = Self::map_side(&side);
        let quantity = decimal_from_f64(requested_size, "quantity")?;

        let order_type = if maybe_price.is_some() {
            NewOrderTypeEnum::Limit
        } else {
            NewOrderTypeEnum::Market
        };

        let mut builder = NewOrderParams::builder(symbol.clone(), order_side, order_type)
            .quantity(quantity)
            .new_order_resp_type(NewOrderNewOrderRespTypeEnum::Full);

        if let Some(limit_price) = maybe_price {
            let price_decimal = decimal_from_f64(limit_price, "price")?;
            builder = builder
                .price(price_decimal)
                .time_in_force(Self::map_time_in_force(time_in_force));
        } else if time_in_force.is_some() {
            warn!(
                pair = %pair,
                "忽略 market order 的 time_in_force 参数"
            );
        }

        if let Some(recv) = self.recv_window {
            builder = builder.recv_window(recv);
        }

        let params = builder
            .build()
            .context("failed to build Binance new_order params")?;

        let response = self
            .client
            .new_order(params)
            .await
            .context("Binance new_order 请求失败")?;
        let data = response
            .data()
            .await
            .context("Binance new_order 解析响应失败")?;

        let order_symbol = data
            .symbol
            .clone()
            .unwrap_or_else(|| symbol.clone());
        let raw_order_id = data
            .order_id
            .map(|id| id.to_string())
            .or_else(|| data.client_order_id.clone())
            .ok_or_else(|| anyhow!("Binance 响应缺少 order id"))?;
        let filled_size = Self::parse_decimal(data.executed_qty.as_ref())?;
        let filled_quote = Self::parse_decimal(data.cummulative_quote_qty.as_ref())?;
        let remaining_size = (requested_size - filled_size).max(0.0);

        let avg_fill_price = if filled_size > 0.0 && filled_quote > 0.0 {
            filled_quote / filled_size
        } else if let Some(limit_price) = maybe_price {
            limit_price
        } else {
            0.0
        };

        let slippage_bps = maybe_price.map(|p| {
            if p.abs() < f64::EPSILON {
                0.0
            } else {
                ((avg_fill_price - p) / p) * 10_000.0
            }
        }).unwrap_or(0.0);

        let order_id = format!(
            "{}{}{}",
            order_symbol.to_uppercase(),
            ORDER_ID_DELIMITER,
            raw_order_id
        );

        info!(
            %order_id,
            pair = %pair,
            side = ?side,
            filled = filled_size,
            avg_price = avg_fill_price,
            "placed Binance spot order"
        );

        Ok(CexExecutionReport {
            order_id,
            pair,
            side,
            requested_size,
            filled_size,
            avg_fill_price,
            remaining_size,
            slippage_bps,
        })
    }

    async fn cancel_order(&self, order_id: &str) -> Result<()> {
        let (symbol, native_id) = self.split_order_id(order_id)?;
        let mut builder = DeleteOrderParams::builder(symbol.clone());

        if let Ok(numeric) = native_id.parse::<i64>() {
            builder = builder.order_id(numeric);
        } else {
            builder = builder.orig_client_order_id(native_id.clone());
        }

        if let Some(recv) = self.recv_window {
            builder = builder.recv_window(recv);
        }

        let params = builder
            .build()
            .context("failed to build Binance delete_order params")?;

        self.client
            .delete_order(params)
            .await
            .context("Binance 撤单请求失败")?;

        info!(%order_id, "submitted Binance cancel order");
        Ok(())
    }

    async fn cancel_all(&self, pair: Option<&str>) -> Result<usize> {
        let pair = pair.ok_or_else(|| anyhow!("cancel_all 需要提供交易对 symbol"))?;
        let symbol = Self::symbol_from_pair(pair)?;
        let mut builder = DeleteOpenOrdersParams::builder(symbol.clone());

        if let Some(recv) = self.recv_window {
            builder = builder.recv_window(recv);
        }

        let params = builder
            .build()
            .context("failed to build Binance delete_open_orders params")?;

        let response = self
            .client
            .delete_open_orders(params)
            .await
            .context("Binance 批量撤单请求失败")?;

        let cancelled = response
            .data()
            .await
            .context("解析 Binance 批量撤单响应失败")?
            .len();

        info!(pair = %pair, cancelled, "cancelled Binance open orders");
        Ok(cancelled)
    }

    async fn fetch_open_position(&self, pair: &str) -> Result<Option<ExchangePosition>> {
        let base_asset = Self::base_asset(pair)?;

        let params = GetAccountParams::default();
        let response = self
            .client
            .get_account(params)
            .await
            .context("Binance 查询账户资产失败")?;
        let account = response
            .data()
            .await
            .context("解析 Binance 账户资产失败")?;

        let balances = account.balances.unwrap_or_default();
        let balance = balances
            .into_iter()
            .find(|b| b.asset.as_deref().map(|s| s.eq_ignore_ascii_case(&base_asset)) == Some(true));

        if let Some(entry) = balance {
            let free = Self::parse_decimal(entry.free.as_ref())?;
            let locked = Self::parse_decimal(entry.locked.as_ref())?;
            let net_position = free + locked;

            if net_position.abs() < f64::EPSILON {
                return Ok(None);
            }

            Ok(Some(ExchangePosition {
                pair: pair.to_string(),
                net_position,
                avg_entry_price: 0.0,
            }))
        } else {
            Ok(None)
        }
    }
}

#[derive(Debug, Clone)]
pub struct PositionUpdate {
    pub pair: String,
    pub side: MarketSide,
    pub filled_size: f64,
    pub avg_fill_price: f64,
    pub realized_pnl_delta: f64,
    pub realized_pnl_total: f64,
    pub net_position: f64,
    pub avg_entry_price: f64,
    pub remaining_size: f64,
}

#[derive(Debug, Default, Clone)]
pub struct PositionManager {
    positions: HashMap<String, PositionState>,
}

impl PositionManager {
    pub fn apply_execution(&mut self, report: &CexExecutionReport) -> PositionUpdate {
        let filled = report.filled_size.max(0.0);
        let pair = report.pair.clone();
        let side = report.side.clone();
        let price = report.avg_fill_price.max(0.0);
        let mut realized_delta = 0.0;

        let state = self.positions.entry(pair.clone()).or_default();

        if filled > 0.0 {
            match side {
                MarketSide::Bid => handle_buy(state, filled, price, &mut realized_delta),
                MarketSide::Ask => handle_sell(state, filled, price, &mut realized_delta),
            }

            if state.net_position.abs() < f64::EPSILON {
                state.net_position = 0.0;
                state.avg_entry_price = 0.0;
            }
            state.realized_pnl += realized_delta;
        }

        PositionUpdate {
            pair,
            side,
            filled_size: filled,
            avg_fill_price: price,
            realized_pnl_delta: realized_delta,
            realized_pnl_total: state.realized_pnl,
            net_position: state.net_position,
            avg_entry_price: state.avg_entry_price,
            remaining_size: report.remaining_size,
        }
    }

    pub fn snapshot(&self) -> HashMap<String, PositionState> {
        self.positions.clone()
    }
}

fn handle_buy(state: &mut PositionState, filled: f64, price: f64, realized: &mut f64) {
    if state.net_position >= 0.0 {
        let new_total = state.net_position + filled;
        if new_total > 0.0 {
            state.avg_entry_price = if state.net_position == 0.0 {
                price
            } else {
                weighted_average(state.avg_entry_price, state.net_position, price, filled)
            };
            state.net_position = new_total;
        }
    } else {
        let short_size = -state.net_position;
        let closing = filled.min(short_size);
        *realized += closing * (state.avg_entry_price - price);
        state.net_position += closing;
        let remaining = filled - closing;
        if remaining > 0.0 {
            state.avg_entry_price = price;
            state.net_position = remaining;
        }
    }
}

fn handle_sell(state: &mut PositionState, filled: f64, price: f64, realized: &mut f64) {
    if state.net_position <= 0.0 {
        let short_size = -state.net_position;
        state.avg_entry_price = if state.net_position == 0.0 {
            price
        } else if short_size > 0.0 {
            weighted_average(state.avg_entry_price, short_size, price, filled)
        } else {
            price
        };
        state.net_position -= filled;
    } else {
        let closing = filled.min(state.net_position);
        *realized += closing * (price - state.avg_entry_price);
        state.net_position -= closing;
        let remaining = filled - closing;
        if remaining > 0.0 {
            state.avg_entry_price = price;
            state.net_position = -remaining;
        }
    }
}

fn weighted_average(a_price: f64, a_size: f64, b_price: f64, b_size: f64) -> f64 {
    if (a_size + b_size).abs() < f64::EPSILON {
        return 0.0;
    }
    ((a_price * a_size) + (b_price * b_size)) / (a_size + b_size)
}

fn decimal_from_f64(value: f64, field: &str) -> Result<Decimal> {
    Decimal::from_f64(value)
        .ok_or_else(|| anyhow!("{} 数值无法转换为 Decimal：{}", field, value))
}

fn decimal_from_u64(value: u64) -> Result<Decimal> {
    Decimal::from_u64(value).ok_or_else(|| anyhow!("无法转换 recv_window_ms 数值 {}", value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_position_realizes_pnl_when_selling() {
        let mut manager = PositionManager::default();

        let buy_report = CexExecutionReport {
            order_id: "1".into(),
            pair: "WAL/USDC".into(),
            side: MarketSide::Bid,
            requested_size: 10.0,
            filled_size: 10.0,
            avg_fill_price: 100.0,
            remaining_size: 0.0,
            slippage_bps: 0.0,
        };
        manager.apply_execution(&buy_report);

        let sell_report = CexExecutionReport {
            order_id: "2".into(),
            pair: "WAL/USDC".into(),
            side: MarketSide::Ask,
            requested_size: 5.0,
            filled_size: 5.0,
            avg_fill_price: 110.0,
            remaining_size: 0.0,
            slippage_bps: 0.0,
        };
        let update = manager.apply_execution(&sell_report);
        assert_eq!(update.net_position, 5.0);
        assert!((update.realized_pnl_delta - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn close_long_and_flip_to_short() {
        let mut manager = PositionManager::default();

        manager.apply_execution(&CexExecutionReport {
            order_id: "1".into(),
            pair: "WAL/USDC".into(),
            side: MarketSide::Bid,
            requested_size: 5.0,
            filled_size: 5.0,
            avg_fill_price: 100.0,
            remaining_size: 0.0,
            slippage_bps: 0.0,
        });

        let update = manager.apply_execution(&CexExecutionReport {
            order_id: "2".into(),
            pair: "WAL/USDC".into(),
            side: MarketSide::Ask,
            requested_size: 7.0,
            filled_size: 7.0,
            avg_fill_price: 105.0,
            remaining_size: 0.0,
            slippage_bps: 0.0,
        });

        assert_eq!(update.net_position, -2.0);
        assert!((update.realized_pnl_delta - 25.0).abs() < f64::EPSILON);
        assert!((update.avg_entry_price - 105.0).abs() < f64::EPSILON);
    }

    #[test]
    fn partial_fill_updates_position() {
        let mut manager = PositionManager::default();

        let executor = BinancePerpExecutor::new().with_fill_ratio(0.5);
        let report = futures::executor::block_on(executor.place_order(CexOrderRequest {
            pair: "WAL/USDC".into(),
            side: MarketSide::Bid,
            size: 10.0,
            price: Some(100.0),
            time_in_force: None,
        }))
        .expect("place order");

        let update = manager.apply_execution(&report);
        assert_eq!(update.filled_size, 5.0);
        assert_eq!(update.remaining_size, 5.0);
        assert_eq!(update.net_position, 5.0);
    }
}

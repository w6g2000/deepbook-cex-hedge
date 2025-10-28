use anyhow::{Context, Result, anyhow, ensure};
use async_trait::async_trait;
use shared::{config::CexConfig, types::MarketSide};
use std::{
    collections::HashMap,
    env,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::time::Duration;
use tracing::{debug, error, info, warn};

use binance_sdk::{
    config::ConfigurationRestApi,
    spot::{
        SpotRestApi,
        rest_api::{
            self, DeleteOpenOrdersParams, DeleteOpenOrdersResponseInner, DeleteOrderParams,
            GetAccountParams, GetAccountResponse, NewOrderNewOrderRespTypeEnum, NewOrderParams,
            NewOrderResponse, NewOrderSideEnum, NewOrderTimeInForceEnum, NewOrderTypeEnum,
        },
    },
    wallet::{
        WalletRestApi,
        rest_api::{
            self as wallet_rest_api, WithdrawParams as WalletWithdrawParams,
            WithdrawResponse as WalletWithdrawResponse,
        },
    },
};
use dotenvy::dotenv;
use rust_decimal::{
    Decimal,
    prelude::{FromPrimitive, ToPrimitive},
};

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

#[derive(Debug, Clone, Copy, Default)]
pub struct SpotBalance {
    pub free: f64,
    pub locked: f64,
}

impl SpotBalance {
    pub fn total(&self) -> f64 {
        self.free + self.locked
    }
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

#[async_trait]
trait BinanceWalletClient: Send + Sync + std::fmt::Debug {
    async fn withdraw(&self, params: WalletWithdrawParams) -> Result<WalletWithdrawResponse>;
}

#[derive(Debug, Clone)]
struct DefaultWalletClient {
    client: wallet_rest_api::RestApi,
}

impl DefaultWalletClient {
    fn new(client: wallet_rest_api::RestApi) -> Self {
        Self { client }
    }
}

#[async_trait]
impl BinanceWalletClient for DefaultWalletClient {
    async fn withdraw(&self, params: WalletWithdrawParams) -> Result<WalletWithdrawResponse> {
        let response = self.client.withdraw(params).await?;
        let data = response
            .data()
            .await
            .context("Binance withdraw response parsing failed")?;
        Ok(data)
    }
}

#[async_trait]
trait BinanceSpotClient: Send + Sync + std::fmt::Debug {
    async fn new_order(&self, params: NewOrderParams) -> Result<NewOrderResponse>;
    async fn delete_order(&self, params: DeleteOrderParams) -> Result<()>;
    async fn delete_open_orders(
        &self,
        params: DeleteOpenOrdersParams,
    ) -> Result<Vec<DeleteOpenOrdersResponseInner>>;
    async fn get_account(&self, params: GetAccountParams) -> Result<GetAccountResponse>;
}

#[derive(Debug, Clone)]
struct DefaultSpotClient {
    inner: rest_api::RestApi,
}

impl DefaultSpotClient {
    fn new(inner: rest_api::RestApi) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl BinanceSpotClient for DefaultSpotClient {
    async fn new_order(&self, params: NewOrderParams) -> Result<NewOrderResponse> {
        let response = self.inner.new_order(params).await?;
        let data = response
            .data()
            .await
            .map_err(|err| anyhow!(err.to_string()))
            .context("Binance new_order 解析响应失败")?;
        Ok(data)
    }

    async fn delete_order(&self, params: DeleteOrderParams) -> Result<()> {
        self.inner.delete_order(params).await?;
        Ok(())
    }

    async fn delete_open_orders(
        &self,
        params: DeleteOpenOrdersParams,
    ) -> Result<Vec<DeleteOpenOrdersResponseInner>> {
        let response = self.inner.delete_open_orders(params).await?;
        let data = response
            .data()
            .await
            .map_err(|err| anyhow!(err.to_string()))
            .context("解析 Binance 批量撤单响应失败")?;
        Ok(data)
    }

    async fn get_account(&self, params: GetAccountParams) -> Result<GetAccountResponse> {
        let response = self.inner.get_account(params).await?;
        let data = response
            .data()
            .await
            .map_err(|err| anyhow!(err.to_string()))
            .context("解析 Binance 账户资产失败")?;
        Ok(data)
    }
}

#[derive(Debug, Clone)]
pub struct BinanceSpotExecutor {
    client: Arc<dyn BinanceSpotClient>,
    wallet_client: Arc<dyn BinanceWalletClient>,
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
        let builder = ConfigurationRestApi::builder()
            .api_key(api_key)
            .api_secret(api_secret);

        let recv_window = recv_window_ms
            .map(decimal_from_u64)
            .transpose()
            .context("invalid recv_window_ms value")?;

        let config = builder
            .build()
            .context("failed to build Binance REST configuration")?;

        let spot_conf = config.clone();
        let wallet_conf = config;

        let rest_client = match environment {
            BinanceEnvironment::Production => SpotRestApi::production(spot_conf),
            BinanceEnvironment::Testnet => SpotRestApi::testnet(spot_conf),
        };
        let spot_client: Arc<dyn BinanceSpotClient> = Arc::new(DefaultSpotClient::new(rest_client));

        let wallet_api = WalletRestApi::from_config(wallet_conf);
        let wallet_client: Arc<dyn BinanceWalletClient> =
            Arc::new(DefaultWalletClient::new(wallet_api));

        Ok(Self {
            client: spot_client,
            wallet_client,
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

    fn wallet_recv_window_ms(&self) -> Option<i64> {
        self.recv_window.as_ref().and_then(|value| value.to_i64())
    }

    fn build_withdraw_params(
        &self,
        coin: &str,
        address: &str,
        amount: f64,
        network: Option<&str>,
        address_tag: Option<&str>,
        withdraw_order_id: Option<&str>,
    ) -> Result<WalletWithdrawParams> {
        ensure!(!coin.trim().is_empty(), "coin symbol cannot be empty");
        ensure!(
            !address.trim().is_empty(),
            "withdraw address cannot be empty"
        );
        ensure!(amount > 0.0, "withdraw amount must be positive");

        let decimal_amount = decimal_from_f64(amount, "amount")?;
        let mut builder = WalletWithdrawParams::builder(
            coin.trim().to_uppercase(),
            address.trim().to_string(),
            decimal_amount,
        );

        if let Some(order_id) = withdraw_order_id.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }) {
            builder = builder.withdraw_order_id(order_id);
        }

        if let Some(network) = network.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_uppercase())
            }
        }) {
            builder = builder.network(network);
        }

        if let Some(tag) = address_tag.and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }) {
            builder = builder.address_tag(tag);
        }

        if let Some(recv) = self.wallet_recv_window_ms() {
            builder = builder.recv_window(recv);
        }

        builder
            .build()
            .context("failed to build Binance withdraw params")
    }

    pub async fn withdraw_spot(
        &self,
        coin: &str,
        address: &str,
        amount: f64,
        network: Option<&str>,
        address_tag: Option<&str>,
        withdraw_order_id: Option<&str>,
    ) -> Result<Option<String>> {
        let params = self.build_withdraw_params(
            coin,
            address,
            amount,
            network,
            address_tag,
            withdraw_order_id,
        )?;

        info!(
            coin = %params.coin,
            address = %params.address,
            amount = %params.amount,
            network = ?params.network,
            address_tag = ?params.address_tag,
            withdraw_order_id = ?params.withdraw_order_id,
            "submitting Binance spot withdraw"
        );

        let response = match self.wallet_client.withdraw(params.clone()).await {
            Ok(resp) => resp,
            Err(err) => {
                error!(
                    error = %err,
                    coin = %params.coin,
                    address = %params.address,
                    network = ?params.network,
                    withdraw_order_id = ?params.withdraw_order_id,
                    "Binance spot withdraw failed"
                );
                return Err(err);
            }
        };
        let withdraw_id = response.id.clone();

        info!(
            withdraw_id = withdraw_id.as_deref().unwrap_or(""),
            "Binance spot withdraw accepted"
        );

        Ok(withdraw_id)
    }

    pub async fn fetch_balances(&self) -> Result<HashMap<String, SpotBalance>> {
        let params = GetAccountParams::default();
        let account = self
            .client
            .get_account(params)
            .await
            .context("Binance 查询账户资产失败")?;

        let mut balances = HashMap::new();
        for entry in account.balances.unwrap_or_default() {
            let asset = entry.asset.clone().unwrap_or_default().to_uppercase();
            if asset.is_empty() {
                continue;
            }
            let free = Self::parse_decimal(entry.free.as_ref())?;
            let locked = Self::parse_decimal(entry.locked.as_ref())?;
            balances.insert(asset, SpotBalance { free, locked });
        }
        Ok(balances)
    }

    fn parse_decimal(value: Option<&String>) -> Result<f64> {
        let parsed = value
            .map(|s| {
                s.parse::<f64>()
                    .with_context(|| format!("无法解析数值 {}", s))
            })
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

        let data = self
            .client
            .new_order(params)
            .await
            .context("Binance new_order 请求失败")?;

        let order_symbol = data.symbol.clone().unwrap_or_else(|| symbol.clone());
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

        let slippage_bps = maybe_price
            .map(|p| {
                if p.abs() < f64::EPSILON {
                    0.0
                } else {
                    ((avg_fill_price - p) / p) * 10_000.0
                }
            })
            .unwrap_or(0.0);

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

        let cancelled = response.len();

        info!(pair = %pair, cancelled, "cancelled Binance open orders");
        Ok(cancelled)
    }

    async fn fetch_open_position(&self, pair: &str) -> Result<Option<ExchangePosition>> {
        let base_asset = Self::base_asset(pair)?;

        let params = GetAccountParams::default();
        let account = self
            .client
            .get_account(params)
            .await
            .context("Binance 查询账户资产失败")?;

        let balances = account.balances.unwrap_or_default();
        let balance = balances.into_iter().find(|b| {
            b.asset
                .as_deref()
                .map(|s| s.eq_ignore_ascii_case(&base_asset))
                == Some(true)
        });

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
    Decimal::from_f64(value).ok_or_else(|| anyhow!("{} 数值无法转换为 Decimal：{}", field, value))
}

fn decimal_from_u64(value: u64) -> Result<Decimal> {
    Decimal::from_u64(value).ok_or_else(|| anyhow!("无法转换 recv_window_ms 数值 {}", value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use binance_sdk::spot::rest_api::{
        DeleteOpenOrdersResponseInner, GetAccountResponse, NewOrderResponse,
    };
    use rust_decimal::prelude::ToPrimitive;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
    };

    #[derive(Clone, Copy)]
    struct WithdrawSmokeConfig {
        enabled: bool,
        use_testnet: bool,
        recv_window_ms: Option<u64>,
        api_key: Option<&'static str>,
        api_secret: Option<&'static str>,
        coin: Option<&'static str>,
        address: Option<&'static str>,
        amount: Option<f64>,
        network: Option<&'static str>,
        address_tag: Option<&'static str>,
        withdraw_order_id: Option<&'static str>,
    }

    const WITHDRAW_SMOKE_CONFIG: WithdrawSmokeConfig = WithdrawSmokeConfig {
        enabled: false,
        use_testnet: false,
        recv_window_ms: Some(5000),
        api_key: Some(""),
        api_secret: Some(""),
        coin: Some("WAL"),
        address: Some("0xdf6325562dae096110fc3a87a2d4c8727752a749aa583000dcbd6c42e4fba655"),
        amount: Some(5.0),
        network: Some("SUI"),
        address_tag: None,
        withdraw_order_id: None,
    };

    fn require_str(value: Option<&'static str>, name: &str) -> Result<&'static str> {
        value.ok_or_else(|| anyhow!("{name} not configured in WITHDRAW_SMOKE_CONFIG"))
    }

    fn require_amount(value: Option<f64>, name: &str) -> Result<f64> {
        value.ok_or_else(|| anyhow!("{name} not configured in WITHDRAW_SMOKE_CONFIG"))
    }

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

    #[test]
    fn build_withdraw_params_normalizes_inputs() {
        let executor = BinanceSpotExecutor::new(
            "key".to_string(),
            "secret".to_string(),
            BinanceEnvironment::Testnet,
            Some(5000),
        )
        .expect("executor init");

        let params = executor
            .build_withdraw_params(
                " usdc ",
                " 0xabc123 ",
                42.5,
                Some(" trx "),
                Some(" memo-1 "),
                Some(" order-123 "),
            )
            .expect("build params");

        assert_eq!(params.coin, "USDC");
        assert_eq!(params.address, "0xabc123");
        assert_eq!(
            params.amount,
            decimal_from_f64(42.5, "amount").expect("decimal")
        );
        assert_eq!(params.network.as_deref(), Some("TRX"));
        assert_eq!(params.address_tag.as_deref(), Some("memo-1"));
        assert_eq!(params.withdraw_order_id.as_deref(), Some("order-123"));
        assert_eq!(params.recv_window, Some(5000));
    }

    #[test]
    fn build_withdraw_params_rejects_non_positive_amount() {
        let executor = BinanceSpotExecutor::new(
            "key".to_string(),
            "secret".to_string(),
            BinanceEnvironment::Production,
            None,
        )
        .expect("executor init");

        let err = executor
            .build_withdraw_params("USDC", "0xabc", 0.0, None, None, None)
            .expect_err("zero amount should error");
        assert!(
            err.to_string().contains("withdraw amount must be positive"),
            "unexpected error: {err}"
        );
    }

    #[derive(Debug)]
    struct RecordingWalletClient {
        calls: Mutex<Vec<WalletWithdrawParams>>,
        response: WalletWithdrawResponse,
    }

    impl RecordingWalletClient {
        fn with_id(id: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                response: WalletWithdrawResponse {
                    id: Some(id.to_string()),
                },
            }
        }

        fn take_last(&self) -> WalletWithdrawParams {
            self.calls
                .lock()
                .expect("lock wallet client calls")
                .pop()
                .expect("expected at least one call")
        }
    }

    #[async_trait]
    impl BinanceWalletClient for RecordingWalletClient {
        async fn withdraw(&self, params: WalletWithdrawParams) -> Result<WalletWithdrawResponse> {
            self.calls
                .lock()
                .expect("lock wallet client calls")
                .push(params.clone());
            Ok(self.response.clone())
        }
    }

    #[derive(Debug)]
    struct ErrorWalletClient;

    #[async_trait]
    impl BinanceWalletClient for ErrorWalletClient {
        async fn withdraw(&self, _params: WalletWithdrawParams) -> Result<WalletWithdrawResponse> {
            Err(anyhow!("wallet withdraw failed"))
        }
    }

    #[derive(Debug, Clone)]
    struct RecordedOrder {
        symbol: String,
        side: NewOrderSideEnum,
        order_type: NewOrderTypeEnum,
        time_in_force: Option<NewOrderTimeInForceEnum>,
        quantity: Option<Decimal>,
        price: Option<Decimal>,
    }

    impl RecordedOrder {
        fn from_params(params: NewOrderParams) -> Self {
            Self {
                symbol: params.symbol,
                side: params.side,
                order_type: params.r#type,
                time_in_force: params.time_in_force,
                quantity: params.quantity,
                price: params.price,
            }
        }
    }

    #[derive(Debug)]
    struct RecordingSpotClient {
        calls: Mutex<Vec<RecordedOrder>>,
        responses: Mutex<VecDeque<NewOrderResponse>>,
    }

    impl RecordingSpotClient {
        fn new(responses: Vec<NewOrderResponse>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                responses: Mutex::new(responses.into()),
            }
        }

        fn take_last(&self) -> RecordedOrder {
            self.calls
                .lock()
                .expect("lock spot client calls")
                .pop()
                .expect("expected at least one spot order")
        }
    }

    #[async_trait]
    impl BinanceSpotClient for RecordingSpotClient {
        async fn new_order(&self, params: NewOrderParams) -> Result<NewOrderResponse> {
            let record = RecordedOrder::from_params(params);
            self.calls
                .lock()
                .expect("lock recorded orders")
                .push(record);
            self.responses
                .lock()
                .expect("lock responses")
                .pop_front()
                .ok_or_else(|| anyhow!("no prepared new_order response"))
        }

        async fn delete_order(&self, _params: DeleteOrderParams) -> Result<()> {
            Err(anyhow!(
                "delete_order not implemented in RecordingSpotClient"
            ))
        }

        async fn delete_open_orders(
            &self,
            _params: DeleteOpenOrdersParams,
        ) -> Result<Vec<DeleteOpenOrdersResponseInner>> {
            Err(anyhow!(
                "delete_open_orders not implemented in RecordingSpotClient"
            ))
        }

        async fn get_account(&self, _params: GetAccountParams) -> Result<GetAccountResponse> {
            Err(anyhow!(
                "get_account not implemented in RecordingSpotClient"
            ))
        }
    }

    #[tokio::test]
    async fn withdraw_spot_invokes_wallet_client() {
        let mut executor = BinanceSpotExecutor::new(
            "key".to_string(),
            "secret".to_string(),
            BinanceEnvironment::Testnet,
            Some(5000),
        )
        .expect("executor init");

        let wallet_client = Arc::new(RecordingWalletClient::with_id("withdraw-123"));
        executor.wallet_client = wallet_client.clone();

        let result = executor
            .withdraw_spot(
                " usdc ",
                " 0xabc123 ",
                10.5,
                Some(" trx "),
                Some(" memo "),
                Some(" order-1 "),
            )
            .await
            .expect("withdraw should succeed");

        assert_eq!(result.as_deref(), Some("withdraw-123"));

        let params = wallet_client.take_last();
        assert_eq!(params.coin, "USDC");
        assert_eq!(params.address, "0xabc123");
        assert_eq!(
            params.amount,
            decimal_from_f64(10.5, "amount").expect("decimal")
        );
        assert_eq!(params.network.as_deref(), Some("TRX"));
        assert_eq!(params.address_tag.as_deref(), Some("memo"));
        assert_eq!(params.withdraw_order_id.as_deref(), Some("order-1"));
        assert_eq!(params.recv_window, Some(5000));
    }

    #[tokio::test]
    async fn withdraw_spot_propagates_wallet_errors() {
        let mut executor = BinanceSpotExecutor::new(
            "".to_string(),
            "".to_string(),
            BinanceEnvironment::Production,
            None,
        )
        .expect("executor init");

        executor.wallet_client = Arc::new(ErrorWalletClient);

        let err = executor
            .withdraw_spot(
                "WAL",
                "0xdf6325562dae096110fc3a87a2d4c8727752a749aa583000dcbd6c42e4fba655",
                5.0,
                Some("SUI"),
                None,
                None,
            )
            .await
            .expect_err("expected withdraw error");

        assert!(
            err.to_string().contains("wallet withdraw failed"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn place_market_buy_order_uses_expected_params() {
        let mut response = NewOrderResponse::new();
        response.symbol = Some("WALUSDC".to_string());
        response.order_id = Some(42);
        response.executed_qty = Some("5.00000000".to_string());
        response.cummulative_quote_qty = Some("15.00000000".to_string());

        let client = Arc::new(RecordingSpotClient::new(vec![response]));
        let executor = BinanceSpotExecutor {
            client: client.clone(),
            wallet_client: Arc::new(RecordingWalletClient::with_id("unused")),
            recv_window: None,
            environment: BinanceEnvironment::Testnet,
        };

        let report = executor
            .place_order(CexOrderRequest {
                pair: "WAL/USDC".into(),
                side: MarketSide::Bid,
                size: 5.0,
                price: None,
                time_in_force: None,
            })
            .await
            .expect("place order");

        assert_eq!(report.order_id, "WALUSDC:42");
        assert_eq!(report.pair, "WAL/USDC");
        assert_eq!(report.side, MarketSide::Bid);
        assert!((report.filled_size - 5.0).abs() < f64::EPSILON);
        assert!(report.remaining_size.abs() < f64::EPSILON);
        assert!((report.avg_fill_price - 3.0).abs() < f64::EPSILON);
        assert_eq!(report.slippage_bps, 0.0);

        let recorded = client.take_last();
        assert_eq!(recorded.symbol, "WALUSDC");
        assert!(matches!(recorded.side, NewOrderSideEnum::Buy));
        assert!(matches!(recorded.order_type, NewOrderTypeEnum::Market));
        assert!(recorded.time_in_force.is_none());
        assert!(recorded.price.is_none());

        let quantity = recorded
            .quantity
            .expect("market order should set quantity")
            .to_f64()
            .expect("decimal to f64");
        assert!((quantity - 5.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn place_market_sell_order_uses_expected_params() {
        let mut response = NewOrderResponse::new();
        response.symbol = Some("WALUSDC".to_string());
        response.client_order_id = Some("sell-1".to_string());
        response.executed_qty = Some("2.00000000".to_string());
        response.cummulative_quote_qty = Some("620.00000000".to_string());

        let client = Arc::new(RecordingSpotClient::new(vec![response]));
        let executor = BinanceSpotExecutor {
            client: client.clone(),
            wallet_client: Arc::new(RecordingWalletClient::with_id("unused")),
            recv_window: None,
            environment: BinanceEnvironment::Testnet,
        };

        let report = executor
            .place_order(CexOrderRequest {
                pair: "WAL/USDC".into(),
                side: MarketSide::Ask,
                size: 2.0,
                price: None,
                time_in_force: None,
            })
            .await
            .expect("place order");

        assert_eq!(report.order_id, "WALUSDC:sell-1");
        assert_eq!(report.pair, "WAL/USDC");
        assert_eq!(report.side, MarketSide::Ask);
        assert!((report.filled_size - 2.0).abs() < f64::EPSILON);
        assert!(report.remaining_size.abs() < f64::EPSILON);
        assert!((report.avg_fill_price - 310.0).abs() < f64::EPSILON);
        assert_eq!(report.slippage_bps, 0.0);

        let recorded = client.take_last();
        assert_eq!(recorded.symbol, "WALUSDC");
        assert!(matches!(recorded.side, NewOrderSideEnum::Sell));
        assert!(matches!(recorded.order_type, NewOrderTypeEnum::Market));
        assert!(recorded.time_in_force.is_none());
        assert!(recorded.price.is_none());

        let quantity = recorded
            .quantity
            .expect("market order should set quantity")
            .to_f64()
            .expect("decimal to f64");
        assert!((quantity - 2.0).abs() < f64::EPSILON);
    }

    fn env_flag(key: &str) -> bool {
        std::env::var(key)
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes"
                )
            })
            .unwrap_or(false)
    }

    fn env_f64(key: &str) -> Option<f64> {
        std::env::var(key).ok()?.parse().ok()
    }

    fn env_u64(key: &str) -> Option<u64> {
        std::env::var(key).ok()?.parse().ok()
    }

    fn resolve_secret(env_key: &str) -> Result<String> {
        let value = std::env::var(env_key).map_err(|_| anyhow!("环境变量 {env_key} 未设置"))?;

        let trimmed = value.trim();
        ensure!(!trimmed.is_empty(), "环境变量 {env_key} 为空");
        Ok(trimmed.to_string())
    }

    fn build_live_executor() -> Result<BinanceSpotExecutor> {
        let api_key = resolve_secret("BINANCE_API_KEY").context("Binance API Key 未配置")?;
        let api_secret =
            resolve_secret("BINANCE_API_SECRET").context("Binance API Secret 未配置")?;

        let environment = if env_flag("BINANCE_USE_TESTNET") {
            BinanceEnvironment::Testnet
        } else {
            BinanceEnvironment::Production
        };

        let recv_window = env_u64("BINANCE_RECV_WINDOW_MS");

        BinanceSpotExecutor::new(api_key, api_secret, environment, recv_window)
    }

    #[tokio::test]
    async fn place_market_buy_order_live() -> Result<()> {
        let _ = dotenv();
        if !env_flag("BINANCE_LIVE_TEST") {
            println!("跳过 place_market_buy_order_live（设置 BINANCE_LIVE_TEST=1 才会真实下单）");
            return Ok(());
        }

        let pair = std::env::var("BINANCE_TEST_PAIR").unwrap_or_else(|_| "WAL/USDC".to_string());
        let size = env_f64("BINANCE_TEST_BUY_SIZE").unwrap_or(5.0);

        let executor = build_live_executor().context("初始化 Binance 执行器失败")?;
        let report = executor
            .place_order(CexOrderRequest {
                pair: pair.clone(),
                side: MarketSide::Bid,
                size,
                price: None,
                time_in_force: None,
            })
            .await
            .context("place_market_buy_order_live 下单失败")?;

        println!(
            "买入下单成功: id={}, pair={}, filled={}, price={}",
            report.order_id, report.pair, report.filled_size, report.avg_fill_price
        );

        ensure!(report.pair == pair, "返回的交易对与请求不一致");
        ensure!(
            report.side == MarketSide::Bid,
            "返回的方向应为 Bid，但得到 {:?}",
            report.side
        );
        ensure!(report.filled_size > 0.0, "filled_size 应大于 0");
        ensure!(report.avg_fill_price > 0.0, "avg_fill_price 应大于 0");
        Ok(())
    }

    #[tokio::test]
    async fn place_market_sell_order_live() -> Result<()> {
        let _ = dotenv();
        if !env_flag("BINANCE_LIVE_TEST") {
            println!("跳过 place_market_sell_order_live（设置 BINANCE_LIVE_TEST=1 才会真实下单）");
            return Ok(());
        }

        let pair = std::env::var("BINANCE_TEST_PAIR").unwrap_or_else(|_| "WAL/USDC".to_string());
        let size = env_f64("BINANCE_TEST_SELL_SIZE").unwrap_or(2.0);

        let executor = build_live_executor().context("初始化 Binance 执行器失败")?;
        let report = executor
            .place_order(CexOrderRequest {
                pair: pair.clone(),
                side: MarketSide::Ask,
                size,
                price: None,
                time_in_force: None,
            })
            .await
            .context("place_market_sell_order_live 下单失败")?;

        println!(
            "卖出下单成功: id={}, pair={}, filled={}, price={}",
            report.order_id, report.pair, report.filled_size, report.avg_fill_price
        );

        ensure!(report.pair == pair, "返回的交易对与请求不一致");
        ensure!(
            report.side == MarketSide::Ask,
            "返回的方向应为 Ask，但得到 {:?}",
            report.side
        );
        ensure!(report.filled_size > 0.0, "filled_size 应大于 0");
        ensure!(report.avg_fill_price > 0.0, "avg_fill_price 应大于 0");
        Ok(())
    }

    #[tokio::test]
    async fn fetch_balances_live_snapshot() -> Result<()> {
        let _ = dotenv();
        if !env_flag("BINANCE_LIVE_TEST") {
            println!(
                "跳过 fetch_balances_live_snapshot（设置 BINANCE_LIVE_TEST=1 并提供 API 凭证才会真实查询）"
            );
            return Ok(());
        }

        let api_key = std::env::var("BINANCE_API_KEY").unwrap_or_default();
        let api_secret = std::env::var("BINANCE_API_SECRET").unwrap_or_default();
        if api_key.trim().is_empty() || api_secret.trim().is_empty() {
            println!(
                "跳过 fetch_balances_live_snapshot（未配置 BINANCE_API_KEY / BINANCE_API_SECRET）"
            );
            return Ok(());
        }

        let executor = build_live_executor().context("初始化 Binance 执行器失败")?;
        let balances = executor
            .fetch_balances()
            .await
            .context("fetch_balances 查询失败")?;

        if balances.is_empty() {
            println!("账户资产为空");
        } else {
            let mut entries: Vec<_> = balances.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.cmp(b));
            println!("账户资产列表:");
            for (symbol, balance) in entries {
                println!(
                    "  {} -> free: {}, locked: {}",
                    symbol, balance.free, balance.locked
                );
            }
        }

        if let Ok(expected) = std::env::var("BINANCE_EXPECT_BALANCE_ASSET") {
            let symbol = expected.trim().to_uppercase();
            ensure!(
                balances.contains_key(&symbol),
                "账户资产中未找到预期币种 {symbol}"
            );
        }

        println!("成功获取 {} 个资产余额", balances.len());
        Ok(())
    }

    #[tokio::test]
    async fn withdraw_spot_real_environment_smoke() -> Result<()> {
        let _ = dotenv();

        let config = WITHDRAW_SMOKE_CONFIG;

        if !config.enabled {
            info!(
                "skipping real withdraw smoke test; set WITHDRAW_SMOKE_CONFIG.enabled = true to run"
            );
            return Ok(());
        }

        let executor = BinanceSpotExecutor::new(
            require_str(config.api_key, "api_key")?.to_string(),
            require_str(config.api_secret, "api_secret")?.to_string(),
            BinanceEnvironment::from_flag(config.use_testnet),
            config.recv_window_ms,
        )?;

        let withdraw_id = executor
            .withdraw_spot(
                require_str(config.coin, "coin")?,
                require_str(config.address, "address")?,
                require_amount(config.amount, "amount")?,
                config.network.map(|value| value as &str),
                config.address_tag.map(|value| value as &str),
                config.withdraw_order_id.map(|value| value as &str),
            )
            .await
            .context("Binance withdraw request failed")?;

        ensure!(
            withdraw_id.is_some(),
            "Binance withdraw response missing id"
        );

        info!(
            id = withdraw_id.as_deref(),
            "real Binance withdraw smoke test succeeded"
        );

        Ok(())
    }
}

use anyhow::{Result, ensure};
use async_trait::async_trait;
use shared::types::MarketSide;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{debug, info};

mod sdk_backend;

pub use sdk_backend::{DeepbookEnvironment, DeepbookSdkConfig};

pub type OrderId = String;

#[derive(Debug, Clone)]
pub struct PlaceOrderRequest {
    pub pair: String,
    pub pool_key: String,
    pub side: MarketSide,
    pub price: f64,
    pub size: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderStatus {
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Expired,
}

impl OrderStatus {
    fn is_active(&self) -> bool {
        matches!(self, OrderStatus::Open | OrderStatus::PartiallyFilled)
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            OrderStatus::Filled | OrderStatus::Cancelled | OrderStatus::Expired
        )
    }
}

#[derive(Debug, Clone)]
pub struct OrderSnapshot {
    pub id: OrderId,
    pub pair: String,
    pub side: MarketSide,
    pub price: f64,
    pub size: f64,
    pub filled_base: f64,
    pub filled_quote: f64,
    pub status: OrderStatus,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FillUpdate {
    pub base_filled: f64,
    pub quote_filled: f64,
}

impl FillUpdate {
    pub fn zeros() -> Self {
        Self {
            base_filled: 0.0,
            quote_filled: 0.0,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClaimableBalance {
    pub base_asset: f64,
    pub quote_asset: f64,
}

#[derive(Debug, Clone, Default)]
pub struct ClaimSummary {
    pub per_pair: HashMap<String, ClaimableBalance>,
    pub total_base: f64,
    pub total_quote: f64,
}

impl ClaimSummary {
    fn from_map(map: HashMap<String, ClaimableBalance>) -> Self {
        let mut total_base = 0.0;
        let mut total_quote = 0.0;
        for balance in map.values() {
            total_base += balance.base_asset;
            total_quote += balance.quote_asset;
        }
        Self {
            per_pair: map,
            total_base,
            total_quote,
        }
    }
}

#[async_trait]
pub trait DeepbookBackend: Send + Sync {
    async fn place_post_only_order(&self, request: PlaceOrderRequest) -> Result<OrderSnapshot>;
    async fn cancel_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>>;
    async fn cancel_all(&self) -> Result<Vec<OrderSnapshot>>;
    async fn record_fill(&self, order_id: &str, fill: FillUpdate) -> Result<OrderSnapshot>;
    async fn claim_fills(&self) -> Result<ClaimSummary>;
    async fn get_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>>;
    async fn list_orders(&self) -> Result<Vec<OrderSnapshot>>;
}

#[derive(Clone)]
pub struct DeepbookExecution {
    backend: Arc<dyn DeepbookBackend>,
}

impl DeepbookExecution {
    pub fn with_backend<B>(backend: B) -> Self
    where
        B: DeepbookBackend + 'static,
    {
        Self {
            backend: Arc::new(backend),
        }
    }

    pub fn in_memory() -> Self {
        Self::with_backend(InMemoryBackend::new())
    }

    pub async fn with_sdk(config: DeepbookSdkConfig) -> Result<Self> {
        let backend = sdk_backend::SdkBackend::new(config).await?;
        Ok(Self::with_backend(backend))
    }

    pub fn backend(&self) -> Arc<dyn DeepbookBackend> {
        Arc::clone(&self.backend)
    }

    pub async fn place_post_only_order(&self, request: PlaceOrderRequest) -> Result<OrderSnapshot> {
        self.backend.place_post_only_order(request).await
    }

    pub async fn cancel_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>> {
        self.backend.cancel_order(order_id).await
    }

    pub async fn cancel_all(&self) -> Result<Vec<OrderSnapshot>> {
        self.backend.cancel_all().await
    }

    pub async fn record_fill(&self, order_id: &str, fill: FillUpdate) -> Result<OrderSnapshot> {
        self.backend.record_fill(order_id, fill).await
    }

    pub async fn claim_fills(&self) -> Result<ClaimSummary> {
        self.backend.claim_fills().await
    }

    pub async fn get_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>> {
        self.backend.get_order(order_id).await
    }

    pub async fn list_orders(&self) -> Result<Vec<OrderSnapshot>> {
        self.backend.list_orders().await
    }
}

impl Default for DeepbookExecution {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl std::fmt::Debug for DeepbookExecution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeepbookExecution").finish()
    }
}

// TODO: 当前 SdkBackend 仍有若干待完善项：
// - 下单后通过 open-order 差集推导 order_id，存在并发误判风险，最好直接解析链上返回事件。
// - order_index 只维护本地状态，链上手工撤单/过期后不会自动清理，需补同步或刷新机制。
// - record_fill 目前仅 Query 订单，尚未对接实际成交事件，策略端暂时无法拿到 fill 明细。
// - claim_fills 调用 claim_rebates，与策略期望的成交资产 claim 有差异，后续需补全。
// - pool_key 仍由策略按字符串拼接生成，建议落在配置文件，方便多池维护。
// - Gas coin 选择过于简单，仅取第一枚 SUI，后续可加重试或配置化。
// - 缺少集成测试验证 place/cancel 等链路，建议补 devnet/dry-run 用例防止回归。

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("order {0} not found")]
    OrderNotFound(String),
}

pub struct InMemoryBackend {
    inner: Arc<Inner>,
}

impl InMemoryBackend {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }
}

struct Inner {
    orders: Mutex<HashMap<OrderId, DeepbookOrder>>,
    claimables: Mutex<HashMap<String, ClaimableBalance>>,
    id_counter: std::sync::atomic::AtomicU64,
}

impl Inner {
    fn new() -> Self {
        Self {
            orders: Mutex::new(HashMap::new()),
            claimables: Mutex::new(HashMap::new()),
            id_counter: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn next_order_id(&self) -> OrderId {
        let id = self
            .id_counter
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        format!("dbk-order-{id}")
    }
}

#[async_trait]
impl DeepbookBackend for InMemoryBackend {
    async fn place_post_only_order(&self, request: PlaceOrderRequest) -> Result<OrderSnapshot> {
        ensure!(request.size > 0.0, "order size must be positive");
        ensure!(request.price > 0.0, "order price must be positive");

        let now = SystemTime::now();
        let order_id = self.inner.next_order_id();
        let order = DeepbookOrder::new(order_id.clone(), request, now);
        let snapshot = order.snapshot();

        {
            let mut orders = self.inner.orders.lock().await;
            orders.insert(order_id.clone(), order);
        }

        info!(
            order_id = %order_id,
            pair = %snapshot.pair,
            side = ?snapshot.side,
            price = snapshot.price,
            size = snapshot.size,
            "placed deepbook post-only order (in-memory backend)"
        );

        Ok(snapshot)
    }

    async fn cancel_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>> {
        let mut orders = self.inner.orders.lock().await;
        let Some(order) = orders.get_mut(order_id) else {
            return Err(ExecutionError::OrderNotFound(order_id.to_string()).into());
        };

        if order.status.is_terminal() {
            return Ok(None);
        }

        order.status = OrderStatus::Cancelled;
        order.updated_at = SystemTime::now();
        info!(
            order_id = %order.id,
            pair = %order.pair,
            "cancelled deepbook order (in-memory backend)"
        );
        Ok(Some(order.snapshot()))
    }

    async fn cancel_all(&self) -> Result<Vec<OrderSnapshot>> {
        let mut orders = self.inner.orders.lock().await;
        let mut cancelled = Vec::new();
        for order in orders.values_mut() {
            if order.status.is_active() {
                order.status = OrderStatus::Cancelled;
                order.updated_at = SystemTime::now();
                cancelled.push(order.snapshot());
            }
        }

        if !cancelled.is_empty() {
            info!(
                count = cancelled.len(),
                "cancelled all active deepbook orders"
            );
        }

        Ok(cancelled)
    }

    async fn record_fill(&self, order_id: &str, fill: FillUpdate) -> Result<OrderSnapshot> {
        ensure!(
            fill.base_filled >= 0.0 && fill.quote_filled >= 0.0,
            "fill amounts must be non-negative"
        );

        let (snapshot, pair, side, quote_delta) = {
            let mut orders = self.inner.orders.lock().await;
            let order = orders
                .get_mut(order_id)
                .ok_or_else(|| ExecutionError::OrderNotFound(order_id.to_string()))?;

            let prev_filled = order.filled_base;
            order.filled_base = (order.filled_base + fill.base_filled).min(order.size);
            order.filled_quote += fill.quote_filled;
            order.updated_at = SystemTime::now();

            if order.filled_base >= order.size {
                order.status = OrderStatus::Filled;
            } else if order.filled_base > 0.0 || fill.quote_filled > 0.0 {
                order.status = OrderStatus::PartiallyFilled;
            }

            if order.status == OrderStatus::Filled && prev_filled < order.size {
                info!(
                    order_id = %order.id,
                    pair = %order.pair,
                    "deepbook order fully filled"
                );
            } else {
                debug!(
                    order_id = %order.id,
                    pair = %order.pair,
                    filled = order.filled_base,
                    "recorded deepbook partial fill"
                );
            }

            (
                order.snapshot(),
                order.pair.clone(),
                order.side.clone(),
                fill.quote_filled,
            )
        };

        if fill.base_filled > 0.0 || quote_delta > 0.0 {
            let mut claims = self.inner.claimables.lock().await;
            let balance = claims.entry(pair).or_default();
            match side {
                MarketSide::Bid => balance.base_asset += fill.base_filled,
                MarketSide::Ask => balance.quote_asset += quote_delta,
            }
        }

        Ok(snapshot)
    }

    async fn claim_fills(&self) -> Result<ClaimSummary> {
        let mut claims = self.inner.claimables.lock().await;
        if claims.is_empty() {
            return Ok(ClaimSummary::default());
        }

        let snapshot = claims.clone();
        claims.clear();
        Ok(ClaimSummary::from_map(snapshot))
    }

    async fn get_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>> {
        let orders = self.inner.orders.lock().await;
        Ok(orders.get(order_id).map(|order| order.snapshot()))
    }

    async fn list_orders(&self) -> Result<Vec<OrderSnapshot>> {
        let orders = self.inner.orders.lock().await;
        Ok(orders.values().map(|order| order.snapshot()).collect())
    }
}

#[derive(Debug, Clone)]
struct DeepbookOrder {
    id: OrderId,
    pair: String,
    side: MarketSide,
    price: f64,
    size: f64,
    filled_base: f64,
    filled_quote: f64,
    status: OrderStatus,
    created_at: SystemTime,
    updated_at: SystemTime,
}

impl DeepbookOrder {
    fn new(id: OrderId, request: PlaceOrderRequest, now: SystemTime) -> Self {
        Self {
            id,
            pair: request.pair,
            side: request.side,
            price: request.price,
            size: request.size,
            filled_base: 0.0,
            filled_quote: 0.0,
            status: OrderStatus::Open,
            created_at: now,
            updated_at: now,
        }
    }

    fn snapshot(&self) -> OrderSnapshot {
        OrderSnapshot {
            id: self.id.clone(),
            pair: self.pair.clone(),
            side: self.side.clone(),
            price: self.price,
            size: self.size,
            filled_base: self.filled_base,
            filled_quote: self.filled_quote,
            status: self.status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request(side: MarketSide) -> PlaceOrderRequest {
        PlaceOrderRequest {
            pair: "WAL/USDC".to_string(),
            pool_key: "WAL_USDC".to_string(),
            side,
            price: 1.2,
            size: 10.0,
        }
    }

    #[tokio::test]
    async fn place_order_tracks_state() {
        let execution = DeepbookExecution::default();
        let snapshot = execution
            .place_post_only_order(sample_request(MarketSide::Bid))
            .await
            .expect("place order");

        assert_eq!(snapshot.size, 10.0);
        let stored = execution
            .get_order(&snapshot.id)
            .await
            .expect("get order")
            .expect("stored");
        assert_eq!(stored.status, OrderStatus::Open);
        assert_eq!(stored.pair, "WAL/USDC");
    }

    #[tokio::test]
    async fn cancel_all_closes_orders() {
        let execution = DeepbookExecution::default();
        let first = execution
            .place_post_only_order(sample_request(MarketSide::Bid))
            .await
            .expect("place first");
        let second = execution
            .place_post_only_order(sample_request(MarketSide::Ask))
            .await
            .expect("place second");

        let cancelled = execution.cancel_all().await.expect("cancel all");
        assert_eq!(cancelled.len(), 2);

        let refreshed = execution.list_orders().await.expect("list orders");
        let status_first = refreshed
            .iter()
            .find(|order| order.id == first.id)
            .expect("first")
            .status
            .clone();
        let status_second = refreshed
            .iter()
            .find(|order| order.id == second.id)
            .expect("second")
            .status
            .clone();

        assert_eq!(status_first, OrderStatus::Cancelled);
        assert_eq!(status_second, OrderStatus::Cancelled);
    }

    #[tokio::test]
    async fn claim_fills_accumulates_balances() {
        let execution = DeepbookExecution::default();
        let buy = execution
            .place_post_only_order(sample_request(MarketSide::Bid))
            .await
            .expect("buy");
        let mut sell_request = sample_request(MarketSide::Ask);
        sell_request.price = 1.3;
        let sell = execution
            .place_post_only_order(sell_request)
            .await
            .expect("sell");

        execution
            .record_fill(
                &buy.id,
                FillUpdate {
                    base_filled: 4.0,
                    quote_filled: 0.0,
                },
            )
            .await
            .expect("record buy fill");
        execution
            .record_fill(
                &sell.id,
                FillUpdate {
                    base_filled: 2.0,
                    quote_filled: 2.6,
                },
            )
            .await
            .expect("record sell fill");

        let summary = execution.claim_fills().await.expect("claim");
        assert_eq!(summary.total_base, 4.0);
        assert_eq!(summary.total_quote, 2.6);
        let balance = summary.per_pair.get("WAL/USDC").expect("pair balance");
        assert_eq!(balance.base_asset, 4.0);
        assert_eq!(balance.quote_asset, 2.6);

        let summary_after = execution.claim_fills().await.expect("claim again");
        assert_eq!(summary_after.total_base, 0.0);
        assert_eq!(summary_after.total_quote, 0.0);
    }
}

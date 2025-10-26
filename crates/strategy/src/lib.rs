use anyhow::{Result, anyhow};
use cex_execution::BinanceSpotExecutor;
use deepbook_execution::{DeepbookExecution, OrderSnapshot, PlaceOrderRequest};
use lending::LendingClient;
use shared::config::{AppConfig, ArbitrageConfig, StrategyConfig, TradingPair};
use shared::metrics::HealthMetrics;
use shared::types::{MarketEvent, MarketSide, MarketVenue, OrderBookLevel, OrderCommand};
use std::collections::HashMap;
use std::mem;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Interval, MissedTickBehavior, interval};
use tracing::{debug, info, warn};

mod risk;
use risk::{LoggerNotifier, RiskManager};

const DEFAULT_MAX_ORDER_SIZE: f64 = 100.0;
const DEFAULT_MIN_BOOK_DEPTH: f64 = 500.0;
const DEFAULT_CEX_VOLATILITY_BPS: f64 = 150.0;
const DEFAULT_REBID_THRESHOLD_BPS: f64 = 5.0;
const DEFAULT_MIN_SPREAD_BPS: f64 = 10.0;
const LENDING_INTERVAL: Duration = Duration::from_secs(30);
const ORDER_POLL_INTERVAL: Duration = Duration::from_secs(1);
const LADDER_LEVELS: usize = 2;
const SPOT_WITHDRAW_THRESHOLD_ENV: &str = "SPOT_WITHDRAW_THRESHOLD_BPS";

#[derive(Clone)]
pub struct StrategyEngine {
    params: StrategyParams,
    arbitrage: ArbitrageConfig,
    pairs: Vec<TradingPair>,
    deepbook: DeepbookExecution,
    lending: LendingClient,
    order_tx: mpsc::Sender<OrderCommand>,
    risk: RiskManager<LoggerNotifier>,
    metrics: Arc<HealthMetrics>,
    spot_executor: Option<Arc<BinanceSpotExecutor>>,
    rebalance_min_notional: f64,
    rebalance_cooldown: Duration,
    borrow_alert_ratio: f64,
    borrow_target_ratio: f64,
    deposit_address: Option<String>,
    deposit_network: Option<String>,
    deposit_memo: Option<String>,
}

impl StrategyEngine {
    pub fn new(
        config: &AppConfig,
        deepbook: DeepbookExecution,
        lending: LendingClient,
        order_tx: mpsc::Sender<OrderCommand>,
        metrics: Arc<HealthMetrics>,
        spot_executor: Option<Arc<BinanceSpotExecutor>>,
    ) -> Self {
        let strategy = config.strategy.clone();
        let arbitrage = config.arbitrage.clone();
        let params = StrategyParams::from_config(&strategy, &arbitrage);
        let pairs = select_pairs(config);
        let rebalance = config.rebalance.clone();
        let rebalance_min_notional = rebalance.min_notional();
        let rebalance_cooldown = Duration::from_secs(rebalance.cooldown_secs());
        let borrow_alert_ratio = rebalance.borrow_alert_ratio();
        let borrow_target_ratio = rebalance.borrow_target_ratio();

        Self {
            params,
            arbitrage,
            pairs,
            deepbook,
            lending,
            order_tx,
            risk: RiskManager::new(LoggerNotifier),
            metrics,
            spot_executor,
            rebalance_min_notional,
            rebalance_cooldown,
            borrow_alert_ratio,
            borrow_target_ratio,
            deposit_address: rebalance.deposit_address,
            deposit_network: rebalance.deposit_network,
            deposit_memo: rebalance.deposit_memo,
        }
    }

    pub fn spawn(
        self,
        cex_events: mpsc::Receiver<MarketEvent>,
        deepbook_events: mpsc::Receiver<MarketEvent>,
    ) -> StrategyHandle {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join =
            tokio::spawn(async move { self.run(cex_events, deepbook_events, shutdown_rx).await });
        StrategyHandle { shutdown_tx, join }
    }

    async fn run(
        mut self,
        mut cex_rx: mpsc::Receiver<MarketEvent>,
        mut deepbook_rx: mpsc::Receiver<MarketEvent>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<()> {
        info!("strategy engine started");
        self.bootstrap_financing().await?;

        let mut pair_states = self
            .pairs
            .iter()
            .map(|pair| {
                let label = pair.display_pair();
                (label.clone(), StrategyPairState::new(label))
            })
            .collect::<HashMap<_, _>>();

        let mut lending_timer = strategy_interval(LENDING_INTERVAL);
        let mut order_poll_timer = strategy_interval(ORDER_POLL_INTERVAL);

        loop {
            tokio::select! {
                Some(event) = cex_rx.recv() => {
                    self.handle_market_event(event, VenueKind::Cex, &mut pair_states).await?;
                }
                Some(event) = deepbook_rx.recv() => {
                    self.handle_market_event(event, VenueKind::Deepbook, &mut pair_states).await?;
                }
                _ = lending_timer.tick() => {
                    self.rebalance_lending().await?;
                }
                _ = order_poll_timer.tick() => {
                    self.poll_active_orders(&mut pair_states).await?;
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_ok() && *shutdown_rx.borrow() {
                        info!("strategy engine received shutdown signal");
                        self.graceful_stop(&mut pair_states).await?;
                        break;
                    }
                }
                else => {
                    info!("strategy engine exiting - market channels closed");
                    break;
                }
            }
        }

        info!("strategy engine shutdown complete");
        Ok(())
    }

    async fn handle_market_event(
        &self,
        event: MarketEvent,
        venue: VenueKind,
        pair_states: &mut HashMap<String, StrategyPairState>,
    ) -> Result<()> {
        let label = event.pair.clone();
        let entry = pair_states
            .entry(label.clone())
            .or_insert_with(|| StrategyPairState::new(label.clone()));

        match venue {
            VenueKind::Cex => {
                entry.apply_cex_event(event);
                self.metrics.note_cex_event(SystemTime::now()).await;
            }
            VenueKind::Deepbook => {
                entry.apply_deepbook_event(event);
                self.metrics.note_deepbook_event(SystemTime::now()).await;
            }
        }

        self.evaluate_pair(entry).await
    }

    async fn evaluate_pair(&self, state: &mut StrategyPairState) -> Result<()> {
        if state.is_suspended() {
            return Ok(());
        }

        if state.cex_mid().is_none() || state.deepbook_mid().is_none() {
            return Ok(());
        }

        let cex_mid = state.cex_mid().unwrap();
        let deep_mid = state.deepbook_mid().unwrap();

        if let Some(prev_mid) = state.prev_cex_mid() {
            let diff_bps = price_diff_bps(prev_mid, cex_mid);
            if diff_bps >= self.params.cex_volatility_bps {
                self.risk
                    .price_spike(&state.pair_label, diff_bps, self.params.cex_volatility_bps)
                    .await;
                self.cancel_all_orders(state).await?;
                debug!(
                    pair = %state.pair_label,
                    diff_bps,
                    "halt placement due to price spike"
                );
                return Ok(());
            }
        }

        let spread_bps = price_diff_bps(cex_mid, deep_mid);
        if spread_bps < self.min_spread_bps() {
            if state.has_active_orders() {
                self.cancel_all_orders(state).await?;
            }
            debug!(
                pair = %state.pair_label,
                spread_bps,
                "skip placement: spread below threshold"
            );
            return Ok(());
        }

        if !state.has_depth(self.params.min_book_depth) {
            self.risk
                .depth_insufficient(&state.pair_label, self.params.min_book_depth)
                .await;
            debug!(
                pair = %state.pair_label,
                depth = self.params.min_book_depth,
                "skip placement: insufficient deepbook depth"
            );
            self.cancel_all_orders(state).await?;
            return Ok(());
        }

        if !state.within_volatility(self.params.cex_volatility_bps) {
            debug!(
                pair = %state.pair_label,
                "skip placement: cex volatility above threshold"
            );
            self.cancel_all_orders(state).await?;
            return Ok(());
        }

        let desired = self.build_desired_orders(cex_mid);
        if desired.is_empty() {
            self.cancel_all_orders(state).await?;
            return Ok(());
        }

        if state.should_reprice(cex_mid, self.params.rebid_threshold_bps) {
            self.cancel_all_orders(state).await?;
        } else {
            self.drop_orders_not_desired(state, &desired).await?;
        }

        for desired_order in desired {
            let level = desired_order.level;
            let side = desired_order.side.clone();
            if let Some(active) = state.order_mut(level, side.clone()) {
                let diff = price_diff_bps(active.snapshot.price, desired_order.price);
                if diff < self.params.rebid_threshold_bps {
                    continue;
                }
                self.cancel_order(&active.snapshot).await?;
                state.remove_order(level, side.clone());
            }

            let request = PlaceOrderRequest {
                pair: state.pair_label.clone(),
                pool_key: self.pool_key_for_pair(&state.pair_label),
                side: side.clone(),
                price: desired_order.price,
                size: desired_order.size,
            };
            let snapshot = self.deepbook.place_post_only_order(request).await?;
            state.add_order(level, side, snapshot);
        }

        state.set_reference_mid(cex_mid);
        Ok(())
    }

    async fn graceful_stop(
        &self,
        pair_states: &mut HashMap<String, StrategyPairState>,
    ) -> Result<()> {
        for state in pair_states.values_mut() {
            self.cancel_all_orders(state).await?;
        }
        Ok(())
    }

    async fn drop_orders_not_desired(
        &self,
        state: &mut StrategyPairState,
        desired: &[DesiredOrder],
    ) -> Result<()> {
        let mut removals = Vec::new();
        for active in state.orders.iter() {
            if !desired.iter().any(|order| order.matches(active)) {
                self.cancel_order(&active.snapshot).await?;
                removals.push((active.level, active.side.clone()));
            }
        }
        for (level, side) in removals {
            state.remove_order(level, side);
        }
        Ok(())
    }

    async fn cancel_all_orders(&self, state: &mut StrategyPairState) -> Result<()> {
        if state.orders.is_empty() {
            return Ok(());
        }
        let identifiers = state
            .orders
            .iter()
            .map(|order| order.snapshot.id.clone())
            .collect::<Vec<_>>();
        for order_id in identifiers {
            let _ = self.deepbook.cancel_order(&order_id).await?;
        }
        state.clear_orders();
        Ok(())
    }

    async fn cancel_order(&self, snapshot: &OrderSnapshot) -> Result<()> {
        let _ = self.deepbook.cancel_order(&snapshot.id).await?;
        Ok(())
    }

    fn build_desired_orders(&self, cex_mid: f64) -> Vec<DesiredOrder> {
        if cex_mid <= 0.0 {
            return Vec::new();
        }

        let levels = self.params.ladder_levels;
        if levels == 0 {
            return Vec::new();
        }

        let per_level_size = self.order_size_per_level();
        if per_level_size <= 0.0 {
            return Vec::new();
        }

        let base_spread_bps = self.min_spread_bps().max(1.0);
        let base_spread_abs = (base_spread_bps / 10_000.0) * cex_mid;

        let mut orders = Vec::new();
        for level in 0..levels {
            let multiplier = (level + 1) as f64;
            let offset = base_spread_abs * multiplier;
            let bid_price = (cex_mid - offset).max(0.0);
            let ask_price = cex_mid + offset;
            orders.push(DesiredOrder {
                level,
                side: MarketSide::Bid,
                price: bid_price,
                size: per_level_size,
            });
            orders.push(DesiredOrder {
                level,
                side: MarketSide::Ask,
                price: ask_price,
                size: per_level_size,
            });
        }
        orders
    }

    fn min_spread_bps(&self) -> f64 {
        if self.arbitrage.min_spread_bps > 0.0 {
            self.arbitrage.min_spread_bps
        } else {
            DEFAULT_MIN_SPREAD_BPS
        }
    }

    fn order_size_per_level(&self) -> f64 {
        let max_order_size = self.params.max_order_size;
        let configured_position = self.arbitrage.max_position;
        let per_side_cap = if configured_position > 0.0 {
            configured_position / 2.0
        } else {
            max_order_size
        };
        let levels = self.params.ladder_levels.max(1) as f64;
        (max_order_size.min(per_side_cap) / levels).max(0.0)
    }

    fn pool_key_for_pair(&self, pair_label: &str) -> String {
        pair_label.replace('/', "_").to_uppercase()
    }

    async fn poll_active_orders(
        &self,
        pair_states: &mut HashMap<String, StrategyPairState>,
    ) -> Result<()> {
        let open_orders = self.deepbook.list_orders().await?;
        let mut indexed = HashMap::new();
        for order in open_orders {
            indexed.insert(order.id.clone(), order);
        }

        let mut withdraw_pairs = Vec::new();

        for state in pair_states.values_mut() {
            let mut removals = Vec::new();
            let mut hedges = Vec::new();
            let latest_mid = state.latest_mid();
            let mut needs_withdraw = false;

            for active in state.orders.iter_mut() {
                if let Some(snapshot) = indexed.get(&active.snapshot.id) {
                    let snapshot = snapshot.clone();
                    let fill_delta = snapshot.filled_base - active.last_reported_base;
                    if fill_delta > 0.0 {
                        let hedge_side = match snapshot.side.clone() {
                            MarketSide::Bid => MarketSide::Ask,
                            MarketSide::Ask => MarketSide::Bid,
                        };
                        let price_hint = latest_mid.or(Some(snapshot.price));
                        hedges.push((state.pair_label.clone(), hedge_side, fill_delta, price_hint));
                        needs_withdraw = true;
                    }
                    active.last_reported_base = snapshot.filled_base;
                    active.snapshot = snapshot.clone();
                    if !snapshot.status.is_active() {
                        removals.push((active.level, active.side.clone()));
                    }
                } else {
                    // TODO: 下单后立即轮询可能暂时查不到订单，必要时增加重试确认。
                }
            }

            for (pair_label, side, size, price) in hedges {
                self.send_hedge_command(&pair_label, side, size, price)
                    .await?;
            }

            for (level, side) in removals {
                state.remove_order(level, side);
            }

            if needs_withdraw {
                withdraw_pairs.push(state.pair_label.clone());
            }
        }

        for pair_label in withdraw_pairs {
            self.withdraw_settled_for_pair(&pair_label).await?;
        }

        self.evaluate_inventory(pair_states).await?;

        Ok(())
    }

    async fn send_hedge_command(
        &self,
        pair_label: &str,
        side: MarketSide,
        size: f64,
        price: Option<f64>,
    ) -> Result<()> {
        if size <= 0.0 {
            return Ok(());
        }
        let command = OrderCommand {
            venue: MarketVenue::BinanceSpot,
            pair: pair_label.to_string(),
            side,
            size,
            price,
        };
        if let Err(err) = self.order_tx.clone().send(command).await {
            warn!(error = %err, "failed to enqueue hedge command");
        }
        Ok(())
    }

    async fn withdraw_settled_for_pair(&self, pair_label: &str) -> Result<()> {
        let pool_key = self.pool_key_for_pair(pair_label);
        if let Some(balance) = self.deepbook.withdraw_settled_amounts(&pool_key).await? {
            info!(
                pair = %pair_label,
                base = balance.base_asset,
                quote = balance.quote_asset,
                "withdrew settled balances"
            );
        }
        Ok(())
    }

    async fn evaluate_inventory(
        &self,
        pair_states: &mut HashMap<String, StrategyPairState>,
    ) -> Result<()> {
        let Some(spot) = &self.spot_executor else {
            return Ok(());
        };

        let balances = spot.fetch_balances().await?;
        let now = Instant::now();

        for pair in &self.pairs {
            let pair_label = pair.display_pair();
            let state = pair_states
                .entry(pair_label.clone())
                .or_insert_with(|| StrategyPairState::new(pair_label.clone()));

            let Some(mid) = state.cex_mid() else {
                continue;
            };
            if mid <= 0.0 {
                continue;
            }

            let pool_key = self.pool_key_for_pair(&pair_label);
            let snapshot = self.deepbook.pool_account_snapshot(&pool_key).await?;

            let dex_base =
                (snapshot.settled_base + snapshot.unclaimed_base - snapshot.owed_base).max(0.0);
            let dex_quote =
                (snapshot.settled_quote + snapshot.unclaimed_quote - snapshot.owed_quote).max(0.0);

            let base_symbol = pair.base.to_uppercase();
            let quote_symbol = pair.quote.to_uppercase();

            let cex_base = balances.get(&base_symbol).map(|b| b.total()).unwrap_or(0.0);
            let cex_quote = balances
                .get(&quote_symbol)
                .map(|b| b.total())
                .unwrap_or(0.0);

            let dex_value = dex_base * mid + dex_quote;
            let cex_value = cex_base * mid + cex_quote;
            let total_value = dex_value + cex_value;
            if total_value <= f64::EPSILON {
                continue;
            }

            let dex_share = dex_value / total_value;

            if dex_share > 0.6 {
                let value_to_move = dex_value - (total_value * 0.5);
                self.record_rebalance_to_cex(state, value_to_move, dex_base, dex_quote, mid);
            } else if dex_share < 0.4 {
                let value_to_move = (total_value * 0.5) - dex_value;
                self.record_rebalance_to_dex(state, value_to_move, cex_base, cex_quote, mid);
            }

            if self.should_execute(state.pending_to_cex(), state.last_rebalance_at, now) {
                self.execute_rebalance_to_cex(pair, state, now).await?;
            }

            if self.should_execute(state.pending_to_dex(), state.last_rebalance_at, now) {
                self.execute_rebalance_to_dex(pair, state, now).await?;
            }
        }

        Ok(())
    }

    fn record_rebalance_to_cex(
        &self,
        state: &mut StrategyPairState,
        value_to_move: f64,
        available_base: f64,
        available_quote: f64,
        mid: f64,
    ) {
        if value_to_move <= f64::EPSILON || mid <= f64::EPSILON {
            return;
        }

        let current_pending = state.pending_to_cex().notional;
        if value_to_move <= current_pending + f64::EPSILON {
            return;
        }

        let additional_value = value_to_move - current_pending;

        let mut base_amount = (additional_value / mid).min(available_base.max(0.0));
        if !base_amount.is_finite() {
            base_amount = 0.0;
        }
        let mut moved_value = base_amount * mid;
        let mut quote_amount = 0.0;

        if moved_value + 1e-6 < additional_value {
            let remain = additional_value - moved_value;
            quote_amount = remain.min(available_quote.max(0.0));
            moved_value += quote_amount;
        }

        if moved_value <= f64::EPSILON {
            return;
        }

        state
            .pending_to_cex_mut()
            .add(base_amount, quote_amount, moved_value);
    }

    fn record_rebalance_to_dex(
        &self,
        state: &mut StrategyPairState,
        value_to_move: f64,
        available_base: f64,
        available_quote: f64,
        mid: f64,
    ) {
        if value_to_move <= f64::EPSILON || mid <= f64::EPSILON {
            return;
        }

        let current_pending = state.pending_to_dex().notional;
        if value_to_move <= current_pending + f64::EPSILON {
            return;
        }

        let additional_value = value_to_move - current_pending;

        let mut base_amount = (additional_value / mid).min(available_base.max(0.0));
        if !base_amount.is_finite() {
            base_amount = 0.0;
        }
        let mut moved_value = base_amount * mid;
        let mut quote_amount = 0.0;

        if moved_value + 1e-6 < additional_value {
            let remain = additional_value - moved_value;
            quote_amount = remain.min(available_quote.max(0.0));
            moved_value += quote_amount;
        }

        if moved_value <= f64::EPSILON {
            return;
        }

        state
            .pending_to_dex_mut()
            .add(base_amount, quote_amount, moved_value);
    }

    fn should_execute(
        &self,
        pending: &PendingTransfer,
        last_execution: Option<Instant>,
        now: Instant,
    ) -> bool {
        if pending.notional < self.rebalance_min_notional {
            return false;
        }

        match last_execution {
            Some(ts) => now.duration_since(ts) >= self.rebalance_cooldown,
            None => true,
        }
    }

    async fn execute_rebalance_to_cex(
        &self,
        _pair: &TradingPair,
        state: &mut StrategyPairState,
        now: Instant,
    ) -> Result<()> {
        let pending = state.take_pending_to_cex();
        if pending.is_empty() {
            return Ok(());
        }

        state.suspend();
        self.cancel_all_orders(state).await?;

        if let Err(err) = self
            .transfer_to_cex(&state.pair_label, pending.base_amount, pending.quote_amount)
            .await
        {
            warn!(
                pair = %state.pair_label,
                error = %err,
                base = pending.base_amount,
                quote = pending.quote_amount,
                "dex to cex transfer failed; will retry"
            );
            state.pending_to_cex_mut().add(
                pending.base_amount,
                pending.quote_amount,
                pending.notional,
            );
            state.last_rebalance_at = Some(now);
            state.resume();
            return Ok(());
        }

        state.last_rebalance_at = Some(now);
        state.resume();
        info!(
            pair = %state.pair_label,
            base = pending.base_amount,
            quote = pending.quote_amount,
            notional = pending.notional,
            "completed dex to cex rebalance"
        );
        Ok(())
    }

    async fn execute_rebalance_to_dex(
        &self,
        pair: &TradingPair,
        state: &mut StrategyPairState,
        now: Instant,
    ) -> Result<()> {
        let pending = state.take_pending_to_dex();
        if pending.is_empty() {
            return Ok(());
        }

        let Some(executor) = &self.spot_executor else {
            state.pending_to_dex_mut().add(
                pending.base_amount,
                pending.quote_amount,
                pending.notional,
            );
            return Ok(());
        };

        let Some(address) = self.deposit_address.as_deref() else {
            warn!(
                pair = %state.pair_label,
                "missing CEX deposit address; skipping cex->dex rebalance"
            );
            state.pending_to_dex_mut().add(
                pending.base_amount,
                pending.quote_amount,
                pending.notional,
            );
            return Ok(());
        };

        state.suspend();
        self.cancel_all_orders(state).await?;

        let network = self.deposit_network.as_deref();
        let memo = self.deposit_memo.as_deref();
        let mut success = true;

        if pending.base_amount > 0.0 {
            if let Err(err) = executor
                .withdraw_spot(
                    &pair.base.to_uppercase(),
                    address,
                    pending.base_amount,
                    network,
                    memo,
                    None,
                )
                .await
            {
                warn!(
                    pair = %state.pair_label,
                    base = pending.base_amount,
                    error = %err,
                    "cex withdraw for base asset failed"
                );
                success = false;
            }
        }

        if pending.quote_amount > 0.0 {
            if let Err(err) = executor
                .withdraw_spot(
                    &pair.quote.to_uppercase(),
                    address,
                    pending.quote_amount,
                    network,
                    memo,
                    None,
                )
                .await
            {
                warn!(
                    pair = %state.pair_label,
                    quote = pending.quote_amount,
                    error = %err,
                    "cex withdraw for quote asset failed"
                );
                success = false;
            }
        }

        if success {
            state.last_rebalance_at = Some(now);
            info!(
                pair = %state.pair_label,
                base = pending.base_amount,
                quote = pending.quote_amount,
                notional = pending.notional,
                "completed cex to dex rebalance"
            );
        } else {
            state.pending_to_dex_mut().add(
                pending.base_amount,
                pending.quote_amount,
                pending.notional,
            );
            state.last_rebalance_at = Some(now);
        }

        state.resume();
        Ok(())
    }

    async fn transfer_to_cex(
        &self,
        pair_label: &str,
        base_amount: f64,
        quote_amount: f64,
    ) -> Result<()> {
        if base_amount <= f64::EPSILON && quote_amount <= f64::EPSILON {
            return Ok(());
        }
        anyhow::bail!(
            "dex->cex transfer for {pair_label} not implemented (base={base_amount}, quote={quote_amount})"
        );
    }

    async fn rebalance_lending(&self) -> Result<()> {
        let health = self.lending.account_health().await?;
        self.record_spot_withdrawal_probe(health.borrow_ratio);

        if health.borrow_ratio >= self.borrow_alert_ratio {
            self.risk
                .borrow_health(health.borrow_ratio, self.borrow_alert_ratio)
                .await;
            info!(
                borrow_ratio = health.borrow_ratio,
                alert_ratio = self.borrow_alert_ratio,
                target_ratio = self.borrow_target_ratio,
                "borrow ratio above alert; repaying to target"
            );
            self.lending
                .repay_to_target(self.borrow_target_ratio)
                .await?;
            return Ok(());
        }

        if health.borrow_ratio + 1e-6 < self.borrow_target_ratio {
            let adjustment = self.params.max_order_size.max(1.0) * 0.25;
            info!(
                borrow_ratio = health.borrow_ratio,
                target_ratio = self.borrow_target_ratio,
                "borrow ratio below target; borrowing to restore balance"
            );
            self.lending.borrow_asset(adjustment).await?;
        }
        Ok(())
    }

    async fn bootstrap_financing(&mut self) -> Result<()> {
        if self.params.max_order_size <= 0.0 {
            return Ok(());
        }

        let deposit_amount = self.params.max_order_size * 0.5;
        self.lending.deposit_collateral(deposit_amount).await?;
        let borrow_amount = self.params.max_order_size * 0.25;
        self.lending.borrow_asset(borrow_amount).await?;
        Ok(())
    }

    fn record_spot_withdrawal_probe(&self, borrow_ratio: f64) {
        let threshold = self.params.spot_withdraw_threshold;
        // TODO: 在接入 CEX 余额查询后改为比较实际现货库存比例，并触发自动提现。
        match threshold {
            Some(value) => info!(
                borrow_ratio,
                threshold_bps = value,
                "recording spot withdrawal probe"
            ),
            None => info!(
                borrow_ratio,
                "spot withdrawal threshold not configured; probe logged for observability"
            ),
        }
    }
}

pub struct StrategyHandle {
    shutdown_tx: watch::Sender<bool>,
    join: JoinHandle<Result<()>>,
}

impl StrategyHandle {
    pub async fn shutdown(self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        match self.join.await {
            Ok(res) => res,
            Err(err) => Err(anyhow!("strategy task join error: {}", err)),
        }
    }

    pub fn abort(&self) {
        self.join.abort();
    }
}

#[derive(Clone)]
struct StrategyParams {
    max_order_size: f64,
    min_book_depth: f64,
    cex_volatility_bps: f64,
    rebid_threshold_bps: f64,
    ladder_levels: usize,
    spot_withdraw_threshold: Option<f64>,
}

impl StrategyParams {
    fn from_config(strategy: &StrategyConfig, arbitrage: &ArbitrageConfig) -> Self {
        let max_order_size = match strategy.max_order_size {
            Some(value) if value > 0.0 => value,
            _ if arbitrage.max_position > 0.0 => arbitrage.max_position,
            _ => DEFAULT_MAX_ORDER_SIZE,
        };
        let min_book_depth = strategy.min_book_depth.unwrap_or(DEFAULT_MIN_BOOK_DEPTH);
        let cex_volatility_bps = strategy
            .cex_volatility_bps
            .unwrap_or(DEFAULT_CEX_VOLATILITY_BPS);
        let rebid_threshold_bps = strategy
            .rebid_threshold_bps
            .unwrap_or(DEFAULT_REBID_THRESHOLD_BPS);

        Self {
            max_order_size,
            min_book_depth,
            cex_volatility_bps,
            rebid_threshold_bps,
            ladder_levels: LADDER_LEVELS,
            spot_withdraw_threshold: resolve_spot_withdraw_threshold(strategy),
        }
    }
}

struct StrategyPairState {
    pair_label: String,
    cex_event: Option<MarketEvent>,
    deepbook_event: Option<MarketEvent>,
    prev_cex_mid: Option<f64>,
    last_cex_mid: Option<f64>,
    last_deepbook_mid: Option<f64>,
    reference_mid: Option<f64>,
    volatility_mid: Option<f64>,
    orders: Vec<ActiveOrder>,
    pending_to_cex: PendingTransfer,
    pending_to_dex: PendingTransfer,
    last_rebalance_at: Option<Instant>,
    suspended: bool,
}

impl StrategyPairState {
    fn new(pair_label: String) -> Self {
        Self {
            pair_label,
            cex_event: None,
            deepbook_event: None,
            prev_cex_mid: None,
            last_cex_mid: None,
            last_deepbook_mid: None,
            reference_mid: None,
            volatility_mid: None,
            orders: Vec::new(),
            pending_to_cex: PendingTransfer::default(),
            pending_to_dex: PendingTransfer::default(),
            last_rebalance_at: None,
            suspended: false,
        }
    }

    fn apply_cex_event(&mut self, event: MarketEvent) {
        self.prev_cex_mid = self.last_cex_mid;
        self.last_cex_mid = compute_mid(&event);
        self.cex_event = Some(event);
    }

    fn apply_deepbook_event(&mut self, event: MarketEvent) {
        self.last_deepbook_mid = compute_mid(&event);
        self.deepbook_event = Some(event);
    }

    fn cex_mid(&self) -> Option<f64> {
        self.last_cex_mid
    }

    fn prev_cex_mid(&self) -> Option<f64> {
        self.prev_cex_mid
    }

    fn deepbook_mid(&self) -> Option<f64> {
        self.last_deepbook_mid
    }

    fn latest_mid(&self) -> Option<f64> {
        self.last_cex_mid.or(self.last_deepbook_mid)
    }

    fn within_volatility(&mut self, limit_bps: f64) -> bool {
        let Some(current_mid) = self.last_cex_mid else {
            return true;
        };
        let prev_mid = self.volatility_mid.get_or_insert(current_mid);
        let diff = price_diff_bps(*prev_mid, current_mid);
        self.volatility_mid = Some(current_mid);
        diff <= limit_bps
    }

    fn should_reprice(&self, current_mid: f64, threshold_bps: f64) -> bool {
        let Some(reference) = self.reference_mid else {
            return true;
        };
        let diff = price_diff_bps(reference, current_mid);
        diff >= threshold_bps
    }

    fn has_depth(&self, required: f64) -> bool {
        if let Some(event) = &self.deepbook_event {
            total_depth(&event.bids) >= required && total_depth(&event.asks) >= required
        } else {
            false
        }
    }

    fn has_active_orders(&self) -> bool {
        !self.orders.is_empty()
    }

    fn is_suspended(&self) -> bool {
        self.suspended
    }

    fn suspend(&mut self) {
        self.suspended = true;
    }

    fn resume(&mut self) {
        self.suspended = false;
    }

    fn pending_to_cex(&self) -> &PendingTransfer {
        &self.pending_to_cex
    }

    fn pending_to_dex(&self) -> &PendingTransfer {
        &self.pending_to_dex
    }

    fn pending_to_cex_mut(&mut self) -> &mut PendingTransfer {
        &mut self.pending_to_cex
    }

    fn pending_to_dex_mut(&mut self) -> &mut PendingTransfer {
        &mut self.pending_to_dex
    }

    fn take_pending_to_cex(&mut self) -> PendingTransfer {
        mem::take(&mut self.pending_to_cex)
    }

    fn take_pending_to_dex(&mut self) -> PendingTransfer {
        mem::take(&mut self.pending_to_dex)
    }

    fn add_order(&mut self, level: usize, side: MarketSide, snapshot: OrderSnapshot) {
        let last_reported = snapshot.filled_base;
        self.orders.push(ActiveOrder {
            level,
            side,
            snapshot,
            last_reported_base: last_reported,
        });
    }

    fn order_mut(&mut self, level: usize, side: MarketSide) -> Option<&mut ActiveOrder> {
        self.orders
            .iter_mut()
            .find(|order| order.level == level && order.side == side)
    }

    fn remove_order(&mut self, level: usize, side: MarketSide) {
        self.orders
            .retain(|order| !(order.level == level && order.side == side));
    }

    fn clear_orders(&mut self) {
        self.orders.clear();
        self.reference_mid = None;
    }

    fn set_reference_mid(&mut self, mid: f64) {
        self.reference_mid = Some(mid);
    }
}

struct ActiveOrder {
    level: usize,
    side: MarketSide,
    snapshot: OrderSnapshot,
    last_reported_base: f64,
}

#[derive(Clone)]
struct DesiredOrder {
    level: usize,
    side: MarketSide,
    price: f64,
    size: f64,
}

impl DesiredOrder {
    fn matches(&self, order: &ActiveOrder) -> bool {
        self.level == order.level && self.side == order.side
    }
}

#[derive(Debug, Default, Clone)]
struct PendingTransfer {
    notional: f64,
    base_amount: f64,
    quote_amount: f64,
}

impl PendingTransfer {
    fn add(&mut self, base: f64, quote: f64, notional: f64) {
        if base > 0.0 {
            self.base_amount += base;
        }
        if quote > 0.0 {
            self.quote_amount += quote;
        }
        if notional > 0.0 {
            self.notional += notional;
        }
    }

    fn is_empty(&self) -> bool {
        self.notional <= f64::EPSILON
    }
}

#[derive(Clone, Copy)]
enum VenueKind {
    Cex,
    Deepbook,
}

fn strategy_interval(period: Duration) -> Interval {
    let mut intv = interval(period);
    intv.set_missed_tick_behavior(MissedTickBehavior::Delay);
    intv
}

fn select_pairs(config: &AppConfig) -> Vec<TradingPair> {
    if !config.pairs.is_empty() {
        return config.pairs.clone();
    }

    vec![TradingPair {
        base: "WAL".to_string(),
        quote: "USDC".to_string(),
    }]
}

fn compute_mid(event: &MarketEvent) -> Option<f64> {
    let bid = best_bid(event);
    let ask = best_ask(event);
    match (bid, ask) {
        (Some(bid), Some(ask)) if bid > 0.0 && ask > 0.0 => Some((bid + ask) / 2.0),
        _ => None,
    }
}

fn best_bid(event: &MarketEvent) -> Option<f64> {
    event
        .bids
        .iter()
        .map(|level| level.price)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn best_ask(event: &MarketEvent) -> Option<f64> {
    event
        .asks
        .iter()
        .map(|level| level.price)
        .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
}

fn total_depth(levels: &[OrderBookLevel]) -> f64 {
    levels.iter().map(|level| level.size).sum()
}

fn price_diff_bps(a: f64, b: f64) -> f64 {
    if a <= 0.0 || b <= 0.0 {
        return 0.0;
    }
    ((a - b).abs() / b) * 10_000.0
}

fn resolve_spot_withdraw_threshold(config: &StrategyConfig) -> Option<f64> {
    if let Ok(value) = std::env::var(SPOT_WITHDRAW_THRESHOLD_ENV) {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return None;
        }
        match trimmed.parse::<f64>() {
            Ok(parsed) if parsed >= 0.0 => return Some(parsed),
            Ok(parsed) => {
                warn!(
                    env = SPOT_WITHDRAW_THRESHOLD_ENV,
                    parsed, "ignoring negative spot withdraw threshold"
                );
            }
            Err(err) => {
                warn!(
                    env = SPOT_WITHDRAW_THRESHOLD_ENV,
                    error = %err,
                    raw = trimmed,
                    "failed to parse spot withdraw threshold from env"
                );
            }
        }
    }
    match config.spot_withdraw_threshold_bps {
        Some(value) if value >= 0.0 => Some(value),
        Some(value) => {
            warn!(
                config_value = value,
                "ignoring negative spot withdraw threshold from config"
            );
            None
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::types::{MarketEvent, MarketSide, MarketVenue, OrderBookLevel};
    use tokio::sync::mpsc;

    fn sample_event(bid: f64, ask: f64) -> MarketEvent {
        MarketEvent {
            venue: MarketVenue::BinanceSpot,
            pair: "WAL/USDC".to_string(),
            bids: vec![OrderBookLevel {
                price: bid,
                size: 10.0,
            }],
            asks: vec![OrderBookLevel {
                price: ask,
                size: 10.0,
            }],
            ts_ms: 0,
        }
    }

    #[test]
    fn diff_bps_calculates() {
        let diff = price_diff_bps(101.0, 100.0);
        assert!((diff - 100.0).abs() < f64::EPSILON);
        let same = price_diff_bps(100.0, 100.0);
        assert_eq!(same, 0.0);
    }

    #[test]
    fn pair_state_updates_mid() {
        let mut state = StrategyPairState::new("WAL/USDC".to_string());
        state.apply_cex_event(sample_event(99.0, 101.0));
        assert_eq!(state.cex_mid().unwrap(), 100.0);
        state.apply_deepbook_event(sample_event(100.0, 102.0));
        assert_eq!(state.deepbook_mid().unwrap(), 101.0);
    }

    #[test]
    fn desired_order_matches() {
        let order = ActiveOrder {
            level: 1,
            side: MarketSide::Ask,
            snapshot: OrderSnapshot {
                id: "1".into(),
                pair: "WAL/USDC".into(),
                side: MarketSide::Ask,
                price: 1.0,
                size: 1.0,
                filled_base: 0.0,
                filled_quote: 0.0,
                status: deepbook_execution::OrderStatus::Open,
                created_at: std::time::SystemTime::now(),
                updated_at: std::time::SystemTime::now(),
            },
            last_reported_base: 0.0,
        };
        let desired = DesiredOrder {
            level: 1,
            side: MarketSide::Ask,
            price: 1.0,
            size: 1.0,
        };
        assert!(desired.matches(&order));
    }

    #[tokio::test]
    async fn evaluate_pair_places_orders_when_spread_sufficient() {
        let config = AppConfig {
            strategy: StrategyConfig {
                max_order_size: Some(10.0),
                min_book_depth: Some(5.0),
                ..Default::default()
            },
            arbitrage: ArbitrageConfig {
                min_spread_bps: 5.0,
                max_position: 10.0,
            },
            ..Default::default()
        };

        let (order_tx, _order_rx) = mpsc::channel(8);
        let health = HealthMetrics::new();
        let engine = StrategyEngine::new(
            &config,
            DeepbookExecution::default(),
            LendingClient::from_config(&config),
            order_tx,
            Arc::clone(&health),
            None,
        );

        let mut state = StrategyPairState::new("WAL/USDC".into());
        state.apply_cex_event(sample_event(100.0, 100.2));
        state.apply_deepbook_event(sample_event(98.0, 99.0));

        engine
            .evaluate_pair(&mut state)
            .await
            .expect("evaluate pair");

        assert!(
            state.has_active_orders(),
            "orders should be active after evaluation"
        );
    }

    #[tokio::test]
    async fn evaluate_pair_skips_when_depth_insufficient() {
        let config = AppConfig {
            strategy: StrategyConfig {
                max_order_size: Some(10.0),
                min_book_depth: Some(1_000.0),
                ..Default::default()
            },
            arbitrage: ArbitrageConfig {
                min_spread_bps: 5.0,
                max_position: 10.0,
            },
            ..Default::default()
        };

        let (order_tx, mut order_rx) = mpsc::channel(8);
        let health = HealthMetrics::new();
        let engine = StrategyEngine::new(
            &config,
            DeepbookExecution::default(),
            LendingClient::from_config(&config),
            order_tx,
            Arc::clone(&health),
            None,
        );

        let mut state = StrategyPairState::new("WAL/USDC".into());
        state.apply_cex_event(sample_event(100.0, 100.1));
        state.apply_deepbook_event(MarketEvent {
            venue: MarketVenue::Deepbook,
            pair: "WAL/USDC".into(),
            bids: vec![OrderBookLevel {
                price: 99.5,
                size: 1.0,
            }],
            asks: vec![OrderBookLevel {
                price: 100.5,
                size: 1.0,
            }],
            ts_ms: 0,
        });

        engine
            .evaluate_pair(&mut state)
            .await
            .expect("evaluate pair");

        assert!(
            order_rx.try_recv().is_err(),
            "no orders expected when depth insufficient"
        );
    }
}

use anyhow::{Context, Result};
use cex_execution::{
    CexExecutionReport, CexExecutor, CexOrderRequest, PositionManager, PositionUpdate,
};
use shared::types::{MarketVenue, OrderCommand};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

const MAX_ORDER_RETRIES: usize = 3;

pub struct ExecutionEngine {
    command_tx: mpsc::Sender<OrderCommand>,
}

impl ExecutionEngine {
    pub fn new(buffer: usize) -> (Self, mpsc::Receiver<OrderCommand>) {
        let (command_tx, command_rx) = mpsc::channel(buffer);
        (Self { command_tx }, command_rx)
    }

    pub fn command_sender(&self) -> mpsc::Sender<OrderCommand> {
        self.command_tx.clone()
    }
}

pub fn spawn_order_router(
    order_rx: mpsc::Receiver<OrderCommand>,
    executor: Arc<dyn CexExecutor>,
    positions: Arc<Mutex<PositionManager>>,
) -> JoinHandle<()> {
    let router = OrderRouter::new(executor, positions, MAX_ORDER_RETRIES);
    tokio::spawn(async move { router.run(order_rx).await })
}

struct OrderRouter {
    executor: Arc<dyn CexExecutor>,
    positions: Arc<Mutex<PositionManager>>,
    max_retries: usize,
}

impl OrderRouter {
    fn new(
        executor: Arc<dyn CexExecutor>,
        positions: Arc<Mutex<PositionManager>>,
        max_retries: usize,
    ) -> Self {
        Self {
            executor,
            positions,
            max_retries,
        }
    }

    async fn run(self, mut order_rx: mpsc::Receiver<OrderCommand>) {
        info!("cex order router started");
        let mut router = self;
        while let Some(command) = order_rx.recv().await {
            if let Err(err) = router.process_command(command).await {
                warn!(error = %err, "failed to process cex order command");
            }
        }
        info!("cex order router exiting");
    }

    async fn process_command(&mut self, command: OrderCommand) -> Result<()> {
        if command.venue != MarketVenue::BinanceSpot {
            warn!(?command.venue, "unsupported venue for cex order router");
            return Ok(());
        }

        if command.size <= 0.0 {
            warn!(
                pair = %command.pair,
                size = command.size,
                "ignored cex order with non-positive size"
            );
            return Ok(());
        }

        let pair = command.pair.clone();
        let side = command.side.clone();
        let mut remaining = command.size;
        let mut attempts = 0usize;

        while remaining > 0.0 && attempts <= self.max_retries {
            let report = self
                .executor
                .place_order(CexOrderRequest {
                    pair: pair.clone(),
                    side: side.clone(),
                    size: remaining,
                    price: command.price,
                    time_in_force: None,
                })
                .await
                .with_context(|| format!("place order on binance for {}", pair))?;

            let update = self.apply_execution(report).await;
            remaining = update.remaining_size;

            if remaining <= 0.0 {
                break;
            }

            attempts += 1;
        }

        if remaining > 0.0 {
            warn!(
                pair = %pair,
                side = ?side,
                remaining,
                "cex order not fully filled after retries"
            );
        }

        Ok(())
    }

    async fn apply_execution(&self, report: CexExecutionReport) -> PositionUpdate {
        let mut guard = self.positions.lock().await;
        let update = guard.apply_execution(&report);
        drop(guard);

        info!(
            pair = %update.pair,
            side = ?update.side,
            filled = update.filled_size,
            remaining = update.remaining_size,
            net_position = update.net_position,
            avg_entry = update.avg_entry_price,
            realized_delta = update.realized_pnl_delta,
            realized_total = update.realized_pnl_total,
            "processed cex execution report"
        );

        update
    }
}

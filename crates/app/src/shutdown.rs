use anyhow::{Context, Result, anyhow};
use cex_execution::PositionManager;
use serde_json::json;
use shared::config::ShutdownConfig;
use shared::metrics::HealthMetrics;
use shared::types::{MarketSide, MarketVenue, OrderCommand};
use strategy::StrategyHandle;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

use std::fs;
use std::sync::Arc;

pub struct ShutdownCoordinator {
    strategy: StrategyHandle,
    deepbook: deepbook_execution::DeepbookExecution,
    lending: lending::LendingClient,
    order_tx: mpsc::Sender<OrderCommand>,
    positions: Arc<Mutex<PositionManager>>,
    health: Arc<HealthMetrics>,
    cancel_timeout: Duration,
    repay_timeout: Duration,
    close_timeout: Duration,
}

impl ShutdownCoordinator {
    pub fn new(
        config: &ShutdownConfig,
        strategy: StrategyHandle,
        deepbook: deepbook_execution::DeepbookExecution,
        lending: lending::LendingClient,
        order_tx: mpsc::Sender<OrderCommand>,
        positions: Arc<Mutex<PositionManager>>,
        health: Arc<HealthMetrics>,
    ) -> Self {
        Self {
            strategy,
            deepbook,
            lending,
            order_tx,
            positions,
            health,
            cancel_timeout: duration_from_ms(config.cancel_timeout_ms, 5_000),
            repay_timeout: duration_from_ms(config.repay_timeout_ms, 7_000),
            close_timeout: duration_from_ms(config.close_hedge_timeout_ms, 7_000),
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        let ShutdownCoordinator {
            strategy,
            deepbook,
            lending,
            order_tx,
            positions,
            health,
            cancel_timeout,
            repay_timeout,
            close_timeout,
        } = self;

        info!("shutdown coordinator: pausing strategy and cancelling orders");
        timeout(cancel_timeout, strategy.shutdown())
            .await
            .map_err(|_| anyhow!("strategy shutdown timed out after {:?}", cancel_timeout))??;

        let cancel_res = timeout(cancel_timeout, deepbook.cancel_all())
            .await
            .map_err(|_| anyhow!("deepbook cancel_all timed out after {:?}", cancel_timeout))?
            .context("cancel deepbook orders")?;
        if !cancel_res.is_empty() {
            info!(
                count = cancel_res.len(),
                "deepbook orders cancelled during shutdown"
            );
        }

        info!("shutdown coordinator: settling lending positions");
        let lending_client = lending.clone();
        timeout(repay_timeout, async move {
            let health = lending_client.account_health().await?;
            let repay_amount = (health.borrow_ratio.max(0.1) * 100.0).max(1.0);
            lending_client.repay_asset(repay_amount).await?;
            lending_client
                .withdraw_collateral(repay_amount * 0.5)
                .await?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| anyhow!("lending settlement timed out after {:?}", repay_timeout))??;

        info!("shutdown coordinator: flattening CEX hedge positions");
        let order_tx_clone = order_tx.clone();
        let positions_arc = Arc::clone(&positions);
        timeout(close_timeout, async move {
            let snapshot = positions_arc.lock().await.snapshot();
            for (pair, position) in snapshot {
                let net = position.net_position;
                if net.abs() < f64::EPSILON {
                    continue;
                }
                let side = if net > 0.0 {
                    MarketSide::Ask
                } else {
                    MarketSide::Bid
                };
                let size = net.abs();
                order_tx_clone
                    .send(OrderCommand {
                        venue: MarketVenue::BinanceSpot,
                        pair: pair.clone(),
                        side,
                        size,
                        price: Some(position.avg_entry_price),
                    })
                    .await
                    .with_context(|| format!("send hedge close command for {}", pair))?;
                warn!(
                    pair = %pair,
                    net_position = net,
                    "issued hedge close command during shutdown"
                );
            }
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| {
            anyhow!(
                "closing hedge positions timed out after {:?}",
                close_timeout
            )
        })??;

        let final_health = lending.account_health().await?;
        let health_snapshot = health.snapshot().await;
        let remaining_positions = positions.lock().await.snapshot();
        let open_positions = remaining_positions
            .values()
            .filter(|state| state.net_position.abs() > f64::EPSILON)
            .count();
        let summary = json!({
            "final_borrow_ratio": final_health.borrow_ratio,
            "max_borrow_ratio": final_health.max_borrow_ratio,
            "last_cex_event_ms": health_snapshot.last_cex_event_ms,
            "last_deepbook_event_ms": health_snapshot.last_deepbook_event_ms,
            "remaining_positions": open_positions,
        });
        fs::write(
            "shutdown_summary.json",
            serde_json::to_string_pretty(&summary)?,
        )?;

        info!("shutdown coordinator completed all steps");
        Ok(())
    }
}

fn duration_from_ms(opt: Option<u64>, default_ms: u64) -> Duration {
    Duration::from_millis(opt.unwrap_or(default_ms))
}

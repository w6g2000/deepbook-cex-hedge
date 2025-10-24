use anyhow::Result;
use cex_execution::{BinancePerpExecutor, PositionManager};
use deepbook_cex_app::ShutdownCoordinator;
use deepbook_execution::DeepbookExecution;
use execution::{ExecutionEngine, spawn_order_router};
use lending::LendingClient;
use shared::config::AppConfig;
use shared::metrics::HealthMetrics;
use shared::types::{MarketEvent, MarketVenue, OrderBookLevel, OrderCommand};
use std::sync::Arc;
use strategy::StrategyEngine;
use tokio::sync::{Mutex, mpsc};
use tokio::time::{Duration, sleep};

#[tokio::test]
async fn end_to_end_place_and_shutdown() -> Result<()> {
    let config = AppConfig::default();
    let deepbook_executor = DeepbookExecution::default();
    let lending_client = LendingClient::from_config(&config);
    let health_metrics = HealthMetrics::new();

    let (engine, order_rx) = ExecutionEngine::new(16);
    let strategy = StrategyEngine::new(
        &config,
        deepbook_executor.clone(),
        lending_client.clone(),
        engine.command_sender(),
        Arc::clone(&health_metrics),
    );
    let (tx_cex, rx_cex) = mpsc::channel(8);
    let (tx_deep, rx_deep) = mpsc::channel(8);
    let strategy_handle = strategy.spawn(rx_cex, rx_deep);

    let positions = Arc::new(Mutex::new(PositionManager::default()));
    let order_router = spawn_order_router(
        order_rx,
        Arc::new(BinancePerpExecutor::new()),
        Arc::clone(&positions),
    );

    let shutdown_coordinator = ShutdownCoordinator::new(
        &config.shutdown,
        strategy_handle,
        deepbook_executor,
        lending_client,
        engine.command_sender(),
        Arc::clone(&positions),
        Arc::clone(&health_metrics),
    );

    tx_cex
        .send(MarketEvent {
            venue: MarketVenue::BinanceSpot,
            pair: "WAL/USDC".into(),
            bids: vec![OrderBookLevel {
                price: 100.0,
                size: 10.0,
            }],
            asks: vec![OrderBookLevel {
                price: 100.5,
                size: 10.0,
            }],
            ts_ms: 0,
        })
        .await
        .unwrap();
    tx_deep
        .send(MarketEvent {
            venue: MarketVenue::Deepbook,
            pair: "WAL/USDC".into(),
            bids: vec![OrderBookLevel {
                price: 99.5,
                size: 10.0,
            }],
            asks: vec![OrderBookLevel {
                price: 101.0,
                size: 10.0,
            }],
            ts_ms: 0,
        })
        .await
        .unwrap();

    sleep(Duration::from_millis(100)).await;
    let result = shutdown_coordinator.shutdown().await;
    order_router.abort();
    assert!(result.is_ok());
    Ok(())
}

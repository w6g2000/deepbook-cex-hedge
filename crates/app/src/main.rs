use anyhow::Result;
use cex_execution::{BinancePerpExecutor, BinanceSpotExecutor, CexExecutor, PositionManager};
use cex_watcher::CexWatcher;
use deepbook_cex_app::{HealthServer, ShutdownCoordinator};
use deepbook_execution::DeepbookExecution;
use deepbook_watcher::DeepbookWatcher;
use execution::{ExecutionEngine, spawn_order_router};
use lending::LendingClient;
use shared::config::{AppConfig, TradingPair};
use shared::metrics::HealthMetrics;
use std::sync::Arc;
use std::time::Duration;
use strategy::StrategyEngine;
use tokio::signal;
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing()?;

    tracing::info!("starting deepbook-cex hedge application");

    let config = AppConfig::load()?;
    tracing::info!(
        pairs = config.pairs.len(),
        ?config.binding,
        "configuration loaded"
    );

    let pairs = select_pairs(&config);

    let mut cex_watcher = CexWatcher::new(pairs.clone());
    if let (Some(rest), Some(stream)) =
        (config.cex.rest_base.clone(), config.cex.stream_base.clone())
    {
        cex_watcher = cex_watcher.with_endpoints(rest, stream);
    }

    let mut deepbook_watcher = DeepbookWatcher::new(pairs.clone());
    if let Some(endpoint) = config.deepbook.endpoint.clone() {
        deepbook_watcher = deepbook_watcher.with_endpoint(endpoint);
    }
    if let Some(level) = config.deepbook.level {
        deepbook_watcher = deepbook_watcher.with_level(level);
    }
    if let Some(depth) = config.deepbook.depth {
        deepbook_watcher = deepbook_watcher.with_depth(depth);
    }
    if let Some(interval_ms) = config.deepbook.poll_interval_ms {
        deepbook_watcher =
            deepbook_watcher.with_poll_interval(Duration::from_millis(interval_ms.max(100)));
    }

    let cex_events = cex_watcher.start();
    let deepbook_events = deepbook_watcher.start();

    let deepbook_executor = DeepbookExecution::default();
    let lending_client = LendingClient::from_config(&config);
    let health_metrics = HealthMetrics::new();
    let (engine, order_rx) = ExecutionEngine::new(256);
    let spot_executor = match BinanceSpotExecutor::from_config(&config.cex) {
        Ok(exec) => Some(Arc::new(exec)),
        Err(err) => {
            tracing::warn!(
                error = %err,
                "failed to initialize Binance spot executor; rebalancing disabled"
            );
            None
        }
    };
    let strategy = StrategyEngine::new(
        &config,
        deepbook_executor.clone(),
        lending_client.clone(),
        engine.command_sender(),
        Arc::clone(&health_metrics),
        spot_executor.clone(),
    );
    let strategy_handle = strategy.spawn(cex_events, deepbook_events);

    let positions = Arc::new(Mutex::new(PositionManager::default()));
    let order_router = spawn_order_router(
        order_rx,
        Arc::new(BinancePerpExecutor::default()) as Arc<dyn CexExecutor>,
        Arc::clone(&positions),
    );

    let shutdown_coordinator = ShutdownCoordinator::new(
        &config.shutdown,
        strategy_handle,
        deepbook_executor,
        lending_client.clone(),
        engine.command_sender(),
        Arc::clone(&positions),
        Arc::clone(&health_metrics),
    );

    let health_server = HealthServer::new(
        config.binding.as_deref(),
        Arc::clone(&health_metrics),
        lending_client,
        Arc::clone(&positions),
    )?;
    let health_handle = health_server.spawn();

    signal::ctrl_c().await?;
    tracing::info!("shutdown signal received");
    shutdown_coordinator.shutdown().await?;
    health_handle.abort();
    order_router.abort();
    Ok(())
}

fn init_tracing() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .try_init()
        .map_err(|err| anyhow::anyhow!(err))?;
    Ok(())
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

use anyhow::{Context, Result};
use cex_execution::PositionManager;
use lending::LendingClient;
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tracing::{info, warn};

use shared::metrics::HealthMetrics;

pub struct HealthServer {
    addr: SocketAddr,
    metrics: Arc<HealthMetrics>,
    lending: LendingClient,
    positions: Arc<Mutex<PositionManager>>,
}

impl HealthServer {
    pub fn new(
        binding: Option<&str>,
        metrics: Arc<HealthMetrics>,
        lending: LendingClient,
        positions: Arc<Mutex<PositionManager>>,
    ) -> Result<Self> {
        let addr: SocketAddr = binding
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| "127.0.0.1:8080".parse().expect("default addr"));
        Ok(Self {
            addr,
            metrics,
            lending,
            positions,
        })
    }

    pub fn spawn(self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            if let Err(err) = self.run().await {
                warn!(error = %err, "health server exited with error");
            }
        })
    }

    async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(self.addr)
            .await
            .with_context(|| format!("bind health server on {}", self.addr))?;
        info!(addr = %self.addr, "health server listening");

        loop {
            let (socket, _) = listener.accept().await?;
            let metrics = Arc::clone(&self.metrics);
            let lending = self.lending.clone();
            let positions = Arc::clone(&self.positions);
            tokio::spawn(async move {
                if let Err(err) = handle_connection(socket, metrics, lending, positions).await {
                    warn!(error = %err, "health request failed");
                }
            });
        }
    }
}

async fn handle_connection(
    mut socket: TcpStream,
    metrics: Arc<HealthMetrics>,
    lending: LendingClient,
    positions: Arc<Mutex<PositionManager>>,
) -> Result<()> {
    let mut buffer = [0u8; 512];
    let bytes = socket.read(&mut buffer).await?;
    if bytes == 0 {
        return Ok(());
    }
    let request = String::from_utf8_lossy(&buffer[..bytes]);
    let first_line = request.lines().next().unwrap_or("");
    let path = first_line.split_whitespace().nth(1).unwrap_or("/");

    if path != "/health" {
        let response = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        socket.write_all(response.as_bytes()).await?;
        return Ok(());
    }

    let snapshot = metrics.snapshot().await;
    let health = lending.account_health().await?;
    let positions_map = positions.lock().await.snapshot();
    let open_positions = positions_map
        .values()
        .filter(|state| state.net_position.abs() > f64::EPSILON)
        .count();

    let body = json!({
        "last_cex_event_ms": snapshot.last_cex_event_ms,
        "last_deepbook_event_ms": snapshot.last_deepbook_event_ms,
        "borrow_ratio": health.borrow_ratio,
        "max_borrow_ratio": health.max_borrow_ratio,
        "open_positions": open_positions,
    });

    let body_str = serde_json::to_string(&body)?;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body_str.len(),
        body_str
    );
    socket.write_all(response.as_bytes()).await?;
    Ok(())
}

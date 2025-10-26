use anyhow::Context;
use reqwest::Client;
use serde::Deserialize;
use shared::config::TradingPair;
use shared::types::{MarketEvent, MarketVenue, OrderBookLevel};
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};

const DEFAULT_ENDPOINT: &str = "https://deepbook-indexer.mainnet.mystenlabs.com/orderbook";
const DEFAULT_LEVEL: u32 = 2;
const DEFAULT_DEPTH: u32 = 30;
const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

#[derive(Clone)]
pub struct DeepbookWatcher {
    client: Client,
    endpoint: String,
    level: u32,
    depth: u32,
    poll_interval: Duration,
    pairs: Vec<TradingPair>,
}

impl DeepbookWatcher {
    pub fn new(pairs: Vec<TradingPair>) -> Self {
        Self {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            level: DEFAULT_LEVEL,
            depth: DEFAULT_DEPTH,
            poll_interval: Duration::from_millis(DEFAULT_POLL_INTERVAL_MS),
            pairs,
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    pub fn with_level(mut self, level: u32) -> Self {
        self.level = level.max(1);
        self
    }

    pub fn with_depth(mut self, depth: u32) -> Self {
        self.depth = depth.max(1);
        self
    }

    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval.max(Duration::from_millis(100));
        self
    }

    pub fn start(&self) -> mpsc::Receiver<MarketEvent> {
        let (tx, rx) = mpsc::channel(256);

        if self.pairs.is_empty() {
            tracing::warn!("deepbook watcher started with empty trading pair list");
            return rx;
        }

        for pair in self.pairs.clone() {
            let client = self.client.clone();
            let endpoint = self.endpoint.clone();
            let level = self.level;
            let depth = self.depth;
            let poll_interval = self.poll_interval;
            let tx_pair = tx.clone();

            tokio::spawn(async move {
                if let Err(err) =
                    poll_pair_loop(client, endpoint, level, depth, poll_interval, pair, tx_pair)
                        .await
                {
                    tracing::error!(error = %err, "deepbook polling loop exited with error");
                }
            });
        }

        drop(tx);
        rx
    }
}

async fn poll_pair_loop(
    client: Client,
    endpoint: String,
    level: u32,
    depth: u32,
    poll_interval: Duration,
    pair: TradingPair,
    tx: mpsc::Sender<MarketEvent>,
) -> anyhow::Result<()> {
    let endpoint = endpoint.trim_end_matches('/').to_string();
    let symbol = pair.deepbook_symbol();
    let pair_label = pair.display_pair();

    tracing::info!(
        pair = %pair_label,
        %endpoint,
        level,
        depth,
        poll_ms = poll_interval.as_millis(),
        "starting deepbook polling loop"
    );

    let mut next_delay = poll_interval;

    loop {
        if tx.is_closed() {
            tracing::info!(pair = %pair_label, "channel closed; exiting polling loop");
            break;
        }

        let url = format!("{endpoint}/{symbol}?level={level}&depth={depth}");

        match fetch_orderbook(&client, &url, &pair).await {
            Ok(event) => {
                next_delay = poll_interval;
                let bids_count = event.bids.len();
                let asks_count = event.asks.len();
                tracing::debug!(
                    pair = %pair_label,
                    bids = bids_count,
                    asks = asks_count,
                    "publishing deepbook snapshot"
                );
                if tx.send(event).await.is_err() {
                    tracing::warn!(pair = %pair_label, "receiver dropped; stopping polling loop");
                    break;
                }
            }
            Err(err) => {
                tracing::warn!(pair = %pair_label, error = %err, "failed to fetch deepbook orderbook");
                next_delay = increase_delay(next_delay, poll_interval);
                tracing::debug!(
                    pair = %pair_label,
                    delay_ms = next_delay.as_millis(),
                    "backing off deepbook polling"
                );
            }
        }

        sleep(next_delay).await;
    }

    Ok(())
}

async fn fetch_orderbook(
    client: &Client,
    url: &str,
    pair: &TradingPair,
) -> anyhow::Result<MarketEvent> {
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("request deepbook orderbook {}", url))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unavailable".to_string());
        anyhow::bail!(
            "deepbook orderbook {} returned status {}: {}",
            url,
            status,
            body
        );
    }

    let payload: OrderbookSnapshot = resp
        .json()
        .await
        .with_context(|| format!("decode deepbook snapshot {}", url))?;

    Ok(convert_snapshot(pair, payload))
}

fn convert_snapshot(pair: &TradingPair, snapshot: OrderbookSnapshot) -> MarketEvent {
    let bids = snapshot.bids.into_iter().filter_map(to_level).collect();
    let asks = snapshot.asks.into_iter().filter_map(to_level).collect();

    let ts_ms = snapshot
        .timestamp
        .and_then(|ts| ts.parse::<i64>().ok())
        .unwrap_or_else(current_timestamp_ms);

    MarketEvent {
        venue: MarketVenue::Deepbook,
        pair: pair.display_pair(),
        bids,
        asks,
        ts_ms,
    }
}

fn to_level(entry: [String; 2]) -> Option<OrderBookLevel> {
    let [price_str, size_str] = entry;
    let price = price_str.parse::<f64>().ok()?;
    let size = size_str.parse::<f64>().ok()?;
    Some(OrderBookLevel { price, size })
}

fn current_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

#[derive(Debug, Deserialize)]
struct OrderbookSnapshot {
    #[serde(default)]
    bids: Vec<[String; 2]>,
    #[serde(default)]
    asks: Vec<[String; 2]>,
    #[serde(default)]
    timestamp: Option<String>,
}

fn increase_delay(current: Duration, floor: Duration) -> Duration {
    let current_ms = current.as_millis() as f64;
    let increased = (current_ms * 1.5).round();
    let floor_ms = floor.as_millis() as f64;
    let capped = increased.max(floor_ms).min(30_000.0);
    Duration::from_millis(capped as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{Duration, Instant, timeout};

    #[test]
    fn converts_snapshot_to_market_event() {
        let pair = TradingPair {
            base: "WAL".to_string(),
            quote: "USDC".to_string(),
        };
        let snapshot = OrderbookSnapshot {
            bids: vec![
                ["0.231746".into(), "64.7".into()],
                ["0.231546".into(), "96.8".into()],
            ],
            asks: vec![
                ["0.231918".into(), "98.7".into()],
                ["0.232149".into(), "5154.1".into()],
            ],
            timestamp: Some("1761039530229".to_string()),
        };

        let event = convert_snapshot(&pair, snapshot);
        assert_eq!(event.pair, "WAL/USDC");
        assert_eq!(event.venue, MarketVenue::Deepbook);
        assert_eq!(event.bids.len(), 2);
        assert!((event.bids[0].price - 0.231746).abs() < f64::EPSILON);
        assert_eq!(event.ts_ms, 1761039530229);
    }

    #[tokio::test]
    async fn polling_loop_emits_events() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind http listener");
        let addr = listener.local_addr().unwrap();

        let http_task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..read]);
                    if buffer.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let body = r#"{
                    "asks":[["0.231918","98.7"]],
                    "bids":[["0.231746","64.7"]],
                    "timestamp":"1761039530229"
                }"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });

        let pair = TradingPair {
            base: "WAL".to_string(),
            quote: "USDC".to_string(),
        };
        let endpoint = format!("http://{}/orderbook", addr);

        let watcher = DeepbookWatcher::new(vec![pair.clone()])
            .with_endpoint(endpoint)
            .with_poll_interval(Duration::from_millis(200));

        let mut rx = watcher.start();

        let event = timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out waiting for event")
            .expect("expected market event");

        assert_eq!(event.pair, "WAL/USDC");
        assert_eq!(event.venue, MarketVenue::Deepbook);
        assert_eq!(event.bids.len(), 1);

        drop(rx);
        let _ = http_task.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires external network access to Deepbook indexer"]
    async fn polling_loop_hits_real_endpoint() {
        let _ = tracing_subscriber::fmt().with_target(false).try_init();

        let pair = TradingPair {
            base: "WAL".to_string(),
            quote: "USDC".to_string(),
        };
        let expected_pair = pair.display_pair();

        let watcher = DeepbookWatcher::new(vec![pair.clone()])
            .with_poll_interval(Duration::from_secs(1))
            .with_depth(30)
            .with_level(2);

        let mut rx = watcher.start();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut snapshots = 0usize;
        let mut saw_expected_pair = false;
        let mut saw_liquidity = false;

        const PRICE_BUCKET: f64 = 0.001;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            match timeout(remaining.min(Duration::from_secs(2)), rx.recv()).await {
                Ok(Some(event)) => {
                    snapshots += 1;
                    if event.pair == expected_pair && event.venue == MarketVenue::Deepbook {
                        saw_expected_pair = true;
                    }
                    if !event.bids.is_empty() || !event.asks.is_empty() {
                        saw_liquidity = true;
                    }

                    let bid_buckets = bucket_levels(&event.bids, PRICE_BUCKET, true);
                    let ask_buckets = bucket_levels(&event.asks, PRICE_BUCKET, false);

                    tracing::info!(
                        pair = %event.pair,
                        venue = ?event.venue,
                        bids = ?bid_buckets,
                        asks = ?ask_buckets,
                        ts_ms = event.ts_ms,
                        "deepbook live orderbook snapshot"
                    );
                }
                Ok(None) => break,
                Err(_) => {
                    tracing::debug!("timed out waiting for deepbook snapshot before deadline");
                }
            }
        }

        assert!(
            snapshots > 0,
            "expected at least one orderbook snapshot within 5s"
        );
        assert!(saw_expected_pair, "expected WAL/USDC deepbook snapshots");
        assert!(saw_liquidity, "orderbook should contain bids or asks");

        drop(rx);
    }

    fn bucket_levels(
        levels: &[OrderBookLevel],
        step: f64,
        descending: bool,
    ) -> Vec<OrderBookLevel> {
        use std::collections::BTreeMap;

        if step <= 0.0 {
            return levels.to_vec();
        }

        let mut buckets: BTreeMap<i64, f64> = BTreeMap::new();
        let scale = (1.0 / step).round() as f64;

        for level in levels {
            let idx = (level.price * scale).floor() as i64;
            *buckets.entry(idx).or_insert(0.0) += level.size;
        }

        let mut aggregated: Vec<OrderBookLevel> = buckets
            .into_iter()
            .map(|(idx, size)| OrderBookLevel {
                price: (idx as f64) / scale,
                size,
            })
            .collect();

        if descending {
            aggregated.sort_by(|a, b| b.price.partial_cmp(&a.price).unwrap());
        }

        aggregated
    }
}

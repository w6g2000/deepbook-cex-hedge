use anyhow::Context;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use shared::config::TradingPair;
use shared::types::{MarketEvent, MarketVenue, OrderBookLevel};
use std::cmp::Ordering;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{Duration, sleep};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use url::Url;

const DEFAULT_STREAM_BASE: &str = "wss://stream.binance.com:9443/stream?streams=";
const DEFAULT_REST_BASE: &str = "https://api.binance.com/api/v3/depth";
const DEFAULT_DEPTH_LEVELS: usize = 20;
const DEFAULT_UPDATE_INTERVAL: &str = "100ms";
const INITIAL_BACKOFF_SECS: u64 = 1;
const MAX_BACKOFF_SECS: u64 = 64;
const INACTIVITY_TIMEOUT_SECS: u64 = 5;
const DEFAULT_PRICE_BUCKET: f64 = 0.0001;

pub struct CexWatcher {
    pairs: Vec<TradingPair>,
    depth_levels: usize,
    rest_base: String,
    stream_base: String,
    price_bucket: Option<f64>,
}

impl CexWatcher {
    pub fn new(pairs: Vec<TradingPair>) -> Self {
        Self {
            pairs,
            depth_levels: DEFAULT_DEPTH_LEVELS,
            rest_base: DEFAULT_REST_BASE.to_string(),
            stream_base: DEFAULT_STREAM_BASE.to_string(),
            price_bucket: Some(DEFAULT_PRICE_BUCKET),
        }
    }

    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth_levels = depth.clamp(1, 20);
        self
    }

    pub fn with_endpoints(
        mut self,
        rest_base: impl Into<String>,
        stream_base: impl Into<String>,
    ) -> Self {
        self.rest_base = rest_base.into();
        self.stream_base = stream_base.into();
        self
    }

    pub fn with_price_bucket(mut self, bucket: f64) -> Self {
        if bucket > 0.0 {
            self.price_bucket = Some(bucket);
        }
        self
    }

    pub fn without_price_bucket(mut self) -> Self {
        self.price_bucket = None;
        self
    }

    pub fn start(&self) -> mpsc::Receiver<MarketEvent> {
        let (tx, rx) = mpsc::channel(512);

        if self.pairs.is_empty() {
            tracing::warn!("cex watcher started with empty trading pair list");
            return rx;
        }

        let pairs = self.pairs.clone();
        let depth = self.depth_levels;
        let rest_base = self.rest_base.clone();
        let stream_base = self.stream_base.clone();
        let price_bucket = self.price_bucket;
        tokio::spawn(async move {
            run_combined_stream(pairs, depth, stream_base, rest_base, price_bucket, tx).await;
        });

        rx
    }
}

async fn run_combined_stream(
    pairs: Vec<TradingPair>,
    depth_levels: usize,
    stream_base: String,
    rest_base: String,
    price_bucket: Option<f64>,
    tx: mpsc::Sender<MarketEvent>,
) {
    let symbol_lookup = build_symbol_lookup(&pairs);
    if symbol_lookup.is_empty() {
        tracing::warn!("cex watcher has no valid symbol mappings, exiting");
        return;
    }

    let stream_url = combined_stream_url(&stream_base, &pairs, depth_levels);
    let client = Client::new();
    publish_initial_snapshots(
        &client,
        &rest_base,
        &pairs,
        depth_levels,
        &symbol_lookup,
        price_bucket,
        &tx,
    )
    .await;
    let mut backoff = ReconnectBackoff::new();

    loop {
        match Url::parse(&stream_url) {
            Ok(url) => match connect_async(url).await {
                Ok((mut ws_stream, _)) => {
                    tracing::info!("connected to binance combined depth stream");
                    backoff.reset();

                    let inactivity_limit = Duration::from_secs(INACTIVITY_TIMEOUT_SECS);

                    loop {
                        match tokio::time::timeout(inactivity_limit, ws_stream.next()).await {
                            Ok(Some(msg_result)) => match msg_result {
                                Ok(Message::Text(payload)) => {
                                    if let Some(event) =
                                        parse_market_event(&payload, &symbol_lookup)
                                    {
                                        let event = apply_price_bucket(event, price_bucket);
                                        if tx.send(event).await.is_err() {
                                            tracing::warn!(
                                                "market event channel closed, stopping cex watcher task"
                                            );
                                            return;
                                        }
                                    }
                                }
                                Ok(Message::Binary(bytes)) => {
                                    tracing::debug!(
                                        size = bytes.len(),
                                        "ignored binary binance message"
                                    );
                                }
                                Ok(Message::Ping(payload)) => {
                                    tracing::trace!(
                                        "received ping from binance, replying with pong"
                                    );
                                    if let Err(err) = ws_stream.send(Message::Pong(payload)).await {
                                        tracing::warn!(error = %err, "failed to send pong");
                                        break;
                                    }
                                }
                                Ok(Message::Pong(_)) => {
                                    tracing::trace!("received pong from binance");
                                }
                                Ok(Message::Frame(_)) => {
                                    tracing::trace!("ignored tungstenite frame message");
                                }
                                Ok(Message::Close(frame)) => {
                                    tracing::warn!(?frame, "binance stream closed by server");
                                    break;
                                }
                                Err(err) => {
                                    tracing::warn!(
                                        error = %err,
                                        "binance stream read error"
                                    );
                                    break;
                                }
                            },
                            Ok(None) => {
                                tracing::info!("binance stream ended");
                                break;
                            }
                            Err(_) => {
                                tracing::warn!(
                                    "no depth updates from binance for {} seconds; reconnecting",
                                    INACTIVITY_TIMEOUT_SECS
                                );
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(error = %err, "failed to connect to binance stream");
                }
            },
            Err(err) => {
                tracing::error!(error = %err, url = %stream_url, "invalid binance stream url");
                return;
            }
        }

        if tx.is_closed() {
            tracing::info!("cex watcher channel closed, giving up reconnect");
            return;
        }

        let wait = backoff.next_wait();
        tracing::info!("binance watcher reconnecting in {}s", wait.as_secs());
        sleep(wait).await;
    }
}

fn build_symbol_lookup(pairs: &[TradingPair]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|pair| (pair.binance_symbol(), pair.display_pair()))
        .collect()
}

async fn publish_initial_snapshots(
    client: &Client,
    rest_base: &str,
    pairs: &[TradingPair],
    depth_levels: usize,
    lookup: &HashMap<String, String>,
    price_bucket: Option<f64>,
    tx: &mpsc::Sender<MarketEvent>,
) {
    for pair in pairs {
        match fetch_depth_snapshot(client, rest_base, pair, depth_levels).await {
            Ok(payload) => {
                if let Some(event) = compose_market_event(payload, lookup) {
                    let event = apply_price_bucket(event, price_bucket);
                    if tx.send(event).await.is_err() {
                        tracing::warn!(
                            pair = %pair.display_pair(),
                            "initial snapshot channel closed"
                        );
                        return;
                    }
                    tracing::info!(pair = %pair.display_pair(), "published initial depth snapshot");
                }
            }
            Err(err) => {
                tracing::warn!(
                    pair = %pair.display_pair(),
                    error = %err,
                    "failed to fetch initial depth snapshot"
                );
            }
        }
    }
}

async fn fetch_depth_snapshot(
    client: &Client,
    rest_base: &str,
    pair: &TradingPair,
    depth_levels: usize,
) -> anyhow::Result<DepthPayload> {
    let mut url = Url::parse(rest_base).context("invalid binance depth base url")?;

    let symbol = pair.binance_symbol();
    let limit = depth_levels.to_string();
    {
        let mut qp = url.query_pairs_mut();
        qp.append_pair("symbol", &symbol);
        qp.append_pair("limit", &limit);
    }

    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("request depth snapshot for {}", pair.display_pair()))?;

    let status = response.status();
    if !status.is_success() {
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "unavailable".to_string());
        anyhow::bail!(
            "snapshot response for {} returned status {}: {}",
            pair.display_pair(),
            status,
            body
        );
    }

    let mut payload: DepthPayload = response
        .json()
        .await
        .with_context(|| format!("decode depth snapshot for {}", pair.display_pair()))?;
    payload.symbol = Some(symbol);
    if payload.event_time == 0 {
        payload.event_time = current_timestamp_ms();
    }

    Ok(payload)
}

fn combined_stream_url(stream_base: &str, pairs: &[TradingPair], depth_levels: usize) -> String {
    let streams: Vec<String> = pairs
        .iter()
        .map(|pair| {
            format!(
                "{}@depth{}@{}",
                pair.binance_stream_key(),
                depth_levels,
                DEFAULT_UPDATE_INTERVAL
            )
        })
        .collect();

    format!("{}{}", stream_base, streams.join("/"))
}

fn parse_market_event(payload: &str, lookup: &HashMap<String, String>) -> Option<MarketEvent> {
    let value: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!(error = %err, "failed to parse binance payload as json");
            return None;
        }
    };

    if value.get("stream").is_some() {
        match serde_json::from_value::<CombinedStreamEnvelope>(value) {
            Ok(envelope) => compose_market_event(envelope.into_payload(), lookup),
            Err(err) => {
                tracing::warn!(error = %err, "failed to parse combined stream envelope");
                None
            }
        }
    } else {
        match serde_json::from_value::<DepthPayload>(value) {
            Ok(payload) => compose_market_event(payload, lookup),
            Err(err) => {
                tracing::debug!(error = %err, "unable to parse binance payload");
                None
            }
        }
    }
}

fn convert_levels(levels: &[[String; 2]]) -> Vec<OrderBookLevel> {
    levels
        .iter()
        .filter_map(|entry| {
            let price = entry[0].parse::<f64>().ok()?;
            let size = entry[1].parse::<f64>().ok()?;
            Some(OrderBookLevel { price, size })
        })
        .collect()
}

fn compose_market_event(
    data: DepthPayload,
    lookup: &HashMap<String, String>,
) -> Option<MarketEvent> {
    let symbol = data
        .symbol
        .clone()
        .or(data.stream_symbol.clone())
        .unwrap_or_default()
        .to_uppercase();

    let pair_label = if symbol.is_empty() {
        if lookup.len() > 1 {
            tracing::warn!("binance depth update missing symbol while multiple pairs watched");
            return None;
        }
        match lookup.values().next().cloned() {
            Some(label) => label,
            None => {
                tracing::warn!("binance depth update missing symbol and lookup empty");
                return None;
            }
        }
    } else {
        lookup
            .get(&symbol)
            .cloned()
            .unwrap_or_else(|| symbol.clone())
    };

    let bids = convert_levels(&data.bids);
    let asks = convert_levels(&data.asks);

    if bids.is_empty() && asks.is_empty() {
        tracing::debug!(%pair_label, "empty order book update");
        return None;
    }

    let ts_ms = if data.event_time > 0 {
        data.event_time
    } else {
        current_timestamp_ms()
    };

    Some(MarketEvent {
        venue: MarketVenue::BinanceSpot,
        pair: pair_label,
        bids,
        asks,
        ts_ms,
    })
}

fn apply_price_bucket(event: MarketEvent, bucket: Option<f64>) -> MarketEvent {
    match bucket {
        Some(step) if step > 0.0 => MarketEvent {
            bids: bucket_levels(&event.bids, step, true),
            asks: bucket_levels(&event.asks, step, false),
            ..event
        },
        _ => event,
    }
}

fn bucket_levels(levels: &[OrderBookLevel], step: f64, descending: bool) -> Vec<OrderBookLevel> {
    use std::collections::BTreeMap;

    if !step.is_finite() || step <= 0.0 {
        return levels.to_vec();
    }

    let scale = (1.0 / step).round();
    if !scale.is_normal() {
        return levels.to_vec();
    }

    let mut buckets: BTreeMap<i64, f64> = BTreeMap::new();
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

    aggregated.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(Ordering::Equal));
    if descending {
        aggregated.reverse();
    }

    aggregated
}

fn current_timestamp_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or_default()
}

struct ReconnectBackoff {
    current: Duration,
}

impl ReconnectBackoff {
    fn new() -> Self {
        Self {
            current: Duration::from_secs(INITIAL_BACKOFF_SECS),
        }
    }

    fn reset(&mut self) {
        self.current = Duration::from_secs(INITIAL_BACKOFF_SECS);
    }

    fn next_wait(&mut self) -> Duration {
        let wait = self.current;
        let next_secs = (self.current.as_secs().saturating_mul(2)).min(MAX_BACKOFF_SECS);
        self.current = Duration::from_secs(next_secs.max(INITIAL_BACKOFF_SECS));
        wait
    }
}

#[derive(Debug, Deserialize)]
struct CombinedStreamEnvelope {
    #[allow(dead_code)]
    stream: String,
    data: DepthPayload,
}

impl CombinedStreamEnvelope {
    fn into_payload(mut self) -> DepthPayload {
        if self.data.symbol.is_none() || self.data.symbol.as_deref() == Some("") {
            let stream_symbol = self
                .stream
                .split('@')
                .next()
                .unwrap_or_default()
                .to_uppercase();
            if !stream_symbol.is_empty() {
                self.data.stream_symbol = Some(stream_symbol);
            }
        }
        self.data
    }
}

#[derive(Debug, Deserialize)]
struct DepthPayload {
    #[serde(rename = "E", default)]
    event_time: i64,
    #[serde(rename = "s", default)]
    symbol: Option<String>,
    #[serde(rename = "lastUpdateId", default)]
    #[allow(dead_code)]
    last_update_id: Option<i64>,
    #[serde(rename = "b", alias = "bids", default)]
    bids: Vec<[String; 2]>,
    #[serde(rename = "a", alias = "asks", default)]
    asks: Vec<[String; 2]>,
    #[serde(skip)]
    stream_symbol: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;
    use shared::config::TradingPair;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message;

    #[test]
    fn parses_depth_update_into_market_event() {
        let payload = r#"{
            "stream":"btcusdt@depth5@100ms",
            "data":{
                "lastUpdateId":78743038148,
                "bids":[["20000.10","0.5"]],
                "asks":[["20001.20","1.2"]]
            }
        }"#;

        let mut lookup = HashMap::new();
        lookup.insert("BTCUSDT".to_string(), "BTC/USDT".to_string());

        let event = parse_market_event(payload, &lookup).expect("market event");

        assert_eq!(event.venue, MarketVenue::BinanceSpot);
        assert_eq!(event.pair, "BTC/USDT");
        assert_eq!(event.bids.len(), 1);
        assert!((event.bids[0].price - 20000.10).abs() < f64::EPSILON);
        assert!((event.asks[0].size - 1.2).abs() < f64::EPSILON);
        assert!(event.ts_ms > 0);
    }

    #[test]
    fn parses_plain_snapshot_into_market_event() {
        let payload = r#"{
            "lastUpdateId":12404468,
            "bids":[["0.22130000","9597.90000000"]],
            "asks":[["0.22140000","3088.80000000"]]
        }"#;

        let mut lookup = HashMap::new();
        lookup.insert("WALUSDC".to_string(), "WAL/USDC".to_string());

        let event = parse_market_event(payload, &lookup).expect("snapshot event");
        assert_eq!(event.venue, MarketVenue::BinanceSpot);
        assert_eq!(event.pair, "WAL/USDC");
        assert_eq!(event.bids.len(), 1);
        assert_eq!(event.asks.len(), 1);
        assert!(event.ts_ms > 0);
    }

    #[test]
    fn parses_direct_depth_update() {
        let payload = r#"{
            "e":"depthUpdate",
            "E":1661999200000,
            "s":"ETHUSDT",
            "b":[["1700.10","1.0"]],
            "a":[["1700.30","2.0"]]
        }"#;

        let mut lookup = HashMap::new();
        lookup.insert("ETHUSDT".to_string(), "ETH/USDT".to_string());

        let event = parse_market_event(payload, &lookup).expect("direct depth event");
        assert_eq!(event.pair, "ETH/USDT");
        assert!((event.bids[0].price - 1700.10).abs() < f64::EPSILON);
        assert_eq!(event.ts_ms, 1661999200000);
    }

    #[tokio::test]
    async fn end_to_end_stream_with_snapshot() {
        let http_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind http listener");
        let http_addr = http_listener.local_addr().unwrap();

        let http_task = tokio::spawn(async move {
            if let Ok((mut socket, _)) = http_listener.accept().await {
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

                let body = r#"{"lastUpdateId":1,"bids":[["0.22130000","1.0"]],"asks":[["0.22140000","2.0"]]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });

        let ws_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ws listener");
        let ws_addr = ws_listener.local_addr().unwrap();

        let ws_task = tokio::spawn(async move {
            if let Ok((stream, _)) = ws_listener.accept().await {
                let mut ws = accept_async(stream).await.expect("accept websocket");
                let message = r#"{"stream":"walusdc@depth5@100ms","data":{"lastUpdateId":2,"bids":[["0.22120000","1.5"]],"asks":[["0.22150000","3.0"]]}}"#;
                ws.send(Message::Text(message.into()))
                    .await
                    .expect("send ws payload");
                tokio::time::sleep(Duration::from_millis(50)).await;
                ws.close(None).await.ok();
            }
        });

        let rest_base = format!("http://{}/api/v3/depth", http_addr);
        let stream_base = format!("ws://{}/stream?streams=", ws_addr);

        let pair = TradingPair {
            base: "WAL".to_string(),
            quote: "USDC".to_string(),
        };

        let watcher = CexWatcher::new(vec![pair.clone()]).with_endpoints(rest_base, stream_base);
        let mut rx = watcher.start();

        let snapshot_event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("snapshot timeout")
            .expect("snapshot event");
        assert_eq!(snapshot_event.pair, "WAL/USDC");
        assert_eq!(snapshot_event.venue, MarketVenue::BinanceSpot);
        assert!((snapshot_event.bids[0].price - 0.221).abs() < 1e-9);
        assert!((snapshot_event.bids[0].size - 1.0).abs() < f64::EPSILON);

        let stream_event = timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("stream timeout")
            .expect("stream event");
        assert_eq!(stream_event.pair, "WAL/USDC");
        assert!((stream_event.asks[0].price - 0.221).abs() < 1e-9);
        assert!((stream_event.asks[0].size - 3.0).abs() < f64::EPSILON);

        drop(rx);
        let _ = http_task.await;
        let _ = ws_task.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "hits live Binance endpoints; run manually when internet access is allowed"]
    async fn real_binance_snapshot_stream() {
        let pair = TradingPair {
            base: "WAL".to_string(),
            quote: "USDC".to_string(),
        };

        let watcher = CexWatcher::new(vec![pair]);
        let mut rx = watcher.start();

        let snapshot_event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("snapshot timeout")
            .expect("snapshot event");

        // === 打印快照事件 ===
        eprintln!("\n=== REST bootstrap snapshot ===");
        // 若 MarketEvent: Debug
        eprintln!("{:#?}", snapshot_event);
        // 简要统计
        eprintln!(
            "pair={}, venue={:?}, bids={}, asks={}",
            snapshot_event.pair,
            snapshot_event.venue,
            snapshot_event.bids.len(),
            snapshot_event.asks.len()
        );

        assert_eq!(snapshot_event.pair, "WAL/USDC");
        assert_eq!(snapshot_event.venue, MarketVenue::BinanceSpot);
        assert!(!snapshot_event.bids.is_empty() || !snapshot_event.asks.is_empty());

        let stream_event = timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("stream timeout")
            .expect("stream event");

        // === 打印流式事件 ===
        eprintln!("\n=== WebSocket follow-on snapshot ===");
        eprintln!("{:#?}", stream_event);
        eprintln!(
            "pair={}, venue={:?}, bids={}, asks={}",
            stream_event.pair,
            stream_event.venue,
            stream_event.bids.len(),
            stream_event.asks.len()
        );

        assert_eq!(stream_event.pair, "WAL/USDC");
        assert_eq!(stream_event.venue, MarketVenue::BinanceSpot);
        assert!(!stream_event.bids.is_empty() || !stream_event.asks.is_empty());

        drop(rx);
    }
}

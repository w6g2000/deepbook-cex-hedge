use crate::{
    ClaimSummary, DeepbookBackend, FillUpdate, OrderSnapshot, OrderStatus, PlaceOrderRequest,
};
use anyhow::{anyhow, ensure, Context, Result};
use async_trait::async_trait;
use dotenvy::dotenv;
use shared_crypto::intent::{Intent, IntentMessage};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use sui_deepbookv3::client::{Account, DeepBookClient};
use sui_deepbookv3::utils::config::{BalanceManagerMap, Environment as SdkEnvironment};
use sui_deepbookv3::utils::types::{BalanceManager, OrderType, PlaceLimitOrderParams};
use sui_sdk::rpc_types::{
    Coin, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponse,
    SuiTransactionBlockResponseOptions,
};
use sui_sdk::types::programmable_transaction_builder::ProgrammableTransactionBuilder;
use sui_sdk::types::quorum_driver_types::ExecuteTransactionRequestType;
use sui_sdk::{SuiClient, SuiClientBuilder};
use sui_types::base_types::{ObjectRef, SuiAddress};
use sui_types::crypto::{EncodeDecodeBase64, Signature, SuiKeyPair};
use sui_types::transaction::{Transaction, TransactionData};
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

const DEFAULT_GAS_BUDGET: u64 = 50_000_000;
const SUI_COIN_TYPE: &str = "0x2::sui::SUI";

#[derive(Debug, Clone)]
pub enum DeepbookEnvironment {
    Mainnet,
    Testnet,
}

#[derive(Debug, Clone)]
pub struct DeepbookSdkConfig {
    pub fullnode_url: String,
    pub signer_key: String,
    pub balance_manager_id: String,
    pub environment: DeepbookEnvironment,
    pub gas_budget: Option<u64>,
}

impl DeepbookSdkConfig {
    pub fn new(
        fullnode_url: impl Into<String>,
        signer_key: impl Into<String>,
        balance_manager_id: impl Into<String>,
        environment: DeepbookEnvironment,
    ) -> Self {
        let balance_manager_id = balance_manager_id.into();
        Self {
            fullnode_url: fullnode_url.into(),
            signer_key: signer_key.into(),
            balance_manager_id: balance_manager_id.clone(),
            environment,
            gas_budget: None,
        }
    }

    pub fn with_gas_budget(mut self, gas_budget: u64) -> Self {
        self.gas_budget = Some(gas_budget);
        self
    }
}

#[derive(Clone)]
struct OrderMetadata {
    pool_key: String,
    pair: String,
    last_snapshot: Option<OrderSnapshot>,
}

pub struct SdkBackend {
    sui_client: SuiClient,
    deepbook: DeepBookClient,
    signer: Arc<SuiKeyPair>,
    signer_address: SuiAddress,
    balance_manager_key: String,
    gas_budget: u64,
    client_order_seq: AtomicU64,
    tracked_pools: Mutex<HashSet<String>>,
    order_index: Mutex<HashMap<String, OrderMetadata>>,
}

fn parse_signer_key(raw: &str) -> Result<SuiKeyPair> {
    let trimmed = raw.trim();
    if trimmed.starts_with("suiprivkey") {
        SuiKeyPair::decode(trimmed)
            .map_err(|err| anyhow!("failed to decode Sui signer key from bech32: {err}"))
    } else {
        SuiKeyPair::decode_base64(trimmed)
            .map_err(|err| anyhow!("failed to decode Sui signer key from base64: {err}"))
    }
}

impl SdkBackend {
    pub async fn new(config: DeepbookSdkConfig) -> Result<Self> {
        let _ = dotenv();

        let DeepbookSdkConfig {
            fullnode_url,
            signer_key,
            balance_manager_id,
            environment,
            gas_budget,
        } = config;

        let balance_manager_key = balance_manager_id.clone();
        let signer = Arc::new(parse_signer_key(&signer_key)?);
        let signer_public = signer.public();
        let signer_address = SuiAddress::from(&signer_public);
        info!(signer = %signer_address, "initialized deepbook signer address");

        let sui_client = SuiClientBuilder::default()
            .build(fullnode_url.clone())
            .await
            .with_context(|| format!("failed to connect to Sui fullnode {}", fullnode_url))?;

        let env = match environment {
            DeepbookEnvironment::Mainnet => SdkEnvironment::Mainnet,
            DeepbookEnvironment::Testnet => SdkEnvironment::Testnet,
        };

        // DeepBook SDK expects &'static str keys; leak one key string for config lifetime.
        let leaked_key: &'static str = Box::leak(balance_manager_key.clone().into_boxed_str());
        let mut balance_managers: BalanceManagerMap = BalanceManagerMap::default();
        balance_managers.insert(
            leaked_key,
            BalanceManager {
                address: balance_manager_id.clone(),
                trade_cap: None,
                deposit_cap: None,
                withdraw_cap: None,
            },
        );

        let deepbook = DeepBookClient::new(
            sui_client.clone(),
            signer_address,
            env,
            Some(balance_managers),
            None,
            None,
            None,
        );

        Ok(Self {
            sui_client,
            deepbook,
            signer,
            signer_address,
            balance_manager_key,
            gas_budget: gas_budget.unwrap_or(DEFAULT_GAS_BUDGET),
            client_order_seq: AtomicU64::new(1),
            tracked_pools: Mutex::new(HashSet::new()),
            order_index: Mutex::new(HashMap::new()),
        })
    }

    async fn execute_transaction(
        &self,
        builder: ProgrammableTransactionBuilder,
    ) -> Result<SuiTransactionBlockResponse> {
        let programmable = builder.finish();
        let gas_price = self
            .sui_client
            .read_api()
            .get_reference_gas_price()
            .await
            .context("failed to fetch Sui reference gas price")?;

        let gas_payment = self
            .select_gas_coins(self.gas_budget)
            .await
            .context("failed to select gas coins")?;
        let tx_data = TransactionData::new_programmable(
            self.signer_address,
            gas_payment,
            programmable,
            self.gas_budget,
            gas_price,
        );

        let intent_msg = IntentMessage::new(Intent::sui_transaction(), tx_data.clone());
        let signature = Signature::new_secure(&intent_msg, &*self.signer);
        let transaction = Transaction::from_data(tx_data, vec![signature]);

        let options = SuiTransactionBlockResponseOptions::new()
            .with_effects()
            .with_events();

        let response = self
            .sui_client
            .quorum_driver_api()
            .execute_transaction_block(
                transaction,
                options,
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .context("failed to execute Sui transaction block")?;

        if let Some(effects) = &response.effects {
            debug!(
                digest = %effects.transaction_digest(),
                status = ?effects.status(),
                "executed deepbook transaction"
            );
        }
        Ok(response)
    }

    async fn select_gas_coins(&self, amount: u64) -> Result<Vec<ObjectRef>> {
        let coins = self
            .sui_client
            .coin_read_api()
            .select_coins(
                self.signer_address,
                Some(SUI_COIN_TYPE.to_string()),
                amount as u128,
                vec![],
            )
            .await
            .context("failed to query Sui coins for gas payment")?;

        if coins.is_empty() {
            anyhow::bail!("no SUI coins available for gas payment");
        }

        Ok(coins.iter().map(Coin::object_ref).collect())
    }

    async fn account_open_orders(&self, pool_key: &str) -> Result<HashSet<u128>> {
        let orders = self
            .deepbook
            .account_open_orders(pool_key, &self.balance_manager_key)
            .await
            .context("failed to fetch deepbook open orders")?;
        Ok(orders.into_iter().collect())
    }

    async fn fetch_account(&self, pool_key: &str) -> Result<Account> {
        self.deepbook
            .account(pool_key, &self.balance_manager_key)
            .await
            .context("failed to fetch deepbook account state")
    }

    async fn build_place_order(
        &self,
        request: &PlaceOrderRequest,
        client_order_id: u64,
    ) -> Result<ProgrammableTransactionBuilder> {
        let mut builder = ProgrammableTransactionBuilder::new();
        self.deepbook
            .deep_book
            .place_limit_order(
                &mut builder,
                PlaceLimitOrderParams {
                    pool_key: request.pool_key.clone(),
                    balance_manager_key: self.balance_manager_key.clone(),
                    client_order_id,
                    price: request.price,
                    quantity: request.size,
                    is_bid: matches!(request.side, shared::types::MarketSide::Bid),
                    expiration: None,
                    order_type: Some(OrderType::PostOnly),
                    self_matching_option: None,
                    pay_with_deep: None,
                },
            )
            .await
            .context("failed to build deepbook limit order call")?;
        Ok(builder)
    }

    async fn build_cancel_order(
        &self,
        pool_key: &str,
        order_id: u128,
    ) -> Result<ProgrammableTransactionBuilder> {
        let mut builder = ProgrammableTransactionBuilder::new();
        self.deepbook
            .deep_book
            .cancel_order(
                &mut builder,
                pool_key,
                &self.balance_manager_key,
                order_id,
            )
            .await
            .context("failed to build deepbook cancel order call")?;
        Ok(builder)
    }

    async fn build_cancel_all_orders(
        &self,
        pool_key: &str,
    ) -> Result<ProgrammableTransactionBuilder> {
        let mut builder = ProgrammableTransactionBuilder::new();
        self.deepbook
            .deep_book
            .cancel_all_orders(
                &mut builder,
                pool_key,
                &self.balance_manager_key,
            )
            .await
            .context("failed to build deepbook cancel_all orders call")?;
        Ok(builder)
    }

    async fn build_claim_rebates(
        &self,
        pool_key: &str,
    ) -> Result<ProgrammableTransactionBuilder> {
        let mut builder = ProgrammableTransactionBuilder::new();
        self.deepbook
            .deep_book
            .claim_rebates(&mut builder, pool_key, &self.balance_manager_key)
            .await
            .context("failed to build deepbook claim rebates call")?;
        Ok(builder)
    }

    async fn fetch_order_snapshot(
        &self,
        pool_key: &str,
        order_id: u128,
    ) -> Result<Option<OrderSnapshot>> {
        let normalized = match self
            .deepbook
            .get_order_normalized(pool_key, order_id)
            .await
        {
            Ok(Some(order)) => order,
            Ok(None) => return Ok(None),
            Err(err) => {
                warn!(
                    pool = pool_key,
                    order_id,
                    error = %err,
                    "failed to fetch deepbook order state"
                );
                return Ok(None);
            }
        };

        let price = normalized
            .normalized_price
            .parse::<f64>()
            .unwrap_or_default();
        let size = normalized.quantity.parse::<f64>().unwrap_or_default();
        let filled_base = normalized
            .filled_quantity
            .parse::<f64>()
            .unwrap_or_default();
        let filled_quote = (filled_base * price).abs();

        let status = match normalized.status {
            2 => OrderStatus::Cancelled,
            3 => OrderStatus::Expired,
            _ => {
                if size > 0.0 && filled_base >= size {
                    OrderStatus::Filled
                } else if filled_base > 0.0 {
                    OrderStatus::PartiallyFilled
                } else {
                    OrderStatus::Open
                }
            }
        };

        let pair = pool_key.replace('_', "/");
        let side = if normalized.is_bid {
            shared::types::MarketSide::Bid
        } else {
            shared::types::MarketSide::Ask
        };

        let now = SystemTime::now();

        Ok(Some(OrderSnapshot {
            id: order_id.to_string(),
            pair,
            side,
            price,
            size,
            filled_base,
            filled_quote,
            status,
            created_at: now,
            updated_at: now,
        }))
    }

    async fn register_order(
        &self,
        order_id: u128,
        pool_key: String,
        snapshot: OrderSnapshot,
    ) {
        let mut index = self.order_index.lock().await;
        index.insert(
            order_id.to_string(),
            OrderMetadata {
                pool_key,
                pair: snapshot.pair.clone(),
                last_snapshot: Some(snapshot),
            },
        );
    }

    async fn remove_order(&self, order_id: &str) {
        let mut index = self.order_index.lock().await;
        index.remove(order_id);
    }

    async fn cache_snapshot(&self, order_id: &str, snapshot: OrderSnapshot) {
        let mut index = self.order_index.lock().await;
        if let Some(meta) = index.get_mut(order_id) {
            meta.pair = snapshot.pair.clone();
            meta.last_snapshot = Some(snapshot);
        }
    }

    async fn cached_cancelled_snapshot(&self, order_id: &str) -> Option<OrderSnapshot> {
        let mut index = self.order_index.lock().await;
        let meta = index.get_mut(order_id)?;
        let mut snapshot = meta.last_snapshot.clone()?;
        snapshot.status = OrderStatus::Cancelled;
        snapshot.updated_at = SystemTime::now();
        meta.last_snapshot = Some(snapshot.clone());
        Some(snapshot)
    }

    fn next_client_order_id(&self) -> u64 {
        self.client_order_seq.fetch_add(1, Ordering::Relaxed)
    }
}

#[async_trait]
impl DeepbookBackend for SdkBackend {
    async fn place_post_only_order(&self, request: PlaceOrderRequest) -> Result<OrderSnapshot> {
        ensure!(
            !request.pool_key.is_empty(),
            "pool_key must be provided for SDK backend"
        );

        self.tracked_pools
            .lock()
            .await
            .insert(request.pool_key.clone());

        let before = self.account_open_orders(&request.pool_key).await?;
        let client_order_id = self.next_client_order_id();
        let builder = self.build_place_order(&request, client_order_id).await?;
        let response = self.execute_transaction(builder).await?;
        info!(
            pair = %request.pair,
            pool = %request.pool_key,
            client_order_id,
            "submitted deepbook limit order transaction"
        );
        let mut after = self.account_open_orders(&request.pool_key).await?;
        debug!(
            pool = %request.pool_key,
            ?before,
            before_len = before.len(),
            after_len = after.len(),
            "deepbook open orders snapshot around placement"
        );
        if after.len() == before.len() {
            debug!(
                pool = %request.pool_key,
                "open order set unchanged after placement; attempting to infer order id from events"
            );
            if let Some(events) = &response.events {
                for evt in &events.data {
                    debug!(
                        event_type = %evt.type_,
                        parsed = ?evt.parsed_json,
                        "deepbook placement transaction event"
                    );
                }
            }
            if let Some(event_id) = response.events.as_ref().and_then(|events| {
                events.data.iter().find_map(|evt| {
                    let parsed = &evt.parsed_json;
                    if let Some(order_field) = parsed.get("order_id") {
                        if let Some(as_str) = order_field.as_str() {
                            if let Ok(val) = as_str.parse::<u128>() {
                                return Some(val);
                            }
                        }
                        if let Some(num) = order_field.as_u64() {
                            return Some(num as u128);
                        }
                    }
                    if let Some(order_hex) = parsed.get("order_id_hex").and_then(|v| v.as_str()) {
                        if let Some(stripped) = order_hex.strip_prefix("0x") {
                            if let Ok(val) = u128::from_str_radix(stripped, 16) {
                                return Some(val);
                            }
                        }
                    }
                    None
                })
            }) {
                after.insert(event_id);
            } else {
                warn!(
                    pool = %request.pool_key,
                    "failed to extract order_id from placement events"
                );
            }
        }
        let new_order_id = after
            .difference(&before)
            .next()
            .cloned()
            .ok_or_else(|| anyhow!("failed to determine newly placed order id"))?;

        let snapshot = self
            .fetch_order_snapshot(&request.pool_key, new_order_id)
            .await?
            .ok_or_else(|| anyhow!("deepbook order {new_order_id} not retrievable after placement"))?;

        self.register_order(
            new_order_id,
            request.pool_key.clone(),
            snapshot.clone(),
        )
        .await;

        Ok(snapshot)
    }

    async fn cancel_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>> {
        let metadata = {
            let index = self.order_index.lock().await;
            index
                .get(order_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown order id {order_id} for cancellation"))?
        };

        let parsed_id = order_id
            .parse::<u128>()
            .context("order_id must be numeric for deepbook cancellation")?;

        let builder = self
            .build_cancel_order(&metadata.pool_key, parsed_id)
            .await?;
        self.execute_transaction(builder).await?;

        match self
            .fetch_order_snapshot(&metadata.pool_key, parsed_id)
            .await?
        {
            Some(snapshot) => {
                let terminal = snapshot.status == OrderStatus::Cancelled
                    || snapshot.status == OrderStatus::Filled
                    || snapshot.status == OrderStatus::Expired;
                self.cache_snapshot(order_id, snapshot.clone()).await;
                if terminal {
                    self.remove_order(order_id).await;
                }
                Ok(Some(snapshot))
            }
            None => {
                let fallback = self.cached_cancelled_snapshot(order_id).await;
                self.remove_order(order_id).await;
                Ok(fallback)
            }
        }
    }

    async fn cancel_all(&self) -> Result<Vec<OrderSnapshot>> {
        let entries: Vec<(String, OrderMetadata)> = {
            let index = self.order_index.lock().await;
            index
                .iter()
                .map(|(id, meta)| (id.clone(), meta.clone()))
                .collect()
        };

        if entries.is_empty() {
            return Ok(Vec::new());
        }

        let mut by_pool: HashMap<String, Vec<String>> = HashMap::new();
        for (order_id, meta) in entries {
            by_pool
                .entry(meta.pool_key.clone())
                .or_default()
                .push(order_id);
        }

        let mut cancelled = Vec::new();

        for (pool_key, orders) in by_pool {
            let open_on_chain = match self.account_open_orders(&pool_key).await {
                Ok(set) => set,
                Err(err) => {
                    warn!(pool = %pool_key, error = %err, "failed to fetch open orders before cancel_all");
                    HashSet::new()
                }
            };

            if !open_on_chain.is_empty() {
                let builder = self.build_cancel_all_orders(&pool_key).await?;
                self.execute_transaction(builder).await?;
                debug!(
                    pool = %pool_key,
                    open_count = open_on_chain.len(),
                    "submitted deepbook cancel_all transaction"
                );
            } else {
                debug!(
                    pool = %pool_key,
                    "cancel_all found no open orders on chain; skipping transaction"
                );
            }

            for order_id in orders {
                let parsed_id = match order_id.parse::<u128>() {
                    Ok(value) => value,
                    Err(err) => {
                        warn!(
                            order_id = %order_id,
                            error = %err,
                            "failed to parse order id when processing cancel_all results"
                        );
                        self.remove_order(&order_id).await;
                        continue;
                    }
                };

                match self
                    .fetch_order_snapshot(&pool_key, parsed_id)
                    .await?
                {
                    Some(snapshot) => {
                        self.cache_snapshot(&order_id, snapshot.clone()).await;
                        if snapshot.status == OrderStatus::Cancelled
                            || snapshot.status == OrderStatus::Filled
                            || snapshot.status == OrderStatus::Expired
                        {
                            self.remove_order(&order_id).await;
                        }
                        cancelled.push(snapshot);
                    }
                    None => {
                        if let Some(snapshot) = self.cached_cancelled_snapshot(&order_id).await {
                            self.remove_order(&order_id).await;
                            cancelled.push(snapshot);
                        } else {
                            self.remove_order(&order_id).await;
                        }
                    }
                }
            }
        }

        Ok(cancelled)
    }

    async fn record_fill(&self, order_id: &str, _fill: FillUpdate) -> Result<OrderSnapshot> {
        self.get_order(order_id)
            .await?
            .ok_or_else(|| anyhow!("order {order_id} not found"))
    }

    async fn claim_fills(&self) -> Result<ClaimSummary> {
        let pools: Vec<String> = {
            let guard = self.tracked_pools.lock().await;
            guard.iter().cloned().collect()
        };

        let mut claimed: HashMap<String, crate::ClaimableBalance> = HashMap::new();

        for pool_key in pools {
            let account_before = self.fetch_account(&pool_key).await?;
            let base_rebate = account_before.unclaimed_rebates.base;
            let quote_rebate = account_before.unclaimed_rebates.quote;

            info!(
                pool = %pool_key,
                base_unclaimed = base_rebate,
                quote_unclaimed = quote_rebate,
                base_settled = account_before.settled_balances.base,
                quote_settled = account_before.settled_balances.quote,
                base_owed = account_before.owed_balances.base,
                quote_owed = account_before.owed_balances.quote,
                "deepbook balance manager snapshot before claim"
            );

            if base_rebate <= f64::EPSILON && quote_rebate <= f64::EPSILON {
                continue;
            }

            let builder = self.build_claim_rebates(&pool_key).await?;
            self.execute_transaction(builder).await?;
            let pair = pool_key.replace('_', "/");
            let entry = claimed.entry(pair).or_default();
            entry.base_asset += base_rebate;
            entry.quote_asset += quote_rebate;
        }

        Ok(ClaimSummary::from_map(claimed))
    }

    async fn get_order(&self, order_id: &str) -> Result<Option<OrderSnapshot>> {
        let metadata = {
            let index = self.order_index.lock().await;
            index.get(order_id).cloned()
        };

        let meta = match metadata {
            Some(meta) => meta,
            None => return Ok(None),
        };

        let parsed_id = order_id
            .parse::<u128>()
            .context("order_id must be numeric for deepbook queries")?;

        match self
            .fetch_order_snapshot(&meta.pool_key, parsed_id)
            .await?
        {
            Some(snapshot) => {
                let terminal = snapshot.status == OrderStatus::Filled
                    || snapshot.status == OrderStatus::Cancelled
                    || snapshot.status == OrderStatus::Expired;
                self.cache_snapshot(order_id, snapshot.clone()).await;
                if terminal {
                    self.remove_order(order_id).await;
                }
                Ok(Some(snapshot))
            }
            None => {
                let fallback = self.cached_cancelled_snapshot(order_id).await;
                if fallback.is_some() {
                    self.remove_order(order_id).await;
                }
                Ok(fallback)
            }
        }
    }

    async fn list_orders(&self) -> Result<Vec<OrderSnapshot>> {
        let entries: Vec<(String, OrderMetadata)> = {
            let index = self.order_index.lock().await;
            index
                .iter()
                .map(|(id, meta)| (id.clone(), meta.clone()))
                .collect()
        };

        let mut snapshots = Vec::new();
        for (order_id, meta) in entries {
            let parsed_id = match order_id.parse::<u128>() {
                Ok(value) => value,
                Err(err) => {
                    warn!(
                        order_id,
                        error = %err,
                        "failed to parse deepbook order id when listing"
                    );
                    continue;
                }
            };
            match self
                .fetch_order_snapshot(&meta.pool_key, parsed_id)
                .await?
            {
                Some(snapshot) => {
                    let terminal = snapshot.status == OrderStatus::Filled
                        || snapshot.status == OrderStatus::Cancelled
                        || snapshot.status == OrderStatus::Expired;
                    self.cache_snapshot(&order_id, snapshot.clone()).await;
                    if terminal {
                        self.remove_order(&order_id).await;
                    }
                    snapshots.push(snapshot);
                }
                None => {
                    if let Some(snapshot) = self.cached_cancelled_snapshot(&order_id).await {
                        self.remove_order(&order_id).await;
                        snapshots.push(snapshot);
                    } else {
                        self.remove_order(&order_id).await;
                    }
                }
            }
        }
        Ok(snapshots)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use dotenvy::dotenv;
    use shared::types::MarketSide;
    use std::env;
    use std::sync::Once;

    fn init_tracing() {
        static START: Once = Once::new();
        START.call_once(|| {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
                .with_target(false)
                .try_init();
        });
    }

    const FULLNODE_URL_ENV: &str = "DEEPBOOK_SDK_FULLNODE_URL";
    const SIGNER_KEY_ENV: &str = "DEEPBOOK_SDK_SIGNER_KEY";
    const BALANCE_MANAGER_ID_ENV: &str = "DEEPBOOK_SDK_BALANCE_MANAGER_ID";
    const ENVIRONMENT_ENV: &str = "DEEPBOOK_SDK_ENV";
    const GAS_BUDGET_ENV: &str = "DEEPBOOK_SDK_GAS_BUDGET";
    const POOL_KEY_ENV: &str = "DEEPBOOK_SDK_TEST_POOL_KEY";
    const PAIR_ENV: &str = "DEEPBOOK_SDK_TEST_PAIR";
    const SIDE_ENV: &str = "DEEPBOOK_SDK_TEST_SIDE";
    const PRICE_ENV: &str = "DEEPBOOK_SDK_TEST_PRICE";
    const SIZE_ENV: &str = "DEEPBOOK_SDK_TEST_SIZE";

    #[derive(Clone)]
    struct LiveTestInputs {
        sdk_config: DeepbookSdkConfig,
        order_request: PlaceOrderRequest,
    }

    fn debug_env_snapshot() {
        let entries: [(&str, bool); 10] = [
            (FULLNODE_URL_ENV, false),
            (SIGNER_KEY_ENV, true),
            (BALANCE_MANAGER_ID_ENV, false),
            (ENVIRONMENT_ENV, false),
            (GAS_BUDGET_ENV, false),
            (POOL_KEY_ENV, false),
            (PAIR_ENV, false),
            (SIDE_ENV, false),
            (PRICE_ENV, false),
            (SIZE_ENV, false),
        ];
        eprintln!("deepbook sdk test env snapshot:");
        for (key, is_secret) in entries {
            match env::var(key) {
                Ok(value) => {
                    let trimmed = value.trim();
                    if trimmed.is_empty() {
                        eprintln!("  {key}=<empty>");
                    } else if is_secret {
                        eprintln!("  {key}=<set len={}>", trimmed.len());
                    } else {
                        eprintln!("  {key}={trimmed}");
                    }
                }
                Err(_) => eprintln!("  {key}=<missing>"),
            }
        }
    }

    fn env_var_non_empty(key: &str) -> Option<String> {
        let value = env::var(key).ok()?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    fn parse_market_side(value: &str) -> Option<MarketSide> {
        match value.to_ascii_lowercase().as_str() {
            "bid" => Some(MarketSide::Bid),
            "ask" => Some(MarketSide::Ask),
            _ => None,
        }
    }

    fn parse_environment(value: &str) -> Option<DeepbookEnvironment> {
        match value.to_ascii_lowercase().as_str() {
            "mainnet" => Some(DeepbookEnvironment::Mainnet),
            "testnet" => Some(DeepbookEnvironment::Testnet),
            _ => None,
        }
    }

    fn load_live_test_inputs() -> Option<LiveTestInputs> {
        init_tracing();
        let _ = dotenv();

        let fullnode_url = env_var_non_empty(FULLNODE_URL_ENV)?;
        let signer_key = env_var_non_empty(SIGNER_KEY_ENV)?;
        let balance_manager_id = env_var_non_empty(BALANCE_MANAGER_ID_ENV)?;
        let pool_key = env_var_non_empty(POOL_KEY_ENV)?;

        let price = env_var_non_empty(PRICE_ENV)?.parse::<f64>().ok()?;
        let size = env_var_non_empty(SIZE_ENV)?.parse::<f64>().ok()?;

        let environment = env_var_non_empty(ENVIRONMENT_ENV)
            .and_then(|value| parse_environment(&value))
            .unwrap_or(DeepbookEnvironment::Mainnet);
        let mut sdk_config = DeepbookSdkConfig::new(
            fullnode_url,
            signer_key,
            balance_manager_id,
            environment,
        );

        if let Some(gas_budget) = env_var_non_empty(GAS_BUDGET_ENV)
            .and_then(|value| value.parse::<u64>().ok())
        {
            sdk_config = sdk_config.with_gas_budget(gas_budget);
        }

        let default_pair = pool_key.replace('_', "/");
        let pair = env_var_non_empty(PAIR_ENV).unwrap_or(default_pair);
        let side = env_var_non_empty(SIDE_ENV)
            .and_then(|value| parse_market_side(&value))
            .unwrap_or(MarketSide::Bid);

        Some(LiveTestInputs {
            sdk_config,
            order_request: PlaceOrderRequest {
                pair,
                pool_key,
                side,
                price,
                size,
            },
        })
    }

    async fn backend_from_env() -> Result<Option<(SdkBackend, LiveTestInputs)>> {
        let Some(inputs) = load_live_test_inputs() else {
            debug_env_snapshot();
            eprintln!("skipping deepbook SDK backend tests: env configuration missing");
            return Ok(None);
        };

        match SdkBackend::new(inputs.sdk_config.clone()).await {
            Ok(backend) => Ok(Some((backend, inputs))),
            Err(err) => {
                debug_env_snapshot();
                eprintln!("skipping deepbook SDK backend tests: failed to construct backend: {err:?}");
                Ok(None)
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires live DeepBook access and funded balance manager"]
    async fn place_post_only_order_live_smoke() -> Result<()> {
        let Some((backend, inputs)) = backend_from_env().await? else {
            return Ok(());
        };

        let request = inputs.order_request.clone();
        let snapshot = backend
            .place_post_only_order(request.clone())
            .await?;

        assert_eq!(snapshot.pair, request.pair);
        assert_eq!(snapshot.side, request.side);
        assert!(snapshot.price > 0.0);
        assert!(snapshot.size > 0.0);

        let _ = backend.cancel_order(&snapshot.id).await?;

        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires live DeepBook access and funded balance manager"]
    async fn cancel_order_live_smoke() -> Result<()> {
        let Some((backend, inputs)) = backend_from_env().await? else {
            return Ok(());
        };

        let request = inputs.order_request.clone();
        let snapshot = backend
            .place_post_only_order(request)
            .await?;
        let order_id = snapshot.id.clone();

        let cancelled = backend.cancel_order(&order_id).await?;
        let Some(cancel_snapshot) = cancelled else {
            anyhow::bail!("order {order_id} should return a cancellation snapshot");
        };

        assert_eq!(cancel_snapshot.id, order_id);
        assert!(
            matches!(
                cancel_snapshot.status,
                OrderStatus::Cancelled | OrderStatus::Filled | OrderStatus::Expired
            ),
            "unexpected order status after cancellation: {:?}",
            cancel_snapshot.status
        );

        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires live DeepBook access and funded balance manager"]
    async fn cancel_all_live_smoke() -> Result<()> {
        let Some((backend, inputs)) = backend_from_env().await? else {
            return Ok(());
        };

        let request = inputs.order_request.clone();
        let created = backend
            .place_post_only_order(request)
            .await?;

        let cancelled = backend.cancel_all().await?;
        assert!(
            cancelled.iter().any(|snapshot| snapshot.id == created.id),
            "cancel_all response missing just-created order"
        );

        Ok(())
    }
}

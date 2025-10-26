use crate::error::LendingError;
use anyhow::{Context, Result, anyhow, bail, ensure};
use bcs::from_bytes;
use dotenvy::dotenv;
use move_core_types::identifier::Identifier;
use move_core_types::language_storage::TypeTag;
use move_core_types::u256::U256;
use reqwest::Client;
use rust_decimal::Decimal;
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use serde::Deserialize;
use serde_json::Value;
use shared::config::{NaviLendingConfig, SharedObjectConfig};
use shared_crypto::intent::{Intent, IntentMessage};
use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::Arc;
use sui_sdk::rpc_types::{
    Coin, SuiTransactionBlockEffectsAPI, SuiTransactionBlockResponse,
    SuiTransactionBlockResponseOptions, SuiTypeTag,
};
use sui_sdk::types::programmable_transaction_builder::ProgrammableTransactionBuilder;
use sui_sdk::types::quorum_driver_types::ExecuteTransactionRequestType;
use sui_sdk::{SuiClient, SuiClientBuilder};
use sui_types::base_types::{ObjectID, ObjectRef, SequenceNumber, SuiAddress};
use sui_types::crypto::{EncodeDecodeBase64, Signature, SuiKeyPair};
use sui_types::transaction::{
    Argument, Command, ObjectArg, SharedObjectMutability, Transaction, TransactionData,
    TransactionKind,
};
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

const MIN_FETCH_BATCH: usize = 50;
const SUI_COIN_TYPE: &str = "0x2::sui::SUI";
const NAVI_STORAGE_MODULE: &str = "storage";
const NAVI_GET_USER_ASSETS_FN: &str = "get_user_assets";
const NAVI_GET_INDEX_FN: &str = "get_index";
const NAVI_UI_GETTER_MODULE: &str = "getter_unchecked";
const NAVI_UI_GET_USER_STATE_FN: &str = "get_user_state";
const API_INDEX_DECIMALS: u32 = 27;
const ONCHAIN_INDEX_DECIMALS: u32 = 18;
/// UI Getter 返回的余额按照 share token 的 9 位精度计量。
const UI_GETTER_SHARE_DECIMALS: u32 = 9;

#[derive(Clone)]
pub struct NaviOnchainClient {
    client: Arc<SuiClient>,
    signer: Arc<SuiKeyPair>,
    sender: SuiAddress,
    package: ObjectID,
    module: Identifier,
    function: Identifier,
    coin_type: String,
    coin_type_tag: TypeTag,
    coin_decimals: u8,
    asset_id: u8,
    gas_budget: u64,
    clock: SharedObjectArg,
    storage: SharedObjectArg,
    pool: SharedObjectArg,
    incentive_v2: SharedObjectArg,
    incentive_v3: SharedObjectArg,
    storage_module: Identifier,
    storage_get_user_assets: Identifier,
    storage_get_index: Identifier,
    ui_getter_package: ObjectID,
    ui_getter_module: Identifier,
    ui_getter_function: Identifier,
    http_client: Client,
    api_base_url: String,
    api_env: String,
    pool_cache: OnceCell<Vec<NaviPoolInfo>>,
    gas_coins_cache: OnceCell<Vec<ObjectRef>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaviUserAssets {
    /// Navi 记录的资产 ID 列表（原始字节形式）。
    pub asset_ids: Vec<u8>,
    /// `storage::get_user_assets` 返回的原始字节向量。
    pub raw_values: Vec<u8>,
    /// 如果能够均匀切分，则将 `raw_values` 分片后的条目。
    pub entries: Vec<Vec<u8>>,
}

impl NaviUserAssets {
    fn new(asset_ids: Vec<u8>, raw_values: Vec<u8>) -> Self {
        let entries = Self::split_entries(&asset_ids, &raw_values);
        Self {
            asset_ids,
            raw_values,
            entries,
        }
    }

    fn split_entries(ids: &[u8], raw: &[u8]) -> Vec<Vec<u8>> {
        if ids.is_empty() || raw.is_empty() {
            return Vec::new();
        }
        if raw.len() % ids.len() == 0 {
            let chunk = raw.len() / ids.len();
            if chunk > 0 {
                return raw
                    .chunks(chunk)
                    .map(|slice| slice.to_vec())
                    .collect::<Vec<_>>();
            }
        }
        vec![raw.to_vec()]
    }

    pub fn entry_for(&self, asset_id: u8) -> Option<Vec<u8>> {
        let idx = self.asset_ids.iter().position(|id| *id == asset_id)?;
        self.entries.get(idx).cloned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaviAssetIndex {
    pub asset_id: u8,
    pub borrow_index: U256,
    pub supply_index: U256,
    pub scale: u32,
    pub coin_decimals: Option<u8>,
}

impl NaviAssetIndex {
    pub fn borrow_index_decimal(&self) -> Option<Decimal> {
        u256_to_decimal_scaled(&self.borrow_index, self.scale)
    }

    pub fn supply_index_decimal(&self) -> Option<Decimal> {
        u256_to_decimal_scaled(&self.supply_index, self.scale)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NaviAssetBalance {
    pub asset_id: u8,
    pub supplied_shares: U256,
    pub borrowed_shares: U256,
    pub index: NaviAssetIndex,
}

impl NaviAssetBalance {
    pub fn supplied_amount_decimal(&self, decimals: u8) -> Option<Decimal> {
        let amount = apply_index(
            &self.supplied_shares,
            &self.index.supply_index,
            self.index.scale,
        )?;
        amount_from_shares(&amount, decimals)
    }

    pub fn borrowed_amount_decimal(&self, decimals: u8) -> Option<Decimal> {
        let amount = apply_index(
            &self.borrowed_shares,
            &self.index.borrow_index,
            self.index.scale,
        )?;
        amount_from_shares(&amount, decimals)
    }

    pub fn supplied_shares_decimal(&self, decimals: u8) -> Option<Decimal> {
        u256_to_decimal(&self.supplied_shares, decimals)
    }

    pub fn borrowed_shares_decimal(&self, decimals: u8) -> Option<Decimal> {
        u256_to_decimal(&self.borrowed_shares, decimals)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct NaviUserStateEntry {
    asset_id: u8,
    #[serde(rename = "borrow_balance")]
    borrow_balance: U256,
    #[serde(rename = "supply_balance")]
    supply_balance: U256,
}

#[derive(Debug, Deserialize)]
struct NaviPoolsResponse {
    data: Vec<Value>,
}

#[derive(Debug, Deserialize, Clone)]
struct NaviConfigResponse {
    data: NaviApiConfig,
}

#[derive(Debug, Deserialize, Clone)]
struct NaviApiConfig {
    #[serde(rename = "uiGetter")]
    ui_getter: String,
}

#[derive(Debug, Clone)]
struct NaviPoolInfo {
    asset_id: u8,
    current_supply_index: U256,
    current_borrow_index: U256,
    coin_decimals: Option<u8>,
}

impl NaviPoolInfo {
    fn try_from_value(value: Value) -> Result<Self> {
        let asset_id = extract_u8(&value, "id")?;
        let current_supply_index = extract_u256(&value, "currentSupplyIndex")?;
        let current_borrow_index = extract_u256(&value, "currentBorrowIndex")?;
        let coin_decimals = extract_u8_optional(&value, "coinDecimals")
            .or_else(|| extract_u8_optional(&value, "supplyDecimals"))
            .or_else(|| extract_u8_optional(&value, "decimals"));

        Ok(Self {
            asset_id,
            current_supply_index,
            current_borrow_index,
            coin_decimals,
        })
    }
}

impl NaviOnchainClient {
    pub fn client(&self) -> Arc<SuiClient> {
        Arc::clone(&self.client)
    }

    pub fn package(&self) -> ObjectID {
        self.package
    }

    pub fn ui_getter_package(&self) -> ObjectID {
        self.ui_getter_package
    }

    pub async fn connect(config: &NaviLendingConfig) -> Result<Self> {
        let _ = dotenv();

        let rpc_endpoint = config
            .rpc_endpoint
            .clone()
            .with_context(|| "lending.navi.rpc_endpoint 未配置")?;
        let package_id = config
            .package_id
            .clone()
            .with_context(|| "lending.navi.package_id 未配置")?;

        let package = ObjectID::from_str(&package_id)
            .with_context(|| format!("invalid package id {}", package_id))?;
        let module = Identifier::new(config.module.clone())
            .with_context(|| format!("invalid module name {}", config.module))?;
        let function = Identifier::new(config.function.clone())
            .with_context(|| format!("invalid function name {}", config.function))?;

        let coin_type_tag = sui_sdk::types::parse_sui_type_tag(&config.coin_type)
            .with_context(|| format!("invalid coin type {}", config.coin_type))?;

        let clock = SharedObjectArg::from_config(config.clock.as_ref(), "lending.navi.clock")?;
        let storage =
            SharedObjectArg::from_config(config.storage.as_ref(), "lending.navi.storage")?;
        let pool = SharedObjectArg::from_config(config.pool.as_ref(), "lending.navi.pool")?;
        let incentive_v2 = SharedObjectArg::from_config(
            config.incentive_v2.as_ref(),
            "lending.navi.incentive_v2",
        )?;
        let incentive_v3 = SharedObjectArg::from_config(
            config.incentive_v3.as_ref(),
            "lending.navi.incentive_v3",
        )?;

        let signer_key = std::env::var(&config.signer_env).with_context(|| {
            format!(
                "environment variable {} missing Navi signer key",
                config.signer_env
            )
        })?;
        let signer = Arc::new(parse_signer_key(&signer_key)?);
        let sender = SuiAddress::from(&signer.public());

        let client = Arc::new(
            SuiClientBuilder::default()
                .build(rpc_endpoint.clone())
                .await
                .with_context(|| format!("failed to connect to Sui RPC {}", rpc_endpoint))?,
        );
        let http_client = Client::builder()
            .user_agent("navi-rust-client/0.1")
            .build()
            .context("failed to construct HTTP client for Navi client")?;

        let ui_getter_module = Identifier::new(NAVI_UI_GETTER_MODULE.to_string())
            .with_context(|| format!("invalid module name {}", NAVI_UI_GETTER_MODULE))?;
        let ui_getter_function = Identifier::new(NAVI_UI_GET_USER_STATE_FN.to_string())
            .with_context(|| format!("invalid function name {}", NAVI_UI_GET_USER_STATE_FN))?;
        let ui_getter_package = resolve_ui_getter_package(
            config.ui_getter_package.as_deref(),
            &http_client,
            &config.api_base_url,
            &config.api_env,
        )
        .await?;

        info!(
            sender = %sender,
            rpc = %rpc_endpoint,
            coin_type = %config.coin_type,
            module = config.module,
            function = config.function,
            "initialized Navi on-chain client"
        );

        let storage_module = Identifier::new(NAVI_STORAGE_MODULE.to_string())
            .with_context(|| format!("invalid module name {}", NAVI_STORAGE_MODULE))?;
        let storage_get_user_assets = Identifier::new(NAVI_GET_USER_ASSETS_FN.to_string())
            .with_context(|| format!("invalid function name {}", NAVI_GET_USER_ASSETS_FN))?;
        let storage_get_index = Identifier::new(NAVI_GET_INDEX_FN.to_string())
            .with_context(|| format!("invalid function name {}", NAVI_GET_INDEX_FN))?;
        Ok(Self {
            client,
            signer,
            sender,
            package,
            module,
            function,
            coin_type: config.coin_type.clone(),
            coin_type_tag,
            coin_decimals: config.coin_decimals,
            asset_id: config.asset_id,
            gas_budget: config.gas_budget,
            clock,
            storage,
            pool,
            incentive_v2,
            incentive_v3,
            storage_module,
            storage_get_user_assets,
            storage_get_index,
            ui_getter_package,
            ui_getter_module,
            ui_getter_function,
            http_client,
            api_base_url: config.api_base_url.clone(),
            api_env: config.api_env.clone(),
            pool_cache: OnceCell::new(),
            gas_coins_cache: OnceCell::new(),
        })
    }

    pub async fn deposit(&self, amount: f64) -> Result<SuiTransactionBlockResponse> {
        ensure!(amount > 0.0, LendingError::InvalidAmount(amount));
        let amount_units = self
            .amount_to_base_units(amount)
            .with_context(|| format!("failed to convert amount {} to base units", amount))?;
        let coin_refs = self.collect_usdc_coins(amount_units).await?;
        let builder = self.build_deposit_transaction(amount_units, &coin_refs)?;
        let response = self.execute_transaction(builder).await?;

        info!(
            amount,
            amount_units,
            tx_digest = %response.digest,
            "submitted Navi USDC deposit"
        );

        Ok(response)
    }

    fn amount_to_base_units(&self, amount: f64) -> Result<u64> {
        let decimal = Decimal::from_f64(amount)
            .ok_or_else(|| anyhow!("invalid decimal amount {}", amount))?;
        ensure!(decimal > Decimal::ZERO, LendingError::InvalidAmount(amount));
        let scale = Decimal::from(10u64.pow(u32::from(self.coin_decimals)));
        let units = (decimal * scale).round();
        let units_u64 = units
            .to_u64()
            .ok_or_else(|| anyhow!("amount {} overflows {}", amount, scale))?;
        ensure!(units_u64 > 0, LendingError::InvalidAmount(amount));
        Ok(units_u64)
    }

    async fn collect_usdc_coins(&self, required: u64) -> Result<Vec<ObjectRef>> {
        let mut cursor = None;
        let mut accumulated: Vec<Coin> = Vec::new();
        let mut total: u128 = 0;
        let required_u128 = u128::from(required);

        while total < required_u128 {
            let page = self
                .client
                .coin_read_api()
                .get_coins(
                    self.sender,
                    Some(self.coin_type.clone()),
                    cursor.clone(),
                    Some(MIN_FETCH_BATCH),
                )
                .await
                .with_context(|| format!("failed to fetch {} coins", self.coin_type))?;

            if page.data.is_empty() {
                break;
            }

            for coin in &page.data {
                total += u128::from(coin.balance);
            }

            accumulated.extend(page.data.into_iter());

            if let Some(next) = page.next_cursor {
                cursor = Some(next);
            } else {
                break;
            }
        }

        ensure!(
            total >= required_u128,
            anyhow!(
                "insufficient {} balance: required {}, available {}",
                self.coin_type,
                required_u128,
                total
            )
        );

        accumulated.sort_by(|a, b| b.balance.cmp(&a.balance));
        let mut selected = VecDeque::new();
        let mut running = 0u128;
        for coin in accumulated {
            running += u128::from(coin.balance);
            selected.push_back(coin);
            if running >= required_u128 {
                break;
            }
        }

        let refs = selected
            .into_iter()
            .map(|coin| coin.object_ref())
            .collect::<Vec<_>>();

        ensure!(!refs.is_empty(), "no coins selected for deposit");
        Ok(refs)
    }

    /// 调用 `storage::get_user_assets` 查询指定地址的资产信息。
    pub async fn fetch_user_assets(&self, user: SuiAddress) -> Result<NaviUserAssets> {
        let mut builder = ProgrammableTransactionBuilder::new();

        let storage = self.storage.to_argument(&mut builder)?;
        let user_arg = builder.pure(user)?;

        builder.programmable_move_call(
            self.package,
            self.storage_module.clone(),
            self.storage_get_user_assets.clone(),
            vec![],
            vec![storage, user_arg],
        );

        let programmable = builder.finish();
        let tx_kind = TransactionKind::ProgrammableTransaction(programmable);
        let mut inspection = self
            .client
            .read_api()
            .dev_inspect_transaction_block(self.sender, tx_kind, None, None, None)
            .await
            .context("dev inspect Navi storage::get_user_assets 失败")?;

        if let Some(error) = inspection.error.clone() {
            bail!("storage::get_user_assets 执行失败: {error}");
        }

        let mut results = inspection
            .results
            .take()
            .ok_or_else(|| anyhow!("storage::get_user_assets 未返回结果"))?;

        ensure!(
            !results.is_empty(),
            "storage::get_user_assets 没有返回任何执行结果"
        );
        let execution = results.remove(0);
        ensure!(
            !execution.return_values.is_empty(),
            "storage::get_user_assets 未返回任何值"
        );

        let (asset_bytes, asset_tag) = execution
            .return_values
            .get(0)
            .ok_or_else(|| anyhow!("storage::get_user_assets 缺少 asset_ids 返回值"))?;
        let (value_bytes, value_tag) = execution
            .return_values
            .get(1)
            .ok_or_else(|| anyhow!("storage::get_user_assets 缺少 raw_values 返回值"))?;

        ensure_vector_of_u8(asset_tag, "asset_ids")?;
        ensure_vector_of_u8(value_tag, "raw_values")?;
        debug!(
            asset_ids_len = asset_bytes.len(),
            raw_values_bcs_len = value_bytes.len(),
            raw_values_bcs_hex = %encode_hex(value_bytes),
            "storage::get_user_assets raw return snapshot"
        );

        let asset_ids: Vec<u8> = from_bytes(asset_bytes)
            .context("解析 storage::get_user_assets 返回的 asset_ids 失败")?;
        let raw_values: Vec<u8> = from_bytes(value_bytes)
            .context("解析 storage::get_user_assets 返回的 raw_values 失败")?;

        Ok(NaviUserAssets::new(asset_ids, raw_values))
    }

    /// 查询当前签名地址自身的资产记录。
    pub async fn fetch_self_assets(&self) -> Result<NaviUserAssets> {
        self.fetch_user_assets(self.sender).await
    }

    /// 调用 `storage::get_index` 获取指定资产的借款/抵押指数。
    pub async fn fetch_asset_index(&self, asset_id: u8) -> Result<NaviAssetIndex> {
        let mut builder = ProgrammableTransactionBuilder::new();

        let storage = self.storage.to_argument(&mut builder)?;
        let asset_arg = builder.pure(asset_id)?;

        builder.programmable_move_call(
            self.package,
            self.storage_module.clone(),
            self.storage_get_index.clone(),
            vec![],
            vec![storage, asset_arg],
        );

        let programmable = builder.finish();
        let tx_kind = TransactionKind::ProgrammableTransaction(programmable);
        let mut inspection = self
            .client
            .read_api()
            .dev_inspect_transaction_block(self.sender, tx_kind, None, None, None)
            .await
            .with_context(|| {
                format!("dev inspect storage::get_index 失败 (asset_id={asset_id})")
            })?;

        if let Some(error) = inspection.error.clone() {
            bail!("storage::get_index 执行失败: {error}");
        }

        let mut results = inspection
            .results
            .take()
            .ok_or_else(|| anyhow!("storage::get_index 未返回结果"))?;
        ensure!(
            !results.is_empty(),
            "storage::get_index 没有返回任何执行结果"
        );

        let execution = results.remove(0);
        ensure!(
            execution.return_values.len() >= 2,
            "storage::get_index 返回值数量不足"
        );

        let (borrow_bytes, borrow_tag) = &execution.return_values[0];
        let (supply_bytes, supply_tag) = &execution.return_values[1];

        ensure_u256(borrow_tag, "borrow_index")?;
        ensure_u256(supply_tag, "supply_index")?;

        let borrow_index: U256 =
            from_bytes(borrow_bytes).context("解析 storage::get_index borrow_index 失败")?;
        let supply_index: U256 =
            from_bytes(supply_bytes).context("解析 storage::get_index supply_index 失败")?;

        debug!(
            asset_id,
            borrow_index_raw = %borrow_index,
            supply_index_raw = %supply_index,
            "storage::get_index 返回原始指数"
        );

        Ok(NaviAssetIndex {
            asset_id,
            borrow_index,
            supply_index,
            scale: ONCHAIN_INDEX_DECIMALS,
            coin_decimals: Some(self.coin_decimals),
        })
    }

    async fn fetch_pool_index(&self, asset_id: u8) -> Result<NaviAssetIndex> {
        let pools = self
            .pool_cache
            .get_or_try_init(|| async { self.load_pools().await })
            .await?;
        let pool = pools
            .iter()
            .find(|pool| pool.asset_id == asset_id)
            .cloned()
            .with_context(|| format!("未找到资产 {asset_id} 的池化数据"))?;

        Ok(NaviAssetIndex {
            asset_id,
            borrow_index: pool.current_borrow_index,
            supply_index: pool.current_supply_index,
            scale: API_INDEX_DECIMALS,
            coin_decimals: pool.coin_decimals,
        })
    }

    async fn load_pools(&self) -> Result<Vec<NaviPoolInfo>> {
        let base = self.api_base_url.trim_end_matches('/');
        let url = format!("{base}/api/navi/pools?env={}", self.api_env);
        let body = self.fetch_json(&url, "Navi Pools").await?;
        let payload: NaviPoolsResponse = serde_json::from_str(&body)
            .with_context(|| format!("解析 Navi Pools 响应 JSON 失败: {}", body))?;

        payload
            .data
            .into_iter()
            .map(|value| NaviPoolInfo::try_from_value(value))
            .collect()
    }

    async fn fetch_user_state_entries(&self, owner: SuiAddress) -> Result<Vec<NaviUserStateEntry>> {
        let mut builder = ProgrammableTransactionBuilder::new();

        let storage = self.storage.to_argument(&mut builder)?;
        let owner_arg = builder.pure(owner)?;

        builder.programmable_move_call(
            self.ui_getter_package,
            self.ui_getter_module.clone(),
            self.ui_getter_function.clone(),
            vec![],
            vec![storage, owner_arg],
        );

        let programmable = builder.finish();
        let tx_kind = TransactionKind::ProgrammableTransaction(programmable);
        let mut inspection = self
            .client
            .read_api()
            .dev_inspect_transaction_block(owner, tx_kind, None, None, None)
            .await
            .with_context(|| {
                format!(
                    "dev inspect ui_getter::get_user_state 失败 (owner={})",
                    owner
                )
            })?;

        if let Some(error) = inspection.error.clone() {
            bail!("ui_getter::get_user_state 执行失败: {error}");
        }

        let mut results = inspection
            .results
            .take()
            .ok_or_else(|| anyhow!("ui_getter::get_user_state 未返回结果"))?;
        ensure!(
            !results.is_empty(),
            "ui_getter::get_user_state 没有返回任何执行结果"
        );

        let execution = results.remove(0);
        ensure!(
            !execution.return_values.is_empty(),
            "ui_getter::get_user_state 未返回任何值"
        );

        let (entries_bytes, _) = &execution.return_values[0];
        let entries: Vec<NaviUserStateEntry> =
            from_bytes(entries_bytes).context("解析 ui_getter::get_user_state 返回值失败")?;

        debug!(
            owner = %owner,
            entries_len = entries.len(),
            "ui_getter::get_user_state 返回条目"
        );

        Ok(entries)
    }

    async fn fetch_json(&self, url: &str, label: &str) -> Result<String> {
        fetch_json_raw(&self.http_client, url, label).await
    }

    /// 调用 Navi UI Getter 获取指定资产的抵押/借款数值。
    pub async fn fetch_user_balance(&self, asset_id: u8) -> Result<NaviAssetBalance> {
        let entries = self.fetch_user_state_entries(self.sender).await?;
        let (supplied_shares, borrowed_shares) = entries
            .into_iter()
            .find(|entry| entry.asset_id == asset_id)
            .map(|entry| (entry.supply_balance, entry.borrow_balance))
            .unwrap_or_else(|| (U256::zero(), U256::zero()));

        debug!(
            asset_id,
            supplied_shares = %supplied_shares,
            borrowed_shares = %borrowed_shares,
            "ui_getter::get_user_state 返回原始数值 (shares)"
        );

        let index = match self.fetch_pool_index(asset_id).await {
            Ok(idx) => idx,
            Err(err) => {
                warn!(asset_id, error = %err, "fetch_pool_index 失败，回退到链上 get_index");
                self.fetch_asset_index(asset_id).await?
            }
        };

        Ok(NaviAssetBalance {
            asset_id,
            supplied_shares,
            borrowed_shares,
            index,
        })
    }

    fn build_deposit_transaction(
        &self,
        amount_units: u64,
        coins: &[ObjectRef],
    ) -> Result<ProgrammableTransactionBuilder> {
        ensure!(!coins.is_empty(), "deposit requires at least one coin");

        let mut builder = ProgrammableTransactionBuilder::new();
        let primary = if coins.len() == 1 {
            builder.obj(ObjectArg::ImmOrOwnedObject(coins[0]))?
        } else {
            builder.smash_coins(coins.to_vec())?
        };

        let amount_arg_split = builder.pure(amount_units)?;
        let split_result = builder.command(Command::SplitCoins(primary, vec![amount_arg_split]));
        let split_index = match split_result {
            Argument::Result(idx) => idx,
            other => bail!("unexpected split result {:?}", other),
        };
        let deposit_coin = Argument::NestedResult(split_index, 0);

        let clock = self.clock.to_argument(&mut builder)?;
        let storage = self.storage.to_argument(&mut builder)?;
        let pool = self.pool.to_argument(&mut builder)?;
        let incentive_v2 = self.incentive_v2.to_argument(&mut builder)?;
        let incentive_v3 = self.incentive_v3.to_argument(&mut builder)?;

        let asset_id = builder.pure(self.asset_id)?;
        let amount_arg_call = builder.pure(amount_units)?;

        builder.programmable_move_call(
            self.package,
            self.module.clone(),
            self.function.clone(),
            vec![self.coin_type_tag.clone()],
            vec![
                clock,
                storage,
                pool,
                asset_id,
                deposit_coin,
                amount_arg_call,
                incentive_v2,
                incentive_v3,
            ],
        );

        Ok(builder)
    }

    async fn execute_transaction(
        &self,
        builder: ProgrammableTransactionBuilder,
    ) -> Result<SuiTransactionBlockResponse> {
        let programmable = builder.finish();
        let gas_price = self
            .client
            .read_api()
            .get_reference_gas_price()
            .await
            .context("failed to fetch Sui gas price")?;
        let gas_payment = self
            .select_gas_coins(self.gas_budget)
            .await
            .context("failed to select SUI gas coins")?;

        let tx_data = TransactionData::new_programmable(
            self.sender,
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
            .client
            .quorum_driver_api()
            .execute_transaction_block(
                transaction,
                options,
                Some(ExecuteTransactionRequestType::WaitForLocalExecution),
            )
            .await
            .context("failed to execute Navi deposit transaction")?;

        if let Some(effects) = &response.effects {
            debug!(
                digest = %effects.transaction_digest(),
                status = ?effects.status(),
                "Navi deposit transaction effects"
            );
        }

        Ok(response)
    }

    async fn select_gas_coins(&self, amount: u64) -> Result<Vec<ObjectRef>> {
        if let Some(cached) = self.gas_coins_cache.get() {
            return Ok(cached.clone());
        }

        let coins = self
            .client
            .coin_read_api()
            .select_coins(
                self.sender,
                Some(SUI_COIN_TYPE.to_string()),
                amount as u128,
                vec![],
            )
            .await
            .context("failed to select SUI coins for gas")?;

        ensure!(!coins.is_empty(), "no SUI coins available for gas payment");
        let refs = coins.iter().map(Coin::object_ref).collect::<Vec<_>>();
        let _ = self.gas_coins_cache.set(refs.clone());
        Ok(refs)
    }
}

#[derive(Clone, Copy)]
struct SharedObjectArg {
    id: ObjectID,
    version: SequenceNumber,
    mutable: bool,
}

impl SharedObjectArg {
    fn from_config(config: Option<&SharedObjectConfig>, label: &str) -> Result<Self> {
        let cfg = config.with_context(|| format!("{} 未配置", label))?;
        let id = ObjectID::from_str(&cfg.object_id)
            .with_context(|| format!("invalid object id {} for {}", cfg.object_id, label))?;
        Ok(Self {
            id,
            version: SequenceNumber::from(cfg.initial_shared_version),
            mutable: cfg.mutable,
        })
    }

    fn to_argument(&self, builder: &mut ProgrammableTransactionBuilder) -> Result<Argument> {
        let arg = ObjectArg::SharedObject {
            id: self.id,
            initial_shared_version: self.version,
            mutability: if self.mutable {
                SharedObjectMutability::Mutable
            } else {
                SharedObjectMutability::Immutable
            },
        };
        builder.obj(arg)
    }
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

async fn resolve_ui_getter_package(
    override_package: Option<&str>,
    http_client: &Client,
    api_base_url: &str,
    api_env: &str,
) -> Result<ObjectID> {
    if let Some(value) = override_package {
        return parse_object_id(value)
            .with_context(|| format!("invalid ui_getter package id override {}", value));
    }
    let config = load_navi_config(http_client, api_base_url, api_env).await?;
    parse_object_id(&config.ui_getter)
        .with_context(|| format!("invalid ui_getter package id {}", config.ui_getter))
}

async fn load_navi_config(
    http_client: &Client,
    api_base_url: &str,
    api_env: &str,
) -> Result<NaviApiConfig> {
    let base = api_base_url.trim_end_matches('/');
    let url = format!("{base}/api/navi/config?env={api_env}");
    let body = fetch_json_raw(http_client, &url, "Navi Config").await?;
    let payload: NaviConfigResponse = serde_json::from_str(&body)
        .with_context(|| format!("解析 Navi Config 响应 JSON 失败: {}", body))?;
    Ok(payload.data)
}

async fn fetch_json_raw(client: &Client, url: &str, label: &str) -> Result<String> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .with_context(|| format!("请求 {label} 接口失败: {url}"))?;
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("读取 {label} 响应体失败: {url}"))?;
    if !status.is_success() {
        bail!(
            "{label} 接口返回错误状态: {} status={} body={}",
            url,
            status.as_u16(),
            body
        );
    }
    Ok(body)
}

fn parse_object_id(raw: &str) -> Result<ObjectID> {
    let trimmed = raw.trim();
    let package_part = trimmed.split("::").next().unwrap_or(trimmed);
    ObjectID::from_str(package_part.trim())
        .with_context(|| format!("failed to parse object id from {}", raw))
}

fn ensure_vector_of_u8(tag: &SuiTypeTag, label: &str) -> Result<()> {
    let move_tag: TypeTag = tag
        .clone()
        .try_into()
        .with_context(|| format!("{label} 类型解析失败: {tag:?}"))?;
    ensure!(
        matches!(&move_tag, TypeTag::Vector(inner) if matches!(inner.as_ref(), TypeTag::U8)),
        "{label} 类型不是 vector<u8>: {move_tag:?}"
    );
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn u256_to_decimal(amount: &U256, decimals: u8) -> Option<Decimal> {
    let raw = Decimal::from_str(&amount.to_string()).ok()?;
    let scale = decimal_pow10(u32::from(decimals));
    Some((raw / scale).normalize())
}

fn amount_from_shares(amount: &U256, asset_decimals: u8) -> Option<Decimal> {
    let raw = Decimal::from_str(&amount.to_string()).ok()?;
    let exponent = UI_GETTER_SHARE_DECIMALS.max(u32::from(asset_decimals));
    let scale = decimal_pow10(exponent);
    Some((raw / scale).normalize())
}

fn u256_to_decimal_scaled(amount: &U256, scale_decimals: u32) -> Option<Decimal> {
    let raw = Decimal::from_str(&amount.to_string()).ok()?;
    let scale = decimal_pow10(scale_decimals);
    Some((raw / scale).normalize())
}

fn ensure_u256(tag: &SuiTypeTag, label: &str) -> Result<()> {
    let move_tag: TypeTag = tag
        .clone()
        .try_into()
        .with_context(|| format!("{label} 类型解析失败: {tag:?}"))?;
    ensure!(
        matches!(move_tag, TypeTag::U256),
        "{label} 类型不是 u256: {move_tag:?}"
    );
    Ok(())
}

fn apply_index(shares: &U256, index: &U256, scale_decimals: u32) -> Option<U256> {
    if shares == &U256::zero() || index == &U256::zero() {
        return Some(U256::zero());
    }
    let scale = pow10_u256(scale_decimals);
    let half_scale = scale.clone() / U256::from(2u8);
    let product = shares.clone().checked_mul(index.clone())?;
    let adjusted = product.checked_add(half_scale)?;
    Some(adjusted / scale)
}

fn pow10_u256(exp: u32) -> U256 {
    let mut result = U256::one();
    let base = U256::from(10u8);
    for _ in 0..exp {
        result = result * base;
    }
    result
}

fn decimal_pow10(exp: u32) -> Decimal {
    let mut result = Decimal::ONE;
    let base = Decimal::from(10u64);
    for _ in 0..exp {
        result *= base;
    }
    result
}

fn extract_u8(value: &Value, key: &str) -> Result<u8> {
    extract_u8_optional(value, key).with_context(|| format!("{key} 字段不存在或不是有效的 u8"))
}

fn extract_u8_optional(value: &Value, key: &str) -> Option<u8> {
    let entry = value.get(key)?;
    match entry {
        Value::Number(num) => num.as_u64().map(|v| v as u8),
        Value::String(s) => s.parse::<u64>().ok().map(|v| v as u8),
        _ => None,
    }
}

fn extract_u256(value: &Value, key: &str) -> Result<U256> {
    let entry = value.get(key).with_context(|| format!("{key} 字段缺失"))?;
    let as_str = match entry {
        Value::String(s) => s.as_str().to_string(),
        Value::Number(num) => num.to_string(),
        other => other.to_string(),
    };
    U256::from_str(as_str.trim()).with_context(|| format!("无法解析 {key} 为 U256: {as_str}"))
}

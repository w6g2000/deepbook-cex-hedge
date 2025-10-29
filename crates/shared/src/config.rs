use crate::types::NaviAssetId;
use anyhow::{Context, Result};
use dotenvy::dotenv;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_CONFIG_PATH: &str = "config/app.toml";

#[derive(Debug, Deserialize, Clone, Default)]
pub struct AppConfig {
    /// 可选的应用监听地址（如 `0.0.0.0:9000`），目前仅用于日志记录。
    pub binding: Option<String>,
    /// 运行时需要订阅/交易的交易对列表，留空则使用默认交易对。
    #[serde(default)]
    pub pairs: Vec<TradingPair>,
    /// 套利策略相关的阈值配置。
    #[serde(default)]
    pub arbitrage: ArbitrageConfig,
    /// DeepBook 订单簿订阅/轮询所需的参数。
    #[serde(default)]
    pub deepbook: DeepbookConfig,
    /// 中心化交易所（Binance）相关的网络参数。
    #[serde(default)]
    pub cex: CexConfig,
    /// 借贷流程使用的 Navi 参数与阈值。
    #[serde(default)]
    pub lending: LendingConfig,
    /// 链上合约地址和外部依赖。
    #[serde(default)]
    pub contracts: ContractsConfig,
    /// 策略执行层的风控与触发条件。
    #[serde(default)]
    pub strategy: StrategyConfig,
    /// 程序优雅退出时的时间控制。
    #[serde(default)]
    pub shutdown: ShutdownConfig,
    /// 跨 venue 库存搬运与借贷联动的参数。
    #[serde(default)]
    pub rebalance: RebalanceConfig,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct TradingPair {
    /// 交易对中的基础资产符号（如 `WAL`）。
    pub base: String,
    /// 交易对中的计价资产符号（如 `USDC`）。
    pub quote: String,
}

impl TradingPair {
    pub fn binance_stream_key(&self) -> String {
        format!("{}{}", self.base, self.quote).to_lowercase()
    }

    pub fn binance_symbol(&self) -> String {
        format!("{}{}", self.base, self.quote).to_uppercase()
    }

    pub fn display_pair(&self) -> String {
        format!("{}/{}", self.base, self.quote)
    }

    pub fn deepbook_symbol(&self) -> String {
        format!("{}_{}", self.base.to_uppercase(), self.quote.to_uppercase())
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ArbitrageConfig {
    /// 套利触发所需的最小价差（基点）。
    pub min_spread_bps: f64,
    /// 策略允许持有的最大头寸数量（基础资产计）。
    pub max_position: f64,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct DeepbookConfig {
    /// DeepBook 指标服务的 HTTP Endpoint。
    pub endpoint: Option<String>,
    /// 查询订单簿时使用的 level 参数（深度精度）。
    pub level: Option<u32>,
    /// 查询订单簿时返回的档位数量。
    pub depth: Option<u32>,
    /// 轮询订单簿的间隔（毫秒）。
    pub poll_interval_ms: Option<u64>,
    /// DeepBook 下单使用的私钥所对应的环境变量名称。
    #[serde(default = "default_signer_env")]
    pub signer_env: String,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct CexConfig {
    /// Binance WebSocket 深度流的基础 URL。
    pub stream_base: Option<String>,
    /// Binance REST 深度快照接口的基础 URL。
    pub rest_base: Option<String>,
    /// API Key 对应的环境变量名称。
    #[serde(default = "default_binance_api_key_env")]
    pub api_key_env: String,
    /// API Secret 对应的环境变量名称。
    #[serde(default = "default_binance_api_secret_env")]
    pub api_secret_env: String,
    /// 请求使用测试网环境时设为 true。
    #[serde(default)]
    pub use_testnet: bool,
    /// 调用 REST 接口时的 recvWindow（毫秒）。
    #[serde(default)]
    pub recv_window_ms: Option<u64>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct LendingConfig {
    /// 在 Navi 进行抵押操作的币种符号。
    pub collateral_symbol: Option<String>,
    /// 借出的目标资产符号。
    pub borrow_symbol: Option<String>,
    /// 借款价值占抵押物价值的上限比例。
    pub max_borrow_ratio: Option<f64>,
    /// 达到该比例时触发归还借款的阈值。
    pub repay_threshold_ratio: Option<f64>,
    /// Navi 链上交互所需的详细参数。
    #[serde(default)]
    pub navi: Option<NaviLendingConfig>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct NaviLendingConfig {
    /// Sui RPC 节点地址。
    pub rpc_endpoint: Option<String>,
    /// 存放 Navi 签名私钥的环境变量名。
    #[serde(default = "default_navi_signer_env")]
    pub signer_env: String,
    /// Navi entry_deposit 所在的 package id。
    pub package_id: Option<String>,
    /// 调用的模块名称，默认 `incentive_v3`。
    #[serde(default = "default_navi_module")]
    pub module: String,
    /// 存入抵押物时调用的函数名称，默认 `entry_deposit`。
    #[serde(default = "default_navi_deposit_function", alias = "function")]
    pub deposit_function: String,
    /// 调用的提现函数名称，默认 `withdraw`。
    #[serde(default = "default_navi_withdraw_function")]
    pub withdraw_function: String,
    /// 调用的借款函数名称，默认 `borrow`。
    #[serde(default = "default_navi_borrow_function")]
    pub borrow_function: String,
    /// 调用的还款函数名称，默认 `entry_repay`。
    #[serde(default = "default_navi_repay_function")]
    pub repay_function: String,
    /// 质押资产的 Move 类型。
    #[serde(default = "default_navi_coin_type")]
    pub coin_type: String,
    /// 质押资产的小数位数，默认 6（USDC）。
    #[serde(default = "default_usdc_decimals")]
    pub coin_decimals: u8,
    /// Navi 合约定义的 asset id。
    #[serde(default)]
    pub asset_id: NaviAssetId,
    /// 借款资产的 Move 类型。
    #[serde(default = "default_navi_borrow_coin_type")]
    pub borrow_coin_type: String,
    /// 借款资产的小数位数，默认 9（WAL）。
    #[serde(default = "default_wal_decimals")]
    pub borrow_coin_decimals: u8,
    /// Navi 合约定义的借款资产 ID。
    #[serde(default = "default_navi_borrow_asset_id")]
    pub borrow_asset_id: NaviAssetId,
    /// Navi 提供的 UI Getter 包地址，若未配置则通过 OpenAPI 获取。
    #[serde(default)]
    pub ui_getter_package: Option<String>,
    /// Navi OpenAPI 基础地址。
    #[serde(default = "default_navi_api_base_url")]
    pub api_base_url: String,
    /// Navi OpenAPI 环境标识，例如 `prod`。
    #[serde(default = "default_navi_api_env")]
    pub api_env: String,
    /// Sui Clock 对象。
    #[serde(default)]
    pub clock: Option<SharedObjectConfig>,
    /// Navi Storage 对象。
    #[serde(default)]
    pub storage: Option<SharedObjectConfig>,
    /// Navi Pool 对象。
    #[serde(default)]
    pub pool: Option<SharedObjectConfig>,
    /// 抵押资产（例如 USDC）对应的 Pool 对象。
    #[serde(default)]
    pub collateral_pool: Option<SharedObjectConfig>,
    /// 借款资产（例如 WAL）对应的 Pool 对象。
    #[serde(default)]
    pub borrow_pool: Option<SharedObjectConfig>,
    /// 价格预言机对象。
    #[serde(default)]
    pub oracle: Option<SharedObjectConfig>,
    /// Incentive V2 对象。
    #[serde(default)]
    pub incentive_v2: Option<SharedObjectConfig>,
    /// Incentive V3 对象。
    #[serde(default)]
    pub incentive_v3: Option<SharedObjectConfig>,
    /// 交易 gas budget。
    #[serde(default = "default_navi_gas_budget")]
    pub gas_budget: u64,
}

fn default_navi_signer_env() -> String {
    "HEDGE_SIGNER_KEY".to_string()
}

fn default_navi_module() -> String {
    "incentive_v3".to_string()
}

fn default_navi_deposit_function() -> String {
    "entry_deposit".to_string()
}

fn default_navi_borrow_function() -> String {
    "borrow".to_string()
}

fn default_navi_repay_function() -> String {
    "entry_repay".to_string()
}

fn default_navi_withdraw_function() -> String {
    "withdraw".to_string()
}

fn default_navi_coin_type() -> String {
    "0xdba34672e30cb065b1f93e3ab55318768fd6fef66c15942c9f7cb846e2f900e7::usdc::USDC".to_string()
}

fn default_usdc_decimals() -> u8 {
    6
}

fn default_navi_borrow_coin_type() -> String {
    "0x356a26eb9e012a68958082340d4c4116e7f55615cf27affcff209cf0ae544f59::wal::WAL".to_string()
}

fn default_wal_decimals() -> u8 {
    9
}

fn default_navi_borrow_asset_id() -> NaviAssetId {
    NaviAssetId::Wal
}

fn default_navi_api_base_url() -> String {
    "https://open-api.naviprotocol.io".to_string()
}

fn default_navi_api_env() -> String {
    "prod".to_string()
}

fn default_navi_gas_budget() -> u64 {
    50_000_000
}

fn default_binance_api_key_env() -> String {
    "BINANCE_API_KEY".to_string()
}

fn default_binance_api_secret_env() -> String {
    "BINANCE_API_SECRET".to_string()
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ContractsConfig {
    /// Navi 主合约地址。
    pub navi_core: Option<String>,
    /// Navi 借贷市场合约地址。
    pub navi_market: Option<String>,
    /// Navi 价格预言机合约地址。
    pub navi_price_oracle: Option<String>,
    /// DeepBook 交易池合约地址。
    pub deepbook_pool: Option<String>,
    /// DeepBook 链上执行相关合约配置。
    #[serde(default)]
    pub deepbook_execution: Option<DeepbookExecutionContracts>,
    /// Navi 质押/借贷对象配置。
    #[serde(default)]
    pub lending_navi: Option<NaviLendingConfig>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct SharedObjectConfig {
    pub object_id: String,
    pub initial_shared_version: u64,
    #[serde(default)]
    pub mutable: bool,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct DeepbookExecutionContracts {
    pub rpc_endpoint: Option<String>,
    pub signer_key: Option<String>,
    pub gas_budget: Option<u64>,
    pub gas_price: Option<u64>,
    pub package_id: Option<String>,
    pub module: Option<String>,
    pub function: Option<String>,
    pub base_type: Option<String>,
    pub quote_type: Option<String>,
    #[serde(default)]
    pub shared_objects: Vec<SharedObjectConfig>,
    pub clock: Option<SharedObjectConfig>,
    pub price_scale: Option<u64>,
    pub quantity_scale: Option<u64>,
    pub restriction: Option<u8>,
    pub self_matching_prevention: Option<u8>,
    pub post_only: Option<bool>,
    pub client_order_id_start: Option<u64>,
}

impl ContractsConfig {
    pub fn merge(&mut self, other: ContractsConfig) {
        if let Some(value) = other.navi_core {
            self.navi_core = Some(value);
        }
        if let Some(value) = other.navi_market {
            self.navi_market = Some(value);
        }
        if let Some(value) = other.navi_price_oracle {
            self.navi_price_oracle = Some(value);
        }
        if let Some(value) = other.deepbook_pool {
            self.deepbook_pool = Some(value);
        }
        if let Some(value) = other.deepbook_execution {
            self.deepbook_execution = Some(value);
        }
        if let Some(value) = other.lending_navi {
            self.lending_navi = Some(value);
        }
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct StrategyConfig {
    /// 单笔挂单的最大数量（基础资产计）。
    pub max_order_size: Option<f64>,
    /// 触发策略时订单簿需要达到的最小深度。
    pub min_book_depth: Option<f64>,
    /// CEX 价格波动率的阈值（基点），超出后暂停操作。
    pub cex_volatility_bps: Option<f64>,
    /// CEX 价格变化低于该阈值时不重新挂单。
    pub rebid_threshold_bps: Option<f64>,
    /// Binance 现货提现触发阈值（基点）。可被环境变量 `SPOT_WITHDRAW_THRESHOLD_BPS` 覆盖。
    pub spot_withdraw_threshold_bps: Option<f64>,
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct ShutdownConfig {
    /// 程序退出时撤单操作的超时时间（毫秒）。
    pub cancel_timeout_ms: Option<u64>,
    /// 归还借款操作的超时时间（毫秒）。
    pub repay_timeout_ms: Option<u64>,
    /// 平掉对冲仓位操作的超时时间（毫秒）。
    pub close_hedge_timeout_ms: Option<u64>,
}

const DEFAULT_REBALANCE_MIN_NOTIONAL_USD: f64 = 50.0;
const DEFAULT_REBALANCE_COOLDOWN_SECS: u64 = 60;
const DEFAULT_BORROW_ALERT_RATIO: f64 = 0.6;
const DEFAULT_BORROW_TARGET_RATIO: f64 = 0.5;

#[derive(Debug, Deserialize, Clone)]
pub struct RebalanceConfig {
    #[serde(default)]
    pub min_notional_usd: Option<f64>,
    #[serde(default)]
    pub cooldown_secs: Option<u64>,
    #[serde(default)]
    pub deposit_address: Option<String>,
    #[serde(default)]
    pub deposit_network: Option<String>,
    #[serde(default)]
    pub deposit_memo: Option<String>,
    #[serde(default)]
    pub borrow_alert_ratio: Option<f64>,
    #[serde(default)]
    pub borrow_target_ratio: Option<f64>,
}

impl Default for RebalanceConfig {
    fn default() -> Self {
        Self::from_env()
    }
}

impl RebalanceConfig {
    pub fn from_env() -> Self {
        let _ = dotenv();
        Self {
            min_notional_usd: read_env_f64("REBALANCE_MIN_NOTIONAL_USD"),
            cooldown_secs: read_env_u64("REBALANCE_COOLDOWN_SECS"),
            deposit_address: read_env_string("CEX_DEPOSIT_ADDRESS"),
            deposit_network: read_env_string("CEX_DEPOSIT_NETWORK"),
            deposit_memo: read_env_string("CEX_DEPOSIT_MEMO"),
            borrow_alert_ratio: read_env_f64("LENDING_BORROW_RATIO_ALERT"),
            borrow_target_ratio: read_env_f64("LENDING_BORROW_RATIO_TARGET"),
        }
    }

    pub fn min_notional(&self) -> f64 {
        self.min_notional_usd
            .filter(|value| *value > 0.0)
            .unwrap_or(DEFAULT_REBALANCE_MIN_NOTIONAL_USD)
    }

    pub fn cooldown_secs(&self) -> u64 {
        self.cooldown_secs
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_REBALANCE_COOLDOWN_SECS)
    }

    pub fn borrow_alert_ratio(&self) -> f64 {
        self.borrow_alert_ratio
            .filter(|value| *value > 0.0)
            .unwrap_or(DEFAULT_BORROW_ALERT_RATIO)
    }

    pub fn borrow_target_ratio(&self) -> f64 {
        self.borrow_target_ratio
            .filter(|value| *value > 0.0)
            .unwrap_or(DEFAULT_BORROW_TARGET_RATIO)
    }
}

fn read_env_string(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn read_env_f64(name: &str) -> Option<f64> {
    read_env_string(name)?.parse::<f64>().ok()
}

fn read_env_u64(name: &str) -> Option<u64> {
    read_env_string(name)?.parse::<u64>().ok()
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        if let Some(path) = cli_config_path() {
            return Self::load_from_path(path);
        }
        if let Some(path) = env_config_path() {
            return Self::load_from_path(path);
        }
        Self::load_from_path(DEFAULT_CONFIG_PATH)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&contents)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        if let Some(overrides) = load_contracts_override(path)? {
            cfg.contracts.merge(overrides);
        }
        if cfg.contracts.lending_navi.is_some() {
            cfg.lending.navi = cfg.contracts.lending_navi.clone();
        }
        Ok(cfg)
    }
}

fn cli_config_path() -> Option<PathBuf> {
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        if arg == "--config" || arg == "-c" {
            if let Some(path) = args.next() {
                return Some(path.into());
            }
        } else if let Some(value) = arg.strip_prefix("--config=") {
            return Some(value.to_string().into());
        }
    }
    None
}

fn env_config_path() -> Option<PathBuf> {
    std::env::var("APP_CONFIG_PATH").ok().map(Into::into)
}

fn load_contracts_override(app_config_path: &Path) -> Result<Option<ContractsConfig>> {
    let contracts_path = app_config_path
        .parent()
        .map(|dir| dir.join("contracts.toml"))
        .unwrap_or_else(|| PathBuf::from("contracts.toml"));

    if !contracts_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&contracts_path)
        .with_context(|| format!("failed to read contracts file {}", contracts_path.display()))?;
    let overrides: ContractsConfig = toml::from_str(&contents).with_context(|| {
        format!(
            "failed to parse contracts file {}",
            contracts_path.display()
        )
    })?;
    Ok(Some(overrides))
}

fn default_signer_env() -> String {
    "HEDGE_SIGNER_KEY".to_string()
}

impl DeepbookConfig {
    pub fn signer_private_key(&self) -> Result<String> {
        let env_name = self.signer_env.trim();
        if env_name.is_empty() {
            anyhow::bail!("deepbook.signer_env 不能为空，请配置私钥环境变量名称");
        }
        std::env::var(env_name).with_context(|| {
            format!(
                "未找到 DeepBook 私钥环境变量 {}，请在 .env 或系统环境中设置",
                env_name
            )
        })
    }
}

use anyhow::{Context, Result};
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
    "DEEPBOOK_PRIVATE_KEY".to_string()
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

pub mod constants;
mod error;
mod onchain;

use anyhow::{Result, ensure};
use constants::{BORROW_SYMBOL, COLLATERAL_SYMBOL};
use error::LendingError;
use onchain::NaviOnchainClient;
use shared::config::{AppConfig, LendingConfig, NaviLendingConfig};
use std::sync::Arc;
use tokio::sync::{Mutex, OnceCell};
use tracing::{debug, info, warn};

/// Navi 借贷客户端占位实现。
///
/// 未来接入链上 RPC 时，补充 Sui/Navi SDK 调用逻辑。
#[derive(Clone)]
pub struct LendingClient {
    config: LendingConfig,
    collateral_symbol: String,
    borrow_symbol: Option<String>,
    onchain_config: Option<Arc<NaviLendingConfig>>,
    onchain_client: Arc<OnceCell<Option<Arc<NaviOnchainClient>>>>,
    state: Arc<Mutex<LendingState>>,
}

#[derive(Debug, Default)]
struct LendingState {
    collateral_value: f64,
    borrowed_value: f64,
}

pub use onchain::{NaviAssetBalance, NaviAssetIndex, NaviUserAssets};

impl LendingClient {
    /// 基于应用配置构造客户端。
    pub fn from_config(app_config: &AppConfig) -> Self {
        let lending = app_config.lending.clone();
        Self::new(lending)
    }

    /// 使用给定的借贷配置创建客户端。
    pub fn new(config: LendingConfig) -> Self {
        let collateral_symbol = config
            .collateral_symbol
            .clone()
            .unwrap_or_else(|| COLLATERAL_SYMBOL.to_string());
        let borrow_symbol = config
            .borrow_symbol
            .clone()
            .filter(|symbol| !symbol.trim().is_empty())
            .or_else(|| Some(BORROW_SYMBOL.to_string()));
        let onchain_config = config.navi.clone().map(Arc::new);

        Self {
            config,
            collateral_symbol,
            borrow_symbol,
            onchain_config,
            onchain_client: Arc::new(OnceCell::new()),
            state: Arc::new(Mutex::new(LendingState::default())),
        }
    }

    async fn onchain_client(&self) -> Result<Option<Arc<NaviOnchainClient>>> {
        let Some(cfg) = self.onchain_config.clone() else {
            return Ok(None);
        };

        let client = self
            .onchain_client
            .get_or_try_init(|| {
                let cfg = Arc::clone(&cfg);
                async move {
                    let client = NaviOnchainClient::connect(&cfg).await?;
                    Ok::<Option<Arc<NaviOnchainClient>>, anyhow::Error>(Some(Arc::new(client)))
                }
            })
            .await?;

        Ok(client.clone())
    }

    /// 暴露底层 Navi 链上客户端，供调试/测试使用。
    pub async fn navi_onchain_client(&self) -> Result<Option<Arc<NaviOnchainClient>>> {
        self.onchain_client().await
    }

    /// 质押抵押物（USDC）到 Navi。
    pub async fn deposit_collateral(&self, amount: f64) -> Result<()> {
        ensure_positive(amount)?;
        if let Some(client) = self.onchain_client().await? {
            client.deposit(amount).await?;
        }

        let total_after = {
            let mut state = self.state.lock().await;
            state.collateral_value += amount;
            state.collateral_value
        };
        info!(
            amount,
            collateral = %self.collateral_symbol,
            total_collateral = total_after,
            onchain = self.onchain_config.is_some(),
            "collateral deposit recorded"
        );
        Ok(())
    }

    /// 查询 Navi 链上记录的当前账号资产信息。
    pub async fn navi_user_assets(&self) -> Result<Option<NaviUserAssets>> {
        if let Some(client) = self.onchain_client().await? {
            let assets = client.fetch_self_assets().await?;
            return Ok(Some(assets));
        }
        Ok(None)
    }

    /// 查询指定资产 ID 的抵押/借款余额。
    pub async fn navi_user_balance(&self, asset_id: u8) -> Result<Option<NaviAssetBalance>> {
        if let Some(client) = self.onchain_client().await? {
            let balance = client.fetch_user_balance(asset_id).await?;
            return Ok(Some(balance));
        }
        Ok(None)
    }

    /// 借出目标代币（WAL），返回借款凭证 ID 或占位信息。
    pub async fn borrow_asset(&self, amount: f64) -> Result<BorrowReceipt> {
        ensure_positive(amount)?;
        self.borrow_asset_for(amount, self.borrow_symbol.as_deref())
            .await
    }

    /// 借出指定资产符号，未传入则使用默认配置。
    pub async fn borrow_asset_for(
        &self,
        amount: f64,
        asset_symbol: Option<&str>,
    ) -> Result<BorrowReceipt> {
        ensure_positive(amount)?;
        let (projected_ratio, max_ratio) = {
            let mut state = self.state.lock().await;
            state.borrowed_value += amount;
            let collateral = state.collateral_value;
            let ratio = if collateral <= f64::EPSILON {
                f64::INFINITY
            } else {
                state.borrowed_value / collateral
            };
            (
                ratio,
                self.config
                    .max_borrow_ratio
                    .unwrap_or(DEFAULT_MAX_BORROW_RATIO),
            )
        };
        if projected_ratio.is_infinite() || projected_ratio > max_ratio {
            warn!(
                projected_ratio,
                max_ratio,
                amount,
                asset = asset_symbol.unwrap_or("N/A"),
                "borrow amount pushes ratio above configured maximum"
            );
        }
        info!(
            amount,
            asset = asset_symbol.unwrap_or("N/A"),
            "borrow asset stub invoked"
        );
        Ok(BorrowReceipt::placeholder(amount, asset_symbol))
    }

    /// 归还借出的代币。
    pub async fn repay_asset(&self, amount: f64) -> Result<()> {
        ensure_positive(amount)?;
        let (repay_actual, remaining) = {
            let mut state = self.state.lock().await;
            if state.borrowed_value <= f64::EPSILON {
                (0.0, 0.0)
            } else {
                let repay = amount.min(state.borrowed_value);
                state.borrowed_value -= repay;
                (repay, state.borrowed_value)
            }
        };
        if repay_actual <= f64::EPSILON {
            warn!(
                amount,
                "repay request ignored because outstanding borrow is zero"
            );
            return Ok(());
        }
        info!(
            amount = repay_actual,
            remaining = remaining.max(0.0),
            asset = self.borrow_symbol.as_deref().unwrap_or("N/A"),
            "repay asset stub invoked"
        );
        Ok(())
    }

    /// 将借款比例降至目标值（占位实现）。
    pub async fn repay_to_target(&self, target_ratio: f64) -> Result<()> {
        ensure!(
            target_ratio >= 0.0,
            "target ratio must be non-negative, got {target_ratio}"
        );
        let max_ratio = self
            .config
            .max_borrow_ratio
            .unwrap_or(DEFAULT_MAX_BORROW_RATIO);
        let clamped_target = target_ratio.min(max_ratio).max(0.0);

        let (current_ratio, repay_amount) = {
            let state = self.state.lock().await;
            let collateral = state.collateral_value.max(0.0);
            if collateral <= f64::EPSILON {
                (0.0, 0.0)
            } else {
                let borrowed = state.borrowed_value.max(0.0);
                let ratio = borrowed / collateral;
                let desired_borrow = (collateral * clamped_target).min(borrowed);
                let payoff = (borrowed - desired_borrow).max(0.0);
                (ratio, payoff)
            }
        };

        if repay_amount <= f64::EPSILON {
            info!(
                current_ratio,
                target_ratio = clamped_target,
                "repay_to_target skipped; no adjustment required"
            );
            return Ok(());
        }

        info!(
            current_ratio,
            target_ratio = clamped_target,
            repay_amount,
            "repay_to_target executing repay"
        );
        self.repay_asset(repay_amount).await
    }

    /// 撤回多余抵押物（如未使用的 USDC）。
    pub async fn withdraw_collateral(&self, amount: f64) -> Result<()> {
        ensure_positive(amount)?;
        let total_after = {
            let mut state = self.state.lock().await;
            ensure!(
                amount <= state.collateral_value + f64::EPSILON,
                "withdraw amount {amount} exceeds collateral balance {}",
                state.collateral_value
            );
            state.collateral_value = (state.collateral_value - amount).max(0.0);
            state.collateral_value
        };
        info!(
            amount,
            collateral = %self.collateral_symbol,
            remaining_collateral = total_after,
            "withdraw collateral stub invoked"
        );
        Ok(())
    }

    /// 查询当前仓位健康度（借款/抵押比例）。
    pub async fn account_health(&self) -> Result<AccountHealth> {
        debug!("fetching account health (stub)");
        let state = self.state.lock().await;
        let collateral = state.collateral_value.max(0.0);
        let borrowed = state.borrowed_value.max(0.0);
        let borrow_ratio = if collateral <= f64::EPSILON {
            0.0
        } else {
            (borrowed / collateral).max(0.0)
        };
        Ok(AccountHealth {
            borrow_ratio,
            max_borrow_ratio: self
                .config
                .max_borrow_ratio
                .unwrap_or(DEFAULT_MAX_BORROW_RATIO),
            repay_threshold_ratio: self
                .config
                .repay_threshold_ratio
                .unwrap_or(DEFAULT_REPAY_THRESHOLD_RATIO),
            collateral_value: collateral,
            borrowed_value: borrowed,
        })
    }

    /// 根据上限与阈值决定是否可以继续借款。
    pub fn can_borrow_more(&self, health: &AccountHealth, planned_ratio: f64) -> bool {
        let max_ratio = health.max_borrow_ratio;
        let within_limit = planned_ratio <= max_ratio;
        if !within_limit {
            warn!(
                planned_ratio,
                max_ratio, "planned borrow ratio exceeds max limit"
            );
        }
        within_limit
    }

    /// 根据健康度判断是否需要归还借款。
    pub fn should_repay(&self, health: &AccountHealth) -> bool {
        health.borrow_ratio >= health.repay_threshold_ratio
    }
}

fn ensure_positive(amount: f64) -> Result<()> {
    if amount <= 0.0 {
        return Err(LendingError::InvalidAmount(amount).into());
    }
    Ok(())
}

const DEFAULT_MAX_BORROW_RATIO: f64 = 0.5;
const DEFAULT_REPAY_THRESHOLD_RATIO: f64 = 0.6;

/// 借款回执占位，未来可填充链上返回的对象 ID 等信息。
#[derive(Debug, Clone, PartialEq)]
pub struct BorrowReceipt {
    pub amount: f64,
    pub ticket_id: Option<String>,
    pub asset_symbol: Option<String>,
}

impl BorrowReceipt {
    pub fn placeholder(amount: f64, asset_symbol: Option<&str>) -> Self {
        Self {
            amount,
            ticket_id: None,
            asset_symbol: asset_symbol.map(|s| s.to_string()),
        }
    }
}

/// 账户健康度信息。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountHealth {
    pub borrow_ratio: f64,
    pub max_borrow_ratio: f64,
    pub repay_threshold_ratio: f64,
    pub collateral_value: f64,
    pub borrowed_value: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::config::AppConfig;
    fn sample_config() -> AppConfig {
        AppConfig {
            lending: LendingConfig {
                collateral_symbol: Some("USDC".into()),
                borrow_symbol: Some("WAL".into()),
                max_borrow_ratio: Some(0.5),
                repay_threshold_ratio: Some(0.6),
                navi: None,
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn deposit_requires_positive_amount() {
        let client = LendingClient::from_config(&sample_config());
        assert!(client.deposit_collateral(100.0).await.is_ok());
        assert!(client.deposit_collateral(0.0).await.is_err());
    }

    #[tokio::test]
    async fn borrow_and_repay_stubs_work() {
        let client = LendingClient::from_config(&sample_config());
        let receipt = client.borrow_asset(50.0).await.expect("borrow");
        assert_eq!(receipt.amount, 50.0);
        assert_eq!(receipt.asset_symbol.as_deref(), Some("WAL"));
        assert!(client.repay_asset(50.0).await.is_ok());
    }

    #[tokio::test]
    async fn borrow_asset_accepts_custom_symbol() {
        let client = LendingClient::from_config(&sample_config());
        let receipt = client
            .borrow_asset_for(25.0, Some("SAMPLE"))
            .await
            .expect("borrow custom");
        assert_eq!(receipt.asset_symbol.as_deref(), Some("SAMPLE"));
    }

    #[tokio::test]
    async fn account_health_defaults() {
        let mut cfg = sample_config();
        cfg.lending.max_borrow_ratio = None;
        cfg.lending.repay_threshold_ratio = None;

        let client = LendingClient::from_config(&cfg);
        client.deposit_collateral(100.0).await.unwrap();
        client.borrow_asset(40.0).await.unwrap();
        let health = client.account_health().await.expect("health");

        assert_eq!(health.max_borrow_ratio, DEFAULT_MAX_BORROW_RATIO);
        assert_eq!(health.repay_threshold_ratio, DEFAULT_REPAY_THRESHOLD_RATIO);
        assert_eq!(health.collateral_value, 100.0);
        assert_eq!(health.borrowed_value, 40.0);
        assert!((health.borrow_ratio - 0.4).abs() < 1e-6);
    }

    #[test]
    fn borrow_guard_flags_limit() {
        let client = LendingClient::new(LendingConfig {
            max_borrow_ratio: Some(0.5),
            ..Default::default()
        });
        let health = AccountHealth {
            borrow_ratio: 0.4,
            max_borrow_ratio: 0.5,
            repay_threshold_ratio: 0.6,
            collateral_value: 100.0,
            borrowed_value: 40.0,
        };
        assert!(client.can_borrow_more(&health, 0.45));
        assert!(!client.can_borrow_more(&health, 0.6));
    }

    #[test]
    fn repay_trigger_detects_threshold() {
        let client = LendingClient::new(LendingConfig {
            repay_threshold_ratio: Some(0.6),
            ..Default::default()
        });
        let health = AccountHealth {
            borrow_ratio: 0.59,
            max_borrow_ratio: 0.5,
            repay_threshold_ratio: 0.6,
            collateral_value: 100.0,
            borrowed_value: 59.0,
        };
        assert!(!client.should_repay(&health));

        let health = AccountHealth {
            borrow_ratio: 0.6,
            collateral_value: 100.0,
            borrowed_value: 60.0,
            ..health
        };
        assert!(client.should_repay(&health));
    }

    #[tokio::test]
    async fn repay_to_target_reduces_outstanding_borrow() {
        let client = LendingClient::from_config(&sample_config());
        client.deposit_collateral(200.0).await.unwrap();
        client.borrow_asset(120.0).await.unwrap();

        // Current ratio is 0.6, target is 0.4, expect repay to bring borrowed to 80.
        client.repay_to_target(0.4).await.unwrap();
        let health = client.account_health().await.unwrap();
        assert!((health.borrow_ratio - 0.4).abs() < 1e-6);
        assert!((health.borrowed_value - 80.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn repay_to_target_skips_when_already_below() {
        let client = LendingClient::from_config(&sample_config());
        client.deposit_collateral(5.0).await.unwrap();
        // client.borrow_asset(1.0).await.unwrap(); // ratio 0.2

        // client.repay_to_target(0.3).await.unwrap();
        // let health = client.account_health().await.unwrap();
        // assert!((health.borrow_ratio - 0.2).abs() < 1e-6);
        // assert!((health.borrowed_value - 30.0).abs() < 1e-6);
    }
}

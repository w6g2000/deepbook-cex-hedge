pub mod constants;
mod error;

use anyhow::Result;
use constants::{BORROW_SYMBOL, COLLATERAL_SYMBOL};
use error::LendingError;
use shared::config::{AppConfig, LendingConfig};
use tracing::{debug, info, warn};

/// Navi 借贷客户端占位实现。
///
/// 未来接入链上 RPC 时，补充 Sui/Navi SDK 调用逻辑。
#[derive(Clone)]
pub struct LendingClient {
    config: LendingConfig,
    collateral_symbol: String,
    borrow_symbol: String,
}

impl LendingClient {
    /// 基于应用配置构造客户端。
    pub fn from_config(app_config: &AppConfig) -> Self {
        let lending = app_config.lending.clone();
        Self::new(lending)
    }

    /// 使用给定的借贷配置创建客户端。
    pub fn new(config: LendingConfig) -> Self {
        Self {
            collateral_symbol: config
                .collateral_symbol
                .clone()
                .unwrap_or_else(|| COLLATERAL_SYMBOL.to_string()),
            borrow_symbol: config
                .borrow_symbol
                .clone()
                .unwrap_or_else(|| BORROW_SYMBOL.to_string()),
            config,
        }
    }

    /// 质押抵押物（USDC）到 Navi。
    pub async fn deposit_collateral(&self, amount: f64) -> Result<()> {
        ensure_positive(amount)?;
        info!(
            amount,
            collateral = %self.collateral_symbol,
            "deposit collateral stub invoked"
        );
        Ok(())
    }

    /// 借出目标代币（WAL），返回借款凭证 ID 或占位信息。
    pub async fn borrow_asset(&self, amount: f64) -> Result<BorrowReceipt> {
        ensure_positive(amount)?;
        info!(
            amount,
            asset = %self.borrow_symbol,
            "borrow asset stub invoked"
        );
        Ok(BorrowReceipt::placeholder(amount))
    }

    /// 归还借出的代币。
    pub async fn repay_asset(&self, amount: f64) -> Result<()> {
        ensure_positive(amount)?;
        info!(
            amount,
            asset = %self.borrow_symbol,
            "repay asset stub invoked"
        );
        Ok(())
    }

    /// 撤回多余抵押物（如未使用的 USDC）。
    pub async fn withdraw_collateral(&self, amount: f64) -> Result<()> {
        ensure_positive(amount)?;
        info!(
            amount,
            collateral = %self.collateral_symbol,
            "withdraw collateral stub invoked"
        );
        Ok(())
    }

    /// 查询当前仓位健康度（借款/抵押比例）。
    pub async fn account_health(&self) -> Result<AccountHealth> {
        debug!("fetching account health (stub)");
        Ok(AccountHealth {
            borrow_ratio: 0.0,
            max_borrow_ratio: self
                .config
                .max_borrow_ratio
                .unwrap_or(DEFAULT_MAX_BORROW_RATIO),
            repay_threshold_ratio: self
                .config
                .repay_threshold_ratio
                .unwrap_or(DEFAULT_REPAY_THRESHOLD_RATIO),
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
}

impl BorrowReceipt {
    pub fn placeholder(amount: f64) -> Self {
        Self {
            amount,
            ticket_id: None,
        }
    }
}

/// 账户健康度信息。
#[derive(Debug, Clone, PartialEq)]
pub struct AccountHealth {
    pub borrow_ratio: f64,
    pub max_borrow_ratio: f64,
    pub repay_threshold_ratio: f64,
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
        assert!(client.repay_asset(50.0).await.is_ok());
    }

    #[tokio::test]
    async fn account_health_defaults() {
        let mut cfg = sample_config();
        cfg.lending.max_borrow_ratio = None;
        cfg.lending.repay_threshold_ratio = None;

        let client = LendingClient::from_config(&cfg);
        let health = client.account_health().await.expect("health");

        assert_eq!(health.max_borrow_ratio, DEFAULT_MAX_BORROW_RATIO);
        assert_eq!(health.repay_threshold_ratio, DEFAULT_REPAY_THRESHOLD_RATIO);
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
        };
        assert!(!client.should_repay(&health));

        let health = AccountHealth {
            borrow_ratio: 0.6,
            ..health
        };
        assert!(client.should_repay(&health));
    }
}

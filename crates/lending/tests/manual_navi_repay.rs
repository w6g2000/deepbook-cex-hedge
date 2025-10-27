//! 手动运行的链上还款冒烟测试。
//!
//! 运行前准备：
//! 1. `.env` 中提供 `HEDGE_SIGNER_KEY`、Sui RPC 及 `config/app.toml` 内的 `[lending.navi]` 参数。
//! 2. 账号需持有待还款资产（默认 WAL）。
//!
//! 执行命令示例：
//! ```bash
//! cargo test -p lending --test manual_navi_repay -- --ignored --nocapture
//! ```

use anyhow::{Context, Result, anyhow, bail};
use lending::LendingClient;
use move_core_types::u256::U256;
use rust_decimal::prelude::ToPrimitive;
use shared::config::AppConfig;
use shared::types::NaviAssetId;
use std::sync::Once;
use tracing::info;
use tracing_subscriber::EnvFilter;

const OVERPAY_MULTIPLIER: f64 = 1.1;

fn init_tracing() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_target(false)
            .try_init();
    });
}

fn borrow_asset(config: &AppConfig) -> NaviAssetId {
    config
        .lending
        .navi
        .as_ref()
        .map(|cfg| cfg.borrow_asset_id)
        .unwrap_or(NaviAssetId::Wal)
}

fn borrow_decimals(config: &AppConfig) -> u8 {
    config
        .lending
        .navi
        .as_ref()
        .map(|cfg| cfg.borrow_coin_decimals)
        .unwrap_or(9)
}

#[tokio::test]
#[ignore = "需要真实链上配置与资金，默认跳过"]
async fn repay_borrow_onchain_smoke() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_config_path = manifest.join("../../config/app.toml");

    std::env::var("HEDGE_SIGNER_KEY").context("HEDGE_SIGNER_KEY 未设置，无法执行链上还款测试")?;

    let config = AppConfig::load_from_path(&app_config_path)
        .with_context(|| format!("加载应用配置 {:?} 失败", app_config_path))?;
    config
        .lending
        .navi
        .as_ref()
        .ok_or_else(|| anyhow!("config.lending.navi 未配置，无法执行链上还款"))?;

    if config.contracts.lending_navi.is_none() {
        bail!("contracts.lending_navi 未配置");
    }

    let client = LendingClient::from_config(&config);
    let asset = borrow_asset(&config);
    let decimals = borrow_decimals(&config);

    let before_balance = client
        .navi_user_balance(asset)
        .await?
        .ok_or_else(|| anyhow!("未查询到资产 {:?} 的借款余额", asset))?;
    if before_balance.borrowed_shares == U256::zero() {
        bail!("当前借款余额为 0，无法执行还款，请先借入目标资产");
    }

    let borrowed_amount = before_balance
        .borrowed_amount_decimal(decimals)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);
    if borrowed_amount <= f64::EPSILON {
        bail!("借款实数金额过小 ({borrowed_amount}), 无法测试还款");
    }

    let repay_amount = borrowed_amount * OVERPAY_MULTIPLIER;

    info!(
        asset = %asset,
        asset_id = asset.as_u8(),
        borrowed_shares = %before_balance.borrowed_shares,
        borrowed_amount,
        repay_amount,
        "还款前余额"
    );

    client
        .repay_asset(repay_amount)
        .await
        .context("调用链上还款失败")?;

    let after_balance = client
        .navi_user_balance(asset)
        .await?
        .ok_or_else(|| anyhow!("还款后无法查询到资产 {:?} 的余额", asset))?;

    info!(
        asset = %asset,
        asset_id = asset.as_u8(),
        borrowed_shares = %after_balance.borrowed_shares,
        "还款后余额"
    );

    if after_balance.borrowed_shares > before_balance.borrowed_shares {
        bail!(
            "还款后借款份额未降低：before={} after={}",
            before_balance.borrowed_shares,
            after_balance.borrowed_shares
        );
    }

    let before_amount = before_balance
        .borrowed_amount_decimal(decimals)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);
    let after_amount = after_balance
        .borrowed_amount_decimal(decimals)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);

    info!(
        asset = %asset,
        amount_before = before_amount,
        amount_after = after_amount,
        repay_amount,
        "还款测试完成"
    );

    Ok(())
}

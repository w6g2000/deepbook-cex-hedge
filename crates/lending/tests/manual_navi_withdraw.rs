//! 手动运行的链上提现冒烟测试。
//!
//! 运行前准备：
//! 1. `.env` 中提供 `HEDGE_SIGNER_KEY`、Sui RPC 及 `config/app.toml` 内的 `[lending.navi]` 参数。
//! 2. 账号需提前在 Navi 质押足够抵押品（默认 USDC）。
//!
//! 执行命令示例：
//! ```bash
//! cargo test -p lending --test manual_navi_withdraw -- --ignored --nocapture
//! ```

use anyhow::{Context, Result, anyhow, bail};
use lending::LendingClient;
use move_core_types::u256::U256;
use rust_decimal::prelude::ToPrimitive;
use shared::config::AppConfig;
use shared::types::NaviAssetId;
use std::sync::Once;
use std::time::Duration;
use tokio::time::sleep;
use tracing::info;
use tracing_subscriber::EnvFilter;
const RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRIES: usize = 10;

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

fn collateral_asset(config: &AppConfig) -> NaviAssetId {
    config
        .lending
        .navi
        .as_ref()
        .map(|cfg| cfg.asset_id)
        .unwrap_or(NaviAssetId::Usdc)
}

fn collateral_decimals(config: &AppConfig) -> u8 {
    config
        .lending
        .navi
        .as_ref()
        .map(|cfg| cfg.coin_decimals)
        .unwrap_or(6)
}

#[tokio::test]
#[ignore = "需要真实链上配置与资金，默认跳过"]
async fn withdraw_collateral_onchain_smoke() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_config_path = manifest.join("../../config/app.toml");

    std::env::var("HEDGE_SIGNER_KEY").context("HEDGE_SIGNER_KEY 未设置，无法执行链上提现测试")?;

    let config = AppConfig::load_from_path(&app_config_path)
        .with_context(|| format!("加载应用配置 {:?} 失败", app_config_path))?;
    config
        .lending
        .navi
        .as_ref()
        .ok_or_else(|| anyhow!("config.lending.navi 未配置，无法执行链上提现"))?;

    if config.contracts.lending_navi.is_none() {
        bail!("contracts.lending_navi 未配置");
    }

    let client = LendingClient::from_config(&config);
    let asset = collateral_asset(&config);
    let decimals = collateral_decimals(&config);

    let before_balance = client
        .navi_user_balance(asset)
        .await?
        .ok_or_else(|| anyhow!("未查询到资产 {:?} 的抵押余额", asset))?;
    if before_balance.supplied_shares == U256::zero() {
        bail!("当前抵押份额为 0，无法执行提现，请先质押对应资产");
    }

    let before_amount = before_balance
        .supplied_amount_decimal(decimals)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);
    let env_withdraw = std::env::var("NAVI_TEST_WITHDRAW")
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .filter(|value| *value > 0.0);
    let withdraw_amount = env_withdraw.unwrap_or(before_amount);

    info!(
        asset = %asset,
        asset_id = asset.as_u8(),
        supplied_shares = %before_balance.supplied_shares,
        supplied_amount = before_amount,
        withdraw_amount,
        "提现前余额"
    );

    client
        .withdraw_collateral(withdraw_amount)
        .await
        .context("调用链上提现失败")?;

    let mut observed_balance = None;
    let mut observed_amount = before_amount;

    for attempt in 0..=MAX_RETRIES {
        let balance = client
            .navi_user_balance(asset)
            .await?
            .ok_or_else(|| anyhow!("提现后无法查询到资产 {:?} 的余额", asset))?;

        let amount = balance
            .supplied_amount_decimal(decimals)
            .and_then(|d| d.to_f64())
            .unwrap_or(0.0);

        if balance.supplied_shares < before_balance.supplied_shares || attempt == MAX_RETRIES {
            observed_balance = Some(balance);
            observed_amount = amount;
            break;
        }

        info!(
            attempt = attempt + 1,
            max_attempts = MAX_RETRIES + 1,
            wait_secs = RETRY_DELAY.as_secs(),
            "提现结果尚未上链，等待确认"
        );
        sleep(RETRY_DELAY).await;
    }

    let after_balance = observed_balance.expect("withdraw balance must be set after retries");
    let after_amount = observed_amount;

    info!(
        asset = %asset,
        asset_id = asset.as_u8(),
        supplied_shares = %after_balance.supplied_shares,
        supplied_amount = after_amount,
        withdraw_amount,
        "提现后余额"
    );

    if after_balance.supplied_shares >= before_balance.supplied_shares {
        bail!(
            "提现后抵押份额未减少：before={} after={}",
            before_balance.supplied_shares,
            after_balance.supplied_shares
        );
    }

    let delta = before_amount - after_amount;
    if delta <= f64::EPSILON {
        bail!(
            "提现后抵押余额未降低：before={} after={} delta={}",
            before_amount,
            after_amount,
            delta
        );
    }

    info!(
        asset = %asset,
        amount_before = before_amount,
        amount_after = after_amount,
        withdraw_amount,
        observed_delta = delta,
        "提现测试完成"
    );

    Ok(())
}

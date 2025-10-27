//! 手动运行的链上借款冒烟测试。
//!
//! 运行前准备：
//! 1. `.env` 中提供 `HEDGE_SIGNER_KEY`、Sui RPC 及 `config/app.toml` 内的 `[lending.navi]` 参数。
//! 2. 账号需提前质押足够抵押品（默认 USDC）。
//!
//! 执行命令示例：
//! ```bash
//! cargo test -p lending --test manual_navi_borrow -- --ignored --nocapture
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

const BORROW_AMOUNT: f64 = 1.0;
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
async fn borrow_onchain_smoke() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_config_path = manifest.join("../../config/app.toml");

    std::env::var("HEDGE_SIGNER_KEY").context("HEDGE_SIGNER_KEY 未设置，无法执行链上借款测试")?;

    let config = AppConfig::load_from_path(&app_config_path)
        .with_context(|| format!("加载应用配置 {:?} 失败", app_config_path))?;
    config
        .lending
        .navi
        .as_ref()
        .ok_or_else(|| anyhow!("config.lending.navi 未配置，无法执行链上借款"))?;

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

    let before_amount = before_balance
        .borrowed_amount_decimal(decimals)
        .and_then(|d| d.to_f64())
        .unwrap_or(0.0);

    info!(
        asset = %asset,
        asset_id = asset.as_u8(),
        borrowed_shares = %before_balance.borrowed_shares,
        borrowed_amount = before_amount,
        "借款前余额"
    );

    if before_balance.borrowed_shares == U256::zero() {
        info!("当前借款份额为 0，将直接借入测试金额 {}", BORROW_AMOUNT);
    }

    let receipt = client
        .borrow_asset(BORROW_AMOUNT)
        .await
        .context("调用链上借款失败")?;

    let tx_digest = receipt
        .tx_digest
        .as_deref()
        .ok_or_else(|| anyhow!("借款未返回链上交易哈希，请确认已配置 Navi 客户端"))?;

    info!(
        asset = receipt.asset_symbol.as_deref().unwrap_or("unknown"),
        amount = receipt.amount,
        tx_digest,
        "提交借款交易"
    );

    let mut observed_balance = None;
    let mut observed_amount = 0.0;

    for attempt in 0..=MAX_RETRIES {
        let balance = client
            .navi_user_balance(asset)
            .await?
            .ok_or_else(|| anyhow!("借款后无法查询到资产 {:?} 的余额", asset))?;

        let amount = balance
            .borrowed_amount_decimal(decimals)
            .and_then(|d| d.to_f64())
            .unwrap_or(0.0);

        if balance.borrowed_shares > before_balance.borrowed_shares || attempt == MAX_RETRIES {
            observed_balance = Some(balance);
            observed_amount = amount;
            break;
        }

        info!(
            attempt = attempt + 1,
            max_attempts = MAX_RETRIES + 1,
            wait_secs = RETRY_DELAY.as_secs(),
            "借款结果尚未上链，等待确认"
        );
        sleep(RETRY_DELAY).await;
    }

    let after_balance = observed_balance.expect("borrow balance must be set after retries");
    let after_amount = observed_amount;

    info!(
        asset = %asset,
        asset_id = asset.as_u8(),
        borrowed_shares = %after_balance.borrowed_shares,
        borrowed_amount = after_amount,
        tx_digest,
        "借款后余额"
    );

    let delta = after_amount - before_amount;

    if after_balance.borrowed_shares <= before_balance.borrowed_shares && delta <= f64::EPSILON {
        bail!(
            "借款等待超时：before_shares={} after_shares={} before_amount={} after_amount={}",
            before_balance.borrowed_shares,
            after_balance.borrowed_shares,
            before_amount,
            after_amount
        );
    }

    if after_balance.borrowed_shares <= before_balance.borrowed_shares {
        bail!(
            "借款后借款份额未增加：before={} after={}",
            before_balance.borrowed_shares,
            after_balance.borrowed_shares
        );
    }

    if delta <= f64::EPSILON {
        bail!(
            "借款后金额未增加：before={} after={} delta={}",
            before_amount,
            after_amount,
            delta
        );
    }

    info!(
        asset = %asset,
        amount_before = before_amount,
        amount_after = after_amount,
        borrowed_delta = delta,
        requested = BORROW_AMOUNT,
        "借款测试完成"
    );

    Ok(())
}

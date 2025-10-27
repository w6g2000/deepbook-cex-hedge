//! 手动运行的链上质押冒烟测试。
//!
//! 运行前准备：
//! 1. `.env` 中提供 `HEDGE_SIGNER_KEY`、Sui RPC 及 `config/app.toml` 内的 `[lending.navi]` 参数。
//! 2. 账号需持有足够 SUI 作为 gas 以及待质押的 USDC。
//!
//! 执行命令示例：
//! ```bash
//! cargo test -p lending --test manual_navi_deposit -- --ignored --nocapture
//! ```

use anyhow::{Context, Result, anyhow, bail};
use lending::LendingClient;
use shared::config::AppConfig;
use shared::types::NaviAssetId;
use std::sync::Once;
use tracing::info;
use tracing_subscriber::EnvFilter;

const DEFAULT_DEPOSIT_AMOUNT: f64 = 0.000001;

fn tracked_assets(config: &AppConfig) -> Vec<NaviAssetId> {
    let mut assets = vec![NaviAssetId::Sui, NaviAssetId::Wal];
    if let Some(navi) = &config.lending.navi {
        for asset in [navi.asset_id, navi.borrow_asset_id] {
            if !assets.contains(&asset) {
                assets.push(asset);
            }
        }
    }
    assets
}

fn asset_decimals(config: &AppConfig, asset_id: NaviAssetId) -> u8 {
    match asset_id {
        NaviAssetId::Sui => 9,
        NaviAssetId::Usdc => config
            .lending
            .navi
            .as_ref()
            .map(|cfg| cfg.coin_decimals)
            .unwrap_or(6),
        NaviAssetId::Wal => config
            .lending
            .navi
            .as_ref()
            .map(|cfg| cfg.borrow_coin_decimals)
            .unwrap_or(9),
    }
}

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

#[tokio::test]
#[ignore = "需要真实链上配置与资金，默认跳过"]
async fn deposit_collateral_onchain_smoke() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_config_path = manifest.join("../../config/app.toml");

    // 确认关键环境变量已配置。
    std::env::var("HEDGE_SIGNER_KEY").context("HEDGE_SIGNER_KEY 未设置，无法执行链上质押测试")?;

    let amount = std::env::var("NAVI_TEST_DEPOSIT")
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(DEFAULT_DEPOSIT_AMOUNT);
    if amount <= 0.0 {
        bail!("NAVI_TEST_DEPOSIT 必须大于 0，当前值 {amount}");
    }

    let config = AppConfig::load_from_path(&app_config_path)
        .with_context(|| format!("加载应用配置 {:?} 失败", app_config_path))?;
    if config.contracts.lending_navi.is_none() {
        bail!("contracts.lending_navi 未配置");
    }
    if config.lending.navi.is_none() {
        bail!("config.lending.navi 未配置，无法执行链上质押");
    }

    let client = LendingClient::from_config(&config);
    client
        .deposit_collateral(amount)
        .await
        .context("调用链上质押失败")?;

    let health = client.account_health().await?;
    assert!(
        health.collateral_value >= amount - 1e-9,
        "质押后抵押仓位未更新：{}",
        health.collateral_value
    );

    let tracked = tracked_assets(&config);
    if let Some(user_assets) = client.navi_user_assets().await? {
        info!(
            asset_count = user_assets.asset_ids.len(),
            entry_count = user_assets.entries.len(),
            raw_blob_hex = %hex::encode(&user_assets.raw_values),
            "Navi user assets summary"
        );
        for (asset_id, entry) in user_assets
            .asset_ids
            .iter()
            .copied()
            .zip(user_assets.entries.iter())
        {
            info!(
                asset_id,
                raw_hex = %hex::encode(entry),
                "获取 Navi 账户资产成功"
            );
        }
    }

    for asset_id in tracked {
        let asset_numeric = asset_id.as_u8();
        if let Some(balance) = client.navi_user_balance(asset_id).await? {
            let decimals = asset_decimals(&config, asset_id);
            let supplied_shares = balance
                .supplied_shares_decimal(decimals)
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            let borrowed_shares = balance
                .borrowed_shares_decimal(decimals)
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            let supplied_real = balance
                .supplied_amount_decimal(decimals)
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            let borrowed_real = balance
                .borrowed_amount_decimal(decimals)
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            let borrow_index = balance
                .index
                .borrow_index_decimal()
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            let supply_index = balance
                .index
                .supply_index_decimal()
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            info!(
                asset = %asset_id,
                asset_id = asset_numeric,
                decimals,
                supplied_shares_raw = %balance.supplied_shares,
                borrowed_shares_raw = %balance.borrowed_shares,
                supplied_shares = %supplied_shares,
                borrowed_shares = %borrowed_shares,
                supply_index_raw = %balance.index.supply_index,
                borrow_index_raw = %balance.index.borrow_index,
                supply_index = %supply_index,
                borrow_index = %borrow_index,
                supplied = %supplied_real,
                borrowed = %borrowed_real,
                "ui_getter::get_user_state 返回数据"
            );
        } else {
            info!(asset = %asset_id, asset_id = asset_numeric, "ui_getter::get_user_state 未返回记录");
        }
    }

    Ok(())
}

#[tokio::test]
#[ignore = "需要真实链上配置与资金，默认跳过"]
async fn fetch_user_assets_devinspect() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_config_path = manifest.join("../../config/app.toml");

    std::env::var("HEDGE_SIGNER_KEY").context("HEDGE_SIGNER_KEY 未设置，无法执行链上查询测试")?;

    let config = AppConfig::load_from_path(&app_config_path)
        .with_context(|| format!("加载应用配置 {:?} 失败", app_config_path))?;
    if config.contracts.lending_navi.is_none() {
        bail!("contracts.lending_navi 未配置");
    }

    let client = LendingClient::from_config(&config);
    if let Some(assets) = client
        .navi_user_assets()
        .await
        .context("查询 Navi 用户资产失败")?
    {
        info!(
            asset_count = assets.asset_ids.len(),
            entry_count = assets.entries.len(),
            raw_blob_hex = %hex::encode(&assets.raw_values),
            "Navi user assets summary"
        );
        for (asset_id, entry) in assets.asset_ids.iter().copied().zip(assets.entries.iter()) {
            info!(asset_id, raw_hex = %hex::encode(entry), "storage::get_user_assets 返回条目");
        }
    }

    for asset_id in tracked_assets(&config) {
        let asset_numeric = asset_id.as_u8();
        if let Some(balance) = client.navi_user_balance(asset_id).await? {
            let decimals = asset_decimals(&config, asset_id);
            let supplied_real = balance
                .supplied_amount_decimal(decimals)
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            let borrowed_real = balance
                .borrowed_amount_decimal(decimals)
                .map(|d| d.normalize().to_string())
                .unwrap_or_else(|| "N/A".to_string());
            info!(
                asset = %asset_id,
                asset_id = asset_numeric,
                decimals,
                supplied_shares_raw = %balance.supplied_shares,
                borrowed_shares_raw = %balance.borrowed_shares,
                supply_index_raw = %balance.index.supply_index,
                borrow_index_raw = %balance.index.borrow_index,
                supplied = %supplied_real,
                borrowed = %borrowed_real,
                "ui_getter::get_user_state 返回数据"
            );
        } else {
            info!(asset = %asset_id, asset_id = asset_numeric, "ui_getter::get_user_state 未返回记录");
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "调试用：打印 storage 模块函数签名"]
async fn inspect_storage_module() -> Result<()> {
    dotenvy::dotenv().ok();
    init_tracing();

    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let app_config_path = manifest.join("../../config/app.toml");

    let config = AppConfig::load_from_path(&app_config_path)
        .with_context(|| format!("加载应用配置 {:?} 失败", app_config_path))?;
    let client = LendingClient::from_config(&config);
    let onchain_client = client
        .navi_onchain_client()
        .await?
        .ok_or_else(|| anyhow!("navi onchain client not configured"))?;

    let modules = onchain_client
        .client()
        .read_api()
        .get_normalized_move_modules_by_package(onchain_client.package())
        .await
        .context("获取 storage 模块失败")?;
    let module = modules
        .get("storage")
        .cloned()
        .ok_or_else(|| anyhow!("storage 模块信息不存在"))?;

    if let Some(func) = module.exposed_functions.get("get_pools") {
        info!(return_type = ?func.return_, params = ?func.parameters, visibility = ?func.visibility, "storage::get_pools");
    }
    if let Some(func) = module.exposed_functions.get("get_user_balance") {
        info!(return_type = ?func.return_, params = ?func.parameters, visibility = ?func.visibility, "storage::get_user_balance");
    }
    if let Some(func) = module.exposed_functions.get("get_index") {
        info!(return_type = ?func.return_, params = ?func.parameters, visibility = ?func.visibility, "storage::get_index");
    }

    let ui_modules = onchain_client
        .client()
        .read_api()
        .get_normalized_move_modules_by_package(onchain_client.ui_getter_package())
        .await
        .context("获取 ui_getter 模块失败")?;
    if let Some(ui_module) = ui_modules.get("getter_unchecked") {
        if let Some(func) = ui_module.exposed_functions.get("get_user_state") {
            info!(return_type = ?func.return_, params = ?func.parameters, visibility = ?func.visibility, "ui_getter::get_user_state");
        }
    }

    Ok(())
}

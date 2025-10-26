deepbook-cex套利（现阶段使用 Binance 现货做对冲，永续接口尚未启用）

## 构建与测试

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 运行

启动前请确保：

- Binance 现货账户已充值足够的对冲资金（直接向交易所提供的充值地址转入，程序不会主动触发充值）。
- `.env` 中配置了 Binance API Key/Secret 以及私有充值地址等敏感信息。
- `config/app.toml` 已根据实际策略调参。

```bash
cargo run -p deepbook-cex-app -- --config config/app.toml
```

## 关机

按 `Ctrl+C` 触发优雅关机，步骤包括暂停策略、撤销 DeepBook 挂单、偿还借贷与平掉 CEX 对冲仓位。

## 现货资金管理说明

- 仅对接 Binance 现货下单、撤单与仓位查询；永续执行器目前维持 stub 状态。
- 提现功能计划加入 `CexExecutor::withdraw_spot`，待对齐阈值后再实现；当前版本只记录阈值比较结果，自动提现尚未启用。
- 充值流程仍需人工将资产汇入交易所现货账户，对应地址见 `.env` 配置示例。
- 可通过环境变量 `SPOT_WITHDRAW_THRESHOLD_BPS`（写入 `.env`）随时调整现货提现触发阈值，留空则禁用提现逻辑。

deepbook-cex套利

## 构建与测试

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## 运行

```bash
cargo run -p deepbook-cex-app -- --config config/app.toml
```

## 关机

按 `Ctrl+C` 触发优雅关机，步骤包括暂停策略、撤销 DeepBook 挂单、偿还借贷与平掉 CEX 对冲仓位。

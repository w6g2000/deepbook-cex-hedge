# Deepbook-CEX Hedge 精细化迭代清单

## 1. 核心对接
- 在 `crates/deepbook-execution/src/lib.rs` 接入真实 DeepBook RPC，下单获取链上 ID，并完善撤单与 claim 资金流程。
- 完成 `crates/cex-execution/src/lib.rs` 的 Binance 永续实盘执行：签名鉴权、限速、滑点/风控、仓位同步。
- `crates/lending/src/lib.rs` 串联 Navi 借贷 SDK，实时查询健康度，落实 40%–50% 借款区间的自动调节。

## 2. 策略与风控
- 强化 `crates/strategy/src/lib.rs` 中 `evaluate_pair` 与梯度管理，结合库存、借贷状态、DeepBook 成交通道执行，完善撤单重试。
- 深化 `rebalance_lending` 动态调仓逻辑：根据健康度和在途订单计算借/还规模，记录风控日志。
- 在 `crates/strategy/src/risk.rs` 提供可插拔告警（Webhook/Slack/持久化），并增加告警节流策略。

## 3. 行情抓取
- 加固 `crates/cex-watcher/src/lib.rs`：实现 Binance 增量深度校验、序列号对齐、重连退避与监控指标。
- 强化 `crates/deepbook-watcher/src/lib.rs`：自适应退避、接口 schema 校验、缓存复用，并预留 SSE/WebSocket 备选。

## 4. 执行与应用服务
- 在 `crates/execution/src/lib.rs` 为订单路由加入优先级、去重、错误分类与指标上报，提供线程安全的仓位快照。
- 将 `crates/app/src/health.rs` 升级为标准 HTTP 服务（如 axum），支持鉴权/TLS，并输出更丰富的运行指标。
- `crates/app/src/shutdown.rs` 支持自定义输出路径、IO 失败兜底，记录每个步骤的结果和耗时。

## 5. 配置与工具链
- `crates/shared/src/config.rs` 增加配置验证（阈值范围、URL 校验、环境变量覆盖）并补充文档。
- 扩充 `config/app.toml` 示例：多交易对、API 凭证占位、生产/测试环境调优提示。
- 搭建工作区 CI：统一执行 fmt、clippy、tests，加入 Binance/DeepBook 回放脚本和 `shutdown_summary.json` 归档。

## 6. 测试覆盖
- 扩展策略单测：覆盖价格剧烈波动、深度不足、借贷再平衡、多层挂单更新等场景。
- 增加集成测试：验证 CEX/DeepBook watcher、执行引擎、健康探针；真实交易所联调使用 `#[ignore]` 隔离。
- 补齐 DeepBook 执行单测：部分成交、超时 claim、重复 claim 的幂等性。


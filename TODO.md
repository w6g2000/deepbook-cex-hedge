# Deepbook-CEX Hedge TODO

## Phase 1 – Infrastructure Prep
- [x] Add `config/app.toml` sample with Navi合约常量、交易对、阈值（借款50%、清算60%、价差/深度/波动阈值）
- [x] 扩展 `shared::config` 读取 CLI flag / ENV（`--config` or `APP_CONFIG_PATH`）
- [x] 定义全局常量文件 `crates/lending/src/constants.rs` 填入 Navi 合约地址占位

## Phase 2 – Lending (Navi) 模块
- [x] 新建 `crates/lending`：暴露 `LendingClient`
- [x] 实现 `deposit_usdc`, `borrow_asset`, `repay_asset`, `withdraw_collateral` 的 stub
- [x] 实现 `fetch_account_health`（返回抵押率，供风控判定50%/60%）
- [x] 写单元测试（mock RPC）验证参数、借款上限计算

## Phase 3 – Deepbook 订单管理
- [x] 新建 `crates/deepbook-execution`（内存 backend 已提供接口，链上实现可按需替换）
- [x] 提供 `place_post_only_order`, `cancel_order`, `cancel_all`, `claim_fills` 接口（现由 trait 抽象，可挂真实 RPC）
- [x] 维护本地订单状态（order_id → 状态），支持超时自动撤单（默认 in-memory backend）
- [x] 把已成交但未 claim 的 token / USDC 统计出来供资金管理使用（claim 汇总由 backend 暴露）

## Phase 4 – Strategy & Hedging
- [x] 在 `crates/strategy` 实现主策略循环：监听 CEX/Deepbook 行情
- [x] 根据配置计算价差、深度、波动率，决定是否挂上针/下针
- [x] 调 `LendingClient` 动态调整借款（保持 ≤50%，超过60%触发 repay）
- [x] 调 `DeepbookExecution` 生成/撤销挂单，调用 `claim_fills`
- [x] 成交后向执行模块发送 hedge 指令（包含方向、数量、目标价格）
- [x] 挂单前需确认 Deepbook 与 CEX 中心价差不超过配置阈值（防止波动阶段“接飞刀”），否则暂缓/撤单
- [x] 融入资金配比流程：启动时校验 CEX/链上资金比例，不符则执行再平衡（CEX↔链上转账）后再开始策略
- [x] 订阅 CEX 价格为基准，生成阶梯式 Deepbook 挂单（顶部卖/底部买），借贷/挂单都依据 CEX 当前价
- [x] 为每档挂单记录 CEX 基准价，价差超阈值（ `rebid_threshold_bps` ）时自动撤单并重新挂单，防止频繁刷新
- [x] 定期监控借贷健康度，超过 `repay_threshold_ratio` 时触发部分还款，维持抵押占用在安全范围
- [x] Deepbook 成交后：轮询/监听确认 → claim 代币与剩余 USDC → 重新平衡 CEX/链上资金 → 下发下一轮挂单任务

## Phase 5 – CEX 执行与仓位管理
- [x] 新建 `CexExecutor` trait + Binance 永续实现（下单/撤单/查询仓位）
- [x] 建 `PositionManager` 追踪 CEX 对冲仓位、PnL、剩余头寸
- [x] 实现“链上先出再平仓”流程：策略发出回补信号 → Deepbook 平仓 → 成功后平掉 CEX
- [x] 处理部分成交/滑点，必要时调整仓位

## Phase 6 – 风控 & 报警
- [x] 实现价格突变报警：监控 CEX 秒级涨跌幅，超过阈值撤空 Deepbook 挂单、暂停策略
- [x] 实现深度不足报警：当所需数量对应深度 < 阈值时禁用新挂单
- [x] 监控借贷健康度、CEX 爆仓价、未平仓数量，超过阈值触发 `RiskAction`
- [x] 集成通知通道（占位：日志/HTTP 等）

## Phase 7 – 运行时与退出流程
- [x] 在 `app` 中加入 `ShutdownCoordinator`，Ctrl+C 时依次执行：暂停策略 → 撤单 → repay → 平仓
- [x] 增加健康检查 / metrics HTTP 接口（最近拉取时间、借贷比例、未平仓数量）
- [x] 记录关键信息到持久化（可选：JSON/SQLite），方便崩溃恢复

## Phase 8 – 测试与文档
- [x] 单元测试：策略触发逻辑、借贷控制、风控策略
- [x] 集成测试：使用 mock CEX/Deepbook 数据模拟完整流程（借贷→挂单→成交→对冲→回补）
- [ ] `#[ignore]` 实测：Navi 借贷、Deepbook 下单、Binance 永续下单
- [x] 完善 README，描述运行流程、配置示例、风险提示

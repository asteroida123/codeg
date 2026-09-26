# 自用集成线路线图（selfhost/main）

> 状态：Active（2026-09-26 建立，同日按 Discussion #731 修订）
> 性质：个人自用集成分支，先完整验证再拆小 PR 回上游（与 Discussion #731 的边界一致）
> 基座：`codex/delegation-continuation-local`（PR #693 同会话续作）+ ZCode 内置（PR #768）+ upstream main 0.32.2
>
> 规划来源（以 Discussion #731 为主线，其余为支撑）：
> - [Discussion #731](https://github.com/xintaofei/codeg/discussions/731) — **主线路**：委托编排、配置选择与评测闭环
> - `docs/codeg-continuable-delegation-session-design.md` — CollaborationSession 完整设计（形态参考）
> - `docs/STRATEGY-MEMO.md` — 战略备忘（PK/角色团队为最末期产品功能）
> - Issue #603（续作跟踪）、Issue #724（委托结果持久化 + 子 Agent 看板）、PR #693 范围映射

## 主线（Discussion #731 的闭环）

```text
发现 Agent 能力
→ 选择 Agent、Model 和 Reasoning / Mode
→ 执行 delegation 或 WorkTask
→ 审查、继续和返工
→ 记录真实运行结果和验证结果
→ 形成评测数据
→ （远期）为 Agent / Model / Reasoning 推荐和受控自动路由提供依据
```

边界：复用现有 WorkTask Engine、Delegation Broker、ACP 和 Conversation，不新建独立 Runtime；暂不实现完全自动的 Agent/Model 路由。

## 已就位

- delegation_task 账本：准入、ResumeBinding、冻结终态、每源单后继、同请求幂等
- 严格恢复：live reuse → resume → load → 明确失败，不静默冷启动；agent/session/cwd 身份硬约束
- 启动对账：中断任务投影为 unknown/interrupted，不盲发
- WorkTask 引擎：CAS 状态机、事件流、模板、scheduled_at、worktree/merge 集成；Agent 侧现有 `task_progress` / `task_complete` 上报工具
- ZCode 内置 agent（adapter 0.1.4，协议已对 zai-org/zcode 校准）
- upstream 0.32.2：agent-client-protocol 2.2 迁移、ACP Session Notices/Compaction

## 推进顺序

### 第 0 步：ZCode × 委托链路实测（先于一切功能开发）

合并 ≠ 可用。ZCode 进入委托线后需要实测三条链路：

| 链路 | 预期 | 风险 |
| --- | --- | --- |
| ZCode 作为子 agent 被委托 | `delegate_to_agent` → 首轮 TurnComplete → 账本落库 | 低（不依赖 ZCode 的 MCP） |
| ZCode 子会话续作 | `continue_from_task_id` → 严格 resume 原 session | 中：adapter 的 session/resume 必须满足身份硬约束 |
| ZCode 作为父 agent 委托他人 | 父会话挂 codeg-mcp → delegate_to_agent | **已知边界**：ZCode 0.16.5 后端 create 不 wire mcpServers（adapter 已前向兼容，等 ZCode 后端修复） |

### 第一阶段（#731 明确的第一阶段边界：能力发现 + 显式选择 + 可靠执行 + 数据积累）

1. **能力发现 + 显式选择**（#731 问题 1）
   - 向主 Agent 暴露子 Agent 支持的 Model / Reasoning / Mode 清单（get_session_info 或新工具）
   - 委托时按 call 选 model/mode（与上游 PR #505 / #616 同向，但自用线不受上游节奏限制）
   - ZCode 注意：模型目录在 `~/.zcode/v2/config.json` provider 表，会话快照只带当前模型（协议校准结论）
2. **审查返工入口**：@Session 找回子会话（#693 未覆盖 + 设计文档 §14.4 一等入口）
   - 父会话 @ 面板「本次对话的子智能体」分组（运行中 / 可续 / 需恢复 / 已关闭）
   - 当前搜索默认排除 delegation children，需要打开受控入口
3. **数据积累 + 看板**（#731 问题 4 / Issue #724）
   - 每次 delegation 落库：agent、model、reasoning、token、耗时、验证结果、返工轮数、最终接受与否
   - delegation_task 账本已有骨架，缺 per-turn 指标列与聚合视图
   - 这是评测闭环的数据地基；有了它，后续推荐/路由才有依据

### 第二阶段：WorkTask × Delegation 协同（#731 问题 2 + 3）

- **Agent 创建与拆分 WorkTask**：主 Agent 把大任务拆成多个持久化子任务，用依赖、并发和预算限制编排
  - 现状缺口：Agent 侧只有 `task_progress` / `task_complete` 上报，没有创建/拆分/编排工具；引擎本身（状态机/事件/模板）已具备
- **WorkTask ↔ Delegation Session 稳定关联**：审查和返工续接原来的子 Agent 会话（#693 的 `continue_from_task_id`），不再冷会话重派
  - 落点：work_task 执行与 delegation_task 账本之间建立外键级关联，返工轮走续作通道

### 探索线（与第一/二阶段并行）：Jev 类判断模型做任务路由

为「自行委托」（上游 PR #478 / #480，无 @ 自动派发子智能体）提供路由决策。不用父 LLM 烧 token 纠结选谁，也不用硬编码规则：

- **机制**：TypeSafe System One 模型（Jev）的 **Choice** 原语——从第一阶段产出的能力矩阵（agent × model × mode）中选目标，返回概率分布 + 置信度；state 携带任务简述、工作目录上下文、（积累后的）历史表现数据
- **受控性**：置信度门控——高置信自动路由，低置信回落给父智能体/用户决定（对应 #731「受控自动路由」而非「完全自动路由」，边界不变）
- **与评测闭环复合**：#724 积累的委托结果数据作为路由 state / 复合评分特征反哺（composite scoring 模式），语义判断（Jev）+ 真实表现（数据）双通道
- **落点**：broker/spawner 派发路径上一个可插拔的 router 模块（Rust 侧走 TypeSafe HTTP API），`delegate_to_agent` 未显式指定目标时启用

**Spike 结论（2026-09-26，`spikes/jev-routing/`，24 任务 × 6 agent，29 次调用）**：

- 可用性成立：干净任务集 21/21 = 100% 准确，中英文无差异，混淆矩阵纯对角；单次路由中位 730ms、$0.000035（$0.035/千次）
- 门控建议：地板 0.7 + 按委托代价分级（低代价 ≥0.6 自动、agentic ≥0.8）；所有阈值下已路由条目准确率不降，阈值只是覆盖率 vs 打扰率旋钮；低置信时把 top-2 概率给父 agent 一键确认
- 问题设计纪律（关键经验）：criteria 必须写得**平实**——结构化 `what`/`not_for` 会把模糊任务置信从 0.74 吹到 0.97，掩盖真实歧义；criteria 词汇是概率吸引子（简报里的 "mechanical" 会把 0.30 概率吸给 deepseek）；层级路由（agent → model/mode）已验证可并入同一调用
- 诚实的 caveat：本次 ground truth 与 criteria 同源（验证的是"Jev 忠实执行矩阵"，不是矩阵正确性），n=21 是手写干净集。接入前先做：能力矩阵去重叠 + 真实委托语料回放 + 历史完成质量数据（#724）接入

### 第三阶段：CollaborationSession 完整形态按需吸收（设计文档 D1–D10）

按需拆选，不必全做：close_delegation_session（持久化关闭）、多轮状态面板 / 完整 timeline、Coordinator 统一入口（子会话弹窗与 full-tab 发送都走账本准入）。

### 远期

- **推荐与全量自动路由**（#731 的终点：Jev 路由线 + 评测数据成熟后的演进形态）
- 角色化 Agent 团队、v3 远程 Agent（RemoteSpawner）

## 维护规则

- 定期 `git merge upstream/main` 同步上游，冲突原则：**上游的架构演进优先**（如 acp→agent-client-protocol 2.2 迁移吞掉 DropSite 双位点诊断——新架构只有单一丢失位点，属正确简化）
- ZCode adapter 版本 pin 与 codeg 侧必须同步 bump（registry 测试强制）
- 自用验证通过的能力，仍按 #731 边界拆小 PR 回上游，保持上游影响力

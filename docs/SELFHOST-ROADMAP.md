# 自用集成线路线图（selfhost/main）

> 状态：Active（2026-09-26 建立）
> 性质：个人自用集成分支，先完整验证再拆小 PR 回上游（与 Discussion #731 的边界一致）
> 基座：`codex/delegation-continuation-local`（PR #693 同会话续作）+ ZCode 内置（PR #768）+ upstream main 0.32.2
>
> 规划来源：
> - `docs/codeg-continuable-delegation-session-design.md` — CollaborationSession 完整设计（终极形态）
> - `docs/STRATEGY-MEMO.md` — v1→v2→v3 演进与产品功能（PK / 角色团队）
> - [Discussion #731](https://github.com/xintaofei/codeg/discussions/731) — 委托编排、配置选择与评测闭环提案
> - Issue #603（续作跟踪）、Issue #724（委托结果持久化 + 子 Agent 看板）、PR #693 范围映射

## 已就位

- delegation_task 账本：准入、ResumeBinding、冻结终态、每源单后继、同请求幂等
- 严格恢复：live reuse → resume → load → 明确失败，不静默冷启动；agent/session/cwd 身份硬约束
- 启动对账：中断任务投影为 unknown/interrupted，不盲发
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

### 第 1 步：能力发现 + 显式选择（Discussion #731 第一阶段）

- 向主 Agent 暴露子 Agent 支持的 Model / Reasoning / Mode 清单（get_session_info 或新工具）
- 委托时按 call 选 model/mode（与上游 PR #505 / #616 同向，但自用线不受上游节奏限制）
- 注意 ZCode：模型目录在 `~/.zcode/v2/config.json` provider 表，会话快照只带当前模型（协议校准结论）

### 第 2 步：@Session 找回子会话（PR #693 未覆盖 + 设计文档一等入口）

- 父会话 @ 面板增加「本次对话的子智能体」分组（运行中 / 可续 / 需恢复 / 已关闭）
- 当前搜索默认排除 delegation children，需要打开受控入口
- 设计文档 §14.4 的排序与条目形态直接可用

### 第 3 步：数据积累 + 看板（Issue #724）

- 每次 delegation 落库：agent、model、reasoning、token、耗时、验证结果、返工轮数、最终接受与否
- delegation_task 账本已有骨架，缺的是 per-turn 指标列与聚合视图
- 这是 Discussion #731「评测闭环」的数据地基；后续才谈推荐与受控路由

### 第 4 步：向 CollaborationSession 完整形态靠拢（设计文档 D1-D10）

按需拆选，不必全做：

- close_delegation_session（持久化关闭，现在只有释放确认）
- 多轮状态面板 / 完整 timeline（子会话弹窗增强）
- Coordinator 统一入口（子会话弹窗与 full-tab 发送都走账本准入，父智能体可见）

### 远期（STRATEGY-MEMO，未排期）

- 编程 PK 场（周末档位：触发器 + 分屏对比 + 计分板；底层委托全有，只缺 UI）
- 角色化 Agent 团队（leader/build/review）
- v3 远程 Agent（RemoteSpawner）

## 维护规则

- 定期 `git merge upstream/main` 同步上游，冲突原则：**上游的架构演进优先**（如 acp→agent-client-protocol 2.2 迁移吞掉 DropSite 双位点诊断——新架构只有单一丢失位点，属正确简化）
- ZCode adapter 版本 pin 与 codeg 侧必须同步 bump（registry 测试强制）
- 自用验证通过的能力，仍按 #731 边界拆小 PR 回上游，保持上游影响力

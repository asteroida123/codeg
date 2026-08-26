//! Launch-time capability snapshot — `ResolvedLaunchProfile`.
//!
//! 在启动前把「请求了什么 / 实际送达了什么 / 出了什么警告」固化为值对象，
//! 写入 Session / WorkTask 的审计事件（`config_effective`）。它不改变执行
//! 语义：审计不阻断，也不替调用方决定值；真实送达由各 Adapter 决定，这里
//! 只负责基于能力描述（[`capability_descriptor`]）记录与提示。
//!
//! Arena 这类控制变量实验依赖这份快照展示 requested vs applied——启动
//! 前明确提示「不支持」，而不是把 `max` 默默降级成 `high`。所有维度在
//! Core 侧命名保持通用（model / effort / permission / skills / mcp），
//! 不出现 Arena 业务名词（ADR 0001，P2）。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::acp::connection::agent_delivers_wire_mcp;
use crate::acp::registry::get_agent_meta;
use crate::commands::acp::{skill_storage_spec, GROK_PERMISSION_MODES};
use crate::models::agent::AgentType;

/// 一个能力维度在 Codeg 侧的可送达性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySupport {
    /// Codeg 可经 ACP 线缆 / 公开接口直接送达并控制。
    Native,
    /// 存在送达路径，但受 adapter 限制（如按 agent 全局生效、需环境映射）。
    Adapted,
    /// 该维度无法送达；请求会被如实记录为未应用。
    Unsupported,
}

/// 会话级选择策略：`inherit` 跟随全局，`none` 禁用，`selected` 白名单。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    #[default]
    Inherit,
    None,
    Selected,
}

/// Skills 的会话级选择策略。当前 Core 尚未实现会话级隔离（最细只到
/// agent 全局 / 项目级 scope），但 `ResolvedLaunchProfile` 从第一天就把
/// 该维度纳入快照，后续 Skill Policy（inherit / none / selected）只是把
/// `applied` 变为真实送达，而不改变审计形状。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SkillPolicy {
    #[serde(default)]
    pub mode: SelectionMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// MCP 的会话级选择策略，语义同 [`SkillPolicy`]。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct McpPolicy {
    #[serde(default)]
    pub mode: SelectionMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

/// 启动前请求的能力集合。空字段 = 沿用 adapter 默认 / 全局设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LaunchProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 统一 effort 词表不存在（grok: low/medium/high/xhigh；codex 按模型
    /// 各自声明；claude/kimi 走自有键）。这里如实传递请求值，由已知词表的
    /// agent 负责校验提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default)]
    pub skills: SkillPolicy,
    #[serde(default)]
    pub mcp: McpPolicy,
}

/// 单个 agent 的能力描述。判定基于仓库现有事实，不发明新的数据源：
/// - mcp：`supports_mcp`（registry）+ [`agent_delivers_wire_mcp`] 闸门
/// - skills：`skill_storage_spec`（有存储规格 = 可注入，但为全局/项目级）
/// - model / effort / permission：全部 agent 走 ACP config option，wire 级
///   可送达；effort 词表是**模型维度**的属性（见 [`effort_levels_for`]），
///   不挂在 agent 描述上；permission 已知静态词表仅 grok（launch flag 级，
///   全模型共享）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentCapabilityDescriptor {
    pub agent_id: AgentType,
    pub models: CapabilitySupport,
    pub reasoning: CapabilitySupport,
    pub permissions: CapabilitySupport,
    /// 已知 permission 词表；`None` = 无公开词表。
    pub permission_modes: Option<&'static [&'static str]>,
    /// 会话级 skills 隔离等级（当前最细为 agent 全局 → Adapted）。
    pub skills: CapabilitySupport,
    /// 会话级 MCP 隔离等级。
    pub mcp: CapabilitySupport,
}

/// Grok 的 reasoning-effort 档位，与 `connection.rs::grok_effort_label`
/// 的 canonical id（low/medium/high/xhigh）保持一致。Grok 的 per-model
/// 可切换列表来自运行时 `sessionConfig` 事件，静态层没有按模型的词表，
/// 因此这里以跨模型超集兜底（任何模型可能出现的档位都在其中）。
const GROK_EFFORT_LEVELS: &[&str] = &["low", "medium", "high", "xhigh"];

/// 按 (agent, model) 查询 effort 词表——effort 的合法值集跟随**模型**，
/// 最终会体现到发给模型的请求中：
/// - codex：直接读 bundled snapshot 里该模型的 `supported_reasoning_levels`
/// - grok：静态层无 per-model 列表（运行时事件），以 agent 级超集兜底
/// - 其余无公开词表 → `None`，值原样透传且不校验
pub fn effort_levels_for(agent: AgentType, model: Option<&str>) -> Option<Vec<String>> {
    match agent {
        AgentType::Codex => crate::acp::codex_model_catalog::reasoning_levels_for_model(model?),
        AgentType::Grok => Some(
            GROK_EFFORT_LEVELS
                .iter()
                .map(|level| (*level).to_string())
                .collect(),
        ),
        _ => None,
    }
}

/// 基于仓库现有事实为内置 / 自定义 agent 构造能力描述。
pub fn capability_descriptor(agent: AgentType) -> AgentCapabilityDescriptor {
    let mcp = if agent == AgentType::OpenClaw {
        // OpenClaw 拒绝 `mcpServers` 中的任何条目（session/new 会失败）。
        CapabilitySupport::Unsupported
    } else if !agent_delivers_wire_mcp(agent) {
        // pi 不接收 wire MCP；supports_mcp 为 true 但注入被单独闸门拦截。
        CapabilitySupport::Adapted
    } else {
        CapabilitySupport::Native
    };
    let skills = if skill_storage_spec(agent).is_some() {
        // 有存储规格 = 可注入，但只有 Global / Project 两级，无会话级选择。
        CapabilitySupport::Adapted
    } else {
        CapabilitySupport::Unsupported
    };
    let permission_modes = match agent {
        AgentType::Grok => Some(GROK_PERMISSION_MODES),
        _ => None,
    };
    AgentCapabilityDescriptor {
        agent_id: agent,
        models: CapabilitySupport::Native,
        reasoning: CapabilitySupport::Native,
        permissions: CapabilitySupport::Native,
        permission_modes,
        skills,
        mcp,
    }
}

/// 解析后的启动能力快照。`applied` 是实际送达值；`warnings` 解释请求与
/// 送达之间的任何落差。序列化后可直接进入审计事件。
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedLaunchProfile {
    pub agent_id: String,
    /// Adapter（ACP registry）版本；未注册自定义 agent 为占位版本。
    pub adapter_version: Option<String>,
    pub requested: LaunchProfileRequest,
    /// 键：`model` / `effort` / `permission` / `skills.mode` / `mcp.mode`；
    /// 值：送达值（无法送达的会话级请求回落为 `inherit`）。
    pub applied: BTreeMap<String, serde_json::Value>,
    pub warnings: Vec<String>,
}

/// 把启动前请求解析为能力快照。纯函数：不碰 DB、不启动任何进程。
pub fn resolve_launch_profile(
    agent: AgentType,
    request: &LaunchProfileRequest,
) -> ResolvedLaunchProfile {
    let descriptor = capability_descriptor(agent);
    let meta = get_agent_meta(agent);
    let mut applied = BTreeMap::new();
    let mut warnings = Vec::new();

    if let Some(model) = &request.model {
        applied.insert("model".into(), serde_json::Value::String(model.clone()));
    }
    if let Some(effort) = &request.effort {
        applied.insert("effort".into(), serde_json::Value::String(effort.clone()));
        // 词表跟随模型（codex per-model、grok 超集兜底）；无词表 → 透传。
        if let Some(levels) = effort_levels_for(agent, request.model.as_deref()) {
            if !levels.iter().any(|level| level == effort) {
                warnings.push(format!(
                    "effort '{}' is not in the known vocabulary for {} model {:?} ({:?}); \
                     delivered verbatim to the adapter",
                    effort, meta.name, request.model, levels
                ));
            }
        }
    }
    if let Some(permission) = &request.permission {
        applied.insert(
            "permission".into(),
            serde_json::Value::String(permission.clone()),
        );
        if let Some(modes) = descriptor.permission_modes {
            if !modes.contains(&permission.as_str()) {
                warnings.push(format!(
                    "permission '{}' is not in the known vocabulary for {} ({:?}); \
                     delivered verbatim to the adapter",
                    permission, meta.name, modes
                ));
            }
        }
    }

    resolve_session_level_policy(
        "skills",
        request.skills.mode,
        descriptor.skills,
        meta.name,
        &mut applied,
        &mut warnings,
    );
    resolve_session_level_policy(
        "mcp",
        request.mcp.mode,
        descriptor.mcp,
        meta.name,
        &mut applied,
        &mut warnings,
    );

    ResolvedLaunchProfile {
        agent_id: meta.name.to_string(),
        adapter_version: meta.registry_version().map(str::to_string),
        requested: request.clone(),
        applied,
        warnings,
    }
}

/// 会话级选择策略的统一回落：`inherit` 静默；`none` / `selected` 在
/// descriptor 非 Native 时回落到 `inherit` 并生成告警，Native 时如实记录。
fn resolve_session_level_policy(
    dimension: &str,
    mode: SelectionMode,
    support: CapabilitySupport,
    agent_name: &str,
    applied: &mut BTreeMap<String, serde_json::Value>,
    warnings: &mut Vec<String>,
) {
    let key = format!("{dimension}.mode");
    let mode_label = match mode {
        SelectionMode::Inherit => "inherit",
        SelectionMode::None => "none",
        SelectionMode::Selected => "selected",
    };
    if mode == SelectionMode::Inherit {
        applied.insert(key, serde_json::Value::String("inherit".into()));
        return;
    }
    match support {
        CapabilitySupport::Native => {
            applied.insert(key, serde_json::Value::String(mode_label.into()));
        }
        CapabilitySupport::Adapted => {
            warnings.push(format!(
                "session-level {dimension} selection is not enforceable for {agent_name}; \
                 only the global scope can be delivered — requested {mode_label} \
                 falls back to inherit"
            ));
            applied.insert(key, serde_json::Value::String("inherit".into()));
        }
        CapabilitySupport::Unsupported => {
            warnings.push(format!(
                "{agent_name} does not support {dimension} delivery; requested \
                 {mode_label} cannot be applied"
            ));
            applied.insert(key, serde_json::Value::String("inherit".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(agent: AgentType, request: &LaunchProfileRequest) -> ResolvedLaunchProfile {
        resolve_launch_profile(agent, request)
    }

    #[test]
    fn descriptor_flags_wire_and_scope_limits() {
        assert_eq!(
            capability_descriptor(AgentType::OpenClaw).mcp,
            CapabilitySupport::Unsupported,
            "OpenClaw rejects any mcpServers entry"
        );
        assert_eq!(
            capability_descriptor(AgentType::Pi).mcp,
            CapabilitySupport::Adapted,
            "pi's wire MCP is gated out despite supports_mcp"
        );
        let claude = capability_descriptor(AgentType::ClaudeCode);
        assert_eq!(claude.mcp, CapabilitySupport::Native);
        assert_eq!(
            claude.skills,
            CapabilitySupport::Adapted,
            "skills inject via per-agent symlink, global/project scope only"
        );
        let grok = capability_descriptor(AgentType::Grok);
        assert_eq!(grok.permission_modes, Some(GROK_PERMISSION_MODES));
        // All ACP agents take model/effort/permission via config options.
        assert_eq!(grok.models, CapabilitySupport::Native);
        assert_eq!(grok.reasoning, CapabilitySupport::Native);
        assert_eq!(grok.permissions, CapabilitySupport::Native);
    }

    #[test]
    fn codex_effort_vocabulary_follows_the_model() {
        let snapshot = crate::acp::codex_model_catalog::bundled_snapshot_models();
        let model = snapshot
            .iter()
            .find(|m| {
                m.get("supported_reasoning_levels")
                    .and_then(serde_json::Value::as_array)
                    .is_some()
            })
            .expect("a bundled model with reasoning levels");
        let slug = model
            .get("slug")
            .and_then(serde_json::Value::as_str)
            .expect("slug");
        let tier = model
            .get("supported_reasoning_levels")
            .and_then(serde_json::Value::as_array)
            .and_then(|levels| levels.first())
            .and_then(|level| level.get("effort"))
            .and_then(serde_json::Value::as_str)
            .expect("first tier");

        // 模型已知且档位在该模型自己的列表内 → 静默。
        let ok = profile(
            AgentType::Codex,
            &LaunchProfileRequest {
                model: Some(slug.to_string()),
                effort: Some(tier.to_string()),
                ..Default::default()
            },
        );
        assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);

        // 模型已知但档位不在该模型的列表内 → 告警（抓"档位对模型无效"）。
        let bad = profile(
            AgentType::Codex,
            &LaunchProfileRequest {
                model: Some(slug.to_string()),
                effort: Some("nonexistent-tier".into()),
                ..Default::default()
            },
        );
        assert!(
            bad.warnings.iter().any(|w| w.contains("nonexistent-tier")),
            "{:?}",
            bad.warnings
        );

        // 模型未知 → 无词表可查，透传无告警。
        let unknown = profile(
            AgentType::Codex,
            &LaunchProfileRequest {
                model: Some("definitely-not-a-model".into()),
                effort: Some("whatever".into()),
                ..Default::default()
            },
        );
        assert!(unknown.warnings.is_empty(), "{:?}", unknown.warnings);
    }

    #[test]
    fn grok_effort_super_set_still_covers_model_requests() {
        // grok 的 per-model 列表来自运行时事件；静态层以跨模型超集兜底。
        assert!(effort_levels_for(AgentType::Grok, Some("grok-4.5")).is_some());
        assert!(effort_levels_for(AgentType::Grok, None).is_some());
        assert!(effort_levels_for(AgentType::ClaudeCode, None).is_none());
    }

    #[test]
    fn known_vocabulary_values_resolve_cleanly() {
        let p = profile(
            AgentType::Grok,
            &LaunchProfileRequest {
                effort: Some("high".into()),
                permission: Some("plan".into()),
                ..Default::default()
            },
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(p.applied["effort"].as_str(), Some("high"));
        assert_eq!(p.applied["permission"].as_str(), Some("plan"));
    }

    #[test]
    fn unknown_effort_is_delivered_verbatim_with_a_warning() {
        let p = profile(
            AgentType::Grok,
            &LaunchProfileRequest {
                effort: Some("turbo".into()),
                ..Default::default()
            },
        );
        assert_eq!(p.applied["effort"].as_str(), Some("turbo"));
        assert!(
            p.warnings.iter().any(|w| w.contains("turbo")),
            "{:?}",
            p.warnings
        );
    }

    #[test]
    fn agents_without_a_published_vocabulary_pass_through_silently() {
        let p = profile(
            AgentType::ClaudeCode,
            &LaunchProfileRequest {
                effort: Some("whatever".into()),
                ..Default::default()
            },
        );
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(p.applied["effort"].as_str(), Some("whatever"));
    }

    #[test]
    fn unsupported_dimension_degrades_to_inherit_with_a_warning() {
        let p = profile(
            AgentType::OpenClaw,
            &LaunchProfileRequest {
                mcp: McpPolicy {
                    mode: SelectionMode::Selected,
                    ids: vec!["github".into()],
                },
                ..Default::default()
            },
        );
        assert_eq!(p.applied["mcp.mode"].as_str(), Some("inherit"));
        assert!(p.warnings.iter().any(|w| w.contains("mcp")), "{:?}", p.warnings);
    }

    #[test]
    fn adapted_dimension_warns_that_selection_falls_back_to_global_scope() {
        let p = profile(
            AgentType::ClaudeCode,
            &LaunchProfileRequest {
                skills: SkillPolicy {
                    mode: SelectionMode::Selected,
                    ids: vec!["sql-review".into()],
                },
                ..Default::default()
            },
        );
        assert_eq!(p.applied["skills.mode"].as_str(), Some("inherit"));
        assert!(
            p.warnings.iter().any(|w| w.contains("global")),
            "{:?}",
            p.warnings
        );
    }

    #[test]
    fn inherit_mode_is_silent_even_for_unsupported_dimensions() {
        let p = profile(AgentType::OpenClaw, &LaunchProfileRequest::default());
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
        assert_eq!(p.applied["skills.mode"].as_str(), Some("inherit"));
        assert_eq!(p.applied["mcp.mode"].as_str(), Some("inherit"));
    }

    #[test]
    fn profile_serializes_as_an_audit_snapshot() {
        let p = profile(
            AgentType::Grok,
            &LaunchProfileRequest {
                model: Some("grok-4.5".into()),
                effort: Some("low".into()),
                ..Default::default()
            },
        );
        let json = serde_json::to_value(&p).expect("serialize");
        assert!(json.get("agent_id").is_some());
        assert!(json.get("adapter_version").is_some());
        assert_eq!(json["requested"]["model"].as_str(), Some("grok-4.5"));
        assert_eq!(json["applied"]["model"].as_str(), Some("grok-4.5"));
        assert!(json.get("warnings").is_some());
    }
}
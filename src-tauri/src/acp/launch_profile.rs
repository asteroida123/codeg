//! Launch-time configuration snapshot — `ResolvedLaunchProfile`.
//!
//! 设计立场（评审校准）：底层改动做的是 **Codeg 的基础增强**，不为其某个
//! App（Arena / CCG）服务。因此本模块只做两件事：
//!
//! 1. **可解释性**——把「任务请求了什么配置 / 实际生效了什么 / 有什么落差」
//!    固化为快照，进入 WorkTask 的 `config_effective` 审计事件，任务详情
//!    页可查。这与竞技场无关，是所有任务的基础能力。
//! 2. **配置健康**——只对**有真实数据来源**的维度做校验：effort 词表跟随
//!    模型（codex 的 bundled catalog 自带 per-model 档位）；permission
//!    词表只有 grok 有（launch flag / `config.toml` 的真实机制）。
//!
//! 明确不做：为能力等级发明静态判断（如「某 agent 支持/不支持 mcp」）。
//! ACP 协议没有 mcp/skills 隔离的声明渠道，静态拍脑袋的判断没有权威
//! 来源，既不增强 Core，也不该成为未来 App 的地基。会话级选择
//! （`none` / `selected`）在 Core 实现会话级隔离之前，对所有 agent
//! **一致**回落 `inherit` 并告警——诚实标注能力边界，而不是假装知道。

use std::collections::BTreeMap;

use serde::Serialize;

use crate::acp::registry::get_agent_meta;
use crate::commands::acp::GROK_PERMISSION_MODES;
use crate::models::agent::AgentType;

/// 会话级选择策略：`inherit` 跟随全局，`none` 禁用，`selected` 白名单。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    #[default]
    Inherit,
    None,
    Selected,
}

/// Skills 的会话级选择策略。Core 尚未实现会话级隔离，但快照从第一天就
/// 纳入该维度：请求被如实记录，回落与告警统一处理（见 `resolve`）。
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

/// 启动前请求的配置集合。空字段 = 沿用 adapter 默认 / 全局设置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct LaunchProfileRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 统一 effort 词表不存在（grok: low/medium/high/xhigh；codex 按模型
    /// 各自声明；claude/kimi 走自有键）。这里如实传递请求值，由有真实
    /// 数据来源的 agent 校验提示。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    #[serde(default)]
    pub skills: SkillPolicy,
    #[serde(default)]
    pub mcp: McpPolicy,
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

/// Grok 的 permission 词表（`commands/acp.rs::GROK_PERMISSION_MODES` 的
/// launch flag / `config.toml` 机制，全模型共享）；其余 agent 的 permission
/// 走 ACP modes 事件，无静态词表 → `None`，值透传不校验。
pub fn permission_modes_for(agent: AgentType) -> Option<&'static [&'static str]> {
    match agent {
        AgentType::Grok => Some(GROK_PERMISSION_MODES),
        _ => None,
    }
}

/// 解析后的启动配置快照。`applied` 是实际生效值；`warnings` 解释请求与
/// 生效之间的任何落差。序列化后可直接进入审计事件。
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedLaunchProfile {
    pub agent_id: String,
    /// Adapter（ACP registry）版本；未注册自定义 agent 为占位版本。
    pub adapter_version: Option<String>,
    pub requested: LaunchProfileRequest,
    /// 键：`model` / `effort` / `permission` / `skills.mode` / `mcp.mode`；
    /// 值：生效值（无法送达的会话级请求回落为 `inherit`）。
    pub applied: BTreeMap<String, serde_json::Value>,
    pub warnings: Vec<String>,
}

/// 把启动前请求解析为配置快照。纯函数：不碰 DB、不启动任何进程。
pub fn resolve_launch_profile(
    agent: AgentType,
    request: &LaunchProfileRequest,
) -> ResolvedLaunchProfile {
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
        if let Some(modes) = permission_modes_for(agent) {
            if !modes.contains(&permission.as_str()) {
                warnings.push(format!(
                    "permission '{}' is not in the known vocabulary for {} ({:?}); \
                     delivered verbatim to the adapter",
                    permission, meta.name, modes
                ));
            }
        }
    }

    resolve_session_level_policy("skills", request.skills.mode, &mut applied, &mut warnings);
    resolve_session_level_policy("mcp", request.mcp.mode, &mut applied, &mut warnings);

    ResolvedLaunchProfile {
        agent_id: meta.name.to_string(),
        adapter_version: meta.registry_version().map(str::to_string),
        requested: request.clone(),
        applied,
        warnings,
    }
}

/// 会话级选择策略的统一裁决。Core 尚未实现会话级隔离（ACP 协议也没有
/// 声明渠道），因此 `none` / `selected` 对**所有 agent 一致**回落 `inherit`
/// 并告警——不按 agent 区分，因为任何区分都缺乏权威来源。
fn resolve_session_level_policy(
    dimension: &str,
    mode: SelectionMode,
    applied: &mut BTreeMap<String, serde_json::Value>,
    warnings: &mut Vec<String>,
) {
    let key = format!("{dimension}.mode");
    if mode == SelectionMode::Inherit {
        applied.insert(key, serde_json::Value::String("inherit".into()));
        return;
    }
    let mode_label = match mode {
        SelectionMode::Inherit => "inherit",
        SelectionMode::None => "none",
        SelectionMode::Selected => "selected",
    };
    warnings.push(format!(
        "session-level {dimension} selection is not available in Codeg core yet \
         (the ACP protocol declares no isolation channel); requested {mode_label} \
         falls back to inherit"
    ));
    applied.insert(key, serde_json::Value::String("inherit".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(agent: AgentType, request: &LaunchProfileRequest) -> ResolvedLaunchProfile {
        resolve_launch_profile(agent, request)
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
    fn permission_vocabulary_warns_outside_grok_launch_flag_mechanism() {
        // grok 六档（launch flag 机制）：plan 静默，词表外值告警。
        let ok = profile(
            AgentType::Grok,
            &LaunchProfileRequest {
                permission: Some("acceptEdits".into()),
                ..Default::default()
            },
        );
        assert!(ok.warnings.is_empty(), "{:?}", ok.warnings);

        let bad = profile(
            AgentType::Grok,
            &LaunchProfileRequest {
                permission: Some("sneaky".into()),
                ..Default::default()
            },
        );
        assert!(
            bad.warnings.iter().any(|w| w.contains("sneaky")),
            "{:?}",
            bad.warnings
        );

        // 无公开词表的 agent 透传不校验。
        let pass = profile(
            AgentType::ClaudeCode,
            &LaunchProfileRequest {
                permission: Some("whatever".into()),
                ..Default::default()
            },
        );
        assert!(pass.warnings.is_empty(), "{:?}", pass.warnings);
    }

    #[test]
    fn session_level_selection_falls_back_uniformly_with_a_warning() {
        // Core 尚未实现会话级隔离：所有 agent 一致回落 inherit + 告警，
        // 不按 agent 区分（任何区分都缺乏权威来源）。
        for agent in [AgentType::ClaudeCode, AgentType::OpenClaw, AgentType::Grok] {
            let p = profile(
                agent,
                &LaunchProfileRequest {
                    skills: SkillPolicy {
                        mode: SelectionMode::None,
                        ids: Vec::new(),
                    },
                    mcp: McpPolicy {
                        mode: SelectionMode::Selected,
                        ids: vec!["github".into()],
                    },
                    ..Default::default()
                },
            );
            assert_eq!(p.applied["skills.mode"].as_str(), Some("inherit"));
            assert_eq!(p.applied["mcp.mode"].as_str(), Some("inherit"));
            assert!(
                p.warnings.iter().any(|w| w.contains("skills") && w.contains("inherit")),
                "{:?}",
                p.warnings
            );
            assert!(
                p.warnings.iter().any(|w| w.contains("mcp")),
                "{:?}",
                p.warnings
            );
        }
    }

    #[test]
    fn inherit_mode_is_silent() {
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
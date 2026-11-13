//! 商奕参与者子智能体仿真（Agent Swarm，设计文档 §5.7）。
//!
//! # 两项职责（§5.7）
//!
//! 1. **均衡选择引擎的行为证据来源**——`persona_sim` 判据的架构化升级
//! 2. **非常规（矩阵外）行动的生成器**——与 §5.7.1 的开放行动空间配套
//!
//! # 铁律 1 在 Swarm 的落点（§5.7 明确）
//!
//! sub-agent 可以**提议**行动（含矩阵外），但**评估一律回主引擎**——
//! novel move 注册入 `GameSpec` 后由解析求解与选择引擎重算后果。
//! **Swarm 的输出是行为证据与候选来源，不是结论本身。**
//!
//! 本 crate 在类型上区分：
//! - [`Move`]：persona 的**提议**（可能是矩阵外的）
//! - [`CandidateResult`]：**引擎判定**后的结果
//!
//! 后者**只能**由 [`crate::orchestrator::evaluate_candidates`] 之类调用求解器产生，
//! 不能由 persona 直接构造——这使"提议 ≠ 结论"在类型上成立。
//!
//! # capability 预检必须响亮失败（§5.7，仅借鉴设计自 dsh）
//!
//! 在委派任何 persona 之前，先校验所选 provider/模型-角色组合是否支持所需能力
//! （persona 覆盖、工具白名单、输出 schema、递归深度上限）；**不支持即返回类型化错误，
//! 绝不静默降级**。
//!
//! 原因很直接：若"对抗性监管者"persona 因后端不支持而被静默忽略、退化成
//! "乐于助人的助手"，仿真结论就是**错的**——而这种错误在报告里看不出来。
//! 故 [`CapabilitySet::require`] 对缺失能力返回 [`SwarmError::CapabilityMissing`]，
//! **没有**"降级继续"的分支。

pub mod llm_actor;
pub mod orchestrator;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use shangyi_gamespec::GameSpec;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SwarmError {
    #[error("provider `{provider}` 缺少所需能力 `{capability}`——绝不静默降级")]
    CapabilityMissing {
        provider: String,
        capability: String,
    },
    #[error("递归深度超限：当前 {current}，上限 {limit}")]
    DepthExceeded { current: u32, limit: u32 },
    #[error("persona `{0}` 未被注册")]
    UnknownPersona(String),
    #[error("rollout 次数须 > 0")]
    ZeroRollouts,
    #[error("persona 集合为空——无仿真对象")]
    EmptyPersonas,
    #[error("提案的 `actor` 不在博弈的玩家或环境方中：`{0}`")]
    UnknownActor(String),
    #[error("rollout 未收敛：{0}")]
    NotConverged(String),
}

/// 能力集合——描述一个 provider/模型-角色组合**支持什么**。
///
/// 与 dsh 的 `SubagentCapabilities` 对应（`仅借鉴设计`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// 是否支持覆盖系统提示（persona 注入）。
    pub persona: bool,
    /// 是否支持按 persona 限定工具集。
    pub tool_filter: bool,
    /// 是否支持结构化输出 schema。
    pub output_schema: bool,
    /// 是否支持递归深度上限。
    pub depth_limit: bool,
    /// 是否支持**可续子会话**（崩溃后恢复同一批 persona，§5.7）。
    pub continuable: bool,
}

impl CapabilitySet {
    /// 全部能力（用于内置 provider）。
    pub fn full() -> Self {
        Self {
            persona: true,
            tool_filter: true,
            output_schema: true,
            depth_limit: true,
            continuable: true,
        }
    }

    /// 检查是否具备某项能力，缺失即返回类型化错误。
    ///
    /// **这是 fail-loud 的唯一实现入口**——调用方不得绕过它自行判断后降级。
    pub fn require(
        &self,
        capability: &str,
        provider: &str,
    ) -> Result<(), SwarmError> {
        let has = match capability {
            "persona" => self.persona,
            "tool_filter" => self.tool_filter,
            "output_schema" => self.output_schema,
            "depth_limit" => self.depth_limit,
            "continuable" => self.continuable,
            other => {
                return Err(SwarmError::CapabilityMissing {
                    provider: provider.to_string(),
                    capability: other.to_string(),
                })
            }
        };
        if !has {
            return Err(SwarmError::CapabilityMissing {
                provider: provider.to_string(),
                capability: capability.to_string(),
            });
        }
        Ok(())
    }

    /// 缺失的能力清单（空 = 全具备）。
    pub fn missing(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.persona {
            out.push("persona");
        }
        if !self.tool_filter {
            out.push("tool_filter");
        }
        if !self.output_schema {
            out.push("output_schema");
        }
        if !self.depth_limit {
            out.push("depth_limit");
        }
        if !self.continuable {
            out.push("continuable");
        }
        out
    }
}

/// 委派请求所需的能力（与 provider 能力做交集校验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RequiredCapabilities {
    pub persona: bool,
    pub tool_filter: bool,
    pub output_schema: bool,
}

impl RequiredCapabilities {
    pub fn all() -> Self {
        Self {
            persona: true,
            tool_filter: true,
            output_schema: true,
        }
    }

    /// 校验 provider 是否满足全部所需能力。
    ///
    /// **返回类型化错误而非 bool**：调用方必须显式处理失败，
    /// 无法"忘记检查"后继续（返回 bool 时容易被 `if !ok { /* 忽略 */ }`）。
    pub fn check(
        &self,
        caps: &CapabilitySet,
        provider: &str,
    ) -> Result<(), SwarmError> {
        if self.persona {
            caps.require("persona", provider)?;
        }
        if self.tool_filter {
            caps.require("tool_filter", provider)?;
        }
        if self.output_schema {
            caps.require("output_schema", provider)?;
        }
        Ok(())
    }
}

/// 行动类型：策略集内动作，或矩阵外的非常规动作。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "body", rename_all = "snake_case")]
pub enum Action {
    /// 策略集内的动作（下标指向 `GameSpec::strategies`）。
    InMatrix { strategy: String },
    /// **矩阵外**动作——§5.7.1 的开放行动空间。
    Novel {
        /// 行动描述。
        description: String,
        /// 该行动引入的新参与者（进 `external_actors`）。
        #[serde(default)]
        introduces: Vec<String>,
        /// 合法性依据。
        #[serde(default)]
        legality_basis: Vec<String>,
        /// 资源可得性依据。
        #[serde(default)]
        feasibility_basis: Vec<String>,
        /// 预期收益区间。
        expected_benefit: (f64, f64),
        /// 预期成本区间。
        expected_cost: (f64, f64),
    },
}

impl Action {
    /// 是否矩阵外。
    pub fn is_novel(&self) -> bool {
        matches!(self, Action::Novel { .. })
    }
}

/// persona 的一步行动**提议**。
///
/// ⚠️ 这是**提议**，不是结论。其后果必须经主引擎评估（见 [`orchestrator`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Move {
    /// 行动者（须是 `GameSpec` 的玩家或环境方）。
    pub actor: String,
    pub action: Action,
    /// 理由（进报告，可审计"为什么 persona 这样行动"）。
    pub rationale: String,
}

impl Move {
    pub fn new(actor: impl Into<String>, action: Action, rationale: impl Into<String>) -> Self {
        Self {
            actor: actor.into(),
            action,
            rationale: rationale.into(),
        }
    }
}

/// persona 的定义（由 §6.2 参与者画像生成的角色卡）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Persona {
    /// 对应的参与者 id。
    pub participant_id: String,
    /// 显示名（进报告）。
    pub display_name: String,
    /// 行为风格（由画像的 `decision_style` 与其他字段生成）。
    pub decision_style: String,
    /// 供 LLM 使用的完整角色卡文本（若 provider 支持 persona 覆盖）。
    pub role_card: String,
    /// 该 persona 允许使用的工具（provider 须支持 `tool_filter`）。
    #[serde(default)]
    pub tool_allowlist: Vec<String>,
}

/// persona 集合——rollout 的输入。
///
/// **持久化契约（§5.7）**：集合须能序列化落盘，使 Swarm 崩溃后恢复
/// **同一批 persona**，而非意外重组过的。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaSet {
    pub personas: Vec<Persona>,
    /// 递归深度（**持久化**，不存运行时：否则恢复后预算被重置）。
    pub delegation_depth: u32,
    /// 深度上限。
    pub max_depth: u32,
}

impl PersonaSet {
    pub fn new(personas: Vec<Persona>) -> Self {
        Self {
            personas,
            delegation_depth: 0,
            max_depth: 3,
        }
    }

    pub fn get(&self, participant_id: &str) -> Option<&Persona> {
        self.personas.iter().find(|p| p.participant_id == participant_id)
    }

    /// 校验深度预算。
    ///
    /// **为何深度要持久化**（§5.7 原文）：仅存运行时的深度在崩溃恢复后会被重置，
    /// 导致重组出一批与崩溃前不同的 persona——同一案例两次运行给出不同结论。
    pub fn check_depth(&self) -> Result<(), SwarmError> {
        if self.delegation_depth > self.max_depth {
            return Err(SwarmError::DepthExceeded {
                current: self.delegation_depth,
                limit: self.max_depth,
            });
        }
        Ok(())
    }

    /// 全部 participant_id。
    pub fn ids(&self) -> BTreeSet<&str> {
        self.personas.iter().map(|p| p.participant_id.as_str()).collect()
    }
}

/// 一次 rollout 的轨迹。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Trajectory {
    /// 逐步的 move 序列（按时序）。
    pub moves: Vec<Move>,
    /// 该轨迹的终止原因。
    pub stop_reason: StopReason,
    /// 本次 rollout 的 persona 参数扰动种子（多样性采样的可复现凭据）。
    pub seed: u64,
}

/// 轨迹终止原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 达到回合上限。
    MaxRounds,
    /// 无玩家再有动机改变行动（事实上的稳定点）。
    Stable,
    /// 外部中止。
    Aborted,
}

/// 行动分布——`persona_sim` 判据的输出（进均衡选择引擎）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionDistribution {
    /// 通过引擎评估后的候选结果，按 (actor, action) 归一化。
    pub entries: Vec<CandidateResult>,
    /// 总 rollout 数。
    pub rollouts: u32,
    /// 是否施加了 persona 参数扰动（多样性采样）。
    pub perturbed: bool,
}

impl ActionDistribution {
    /// 找到某 actor 的全部结果（按概率降序）。
    pub fn for_actor(&self, actor: &str) -> Vec<&CandidateResult> {
        let mut v: Vec<&CandidateResult> = self
            .entries
            .iter()
            .filter(|e| e.actor == actor)
            .collect();
        v.sort_by(|a, b| {
            b.probability
                .partial_cmp(&a.probability)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.action_key().cmp(&b.action_key()))
        });
        v
    }

    /// 全部矩阵外候选（§5.7.1 非常规行动的来源）。
    pub fn novel_actions(&self) -> Vec<&CandidateResult> {
        self.entries.iter().filter(|e| e.is_novel).collect()
    }

    /// 某 actor 上的概率和（用于自检是否归一化）。
    pub fn probability_sum_for(&self, actor: &str) -> f64 {
        self.entries
            .iter()
            .filter(|e| e.actor == actor)
            .map(|e| e.probability)
            .sum()
    }
}

/// **引擎评估后**的候选结果。
///
/// # 为何与 [`Move`] 分开
///
/// [`Move`] 是 persona 的提议；`CandidateResult` 是**引擎判定**。
/// dsh 的 `SubagentCapabilities` 与 §5.7 的铁律都要求"提议 ≠ 结论"。
/// 若允许 persona 直接产出"后果预测"，那 Swarm 就变成了"LLM 算数"——
/// 违背铁律 1。
///
/// 本类型**没有**公开构造器接收 `Move` 后直接返回：它只能由
/// [`orchestrator::evaluate_candidates`] 经求解器产生。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateResult {
    pub actor: String,
    /// 行动键（矩阵内为策略名；矩阵外为描述）。
    pub action: String,
    pub is_novel: bool,
    /// 该行动在 rollout 中出现的频率（作为"行为证据"，不是最终的 P(e)）。
    pub probability: f64,
    /// 出现频次。
    pub count: u32,
    /// 该候选是否**通过**引擎评估（对矩阵外候选 = 通过三重过滤）。
    ///
    /// 未通过的候选仍保留在结果里但标记为 false——**如实返回而非静默丢弃**，
    /// 使报告能展示"考虑过但排除了哪些路径"（§5.6.1 的透明性要求）。
    pub engine_accepted: bool,
    /// 引擎给出的拒绝原因（`engine_accepted == false` 时非空）。
    #[serde(default)]
    pub rejection: Vec<String>,
}

impl CandidateResult {
    /// 排序键（保证输出的确定性）。
    pub fn action_key(&self) -> (&str, &str) {
        (self.actor.as_str(), self.action.as_str())
    }
}

/// 校验全部 move 的 actor 都在博弈中有定义。
///
/// 使"persona 幻觉出一个不存在的参与者"在进入引擎前即被拦住——
/// 否则那些 move 会被静默丢弃，导致分布失真。
pub fn validate_actors(spec: &GameSpec, moves: &[Move]) -> Result<(), SwarmError> {
    let known: BTreeSet<&str> = spec
        .players
        .iter()
        .chain(spec.external_actors.iter())
        .map(|p| p.id.as_str())
        .collect();
    for m in moves {
        if !known.contains(m.actor.as_str()) {
            return Err(SwarmError::UnknownActor(m.actor.clone()));
        }
    }
    Ok(())
}

/// 把一系列轨迹汇总为 action → count 的计数表。
///
/// **不在此处算概率**：概率的分母取决于"哪些 move 通过了引擎评估"，
/// 那是 [`orchestrator::evaluate_candidates`] 的职责。此处只做确定性汇总。
pub fn tally_actions(trajectories: &[Trajectory]) -> BTreeMap<(String, String, bool), u32> {
    let mut out: BTreeMap<(String, String, bool), u32> = BTreeMap::new();
    for t in trajectories {
        for m in &t.moves {
            let (key, novel) = match &m.action {
                Action::InMatrix { strategy } => (strategy.clone(), false),
                Action::Novel { description, .. } => (description.clone(), true),
            };
            *out.entry((m.actor.clone(), key, novel)).or_insert(0) += 1;
        }
    }
    out
}

/// 持久化描述符（§5.7：借鉴 dsh 的 `subagent/descriptor`）。
///
/// 使 Swarm 崩溃后能恢复**同一批 persona**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmDescriptor {
    /// 描述符版本（升级时用于兼容判断）。
    pub version: u32,
    /// provider 名。
    pub provider: String,
    /// persona 集合（**完整持久化**，不是引用）。
    pub personas: PersonaSet,
    /// 本案例的 rollout 次数（恢复后沿用，保证可比）。
    pub rollouts: u32,
    /// 随机种子（恢复后沿用，保证可复现）。
    pub seed: u64,
}

/// 当前描述符版本。结构变更时递增。
pub const SWARM_DESCRIPTOR_VERSION: u32 = 1;

impl SwarmDescriptor {
    pub fn new(provider: impl Into<String>, personas: PersonaSet, rollouts: u32, seed: u64) -> Self {
        Self {
            version: SWARM_DESCRIPTOR_VERSION,
            provider: provider.into(),
            personas,
            rollouts,
            seed,
        }
    }

    /// 版本兼容检查。
    pub fn is_compatible(&self) -> bool {
        self.version == SWARM_DESCRIPTOR_VERSION
    }
}

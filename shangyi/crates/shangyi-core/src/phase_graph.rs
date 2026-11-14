//! 阶段图：**模式感知**的阶段/边集 + 产物闸门 + checkpoint 结算（设计文档 §7.3）。
//!
//! # 为什么需要这一层（而不是一个固定的阶段数组）
//!
//! §7.3 给出的三种模式走的是**同一状态机的不同边集**：
//!
//! | 模式 | 路径 |
//! |---|---|
//! | `scenario` | intake → intel → model_build → solve → select → recommend → report → archived |
//! | `whatif`   | intake → intel → model_build → **[solve 可选]** → **consequence** → recommend → report → archived |
//! | `discover` | intake → intel → model_build → solve → **discover** → recommend → report → archived |
//!
//! 用一个全局线性 `ORDER` 数组表达这三条路径是**做不到**的：它会把 `select`
//! 放进 `whatif`（那条路径上根本没有 select），也会让 `consequence` / `discover`
//! 两个节点无从表达。故本模块把"当前处于哪个阶段"与"能走到哪里"分开：
//! 前者是状态，后者由 [`PhaseGraph`] 按模式给出。
//!
//! # 单一权威（为什么闸门判据是 `produces`）
//!
//! 离开一个阶段时，闸门要求**该阶段自己的产物**已登记（[`PhaseGraph::produces`]）。
//! 等价地可以表述为"进入目标阶段要求前驱的产物"（§7.3 的 `requires`），但在带
//! **可选节点**的图上，后者需要一个"实际走过的路径"状态才能正确判断（跳过的
//! `solve` 不应被要求交出均衡解）。前者是路径局部的、无需额外状态、且结论相同。
//! 因此实现取 `produces` 为准，并把边界（严格小于）交给
//! [`PhaseMachine::rollback_to`] 统一处理——那里正是历史上出过一次差一错误的地方。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::consultation::ConsultationMode;
use crate::task_scope::{ScopedTaskResult, TaskScope};

// ─────────────────────────── 阶段 ───────────────────────────

/// 会话阶段（§7.3）。
///
/// **十个变体而非八个**（差异登记 R20）：§3.1 的八个状态是 `scenario` 主线的
/// 节点；`whatif` / `discover` 的不同边集（§7.3 的模式表）引入了 `consequence`
/// 与 `discover` 两个节点。这两个名字**在 §7.2 的工具阶段白名单与 §7.3 的模式
/// 路径表里已被当作阶段使用**，故补全它们是既有设计的必然结果，而非新增概念。
/// §3.1 的八个命名与语义**逐字未动**（R5）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    /// 剧本识别与信息收集。
    Intake,
    /// 公开数据搜集与参与者画像。
    Intel,
    /// GameSpec 构建。
    ModelBuild,
    /// 均衡枚举 + 精炼。
    Solve,
    /// 均衡选择（`scenario` 主线）。
    Select,
    /// 条件后果预测（`whatif` 主线：用户给定动作 d，回答"如果这么做会怎样"）。
    Consequence,
    /// 满意化策略发现（`discover` 主线：用户给满意标准，回答"哪些策略达标"）。
    Discover,
    /// 对策与敏感性。
    Recommend,
    /// 报告生成。
    Report,
    /// 已归档。
    Archived,
}

impl SessionPhase {
    /// 全部阶段的**规范顺序**（用于展示、序列化、日志排序）。
    ///
    /// ⚠️ 它**不是**跃迁判据：跃迁由 [`PhaseGraph`] 按模式裁定。任何用本数组
    /// 相邻性去判断"能否推进"的代码都是错的（`whatif` 里 `select` 不在路径上，
    /// `discover` 里 `select` 被 `discover` 替代）。
    pub const CANONICAL_ORDER: [SessionPhase; 10] = [
        SessionPhase::Intake,
        SessionPhase::Intel,
        SessionPhase::ModelBuild,
        SessionPhase::Solve,
        SessionPhase::Select,
        SessionPhase::Consequence,
        SessionPhase::Discover,
        SessionPhase::Recommend,
        SessionPhase::Report,
        SessionPhase::Archived,
    ];

    /// 阶段名（snake_case）。与 `shangyi-tools` 的阶段白名单字面量、
    /// 以及事件日志里的 `phase` 字段**共用同一套字面量**。
    pub fn as_str(self) -> &'static str {
        match self {
            SessionPhase::Intake => "intake",
            SessionPhase::Intel => "intel",
            SessionPhase::ModelBuild => "model_build",
            SessionPhase::Solve => "solve",
            SessionPhase::Select => "select",
            SessionPhase::Consequence => "consequence",
            SessionPhase::Discover => "discover",
            SessionPhase::Recommend => "recommend",
            SessionPhase::Report => "report",
            SessionPhase::Archived => "archived",
        }
    }

    /// 规范顺序下的编号。**仅**用于展示与稳定排序，不用于跃迁判定。
    pub fn canonical_index(self) -> usize {
        Self::CANONICAL_ORDER
            .iter()
            .position(|p| *p == self)
            .unwrap_or(0)
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::CANONICAL_ORDER.iter().copied().find(|p| p.as_str() == s)
    }
}

impl std::fmt::Display for SessionPhase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────── 产物 ───────────────────────────

/// 阶段产物种类（§7.3，含 R20 补全的两项）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// 案例简报（intake）。
    CaseBrief,
    /// 价值网图（intake，五向扫描）。
    ValueNetMap,
    /// 参与者画像（intel）。
    ParticipantProfiles,
    /// 情报摘要（intel）。
    IntelDigest,
    /// 已过 checkpoint② 的 GameSpec（model_build）。
    GameSpecValidated,
    /// 均衡解集（solve）。
    EquilibriumSet,
    /// 均衡选择输出（select）。
    SelectionOutput,
    /// 条件后果报告（consequence，whatif）。
    ConsequenceReport,
    /// 满意策略发现报告（discover）。
    DiscoveryReport,
    /// 建议包（recommend）。
    RecommendationPack,
    /// 报告束（report）。
    ReportBundle,
    /// **已冻结的案例预测**（archived）——归档的交付物。
    ///
    /// # 为什么必须有它（run31 实测）
    ///
    /// `archived` 阶段的 `produces` 此前是**空集**，而它的指引写着
    /// "本阶段必须以 `case_store_write` 收尾"。于是：
    /// - 空集 ⇒ "产物齐备即推进"的判据永远成立 ⇒ 阶段机无终止条件
    /// - 而 runner 当时一到 archived 就返回 ⇒ `case_store_write`
    ///   **一次都没被调用** ⇒ 案例从不入库 ⇒ 校准闭环拿不到样本
    ///
    /// 两者的组合是矛盾的：**要么承认 archived 是终点标记（那就别要求
    /// 归档），要么承认它有交付物（那就得有产物、有终止条件）**。
    /// 设计文档 §7.2 说归档是回测闭环的起点，故取后者。
    CaseArchived,
}

impl ArtifactKind {
    /// 产物键——与 [`PhaseMachine`] 的产物表、`consultation::artifact_status`
    /// 的键**同名**（一份字面量，一处定义）。
    pub fn key(self) -> &'static str {
        match self {
            ArtifactKind::CaseBrief => "case_brief",
            ArtifactKind::ValueNetMap => "value_net_map",
            ArtifactKind::ParticipantProfiles => "participant_profiles",
            ArtifactKind::IntelDigest => "intel_digest",
            ArtifactKind::GameSpecValidated => "gamespec",
            ArtifactKind::EquilibriumSet => "equilibrium_set",
            ArtifactKind::SelectionOutput => "equilibrium_distribution",
            ArtifactKind::ConsequenceReport => "consequence_report",
            ArtifactKind::DiscoveryReport => "discovery_report",
            ArtifactKind::RecommendationPack => "recommendation",
            // ⚠️ 历史遗留：`report_model` 是一个**内部字段名**
            // （`DomainState::report_model`），不该当持久化键。
            // 改成 `report_bundle` 需要同时改 `consultation.rs` 的三处引用
            // 与已落盘案例的兼容性，故**本轮不改**——但在这里如实记录，
            // 而不是让它看起来像个正常命名。
            ArtifactKind::ReportBundle => "report_model",
            ArtifactKind::CaseArchived => "case_archived",
        }
    }

    pub fn as_str(self) -> &'static str {
        self.key()
    }

    /// **人类可读名**（进错误信息与报告）。
    ///
    /// # 为什么需要它与 `key()` 分开（实测：错误信息泄漏了内部名）
    ///
    /// 一次脚本化 E2E 里，循环报出：
    ///
    /// > 模型在本阶段（`report`）一次工具调用都没有发起，但该阶段要求产出 report_model
    ///
    /// `report_model` 是 `DomainState` 的**字段名**——模型看到它做不了任何事：
    /// 它既不知道这是"报告束"，也不知道该调哪个工具产出它。
    /// 而这条错误**本来就是给模型看的**（它要据此决定下一步）。
    ///
    /// 故：错误信息用本函数（人话），持久化用 `key()`（稳定标识）。
    /// 两者混用会把内部实现泄漏到不该看见它的读者面前。
    pub fn display_name(self) -> &'static str {
        match self {
            ArtifactKind::CaseBrief => "案例简报",
            ArtifactKind::ValueNetMap => "价值网图",
            ArtifactKind::ParticipantProfiles => "参与者画像",
            ArtifactKind::IntelDigest => "情报摘要",
            ArtifactKind::GameSpecValidated => "通过校验的博弈规格",
            ArtifactKind::EquilibriumSet => "均衡解集",
            ArtifactKind::SelectionOutput => "均衡选择输出",
            ArtifactKind::ConsequenceReport => "条件后果报告",
            ArtifactKind::DiscoveryReport => "满意策略报告",
            ArtifactKind::RecommendationPack => "建议包",
            ArtifactKind::ReportBundle => "报告束",
            ArtifactKind::CaseArchived => "已冻结的案例预测",
        }
    }
}

// ────────────────────────── checkpoint ──────────────────────────

/// 三个 checkpoint 的标识（§7.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointId {
    /// checkpoint①：intel 末的事实确认。
    Cp1,
    /// checkpoint②：model_build 末的硬假设确认（铁律 2 的落点）。
    Cp2,
    /// checkpoint③：结果预览（`scenario` 在 select 末，`whatif` 在 consequence 末，
    /// `discover` 在 discover 末——位置由模式决定，这是 cp3 唯一的模式差异）。
    Cp3,
}

impl CheckpointId {
    pub fn as_str(self) -> &'static str {
        match self {
            CheckpointId::Cp1 => "cp1",
            CheckpointId::Cp2 => "cp2",
            CheckpointId::Cp3 => "cp3",
        }
    }

    /// 该 checkpoint 确认的内容类别（§7.6 的三类）。
    pub fn kind(self) -> CheckpointKind {
        match self {
            CheckpointId::Cp1 => CheckpointKind::FactConfirm,
            CheckpointId::Cp2 => CheckpointKind::AssumptionConfirm,
            CheckpointId::Cp3 => CheckpointKind::ResultPreview,
        }
    }
}

impl std::fmt::Display for CheckpointId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind {
    /// 事实确认（摘取结果、画像证据）。
    FactConfirm,
    /// 硬假设确认（payoff 参数、对手估计 + 来源）。
    AssumptionConfirm,
    /// 结果预览（均衡概率分布 + 判据驱动）。
    ResultPreview,
}

/// 待用户确认的一项。
///
/// `proposed_value` 是**代码算出的**待确认值（不是模型措辞）；
/// `source` 是该值从哪来——checkpoint② 的每一条硬假设都必须有来源（铁律 2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointItem {
    pub id: String,
    pub content: String,
    /// `None` = 无来源。**cp2 的硬假设项不接受 `None`**（见
    /// [`CheckpointRequest::validate`]）。
    pub source: Option<String>,
    pub proposed_value: Option<String>,
}

/// 一次 checkpoint 提问（§7.6 步骤 1 的载荷）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointRequest {
    pub id: CheckpointId,
    pub phase: SessionPhase,
    pub items: Vec<CheckpointItem>,
    /// 用户的三操作：确认 / 修改 / 追问（§8.3）。
    pub revision_requested: bool,
}

impl CheckpointRequest {
    /// 校验提问件本身是否合法。
    ///
    /// **为什么在这里死磕来源**：铁律 2 要求"关键假设必须过 checkpoint"，
    /// 而一条**没有来源**的假设即使被用户点了"确认"，确认的也是一个凭空的值——
    /// 那等于把铁律 2 降级成一句愿望。故 cp2 的每一项都**必须**带来源，
    /// 缺失即拒绝构造（不变量②：失败即拒绝，绝不静默降级）。
    pub fn validate(&self) -> Result<(), PhaseGateError> {
        if self.items.is_empty() {
            return Err(PhaseGateError::EmptyCheckpoint(self.id));
        }
        if matches!(self.id.kind(), CheckpointKind::AssumptionConfirm) {
            let missing: Vec<&str> = self
                .items
                .iter()
                .filter(|i| i.source.as_deref().map(str::trim).unwrap_or("").is_empty())
                .map(|i| i.id.as_str())
                .collect();
            if !missing.is_empty() {
                return Err(PhaseGateError::AssumptionWithoutSource {
                    checkpoint: self.id,
                    items: missing.iter().map(|s| s.to_string()).collect(),
                });
            }
        }
        Ok(())
    }
}

/// checkpoint 的结算状态。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CheckpointStatus {
    /// 已打开、等待用户作答。**未结算 ⇒ 循环不推进**（§7.6）。
    Open {
        items: Vec<CheckpointItem>,
    },
    /// 用户确认。
    Approved {
        /// 确认时该阶段产物的指纹。用于检测"批准后产物被改"（§7.7 的
        /// `PhaseApproved { fingerprint }`）——批准必须绑定到**被批准的那份内容**。
        fingerprint: String,
        /// 作答者（审计字段；`user_actor`）。
        by: String,
        at: String,
    },
    /// 用户拒绝（选"修改"）。这不是终止，而是"回到该阶段重做"的信号（§7.3）。
    Rejected {
        reason: String,
        at: String,
    },
}

impl CheckpointStatus {
    pub fn is_approved(&self) -> bool {
        matches!(self, CheckpointStatus::Approved { .. })
    }
    pub fn is_open(&self) -> bool {
        matches!(self, CheckpointStatus::Open { .. })
    }
}

// ─────────────────────────── 错误 ───────────────────────────

/// 阶段闸门错误（§7.3 的 `PhaseGateError`，含 R20 补全）。
#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum PhaseGateError {
    #[error("任务范围：{0}")]
    TaskScope(String),
    #[error("阶段 {phase} 的 checkpoint {checkpoint} 尚未结算；请调用 `checkpoint` 工具向用户确认后继续")]
    CheckpointPending {
        phase: SessionPhase,
        checkpoint: CheckpointId,
    },
    #[error("阶段 {phase} 的 checkpoint {checkpoint} 被用户拒绝（{reason}）；该阶段须修订后重做")]
    CheckpointRejected {
        phase: SessionPhase,
        checkpoint: CheckpointId,
        reason: String,
    },
    #[error("阶段 {phase} 缺少必需产物：{}；请先通过对应工具产出", artifact_keys(.missing))]
    MissingArtifacts {
        phase: SessionPhase,
        missing: Vec<ArtifactKind>,
    },
    #[error("阶段 {0} 是本剧本的终态，无法继续推进")]
    Terminal(SessionPhase),
    #[error("非法阶段跃迁：{from} → {to}（目标不是当前阶段的直接后继；本模式的后继为 {allowed:?}）")]
    IllegalTransition {
        from: SessionPhase,
        to: SessionPhase,
        allowed: Vec<SessionPhase>,
    },
    #[error("阶段 {0} 不在本模式的路径上")]
    NotOnPath(SessionPhase),
    #[error("checkpoint {0} 的提问件为空——空提问件不得被结算为“已确认”")]
    EmptyCheckpoint(CheckpointId),
    #[error("checkpoint {checkpoint} 的以下硬假设项缺少来源：{items:?}（铁律 2：硬假设须可溯源）")]
    AssumptionWithoutSource {
        checkpoint: CheckpointId,
        items: Vec<String>,
    },
    #[error("checkpoint {checkpoint} 未打开，无法结算")]
    CheckpointNotOpen { checkpoint: CheckpointId },
    #[error("checkpoint {checkpoint} 已结算（{current}），不得重复结算")]
    CheckpointAlreadySettled {
        checkpoint: CheckpointId,
        current: &'static str,
    },
    #[error(
        "阶段 {phase} 的产物在 checkpoint {checkpoint} 批准后被修改（批准指纹 {approved}，当前 {current}）；\
         须重新确认——批准绑定的是被批准的那份内容"
    )]
    ArtifactChangedAfterApproval {
        phase: SessionPhase,
        checkpoint: CheckpointId,
        approved: String,
        current: String,
    },
}

// ─────────────────────────── 阶段图 ───────────────────────────

/// 模式感知的阶段图。
///
/// 构造即校验：三条路径必须首尾相接、节点不重复、checkpoint 落在路径节点上。
/// 图是**数据**（当然也可以是剧本 YAML 的数据，见 §7.3 的"阶段转移由剧本驱动"），
/// 但在 v0.2 的三模式范围内，把它固化为代码常量能让"模式与边集"的关系可被
/// 穷举测试；未来接入剧本自定义路径时，本结构即 YAML 的反序列化目标。
#[derive(Debug, Clone, PartialEq)]
pub struct PhaseGraph {
    mode: ConsultationMode,
    /// 主路径（含可选节点），按顺序。
    path: Vec<SessionPhase>,
    /// 可跳过的节点（`whatif` 的 `solve`）：可被下一个非可选节点绕过。
    optional: BTreeSet<SessionPhase>,
    /// 阶段 → 该阶段结束时需结算的 checkpoint。
    checkpoints: BTreeMap<SessionPhase, CheckpointId>,
    /// 有向边（由 `path` 与 `optional` 派生，构造时算好）。
    edges: BTreeMap<SessionPhase, Vec<SessionPhase>>,
}

impl PhaseGraph {
    /// 取某模式的阶段图（§7.3 的模式表，逐行对应）。
    pub fn for_mode(mode: ConsultationMode) -> Self {
        use SessionPhase::*;
        let (path, optional): (Vec<SessionPhase>, &[SessionPhase]) = match mode {
            ConsultationMode::Scenario => (
                vec![Intake, Intel, ModelBuild, Solve, Select, Recommend, Report, Archived],
                &[],
            ),
            // whatif：用户给定动作 d。solve 仅在需要基线均衡时执行；
            // consequence 取代 select 的位置（那条路径上不做均衡选择）。
            ConsultationMode::Whatif => (
                vec![Intake, Intel, ModelBuild, Solve, Consequence, Recommend, Report, Archived],
                &[Solve],
            ),
            // discover：select 被 discover 替代（候选评估内部复用 consequence 管线）。
            ConsultationMode::Discover => (
                vec![Intake, Intel, ModelBuild, Solve, Discover, Recommend, Report, Archived],
                &[],
            ),
        };

        // cp1/cp2 三个模式完全一致；cp3 落在该模式的"结果产出阶段"之后——
        // 这正是 §7.3 "cp3 前移到 consequence 之后"的那句话的实现。
        let cp3_phase = match mode {
            ConsultationMode::Scenario => Select,
            ConsultationMode::Whatif => Consequence,
            ConsultationMode::Discover => Discover,
        };
        let checkpoints = BTreeMap::from([
            (Intel, CheckpointId::Cp1),
            (ModelBuild, CheckpointId::Cp2),
            (cp3_phase, CheckpointId::Cp3),
        ]);

        let optional: BTreeSet<SessionPhase> = optional.iter().copied().collect();
        let edges = Self::derive_edges(&path, &optional);
        let g = Self {
            mode,
            path,
            optional,
            checkpoints,
            edges,
        };
        g.assert_well_formed();
        g
    }

    /// 由路径与可选节点派生边集：`p[i] → p[i+1]`；若 `p[i+1]` 可选，则额外有
    /// `p[i] → p[i+2]`（绕过）。
    fn derive_edges(
        path: &[SessionPhase],
        optional: &BTreeSet<SessionPhase>,
    ) -> BTreeMap<SessionPhase, Vec<SessionPhase>> {
        let mut edges: BTreeMap<SessionPhase, Vec<SessionPhase>> = BTreeMap::new();
        for w in path.windows(2) {
            edges.entry(w[0]).or_default().push(w[1]);
        }
        // 绕过边：optional 节点的后继，接回 optional 的前驱
        for (i, p) in path.iter().enumerate() {
            if optional.contains(p) {
                if let (Some(prev), Some(next)) = (
                    i.checked_sub(1).and_then(|j| path.get(j)),
                    path.get(i + 1),
                ) {
                    let e = edges.entry(*prev).or_default();
                    if !e.contains(next) {
                        e.push(*next);
                    }
                }
            }
        }
        edges
    }

    /// 构造后自检：图必须良构，否则**在此 panic**（这是编程错误，不是数据错误——
    /// `for_mode` 的三条路径是代码常量，不存在"用户给了坏路径"的情形）。
    fn assert_well_formed(&self) {
        assert!(!self.path.is_empty(), "阶段路径不得为空");
        let mut seen = BTreeSet::new();
        for p in &self.path {
            assert!(seen.insert(*p), "阶段路径出现重复节点：{p}");
        }
        assert_eq!(
            *self.path.last().unwrap(),
            SessionPhase::Archived,
            "每条路径都必须终止于 archived"
        );
        for (phase, cp) in &self.checkpoints {
            assert!(seen.contains(phase), "checkpoint {cp} 落在不在路径上的阶段 {phase}");
        }
        for (from, tos) in &self.edges {
            assert!(seen.contains(from), "边起点 {from} 不在路径上");
            for t in tos {
                assert!(seen.contains(t), "边终点 {t} 不在路径上");
            }
            assert!(!tos.is_empty(), "节点 {from} 有出边集合为空");
        }
        assert!(
            !self.edges.contains_key(&SessionPhase::Archived),
            "终态 archived 不得有后继"
        );
    }

    pub fn mode(&self) -> ConsultationMode {
        self.mode
    }

    /// 主路径（含可选节点）。
    pub fn path(&self) -> &[SessionPhase] {
        &self.path
    }

    /// 该模式路径上的节点（等价于 `path()`，语义化别名）。
    pub fn nodes(&self) -> &[SessionPhase] {
        &self.path
    }

    /// 阶段在本模式路径上的位置。`None` = 该阶段不在本模式路径上。
    ///
    /// **这是"更早/更晚"的唯一判据**（不是 `canonical_index`）：在 `whatif` 里
    /// `consequence` 位于 `solve` 之后，而在规范顺序里 `consequence`(5) 也排在
    /// `select`(4) 之后，两者恰好一致但**不是因为**同一个理由。用路径位置，
    /// 是因为它按定义就是该模式的先后关系。
    pub fn position(&self, phase: SessionPhase) -> Option<usize> {
        self.path.iter().position(|p| *p == phase)
    }

    /// 直接后继集合（`whatif` 的 `model_build` 有两个：`solve` 与 `consequence`）。
    pub fn successors(&self, phase: SessionPhase) -> Vec<SessionPhase> {
        self.edges.get(&phase).cloned().unwrap_or_default()
    }

    /// 主后继（走"不绕过"的那条边），供确定性推进使用。
    pub fn next(&self, phase: SessionPhase) -> Option<SessionPhase> {
        self.successors(phase).into_iter().next()
    }

    /// 从 `from` 走到 `to` 是否是一条合法边。
    pub fn is_edge(&self, from: SessionPhase, to: SessionPhase) -> bool {
        self.successors(from).contains(&to)
    }

    /// `to` 是否严格早于 `from`（本模式路径上的先后）。
    pub fn is_earlier(&self, to: SessionPhase, from: SessionPhase) -> bool {
        match (self.position(to), self.position(from)) {
            (Some(a), Some(b)) => a < b,
            _ => false,
        }
    }

    pub fn is_optional(&self, phase: SessionPhase) -> bool {
        self.optional.contains(&phase)
    }

    /// 阶段结束时需结算的 checkpoint。
    pub fn checkpoint_at(&self, phase: SessionPhase) -> Option<CheckpointId> {
        self.checkpoints.get(&phase).copied()
    }

    /// 该阶段产出的产物。
    ///
    /// `intake` 产出为空集是**有意的**：它的交付物（案例简报、价值网图）是
    /// 过程笔记而非可机检的产物——把闸门压在一个"没有引擎会产出它"的条目上，
    /// 只会让循环卡死在第一步。闸门从 `intel` 起生效（与 §7.3 的闸门示例一致）。
    pub fn produces(&self, phase: SessionPhase) -> &'static [ArtifactKind] {
        use ArtifactKind as A;
        match phase {
            SessionPhase::Intake => &[],
            SessionPhase::Intel => &[A::ParticipantProfiles],
            SessionPhase::ModelBuild => &[A::GameSpecValidated],
            SessionPhase::Solve => &[A::EquilibriumSet],
            SessionPhase::Select => &[A::SelectionOutput],
            SessionPhase::Consequence => &[A::ConsequenceReport],
            SessionPhase::Discover => &[A::DiscoveryReport],
            SessionPhase::Recommend => &[A::RecommendationPack],
            SessionPhase::Report => &[A::ReportBundle],
            // ⚠️ 此前是空集——那让 archived **没有终止条件**（见 `CaseArchived` 的说明）
            SessionPhase::Archived => &[A::CaseArchived],
        }
    }

    /// 本模式**必须产出产物**的阶段（供循环自检 / CLI 展示）。
    pub fn producing_phases(&self) -> Vec<SessionPhase> {
        self.path
            .iter()
            .copied()
            .filter(|p| !self.produces(*p).is_empty())
            .collect()
    }
}

// ─────────────────────────── 阶段机 ───────────────────────────

/// 阶段机：持有当前阶段、已登记产物、checkpoint 结算状态。
///
/// 阶段机**拥有**产物表，因而回退时由它自己清理下游产物（[`Self::rollback_to`]）——
/// 调用方不需要（也不应该）重新推导"该清哪些"，那正是差一错误的滋生地。
#[derive(Debug, Clone)]
pub struct PhaseMachine {
    graph: PhaseGraph,
    phase: SessionPhase,
    artifacts: BTreeMap<String, String>,
    checkpoints: BTreeMap<CheckpointId, CheckpointStatus>,
    task_scope: Option<TaskScope>,
    scoped_result: Option<ScopedTaskResult>,
}

impl PhaseMachine {
    /// 按模式新建（起点为 `intake`）。
    pub fn new(mode: ConsultationMode) -> Self {
        Self {
            graph: PhaseGraph::for_mode(mode),
            phase: SessionPhase::Intake,
            artifacts: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            task_scope: Some(TaskScope::Consultation),
            scoped_result: None,
        }
    }

    /// 用给定图新建（测试与未来"剧本自定义路径"的入口）。
    pub fn with_graph(graph: PhaseGraph) -> Self {
        let phase = graph.path[0];
        Self {
            graph,
            phase,
            artifacts: BTreeMap::new(),
            checkpoints: BTreeMap::new(),
            task_scope: Some(TaskScope::Consultation),
            scoped_result: None,
        }
    }

    pub fn graph(&self) -> &PhaseGraph {
        &self.graph
    }

    pub fn mode(&self) -> ConsultationMode {
        self.graph.mode()
    }

    pub fn phase(&self) -> SessionPhase {
        self.phase
    }

    /// Production intake routes from the user's request; established explicit
    /// consultation callers retain the original graph and validation gates.
    pub fn require_task_scope_selection(mut self) -> Self {
        assert_eq!(self.phase, SessionPhase::Intake);
        self.task_scope = None;
        self
    }

    pub fn task_scope(&self) -> Option<TaskScope> { self.task_scope }
    pub fn scoped_result(&self) -> Option<&ScopedTaskResult> { self.scoped_result.as_ref() }

    pub fn allows_task_tool(&self, tool: &str) -> bool {
        if tool == "begin_followup_task" {
            return self.scoped_result.is_some();
        }
        self.task_scope.map_or(matches!(tool, "select_task_scope" | "ask_user"), |scope| scope.allows_tool(tool))
    }

    pub fn select_task_scope(&mut self, scope: TaskScope) -> Result<(), PhaseGateError> {
        if scope==TaskScope::Conditional {
            return Err(PhaseGateError::TaskScope("conditional 是关键证据未取得时的明确交付，不是可绕过咨询的初始范围".into()));
        }
        if self.scoped_result.is_some() {
            return Err(PhaseGateError::TaskScope("当前任务已交付；若用户继续追问，请先调用 `begin_followup_task` 重开新一轮范围选择，再调用 `select_task_scope`".into()));
        }
        if self.phase != SessionPhase::Intake {
            return Err(PhaseGateError::TaskScope("只可在新任务 intake 选择范围；不能绕过已进入的咨询阶段".into()));
        }
        self.task_scope = Some(scope);
        Ok(())
    }

    pub fn finish_scoped_task(&mut self, result: ScopedTaskResult) -> Result<(), PhaseGateError> {
        if self.phase != SessionPhase::Intake || !result.scope.is_brief()
            || self.task_scope != Some(result.scope) || self.scoped_result.is_some() {
            return Err(PhaseGateError::TaskScope("只有当前资料或费用任务可以按范围交付；完整咨询仍须走全部阶段闸门".into()));
        }
        self.scoped_result = Some(result);
        Ok(())
    }

    pub fn begin_followup_task(&mut self) -> Result<(), PhaseGateError> {
        if self.scoped_result.is_none() {
            return Err(PhaseGateError::TaskScope("当前任务尚未交付，不能重置范围".into()));
        }
        if self.scoped_result.as_ref().is_some_and(|r|r.scope==TaskScope::Conditional) {
            self.phase=SessionPhase::Intake;
            self.artifacts.clear();self.checkpoints.clear();
        }
        // Evidence remains in the executor/history; old decision approvals do not carry forward.
        self.task_scope = None;
        self.scoped_result = None;
        Ok(())
    }

    pub fn finish_conditional_task(&mut self, result: ScopedTaskResult) -> Result<(), PhaseGateError> {
        if self.task_scope!=Some(TaskScope::Consultation)||self.scoped_result.is_some()||self.phase==SessionPhase::Archived {
            return Err(PhaseGateError::TaskScope("只有尚未完整归档的咨询可以明确交付条件分析".into()));
        }
        crate::conditional::validate_conditional_result(&result).map_err(PhaseGateError::TaskScope)?;
        self.task_scope=Some(TaskScope::Conditional);self.scoped_result=Some(result);Ok(())
    }

    /// An actual user change invalidates dependent decisions and their approvals.
    pub fn invalidate_user_dependent_artifacts(&mut self, kinds: &[ArtifactKind]) -> Option<SessionPhase> {
        let first=self.graph.path.iter().position(|phase|self.graph.produces(*phase).iter().any(|k|kinds.contains(k)));
        for kind in kinds {self.clear_artifact(*kind);}
        let first=first?;
        let target=self.graph.path[first];
        let later:Vec<_>=self.graph.path[first..].to_vec();
        for phase in later {
            for kind in self.graph.produces(phase) {self.artifacts.remove(kind.key());}
            if let Some(cp)=self.graph.checkpoint_at(phase){self.checkpoints.remove(&cp);}
        }
        if self.graph.position(self.phase).is_some_and(|current|current>first) {
            self.rollback_to(target).expect("target is an earlier phase on the current graph");Some(target)
        } else {None}
    }

    // ── 产物 ──

    /// 登记某产物。键取 [`ArtifactKind::key`]。
    pub fn record_artifact(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.artifacts.insert(key.into(), value.into());
    }

    /// 按产物种类登记（推荐入口——字面量只存在于 `ArtifactKind`）。
    pub fn record(&mut self, kind: ArtifactKind, value: impl Into<String>) {
        self.record_artifact(kind.key(), value);
    }

    pub fn artifact(&self, key: &str) -> Option<&str> {
        self.artifacts.get(key).map(|s| s.as_str())
    }

    pub fn has_artifact(&self, kind: ArtifactKind) -> bool {
        self.artifacts.contains_key(kind.key())
    }

    /// 清除某产物（回退时用；也供"产物被上游改动 ⇒ 下游作废"的场景）。
    pub fn clear_artifact(&mut self, kind: ArtifactKind) {
        self.artifacts.remove(kind.key());
    }

    /// 已登记产物的键集合（有序）。
    pub fn artifact_keys(&self) -> BTreeSet<&str> {
        self.artifacts.keys().map(|s| s.as_str()).collect()
    }

    /// 当前产物的指纹：对 `(键, 值)` 规范序做 FNV-1a 64。
    ///
    /// 用途单一但承重：checkpoint 批准时记录指纹，推进时比对，从而发现
    /// "批准之后产物又被改了"。没有它，"用户确认过"这句话就与具体内容脱钩。
    pub fn artifact_fingerprint(&self) -> String {
        let mut canonical = String::new();
        for (k, v) in &self.artifacts {
            canonical.push_str(k);
            canonical.push('\u{1f}');
            canonical.push_str(v);
            canonical.push('\u{1e}');
        }
        format!("{:016x}", fnv1a64(canonical.as_bytes()))
    }

    // ── checkpoint ──

    /// 打开一个 checkpoint（向用户提问）。提问件先经 [`CheckpointRequest::validate`]。
    ///
    /// 幂等性：已结算的 checkpoint 不得重开（要重开须先 [`Self::reopen_checkpoint`]，
    /// 后者会把状态退回 `Open` 并清空批准——即"重新确认"）。
    pub fn open_checkpoint(&mut self, req: CheckpointRequest) -> Result<(), PhaseGateError> {
        req.validate()?;
        // 提问件声明的阶段必须是当前阶段——否则可以给"未来的阶段"预先盖章
        if req.phase != self.phase {
            return Err(PhaseGateError::IllegalTransition {
                from: self.phase,
                to: req.phase,
                allowed: vec![self.phase],
            });
        }
        if let Some(existing) = self.checkpoints.get(&req.id) {
            return Err(PhaseGateError::CheckpointAlreadySettled {
                checkpoint: req.id,
                current: match existing {
                    CheckpointStatus::Open { .. } => "open",
                    CheckpointStatus::Approved { .. } => "approved",
                    CheckpointStatus::Rejected { .. } => "rejected",
                },
            });
        }
        self.checkpoints
            .insert(req.id, CheckpointStatus::Open { items: req.items });
        Ok(())
    }

    /// 结算 checkpoint。
    ///
    /// `Approved` 时**记录当前产物指纹**——批准因此绑定到被批准的那份内容。
    pub fn settle_checkpoint(
        &mut self,
        id: CheckpointId,
        status: CheckpointStatus,
    ) -> Result<(), PhaseGateError> {
        match self.checkpoints.get(&id) {
            None => return Err(PhaseGateError::CheckpointNotOpen { checkpoint: id }),
            Some(CheckpointStatus::Approved { .. }) => {
                return Err(PhaseGateError::CheckpointAlreadySettled {
                    checkpoint: id,
                    current: "approved",
                });
            }
            // 被拒绝后**不得直接翻成批准**：拒绝意味着"内容要改"，改完必须
            // 重新向用户提问（`reopen_checkpoint` → 再结算）。允许 Rejected→Approved
            // 会让"先拒绝、再偷偷批准"成为一条绕过用户确认的路径——那正是
            // 铁律 2 要防的事。故结算只接受**处于 Open**的 checkpoint。
            Some(CheckpointStatus::Rejected { .. }) => {
                return Err(PhaseGateError::CheckpointAlreadySettled {
                    checkpoint: id,
                    current: "rejected（须修订后重新提问）",
                });
            }
            Some(CheckpointStatus::Open { .. }) => {}
        }
        self.checkpoints.insert(id, status);
        Ok(())
    }

    pub fn checkpoint_status(&self, id: CheckpointId) -> Option<&CheckpointStatus> {
        self.checkpoints.get(&id)
    }

    /// Re-enter the current review after explicit user correction or explanation request.
    /// This never approves a checkpoint and cannot reopen an already-approved one.
    ///
    /// Returns cleared artifact wire keys so callers can keep executor state in sync.
    pub fn reconsider_current_checkpoint(&mut self, id: CheckpointId) -> Result<Vec<String>, PhaseGateError> {
        if self.graph.checkpoint_at(self.phase) != Some(id)
            || !matches!(self.checkpoints.get(&id), Some(CheckpointStatus::Open { .. } | CheckpointStatus::Rejected { .. }))
        {
            return Err(PhaseGateError::CheckpointNotOpen { checkpoint: id });
        }
        self.checkpoints.remove(&id);
        // The current proposed artifact is stale until the model rebuilds/re-presents it.
        let mut cleared = Vec::new();
        let kinds: Vec<_> = self.graph.produces(self.phase).iter().copied().collect();
        for kind in kinds {
            if self.artifacts.remove(kind.key()).is_some() {
                cleared.push(kind.key().to_string());
            }
        }
        Ok(cleared)
    }

    /// 重新打开已结算的 checkpoint（用户选"修改"后的"重新确认"路径）。
    pub fn reopen_checkpoint(
        &mut self,
        id: CheckpointId,
        items: Vec<CheckpointItem>,
    ) -> Result<(), PhaseGateError> {
        if items.is_empty() {
            return Err(PhaseGateError::EmptyCheckpoint(id));
        }
        self.checkpoints
            .insert(id, CheckpointStatus::Open { items });
        Ok(())
    }

    // ── 闸门 ──

    /// 当前阶段能否离开；不能则给出**可操作**的原因（缺哪些产物、哪个 checkpoint）。
    ///
    /// # 检查顺序：先产物、后 checkpoint（与 §7.4 的 step 级拒绝**相反**）
    ///
    /// 两个查询的受众不同，故最优顺序也不同：
    ///
    /// - **step 级拒绝**（§7.4 的 `PhaseGateHook`）：先查 checkpoint。它要拦住的是
    ///   "checkpoint 还没过，模型就想继续跑"——此时无论产物是否齐备都必须停。
    /// - **推进级闸门**（本函数）：先查产物。产物缺失时，**用户没有任何可确认的东西**；
    ///   若此时报"checkpoint 未结算"，模型会去调 `checkpoint` 工具，然后（正确地）
    ///   被 [`PhaseGateError::EmptyCheckpoint`] 拦住——一条死胡同。报"缺哪些产物"
    ///   则直接把模型引向唯一能解开局面的动作。
    pub fn gate_to_advance(&self) -> Result<SessionPhase, PhaseGateError> {
        if self.phase == SessionPhase::Intake && self.task_scope != Some(TaskScope::Consultation) {
            return Err(PhaseGateError::TaskScope(match self.task_scope {
                None => "先用 select_task_scope 根据用户实际请求选择范围，不能默认进入完整咨询",
                Some(_) => "当前仅整理资料或计算费用；完成后用 complete_task 交付，不推进到博弈阶段",
            }.into()));
        }
        // 1) 本阶段产物必须齐备（理由见上）
        let missing: Vec<ArtifactKind> = self
            .graph
            .produces(self.phase)
            .iter()
            .filter(|k| !self.artifacts.contains_key(k.key()))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(PhaseGateError::MissingArtifacts {
                phase: self.phase,
                missing,
            });
        }

        // 2) 当前阶段的 checkpoint 必须已结算且批准
        if let Some(cp) = self.graph.checkpoint_at(self.phase) {
            match self.checkpoints.get(&cp) {
                None | Some(CheckpointStatus::Open { .. }) => {
                    return Err(PhaseGateError::CheckpointPending {
                        phase: self.phase,
                        checkpoint: cp,
                    });
                }
                Some(CheckpointStatus::Rejected { reason, .. }) => {
                    return Err(PhaseGateError::CheckpointRejected {
                        phase: self.phase,
                        checkpoint: cp,
                        reason: reason.clone(),
                    });
                }
                Some(CheckpointStatus::Approved {
                    fingerprint,
                    ..
                }) => {
                    // 批准后产物被改 ⇒ 批准作废（批准绑定的是那份内容）
                    let current = self.artifact_fingerprint();
                    if fingerprint != &current {
                        return Err(PhaseGateError::ArtifactChangedAfterApproval {
                            phase: self.phase,
                            checkpoint: cp,
                            approved: fingerprint.clone(),
                            current,
                        });
                    }
                }
            }
        }

        // 3) 下一个阶段
        self.graph.next(self.phase).ok_or(PhaseGateError::Terminal(self.phase))
    }

    /// 推进到主后继（`whatif` 的 `model_build` 走 `solve`；要跳过 solve 用
    /// [`Self::advance_to`]）。
    pub fn advance(&mut self) -> Result<SessionPhase, PhaseGateError> {
        let next = self.gate_to_advance()?;
        self.phase = next;
        Ok(next)
    }

    /// 推进到指定的直接后继（用于走 `whatif` 的绕过边 `model_build → consequence`）。
    pub fn advance_to(&mut self, target: SessionPhase) -> Result<SessionPhase, PhaseGateError> {
        if !self.graph.is_edge(self.phase, target) {
            return Err(PhaseGateError::IllegalTransition {
                from: self.phase,
                to: target,
                allowed: self.graph.successors(self.phase),
            });
        }
        // 先按"主后继"过闸门（闸门只依赖当前阶段，与目标无关）
        let _ = self.gate_to_advance()?;
        self.phase = target;
        Ok(target)
    }

    /// 跳过当前可选阶段并直接前往其边后继（如 `whatif` 模式下从 `solve` 直入 `consequence`）。
    ///
    /// 安全不变量：仅允许当前阶段本身为可选节点（`is_optional`）、无自身 checkpoint，
    /// 且 `target` 是当前阶段的合法后继边。前置必选阶段（如 `model_build` 与 `cp2`）
    /// 在进入当前阶段前已通过闸门校验。
    pub fn skip_optional_to(&mut self, target: SessionPhase) -> Result<SessionPhase, PhaseGateError> {
        if !self.graph.is_optional(self.phase) || self.graph.checkpoint_at(self.phase).is_some() {
            return Err(PhaseGateError::IllegalTransition {
                from: self.phase,
                to: target,
                allowed: vec![],
            });
        }
        if !self.graph.is_edge(self.phase, target) {
            return Err(PhaseGateError::IllegalTransition {
                from: self.phase,
                to: target,
                allowed: self.graph.successors(self.phase),
            });
        }
        self.phase = target;
        Ok(target)
    }

    /// 回退到更早阶段，并**由阶段机自己**清理被撤销阶段的产物。
    ///
    /// 返回被清除的产物键（供调用方同步自己的字段——调用方不该重新推导边界）。
    ///
    /// 语义：回退到 `target` = 撤销 `target` **之后**的工作，保留 `target` 自己的
    /// 产物。判据是路径位置严格大于 `target`（**严格**：`<=` 会多清一级，
    /// 这正是历史上 `rollback_clears_downstream_artifacts` 暴露过的差一错误）。
    ///
    /// 回退同时**作废被撤销阶段之后的 checkpoint 结算**：批准绑定的是那份内容，
    /// 内容被撤销后批准不得继续有效。
    pub fn rollback_to(&mut self, target: SessionPhase) -> Result<Vec<String>, PhaseGateError> {
        let target_pos = self
            .graph
            .position(target)
            .ok_or(PhaseGateError::NotOnPath(target))?;
        let current_pos = self.graph.position(self.phase).ok_or(PhaseGateError::NotOnPath(self.phase))?;
        if target_pos >= current_pos {
            return Err(PhaseGateError::IllegalTransition {
                from: self.phase,
                to: target,
                allowed: self.graph.path[..current_pos].to_vec(),
            });
        }

        let mut cleared = Vec::new();
        for (i, phase) in self.graph.path.iter().enumerate() {
            if i > target_pos {
                for kind in self.graph.produces(*phase) {
                    if self.artifacts.remove(kind.key()).is_some() {
                        cleared.push(kind.key().to_string());
                    }
                }
            }
        }
        // 被撤销阶段上的 checkpoint 结算作废（重新确认）
        let revoked: Vec<CheckpointId> = self
            .graph
            .path
            .iter()
            .enumerate()
            .filter(|(i, _)| *i > target_pos)
            .filter_map(|(_, p)| self.graph.checkpoint_at(*p))
            .collect();
        for cp in revoked {
            self.checkpoints.remove(&cp);
        }

        self.phase = target;
        Ok(cleared)
    }
}

/// 把产物列表渲染成**产物键**（wire 名）列表。
///
/// # 为什么不能用 `{missing:?}`
///
/// `{:?}` 会打印 Rust 变体名 `[ParticipantProfiles]`，而这条消息的读者是**模型**
/// ——它要按 wire 名去调工具（`participant_profile_update` 产出
/// `participant_profiles`）。给它一个代码里的类型名等于给了一个查不到的名字。
/// 类型安全留在内部（字段仍是 `ArtifactKind`），对外一律用键。
fn artifact_keys(kinds: &[ArtifactKind]) -> String {
    kinds
        .iter()
        .map(|k| k.key())
        .collect::<Vec<_>>()
        .join("、")
}

/// FNV-1a 64：无依赖、确定性、跨进程稳定（`DefaultHasher` 不保证跨版本稳定，
/// 而指纹要写进事件日志并被后续比对，故不用它）。**不是**密码学哈希——
/// 这里只需要"内容变了指纹就变"的碰撞抵抗，不需要抗伪造。
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod conditional_delivery_tests {
    use super::*;
    use crate::conditional::{ConditionalSnapshot,ConditionalTrigger,EvidenceGap,EvidenceGapReason,build_conditional_result};
    fn advance_ready(machine:&mut PhaseMachine) {
        for kind in machine.graph().produces(machine.phase()) {machine.record(*kind,kind.key());}
        if let Some(cp)=machine.graph().checkpoint_at(machine.phase()) {
            machine.open_checkpoint(CheckpointRequest{id:cp,phase:machine.phase(),items:vec![CheckpointItem{id:"fixture".into(),content:"fixture review".into(),source:Some("fixture source".into()),proposed_value:None}],revision_requested:false}).unwrap();
            machine.settle_checkpoint(cp,CheckpointStatus::Approved {fingerprint:machine.artifact_fingerprint(),by:"fixture-user".into(),at:"fixture".into()}).unwrap();
        }
        machine.advance().unwrap();
    }
    fn result()->ScopedTaskResult {
        build_conditional_result(&ConditionalSnapshot {question:"如何比较续租与搬店？".into(),gaps:vec![EvidenceGap {
            reason:EvidenceGapReason::UnknownOpponentPayoff,participant_id:Some("landlord".into()),profile:None,detail:"房东外租完整收益未取得".into()}],..Default::default()},ConditionalTrigger::ExplicitRequest).unwrap()
    }
    #[test]
    fn conditional_delivery_is_partial_and_does_not_pass_game_or_approval_gates() {
        let mut m=PhaseMachine::new(ConsultationMode::Scenario);
        assert!(m.select_task_scope(TaskScope::Conditional).is_err());
        advance_ready(&mut m);advance_ready(&mut m);assert_eq!(m.phase(),SessionPhase::ModelBuild);
        assert!(m.finish_scoped_task(result()).is_err());
        m.finish_conditional_task(result()).unwrap();
        assert_eq!(m.phase(),SessionPhase::ModelBuild);assert!(!m.scoped_result().unwrap().is_complete());
        assert!(!m.has_artifact(ArtifactKind::GameSpecValidated));assert!(!m.has_artifact(ArtifactKind::EquilibriumSet));
        assert!(!m.has_artifact(ArtifactKind::CaseArchived));assert!(m.checkpoint_status(CheckpointId::Cp2).is_none());
        m.begin_followup_task().unwrap();
        assert_eq!(m.phase(),SessionPhase::Intake);assert_eq!(m.task_scope(),None);
        assert!(m.artifact_keys().is_empty());assert!(m.checkpoint_status(CheckpointId::Cp1).is_none());
    }
    #[test]
    fn fresh_user_limits_revoke_old_choices_and_confirmation_before_recommendation_can_continue() {
        let mut m=PhaseMachine::new(ConsultationMode::Scenario);
        while m.phase()!=SessionPhase::Report {advance_ready(&mut m);}
        m.record(ArtifactKind::ReportBundle,"old-report");
        assert_eq!(m.invalidate_user_dependent_artifacts(&[ArtifactKind::SelectionOutput,ArtifactKind::RecommendationPack,ArtifactKind::ReportBundle]),Some(SessionPhase::Select));
        assert!(m.has_artifact(ArtifactKind::GameSpecValidated));assert!(m.has_artifact(ArtifactKind::EquilibriumSet));
        assert!(!m.has_artifact(ArtifactKind::SelectionOutput));assert!(!m.has_artifact(ArtifactKind::RecommendationPack));
        assert!(!m.has_artifact(ArtifactKind::ReportBundle));assert!(m.checkpoint_status(CheckpointId::Cp3).is_none());
        assert!(m.advance().is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ConsultationMode::{Discover, Scenario, Whatif};

    fn settle_ok(m: &mut PhaseMachine, cp: CheckpointId) {
        m.settle_checkpoint(
            cp,
            CheckpointStatus::Approved {
                fingerprint: m.artifact_fingerprint(),
                by: "test".into(),
                at: "2026-01-01T00:00:00Z".into(),
            },
        )
        .unwrap();
    }

    // ── 路径与边集 ──

    #[test]
    fn three_modes_have_distinct_paths() {
        let s: Vec<&str> = PhaseGraph::for_mode(Scenario).path().iter().map(|p| p.as_str()).collect();
        let w: Vec<&str> = PhaseGraph::for_mode(Whatif).path().iter().map(|p| p.as_str()).collect();
        let d: Vec<&str> = PhaseGraph::for_mode(Discover).path().iter().map(|p| p.as_str()).collect();

        assert_eq!(
            s,
            ["intake", "intel", "model_build", "solve", "select", "recommend", "report", "archived"]
        );
        assert_eq!(
            w,
            ["intake", "intel", "model_build", "solve", "consequence", "recommend", "report", "archived"]
        );
        assert_eq!(
            d,
            ["intake", "intel", "model_build", "solve", "discover", "recommend", "report", "archived"]
        );
        // 三条路径两两不同——旧实现里三者返回同一个数组，这个断言就是它的回归测试
        assert_ne!(s, w);
        assert_ne!(s, d);
        assert_ne!(w, d);
    }

    #[test]
    fn whatif_has_no_select_and_discover_replaces_it() {
        let w = PhaseGraph::for_mode(Whatif);
        assert_eq!(w.position(SessionPhase::Select), None, "whatif 路径上不得有 select");
        assert!(w.position(SessionPhase::Consequence).is_some());

        let d = PhaseGraph::for_mode(Discover);
        assert_eq!(d.position(SessionPhase::Select), None, "discover 模式下 select 被替代");
        assert!(d.position(SessionPhase::Discover).is_some());

        // scenario 反过来：必须有 select、不得有 consequence/discover
        let s = PhaseGraph::for_mode(Scenario);
        assert!(s.position(SessionPhase::Select).is_some());
        assert_eq!(s.position(SessionPhase::Consequence), None);
        assert_eq!(s.position(SessionPhase::Discover), None);
    }

    #[test]
    fn whatif_solve_is_optional_with_bypass_edge() {
        let g = PhaseGraph::for_mode(Whatif);
        assert!(g.is_optional(SessionPhase::Solve));
        assert_eq!(
            g.successors(SessionPhase::ModelBuild),
            vec![SessionPhase::Solve, SessionPhase::Consequence],
            "model_build 有两个后继：solve 与（绕过 solve 的）consequence"
        );
        // 确定性推进仍走 solve
        assert_eq!(g.next(SessionPhase::ModelBuild), Some(SessionPhase::Solve));
        // 其他模式无绕过边
        assert_eq!(PhaseGraph::for_mode(Scenario).successors(SessionPhase::ModelBuild).len(), 1);
    }

    #[test]
    fn cp3_lands_on_mode_specific_phase() {
        assert_eq!(PhaseGraph::for_mode(Scenario).checkpoint_at(SessionPhase::Select), Some(CheckpointId::Cp3));
        assert_eq!(PhaseGraph::for_mode(Whatif).checkpoint_at(SessionPhase::Consequence), Some(CheckpointId::Cp3));
        assert_eq!(PhaseGraph::for_mode(Discover).checkpoint_at(SessionPhase::Discover), Some(CheckpointId::Cp3));
        // cp1/cp2 三模式一致
        for mode in [Scenario, Whatif, Discover] {
            let g = PhaseGraph::for_mode(mode);
            assert_eq!(g.checkpoint_at(SessionPhase::Intel), Some(CheckpointId::Cp1));
            assert_eq!(g.checkpoint_at(SessionPhase::ModelBuild), Some(CheckpointId::Cp2));
        }
    }

    /// 每个模式都必须有一条从起点到终点的完整通路（良构性由 `assert_well_formed`
    /// 在构造时保证；此处验证它确实被调用了）。
    #[test]
    fn every_mode_reaches_archived() {
        for mode in [Scenario, Whatif, Discover] {
            let g = PhaseGraph::for_mode(mode);
            let mut cur = g.path()[0];
            let mut steps = 0;
            while let Some(n) = g.next(cur) {
                cur = n;
                steps += 1;
                assert!(steps < 32, "路径疑似成环");
            }
            assert_eq!(cur, SessionPhase::Archived);
        }
    }

    // ── 闸门 ──

    #[test]
    fn advance_blocked_by_missing_artifacts() {
        let mut m = PhaseMachine::new(Scenario);
        m.advance().unwrap(); // intake → intel（intake 产物为空集）
        assert_eq!(m.phase(), SessionPhase::Intel);

        // 离开 intel 需要画像
        let e = m.advance().unwrap_err();
        assert!(matches!(
            e,
            PhaseGateError::MissingArtifacts { phase: SessionPhase::Intel, ref missing }
                if missing == &vec![ArtifactKind::ParticipantProfiles]
        ));
        assert_eq!(m.phase(), SessionPhase::Intel, "闸门拒绝不得改变阶段");

        // 登记产物但 checkpoint① 未结算 ⇒ 仍然拒绝
        m.record(ArtifactKind::ParticipantProfiles, "profiles.json");
        let e = m.advance().unwrap_err();
        assert!(matches!(e, PhaseGateError::CheckpointPending { checkpoint: CheckpointId::Cp1, .. }));
    }

    #[test]
    fn checkpoint_must_be_opened_and_validated() {
        let mut m = PhaseMachine::new(Scenario);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "profiles.json");

        // 未打开就结算 ⇒ 拒绝
        let e = m
            .settle_checkpoint(
                CheckpointId::Cp1,
                CheckpointStatus::Approved { fingerprint: String::new(), by: "u".into(), at: "t".into() },
            )
            .unwrap_err();
        assert!(matches!(e, PhaseGateError::CheckpointNotOpen { .. }));

        // 空提问件 ⇒ 拒绝
        let e = m
            .open_checkpoint(CheckpointRequest {
                id: CheckpointId::Cp1,
                phase: SessionPhase::Intel,
                items: vec![],
                revision_requested: false,
            })
            .unwrap_err();
        assert!(matches!(e, PhaseGateError::EmptyCheckpoint(CheckpointId::Cp1)));

        // 给"未来的阶段"预先盖章 ⇒ 拒绝
        let e = m
            .open_checkpoint(CheckpointRequest {
                id: CheckpointId::Cp2,
                phase: SessionPhase::ModelBuild,
                items: vec![CheckpointItem {
                    id: "p".into(),
                    content: "c".into(),
                    source: Some("s".into()),
                    proposed_value: None,
                }],
                revision_requested: false,
            })
            .unwrap_err();
        assert!(matches!(e, PhaseGateError::IllegalTransition { .. }));

        // 正常打开 + 结算 + 推进
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "f1".into(),
                content: "对手是三家而不是两家".into(),
                source: Some("公开年报".into()),
                proposed_value: Some("3".into()),
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp1);
        assert_eq!(m.advance().unwrap(), SessionPhase::ModelBuild);
    }

    /// 铁律 2 的落点：cp2 的硬假设**无来源即拒绝构造**。
    #[test]
    fn assumption_checkpoint_rejects_unsourced_items() {
        let req = CheckpointRequest {
            id: CheckpointId::Cp2,
            phase: SessionPhase::ModelBuild,
            items: vec![
                CheckpointItem {
                    id: "payoff_A".into(),
                    content: "对手 A 在价格战中的收益".into(),
                    source: None, // ← 凭空的值
                    proposed_value: Some("120".into()),
                },
            ],
            revision_requested: false,
        };
        let e = req.validate().unwrap_err();
        assert!(matches!(e, PhaseGateError::AssumptionWithoutSource { .. }));

        // 空白来源同样不算来源（`Some("  ")` 不是来源）
        let mut req2 = req.clone();
        req2.items[0].source = Some("   ".into());
        assert!(matches!(
            req2.validate().unwrap_err(),
            PhaseGateError::AssumptionWithoutSource { .. }
        ));

        // 事实确认 checkcp1 不要求来源（事实可能来自用户口述）
        let mut req3 = req.clone();
        req3.id = CheckpointId::Cp1;
        assert!(req3.validate().is_ok());
    }

    #[test]
    fn rejected_checkpoint_blocks_with_reason() {
        let mut m = PhaseMachine::new(Scenario);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "profiles.json");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "f1".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        m.settle_checkpoint(
            CheckpointId::Cp1,
            CheckpointStatus::Rejected { reason: "第二条事实不对".into(), at: "t".into() },
        )
        .unwrap();

        let e = m.advance().unwrap_err();
        match e {
            PhaseGateError::CheckpointRejected { reason, .. } => assert_eq!(reason, "第二条事实不对"),
            other => panic!("期望 CheckpointRejected，得到 {other:?}"),
        }
        assert_eq!(m.phase(), SessionPhase::Intel, "拒绝不是终止，而是留在原阶段重做");
    }

    /// 批准绑定到**被批准的那份内容**：批准后改产物 ⇒ 批准作废。
    #[test]
    fn artifact_change_after_approval_revokes_it() {
        let mut m = PhaseMachine::new(Scenario);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "v1");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "f1".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp1);

        // 批准后把画像改掉（模拟"上游数据被修正但没重新确认"）
        m.record(ArtifactKind::ParticipantProfiles, "v2");
        let e = m.advance().unwrap_err();
        assert!(matches!(e, PhaseGateError::ArtifactChangedAfterApproval { .. }));

        // 重新确认后可推进
        m.reopen_checkpoint(CheckpointId::Cp1, vec![CheckpointItem {
            id: "f1".into(),
            content: "c".into(),
            source: Some("s".into()),
            proposed_value: None,
        }])
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp1);
        assert_eq!(m.advance().unwrap(), SessionPhase::ModelBuild);
    }

    #[test]
    fn terminal_phase_cannot_advance() {
        let mut m = PhaseMachine::new(Scenario);
        // 直接推到 archived（逐阶段补产物与确认）
        let steps: [(SessionPhase, ArtifactKind, Option<CheckpointId>); 6] = [
            (SessionPhase::Intel, ArtifactKind::ParticipantProfiles, Some(CheckpointId::Cp1)),
            (SessionPhase::ModelBuild, ArtifactKind::GameSpecValidated, Some(CheckpointId::Cp2)),
            (SessionPhase::Solve, ArtifactKind::EquilibriumSet, None),
            (SessionPhase::Select, ArtifactKind::SelectionOutput, Some(CheckpointId::Cp3)),
            (SessionPhase::Recommend, ArtifactKind::RecommendationPack, None),
            (SessionPhase::Report, ArtifactKind::ReportBundle, None),
        ];
        m.advance().unwrap(); // intake → intel
        for (phase, kind, cp) in steps {
            assert_eq!(m.phase(), phase);
            m.record(kind, "x");
            if let Some(cp) = cp {
                m.open_checkpoint(CheckpointRequest {
                    id: cp,
                    phase,
                    items: vec![CheckpointItem {
                        id: "i".into(),
                        content: "c".into(),
                        source: Some("s".into()),
                        proposed_value: None,
                    }],
                    revision_requested: false,
                })
                .unwrap();
                settle_ok(&mut m, cp);
            }
            m.advance().unwrap();
        }
        assert_eq!(m.phase(), SessionPhase::Archived);
        // ── archived 现在**有交付物**（`CaseArchived`），故语义变了 ──
        //
        // 早先它的 `produces` 是空集 ⇒ 推进直接报 `Terminal`。
        // 而那个空集制造了一对矛盾（run31 实测）：
        // - 空集 ⇒ 阶段机对 archived **没有终止条件**
        // - 而 runner 一到 archived 就 return ⇒ `case_store_write` 从未被调用
        //   ⇒ 案例从不入库 ⇒ 校准闭环拿不到样本
        //
        // 现在它有产物（由 `case_store_write` 登记）⇒ 会话在**归档完成后**
        // 才结束。故"未归档时的推进失败"报的是**缺产物**，不是 `Terminal`。
        let err = m.advance().unwrap_err();
        assert!(
            matches!(err, PhaseGateError::MissingArtifacts { .. }),
            "archived 未归档时应报「缺产物」（它现在有交付物），实得：{err:?}"
        );

        // 归档后：仍是路径末尾（`Terminal`）——这一条没变
        m.record(ArtifactKind::CaseArchived, "archived:1");
        // 注意 `advance` 会先过闸门；产物齐了之后才走到"无后继"
        let err2 = m.advance().unwrap_err();
        assert!(
            matches!(err2, PhaseGateError::Terminal(SessionPhase::Archived)),
            "归档后应报「终态」（路径末尾无后继），实得：{err2:?}"
        );
    }

    // ── whatif 的绕过路径 ──

    #[test]
    fn whatif_can_skip_solve_and_does_not_require_equilibrium() {
        let mut m = PhaseMachine::new(Whatif);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "p");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp1);
        m.advance().unwrap(); // → model_build
        m.record(ArtifactKind::GameSpecValidated, "g");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp2,
            phase: SessionPhase::ModelBuild,
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp2);

        // 绕过 solve：直接进 consequence，且**不需要**均衡解
        assert_eq!(m.advance_to(SessionPhase::Consequence).unwrap(), SessionPhase::Consequence);
        assert!(!m.has_artifact(ArtifactKind::EquilibriumSet));
        // consequence 的闸门只要求它自己的产物
        assert!(matches!(
            m.advance().unwrap_err(),
            PhaseGateError::MissingArtifacts { phase: SessionPhase::Consequence, .. }
        ));
        m.record(ArtifactKind::ConsequenceReport, "whatif.json");
        // cp3 在 consequence 上（whatif 的模式差异）
        assert!(matches!(
            m.advance().unwrap_err(),
            PhaseGateError::CheckpointPending { checkpoint: CheckpointId::Cp3, .. }
        ));
    }

    #[test]
    fn advance_to_a_non_successor_is_rejected() {
        let mut m = PhaseMachine::new(Scenario);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "p");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp1);
        // intel → report 不是边
        let e = m.advance_to(SessionPhase::Report).unwrap_err();
        assert!(matches!(e, PhaseGateError::IllegalTransition { .. }));
        assert_eq!(m.phase(), SessionPhase::Intel);
    }

    // ── 回退 ──

    /// 差一错误的回归测试（严格小于判据），且现在是**模式感知**的。
    #[test]
    fn rollback_clears_strictly_downstream_artifacts() {
        let mut m = PhaseMachine::new(Scenario);
        m.record(ArtifactKind::ParticipantProfiles, "p");
        m.record(ArtifactKind::GameSpecValidated, "g");
        m.record(ArtifactKind::EquilibriumSet, "e");
        // 推到 solve（阶段位置 3）
        m.phase = SessionPhase::Solve;

        // 回退到 model_build：保留 game_spec（它是 model_build 的产物），清 equilibrium_set
        let cleared = m.rollback_to(SessionPhase::ModelBuild).unwrap();
        assert_eq!(cleared, vec!["equilibrium_set".to_string()]);
        assert!(m.has_artifact(ArtifactKind::GameSpecValidated), "回退到 ModelBuild 必须保留其自身产物");
        assert!(!m.has_artifact(ArtifactKind::EquilibriumSet));
        assert!(m.has_artifact(ArtifactKind::ParticipantProfiles));
    }

    /// whatif 的回退边界与 scenario **不同**：consequence 之后是 recommend，
    /// 没有 select 可供误清；而 consequence 的产物在回退到 consequence 时保留。
    #[test]
    fn rollback_boundary_is_mode_specific() {
        let mut m = PhaseMachine::new(Whatif);
        m.record(ArtifactKind::GameSpecValidated, "g");
        m.record(ArtifactKind::ConsequenceReport, "w");
        m.record(ArtifactKind::RecommendationPack, "r");
        m.phase = SessionPhase::Recommend;

        let cleared = m.rollback_to(SessionPhase::Consequence).unwrap();
        assert_eq!(cleared, vec!["recommendation".to_string()]);
        assert!(m.has_artifact(ArtifactKind::ConsequenceReport), "consequence 是其回退目标的产物，须保留");
    }

    #[test]
    fn rollback_revokes_downstream_checkpoint_settlements() {
        let mut m = PhaseMachine::new(Scenario);
        m.advance().unwrap(); // → intel（在 intel 上才可打开 cp1）
        m.record(ArtifactKind::ParticipantProfiles, "p");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp1);

        // 前进到 model_build 并确认 cp2
        m.advance().unwrap();
        assert_eq!(m.phase(), SessionPhase::ModelBuild);
        m.record(ArtifactKind::GameSpecValidated, "g");
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp2,
            phase: SessionPhase::ModelBuild,
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        settle_ok(&mut m, CheckpointId::Cp2);
        m.rollback_to(SessionPhase::Intel).unwrap();
        assert!(m.checkpoint_status(CheckpointId::Cp2).is_none(), "被撤销阶段的批准必须作废");
        assert!(
            m.checkpoint_status(CheckpointId::Cp1).is_some_and(|s| s.is_approved()),
            "回退目标阶段自身的批准必须保留（用户没有再改 intel 的内容）"
        );
    }

    #[test]
    fn rollback_forward_or_same_is_rejected() {
        let mut m = PhaseMachine::new(Scenario);
        assert!(m.rollback_to(SessionPhase::Intel).is_err(), "回退到更晚阶段必须拒绝");
        assert!(m.rollback_to(SessionPhase::Intake).is_err(), "回退到自身必须拒绝");
        // 不在本模式路径上的阶段
        assert!(matches!(
            m.rollback_to(SessionPhase::Consequence).unwrap_err(),
            PhaseGateError::NotOnPath(SessionPhase::Consequence)
        ));
    }

    // ── 指纹 ──

    #[test]
    fn fingerprint_tracks_content_and_is_order_stable() {
        let mut a = PhaseMachine::new(Scenario);
        let mut b = PhaseMachine::new(Scenario);
        a.record(ArtifactKind::EquilibriumSet, "e1");
        a.record(ArtifactKind::ParticipantProfiles, "p1");
        b.record(ArtifactKind::ParticipantProfiles, "p1");
        b.record(ArtifactKind::EquilibriumSet, "e1");
        assert_eq!(a.artifact_fingerprint(), b.artifact_fingerprint(), "指纹与登记顺序无关");

        b.record(ArtifactKind::EquilibriumSet, "e2");
        assert_ne!(a.artifact_fingerprint(), b.artifact_fingerprint());
        assert_eq!(a.artifact_fingerprint().len(), 16);
    }

    // ── 与工具白名单的名字一致性 ──

    /// 阶段名的**唯一**来源是 `SessionPhase::as_str`；`shangyi-tools` 的注册表用
    /// 同名字面量做白名单。这条测试保证两边不会悄悄漂移。
    #[test]
    fn phase_names_match_tool_registry_whitelist_vocabulary() {
        let mut names: Vec<&str> = SessionPhase::CANONICAL_ORDER.iter().map(|p| p.as_str()).collect();
        names.sort_unstable();
        let mut expected = vec![
            "archived", "consequence", "discover", "intake", "intel", "model_build",
            "recommend", "report", "select", "solve",
        ];
        expected.sort_unstable();
        assert_eq!(names, expected);
        // parse 往返
        for p in SessionPhase::CANONICAL_ORDER {
            assert_eq!(SessionPhase::parse(p.as_str()), Some(p));
        }
        assert_eq!(SessionPhase::parse("not_a_phase"), None);
    }

    #[test]
    fn artifact_keys_are_unique() {
        use ArtifactKind::*;
        let all = [
            CaseBrief, ValueNetMap, ParticipantProfiles, IntelDigest, GameSpecValidated,
            EquilibriumSet, SelectionOutput, ConsequenceReport, DiscoveryReport,
            RecommendationPack, ReportBundle,
        ];
        let keys: BTreeSet<&str> = all.iter().map(|k| k.key()).collect();
        assert_eq!(keys.len(), all.len(), "产物键必须互不相同");
    }
}

//! 咨询流程接线（设计文档 §2.2 的一次咨询生命周期）。
//!
//! # 本模块的职责
//!
//! 把 L0–L2 的引擎串成**可运行的咨询流程**。在此之前各引擎是独立的库；
//! 本模块定义"哪个阶段调哪个引擎、产物如何传递、checkpoint 在哪停下"。
//!
//! # 为何接线的诚实性很重要
//!
//! 一个"看起来能跑"的接线最容易犯的错是**假装完成了引擎调用**——
//! 例如把 phase 推进了但没真正求解。故本模块的设计原则是：
//!
//! 1. **阶段推进必须伴随真实产物**：`PhaseMachine::advance` 已在类型上要求
//!    各阶段交付产物（见 §7.3），本模块负责真的产出它们
//! 2. **引擎失败即中止**：不用默认值兜底，不"继续推进看看"
//! 3. **明确标注骨架部分**：尚未实现 LLM 交互的环节（如 intake 提问）
//!    以类型化错误上报，**不伪造**用户回答

use std::collections::BTreeMap;

use shangyi_consequence::TripleFilter;
use shangyi_gamespec::GameSpec;
use shangyi_intel::ParticipantProfile;
use shangyi_playbooks::Playbook;
use shangyi_report::Report;
use shangyi_selection::{Confidence, SelectorWeights};
use shangyi_tools::ToolRegistry;

use crate::phase_graph::{
    ArtifactKind, PhaseGateError, PhaseGraph, PhaseMachine, SessionPhase,
};

/// 咨询模式（与 `shangyi-cli::Mode` 对应；此处独立定义以避免 CLI 依赖）。
///
/// **为何不直接复用 `shangyi-cli::Mode`**：`shangyi-core` 是 L3 接线层，
/// 不应依赖 CLI（CLI 是交付层）。两者语义一致但分属不同层——
/// 这是 §6.3 边界规则的体现。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsultationMode {
    Scenario,
    Whatif,
    Discover,
}

impl ConsultationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsultationMode::Scenario => "scenario",
            ConsultationMode::Whatif => "whatif",
            ConsultationMode::Discover => "discover",
        }
    }

    /// 该模式的主工具（用于自检与报告标注）。
    /// 该模式**路径上各阶段的交付工具**（按阶段顺序）。
    ///
    /// 用途：诊断"剧本不支持该模式"时，一次列出**全部**缺失的工具，
    /// 而不是只报第一个——后者会让用户改一次、再撞一次。
    pub fn primary_tool_names(self) -> Vec<&'static str> {
        // 与 `PhaseGraph::produces` 一一对应：每个有产物的阶段都有产出它的工具
        let common = ["participant_profile_update", "gamespec_build", "solve_equilibria"];
        match self {
            ConsultationMode::Scenario => {
                let mut v = common.to_vec();
                v.extend(["select_fuse", "recommend_build", "report_render"]);
                v
            }
            ConsultationMode::Whatif => {
                let mut v = common.to_vec();
                v.extend(["consequence_predict", "recommend_build", "report_render"]);
                v
            }
            ConsultationMode::Discover => {
                let mut v = common.to_vec();
                v.extend(["discover_satisfice", "recommend_build", "report_render"]);
                v
            }
        }
    }

    pub fn primary_tool(self) -> &'static str {
        match self {
            ConsultationMode::Scenario => "solve_equilibria",
            ConsultationMode::Whatif => "consequence_predict",
            ConsultationMode::Discover => "discover_satisfice",
        }
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConsultationError {
    #[error("阶段闸门：{0}")]
    Phase(#[from] PhaseGateError),
    #[error("工具 `{tool}` 在阶段 `{phase}` 不可用：{reason}")]
    ToolUnavailable {
        tool: String,
        phase: String,
        reason: String,
    },
    #[error("剧本 `{0}` 不支持模式 `{1}`（白名单缺主工具 `{2}`）")]
    ModeUnsupportedByPlaybook(String, String, String),
    #[error("该环节需要外部交互（{0}），当前无实现——不伪造结果")]
    RequiresInteraction(String),
    #[error("引擎失败：{0}")]
    Engine(String),
    #[error("前置产物缺失：{0}（不应发生——PhaseMachine 应已拦截）")]
    MissingArtifact(String),
    #[error("GameSpec 校验未通过：{0:?}")]
    GameSpecInvalid(Vec<shangyi_gamespec::ValidationError>),
}

/// **校验剧本是否支持该模式**——启动前的快速失败。
///
/// # 为什么它必须是生产路径上的调用（实测审计发现）
///
/// 这个检查此前**只在一个未使用的 `Consultation` 结构上**存在
/// （该结构仅测试使用，见文件末尾的说明）。于是真实路径上：
/// 用不匹配的剧本跑某模式时**没有任何提示**——模型只能干等到
/// 「阶段推不动 + 单阶段预算耗尽」才被发现，而那时错误信息指向的是
/// "阶段没产物"，离真因（剧本缺工具）很远。
///
/// 返回 `Ok(())` 或一条**可操作**的错误说明。
pub fn check_playbook_supports_mode(
    mode: ConsultationMode,
    playbook: &shangyi_playbooks::Playbook,
) -> Result<(), String> {
    let primary = mode.primary_tool();
    // 空白名单 = 不限制（见 `Playbook::allows_tool` 的语义）
    if playbook.tool_allowlist.is_empty() || playbook.allows_tool(primary) {
        return Ok(());
    }
    // 一次列出**全部**缺失的工具——只报第一个会让用户改一次、再撞一次
    let missing: Vec<&str> = mode
        .primary_tool_names()
        .into_iter()
        .filter(|t| !playbook.allows_tool(t))
        .collect();
    Err(format!(
        "剧本 `{}` 不支持 `{}` 模式：它缺少该模式路径上的交付工具 `{}`。\n\
         → 会话会卡在对应阶段（该阶段的交付物产出不了）直到单阶段预算耗尽，\
         而错误信息只会显示「缺少产物」——离真因很远。\n\
         处置：在该剧本的 `tool_allowlist` 里加上缺的工具，或改用支持该模式的剧本。",
        playbook.meta.id,
        mode.as_str(),
        missing.join("、")
    ))
}

// ────────────────────────────────────────────────────────────────────────────
// 关于被删除的 `Consultation` 结构（记录于此，避免这段历史丢失）
//
// 本模块曾有一个 400 行的 `Consultation` 结构与 17 个公开方法
// （`register_profiles` / `register_game_spec` / `check_tool` / `confidence` / …），
// 它做的是"上下文登记 + 校验 + 阶段推进"——**与 `SessionRunner` +
// `DomainExecutor` 并行的一套实现**。
//
// 审计发现：它的方法**生产调用全为 0**，只在 `tests/wiring.rs` 里被用。
// 而那带来一个隐蔽的危害：**那些测试验证的是平行实现，不是真实路径**。
// 例如 `empty_profiles_rejected` 断言"登记空画像要被拒"，
// 而真实路径把"没有画像"当作**合法状态**（阶段闸门会拦住缺产物的推进）。
// 两者结论相反——测试通过给了**假信心**。
//
// 故删除它，并把其中**真实路径上确实缺的**那一项接了上去：
// `check_playbook_supports_mode`（剧本是否支持该模式）现在由 CLI
// 在创建案例目录之前调用（见函数文档）。
//
// 教训：**并行实现 + 只测平行实现的测试 = 假信心**。
// 保留它比删除它更坏，因为"有测试覆盖"会让人以为那条路径被守住了。

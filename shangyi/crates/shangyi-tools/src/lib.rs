//! 商奕领域工具集（设计文档 §4 与 Harness §9.3）。
//!
//! # 本 crate 只定义**契约与约束**，不实现 I/O
//!
//! 工具的实际执行（网络搜索、数据解析、求解器调用、侧车调用）分散在其所属 crate
//! 中。本 crate 的职责是**注册表 + 阶段白名单 + 可信度分类**——即那些
//! "让错误在类型层面不可能"的约束。
//!
//! # 三项核心约束
//!
//! ## 1. 工具白名单来自剧本（§13.3）
//!
//! `tools.allowlist_from: playbook.tool_allowlist`——剧本声明它能用哪些工具。
//! 这使"某个剧本不该用某工具"成为**数据**而非代码判断。
//!
//! ## 2. 阶段白名单（Harness §9.3 的表）
//!
//! 每个工具声明它可在哪些会话阶段被调用。在 `intake` 阶段调 `solve_equilibria`
//! 是逻辑错误——博弈尚未建模，求解什么？故 [`ToolRegistry::check_call`] 会拒绝。
//!
//! ## 3. 可信度分类（铁律 1）
//!
//! 工具结果分三类，**报告生成器按类过滤**：
//!
//! | 类 | 来源 | 可否进报告 |
//! |---|---|---|
//! | [`TrustLevel::Engine`] | 确定性 Rust 代码 | ✅ 可（且必须带 provenance） |
//! | [`TrustLevel::Sidecar`] | 进程隔离的 Python 侧车 | ⚠️ 仅渲染/检索，**不产生数字** |
//! | [`TrustLevel::Proposal`] | LLM 提案 | ❌ **须经引擎评估**后才能进报告 |
//! | [`TrustLevel::Exploratory`] | 临时数值任务 | ❌ **报告生成器拒绝读取** |
//!
//! 最后一类对应 Harness §9.3 对 `sandbox_exec` 的收窄（R8）：
//! 其产出标注 `exploratory: true`，报告生成器拒绝读取。


pub mod schema;

/// Intake routing and scoped delivery are session controls, not optional domain
/// capabilities in a playbook. Their phase restriction still applies.
pub fn is_session_control(name: &str) -> bool {
    matches!(name, "select_task_scope" | "complete_task" | "begin_followup_task" | "conditional_analysis") || is_research_control(name) || is_constraint_control(name)
}
pub fn is_constraint_control(name:&str)->bool {matches!(name,"constraint_register"|"constraint_check")}
pub fn is_research_control(name: &str) -> bool {
    matches!(name,"research_task_plan"|"research_task_update"|"research_document_lookup")
}
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use shangyi_playbooks::Playbook;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    #[error("工具 `{0}` 未注册")]
    Unknown(String),
    #[error("工具 `{tool}` 不允许在阶段 `{phase}` 调用（允许：{allowed:?}）")]
    PhaseNotAllowed {
        tool: String,
        phase: String,
        allowed: Vec<String>,
    },
    #[error("工具 `{tool}` 不在剧本 `{playbook}` 的白名单内")]
    NotInPlaybookAllowlist { tool: String, playbook: String },
    #[error("工具 `{0}` 不幂等，不得在只读/可重放上下文调用")]
    NotIdempotent(String),
    #[error("工具 `{0}` 的结果可信度不足，不得进报告（须先经引擎评估）")]
    InsufficientTrust(String),
    #[error("工具 `{0}` 的归属 crate 未声明")]
    MissingOwner(String),
}

/// 结果可信度分级（铁律 1 的落点）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevel {
    /// 确定性 Rust 代码算出——**可进报告**（须带 provenance）。
    Engine,
    /// 进程隔离的 Python 侧车——仅渲染与检索，**不产生进入报告的数字**。
    Sidecar,
    /// LLM 提案——**须经引擎评估**后方可进报告。
    Proposal,
    /// 临时数值探索——**报告生成器拒绝读取**。
    Exploratory,
}

impl TrustLevel {
    /// 该可信度级别的结果能否**直接**进报告。
    ///
    /// `Engine` 可（带上 provenance）。其余均不可：
    /// `Sidecar` 只该产出图与检索结果（数字来自 Rust）；
    /// `Proposal` 需先经引擎评估；`Exploratory` 一律拒绝。
    pub fn reportable_directly(self) -> bool {
        matches!(self, TrustLevel::Engine)
    }
}

/// 一个工具的规格。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// wire 名（LLM 看到的工具名）。
    pub name: String,
    /// 人类可读描述（进 tool schema）。
    pub description: String,
    /// **允许被调用的会话阶段**（Harness §9.3 表）。
    pub phases: BTreeSet<String>,
    /// 是否幂等（幂等者可安全重放；非幂等者不得在只读上下文调用）。
    pub idempotent: bool,
    /// 结果可信度。
    pub trust: TrustLevel,
    /// 归属 crate（实现该工具的 crate）。
    pub owner: String,
    /// 是否会产生进报告的数值（`Engine` 类通常为 true）。
    pub produces_reportable_numbers: bool,
}

impl ToolSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        owner: impl Into<String>,
        trust: TrustLevel,
        idempotent: bool,
        phases: &[&str],
    ) -> Self {
        // 归属 crate 的**校验放在 register()**，而非在此 panic：
        // 构造器不应因数据问题中止进程（库代码 panic 会拖垮调用方）。
        // 见 ToolError::MissingOwner 与 register() 的检查。
        let owner = owner.into();
        Self {
            name: name.into(),
            description: description.into(),
            phases: phases.iter().map(|s| s.to_string()).collect(),
            idempotent,
            trust,
            owner,
            produces_reportable_numbers: matches!(trust, TrustLevel::Engine),
        }
    }

    /// 该工具是否允许在某阶段调用。
    pub fn allows_phase(&self, phase: &str) -> bool {
        // 空 phases 视为"任意阶段"（如 kb_retrieve 的"全阶段"）
        self.phases.is_empty() || self.phases.contains(phase)
    }
}

/// 工具注册表。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolSpec>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册一个工具。重复名报错（不静默覆盖）。
    pub fn register(&mut self, spec: ToolSpec) -> Result<(), ToolError> {
        if spec.owner.trim().is_empty() {
            return Err(ToolError::MissingOwner(spec.name));
        }
        if self.tools.contains_key(&spec.name) {
            return Err(ToolError::Unknown(format!(
                "工具 `{}` 重复注册",
                spec.name
            )));
        }
        self.tools.insert(spec.name.clone(), spec);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.get(name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn names(&self) -> BTreeSet<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    /// 构建**商奕标准的 39 个领域工具**（含 `begin_followup_task`；2026-11-10 起随范围重开入口增至 39）。
    ///
    /// 这是"默认注册表"——剧本可从中取子集（见 [`ToolRegistry::for_playbook`]）。
    pub fn standard() -> Self {
        let mut r = Self::new();
        // (name, description, owner, trust, idempotent, phases)
        // phases 为空 = 任意阶段
        let specs: Vec<(&str, &str, &str, TrustLevel, bool, &[&str])> = vec![
            ("select_task_scope", "按用户实际请求选择资料、费用或完整咨询范围", "shangyi-core", TrustLevel::Engine, false, &["intake"]),
            ("begin_followup_task", "已结案短任务或条件分析交付后，按用户继续追问重开新一轮任务范围选择", "shangyi-core", TrustLevel::Engine, false, &[]),
            ("complete_task", "交付当前资料或费用任务及未取得项，不代表完整咨询归档", "shangyi-core", TrustLevel::Engine, false, &["intake"]),
            ("research_task_plan", "将用户实际请求字段与原文和证据要求关联", "shangyi-core", TrustLevel::Sidecar, true, &["intake"]),
            ("research_task_update", "登记请求字段的已读原文依据或具体缺口，不新增无关缺口", "shangyi-core", TrustLevel::Sidecar, true, &["intake"]),
            ("research_document_lookup", "在已读取站点按已见确切文件标题定向检索并读取原文", "shangyi-core", TrustLevel::Sidecar, true, &["intake"]),
            ("vn_map", "价值网五向扫描", "shangyi-intel", TrustLevel::Engine, false, &["intake", "intel"]),
            ("pars_check", "PARTS 检查表", "shangyi-intel", TrustLevel::Engine, true, &["intake", "intel"]),
            ("ask_user", "必要信息提问", "shangyi-core", TrustLevel::Proposal, false, &["intake", "intel", "model_build"]),
            ("research_record", "登记来自已抓取正文的租赁与竞争门店资料", "shangyi-core", TrustLevel::Sidecar, true, &["intake", "intel"]),
            ("constraint_register", "核对真实用户原句中的硬上限，不接受模型授权或自填数值", "shangyi-core", TrustLevel::Engine, true, &["intake","intel","model_build","select","consequence","discover","recommend","report"]),
            ("conditional_analysis", "基于实际证据缺口交付条件分析，不伪造收益或完整均衡", "shangyi-core", TrustLevel::Engine, false, &["intake","intel","model_build","solve","select","consequence","discover","recommend","report"]),
            ("constraint_check", "按当前真实用户上限筛选已计算方案；超限仅作对照、未知不视为可接受", "shangyi-core", TrustLevel::Engine, true, &["intake","intel","model_build","select","consequence","discover","recommend","report"]),
            ("business_cost", "单一方案的现金费用、押金与机会成本分列；条件现金门槛与续航计算", "shangyi-core", TrustLevel::Engine, true, &["intake", "intel", "model_build", "recommend"]),
            ("poi_survey", "按地址查周边门店计数（同类密度、写字楼、地铁口、同品牌存量）；只回计数不回明细", "shangyi-intel", TrustLevel::Sidecar, true, &["intake", "intel"]),
            ("web_search", "公开数据检索（多后端融合 + 去重 + 意图重排）", "shangyi-intel", TrustLevel::Sidecar, true, &["intake", "intel"]),
            ("web_fetch", "抓取正文（尊重 robots.txt、限速）", "shangyi-intel", TrustLevel::Sidecar, true, &["intake", "intel"]),
            ("data_ingest", "CSV/Excel 解析与列映射建议", "shangyi-intel", TrustLevel::Sidecar, true, &["intel", "model_build"]),
            ("participant_profile_update", "参与者画像更新（含贝叶斯后验）", "shangyi-intel", TrustLevel::Engine, false, &["intel"]),
            ("gamespec_build", "构建 GameSpec", "shangyi-gamespec", TrustLevel::Engine, false, &["model_build"]),
            ("gamespec_validate", "校验 GameSpec（铁律 2 的可执行落点）", "shangyi-gamespec", TrustLevel::Engine, true, &["model_build", "solve"]),
            ("solve_equilibria", "均衡枚举与精炼（确定性 Rust，不经沙箱）", "shangyi-solver", TrustLevel::Engine, true, &["solve"]),
            ("select_equilibria", "单选择器评估", "shangyi-selection", TrustLevel::Engine, true, &["select"]),
            ("select_fuse", "多判据融合", "shangyi-selection", TrustLevel::Engine, true, &["select"]),
            ("swarm_run", "persona 群 rollout（提议，非结论）", "shangyi-swarm", TrustLevel::Proposal, false, &["select", "recommend", "consequence"]),
            ("novel_move_filter", "非常规行动三重过滤", "shangyi-consequence", TrustLevel::Engine, true, &["consequence", "discover"]),
            ("consequence_predict", "条件后果预测（whatif 管线）", "shangyi-consequence", TrustLevel::Engine, true, &["consequence", "discover"]),
            ("monte_carlo", "参数不确定性传播（种子固定）", "shangyi-consequence", TrustLevel::Engine, true, &["recommend", "consequence"]),
            ("sensitivity_scan", "翻转阈值扫描", "shangyi-selection", TrustLevel::Engine, true, &["recommend"]),
            ("discover_satisfice", "满意策略发现", "shangyi-discover", TrustLevel::Engine, true, &["discover"]),
            ("preference_inject", "偏好注入", "shangyi-core", TrustLevel::Engine, true, &["recommend"]),
            // §7.3 的阶段表列了它，但**从未注册**——于是 recommend 阶段
            // 永远产不出 `RecommendationPack`，report 阶段在结构上不可达。
            // 实测：三次端到端都停在 recommend（见 iter27）。
            ("recommend_build", "汇总建议包（recommend 阶段产物）", "shangyi-core", TrustLevel::Engine, true, &["recommend"]),
            ("report_render", "报告渲染（产出 ReportModel → 侧车）", "shangyi-report", TrustLevel::Engine, false, &["report"]),
            ("case_store_read", "案例库读取", "shangyi-kb", TrustLevel::Engine, true, &[]),
            ("case_store_write", "案例库写入", "shangyi-kb", TrustLevel::Engine, false, &["archived"]),
            ("kb_retrieve", "理论库检索", "shangyi-kb", TrustLevel::Engine, true, &[]),
            ("checkpoint", "检查点确认（铁律 2）", "shangyi-core", TrustLevel::Engine, false, &[]),
            // 回退**提议**（决定权在用户，见该工具的实现文档）
            ("request_rewind", "提议回退到更早阶段（须用户同意）", "shangyi-core", TrustLevel::Proposal, false, &[]),
            ("sandbox_exec", "临时数值探索（结果不得进报告）", "shangyi-core", TrustLevel::Exploratory, false, &[]),
        ];
        for (name, desc, owner, trust, idem, phases) in specs {
            r.register(ToolSpec::new(name, desc, owner, trust, idem, phases))
                .expect("标准工具表不应有重复名");
        }
        r
    }

    /// **调用前校验**：工具存在、阶段允许、且（若给了剧本）在白名单内。
    ///
    /// 三项都查——任一失败即返回类型化错误。这是"失败即拒绝"的应用。
    pub fn check_call(
        &self,
        tool: &str,
        phase: &str,
        playbook: Option<&Playbook>,
    ) -> Result<&ToolSpec, ToolError> {
        let spec = self.get(tool).ok_or_else(|| ToolError::Unknown(tool.to_string()))?;
        if !spec.allows_phase(phase) {
            return Err(ToolError::PhaseNotAllowed {
                tool: tool.to_string(),
                phase: phase.to_string(),
                allowed: spec.phases.iter().cloned().collect(),
            });
        }
        if let Some(pb) = playbook {
            // 剧本若声明了工具白名单，则它必须包含该工具
            if !is_session_control(tool) && !pb.tool_allowlist.is_empty() && !pb.allows_tool(tool) {
                return Err(ToolError::NotInPlaybookAllowlist {
                    tool: tool.to_string(),
                    playbook: pb.meta.id.clone(),
                });
            }
        }
        Ok(spec)
    }

    /// 取某阶段**可用**的工具集（供 tool schema 生成）。
    ///
    /// **为何按阶段过滤工具 schema**：把所有工具都塞给模型会增加误用概率——
    /// 在 `intake` 阶段看到 `solve_equilibria`，模型可能过早尝试求解。
    /// 按阶段暴露正确的工具集，是"用约束引导正确行为"而非靠提示词。
    pub fn tools_for_phase(&self, phase: &str) -> Vec<&ToolSpec> {
        self.tools
            .values()
            .filter(|t| t.allows_phase(phase))
            .collect()
    }

    /// 取某阶段**且**在某剧本白名单内的工具集（两层过滤后的最终 schema）。
    pub fn tools_for(&self, phase: &str, playbook: &Playbook) -> Vec<&ToolSpec> {
        self.tools
            .values()
            .filter(|t| t.allows_phase(phase))
            .filter(|t| is_session_control(&t.name) || playbook.tool_allowlist.is_empty() || playbook.allows_tool(&t.name))
            .collect()
    }

    /// 校验一个工具结果能否进报告。
    ///
    /// **这是铁律 1 的最后一道闸**：即使工具调用成功，其结果的 `trust`
    /// 级别不足时报告生成器仍须拒绝。`sandbox_exec`（`Exploratory`）的产出
    /// 在此被拦下——对应 Harness §9.3 的"报告生成器拒绝读取带
    /// `exploratory: true` 标记的结果"。
    pub fn check_reportable(&self, tool: &str) -> Result<(), ToolError> {
        let spec = self.get(tool).ok_or_else(|| ToolError::Unknown(tool.to_string()))?;
        if !spec.trust.reportable_directly() {
            return Err(ToolError::InsufficientTrust(format!(
                "{}（可信度 {:?}）",
                tool, spec.trust
            )));
        }
        Ok(())
    }

    /// 全部 `Engine` 类工具（会产出可进报告的数字者）。
    pub fn engine_tools(&self) -> Vec<&ToolSpec> {
        self.tools
            .values()
            .filter(|t| t.trust == TrustLevel::Engine && t.produces_reportable_numbers)
            .collect()
    }

    /// 全部非幂等工具（不得在只读/可重放上下文调用）。
    pub fn non_idempotent(&self) -> Vec<&ToolSpec> {
        self.tools.values().filter(|t| !t.idempotent).collect()
    }
}

// ────────────────────────────────────────────────────────────────────────────
// 阶段回退请求（供模型向用户**提议**回退，而非自行回退）
// ────────────────────────────────────────────────────────────────────────────
//
// # 为什么需要这个工具（实测暴露）
//
// 设计文档规定回退的触发者是**用户**（§7.5：`shangyi resume --reopen cp2`），
// 不是模型。实测里模型正确地**拒绝**了自行回退——但它没有任何工具能表达
// "我需要回退"这个判断，于是它只能停在 recommend 阶段把话说完，**不再前进**。
//
// 结果是：一次咨询走到了 recommend（求解、选择、敏感性都完成了），
// 却因为"它想先补来源"而**停在那里**。既没进 report，也没告诉用户该怎么继续。
//
// 正确处置：给模型一个**提议**回退的工具。它只能提议，决定权在用户——
// 这与铁律 2 的精神一致（领域判断可以提议，确认必须由人做）。

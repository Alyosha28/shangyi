//! Agent 核心循环（设计文档 §7）。
//!
//! # 职责边界
//!
//! 本模块是**调度**，不是智能，也不是数值。它做四件事、且只做四件事：
//!
//! 1. **步进**：构造请求 → 调模型 → 执行它请求的工具 → 把结果回灌 → 再来一步。
//! 2. **闸门**：每一步之前跑 [`PreStepGate`]；checkpoint 未结算则**拒绝推进**（§7.4）。
//! 3. **记账**：把每一步写成 [`Event`]（§7.2），使"当时发生了什么"可审计。
//! 4. **诚实终止**：turn 的结束原因必须区分"正常完成"、"外部阻塞"、"出错"
//!    （§7.1）——**绝不把任何一种伪装成 `Completed`**。
//!
//! # 为什么循环本身不含领域逻辑
//!
//! 领域决策全部在工具里（确定性引擎）与剧本里（数据）。循环若开始"自己判断
//! 该不该求解"，那两处就都有了第二个真相源。故本模块对领域的全部认知就是
//! [`ToolExecutor`] 这个 seam——测试可以注入假执行器，生产注入真引擎。
//!
//! # 铁律 1 的落点
//!
//! 循环**从不让模型产生数字**。模型只能调用工具；工具的结果由确定性代码产出，
//! 并且带 [`TrustLevel`]——`report_render` 会拒绝读取不可报告的级别（§11.9）。

use std::fmt;

use shangyi_llm::{
    InputItem, LlmError, LlmRequest, LlmResponse, OutputItem, ToolChoice, ToolSchema, Transport,
};
use shangyi_playbooks::Playbook;
use shangyi_tools::ToolRegistry;

use crate::phase_graph::{CheckpointRequest, PhaseGateError, PhaseMachine, SessionPhase};
use crate::clock::{Clock, SeqClock, SystemClock};
use crate::session::{
    CancelCause, Event, StepId, ToolOutcome, TurnEndReason, TurnId,
};

// ─────────────────────────── 工具 seam ───────────────────────────

/// 一次工具调用（模型请求的，或循环编排的）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

/// 工具执行结果。
///
/// # 三类结果必须分开（不是一个 `Result`）
///
/// `Ok` 与 `Failed` 的区别是显然的；关键是 [`ToolOutcome::Blocked`]：
/// **"无法评估"与"评估为负面"是两回事**。数据源不可用导致的情报缺口，
/// 与"查到了，结论是负面"，在报告里必须写法不同——前者是"未取得"，
/// 后者是"结论"。把它们合成一个 `Err(String)` 会让报告把缺口写成结论。
#[derive(Debug, Clone, PartialEq)]
pub enum ExecOutcome {
    TaskScopeSelected { scope: crate::task_scope::TaskScope, reason: String },
    ScopedTaskCompleted(crate::task_scope::ScopedTaskResult),
    FollowupTaskRequested { reason: Option<String> },
    InsufficientEvidence { gaps: Vec<crate::conditional::EvidenceGap> },
    /// 成功。`output` 是回灌给模型的文本（通常是 JSON）。
    Ok(String),
    /// 工具执行失败（参数非法、内部错误）。**可以通过改参数重试**。
    Failed(String),
    /// 外部依赖不可用，**无法得出结论**。这不是错误，是"缺"。
    Blocked(String),
    /// checkpoint 类工具：请求向用户确认，循环在此挂起（§7.6）。
    NeedsUserInput(CheckpointRequest),
    /// `ask_user`：向用户**采集**信息，循环在此挂起（§8.1）。
    ///
    /// 它与 `NeedsUserInput` 分开，是因为后续处理不同（见 [`UserQuestion`]）。
    /// 合并二者会让"确认"与"采集"在日志里无法区分——而审计要能回答
    /// "这份结论依赖哪些用户输入"，那是采集；"哪些环节用户盖过章"，那是确认。
    AsksUser(Vec<UserQuestion>),
    /// `request_rewind`：模型**提议**回退到更早阶段，等用户裁决。
    ///
    /// # 为什么是"提议"而不是"执行"
    ///
    /// 设计文档 §7.5 规定回退的触发者是**用户**（`shangyi resume --reopen cp2`），
    /// 不是模型。这不只是权限分配——回退意味着**作废已完成的工作**，
    /// 那是有代价的决定，用户必须知情。
    ///
    /// # 实测暴露的缺口（本变体存在的原因）
    ///
    /// 一次真实咨询走到了 `recommend`（求解、选择、敏感性都完成），
    /// 模型判断"几个硬假设缺来源、结论不可靠"，于是**建议回 model_build**——
    /// 但它没有任何工具能表达这个判断，也没有权限执行回退。
    /// 结果它把话说完就**停在那里**：既没进 report，也没告诉用户怎么继续。
    ///
    /// 正确处置：让模型能**提议**，由用户裁决。这与铁律 2 的精神一致
    /// （领域判断可以提议，确认必须由人做）。
    RewindRequest {
        target: crate::phase_graph::SessionPhase,
        reason: String,
    },
}

/// 工具执行的 seam。
///
/// 实现者负责：查注册表校验（阶段白名单、剧本白名单）、调对应引擎、
/// 把结果序列化成回灌文本。**实现者不负责**判断阶段能否推进——那是循环的事。
pub trait ToolExecutor {
    /// 执行一次工具调用。
    ///
    /// 取 `&mut self` 而非 `&self`：领域工具有真实状态（画像、GameSpec、解集），
    /// 而 `gamespec_build` / `novel_move_filter` 等**本来就要改写它**。
    /// 用 `&self` + `RefCell` 只会把这个事实藏起来，并让"执行器不可重入"
    /// 这一真实约束变成运行期 panic。
    fn execute(&mut self, call: &ToolCall, phase: SessionPhase) -> ExecOutcome;

    fn begin_followup_task(&mut self) {}
    fn record_user_input(&mut self, _text: &str, _actor: &str) {}
    fn take_invalidated_artifacts(&mut self) -> Vec<crate::phase_graph::ArtifactKind> { Vec::new() }
    fn conditional_fallback(&mut self, _gaps: &[crate::conditional::EvidenceGap]) -> Result<crate::task_scope::ScopedTaskResult, String> {
        Err("执行器没有可供条件交付的真实证据快照".into())
    }

    /// 执行器当前拥有的产物（键 → 指纹）。循环在每步后据此登记到阶段机。
    ///
    /// 默认空实现使"无状态的假执行器"不必写样板。
    fn artifacts(&self) -> Vec<(crate::phase_graph::ArtifactKind, String)> {
        Vec::new()
    }
}

/// 一个要向用户提出的问题（`ask_user` 的载荷）。
///
/// 与 [`CheckpointRequest`] 的区别是**目的不同**：
/// - `CheckpointRequest` 是**确认**（"这些我查到的，对吗？"）——铁律 2 的闸门。
/// - [`UserQuestion`] 是**采集**（"这个我不知道，你说的算"）——intake 的入口。
///
/// 两者都会挂起循环，但语义与后续处理完全不同：确认的答案用于**结算**
/// （并绑定产物指纹），采集的答案用于**补充输入**（进对话历史）。
#[derive(Debug, Clone, PartialEq)]
pub struct UserQuestion {
    pub id: String,
    pub text: String,
    /// `choice` / `text` / `number`（与剧本 `IntakeQuestion.kind` 同口径）。
    pub kind: String,
    /// `choice` 型的选项。
    pub options: Vec<String>,
}

/// 一个什么都不做的执行器：任何调用都返回 `Failed`。
///
/// 用途单一：让"循环骨架"可以在没有引擎的情况下被测试。
/// **它是 fail-closed 的**——不会悄悄返回成功，那会让测试通过而产品撒谎。
pub struct NullExecutor;

impl ToolExecutor for NullExecutor {
    fn execute(&mut self, call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
        ExecOutcome::Failed(format!("工具 `{}` 未接线（NullExecutor）", call.name))
    }
}

// ─────────────────────────── 步前闸门 ───────────────────────────

/// 步前决策（§7.4 的 `PreStepDecision`）。
#[derive(Debug, Clone, PartialEq)]
pub enum PreStepDecision {
    /// 放行。
    Accept,
    /// 放行，但**替换**进入 step 的输入（阶段指引注入）。
    Inject { notes: Vec<String> },
    /// 拒绝拟议 step。
    Reject { reason: String },
}

/// 步前闸门：阶段闸门 + checkpoint 闸门（§7.4）。
///
/// # 为什么这是一个硬拒绝而不是一句 prompt
///
/// §7.6 说得很直白：**如果 checkpoint 只是 prompt 文本，模型可以（并且迟早会）
/// 跳过它**——那时铁律 2 就退化为一句愿望。实现为循环内的硬拒绝意味着：
/// 只要 checkpoint 未结算，**任何** step 都无法通过，无论模型怎么想。
pub struct PreStepGate<'a> {
    machine: &'a PhaseMachine,
}

impl<'a> PreStepGate<'a> {
    pub fn new(machine: &'a PhaseMachine) -> Self {
        Self { machine }
    }

    /// 判定当前是否允许发起 step。
    ///
    /// # 三条判据（顺序不可换）
    ///
    /// 1. **本阶段的 checkpoint 处于 `Open`** ⇒ 硬拒绝。这是 §7.6 的承重点：
    ///    checkpoint 挂着时循环不许推进，无论模型怎么想。
    /// 2. **本阶段的 checkpoint 被 `Rejected`** ⇒ 硬拒绝。拒绝意味着"内容要改"，
    ///    必须修订后重新确认；允许继续跑就等于绕过了用户的否决（铁律 2）。
    /// 3. **进入条件**（前驱阶段的产物 + 前驱的 checkpoint 已批准）⇒ 拒绝。
    ///    这是 §7.3 `requires` 的语义。它通常是**防御性**的（正常路径下
    ///    `advance` 已保证），但它堵住的是"绕过 `advance` 直接构造状态"的路径。
    ///
    /// # ⚠️ 为什么不检查"本阶段的 checkpoint **尚未打开**"
    ///
    /// §7.4 的示例代码在 checkpoint 未结算时一律拒绝，理由是"模型不能先跑再说"。
    /// **该写法在工程上不可实现**，原因是它自锁：
    ///
    /// - `intel` 的 checkpoint① 在 `intel` 内结算。若"未打开即拒绝"，则 `intel`
    ///   期间**任何** step 都过不去 ⇒ 模型无法调用 `web_search` 采集证据。
    /// - 唯一出路是先调 `checkpoint` 工具——但那时它没有任何事实可提交，
    ///   而 [`CheckpointRequest::validate`] 拒绝空提问件 ⇒ 死锁。
    ///
    /// 因此本实现的判据是"**挂着或被拒**则拒绝"，而"尚未打开"通过
    /// **指引注入**（见 [`phase_guidance`]）提示模型在该阶段末发起确认；
    /// "没有用户确认就不得离开该阶段"这一条由
    /// [`crate::phase_graph::PhaseMachine::gate_to_advance`] **硬性**保证。
    /// 铁律 2 的强制力因此不来自"每一步都拦"，而来自"**离开**必须经确认"——
    /// 这与 §7.6 的 checkpoint 生命周期（阶段末 → 提问 → 挂起 → 结算）一致，
    /// 且不存在自锁路径。该偏差已登记为差异 R21（见 `.shangyi-loop/DECISIONS.md`）。
    /// 当前阶段在本模式路径上的**前驱**（进入条件的来源）。
    ///
    /// 取主路径的上一个节点；若它是可选节点（`whatif` 的 `solve`），进入条件
    /// 仍需满足它——因为"选择绕过"是运行期的决定，而闸门问的是"这条路走得通吗"。
    fn predecessor(&self, phase: SessionPhase) -> Option<SessionPhase> {
        let g = self.machine.graph();
        let i = g.position(phase)?;
        i.checked_sub(1).and_then(|j| g.path().get(j)).copied()
    }

    pub fn decide(&self) -> PreStepDecision {
        let phase = self.machine.phase();

        // 1) + 2) 当前阶段的 checkpoint：挂着或被拒 ⇒ 硬拒绝
        if let Some(cp) = self.machine.graph().checkpoint_at(phase) {
            match self.machine.checkpoint_status(cp) {
                Some(s) if s.is_approved() => {}
                Some(s) if s.is_open() => {
                    return PreStepDecision::Reject {
                        reason: format!(
                            "阶段 `{phase}` 正在等待用户确认（{cp}），循环必须挂起而非继续推理。"
                        ),
                    };
                }
                Some(_) => {
                    return PreStepDecision::Reject {
                        reason: format!(
                            "阶段 `{phase}` 的 {cp} 已被用户拒绝，须修订该阶段产物后重新确认——\
                             不得绕过继续推进（铁律 2）。"
                        ),
                    };
                }
                // 尚未打开：不在本函数拦截（见文档注释的上锁论证）
                None => {}
            }
        }

        // 3) 进入条件：前驱阶段的产物 + 前驱的 checkpoint 已批准
        if let Some(pred) = self.predecessor(phase) {
            let missing: Vec<&str> = self
                .machine
                .graph()
                .produces(pred)
                .iter()
                .filter(|k| !self.machine.has_artifact(**k))
                .map(|k| k.key())
                .collect();
            if !missing.is_empty() {
                return PreStepDecision::Reject {
                    reason: format!(
                        "阶段 `{phase}` 的进入条件未满足：前驱阶段 `{pred}` 的产物 {} 缺失。\
                         请先回到 `{pred}` 补齐。",
                        missing.join("、")
                    ),
                };
            }
            if let Some(pcp) = self.machine.graph().checkpoint_at(pred) {
                match self.machine.checkpoint_status(pcp) {
                    Some(s) if s.is_approved() => {}
                    other => {
                        return PreStepDecision::Reject {
                            reason: format!(
                                "阶段 `{phase}` 的进入条件未满足：前驱阶段 `{pred}` 的 {pcp} 未获批准\
                                 （当前：{}）。",
                                match other {
                                    None => "尚未打开",
                                    Some(s) if s.is_open() => "等待用户作答",
                                    _ => "已被用户拒绝",
                                }
                            ),
                        };
                    }
                }
            }
        }

        // 4) 放行并注入阶段指引（surface 化：它影响请求，所以它必须被记录）
        //
        // 指引里附带"本阶段末需要确认什么"——**这是提示，不是强制**。
        // 强制在 gate_to_advance：没有用户批准，离开本阶段的尝试一律被拒。
        // 二者分工明确：提示让模型知道下一步动作，强制保证它无法跳过。
        let scope_guidance = if phase == SessionPhase::Intake {
            match self.machine.task_scope() {
                None => "先调用 select_task_scope，根据用户此次明确请求选择 research（查公开资料）、cost（计算费用）、research_and_cost（两者）或 consultation（需要策略/博弈分析）。用户只查资料或算费用时不能扩大为完整咨询，也不要求用户选择专业模式。只在无法判断实际目标时问一个简短问题。".to_string(),
                Some(scope) if scope.is_brief() => format!("本次任务范围为 {}。完成用户要的资料整理或费用计算即可，保留来源、时效、假设和缺口。不要做价值网问卷、参与者画像、checkpoint、建模或求解。使用真实工具取得依据；可用结果齐后调用 complete_task 提交普通中文最终答复与 missing 缺口清单。公开事实优先主动查询；检索受阻如实列为未取得，不能编造补齐。费用未知金额保持 null，结果只能按已知部分交付。",scope.as_str()),
                _ => phase_guidance(phase).to_string(),
            }
        } else { phase_guidance(phase).to_string() };
        let mut notes = vec![
            include_str!("../../../prompts/consultant-system.md").to_string(),
            scope_guidance,
        ];
        if let Some(cp) = self.machine.graph().checkpoint_at(phase) {
            notes.push(format!(
                "本阶段结束时必须调用 `checkpoint` 工具提交{}清单并等待用户确认（{cp}）；\
                 未获确认不得进入下一阶段。",
                cp.kind_str_zh()
            ));
        }
        if self.machine.graph().is_optional(phase) {
            notes.push(format!(
                "`{phase}` 是可选阶段：若本次咨询不需要它，可用跳过边直接进入下一阶段。"
            ));
        }
        PreStepDecision::Inject { notes }
    }
}

/// 阶段的指引文本（§7.4 的"阶段指引 prompt 注入"）。
///
/// 指引是**数据**在代码里的最小落点；正式版本由 `shangyi/prompts/stages/*.md`
/// 提供（§6.1）。此处给出内建默认，使循环在提示词文件缺失时仍有可用的行为
/// ——但**不假装**它就是最终提示词。
/// 测试用：读取某阶段的指引文本。
///
/// 暴露它使"指引是否点名了交付物工具"成为**可检查的约束**——
/// 实测 run17 正是因为指引没提 `recommend_build` 而卡在 recommend。
pub fn phase_guidance_for_test(phase: SessionPhase) -> &'static str {
    phase_guidance(phase)
}

fn phase_guidance(phase: SessionPhase) -> &'static str {
    match phase {
        SessionPhase::Intake => {
            "当前阶段：intake（剧本识别与信息收集）。用 `vn_map` 做价值网五向扫描。\
             先区分本次目标需要核验的公开事实与用户内部信息：公司、产品、公开价格、行业背景、政策和竞对动态，\
             主动用 `web_search` 检索、`web_fetch` 核对原文，保留来源、日期与适用范围，\
             不要让用户代查或确认每条公开资料。只有主体或地区不明确到无法检索时，才先询问最小定位信息。\
             `ask_user` 仅询问无法公开取得且会改变决策的内部信息、目标、偏好或约束；\
             每轮优先一个、最多两个问题，不得把多个信息点塞进一道题。\
             剧本清单仅供查漏，不要逐项提问；已回答或表示不知道的内容不要重复问。\
             检索失败如实标为未取得，不能编造，也不要把缺口清单全部交给用户；\
             非关键缺口保留未知继续分析，关键缺口才说明影响并询问用户是否掌握。\
             若已提供开店费用与销售假设，先调用 `business_cost` 计算已列成本及条件现金门槛，不必等租金资料收齐；它不等于博弈求解。\
             每次成本计算只含一个具体 scenario，各行 scenario 与顶层一致；不同方案或互斥报价必须分开调用，不能合并租金。首次方案 operation=create，由引擎分配 scenario_id；用户改名、补充费用或改累计期间用 operation=update，引用原返回标识，不能另造标识或另建重复方案。比较两年费用时提供 horizon_months=24，租金仍按每月输入；不得把未来月租合计写成一次性现金付款。\
             各费用明确 treatment：实际支付用 cash_expense，可退押金用 refundable_deposit，停业少赚或放弃收益用 opportunity_cost，折旧等用 noncash_expense。\
             period=once 不表示现金；机会损失不能扣可用现金，押金另列资金占用且不得假设退还时间。性质未知用 unknown，费用金额未取得须显式 amount:null 并标 costs_complete=false，不能填0或1占位。\
             costs 不接收收入；未来租金收入等未知项保留在分析缺口中，不得伪装为费用交给成本工具。可用现金与 sales 未知时不提供对应数值输入，不得编数凑齐。\
             来源中的虚构条件与假设仍是条件，不能因工具给出数值就称为已核实事实；若用户已明确分类，直接据此计算，不重复追问。\
             本次咨询网络调用总预算 16 次，先做少量定向搜索再抓最相关原文；不要穷举搜索词。\
             提问只针对用户自身情况，不夹带未经核实的外部现状断言；旧报道中的店铺数量只能称为当年报道，不能冒充当前竞争规模。\
             不要在本阶段构建或求解博弈模型。"
        }
        SessionPhase::Intel => {
            // ⚠️ 必须点名**交付工具**并要求收尾——与 recommend 阶段同因。
            //
            // 实测（run28，glm-4-flash）：模型在该阶段做了 16 次工具调用
            // （`web_search` × 10、`kb_retrieve` × 4、`ask_user`、`case_store_read`），
            // 却**一次都没调 `participant_profile_update`**，于是 6 个 turn
            // 全用满、`participant_profiles` 始终缺失、阶段推进不了。
            //
            // 它的问题不是"不会用工具"，是**不知道要收尾**——指引只说了
            // "用什么工具取证"，没说"什么时候算完"。而检索是**可以无限做下去**的
            // （总能再搜一个关键词），故它永远不会自己停。
            //
            // 这是同一个教训的第二次出现（第一次在 recommend：模型调了 9 次
            // `sensitivity_scan` 而 0 次 `recommend_build`）。**每个阶段的指引
            // 都必须点名交付物工具，并说明"不调它就无法进入下一阶段"。**
            "当前阶段：intel（公开数据搜集与参与者画像）。\n\
             1. 对本次目标需要且用户没有明确排除的公开资料，用 `web_search`/`web_fetch` 取证——每条证据都要留来源；\
                抓不到的要如实标为未取得，不得用常识补；公开资料主动搜索，不让用户代查。\
                核对原文、日期和适用范围，区分事实与推断。仅对影响决策且无法公开取得的\
                内部信息、目标或约束使用 `ask_user`，每轮最多两个问题；非关键缺口保留未知\n\
             2. **用 `participant_profile_update` 把结论落成画像**——\
                检索结果不落成画像是**没有产出**的：本阶段要求 `participant_profiles` 产物\n\
             3. **本阶段必须以 `participant_profile_update` 收尾**：\
                检索可以无限做下去（总能再搜一个关键词），故请自行判断「够建模了」\
                就停下并产出画像。剩余缺口如实标为未取得即可。"
        }
        SessionPhase::ModelBuild => {
            // 每个阶段的指引都必须说清**怎么算完**——见 intel 的注释
            // （run28：只点名工具、不说收尾，模型会无限检索下去）。
            "当前阶段：model_build（构建 GameSpec）。\n\
             1. 用 `gamespec_build` 构建、`gamespec_validate` 校验；\
                硬假设必须带来源，否则 `checkpoint` 会拒绝提交\n\
                gamespec_build 成功后，直接以 {} 调 gamespec_validate 校验当前模型，不要为校验重写矩阵。\
                只有单独检查另一份完整 GameSpec 才传 spec；validate 不会保存输入，也不会在输入错误时改验已有模型。\
                校验错误须按字段路径修正；payoffs 须是 form/values/cells，不能传玩家名到二维收益数组。若只想检查已有模型，改用 {}。\
             2. **本阶段必须以 `gamespec_build` 收尾**（产物是 `gamespec`，\
                它落盘并过 cp2 后才能进 solve）。缺来源的参数如实标注，\
                不要为了「完整」而编数。关键收益未取得时，不得补零、改参与者身份或伪造参数绕过校验；明确说明不能据此作完整策略求解，保留已经核实的费用与条件结果。"
        }
        SessionPhase::Solve => {
            "当前阶段：solve（均衡枚举与精炼）。\n\
             1. 用 `solve_equilibria`——它是确定性 Rust 代码，结果带 provenance；\
                不得自行推断或估计均衡\n\
             2. （可选）`solve_equilibria` 的 `refine` 参数可做**精炼过滤**\
                （颤抖手完美 / 演化稳定）——用更强的解概念筛掉不稳健的均衡。\
                不传则返回全部均衡（交给 select 阶段的判据决定）\
             3. **本阶段必须以 `solve_equilibria` 收尾**（产物 `equilibrium_set`）。\
                若求解器类型化拒绝，那是博弈定义的问题，回 model_build 修，\
                而不是反复重试同一个定义。"
        }
        SessionPhase::Select => {
            // ⚠️ 关于 `swarm_run` 的那条提示是**必要的**（实测缺陷）：
            //
            // 它会**改 GameSpec**（把被引擎接受的矩阵外动作注册成策略），
            // 而此前没有任何地方告诉模型这件事。后果是模型可能
            // 先 `select_equilibria` + `select_fuse` 得出一组分布，
            // 再调 `swarm_run`——此时博弈已经变了，而分布是**旧博弈**的，
            // 两者对不上却看不出来。
            "当前阶段：select（均衡选择）。\n\
             1. 用 `select_equilibria` 为各判据登记评分（**每个判据一次**，\
                分数个数须等于均衡个数）。**每项带上 `rationale` 与 `evidence`**——\
                它们是报告里「为什么选这个均衡」的依据，裸数字会让那句话永远是空话；\
                分数不必归一化（引擎会统一量纲）\n\
             2. 用 `select_fuse` 融合为概率分布\n\
             3. **本阶段必须以 `select_fuse` 收尾**——产物是 `selection_output`，\
                不调它无法进入下一阶段。判据权重未校准时如实标注，不要假装已校准。\n\
             ⚠️ 若你要调 `swarm_run`：它会**把被引擎接受的矩阵外动作注册成策略**，\
             从而改变博弈本身。请**先调它、再求解与融合**——\
             否则你融合出的分布属于旧博弈，而新策略不在里面。"
        }
        SessionPhase::Consequence => {
            "当前阶段：consequence（条件后果预测）。\n\
             1. 用 `consequence_predict` 回答用户给定的动作（它会构造残余子博弈并重解）\n\
             2. 必要时用 `novel_move_filter` 过滤矩阵外的非常规行动\n\
             3. **本阶段必须以 `consequence_predict` 收尾**\
                （产物 `consequence_report` 就是它的输出，故至少调它一次才算完成）"
        }
        SessionPhase::Discover => {
            "当前阶段：discover（满意策略发现）。\n\
             1. 用 `consequence_predict` 取各候选的后果分位数（引擎算，不是你算）\n\
             2. 用 `discover_satisfice` 评估——它接收候选与抱负水平，\
                **由引擎判定满意与否**（不要自己下结论）\n\
             3. **本阶段必须以 `discover_satisfice` 收尾**——产物是 `discovery_report`。"
        }
        SessionPhase::Recommend => {
            // ⚠️ 这条指引是**必须的**，不是可选的旁白。
            //
            // 实测（run17）：模型在 recommend 里正确完成了敏感性扫描与蒙特卡洛，
            // 但**从未调用 `recommend_build`**——于是本阶段的交付物
            // `RecommendationPack` 登记不上，阶段**推不进 report**。
            // 它在 recommend 里跑了 6 个 turn 直到单阶段预算耗尽。
            //
            // 根因是指引没说"本阶段的交付物由哪个工具产出"。只说
            // "用 sensitivity_scan 与 monte_carlo"会让模型以为做完它们就结束了。
            "当前阶段：recommend（对策与敏感性）。\n\
             1. 用 `sensitivity_scan` 找翻转阈值、`monte_carlo` 传播不确定性\n\
             2. 用 `preference_inject` 注入用户偏好（可选，但会改变推荐）\n\
             3. **本阶段必须以 `recommend_build` 收尾**——它产出本阶段的交付物\
                （建议包），**不调用它就无法进入 report**。\n\
             ⚠️ **无分布时也要收尾**（这条指引来自 run29 实测）：
             本模式的依据**不止分布**——`recommend_build` 接受本模式实际产出的\
             任何一种引擎结果（scenario 用均衡分布、whatif 用后果报告、\
             discover 用满意评估）。若缺了它，**不要只提议回退**：\
             先尽力产出，把缺来源的部分**如实标注为待确认项**。\n\
             （回退需要用户裁决，而自动模式会拒绝——那时「只提议回退」\
             等于把会话卡死。如实交付残缺但有依据的结论，比空转有价值。）"
        }
        SessionPhase::Report => {
            // 与其他阶段同款：三句话——怎么做、产物是什么、**以什么收尾**。
            "当前阶段：report（报告生成）。\n\
             1. 用 `report_render` 生成报告——报告的每个数字都必须有 provenance，\
                缺 provenance 的数字会使报告被拒\n\
             2. **本阶段必须以 `report_render` 收尾**（产物是报告束）。\
                它是全链路最后一环：产出后才能进 archived。"
        }
        SessionPhase::Archived => {
            // 与其他阶段同款的三段式。
            //
            // ⚠️ 早先这里写着"它要求已有均衡分布"——那句话在被修掉之前
            // **把 whatif/discover 模式的终态堵死了**（那两条路径没有 select）。
            // 指引的措辞会成为模型的行动依据，故它必须与实现一致：
            // 归档要求的是**此刻的预测**，而预测的形态按模式而异。
            "当前阶段：archived（终态）。\n\
             1. 用 `case_store_write` 归档本次咨询——归档时**预测被冻结**，\
                供日后回填结果、校准判据权重\n\
             2. **必须给 `provenance`**（案例来源），且必须如实：\n\
                · `real_engagement` —— **真实客户委托**（事后真有结果可回填）\n\
                · `exercise` —— 演练 / 样例 / 批量跑批（**不会有**真实结果）\n\
                为什么必须如实：只有 `real_engagement` 会被判据权重校准采纳。\
                演练案例的「结果」是假设情景下的产物，拿它调权重等于\
                **用编出来的数据拟合可信度**；而把真实委托误标成演练，\
                会让校准永远攒不够样本。两个方向都看不出来，故不许默认、不许含糊。\n\
                若本次是替用户做的**真实决策**，就填 `real_engagement`。\n\
             3. **本阶段必须以 `case_store_write` 收尾**：没有冻结的预测，\
                三个月后有了结果也无从比对，那份记录对校准毫无用处。\n\
                它能接受三种预测形态（均衡分布 / 条件后果报告 / 满意策略集），\
                取决于本模式实际产出了哪一种。"
        }
    }
}

impl crate::phase_graph::CheckpointId {
    /// 该 checkpoint 确认什么（中文，用于面向模型的拒绝理由）。
    pub fn kind_str_zh(self) -> &'static str {
        match self {
            crate::phase_graph::CheckpointId::Cp1 => "事实",
            crate::phase_graph::CheckpointId::Cp2 => "硬假设",
            crate::phase_graph::CheckpointId::Cp3 => "结果预览",
        }
    }
}

// ─────────────────────────── 循环 ───────────────────────────

/// 一次 turn 的结局。
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// 模型不再请求工具，turn 正常结束。
    Completed {
        output: String,
        steps: u32,
    },
    /// 挂起等待用户（checkpoint）。**不是失败**——是设计中的暂停点。
    AwaitingUser {
        checkpoint: CheckpointRequest,
        steps: u32,
    },
    /// 挂起等待用户**回答问题**（`ask_user` 采集）。
    AwaitingAnswers {
        questions: Vec<UserQuestion>,
        steps: u32,
    },
    /// 挂起等待用户裁决"是否回退"。
    AwaitingRewindDecision {
        target: crate::phase_graph::SessionPhase,
        reason: String,
        steps: u32,
    },
    /// 外部依赖不可用，无法继续（§7.1 的 `Blocked`）。
    Blocked {
        detail: String,
        steps: u32,
    },
    /// 出错（模型调用失败、步数超限等）。
    Failed {
        reason: TurnEndReason,
        steps: u32,
        /// **可操作的**原因说明（供 CLI 直接展示）。
        ///
        /// 不带 detail 时 CLI 只能打印 `{reason:?}`（形如 `Error { detail: "…" }`）
        /// ——那对用户没有信息量，他看到一个 Rust 枚举名。这个字段存在的理由
        /// 就是让失败**能被人读懂**。
        detail: String,
    },
}

impl TurnOutcome {
    /// 映射为日志用的终止原因。
    ///
    /// `AwaitingUser` 映射为 `Aborted { Parent }`：会话在此**停止推进**，
    /// 等待外层（用户）输入。它**不是** `Completed`——把它记成完成会让日志
    /// 显示"分析做完了"，而实际上用户还没被问过（审计失败）。
    pub fn end_reason(&self) -> TurnEndReason {
        match self {
            TurnOutcome::Completed { .. } => TurnEndReason::Completed,
            TurnOutcome::AwaitingUser { .. }
            | TurnOutcome::AwaitingAnswers { .. }
            | TurnOutcome::AwaitingRewindDecision { .. } => TurnEndReason::Aborted {
                reason: CancelCause::Parent,
            },
            TurnOutcome::Blocked { detail, .. } => TurnEndReason::Blocked {
                detail: detail.clone(),
            },
            TurnOutcome::Failed { reason, .. } => reason.clone(),
        }
    }

    pub fn steps(&self) -> u32 {
        match self {
            TurnOutcome::Completed { steps, .. }
            | TurnOutcome::AwaitingUser { steps, .. }
            | TurnOutcome::AwaitingAnswers { steps, .. }
            | TurnOutcome::AwaitingRewindDecision { steps, .. }
            | TurnOutcome::Blocked { steps, .. }
            | TurnOutcome::Failed { steps, .. } => *steps,
        }
    }
}

/// 循环配置。
#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub model: String,
    /// 单个 turn 的最大 step 数。触顶 ⇒ `MaxTokens`（**不是** `Completed`：
    /// 分析没做完，只是没报错）。
    pub max_steps_per_turn: u32,
    /// 输入 token 的**提醒阈值**：超过它就在下一步注入一条提醒，让模型收尾。
    ///
    /// # 为什么是"提醒"而不是"截断"
    ///
    /// 实测（run26）：模型在 intel 阶段一个 turn 内调了 **115 次 `web_search`**，
    /// 输入累积到 **138,606 token**。每次调用都要重发全部历史 ⇒ 成本随
    /// 步数**超线性**增长，且最终会撞上模型上下文上限。
    ///
    /// 三条可能的处置：
    ///
    /// | 做法 | 问题 |
    /// |---|---|
    /// | 静默截断旧工具结果 | **丢审计**——报告引用的证据可能已被删；且模型不知道自己看不见什么 |
    /// | 硬性禁止再调工具 | 它可能正差一次检索就能产出交付物，被拦会卡死 |
    /// | **注入提醒**（本实现） | 模型自己决定何时收尾；代价是它可能忽视提醒 |
    ///
    /// 前两条都在**替模型做决定**，而它比我们更清楚还差什么。故取第三条：
    /// 把"上下文已经很重"这件事如实告诉它，并给出可操作的选项
    /// （先产出当前阶段的交付物，剩下的下一 turn 再补）。
    ///
    /// # 为什么阈值是 96k
    ///
    /// 主流模型的上下文窗口在 128k–200k 之间。96k 留出约 30% 余量给
    /// 后续步与产出，且远高于实测的"正常步"规模（早期步骤在 2k–20k）。
    pub context_warn_tokens: u64,
    /// 请求温度。focal_llm 等受控 prompt 必须 0（§7.4）。
    pub temperature: Option<f64>,
    pub max_output_tokens: Option<u32>,
    pub case_id: String,
    /// 思考量（`None` = 用供应商行为）。
    ///
    /// # 一个反直觉的实测结论（2026-09-18）
    ///
    /// 我原本以为"档位越高越贵"。**四次同设置会话的成本差 3 倍**
    /// （$0.0134 / $0.0097 / $0.0071 / $0.0043），而 `high` 与默认的差
    /// （$0.0060 vs $0.0085）**完全落在这个波动范围内**。
    ///
    /// 即：**档位不是成本的主导变量**。真正的成本杠杆是**一次会话要跑多少步**
    /// （上表里 10 步的会话比 3 步的贵 3 倍）——那取决于任务复杂度与模型收敛速度，
    /// 不取决于思考档。
    ///
    /// 故不必为省钱而调低它（调低换来的是质量不确定，而省下的钱在噪声里）。
    pub reasoning_effort: Option<String>,
}

impl LoopConfig {
    pub fn new(model: impl Into<String>, case_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            // 单 turn 步数上限。
            //
            // # 实测依据（2026-09-18）
            //
            // 早先设 24，而一次真实咨询里模型在**一个 turn 内**调了
            // 24 次 `participant_profile_update`（逐个字段更新画像）后触顶。
            // 那是**真实的工作模式**，不是失控——模型的工具调用粒度比预期细。
            //
            // 故上限应容纳"一次把画像填完"这类批量操作。64 的依据：
            // 实测观察到的最长单 turn 是 38 步（含检索、抓取、画像更新），
            // 留约 1.7 倍余量。
            //
            // ⚠️ 它仍必须是**有限的**：无上限会让一个陷入循环的 turn
            // 无限消耗配额，而那种失败比触顶难查得多（触顶有明确诊断）。
            max_steps_per_turn: 64,
            context_warn_tokens: 96_000,
            temperature: None,
            max_output_tokens: None,
            case_id: case_id.into(),
            reasoning_effort: None,
        }
    }
}

/// 循环错误（**结构性**错误——循环跑不起来，不是模型说错了话）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoopError {
    #[error("LLM 调用失败：{0}")]
    Llm(String),
    #[error("工具 `{tool}` 在阶段 `{phase}` 不可用：{reason}")]
    ToolRejected {
        tool: String,
        phase: String,
        reason: String,
    },
    #[error("工具参数不是合法 JSON：{0}")]
    BadToolArguments(String),
    #[error("步前闸门拒绝：{0}")]
    GateRejected(String),
}

impl From<LlmError> for LoopError {
    fn from(e: LlmError) -> Self {
        LoopError::Llm(e.to_string())
    }
}
impl From<PhaseGateError> for LoopError {
    fn from(e: PhaseGateError) -> Self {
        LoopError::GateRejected(e.to_string())
    }
}

/// Agent 核心循环。
///
/// 泛型参数 `T: Transport` 使**请求形状可在无网络下被断言**——
/// 这是本设计与"直接把 reqwest 焊进循环"的关键差别：后者只有在联调时
/// 才会发现"模型收到的工具集不对/温度没归零"。
pub struct AgentLoop<'a, T: Transport> {
    client: shangyi_llm::LlmClient<T>,
    registry: &'a ToolRegistry,
    executor: &'a mut dyn ToolExecutor,
    config: LoopConfig,
    /// 会话日志（追加式）。循环只追加，不改写历史。
    events: Vec<Event>,
    playbook: Option<&'a Playbook>,
    /// 时间源。可注入使日志内容在测试中可断言（见 [`crate::clock`]）。
    clock: Box<dyn Clock>,
    event_sink: Option<Box<dyn FnMut(&Event)>>,
    // Canonical conversation after the turn, excluding per-step instructions.
    // Kept privately for protocol replay, not displayed as model reasoning.
    last_turn_history: Option<Vec<InputItem>>,
    evidence_failures: std::collections::BTreeMap<String,u32>,
    last_evidence_gaps: Vec<crate::conditional::EvidenceGap>,
}

impl<'a, T: Transport> AgentLoop<'a, T> {
    pub fn new(
        client: shangyi_llm::LlmClient<T>,
        registry: &'a ToolRegistry,
        executor: &'a mut dyn ToolExecutor,
        config: LoopConfig,
    ) -> Self {
        Self {
            client,
            registry,
            executor,
            config,
            events: Vec::new(),
            playbook: None,
            // 默认系统时钟：日志时间戳要能表达"现实世界何时"。
            // 测试用 `with_clock` 换成确定性时钟。
            clock: Box::new(SystemClock),
            event_sink: None,
            last_turn_history: None,
            evidence_failures: Default::default(),
            last_evidence_gaps: Vec::new(),
        }
    }

    /// Append-only progress mirror; the case log remains the authoritative ledger.
    pub fn with_event_sink(mut self, sink: Box<dyn FnMut(&Event)>) -> Self {
        self.event_sink=Some(sink); self
    }

    pub fn take_turn_history(&mut self) -> Vec<InputItem> {
        self.last_turn_history.take().expect("finished turn must retain its conversation")
    }

    fn emit_event(&mut self, event: Event) {
        if let Some(sink)=&mut self.event_sink {sink(&event);}
        self.events.push(event);
    }

    /// 换用确定性时钟（测试用）。
    ///
    /// 不给它一个"默认就是假时钟"的开关：生产路径上忘接系统时钟，
    /// 结果是日志里全是 `seq:N` 而**没人会发现**——那比测试不便更糟。
    pub fn with_clock(mut self, clock: Box<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// 换用序列时钟（测试便利方法）。
    pub fn with_seq_clock(self) -> Self {
        self.with_clock(Box::new(SeqClock::new()))
    }

    /// 绑定剧本（工具白名单的第二层过滤，§7.3）。
    pub fn with_playbook(mut self, pb: &'a Playbook) -> Self {
        self.playbook = Some(pb);
        self
    }

    /// 已写入的日志（追加序）。
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// 取走日志（归档用）。
    pub fn into_events(self) -> Vec<Event> {
        self.events
    }

    /// 当前时刻（与写入日志的时间戳**同源**）。
    ///
    /// 暴露它使会话驱动器不必引入第二个时钟——两个时间源会让日志里的
    /// 时间无法相互比较，而审计恰恰要按时间对齐事件。
    pub fn now(&self) -> String {
        self.clock.now_iso8601()
    }

    pub fn begin_followup_task(&mut self) { self.evidence_failures.clear();self.last_evidence_gaps.clear();self.executor.begin_followup_task(); }
    pub fn record_user_input(&mut self, text: &str, actor: &str) {
        self.executor.record_user_input(text,actor);
        // A new message (including "still unknown") is not proof that a gap was resolved.
        // Changed committed artifacts get a new budget; a new task resets the ledger.
    }
    pub fn take_invalidated_artifacts(&mut self) -> Vec<crate::phase_graph::ArtifactKind> {
        self.executor.take_invalidated_artifacts()
    }

    /// 生成本阶段可用的工具 schema（阶段白名单 + 剧本白名单，§7.3 的两层过滤）。
    /// 把执行器的产物登记进阶段机（循环每步后调用）。
    ///
    /// **为什么由循环登记而不是执行器直接写阶段机**：执行器不该知道阶段机的存在
    /// ——那会让"引擎产物"与"阶段状态"互相依赖，两个权威纠缠在一起。
    /// 循环是唯一的协调点。
    fn sync_artifacts(&self, machine: &mut PhaseMachine) {
        for (kind, fingerprint) in self.executor.artifacts() {
            machine.record(kind, fingerprint);
        }
    }

    fn tool_schemas(&self, machine: &PhaseMachine) -> Vec<ToolSchema> {
        let phase = machine.phase();
        let mut specs = self.registry.tools_for_phase(phase.as_str());
        specs.retain(|s| machine.allows_task_tool(&s.name));
        // A confirmation control is only meaningful at this mode's actual
        // checkpoint. Do not offer it and then charge a model turn to reject it.
        specs.retain(|s| s.name != "checkpoint" || machine.graph().checkpoint_at(phase).is_some());
        if let Some(pb) = self.playbook {
            if !pb.tool_allowlist.is_empty() {
                specs.retain(|s| shangyi_tools::is_session_control(&s.name) || pb.allows_tool(&s.name));
            }
        }
        // 稳定顺序（注册表本身有序，此处再排一次以防未来换容器）——
        // 工具顺序变化会让"同一输入的请求体"变化，从而破坏可复现性。
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
            .into_iter()
            .map(|s| {
                // 参数 schema 取自 `shangyi-tools::schema`（设计文档 §7.2 的落地）。
                //
                // # 为什么这个取值方式重要（实测依据）
                //
                // 早先一律用 `empty_params()`（自由对象），模型**完全靠猜**
                // 参数结构——一次真实咨询里 `gamespec_build` 被调了 97 次，
                // 最后还**放弃了建模**并给用户编了个解释
                // （「params 被环境层吞掉」）。
                //
                // 现在：有 schema 的用 schema；没有的退回自由对象，
                // 并在**描述里如实标注**——模型据此知道"这个工具的参数面是开放的"，
                // 而不是以为漏了文档。
                let params = match shangyi_tools::schema::tool_schema(&s.name) {
                    Some(v) => v,
                    None => shangyi_tools::schema::freeform(),
                };
                ToolSchema::function(&s.name, &s.description, params)
            })
            .collect()
    }

    /// 执行一个 turn：从当前阶段出发，直到模型不再要工具、或必须停。
    ///
    /// `input` 是进入本 turn 的初始输入（用户消息 / checkpoint 答案 / 工具回灌）。
    pub fn run_turn(
        &mut self,
        machine: &mut PhaseMachine,
        turn: TurnId,
        mut input: Vec<InputItem>,
    ) -> Result<TurnOutcome, LoopError> {
        self.last_turn_history=None;
        self.emit_event(Event::TurnStarted {
            turn,
            at: self.now(),
        });

        let mut steps: u32 = 0;
        // 本 turn 内累计的工具调用数（用于区分「模型交付完了」与「模型不会用工具」）
        let mut tools_called_this_turn: u32 = 0;
        // 本步的阶段指引（每步重算，不入对话历史）
        let mut step_guidance: Vec<String> = Vec::new();
        // 上下文预警：**一经触发就在本 turn 的每步都带上**。
        //
        // 为什么不放在 `step_guidance` 里：那个 Vec 每步被重算覆盖
        // （见上面 Inject 分支的说明），提醒会在下一步消失——
        // 而"上下文重"是持续状态，不是一次性事件。
        //
        // 为什么不在每步都重新判断：已超阈值后每步都会超，
        // 重复判断只会产生同样的字符串。
        let mut context_warning: Option<String> = None;
        // 本 turn 是否产生过**任何成功的工具结果**（判断"能否继续"的依据）
        let mut turn_produced_result = false;
        // 同一工具的连续失败计数（工具名 → (次数, 已见错误首行摘要)）。
        // 用途见工具结果回灌处的「原地打转闸门」。
        let mut tool_fail_streak: std::collections::HashMap<String, (u32, Vec<String>)> =
            std::collections::HashMap::new();

        let outcome = loop {
            // ── 步前闸门（§7.4）──
            let decision = PreStepGate::new(machine).decide();
            match decision {
                PreStepDecision::Accept => {}
                PreStepDecision::Inject { notes } => {
                    // 指引**每步重新算、但不入历史**。
                    //
                    // ⚠️ 早先的写法是 `input.push(InputItem::system(n))`，而
                    // `input` 在整个 turn 内跨 step 复用——于是每步都追加一份，
                    // 实测第 4 步的请求里已经出现 3 组重复的阶段指引。
                    // 重复的 system 消息不只是浪费 token：它会让模型以为
                    // "阶段指引"被反复强调，从而过度服从（实测中它确实在
                    // 反复声明"我不会编造缺失的环节"）。
                    //
                    // 正确做法：指引属于**本步的请求**，不属于**对话历史**。
                    step_guidance = notes;
                }
                PreStepDecision::Reject { reason } => {
                    // 挂起等待用户 vs 硬失败：checkpoint 未结算是**挂起**，不是错误
                    if let Some(cp) = machine.graph().checkpoint_at(machine.phase()) {
                        let items = machine
                            .checkpoint_status(cp)
                            .and_then(|s| match s {
                                crate::phase_graph::CheckpointStatus::Open { items } => {
                                    Some(items.clone())
                                }
                                _ => None,
                            })
                            .unwrap_or_default();
                        if !items.is_empty() {
                            break TurnOutcome::AwaitingUser {
                                checkpoint: CheckpointRequest {
                                    id: cp,
                                    phase: machine.phase(),
                                    items,
                                    revision_requested: false,
                                },
                                steps,
                            };
                        }
                    }
                    break TurnOutcome::Failed {
                        reason: TurnEndReason::Error { detail: reason.clone() },
                        steps,
                        detail: reason,
                    };
                }
            }

            // ── 步数上限 ──
            if steps >= self.config.max_steps_per_turn {
                break TurnOutcome::Failed {
                    reason: TurnEndReason::MaxTokens,
                    steps,
                    detail: format!(
                        "单 turn 步数触顶（上限 {} 步）。这**不是**完成——分析没做完，只是没报错。\n\
                         常见原因：模型把一件可批量做的事拆成很多次调用\
                         （实测：一次咨询里它逐个字段更新画像，单 turn 用了 24+ 步）。\n\
                         可提高 `LoopConfig::max_steps_per_turn`，或把任务拆小。",
                        self.config.max_steps_per_turn
                    ),
                };
            }

            let step = StepId::new(turn, steps as u64);
            self.emit_event(Event::StepStarted {
                step,
                at: self.now(),
            });

            // ── 构造并发出请求 ──
            let schemas = self.tool_schemas(machine);
            // 历史 + 本步指引（指引不入历史，见上面 Inject 分支的说明）
            let mut req_input = input.clone();
            for n in &step_guidance {
                req_input.push(InputItem::system(n.clone()));
            }
            // 上下文预警**每步都带**（直到本 turn 结束）——它是"当前状态"，
            // 不是"一次性通知"。而 `step_guidance` 每步会被重算覆盖，
            // 故它必须放在**独立的变量**里，不能 push 进去（那样下一步就没了）。
            if let Some(w) = &context_warning {
                req_input.push(InputItem::system(w.clone()));
            }
            let mut req = LlmRequest::new(self.config.model.clone(), req_input)
                .with_tools(schemas);
            if let Some(t) = self.config.temperature {
                req = req.with_temperature(t);
            }
            if let Some(m) = self.config.max_output_tokens {
                req = req.with_max_tokens(m);
            }
            if let Some(e) = &self.config.reasoning_effort {
                req = req.with_reasoning_effort(e.clone());
            }
            // 工具全被阶段白名单滤掉时，别把"空工具列表"发出去——
            // 空列表 + tool_choice 会让请求语义含糊；显式设 none。
            if req.tools.is_empty() {
                req = req.with_tool_choice(ToolChoice::None);
            }

            let resp: LlmResponse = match self.client.complete(&req) {
                Ok(r) => r,
                Err(e) => {
                    // 网络/协议失败 ⇒ Error（不是 Blocked：Blocked 专指"外部数据源
                    // 不可用导致无法得出结论"，而这里是通信本身失败）
                    self.emit_event(Event::StepEnded {
                        step,
                        at: self.now(),
                    });
                    break TurnOutcome::Failed {
                        reason: TurnEndReason::Error {
                            detail: e.to_string(),
                        },
                        steps,
                        detail: format!("模型调用失败：{e}"),
                    };
                }
            };

            // ── 上下文预警 ──
            //
            // 见 `context_warn_tokens` 的文档：**不截断历史**（那会丢审计），
            // 而是把"上下文已经很重"如实告诉模型，让它自己决定收尾时机。
            //
            // 注入的是一条 `system` 级指引（与阶段指引同族，**不入历史**）——
            // 若入历史，提醒本身会越积越多，反而加速上下文膨胀。
            if resp.usage.input_tokens >= self.config.context_warn_tokens
                && context_warning.is_none()
            {
                context_warning = Some(format!(
                    "⚠️ **上下文已经很重**（本次调用输入 {} token，已超过提醒阈值 {}）。\n\
                     继续在同一步里加大段材料（检索结果、画像详情）会让后续每一步                     都重发全部历史——成本随步数**超线性**增长，且迟早撞上上下文上限。\n\
                     建议按此顺序处置：\n\
                     1. **先产出本阶段的交付物**（把它落到状态里，那样即使后续中断，\
                        这份成果也在）\n\
                     2. 剩余的信息缺口如实标注为「未取得」，**不要**为了补全而继续检索\n\
                     3. 若确实还差关键材料，用一个 turn 专门补它，而不是在本 turn 继续堆",
                    resp.usage.input_tokens, self.config.context_warn_tokens
                ));
            }

            // 记录用量（成本可观测性）——**在解析输出之前**，
            // 因为即使模型没给工具调用（例如能力不足），这次调用也已经花了钱。
            self.emit_event(Event::TokenUsage {
                step,
                model: if resp.model.is_empty() {
                    self.config.model.clone()
                } else {
                    resp.model.clone()
                },
                input_tokens: resp.usage.input_tokens,
                output_tokens: resp.usage.output_tokens,
                cached_input_tokens: resp.usage.cached_input_tokens,
                reasoning_tokens: resp.usage.reasoning_tokens,
                at: self.now(),
            });

            // ── 截断检查（**必须在解析输出之前**）──
            //
            // 输出触到上限时端点返回的是 HTTP 200 + `status: incomplete`，
            // 而 `arguments` 可能是**半截 JSON**。若不检查，用户看到的是
            // "JSON 解析失败"——**一个指向错误方向的诊断**：它会让人去查解析代码，
            // 而真正的原因是**预算不足**（实测：`high` 思考档下一次 256 token 的
            // 调用把 234 个花在思考上，参数剩 22 个 token，被截在半句）。
            //
            // 终止原因为 `MaxTokens`（不是 `Error`）：它表达的是"这次没做完"，
            // 而 §7.1 要求它与"出错"永久可区分。
            if resp.is_truncated() {
                self.emit_event(Event::StepEnded {
                    step,
                    at: self.now(),
                });
                break TurnOutcome::Failed {
                    reason: TurnEndReason::MaxTokens,
                    steps: steps + 1,
                    detail: format!(
                        "模型输出被截断（{}）——本次调用的输出未完成，其工具参数可能是半截 JSON。\
                         这**不是**解析问题，是输出预算不足。\
                         常见原因：思考档过高（思考 token 也占输出预算）或 max_output_tokens 过小。\
                         本步用量：输出 {} tokens（其中思考 {}）。",
                        resp.truncation_note(),
                        resp.usage.output_tokens,
                        resp.usage.reasoning_tokens
                    ),
                };
            }

            let calls = resp.function_calls();
            let text = resp.text();

            if calls.is_empty() {
                // Preserve the actual assistant answer across future turns, along
                // with opaque provider replay data where present. Never show that
                // replay data to the user or turn it into factual evidence.
                for item in &resp.output {
                    if let OutputItem::Reasoning { content, summary, .. } = item {
                        input.push(InputItem::reasoning(None, content.clone(), summary.clone()));
                    }
                }
                if !text.is_empty() {
                    input.push(InputItem::Message {role:"assistant".into(),content:text.clone()});
                }
                // ⚠️ 「没有工具调用」有**两种**截然不同的原因，而它们必须分开：
                //
                // | 原因 | 真相 | 处置 |
                // |---|---|---|
                // | 模型认为本步交付完了 | 正常收尾 | `Completed` |
                // | **模型不会用工具，改用说话** | **能力不足** | 响亮失败 |
                //
                // # 实测（run27，glm-4-flash）
                //
                // 一个 7-turn 的真实咨询里**只有 1 次工具调用**——其余 6 个 turn
                // 模型都在输出自然语言（"现在我将使用 ask_user 工具向用户提问。
                // ask_user {...}"），**把工具名与参数当正文写出来**。
                //
                // 而循环把每一次都判成 `Completed`：turn 正常结束、阶段不推进、
                // 于是它无限循环下去——**看起来一切正常**，只是永远不动。
                // 这是最坏的一类失效：没有报错，没有人知道它在空转。
                //
                // # 判据（保守：只在本阶段**必须**用工具时才判失败）
                //
                // 不能简单地说"没调工具就是失败"：确实有阶段（如 report 收尾）
                // 模型可以说一段总结就完成。故用**本阶段是否有交付工具**判断：
                // 若本阶段要求产物（`graph.produces(phase)` 非空）而模型一步都没
                // 调过工具，那它**不可能**产出那些产物 ⇒ 如实失败。
                let phase_produces = machine.graph().produces(machine.phase());
                let never_used_tools = tools_called_this_turn == 0;
                // ⚠️ **必须同时看"产物是否已齐备"**——只看"没调工具"会误杀。
                //
                // 实测踩到（我自己的第一版就是这样）：一个 runner 测试里模型回
                // `"ok"`，而阶段产物（gamespec）**已被前一步登记好**——
                // 那种情形下"不再调工具、直接收尾"是**完全合法的**。
                //
                // 正确的语义是：**该阶段要求产物，而执行器手里还没有**，
                // 且模型一步工具都没调 ⇒ 它不可能产出那些产物 ⇒ 才是故障。
                let have: std::collections::BTreeSet<_> = self
                    .executor
                    .artifacts()
                    .into_iter()
                    .map(|(k, _)| k)
                    .collect();
                let missing: Vec<_> = phase_produces
                    .iter()
                    .filter(|a| !have.contains(a))
                    .collect();
                if never_used_tools && !missing.is_empty() {
                    self.emit_event(Event::StepEnded {
                        step,
                        at: self.now(),
                    });
                    break TurnOutcome::Failed {
                        reason: TurnEndReason::Error {
                            detail: format!(
                                "模型在本阶段（`{}`）**一次工具调用都没有发起**，                                 但该阶段要求产出 {}。\n\
                                 它很可能把工具名与参数当正文写了出来                                 （实测：小模型在工具较多时会出现这种退化）。\n\
                                 本循环**不把这种情况当作完成**——那会让会话\
                                 无限循环而看不出问题。\n\
                                 建议：换用工具调用能力更强的模型，\
                                 或确认该模型的函数调用格式与端点约定一致。",
                                machine.phase().as_str(),
                                missing
                                    .iter()
                                    .map(|a| a.display_name())
                                    .collect::<Vec<_>>()
                                    .join("、")
                            ),
                        },
                        steps: steps + 1,
                        detail: format!(
                            "模型未发起任何工具调用，但阶段 `{}` 要求产出 {}。                             输出开头：{}",
                            machine.phase().as_str(),
                            missing
                                .iter()
                                .map(|a| a.display_name())
                                .collect::<Vec<_>>()
                                .join("、"),
                            truncate_for_display(text.trim(), 200)
                        ),
                    };
                }
                // 没有工具调用 = 模型认为本步交付了产出 ⇒ turn 结束
                self.emit_event(Event::StepEnded {
                    step,
                    at: self.now(),
                });
                break TurnOutcome::Completed {
                    output: text,
                    steps: steps + 1,
                };
            }
            tools_called_this_turn += calls.len() as u32;

            // ── 收齐本步的思维链（回放时要**每个 function_call 前各带一份**）──
            //
            // 实测：一个响应里若有 N 个 `function_call`，`input` 里必须在**每一个**
            // 之前各放一份 reasoning，只放一份会 400：
            //
            //   The `reasoning_text` in the thinking mode must be passed back to the API.
            //
            // 该结论由四组对照实验得出（同一份真实历史）：
            //
            // | 变体 | 结果 |
            // |---|---|
            // | 1 个 fc 对 + 1 份 reasoning | ✅ |
            // | 2 个 fc 对 + 1 份 reasoning | ❌ |
            // | 3 个 fc 对 + 1 份 reasoning | ❌ |
            // | 3 个 fc 对 + **每个前各 1 份** reasoning | ✅ |
            // | 3 个 fc 对 + 0 份 reasoning | ❌ |
            //
            // 注意错误消息**具有误导性**：它说的是 reasoning，但触发方式看似与
            // reasoning 无关（多一个工具调用就会失败）。故这批对照实验是必要的。
            let step_reasoning: Vec<InputItem> = resp
                .output
                .iter()
                .filter_map(|item| match item {
                    OutputItem::Reasoning { content, summary, .. } => Some(
                        InputItem::reasoning(None, content.clone(), summary.clone()),
                    ),
                    _ => None,
                })
                .collect();

            // ── 执行工具 ──
            //
            // 先做注册表校验：未知工具 / 阶段不允许 ⇒ 把**类型化原因**回灌给模型，
            // 而不是中止 turn。理由：模型调用一个本阶段不可用的工具是常见的、
            // 可自我纠正的失误；中止会浪费整个 turn。但**绝不静默忽略**——
            // 回灌的文本里含确切原因，日志里也有。
            let mut blocked: Option<String> = None;
            for (call_id, name, args) in calls {
                // **先规范化，再同时用于日志与执行**。
                //
                // 两处用同一份值是有意的：若日志记原始形态、执行用规范化形态，
                // 审计看到的参数与工具实际收到的参数就会**不一致**——那会让
                // "为什么这个工具这么算"变成无法回答的问题。
                let effective_args = normalize_arguments(args);

                // 回放该步的思维链——**每个调用前各一份**（见上面 `step_reasoning`
                // 的对照实验）。没有 reasoning 的响应（例如模型直接给文本）就不放。
                for r in &step_reasoning {
                    input.push(r.clone());
                }

                // **必须把模型的 `function_call` 项也放进下一轮的输入**。
                //
                // Responses API 在无状态调用下（我们没有传 `previous_response_id`）
                // 要求 `function_call_output` 与产生它的 `function_call` **成对出现**。
                // 只回灌输出会得到：
                //
                //   HTTP 400 No tool call found for tool output with call_id call_00_...
                //
                // 这个错误在离线测试里看不见（假传输层不校验配对），只有真跑
                // 一次才会暴露——所以它是一次性错误，但代价是整条链路不可用。
                input.push(InputItem::function_call(
                    call_id,
                    name,
                    effective_args.clone(),
                ));

                self.emit_event(Event::ToolCallStarted {
                    step,
                    call_id: call_id.to_string(),
                    name: name.to_string(),
                    arguments: effective_args.to_string(),
                    at: self.now(),
                });

                let outcome = if !machine.allows_task_tool(name) {
                    ExecOutcome::Failed("该工具不属于用户当前任务范围；仅查资料/算费用时不得扩大为完整咨询。未选择范围时先用 select_task_scope。".into())
                } else { match self.registry.check_call(
                    name,
                    machine.phase().as_str(),
                    self.playbook,
                ) {
                    Err(e) => ExecOutcome::Failed(e.to_string()),
                    Ok(_) => {
                        let call = ToolCall {
                            call_id: call_id.to_string(),
                            name: name.to_string(),
                            arguments: effective_args,
                        };
                        self.executor.execute(&call, machine.phase())
                    }
                }};

                let invalidated=self.executor.take_invalidated_artifacts();
                if !invalidated.is_empty() {
                    let from=machine.phase();
                    if let Some(to)=machine.invalidate_user_dependent_artifacts(&invalidated) {
                        self.emit_event(Event::PhaseRewound {from:from.as_str().into(),to:to.as_str().into(),
                            due_to:"依据或用户约束更新，旧决定产物已失效，需重新核对".into(),at:self.now()});
                    }
                }

                let outcome = match outcome {
                    ExecOutcome::TaskScopeSelected { scope, reason } => match machine.select_task_scope(scope) {
                        Ok(()) => {
                            self.emit_event(Event::TaskScopeSelected { scope, reason, at: self.now() });
                            ExecOutcome::Ok(serde_json::json!({"scope":scope,"phase":"intake","requires_game_analysis":!scope.is_brief()}).to_string())
                        }
                        Err(error) => ExecOutcome::Failed(error.to_string()),
                    },
                    ExecOutcome::ScopedTaskCompleted(result) => match if result.scope==crate::task_scope::TaskScope::Conditional {
                        machine.finish_conditional_task(result.clone())
                    } else {machine.finish_scoped_task(result.clone())} {
                        Ok(()) => {
                            let receipt = serde_json::to_string(&result).expect("task result is serializable");
                            self.emit_event(Event::ToolCallFinished { call_id:call_id.to_string(), outcome:ToolOutcome::Ok{output:receipt.clone()},at:self.now() });
                            input.push(InputItem::tool_output(call_id, receipt));
                            self.emit_event(Event::ScopedTaskCompleted { result:result.clone(),at:self.now() });
                            self.emit_event(Event::StepEnded { step,at:self.now() });
                            return Ok(self.finish(turn,input,TurnOutcome::Completed{output:result.summary,steps:steps+1}));
                        }
                        Err(error) => ExecOutcome::Failed(error.to_string()),
                    },
                    ExecOutcome::FollowupTaskRequested { reason } => match machine.begin_followup_task() {
                        Ok(()) => {
                            self.begin_followup_task();
                            let _ = self.executor.take_invalidated_artifacts();
                            self.emit_event(Event::FollowupTaskStarted { at: self.now() });
                            ExecOutcome::Ok(serde_json::json!({
                                "reopened": true,
                                "phase": machine.phase().as_str(),
                                "reason": reason,
                            }).to_string())
                        }
                        Err(error) => ExecOutcome::Failed(error.to_string()),
                    },
                    ExecOutcome::InsufficientEvidence { gaps } => {
                        if gaps.is_empty() {ExecOutcome::Failed("缺少类型化证据缺口，不能转入条件分析".into())}
                        else {
                            self.last_evidence_gaps=gaps.clone();
                            let mut artifacts=self.executor.artifacts();artifacts.sort_by(|a,b|a.0.key().cmp(b.0.key()));
                            // Renaming action profiles or rewriting a missing-payoff error is
                            // not progress. Only changed committed evidence/artifacts earns a
                            // new modelling attempt budget inside the same task.
                            let identity=format!("insufficient-evidence|{:?}",artifacts);
                            let count=self.evidence_failures.entry(identity).or_default();*count+=1;
                            if *count>=3 {
                                let receipt=serde_json::to_string(&gaps).expect("gap serialization");
                                self.emit_event(Event::ToolCallFinished {call_id:call_id.to_string(),outcome:ToolOutcome::InsufficientEvidence{gaps:gaps.clone()},at:self.now()});
                                input.push(InputItem::tool_output(call_id,format!("关键证据连续未取得，停止重复尝试；保留原模型与参与者。{receipt}")));
                                let result=self.executor.conditional_fallback(&gaps)
                                    .and_then(|result|machine.finish_conditional_task(result.clone()).map(|()|result).map_err(|e|e.to_string()));
                                self.emit_event(Event::StepEnded {step,at:self.now()});
                                return match result {
                                    Ok(result)=>{
                                        self.emit_event(Event::ScopedTaskCompleted {result:result.clone(),at:self.now()});
                                        Ok(self.finish(turn,input,TurnOutcome::Completed{output:result.summary,steps:steps+1}))
                                    },
                                    Err(detail)=>Ok(self.finish(turn,input,TurnOutcome::Blocked{detail:format!("相同关键证据缺口已达三次，已停止重复调用；条件交付未成立：{detail}"),steps:steps+1})),
                                };
                            }
                            ExecOutcome::InsufficientEvidence { gaps }
                        }
                    },
                    other => other,
                };

                // 回退提议：记工具结果后挂起，等用户裁决。
                // **不在此处执行回退**——那是用户的决定（§7.5）。
                let outcome = match outcome {
                    ExecOutcome::RewindRequest { target, reason } => {
                        self.emit_event(Event::ToolCallFinished {
                            call_id: call_id.to_string(),
                            outcome: ToolOutcome::Ok {
                                output: format!(
                                    "PENDING_REWIND_DECISION:{}（等用户裁决）",
                                    target.as_str()
                                ),
                            },
                            at: self.now(),
                        });
                        self.emit_event(Event::StepEnded { step, at: self.now() });
                        return Ok(self.finish(turn, input, TurnOutcome::AwaitingRewindDecision {
                            target,
                            reason,
                            steps: steps + 1,
                        }));
                    }
                    other => other,
                };

                let (feed, log_outcome) = match outcome {
                    ExecOutcome::TaskScopeSelected { .. }
                    | ExecOutcome::ScopedTaskCompleted(_)
                    | ExecOutcome::FollowupTaskRequested { .. } => {
                        unreachable!("scope outcomes handled before ordinary tool output")
                    }
                    ExecOutcome::InsufficientEvidence { gaps } => (
                        format!("关键证据未取得：{}。不能填零、删除对手或改成背景方以通过校验；可补真实来源，或调用 conditional_analysis 正式交付已有费用、条件比较及信息收集步骤。相同缺口最多三次。",serde_json::to_string(&gaps).expect("gap serialization")),
                        ToolOutcome::InsufficientEvidence {gaps},
                    ),
                    ExecOutcome::Ok(s) => {
                        turn_produced_result = true;
                        (
                        format!("工具 `{name}` 的结果：\n{s}"),
                        ToolOutcome::Ok { output: s },
                    )
                    }
                    // 失败记为 `Failed`——**不得**记成 `Ok`（见 `ToolOutcome` 文档）
                    ExecOutcome::Failed(e) => (
                        format!(
                            "工具 `{name}` 调用失败：{e}\n\
                             （这是可纠正的错误：检查参数或改用本阶段允许的工具后重试。）"
                        ),
                        ToolOutcome::Failed {
                            error: e,
                        },
                    ),
                    ExecOutcome::Blocked(d) => {
                        // "无法评估"——必须与"评估为负面"区分开（见 ExecOutcome 文档）
                        if !machine.task_scope().is_some_and(|scope|scope.is_brief()) {
                            blocked = Some(format!("工具 `{name}`：{d}"));
                        }
                        (
                            format!(
                                "工具 `{name}` 无法执行：{d}\n\
                                 （这是**外部依赖不可用**，不是结论。请勿用推测填补该缺口；\
                                 资料/费用短任务应把该缺口列入 complete_task.missing 并如实交付已知部分；完整咨询如无法绕过则以 blocked 结束。）"
                            ),
                            // 记为 `Blocked`，**不是** `Unknown`：我们完全知道
                            // 发生了什么（后端不可用），也知道它没产生副作用。
                            ToolOutcome::Blocked { detail: d },
                        )
                    }
                    ExecOutcome::AsksUser(questions) => {
                        // 采集问题：先记函数调用与结果（可审计），再挂起。
                        // 用户的答案将作为下一 turn 的输入（不是产物）——
                        // 采集**不**改阶段机状态，故此处也不碰 checkpoint。
                        self.emit_event(Event::ToolCallFinished {
                            call_id: call_id.to_string(),
                            outcome: ToolOutcome::Ok {
                                output: format!("PENDING_USER_ANSWER:{}", questions.len()),
                            },
                            at: self.now(),
                        });
                        self.emit_event(Event::StepEnded {
                            step,
                            at: self.now(),
                        });
                        return Ok(self.finish(turn, input, TurnOutcome::AwaitingAnswers {
                                questions,
                                steps: steps + 1,
                            },
                        ));
                    }
                    ExecOutcome::RewindRequest { target, reason } => {
                        // 回退提议：记工具结果后挂起，等用户裁决。
                        // **不在此处执行回退**——那是用户的决定（§7.5），
                        // 因为它会作废已完成的工作。
                        self.emit_event(Event::ToolCallFinished {
                            call_id: call_id.to_string(),
                            outcome: ToolOutcome::Ok {
                                output: format!(
                                    "PENDING_REWIND_DECISION:{}（等用户裁决）",
                                    target.as_str()
                                ),
                            },
                            at: self.now(),
                        });
                        self.emit_event(Event::StepEnded { step, at: self.now() });
                        return Ok(self.finish(turn, input, TurnOutcome::AwaitingRewindDecision {
                                target,
                                reason,
                                steps: steps + 1,
                            },
                        ));
                    }
                    ExecOutcome::NeedsUserInput(req) => {
                        // checkpoint 工具：把提问件挂到阶段机上，再退出循环。
                        //
                        // # 三种既有状态下的正确行为（这里的顺序是承重的）
                        //
                        // | 现状 | 行为 |
                        // |---|---|
                        // | 已批准 **且** 产物未变 | **幂等 no-op** —— 告知已确认，不重开 |
                        // | 已批准 **但** 产物已改 | 重开（须重新确认，§7.7 第 1633 行） |
                        // | 未打开 / 被拒 | 正常打开 |
                        //
                        // ⚠️ 第一行是实测暴露的缺陷：早先的实现**无条件重开**，
                        // 于是模型每轮抓到新信息就改产物 → 触发重开 → 再确认 → 又改…
                        // 实测跑了 4 轮 cp1（14/12/25/15 项）后以
                        // 「checkpoint cp1 已结算（approved），不得重复结算」失败。
                        //
                        // 修法：**只有指纹真的变了才重开**。指纹相同的重复调用
                        // 是幂等的（模型可能在同一 turn 内多次要求确认同一件事，
                        // 那不是错误）。
                        let already_approved_same = match machine.checkpoint_status(req.id) {
                            Some(crate::phase_graph::CheckpointStatus::Approved {
                                fingerprint,
                                ..
                            }) => *fingerprint == machine.artifact_fingerprint(),
                            _ => false,
                        };
                        if already_approved_same {
                            let output = format!(
                                "ALREADY_APPROVED:{}（同内容已确认过，无需重复）",
                                req.id.as_str()
                            );
                            self.emit_event(Event::ToolCallFinished {
                                call_id: call_id.to_string(),
                                outcome: ToolOutcome::Ok {
                                    output: output.clone(),
                                },
                                at: self.now(),
                            });
                            // ⚠️ **必须把结果回灌进对话历史**，不能只 `continue`。
                            //
                            // 实测缺陷：早先这里 `continue` 直接进下一轮，
                            // 而 `input` 里只有该调用的 `function_call` 项、
                            // 没有配对的 `function_call_output`。下一轮请求因此被
                            // 端点拒绝：
                            //   `400 No tool output found for tool call call_00_...`
                            //
                            // 这与"成对回放"是**同一条规则**的两次应用：
                            // Responses API 要求每个 `function_call` 都必须有
                            // 配对的 output（见 `InputItem::function_call` 的文档）。
                            // 任何"提前退出本步"的路径都要记得补它——
                            // 这正是把回灌放在 `continue` 之前的原因。
                            input.push(InputItem::tool_output(call_id, output));
                            continue;
                        }

                        // 已批准但内容变了 ⇒ 重开（须重新确认）
                        let approved_changed = matches!(
                            machine.checkpoint_status(req.id),
                            Some(crate::phase_graph::CheckpointStatus::Approved { .. })
                        );
                        if approved_changed {
                            self.emit_event(Event::Warning {
                                message: format!(
                                    "阶段 `{}` 的 {} 已在批准后被改动，须重新确认。",
                                    machine.phase(),
                                    req.id
                                ),
                                at: self.now(),
                            });
                            if let Err(e) =
                                machine.reopen_checkpoint(req.id, req.items.clone())
                            {
                                self.emit_event(Event::StepEnded { step, at: self.now() });
                                return Ok(self.finish(turn, input, TurnOutcome::Failed {
                                    reason: TurnEndReason::Error {
                                        detail: format!("无法重新打开 checkpoint：{e}"),
                                    },
                                    steps: steps + 1,
                                    detail: format!("无法重新打开 checkpoint：{e}"),
                                }));
                            }
                            self.emit_event(Event::CheckpointOpened {
                                id: req.id.as_str().to_string(),
                                phase: req.phase.as_str().to_string(),
                                items: req.items.iter().map(|i| i.content.clone()).collect(),
                                at: self.now(),
                            });
                            self.emit_event(Event::ToolCallFinished {
                                call_id: call_id.to_string(),
                                outcome: ToolOutcome::Ok {
                                    output: format!(
                                        "REOPENED:{}（产物在批准后被改动，须重新确认）",
                                        req.id.as_str()
                                    ),
                                },
                                at: self.now(),
                            });
                            self.emit_event(Event::StepEnded { step, at: self.now() });
                            return Ok(self.finish(turn, input, TurnOutcome::AwaitingUser {
                                checkpoint: req,
                                steps: steps + 1,
                            }));
                        }

                        if let Err(e) = machine.open_checkpoint(req.clone()) {
                            self.emit_event(Event::StepEnded {
                                step,
                                at: self.now(),
                            });
                            self.emit_event(Event::ToolCallFinished {
                                call_id: call_id.to_string(),
                                outcome: ToolOutcome::Ok {
                                    output: format!("CHECKPOINT_REJECTED: {e}"),
                                },
                                at: self.now(),
                            });
                            return Ok(self.finish(turn, input, TurnOutcome::Failed {
                                reason: TurnEndReason::Error {
                                    detail: format!("无法打开 checkpoint：{e}"),
                                },
                                steps: steps + 1,
                                detail: format!("无法打开 checkpoint：{e}"),
                            }));
                        }
                        self.emit_event(Event::CheckpointOpened {
                            id: req.id.as_str().to_string(),
                            phase: req.phase.as_str().to_string(),
                            items: req.items.iter().map(|i| i.content.clone()).collect(),
                            at: self.now(),
                        });
                        self.emit_event(Event::ToolCallFinished {
                            call_id: call_id.to_string(),
                            outcome: ToolOutcome::Ok {
                                output: "PENDING_USER_INPUT".into(),
                            },
                            at: self.now(),
                        });
                        self.emit_event(Event::StepEnded {
                            step,
                            at: self.now(),
                        });
                        // turn 的收尾由循环末尾统一追加（那里也覆盖这条路）
                        return Ok(self.finish(turn, input, TurnOutcome::AwaitingUser {
                            checkpoint: req,
                            steps: steps + 1,
                        }));
                    }
                };

                self.emit_event(Event::ToolCallFinished {
                    call_id: call_id.to_string(),
                    outcome: log_outcome.clone(),
                    at: self.now(),
                });
                if machine.task_scope()==Some(crate::task_scope::TaskScope::Consultation)
                    && matches!(name,"gamespec_build"|"gamespec_validate"|"solve_equilibria") {
                    if let ToolOutcome::Failed {error}=&log_outcome {
                        let mut artifacts=self.executor.artifacts();artifacts.sort_by(|a,b|a.0.key().cmp(b.0.key()));
                        let identity=format!("model-error:{name}:{}|{:?}",error.lines().next().unwrap_or_default(),artifacts);
                        let count=self.evidence_failures.entry(identity).or_default();*count+=1;
                        if *count>=3 {
                            input.push(InputItem::tool_output(call_id,feed));
                            let known_gaps=self.last_evidence_gaps.clone();
                            let result=self.executor.conditional_fallback(&known_gaps)
                                .and_then(|r|machine.finish_conditional_task(r.clone()).map(|()|r).map_err(|e|e.to_string()));
                            self.emit_event(Event::StepEnded {step,at:self.now()});
                            return match result {
                                Ok(result)=>{self.emit_event(Event::ScopedTaskCompleted {result:result.clone(),at:self.now()});
                                    Ok(self.finish(turn,input,TurnOutcome::Completed{output:result.summary,steps:steps+1}))},
                                Err(reason)=>Ok(self.finish(turn,input,TurnOutcome::Blocked{detail:format!("同一建模错误且产物未变化已重复三次，停止原地重试：{error}。条件交付仍缺依据：{reason}"),steps:steps+1})),
                            };
                        }
                    }
                }
                // ── 原地打转闸门 ──
                //
                // # 为什么需要（实测：一个 turn 里 61 次调用、60 次失败）
                //
                // negotiation/scenario 那次，模型反复重发 `gamespec_build`：
                // 8 类不同的形状错误轮着试（信封套错、子对象字符串化、
                // `meta` 里放了数组、参数来源是 `assumed`…），
                // 每一步都在耗钱，而**每一次的报错它都收到了**——
                // 只是没人告诉它"你已经试了很多次，换条路"。
                //
                // 单靠步数上限（64 步）兜底太晚：等它触发时整个 turn 已经废了，
                // 用户看到的是一句"步数触顶"，而不是"卡在哪个错误上"。
                //
                // 故在这里做**早停提示**：同一工具连错 3 次，就把已见错误
                // 摘要一并回灌，并明确要求换路。注意这不是硬中止——
                // 模型仍可以继续（有些失败确实需要多试几次才能摸到形状），
                // 但从此每一步都看得见"我在打转"。
                let streak_note = match &log_outcome {
                    ToolOutcome::Ok { .. } => {
                        tool_fail_streak.remove(name);
                        None
                    }
                    ToolOutcome::Failed { error } => {
                        let entry = tool_fail_streak
                            .entry(name.to_string())
                            .or_insert_with(|| (0u32, Vec::new()));
                        entry.0 += 1;
                        let first = error
                            .lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(140)
                            .collect::<String>();
                        if !entry.1.contains(&first) && entry.1.len() < 6 {
                            entry.1.push(first);
                        }
                        if entry.0 >= 3 {
                            let (n, seen) = (entry.0, entry.1.clone());
                            let mut s = format!(
                                "\n\n⚠️ **`{name}` 已连续失败 {n} 次**——\
                                 继续用同样的思路重试不会成功（每一次都在消耗用户的钱）。\n\
                                 已见到的错误（各不相同的话，说明你在换写法试错）：\n"
                            );
                            for (i, e) in seen.iter().enumerate() {
                                s.push_str(&format!("  {}. {e}\n", i + 1));
                            }
                            s.push_str(
                                "请**停下来换一条路**，从下面三选一：\n\
                                 1. **换交付方式**：把一件事拆成多次调用做\
                                    （如先建最小可解的博弈，再用别的工具增量补参数）\n\
                                 2. **换工具**：本阶段可能另有一个工具能达成同样目的\
                                    （工具清单见阶段指引）\n\
                                 3. **如实收尾**：若确实卡住了，用自然语言把卡点\
                                    告诉用户并结束本阶段——\n\
                                    「做不出来」比「假装在做」有用得多。\n\
                                 若报错里已经写明了具体形状（如「缺 X 字段」「X 要的是容器」），\
                                 **先按那句话改**，不要继续试新写法。",
                            );
                            Some(s)
                        } else {
                            None
                        }
                    }
                    _ => None,
                };
                input.push(InputItem::tool_output(
                    call_id,
                    match streak_note {
                        Some(note) => format!("{feed}{note}"),
                        None => feed,
                    },
                ));
            }

            // 工具可能产出了新产物（画像、GameSpec、解集…）⇒ 登记进阶段机，
            // 使下一轮的闸门能看到它们
            self.sync_artifacts(machine);

            self.emit_event(Event::StepEnded {
                step,
                at: self.now(),
            });
            steps += 1;

            // ── 外部阻塞的终止判据 ──
            //
            // # 早先的实现是错的（实测暴露）
            //
            // 它写的是"只要本步有任一工具返回 `Blocked`，立即终止整个 turn"。
            // 后果：`web_search` 没配后端时，模型刚试一次检索就**整个 turn 被终止**，
            // 哪怕它还有 `web_fetch`、`ask_user` 等能用的工具、也还有话要说。
            // 实测那次会话正是这样卡死的（模型连试两次 web_search 后被杀）。
            //
            // # 设计文档的判据（§7.1 的表格）
            //
            // 原文是「**数据源全部不可用，无法继续** intel」——
            // 关键词是**全部**与**无法继续**，不是"任一工具不可用"。
            //
            // # 故此处按文档修正
            //
            // 只在**本 turn 一次成功调用都没有**（全部尝试都阻塞/失败）时，
            // 才判定"无法继续"。只要还有成功调用，就说明这条链路能走，
            // 阻塞项已作为**可读的反馈**回灌给模型（它可以选择绕过或如实标注未取得）。
            let any_success = turn_produced_result;
            if let Some(detail) = blocked {
                if !any_success {
                    break TurnOutcome::Blocked { detail, steps };
                }
                // 有成功调用 ⇒ **不终止**：让模型继续，阻塞项已在回灌文本里说明。
                // 这使"部分数据源不可用"不再等于"整个流程停下"。
                self.emit_event(Event::Warning {
                    message: format!(
                        "本步有工具因外部依赖不可用而未取得（{detail}），\
                         但同一步内另有成功调用，故不终止 turn。\
                         缺口已回灌给模型，须在产出中如实标注为「未取得」。"
                    ),
                    at: self.now(),
                });
            }
        };

        Ok(self.finish(turn, input, outcome))
    }

    /// 统一收尾：追加 `TurnEnded`。**所有**退出路径都必须经过它——
    /// 少记一条终止事件，日志就无法回答"这个 turn 是怎么结束的"（§7.1）。
    fn finish(&mut self, turn: TurnId, mut history: Vec<InputItem>, outcome: TurnOutcome) -> TurnOutcome {
        // Suspending for a question/review returns before the ordinary tool-output
        // append. Complete those protocol pairs with the actual logged outcome;
        // pending confirmation must never be presented as an approval.
        let mut open_calls:Vec<String> = Vec::new();
        for item in &history {
            match item {
                InputItem::FunctionCall { call_id, .. } => open_calls.push(call_id.clone()),
                InputItem::FunctionCallOutput { call_id, .. } => {
                    if let Some(index)=open_calls.iter().position(|id| id==call_id) {open_calls.remove(index);}
                }
                _ => {}
            }
        }
        for call_id in open_calls {
            let start=self.events.iter().rposition(|event|matches!(event,Event::ToolCallStarted{call_id:id,..} if id==&call_id));
            let logged=start.and_then(|index|self.events[index+1..].iter().find_map(|event|match event {
                Event::ToolCallFinished{call_id:id,outcome,..} if id==&call_id => Some(outcome),
                _ => None,
            }));
            let output=logged.map(|event|serde_json::to_string(event).expect("tool outcome is serializable"))
                .unwrap_or_else(||"调用中断，未取得完成结果；不能当作成功或用户确认。".to_owned());
            history.push(InputItem::tool_output(call_id,output));
        }
        self.last_turn_history=Some(history);
        self.emit_event(Event::TurnEnded {
            turn,
            reason: outcome.end_reason(),
            at: self.now(),
        });
        outcome
    }
}

/// 截断到不超过 `max` 字符且不切坏 UTF-8（仅用于**错误信息**里的摘要）。
///
/// 与 `domain_exec::truncate_str` 分开：那个按字节、服务于工具输出；
/// 这个按**字符**、服务于给用户看的错误摘要。两者需求不同，
/// 而共用会让其中一方被迫接受另一方的语义。
fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…（已截断）")
}

/// 把工具调用参数规范化为 JSON 对象。
///
/// # 为什么需要它
///
/// Responses API 的 `arguments` 在不同实现间形态不一：实测的 DeepSeek 响应里
/// 它是**对象**（`{"city":"东京"}`），而 OpenAI 风格的一部分实现给的是
/// **JSON 字符串**（`"{\"city\":\"东京\"}"`）。
///
/// 若不规范化，字符串形态会让每个工具都拿到一个字符串——`args.get("query")`
/// 返回 `None`，于是**每一个**工具调用都以"缺少参数"失败。那是一种极难定位的
/// 全盘失效：错误消息合理（确实"缺参数"）、日志正常、只有换 provider 才复现。
///
/// 规范化策略：是字符串就尝试解析成 JSON；解析失败则**原样返回**
/// （让工具自己报"参数非法"，而不是在这里 panic 或静默丢弃内容）。
fn normalize_arguments(args: &serde_json::Value) -> serde_json::Value {
    if let serde_json::Value::String(s) = args {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(s) {
            return parsed;
        }
    }
    args.clone()
}

/// 工具参数 schema 占位。
///
impl fmt::Display for ToolCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::consultation::ConsultationMode;
    use crate::phase_graph::{
        ArtifactKind, CheckpointId, CheckpointItem, CheckpointStatus,
    };
    use crate::domain_exec::DomainExecutor;
    use shangyi_kb::TheoryLibrary;
    use shangyi_playbooks::{Playbook, PlaybookMeta};
    use shangyi_llm::{Endpoint, LlmClient};
    use std::cell::RefCell;
    use std::sync::Arc;

    // ── 假传输层：记录请求、回放响应 ──

    struct FakeTransport {
        responses: RefCell<Vec<(u16, String)>>,
        seen: RefCell<Vec<serde_json::Value>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                responses: RefCell::new(responses),
                seen: RefCell::new(Vec::new()),
            }
        }
        fn requests(&self) -> Vec<serde_json::Value> {
            self.seen.borrow().clone()
        }
    }

    impl Transport for FakeTransport {
        fn post_json(
            &self,
            _url: &str,
            _headers: &[(String, String)],
            body: &serde_json::Value,
        ) -> Result<(u16, String), LlmError> {
            self.seen.borrow_mut().push(body.clone());
            let mut r = self.responses.borrow_mut();
            if r.is_empty() {
                return Err(LlmError::Transport("fake transport 的响应已耗尽".into()));
            }
            Ok(r.remove(0))
        }
    }

    /// 供**其他模块的测试**使用的单调用响应构造器。
    ///
    /// 跨模块可见是刻意的：runner 的测试要脚本化"模型调了工具"的行为，
    /// 而重写一份响应构造器会让两处的 wire 形状可能分叉。
    pub(crate) fn response_with_one_call(id: &str, name: &str, args: &str) -> String {
        response_with_calls(&[(id, name, args)])
    }

    fn response_with_calls(calls: &[(&str, &str, &str)]) -> String {
        let items: Vec<serde_json::Value> = calls
            .iter()
            .map(|(id, name, args)| {
                serde_json::json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": args,
                })
            })
            .collect();
        serde_json::json!({
            "id": "resp_x",
            "status": "completed",
            "output": items,
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
        .to_string()
    }

    /// 上下文预警：**不截断历史，而是告知模型**。
    ///
    /// 来历：run26 里模型在 intel 阶段单 turn 调了 115 次 `web_search`，
    /// 输入累积到 138,606 token。每次调用都要重发全部历史 ⇒ 成本随步数
    /// **超线性**增长，且迟早撞上上下文上限。
    ///
    /// 三条可能处置里选了"提醒"：静默截断会**丢审计**（报告引用的证据
    /// 可能已被删，且模型不知道自己看不见什么）；硬性禁止调工具会**卡死**
    /// （它可能正差一次检索就能产出交付物）。前两条都在替模型做决定，
    /// 而它比我们更清楚还差什么。
    #[test]
    fn warns_the_model_when_context_is_heavy() {
        let (c, probe) = client_with_probe(vec![
            (
                200,
                response_with_calls_tokens(&[("c1", "vn_map", "{}")], 200_000),
            ),
            (200, text_response("好")),
        ]);
        let theory = TheoryLibrary::new();
        let pb = Playbook {
            meta: PlaybookMeta {
                id: "p".into(),
                name: "n".into(),
                description: String::new(),
                order: 0,
                version: 1,
            },
            applicable_when: vec![],
            fails_when: vec![],
            required_inputs: vec![],
            value_net: None,
            intake: vec![],
            models: vec![],
            selectors: Default::default(),
            tool_allowlist: vec![],
            report_vars: Default::default(),
        };
        let mut ex = DomainExecutor::new(&theory, &pb);
        let reg = ToolRegistry::standard();
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        let _ = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]);

        // 提醒是否真的进了请求 —— 看**请求体**（FakeTransport 记录了它）
        let bodies: Vec<String> = probe
            .requests()
            .iter()
            .map(|r| r.to_string())
            .collect();
        let joined = bodies.join("\n");
        assert!(
            joined.contains("上下文已经很重"),
            "输入 200k token（远超 96k 阈值）时必须注入上下文提醒。请求数={}",
            bodies.len()
        );
        assert!(
            joined.contains("先产出本阶段的交付物"),
            "提醒里必须含**可操作的下一步**——只说「上下文重」会让模型不知所措"
        );
    }

    /// **未超阈值时不打扰**——否则提醒会变成噪音而模型开始忽略它。
    #[test]
    fn does_not_warn_below_threshold() {
        let (c, probe) = client_with_probe(vec![
            (
                200,
                response_with_calls_tokens(&[("c1", "vn_map", "{}")], 1_000),
            ),
            (200, text_response("完成")),
        ]);
        let theory = TheoryLibrary::new();
        let pb = Playbook {
            meta: PlaybookMeta {
                id: "p".into(),
                name: "n".into(),
                description: String::new(),
                order: 0,
                version: 1,
            },
            applicable_when: vec![],
            fails_when: vec![],
            required_inputs: vec![],
            value_net: None,
            intake: vec![],
            models: vec![],
            selectors: Default::default(),
            tool_allowlist: vec!["vn_map".into()],
            report_vars: Default::default(),
        };
        let mut ex = DomainExecutor::new(&theory, &pb);
        let reg = ToolRegistry::standard();
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        let _ = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]);
        let joined: String = probe
            .requests()
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !joined.contains("上下文已经很重"),
            "1k token 远低于阈值，不应注入提醒"
        );
    }

    /// 提醒**一经触发，本 turn 的后续每步都带**——它是持续状态，不是一次性通知。
    ///
    /// # 我一开始把这条测试写错了
    ///
    /// 初版断言"只出现一次"，理由是"重复提醒会变成噪音"。**理由对，但对象错了**：
    ///
    /// - 会变成噪音的是**同一份提示在对话历史里越积越多**
    ///   （那正是 `step_guidance` 早先的缺陷——每步追加一份，第 4 步请求里
    ///   已有 3 组重复）
    /// - 而"每步的请求里各带一份"不会累积：它是 `system` 级指引、**不入历史**
    ///   （见 `Inject` 分支的说明）
    ///
    /// 若只提醒一次，第 2 步之后模型就"忘了"上下文已经很重——而它仍需决定
    /// 是否继续堆材料。故正确行为是：**触发后本 turn 每步都带**。
    ///
    /// 这条测试因此断言两件事：出现过（≥1），且**没有累积到历史里**。
    #[test]
    fn warns_every_step_once_triggered_but_does_not_accumulate() {
        let (c, probe) = client_with_probe(vec![
            (
                200,
                response_with_calls_tokens(&[("c1", "vn_map", "{}")], 200_000),
            ),
            (
                200,
                response_with_calls_tokens(&[("c2", "vn_map", "{}")], 210_000),
            ),
            (200, text_response("完成")),
        ]);
        let theory = TheoryLibrary::new();
        let pb = Playbook {
            meta: PlaybookMeta {
                id: "p".into(),
                name: "n".into(),
                description: String::new(),
                order: 0,
                version: 1,
            },
            applicable_when: vec![],
            fails_when: vec![],
            required_inputs: vec![],
            value_net: None,
            intake: vec![],
            models: vec![],
            selectors: Default::default(),
            // ⚠️ 两个都必须对，否则 turn 在第 1 步就结束，"多步"的断言跑不到：
            // ① 白名单为空 ⇒ 所有工具被拒
            // ② 选了需要外部资源的工具（如 `kb_retrieve` 需要知识库）
            //    ⇒ 它会 `Blocked`，而 Blocked 会**终止整个 turn**
            //    （实测：请求数只有 1，而测试期待 3）
            //  `vn_map` 不依赖外部资源，是这类测试的合适选择。
            tool_allowlist: vec!["vn_map".into()],
            report_vars: Default::default(),
        };
        let mut ex = DomainExecutor::new(&theory, &pb);
        let reg = ToolRegistry::standard();
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        let _ = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]);
        let reqs = probe.requests();
        let count = probe
            .requests()
            .iter()
            .filter(|r| r.to_string().contains("上下文已经很重"))
            .count();
        assert!(
            count >= 1,
            "超阈值后必须出现提醒（否则模型不知道上下文已重）"
        );
        // **关键**：提醒不进历史 ⇒ 同一份提醒在**单个请求里只出现一次**。
        // 若有人在某步把它 push 进了 `input`，某一请求里会出现多份——
        // 那才是会累积的那类缺陷。
        for (i, r) in probe.requests().iter().enumerate() {
            let txt = r.to_string();
            let n = txt.matches("上下文已经很重").count();
            assert!(
                n <= 1,
                "第 {i} 个请求里出现了 {n} 份同一提醒——它会随步数累积。\
                 提醒必须是「本步请求独有的 system 指引」，不入对话历史。"
            );
        }
    }

    /// **模型只用文字说话、从不调工具 ⇒ 必须响亮失败，不能当完成。**
    ///
    /// # 这条测试守的是什么（run27 的真实事故）
    ///
    /// 一次 7-turn 的 GLM 咨询里**只有 1 次工具调用**——其余 6 个 turn 模型
    /// 都在输出自然语言（"现在我将使用 `ask_user` 工具向用户提问。ask_user {...}"），
    /// **把工具名与参数当正文写出来**。
    ///
    /// 而循环把每一次都判成 `Completed`：turn 正常结束、阶段不推进、
    /// 无限循环——**看起来一切正常，只是永远不动**。
    /// 这是最坏的一类失效：没有报错，没人知道它在空转。
    ///
    /// 判据是保守的（只在本阶段**要求产物**时才判失败）：确实有些阶段
    /// 模型说一段总结就能完成，不能一律当失败。
    #[test]
    fn text_only_model_fails_loudly_instead_of_spinning() {
        // 模型每次都只回文字，从不调工具。intel 阶段要求 ParticipantProfiles 产物
        let c = client(vec![
            (200, text_response("现在我将使用 participant_profile_update 工具：{\"id\": \"x\"}")),
            (200, text_response("好的，我来更新画像")),
        ]);
        let theory = TheoryLibrary::new();
        let pb = Playbook {
            meta: PlaybookMeta {
                id: "p".into(),
                name: "n".into(),
                description: String::new(),
                order: 0,
                version: 1,
            },
            applicable_when: vec![],
            fails_when: vec![],
            required_inputs: vec![],
            value_net: None,
            intake: vec![],
            models: vec![],
            selectors: Default::default(),
            tool_allowlist: vec![],
            report_vars: Default::default(),
        };
        let mut ex = DomainExecutor::new(&theory, &pb);
        let reg = ToolRegistry::standard();
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        // 推进到 intel（该阶段要求产物）
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        m.advance().expect("intake → intel");
        let out = l
            .run_turn(&mut m, TurnId(1), vec![InputItem::user("x")])
            .expect("run_turn 应返回结果");

        match out {
            TurnOutcome::Failed { detail, .. } => {
                assert!(
                    detail.contains("一次工具调用都没有") || detail.contains("未发起任何工具调用"),
                    "失败原因应点明「完全没调工具」：{detail}"
                );
                // 产物名必须是**人话**（"参与者画像"），不能是内部键
                // （`participant_profiles`）——见 `ArtifactKind::display_name`
                // 的文档：内部名对模型没有任何可操作性。
                assert!(
                    detail.contains("参与者画像"),
                    "应说明该阶段要求什么产物（且用人话而非内部键）：{detail}"
                );
                assert!(
                    !detail.contains("report_model"),
                    "错误信息里不得出现内部字段名：{detail}"
                );
            }
            TurnOutcome::Completed { .. } => panic!(
                "**必须失败而不是 Completed**——把它当完成会让会话无限空转\
                 而看不出任何问题（run27 实测：7 turn 只有 1 次工具调用）"
            ),
            other => panic!("实得：{other:?}"),
        }
    }

    /// 与 `response_with_calls` 同，但**可指定 input_tokens**
    /// （用于测上下文预警：阈值判断依赖这个数）。
    fn response_with_calls_tokens(calls: &[(&str, &str, &str)], input_tokens: u64) -> String {
        let items: Vec<serde_json::Value> = calls
            .iter()
            .map(|(id, name, args)| {
                serde_json::json!({
                    "type": "function_call",
                    "call_id": id,
                    "name": name,
                    "arguments": args,
                })
            })
            .collect();
        serde_json::json!({
            "id": "resp_x",
            "status": "completed",
            "output": items,
            "usage": {"input_tokens": input_tokens, "output_tokens": 1, "total_tokens": input_tokens + 1}
        })
        .to_string()
    }

    fn text_response(text: &str) -> String {
        serde_json::json!({
            "id": "resp_x",
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
        .to_string()
    }

    /// 构造一个**显式注入凭据**的客户端。
    ///
    /// 关键点：测试既不读也不写进程环境变量。读会让测试依赖外部配置
    /// （漏配即静默跳过），写则是 Rust 2024 下的 `unsafe` 且并发下是数据竞态。
    /// `KeySource::Explicit` 使两者都不必要——凭据来源本来就是可注入的
    /// （生产里它来自 secrets 文件）。
    /// 与 `client` 同，但**返回 transport 的共享句柄**，使测试能看到
    /// 实际发出的请求体。
    ///
    /// # 为什么需要它（而不是给 `AgentLoop` 加观测方法）
    ///
    /// "提醒有没有真的进请求"是**请求形状**的问题，而请求形状只能在
    /// transport 层看到。给 `AgentLoop` 加一个 `last_request_contained()`
    /// 会让生产代码带上一件只服务于测试的能力——那是把测试需求渗进产品。
    /// 用 `Arc` 共享则完全不动生产代码。
    fn client_with_probe(
        responses: Vec<(u16, String)>,
    ) -> (LlmClient<ProbedTransport>, Arc<FakeTransport>) {
        let ep = Endpoint {
            base_url: "https://example.invalid".into(),
            api_key_env: "SHANGYI_TEST_KEY_UNUSED".into(),
            wire: shangyi_llm::Wire::Responses,
            echo_reasoning: true,
        };
        let t = Arc::new(FakeTransport::new(responses));
        let probe = t.clone();
        (
            LlmClient::new(ep, ProbedTransport(t)).with_key("test-key-not-real"),
            probe,
        )
    }

    /// `Arc<FakeTransport>` 的**本地包装**。
    ///
    /// 为什么需要它：孤儿规则（orphan rule）不允许为 `Arc<外部类型>`
    /// 实现本 crate 之外的 trait。包一层本地类型即可——而这也是
    /// 共享观测句柄的标准做法。
    struct ProbedTransport(Arc<FakeTransport>);

    impl shangyi_llm::Transport for ProbedTransport {
        fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &serde_json::Value,
        ) -> Result<(u16, String), shangyi_llm::LlmError> {
            self.0.post_json(url, headers, body)
        }
    }

    fn client(responses: Vec<(u16, String)>) -> LlmClient<FakeTransport> {
        let ep = Endpoint {
            base_url: "https://example.invalid".into(),
            api_key_env: "SHANGYI_TEST_KEY_UNUSED".into(),
            wire: shangyi_llm::Wire::Responses,
            echo_reasoning: true,
        };
        LlmClient::new(ep, FakeTransport::new(responses)).with_key("test-key-not-real")
    }

    struct RecordingExecutor {
        outcome: ExecOutcome,
        calls: RefCell<Vec<String>>,
    }
    impl RecordingExecutor {
        fn new(outcome: ExecOutcome) -> Self {
            Self {
                outcome,
                calls: RefCell::new(Vec::new()),
            }
        }
    }
    impl ToolExecutor for RecordingExecutor {
        fn execute(&mut self, call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
            self.calls.borrow_mut().push(call.name.clone());
            self.outcome.clone()
        }
    }

    fn machine_at_intel_with_profiles() -> PhaseMachine {
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        m.advance().unwrap();
        m.record(ArtifactKind::ParticipantProfiles, "profiles.json");
        m
    }

    fn approve(m: &mut PhaseMachine, cp: CheckpointId) {
        m.open_checkpoint(CheckpointRequest {
            id: cp,
            phase: m.phase(),
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        m.settle_checkpoint(
            cp,
            CheckpointStatus::Approved {
                fingerprint: m.artifact_fingerprint(),
                by: "test".into(),
                at: "t".into(),
            },
        )
        .unwrap();
    }

    // ── 闸门 ──

    #[test]
    fn gate_blocks_step_while_checkpoint_is_open() {
        let mut m = machine_at_intel_with_profiles();
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "f1".into(),
                content: "事实".into(),
                source: Some("来源".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();

        match PreStepGate::new(&m).decide() {
            PreStepDecision::Reject { reason } => {
                assert!(reason.contains("等待用户确认"), "实得：{reason}");
            }
            other => panic!("checkpoint 打开时必须拒绝 step，实得 {other:?}"),
        }
    }

    /// "checkpoint 尚未打开"**不拦截** step——否则 `intel` 永远无法采集证据
    /// （它既不能跑 `web_search`，也无事实可提交给空提问件）。见 `PreStepGate::decide`
    /// 文档里的上锁论证，以及差异登记 R21。
    #[test]
    fn gate_allows_step_when_checkpoint_not_yet_opened_but_guides_toward_it() {
        let m = machine_at_intel_with_profiles();
        match PreStepGate::new(&m).decide() {
            PreStepDecision::Inject { notes } => {
                let joined = notes.join("\n");
                assert!(joined.contains("intel"), "应注入本阶段指引：{joined}");
                assert!(
                    joined.contains("cp1") && joined.contains("事实"),
                    "指引必须告诉模型本阶段末要就什么发起确认：{joined}"
                );
                assert!(joined.contains(include_str!("../../../prompts/consultant-system.md")),
                    "通用咨询提示词须完整注入，不能只在文件中存在而未用于真实请求");
            }
            other => panic!("未打开不得拦截 step（否则 intel 自锁），实得 {other:?}"),
        }
    }

    #[test]
    fn gate_blocks_step_after_rejection() {
        let mut m = machine_at_intel_with_profiles();
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "f1".into(),
                content: "事实".into(),
                source: Some("来源".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        m.settle_checkpoint(
            CheckpointId::Cp1,
            CheckpointStatus::Rejected {
                reason: "不对".into(),
                at: "t".into(),
            },
        )
        .unwrap();
        match PreStepGate::new(&m).decide() {
            PreStepDecision::Reject { reason } => {
                assert!(reason.contains("拒绝"), "实得：{reason}");
                assert!(reason.contains("铁律 2"), "拒绝理由应点明它守的是什么：{reason}");
            }
            other => panic!("期望拒绝，实得 {other:?}"),
        }
    }

    /// 阶段内**采集期**必须放行：`intel` 的产物（画像）是在 `intel` 期间产出的，
    /// 若要求"先有产物才准做事"，采集工作永远无法开始。
    #[test]
    fn gate_allows_steps_during_the_phase_that_produces_the_artifact() {
        let m = PhaseMachine::new(ConsultationMode::Scenario);
        // intake：无产物要求、无 checkpoint
        assert!(matches!(PreStepGate::new(&m).decide(), PreStepDecision::Inject { .. }));

        let mut m2 = PhaseMachine::new(ConsultationMode::Scenario);
        m2.advance().unwrap(); // → intel，画像尚未产出
        assert!(
            matches!(PreStepGate::new(&m2).decide(), PreStepDecision::Inject { .. }),
            "intel 采集期必须放行，否则模型无法调用 web_search"
        );
    }

    /// 进入条件（前驱产物 + 前驱 checkpoint）是防御性的：正常路径下 `advance`
    /// 已保证。此处通过 `advance_to` 绕过常规路径来验证它确实会被检查。
    #[test]
    fn gate_rejects_when_entering_without_predecessor_checkpoint_approval() {
        let mut m = PhaseMachine::new(ConsultationMode::Whatif);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "p");
        approve(&mut m, CheckpointId::Cp1);
        m.advance().unwrap(); // → model_build
        m.record(ArtifactKind::GameSpecValidated, "g");
        approve(&mut m, CheckpointId::Cp2);
        // 绕过 solve 进 consequence
        m.advance_to(SessionPhase::Consequence).unwrap();

        // 从 consequence 回退到 model_build 会撤销 cp2（它在 model_build 上，
        // 位置严格晚于 model_build？不——cp2 在 model_build 上，回退到 model_build
        // 时它是"目标阶段的 checkpoint"，故保留）。用一个能真正触发进入条件缺失的
        // 路径：回退到 intel（撤销 model_build 的产物与 cp2），再尝试直接跳到 consequence。
        m.rollback_to(SessionPhase::Intel).unwrap();
        assert!(!m.has_artifact(ArtifactKind::GameSpecValidated));
        assert!(m.checkpoint_status(CheckpointId::Cp2).is_none());
        // intel → consequence 不是一条边
        assert!(m.advance_to(SessionPhase::Consequence).is_err());
        // 而进入 model_build 的前驱条件（intel 的画像与 cp1）仍然满足
        m.record(ArtifactKind::GameSpecValidated, "g2");
        assert!(matches!(
            PreStepGate::new(&m).decide(),
            PreStepDecision::Inject { .. }
        ));
    }

    // ── 循环：正常路径 ──

    #[test]
    fn turn_completes_when_model_stops_calling_tools() {
        let c = client(vec![(200, text_response("分析完成"))]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("deepseek-flash", "case-1"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("开始")]).unwrap();
        match &out {
            TurnOutcome::Completed { output, steps } => {
                assert_eq!(output, "分析完成");
                assert_eq!(*steps, 1);
            }
            other => panic!("期望 Completed，实得 {other:?}"),
        }
        assert!(out.end_reason().is_clean_completion());

        // 日志：TurnStarted → StepStarted → TokenUsage → StepEnded → TurnEnded
        //
        // `token_usage` 在 `step_started` 之后、`step_ended` 之前——**每次模型调用
        // 都记**（即使它没请求工具，那次调用也已经花了钱）。
        let types: Vec<&str> = l.events().iter().map(|e| event_tag(e)).collect();
        assert_eq!(
            types,
            vec!["turn_started", "step_started", "token_usage", "step_ended", "turn_ended"]
        );
    }

    /// **成本可观测性**：每次模型调用的 token 用量必须进日志，且缓存/思考
    /// 分开记（价差 50 倍 / 思考计入输出价，混在一起算不出成本）。
    #[test]
    fn token_usage_is_recorded_with_cache_and_reasoning_breakdown() {
        // 响应里带嵌套 details（与 DeepSeek 实测形态一致）
        let resp = serde_json::json!({
            "id": "r", "status": "completed",
            "output": [{"type":"message","content":[{"type":"text","text":"ok"}]}],
            "usage": {
                "input_tokens": 1000,
                "input_tokens_details": {"cached_tokens": 800},
                "output_tokens": 200,
                "output_tokens_details": {"reasoning_tokens": 150},
                "total_tokens": 1200
            }
        })
        .to_string();

        let c = client(vec![(200, resp)]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();

        let usage = l
            .events()
            .iter()
            .find_map(|e| match e {
                Event::TokenUsage {
                    input_tokens,
                    output_tokens,
                    cached_input_tokens,
                    reasoning_tokens,
                    ..
                } => Some((*input_tokens, *output_tokens, *cached_input_tokens, *reasoning_tokens)),
                _ => None,
            })
            .expect("每次模型调用都应记一条 token_usage");

        assert_eq!(usage, (1000, 200, 800, 150));
        // 这两个派生量直接决定成本能不能算对
        assert_eq!(
            usage.0 - usage.2,
            200,
            "未命中缓存的输入 = 1000-800（按 50 倍价差计价的那部分）"
        );
    }

    #[test]
    fn tool_call_is_executed_and_result_fed_back() {
        // 第一步要工具，第二步收尾
        let c = client(vec![
            (200, response_with_calls(&[("call_1", "vn_map", "{\"depth\":\"deep\"}")])),
            (200, text_response("已扫描")),
        ]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{\"directions\":5}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("开始")]).unwrap();
        assert_eq!(out.steps(), 2, "两个 step：一个调工具，一个收尾");

        // 先把请求快照取出来（它借用 `l`），再释放 `l` 对 `ex` 的可变借用
        let reqs = l.client.transport().requests();
        drop(l);
        assert_eq!(ex.calls.borrow().as_slice(), &["vn_map".to_string()]);

        // 第二次请求必须带上工具输出与**阶段指引**
        assert_eq!(reqs.len(), 2);
        let second = serde_json::to_string(&reqs[1]).unwrap();
        assert!(second.contains("directions"), "工具结果必须回灌：{second}");
        assert!(second.contains("intake"), "阶段指引必须注入：{second}");
    }

    /// **铁律 1 的回归测试**：报告类工具绝不能从模型那里拿到"数字"。
    /// 这里验证的是循环的**结构**性质——阶段白名单真的被用在请求里，
    /// 使模型在 solve 阶段看不到 `web_search`，在 intake 阶段看不到 `solve_equilibria`。
    #[test]
    fn request_tools_are_filtered_by_phase() {
        let c = client(vec![(200, text_response("done"))]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        let body = l.client.transport().requests()[0].clone();
        let names: Vec<String> = body["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        assert!(names.contains(&"vn_map".to_string()), "intake 阶段应可用 vn_map");
        assert!(!names.contains(&"checkpoint".to_string()), "尚无检查点的阶段不应暴露确认工具");
        assert!(
            !names.contains(&"solve_equilibria".to_string()),
            "intake 阶段**不得**向模型暴露求解器——过早求解是常见误用：{names:?}"
        );
        assert!(
            !names.contains(&"report_render".to_string()),
            "报告工具不得在 intake 暴露"
        );
        // tool_choice 的默认值必须是 auto（`required` 实测恒 400）
        assert_eq!(body["tool_choice"], serde_json::json!("auto"));
    }

    #[test]
    fn checkpoint_tool_becomes_available_at_the_real_review_phase() {
        let c=client(vec![]);
        let reg=ToolRegistry::standard();
        let mut ex=RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let l=AgentLoop::new(c,&reg,&mut ex,LoopConfig::new("m","c"));
        let mut m=PhaseMachine::new(ConsultationMode::Scenario);
        assert!(!l.tool_schemas(&m).iter().any(|t|t.name=="checkpoint"));
        m.advance().unwrap();
        assert!(l.tool_schemas(&m).iter().any(|t|t.name=="checkpoint"));
    }

    #[test]
    fn tools_are_also_filtered_by_playbook_allowlist() {
        use shangyi_playbooks::{Playbook, PlaybookMeta};
        let pb = Playbook {
            meta: PlaybookMeta {
                id: "p".into(),
                name: "n".into(),
                description: String::new(),
                order: 0,
                version: 1,
            },
            applicable_when: vec![],
            fails_when: vec![],
            required_inputs: vec![],
            value_net: None,
            intake: vec![],
            models: vec![],
            selectors: Default::default(),
            tool_allowlist: vec!["pars_check".into()],
            report_vars: Default::default(),
        };
        let c = client(vec![(200, text_response("done"))]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c")).with_playbook(&pb);
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();

        let body = l.client.transport().requests()[0].clone();
        let names: Vec<String> = body["tools"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(names, vec!["conditional_analysis","constraint_check","constraint_register","pars_check","research_document_lookup","research_task_plan","research_task_update","select_task_scope"].into_iter().map(String::from).collect::<Vec<_>>(),
            "剧本白名单继续限制领域工具；intake范围控制始终可用，完整咨询不暴露短任务完成工具");
    }

    /// 模型调用本阶段不允许的工具：**回灌类型化原因，而不是中止 turn**。
    /// 这是可自我纠正的失误，中止会浪费整个 turn——但也绝不静默忽略。
    #[test]
    fn disallowed_tool_call_is_fed_back_not_fatal() {
        let c = client(vec![
            (200, response_with_calls(&[("c1", "solve_equilibria", "{}")])),
            (200, text_response("改用 vn_map")),
        ]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        assert!(matches!(out, TurnOutcome::Completed { .. }), "实得 {out:?}");
        let reqs = l.client.transport().requests();
        drop(l);
        assert!(
            ex.calls.borrow().is_empty(),
            "阶段不允许的工具**不得被执行**（闸门在实现者之前）"
        );
        let second = serde_json::to_string(&reqs[1]).unwrap();
        assert!(second.contains("solve"), "回灌文本应含类型化原因：{second}");
    }

    // ── 循环：三条"不干净"的终止路径 ──

    #[test]
    fn blocked_tool_call_ends_turn_as_blocked_not_completed() {
        let c = client(vec![(200, response_with_calls(&[("c1", "web_search", "{}")]))]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Blocked("三后端全部超时".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        match &out {
            TurnOutcome::Blocked { detail, .. } => assert!(detail.contains("三后端全部超时")),
            other => panic!("数据源不可用必须终止为 Blocked，实得 {other:?}"),
        }
        assert!(
            !out.end_reason().is_clean_completion(),
            "Blocked 绝不能被记成干净完成——那会让报告读者以为分析做完了"
        );
        assert_eq!(out.end_reason().kind_str(), "blocked");
    }

    #[test]
    fn llm_failure_ends_turn_as_error() {
        let c = client(vec![(500, "boom".into())]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        assert!(matches!(out, TurnOutcome::Failed { .. }), "实得 {out:?}");
        assert!(!out.end_reason().is_clean_completion());
    }

    #[test]
    fn step_limit_ends_turn_as_max_tokens_not_completed() {
        // 每次都只回工具调用 ⇒ 永不自然收尾
        let mut rs = Vec::new();
        for i in 0..40 {
            rs.push((
                200,
                response_with_calls(&[(&format!("c{i}"), "pars_check", "{}")]),
            ));
        }
        let c = client(rs);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut cfg = LoopConfig::new("m", "c");
        cfg.max_steps_per_turn = 3;
        let mut l = AgentLoop::new(c, &reg, &mut ex, cfg);
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        match &out {
            TurnOutcome::Failed { reason, steps, .. } => {
                assert_eq!(*reason, TurnEndReason::MaxTokens);
                assert_eq!(*steps, 3);
            }
            other => panic!("步数触顶必须终止，实得 {other:?}"),
        }
        assert!(
            !out.end_reason().is_clean_completion(),
            "触顶意味着分析没做完，不是干净完成"
        );
    }


    // ── 原地打转闸门 ──

    #[test]
    fn typed_missing_payoff_is_bounded_across_turns_and_preserves_the_original_game() {
        use crate::conditional::{ConditionalSnapshot,ConditionalTrigger,EvidenceGap,EvidenceGapReason,build_conditional_result};
        struct MissingPayoff { attempts:u32 }
        impl ToolExecutor for MissingPayoff {
            fn execute(&mut self,call:&ToolCall,_:SessionPhase)->ExecOutcome {
                if call.name=="pars_check" {return ExecOutcome::Ok("已读已有检查项；没有新收益来源".into());}
                self.attempts+=1;
                ExecOutcome::InsufficientEvidence {gaps:vec![EvidenceGap {reason:EvidenceGapReason::UnknownOpponentPayoff,
                    participant_id:Some("landlord".into()),profile:Some(if self.attempts%2==0 {"move,reject"}else{"stay,accept"}.into()),detail:"对方外租的完整收益未知".into()}]}
            }
            fn artifacts(&self)->Vec<(ArtifactKind,String)> {vec![(ArtifactKind::ParticipantProfiles,"profiles.json".into())]}
            fn conditional_fallback(&mut self,gaps:&[EvidenceGap])->Result<crate::task_scope::ScopedTaskResult,String> {
                build_conditional_result(&ConditionalSnapshot {question:"怎样谈续租？".into(),gaps:gaps.to_vec(),
                    attempted_model:Some(serde_json::json!({"players":[{"id":"tenant","name":"我方","relation_to_game":"player"},{"id":"landlord","name":"房东","relation_to_game":"player"}],"strategies":{"tenant":["stay","move"],"landlord":["accept","reject"]},"payoffs":{"unknown":null}})),..Default::default()},ConditionalTrigger::RepeatedNoProgress)
            }
        }
        let responses=vec![
            (200,response_with_calls(&[("g1","gamespec_build","{}")])),(200,text_response("先整理当前信息")),
            (200,response_with_calls(&[("g2","gamespec_build","{}")])),
            (200,response_with_calls(&[("p1","pars_check","{}")])),
            (200,response_with_calls(&[("g3","gamespec_build","{}")])),
        ];
        let (c,probe)=client_with_probe(responses);let reg=ToolRegistry::standard();let mut ex=MissingPayoff{attempts:0};
        let mut loop_=AgentLoop::new(c,&reg,&mut ex,LoopConfig::new("m","typed-gap"));
        let mut machine=machine_at_intel_with_profiles();approve(&mut machine,CheckpointId::Cp1);machine.advance().unwrap();
        loop_.run_turn(&mut machine,TurnId(1),vec![InputItem::user("咨询")]).unwrap();
        let history=loop_.take_turn_history();
        loop_.record_user_input("我仍不知道对方收益。","user");
        let outcome=loop_.run_turn(&mut machine,TurnId(2),history).unwrap();
        assert!(matches!(outcome,TurnOutcome::Completed{..}));assert_eq!(probe.requests().len(),5);
        let result=machine.scoped_result().expect("bounded conditional delivery");
        assert_eq!(result.scope,crate::task_scope::TaskScope::Conditional);assert!(!result.is_complete());
        let model=result.conditional.as_ref().unwrap().snapshot.attempted_model.as_ref().unwrap();
        assert_eq!(model["players"].as_array().unwrap().len(),2);assert!(model["payoffs"]["unknown"].is_null());
        assert!(!machine.has_artifact(ArtifactKind::GameSpecValidated));assert!(!machine.has_artifact(ArtifactKind::EquilibriumSet));
        assert!(machine.checkpoint_status(CheckpointId::Cp2).is_none());
        assert_eq!(loop_.events().iter().filter(|e|matches!(e,Event::ToolCallFinished {outcome:ToolOutcome::InsufficientEvidence{..},..})).count(),3);
    }

    #[test]
    fn repeated_identical_model_errors_stop_before_the_turn_budget_without_fabricating_delivery() {
        struct BrokenModel;
        impl ToolExecutor for BrokenModel {
            fn execute(&mut self,_:&ToolCall,_:SessionPhase)->ExecOutcome {ExecOutcome::Failed("missing field players".into())}
            fn artifacts(&self)->Vec<(ArtifactKind,String)> {vec![(ArtifactKind::ParticipantProfiles,"profiles.json".into())]}
        }
        let calls=(0..6).map(|i|(200,response_with_calls(&[(&format!("g{i}"),"gamespec_build","{}")]))) .collect();
        let (c,probe)=client_with_probe(calls);let reg=ToolRegistry::standard();let mut ex=BrokenModel;
        let mut loop_=AgentLoop::new(c,&reg,&mut ex,LoopConfig::new("m","bounded-error"));
        let mut machine=machine_at_intel_with_profiles();approve(&mut machine,CheckpointId::Cp1);machine.advance().unwrap();
        let outcome=loop_.run_turn(&mut machine,TurnId(1),vec![InputItem::user("咨询")]).unwrap();
        assert!(matches!(outcome,TurnOutcome::Blocked{steps:3,..}));assert_eq!(probe.requests().len(),3);
        assert!(machine.scoped_result().is_none());
    }

    /// 同一工具连续失败到第 3 次时，回灌给模型的结果里必须带上**打转警告**。
    ///
    /// 实测依据（`case-20260919-scenario-063033`）：61 次 `gamespec_build`
    /// 里 60 次失败，8 类形状错误轮着试，把单 turn 的 64 步额度耗光、
    /// 整个会话作废。报错它每次都收到了——缺的是"你已经试了很多次，换条路"。
    #[test]
    fn repeated_tool_failures_inject_a_stop_and_reroute_note() {
        // 6 个响应，全部调用同一个工具（每次换个 call_id，避免被当重复）
        let mut rs = Vec::new();
        for i in 0..6 {
            rs.push((
                200,
                response_with_calls(&[(&format!("c{i}"), "pars_check", "{}")]),
            ));
        }
        let reg = ToolRegistry::standard();
        struct AlwaysFails;
        impl ToolExecutor for AlwaysFails {
            fn execute(&mut self, _call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
                ExecOutcome::Failed("形状不对：缺 `values`".into())
            }
        }
        let mut ex = AlwaysFails;
        let mut cfg = LoopConfig::new("m", "c");
        cfg.max_steps_per_turn = 6;
        // 带探针的 client：只有它能让我们看到**回灌给模型的内容**
        // （打转警告正是在那里注入的）
        let (client_with_probe_ref, probe) = client_with_probe(rs);
        let mut l = AgentLoop::new(client_with_probe_ref, &reg, &mut ex, cfg);
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        let _ = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]);

        let reqs = probe.requests();
        assert!(reqs.len() >= 4, "应至少有 4 次请求，实得 {}", reqs.len());
        // 第 4 次请求（index 3）对应第 3 次失败之后——应当带上警告
        let body = serde_json::to_string(&reqs[3]).unwrap();
        assert!(
            body.contains("已连续失败"),
            "第 3 次失败后应注入打转警告；实得请求体：{}",
            &body[..body.len().min(600)]
        );
        assert!(
            body.contains("换一条路") || body.contains("换一条"),
            "警告必须给出换路指示"
        );
        // 且必须带上已见错误的摘要（否则模型不知道自己在重复什么）
        assert!(
            body.contains("形状不对：缺 `values`"),
            "警告里应包含已见错误的摘要"
        );
    }

    /// 成功一次就**重置**连败计数——闸门针对的是"连续"失败，
    /// 而不是"累计"失败（后者会把正常的多步试错误判成打转）。
    #[test]
    fn a_success_resets_the_failure_streak() {
        let mut rs = Vec::new();
        // 失败、失败、成功、失败、失败 —— 每次都是同一个工具
        for i in 0..5 {
            rs.push((
                200,
                response_with_calls(&[(&format!("e{i}"), "pars_check", "{}")]),
            ));
        }
        let (c, probe) = client_with_probe(rs);
        let reg = ToolRegistry::standard();
        struct Flaky {
            n: u32,
        }
        impl ToolExecutor for Flaky {
            fn execute(&mut self, _call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
                self.n += 1;
                if self.n == 3 {
                    ExecOutcome::Ok("{}".into())
                } else {
                    ExecOutcome::Failed("临时错误".into())
                }
            }
        }
        let mut ex = Flaky { n: 0 };
        let mut cfg = LoopConfig::new("m", "c");
        cfg.max_steps_per_turn = 5;
        let mut l = AgentLoop::new(c, &reg, &mut ex, cfg);
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        let _ = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]);

        // 第 4 次请求（成功之后的那次）不应带警告——因为连败已被重置为 1
        let reqs = probe.requests();
        assert!(reqs.len() >= 5, "实得 {} 次请求", reqs.len());
        let body = serde_json::to_string(&reqs[3]).unwrap();
        assert!(
            !body.contains("已连续失败"),
            "成功之后连败计数应重置，不该出现打转警告"
        );
    }


    #[test]
    fn suspended_question_and_rewind_keep_paired_pending_outputs_for_next_turn() {
        let reg=ToolRegistry::standard();
        let cases=[
            ("ask_user",ExecOutcome::AsksUser(vec![UserQuestion{id:"goal".into(),text:"目标是什么？".into(),kind:"text".into(),options:vec![]}]),"PENDING_USER_ANSWER"),
            ("request_rewind",ExecOutcome::RewindRequest{target:SessionPhase::Intake,reason:"补充条件".into()},"PENDING_REWIND_DECISION"),
        ];
        for (name,outcome,pending) in cases {
            let c=client(vec![(200,response_with_calls(&[("pending-call",name,"{}")]))]);
            let mut ex=RecordingExecutor::new(outcome);
            let mut agent=AgentLoop::new(c,&reg,&mut ex,LoopConfig::new("m","pending"));
            let mut machine=PhaseMachine::new(ConsultationMode::Scenario);
            agent.run_turn(&mut machine,TurnId(1),vec![InputItem::user("开始")]).unwrap();
            let history=agent.take_turn_history();
            assert_eq!(history.iter().filter(|x|matches!(x,InputItem::FunctionCall{call_id,..} if call_id=="pending-call")).count(),1);
            let outputs:Vec<_>=history.iter().filter_map(|x|match x {InputItem::FunctionCallOutput{call_id,output} if call_id=="pending-call"=>Some(output),_=>None}).collect();
            assert_eq!(outputs.len(),1);assert!(outputs[0].contains(pending));
            assert!(!outputs[0].contains("ALREADY_APPROVED"));
        }
    }

    // ── checkpoint 挂起 ──

    #[test]
    fn checkpoint_tool_suspends_the_turn() {
        let c = client(vec![(200, response_with_calls(&[("c1", "checkpoint", "{}")]))]);
        let reg = ToolRegistry::standard();
        struct CpExecutor;
        impl ToolExecutor for CpExecutor {
            fn execute(&mut self, _call: &ToolCall, phase: SessionPhase) -> ExecOutcome {
                ExecOutcome::NeedsUserInput(CheckpointRequest {
                    id: CheckpointId::Cp1,
                    phase,
                    items: vec![CheckpointItem {
                        id: "f1".into(),
                        content: "对手是三家".into(),
                        source: Some("年报".into()),
                        proposed_value: Some("3".into()),
                    }],
                    revision_requested: false,
                })
            }
        }
        let mut ex = CpExecutor;
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "p");

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        match &out {
            TurnOutcome::AwaitingUser { checkpoint, .. } => {
                assert_eq!(checkpoint.id, CheckpointId::Cp1);
                assert_eq!(checkpoint.items.len(), 1);
            }
            other => panic!("checkpoint 必须挂起 turn，实得 {other:?}"),
        }
        // 挂起**不是** Completed
        assert!(!out.end_reason().is_clean_completion());
        assert_eq!(out.end_reason().kind_str(), "aborted");

        // 日志以 CheckpointOpened 之外的方式留痕：工具调用有开始有结束
        let has_finished = l.events().iter().any(|e| {
            matches!(e, Event::ToolCallFinished { outcome: ToolOutcome::Ok { output }, .. }
                if output == "PENDING_USER_INPUT")
        });
        assert!(has_finished, "挂起也必须留下完整的工具调用记录（可审计）");
        let history=l.take_turn_history();
        assert!(history.iter().any(|x|matches!(x,InputItem::FunctionCallOutput{call_id,output} if call_id=="c1"&&output.contains("PENDING_USER_INPUT"))));
        assert!(!history.iter().any(|x|matches!(x,InputItem::FunctionCallOutput{output,..} if output.contains("ALREADY_APPROVED"))));
    }

    /// **checkpoint 挂着时，一次模型调用都不该发出。**
    ///
    /// 这条是 §7.6 的核心保证："只要 checkpoint 未结算，任何 step 都无法通过，
    /// 无论模型怎么想。"测试用 `FakeTransport` 的 `requests()` 来断言——
    /// 它记录的是**真的发出去了什么**，不是"循环以为自己没发"。这一点很关键：
    /// 只断言"决策是 Reject"只能证明判断逻辑，证明不了没有副作用。
    #[test]
    fn no_model_call_is_made_while_a_checkpoint_is_open() {
        let c = client(vec![(200, text_response("我不该被调用"))]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = machine_at_intel_with_profiles();

        // 打开 cp1 但**不结算**——正是"用户还没答"的那个状态
        m.open_checkpoint(CheckpointRequest {
            id: CheckpointId::Cp1,
            phase: SessionPhase::Intel,
            items: vec![CheckpointItem {
                id: "f1".into(),
                content: "对手是三家".into(),
                source: Some("年报".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();

        let out = l.run_turn(&mut m, TurnId(1), vec![InputItem::user("继续")]).unwrap();
        assert!(
            !out.end_reason().is_clean_completion(),
            "挂起不得被记成完成：{out:?}"
        );
        assert!(
            l.client.transport().requests().is_empty(),
            "checkpoint 未结算时**一次模型调用都不该发出**——这是硬拒绝的全部意义"
        );
        // 但 turn 的起止仍被完整记账（可审计）
        assert!(l.events().iter().any(|e| matches!(e, Event::TurnStarted { .. })));
        assert!(l.events().iter().any(|e| matches!(e, Event::TurnEnded { .. })));
    }

    /// 字符串形态的 `arguments` 必须被解析成对象——否则每个工具都会
    /// 以"缺少参数"失败（见 `normalize_arguments` 的说明）。
    #[test]
    fn string_shaped_tool_arguments_are_normalized() {
        // 模拟 OpenAI 风格：arguments 是一个 **JSON 字符串**
        let response = serde_json::json!({
            "id": "r", "status": "completed",
            "output": [{
                "type": "function_call",
                "call_id": "c1",
                "name": "vn_map",
                "arguments": "{\"depth\":\"deep\"}"
            }]
        })
        .to_string();

        let c = client(vec![(200, response), (200, text_response("done"))]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);

        l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();
        // 事件里记的 arguments 应是**规范化后的对象**（可审计、可重放）
        let logged = l
            .events()
            .iter()
            .find_map(|e| match e {
                Event::ToolCallStarted { arguments, .. } => Some(arguments.clone()),
                _ => None,
            })
            .expect("应有工具调用事件");
        let v: serde_json::Value = serde_json::from_str(&logged).unwrap();
        assert!(
            v.is_object(),
            "字符串形态的参数必须被解析为对象，实得：{logged}"
        );
        assert_eq!(v["depth"], serde_json::json!("deep"));
    }

    /// 对象形态（DeepSeek 实测）保持原样。
    #[test]
    fn object_shaped_tool_arguments_pass_through() {
        let v = serde_json::json!({"a": 1});
        assert_eq!(normalize_arguments(&v), v);
        // 非 JSON 的字符串原样保留（交给工具报错，不在这里丢内容）
        let bad = serde_json::json!("不是 JSON");
        assert_eq!(normalize_arguments(&bad), bad);
    }

    fn event_tag(e: &Event) -> &'static str {
        match e {
            Event::TurnStarted { .. } => "turn_started",
            Event::TurnEnded { .. } => "turn_ended",
            Event::StepStarted { .. } => "step_started",
            Event::StepEnded { .. } => "step_ended",
            Event::ToolCallStarted { .. } => "tool_call_started",
            Event::ToolCallFinished { .. } => "tool_call_finished",
            Event::TokenUsage { .. } => "token_usage",
            Event::Warning { .. } => "warning",
            Event::PhaseEntered { .. } => "phase_entered",
            Event::PhaseApproved { .. } => "phase_approved",
            Event::PhaseRewound { .. } => "phase_rewound",
            Event::PlaybookSelected { .. } => "playbook_selected",
            Event::CheckpointOpened { .. } => "checkpoint_opened",
            Event::CheckpointAnswered { .. } => "checkpoint_answered",
            Event::UserInputRecorded { .. } => "user_input_recorded",
            Event::TaskScopeSelected { .. } => "task_scope_selected",
            Event::ScopedTaskCompleted { .. } => "scoped_task_completed",
            Event::FollowupTaskStarted { .. } => "followup_task_started",
        }
    }

    /// **结构性不变量**：对话历史里每个 `function_call` 必须有配对的
    /// `function_call_output`。
    ///
    /// # 为什么这条必须是测试（而不是只写在注释里）
    ///
    /// Responses API 会拒绝不成对的历史：
    /// `400 No tool output found for tool call call_00_...`
    ///
    /// 而"漏配对"的成因是**任何提前退出本步的路径**——实测踩到过两次：
    /// 一次是 checkpoint 幂等分支的 `continue`，一次是 checkpoint 挂起分支的
    /// `return`。两次都是"新加了一条快捷路径，忘了补 output"。
    ///
    /// 这类缺陷在离线测试里看不见（假传输层不校验配对），
    /// 只有真跑才暴露，且错误信息（`No tool output found`）说得像
    /// "服务端状态问题"，而不是"你少传了一个字段"。
    ///
    /// 故用测试穷举本 turn 的每条退出路径，断言历史成对。
    #[test]
    fn every_function_call_in_history_has_a_matching_output() {
        // 场景 1：正常工具调用（有 output）
        let c = client(vec![
            (200, response_with_calls(&[("c1", "vn_map", "{}")])),
            (200, text_response("done")),
        ]);
        let reg = ToolRegistry::standard();
        let mut ex = RecordingExecutor::new(ExecOutcome::Ok("{}".into()));
        let mut l = AgentLoop::new(c, &reg, &mut ex, LoopConfig::new("m", "c"));
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        l.run_turn(&mut m, TurnId(1), vec![InputItem::user("x")]).unwrap();

        // 场景 2：checkpoint 幂等路径（**曾经漏配对的路径**）
        let c2 = client(vec![(200, response_with_calls(&[("c2", "checkpoint", "{}")]))]);
        struct CpIdem;
        impl ToolExecutor for CpIdem {
            fn execute(&mut self, _call: &ToolCall, phase: SessionPhase) -> ExecOutcome {
                ExecOutcome::NeedsUserInput(CheckpointRequest {
                    id: CheckpointId::Cp1,
                    phase,
                    items: vec![CheckpointItem {
                        id: "i".into(),
                        content: "c".into(),
                        source: Some("s".into()),
                        proposed_value: None,
                    }],
                    revision_requested: false,
                })
            }
        }
        let mut ex2 = CpIdem;
        let mut l2 = AgentLoop::new(c2, &reg, &mut ex2, LoopConfig::new("m", "c"));
        let mut m2 = PhaseMachine::new(ConsultationMode::Scenario);
        m2.advance().unwrap();
        m2.record(ArtifactKind::ParticipantProfiles, "p");
        // 先批准一次
        m2.open_checkpoint(CheckpointRequest {
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
        m2.settle_checkpoint(
            CheckpointId::Cp1,
            CheckpointStatus::Approved {
                fingerprint: m2.artifact_fingerprint(),
                by: "t".into(),
                at: "t".into(),
            },
        )
        .unwrap();
        // 再要求同一 checkpoint ⇒ 走幂等分支
        l2.run_turn(&mut m2, TurnId(1), vec![InputItem::user("x")]).unwrap();

        // 两条路径的历史都必须成对
        for (name, l) in [("normal", &l), ("idempotent", &l2)] {
            assert_history_is_paired(name, l);
        }
    }

    /// 断言：请求历史里每个 `function_call` 都有配对的 `function_call_output`。
    fn assert_history_is_paired(label: &str, l: &AgentLoop<'_, FakeTransport>) {
        // 从传输层取**实际发出的请求体**——那才是端点看到的，
        // 而不是"循环以为它发了什么"
        let reqs = l.client.transport().requests();
        for (i, req) in reqs.iter().enumerate() {
            let mut calls: Vec<String> = Vec::new();
            let mut outputs: Vec<String> = Vec::new();
            if let Some(items) = req.get("input").and_then(|v| v.as_array()) {
                for it in items {
                    match it.get("type").and_then(|t| t.as_str()) {
                        Some("function_call") => {
                            if let Some(id) = it.get("call_id").and_then(|x| x.as_str()) {
                                calls.push(id.to_string());
                            }
                        }
                        Some("function_call_output") => {
                            if let Some(id) = it.get("call_id").and_then(|x| x.as_str()) {
                                outputs.push(id.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
            let missing: Vec<&String> =
                calls.iter().filter(|c| !outputs.contains(c)).collect();
            assert!(
                missing.is_empty(),
                "[{label}] 第 {i} 个请求里有 {} 个 function_call 没有配对 output：{:?}\n\
                 这会被端点拒绝（400 No tool output found for tool call ...）。\n\
                 成因通常是「新加了一条提前退出本步的快捷路径，忘了补 output」。",
                missing.len(),
                missing
            );
        }
    }
}

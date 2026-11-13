//! 咨询会话驱动器：把循环、阶段机、持久化与用户交互串成一次真实咨询。
//!
//! # 它解决什么问题
//!
//! 循环（[`crate::agent`]）只负责"跑一个 turn"。一次真实咨询还需要：
//!
//! | 职责 | 落点 |
//! |---|---|
//! | 事件落盘（边发生边写） | [`SessionRunner::persist_new_events`] |
//! | 何时走 durable 写 | [`is_durable`] |
//! | 挂起时把问题交给用户、收回答 | [`UserIo`] |
//! | 回答后**用它重算产物**、结算 checkpoint | [`SessionRunner::settle`] |
//! | 阶段推进（过闸门才走） | [`SessionRunner::try_advance`] |
//! | 诚实终止 | [`RunReport`] |
//!
//! # 两个 seam 使全流程可离线测试
//!
//! 1. **LLM**：`shangyi_llm::Transport`——注入假传输层即无网络跑通全流程。
//! 2. **用户**：[`UserIo`]——注入脚本化回答即可测"用户拒绝后发生什么"。
//!
//! 没有这两个 seam，"拒绝 checkpoint""外部阻塞"这类**分支**就只能靠人工点，
//! 而人工点不到的分支等于没有实现。

use shangyi_llm::Transport;

use crate::agent::{AgentLoop, LoopConfig, LoopError, ToolExecutor, TurnOutcome};
use crate::case_log::{CaseLog, LogError};
use crate::phase_graph::{
    ArtifactKind, CheckpointId, CheckpointRequest, CheckpointStatus, PhaseGateError, PhaseMachine,
    SessionPhase,
};
use crate::session::{CancelCause, Event, TurnEndReason, TurnId};

// ─────────────────────────── 用户交互 seam ───────────────────────────

/// 用户对一次 checkpoint 提问的三种回答（§8.3 的三操作）。
#[derive(Debug, Clone, PartialEq)]
pub enum CheckpointAnswer {
    /// 确认：接受清单里的全部内容。
    Approve,
    /// 修改：指出哪里不对。**不是**"拒绝整个分析"，而是"这部分要改"。
    Revise { reason: String },
    /// 追问：用户想先问清楚再答。补充说明会回灌给模型，
    /// **checkpoint 保持打开**（不结算）——因为用户还没表态。
    AskFollowUp { question: String },
}

/// 用户对"是否回退"的裁决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RewindDecision {
    /// 同意回退（作废已完成的工作）。
    Approve,
    /// 拒绝——模型应在当前阶段尽力完成，并把缺口如实标注为待确认项。
    Reject,
}

/// 用户交互的 seam。
pub trait UserIo {
    /// A short task is delivered without creating a checkpoint. Interactive
    /// browser implementations may wait for another user message without LLM work.
    fn scoped_task_completed(&mut self, _result: &crate::task_scope::ScopedTaskResult) {}
    fn scoped_task_completed_at(&mut self, result: &crate::task_scope::ScopedTaskResult, _phase: SessionPhase) {
        self.scoped_task_completed(result);
    }
    fn wait_for_followup_task(&mut self) -> Option<String> { None }
    /// Free-form context queued while a model turn was running. Never an approval.
    fn take_pending_messages(&mut self) -> Vec<String> { Vec::new() }
    /// Browser sessions keep their input channel open while revising the current review.
    fn can_revise_in_place(&self) -> bool { false }
    /// 呈现一个 checkpoint 提问件并取得回答（**确认**）。
    fn present_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer;

    /// 提出问题并取得答案（**采集**，`ask_user` 用）。
    ///
    /// 与 `present_checkpoint` 分开的理由见 [`crate::agent::UserQuestion`]：
    /// 确认是闸门（答案绑定产物指纹），采集是输入（答案进对话历史）。
    /// 默认实现返回空答案——它使**不需要采集**的自动化路径（例如纯
    /// 校验场景）不必实现它，但任何会走到 `ask_user` 的实现都必须覆写。
    fn ask(&mut self, questions: &[crate::agent::UserQuestion]) -> Vec<(String, String)> {
        let _ = questions;
        Vec::new()
    }

    /// 告知用户某件事（进度、报错、阻塞）。**不隐藏坏消息**。
    fn inform(&mut self, message: &str);

    /// 裁决模型的回退提议。
    ///
    /// 默认**拒绝**——这是刻意的 fail-closed：
    /// 回退会作废已完成的工作，而"没人回答"绝不能被当成"同意作废"。
    /// 与 §7.6 的"无应答者 ⇒ 拒绝推进"同源。
    fn decide_rewind(
        &mut self,
        target: SessionPhase,
        reason: &str,
    ) -> RewindDecision {
        let _ = (target, reason);
        RewindDecision::Reject
    }

    /// 本实现是否**自动**作答（无人值守/脚本化）。
    ///
    /// # 为什么这必须是 trait 的属性（而不是调用方传的字符串）
    ///
    /// 实测缺陷：`assume_yes` 自动确认时，日志里记的是
    /// `user_actor = "user"`——**把"没人看过"记成了"用户确认过"**。
    /// 那正是本项目要防的审计失败。
    ///
    /// 根因是实现知道自己是自动的（`TerminalIo::assume_yes`），
    /// 而"记谁确认的"这件事由调用方传的字符串决定，两者可以不一致。
    /// 让实现**自己声明**，调用方就不可能忘。
    fn is_automated(&self) -> bool {
        false
    }
}

/// 一个总是"确认"的 [`UserIo`]（自动化与冒烟测试用）。
///
/// **它存在的前提是明确标注**：用它的会话在审计上等同于"用户从未看过"，
/// 故 [`SessionRunner`] 会把它的每次自动确认记成 `user_actor = "auto"`，
/// 与真实用户（`user_actor = "user"`）在日志里**可区分**。
#[derive(Debug, Default)]
pub struct AutoApprove {
    pub messages: Vec<String>,
}

impl UserIo for AutoApprove {
    fn is_automated(&self) -> bool {
        true
    }
    fn decide_rewind(&mut self, _t: SessionPhase, _r: &str) -> RewindDecision {
        // 自动模式**拒绝回退**：作废已完成的工作不是自动决策该做的事。
        // 这会让模型按 Reject 分支继续——把缺口标注为待确认项，
        // 而不是无限提议回退。
        self.messages.push(
            "[auto] 收到回退提议，自动模式拒绝（作废工作需人决定）".into(),
        );
        RewindDecision::Reject
    }

    fn present_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer {
        self.messages
            .push(format!("[auto] 自动确认 {}（{} 项）", req.id.as_str(), req.items.len()));
        CheckpointAnswer::Approve
    }
    fn ask(&mut self, questions: &[crate::agent::UserQuestion]) -> Vec<(String, String)> {
        // **不编造答案**：自动模式的采集返回空答案，让上游把"未采集到"如实处理。
        // 若这里返回"（自动）无意见"之类的占位文本，它会作为**用户输入**进入
        // 对话历史——那就是把工具的缺失伪装成用户的陈述。
        self.messages.push(format!(
            "[auto] 收到 {} 个采集问题但自动模式不提供答案（未采集到）",
            questions.len()
        ));
        Vec::new()
    }
    fn inform(&mut self, message: &str) {
        self.messages.push(format!("[auto] {message}"));
    }
}

// ─────────────────────────── durable 策略 ───────────────────────────

/// 该事件是否必须**立即落盘**（§8.3 的强制写点）。
///
/// # 判据
///
/// "这件事如果丢了，用户会认为自己没做过"——即**不可逆的用户动作**
/// 与**决定阶段机位置的事件**。
///
/// 反例（不 durable）：`StepStarted`、`ToolCallStarted`。它们可被重跑，
/// 且每步 fsync 会显著拖慢循环。
///
/// 把这条策略写成**一个函数**（而不是散在各调用点的 `if`）：它是一条
/// 可被测试、可被审计的规则；散开后每隔几个月就会有人在某处漏掉一次。
pub fn is_durable(event: &Event) -> bool {
    matches!(
        event,
        Event::CheckpointOpened { .. }
            | Event::TaskScopeSelected { .. }
            | Event::ScopedTaskCompleted { .. }
            | Event::FollowupTaskStarted { .. }
            | Event::CheckpointAnswered { .. }
            | Event::UserInputRecorded { .. }
            | Event::PhaseApproved { .. }
            | Event::PhaseRewound { .. }
            | Event::PhaseEntered { .. }
            | Event::PlaybookSelected { .. }
            // turn 的结束原因决定"这段历史算不算做完"，丢了会让恢复出的
            // 状态与用户认知不符（§7.1 的 Blocked/Interrupted 可区分性）
            | Event::TurnEnded { .. }
    )
}

// ─────────────────────────── 运行报告 ───────────────────────────

/// 一次会话运行的结局。
#[derive(Debug, Clone, PartialEq)]
pub enum RunReport {
    TaskCompleted {
        result: crate::task_scope::ScopedTaskResult,
        phase: SessionPhase,
        turns: u32,
    },
    /// 走到了终态（`archived`）。
    Completed {
        phase: SessionPhase,
        turns: u32,
    },
    /// 挂起等待用户**确认**某个 checkpoint（正常的暂停点，**不是失败**）。
    AwaitingUser {
        phase: SessionPhase,
        checkpoint: CheckpointId,
        turns: u32,
    },
    /// 挂起等待用户**回答问题**（`ask_user` 采集；`checkpoint` 为 `None`）。
    ///
    /// # 为什么要与 `AwaitingUser` 分开
    ///
    /// 早先的实现用 `AwaitingUser` + `checkpoint_at(phase).unwrap_or(Cp1)` 表达
    /// "在采集期等用户回答"。后果：在 `intake` 阶段（**根本没有 checkpoint**）
    /// 它会报出 `等待你对 cp1 的确认`——一个**不存在的确认点**。
    ///
    /// 这类"为了让类型装上而编一个默认值"的做法，产出的正是虚假报告：
    /// 用户会去找那个确认点，而它并不存在。故必须用独立变体表达。
    AwaitingAnswers {
        phase: SessionPhase,
        turns: u32,
    },
    /// 外部依赖不可用，无法继续（§7.1 的 `Blocked`）。
    ///
    /// **与 `Failed` 永久可区分**：审计问"这份尽调是上游挂了还是程序错了"，
    /// 必须能答上来。
    Blocked {
        phase: SessionPhase,
        detail: String,
        turns: u32,
    },
    /// 出错或触顶。
    Failed {
        phase: SessionPhase,
        reason: TurnEndReason,
        detail: String,
        turns: u32,
    },
    /// 用户主动中止。
    Aborted {
        phase: SessionPhase,
        turns: u32,
    },
    /// 在**同一阶段**内用满 turn 预算仍未推进。
    ///
    /// 与 `BudgetExhausted`（整会话）的区别：这个变体指向"卡在一个阶段里"，
    /// 而那次失败通常是"模型问不完/改不完"。诊断意义不同，故分开。
    StageBudgetExhausted {
        phase: SessionPhase,
        turns_in_stage: u32,
        turns: u32,
    },
    /// 达到 turn 预算而阶段仍未推进。
    ///
    /// **与 `Failed` 的区别**：这不表示程序出错，只表示"这样跑下去不会收敛"。
    /// 审计看到它应问"为什么推不动"，而不是"哪里崩了"。
    BudgetExhausted {
        phase: SessionPhase,
        turns: u32,
    },
}

impl RunReport {
    pub fn phase(&self) -> SessionPhase {
        match self {
            RunReport::TaskCompleted { phase, .. } => *phase,
            RunReport::Completed { phase, .. }
            | RunReport::AwaitingUser { phase, .. }
            | RunReport::AwaitingAnswers { phase, .. }
            | RunReport::Blocked { phase, .. }
            | RunReport::Failed { phase, .. }
            | RunReport::Aborted { phase, .. }
            | RunReport::BudgetExhausted { phase, .. }
            | RunReport::StageBudgetExhausted { phase, .. } => *phase,
        }
    }

    pub fn turns(&self) -> u32 {
        match self {
            RunReport::TaskCompleted { turns, .. } => *turns,
            RunReport::Completed { turns, .. }
            | RunReport::AwaitingUser { turns, .. }
            | RunReport::AwaitingAnswers { turns, .. }
            | RunReport::Blocked { turns, .. }
            | RunReport::Failed { turns, .. }
            | RunReport::Aborted { turns, .. }
            | RunReport::BudgetExhausted { turns, .. }
            | RunReport::StageBudgetExhausted { turns, .. } => *turns,
        }
    }

    /// 这次运行是否"走完了"。
    ///
    /// 完整咨询须归档；短任务须按自身范围完整交付。partial 交付与等待用户
    /// 均不算完整完成，且短任务完成从不等同于博弈咨询已经求解或归档。
    pub fn is_finished(&self) -> bool {
        matches!(self, RunReport::Completed { .. })
            || matches!(self, RunReport::TaskCompleted { result, .. } if result.is_complete())
    }

    /// 该结局是否允许**重试**（§7.1 的 `is_retryable` 在会话层的对应）。
    pub fn is_retryable(&self) -> bool {
        match self {
            RunReport::Blocked { .. } => true,
            RunReport::Failed { reason, .. } => reason.is_retryable(),
            _ => false,
        }
    }
}

/// 运行器错误（**结构性**：跑不起来，不是会话结论）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RunError {
    #[error("循环错误：{0}")]
    Loop(String),
    #[error("日志错误：{0}")]
    Log(String),
    #[error("阶段闸门：{0}")]
    Gate(String),
    #[error("checkpoint `{id}` 不在等待状态（实为 {actual}）")]
    CheckpointNotOpen { id: String, actual: String },
    #[error("用户追问需要模型回应，但本 turn 已结束——追问将作为下一 turn 的输入")]
    FollowUpNeedsNewTurn,
}

impl From<LoopError> for RunError {
    fn from(e: LoopError) -> Self {
        RunError::Loop(e.to_string())
    }
}
impl From<LogError> for RunError {
    fn from(e: LogError) -> Self {
        RunError::Log(e.to_string())
    }
}
impl From<PhaseGateError> for RunError {
    fn from(e: PhaseGateError) -> Self {
        RunError::Gate(e.to_string())
    }
}

// ─────────────────────────── 运行器 ───────────────────────────

/// 会话驱动器。
///
/// 生命周期与一次 CLI 调用对齐：构造 → `run()` 或逐步驱动 → 结束。
pub struct SessionRunner<'a, T: Transport> {
    r#loop: AgentLoop<'a, T>,
    log: CaseLog,
    machine: PhaseMachine,
    /// 已落盘的循环事件数（游标）。
    ///
    /// 用游标而不是"清空循环事件"：循环的事件列表是**只读证据**
    /// （测试要能整段断言），持久化只是把它同步到磁盘。
    persisted: usize,
    turn_counter: u32,
    /// 是否允许自动推进阶段（交互模式下由 CLI 决定）。
    auto_advance: bool,
    /// 跨 turn 的对话历史。
    ///
    /// # 为什么必须有（这是一个真实缺陷的修复）
    ///
    /// `AgentLoop::run_turn` 的 `input` 是**函数局部**的：turn 内工具结果会累积，
    /// 但 turn 一结束就丢了。于是第二个 turn 完全不知道第一个 turn 说过什么——
    /// 实测表现为模型在第二个 turn 重新自我介绍、重问已经问过的问题。
    ///
    /// 修法是在**会话层**持有历史：轨迹由事件日志权威化（§8.1），而这里持有的是
    /// 重建出来的对话视图。每个 turn 把新的用户/工具消息追加进来。
    history: Vec<shangyi_llm::InputItem>,
    /// 在**同一阶段**内连续停留的 turn 上限。
    ///
    /// # 为什么需要它（实测暴露）
    ///
    /// 一次真实咨询在 `intake` 阶段跑了 **26 个 turn**、其中 `ask_user` 被调用
    /// **37 次**——模型一轮轮追问细节（"A、B 的挂牌价各是多少？""份额比例？"），
    /// 而每次都能得到部分回答，于是它继续问。
    ///
    /// 设计文档说 intake 应是「**1 个 turn**，内含若干 step」。实测偏离的原因
    /// 是**没有终止条件**：模型问不完是因为它永远能想到下一个问题。
    ///
    /// 这与 `max_turns`（整会话）不同：整会话预算再大也拦不住
    /// "在一个阶段里耗光全部预算"。故需要一个**按阶段**的预算。
    ///
    /// 触顶后的行为：**停在原地并如实报告**（不是失败，也不是强推）。
    /// 用户可以补充信息后 resume——那时计数重置。
    stage_turn_budget: u32,
    /// 当前阶段已用 turn 数（阶段变化时重置）。
    turns_in_stage: u32,
    /// 上一轮观察到的阶段（用于检测变化）。
    last_stage: Option<SessionPhase>,
    /// 一次 `run()` 的 turn 上限。
    ///
    /// # 为什么必须有
    ///
    /// 若非如此，存在一条真实的无限循环路径：模型每轮都"正常完成"但不产出
    /// 本阶段所需产物 ⇒ `try_advance` 因缺产物被闸门拦下 ⇒ `continue` ⇒
    /// 再来一轮……**永远不停，且每轮都在花 LLM 配额**。它不会被任何单步
    /// 检查发现，因为每一步都是合法的。
    max_turns: u32,
}

impl<'a, T: Transport> SessionRunner<'a, T> {
    pub fn new(r#loop: AgentLoop<'a, T>, log: CaseLog, machine: PhaseMachine) -> Self {
        Self {
            r#loop,
            log,
            machine,
            persisted: 0,
            turn_counter: 0,
            auto_advance: true,
            max_turns: 120,
            stage_turn_budget: 6,
            turns_in_stage: 0,
            last_stage: None,
            history: Vec::new(),
        }
    }

    /// 设置单阶段 turn 预算（见字段文档）。
    pub fn with_stage_turn_budget(mut self, n: u32) -> Self {
        self.stage_turn_budget = n.max(1);
        self
    }

    /// 追加一条消息到跨 turn 历史。
    pub fn push_history(&mut self, item: shangyi_llm::InputItem) {
        self.history.push(item);
    }

    fn apply_user_invalidations(&mut self) -> Result<(),RunError> {
        let invalidated=self.r#loop.take_invalidated_artifacts();
        if invalidated.is_empty(){return Ok(());}
        let had_results=invalidated.iter().any(|kind|self.machine.has_artifact(*kind));
        let from=self.machine.phase();
        if let Some(to)=self.machine.invalidate_user_dependent_artifacts(&invalidated) {
            self.record(Event::PhaseRewound {from:from.as_str().into(),to:to.as_str().into(),
                due_to:"当前上下文或约束发生变化，旧决定产物与相关确认须重新核对".into(),at:self.r#loop_now()},true)?;
        } else if had_results {
            self.record(Event::Warning {message:"当前上下文或约束已更新，受影响的旧决定产物与确认已作废，须重新核对".into(),at:self.r#loop_now()},true)?;
        }
        Ok(())
    }

    /// 当前历史长度（供测试与诊断）。
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// 设置一次 `run()` 的 turn 上限。
    pub fn with_max_turns(mut self, n: u32) -> Self {
        self.max_turns = n.max(1);
        self
    }

    /// 是否在 TurnOutcome::Completed 后自动尝试推进阶段。
    ///
    /// 关掉它（`false`）时，每次推进都要调用方显式 [`Self::try_advance`]——
    /// 交互式 CLI 用它把"推进"变成用户可见的一步。
    pub fn with_auto_advance(mut self, yes: bool) -> Self {
        self.auto_advance = yes;
        self
    }

    pub fn machine(&self) -> &PhaseMachine {
        &self.machine
    }

    pub fn machine_mut(&mut self) -> &mut PhaseMachine {
        &mut self.machine
    }

    pub fn log(&self) -> &CaseLog {
        &self.log
    }

    pub fn loop_events(&self) -> &[Event] {
        self.r#loop.events()
    }

    /// 把循环新产生的事件同步到日志。
    ///
    /// 返回新落盘的条数。**边发生边写**：等到会话结束再写，等于崩溃时
    /// 丢掉整段历史——而崩溃恰恰是最需要历史的时候。
    pub fn persist_new_events(&mut self) -> Result<usize, RunError> {
        let events = self.r#loop.events().to_vec();
        let mut n = 0;
        for e in events.iter().skip(self.persisted) {
            if is_durable(e) {
                self.log.append_durable(e.clone())?;
            } else {
                self.log.append(e.clone())?;
            }
            n += 1;
        }
        self.persisted = events.len();
        Ok(n)
    }

    /// 记一条**非循环**事件（如 checkpoint 结算、阶段推进）。
    pub fn record(&mut self, event: Event, durable: bool) -> Result<(), RunError> {
        if durable {
            self.log.append_durable(event)?;
        } else {
            self.log.append(event)?;
        }
        Ok(())
    }

    /// 跑一个 turn，并把事件落盘。
    pub fn run_turn(
        &mut self,
        input: Vec<shangyi_llm::InputItem>,
    ) -> Result<TurnOutcome, RunError> {
        self.turn_counter += 1;
        let turn = TurnId(self.turn_counter as u64);
        // 把本次输入并入历史，再把**完整历史**交给循环——
        // 这是"turn 之间不丢上下文"的落点。
        self.history.extend(input.iter().cloned());
        let out = self
            .r#loop
            .run_turn(&mut self.machine, turn, self.history.clone())
            .map_err(RunError::from)?;
        // The loop appends assistant replies and paired tool observations.
        // Retaining only our incoming user messages loses verified evidence at
        // every phase boundary and makes the model repeat searches or contradict it.
        self.history = self.r#loop.take_turn_history();
        self.persist_new_events()?;
        Ok(out)
    }

    /// 结算一个 checkpoint。
    ///
    /// # 为什么结算要由运行器做（而不是让模型调工具）
    ///
    /// checkpoint 的作答是**用户的动作**，不是模型的。模型提议"该问什么"
    /// （通过 `checkpoint` 工具），用户回答后由**确定性代码**重算产物并结算——
    /// 让模型去结算等于让被审查者自己盖章（§7.6 第 4 步）。
    pub fn settle(
        &mut self,
        id: CheckpointId,
        answer: &CheckpointAnswer,
        actor: &str,
    ) -> Result<(), RunError> {
        let at = self.r#loop_now();
        match answer {
            CheckpointAnswer::Approve => {
                let fingerprint = self.machine.artifact_fingerprint();
                self.machine.settle_checkpoint(
                    id,
                    CheckpointStatus::Approved {
                        fingerprint: fingerprint.clone(),
                        by: actor.to_string(),
                        at: at.clone(),
                    },
                )?;
                // 用户答题 + 阶段批准：两个都不可逆，都 durable
                self.record(
                    Event::CheckpointAnswered {
                        id: id.as_str().to_string(),
                        answers: vec!["确认".into()],
                        user_actor: actor.to_string(),
                        at: at.clone(),
                    },
                    true,
                )?;
                if let Some(phase) = self.machine.graph().path().iter().copied().find(|p| {
                    self.machine.graph().checkpoint_at(*p) == Some(id)
                }) {
                    self.record(
                        Event::PhaseApproved {
                            phase: phase.as_str().to_string(),
                            fingerprint,
                            at,
                        },
                        true,
                    )?;
                }
                Ok(())
            }
            CheckpointAnswer::Revise { reason } => {
                self.r#loop.record_user_input(reason,actor);
                self.machine.settle_checkpoint(
                    id,
                    CheckpointStatus::Rejected {
                        reason: reason.clone(),
                        at: at.clone(),
                    },
                )?;
                self.record(
                    Event::CheckpointAnswered {
                        id: id.as_str().to_string(),
                        answers: vec![format!("修改：{reason}")],
                        user_actor: actor.to_string(),
                        at: at.clone(),
                    },
                    true,
                )?;
                // 用户否决 ⇒ 阶段回退到该 checkpoint 所在阶段重做。
                // **不由这里自动回退**：回退目标取决于哪个阶段出问题，
                // 那是调用方的领域判断（§2.2：可能进一步退回 intel）。
                Ok(())
            }
            CheckpointAnswer::AskFollowUp { question } => {
                self.r#loop.record_user_input(question,actor);
                // 追问**不结算**：用户还没表态。记一条普通事件说明追问已发出，
                // checkpoint 保持 Open（闸门因此仍然拦着推进）。
                self.record(
                    Event::CheckpointAnswered {
                        id: id.as_str().to_string(),
                        answers: vec![format!("追问：{question}")],
                        user_actor: actor.to_string(),
                        at,
                    },
                    false,
                )?;
                Ok(())
            }
        }
    }

    /// 尝试推进阶段（过闸门才走）。
    pub fn try_advance(&mut self) -> Result<SessionPhase, RunError> {
        self.apply_user_invalidations()?;
        let from = self.machine.phase();
        let to = self.machine.advance()?;
        self.record(
            Event::PhaseEntered {
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
                playbook: self.machine.mode().as_str().to_string(),
                reason: "闸门通过".into(),
                at: self.r#loop_now(),
            },
            true,
        )?;
        Ok(to)
    }

    /// 回退阶段（用户否决 checkpoint 后重做）。
    pub fn rewind_to(&mut self, target: SessionPhase, due_to: &str) -> Result<(), RunError> {
        let from = self.machine.phase();
        self.machine.rollback_to(target)?;
        self.record(
            Event::PhaseRewound {
                from: from.as_str().to_string(),
                to: target.as_str().to_string(),
                due_to: due_to.to_string(),
                at: self.r#loop_now(),
            },
            true,
        )?;
        Ok(())
    }

    pub fn turn_counter(&self) -> u32 {
        self.turn_counter
    }

    /// 时间戳来源：与循环同一个时钟（避免"日志里的时间来自两个时钟"）。
    ///
    /// 取当前事件数作为一次"逻辑时刻"推进的标记不合适，故这里直接用循环
    /// 即将写入的时间——通过多跑一次 `now()` 实现。它由同一个 `Clock` 提供，
    /// 因此与循环事件时间戳**同源**。
    fn r#loop_now(&self) -> String {
        // 与循环写入事件时间戳**同一个时钟**（见 AgentLoop::now 的说明）。
        self.r#loop.now()
    }

    /// 跑到停：反复 turn，遇到挂起/阻塞/失败就如实返回。
    ///
    /// `initial` 是首个 turn 的输入。
    pub fn run<I: UserIo>(
        &mut self,
        mut initial: Vec<shangyi_llm::InputItem>,
        io: &mut I,
        actor: &str,
    ) -> Result<RunReport, RunError> {
        // 若实现自述是自动的，则**忽略调用方传的 actor**，一律记 `auto`。
        //
        // 这条覆盖是刻意的：宁可记成 `auto`（保守、不冒充实人确认），
        // 也不接受一个可能与事实不符的 `user`。审计上"这份分析无人复核"
        // 是安全的默认，反过来则是**危险的错误**。
        let actor: &str = if io.is_automated() { "auto" } else { actor };
        // 崩溃修复：若上一进程留下的日志有未闭合 turn，先补上（§7.1）
        self.repair_if_crashed(io)?;
        let mut task_start_turn = self.turn_counter;

        loop {
            self.apply_user_invalidations()?;
            // 单阶段预算：防"在一个阶段里无限追问"
            let waiting_on_review = self.machine.graph().checkpoint_at(self.machine.phase())
                .and_then(|cp| self.machine.checkpoint_status(cp)).is_some_and(|s| s.is_open());
            for message in if waiting_on_review { Vec::new() } else { io.take_pending_messages() } {
                self.r#loop.record_user_input(&message,actor);
                self.apply_user_invalidations()?;
                self.record(Event::CheckpointAnswered {
                    id: "user_context".into(), answers: vec![message.clone()],
                    user_actor: actor.to_string(), at: self.r#loop_now(),
                }, true)?;
                self.history.push(shangyi_llm::InputItem::user(message));
            }
            let cur_stage = self.machine.phase();
            if self.last_stage != Some(cur_stage) {
                self.last_stage = Some(cur_stage);
                self.turns_in_stage = 0;
            }
            if self.turns_in_stage >= self.stage_turn_budget {
                io.inform(&format!(
                    "在阶段 `{}` 已用满 {} 个 turn 仍未推进。\n\
                     **停下来而不是继续追问**：模型总能想到下一个问题，\
                     而一份永远问不完的 intake 等于没有产出。\n\
                     可选做法：\n\
                     1. 补充信息后用 `shangyi resume` 继续（计数会重置）\n\
                     2. 若已知信息够用，直接推进——模型会如实标注未取得的项\n\
                     3. 若这个剧本的问题确实多，调高 `with_stage_turn_budget`",
                    cur_stage.as_str(),
                    self.stage_turn_budget
                ));
                return Ok(RunReport::StageBudgetExhausted {
                    phase: cur_stage,
                    turns_in_stage: self.turns_in_stage,
                    turns: self.turn_counter,
                });
            }
            self.turns_in_stage += 1;

            // turn 预算：见 `max_turns` 的说明（防"每步合法但永不收敛"）
            if self.turn_counter - task_start_turn >= self.max_turns {
                io.inform(&format!(
                    "已达 turn 上限（{}）而阶段仍未推进——停止并发问，                     因为继续只会继续消耗配额而不改变结果。",
                    self.max_turns
                ));
                return Ok(RunReport::BudgetExhausted {
                    phase: self.machine.phase(),
                    turns: self.turn_counter,
                });
            }

            // 每次 turn 前先看闸门：checkpoint 挂着就直接问用户，
            // 不发请求——这是 §7.6 硬拒绝的落点。
            if let Some(cp) = self.machine.graph().checkpoint_at(self.machine.phase()) {
                let status = self.machine.checkpoint_status(cp);
                let open_items = match status {
                    Some(CheckpointStatus::Open { items }) => Some(items.clone()),
                    _ => None,
                };
                if let Some(items) = open_items {
                    let req = CheckpointRequest {
                        id: cp,
                        phase: self.machine.phase(),
                        items,
                        revision_requested: false,
                    };
                    // 用户的实际回答**直接**用于结算。
                    //
                    // ⚠️ 此处曾有一个 `io_last_answer` 辅助函数，它在结算时
                    // **总是返回 Approve**——于是"用户否决"被记成了"用户确认"。
                    // 那正是本项目存在的理由所要防的事（伪造用户同意），
                    // 而它当时还通过了"否决后停住"的断言（因为停下是因为别的原因）。
                    // 教训：**任何"替用户作答"的辅助函数都不得存在**。
                    let answer = io.present_checkpoint(&req);
                    let is_revise = matches!(answer, CheckpointAnswer::Revise { .. });
                    self.settle(cp, &answer, actor)?;

                    if io.can_revise_in_place() {
                        let request = match &answer {
                            CheckpointAnswer::Revise { reason } => Some(format!("用户修改了本轮材料：{reason}。请重新核对并重新运行受影响的工具，不能沿用被否决的产物。")),
                            CheckpointAnswer::AskFollowUp { question } => Some(format!("用户暂未确认，要求先解释：{question}。请先针对性解释。")),
                            CheckpointAnswer::Approve => None,
                        };
                        if let Some(request) = request {
                            self.machine.reconsider_current_checkpoint(cp)?;
                            self.record(Event::PhaseRewound {
                                from:self.machine.phase().as_str().into(),
                                to:self.machine.phase().as_str().into(),due_to:request.clone(),at:self.r#loop_now(),
                            }, true)?;
                            initial = vec![shangyi_llm::InputItem::user(format!("{request} 当前阶段保持不变，修订后必须重新提交 {cp} 等待用户确认，禁止视为已批准。"))];
                            continue;
                        }
                    }

                    if is_revise {
                        // 否决 ⇒ 停在这里，由调用方决定退到哪（§2.2：可能进一步退回 intel）
                        return Ok(RunReport::Failed {
                            phase: self.machine.phase(),
                            reason: TurnEndReason::Error {
                                detail: format!(
                                    "用户否决了 {cp}，需修订本阶段产物后重新确认"
                                ),
                            },
                            detail: "阶段须回退重做".into(),
                            turns: self.turn_counter,
                        });
                    }
                    // ── 结算完成后**尝试推进** ──
                    //
                    // # 为什么必须在这里（实测暴露的不对称）
                    //
                    // 早先只在 `AwaitingAnswers`（ask_user 挂起）之后尝试推进，
                    // 而 **`AwaitingUser`（checkpoint 挂起）之后没有**。
                    // 后果：一次真实咨询里模型每个 turn 都调 `checkpoint`，
                    // 4 个 turn 全以 `aborted` 结束；cp1 被反复打开/确认，
                    // 阶段却**一次都没推进**（CLI 显示「可进入 model_build」却停在 intel）。
                    //
                    // 两条挂起路径在语义上是对称的：**用户答完/确认完，
                    // 就是尝试推进的时机**。闸门会拦住不该走的（缺产物、
                    // 该确认的没确认），故尝试是安全的。
                    if self.auto_advance {
                        if let Ok(to) = self.try_advance() {
                            io.inform(&format!("阶段推进到 {}", to.as_str()));
                            // ⚠️ **到 archived 后不立即返回**——先让模型跑一轮归档。
                            //
                            // 实测（run31）：此前一到 archived 就 return，
                            // 于是 `case_store_write` **一次都没被调用**——
                            // 而 archived 阶段的指引写着"本阶段必须以它收尾"。
                            // 后果：案例从不入库 ⇒ 校准闭环永远拿不到样本
                            // （即使我把库的持久化修好了，也没有数据可存）。
                            //
                            // 这与"阶段推进后继续跑该阶段的 turn"是同一个语义：
                            // archived 是**路径上的最后一个阶段**，不是终点标记。
                            // 它的交付物（冻结的预测）由 `case_store_write` 产出，
                            // 而产出需要一次 turn。
                            if to == SessionPhase::Archived {
                                continue;
                            }
                        }
                    }

                    if let CheckpointAnswer::AskFollowUp { question } = answer {
                        // 追问**不结算**（checkpoint 仍 Open）⇒ 下一轮仍回到这里。
                        // 把追问作为下一 turn 的输入。
                        initial = vec![shangyi_llm::InputItem::user(format!(
                            "用户对 {cp} 提出追问：{question}。请针对性补充说明，\
                             然后重新提交 checkpoint 提问件。"
                        ))];
                    }
                    continue;
                }
            }

            let out = self.run_turn(initial.clone())?;
            input_consumed(&mut initial);

            match out {
                TurnOutcome::Completed { output, steps } => {
                    if self.machine.scoped_result().is_none() {
                        io.inform(&format!("阶段 {} 的一个 turn 完成（{steps} 步）", self.machine.phase()));
                    }
                    let uncommitted_research=self.machine.scoped_result().is_none()
                        && self.machine.task_scope().is_some_and(|s|s.needs_research());
                    if !output.is_empty()&&!uncommitted_research {
                        io.inform(&output);
                    }

                    if let Some(result) = self.machine.scoped_result().cloned() {
                        io.scoped_task_completed_at(&result,self.machine.phase());
                        if let Some(message) = io.wait_for_followup_task() {
                            self.machine.begin_followup_task()?;
                            self.r#loop.begin_followup_task();
                            self.r#loop.record_user_input(&message,actor);
                            self.apply_user_invalidations()?;
                            self.turns_in_stage=0;
                            self.last_stage=None;
                            task_start_turn=self.turn_counter;
                            self.record(Event::FollowupTaskStarted {at:self.r#loop_now()},true)?;
                            self.record(Event::CheckpointAnswered {
                                id:"followup_task".into(),answers:vec![message.clone()],user_actor:actor.into(),at:self.r#loop_now(),
                            },true)?;
                            initial=vec![shangyi_llm::InputItem::user(format!("用户追加了一个新任务，请按本次原话重新选择范围；保留已有对话与资料但不要沿用先前任务的完成状态：\n{message}"))];
                            continue;
                        }
                        return Ok(RunReport::TaskCompleted {result,phase:self.machine.phase(),turns:self.turn_counter});
                    }

                    // ── archived 的终止条件（run31 实测的一对矛盾）──
                    //
                    // `archived` 是路径的**最后一个阶段**，`try_advance` 对它
                    // 只会失败（没有后继）⇒ 不能用它判断"该不该结束"。
                    //
                    // 正确的判据是它**自己的交付物**齐了没：`CaseArchived`
                    // （由 `case_store_write` 登记）。齐了 ⇒ 会话完成。
                    //
                    // ⚠️ 此前一到 archived 就无条件 `return Completed`，
                    // 于是 `case_store_write` **从未被调用**、案例从不入库。
                    // 而若只是简单 `continue`，又会在 archived 里无限循环。
                    // 用它的产物作判据，两个问题一起解决。
                    if self.machine.phase() == SessionPhase::Archived {
                        if self
                            .machine
                            .has_artifact(crate::phase_graph::ArtifactKind::CaseArchived)
                        {
                            return Ok(RunReport::Completed {
                                phase: SessionPhase::Archived,
                                turns: self.turn_counter,
                            });
                        }
                        // 还没归档 ⇒ 继续给模型 turn（指引要求它调 `case_store_write`）
                        continue;
                    }

                    if self.auto_advance {
                        match self.try_advance() {
                            Ok(to) if to == SessionPhase::Archived => {
                                let _ = to;
                                continue;
                            }
                            Ok(_) => continue,
                            // 闸门没过（缺产物 / checkpoint 未结算）：这是**正常的**
                            // ——下一步会回到闸门检查，必要时问用户。不是错误。
                            Err(RunError::Gate(_)) => continue,
                            Err(e) => return Err(e),
                        }
                    } else {
                        // 手动推进模式：按当前阶段是否真有 checkpoint 选择变体
                        return Ok(match self.machine.graph().checkpoint_at(self.machine.phase()) {
                            Some(cp) => RunReport::AwaitingUser {
                                phase: self.machine.phase(),
                                checkpoint: cp,
                                turns: self.turn_counter,
                            },
                            None => RunReport::AwaitingAnswers {
                                phase: self.machine.phase(),
                                turns: self.turn_counter,
                            },
                        });
                    }
                }
                TurnOutcome::AwaitingUser { checkpoint, .. } => {
                    // 循环自己挂起了（模型调了 checkpoint 工具）：回到顶部处理
                    io.inform(&format!(
                        "等待用户确认 {}（{} 项）",
                        checkpoint.id.as_str(),
                        checkpoint.items.len()
                    ));
                    continue;
                }
                TurnOutcome::AwaitingAnswers { questions, .. } => {
                    // 采集：把问题交给用户，答案作为**用户输入**进历史。
                    //
                    // 注意答案进的是历史（对话的一部分），不是产物——
                    // 采集不改变阶段机状态，也不结算任何 checkpoint。
                    let answers = io.ask(&questions);
                    for (_,answer) in &answers {self.r#loop.record_user_input(answer,actor);}
                    self.apply_user_invalidations()?;

                    // ── 采集之后**尝试推进** ──
                    //
                    // # 为什么必须在这里试（实测暴露的缺口）
                    //
                    // 早先只有 `TurnOutcome::Completed` 之后才 auto_advance。
                    // 而一次真实咨询里，模型在 intake **每个 turn 都调 `ask_user`**，
                    // 于是 6 个 turn 全以 `aborted`（挂起等用户）结束，
                    // `Completed` 一次都没出现 ⇒ 阶段永远不推进 ⇒ 卡死在 intake。
                    //
                    // 正确语义：`intake` 的交付物是"信息收集够了"，
                    // 而它没有产物要求（见 `PhaseGraph::produces`）——
                    // 所以**用户答完一轮之后**就是尝试推进的时机。
                    // 闸门会拦住不该走的（缺产物/该确认的没确认），
                    // 故这里试一下是安全的。
                    if self.auto_advance {
                        if let Ok(to) = self.try_advance() {
                            io.inform(&format!("阶段推进到 {}", to.as_str()));
                            // ⚠️ **到 archived 后不立即返回**——先让模型跑一轮归档。
                            //
                            // 实测（run31）：此前一到 archived 就 return，
                            // 于是 `case_store_write` **一次都没被调用**——
                            // 而 archived 阶段的指引写着"本阶段必须以它收尾"。
                            // 后果：案例从不入库 ⇒ 校准闭环永远拿不到样本
                            // （即使我把库的持久化修好了，也没有数据可存）。
                            //
                            // 这与"阶段推进后继续跑该阶段的 turn"是同一个语义：
                            // archived 是**路径上的最后一个阶段**，不是终点标记。
                            // 它的交付物（冻结的预测）由 `case_store_write` 产出，
                            // 而产出需要一次 turn。
                            if to == SessionPhase::Archived {
                                continue;
                            }
                        }
                        // 推进失败（缺产物/待确认）不是错误——继续当前阶段
                    }
                    if answers.is_empty() {
                        // 采集为空是**正常结局**（用户可能不知道、或已无更多信息）：
                        // 如实报告"等回答"，不编造一个 checkpoint，也不判为失败。
                        io.inform(
                            "用户未提供任何答案（采集为空）。这不是拒绝——\
                             该项目保持「未取得」，不得用推测填补。",
                        );
                        return Ok(RunReport::AwaitingAnswers {
                            phase: self.machine.phase(),
                            turns: self.turn_counter,
                        });
                    }
                    let mut text = String::from("用户对提问的回答：\n");
                    for (id, a) in &answers {
                        text.push_str(&format!("- {id}: {a}\n"));
                    }
                    self.history.push(shangyi_llm::InputItem::user(text));
                    // 记录到事件日志（用户输入是审计对象）
                    self.record(
                        Event::CheckpointAnswered {
                            id: "ask_user".into(),
                            answers: answers
                                .iter()
                                .map(|(id, a)| format!("{id}={a}"))
                                .collect(),
                            user_actor: actor.to_string(),
                            at: self.r#loop_now(),
                        },
                        true,
                    )?;
                    continue;
                }
                TurnOutcome::AwaitingRewindDecision { target, reason, .. } => {
                    // **回退是用户的决定**（§7.5）。这里问，不自行执行。
                    io.inform(&format!(
                        "模型建议回退到阶段 `{}`，理由：{reason}",
                        target.as_str()
                    ));
                    match io.decide_rewind(target, &reason) {
                        RewindDecision::Approve => {
                            // 用户同意 ⇒ 执行回退（清下游产物 + 记事件）
                            match self.rewind_to(target, "模型提议、用户同意") {
                                Ok(()) => {
                                    io.inform(&format!(
                                        "已回退到 `{}`（下游产物已清理）。",
                                        target.as_str()
                                    ));
                                    // 清空历史里"回退前"的部分不合适——历史是审计凭据。
                                    // 但**当前 turn 的输入要换**：告知模型回退已生效。
                                    initial = vec![shangyi_llm::InputItem::user(format!(
                                        "用户已同意回退到 `{}`。请在该阶段补齐材料后重新推进。",
                                        target.as_str()
                                    ))];
                                    continue;
                                }
                                Err(e) => {
                                    return Err(RunError::Gate(e.to_string()));
                                }
                            }
                        }
                        RewindDecision::Reject => {
                            io.inform("用户拒绝了回退，继续当前阶段。");
                            initial = vec![shangyi_llm::InputItem::user(
                                "用户**拒绝**了回退请求。请在当前阶段尽力完成，                                 并把缺来源的假设如实标注为待确认项，不要因此停下。"
                                    .to_string(),
                            )];
                            continue;
                        }
                    }
                }
                TurnOutcome::Blocked { detail, .. } => {
                    io.inform(&format!("会话因外部依赖不可用而暂停：{detail}"));
                    return Ok(RunReport::Blocked {
                        phase: self.machine.phase(),
                        detail,
                        turns: self.turn_counter,
                    });
                }
                TurnOutcome::Failed { reason, detail, .. } => {
                    // 直接用循环给的**可读** detail，不再打印 `{reason:?}`——
                    // 那对用户没有信息量（他会看到一个 Rust 枚举名）。
                    io.inform(&format!("会话失败：{detail}"));
                    return Ok(RunReport::Failed {
                        phase: self.machine.phase(),
                        reason,
                        detail,
                        turns: self.turn_counter,
                    });
                }
            }
        }
    }

    /// 若日志里有崩溃遗留的未闭合 turn，补写 `Interrupted` 收尾。
    ///
    /// 这事**必须由运行器在会话开始时做**：否则恢复出的历史里，那个 turn
    /// 看起来像是"还在进行"，而实际上进程早就没了（§7.1）。
    pub fn repair_if_crashed<I: UserIo>(&mut self, io: &mut I) -> Result<usize, RunError> {
        let events = self.log.events()?;
        let repairs = crate::session::interrupted_turn_closers(&events);
        if repairs.is_empty() {
            return Ok(0);
        }
        io.inform(&format!(
            "检测到上次会话异常结束（未闭合的 turn）：补写 {} 条收尾事件，\
             并把该 turn 的结论标为「中断」——它**不是**一次完整分析。",
            repairs.len()
        ));
        let n = crate::case_log::apply_repairs(&mut self.log, &repairs)?;
        Ok(n)
    }
}

/// 把 `initial` 标记为已消费（下一轮起用新输入）。
///
/// 单独抽出来是为了让"输入只用于一个 turn"这件事**显式可见**——
/// 若下一轮复用同一个输入，模型会看到重复的用户消息（那是 bug 的常见形态）。
fn input_consumed(initial: &mut Vec<shangyi_llm::InputItem>) {
    initial.clear();
}

/// 供 CLI 使用的便捷构造：把"运行器 + 自动确认"的冒烟路径写在一处。
pub fn smoke_run<T: Transport, I: UserIo>(
    runner: &mut SessionRunner<'_, T>,
    initial: Vec<shangyi_llm::InputItem>,
    io: &mut I,
) -> Result<RunReport, RunError> {
    runner.run(initial, io, "auto")
}

// ─────────────────────────── 会话进度自检 ───────────────────────────

/// 会话能否推进（供 CLI 打印"卡在哪、为什么"）。
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub phase: SessionPhase,
    /// 本阶段还缺哪些产物。
    pub missing_artifacts: Vec<ArtifactKind>,
    /// 本阶段待结算的 checkpoint（`None` = 无）。
    pub pending_checkpoint: Option<CheckpointId>,
    /// 本阶段 checkpoint 的状态描述。
    pub checkpoint_state: String,
    /// 当前能否推进。
    pub can_advance: bool,
    /// 不能推进的**可操作**原因。
    pub reason: String,
}

/// 计算会话进度（纯查询，无副作用）。
pub fn progress(machine: &PhaseMachine) -> Progress {
    let phase = machine.phase();
    let missing: Vec<ArtifactKind> = machine
        .graph()
        .produces(phase)
        .iter()
        .filter(|k| !machine.has_artifact(**k))
        .copied()
        .collect();
    let cp = machine.graph().checkpoint_at(phase);
    let state = match cp.and_then(|c| machine.checkpoint_status(c)) {
        None => "未打开".to_string(),
        Some(CheckpointStatus::Open { items }) => format!("等待用户（{} 项）", items.len()),
        Some(CheckpointStatus::Approved { by, .. }) => format!("已由 {by} 确认"),
        Some(CheckpointStatus::Rejected { reason, .. }) => format!("已被否决：{reason}"),
    };

    match machine.gate_to_advance() {
        Ok(next) => Progress {
            phase,
            missing_artifacts: missing,
            pending_checkpoint: cp,
            checkpoint_state: state,
            can_advance: true,
            reason: format!("可进入 {}", next.as_str()),
        },
        Err(e) => Progress {
            phase,
            missing_artifacts: missing,
            pending_checkpoint: cp,
            checkpoint_state: state,
            can_advance: false,
            reason: e.to_string(),
        },
    }
}

/// 供外部构造"中止"报告的辅助（用户按 Esc 等）。
pub fn abort_report(phase: SessionPhase, turns: u32) -> RunReport {
    RunReport::Aborted { phase, turns }
}

/// 中止原因（保留 `CancelCause` 的使用点，避免它成为死代码）。
pub fn abort_cause_user() -> CancelCause {
    CancelCause::User
}

/// 会话配置的默认值（供 CLI 与测试共用一处）。
pub fn default_loop_config(model: &str, case_id: &str) -> LoopConfig {
    LoopConfig::new(model, case_id)
}

/// 执行器类型别名（避免调用方写长泛型）。
pub type DynExecutor = dyn ToolExecutor;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ExecOutcome, ToolCall};
    use crate::case_log::CaseHeader;
    use crate::consultation::ConsultationMode;
    use crate::phase_graph::{CheckpointItem, PhaseGraph};
    use shangyi_llm::{Endpoint, LlmClient, LlmError};
    use shangyi_tools::ToolRegistry;
    use std::cell::RefCell;

    // ── 假传输层 ──

    struct FakeTransport {
        responses: RefCell<Vec<(u16, String)>>,
        seen: std::rc::Rc<RefCell<Vec<serde_json::Value>>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                responses: RefCell::new(responses),
                seen: std::rc::Rc::new(RefCell::new(Vec::new())),
            }
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
                return Err(LlmError::Transport("响应耗尽".into()));
            }
            Ok(r.remove(0))
        }
    }

    fn text_response(text: &str) -> String {
        serde_json::json!({
            "id": "r",
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "text", "text": text}]}],
        })
        .to_string()
    }

    fn call_response(id: &str, name: &str, args: &str) -> String {
        serde_json::json!({
            "id": "r",
            "status": "completed",
            "output": [{"type": "function_call", "call_id": id, "name": name, "arguments": args}],
        })
        .to_string()
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shangyi-runner-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("建目录");
        p
    }

    fn client(responses: Vec<(u16, String)>) -> LlmClient<FakeTransport> {
        let ep = Endpoint {
            base_url: "https://example.invalid".into(),
            api_key_env: "UNUSED".into(),
            wire: shangyi_llm::Wire::Responses,
            echo_reasoning: true,
        };
        LlmClient::new(ep, FakeTransport::new(responses)).with_key("k")
    }

    /// 无状态执行器：任何调用都成功返回 `{}`。
    struct OkExecutor;
    impl ToolExecutor for OkExecutor {
        fn execute(&mut self, _call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
            ExecOutcome::Ok("{}".into())
        }
    }

    /// 产出指定产物的执行器（模拟引擎把产物交给阶段机）。
    struct ArtifactExecutor {
        kinds: Vec<ArtifactKind>,
    }
    impl ToolExecutor for ArtifactExecutor {
        fn execute(&mut self, _call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
            ExecOutcome::Ok("{}".into())
        }
        fn artifacts(&self) -> Vec<(ArtifactKind, String)> {
            self.kinds.iter().map(|k| (*k, "fp".to_string())).collect()
        }
    }

    fn header(id: &str) -> CaseHeader {
        CaseHeader {
            case_id: id.into(),
            mode: "scenario".into(),
            playbook_id: "p".into(),
            created_at: "2026-09-17T00:00:00Z".into(),
            schema_version: CaseHeader::SCHEMA_VERSION,
        }
    }

    /// 脚本化的 UserIo：按预设序列回答，并记录被问过哪些。
    struct ScriptedIo {
        answers: RefCell<Vec<CheckpointAnswer>>,
        asked: RefCell<Vec<CheckpointId>>,
        informed: RefCell<Vec<String>>,
        /// 采集问题的答案（`ask_user` 用），按 id 配对。
        answers_for_ask: RefCell<Vec<(String, String)>>,
        asked_questions: RefCell<Vec<String>>,
    }
    impl ScriptedIo {
        fn new(answers: Vec<CheckpointAnswer>) -> Self {
            Self {
                answers: RefCell::new(answers),
                asked: RefCell::new(Vec::new()),
                informed: RefCell::new(Vec::new()),
                answers_for_ask: RefCell::new(Vec::new()),
                asked_questions: RefCell::new(Vec::new()),
            }
        }
        fn with_answers_for(mut self, pairs: Vec<(&str, &str)>) -> Self {
            self.answers_for_ask = RefCell::new(
                pairs.into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
            );
            self
        }
    }
    impl UserIo for ScriptedIo {
        fn ask(&mut self, questions: &[crate::agent::UserQuestion]) -> Vec<(String, String)> {
            self.asked_questions
                .borrow_mut()
                .extend(questions.iter().map(|q| q.id.clone()));
            // 脚本提供的答案（按顺序一一对应）；不足则**只回有答案的那些**，
            // 不补齐占位——未回答就是未回答。
            questions
                .iter()
                .filter_map(|q| {
                    self.answers_for_ask
                        .borrow_mut()
                        .iter()
                        .position(|(id, _)| id == &q.id)
                        .map(|i| self.answers_for_ask.borrow_mut().remove(i))
                })
                .collect()
        }
        fn present_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer {
            self.asked.borrow_mut().push(req.id);
            let mut a = self.answers.borrow_mut();
            if a.is_empty() {
                // 没人预设回答时**默认拒绝**（fail-closed）：脚本没写清楚，
                // 不该被当成"用户同意了"。这与 §7.6 的"无应答者 ⇒ 拒绝推进"同源。
                return CheckpointAnswer::Revise {
                    reason: "脚本未提供回答（fail-closed）".into(),
                };
            }
            a.remove(0)
        }
        fn inform(&mut self, message: &str) {
            self.informed.borrow_mut().push(message.to_string());
        }
    }

    // ── durable 策略 ──

    #[test]
    fn durable_policy_covers_irreversible_user_actions() {
        // 用户的关键动作必须 durable
        assert!(is_durable(&Event::CheckpointAnswered {
            id: "cp1".into(),
            answers: vec![],
            user_actor: "u".into(),
            at: "t".into(),
        }));
        assert!(is_durable(&Event::CheckpointOpened {
            id: "cp1".into(),
            phase: "intel".into(),
            items: vec![],
            at: "t".into(),
        }));
        assert!(is_durable(&Event::PhaseApproved {
            phase: "intel".into(),
            fingerprint: "f".into(),
            at: "t".into(),
        }));
        assert!(is_durable(&Event::PhaseRewound {
            from: "a".into(),
            to: "b".into(),
            due_to: "d".into(),
            at: "t".into(),
        }));
        assert!(is_durable(&Event::TurnEnded {
            turn: TurnId(1),
            reason: TurnEndReason::Completed,
            at: "t".into(),
        }));

        // 可重跑的步骤事件**不必** durable（每步 fsync 会拖慢循环）
        assert!(!is_durable(&Event::StepStarted {
            step: crate::session::StepId::new(TurnId(1), 0),
            at: "t".into(),
        }));
        assert!(!is_durable(&Event::StepEnded {
            step: crate::session::StepId::new(TurnId(1), 0),
            at: "t".into(),
        }));
    }


    fn scope_playbook() -> shangyi_playbooks::Playbook {
        shangyi_playbooks::Playbook {
            meta:shangyi_playbooks::PlaybookMeta {id:"scope-test".into(),name:"任务范围测试".into(),description:String::new(),order:0,version:1},
            applicable_when:vec![],fails_when:vec![],required_inputs:vec![],value_net:None,intake:vec![],models:vec![],
            selectors:std::collections::BTreeMap::new(),
            tool_allowlist:vec!["web_fetch".into(),"web_search".into(),"business_cost".into()],
            report_vars:std::collections::BTreeMap::new(),
        }
    }

    struct ScopePage;
    impl crate::domain_exec::SearchProvider for ScopePage {
        fn search(&self,_:&str)->Result<Vec<crate::domain_exec::SearchHit>,crate::domain_exec::SearchUnavailable>{Ok(vec![])}
        fn fetch(&self,url:&str)->Result<crate::domain_exec::FetchedText,crate::domain_exec::SearchUnavailable>{
            Ok(crate::domain_exec::FetchedText{url:url.into(),text:"官方测试正文：费用信息未发布。".into(),raw_bytes:50})
        }
    }

    #[derive(Default)]
    struct ScopeIo {
        messages:Vec<String>,
        delivered:Vec<crate::task_scope::ScopedTaskResult>,
        followups:std::collections::VecDeque<String>,
    }
    impl UserIo for ScopeIo {
        fn present_checkpoint(&mut self,_:&CheckpointRequest)->CheckpointAnswer {panic!("short task must never open a checkpoint")}
        fn inform(&mut self,message:&str){self.messages.push(message.into());}
        fn scoped_task_completed(&mut self,result:&crate::task_scope::ScopedTaskResult){self.delivered.push(result.clone());}
        fn wait_for_followup_task(&mut self)->Option<String>{self.followups.pop_front()}
    }

    #[test]
    fn research_scope_delivers_without_entering_game_analysis_or_checkpoint() {
        let c=client(vec![
            (200,call_response("scope","select_task_scope",r#"{"scope":"research","reason":"用户只要求查原始公开资料"}"#)),
            (200,call_response("plan","research_task_plan",r#"{"fields":[{"id":"body","label":"官方测试正文","request_quote":"查官方测试正文","evidence_required":"original_text"}]}"#)),
            (200,call_response("fetch","web_fetch",r#"{"url":"https://example.invalid/official"}"#)),
            (200,call_response("record","research_task_update",r#"{"updates":[{"field_id":"body","status":"supported","evidence":[{"value":"官方测试正文","source_url":"https://example.invalid/official","quote":"官方测试正文：费用信息未发布。"}]}]}"#)),
            (200,call_response("done","complete_task",r#"{"summary":"已读取官方测试正文，页面尚未发布费用信息。来源：https://example.invalid/official","missing":[]}"#)),
        ]);
        let theory=shangyi_kb::TheoryLibrary::new();let pb=scope_playbook();let reg=ToolRegistry::standard();let search=ScopePage;
        let mut ex=crate::domain_exec::DomainExecutor::new(&theory,&pb).with_search(&search);
        ex.record_user_input("查官方测试正文","user");
        let agent=AgentLoop::new(c,&reg,&mut ex,default_loop_config("m","scope-research")).with_playbook(&pb);
        let log=CaseLog::open(&tmp_dir("scope-research"),&header("scope-research")).unwrap();
        let mut runner=SessionRunner::new(agent,log,PhaseMachine::new(ConsultationMode::Scenario).require_task_scope_selection());
        let mut io=ScopeIo::default();
        let report=runner.run(vec![shangyi_llm::InputItem::user("查官方测试正文")],&mut io,"user").unwrap();
        assert!(matches!(report,RunReport::TaskCompleted{ref result,..} if result.is_complete()));
        assert_eq!(runner.machine().phase(),SessionPhase::Intake);
        assert!(!runner.machine().has_artifact(ArtifactKind::GameSpecValidated));
        assert!(!runner.machine().has_artifact(ArtifactKind::CaseArchived));
        assert_eq!(io.delivered[0].source_urls,vec!["https://example.invalid/official"]);
        assert!(io.messages.iter().any(|m|m.contains("官方测试正文")));
        assert!(!runner.loop_events().iter().any(|e|matches!(e,Event::CheckpointOpened{..})));
    }

    #[test]
    fn conditional_delivery_keeps_the_session_for_a_fresh_followup_and_records_only_raw_user_inputs() {
        use crate::conditional::{ConditionalSnapshot,ConditionalTrigger,EvidenceGap,EvidenceGapReason,build_conditional_result};
        use crate::task_scope::{ScopedTaskResult,TaskResultStatus,TaskScope};
        struct Executor {inputs:std::rc::Rc<RefCell<Vec<(String,String)>>>,fresh:bool}
        impl ToolExecutor for Executor {
            fn execute(&mut self,call:&ToolCall,_:SessionPhase)->ExecOutcome {
                match call.name.as_str() {
                    "ask_user"=>ExecOutcome::AsksUser(vec![crate::agent::UserQuestion{id:"q".into(),text:"对方收益是否已取得？".into(),kind:"text".into(),options:vec![]}]),
                    "gamespec_build"=>ExecOutcome::InsufficientEvidence{gaps:vec![EvidenceGap{reason:EvidenceGapReason::UnknownOpponentPayoff,participant_id:Some("other".into()),profile:None,detail:"对方收益未知".into()}]},
                    "select_task_scope"=>ExecOutcome::TaskScopeSelected{scope:TaskScope::Research,reason:"用户追加只查资料".into()},
                    "complete_task"=>ExecOutcome::ScopedTaskCompleted(ScopedTaskResult{scope:TaskScope::Research,status:TaskResultStatus::Complete,summary:"受控夹具已整理资料".into(),missing:vec![],source_urls:vec![],retrieval_attempts:1,cost_scenarios:vec![],conditional:None,research:None}),
                    _=>ExecOutcome::Ok("受控资料结果".into()),
                }
            }
            fn conditional_fallback(&mut self,gaps:&[EvidenceGap])->Result<ScopedTaskResult,String> {
                build_conditional_result(&ConditionalSnapshot{question:"怎样在底线内续租？".into(),gaps:gaps.to_vec(),..Default::default()},ConditionalTrigger::RepeatedNoProgress)
            }
            fn record_user_input(&mut self,text:&str,actor:&str){self.inputs.borrow_mut().push((text.into(),actor.into()));}
            fn begin_followup_task(&mut self){self.fresh=true;}
            fn artifacts(&self)->Vec<(ArtifactKind,String)>{if self.fresh {vec![]}else{vec![(ArtifactKind::ParticipantProfiles,"p".into())]}}
        }
        struct Io {queued:Vec<String>,followups:std::collections::VecDeque<String>,delivered:Vec<(ScopedTaskResult,SessionPhase)>}
        impl UserIo for Io {
            fn present_checkpoint(&mut self,_:&CheckpointRequest)->CheckpointAnswer{panic!("conditional delivery must not invent a confirmation")}
            fn inform(&mut self,_:&str){}
            fn take_pending_messages(&mut self)->Vec<String>{std::mem::take(&mut self.queued)}
            fn ask(&mut self,_:&[crate::agent::UserQuestion])->Vec<(String,String)>{vec![("q".into(),"我不知道对方的收益，请保留未知。".into())]}
            fn scoped_task_completed_at(&mut self,result:&ScopedTaskResult,phase:SessionPhase){self.delivered.push((result.clone(),phase));}
            fn wait_for_followup_task(&mut self)->Option<String>{self.followups.pop_front()}
        }
        let c=client(vec![(200,call_response("ask","ask_user","{}")),
            (200,call_response("g1","gamespec_build","{}")),(200,call_response("g2","gamespec_build","{}")),(200,call_response("g3","gamespec_build","{}")),
            (200,call_response("scope","select_task_scope",r#"{"scope":"research","reason":"只查资料"}"#)),
            (200,call_response("fetch","web_fetch",r#"{"url":"https://example.invalid"}"#)),
            (200,call_response("complete","complete_task",r#"{"summary":"受控资料","missing":[]}"#))]);
        let seen=c.transport().seen.clone();let inputs=std::rc::Rc::new(RefCell::new(Vec::new()));
        let mut ex=Executor{inputs:inputs.clone(),fresh:false};let registry=ToolRegistry::standard();
        let agent=AgentLoop::new(c,&registry,&mut ex,default_loop_config("m","conditional-followup"));
        let mut machine=PhaseMachine::new(ConsultationMode::Scenario);machine.advance().unwrap();machine.record(ArtifactKind::ParticipantProfiles,"p");
        machine.open_checkpoint(CheckpointRequest{id:CheckpointId::Cp1,phase:SessionPhase::Intel,items:vec![CheckpointItem{id:"fixture".into(),content:"fixture review".into(),source:None,proposed_value:None}],revision_requested:false}).unwrap();
        machine.settle_checkpoint(CheckpointId::Cp1,CheckpointStatus::Approved{fingerprint:machine.artifact_fingerprint(),by:"fixture".into(),at:"fixture".into()}).unwrap();machine.advance().unwrap();
        let log=CaseLog::open(&tmp_dir("conditional-followup"),&header("conditional-followup")).unwrap();
        let mut runner=SessionRunner::new(agent,log,machine);
        let mut io=Io{queued:vec!["租金上限每月11000元。".into()],followups:std::collections::VecDeque::from(["追加只查租赁政策，不做新假设。".into()]),delivered:vec![]};
        let result=runner.run(vec![shangyi_llm::InputItem::user("由系统拼接的intake模板不是用户证据")],&mut io,"user").unwrap();
        assert!(matches!(result,RunReport::TaskCompleted{phase:SessionPhase::Intake,..}));assert_eq!(io.delivered.len(),2);
        assert_eq!(io.delivered[0].0.scope,TaskScope::Conditional);assert_eq!(io.delivered[0].1,SessionPhase::ModelBuild);
        assert!(!io.delivered[0].0.is_complete());assert_eq!(io.delivered[1].0.scope,TaskScope::Research);
        assert_eq!(inputs.borrow().as_slice(),&[("租金上限每月11000元。".into(),"user".into()),("我不知道对方的收益，请保留未知。".into(),"user".into()),("追加只查租赁政策，不做新假设。".into(),"user".into())]);
        assert_eq!(seen.borrow().len(),7);assert!(runner.machine().checkpoint_status(CheckpointId::Cp1).is_none());
        assert!(!runner.machine().has_artifact(ArtifactKind::GameSpecValidated));
    }

    #[test]
    fn uncommitted_research_prose_is_not_delivered_before_the_completion_contract() {
        let c=client(vec![(200,call_response("scope","select_task_scope",r#"{"scope":"research","reason":"只查资料"}"#)),
            (200,text_response("不可信的提前总结：我猜全部事实都核对了")),
            (200,call_response("fetch","web_fetch",r#"{"url":"https://example.invalid/official"}"#)),
            (200,call_response("done","complete_task",r#"{"summary":"现在才是依据工具收尾的答复。","missing":[]}"#))]);
        struct Delivery;
        impl ToolExecutor for Delivery {
            fn execute(&mut self,call:&ToolCall,_:SessionPhase)->ExecOutcome {
                match call.name.as_str() {
                    "select_task_scope"=>ExecOutcome::TaskScopeSelected{scope:crate::task_scope::TaskScope::Research,reason:"只查资料".into()},
                    "complete_task"=>ExecOutcome::ScopedTaskCompleted(crate::task_scope::ScopedTaskResult{scope:crate::task_scope::TaskScope::Research,status:crate::task_scope::TaskResultStatus::Complete,summary:"已通过交付契约的受控结果".into(),missing:vec![],source_urls:vec![],retrieval_attempts:1,cost_scenarios:vec![],conditional:None,research:None}),
                    _=>ExecOutcome::Ok("受控工具结果".into()),
                }
            }
        }
        let reg=ToolRegistry::standard();let mut ex=Delivery;
        let agent=AgentLoop::new(c,&reg,&mut ex,default_loop_config("m","research-contract"));
        let log=CaseLog::open(&tmp_dir("research-contract"),&header("research-contract")).unwrap();
        let mut runner=SessionRunner::new(agent,log,PhaseMachine::new(ConsultationMode::Scenario).require_task_scope_selection());let mut io=ScopeIo::default();
        let report=runner.run(vec![],&mut io,"user").unwrap();
        assert!(matches!(report,RunReport::TaskCompleted{..}));assert_eq!(io.delivered.len(),1);
        assert!(io.messages.iter().any(|m|m=="已通过交付契约的受控结果"));
        assert!(!io.messages.iter().any(|m|m.contains("不可信的提前总结")));
    }

    #[test]
    fn checkpoint_edits_and_questions_register_original_words_without_generated_prefixes() {
        struct Capture(std::rc::Rc<RefCell<Vec<(String,String)>>>);
        impl ToolExecutor for Capture {
            fn execute(&mut self,_:&ToolCall,_:SessionPhase)->ExecOutcome{ExecOutcome::Ok("{}".into())}
            fn record_user_input(&mut self,text:&str,actor:&str){self.0.borrow_mut().push((text.into(),actor.into()));}
        }
        let captured=std::rc::Rc::new(RefCell::new(Vec::new()));let mut ex=Capture(captured.clone());let registry=ToolRegistry::standard();
        let agent=AgentLoop::new(client(vec![]),&registry,&mut ex,default_loop_config("m","raw-checkpoint"));
        let mut machine=PhaseMachine::new(ConsultationMode::Scenario);machine.advance().unwrap();
        machine.open_checkpoint(CheckpointRequest{id:CheckpointId::Cp1,phase:SessionPhase::Intel,items:vec![CheckpointItem{id:"x".into(),content:"待核对".into(),source:None,proposed_value:None}],revision_requested:false}).unwrap();
        let log=CaseLog::open(&tmp_dir("raw-checkpoint"),&header("raw-checkpoint")).unwrap();let mut runner=SessionRunner::new(agent,log,machine);
        runner.settle(CheckpointId::Cp1,&CheckpointAnswer::AskFollowUp{question:"这个上限依据是什么？".into()},"user").unwrap();
        runner.settle(CheckpointId::Cp1,&CheckpointAnswer::Revise{reason:"我的租金上限改为每月10000元。".into()},"user").unwrap();
        assert_eq!(captured.borrow().as_slice(),&[("这个上限依据是什么？".into(),"user".into()),("我的租金上限改为每月10000元。".into(),"user".into())]);
    }

    #[test]
    fn automated_io_cannot_register_queued_text_as_a_real_user_statement() {
        struct Capture(std::rc::Rc<RefCell<Vec<String>>>);
        impl ToolExecutor for Capture {
            fn execute(&mut self,_:&ToolCall,_:SessionPhase)->ExecOutcome{ExecOutcome::Ok("{}".into())}
            fn record_user_input(&mut self,_:&str,actor:&str){self.0.borrow_mut().push(actor.into());}
        }
        struct Automated(bool);
        impl UserIo for Automated {
            fn is_automated(&self)->bool{true}
            fn take_pending_messages(&mut self)->Vec<String>{if self.0 {self.0=false;vec!["脚本自报的上限".into()]}else{vec![]}}
            fn inform(&mut self,_:&str){}
            fn present_checkpoint(&mut self,_:&CheckpointRequest)->CheckpointAnswer{panic!("no checkpoint expected")}
        }
        let captured=std::rc::Rc::new(RefCell::new(Vec::new()));let mut ex=Capture(captured.clone());let registry=ToolRegistry::standard();
        let agent=AgentLoop::new(client(vec![(200,text_response("运行记录"))]),&registry,&mut ex,default_loop_config("m","auto-source"));
        let log=CaseLog::open(&tmp_dir("auto-source"),&header("auto-source")).unwrap();
        let mut runner=SessionRunner::new(agent,log,PhaseMachine::new(ConsultationMode::Scenario)).with_auto_advance(false);
        runner.run(vec![],&mut Automated(true),"user").unwrap();
        assert_eq!(captured.borrow().as_slice(),&["auto"]);
    }

    #[test]
    fn unavailable_research_is_a_partial_delivery_not_a_fabricated_success() {
        let c=client(vec![
            (200,call_response("scope","select_task_scope",r#"{"scope":"research","reason":"用户仅查资料"}"#)),
            (200,call_response("plan","research_task_plan",r#"{"fields":[{"id":"body","label":"示例公开资料","request_quote":"查示例公开资料","evidence_required":"original_text"}]}"#)),
            (200,call_response("search","web_search",r#"{"query":"示例公开资料"}"#)),
            (200,call_response("record","research_task_update",r#"{"updates":[{"field_id":"body","status":"unavailable","reason_code":"provider_unavailable"}]}"#)),
            (200,call_response("done","complete_task",r#"{"summary":"当前未取得检索结果，无法确认所问事实。","missing":[]}"#)),
        ]);
        let theory=shangyi_kb::TheoryLibrary::new();let pb=scope_playbook();let reg=ToolRegistry::standard();
        let mut ex=crate::domain_exec::DomainExecutor::new(&theory,&pb);
        ex.record_user_input("查示例公开资料","user");
        let agent=AgentLoop::new(c,&reg,&mut ex,default_loop_config("m","scope-missing")).with_playbook(&pb);
        let log=CaseLog::open(&tmp_dir("scope-missing"),&header("scope-missing")).unwrap();
        let mut runner=SessionRunner::new(agent,log,PhaseMachine::new(ConsultationMode::Scenario).require_task_scope_selection());
        let report=runner.run(vec![shangyi_llm::InputItem::user("查公开资料")],&mut ScopeIo::default(),"user").unwrap();
        let RunReport::TaskCompleted{result,..}=report else {panic!("expected partial task delivery")};
        assert!(!result.is_complete());assert!(!result.missing.is_empty());assert_eq!(result.retrieval_attempts,1);
    }

    #[test]
    fn followup_task_resets_scope_receipts_and_budget_but_preserves_history() {
        let c=client(vec![
            (200,call_response("scope-1","select_task_scope",r#"{"scope":"research","reason":"用户只查公开资料"}"#)),
            (200,call_response("plan-1","research_task_plan",r#"{"fields":[{"id":"body","label":"官方测试正文","request_quote":"查官方测试正文","evidence_required":"original_text"}]}"#)),
            (200,call_response("fetch-1","web_fetch",r#"{"url":"https://example.invalid/official"}"#)),
            (200,call_response("record-1","research_task_update",r#"{"updates":[{"field_id":"body","status":"supported","evidence":[{"value":"官方测试正文","source_url":"https://example.invalid/official","quote":"官方测试正文：费用信息未发布。"}]}]}"#)),
            (200,call_response("done-1","complete_task",r#"{"summary":"已整理官方资料。","missing":[]}"#)),
            (200,call_response("scope-2","select_task_scope",r#"{"scope":"cost","reason":"用户追加费用计算"}"#)),
            (200,call_response("cost-2","business_cost",r#"{"operation":"create","scenario":"设备报价","currency":"CNY","costs_complete":false,"costs":[{"name":"设备","scenario":"设备报价","treatment":"cash_expense","amount":null,"period":"once","basis":"user_provided","source":"报价未取得"}]}"#)),
            (200,call_response("done-2","complete_task",r#"{"summary":"设备报价尚未取得，金额保留未知。","missing":[]}"#)),
        ]);
        let seen=c.transport().seen.clone();
        let theory=shangyi_kb::TheoryLibrary::new();let pb=scope_playbook();let reg=ToolRegistry::standard();let search=ScopePage;
        let mut ex=crate::domain_exec::DomainExecutor::new(&theory,&pb).with_search(&search);
        ex.record_user_input("查官方测试正文","user");
        let agent=AgentLoop::new(c,&reg,&mut ex,default_loop_config("m","scope-followup")).with_playbook(&pb);
        let log=CaseLog::open(&tmp_dir("scope-followup"),&header("scope-followup")).unwrap();
        let mut runner=SessionRunner::new(agent,log,PhaseMachine::new(ConsultationMode::Scenario).require_task_scope_selection()).with_max_turns(1).with_stage_turn_budget(1);
        let mut io=ScopeIo::default();io.followups.push_back("再算设备费用，报价还没取得。".into());
        let report=runner.run(vec![shangyi_llm::InputItem::user("先查资料")],&mut io,"user").unwrap();
        assert!(matches!(report,RunReport::TaskCompleted{..}));
        assert_eq!(io.delivered.len(),2);assert!(io.delivered[0].is_complete());assert!(!io.delivered[1].is_complete());
        assert!(io.delivered[1].source_urls.is_empty());assert_eq!(io.delivered[1].retrieval_attempts,0);
        assert!(io.delivered[1].cost_scenarios[0]["items"][0]["normalized_amount"].is_null());
        assert_eq!(runner.turn_counter(),2);
        assert!(seen.borrow()[5]["input"].to_string().contains("官方测试正文"));
        assert!(!runner.machine().has_artifact(ArtifactKind::CaseArchived));
    }

    #[test]
    fn verified_tool_evidence_and_answer_survive_phase_change_without_stale_instructions() {
        let transport=FakeTransport::new(vec![
            (200,call_response("fetch-1","web_fetch",r#"{"url":"https://example.invalid/official"}"#)),
            (200,text_response("已从官方正文核实历史活动时间。")),
            (200,call_response("profile-1","participant_profile_update","{}")),
            (200,text_response("沿用已核实的正文，不重复搜索。")),
        ]);
        let seen=transport.seen.clone();
        let endpoint=Endpoint {base_url:"https://example.invalid".into(),api_key_env:"UNUSED".into(),wire:shangyi_llm::Wire::Responses,echo_reasoning:true};
        let c=LlmClient::new(endpoint,transport).with_key("k");
        struct EvidenceExecutor {profile:bool}
        impl ToolExecutor for EvidenceExecutor {
            fn execute(&mut self,call:&ToolCall,_phase:SessionPhase)->ExecOutcome {
                if call.name=="participant_profile_update" {self.profile=true;}
                ExecOutcome::Ok("FETCHED_OFFICIAL_BODY: source=https://example.invalid/official; published=2025-04-24; festival_end=2025-06-30".into())
            }
            fn artifacts(&self)->Vec<(ArtifactKind,String)> {
                if self.profile {vec![(ArtifactKind::ParticipantProfiles,"profile-v1".into())]} else {vec![]}
            }
        }
        let reg=ToolRegistry::standard();let mut ex=EvidenceExecutor {profile:false};
        let agent=AgentLoop::new(c,&reg,&mut ex,default_loop_config("m","evidence"));
        let log=CaseLog::open(&tmp_dir("carry-evidence"),&header("evidence")).unwrap();
        let mut runner=SessionRunner::new(agent,log,PhaseMachine::new(ConsultationMode::Scenario));
        assert!(matches!(runner.run_turn(vec![shangyi_llm::InputItem::user("查历史活动")]).unwrap(),TurnOutcome::Completed{..}));
        assert_eq!(runner.try_advance().unwrap(),SessionPhase::Intel);
        assert!(matches!(runner.run_turn(vec![]).unwrap(),TurnOutcome::Completed{..}));
        let requests=seen.borrow();let next=&requests[2]["input"];
        let body=next.to_string();
        assert!(body.contains("FETCHED_OFFICIAL_BODY")&&body.contains("2025-06-30"));
        assert!(body.contains("已从官方正文核实历史活动时间"));
        let rows=next.as_array().unwrap();
        assert_eq!(rows.iter().filter(|x|x["type"]=="function_call"&&x["call_id"]=="fetch-1").count(),1);
        assert_eq!(rows.iter().filter(|x|x["type"]=="function_call_output"&&x["call_id"]=="fetch-1").count(),1);
        assert!(!runner.history.iter().any(|x|matches!(x,shangyi_llm::InputItem::Message{role,..} if role=="system")),"step instructions must be regenerated, not persisted into conversation history");
    }

    // ── 正常路径 ──

    #[test]
    fn completed_turn_is_persisted_to_log() {
        let dir = tmp_dir("persist");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();
        let mut ex = OkExecutor;
        let l = AgentLoop::new(client(vec![(200, text_response("完成"))]), &reg, &mut ex, default_loop_config("m", "c1"))
            .with_seq_clock();
        let mut r = SessionRunner::new(l, log, PhaseMachine::new(ConsultationMode::Scenario));

        r.run_turn(vec![shangyi_llm::InputItem::user("开始")]).unwrap();

        // 日志里必须已有事件（边发生边写）
        let events = r.log().events().unwrap();
        assert!(!events.is_empty(), "事件必须落盘，不得等到会话结束");
        assert!(events.iter().any(|e| matches!(e, Event::TurnStarted { .. })));
        assert!(events.iter().any(|e| matches!(e, Event::TurnEnded { .. })));
    }

    /// 事件只落盘一次：重复 `persist_new_events` 不得重复写（游标语义）。
    #[test]
    fn persist_is_idempotent_via_cursor() {
        let dir = tmp_dir("cursor");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();
        let mut ex = OkExecutor;
        let l = AgentLoop::new(client(vec![(200, text_response("x"))]), &reg, &mut ex, default_loop_config("m", "c"))
            .with_seq_clock();
        let mut r = SessionRunner::new(l, log, PhaseMachine::new(ConsultationMode::Scenario));

        r.run_turn(vec![shangyi_llm::InputItem::user("a")]).unwrap();
        let n1 = r.log().read_all().unwrap().len();
        let extra = r.persist_new_events().unwrap();
        assert_eq!(extra, 0, "第二次同步不应重复写");
        assert_eq!(r.log().read_all().unwrap().len(), n1);
    }

    // ── 用户否决 ──

    #[test]
    fn browser_revision_or_followup_reaches_model_without_approving_checkpoint() {
        struct ReviewIo(ScriptedIo);
        impl UserIo for ReviewIo {
            fn can_revise_in_place(&self) -> bool { true }
            fn present_checkpoint(&mut self, r: &CheckpointRequest) -> CheckpointAnswer {
                self.0.present_checkpoint(r)
            }
            fn inform(&mut self, message: &str) { self.0.inform(message); }
        }
        for (tag, answer) in [
            ("web-revise", CheckpointAnswer::Revise { reason:"更正：我关心退出条件".into() }),
            ("web-followup", CheckpointAnswer::AskFollowUp { question:"请解释来源".into() }),
        ] {
            let dir=tmp_dir(tag);
            let log=CaseLog::open(&dir,&header(tag)).unwrap();
            let reg=ToolRegistry::standard();
            let mut ex=ArtifactExecutor { kinds:vec![ArtifactKind::ParticipantProfiles] };
            let l=AgentLoop::new(client(vec![(200,text_response("收到，我会重新核对后请你确认。"))]),
                &reg,&mut ex,default_loop_config("m",tag)).with_seq_clock();
            let mut m=PhaseMachine::new(ConsultationMode::Scenario);
            m.advance().unwrap();
            m.open_checkpoint(CheckpointRequest { id:CheckpointId::Cp1,phase:SessionPhase::Intel,
                items:vec![CheckpointItem { id:"f".into(),content:"待确认".into(),
                    source:Some("用户描述".into()),proposed_value:None }],revision_requested:false }).unwrap();
            let mut runner=SessionRunner::new(l,log,m).with_max_turns(1);
            let mut io=ReviewIo(ScriptedIo::new(vec![answer]));
            runner.run(vec![shangyi_llm::InputItem::user("初始问题")],&mut io,"user").unwrap();
            assert_eq!(runner.turn_counter(),1,"应将追问/修正交回模型，而不是重新询问同一个确认框");
            assert_eq!(runner.machine().phase(),SessionPhase::Intel);
            assert!(runner.machine().checkpoint_status(CheckpointId::Cp1).is_none());
            assert!(runner.machine().gate_to_advance().is_err());
            assert!(runner.history.iter().any(|x|format!("{x:?}").contains("用户")));
        }
    }

    /// 用户否决 ⇒ 会话**不得**继续推进，且要如实报告需要修订。
    #[test]
    fn user_rejection_stops_progress_and_reports_honestly() {
        let dir = tmp_dir("reject");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();
        let mut ex = ArtifactExecutor {
            // 声明**所有**阶段的产物：本测试关心的是推进逻辑，
            // 不是"产物从哪来"。而新检测恰好要求"阶段所需产物不得缺席"。
            kinds: vec![
                ArtifactKind::ParticipantProfiles,
                ArtifactKind::GameSpecValidated,
                ArtifactKind::EquilibriumSet,
                ArtifactKind::SelectionOutput,
                ArtifactKind::RecommendationPack,
                ArtifactKind::ReportBundle,
            ],
        };
        // 模型先调 checkpoint 工具（会被闸门放行），由循环挂起
        let l = AgentLoop::new(
            client(vec![
                (200, call_response("c1", "checkpoint", "{}")),
                (200, text_response("补充说明")),
            ]),
            &reg,
            &mut ex,
            default_loop_config("m", "c"),
        )
        .with_seq_clock();
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        m.advance().unwrap(); // → intel
        let mut r = SessionRunner::new(l, log, m);

        // 循环需要执行器把 checkpoint 请求交出来；此处用假执行器不产出它，
        // 故直接驱动"人工挂起"场景：手工打开 checkpoint 再让 runner 走到闸门。
        r.machine_mut()
            .open_checkpoint(CheckpointRequest {
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

        let mut io = ScriptedIo::new(vec![CheckpointAnswer::Revise {
            reason: "第三条事实来源不可靠".into(),
        }]);
        let report = r
            .run(vec![shangyi_llm::InputItem::user("x")], &mut io, "user")
            .unwrap();

        // 否决后必须停下，且**不得**声称完成
        assert!(!report.is_finished(), "用户否决后不得报告完成：{report:?}");
        assert_eq!(io.asked.borrow().as_slice(), &[CheckpointId::Cp1]);
        // 日志里必须留下否决的痕迹（可审计）
        let events = r.log().events().unwrap();
        assert!(
            events.iter().any(|e| matches!(
                e,
                Event::CheckpointAnswered { answers, .. }
                    if answers.iter().any(|a| a.contains("不可靠"))
            )),
            "用户否决必须留档：{events:?}"
        );
        // checkpoint 处于拒绝态 ⇒ 闸门必须仍拦着推进
        let cp = r
            .machine()
            .checkpoint_status(CheckpointId::Cp1)
            .cloned()
            .unwrap();
        assert!(!cp.is_approved());
        assert!(r.machine().gate_to_advance().is_err(), "被否决后不得推进");
    }

    /// 用户确认 ⇒ checkpoint 结算、事件 durable 落盘、可推进。
    #[test]
    fn user_approval_settles_and_allows_advance() {
        let dir = tmp_dir("approve");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();
        let mut ex = ArtifactExecutor {
            // 声明**所有**阶段的产物：本测试关心的是推进逻辑，
            // 不是"产物从哪来"。而新检测恰好要求"阶段所需产物不得缺席"。
            kinds: vec![
                ArtifactKind::ParticipantProfiles,
                ArtifactKind::GameSpecValidated,
                ArtifactKind::EquilibriumSet,
                ArtifactKind::SelectionOutput,
                ArtifactKind::RecommendationPack,
                ArtifactKind::ReportBundle,
            ],
        };
        // 每个 turn 需要一份响应（auto_advance 会连跑若干 turn，
        // 直到闸门因缺产物/未确认而拦下）。给足份数，避免夹具先于逻辑耗尽。
        // 说明：本测试用**纯文本**假响应（`text_response("ok")`）——
        // 它验的是"确认点结算后能推进"，不是"模型会不会调工具"。
        //
        // ⚠️ 而新加的检测（`text_only_model_fails_loudly...`）会在
        // 「阶段要求产物 + 执行器手里没有 + 模型没调工具」时判失败。
        // 故这里必须让执行器**预先声明阶段所需的产物**——
        // 它模拟的是"模型已经在别处产出过了"这个更简单的场景。
        //
        // 这也是那个检测的**正确性证据**：它只在产物确实缺失时触发。
        let l = AgentLoop::new(
            client(
                (0..8)
                    .map(|_| (200, text_response("ok")))
                    .collect::<Vec<_>>(),
            ),
            &reg,
            &mut ex,
            default_loop_config("m", "c"),
        )
        .with_seq_clock();
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        m.advance().unwrap(); // → intel
        m.record(ArtifactKind::ParticipantProfiles, "p");
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
        // auto_advance 会持续推进（这是正确行为：用户没喊停就不停）。
        // 故给一个 turn 预算，让测试在有限步内收敛。
        let mut r = SessionRunner::new(l, log, m).with_max_turns(3);

        let mut io = ScriptedIo::new(vec![CheckpointAnswer::Approve]);
        let report = r.run(vec![shangyi_llm::InputItem::user("x")], &mut io, "user").unwrap();

        // 核心断言：用户确认**真的**结算了，且阶段因此得以推进
        assert!(
            r.machine()
                .checkpoint_status(CheckpointId::Cp1)
                .is_some_and(|s| s.is_approved()),
            "确认后 checkpoint 应处于批准态"
        );
        assert_eq!(
            r.machine().phase(),
            SessionPhase::ModelBuild,
            "确认 intel 的事实后应推进到 model_build"
        );

        // 批准落档，且 user_actor 记录了**谁**批准的
        let events = r.log().events().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::CheckpointAnswered { user_actor, .. } if user_actor == "user"
        )));
        assert!(events.iter().any(|e| matches!(e, Event::PhaseApproved { .. })));
        assert!(events.iter().any(|e| matches!(e, Event::PhaseEntered { .. })));

        // 报错不应来自"用户否决"；配额耗尽或完成都可接受
        if let RunReport::Failed { detail, .. } = &report {
            panic!("确认路径不应失败：{detail}");
        }
        assert!(
            !matches!(report, RunReport::Aborted { .. }),
            "用户确认不是中止：{report:?}"
        );
    }

    // ── 诚实终止 ──

    /// 外部依赖不可用 ⇒ `Blocked`，**且与 `Failed` 可区分**。
    #[test]
    fn blocked_tool_yields_blocked_report_distinct_from_failed() {
        let dir = tmp_dir("blocked");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();

        struct BlockingExecutor;
        impl ToolExecutor for BlockingExecutor {
            fn execute(&mut self, _call: &ToolCall, _phase: SessionPhase) -> ExecOutcome {
                ExecOutcome::Blocked("检索后端不可用".into())
            }
        }
        let mut ex = BlockingExecutor;
        let l = AgentLoop::new(
            client(vec![(200, call_response("c1", "web_search", "{\"query\":\"x\"}"))]),
            &reg,
            &mut ex,
            default_loop_config("m", "c"),
        )
        .with_seq_clock();
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        m.advance().unwrap();
        let mut r = SessionRunner::new(l, log, m);

        let mut io = ScriptedIo::new(vec![]);
        let report = r.run(vec![shangyi_llm::InputItem::user("x")], &mut io, "user").unwrap();

        match &report {
            RunReport::Blocked { detail, .. } => assert!(detail.contains("检索后端")),
            other => panic!("外部阻塞必须报 Blocked，实得 {other:?}"),
        }
        assert!(!report.is_finished(), "阻塞不是完成");
        assert!(report.is_retryable(), "外部阻塞可重试");

        // 日志里的 turn 终止原因必须是 blocked（审计可查）
        assert!(r.log().events().unwrap().iter().any(|e| matches!(
            e,
            Event::TurnEnded {
                reason: TurnEndReason::Blocked { .. },
                ..
            }
        )));
    }

    #[test]
    fn llm_error_yields_failed_report() {
        let dir = tmp_dir("fail");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();
        let mut ex = OkExecutor;
        let l = AgentLoop::new(client(vec![(500, "boom".into())]), &reg, &mut ex, default_loop_config("m", "c"))
            .with_seq_clock();
        let mut r = SessionRunner::new(l, log, PhaseMachine::new(ConsultationMode::Scenario));

        let mut io = ScriptedIo::new(vec![]);
        let report = r.run(vec![shangyi_llm::InputItem::user("x")], &mut io, "user").unwrap();
        assert!(matches!(report, RunReport::Failed { .. }), "实得 {report:?}");
        assert!(!report.is_finished());
    }

    // ── 崩溃恢复 ──

    /// 上一进程崩溃遗留的 turn，在下次会话开始时被补写 `Interrupted`。
    #[test]
    fn crashed_log_is_repaired_at_session_start() {
        let dir = tmp_dir("repairrunner");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(Event::TurnStarted {
                turn: TurnId(1),
                at: "2026-09-17T00:00:00Z".into(),
            })
            .unwrap();
            // 崩溃：无 TurnEnded
        }
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        let reg = ToolRegistry::standard();
        let mut ex = OkExecutor;
        let l = AgentLoop::new(client(vec![(200, text_response("x"))]), &reg, &mut ex, default_loop_config("m", "c"))
            .with_seq_clock();
        let mut r = SessionRunner::new(l, log, PhaseMachine::new(ConsultationMode::Scenario));

        let mut io = ScriptedIo::new(vec![]);
        let n = r.repair_if_crashed(&mut io).unwrap();
        assert!(n > 0, "崩溃遗留的 turn 必须被补上收尾");

        // 补丁已落盘，且原因标为 interrupted
        let events = r.log().events().unwrap();
        assert!(events.iter().any(|e| matches!(
            e,
            Event::TurnEnded {
                reason: TurnEndReason::Interrupted,
                ..
            }
        )), "必须是 Interrupted，不得伪造成 Completed：{events:?}");

        // 幂等：再修复不产生新事件（否则每次开会话都会追加）
        let n2 = r.repair_if_crashed(&mut io).unwrap();
        assert_eq!(n2, 0);
    }

    // ── 进度自检 ──

    #[test]
    fn progress_reports_missing_artifacts_and_pending_checkpoint() {
        let mut m = PhaseMachine::new(ConsultationMode::Scenario);
        let p0 = progress(&m);
        assert_eq!(p0.phase, SessionPhase::Intake);
        assert!(p0.can_advance, "intake 无产物要求：{}", p0.reason);

        m.advance().unwrap(); // → intel
        let p1 = progress(&m);
        assert_eq!(p1.phase, SessionPhase::Intel);
        assert!(!p1.can_advance, "缺画像不得推进");
        assert_eq!(p1.missing_artifacts, vec![ArtifactKind::ParticipantProfiles]);
        assert_eq!(p1.pending_checkpoint, Some(CheckpointId::Cp1));
        assert_eq!(p1.checkpoint_state, "未打开");
        assert!(p1.reason.contains("participant_profiles"), "原因要可操作：{}", p1.reason);
    }

    /// 阶段图在三种模式下都能被进度查询正确解释（防 R20 类塌缩回潮）。
    #[test]
    fn progress_is_mode_aware() {
        for mode in [
            ConsultationMode::Scenario,
            ConsultationMode::Whatif,
            ConsultationMode::Discover,
        ] {
            let g = PhaseGraph::for_mode(mode);
            let m = PhaseMachine::new(mode);
            let p = progress(&m);
            assert_eq!(p.phase, SessionPhase::Intake);
            assert_eq!(m.graph().path(), g.path(), "阶段机的图应与该模式的图一致");
        }
    }

    #[test]
    fn abort_report_is_not_a_completion() {
        let r = abort_report(SessionPhase::Intel, 3);
        assert!(!r.is_finished(), "用户中止不是完成");
        assert!(!r.is_retryable(), "用户中止不得被自动重试");
        assert_eq!(r.turns(), 3);
        assert_eq!(abort_cause_user(), CancelCause::User);
    }
}

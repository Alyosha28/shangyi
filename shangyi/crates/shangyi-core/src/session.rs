//! 会话事件（设计文档 §7.1、§7.2、§7.7）。
//!
//! # 为什么是事件而不是可变状态
//!
//! 设计文档 §8.1 要求事件溯源：**权威是日志，状态是投影**。本模块只定义
//! 事件词汇与一个纯函数（[`interrupted_turn_closers`]），不持有任何状态——
//! 这样"崩溃恢复"就是一个可以在无 IO 下穷举测试的纯函数。
//!
//! # `Blocked` 与 `Interrupted` 为什么是承重的（§7.1）
//!
//! 一次**因数据源不可用而终止**的尽调，必须与一次**正常得出结论**的尽调
//! **永久可区分**。如果崩溃恢复时伪造一个干净的 `Completed`，报告读者会以为
//! 这个案例是完整分析——那是会导致审计失败的**正确性 bug**，不是体验瑕疵。
//! 故 [`TurnEndReason`] 的六个变体刻意不合并。

use serde::{Deserialize, Serialize};

// ─────────────────────────── 标识 ───────────────────────────

/// turn 标识（§7.1）。turn = 0..n 个 step。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TurnId(pub u64);

/// step 标识：一次模型调用 + 它请求的工具执行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StepId {
    pub turn: TurnId,
    pub step: u64,
}

impl StepId {
    pub fn new(turn: TurnId, step: u64) -> Self {
        Self { turn, step }
    }
}

// ─────────────────────────── 终止原因 ───────────────────────────

/// turn 的终止原因（§7.1）。
///
/// `Interrupted` **从不实时发出**——崩溃遗留的 turn 由
/// [`interrupted_turn_closers`] 事后合成。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TurnEndReason {
    Completed,
    Aborted { reason: CancelCause },
    /// 数据源不可用等**外部**阻塞；与 `completed` 必须永久可区分。
    Blocked { detail: String },
    Error { detail: String },
    MaxTokens,
    /// 仅由 resume repair 为崩溃遗留 turn 写入。
    Interrupted,
}

impl TurnEndReason {
    /// 该终止原因是否表示"turn 正常走完"。
    ///
    /// 用途单一但关键：只有它为真时，报告才可以把本 turn 的产物当作
    /// "完整分析得出的"。其余一律不是——包括 `MaxTokens`（上下文超限
    /// 意味着分析**没做完**，只是没报错而已）。
    pub fn is_clean_completion(&self) -> bool {
        matches!(self, TurnEndReason::Completed)
    }

    /// 该终止原因是否允许**自动重试**。
    ///
    /// `Blocked` 可以（数据源可能恢复）；`Aborted` 与 `Interrupted` 不可以
    /// （用户的意图与崩溃的事实不会因重试而改变）。
    pub fn is_retryable(&self) -> bool {
        matches!(self, TurnEndReason::Blocked { .. } | TurnEndReason::Error { .. })
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            TurnEndReason::Completed => "completed",
            TurnEndReason::Aborted { .. } => "aborted",
            TurnEndReason::Blocked { .. } => "blocked",
            TurnEndReason::Error { .. } => "error",
            TurnEndReason::MaxTokens => "max-tokens",
            TurnEndReason::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CancelCause {
    User,
    Parent,
    Hook { reason: String },
    Disposed,
    Legacy,
}

// ─────────────────────────── 工具调用状态 ───────────────────────────

/// 工具调用的结局。
///
/// # 四个变体，四件不同的事（不可合并）
///
/// | 变体 | 含义 | 报告里怎么读 |
/// |---|---|---|
/// | `Ok` | 成功，有结果 | 有结果 |
/// | `Failed` | 工具执行失败（参数错、实现未完成） | **出错了**，可纠正 |
/// | `Blocked` | 外部依赖不可用，**未能评估** | **未取得**（不是"没有"） |
/// | `Unknown` | 崩溃点的结局不明 | 可能有副作用，**不得盲目重放** |
///
/// # 为什么这次拆分是必要的（实测发现）
///
/// 早先的实现把 `Failed` 记为 `Ok { output: "FAILED: ..." }`、把 `Blocked`
/// 记为 `Unknown`。一次真实运行的事件日志因此长这样：
///
/// ```json
/// {"status":"ok","output":"FAILED: 工具 `vn_map` 尚未接线…"}
/// {"status":"unknown"}
/// ```
///
/// 两处都错：**失败被记成成功**（"ok" 这个字会进任何只看状态字段的统计），
/// **外部阻塞被记成结局不明**（审计会以为可能已经产生了副作用，从而不敢重试
/// 一个其实什么都没做的调用）。逐字段核对日志才能发现——这正是"日志即真相"
/// 要求我们把结局按语义分开的原因。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ToolOutcome {
    Ok {
        output: String,
    },
    /// 工具执行失败。**不是** `Ok`——失败不得被记成成功。
    Failed {
        error: String,
    },
    InsufficientEvidence {
        gaps: Vec<crate::conditional::EvidenceGap>,
    },
    /// 外部依赖不可用 ⇒ 该项**未取得**。与 `Failed` 的区别是语义性的：
    /// "查不到"不是"出错"，报告里两句话完全不同（不变量②）。
    Blocked {
        detail: String,
    },
    /// 工具**开始了但结局未知**——崩溃恰在结果落盘前发生。
    ///
    /// 与 `Failed` 分开是因为二者的**副作用风险不同**：未知结局的工具
    /// （尤其非幂等的）不得被盲目重放。
    Unknown,
}

impl ToolOutcome {
    /// 该结局是否表示"调用成功"。
    pub fn is_ok(&self) -> bool {
        matches!(self, ToolOutcome::Ok { .. })
    }
    /// 该结局是否允许**自动重放**该工具调用。
    ///
    /// 只有"确定没产生副作用"的两种（`Failed` 与 `Blocked`，前者多为参数错误、
    /// 后者根本没执行）才可重放。`Unknown` 一律**不可**——那正是它存在的理由。
    pub fn is_safe_to_replay(&self) -> bool {
        matches!(self, ToolOutcome::Failed { .. } | ToolOutcome::Blocked { .. } | ToolOutcome::InsufficientEvidence { .. })
    }
}

/// 工具调用未开始的标记文本（repair 合成用）。
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";
/// 工具结局未知的标记文本（repair 合成用）。
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

// ─────────────────────────── 事件 ───────────────────────────

/// 会话事件（§7.2 的领域事件层）。
///
/// 只列商奕真正会写的事件；基座事件（`codex_protocol::EventMsg`）**不在此**
/// 重复定义——§5.4 规则 4 要求不新增基座协议变体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    TaskScopeSelected {
        scope: crate::task_scope::TaskScope,
        reason: String,
        at: String,
    },
    ScopedTaskCompleted {
        result: crate::task_scope::ScopedTaskResult,
        at: String,
    },
    FollowupTaskStarted { at: String },
    // ── turn / step 骨架 ──
    TurnStarted {
        turn: TurnId,
        at: String,
    },
    TurnEnded {
        turn: TurnId,
        reason: TurnEndReason,
        at: String,
    },
    StepStarted {
        step: StepId,
        at: String,
    },
    StepEnded {
        step: StepId,
        at: String,
    },

    // ── 用量（成本可观测性的落点）──
    /// 一次模型调用的 token 用量。
    ///
    /// # 为什么它必须进事件日志
    ///
    /// 在加这条事件之前，商奕**完全不知道自己花了多少 token**——
    /// 于是"哪个模型贵、该换哪个"只能靠外部文档猜，而 GLM 的定价页是
    /// JS 空壳（抓不到数字），连猜的输入都没有。
    ///
    /// 这条事件把成本从"外部文档"变成"我们自己的实测数据"：
    /// 一次真实会话跑完，日志里就有每一步的 token 明细。
    ///
    /// # 为什么缓存与思考 token 要分开记
    ///
    /// 见 `shangyi_llm::Usage`：缓存命中与未命中的输入价差 **50 倍**，
    /// 而思考 token 计入输出价（实测一例占输出 97%）。
    /// 不分开记，算出来的成本会错一个数量级。
    TokenUsage {
        step: StepId,
        /// 模型名（同一次会话可能换模型，故逐步记录）。
        model: String,
        input_tokens: u64,
        output_tokens: u64,
        /// 输入里命中缓存的部分。
        cached_input_tokens: u64,
        /// 输出里属于思考的部分。
        reasoning_tokens: u64,
        at: String,
    },

    /// 警告：某事值得留档，但**不终止流程**。
    ///
    /// 与 `Blocked` 的区别：`Blocked` 是"无法继续"（终止），
    /// `Warning` 是"有缺口，但其它路径可用"（继续）。二者混同会让
    /// "部分数据源不可用"看起来像"整个流程失败"。
    Warning { message: String, at: String },

    // ── 工具调用 ──
    ToolCallStarted {
        step: StepId,
        call_id: String,
        name: String,
        /// 参数原文（JSON 字符串）。留档使"模型当时要什么"可被审计。
        arguments: String,
        at: String,
    },
    ToolCallFinished {
        call_id: String,
        outcome: ToolOutcome,
        at: String,
    },

    // ── 阶段（§7.7：阶段转移是事件，不是可变状态）──
    PhaseEntered {
        from: String,
        to: String,
        playbook: String,
        reason: String,
        at: String,
    },
    /// `fingerprint` = 该阶段产物摘要，用于检测"批准后产物被改"。
    PhaseApproved {
        phase: String,
        fingerprint: String,
        at: String,
    },
    PhaseRewound {
        from: String,
        to: String,
        due_to: String,
        at: String,
    },
    PlaybookSelected {
        id: String,
        source: String,
        at: String,
    },

    // ── checkpoint（§7.6 的生命周期）──
    CheckpointOpened {
        id: String,
        phase: String,
        items: Vec<String>,
        at: String,
    },
    CheckpointAnswered {
        id: String,
        answers: Vec<String>,
        user_actor: String,
        at: String,
    },
    /// 真实用户输入边界登记（供约束底线、收益证据与会话式检查点裁决审计）。
    UserInputRecorded {
        text: String,
        user_actor: String,
        at: String,
    },
}

impl Event {
    /// 事件时间戳（用于 repair 复用"最后一个真实事件"的时间）。
    pub fn at(&self) -> &str {
        match self {
            Event::TaskScopeSelected { at, .. }
            | Event::ScopedTaskCompleted { at, .. }
            | Event::FollowupTaskStarted { at, .. }
            | Event::TurnStarted { at, .. }
            | Event::TurnEnded { at, .. }
            | Event::StepStarted { at, .. }
            | Event::StepEnded { at, .. }
            | Event::ToolCallStarted { at, .. }
            | Event::ToolCallFinished { at, .. }
            | Event::TokenUsage { at, .. }
            | Event::Warning { at, .. }
            | Event::PhaseEntered { at, .. }
            | Event::PhaseApproved { at, .. }
            | Event::PhaseRewound { at, .. }
            | Event::PlaybookSelected { at, .. }
            | Event::CheckpointOpened { at, .. }
            | Event::CheckpointAnswered { at, .. }
            | Event::UserInputRecorded { at, .. } => at,
        }
    }
}

// ─────────────────────────── 崩溃修复（纯函数）───────────────────────────

/// 为**崩溃遗留**的 turn 合成收尾事件（§7.1，逐字移植 dsh 的 repair 语义）。
///
/// # 三步
///
/// 1. 给**未匹配**的工具调用补错误结果：有 `ToolCallStarted` 而无对应
///    `ToolCallFinished` 的，补 `Unknown`（结局未知——不是"没开始"：
///    进程崩溃时它可能已经产生了副作用）。
/// 2. 补未闭合的 `StepEnded`。
/// 3. 补 `Interrupted` 的 `TurnEnded`，时间戳**复用最后一个真实事件**的时间。
///
/// # 为什么是纯函数且必须确定性
///
/// 它在崩溃恢复路径上运行，此时**没有**可靠的时钟与网络。若它的输出依赖
/// 当前时间或环境，那么同一份日志在两次恢复中会得到不同的补丁，事件溯源
/// 的"日志即真相"就破了。**确定性就是它的全部价值**，故时间戳取自日志本身。
///
/// # 幂等
///
/// 对已经修复过的日志再跑一次，不产生新事件（已有 `TurnEnded` 即不补）。
pub fn interrupted_turn_closers(events: &[Event]) -> Vec<Event> {
    // 最后一个真实事件的时间；空日志则用固定纪元（确定性优先于"看起来合理"）
    let last_at = events
        .last()
        .map(|e| e.at().to_string())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string());

    // 已结算的工具调用
    let mut finished: std::collections::BTreeSet<&str> = Default::default();
    for e in events {
        if let Event::ToolCallFinished { call_id, .. } = e {
            finished.insert(call_id.as_str());
        }
    }

    let mut out = Vec::new();

    // 1) 未匹配的工具调用 → 结局未知
    //
    // 为什么是 `Unknown` 而不是 `NotStarted`：崩溃点**可能在副作用之后**。
    // 标成"未开始"会诱导恢复逻辑重放一个已经执行过的非幂等工具（重复扣款那类错误）。
    for e in events {
        if let Event::ToolCallStarted { call_id, .. } = e {
            if !finished.contains(call_id.as_str()) {
                out.push(Event::ToolCallFinished {
                    call_id: call_id.clone(),
                    outcome: ToolOutcome::Unknown,
                    at: last_at.clone(),
                });
            }
        }
    }

    // 2) 未闭合的 step
    let mut open_steps: Vec<StepId> = Vec::new();
    for e in events {
        match e {
            Event::StepStarted { step, .. } => {
                if !open_steps.contains(step) {
                    open_steps.push(*step);
                }
            }
            Event::StepEnded { step, .. } => open_steps.retain(|s| s != step),
            _ => {}
        }
    }
    for step in &open_steps {
        out.push(Event::StepEnded {
            step: *step,
            at: last_at.clone(),
        });
    }

    // 3) 未闭合的 turn → Interrupted
    //
    // 只处理**最后一个**未闭合 turn：更早的未闭合 turn 若存在，说明日志本身
    // 有洞，那不是本函数该猜的事（补错比不补更坏）。
    let mut open_turn: Option<TurnId> = None;
    let mut closed: std::collections::BTreeSet<TurnId> = Default::default();
    for e in events {
        match e {
            Event::TurnStarted { turn, .. } => open_turn = Some(*turn),
            Event::TurnEnded { turn, .. } => {
                closed.insert(*turn);
            }
            _ => {}
        }
    }
    if let Some(t) = open_turn {
        if !closed.contains(&t) {
            out.push(Event::TurnEnded {
                turn: t,
                reason: TurnEndReason::Interrupted,
                at: last_at,
            });
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(n: u32) -> String {
        format!("2026-01-01T00:00:{n:02}Z")
    }

    fn started(turn: u64, at: &str) -> Event {
        Event::TurnStarted {
            turn: TurnId(turn),
            at: at.to_string(),
        }
    }

    fn step_started(turn: u64, step: u64, at: &str) -> Event {
        Event::StepStarted {
            step: StepId::new(TurnId(turn), step),
            at: at.to_string(),
        }
    }

    fn tool_started(turn: u64, step: u64, call_id: &str, at: &str) -> Event {
        Event::ToolCallStarted {
            step: StepId::new(TurnId(turn), step),
            call_id: call_id.to_string(),
            name: "solve_equilibria".to_string(),
            arguments: "{}".to_string(),
            at: at.to_string(),
        }
    }

    // ── 空与幂等 ──

    #[test]
    fn empty_log_produces_nothing() {
        assert!(interrupted_turn_closers(&[]).is_empty());
    }

    #[test]
    fn clean_log_is_left_alone() {
        let log = vec![
            started(1, &ts(0)),
            step_started(1, 0, &ts(1)),
            Event::StepEnded {
                step: StepId::new(TurnId(1), 0),
                at: ts(2),
            },
            Event::TurnEnded {
                turn: TurnId(1),
                reason: TurnEndReason::Completed,
                at: ts(3),
            },
        ];
        assert!(
            interrupted_turn_closers(&log).is_empty(),
            "正常结束的日志不得被修补——否则每次 resume 都会污染干净会话"
        );
    }

    #[test]
    fn repair_is_idempotent() {
        let log = vec![started(1, &ts(0)), step_started(1, 0, &ts(1))];
        let once = interrupted_turn_closers(&log);
        assert!(!once.is_empty());

        let mut applied = log.clone();
        applied.extend(once.clone());
        assert!(
            interrupted_turn_closers(&applied).is_empty(),
            "修复后再修复不得产生新事件（否则 resume 会无限追加）"
        );
    }

    // ── 三步各自的行为 ──

    /// 崩溃点的工具调用必须标为**结局未知**而非"未开始"。
    #[test]
    fn unmatched_tool_call_is_unknown_not_not_started() {
        let log = vec![
            started(1, &ts(0)),
            step_started(1, 0, &ts(1)),
            tool_started(1, 0, "call_1", &ts(2)),
        ];
        let closers = interrupted_turn_closers(&log);

        let fixed: Vec<&Event> = closers
            .iter()
            .filter(|e| matches!(e, Event::ToolCallFinished { .. }))
            .collect();
        assert_eq!(fixed.len(), 1, "恰好一个工具调用需要补结局");
        match fixed[0] {
            Event::ToolCallFinished { call_id, outcome, .. } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(
                    *outcome,
                    ToolOutcome::Unknown,
                    "崩溃点的工具必须标为结局未知——标成未开始会诱导重放真实副作用"
                );
            }
            _ => unreachable!(),
        }
    }

    /// 已有结局的工具调用不得被重复补。
    #[test]
    fn matched_tool_call_is_not_touched() {
        let log = vec![
            started(1, &ts(0)),
            tool_started(1, 0, "call_1", &ts(1)),
            Event::ToolCallFinished {
                call_id: "call_1".into(),
                outcome: ToolOutcome::Ok {
                    output: "[]".into(),
                },
                at: ts(2),
            },
        ];
        let closers = interrupted_turn_closers(&log);
        assert!(
            !closers
                .iter()
                .any(|e| matches!(e, Event::ToolCallFinished { .. })),
            "已有结局者不得被覆盖——那会丢失真实结果"
        );
    }

    #[test]
    fn open_step_and_turn_are_closed_with_interrupted() {
        let log = vec![started(1, &ts(0)), step_started(1, 0, &ts(5))];
        let closers = interrupted_turn_closers(&log);
        assert!(closers.iter().any(|e| matches!(e, Event::StepEnded { .. })));
        match closers.last().unwrap() {
            Event::TurnEnded { reason, .. } => {
                assert_eq!(
                    *reason,
                    TurnEndReason::Interrupted,
                    "崩溃遗留 turn 必须标为 Interrupted，不得伪造 Completed"
                );
            }
            other => panic!("最后一条应为 TurnEnded，实得 {other:?}"),
        }
    }

    /// **本模块最重要的断言**：修复出的 `Interrupted` 不得是"干净完成"。
    /// 这条把"崩溃恢复伪造 Completed"这个正确性 bug 钉死在类型语义上。
    #[test]
    fn interrupted_is_never_a_clean_completion() {
        let log = vec![started(1, &ts(0))];
        let closers = interrupted_turn_closers(&log);
        for e in &closers {
            if let Event::TurnEnded { reason, .. } = e {
                assert!(!reason.is_clean_completion());
                assert!(!reason.is_retryable(), "崩溃不得被当作可自动重试的外部阻塞");
            }
        }
    }

    /// 时间戳复用最后一个真实事件——**不引入当前时间**（确定性）。
    #[test]
    fn closers_reuse_last_real_timestamp() {
        let log = vec![started(1, &ts(0)), step_started(1, 0, &ts(42))];
        let closers = interrupted_turn_closers(&log);
        assert!(!closers.is_empty());
        for e in &closers {
            assert_eq!(e.at(), ts(42), "补丁时间戳必须取自日志，不得取当前时钟");
        }
        // 同一输入两次调用结果完全相同
        assert_eq!(closers, interrupted_turn_closers(&log));
    }

    /// 已闭合的 turn 不受影响（多 turn 日志）。
    #[test]
    fn closed_earlier_turns_are_untouched() {
        let log = vec![
            started(1, &ts(0)),
            Event::TurnEnded {
                turn: TurnId(1),
                reason: TurnEndReason::Completed,
                at: ts(1),
            },
            started(2, &ts(2)),
        ];
        let closers = interrupted_turn_closers(&log);
        let turn_ends: Vec<&Event> = closers
            .iter()
            .filter(|e| matches!(e, Event::TurnEnded { .. }))
            .collect();
        assert_eq!(turn_ends.len(), 1);
        match turn_ends[0] {
            Event::TurnEnded { turn, reason, .. } => {
                assert_eq!(*turn, TurnId(2), "只补最后一个未闭合的 turn");
                assert_eq!(*reason, TurnEndReason::Interrupted);
            }
            _ => unreachable!(),
        }
    }

    // ── 终止原因的语义 ──

    #[test]
    fn only_completed_is_a_clean_completion() {
        assert!(TurnEndReason::Completed.is_clean_completion());
        for r in [
            TurnEndReason::Aborted { reason: CancelCause::User },
            TurnEndReason::Blocked { detail: "数据源不可用".into() },
            TurnEndReason::Error { detail: "x".into() },
            TurnEndReason::MaxTokens,
            TurnEndReason::Interrupted,
        ] {
            assert!(!r.is_clean_completion(), "{r:?} 不得被视为干净完成");
        }
    }

    /// `Blocked` 与 `Error` 的**可区分性**：审计要能回答"这份尽调是因为上游
    /// 数据源挂了而中断，还是因为程序出错"。
    #[test]
    fn blocked_and_error_are_distinguishable() {
        let b = TurnEndReason::Blocked { detail: "web_search 三后端全失败".into() };
        let e = TurnEndReason::Error { detail: "parse".into() };
        assert_ne!(b.kind_str(), e.kind_str());
        assert!(b.is_retryable() && e.is_retryable());
        let aborted = TurnEndReason::Aborted { reason: CancelCause::User };
        assert!(!aborted.is_retryable(), "用户主动中止不得被自动重试");
    }

    /// **回归测试**：工具失败不得被记成 `ok`，外部阻塞不得被记成 `unknown`。
    ///
    /// 早先的实现把两者分别记成 `Ok { output: "FAILED: …" }` 与 `Unknown`。
    /// 一次真实运行的事件日志因此出现 `{"status":"ok","output":"FAILED: …"}`
    /// 与 `{"status":"unknown"}`——前者让"只看状态字段"的统计把失败算成成功，
    /// 后者让审计误以为可能已产生副作用，从而不敢重试一个其实没执行的调用。
    #[test]
    fn tool_outcome_states_are_not_conflated() {
        let failed = ToolOutcome::Failed { error: "未接线".into() };
        let blocked = ToolOutcome::Blocked { detail: "后端不可用".into() };
        let unknown = ToolOutcome::Unknown;

        assert!(!failed.is_ok(), "失败不得被当作成功");
        assert!(!blocked.is_ok());
        assert!(!unknown.is_ok());
        assert!(ToolOutcome::Ok { output: "x".into() }.is_ok());

        // 可重放性：只有"确定无副作用"的才可自动重放
        assert!(failed.is_safe_to_replay(), "参数错误可直接重试");
        assert!(blocked.is_safe_to_replay(), "未执行，可重试");
        assert!(!unknown.is_safe_to_replay(), "结局未知⇒可能已有副作用⇒不得盲目重放");

        // 序列化后状态字面量互不相同（日志读者靠它区分）
        let tags: Vec<String> = [&failed, &blocked, &unknown]
            .iter()
            .map(|o| {
                let v: serde_json::Value =
                    serde_json::from_str(&serde_json::to_string(o).unwrap()).unwrap();
                v["status"].as_str().unwrap_or("?").to_string()
            })
            .collect();
        assert_eq!(tags, vec!["failed", "blocked", "unknown"]);
        assert_eq!(
            tags.iter().collect::<std::collections::BTreeSet<_>>().len(),
            3,
            "三种结局的状态字面量必须互不相同"
        );
    }

    #[test]
    fn event_round_trips_through_json() {
        let events = vec![
            started(3, &ts(1)),
            tool_started(3, 0, "c1", &ts(2)),
            Event::CheckpointOpened {
                id: "cp2".into(),
                phase: "model_build".into(),
                items: vec!["payoff_A".into()],
                at: ts(3),
            },
            Event::TurnEnded {
                turn: TurnId(3),
                reason: TurnEndReason::Interrupted,
                at: ts(4),
            },
        ];
        for e in &events {
            let s = serde_json::to_string(e).unwrap();
            let back: Event = serde_json::from_str(&s).unwrap();
            assert_eq!(&back, e, "事件必须 JSON 往返无损（日志格式即契约）");
        }
    }
}

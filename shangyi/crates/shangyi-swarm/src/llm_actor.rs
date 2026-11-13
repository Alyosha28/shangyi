//! 由真实 LLM 驱动的 persona——§5.7「动态编排 persona 子会话」的落点。
//!
//! # 与 crate 模块头的关系（如实标注一处差异）
//!
//! [`crate`] 的模块头写着「本 crate **不调用 LLM**：persona 的行动由外部产生，
//! 经 trait 注入」。本模块是那条约束的**受控例外**：它把「外部」做成了本 crate
//! 内的一个实现，但进入编排器的路径没变——仍只经 [`PersonaActor`]。
//! 铁律 1 因此未被绕过：本模块的全部产出是 [`Move`]（提议），
//! 没有任何一条通往 [`crate::CandidateResult`] 的路径（后者只能由
//! [`crate::orchestrator::evaluate_candidates`] 构造）。
//!
//! # 铁律 1 的两条具体落点
//!
//! 1. **提示词不给 persona 任何支付参数**：`GameSpec::params` 与 `GameSpec::payoffs`
//!    都不进提示词。递数字等于递计算材料——模型拿到就会去算，而它算出来的数
//!    不该出现在报告里。persona 只需要知道「自己是谁、有哪些可选行动、已经发生了什么」。
//! 2. `expected_benefit` / `expected_cost` 是**提议的参数**，不是结论：
//!    它们随 [`Action::Novel`] 进入 `shangyi-consequence` 的三重过滤，
//!    由引擎自己算 `P(收益 > 成本)` 再判定是否接受。persona 说「这招划算」不算数。
//!
//! # 失败语义：返回 `None` 并留痕，**绝不伪造默认行动**
//!
//! 以下情形一律**拒绝**（`act` 返回 `None`；`propose` 返回类型化错误）：
//!
//! | 情形 | 拒绝理由 |
//! |---|---|
//! | persona 未在 [`PersonaSet`] 注册 | 没有角色卡就调用，等于退化成「乐于助人的通用助手」——正是 §5.7 明令禁止的静默降级 |
//! | LLM 调用失败（传输 / 4xx / 5xx） | 没有提议就是没有提议 |
//! | 响应未完成（`status != completed`，如触顶截断） | 截断的输出连字段都可能缺一半，不得当完整提议用 |
//! | 输出里找不到 JSON 对象 | **不把自由文本当提议**：无法区分「行动」与「评论」，硬认会把评论变成行动 |
//! | `kind` 声明非常规、但名字命中矩阵内策略 | 语义自相矛盾，拒绝猜模型指哪一种 |
//! | 区间倒置（下界 > 上界） | 不静默交换端点——那是替模型改数 |
//!
//! 与之相对，「名字不在策略空间内」**不是**失败：那是 §5.7.1 要的矩阵外提议，
//! 转成 [`Action::Novel`] 交给三重过滤（过滤不过就是引擎如实拒绝，进报告的
//! 「考虑过但排除了哪些路径」）。
//!
//! # 一处本模块**修不了**的坑（留给接线方）
//!
//! [`PersonaActor::act`] 的返回类型是 `Option<Move>`，因此「模型挂了」与
//! 「该 persona 本就无行动可提议」在 `rollout` 眼里**完全一样**：两者都让该回合
//! 少一个 move，最终都表现为 `StopReason::Stable`。即：一次 endpoint 故障会以
//! 「博弈已稳定」的面目出现在报告里。本模块能做的只是把次数与原因记进
//! [`ActorTrace`]，**接线方（`swarm_run`）必须读它**，否则这仍是一条静默降级路径。
//!
//! # 可复现性：提示词可复现，**模型输出不可复现**（不假装）
//!
//! - **确定的部分**：提示词是 `(persona, spec, history, perturbation)` 的纯函数
//!   （已由测试锁定：同输入 ⇒ 逐字节相同的请求体）。`GameSpec` 用 `BTreeMap`，
//!   顺序稳定；提示词里没有任何时间戳、随机数或哈希表迭代序。
//! - **不确定的部分**：采样温度虽设为 0.0 以压低方差，但端点不提供 `seed` 参数，
//!   供应商侧的 MoE 路由、批处理与模型版本更新都可能改变输出。
//!   **故同一 `(spec, history, perturbation)` 不保证得到同一个提议。**
//! - 结论：`rollout` 的落点频率是**采样证据**，不是可重放的确定性产物。
//!   §5.7 的「种子 → 同一分布」在 LLM 参与时只能理解为「同一批 persona 参数」，
//!   不能理解成「同一批输出」。报告若引用 persona 覆盖率，须按此口径标注。

use std::sync::Mutex;

use serde_json::{Map, Value};
use shangyi_gamespec::GameSpec;
use shangyi_llm::{InputItem, LlmClient, LlmRequest, LlmResponse, ToolSchema, Transport};

use crate::orchestrator::PersonaActor;
use crate::{Action, Move, Persona, PersonaSet};

/// 提交提议的工具名（模型走 function call 时用）。
pub const PROPOSE_TOOL_NAME: &str = "propose_move";

/// 提示词里最多展示多少条历史。
///
/// 设上限的理由是成本与注意力：每轮都要重发全部上下文，而 rollout 可能有几十轮。
/// 截断**如实标注**（提示词里写明「仅列最近 N 条」），不假装那就是全部历史。
const MAX_HISTORY_SHOWN: usize = 12;

/// 单条历史理由的展示上限（字符数）——理由进提示词只为给下一步提供语境，
/// 不需要全文；避免某条超长理由把上下文预算吃光。
const MAX_RATIONALE_SHOWN: usize = 80;

/// 拒绝原因（类型化）。
///
/// `PersonaActor::act` 只能给 `Option`，故这些变体主要供 `propose`（直接调用路径）
/// 与 [`ActorTrace::last_failure`] 使用。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmActorError {
    #[error("persona `{0}` 未在 PersonaSet 中注册——不得退化为无角色卡的通用助手")]
    UnknownPersona(String),
    #[error("LLM 调用失败：{0}")]
    Call(String),
    #[error("响应不可用：{0}")]
    Unusable(String),
    #[error("输出无法解析为行动提议：{0}")]
    Unparseable(String),
}

/// 一条提议的解析结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProposal {
    pub action: Action,
    /// 模型给出的理由。`None` = 模型没给，由调用方决定如何标注
    /// （**不是**替它编一个理由）。
    pub rationale: Option<String>,
    /// 该非常规提议是否**缺收益/成本区间**。
    ///
    /// 用途：缺区间时 [`Action::Novel`] 只能填占位 `(0.0, 0.0)`（见
    /// [`parse_proposal`] 的说明），这类提议会被激励相容判据拒绝。
    /// 区分「模型没给区间」与「模型给了零区间」靠这个标记——
    /// 否则诊断时两种情形长得一模一样。
    pub missing_quantification: bool,
}

impl ParsedProposal {
    /// 转成 [`Move`]。actor 由调用方给出（= 本次提议的 participant_id），
    /// 不取自模型输出——模型说自己是别人也不算数。
    pub fn into_move(self, actor: &str) -> Move {
        let rationale = self.rationale.unwrap_or_else(|| {
            // 模型没给理由时的**如实标注**：这不是伪造理由，而是把缺失写在脸上。
            // 理由字段进报告，写下「模型未提供」比留空或编一句都诚实。
            "（模型未提供理由）".to_string()
        });
        Move::new(actor, self.action, rationale)
    }
}

/// persona 提议的运行时留痕。
///
/// # 为什么必须有（而不是「诊断加着玩」）
///
/// 见模块头「一处本模块修不了的坑」：`act` 的失败与「本该无行动」在编排器层面
/// 不可区分，都会走向 `StopReason::Stable`。没有这份留痕，接线方就没办法知道
/// 报告里的「稳定」是真的还是 endpoint 挂了。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ActorTrace {
    /// `act` 被调用次数。
    pub attempts: u32,
    /// 成功产出提议的次数。
    pub proposed: u32,
    /// 拒绝次数（= attempts − proposed）。
    pub rejected: u32,
    /// 非常规提议中缺收益/成本区间的条数（这些会被引擎的激励相容判据拒绝）。
    pub novels_missing_ranges: u32,
    /// 最近一次拒绝的原因。
    pub last_failure: Option<String>,
}

impl ActorTrace {
    /// 本次 rollout 里**是否发生过"该有提议却没有"的故障**。
    ///
    /// # 为什么必须在接线层检查它（这是本 crate 最容易漏的一处）
    ///
    /// [`PersonaActor::act`] 的签名是 `-> Option<Move>`，于是两种截然不同的
    /// 情形在编排器眼里**完全一样**：
    ///
    /// | 实际发生的事 | `act` 返回 | 编排器看到 |
    /// |---|---|---|
    /// | 该 persona 在这一轮确实没想法 | `None` | 空轮 ⇒ `StopReason::Stable` |
    /// | **LLM 端点挂了 / 响应被截断 / 解析失败** | `None` | 空轮 ⇒ `StopReason::Stable` |
    ///
    /// 后者会让一次**故障**以「博弈已稳定」的面目写进报告——这是静默降级，
    /// 而它比"报错"危险得多：报告读者不会知道那些"稳定"的结论其实来自
    /// 一台没回话的服务器。
    ///
    /// 故接线方**必须**在采纳"稳定"这个结论前调用本方法。
    /// `rollout` 本身不改（它不该知道 LLM 的存在），判别留在接线处。
    pub fn saw_failure(&self) -> bool {
        self.rejected > 0 || self.attempts > self.proposed
    }

    /// 全部尝试都失败了（一次提议都没产出）。
    ///
    /// 这是最强的一种信号：不是"偶尔有一轮没想法"，而是**从头到尾没通过**。
    /// 接线方遇到它必须返回错误，绝不能把 rollout 结果当证据用。
    pub fn total_failure(&self) -> bool {
        self.attempts > 0 && self.proposed == 0
    }
}

/// 一次 persona 提议的提示词（拆成两段，便于审查与测试断言）。
#[derive(Debug, Clone, PartialEq)]
pub struct PersonaPrompt {
    /// 系统段：角色卡 + 决策风格 + 扰动 + 铁律。
    pub system: String,
    /// 用户段：博弈结构 + 策略空间 + 历史 + 输出格式。
    pub user: String,
}

/// LLM 驱动的 [`PersonaActor`]。
///
/// 持有客户端**引用**（而非所有权）：`LlmClient` 不可克隆，而 CLI 只有一份
/// 客户端（凭据在里面），借用使 swarm 与主 agent 循环可以共用它。
pub struct LlmPersonaActor<'a, T: Transport> {
    client: &'a LlmClient<T>,
    personas: PersonaSet,
    model: String,
    temperature: f64,
    max_output_tokens: Option<u32>,
    trace: Mutex<ActorTrace>,
}

impl<'a, T: Transport> LlmPersonaActor<'a, T> {
    /// `model` 用端点 `/models` 实际返回的名字（如 `deepseek-flash`），
    /// 不是别名——A10b 实测记录过这一点。
    pub fn new(client: &'a LlmClient<T>, personas: PersonaSet, model: impl Into<String>) -> Self {
        Self {
            client,
            personas,
            model: model.into(),
            // 0.0：压低采样方差。它**不**保证可复现（端点无 seed 参数，
            // 供应商侧路由与版本都可能变），只是把随机性降到最低。
            // 需要别的取值时用 `with_temperature` 显式覆盖。
            temperature: 0.0,
            max_output_tokens: None,
            trace: Mutex::new(ActorTrace::default()),
        }
    }

    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = temperature;
        self
    }

    /// 输出上限。**默认不设**（用供应商默认值）。
    ///
    /// 设小了有真实代价：输出被截断时端点是 HTTP 200 + `status: incomplete`，
    /// 提议会以「无法解析」的面目失败——诊断方向容易跑偏（见 `shangyi-llm`
    /// 对 `incomplete_reason` 的说明）。故默认交给供应商，调小需显式。
    pub fn with_max_output_tokens(mut self, n: u32) -> Self {
        self.max_output_tokens = Some(n);
        self
    }

    pub fn personas(&self) -> &PersonaSet {
        &self.personas
    }

    /// 当前留痕的快照。
    pub fn trace(&self) -> ActorTrace {
        self.trace_guard().clone()
    }

    /// 构造提示词（纯函数，不发起调用）。
    ///
    /// 公开它有两个用途：接线方可以**离线审查**「persona 覆盖到底注入了什么」，
    /// 测试可以断言提示词逐字节可复现。
    pub fn build_prompt(
        &self,
        spec: &GameSpec,
        participant_id: &str,
        history: &[Move],
        perturbation: f64,
    ) -> Result<PersonaPrompt, LlmActorError> {
        let persona = self
            .personas
            .get(participant_id)
            .ok_or_else(|| LlmActorError::UnknownPersona(participant_id.to_string()))?;
        let strategies = strategies_for(spec, participant_id);
        Ok(compose_prompt(
            persona,
            spec,
            &strategies,
            history,
            perturbation,
        ))
    }

    /// 产出一个提议。**类型化错误路径**——`act` 是它的 `Option` 包装。
    ///
    /// 无论成功失败都记入 [`ActorTrace`]（直接调用本函数的接线方同样拿得到留痕）。
    pub fn propose(
        &self,
        spec: &GameSpec,
        participant_id: &str,
        history: &[Move],
        perturbation: f64,
    ) -> Result<Move, LlmActorError> {
        let outcome = self.propose_inner(spec, participant_id, history, perturbation);
        {
            let mut t = self.trace_guard();
            t.attempts += 1;
            match &outcome {
                Ok((_, missing_ranges)) => {
                    t.proposed += 1;
                    if *missing_ranges {
                        t.novels_missing_ranges += 1;
                    }
                }
                Err(e) => {
                    t.rejected += 1;
                    t.last_failure = Some(e.to_string());
                }
            }
        }
        outcome.map(|(m, _)| m)
    }

    fn propose_inner(
        &self,
        spec: &GameSpec,
        participant_id: &str,
        history: &[Move],
        perturbation: f64,
    ) -> Result<(Move, bool), LlmActorError> {
        let persona = self
            .personas
            .get(participant_id)
            .ok_or_else(|| LlmActorError::UnknownPersona(participant_id.to_string()))?;
        let strategies = strategies_for(spec, participant_id);
        let prompt = compose_prompt(persona, spec, &strategies, history, perturbation);

        let mut request = LlmRequest::new(
            self.model.clone(),
            vec![InputItem::system(prompt.system), InputItem::user(prompt.user)],
        )
        .with_tools(vec![propose_move_tool()])
        .with_temperature(self.temperature);
        if let Some(n) = self.max_output_tokens {
            request = request.with_max_tokens(n);
        }

        let resp = self
            .client
            .complete(&request)
            .map_err(|e| LlmActorError::Call(e.to_string()))?;

        let raw = proposal_source(&resp)?;
        let parsed = parse_proposal(&raw, &strategies).map_err(LlmActorError::Unparseable)?;
        let missing = parsed.missing_quantification;
        Ok((parsed.into_move(participant_id), missing))
    }

    /// 取留痕锁。
    ///
    /// 毒化（持锁时 panic）时取回内部值继续用：这是**诊断计数器**，
    /// 不是需要跨线程一致性的状态；让一份错误的锁把诊断信息永久锁死更糟。
    fn trace_guard(&self) -> std::sync::MutexGuard<'_, ActorTrace> {
        self.trace.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 一个**能如实报告自身故障**的 persona actor。
///
/// # 为什么需要这个 trait（而不是直接用 [`PersonaActor`]）
///
/// [`PersonaActor::act`] 返回 `Option<Move>`，这个签名**抹掉了**
/// 「该 persona 没想法」与「LLM 调用失败」的区别——两者都变成 `None`。
/// 对 `rollout` 来说那没问题（它不该知道 LLM 的存在），但对**接线方**
/// 是致命的：一次端点故障会以「博弈已稳定」的面目进报告。
///
/// 本 trait 把那条信息露出来，使接线方能做判别。判别动作留在接线方，
/// 因为**只有它知道该"当作故障"还是"当作正常空轮"**——例如规则驱动的
/// actor 永远不会有"故障"，它的 `None` 就是字面意思。
pub trait TracedPersonaActor: PersonaActor {
    /// 本 actor 自构造以来的累计统计。
    fn trace(&self) -> ActorTrace;
}

impl<T: Transport> TracedPersonaActor for LlmPersonaActor<'_, T> {
    fn trace(&self) -> ActorTrace {
        LlmPersonaActor::trace(self)
    }
}

impl<T: Transport> PersonaActor for LlmPersonaActor<'_, T> {
    fn act(
        &self,
        spec: &GameSpec,
        participant_id: &str,
        history: &[Move],
        perturbation: f64,
    ) -> Option<Move> {
        // 失败一律 None（并已记入 trace），**不**造一个默认行动顶上。
        //
        // 被否决的方案：调用失败时回退到「维持现状」这类保守默认值。
        // 看起来能提高 rollout 的成功率，但它会把「模型不可用」变成
        // 「该参与者选择不动」——行为证据被污染，而报告里看不出来。
        self.propose(spec, participant_id, history, perturbation).ok()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 提示词
// ─────────────────────────────────────────────────────────────────────────────

fn strategies_for(spec: &GameSpec, participant_id: &str) -> Vec<String> {
    // 取不到就是空空间（如环境方 external_actor，本就没有登记策略）：
    // 此时任何提议都只能是「非常规」——这是正确的语义，不是错误。
    // 参与者根本不在博弈里这种情况**不在这里拦**：那由编排器的
    // `validate_actors` 响亮报错；若在本函数里悄悄返回空空间，
    // 「配置错误」会被伪装成「该 persona 没有可用行动」。
    spec.strategies
        .get(participant_id)
        .cloned()
        .unwrap_or_default()
}

fn compose_prompt(
    persona: &Persona,
    spec: &GameSpec,
    strategies: &[String],
    history: &[Move],
    perturbation: f64,
) -> PersonaPrompt {
    // 非有限值当作 0.0：调用方给 NaN 是编程错误，但提示词构造不该 panic。
    let p = if perturbation.is_finite() {
        perturbation
    } else {
        0.0
    };

    let mut system = String::new();
    system.push_str(&format!(
        "你是「商奕」博弈仿真中的参与者子智能体，只扮演一个角色：{}（participant_id = {}）。\n\
         你的任务是：在当前局面下提议**本角色这一轮的下一步行动**。\n\n",
        persona.display_name, persona.participant_id
    ));
    system.push_str("## 角色卡\n");
    system.push_str(&persona.role_card);
    system.push_str("\n\n## 决策风格\n");
    system.push_str(&persona.decision_style);
    system.push_str(&format!(
        "\n本轮 persona 扰动：{p:+.2}（{}）\n\n",
        perturbation_note(p)
    ));
    // 工具白名单如实写入：provider 侧若不支持 tool_filter，这份声明只是提示，
    // 真正的强制点在编排器与能力预检（§5.7 的 fail-loud）。
    if !persona.tool_allowlist.is_empty() {
        system.push_str(&format!(
            "你可用的工具：{}。\n\n",
            persona.tool_allowlist.join("、")
        ));
    }
    system.push_str(
        "## 铁律（违反任一条，输出即视为无效）\n\
         1. 你只**提议行动**，不给结论：不预测支付、不计算收益或概率、不声称某结果必然发生——\
            行动后果由确定性引擎计算，不由你给出。\n\
         2. 你只代表本角色：不得替其他参与者做决定。\n\
         3. 允许提议策略空间之外的非常规行动，但须给出可核查的合法性依据与执行路径；\
            没有依据的非常规行动会被引擎拒绝。\n\
         4. 不得编造法条、平台规则、渠道、金额或人名；不确定就如实说不确定。\n",
    );

    let mut user = String::new();
    user.push_str("## 博弈结构\n");
    user.push_str(&format!("game_id：{}\n", spec.game_id));
    user.push_str("参与者：\n");
    for pt in spec.players.iter().chain(spec.external_actors.iter()) {
        let mark = if pt.id == persona.participant_id {
            "　← 你"
        } else {
            ""
        };
        user.push_str(&format!(
            "- {}（{}，{}）{}\n",
            pt.id,
            pt.vn_role,
            relation_zh(&pt.relation_to_game),
            mark
        ));
    }

    user.push_str("\n## 策略空间\n");
    if strategies.is_empty() {
        user.push_str(
            "- 你在本次博弈中**没有登记在册的策略**（你可能不是直接玩家）。\
             若要行动，只能提议非常规行动。\n",
        );
    } else {
        user.push_str(&format!(
            "- 你（{}）可选的「矩阵内」行动（**必须逐字使用**）：{}\n",
            persona.participant_id,
            strategies.join("、")
        ));
    }
    for (pid, list) in &spec.strategies {
        if pid == &persona.participant_id {
            continue;
        }
        user.push_str(&format!(
            "- 其他玩家 {} 的策略（供你判断对方的选择空间）：{}\n",
            pid,
            list.join("、")
        ));
    }

    user.push_str("\n## 到目前为止的行动历史\n");
    if history.is_empty() {
        user.push_str("（尚无行动：这是本轮的第一步。）\n");
    } else {
        let shown = history.len().min(MAX_HISTORY_SHOWN);
        if history.len() > shown {
            user.push_str(&format!(
                "（历史共 {} 条，此处仅列最近 {} 条。）\n",
                history.len(),
                shown
            ));
        }
        let start = history.len() - shown;
        for (i, m) in history[start..].iter().enumerate() {
            user.push_str(&format!(
                "{}. {} → {}（理由：{}）\n",
                start + i + 1,
                m.actor,
                action_key(&m.action),
                head(&m.rationale, MAX_RATIONALE_SHOWN)
            ));
        }
    }

    user.push_str(
        "\n## 输出\n\
         优先调用 `propose_move` 工具；若不能调用工具，则**只输出一个 JSON 对象**\
         （不要写解释文字，不要在 JSON 之外输出任何内容）：\n",
    );
    user.push_str(
        r#"{
  "kind": "in_matrix" 或 "novel",
  "strategy": "矩阵内行动：逐字照抄策略空间里的策略名",
  "description": "非常规行动：一句话描述（kind=novel 时必填）",
  "rationale": "为什么这样行动（中文，1–3 句）",
  "introduces": ["被引入的新参与者 id（可选；写进来就等于把新参与者带入博弈）"],
  "legality_basis": ["合法性依据：法条 / 平台规则 / 先例（novel 必填）"],
  "feasibility_basis": ["执行路径依据：本角色确实能执行的渠道（novel 必填）"],
  "expected_benefit": [下界, 上界],
  "expected_cost": [下界, 上界]
}
"#,
    );
    user.push_str(
        "（`expected_*` 是你基于角色卡信息的**粗略区间**，只交给引擎做三重过滤；\
         引擎自己会算 P(收益 > 成本)，你不要替它下判断。）\n",
    );

    PersonaPrompt { system, user }
}

/// 扰动 → 风格指引。
///
/// 只做**定性**映射、不做数值缩放：persona 是文本角色卡，没有可缩放的数值参数，
/// 硬把扰动乘到某个系数上等于凭空造数。三档是刻意的粗粒度——
/// 它要的是「同一位 persona 在多次 rollout 中呈现出行为分布」，不是精确响应曲线。
fn perturbation_note(p: f64) -> &'static str {
    if p > 0.33 {
        "更激进：更愿意承担风险、更倾向打破现状（含策略空间外的非常规行动）"
    } else if p < -0.33 {
        "更保守：优先维持现状与既有关系，倾向低风险行动"
    } else {
        "中性：按角色卡与当前局势本身权衡"
    }
}

fn relation_zh(r: &shangyi_gamespec::RelationToGame) -> &'static str {
    use shangyi_gamespec::RelationToGame as R;
    match r {
        R::Player => "直接玩家",
        R::ExternalActor => "环境方（可被引入）",
        R::ContextOnly => "仅画像库",
    }
}

fn action_key(a: &Action) -> String {
    match a {
        Action::InMatrix { strategy } => strategy.clone(),
        Action::Novel { description, .. } => format!("【非常规】{description}"),
    }
}

fn propose_move_tool() -> ToolSchema {
    // Schema 只用 A10b 实测支撑过的关键字（enum / 嵌套 object / required /
    // additionalProperties），不引入未验证的关键字（如 minItems）——
    // 未验证的 schema 关键字在真实端点上可能被拒或静默忽略，而离线测试看不出来。
    ToolSchema::function(
        PROPOSE_TOOL_NAME,
        "提交本角色这一轮的行动提议（只提议，不评估）",
        serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["in_matrix", "novel"],
                    "description": "矩阵内行动填 in_matrix；策略空间外的非常规行动填 novel"
                },
                "strategy": {
                    "type": "string",
                    "description": "矩阵内行动：策略空间里的策略名（逐字照抄）"
                },
                "description": {
                    "type": "string",
                    "description": "非常规行动的一句话描述（kind=novel 时必填）"
                },
                "rationale": {"type": "string", "description": "为什么这样行动（中文）"},
                "introduces": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "该行动引入的新参与者 id"
                },
                "legality_basis": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "合法性依据（法条 / 平台规则 / 先例）"
                },
                "feasibility_basis": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "执行路径依据"
                },
                "expected_benefit": {
                    "type": "array",
                    "items": {"type": "number"},
                    "description": "预期收益区间 [下界, 上界]"
                },
                "expected_cost": {
                    "type": "array",
                    "items": {"type": "number"},
                    "description": "预期成本区间 [下界, 上界]"
                }
            },
            "required": ["kind", "rationale"],
            "additionalProperties": false
        }),
    )
}

// ─────────────────────────────────────────────────────────────────────────────
// 响应 → 提议
// ─────────────────────────────────────────────────────────────────────────────

/// 从响应里取出可供解析的原文。
///
/// 优先 `propose_move` 的函数调用；若模型调用了**别的**工具名，直接拒绝而不去读它的
/// 参数——我们只声明了一个工具，出现别的名字说明模型没按契约走，
/// 此时把参数当提议读是猜测。
fn proposal_source(resp: &LlmResponse) -> Result<String, LlmActorError> {
    let calls = resp.function_calls();
    if !calls.is_empty() {
        let mut other: Vec<&str> = Vec::new();
        for (_, name, args) in &calls {
            if *name == PROPOSE_TOOL_NAME {
                // `arguments` 在响应侧是字符串、在内部已尝试解析为 Value；
                // 内层 JSON 非法时（典型：输出被截断）这里拿到的是原字符串，
                // 于是解析会失败并如实报「找不到 JSON 对象」——不 panic，也不猜。
                return Ok(match args {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                });
            }
            other.push(name);
        }
        return Err(LlmActorError::Unusable(format!(
            "模型调用了未声明的工具 {}——拒绝猜测其意图",
            other.join("、")
        )));
    }

    if !resp.is_completed() {
        // 截断/失败的响应不得当提议用（字段可能缺一半）。
        return Err(LlmActorError::Unusable(format!(
            "响应未正常完成（{}）",
            resp.truncation_note()
        )));
    }

    let text = resp.text();
    if text.trim().is_empty() {
        return Err(LlmActorError::Unusable(
            "响应既没有 propose_move 调用，也没有文本输出".to_string(),
        ));
    }
    Ok(text)
}

/// 把模型输出解析为提议（纯函数，可离线测试）。
///
/// # 容错的是**格式**，不容错的是**语义**
///
/// 接受的格式变体：整段 JSON、```` ```json ```` 代码块、夹在说明文字中间的
/// 平衡 JSON 片段、单键包裹（`{"move": {...}}`）、以及收益/成本的
/// `[lo, hi]` / `{lo, hi}` / 单点数（点估计 ⇒ `(x, x)`）。
///
/// 不接受的是语义歧义：找不到 JSON 就拒绝（**不把自由文本当行动**：无法区分
/// 「行动」与「评论」，硬认等于替模型编行动）；`kind=novel` 却用了矩阵内的策略名
/// 也拒绝（两种读法在引擎里走的是两条不同的路，拒绝猜）；区间倒置拒绝
/// （不静默交换端点——那是替模型改数）；策略名只在**本参与者的**策略空间里匹配，
/// 且只去首尾空白后**精确比对**（不模糊匹配、不做大小写折叠：
/// 把「降价10%」吸附到最近的策略上，是替模型宣布「它选的是这个」）。
///
/// # 缺区间时用占位 `(0.0, 0.0)`，并标记
///
/// [`Action::Novel`] 的收益/成本是必填字段，而模型可能只给了描述与依据。
/// 此时填 `(0.0, 0.0)` 并置 [`ParsedProposal::missing_quantification`]：
/// 语义是「**没有给出数字**」，不是「收益与成本为零」。
/// 该占位会（且应当）被激励相容判据拒绝——`P(0 > 0) = 0 < 阈值`——
/// 于是提议以「引擎明确拒绝 + 拒绝原因进报告」收场，
/// 而不是被静默丢弃（§5.6.1 要求报告能展示「考虑过但排除了哪些路径」）。
///
/// **被否决的方案**：在这里替模型补一个保守区间（如 `(0, 0)` 之外的中位数）。
/// 那是无中生有的数字，直接违背铁律 1。
pub fn parse_proposal(raw: &str, strategies: &[String]) -> Result<ParsedProposal, String> {
    let value = extract_json_object(raw).ok_or_else(|| {
        format!(
            "输出里找不到 JSON 对象（不把自由文本当行动：无法区分「行动」与「评论」）。原文前 200 字：{}",
            head(raw, 200)
        )
    })?;
    let value = unwrap_wrapper(&value);
    let obj = value
        .as_object()
        .ok_or_else(|| "解析出的 JSON 不是对象".to_string())?;

    let rationale = str_field(obj, &["rationale", "reason", "why"]).map(str::to_string);
    let kind = str_field(obj, &["kind", "action_kind", "type"]).map(|s| s.to_ascii_lowercase());
    let named = str_field(obj, &["strategy", "strategy_name", "action_name", "action"]);
    let described = str_field(obj, &["description", "novel_action", "novel_description"]);

    let matched = named.and_then(|n| canonical_strategy(strategies, n));

    if matches!(kind.as_deref(), Some("novel")) && matched.is_some() {
        return Err(format!(
            "语义冲突：kind 声明为非常规（novel），但名字「{}」命中策略空间内的策略——\
             矩阵内行动与非常规行动在引擎里走两条不同的路（后者要过三重过滤），拒绝猜是哪一种",
            named.unwrap_or_default()
        ));
    }

    if let Some(canonical) = matched {
        return Ok(ParsedProposal {
            // 用**矩阵里的拼写**而非模型给的（去空白后两者等价，取矩阵的更稳）
            action: Action::InMatrix {
                strategy: canonical.to_string(),
            },
            rationale,
            missing_quantification: false,
        });
    }

    let description = named.or(described).ok_or_else(|| {
        format!(
            "JSON 里没有可识别的行动字段（期望 `strategy` 或 `description`）；实得键：{}",
            obj.keys().cloned().collect::<Vec<_>>().join("、")
        )
    })?;

    let (expected_benefit, benefit_given) =
        range_field(obj, &["expected_benefit", "benefit", "benefit_range"])?;
    let (expected_cost, cost_given) = range_field(obj, &["expected_cost", "cost", "cost_range"])?;

    Ok(ParsedProposal {
        action: Action::Novel {
            description: description.to_string(),
            introduces: list_field(obj, &["introduces", "introduces_actors", "new_actors"]),
            legality_basis: list_field(obj, &["legality_basis", "legality"]),
            feasibility_basis: list_field(obj, &["feasibility_basis", "feasibility"]),
            expected_benefit,
            expected_cost,
        },
        rationale,
        missing_quantification: !(benefit_given && cost_given),
    })
}

fn canonical_strategy<'a>(strategies: &'a [String], name: &str) -> Option<&'a str> {
    strategies
        .iter()
        .map(String::as_str)
        .find(|s| s.trim() == name)
}

fn str_field<'a>(obj: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// 字符串列表字段（宽容三种形态：数组、单个字符串、`{id|name}` 对象数组）。
fn list_field(obj: &Map<String, Value>, keys: &[&str]) -> Vec<String> {
    let Some(v) = keys.iter().find_map(|k| obj.get(*k)) else {
        return Vec::new();
    };
    match v {
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                Vec::new()
            } else {
                vec![s.to_string()]
            }
        }
        Value::Array(a) => a.iter().filter_map(item_text).collect(),
        // 其他形态（数字、对象）在此丢弃：结果是空依据 ⇒ 引擎会**响亮拒绝**
        // 这条非常规提议，不会被误当成「有依据」。
        _ => Vec::new(),
    }
}

fn item_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        }
        Value::Object(o) => o
            .get("id")
            .or_else(|| o.get("name"))
            .or_else(|| o.get("actor"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// 区间字段。返回 `((lo, hi), 是否由模型给出)`。
fn range_field(
    obj: &Map<String, Value>,
    keys: &[&str],
) -> Result<((f64, f64), bool), String> {
    let Some(key) = keys
        .iter()
        .find(|k| obj.get(**k).is_some_and(|v| !v.is_null()))
    else {
        return Ok(((0.0, 0.0), false));
    };
    let v = &obj[*key];
    let pair = match v {
        // 点估计宽容为退化区间——这是形态差异，不是语义歧义
        Value::Number(n) => {
            let x = finite(n, key)?;
            (x, x)
        }
        Value::Array(a) if a.len() == 2 => {
            let lo = value_as_f64(&a[0], key)?;
            let hi = value_as_f64(&a[1], key)?;
            (lo, hi)
        }
        Value::Object(o) => {
            let lo = ["lo", "low", "min", "lower"]
                .iter()
                .find_map(|k| o.get(*k));
            let hi = ["hi", "high", "max", "upper"]
                .iter()
                .find_map(|k| o.get(*k));
            match (lo, hi) {
                (Some(l), Some(h)) => (value_as_f64(l, key)?, value_as_f64(h, key)?),
                _ => {
                    return Err(format!(
                        "`{key}` 的对象形态缺端点（可用 lo/hi、low/high、min/max）"
                    ))
                }
            }
        }
        other => {
            return Err(format!(
                "`{key}` 不是 [下界, 上界] 数组、也不是 {{lo, hi}} 对象或单个数，实得 {other}——\
                 「10-20」这类字符串写法有歧义，不做字符串解析"
            ))
        }
    };
    if pair.0 > pair.1 {
        return Err(format!(
            "`{key}` 区间倒置：下界 {} > 上界 {}——不静默交换端点（那是替模型改数）",
            pair.0, pair.1
        ));
    }
    Ok((pair, true))
}

fn value_as_f64(v: &Value, key: &str) -> Result<f64, String> {
    match v {
        Value::Number(n) => finite(n, key),
        other => Err(format!("`{key}` 的区间端点不是数字，实得 {other}")),
    }
}

fn finite(n: &serde_json::Number, key: &str) -> Result<f64, String> {
    n.as_f64()
        .filter(|x| x.is_finite())
        .ok_or_else(|| format!("`{key}` 不是有限数：{n}"))
}

// ─────────────────────────────────────────────────────────────────────────────
// JSON 抽取
// ─────────────────────────────────────────────────────────────────────────────

/// 从输出里抽出提议 JSON 对象。
///
/// 三道尝试，按「模型越守规矩越先命中」排序：整段 JSON → 代码块 → 正文里
/// 第一个带行动字段的平衡花括号片段。
///
/// 多个候选里取**第一个含行动字段的**（其次是第一个可解析的，好让报错信息
/// 落在「没有可识别的行动字段」上，而不是「找不到 JSON」）。
/// 不做「多个候选里挑一个」的智能裁决：那是猜测。
fn extract_json_object(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    // 整段就是一个 JSON 对象（最守规矩的输出形态）
    if let Some(v) = serde_json::from_str::<Value>(trimmed)
        .ok()
        .filter(Value::is_object)
    {
        return Some(v);
    }

    let mut first_parseable: Option<Value> = None;
    let mut candidates: Vec<&str> = fenced_blocks(text);
    let mut scanned = balanced_braces(text);
    candidates.append(&mut scanned);

    for cand in candidates {
        let Ok(v) = serde_json::from_str::<Value>(cand.trim()) else {
            continue;
        };
        if !v.is_object() {
            continue;
        }
        if has_action_key(&v) {
            return Some(v);
        }
        if first_parseable.is_none() {
            first_parseable = Some(v);
        }
    }
    first_parseable
}

const ACTION_KEYS: [&str; 9] = [
    "strategy",
    "strategy_name",
    "action_name",
    "action",
    "description",
    "kind",
    "action_kind",
    "move",
    "proposal",
];

fn has_action_key(v: &Value) -> bool {
    v.as_object()
        .map(|o| ACTION_KEYS.iter().any(|k| o.contains_key(*k)))
        .unwrap_or(false)
}

/// 单键包裹解包（如 `{"move": {...}}`），最多两层。
///
/// 只认白名单键名且内层必须是对象——`{"strategy": "..."}` 这类**不会**被解包
/// （它的值不是对象）。限定两层是防病态嵌套，不是省事。
fn unwrap_wrapper(v: &Value) -> &Value {
    const WRAPPERS: [&str; 5] = ["move", "proposal", "result", "output", "action"];
    let mut cur = v;
    for _ in 0..2 {
        let Some(obj) = cur.as_object() else { break };
        if obj.len() != 1 {
            break;
        }
        let Some((k, inner)) = obj.iter().next() else {
            break;
        };
        if !WRAPPERS.contains(&k.as_str()) || !inner.is_object() {
            break;
        }
        cur = inner;
    }
    cur
}

/// 三引号代码块的内容（去掉语言标注行）。
fn fenced_blocks(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        let Some(close) = after.find("```") else {
            break;
        };
        let block = &after[..close];
        // 去掉 ```json 这一行标注：首个换行前没有 `{` 就认定它是标注
        let block = match block.find('\n') {
            Some(nl) if !block[..nl].contains('{') => &block[nl + 1..],
            _ => block,
        };
        out.push(block);
        rest = &after[close + 3..];
    }
    out
}

/// 正文里所有**平衡**的 `{...}` 片段（跳过字符串字面量内的花括号与转义）。
fn balanced_braces(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut in_str = false;
    let mut escaped = false;

    for (i, b) in bytes.iter().enumerate() {
        if in_str {
            if escaped {
                escaped = false;
            } else if *b == b'\\' {
                escaped = true;
            } else if *b == b'"' {
                in_str = false;
            }
            continue;
        }
        match *b {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    start = i;
                }
                depth += 1;
            }
            b'}' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    // `{`/`}` 是 ASCII，切片端点必落在字符边界上
                    out.push(&text[start..=i]);
                }
            }
            _ => {}
        }
    }
    out
}

/// 按字符数截断（避免按字节切坏 UTF-8）。
fn head(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use shangyi_gamespec::{
        Expr, Param, ParamSource, Participant, PayoffForm, RelationToGame, ValueNetRole,
    };
    use shangyi_llm::{Endpoint, LlmError, Wire};

    use super::*;
    use crate::orchestrator::{OrchestratorConfig, rollout};
    use crate::{StopReason, SwarmError};

    // ── 夹具 ────────────────────────────────────────────────────────────────

    struct FakeTransport {
        responses: RefCell<Vec<(u16, String)>>,
        seen_bodies: RefCell<Vec<serde_json::Value>>,
    }

    impl FakeTransport {
        fn new(responses: Vec<(u16, String)>) -> Self {
            Self {
                responses: RefCell::new(responses),
                seen_bodies: RefCell::new(Vec::new()),
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
            self.seen_bodies.borrow_mut().push(body.clone());
            let mut r = self.responses.borrow_mut();
            if r.is_empty() {
                return Err(LlmError::Transport("测试用响应已耗尽".into()));
            }
            Ok(r.remove(0))
        }
    }

    /// 显式注入凭据的客户端：测试**不读也不写**进程环境变量
    /// （读会依赖外部配置，写是 Rust 2024 下的 unsafe 且并发下是数据竞态）。
    fn client(responses: Vec<(u16, String)>) -> LlmClient<FakeTransport> {
        let ep = Endpoint {
            base_url: "https://example.invalid".into(),
            api_key_env: "SHANGYI_TEST_KEY_UNUSED".into(),
            wire: Wire::Responses,
            echo_reasoning: true,
        };
        LlmClient::new(ep, FakeTransport::new(responses)).with_key("test-key-not-real")
    }

    /// Responses 形态的文本响应。
    fn text_response(text: &str) -> String {
        serde_json::json!({
            "id": "resp_test",
            "status": "completed",
            "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
            "usage": {"input_tokens": 1, "output_tokens": 1, "total_tokens": 2}
        })
        .to_string()
    }

    /// Responses 形态的函数调用响应（`arguments` 在线上是**字符串**）。
    fn call_response(name: &str, args: &str) -> String {
        serde_json::json!({
            "id": "resp_test",
            "status": "completed",
            "output": [{"type": "function_call", "call_id": "call_1", "name": name, "arguments": args}],
        })
        .to_string()
    }

    fn participant(id: &str, role: ValueNetRole) -> Participant {
        Participant {
            id: id.to_string(),
            name: id.to_string(),
            vn_role: role,
            relation_to_game: RelationToGame::Player,
            profile_ref: None,
        }
    }

    fn spec() -> GameSpec {
        let mut strategies = std::collections::BTreeMap::new();
        strategies.insert("focal".to_string(), vec!["维持价".into(), "降价10%".into()]);
        strategies.insert("comp_a".to_string(), vec!["跟随".into(), "不动".into()]);
        let mut params = std::collections::BTreeMap::new();
        // 这个参数名是**故意的探针**：它不得出现在提示词里（铁律 1，见下断言）
        params.insert(
            "P1".to_string(),
            Param {
                name: "P1".into(),
                value: Expr::Literal(42.0),
                dist: None,
                source: ParamSource::UserConfirmed {
                    checkpoint: "cp2".into(),
                },
                unit: None,
            },
        );
        GameSpec {
            game_id: "case-llm-actor".into(),
            players: vec![
                participant("focal", ValueNetRole::Focal),
                participant("comp_a", ValueNetRole::Substitutor),
            ],
            strategies,
            payoffs: PayoffForm::Normal {
                cells: std::collections::BTreeMap::new(),
            },
            params,
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: std::collections::BTreeMap::new(),
        }
    }

    fn personas() -> PersonaSet {
        PersonaSet::new(vec![
            Persona {
                participant_id: "focal".into(),
                display_name: "焦点企业".into(),
                decision_style: "务实、对价格战敏感".into(),
                role_card: "你是 focal，只代表其立场行事。".into(),
                tool_allowlist: vec!["game_solve".into()],
            },
            Persona {
                participant_id: "comp_a".into(),
                display_name: "替代者 A".into(),
                decision_style: "跟随为主".into(),
                role_card: "你是 comp_a，只代表其立场行事。".into(),
                tool_allowlist: vec![],
            },
        ])
    }

    // ── 正常路径 ────────────────────────────────────────────────────────────

    #[test]
    fn json_response_yields_the_proposed_move() {
        let c = client(vec![(
            200,
            text_response(
                r#"{"kind": "in_matrix", "strategy": "降价10%", "rationale": "对手跟随空间有限"}"#,
            ),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");

        let m = actor
            .act(&spec(), "focal", &[], 0.0)
            .expect("规范 JSON 必须解析出提议");

        assert_eq!(m.actor, "focal", "actor 取自 participant_id，不取自模型输出");
        assert_eq!(
            m.action,
            Action::InMatrix {
                strategy: "降价10%".into()
            }
        );
        assert_eq!(m.rationale, "对手跟随空间有限");
        assert_eq!(actor.trace().proposed, 1);
        assert_eq!(actor.trace().rejected, 0);
    }

    #[test]
    fn function_call_path_is_parsed_like_text() {
        // 线上 `arguments` 是字符串（A10b 实测）；这条路径必须与文本路径等价
        let c = client(vec![(
            200,
            call_response(
                PROPOSE_TOOL_NAME,
                r#"{"kind": "in_matrix", "strategy": "维持价", "rationale": "先观察"}"#,
            ),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");

        let m = actor
            .act(&spec(), "focal", &[], 0.1)
            .expect("函数调用路径必须可解析");
        assert_eq!(
            m.action,
            Action::InMatrix {
                strategy: "维持价".into()
            }
        );
    }

    #[test]
    fn fenced_json_block_is_tolerated() {
        let text = "我建议如下：\n```json\n{\"strategy\": \"维持价\", \"rationale\": \"稳一手\"}\n```\n";
        let c = client(vec![(200, text_response(text))]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let m = actor.act(&spec(), "focal", &[], 0.0).expect("代码块应被容忍");
        assert_eq!(
            m.action,
            Action::InMatrix {
                strategy: "维持价".into()
            }
        );
    }

    // ── 失败路径：绝不伪造默认行动 ──────────────────────────────────────────

    #[test]
    fn unparseable_output_yields_none_never_a_default_move() {
        let c = client(vec![(
            200,
            text_response("我觉得应该降价，因为对手大概率会跟着降，这样我们能抢到份额。"),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");

        let got = actor.act(&spec(), "focal", &[], 0.0);
        assert!(
            got.is_none(),
            "纯文本无法解析时必须返回 None——把这段评论硬认成行动就是伪造提议；实得 {got:?}"
        );
        let t = actor.trace();
        assert_eq!((t.attempts, t.proposed, t.rejected), (1, 0, 1));
        assert!(
            t.last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("JSON"),
            "拒绝原因要留痕（诊断靠它）：{:?}",
            t.last_failure
        );
    }

    #[test]
    fn transport_failure_yields_none_and_is_recorded() {
        // 响应队列为空 ⇒ 假传输返回 Err ⇒ 必须 None，且原因可查
        let c = client(vec![]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        assert!(actor.act(&spec(), "focal", &[], 0.0).is_none());
        let t = actor.trace();
        assert_eq!(t.rejected, 1);
        assert!(
            t.last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("LLM 调用失败"),
            "{:?}",
            t.last_failure
        );
    }

    #[test]
    fn truncated_response_is_rejected() {
        let body = serde_json::json!({
            "id": "resp_test",
            "status": "incomplete",
            "incomplete_details": {"reason": "max_output_tokens"},
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "{\"strategy\": \"降价10%\""}]}],
        })
        .to_string();
        let c = client(vec![(200, body)]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        assert!(
            actor.act(&spec(), "focal", &[], 0.0).is_none(),
            "截断的输出不得当完整提议用"
        );
    }

    #[test]
    fn unknown_persona_is_rejected_not_generic_assistant() {
        let c = client(vec![(
            200,
            text_response(r#"{"strategy": "降价10%", "rationale": "x"}"#),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        // "nobody" 不在 PersonaSet：没有角色卡就调用等于退化为通用助手（§5.7 禁止）
        assert!(actor.act(&spec(), "nobody", &[], 0.0).is_none());
        assert!(
            actor
                .trace()
                .last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("未在 PersonaSet 中注册")
        );
    }

    #[test]
    fn undeclared_tool_call_is_rejected() {
        let c = client(vec![(200, call_response("some_other_tool", "{}"))]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        assert!(actor.act(&spec(), "focal", &[], 0.0).is_none());
        assert!(
            actor
                .trace()
                .last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("未声明的工具")
        );
    }

    // ── 矩阵外：合法路径 ────────────────────────────────────────────────────

    #[test]
    fn novel_proposal_outside_the_strategy_space_is_accepted() {
        let payload = r#"{
          "kind": "novel",
          "description": "联合主要客户向对手的海外供应商发函投诉",
          "rationale": "借客户的 ESG 诉求改变对手的支持结构",
          "introduces": ["major_customer"],
          "legality_basis": ["民法典侵权责任条款", "供应商行为准则"],
          "feasibility_basis": ["已有客户采购联系人"],
          "expected_benefit": [10, 30],
          "expected_cost": [1, 3]
        }"#;
        let c = client(vec![(200, text_response(payload))]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");

        let m = actor
            .act(&spec(), "focal", &[], 0.5)
            .expect("策略空间外的提议是合法路径，不得丢弃");

        match &m.action {
            Action::Novel {
                description,
                introduces,
                legality_basis,
                feasibility_basis,
                expected_benefit,
                expected_cost,
            } => {
                assert_eq!(description, "联合主要客户向对手的海外供应商发函投诉");
                assert_eq!(introduces, &vec!["major_customer".to_string()]);
                assert_eq!(legality_basis.len(), 2);
                assert_eq!(feasibility_basis, &vec!["已有客户采购联系人".to_string()]);
                assert_eq!(*expected_benefit, (10.0, 30.0));
                assert_eq!(*expected_cost, (1.0, 3.0));
            }
            other => panic!("应为 Novel，实得 {other:?}"),
        }
        assert_eq!(
            actor.trace().novels_missing_ranges,
            0,
            "模型给了区间，不该记成「缺区间」"
        );
    }

    #[test]
    fn out_of_space_strategy_name_becomes_novel_not_a_snapped_neighbor() {
        // 模型自称 in_matrix，但名字不在本参与者的策略空间里。
        // 不得吸附到最近的策略名（那是替模型宣布「它选的是这个」），
        // 也不得丢弃（§5.7.1 的非常规行动正是这样冒出来的）。
        let c = client(vec![(
            200,
            text_response(
                r#"{"kind": "in_matrix", "strategy": "向行业协会申请调解", "rationale": "低成本试探"}"#,
            ),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");

        let m = actor.act(&spec(), "focal", &[], 0.0).expect("应为合法路径");
        match &m.action {
            Action::Novel { description, .. } => {
                assert_eq!(description, "向行业协会申请调解");
            }
            other => panic!("空间外的名字必须成为 Novel（不得吸附到矩阵内策略），实得 {other:?}"),
        }
        // 缺区间 ⇒ 记为占位，由引擎如实拒绝（不是 actor 替它编数）
        assert_eq!(actor.trace().novels_missing_ranges, 1);
    }

    #[test]
    fn novel_without_ranges_uses_placeholder_and_is_flagged() {
        let c = client(vec![(
            200,
            text_response(r#"{"description": "向监管投诉", "rationale": "没有别的牌"}"#),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let m = actor.act(&spec(), "focal", &[], 0.0).expect("描述足以形成提议");
        match &m.action {
            Action::Novel {
                expected_benefit,
                expected_cost,
                legality_basis,
                ..
            } => {
                assert_eq!(*expected_benefit, (0.0, 0.0), "占位=未给出数字");
                assert_eq!(*expected_cost, (0.0, 0.0));
                assert!(legality_basis.is_empty(), "无依据由引擎如实拒绝");
            }
            other => panic!("应为 Novel，实得 {other:?}"),
        }
        assert_eq!(actor.trace().novels_missing_ranges, 1);
    }

    #[test]
    fn explicit_novel_kind_with_matrix_strategy_name_is_rejected_as_ambiguous() {
        let c = client(vec![(
            200,
            text_response(r#"{"kind": "novel", "strategy": "降价10%", "rationale": "换个打法"}"#),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        assert!(
            actor.act(&spec(), "focal", &[], 0.0).is_none(),
            "语义冲突时拒绝猜（矩阵内与非常规在引擎里走两条不同的路）"
        );
        assert!(
            actor
                .trace()
                .last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("语义冲突")
        );
    }

    #[test]
    fn inverted_range_is_rejected_not_silently_swapped() {
        let c = client(vec![(
            200,
            text_response(
                r#"{"description": "投诉", "expected_benefit": [30, 10], "expected_cost": [1, 2]}"#,
            ),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        assert!(
            actor.act(&spec(), "focal", &[], 0.0).is_none(),
            "自相矛盾的区间不得静默修正"
        );
        assert!(
            actor
                .trace()
                .last_failure
                .as_deref()
                .unwrap_or_default()
                .contains("区间倒置")
        );
    }

    // ── 提示词：persona 覆盖、铁律 1、可复现 ────────────────────────────────

    #[test]
    fn prompt_injects_persona_and_strategy_space_but_no_payoff_params() {
        let c = client(vec![
            (200, text_response(r#"{"strategy": "降价10%", "rationale": "a"}"#)),
            (200, text_response(r#"{"strategy": "降价10%", "rationale": "b"}"#)),
        ]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let s = spec();

        actor.act(&s, "focal", &[], 0.42).unwrap();
        actor.act(&s, "focal", &[], 0.42).unwrap();

        let bodies = c.transport().seen_bodies.borrow();
        let text = bodies[0].to_string();

        assert!(text.contains("你是 focal，只代表其立场行事。"), "角色卡须注入");
        assert!(text.contains("务实、对价格战敏感"), "决策风格须注入");
        assert!(text.contains("维持价"), "本参与者的策略空间须可见");
        assert!(text.contains("+0.42"), "扰动值须如实写入提示词");
        assert!(text.contains(PROPOSE_TOOL_NAME), "工具须声明");

        // 铁律 1：支付参数不进提示词（P1 = 42.0 是夹具里的探针）
        assert!(
            !text.contains("P1"),
            "参数不得进 persona 提示词——递数字就是递计算材料"
        );

        // 同输入 ⇒ 逐字节相同的请求体（模型输出不可复现，但**提示词**可复现）
        assert_eq!(
            bodies[0], bodies[1],
            "提示词必须是 (persona, spec, history, perturbation) 的纯函数"
        );
    }

    #[test]
    fn perturbation_shifts_the_stated_decision_style() {
        let c = client(vec![]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let s = spec();

        let aggressive = actor.build_prompt(&s, "focal", &[], 0.9).unwrap();
        let conservative = actor.build_prompt(&s, "focal", &[], -0.9).unwrap();

        assert!(aggressive.system.contains("更激进"));
        assert!(conservative.system.contains("更保守"));
        assert_ne!(
            aggressive.system, conservative.system,
            "扰动必须改变提示词，否则「多样性采样」名不副实"
        );
    }

    #[test]
    fn history_is_rendered_and_truncation_is_disclosed() {
        let c = client(vec![]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let s = spec();

        let history: Vec<Move> = (0..15)
            .map(|i| {
                Move::new(
                    "focal",
                    Action::InMatrix {
                        strategy: "维持价".into(),
                    },
                    format!("第{i}轮理由"),
                )
            })
            .collect();

        let prompt = actor.build_prompt(&s, "focal", &history, 0.0).unwrap();
        assert!(prompt.user.contains("第14轮理由"), "最近一条必须在场");
        assert!(!prompt.user.contains("第0轮理由"), "超出上限的旧历史应被裁掉");
        assert!(
            prompt.user.contains("仅列最近 12 条"),
            "截断必须如实标注，不能假装那就是全部历史"
        );
    }

    // ── 与编排器的对接 ──────────────────────────────────────────────────────

    #[test]
    fn actor_plugs_into_orchestrator_rollout() {
        // 两位 persona 各用各的策略空间：focal → 降价10%，comp_a → 跟随
        let payload = |strategy: &str| {
            text_response(&format!(
                r#"{{"kind": "in_matrix", "strategy": "{strategy}", "rationale": "测试"}}"#
            ))
        };
        let mut responses = Vec::new();
        for _ in 0..3 {
            responses.push((200, payload("降价10%")));
            responses.push((200, payload("跟随")));
        }

        let c = client(responses);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let ps = personas();
        let config = OrchestratorConfig {
            max_rounds: 5,
            perturb: false,
            stable_rounds: 2,
        };

        let traj = rollout(&spec(), &ps, &actor, &config, 7).expect("rollout 不应失败");

        assert_eq!(traj.stop_reason, StopReason::Stable);
        assert_eq!(traj.moves.len(), 6, "3 回合 × 2 persona");
        assert!(
            traj.moves.iter().all(|m| !m.action.is_novel()),
            "两条提议都是矩阵内行动"
        );
        assert_eq!(actor.trace().proposed, 6, "留痕与轨迹条数一致");
    }

    #[test]
    fn llm_outage_looks_like_stability_at_orchestrator_level_but_trace_shows_it() {
        // 这条测试锁定**已知的坑**（不是期望行为，是当前事实）：
        // act 返回 None 与「该 persona 无行动」在编排器眼里一样，
        // 都会走向 StopReason::Stable。故接线方必须读 ActorTrace。
        let c = client(vec![(200, text_response(r#"{"strategy": "降价10%", "rationale": "x"}"#))]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let ps = personas();
        let config = OrchestratorConfig {
            max_rounds: 3,
            perturb: false,
            stable_rounds: 2,
        };

        let traj = rollout(&spec(), &ps, &actor, &config, 11).unwrap();
        assert_eq!(
            traj.stop_reason,
            StopReason::Stable,
            "端点在第二回合就耗尽，编排器只会说「稳定」"
        );
        assert!(
            actor.trace().rejected > 0,
            "唯一的真信号在 trace 里——接线方不读它，故障就会以「博弈稳定」进报告"
        );
    }

    #[test]
    fn the_move_actor_comes_from_the_caller_not_from_the_model() {
        // 模型输出里没有任何 actor 字段；即便它写了「替别人行动」的理由，
        // Move.actor 也只能是传入的 participant_id——身份由调用方定，不由模型自报。
        let c = client(vec![(
            200,
            text_response(r#"{"strategy": "跟随", "rationale": "替对手决定"}"#),
        )]);
        let actor = LlmPersonaActor::new(&c, personas(), "test-model");
        let m = actor.act(&spec(), "comp_a", &[], 0.0).unwrap();

        assert_eq!(m.actor, "comp_a");
        assert!(matches!(m.action, Action::InMatrix { .. }));
    }

    #[test]
    fn unknown_actor_is_left_for_the_orchestrator_to_report_loudly() {
        // persona 集合里有一个不在 spec 中的参与者 ⇒ 提议照出，由
        // validate_actors 报 UnknownActor（响亮），而不是在这里悄悄变成 None
        // （悄悄 None 会被编排器读成「无行动可提议」，比报错更安静）
        let mut ps = personas();
        ps.personas.push(Persona {
            participant_id: "ghost".into(),
            display_name: "幽灵".into(),
            decision_style: "无".into(),
            role_card: "你是 ghost。".into(),
            tool_allowlist: vec![],
        });
        let c = client(vec![
            (200, text_response(r#"{"strategy": "维持价", "rationale": "x"}"#)),
            (200, text_response(r#"{"strategy": "跟随", "rationale": "y"}"#)),
            (200, text_response(r#"{"strategy": "维持价", "rationale": "z"}"#)),
        ]);
        let actor = LlmPersonaActor::new(&c, ps.clone(), "test-model");
        let config = OrchestratorConfig {
            max_rounds: 1,
            perturb: false,
            stable_rounds: 2,
        };
        let err = rollout(&spec(), &ps, &actor, &config, 3).unwrap_err();
        assert!(
            matches!(err, SwarmError::UnknownActor(ref a) if a == "ghost"),
            "实得 {err:?}"
        );
    }
}

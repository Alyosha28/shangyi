//! 商奕条件后果预测（whatif 管线，设计文档 §5.6.1）。
//!
//! # 本 crate 的职责
//!
//! 输入用户的一个具体决策动作 `d`，输出：`d → [(对手反应 r, P(r|d), 双方结果分布, 驱动判据)]`。
//!
//! # 为什么"非常规反应生成"是必要组成而非可选增强
//!
//! 设计文档 §5.6.1 记录了**星宇股份事件（2026-08）**的教训：该公司批量解约应届生后，
//! 学生的致命反击**不在劳资博弈的策略矩阵内**——他们向该公司的**客户与海外供应商**
//! 发函投诉、诉至**交易所**，把新参与者引入博弈、借其 ESG / IPO 声誉激励改变支付结构，
//! 最终市值大幅蒸发、公司公开道歉。
//!
//! **封闭策略空间的 BR 枚举对这类反击结构性失明**：无论枚举多完备，都只在给定的
//! 策略集内搜索，而反击者做的是"改变棋盘"——引入新参与者（`external_actors`）或
//! 借第三方之力。故本 crate 的管线中，[`NovelMoveProposal`] 经**三重过滤**后
//! 会**注册回 `GameSpec` 并重解**。
//!
//! # 与 LLM 的分工（铁律 1）
//!
//! - **LLM 负责"想得到"**：非常规反击候选由 LLM（或 swarm sub-agent）**提议**——
//!   人类非常规反击的模式内化于海量公开案例，这是 LLM 的强项。
//! - **代码负责"算得清"**：[`TripleFilter`] 的三项判定全部是确定性代码；
//!   提议只是**输入**，是否成立由代码判定。
//!
//! 故本 crate **不调用 LLM**：提议经 [`NovelMoveProposal`] 注入。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use shangyi_gamespec::{GameSpec, Participant, RelationToGame, ValueNetRole};

/// 一个可由 LLM / swarm 提议的**矩阵外**行动。
///
/// 命名取"提议"而非"行动"：它尚未经过滤，是否成立由代码判定（见 [`TripleFilter`]）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NovelMoveProposal {
    /// 行动者（通常是弱势方）。
    pub actor_id: String,
    /// 行动描述（人类可读，进报告）。
    pub description: String,
    /// 该行动**指向**的既有参与者（如"对手的大客户"）。可为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub targets: Option<String>,
    /// 该行动**引入**的新参与者（如交易所、监管、舆论）——`external_actors` 的来源。
    #[serde(default)]
    pub introduces_actors: Vec<Participant>,
    /// 该行动如何改变支付结构（自然语言，供报告与人工复核）。
    pub payoff_channel: String,
    /// 行动者预期收益（须为区间，反映不确定性）。
    pub expected_benefit: Interval,
    /// 行动者执行成本（须为区间）。
    pub expected_cost: Interval,
    /// 合法性依据（法条/平台规则/已发生的先例）。空 = 未提供依据。
    #[serde(default)]
    pub legality_basis: Vec<String>,
    /// 资源可得性依据（行动者是否**真的**能执行）。
    #[serde(default)]
    pub feasibility_basis: Vec<String>,
}

/// 区间估计 `[lo, hi]`（含端点）。用于收益/成本的**不确定性**表达。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Interval {
    pub lo: f64,
    pub hi: f64,
}

impl Interval {
    pub fn new(lo: f64, hi: f64) -> Result<Self, FilterError> {
        if !lo.is_finite() || !hi.is_finite() {
            return Err(FilterError::NonFiniteInterval);
        }
        if lo > hi {
            return Err(FilterError::InvertedInterval { lo, hi });
        }
        Ok(Self { lo, hi })
    }

    pub fn point(v: f64) -> Result<Self, FilterError> {
        Self::new(v, v)
    }

    pub fn mid(&self) -> f64 {
        (self.lo + self.hi) / 2.0
    }

    pub fn is_entirely_positive(&self) -> bool {
        self.lo > 0.0
    }

    pub fn is_entirely_nonpositive(&self) -> bool {
        self.hi <= 0.0
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FilterError {
    #[error("区间端点非有限值")]
    NonFiniteInterval,
    #[error("区间端点倒置：lo={lo} > hi={hi}")]
    InvertedInterval { lo: f64, hi: f64 },
    #[error("阈值须 ∈ [0,1]，实得 {0}")]
    InvalidThreshold(f64),
    #[error("Monte Carlo 抽样次数须 > 0")]
    ZeroSamples,
}

/// 三重过滤的判定结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterVerdict {
    /// ① 合法性：有法条/平台规则/先例依据。
    pub legal: bool,
    /// ② 资源可得性：行动者**确实**能执行（不是"理论上可以"）。
    pub feasible: bool,
    /// ③ 激励相容：`P(收益 > 成本) ≥ 阈值`。
    ///
    /// ⚠️ 口径修订（设计文档差异登记 R18）：原文写"预期收益 > 成本"，
    /// 现改为**概率阈值**口径，与 §5.6.2 的达标概率一致。
    /// 理由：期望值比较会掩盖方差——一个"期望正、但 40% 概率巨亏"的行动
    /// 对弱势方而言未必理性；概率口径直接表达"多大概率不亏"。
    pub incentive_compatible: bool,
    /// `P(收益 > 成本)` 的估计（区间中点比较的近似，见 [`TripleFilter::incentive_probability`]）。
    pub incentive_probability: f64,
    /// 未通过的原因（逐项列出，进报告）。
    pub rejections: Vec<String>,
}

impl FilterVerdict {
    /// 三项全通过才注册进博弈。
    pub fn passes(&self) -> bool {
        self.legal && self.feasible && self.incentive_compatible
    }
}

/// 三重过滤器（`合法性 × 资源可得性 × 激励相容`）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TripleFilter {
    /// 激励相容的达标概率阈值。设计文档 §5.6.2 的默认达标概率为 0.6。
    pub incentive_threshold: f64,
}

impl Default for TripleFilter {
    fn default() -> Self {
        // 0.6 与 §5.6.2 的"达标概率 ≥ 阈值（默认 0.6，随偏好校准）"一致
        Self {
            incentive_threshold: 0.6,
        }
    }
}

impl TripleFilter {
    pub fn new(incentive_threshold: f64) -> Result<Self, FilterError> {
        if !(0.0..=1.0).contains(&incentive_threshold) || !incentive_threshold.is_finite() {
            return Err(FilterError::InvalidThreshold(incentive_threshold));
        }
        Ok(Self {
            incentive_threshold,
        })
    }

    /// 判定一个提议是否可注册进博弈。
    ///
    /// # 各判据的"依据"要求
    ///
    /// 合法性要求 `legality_basis` **非空**——不接受"看起来合法"这种无依据的判断。
    /// 资源可得性同理要求 `feasibility_basis` 非空。
    ///
    /// 这不是形式主义：星宇事件里，学生的反击之所以有效，恰恰因为**有具体依据与渠道**
    /// （向具体客户发函、向具体交易所投诉）。一个"理论上可能的反击"若找不到执行路径，
    /// 就不该进入博弈矩阵——否则会产出虚高的反击概率，误导用户。
    pub fn evaluate(&self, p: &NovelMoveProposal) -> FilterVerdict {
        let mut rejections = Vec::new();

        // ① 合法性
        let legal = !p.legality_basis.is_empty()
            && p.legality_basis.iter().all(|s| !s.trim().is_empty());
        if !legal {
            rejections.push(
                "合法性：未提供法条/平台规则/先例依据——不得以「看起来合法」入博弈".to_string(),
            );
        }

        // ② 资源可得性
        let feasible = !p.feasibility_basis.is_empty()
            && p.feasibility_basis.iter().all(|s| !s.trim().is_empty());
        if !feasible {
            rejections.push(
                "资源可得性：未提供执行路径依据——「理论上可能」不等于「该行动者能执行」"
                    .to_string(),
            );
        }

        // ③ 激励相容：P(收益 > 成本) ≥ 阈值
        let prob = self.incentive_probability(&p.expected_benefit, &p.expected_cost);
        let incentive_compatible = prob >= self.incentive_threshold;
        if !incentive_compatible {
            rejections.push(format!(
                "激励相容：P(收益 > 成本) = {prob:.3} < 阈值 {:.3}",
                self.incentive_threshold
            ));
        }

        FilterVerdict {
            legal,
            feasible,
            incentive_compatible,
            incentive_probability: prob,
            rejections,
        }
    }

    /// 估计 `P(收益 > 成本)`。
    ///
    /// # 方法（保守，可解释）
    ///
    /// 仅凭两区间无法得到真实分布，故用**三情形加权**给出保守估计：
    ///
    /// - 若 `收益.lo > 成本.hi`：**必然**盈利 ⇒ `P = 1`
    /// - 若 `收益.hi ≤ 成本.lo`：**必然**亏损 ⇒ `P = 0`
    /// - 否则区间重叠，按**重叠程度**线性插值：
    ///   `P ≈ (收益.hi − 成本.lo) / ((收益.hi − 收益.lo) + (成本.hi − 成本.lo))`
    ///
    /// 该式在重叠区间的两端取 0 与 1，且可化为**闭式**：
    ///
    /// ```text
    /// P = 0.5 + (收益中点 − 成本中点) / (2 · (收益半宽 + 成本半宽))
    /// ```
    ///
    /// 由此可读出两个性质（均有测试锁定）：
    /// - **固定中点差时，区间越宽 P 越接近 0.5** —— 信息越少越不确定；
    /// - **中点差为 0 时 P 恒为 0.5**，与宽度无关。
    ///
    /// 后者是一个容易被误用的边界：想测"宽度影响"必须固定非零中点差，
    /// 否则两组输入的 P 都恰好是 0.5，测不出任何东西。
    ///
    /// **诚实标注**：这是 `[推断]` 式启发式，不是由分布推导的精确概率。
    /// 若调用方有更细的分布（如 `GameSpec::params` 的 `Dist`），应改用 Monte Carlo
    /// （见 [`monte_carlo_percentiles`]）并替换此估计。
    pub fn incentive_probability(&self, benefit: &Interval, cost: &Interval) -> f64 {
        if benefit.lo > cost.hi {
            return 1.0;
        }
        if benefit.hi <= cost.lo {
            return 0.0;
        }
        let benefit_width = benefit.hi - benefit.lo;
        let cost_width = cost.hi - cost.lo;
        let denom = benefit_width + cost_width;
        if denom <= f64::EPSILON {
            // 两个点估计且跨过临界：收益.lo ≤ 成本.hi 且 收益.hi > 成本.lo
            // ⇒ 中点比较（等价于原 R18 的"预期收益 > 成本"）
            return if benefit.mid() > cost.mid() { 1.0 } else { 0.0 };
        }
        ((benefit.hi - cost.lo) / denom).clamp(0.0, 1.0)
    }
}

/// 已注册进博弈的非常规行动（通过三重过滤者）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisteredMove {
    pub proposal: NovelMoveProposal,
    /// 过滤时的判定（留档，供审计）。
    pub verdict: FilterVerdict,
}

/// 过滤产物：通过者与被拒者**分开返回**。
///
/// **为什么不只返回通过者**：被拒原因进报告③层（参与者画像与证据），
/// 使用户能看到"系统考虑过但排除了哪些反击路径"——这是可信度的一部分。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterOutcome {
    pub accepted: Vec<RegisteredMove>,
    pub rejected: Vec<(NovelMoveProposal, FilterVerdict)>,
}

/// 对一批提议执行三重过滤，并把通过者**注册回 `GameSpec`**。
///
/// 注册内容：
/// 1. 通过者引入的新参与者 → `spec.external_actors`（去重）
/// 2. 通过者的行动 → 追加为 `actor_id` 的新策略（若该玩家存在）
///
/// # 为什么是"注册回并重解"而非"单独看反击"
///
/// 反击之所以致命，往往正是因为它**改变了其他人的支付结构**（客户施压 → 公司
/// 声誉激励改变）。若不注册回博弈、只算反击者自身收益，就丢掉这个机制——
/// 星宇事件里学生自己的收益（出了一口气）远小于公司的损失（市值蒸发），
/// 真正的作用链条是"借第三方之力改变对手的支付"。
pub fn filter_and_register(
    spec: &mut GameSpec,
    proposals: &[NovelMoveProposal],
    filter: &TripleFilter,
) -> FilterOutcome {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    for p in proposals {
        let verdict = filter.evaluate(p);
        if verdict.passes() {
            // 注册引入的新参与者（按 id 去重，避免重复注册）
            for actor in &p.introduces_actors {
                let exists = spec
                    .external_actors
                    .iter()
                    .chain(spec.players.iter())
                    .any(|e| e.id == actor.id);
                if !exists {
                    let mut a = actor.clone();
                    // 强制其定位为环境方：被引入者不参与原始博弈的策略组合
                    a.relation_to_game = RelationToGame::ExternalActor;
                    spec.external_actors.push(a);
                }
            }
            // 注册行动为策略（仅当行动者是本博弈的直接玩家）
            let is_player = spec
                .players
                .iter()
                .any(|pl| pl.id == p.actor_id && pl.relation_to_game == RelationToGame::Player);
            if is_player {
                let label = format!("[非常规] {}", p.description);
                spec.strategies
                    .entry(p.actor_id.clone())
                    .or_default()
                    .push(label);
            }
            accepted.push(RegisteredMove {
                proposal: p.clone(),
                verdict,
            });
        } else {
            rejected.push((p.clone(), verdict));
        }
    }

    FilterOutcome {
        accepted,
        rejected,
    }
}

/// 分位数三元组 `P10 / P50 / P90`。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Percentiles {
    pub p10: f64,
    pub p50: f64,
    pub p90: f64,
}

/// 从样本估计分位数（线性插值，与 numpy 的 `linear` 方法一致）。
///
/// **为何给 P10/P90 而不只给均值**：设计文档 §5.5 要求呈现"最差情形"，
/// 而中小企业主对"最差能差到哪"的敏感度远高于均值。P10 是悲观情形。
pub fn percentiles_from_samples(mut samples: Vec<f64>) -> Result<Percentiles, FilterError> {
    if samples.is_empty() {
        return Err(FilterError::ZeroSamples);
    }
    samples.retain(|v| v.is_finite());
    if samples.is_empty() {
        return Err(FilterError::ZeroSamples);
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Percentiles {
        p10: quantile(&samples, 0.10),
        p50: quantile(&samples, 0.50),
        p90: quantile(&samples, 0.90),
    })
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    debug_assert!(!sorted.is_empty());
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    if lo == hi {
        return sorted[lo];
    }
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Monte Carlo 参数不确定性传播。
///
/// 输入一组"参数 → 采样值"的函数，输出结果的 P10/P50/P90。
///
/// # 确定性
///
/// 用**显式种子**的线性同余发生器（LCG），不依赖 `rand` crate 的默认熵源，
/// 使同一 `(seed, samples)` 必然给出同一结果。这是可复现性要求：
/// 报告的每个数字都必须能被第三方用同样输入复现（铁律 1 的审计要求）。
pub fn monte_carlo_percentiles<F>(
    samples: usize,
    seed: u64,
    mut evaluate: F,
) -> Result<Percentiles, FilterError>
where
    F: FnMut(&mut Lcg) -> f64,
{
    if samples == 0 {
        return Err(FilterError::ZeroSamples);
    }
    let mut rng = Lcg::new(seed);
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        out.push(evaluate(&mut rng));
    }
    percentiles_from_samples(out)
}

/// 线性同余伪随机数发生器（Numerical Recipes 参数）。
///
/// **为什么不用 `rand`**：本 crate 的确定性要求是"同种子同序列"，
/// 而 `rand` 的实现细节可能随版本变化（其 `StdRng` 明确不保证跨版本稳定）。
/// 自带 LCG 使可复现性不依赖于依赖版本——这比随机数质量更重要，
/// 因为此处只用于蒙特卡洛传播，不用于密码学。
#[derive(Debug, Clone)]
pub struct Lcg {
    state: u64,
}

impl Lcg {
    pub fn new(seed: u64) -> Self {
        // 避免 0 状态（LCG 的 0 是吸收态）
        Self {
            state: if seed == 0 { 0x2545_F491_4F6C_DD1D } else { seed },
        }
    }

    /// 下一个 `u64`。
    pub fn next_u64(&mut self) -> u64 {
        // Numerical Recipes 的 LCG 参数
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// `[0, 1)` 均匀分布。
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// 标准正态（Box–Muller）。
    pub fn next_normal(&mut self) -> f64 {
        let u1 = self.next_f64().max(f64::MIN_POSITIVE);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// 均匀分布 `[lo, hi)`。
    pub fn next_uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }
}

/// 一个对手反应及其概率与后果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReactionOutcome {
    /// 反应标识（策略名或"非常规"行动的描述）。
    pub reaction: String,
    /// `P(r | d)` —— 给定本决策后该反应出现的概率。
    pub probability: f64,
    /// 该反应下各方的结果分布（P10/P50/P90）。
    pub outcomes: BTreeMap<String, Percentiles>,
    /// 驱动该概率判断的判据（进报告，解释"为什么认为对手会这样反应"）。
    pub drivers: Vec<String>,
    /// 该反应是否来自矩阵外（非常规反击）。
    pub is_novel: bool,
}

/// 条件后果报告（`ConsequenceReport`）——直接进报告②层与老板页。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConsequenceReport {
    /// 被推演的决策 `d`。
    pub decision: String,
    /// 各反应的分布。`probability` 之和应为 1。
    pub reactions: Vec<ReactionOutcome>,
    /// 被三重过滤挡下的非常规候选（进报告③层，供用户看到"考虑过哪些路径"）。
    pub rejected_novel_moves: Vec<(NovelMoveProposal, FilterVerdict)>,
    /// 报告层可直接引用的说明。
    pub note: String,
}

impl ConsequenceReport {
    /// 检查反应概率是否归一化（报告生成前的自检）。
    pub fn probabilities_normalized(&self) -> bool {
        let sum: f64 = self.reactions.iter().map(|r| r.probability).sum();
        (sum - 1.0).abs() < 1e-6
    }

    /// 按概率降序的反应（用于"最可能的反应是什么"）。
    pub fn by_probability(&self) -> Vec<&ReactionOutcome> {
        let mut v: Vec<&ReactionOutcome> = self.reactions.iter().collect();
        v.sort_by(|a, b| {
            b.probability
                .partial_cmp(&a.probability)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.reaction.cmp(&b.reaction))
        });
        v
    }

    /// 含非常规反应的项（老板页需要单独标注这类"没想到的可能"）。
    pub fn novel_reactions(&self) -> Vec<&ReactionOutcome> {
        self.reactions.iter().filter(|r| r.is_novel).collect()
    }
}

/// 价值网角色 → 中文名（报告用）。
pub fn role_label(role: ValueNetRole) -> &'static str {
    match role {
        ValueNetRole::Focal => "本企业",
        ValueNetRole::Customer => "顾客",
        ValueNetRole::Supplier => "供应商",
        ValueNetRole::Substitutor => "替代者",
        ValueNetRole::Complementor => "互补者",
    }
}

//! 商奕满意策略发现（discover 模式，设计文档 §5.6.2）。
//!
//! # 理论根基
//!
//! **Herbert Simon 满意化（satisficing）**——有限理性下不求最优解、求达标解。
//! 这与"给中小企业主的建议"高度契合：一位餐馆老板不需要理论最优定价，
//! 他需要一个"利润达标、最差情形可承受"的方案。
//!
//! # 与 `scenario` 模式的关键区别
//!
//! - `scenario` 回答"这局博弈的均衡在哪"（`focal = ?`，让求解器找）
//! - `discover` 回答"在**我的**满意标准下，哪些策略达标"（用户给标准，系统筛选）
//!
//! 故本 crate 的输入是**抱负水平**（aspiration levels），而非收益函数。
//!
//! # Simon 抱负水平动态调整（§5.6.2 步骤 4）
//!
//! 满意集为空时**不是**简单地告诉用户"无解"——那对决策者毫无帮助。
//! Simon 的核心洞见是抱负水平本身会随经验调整。故本 crate 在满意集为空时
//! **反馈冲突所在**并提出放宽方案，让用户看到"差在哪里、放宽多少可达标"。
//!
//! # LLM 的分工（铁律 1）
//!
//! 步骤 1 的"LLM 依对手档案提议创造性候选"——LLM **只提议，代码评估**。
//! 提议经 [`Candidate`] 注入；全部筛选、排序、稳健性计算在确定性代码中完成。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use shangyi_consequence::Percentiles;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum DiscoverError {
    #[error("候选集为空——无可评估对象")]
    NoCandidates,
    #[error("抱负水平阈值须 ∈ [0,1]，实得 {0}")]
    InvalidThreshold(f64),
    #[error("准则 `{0}` 的抱负水平为负——达标标准不应为负值")]
    NegativeAspiration(String),
    #[error("Monte Carlo 样本数须 > 0")]
    ZeroSamples,
    #[error("代价参数 `{0}` 的取值非法：{1}")]
    InvalidCostParam(String, String),
}

/// 一个候选策略（由剧本模板 × 参数网格生成，或由 LLM 提议）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    /// 标识（进报告）。
    pub id: String,
    /// 人类可读描述。
    pub description: String,
    /// 该候选在各准则上的**后果分布**（由 §5.6.1 管线预测得出）。
    ///
    /// 键为准则名（如 `利润`、`份额`、`现金流`）。
    pub outcomes: BTreeMap<String, Percentiles>,
    /// 该候选是否为 LLM 提议（进报告时区分"模板生成"与"创造性命中"）。
    pub llm_proposed: bool,
}

/// 抱负水平：各准则的达标标准。
///
/// **语义**：`利润 → 100.0` 表示"我希望利润 ≥ 100"。
/// `达标概率` = 该候选在该准则上超过抱负水平的概率（由后果分布插值估计）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Aspiration {
    /// 准则 → 抱负水平（下限）。
    pub levels: BTreeMap<String, f64>,
    /// 各准则的达标概率阈值（默认空表 ⇒ 全用 `default_threshold`）。
    ///
    /// `#[serde(default)]`：`Aspiration::new()` 本来就把它初始化成空表，
    /// 故"不写这个字段"与"写一个空表"是**同一个意思**。要求调用方显式写出
    /// 一个空表是纯粹的负担——而实测代价是：模型连试 3 次
    /// （`missing field levels` → `missing field thresholds`），
    /// 每次只看到"缺某个字段"，而它真正缺的是"哪些字段可以不写"。
    #[serde(default)]
    pub thresholds: BTreeMap<String, f64>,
    /// 未单独指定阈值的准则使用的默认阈值。
    ///
    /// `#[serde(default = ...)]`：`Aspiration::new()` 里就是
    /// `DEFAULT_ACHIEVEMENT_THRESHOLD`，故省略与显式给默认值是同一件事。
    /// 用 `default = "…"` 而非 `default`：后者的默认值是 `0.0`，
    /// 而那会让**所有准则**都"达不到"（阈值为 0 意味着要求 100% 达标概率）
    /// ——一个静默改变语义的默认值比缺字段更坏。
    #[serde(default = "default_threshold_value")]
    pub default_threshold: f64,
}

/// `default_threshold` 的 serde 默认值（转调常量，避免两处写同一个数）。
fn default_threshold_value() -> f64 {
    DEFAULT_ACHIEVEMENT_THRESHOLD
}

impl Aspiration {
    pub fn new(levels: BTreeMap<String, f64>) -> Result<Self, DiscoverError> {
        for (k, &v) in &levels {
            if !v.is_finite() {
                return Err(DiscoverError::NegativeAspiration(k.clone()));
            }
            if v < 0.0 {
                return Err(DiscoverError::NegativeAspiration(k.clone()));
            }
        }
        Ok(Self {
            levels,
            thresholds: BTreeMap::new(),
            default_threshold: DEFAULT_ACHIEVEMENT_THRESHOLD,
        })
    }

    pub fn threshold_for(&self, criterion: &str) -> f64 {
        self.thresholds
            .get(criterion)
            .copied()
            .unwrap_or(self.default_threshold)
    }

    /// 校验全部阈值合法。
    pub fn validate(&self) -> Result<(), DiscoverError> {
        for t in self.thresholds.values().chain([&self.default_threshold]) {
            if !t.is_finite() || !(0.0..=1.0).contains(t) {
                return Err(DiscoverError::InvalidThreshold(*t));
            }
        }
        Ok(())
    }
}

/// 设计文档 §5.6.2 的默认达标概率阈值。
pub const DEFAULT_ACHIEVEMENT_THRESHOLD: f64 = 0.6;

/// 一位候选在单个准则上的达标情况。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CriterionResult {
    pub criterion: String,
    /// 抱负水平。
    pub aspiration: f64,
    /// 达标概率估计 `P(结果 ≥ 抱负水平)`。
    pub achieved_probability: f64,
    /// 该准则的阈值。
    pub threshold: f64,
    pub met: bool,
    /// 该候选的 P10（最差情形）——报告需展示。
    pub p10: f64,
    pub p50: f64,
    pub p90: f64,
}

/// 一位候选的完整评估。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateEvaluation {
    pub candidate: Candidate,
    pub criteria: Vec<CriterionResult>,
    /// 全部准则均达标才算满意。
    pub satisfied: bool,
    /// 未达标的准则名（用于步骤 4 的冲突反馈）。
    pub unmet: Vec<String>,
    /// 稳健满意度：距"结论翻转"的安全余量（见 [`robustness_margin`]）。
    pub robustness: f64,
}

/// `P(结果 ≥ 抱负水平)` 的估计。
///
/// # 方法（分段线性，与 P10/P50/P90 三点的插值一致）
///
/// 只有三个分位数（P10/P50/P90）可用，故用分段线性 CDF 近似：
///
/// - `抱负 ≤ P10` ⇒ 概率 ≥ 0.9 ⇒ 取 0.9
/// - `P10 < 抱负 ≤ P50` ⇒ 在 (P10, 0.9) 与 (P50, 0.5) 之间线性插值
/// - `P50 < 抱负 ≤ P90` ⇒ 在 (P50, 0.5) 与 (P90, 0.1) 之间线性插值
/// - `抱负 > P90` ⇒ 概率 < 0.1 ⇒ 取 0.1
///
/// 端点截断在 0.9 / 0.1 而非 1.0 / 0.0：三点分位数无法确定尾部，
/// 报告 1.0 会把"信息不足"伪装成"确定"——这是不诚实的。
///
/// **诚实标注**：本式是 `[推断]` 式近似。若有更细的样本（如 Monte Carlo 原始样本），
/// 应直接数频率。本 crate 接受三点的原因是它是 `ConsequenceReport` 的既有输出形态，
/// 避免要求上游额外传全样本。
pub fn achievement_probability(p: &Percentiles, aspiration: f64) -> f64 {
    if !aspiration.is_finite() {
        return 0.0;
    }
    if aspiration <= p.p10 {
        return 0.9;
    }
    if aspiration <= p.p50 {
        let span = p.p50 - p.p10;
        if span <= f64::EPSILON {
            return 0.9;
        }
        let frac = (aspiration - p.p10) / span;
        return 0.9 - 0.4 * frac;
    }
    if aspiration <= p.p90 {
        let span = p.p90 - p.p50;
        if span <= f64::EPSILON {
            return 0.5;
        }
        let frac = (aspiration - p.p50) / span;
        return 0.5 - 0.4 * frac;
    }
    0.1
}

/// 稳健满意度：该策略距"结论翻转阈值"的安全余量，取值 `[0, 1]`。
///
/// # 定义
///
/// 对每个准则，先求**翻转点**（"达标概率恰好等于阈值"对应的抱负水平），
/// 再度量**抱负水平距翻转点还有多少缓冲**，以 `(翻转点 − P10)` 为归一化基准：
///
/// ```text
/// margin = (翻转点 − 抱负水平) / (翻转点 − P10)
/// ```
///
/// 语义（三条边界均有测试锁定）：
/// - 抱负水平 ≤ `P10` ⇒ 余量 = 1（即使用最差情形也达标，最安全）
/// - 抱负水平 = 翻转点 ⇒ 余量 = 0（恰好卡在边缘，一有不利波动就不达标）
/// - 抱负水平 > 翻转点 ⇒ 余量 = 0（已不达标）
///
/// 返回到全部准则的**最小**余量——最脆弱的那一环决定整体稳健性。
/// 这是 info-gap 稳健满意度思想的简化体现：稳健性由最坏情形决定，不由平均决定。
///
/// # ⚠️ 分辨率边界（已知且有意的限制）
///
/// 本度量度量的是"抱负水平距 `P10` 的相对位置"，故在 **抱负 ≤ P10** 时**饱和**为 `1.0`：
/// 抱负为 `P10` 与抱负远低于 `P10` 都得 `1.0`，**无法区分"够安全"与"极安全"**。
///
/// 这是信息不足的必然结果，不是实现缺陷：可用输入只有 `P10/P50/P90` 三点，
/// 抱负以下的尾部形状未知。要突破该边界需引入更低分位（如 `P01`），
/// 而 `ConsequenceReport` 当前不提供。
///
/// **刻意不做的"改进"**：用 `exp`/`sqrt` 之类拉伸把 `1.0` 展开成"有梯度"的值——
/// 那只会制造"看起来有分辨率"的假象，而分辨率并不存在。诚实的做法是保留饱和
/// 并声明其边界，让调用方知道何时该补更多分位数。
///
/// 该边界对排序的实际影响：能区分"贴边（≈0）"与"宽松（=1）"——
/// 这已覆盖排序的主要用途（用户需要知道"哪个方案最不冒险"）。
/// 若两个方案都饱和为 1.0，它们**同等安全**，此时按 id 排序以保证确定性。
///
/// # ⚠️ 一个已修正的实现缺陷（记录以免重犯）
///
/// 本函数最初写作 `(翻转点 − P10) / (P90 − P10)`——**完全不引用抱负水平**。
/// 后果：同一分布下无论抱负是 `10` 还是 `170`，余量恒为 `0.375`。
/// 这使"按稳健性排序"退化为按分布宽度排序，`discover` 的排名失去意义。
/// 该缺陷由测试 `robustness_is_positive_when_comfortably_satisfied` 暴露
/// （期望 > 0.8，实得 0.375）。
pub fn robustness_margin(evals: &[CriterionResult]) -> f64 {
    if evals.is_empty() {
        return 0.0;
    }
    let mut worst = f64::INFINITY;
    for c in evals {
        let flip = flip_point(c.p10, c.p50, c.p90, c.threshold);
        let denom = flip - c.p10;
        let margin = if denom <= f64::EPSILON {
            // 分布退化（P10 ≈ P50 ≈ P90）時，`denom` 无法作为归一化基准。
            // **不能**用"是否达标"给 0/1——达标 ≠ 有缓冲：抱负恰等于分布值时
            // 达标概率为 0.9（≥ 阈值）但缓冲实为 0，一旦结果略差即不达标。
            // 故改按 `flip` 与抱负的相对位置给保守值：
            //   翻转点高于抱负（尚有缓冲但幅度不可测）⇒ 0.5（保守，不夸大）
            //   翻转点不高于抱负（已无缓冲）⇒ 0.0
            if flip > c.aspiration {
                0.5
            } else {
                0.0
            }
        } else {
            ((flip - c.aspiration) / denom).clamp(0.0, 1.0)
        };
        worst = worst.min(margin);
    }
    if worst.is_finite() { worst } else { 0.0 }
}

/// 反解"达标概率 = 阈值"对应的抱负水平。
fn flip_point(p10: f64, p50: f64, p90: f64, threshold: f64) -> f64 {
    let t = threshold.clamp(0.0, 1.0);
    // 上半段：概率从 0.9 (在 P10) 降到 0.5 (在 P50)
    if t >= 0.5 {
        let frac = (0.9 - t) / 0.4; // t=0.9 ⇒ 0；t=0.5 ⇒ 1
        return p10 + frac.clamp(0.0, 1.0) * (p50 - p10);
    }
    // 下半段：概率从 0.5 (在 P50) 降到 0.1 (在 P90)
    let frac = (0.5 - t) / 0.4; // t=0.5 ⇒ 0；t=0.1 ⇒ 1
    p50 + frac.clamp(0.0, 1.0) * (p90 - p50)
}

/// 评估一位候选。
pub fn evaluate_candidate(
    candidate: &Candidate,
    aspiration: &Aspiration,
) -> Result<CandidateEvaluation, DiscoverError> {
    aspiration.validate()?;
    let mut criteria = Vec::new();
    let mut unmet = Vec::new();

    for (criterion, &level) in &aspiration.levels {
        let Some(p) = candidate.outcomes.get(criterion) else {
            // 该候选未在该准则上给出预测 ⇒ 按"未达标"处理并记入 unmet。
            // 不静默跳过：缺一个准则的预测意味着该候选无法被完整评估。
            unmet.push(criterion.clone());
            criteria.push(CriterionResult {
                criterion: criterion.clone(),
                aspiration: level,
                achieved_probability: 0.0,
                threshold: aspiration.threshold_for(criterion),
                met: false,
                p10: f64::NAN,
                p50: f64::NAN,
                p90: f64::NAN,
            });
            continue;
        };

        let prob = achievement_probability(p, level);
        let threshold = aspiration.threshold_for(criterion);
        let met = prob >= threshold;
        if !met {
            unmet.push(criterion.clone());
        }
        criteria.push(CriterionResult {
            criterion: criterion.clone(),
            aspiration: level,
            achieved_probability: prob,
            threshold,
            met,
            p10: p.p10,
            p50: p.p50,
            p90: p.p90,
        });
    }

    // 确定性排序（BTreeMap 已保证准则顺序，此处保持）
    let satisfied = !criteria.is_empty() && unmet.is_empty();
    let robustness = robustness_margin(&criteria);

    Ok(CandidateEvaluation {
        candidate: candidate.clone(),
        criteria,
        satisfied,
        unmet,
        robustness,
    })
}

/// 抱负水平放宽方案（§5.6.2 步骤 4）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelaxationProposal {
    /// 需要放宽的准则。
    pub criterion: String,
    /// 当前抱负水平。
    pub current: f64,
    /// 建议放宽到。
    pub suggested: f64,
    /// 放宽后能达到该阈值的候选数。
    pub candidates_meeting: usize,
    /// 人类可读说明（进报告，直接给用户看）。
    pub rationale: String,
}

/// 满意集为空时的冲突反馈与放宽建议。
///
/// # 为什么必须给建议而非只说"无解"
///
/// Simon 抱负水平理论的要义就是**抱负会随经验调整**。用户拿着"没有策略达标"
/// 的结论无法决策；给他"放宽利润底线 15% 后，有 3 个策略达标"才能推进。
///
/// # 建议的生成方式
///
/// 对每个**未被任何候选满足**的准则，找出"使候选数最大化的最小放宽"：
/// 从该准则上全部候选的达标概率分布中，取能达到最多候选的抱负水平
/// （即该准则上候选 P50 的下确界——低于它则半数以上候选可望达标）。
///
/// 该方法是 `[推断]` 式启发式：它保证"放宽后确有效果"（`candidates_meeting > 0`），
/// 但不保证是**最小**放宽量。若要严格最小化，需在抱负水平上做二分搜索——
/// 留作后续（当前 MVP 不需要该精度）。
pub fn propose_relaxation(
    evaluations: &[CandidateEvaluation],
    aspiration: &Aspiration,
) -> Vec<RelaxationProposal> {
    if evaluations.is_empty() {
        return Vec::new();
    }

    // 收集"没有任何候选达标"的准则
    let mut failed_criteria: Vec<(String, f64)> = Vec::new();
    for (criterion, &level) in &aspiration.levels {
        let any_met = evaluations.iter().any(|e| {
            e.criteria
                .iter()
                .any(|c| c.criterion == *criterion && c.met)
        });
        if !any_met {
            failed_criteria.push((criterion.clone(), level));
        }
    }

    let mut out = Vec::new();
    for (criterion, current) in failed_criteria {
        // 该准则上是否存在可用的后果分布（无分布则无法给建议）
        let has_distribution = evaluations.iter().any(|e| {
            e.candidate
                .outcomes
                .get(&criterion)
                .map(|p| p.p10.is_finite() && p.p50.is_finite() && p.p90.is_finite())
                .unwrap_or(false)
        });
        if !has_distribution {
            continue;
        }

        // 建议水位：取**各候选的翻转点**（"达标概率恰等于阈值"的抱负水平），
        // 取其中最小者——它恰是"让至少一个候选达标"的最低要求。
        //
        // ⚠️ 曾用候选的 **P50** 作扫描水位，但 P50 处的达标概率恒为 0.5，
        // 而阈值默认 0.6 ⇒ `count` 恒为 0 ⇒ 建议恒为空（实现缺陷，由测试暴露）。
        // 正确的水位应由 `flip_point` 反解得出，它本身就把阈值纳入计算。
        let threshold = aspiration.threshold_for(&criterion);
        let mut suggested = None;
        let mut best_count = 0usize;

        for e in evaluations {
            let Some(p) = e.candidate.outcomes.get(&criterion) else {
                continue;
            };
            if !p.p10.is_finite() || !p.p50.is_finite() || !p.p90.is_finite() {
                continue;
            }
            let cand_level = flip_point(p.p10, p.p50, p.p90, threshold);
            if cand_level >= current {
                continue;
            }
            let count = evaluations
                .iter()
                .filter(|x| {
                    x.candidate
                        .outcomes
                        .get(&criterion)
                        .map(|q| achievement_probability(q, cand_level) >= threshold)
                        .unwrap_or(false)
                })
                .count();
            // 首选"候选数最多"的水位；并列时取**更高**的水位（放宽幅度更小，对用户更友好）
            if count > best_count || (count == best_count && count > 0 && Some(cand_level) > suggested)
            {
                best_count = count;
                suggested = Some(cand_level);
            }
        }

        let Some(suggested) = suggested else {
            continue;
        };

        let pct = if current.abs() > f64::EPSILON {
            (current - suggested) / current * 100.0
        } else {
            0.0
        };
        out.push(RelaxationProposal {
            criterion: criterion.clone(),
            current,
            suggested,
            candidates_meeting: best_count,
            rationale: format!(
                "准则 `{criterion}` 当前抱负水平 {current}（达标概率 ≥ {threshold}）\
                 无任何候选满足。若放宽到 {suggested}（降低约 {pct:.1}%），\
                 则有 {best_count} 个候选可望达标。\
                 这是 Simon 抱负水平动态调整的落地：\n\
                 达标标准本身可以调整，而非只能接受「无解」。"
            ),
        });
    }
    out.sort_by(|a, b| a.criterion.cmp(&b.criterion));
    out
}

/// 帕累托前沿上的候选（互不支配）。
///
/// # 支配的定义
///
/// 候选 A 支配 B ⇔ 在**全部**准则上 A 的 P50 都不低于 B，且至少一个准则上严格更高。
///
/// **为何用 P50 而非期望**：P50 是中位数，对中小企业主更直观（"一半情况下能到多少"），
/// 且不要求分布对称。期望值对偏态分布会被极值拉动。
/// 若调用方需要期望口径，应显式传入——本 crate 不在内部做这种替换。
pub fn pareto_front(evaluations: &[CandidateEvaluation]) -> Vec<String> {
    let mut front = Vec::new();
    for (i, a) in evaluations.iter().enumerate() {
        let mut dominated = false;
        for (j, b) in evaluations.iter().enumerate() {
            if i == j {
                continue;
            }
            if dominates(b, a) {
                dominated = true;
                break;
            }
        }
        if !dominated {
            front.push(a.candidate.id.clone());
        }
    }
    front.sort();
    front
}

fn dominates(a: &CandidateEvaluation, b: &CandidateEvaluation) -> bool {
    let mut strictly_better = false;
    for (criterion, pa) in &a.candidate.outcomes {
        let Some(pb) = b.candidate.outcomes.get(criterion) else {
            // b 缺该准则的预测 ⇒ a 在该准则上"不劣"（保守：不让缺失变成优势）
            continue;
        };
        if pa.p50 < pb.p50 {
            return false; // a 在某准则上更差 ⇒ 不支配
        }
        if pa.p50 > pb.p50 {
            strictly_better = true;
        }
    }
    strictly_better
}

/// 满意策略发现报告（`DiscoveryReport`）——含 ≤5 个满意策略 + 帕累托图数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryReport {
    /// 满意策略（≤5 个，按稳健满意度降序）。
    pub satisfied: Vec<CandidateEvaluation>,
    /// 帕累托前沿上的候选 id（供绘图）。
    pub pareto_front: Vec<String>,
    /// 抱负水平放宽建议（满意集为空时非空）。
    pub relaxation: Vec<RelaxationProposal>,
    /// 报告层说明。
    pub note: String,
    /// 是否找到了满意策略。
    pub has_satisfied: bool,
}

/// 满意策略清单上限（设计文档 §5.6.2：≤5 个）。
pub const MAX_SATISFIED: usize = 5;

/// 执行满意策略发现。
///
/// 流程对应 §5.6.2 的步骤 3–5：
/// - 步骤 3 满意过滤：各准则达标概率 ≥ 阈值
/// - 步骤 4 满意集为空 ⇒ 生成放宽建议
/// - 步骤 5 排序：帕累托前沿 + 稳健满意度
pub fn discover(
    candidates: &[Candidate],
    aspiration: &Aspiration,
) -> Result<DiscoveryReport, DiscoverError> {
    if candidates.is_empty() {
        return Err(DiscoverError::NoCandidates);
    }
    aspiration.validate()?;

    let mut evaluations: Vec<CandidateEvaluation> = candidates
        .iter()
        .map(|c| evaluate_candidate(c, aspiration))
        .collect::<Result<_, _>>()?;

    let satisfied: Vec<CandidateEvaluation> = evaluations
        .iter()
        .filter(|e| e.satisfied)
        .cloned()
        .collect();

    let pareto = pareto_front(&evaluations);
    let relaxation = if satisfied.is_empty() {
        propose_relaxation(&evaluations, aspiration)
    } else {
        Vec::new()
    };

    // 步骤 5 排序：稳健满意度降序；同稳健性按 id 升序（确定性）
    let mut ranked = satisfied;
    ranked.sort_by(|a, b| {
        b.robustness
            .partial_cmp(&a.robustness)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.id.cmp(&b.candidate.id))
    });
    let truncated = ranked.len() > MAX_SATISFIED;
    ranked.truncate(MAX_SATISFIED);

    let has_satisfied = !ranked.is_empty();
    let note = if has_satisfied {
        format!(
            "在给定抱负水平下找到 {} 个满意策略{}。排序依据为稳健满意度\
             （距「结论翻转阈值」的安全余量，由最脆弱的准则决定）。",
            ranked.len(),
            if truncated { "（已按上限截取）" } else { "" }
        )
    } else if relaxation.is_empty() {
        "在给定抱负水平下无策略达标，且无法给出放宽建议——\
         请检查后果预测是否覆盖了全部准则。"
            .to_string()
    } else {
        format!(
            "在给定抱负水平下无策略达标。已给出 {} 项抱负水平放宽建议\
             （Simon 抱负水平动态调整：达标标准本身可调）。",
            relaxation.len()
        )
    };

    // 避免未使用告警：evaluations 已用于筛选与帕累托
    let _ = evaluations.len();

    Ok(DiscoveryReport {
        satisfied: ranked,
        pareto_front: pareto,
        relaxation,
        note,
        has_satisfied,
    })
}

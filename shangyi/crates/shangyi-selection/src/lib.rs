//! 商奕均衡选择融合与回测校准（设计文档 §5.4）。
//!
//! # 本 crate 解决什么问题
//!
//! 经典博弈论在多均衡博弈面前只能给出"都是均衡"，预测力归零（Folk 定理保证重复博弈
//! 均衡无穷多）。本 crate 的职责是**给每个候选均衡赋出现概率**，并把"哪个判据贡献了多少"
//! 摊开可审计。
//!
//! # 两条铁律在此处的落点
//!
//! - **铁律 1（LLM 不算数）**：全部融合与校准是确定性代码。需要 LLM 的两个判据
//!   （`focal_llm`、`persona_sim`）**不在本 crate 内调用 LLM**，而是经
//!   [`SelectorScore`] 注入——它们的输出与解析判据一样只是"一个带权输入"，
//!   且权重受回测约束。
//! - **铁律 2（关键假设过 checkpoint）**：判据权重初值来自剧本先验，未校准前
//!   必须在 `confidence` 中**如实标注**，不得让读者误以为概率已校准。
//!
//! # 与 `shangyi-solver` 的分工
//!
//! `shangyi-solver` 负责**求均衡**（支持枚举/精炼）与**单判据的方向判断**
//! （风险占优、随机稳定）。本 crate 负责**多判据融合**与**回测校准**——
//! 即把多个判据的结论合成一个概率分布，并用历史案例更新各判据的可信度。

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// 判据标识。与设计文档 §5.4 的内置选择器表一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorId {
    /// 风险占优（Harsanyi–Selten）。
    RiskDom,
    /// 演化动态 + 随机稳定（KMR/Young）。
    Stochastic,
    /// 虚拟对局 / EWA。
    Learning,
    /// QRE + level-k（对手有限理性）。
    Behavioral,
    /// 聚点判断（Schelling）——需 LLM，经适配器注入。
    FocalLlm,
    /// LLM persona 多智能体 rollout——需 LLM，经适配器注入。
    PersonaSim,
}

impl SelectorId {
    /// 全部判据（用于遍历与报告）。
    pub const ALL: [SelectorId; 6] = [
        SelectorId::RiskDom,
        SelectorId::Stochastic,
        SelectorId::Learning,
        SelectorId::Behavioral,
        SelectorId::FocalLlm,
        SelectorId::PersonaSim,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            SelectorId::RiskDom => "risk_dom",
            SelectorId::Stochastic => "stochastic",
            SelectorId::Learning => "learning",
            SelectorId::Behavioral => "behavioral",
            SelectorId::FocalLlm => "focal_llm",
            SelectorId::PersonaSim => "persona_sim",
        }
    }

    /// 该判据是否依赖 LLM。
    ///
    /// **用途**：报告与审计需要区分"确定性判据"与"LLM 判据"——
    /// 前者的权重下限可以更低（它们不会幻觉），后者的权重必须受回测约束。
    pub fn needs_llm(self) -> bool {
        matches!(self, SelectorId::FocalLlm | SelectorId::PersonaSim)
    }
}

impl fmt::Display for SelectorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 单个判据对单个均衡的评分。
///
/// 字段与设计文档 §5.4 的 `SelectorScore` 对应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectorScore {
    /// 判据标识。
    pub selector: SelectorId,
    /// 均衡标识（用于与求解器输出对齐）。
    pub equilibrium_id: String,
    /// 该判据认为此均衡出现的**相对量**，须为有限且非负。
    ///
    /// ⚠️ 取值**不限于** `[0, 1]`：允许原始计数（如 `persona_sim` 的 rollout 落点
    /// 次数 `30 / 70`）或任意非负标量。[`normalize_selector`] 会把它归一到和为 1。
    ///
    /// 之所以不在此处强制 `[0, 1]`：不同判据的原始输出量纲不同（风险占优给方向判断、
    /// `persona_sim` 给落点频次），若强制区间就得让每个调用方各自预处理——
    /// 那会把"归一化"这一件事分散到 N 个地方，反而更容易出错。
    /// 归一化由**唯一的** [`normalize_selector`] 负责。
    pub weight: f64,
    /// 人类可读理由——**进报告**。
    pub rationale: String,
    /// 机器可审计证据（如收敛轨迹摘要、逐对比较数值）。
    pub evidence: BTreeMap<String, String>,
}

impl SelectorScore {
    pub fn new(
        selector: SelectorId,
        equilibrium_id: impl Into<String>,
        weight: f64,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            selector,
            equilibrium_id: equilibrium_id.into(),
            weight,
            rationale: rationale.into(),
            evidence: BTreeMap::new(),
        }
    }

    pub fn with_evidence(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.evidence.insert(key.into(), value.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SelectionError {
    #[error("判据 {0} 的评分含非有限或负权重（须为有限且 ≥ 0）")]
    InvalidWeight(SelectorId),
    #[error("候选均衡列表为空——无可选择的对象")]
    NoCandidates,
    #[error("判据 {0} 对该输入不适用（不得静默按 0 参与融合）")]
    NotApplicable(SelectorId),
    #[error("判据权重表中缺少 {0}——未配置即视为不可用，不得用隐式默认值")]
    MissingWeight(SelectorId),
    #[error("全部判据权重为 0，无法归一化")]
    AllWeightsZero,
    #[error(
        "判据 {selector} 提到的均衡 `{equilibrium_id}` 不在候选集中（候选：{candidates:?}）——\
         判据输入与求解结果不一致；不得静默丢弃该评分"
    )]
    UnknownEquilibrium {
        selector: SelectorId,
        equilibrium_id: String,
        candidates: Vec<String>,
    },
    #[error("回测样本的预测分布长度（{predicted}）与实现均衡下标（{realized}）不匹配")]
    CalibrationShape { predicted: usize, realized: usize },
    #[error("回测样本的预测分布未归一化（和为 {sum}）")]
    CalibrationNotNormalized { sum: f64 },
}

/// 判据权重表：`w_k`。初值来自剧本先验配置。
///
/// **为何是显式表而非默认值**：设计文档 §2.4 的"整行替换"原则要求配置可检视。
/// 刻意**不**实现 `Default`——缺权重即报 [`SelectionError::MissingWeight`]，
/// 避免"忘了配就用了隐式默认值"这类不可审计的情形。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectorWeights {
    weights: BTreeMap<SelectorId, f64>,
}

impl SelectorWeights {
    /// 从剧本先验构造。权重须非负且非 NaN。
    pub fn new(weights: BTreeMap<SelectorId, f64>) -> Result<Self, SelectionError> {
        for (&id, &w) in &weights {
            if !w.is_finite() || w < 0.0 {
                return Err(SelectionError::InvalidWeight(id));
            }
        }
        Ok(Self { weights })
    }

    /// 等权配置（用于"判据无先验"时的诚实基线，报告中须标注未校准）。
    pub fn uniform(ids: &[SelectorId]) -> Self {
        let w = if ids.is_empty() {
            0.0
        } else {
            1.0 / ids.len() as f64
        };
        Self {
            weights: ids.iter().map(|&id| (id, w)).collect(),
        }
    }

    pub fn get(&self, id: SelectorId) -> Option<f64> {
        self.weights.get(&id).copied()
    }

    /// 逐判据取权重；缺失即报错（不静默跳过）。
    pub fn require(&self, id: SelectorId) -> Result<f64, SelectionError> {
        self.get(id).ok_or(SelectionError::MissingWeight(id))
    }

    pub fn iter(&self) -> impl Iterator<Item = (SelectorId, f64)> + '_ {
        self.weights.iter().map(|(&k, &v)| (k, v))
    }

    pub fn ids(&self) -> Vec<SelectorId> {
        self.weights.keys().copied().collect()
    }
}

/// 把某判据对全部均衡的权重归一化到和为 1。
///
/// **为什么要显式归一化**：不同判据的原始输出量纲不同（风险占优给 0/1 方向判断，
/// `persona_sim` 给 rollout 落点频率）。若不归一化，量纲大的判据会支配融合结果——
/// 那是实现缺陷而非算法意图。
pub fn normalize_selector(scores: &mut [SelectorScore]) -> Result<(), SelectionError> {
    if scores.is_empty() {
        return Ok(());
    }
    let selector = scores[0].selector;
    let mut sum = 0.0;
    for s in scores.iter() {
        // 只要求"有限且非负"——允许原始计数（见 SelectorScore::weight 的说明）
        if !s.weight.is_finite() || s.weight < 0.0 {
            return Err(SelectionError::InvalidWeight(s.selector));
        }
        sum += s.weight;
    }
    if sum <= 0.0 {
        // 全 0 是该判据"无信息"的诚实表达；保持全 0，由融合阶段自然降权。
        return Ok(());
    }
    for s in scores.iter_mut() {
        debug_assert_eq!(s.selector, selector);
        s.weight /= sum;
    }
    Ok(())
}

/// 融合结果的一项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributionEntry {
    pub equilibrium_id: String,
    /// 该均衡的出现概率，全部项和为 1。
    pub prob: f64,
    /// 主要驱动判据（按贡献降序，只列贡献 > 0 的）。
    ///
    /// 对应设计文档输出格式里的 `"drivers": ["stochastic", "behavioral"]` ——
    /// **这是报告可解释性的关键**：老板页要能说"为什么认为价格战概率最高"。
    pub drivers: Vec<SelectorId>,
}

/// 置信度分级。
///
/// 设计文档 §12 要求「案例库冷启动无回测」时**诚实标注**，不得让读者
/// 误以为概率已校准。故置信度是**类型**而非自由文本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Confidence {
    /// 判据权重为未校准先验（案例库样本不足）。
    PriorUncalibrated {
        /// 已积累的回测样本数
        samples: usize,
        /// 达到"可校准"所需的样本数下限
        needed: usize,
    },
    /// 已用案例库校准，附样本数与平均对数损失。
    Calibrated {
        samples: usize,
        mean_log_loss: f64,
        /// 参与校准的判据数
        selectors: usize,
    },
}

impl Confidence {
    /// 供报告直接引用的中文说明。
    pub fn note(&self) -> String {
        match self {
            Confidence::PriorUncalibrated { samples, needed } => format!(
                "低—中（先验未校准：已有 {samples} 个回测样本，需 ≥ {needed} 个方可校准）"
            ),
            Confidence::Calibrated {
                samples,
                mean_log_loss,
                selectors,
            } => format!(
                "已校准（{samples} 个样本，{selectors} 个判据，平均对数损失 {mean_log_loss:.3}）"
            ),
        }
    }

    pub fn is_calibrated(&self) -> bool {
        matches!(self, Confidence::Calibrated { .. })
    }
}

/// 融合后的均衡概率分布——**进报告的核心数字**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EquilibriumDistribution {
    pub entries: Vec<DistributionEntry>,
    pub confidence: Confidence,
    /// 判据分歧告警（设计文档 §5.4：`persona_sim` 与解析法差异过大时自动告警）。
    pub divergences: Vec<String>,
}

impl EquilibriumDistribution {
    /// 概率最高的均衡（并列时返回第一个，`ties` 指出是否并列）。
    pub fn most_likely(&self) -> Option<(&DistributionEntry, bool)> {
        let top = self.entries.iter().reduce(|a, b| {
            if b.prob > a.prob {
                b
            } else {
                a
            }
        })?;
        let ties = self
            .entries
            .iter()
            .filter(|e| (e.prob - top.prob).abs() < f64::EPSILON)
            .count()
            > 1;
        Some((top, ties))
    }
}

/// 判据分歧阈值：某判据的最大权重与融合结果的最大概率之差超过该值即告警。
///
/// 取值依据：0.35 意味着"某判据单独看认为 A 最可能，但融合后 A 的概率比最高项低 0.35 以上"。
/// 这是**启发式阈值**，其合理区间应由回测确定；当前值属 `[推断]`，登记为待校准项。
pub const DIVERGENCE_THRESHOLD: f64 = 0.35;

/// 融合：`P(e) = Σ_k w_k · weight_k(e)`，再对 `e` 归一化。
///
/// # 参数
/// - `equilibrium_ids`：候选均衡标识（顺序即输出顺序，保证确定性）
/// - `scores_by_selector`：逐判据的评分。**缺失某判据不报错**（视为该判据未参与，
///   例如脚本未配置 `learning`）；但若判据出现在表中而其权重未配置，则报
///   [`SelectionError::MissingWeight`]。
/// - `weights`：判据权重 `w_k`
/// - `confidence`：置信度（由调用方依据案例库样本量给出）
pub fn fuse(
    equilibrium_ids: &[String],
    scores_by_selector: &BTreeMap<SelectorId, Vec<SelectorScore>>,
    weights: &SelectorWeights,
    confidence: Confidence,
) -> Result<EquilibriumDistribution, SelectionError> {
    if equilibrium_ids.is_empty() {
        return Err(SelectionError::NoCandidates);
    }

    // 逐均衡累计加权贡献；同时记录每个判据的贡献以便导出 drivers
    let mut acc: Vec<f64> = vec![0.0; equilibrium_ids.len()];
    let mut contributions: Vec<BTreeMap<SelectorId, f64>> =
        vec![BTreeMap::new(); equilibrium_ids.len()];

    for (&selector, scores) in scores_by_selector {
        let w = weights.require(selector)?;
        // 该判据的权重为 0 ⇒ 不参与（不是错误：剧本可能显式关闭某判据）
        if w == 0.0 {
            continue;
        }

        // 归一化该判据自身的权重（不修改入参）
        let mut normalized = scores.clone();
        normalize_selector(&mut normalized)?;

        for s in &normalized {
            let Some(idx) = equilibrium_ids.iter().position(|id| *id == s.equilibrium_id) else {
                // 判据提到的均衡不在候选集里 ⇒ 判据输入与求解结果不一致。
                //
                // **必须报错而非跳过**：跳过等于悄悄丢掉该判据的一部分评分，
                // 而融合结果仍是"看起来正常"的概率分布——那就是不可审计的静默降级
                //（不变量②）。且它在报告里表现为"某判据参与过"，实际它的一半输入
                // 被丢弃，读者无从察觉。
                return Err(SelectionError::UnknownEquilibrium {
                    selector,
                    equilibrium_id: s.equilibrium_id.clone(),
                    candidates: equilibrium_ids.to_vec(),
                });
            };
            let contrib = w * s.weight;
            acc[idx] += contrib;
            if contrib > 0.0 {
                *contributions[idx].entry(selector).or_insert(0.0) += contrib;
            }
        }
    }

    let total: f64 = acc.iter().sum();
    if total <= 0.0 {
        return Err(SelectionError::AllWeightsZero);
    }

    let mut entries: Vec<DistributionEntry> = equilibrium_ids
        .iter()
        .enumerate()
        .map(|(i, id)| {
            // drivers 按贡献降序；只列贡献 > 0 者
            let mut drivers: Vec<(SelectorId, f64)> =
                contributions[i].iter().map(|(&k, &v)| (k, v)).collect();
            drivers.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            DistributionEntry {
                equilibrium_id: id.clone(),
                prob: acc[i] / total,
                drivers: drivers.into_iter().map(|(k, _)| k).collect(),
            }
        })
        .collect();

    // 确定性排序：概率降序，同概率按 id 升序（避免输出顺序随 HashMap 迭代变化）
    entries.sort_by(|a, b| {
        b.prob
            .partial_cmp(&a.prob)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.equilibrium_id.cmp(&b.equilibrium_id))
    });

    let divergences = detect_divergences(scores_by_selector, &entries);

    Ok(EquilibriumDistribution {
        entries,
        confidence,
        divergences,
    })
}

/// 检测"某判据的偏好与融合结果显著不一致"。
///
/// 设计文档 §5.4 要求 `persona_sim` 与解析法差异过大时告警——
/// 提示模型假设或参与者档案可能有误。本函数对所有判据做同样的检查（不止 persona_sim），
/// 因为任何判据与融合结果严重背离都值得人工看一眼。
fn detect_divergences(
    scores_by_selector: &BTreeMap<SelectorId, Vec<SelectorScore>>,
    fused: &[DistributionEntry],
) -> Vec<String> {
    let mut out = Vec::new();
    let fused_top = fused.first().map(|e| e.prob).unwrap_or(0.0);

    for (&selector, scores) in scores_by_selector {
        if scores.is_empty() {
            continue;
        }
        // 该判据最看好的均衡
        let best = scores.iter().reduce(|a, b| {
            if b.weight > a.weight {
                b
            } else {
                a
            }
        });
        let Some(best) = best else { continue };

        let fused_prob_of_that = fused
            .iter()
            .find(|e| e.equilibrium_id == best.equilibrium_id)
            .map(|e| e.prob);

        if let Some(p) = fused_prob_of_that {
            if fused_top - p > DIVERGENCE_THRESHOLD {
                out.push(format!(
                    "判据 `{selector}` 最看好 `{}`（自身权重 {:.2}），但融合后其概率仅 {:.2}，\
                     比最高项低 {:.2}（阈值 {DIVERGENCE_THRESHOLD}）——\
                     请检查博弈模型假设或参与者档案是否有误",
                    best.equilibrium_id,
                    best.weight,
                    p,
                    fused_top - p
                ));
            }
        } else {
            // 判据偏好的均衡不在候选集 ⇒ 更严重的信号
            out.push(format!(
                "判据 `{selector}` 偏好 `{}`，但该均衡不在候选集中——判据与求解结果不一致",
                best.equilibrium_id
            ));
        }
    }
    out.sort();
    out
}

// ────────────────────────────────────────────────────────────────────────────
// 回测校准
// ────────────────────────────────────────────────────────────────────────────

/// 一条回测记录：当时的预测分布 vs 事后实际实现的均衡。
///
/// 对应设计文档 §7.2 案例库的 `selection_output` + `outcome.equilibrium_realized`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestRecord {
    /// 预测时各候选均衡的概率（顺序与 `equilibrium_ids` 对齐）。
    pub predicted: Vec<f64>,
    /// 实际实现的均衡下标（`predicted` 的下标）。
    pub realized: usize,
    /// 各判据在该案例中给"实际实现的均衡"的归一化权重。
    ///
    /// 校准的核心：**实际发生的结果上，哪个判据给的分更高，哪个判据就该加权**。
    pub selector_weights: BTreeMap<SelectorId, f64>,
}

/// 多分类对数损失：`−ln(p_realized)`。
///
/// 为何用对数损失而非准确率：概率预测的质量体现在**分布**上，
/// 准确率只看"最高项是否命中"，会奖励过度自信（把 0.9 压成 0.99 但方向对）。
/// 对数损失对过度自信有惩罚，是概率校准的标准度量。
pub fn log_loss(predicted: &[f64], realized: usize) -> Result<f64, SelectionError> {
    if realized >= predicted.len() {
        return Err(SelectionError::CalibrationShape {
            predicted: predicted.len(),
            realized,
        });
    }
    let sum: f64 = predicted.iter().sum();
    if (sum - 1.0).abs() > 1e-6 {
        return Err(SelectionError::CalibrationNotNormalized { sum });
    }
    let p = predicted[realized].max(1e-12); // 防止 ln(0)
    Ok(-p.ln())
}

/// 校准结果。
#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationOutcome {
    /// 更新后的判据权重（已归一化到和为 1）。
    pub updated: SelectorWeights,
    /// 更新前后的平均对数损失（用于验证"越用越准"而非仅仅是"变了"）。
    pub mean_log_loss_before: f64,
    pub mean_log_loss_after: f64,
    /// 每个判据权重的前后变化（进报告，可审计）。
    pub weight_deltas: BTreeMap<SelectorId, (f64, f64)>,
    /// 样本数。
    pub samples: usize,
}

/// 学习率：每次校准的步长上限。
///
/// 取值偏小（0.05）的理由：回测样本通常很少（冷启动期个位数），
/// 大步长会让权重被少数案例主导，反而降低泛化。属**保守**选择。
/// 该值与 [`MIN_SAMPLES_FOR_CALIBRATION`] 均登记为待回测确认项。
pub const CALIBRATION_LEARNING_RATE: f64 = 0.05;

/// 达到"可校准"所需的样本数下限。
///
/// 依据：设计文档 §12 明确"案例库冷启动无回测"时须诚实标注置信度。
/// 样本数低于该值时，任何权重更新都易过拟合，故**不做校准**、
/// 置信度维持 [`Confidence::PriorUncalibrated`]。
pub const MIN_SAMPLES_FOR_CALIBRATION: usize = 30;

/// 用回测样本校准判据权重。
///
/// # 算法
///
/// 对每条记录：判据 `k` 在"实际实现的均衡"上给的权重越高，说明该判据越该被信任。
/// 故按指数加权更新：
///
/// ```text
/// w_k ← w_k · exp(η · (weight_k(realized) − 1/|候选|))
/// ```
///
/// 其中 `1/|候选|` 是"无信息"基线（均匀分布下的权重）。判据表现好于随机则加权，
/// 差于随机则减权。最后归一化。
///
/// # 为什么不是"直接数命中次数"
///
/// 只看"最高项是否命中"会丢掉信息：某判据给实际结果 0.4（最高 0.45 在别处）
/// 与给 0.01，两者都"没命中"，但可信度差别很大。用**给实际结果的权重**做信号
/// 能利用这一差别，与对数损失的取向一致。
///
/// # 诚实的限制
///
/// - 若样本数 < [`MIN_SAMPLES_FOR_CALIBRATION`]，**返回未校准的原权重**并在
///   `CalibrationOutcome` 中如实反映（`samples` 小、delta 为 0），不假装校准过。
/// - 校准只调权重，**不**改变判据本身的算法。若某判据系统性错误（如实现有 bug），
///   校准只会把它权重压低而非修正它——那是求解器/判据层的职责。
pub fn calibrate(
    weights: &SelectorWeights,
    records: &[BacktestRecord],
) -> Result<CalibrationOutcome, SelectionError> {
    let n_candidates = records
        .first()
        .map(|r| r.predicted.len())
        .ok_or(SelectionError::NoCandidates)?;

    // 更新前基线
    let mean_before = mean_log_loss(records)?;

    // 样本不足 ⇒ 不做校准（诚实：不假装"越用越准"）
    if records.len() < MIN_SAMPLES_FOR_CALIBRATION {
        return Ok(CalibrationOutcome {
            updated: weights.clone(),
            mean_log_loss_before: mean_before,
            mean_log_loss_after: mean_before,
            weight_deltas: weights
                .iter()
                .map(|(id, w)| (id, (w, w)))
                .collect(),
            samples: records.len(),
        });
    }

    let baseline = 1.0 / n_candidates as f64;
    let mut log_w: BTreeMap<SelectorId, f64> = weights
        .iter()
        .map(|(id, w)| (id, if w > 0.0 { w.ln() } else { f64::NEG_INFINITY }))
        .collect();

    for rec in records {
        for (&id, &w_k) in &rec.selector_weights {
            // 只更新出现在权重表里的判据；未配置的判据不因回测被"凭空启用"
            let Some(slot) = log_w.get_mut(&id) else {
                continue;
            };
            if !slot.is_finite() {
                continue;
            }
            let signal = w_k - baseline;
            *slot += CALIBRATION_LEARNING_RATE * signal;
        }
    }

    // 归一化（softmax 风格：减去最大值以保证数值稳定）
    let max = log_w
        .values()
        .copied()
        .filter(|v| v.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    let mut exp_sum = 0.0;
    let mut normalized: BTreeMap<SelectorId, f64> = BTreeMap::new();
    for (&id, &lw) in &log_w {
        let v = if lw.is_finite() {
            (lw - max).exp()
        } else {
            0.0
        };
        normalized.insert(id, v);
        exp_sum += v;
    }
    if exp_sum <= 0.0 {
        return Err(SelectionError::AllWeightsZero);
    }
    for v in normalized.values_mut() {
        *v /= exp_sum;
    }

    let updated = SelectorWeights::new(normalized.clone())?;
    let deltas: BTreeMap<SelectorId, (f64, f64)> = weights
        .iter()
        .map(|(id, before)| {
            let after = normalized.get(&id).copied().unwrap_or(0.0);
            (id, (before, after))
        })
        .collect();

    Ok(CalibrationOutcome {
        updated,
        mean_log_loss_before: mean_before,
        mean_log_loss_after: mean_before, // 由调用方在下一轮实测后回填；见测试
        weight_deltas: deltas,
        samples: records.len(),
    })
}

fn mean_log_loss(records: &[BacktestRecord]) -> Result<f64, SelectionError> {
    if records.is_empty() {
        return Ok(f64::NAN);
    }
    let mut sum = 0.0;
    for r in records {
        sum += log_loss(&r.predicted, r.realized)?;
    }
    Ok(sum / records.len() as f64)
}

/// 由案例库样本量决定置信度。**冷启动期如实标注未校准**（设计文档 §12）。
pub fn confidence_for(samples: usize, selectors: usize, mean_log_loss: f64) -> Confidence {
    if samples < MIN_SAMPLES_FOR_CALIBRATION {
        Confidence::PriorUncalibrated {
            samples,
            needed: MIN_SAMPLES_FOR_CALIBRATION,
        }
    } else {
        Confidence::Calibrated {
            samples,
            mean_log_loss,
            selectors,
        }
    }
}

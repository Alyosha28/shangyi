//! 商奕理论库与案例库（设计文档 §7）。
//!
//! # 唯一真相源原则（Harness §13.8）
//!
//! **markdown 文件（含 frontmatter）是唯一真相源**；矢量索引是**派生投影**
//! （可重建、可丢弃）。理由：索引会因 embedding 模型换代而重建，不能承载真相。
//!
//! 故本 crate 只处理 markdown 与其 frontmatter，**不依赖任何向量库**——
//! 检索由进程隔离的 Python 侧车做，索引可随时从本 crate 解析出的条目重建。
//!
//! # 案例库是回测闭环的核心资产（§7.2）
//!
//! 每次咨询的**预测必须留档**（`selection_output`），日后回填实际结果
//! （`outcome`）方可校准选择判据权重。这是"越用越准"的数据基础。
//!
//! 关键设计：`CaseRecord` 区分"已预测未回填"与"已回填"两种状态——
//! **未回填的案例不参与校准**（否则会拿没有答案的题去调权重）。
//! 这一点在类型上表达：`outcome: Option<CaseOutcome>`，且
//! [`CaseLibrary::calibratable`] 只返回已回填者。


pub mod wiki;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum KbError {
    #[error("frontmatter 缺失或格式错误：{0}")]
    Frontmatter(String),
    #[error("条目 `{0}` 缺少必需字段：{1}")]
    MissingField(String, String),
    #[error("`applicable_when` 与 `fails_when` 至少各有一条——只写适用条件不够")]
    MissingConditions,
    #[error("条目 `{0}` 与已有条目重复（同一 theory_id）")]
    DuplicateTheory(String),
    #[error("案例 `{0}` 缺少 `selection_output`——预测未留档则无法回测")]
    MissingPrediction(String),
    #[error("回填的 `equilibrium_realized` 不在预测的候选均衡列表中：`{0}`")]
    OutcomeNotInCandidates(String),
    #[error("案例 `{0}` 的状态不允许该操作：{1}")]
    InvalidState(String, String),
    /// 案例库的**持久化**失败（读/写/序列化）。
    ///
    /// 单独一个变体（而非复用 `InvalidState`）：案例库的存盘失败是
    /// **数据损失风险**，处置与"状态不对"完全不同——前者要立刻停止写入
    /// 并检查磁盘，后者只需改调用顺序。
    #[error("案例库 I/O 失败（{path}）：{detail}")]
    Io { path: String, detail: String },
}

/// 理论条目（§7.1 的四要素 schema）。
///
/// 四要素（`applicable_when` / `fails_when` / `data_needs` / `outputs`）是
/// **开放扩展契约**：每个理论条目都必须声明这四项，Agent 建模时检索并引用。
///
/// `fails_when` 与 `applicable_when` 同等重要——用户最需要的恰恰是
/// "什么时候别用这套分析"（用错模型比不用模型更糟）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TheoryEntry {
    pub theory_id: String,
    pub name: String,
    /// 理论族（如 `noncooperative` / `cooperative` / `behavioral` / `decision`）。
    #[serde(default)]
    pub family: String,
    /// 适用条件。
    #[serde(default)]
    pub applicable_when: Vec<String>,
    /// **失效条件**——必填语义见 [`TheoryEntry::validate`]。
    #[serde(default)]
    pub fails_when: Vec<String>,
    /// 数据需求。
    #[serde(default)]
    pub data_needs: Vec<String>,
    /// 输出物。
    #[serde(default)]
    pub outputs: Vec<String>,
    /// 正文（含公式与直觉解释）。
    #[serde(default)]
    pub body: String,
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "1.0".to_string()
}

impl TheoryEntry {
    /// 校验四要素完备性。
    ///
    /// **为何强制 `fails_when` 非空**：一个说不出"何时不适用"的理论条目
    /// 会让 Agent 在错误场景下引用它。§5.1 的"开放扩展"允许新增条目，
    /// 但新增条目也必须声明失效条件。
    pub fn validate(&self) -> Result<(), KbError> {
        if self.theory_id.trim().is_empty() {
            return Err(KbError::MissingField("<unknown>".into(), "theory_id".into()));
        }
        if self.name.trim().is_empty() {
            return Err(KbError::MissingField(
                self.theory_id.clone(),
                "name".into(),
            ));
        }
        if self.applicable_when.is_empty() || self.fails_when.is_empty() {
            return Err(KbError::MissingConditions);
        }
        Ok(())
    }
}

/// 从 markdown 文本解析理论条目（YAML frontmatter + 正文）。
///
/// 格式：
///
/// ```text
/// ---
/// theory_id: bertrand-differentiated
/// name: 差异化产品价格竞争
/// ...
/// ---
/// 正文（markdown）
/// ```
///
/// **用 YAML frontmatter 而非纯结构化文件**：理论库同时是
/// **方法论卡（`SKILL.md`）**，遵循与 Claude Code 共享的跨工具 Agent Skills 约定
/// （Harness §13.7）。同一份文件既被本 crate 解析为理论条目，
/// 也可被其他 harness 当作 skill 加载——这是"写一次、多处可用"的杠杆点。
pub fn parse_theory_md(text: &str) -> Result<TheoryEntry, KbError> {
    let (fm, body) = split_frontmatter(text)?;
    let mut entry: TheoryEntry =
        serde_yaml::from_str(fm).map_err(|e| KbError::Frontmatter(e.to_string()))?;
    if entry.body.is_empty() {
        entry.body = body.trim().to_string();
    }
    entry.validate()?;
    Ok(entry)
}

/// 切分 frontmatter 与正文（公开版，供 wiki 加载器复用同一套约定）。
///
/// **不让 wiki 自己再写一份切分逻辑**：两份实现迟早会在边界上分歧，
/// 而那种分歧表现为「同一个文件在两条路径上解析出不同结果」。
pub fn split_frontmatter_pub(text: &str) -> Result<(&str, &str), KbError> {
    split_frontmatter(text)
}

/// 切分 frontmatter 与正文。
fn split_frontmatter(text: &str) -> Result<(&str, &str), KbError> {
    let t = text.trim_start_matches('\u{feff}'); // 去 BOM
    if !t.starts_with("---") {
        return Err(KbError::Frontmatter(
            "文件未以 `---` 开头，无 frontmatter".into(),
        ));
    }
    let rest = &t[3..];
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return Err(KbError::Frontmatter("frontmatter 未闭合".into()));
    };
    let fm = &rest[..end];
    let after = &rest[end + 4..]; // 跳过 "\n---"
    Ok((fm, after))
}

// ────────────────────────────────────────────────────────────────────────────
// 案例库（§7.2）——回测闭环的核心资产
// ────────────────────────────────────────────────────────────────────────────

/// 一条假设及其来源（铁律 2 的留档）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseAssumption {
    pub id: String,
    pub content: String,
    /// `user_confirmed` | `public_data` | `industry_prior` | `assumed`
    pub source: String,
}

/// 选择引擎的输出留档（**预测**）。
///
/// 这是回测的"题面"——没有它就无法比对预测与结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CasePrediction {
    /// 候选 id 列表。
    ///
    /// 含义**按 `kind` 而异**：scenario 下是均衡 id，whatif 下是对手反应，
    /// discover 下是达标候选。
    pub candidates: Vec<String>,
    /// 各候选的预测值（与 `candidates` 对齐）。
    ///
    /// 含义**按 `kind` 而异**——见 [`PredictionKind`]。
    pub distribution: Vec<f64>,
    /// 当时的判据权重快照（校准的前后对比需要）。
    pub selector_weights_snapshot: BTreeMap<String, f64>,
    /// **预测的形态**。
    ///
    /// # 为什么需要它（实测暴露的语义冲突）
    ///
    /// 早先本结构隐含假设"预测 = 归一化的均衡概率分布"，于是
    /// `CaseRecord::new` 硬校验归一化。而那在 **discover 模式下是错的**：
    /// 它的预测是"各候选的达标概率"——**多个候选可以同时达标**
    /// （和不等于 1，也**不该**等于 1）。
    ///
    /// 后果：discover 模式的归档被判"预测分布未归一化"而拒绝
    /// ⇒ 该模式的 `archived` 不可达（实测：脚本化 E2E 报出）。
    ///
    /// 而"归一化"这条校验**本来是对的**——它保护的是"对数损失可算"。
    /// 故修法是**在类型上区分两种预测**，让校验只对适用的那一种生效。
    #[serde(default)]
    pub kind: PredictionKind,
}

/// 预测的形态——决定"归一化"与"对数损失"是否适用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PredictionKind {
    /// **归一化的概率分布**（scenario：哪个均衡更可能）。
    ///
    /// 归一化与对数损失都适用——那是校准的数学前提。
    #[default]
    EquilibriumDistribution,
    /// **条件反应概率**（whatif：我方做 d 时对手各反应的概率）。
    ///
    /// 也归一化（对手必选其一），故对数损失同样适用。
    ConditionalReactions,
    /// **达标把握**（discover：各候选达到抱负水平的概率）。
    ///
    /// ⚠️ **不归一化**：多个候选可同时达标，和不等于 1 是正常的，
    /// 硬要它归一化等于编造一个"候选之间互斥"的假设——而那不成立。
    /// 故对数损失**不适用**于本类。
    SatisficingAchievement,
}

impl PredictionKind {
    /// 该形态是否要求归一化（决定 `CaseRecord::new` 是否校验）。
    pub fn requires_normalization(self) -> bool {
        match self {
            PredictionKind::EquilibriumDistribution
            | PredictionKind::ConditionalReactions => true,
            // 达标概率**不该**归一化——见该变体的说明
            PredictionKind::SatisficingAchievement => false,
        }
    }

    /// 该形态是否适用对数损失（校准的数学前提）。
    pub fn supports_log_loss(self) -> bool {
        self.requires_normalization()
    }
}

impl CasePrediction {
    /// 概率是否归一化（回测前提）。
    pub fn normalized(&self) -> bool {
        if self.distribution.is_empty() {
            return false;
        }
        let s: f64 = self.distribution.iter().sum();
        (s - 1.0).abs() < 1e-6
    }

    /// 与 `candidates` 长度一致。
    pub fn aligned(&self) -> bool {
        self.distribution.len() == self.candidates.len()
    }

    /// 最高概率候选（MAP）。
    ///
    /// 并列时按**候选名**字典序取小者（而非按下标），使确定性不依赖输入顺序——
    /// 若按下标，同一组候选换个顺序就会得到不同的 MAP。
    pub fn most_likely(&self) -> Option<(&str, f64)> {
        self.candidates
            .iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), self.distribution[i]))
            .max_by(|a, b| {
                a.1.partial_cmp(&b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.0.cmp(a.0))
            })
    }

    /// 实际实现的均衡所获的预测概率（对数损失的输入）。
    pub fn probability_of(&self, equilibrium: &str) -> Option<f64> {
        self.candidates
            .iter()
            .position(|c| c == equilibrium)
            .map(|i| self.distribution[i])
    }
}

/// 事后回填的结果（**答案**）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseOutcome {
    pub date: String,
    pub what_happened: String,
    /// 实际实现的均衡（须是预测候选之一）。
    pub equilibrium_realized: String,
}

/// 一条案例**从哪来**——这个区分是校准的第一道闸门。
///
/// # 为什么它必须存在（设计缺口，2026-09-19 发现）
///
/// 批量跑批产出的案例（"某社区生鲜店要不要开第二家店"）是**假设情景**，
/// 它们**永远不会有真实结果**。而在此之前，`CaseRecord` 无法区分
/// 「真实客户委托」与「演练/样例」——两者在库里长得一模一样。
///
/// 后果有两条，都很严重：
///
/// 1. **权重会被编出来的结果污染**：有人事后给演练案例填一个"结果"，
///    它就会与其他样本一起进 `calibrate()`，而报告会把调出来的权重
///    当成「已校准」——**那是用编造的数据拟合出来的可信度**。
/// 2. **"样本够了"会变成假信号**：跑批很容易攒到 30 条，
///    而 `MIN_SAMPLES_FOR_CALIBRATION` 会据此判定"可以校准了"。
///
/// 故：**只有 `RealEngagement` 参与校准**（`calibratable()` 的过滤），
/// 其余一律排除。这是 fail-closed —— 未标注的旧记录按 `Unspecified`
/// 处理，同样**不**参与校准（宁可样本少，不可样本假）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CaseProvenance {
    /// 真实客户委托（有真实的结果可回填）。
    RealEngagement,
    /// 演练 / 样例 / 批量跑批 / 测试：**不会**有真实结果。
    Exercise,
    /// 早期记录未标注来源（`load` 时的默认）。
    ///
    /// 不得参与校准：我们**不知道**它是不是真的，
    /// 而"不知道"在校准这件事上必须按"不是"处理。
    #[default]
    Unspecified,
}

impl CaseProvenance {
    /// 是否可参与校准。**只有真实委托可以。**
    pub fn calibratable(self) -> bool {
        matches!(self, CaseProvenance::RealEngagement)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            CaseProvenance::RealEngagement => "real_engagement",
            CaseProvenance::Exercise => "exercise",
            CaseProvenance::Unspecified => "unspecified",
        }
    }
}

/// 一条案例记录（§7.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseRecord {
    pub case_id: String,
    pub date: String,
    /// 客户匿名标识（保密要求）。
    pub client_anonymous: String,
    pub scenario: String,
    pub decision_question: String,
    #[serde(default)]
    pub assumptions: Vec<CaseAssumption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_spec_ref: Option<String>,
    pub prediction: CasePrediction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CaseOutcome>,
    /// 案例来源（真实委托 / 演练 / 未标注）——见 [`CaseProvenance`]。
    ///
    /// `#[serde(default)]` 使旧记录（无此字段）反序列化为 `Unspecified`，
    /// 从而**被排除在校准之外**——这是刻意的：我们不知道它们是不是真的。
    #[serde(default)]
    pub provenance: CaseProvenance,
}

impl CaseRecord {
    pub fn new(
        case_id: impl Into<String>,
        date: impl Into<String>,
        scenario: impl Into<String>,
        prediction: CasePrediction,
    ) -> Result<Self, KbError> {
        let case_id = case_id.into();
        if !prediction.aligned() {
            return Err(KbError::MissingPrediction(format!(
                "{case_id}：预测分布长度与候选数不符"
            )));
        }
        // 只对**要求归一化**的形态校验——见 `PredictionKind::requires_normalization`
        if prediction.kind.requires_normalization() && !prediction.normalized() {
            return Err(KbError::MissingPrediction(format!(
                "{case_id}：预测分布未归一化"
            )));
        }
        Ok(Self {
            case_id,
            date: date.into(),
            client_anonymous: String::new(),
            scenario: scenario.into(),
            decision_question: String::new(),
            assumptions: Vec::new(),
            game_spec_ref: None,
            prediction,
            outcome: None,
            provenance: CaseProvenance::Unspecified,
        })
    }

    /// 是否已回填结果。
    pub fn is_resolved(&self) -> bool {
        self.outcome.is_some()
    }

    /// 回填结果。
    ///
    /// **校验实现的均衡必须在预测候选中**——否则该案例的预测"没有覆盖真相"，
    /// 无法用于校准（拿它调权重会奖励错误的判据）。这是诚实性要求：
    /// 预测漏掉真相说明建模有缺陷，应作为**独立信号**记录，
    /// 而不是硬塞进对数损失里。
    pub fn resolve(&mut self, outcome: CaseOutcome) -> Result<(), KbError> {
        if !self
            .prediction
            .candidates
            .iter()
            .any(|c| *c == outcome.equilibrium_realized)
        {
            return Err(KbError::OutcomeNotInCandidates(
                outcome.equilibrium_realized,
            ));
        }
        self.outcome = Some(outcome);
        Ok(())
    }

    /// 对数损失（`−ln P(实际实现)`）。未回填则返回 `None`。
    pub fn log_loss(&self) -> Option<f64> {
        // ⚠️ 对数损失**只对归一化的概率预测有意义**——见 `PredictionKind`。
        //
        // 早先这里无条件计算，于是 discover 模式的"达标概率"会被拿去算
        // 对数损失；而那个数**没有意义**：达标概率之间不互斥，
        // 其和不为 1，`-ln(p)` 因此不是一个可校准的分数。
        // 返回 `None` 是如实表达"该案例不参与对数损失校准"。
        if !self.prediction.kind.supports_log_loss() {
            return None;
        }
        let o = self.outcome.as_ref()?;
        let p = self.prediction.probability_of(&o.equilibrium_realized)?;
        Some(-p.max(1e-12).ln())
    }
}

/// 案例库。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CaseLibrary {
    cases: BTreeMap<String, CaseRecord>,
}

impl CaseLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    /// 入库。重复 `case_id` 报错（不静默覆盖——覆盖会丢失预测留档）。
    pub fn insert(&mut self, case: CaseRecord) -> Result<(), KbError> {
        if self.cases.contains_key(&case.case_id) {
            return Err(KbError::DuplicateTheory(case.case_id.clone()));
        }
        self.cases.insert(case.case_id.clone(), case);
        Ok(())
    }

    pub fn get(&self, case_id: &str) -> Option<&CaseRecord> {
        self.cases.get(case_id)
    }

    pub fn get_mut(&mut self, case_id: &str) -> Option<&mut CaseRecord> {
        self.cases.get_mut(case_id)
    }

    pub fn len(&self) -> usize {
        self.cases.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cases.is_empty()
    }

    /// **可校准的案例**（已回填结果 **且** 来源是真实委托）。
    ///
    /// 两道过滤，缺一不可：
    /// 1. 已回填结果——拿没有答案的题去调权重是有害的；
    /// 2. **来源是真实委托**——演练案例的"结果"是编出来的，
    ///    拿它调权重会产出"用假数据拟合的可信度"（见 [`CaseProvenance`]）。
    pub fn calibratable(&self) -> Vec<&CaseRecord> {
        self.cases
            .values()
            .filter(|c| c.is_resolved() && c.provenance.calibratable())
            .collect()
    }

    /// 已回填结果、但**因来源不可校准而被排除**的案例数。
    ///
    /// 存在的理由：静默排除会让"样本不足 0/30"看起来像是"没有案例"，
    /// 而真相是"有 20 条演练案例，它们不算数"。这个数字要能被说出来。
    pub fn excluded_from_calibration(&self) -> usize {
        self.cases
            .values()
            .filter(|c| c.is_resolved() && !c.provenance.calibratable())
            .count()
    }

    /// 待回填的案例（预测已留档，结果未到）。
    pub fn pending(&self) -> Vec<&CaseRecord> {
        self.cases.values().filter(|c| !c.is_resolved()).collect()
    }

    /// 按场景筛选已回填案例（校准按场景分组——不同剧本的判据权重应独立）。
    pub fn calibratable_in_scenario(&self, scenario: &str) -> Vec<&CaseRecord> {
        self.calibratable()
            .into_iter()
            .filter(|c| c.scenario == scenario)
            .collect()
    }

    /// 平均对数损失（仅已回填者）。
    ///
    /// 冷启动期返回 `None`（无样本）——调用方据此把置信度标为"未校准"，
    /// 而非报一个 `NaN` 或 0（那会让读者误以为"预测很准"）。
    pub fn mean_log_loss(&self) -> Option<f64> {
        let resolved = self.calibratable();
        if resolved.is_empty() {
            return None;
        }
        let mut sum = 0.0;
        let mut n = 0usize;
        for c in resolved {
            if let Some(ll) = c.log_loss() {
                sum += ll;
                n += 1;
            }
        }
        if n == 0 {
            None
        } else {
            Some(sum / n as f64)
        }
    }

    /// 校准所需样本是否足够（§13.2 的 `MIN_SAMPLES_FOR_CALIBRATION`）。
    pub fn calibration_ready(&self, min_samples: usize) -> bool {
        self.calibratable().len() >= min_samples
    }

    /// **持久化到文件**（JSON）。
    ///
    /// # 为什么这个函数必须存在（实测审计发现的根本缺口）
    ///
    /// `CaseLibrary` 有 `Serialize/Deserialize`，但**没有任何存盘/读盘代码**
    /// ⇒ 它是**纯内存**的：进程退出即丢。
    ///
    /// 后果是校准闭环**从地基上就不成立**：
    /// - `case_store_write` 归档的案例，下次启动就没了
    /// - 案例库**永远积累不到** `MIN_SAMPLES_FOR_CALIBRATION`（30）个样本
    /// - 于是 `calibration_ready` 永远为 false，判据权重永远未校准
    /// - 而报告里那句"权重待案例库回测校准"会**永远**是同一句话
    ///
    /// 这是比"`backtest` 命令未实现"更根本的缺口——后者是入口，
    /// 而这个是**存储**：没有它，即使 backtest 实现了也无数据可用。
    ///
    /// # 写入策略
    ///
    /// 先写临时文件、再原子重命名（同目录 ⇒ 同一文件系统 ⇒ rename 原子）。
    /// 直接覆写会在崩溃时留下半截文件，而**案例库损坏等于样本全丢**——
    /// 那些样本是几个月积累起来的、无法重建。
    pub fn save(&self, path: &std::path::Path) -> Result<(), KbError> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| KbError::Io {
                path: dir.display().to_string(),
                detail: e.to_string(),
            })?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| KbError::Io {
            path: path.display().to_string(),
            detail: format!("序列化失败：{e}"),
        })?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, &json).map_err(|e| KbError::Io {
            path: tmp.display().to_string(),
            detail: e.to_string(),
        })?;
        std::fs::rename(&tmp, path).map_err(|e| KbError::Io {
            path: path.display().to_string(),
            detail: format!("原子重命名失败：{e}"),
        })
    }

    /// **从文件加载**。文件不存在时返回**空库**（冷启动是正常状态）。
    ///
    /// ⚠️ 但**损坏**与**不存在**必须区分：
    /// - 不存在 ⇒ 空库（新用户，正常）
    /// - 损坏 ⇒ **报错**，绝不当作空库
    ///
    /// 后者会让"案例库被写坏了"表现为"还没有积累样本"——
    /// 而用户会继续归档、继续覆盖，**把仅有的证据也覆盖掉**。
    pub fn load(path: &std::path::Path) -> Result<Self, KbError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).map_err(|e| KbError::Io {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        serde_json::from_str(&text).map_err(|e| KbError::Io {
            path: path.display().to_string(),
            detail: format!(
                "案例库文件损坏，**不当作空库处理**：{e}。\n\
                 → 请检查该文件（它是几个月积累的样本，无法重建）；\
                 若确认要重建，请手动把它改名而不是让程序覆盖它。"
            ),
        })
    }

    /// 全部案例（含未回填的）。只读视图。
    pub fn iter(&self) -> impl Iterator<Item = &CaseRecord> {
        self.cases.values()
    }

    /// **把已回填的案例转成校准记录**——校准闭环的数据供给。
    ///
    /// # 为什么需要这个函数（实测审计发现的闭环缺口）
    ///
    /// 校准的完整链条是：
    ///
    /// ```text
    /// 归档（冻结预测）→ 事后回填结果 → 【转换】→ calibrate() → 新权重
    /// ```
    ///
    /// 而第四步**没有实现**：`BacktestRecord` 只在 `shangyi-selection`
    /// 的**测试**里被构造过，生产路径上没有任何地方从案例库生成它
    /// ⇒ 校准**永远无法启动**，案例库永远是 `calibratable=0`。
    ///
    /// 这正是"能力建了但没接"的又一例（本轮发现的第 7 处）。
    ///
    /// # 只收**能用于校准**的案例
    ///
    /// 跳过两类（如实计数并返回，不静默丢弃）：
    /// - 未回填结果的（没有"真相"可比）
    /// - 预测形态不支持对数损失的（`PredictionKind::supports_log_loss`
    ///   ——如 discover 的达标概率，其和不等于 1，`-ln(p)` 无意义）
    ///
    /// # 返回 `(记录, 跳过数)`
    ///
    /// `skipped` 统计的是**已回填但无法转成记录**的案例——即
    /// ① 预测形态不支持对数损失的（如 discover 的达标概率）。
    ///
    /// ⚠️ **未回填的案例不在其中**：它们根本不在 `calibratable()` 集合里，
    /// 而"有多少案例待回填"由 `pending()` 单独提供。
    ///
    /// ## 这两个计数为什么要分开（我第一版把它们混了）
    ///
    /// 它们的**含义与处置完全不同**：
    ///
    /// | 计数 | 含义 | 处置 |
    /// |---|---|---|
    /// | `pending().len()` | 等待回填结果 | 到时间去问"后来发生了什么" |
    /// | `skipped` | 回填了但形态不支持对数损失 | **不适用**——不是待办 |
    ///
    /// 混在一起会让"样本不够"看起来像"还有工作没做"，
    /// 而真相可能是"这些案例本来就不参与对数损失校准"。
    pub fn backtest_records(
        &self,
    ) -> (Vec<shangyi_selection::BacktestRecord>, usize) {
        let mut out = Vec::new();
        let mut skipped = 0usize;
        // 只遍历**已回填**的（`calibratable()` 的定义）——未回填的由 `pending()` 报
        for c in self.calibratable() {
            let Some(o) = &c.outcome else {
                // 理论上不可达（`calibratable()` 已过滤）。保留分支以防御
                // 未来有人放宽那个过滤——那时这里会如实计数而不是静默跳过。
                skipped += 1;
                continue;
            };
            // 预测形态不支持对数损失 ⇒ 该案例不参与校准
            if !c.prediction.kind.supports_log_loss() {
                skipped += 1;
                continue;
            }
            // 实际实现的均衡在预测候选里的**下标**
            let Some(realized) = c
                .prediction
                .candidates
                .iter()
                .position(|x| *x == o.equilibrium_realized)
            else {
                // `resolve()` 已保证候选含真相，故这里只可能因数据被外部改动。
                // 跳过而非 panic：一条坏记录不该让整次校准失败。
                skipped += 1;
                continue;
            };
            // 判据权重快照 → `SelectorId`
            let mut weights = BTreeMap::new();
            for (name, w) in &c.prediction.selector_weights_snapshot {
                if let Some(id) = shangyi_selection::SelectorId::ALL
                    .iter()
                    .find(|s| s.as_str() == name.as_str())
                {
                    weights.insert(*id, *w);
                }
            }
            out.push(shangyi_selection::BacktestRecord {
                predicted: c.prediction.distribution.clone(),
                realized,
                selector_weights: weights,
            });
        }
        (out, skipped)
    }

    /// 全部案例 id（确定性顺序）。
    pub fn ids(&self) -> BTreeSet<&str> {
        self.cases.keys().map(|s| s.as_str()).collect()
    }
}

/// 理论库。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TheoryLibrary {
    entries: BTreeMap<String, TheoryEntry>,
}

impl TheoryLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, e: TheoryEntry) -> Result<(), KbError> {
        e.validate()?;
        if self.entries.contains_key(&e.theory_id) {
            return Err(KbError::DuplicateTheory(e.theory_id));
        }
        self.entries.insert(e.theory_id.clone(), e);
        Ok(())
    }

    /// 从 markdown 文本载入。
    pub fn insert_md(&mut self, text: &str) -> Result<String, KbError> {
        let e = parse_theory_md(text)?;
        let id = e.theory_id.clone();
        self.insert(e)?;
        Ok(id)
    }

    pub fn get(&self, theory_id: &str) -> Option<&TheoryEntry> {
        self.entries.get(theory_id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 按条件关键词**粗筛**候选理论条目。
    ///
    /// **注意**：这是关键词匹配，不是语义检索。语义检索由进程隔离的 Python 侧车
    /// （BGE embedding + 向量库）做，此处只做**不依赖任何索引的降级路径**。
    /// 用途：侧车不可用时仍能给出可用的候选（降级但不失效），
    /// 以及为侧车检索结果提供交叉校验。
    ///
    /// 匹配规则：条目的 `applicable_when` 任一关键词命中即入选；
    /// 若同时命中 `fails_when`，**标记为警示**而非直接排除——
    /// 因为"失效条件命中"本身是需要人来判断的（如容量约束只在小规模时强）。
    pub fn keyword_candidates(&self, keywords: &[String]) -> Vec<TheoryMatch> {
        let mut out = Vec::new();
        for e in self.entries.values() {
            let hits_apply: Vec<String> = e
                .applicable_when
                .iter()
                .filter(|c| keywords.iter().any(|k| c.contains(k.as_str())))
                .cloned()
                .collect();
            if hits_apply.is_empty() {
                continue;
            }
            let hits_fail: Vec<String> = e
                .fails_when
                .iter()
                .filter(|c| keywords.iter().any(|k| c.contains(k.as_str())))
                .cloned()
                .collect();
            out.push(TheoryMatch {
                theory_id: e.theory_id.clone(),
                name: e.name.clone(),
                matched_applicable: hits_apply,
                matched_fails: hits_fail,
            });
        }
        out.sort_by(|a, b| a.theory_id.cmp(&b.theory_id));
        out
    }
}

/// 关键词粗筛的命中结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TheoryMatch {
    pub theory_id: String,
    pub name: String,
    /// 命中的适用条件。
    pub matched_applicable: Vec<String>,
    /// 命中的**失效条件**——非空时应提示用户该理论的适用性存疑。
    pub matched_fails: Vec<String>,
}

impl TheoryMatch {
    /// 是否命中失效条件（需人工判断）。
    pub fn has_fail_warning(&self) -> bool {
        !self.matched_fails.is_empty()
    }

    /// 供报告引用的说明。
    pub fn note(&self) -> String {
        if self.has_fail_warning() {
            format!(
                "{}（命中适用条件 {} 条；**同时命中失效条件 {} 条，适用性需确认**）",
                self.name,
                self.matched_applicable.len(),
                self.matched_fails.len()
            )
        } else {
            format!(
                "{}（命中适用条件 {} 条）",
                self.name,
                self.matched_applicable.len()
            )
        }
    }
}

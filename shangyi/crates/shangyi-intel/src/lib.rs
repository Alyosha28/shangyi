//! 商奕情报层与参与者画像（设计文档 §6）。
//!
//! # 本 crate 的两件事，以及为什么合规红线放在类型里
//!
//! ## 1. 数据源清单与合规红线（§6.1）
//!
//! §6.1 的合规红线是：**仅公开商业信息；不采集个人信息**。
//!
//! 这句话若只写在文档里，在工程压力下总会被绕过。故本 crate 用类型表达：
//! [`SourceKind::Business`] 与 [`SourceKind::Personal`] 是不同变体，而
//! [`DataSource::new`] **拒绝**任何 `Personal` 来源——使"采集个人信息"在
//! 构造层面不可能，而非依赖自律。
//!
//! ## 2. 参与者画像 Schema（§6.2，价值网全角色）
//!
//! 每位参与者（顾客/供应商/替代者/互补者/监管）一份画像：
//! 通用块 + **按角色差异化**的数据块。差异化不是装饰——
//! 供应商画像要看 BATNA 与锁定风险，顾客画像要看支付意愿与弹性，
//! 用同一套字段会导致"对所有人的估计都很粗糙"。
//!
//! ## 3. 贝叶斯类型推断（§6.3 与 `competitor_intel` 剧本）
//!
//! 由对手的**行为时间线**更新其类型后验（如"清库存型 vs 长期战略型"）。
//! 这是 §5.1 场景-模型映射中"竞对研判"的核心方法。
//! 推断是确定性代码（似然由类型特定的行为模型给出），符合铁律 1。


pub mod amap;
pub mod bocha;
pub mod fetch;
pub mod search_rank;
pub mod secrets;
pub mod sidecar;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use shangyi_gamespec::{RelationToGame, ValueNetRole};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IntelError {
    #[error("数据源类别为个人信息——§6.1 合规红线禁止采集（仅限公开商业信息）")]
    PersonalDataForbidden,
    #[error("数据源类别为可疑/未确认——不使用来源不明的数据")]
    UnverifiedSource,
    #[error("经验先验（borrowed）必须标注来源出处，不得作为本条实证使用")]
    PriorWithoutCitation,
    #[error("未知角色 `{0}` 的差异化字段块缺失")]
    MissingRoleSpecific(ValueNetRole),
    #[error("行为观测的幅度须有限且非负，实得 {0}")]
    InvalidMagnitude(f64),
    #[error("类型的先验概率须为正且和为 1，实得 {0:?}")]
    InvalidPriors(BTreeMap<String, f64>),
    #[error("类型 `{0}` 缺少行为似然定义")]
    MissingLikelihood(String),
    #[error("信念更新后全部类型概率为 0——观测与全部类型均不相容")]
    ZeroPosterior,
}

/// 数据源类别——区分公开商业信息与个人信息（§6.1 合规红线）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// 公开商业信息（价格、促销、财报、招聘 JD、工商信息、行业报告、POI 等）。
    Business,
    /// **个人信息**——本 crate 禁止构造此类数据源。
    Personal,
    /// 来源不明或未经确认。
    Unverified,
}

/// 具体的公开数据源（§6.1 清单）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusinessSource {
    /// 电商平台 / 外卖 / 点评（价格轨迹、促销节奏、评分客流代理）。
    Platform,
    /// 工商信息（注册、股东、分支机构）。
    Registry,
    /// 招聘 JD（岗位扩张暴露战略意图）。
    JobPosting,
    /// 上市公司财报 / 公告。
    Filing,
    /// 行业报告 / 新闻。
    Report,
    /// POI 地图数据（网点密度）。
    Poi,
}

impl BusinessSource {
    /// 该来源主要用于什么。
    pub fn primary_use(self) -> &'static str {
        match self {
            BusinessSource::Platform => "价格竞争核心数据",
            BusinessSource::Registry => "进入/扩张信号",
            BusinessSource::JobPosting => "竞对类型推断的富矿",
            BusinessSource::Filing => "payoff 估计",
            BusinessSource::Report => "市场规模与事件",
            BusinessSource::Poi => "空间竞争",
        }
    }
}

/// 一个数据源引用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataSource {
    pub kind: SourceKind,
    /// 具体来源（`Business` 时应有值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub business: Option<BusinessSource>,
    /// 引用位置（URL 或文件路径）——**必须有**，否则无法审计。
    pub locator: String,
    /// 抓取/读取时间（毫秒时间戳）。
    pub fetched_at_ms: i64,
}

impl DataSource {
    /// 构造一个数据源。**个人信息与未确认来源在此被拒绝**。
    ///
    /// # 为什么把合规放进构造器
    ///
    /// §6.1 的红线若只写在文档与评审清单里，在赶工期时最容易被绕过，
    /// 而"采集了个人信息"是**不可逆**的合规事故（数据已落盘）。
    /// 使非法构造在类型层面失败，则这类事故不会发生。
    pub fn new(kind: SourceKind, locator: impl Into<String>, fetched_at_ms: i64) -> Result<Self, IntelError> {
        match kind {
            SourceKind::Personal => return Err(IntelError::PersonalDataForbidden),
            SourceKind::Unverified => return Err(IntelError::UnverifiedSource),
            SourceKind::Business => {}
        }
        Ok(Self {
            kind,
            business: None,
            locator: locator.into(),
            fetched_at_ms,
        })
    }

    /// 带具体来源类别构造。
    pub fn business(
        source: BusinessSource,
        locator: impl Into<String>,
        fetched_at_ms: i64,
    ) -> Self {
        Self {
            kind: SourceKind::Business,
            business: Some(source),
            locator: locator.into(),
            fetched_at_ms,
        }
    }
}

/// 行为观测类型（画像时间线的条目类型）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    PriceCut,
    PriceRaise,
    Promotion,
    NewStore,
    Hiring,
    SupplyChange,
    /// 公开表态 / 声明。
    Statement,
    Other,
}

/// 画像时间线的一条观测。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub date: String,
    pub event_type: EventType,
    /// 幅度（如降价 15% ⇒ 0.15）。
    pub magnitude: f64,
    /// 来源——**必须有**，且须通过 [`DataSource::new`] 的合规检查。
    pub source: DataSource,
    /// 该观测的置信度（0–1）。
    pub confidence: f64,
}

impl TimelineEvent {
    pub fn new(
        date: impl Into<String>,
        event_type: EventType,
        magnitude: f64,
        source: DataSource,
        confidence: f64,
    ) -> Result<Self, IntelError> {
        if !magnitude.is_finite() || magnitude < 0.0 {
            return Err(IntelError::InvalidMagnitude(magnitude));
        }
        Ok(Self {
            date: date.into(),
            event_type,
            magnitude,
            source,
            confidence: confidence.clamp(0.0, 1.0),
        })
    }
}

/// 声誉/能力的定性档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Low,
    Medium,
    High,
}

/// 参与者的通用画像块（全角色共有）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CommonProfile {
    /// 利益诉求。
    #[serde(default)]
    pub interests: Vec<String>,
    /// 掌握的资源。
    #[serde(default)]
    pub resources: Vec<String>,
    /// 决策风格（自然语言）。
    #[serde(default)]
    pub decision_style: String,
    /// 权力不对称性描述。
    #[serde(default)]
    pub power_asymmetry: String,
}

/// 按角色差异化的画像字段（§6.2）。
///
/// **为什么差异化是必要的**：供应商谈判的关键是它与我们的 BATNA 对比，
/// 顾客定价的关键是支付意愿与弹性，竞对的关键是行为参数。
/// 用一套通用字段会导致"对所有人的估计都很粗糙"——
/// 而博弈建模最怕的正是"用一个大概的数去算一个精确的均衡"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", content = "details", rename_all = "snake_case")]
pub enum RoleSpecific {
    /// 供应商：集中度、转换成本、我方 BATNA、锁定风险。
    Supplier {
        supply_concentration: f64,
        switching_cost: Level,
        our_batna: String,
        holdup_risk: Level,
    },
    /// 顾客：细分、支付意愿区间、需求弹性、集中度。
    Customer {
        segment: String,
        wtp_low: f64,
        wtp_high: f64,
        elasticity: f64,
        concentration: String,
    },
    /// 替代者（竞对）：行为参数与成本估计。
    ///
    /// # 为什么这些字段全是 `Option`（这是实测逼出来的）
    ///
    /// 早先它们是**必填 `f64`**。后果在一次真实咨询里暴露得很彻底：
    /// 模型连续失败 7 次，错误依次是
    /// `missing field lambda_qre` → `level_k` → `aggressiveness` → …
    /// （serde 只报**第一个**缺失字段，故模型只能一个个试），
    /// 最后一次是 `invalid type: string "未取得", expected f64`——
    /// 模型如实写了「未取得」，却被类型拒绝。
    ///
    /// 两条独立的坏处：
    ///
    /// 1. **逼人编数字**：这些量在 SME 场景里几乎不可能有来源
    ///    （对手的 QRE λ、推理层级、成本分布——谁能知道？）。
    ///    必填意味着模型只有两条路：编一个（违反铁律 1）或反复试错。
    ///    「用一个大概的数去算一个精确的均衡」正是本文件上面批判的事。
    /// 2. **没有「不知道」的表达**：与 `Level` 同病——缺少未知档，
    ///    于是"未知"被迫伪装成一个具体值。
    ///
    /// 改为 `Option` 后，「未取得」是合法且**显式**的：`null`。
    /// 下游若需要某个参数而它缺失，会拿到 `None` 并如实报告缺什么，
    /// 而不是拿到一个编出来的数。
    ///
    /// 顺带修掉一个更隐蔽的问题：这些字段**目前没有任何消费方**
    /// （全仓 grep 只有定义处）。逼模型为无人消费的字段编数字，
    /// 是纯粹的浪费——而且那些数字会进报告，看起来像证据。
    Substitutor {
        /// QRE 的 λ（反应敏感度）。`None` = 未取得。
        #[serde(default)]
        lambda_qre: Option<f64>,
        /// level-k 的 k（推理层级）。`None` = 未取得。
        #[serde(default)]
        level_k: Option<u32>,
        /// 攻击性（0–1）。`None` = 未取得。
        #[serde(default)]
        aggressiveness: Option<f64>,
        /// 成本估计均值。`None` = 未取得。
        #[serde(default)]
        cost_mu: Option<f64>,
        /// 成本估计标准差。`None` = 未取得。
        #[serde(default)]
        cost_sigma: Option<f64>,
    },
    /// 互补者：生态耦合度。
    Complementor {
        /// 生态耦合度（0–1）。
        coupling: f64,
        /// 互补关系的具体内容。
        synergy_note: String,
    },
}

impl RoleSpecific {
    /// 该字段块对应的角色。
    pub fn role(&self) -> ValueNetRole {
        match self {
            RoleSpecific::Supplier { .. } => ValueNetRole::Supplier,
            RoleSpecific::Customer { .. } => ValueNetRole::Customer,
            RoleSpecific::Substitutor { .. } => ValueNetRole::Substitutor,
            RoleSpecific::Complementor { .. } => ValueNetRole::Complementor,
        }
    }

    /// 校验取值合理性。
    pub fn validate(&self) -> Result<(), IntelError> {
        match self {
            RoleSpecific::Supplier {
                supply_concentration,
                ..
            } => {
                if !(0.0..=1.0).contains(supply_concentration) || !supply_concentration.is_finite() {
                    return Err(IntelError::InvalidMagnitude(*supply_concentration));
                }
            }
            RoleSpecific::Customer {
                wtp_low,
                wtp_high,
                ..
            } => {
                if wtp_low > wtp_high {
                    return Err(IntelError::InvalidMagnitude(*wtp_low));
                }
            }
            RoleSpecific::Substitutor {
                lambda_qre,
                aggressiveness,
                cost_sigma,
                ..
            } => {
                // 校验**只在有值的时候**做。`None` 是合法状态（未取得），
                // 不是"待补的漏洞"——把它当错误会让模型被迫编一个数出来。
                if let Some(a) = aggressiveness {
                    if !(0.0..=1.0).contains(a) || !a.is_finite() {
                        return Err(IntelError::InvalidMagnitude(*a));
                    }
                }
                if let Some(s) = cost_sigma {
                    if !s.is_finite() || *s < 0.0 {
                        return Err(IntelError::InvalidMagnitude(*s));
                    }
                }
                if let Some(l) = lambda_qre {
                    if !l.is_finite() || *l < 0.0 {
                        return Err(IntelError::InvalidMagnitude(*l));
                    }
                }
            }
            RoleSpecific::Complementor { coupling, .. } => {
                if !(0.0..=1.0).contains(coupling) || !coupling.is_finite() {
                    return Err(IntelError::InvalidMagnitude(*coupling));
                }
            }
        }
        Ok(())
    }
}

/// 参与者画像（§6.2）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParticipantProfile {
    pub participant_id: String,
    pub value_net_role: ValueNetRole,
    /// 本次博弈中的定位（与 GameSpec 同步）。
    pub relation_to_game: RelationToGame,
    pub common: CommonProfile,
    /// 角色差异化字段块。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_specific: Option<RoleSpecific>,
    /// 行为时间线。
    #[serde(default)]
    pub timeline: Vec<TimelineEvent>,
    /// 类型后验（如 `{"dumping": 0.35, "strategic": 0.65}`）。
    #[serde(default)]
    pub posterior_types: BTreeMap<String, f64>,
    /// 证据质量评级（如 `"B（两条独立来源交叉）"`）。
    #[serde(default)]
    pub evidence_quality: String,
    /// 画像最后更新时间（Unix 毫秒）。
    ///
    /// `#[serde(default)]` 是**刻意**的：本字段由**调用方（执行器）**填，
    /// 不接受模型提供。模型不知道当前时间，让它填只能得到编造的 epoch
    /// 值（实测报错 `missing field updated_at_ms`，且模型试的方向是错的）。
    /// 时间戳的正确来源是时钟，不是语言模型的猜测。
    #[serde(default)]
    pub updated_at_ms: i64,
}

impl ParticipantProfile {
    pub fn new(
        participant_id: impl Into<String>,
        value_net_role: ValueNetRole,
        relation_to_game: RelationToGame,
    ) -> Self {
        Self {
            participant_id: participant_id.into(),
            value_net_role,
            relation_to_game,
            common: CommonProfile::default(),
            role_specific: None,
            timeline: Vec::new(),
            posterior_types: BTreeMap::new(),
            evidence_quality: String::new(),
            updated_at_ms: 0,
        }
    }

    /// 校验画像内部一致性。
    ///
    /// 关键检查：**角色差异化字段块必须与 `value_net_role` 匹配**。
    /// 若声明是供应商却在 `Substitutor` 块里填数据，说明画像生成逻辑有缺陷，
    /// 而下游会按错误的角色口径解读这些数字。
    pub fn validate(&self) -> Result<(), IntelError> {
        match &self.role_specific {
            // ⚠️ **缺失字段块是合法的**——那是「未取得」的表达。
            //
            // 早先这里无条件报错。而字段类型是 `Option`（即"可以没有"），
            // 且本项目反复确立的原则是「未取得」必须有合法表达形式
            // （见 `RoleSpecific::Substitutor` 那次修复：模型如实写
            // "未取得"却被类型拒绝，只能编数）。
            //
            // SME 场景里"对手的攻击性参数"经常拿不到，而**拿不到就不该编**。
            // 故缺失 ⇒ 通过校验（下游按"无该角色参数"处理）；
            // 但**给了就必须与声明的角色一致**——那才是本条要防的错
            // （声明 supplier 却填 substitutor 的块 ⇒ 下游按错误口径解读）。
            None => Ok(()),
            Some(rs) => {
                if rs.role() != self.value_net_role {
                    return Err(IntelError::MissingRoleSpecific(self.value_net_role));
                }
                rs.validate()
            }
        }
    }

    /// 后验是否归一化（和为 1）。
    pub fn posterior_normalized(&self) -> bool {
        if self.posterior_types.is_empty() {
            return true;
        }
        let s: f64 = self.posterior_types.values().sum();
        (s - 1.0).abs() < 1e-9
    }

    /// 最高后验类型（MAP 估计）。
    pub fn most_likely_type(&self) -> Option<(&str, f64)> {
        self.posterior_types
            .iter()
            .max_by(|a, b| {
                a.1.partial_cmp(b.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.0.cmp(a.0)) // 并列时取字典序小者，保证确定性
            })
            .map(|(k, v)| (k.as_str(), *v))
    }
}

/// 类型特定的行为似然模型。
///
/// `P(观测 | 类型)` —— 由类型对某类事件的**倾向权重**给出。
///
/// **为何用权重而非概率密度**：观测幅度与"该类型多可能这样做"之间的关系
/// 在实务中只能定性给出（"清库存型更可能大幅降价"），
/// 强行拟合密度会制造虚假精度。故用非负权重，归一化由贝叶斯更新负责。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BehaviorModel {
    /// 类型名。
    pub type_name: String,
    /// 事件类型 → 倾向权重（越大越可能做出该类行为）。
    #[serde(default)]
    pub event_weights: BTreeMap<EventType, f64>,
    /// 该类型对"幅度"的敏感度：观测幅度 × 该系数进入似然。
    #[serde(default = "default_magnitude_sensitivity")]
    pub magnitude_sensitivity: f64,
}

fn default_magnitude_sensitivity() -> f64 {
    1.0
}

impl BehaviorModel {
    pub fn new(type_name: impl Into<String>, event_weights: BTreeMap<EventType, f64>) -> Self {
        Self {
            type_name: type_name.into(),
            event_weights,
            magnitude_sensitivity: 1.0,
        }
    }

    /// 似然 `P(观测 | 本类型)`（未归一化）。
    ///
    /// 形式：
    ///
    /// ```text
    /// L_raw   = event_weight × exp(敏感度 × 幅度)
    /// L_eff   = L_raw ^ 置信度
    /// ```
    ///
    /// - `event_weight` 缺失 ⇒ 该类型不太可能做出该类行为（用 0 表示）
    /// - 幅度越大，倾向越强的事件越提高似然（`exp` 保证正值且单调）
    /// - **置信度以指数形式作用**，这是关键（见下）
    ///
    /// # ⚠️ 置信度为何不能作为线性乘子（一个已修正的实现缺陷）
    ///
    /// 最初写作 `L = w × exp(…) × conf`。**这是错的**：`conf` 是同时乘在
    /// 全部类型似然上的**公因子**，归一化时被约掉——于是置信度 0.05 的观测
    /// 与置信度 1.0 的观测对后验的推动**完全相同**。该缺陷由测试
    /// `confidence_scales_observation_influence` 暴露（两种情形都得到 0.769231）。
    ///
    /// 正确做法是几何插值 `L^conf`，它把置信度理解为"似然在多大程度上可信"：
    /// - `conf = 1` ⇒ `L_eff = L_raw`（完全采信）
    /// - `conf → 0` ⇒ `L_eff → L_raw^0 = 1` ⇒ 后验**回到先验**（观测被视为无信息）
    ///
    /// 这也符合直觉：一条"把握不大"的观测不应显著改变判断。
    ///
    /// 注意 `0^conf = 0` 对任何 `conf > 0` 成立，故"该类型不会做此事"这一
    /// 硬信息仍被保留——置信度低不会让不可能变为可能。
    pub fn likelihood(&self, obs: &TimelineEvent) -> f64 {
        let w = self.event_weights.get(&obs.event_type).copied().unwrap_or(0.0);
        if w <= 0.0 {
            // 硬零：该类型不会做出此类行为。置信度不改变这一点。
            return 0.0;
        }
        let raw = w * (self.magnitude_sensitivity * obs.magnitude).exp();
        if raw <= 0.0 {
            return 0.0;
        }
        let conf = obs.confidence.clamp(0.0, 1.0);
        raw.powf(conf)
    }
}

/// 贝叶斯类型推断结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeInference {
    /// 更新后的后验（和为 1）。
    pub posterior: BTreeMap<String, f64>,
    /// 更新前的先验。
    pub prior: BTreeMap<String, f64>,
    /// 参与更新的观测数。
    pub observations: usize,
    /// 是否发生了"某观测与全部类型都不相容"的情形。
    ///
    /// 若是，说明**类型假设集不完备**（真实类型不在候选里），
    /// 这是诚实信号而非噪声：应扩充类型集或降低该观测的置信度。
    pub model_incomplete: bool,
}

impl TypeInference {
    /// 后验是否归一化（和为 1）。报告生成前的自检。
    pub fn posterior_normalized_ok(&self) -> bool {
        let s: f64 = self.posterior.values().sum();
        (s - 1.0).abs() < 1e-9
    }

    /// 相对先验的变化量（L1 距离 / 2，∈ [0,1]）。
    ///
    /// 用途：判断本次推断是否真的带来了信息。若接近 0，
    /// 说明观测对类型区分度不足——报告应如实说明"证据不足以区分类型"。
    pub fn information_gain(&self) -> f64 {
        let mut sum = 0.0;
        for (k, v) in &self.posterior {
            let p = self.prior.get(k).copied().unwrap_or(0.0);
            sum += (v - p).abs();
        }
        (sum / 2.0).clamp(0.0, 1.0)
    }
}

/// 贝叶斯类型推断：由行为时间线更新类型后验。
///
/// # 算法
///
/// 对每个观测按贝叶斯更新：
///
/// ```text
/// P(t | o₁..oₙ) ∝ P(t) · Πᵢ P(oᵢ | t)
/// ```
///
/// 实现上按观测顺序逐个更新（`posterior ∝ prior × likelihood` 后归一化），
/// 与一次乘完等价但数值更稳（逐步归一化避免连乘下溢）。
///
/// # 观测顺序
///
/// 时间线按 `date` 字符串升序处理（ISO 日期字符串的字典序即时间序），
/// 使结果与输入顺序无关——这是可复现性的要求。
pub fn infer_types(
    observations: &[TimelineEvent],
    models: &[BehaviorModel],
    priors: &BTreeMap<String, f64>,
) -> Result<TypeInference, IntelError> {
    // 先验校验
    if priors.is_empty() {
        return Err(IntelError::InvalidPriors(priors.clone()));
    }
    let sum: f64 = priors.values().sum();
    if (sum - 1.0).abs() > 1e-6 || priors.values().any(|v| *v <= 0.0 || !v.is_finite()) {
        return Err(IntelError::InvalidPriors(priors.clone()));
    }
    // 每个有先验的类型都要有行为模型
    for t in priors.keys() {
        if !models.iter().any(|m| m.type_name == *t) {
            return Err(IntelError::MissingLikelihood(t.clone()));
        }
    }

    let mut post: BTreeMap<String, f64> = priors.clone();
    let mut model_incomplete = false;

    // 按日期升序（确定性）
    let mut sorted: Vec<&TimelineEvent> = observations.iter().collect();
    sorted.sort_by(|a, b| a.date.cmp(&b.date));

    for obs in &sorted {
        let mut unnorm: BTreeMap<String, f64> = BTreeMap::new();
        let mut total = 0.0;
        for m in models {
            let Some(prior) = post.get(&m.type_name) else {
                continue;
            };
            let l = m.likelihood(obs);
            let v = prior * l;
            total += v;
            unnorm.insert(m.type_name.clone(), v);
        }

        if total <= 0.0 {
            // 该观测与全部类型都不相容 ⇒ 类型假设集不完备。
            // **不静默跳过**：标记该信号并保持后验不变，让调用方知道要扩充类型集。
            model_incomplete = true;
            continue;
        }

        for v in unnorm.values_mut() {
            *v /= total;
        }
        post = unnorm;
    }

    if post.values().all(|v| *v <= 0.0) {
        return Err(IntelError::ZeroPosterior);
    }
    // 最终归一化（应对浮点累积误差）
    let s: f64 = post.values().sum();
    if s > 0.0 {
        for v in post.values_mut() {
            *v /= s;
        }
    }

    Ok(TypeInference {
        posterior: post,
        prior: priors.clone(),
        observations: sorted.len(),
        model_incomplete,
    })
}

/// 把推断结果写回画像，并更新时间戳。
pub fn apply_inference(profile: &mut ParticipantProfile, inf: &TypeInference, now_ms: i64) {
    profile.posterior_types = inf.posterior.clone();
    profile.updated_at_ms = now_ms;
}

/// 由角色差异化字段生成 persona 决策风格摘要（供 Swarm 的角色卡使用）。
pub fn decision_style_summary(rs: &RoleSpecific) -> String {
    match rs {
        RoleSpecific::Supplier {
            switching_cost,
            holdup_risk,
            ..
        } => format!("供应商：转换成本 {switching_cost:?}，锁定风险 {holdup_risk:?}"),
        RoleSpecific::Customer {
            segment,
            elasticity,
            ..
        } => format!("顾客（{segment}）：需求弹性 {elasticity}"),
        RoleSpecific::Substitutor {
            aggressiveness,
            level_k,
            ..
        } => {
            // 缺值如实写「未取得」——不省略、不用 0 顶替。
            // 用 0 会让读者以为"攻击性为零"，那是一个**关于对手的主张**；
            // 而事实是我们不知道。两者对建模的含义完全相反。
            let agg = aggressiveness
                .map(|a| format!("{a:.2}"))
                .unwrap_or_else(|| "未取得".to_string());
            let k = level_k
                .map(|k| k.to_string())
                .unwrap_or_else(|| "未取得".to_string());
            format!("竞对：攻击性 {agg}，推理层级 k={k}")
        }
        RoleSpecific::Complementor { coupling, .. } => {
            format!("互补者：生态耦合度 {coupling:.2}")
        }
    }
}

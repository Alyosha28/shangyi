//! 商奕 GameSpec：博弈的统一表示（设计文档 §5.2）。
//!
//! 设计要点（两条铁律的落点）：
//! 1. **payoff 不是数字，而是参数表达式**（`Expr`）——每个参数带分布与来源，
//!    使"报告中每个数字可溯源"在数据结构层面即可表达，而非事后补记。
//! 2. **参数必须带来源**（`ParamSource`）——`assumed` 必须经 checkpoint 确认，
//!    类型上区分"用户确认"与"公开数据"，避免先验被当作实证使用。

//! # ⚠️ 序列化规则：**禁用 internally-tagged 与 untagged 枚举**（含数值字段时）
//!
//! 本 crate 的枚举一律使用 **adjacently tagged**（`tag` + `content`）或 externally tagged。
//! 原因不是风格偏好，而是**实测过的兼容性约束**：
//!
//! `serde_json` 的 `arbitrary_precision` feature 下，JSON 数字在内部被表示为 map 形态，
//! 而 serde 的 internally-tagged / untagged 枚举走 `Content` 缓冲路径 ⇒ **缓冲后的数字
//! 无法反序列化为 `f64`**，报 `invalid type: map, expected f64`。
//!
//! **当前生效范围**：曾有上游 vendor crate 启用该 feature、经 cargo 的全图 feature
//! 合并使整个 workspace 都跑在它下面（历史上就是那样踩的坑）；R1（2026-11-10）摘除
//! 该 crate 后，正式依赖图里已无此 feature。为不丢这条规则的回归覆盖，本 crate 在
//! `[dev-dependencies]` 里**显式启用**它 ⇒ 测试构建跑在最坏情况下，release 不受影响。
//!
//! 更隐蔽的是失败模式：**只在含该 feature 的构建里失败**——故护栏必须显式武装
//! （就是上面的 dev-dependencies），否则规则退化成"靠人遵守"。
//!
//! 故规则是：**凡含数值字段的枚举，必须用 adjacent 或 external tagging。**
//! 违反该规则的新类型不会被现有测试拦住（除非它恰好被 round-trip 测到），
//! 故这条规则仍必须靠人遵守 —— 见 `shangyi/README.md` 的对应章节。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 参数的不确定性分布。缺席表示该参数按点估计处理（须在报告中标注）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// 与 `PayoffForm` 同因改名：`content = "params"` 会与顶层的 `params` 字段
// 在错误信息里撞名，使 `missing field params` 有两种含义。
#[serde(tag = "dist", content = "values", rename_all = "snake_case")]
pub enum Dist {
    Normal { mu: f64, sigma: f64 },
    Uniform { low: f64, high: f64 },
    Triangular { low: f64, mode: f64, high: f64 },
}

/// 参数来源——决定该参数是否可信、是否需要 checkpoint 确认。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamSource {
    /// 用户口头确认（经 checkpoint② 留档）。
    UserConfirmed { checkpoint: String },
    /// 用户接受用于情景计算的假设；保留原始依据，不升级为已观察事实。
    ConfirmedAssumption { checkpoint: String, basis: String },
    /// 公开数据推导（须附来源 URL 或工具调用 ID）。
    PublicData { provenance: String },
    /// 行业先验（**必须**在报告中标注 "borrowed，非本对手实证"）。
    IndustryPrior { prior_ref: String },
    /// 纯假设，尚未确认——不得进入求解。
    Assumed { note: String },
    /// **由引擎派生**的值（抽样点、扫描点）。
    ///
    /// # 为什么它需要自己的变体
    ///
    /// `monte_carlo` 与 `sensitivity_scan` 会**改动参数值再重解**。
    /// 若那些改动后的值沿用原来源（如 `user_confirmed`），报告就会把
    /// 一个抽样点读成"用户确认过的数值"——**来源被伪造了**，
    /// 而报告的整个可信度论证都建立在来源之上。
    ///
    /// 记为 `Derived` 使"这个数是我们自己生成的"在类型层面成立。
    /// 它是**可求解的**（抽样点就是要拿去解的），但报告须标注
    /// "由引擎抽样/扫描产生，非原始来源"。
    Derived { note: String },
}

impl ParamSource {
    /// 是否可用于求解。`Assumed` 不可用，这是铁律 2 的类型级强制。
    pub fn is_solvable(&self) -> bool {
        match self {
            ParamSource::Assumed { .. } => false,
            ParamSource::ConfirmedAssumption { checkpoint, basis } => checkpoint == "cp2" && !basis.trim().is_empty(),
            _ => true,
        }
    }
}

/// 参数表达式：字面量或引用其他参数。求解前由 `eval` 归约为数值区间/点值。
///
/// ⚠️ 用 **adjacently tagged** 而非 untagged：`Literal(f64)` 在 `arbitrary_precision`
/// 下经 untagged 的缓冲路径会反序列化失败（见 crate 文档的序列化规则）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Expr {
    Literal(f64),
    Ref(String),
}

/// 一个带来源与分布的参数。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub value: Expr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dist: Option<Dist>,
    pub source: ParamSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
}

/// 参与者在价值网中的角色（设计文档 §5 参与者确定）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueNetRole {
    Focal,
    Customer,
    Supplier,
    Substitutor,
    Complementor,
}

impl std::fmt::Display for ValueNetRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 中文名进报告（面向中小企业主），英文标识供日志与跨语言读取
        f.write_str(match self {
            ValueNetRole::Focal => "本企业(focal)",
            ValueNetRole::Customer => "顾客(customer)",
            ValueNetRole::Supplier => "供应商(supplier)",
            ValueNetRole::Substitutor => "替代者(substitutor)",
            ValueNetRole::Complementor => "互补者(complementor)",
        })
    }
}

/// 参与者在**本次博弈**中的定位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationToGame {
    // ⚠️ 中文 alias：模型在实测中自发传了「我方」「主要威胁」这类中文值。
    // 加 alias 比让每一次调用都失败更实际——语义相同，而拒绝只是惩罚
    // 一个可理解的命名差异（错误信息还帮不上忙）。
    #[serde(alias = "我方", alias = "本企业", alias = "焦点企业")]
    /// 直接玩家，进入支付矩阵。
    Player,
    /// 环境方：平时不行动，可被引入并改变支付结构（§5.7.2）。
    ExternalActor,
    /// 仅入画像库，不参与本次博弈。
    ContextOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Participant {
    pub id: String,
    pub name: String,
    pub vn_role: ValueNetRole,
    pub relation_to_game: RelationToGame,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
}

/// 信息结构。不完全信息时给出各方类型的先验（贝叶斯类型推断的输入）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Information {
    Complete,
    Incomplete {
        #[serde(default)]
        types: BTreeMap<String, Vec<String>>,
    },
}

/// 博弈的动态形式。重复博弈用 (stage game, discount, horizon) 表示。
///
/// # content 键名不能叫 `params`（**同一个坑踩了第三次**）
///
/// `GameSpec` 顶层有 `params` 字段（参数表）。若某个邻接标记枚举的
/// content 也叫 `params`，serde 的 `missing field \`params\`` 就有了多种含义：
///
/// | 场景 | 实际该做什么 |
/// |---|---|
/// | 顶层缺 `params` | 补参数表 |
/// | `payoffs` 缺 content | 把 `cells` 包进 `values` |
/// | **`dynamics` 缺 content** | **补 `values: {discount: …}`** |
///
/// 本仓已有两处按此改名（`PayoffForm`、`Dist`），而 `Dynamics` **漏了**
/// ——实测代价：一次真实咨询里模型为 `dynamics` 的形状**连续失败 7 次**
/// （依次试 `{"repeated":true}` → `{"form":"repeated"}` →
/// `{"type":"repeated"}` → `{"form":"repeated","type":"repeated"}`），
/// 而每次拿到的错误都是 `missing field params`——**指向了顶层的参数表**，
/// 于是它一直去补那个本来就在的字段。
///
/// 教训：**规则写在注释里不足以防止漏改**。故新增一条结构性测试
/// （`no_tagged_enum_collides_with_top_level_params`），它直接检查
/// 序列化结果的键名——不靠人记规则。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", content = "values", rename_all = "snake_case")]
pub enum Dynamics {
    OneShot,
    Repeated {
        discount: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        horizon: Option<u32>,
    },
}

/// 用户偏好（经 §8.2 校准题产出后注入）。
///
/// # 两个字段都有默认值（实测：模型给不出它们）
///
/// 本结构整体是 `Option<Preference>`（`GameSpec::preference`），
/// 即"可以没有偏好"。而**有偏好但不完整**（如只给 `risk_aversion`）
/// 此前会被拒绝——那是个不必要的要求：
///
/// - `risk_aversion` 缺省 ⇒ **风险中性**（0.0），这是它字面上的中性点
/// - `objective_weights` 缺省 ⇒ **空表**（无目标侧重）
///
/// 实测（run29）：模型连续两次因 `missing field risk_aversion` /
/// `missing field objective_weights` 失败——它在写一个"还没有用户偏好"的
/// 博弈，却被要求必须给全。而没有偏好时应当**整个省略** `preference`，
/// 或给它一个空对象——后者此前是解析错误，现在是"风险中性、无侧重"。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preference {
    /// 风险厌恶系数 ∈ [0,1]（0 = 风险中性，1 = 极度厌恶）。
    #[serde(default)]
    pub risk_aversion: f64,
    /// 多目标侧重（如 `{"利润": 0.7, "份额": 0.3}`）。空表 = 无侧重。
    #[serde(default)]
    pub objective_weights: BTreeMap<String, f64>,
}

/// 收益形式：标准式（矩阵）或扩展式（博弈树）。
///
/// # ⚠️ content 的键名**不能**叫 `params`（实测缺陷）
///
/// 早先写作 `#[serde(tag = "form", content = "params")]`，而 `GameSpec`
/// **顶层也有一个 `params` 字段**（参数表）。于是 serde 的报错
/// `missing field \`params\`` 有了**两种完全不同的含义**：
///
/// | 场景 | 实际该做什么 |
/// |---|---|
/// | 顶层缺 params | 补参数表 |
/// | **payoffs 缺 content** | **把 `cells` 包进 `params`** |
///
/// 实测代价：模型在 21 次调用里把它理解成前者，反复补顶层 `params`，
/// 而真正要改的是 `payoffs` 的形状。错误信息**指向了错误的位置**。
///
/// 改名 `values` 后两种含义不再撞车：报 `missing field values` 就只可能是
/// payoffs 的形状问题。这是"同一字段名承担两种语义"必然带来的诊断困难。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", content = "values", rename_all = "snake_case")]
pub enum PayoffForm {
    /// 每个策略组合下各方收益，值为**参数表达式引用**（非数字）。
    ///
    /// 形状：`{"form":"normal","values":{"cells":{"策略A,策略B":{"玩家id": <Expr>}}}}`
    Normal {
        #[serde(default)]
        cells: BTreeMap<String, BTreeMap<String, Expr>>,
    },
    /// 扩展式由独立子树表示（v0.1 留空，见设计文档 §5.3）。
    Extensive,
}

/// 博弈的统一表示。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameSpec {
    pub game_id: String,
    pub players: Vec<Participant>,
    /// 玩家 id → 策略名列表（v0.1 限定离散策略、2–4 玩家）。
    pub strategies: BTreeMap<String, Vec<String>>,
    pub payoffs: PayoffForm,
    pub params: BTreeMap<String, Param>,
    #[serde(default)]
    pub information: Option<Information>,
    #[serde(default)]
    pub dynamics: Option<Dynamics>,
    /// 非市场参与者（§5.7.2）。行动可引入它们并改变支付结构。
    #[serde(default)]
    pub external_actors: Vec<Participant>,
    #[serde(default)]
    pub preference: Option<Preference>,
    #[serde(default)]
    pub meta: BTreeMap<String, String>,
}

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum ValidationError {
    #[error("博弈必须有 2–4 个直接玩家（player），当前 {0} 个")]
    PlayerCount(usize),
    #[error("玩家 `{0}` 缺少策略列表")]
    MissingStrategies(String),
    #[error("玩家 `{0}` 的策略列表为空")]
    EmptyStrategies(String),
    #[error("参数 `{0}` 尚未确认（source = assumed），不得进入求解——铁律 2")]
    UnconfirmedParam(String),
    #[error("参数 `{0}` 引用了不存在的参数 `{1}`")]
    DanglingParamRef(String, String),
    #[error("players 中出现重复 id：`{0}`")]
    DuplicatePlayer(String),
    /// 支付矩阵缺格（**实测：这一条早该在构建期查出来**）。
    ///
    /// # 为什么它必须在 `validate` 里（而不只是求解时）
    ///
    /// 实测（negotiation/scenario，2026-09-19）：模型建了一份
    /// **cells 全空**的 GameSpec，`gamespec_validate` 却回 `valid: true`——
    /// 因为旧校验只看参数来源、不看矩阵完整性。于是模型带着"已验证"的
    /// 信心进 `solve`，在求解器那里连撞 10 次 `缺少策略组合`，**25 次
    /// `gamespec_validate` 全部无效**（它每次都在验一个自己以为对的东西），
    /// 最终整个会话卡死在 solve，用户拿不到任何结论。
    ///
    /// 这是「校验器撒谎」——比"缺校验"更糟：它给出了一个**错误的肯定**，
    /// 而模型据此排除了"矩阵有问题"这个方向。
    #[error("支付矩阵缺 {0} 个策略组合的格子（如 `{1}`）——求解时才会发现，但那时已浪费整个 solve 阶段")]
    IncompletePayoffCells(usize, String),
    /// 支付矩阵**一个格子都没有**（建了骨架忘了填）。
    ///
    /// 单独一个变体，因为它的修法不同：不是"补几个格"，
    /// 而是"这份 spec 还没写支付"——报错要直接这么说。
    #[error("支付矩阵是**空的**（0/{0} 个策略组合有支付）——这份 spec 建了骨架但还没填支付，无法求解")]
    PayoffCellsAllMissing(usize),
    #[error("策略组合 `{0}` 缺少参与者 `{1}` 的结果数值；未知收益不能用0占位，此模型尚不能进行完整求解")]
    MissingPlayerPayoff(String, String),
    #[error("策略组合 `{0}` 的参与者 `{1}` 引用了未定义参数 `{2}`")]
    DanglingPayoffRef(String, String, String),
}

impl GameSpec {
    /// 求解前的校验。这是铁律 2 的**可执行**落点。
    pub fn validate(&self) -> Result<(), Vec<ValidationError>> {
        let mut errs = Vec::new();

        let players: Vec<&Participant> = self
            .players
            .iter()
            .filter(|p| p.relation_to_game == RelationToGame::Player)
            .collect();

        if !(2..=4).contains(&players.len()) {
            errs.push(ValidationError::PlayerCount(players.len()));
        }

        let mut seen = std::collections::BTreeSet::new();
        for p in &self.players {
            if !seen.insert(p.id.as_str()) {
                errs.push(ValidationError::DuplicatePlayer(p.id.clone()));
            }
        }

        for p in &players {
            match self.strategies.get(&p.id) {
                None => errs.push(ValidationError::MissingStrategies(p.id.clone())),
                Some(s) if s.is_empty() => {
                    errs.push(ValidationError::EmptyStrategies(p.id.clone()))
                }
                Some(_) => {}
            }
        }

        for (name, param) in &self.params {
            if !param.source.is_solvable() {
                errs.push(ValidationError::UnconfirmedParam(name.clone()));
            }
            if let Expr::Ref(target) = &param.value {
                if !self.params.contains_key(target) {
                    errs.push(ValidationError::DanglingParamRef(
                        name.clone(),
                        target.clone(),
                    ));
                }
            }
        }

        // ── 支付矩阵完整性（只对 2–4 人的正规形检查）──
        //
        // ⚠️ 只在**结构健全**时检查：玩家数为 0 或策略缺失时，
        // 下面这套遍历会产出噪声错误（"缺 4 个格子"而真因是没写玩家），
        // 那会淹没真正的第一因。
        if errs.is_empty() && players.len() >= 2 {
            if let PayoffForm::Normal { cells } = &self.payoffs {
                let ids: Vec<&str> = players.iter().map(|p| p.id.as_str()).collect();
                let lists: Vec<&Vec<String>> = ids
                    .iter()
                    .map(|id| self.strategies.get(*id).expect("上面已查过缺失"))
                    .collect();
                // 笛卡尔积的规模有界（每玩家策略数上限 20，玩家 2–4）
                let mut missing: Vec<String> = Vec::new();
                let mut total = 0usize;
                for combo in cartesian(&lists) {
                    total += 1;
                    let key = combo.join(",");
                    if let Some(cell) = cells.get(&key).or_else(|| cells.get(&combo.join("|"))) {
                        for id in &ids {
                            match cell.get(*id) {
                                None => errs.push(ValidationError::MissingPlayerPayoff(key.clone(),(*id).to_owned())),
                                Some(Expr::Ref(target)) if !self.params.contains_key(target) => errs.push(ValidationError::DanglingPayoffRef(key.clone(),(*id).to_owned(),target.clone())),
                                _ => {},
                            }
                        }
                    } else {
                        missing.push(key);
                    }
                }
                if !missing.is_empty() {
                    // 全空（`missing == total`）是**最常见**的一种——
                    // 模型建骨架后忘了填。它单独一句话说清，
                    // 而不是让读者从"缺 4 个"里推断"一个都没填"。
                    let e = if missing.len() == total {
                        ValidationError::PayoffCellsAllMissing(total)
                    } else {
                        ValidationError::IncompletePayoffCells(missing.len(), missing[0].clone())
                    };
                    errs.push(e);
                }
            }
        }

        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    /// 该 spec 是否**具备可求解的形状**（支付矩阵齐全、结构完整）。
    ///
    /// 与 `validate()` 的分工：`validate` 是铁律的可执行落点（返回全部错误），
    /// 本方法只回答"能不能进求解"，供 `gamespec_validate` 工具判断
    /// **是否该同时报出适配层的问题**。
    pub fn solvable_shape(&self) -> bool {
        self.validate().is_ok()
    }

    /// 本次博弈的直接玩家 id（用于求解器规模判定）。
    pub fn player_ids(&self) -> Vec<&str> {
        self.players
            .iter()
            .filter(|p| p.relation_to_game == RelationToGame::Player)
            .map(|p| p.id.as_str())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> GameSpec {
        let mut params = BTreeMap::new();
        params.insert(
            "P1".into(),
            Param {
                name: "P1".into(),
                value: Expr::Literal(42.0),
                dist: Some(Dist::Normal { mu: 42.0, sigma: 8.0 }),
                source: ParamSource::UserConfirmed {
                    checkpoint: "cp2".into(),
                },
                unit: Some("万元".into()),
            },
        );
        let mut strategies = BTreeMap::new();
        strategies.insert("focal".to_string(), vec!["维持价".into(), "降价10%".into()]);
        strategies.insert("comp_a".to_string(), vec!["跟随".into(), "不动".into()]);
        GameSpec {
            game_id: "case-test".into(),
            players: vec![
                Participant {
                    id: "focal".into(),
                    name: "本企业".into(),
                    vn_role: ValueNetRole::Focal,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "comp_a".into(),
                    name: "竞争对手A".into(),
                    vn_role: ValueNetRole::Substitutor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: Some("opp-comp_a".into()),
                },
            ],
            strategies,
            payoffs: PayoffForm::Normal {
                // ⚠️ 必须**填满** 2×2：本夹具早先给的是空表，而校验只看参数来源
                // ⇒ 它"通过"了。那正是真实咨询里踩到的那个坑
                // （`IncompletePayoffCells` 的注释详述）——**夹具自己有病**，
                // 于是它验证的东西与真实路径不一致。
                cells: {
                    let mut c = BTreeMap::new();
                    for (f, a) in [("维持价", 3.0), ("降价10%", 1.0)] {
                        for (k, b) in [("跟随", 0.0), ("不动", 5.0)] {
                            let mut m = BTreeMap::new();
                            m.insert("focal".to_string(), Expr::Literal(a));
                            m.insert("comp_a".to_string(), Expr::Literal(b));
                            c.insert(format!("{f},{k}"), m);
                        }
                    }
                    c
                },
            },
            params,
            information: None,
            dynamics: Some(Dynamics::Repeated {
                discount: 0.9,
                horizon: None,
            }),
            external_actors: vec![Participant {
                id: "regulator".into(),
                name: "监管".into(),
                vn_role: ValueNetRole::Focal,
                relation_to_game: RelationToGame::ExternalActor,
                profile_ref: None,
            }],
            preference: None,
            meta: BTreeMap::new(),
        }
    }

    #[test]
    fn minimal_spec_validates() {
        assert!(minimal().validate().is_ok());
    }

    #[test]
    fn assumed_param_blocks_solving() {
        let mut g = minimal();
        g.params.get_mut("P1").unwrap().source = ParamSource::Assumed {
            note: "未确认".into(),
        };
        let errs = g.validate().unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnconfirmedParam(p) if p == "P1")),
            "铁律 2 未生效：assumed 参数应阻止求解"
        );
    }

    #[test]
    fn external_actor_not_counted_as_player() {
        let g = minimal();
        assert_eq!(g.player_ids(), vec!["focal", "comp_a"]);
        assert_eq!(g.external_actors.len(), 1);
    }

    #[test]
    fn dangling_param_ref_detected() {
        let mut g = minimal();
        g.params.get_mut("P1").unwrap().value = Expr::Ref("NOT_EXIST".into());
        let errs = g.validate().unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::DanglingParamRef(_, r) if r == "NOT_EXIST")));
    }

    #[test]
    fn player_count_out_of_range_rejected() {
        let mut g = minimal();
        g.players.retain(|p| p.id == "focal");
        let errs = g.validate().unwrap_err();
        assert!(errs
            .iter()
            .any(|e| matches!(e, ValidationError::PlayerCount(1))));
    }
}


/// 策略名列表的笛卡尔积（按玩家顺序）。
///
/// 规模有界：策略数上限 20/玩家、玩家 2–4 ⇒ 最多 160000 组合，
/// 而 `validate` 只在构建期调用一次，可接受。
fn cartesian<'a>(lists: &[&'a Vec<String>]) -> Vec<Vec<&'a str>> {
    let mut out: Vec<Vec<&str>> = vec![Vec::new()];
    for list in lists {
        let mut next = Vec::with_capacity(out.len() * list.len());
        for prefix in &out {
            for item in list.iter() {
                let mut v = prefix.clone();
                v.push(item.as_str());
                next.push(v);
            }
        }
        out = next;
    }
    out
}

/// 支付矩阵完整性的回归测试。
///
/// 实测依据（negotiation/scenario，2026-09-19）：模型建了一份 **cells 全空**
/// 的 spec，`gamespec_validate` 回 `valid: true`，于是它带着"已验证"的信心
/// 进 solve，连撞 10 次"缺少策略组合"、25 次 validate 全部无效，
/// 整个会话卡死在 solve。**校验器给出错误的肯定**比缺校验更糟。
#[cfg(test)]
mod payoff_completeness_tests {
    use super::*;

    fn spec_with_cells(cells: BTreeMap<String, BTreeMap<String, Expr>>) -> GameSpec {
        let mut strategies = BTreeMap::new();
        strategies.insert("a".to_string(), vec!["x".into(), "y".into()]);
        strategies.insert("b".to_string(), vec!["p".into(), "q".into()]);
        let mk = |id: &str| Participant {
            id: id.into(),
            name: id.into(),
            vn_role: ValueNetRole::Focal,
            relation_to_game: RelationToGame::Player,
            profile_ref: None,
        };
        GameSpec {
            game_id: "t".into(),
            players: vec![mk("a"), mk("b")],
            strategies,
            payoffs: PayoffForm::Normal { cells },
            params: BTreeMap::new(),
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: BTreeMap::new(),
        }
    }

    fn cell() -> BTreeMap<String, Expr> {
        let mut m = BTreeMap::new();
        m.insert("a".to_string(), Expr::Literal(1.0));
        m.insert("b".to_string(), Expr::Literal(2.0));
        m
    }

    /// 全空 ⇒ **必须被拒**，且报错说"是空的"（而不是"缺 4 个"）。
    #[test]
    fn all_missing_cells_is_rejected_as_empty() {
        let g = spec_with_cells(BTreeMap::new());
        let errs = g.validate().unwrap_err();
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::PayoffCellsAllMissing(4))),
            "全空应报 PayoffCellsAllMissing(4)，实得 {errs:?}"
        );
    }

    /// 缺一格 ⇒ 被拒，且报错点名**缺哪个组合**。
    #[test]
    fn partial_cells_reports_the_missing_combo() {
        let mut cells = BTreeMap::new();
        for k in ["x,p", "x,q", "y,p"] {
            cells.insert(k.to_string(), cell());
        }
        let g = spec_with_cells(cells);
        let errs = g.validate().unwrap_err();
        let hit = errs.iter().find_map(|e| match e {
            ValidationError::IncompletePayoffCells(1, combo) => Some(combo.clone()),
            _ => None,
        });
        assert_eq!(
            hit.as_deref(),
            Some("y,q"),
            "应点名缺的那个组合，实得 {errs:?}"
        );
    }

    /// 填满 ⇒ 通过（不误杀）。`|` 分隔的写法也认。
    #[test]
    fn existing_cells_still_require_each_players_payoff_and_defined_refs() {
        let mut cells=BTreeMap::new();
        for key in ["x,p","x,q","y,p","y,q"] {cells.insert(key.to_owned(),cell());}
        cells.get_mut("x,q").unwrap().remove("b");
        let missing=spec_with_cells(cells.clone()).validate().unwrap_err();
        assert!(missing.contains(&ValidationError::MissingPlayerPayoff("x,q".into(),"b".into())));
        cells.get_mut("x,q").unwrap().insert("b".into(),Expr::Ref("unknown_income".into()));
        let dangling=spec_with_cells(cells).validate().unwrap_err();
        assert!(dangling.contains(&ValidationError::DanglingPayoffRef("x,q".into(),"b".into(),"unknown_income".into())));
    }

    #[test]
    fn complete_cells_pass() {
        let mut cells = BTreeMap::new();
        for k in ["x,p", "x,q", "y,p", "y,q"] {
            cells.insert(k.to_string(), cell());
        }
        assert!(spec_with_cells(cells).validate().is_ok());

        let mut cells2 = BTreeMap::new();
        for k in ["x|p", "x|q", "y|p", "y|q"] {
            cells2.insert(k.to_string(), cell());
        }
        assert!(
            spec_with_cells(cells2).validate().is_ok(),
            "`|` 分隔（适配层已兼容）不该被误判为缺格"
        );
    }
}

#[cfg(test)]
mod confirmed_assumption_tests {
    use super::*;
    #[test]
    fn accepted_assumption_preserves_basis_and_is_not_user_fact() {
        let source=ParamSource::ConfirmedAssumption{checkpoint:"cp2".into(),basis:"未核实报价，测试情景".into()};
        let value=serde_json::to_value(&source).unwrap();
        assert_eq!(value["kind"],"confirmed_assumption");
        assert!(source.is_solvable());
        let restored:ParamSource=serde_json::from_value(value).unwrap();assert_eq!(source,restored);
        assert!(!ParamSource::ConfirmedAssumption{checkpoint:"cp2".into(),basis:" ".into()}.is_solvable());
    }
}

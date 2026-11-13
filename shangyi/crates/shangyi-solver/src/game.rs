//! 求解器面向的标准式博弈表示，以及与 `shangyi-gamespec` 的适配。
//!
//! 为什么需要独立的面向求解器的类型：`GameSpec` 面向"参数带来源、支付是参数表达式"
//! 的领域建模（L0），其 `PayoffForm::Normal.cells` 的语义是**参数表达式引用**；
//! 求解器需要的是"每个纯策略组合上的定值支付表 + 快速索引"。二者职责不同，
//! 故此处定义 `NormalFormGame`，并提供 `NormalFormGame::from_gamespec` 适配器
//! （`GameSpec` 的参数一旦求值确定，即归约为本类型）。

use std::collections::BTreeMap;

use shangyi_gamespec::{Expr, GameSpec, PayoffForm, RelationToGame};

use crate::rat::Rational;

/// 求解器的数值容差（字段名与 `09-solver-test-vectors.md` §7 的 `Tolerance` 一致）。
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    /// 混合策略每个分量的绝对容差（浮点路径）。
    pub prob: f64,
    /// `|Σ_j σ_i[j] − 1|`。
    pub simplex: f64,
    /// 允许的负分量下限：σ_i[j] ≥ −τ_neg。
    pub neg: f64,
    /// 期望支付绝对容差。
    pub payoff: f64,
    /// 最佳反应残余（BR_gap）阈值。
    pub br_gap: f64,
    /// true ⇒ 忽略上面全部，用**精确有理数判等**（容差 0）。
    pub exact_rational: bool,
}

impl Tolerance {
    /// `09-solver-test-vectors.md` §2.1 的绝对常量 + 精确有理数路径。
    pub const GOLDEN: Tolerance = Tolerance {
        prob: 1e-9,
        simplex: 1e-9,
        neg: 1e-12,
        payoff: 1e-9,
        br_gap: 1e-9,
        exact_rational: true,
    };

    /// 尺度归一阈值（`10-nashpy-oracle.md` §5.2）：
    /// `scale = max(1, max|A|, max|B|)`，`τ_res = τ_payoff = 1e-9·scale`。
    ///
    /// 黄金用例以 `scale = max(1, max|u|)` 调用它时退化为 §2.1 的绝对量级
    /// （六个用例 `max|u| = 8`，`1e-9·8` 与 `1e-9` 同量级，结论不变）——
    /// 这样全工程只有一套容差函数，避免两套口径在 CI 里互相打架。
    pub fn for_scale(scale: f64) -> Tolerance {
        let s = if scale > 1.0 { scale } else { 1.0 };
        Tolerance {
            prob: 1e-9 * s,
            simplex: 1e-9 * s,
            neg: 1e-12 * s,
            payoff: 1e-9 * s,
            br_gap: 1e-9 * s,
            exact_rational: false,
        }
    }

    /// 容差的精确有理上界（精确路径使用；十进制常量直接写成分数，不经浮点）。
    pub fn exact_br_gap(&self) -> Rational {
        Rational::new(1, 1_000_000_000)
    }
}

/// 博弈定义的构造/校验错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GameError {
    /// 玩家数不在 2..=4。
    PlayerCount(usize),
    /// 某玩家策略数为 0。
    EmptyStrategies(usize),
    /// 同一 profile 被重复赋值。
    DuplicateProfile(String),
    /// 支付表缺格。
    MissingProfile(String),
    /// profile 长度与玩家数不符。
    ProfileArity { index: usize, expect: usize, got: usize },
    /// 策略下标越界。
    StrategyIndex { index: usize, got: usize, count: usize },
    /// `GameSpec` 适配失败（含原因，便于定位）。
    GameSpec(String),
}

impl std::fmt::Display for GameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameError::PlayerCount(n) => write!(f, "玩家数 {n} 不在 2..=4"),
            GameError::EmptyStrategies(p) => write!(f, "玩家 {p} 没有策略"),
            GameError::DuplicateProfile(k) => write!(f, "重复的 profile：{k}"),
            GameError::MissingProfile(k) => write!(f, "缺少 profile 的支付：{k}"),
            GameError::ProfileArity { index, expect, got } => {
                write!(f, "第 {index} 个 profile 长度 {got}，应为 {expect}")
            }
            GameError::StrategyIndex { index, got, count } => {
                write!(f, "profile 第 {index} 项 = {got} 越界（该玩家有 {count} 个策略）")
            }
            GameError::GameSpec(m) => write!(f, "GameSpec 适配失败：{m}"),
        }
    }
}

/// 标准式博弈（离散策略）。
///
/// 支付以 `payoffs[p][idx]` 存储，`idx` 为 profile 的行主序线性下标
/// （玩家 0 变化最慢）：`idx = ((i₀·n₁ + i₁)·n₂ + …)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalFormGame {
    strategy_counts: Vec<usize>,
    /// 玩家 p 在每个纯策略组合上的支付。
    payoffs: Vec<Vec<Rational>>,
}

impl NormalFormGame {
    /// 建一个所有支付为 0 的博弈。
    pub fn new(strategy_counts: Vec<usize>) -> Result<Self, GameError> {
        if !(2..=4).contains(&strategy_counts.len()) {
            return Err(GameError::PlayerCount(strategy_counts.len()));
        }
        for (p, &c) in strategy_counts.iter().enumerate() {
            if c == 0 {
                return Err(GameError::EmptyStrategies(p));
            }
        }
        let total: usize = strategy_counts.iter().product();
        let players = strategy_counts.len();
        Ok(NormalFormGame { strategy_counts, payoffs: vec![vec![Rational::ZERO; total]; players] })
    }

    /// 从 `(profile, 各玩家支付)` 列表构造（黄金用例的直接形态）。
    pub fn from_profiles(
        strategy_counts: Vec<usize>,
        cells: &[(Vec<usize>, Vec<i64>)],
    ) -> Result<Self, GameError> {
        let mut g = NormalFormGame::new(strategy_counts)?;
        let mut seen: BTreeMap<usize, bool> = BTreeMap::new();
        for (index, (profile, values)) in cells.iter().enumerate() {
            if profile.len() != g.players() {
                return Err(GameError::ProfileArity {
                    index,
                    expect: g.players(),
                    got: profile.len(),
                });
            }
            for (p, &s) in profile.iter().enumerate() {
                let count = g.strategy_count(p);
                if s >= count {
                    return Err(GameError::StrategyIndex { index: p, got: s, count });
                }
            }
            if values.len() != g.players() {
                return Err(GameError::ProfileArity {
                    index: values.len(),
                    expect: g.players(),
                    got: values.len(),
                });
            }
            let idx = g.profile_index(profile);
            if seen.insert(idx, true).is_some() {
                return Err(GameError::DuplicateProfile(format!("{profile:?}")));
            }
            for (p, &v) in values.iter().enumerate() {
                g.payoffs[p][idx] = Rational::from_i64(v);
            }
        }
        // 完整性：所有格子都必须被显式赋值（否则静默的 0 会被当成真支付）。
        let total: usize = g.strategy_counts.iter().product();
        if seen.len() != total {
            for idx in 0..total {
                if !seen.contains_key(&idx) {
                    return Err(GameError::MissingProfile(format!(
                        "{:?}",
                        g.profile_from_index(idx)
                    )));
                }
            }
        }
        Ok(g)
    }

    /// 2 人博弈的便利构造：`row[i][j]` 为行玩家支付，`col[i][j]` 为列玩家支付。
    pub fn from_bimatrix(row: &[Vec<i64>], col: &[Vec<i64>]) -> Result<Self, GameError> {
        assert_eq!(row.len(), col.len(), "from_bimatrix: 行数不一致");
        let m = row.len();
        let n = if m == 0 { 0 } else { row[0].len() };
        let mut cells = Vec::with_capacity(m * n);
        for i in 0..m {
            assert_eq!(row[i].len(), n, "from_bimatrix: 列数不一致");
            assert_eq!(col[i].len(), n, "from_bimatrix: 列数不一致");
            for j in 0..n {
                cells.push((vec![i, j], vec![row[i][j], col[i][j]]));
            }
        }
        NormalFormGame::from_profiles(vec![m, n], &cells)
    }

    pub fn players(&self) -> usize {
        self.strategy_counts.len()
    }

    pub fn strategy_count(&self, player: usize) -> usize {
        self.strategy_counts[player]
    }

    /// 全部纯策略组合数。
    pub fn profile_count(&self) -> usize {
        self.strategy_counts.iter().product()
    }

    /// 某个纯策略组合的线性下标（行主序，玩家 0 最慢）。
    pub fn profile_index(&self, profile: &[usize]) -> usize {
        debug_assert_eq!(profile.len(), self.players());
        let mut idx = 0usize;
        for (p, &s) in profile.iter().enumerate() {
            idx = idx * self.strategy_count(p) + s;
        }
        idx
    }

    /// 线性下标还原为 profile。
    pub fn profile_from_index(&self, mut idx: usize) -> Vec<usize> {
        let mut profile = vec![0usize; self.players()];
        for p in (0..self.players()).rev() {
            let c = self.strategy_count(p);
            profile[p] = idx % c;
            idx /= c;
        }
        profile
    }

    pub fn payoff_at(&self, player: usize, index: usize) -> Rational {
        self.payoffs[player][index]
    }

    pub fn payoff(&self, player: usize, profile: &[usize]) -> Rational {
        self.payoff_at(player, self.profile_index(profile))
    }

    pub fn set_payoff(&mut self, player: usize, profile: &[usize], value: Rational) {
        let idx = self.profile_index(profile);
        self.payoffs[player][idx] = value;
    }

    /// 最大绝对支付（尺度归一的 `scale` 用）。
    pub fn max_abs_payoff(&self) -> Rational {
        self.payoffs
            .iter()
            .flat_map(|v| v.iter())
            .map(|x| x.abs())
            .max()
            .unwrap_or(Rational::ZERO)
    }

    /// 是否为 2 人**对称**博弈（列玩家支付矩阵 = 行玩家支付矩阵的转置）。
    ///
    /// 对称性决定 ESS 的口径：对称博弈用单种群读法，非对称博弈用两种群读法。
    pub fn is_symmetric(&self) -> bool {
        if self.players() != 2 {
            return false;
        }
        let (m, n) = (self.strategy_count(0), self.strategy_count(1));
        if m != n {
            return false;
        }
        for i in 0..m {
            for j in 0..n {
                if self.payoff(0, &[i, j]) != self.payoff(1, &[j, i]) {
                    return false;
                }
            }
        }
        true
    }

    /// 给定其他玩家的混合策略时，玩家 `player` 各纯策略的期望支付向量。
    ///
    /// `opponents[k]` 是对手列表中第 k 个对手的混合策略（长度 = 其策略数），
    /// 对手顺序 = 除去 `player` 后按玩家下标升序。
    pub fn pure_payoffs_against(&self, player: usize, opponents: &[Vec<Rational>]) -> Vec<Rational> {
        let opponents_ids: Vec<usize> =
            (0..self.players()).filter(|&p| p != player).collect();
        debug_assert_eq!(opponents.len(), opponents_ids.len());
        for (k, &op) in opponents_ids.iter().enumerate() {
            debug_assert_eq!(opponents[k].len(), self.strategy_count(op));
        }
        let mut out = vec![Rational::ZERO; self.strategy_count(player)];
        for profile in self.iter_profiles() {
            let mut weight = Rational::ONE;
            for (k, &op) in opponents_ids.iter().enumerate() {
                weight = weight * opponents[k][profile[op]];
            }
            if weight.is_zero() {
                continue;
            }
            let idx = self.profile_index(&profile);
            out[profile[player]] += weight * self.payoff_at(player, idx);
        }
        out
    }

    /// 全部纯策略组合的迭代器（行主序）。
    pub fn iter_profiles(&self) -> ProfileIter<'_> {
        ProfileIter { game: self, idx: 0 }
    }

    /// 从 `GameSpec` 适配。
    ///
    /// 约定（`GameSpec::payoffs = PayoffForm::Normal { cells }`）：
    /// - 外层键 = **纯策略组合键**：各玩家策略名按玩家顺序以 `,` 连接（兼容 `|` 分隔）；
    /// - 内层键 = **玩家 id**，值 = 该玩家在此组合下的支付表达式（`Expr`）。
    ///
    /// 参数求值：`Expr::Literal(v)` 直接取（要求 `v` 为整数，保证精确路径；非整数则
    /// 按二进制表示精确保留）；`Expr::Ref(name)` 递归解析 `params[name].value`。
    /// 任何 `ParamSource::Assumed` 参数都会被拒绝（**铁律 2** 的求解侧落点——
    /// 该约束在 `GameSpec::validate` 已有一份，这里再挡一次是因为本适配器可被直接调用）。
    pub fn from_gamespec(spec: &GameSpec) -> Result<Self, GameError> {
        spec.validate().map_err(|errs| {
            GameError::GameSpec(
                errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("；"),
            )
        })?;

        let player_ids: Vec<String> = spec
            .players
            .iter()
            .filter(|p| p.relation_to_game == RelationToGame::Player)
            .map(|p| p.id.clone())
            .collect();
        let counts: Vec<usize> = player_ids
            .iter()
            .map(|id| spec.strategies.get(id).map(|v| v.len()).unwrap_or(0))
            .collect();
        let mut game = NormalFormGame::new(counts)?;

        let PayoffForm::Normal { cells } = &spec.payoffs else {
            return Err(GameError::GameSpec(
                "payoffs.form = extensive：扩展式求解不在本 crate 范围（待办）".into(),
            ));
        };

        let resolve = |name: &str| -> Result<Rational, GameError> { resolve_param(spec, name, 0) };

        let profiles: Vec<Vec<usize>> = game.iter_profiles().collect();
        for profile in profiles {
            let names: Vec<&str> = player_ids
                .iter()
                .enumerate()
                .map(|(p, id)| {
                    let list = spec.strategies.get(id).expect("counts 已校验非空");
                    list[profile[p]].as_str()
                })
                .collect();
            let keys = [names.join(","), names.join("|")];
            let Some(entry) = keys
                .iter()
                .find_map(|k| cells.get(k.as_str()))
            else {
                return Err(GameError::GameSpec(format!(
                    "缺少策略组合 `{}` 的支付（cells 外层键应为策略名按玩家顺序以 `,` 连接；已尝试 {:?}）",
                    names.join(","),
                    keys
                )));
            };
            for (p, id) in player_ids.iter().enumerate() {
                let expr = entry.get(id.as_str()).ok_or_else(|| {
                    GameError::GameSpec(format!(
                        "策略组合 `{}` 缺少玩家 `{id}` 的支付（cells 内层键应为玩家 id）",
                        names.join(",")
                    ))
                })?;
                let value = match expr {
                    Expr::Literal(v) => f64_to_exact_rational(*v),
                    Expr::Ref(target) => resolve(target)?,
                };
                game.set_payoff(p, &profile, value);
            }
        }
        Ok(game)
    }
}

/// 解析参数引用（递归，带深度上限防环）。
fn resolve_param(spec: &GameSpec, name: &str, depth: usize) -> Result<Rational, GameError> {
    if depth > 32 {
        return Err(GameError::GameSpec(format!("参数引用链过深或成环（从 `{name}` 起）")));
    }
    let param = spec
        .params
        .get(name)
        .ok_or_else(|| GameError::GameSpec(format!("支付表达式引用了不存在的参数 `{name}`")))?;
    if !param.source.is_solvable() {
        return Err(GameError::GameSpec(format!(
            "参数 `{name}` 的 source 为 assumed，不得进入求解（铁律 2）"
        )));
    }
    match &param.value {
        Expr::Literal(v) => Ok(f64_to_exact_rational(*v)),
        Expr::Ref(target) => resolve_param(spec, target, depth + 1),
    }
}

/// `f64` → 精确有理数（二进制表示精确转换，不引入额外舍入）。
/// `f64` → 精确有理数，**按十进制解释**。
///
/// # 为什么不是"取 f64 的精确值"（这是一处实测出来的关键设计）
///
/// 直接取 IEEE 754 的精确值得到的分母极大：
/// `9.9` 的精确值是 `5573204538870989/2^49`——因为 0.9 在二进制里是无限循环小数。
/// 而高斯消元的除法会把分母**相乘**，两个 `2^50` 相乘即 `2^100`，
/// 再来一次就超过 `i128` 的 `2^127`。
///
/// **实测边界**（`tests/exact_path_limits.rs`，修复前）：
///
/// | 玩家数 | 整数 | 1 位小数 | 2 位小数 | 3 位小数 |
/// |---|---|---|---|---|
/// | 2 人 | ✓ | **✗ panic** | ✓ | **✗ panic** |
/// | 3 人 | ✓ | ✗ 溢出 | ✗ 溢出 | ✗ 溢出 |
///
/// 即 **2 人博弈都不安全**，这与设计文档"支持枚举 n≤4"的预期差距很大。
///
/// # 十进制解释的正当性
///
/// 用户写 "9.9" 时，意思是 **9.9 元**（十进制），不是它的 IEEE 754 近似。
/// 按十进制解释：
/// - `9.9` → `99/10`（分母 4 bit，而非 50 bit）
/// - `3.513` → `3513/1000`（分母 10 bit）
/// - `0.1 + 0.2 = 0.30000000000000004` → `7500000000000001/2.5e16`
///   —— **浮点误差依然可见**，不会被"抹平"
///
/// 用 `{}`（Rust 的最短往返表示，且不用科学计数法）而非自写舍入：
/// 它保证 `parse(display(v)) == v`，即**不丢信息**。
pub fn f64_to_exact_rational(v: f64) -> Rational {
    if !v.is_finite() {
        // 保持原有的硬约束（非有限值进不了博弈），但改为**可定位的** panic 理由
        panic!("支付表达式含非有限值：{v}（NaN/Inf 不能进支付矩阵）");
    }
    if v == 0.0 {
        return Rational::ZERO;
    }

    // 走十进制：{v} 是最短往返表示，解析它得到"用户看到的那个数"。
    if let Some(r) = decimal_str_to_rational(&format!("{v}")) {
        return r;
    }

    // 回退：`{}` 的形态出乎意料（理论上不会发生），退回二进制精确转换。
    // **保留这条路径**是为了"宁可分母大，也不要算错数"。
    binary_exact_rational(v)
}

/// 把十进制定点字符串（如 `"9.9"`、`"-123.456"`）转成精确有理数。
///
/// **不支持科学计数法**：调用方用 `{}` 格式化，Rust 的浮点 Display
/// 不产生科学计数法（`1e20` 会打印成 `100000000000000000000`）。
/// 遇到不认识的形态返回 `None`，由调用方回退——**不猜**。
fn decimal_str_to_rational(s: &str) -> Option<Rational> {
    let s = s.trim();
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if body.is_empty() {
        return None;
    }
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }

    // 分子 = int_part 与 frac_part 拼成的整数；分母 = 10^len(frac_part)
    let digits = format!("{int_part}{frac_part}");
    let num: i128 = digits.parse().ok()?;
    let num = if neg { -num } else { num };
    let scale = frac_part.len() as u32;
    if scale == 0 {
        return Some(Rational::new(num, 1));
    }
    let den = 10i128.checked_pow(scale)?;
    Some(Rational::new(num, den))
}

/// 回退路径：取 `f64` 的**精确二进制值**（分母可能是 2^50 量级）。
fn binary_exact_rational(v: f64) -> Rational {
    let bits = v.to_bits();
    let sign = if bits >> 63 == 0 { 1i128 } else { -1i128 };
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let mantissa = if exponent == 0 {
        bits & 0xf_ffff_ffff_ffff
    } else {
        (bits & 0xf_ffff_ffff_ffff) | 0x10_0000_0000_0000
    } as i128;
    let e = exponent - 1075;
    if e >= 0 {
        Rational::new(sign * mantissa * (1i128 << e.min(63)), 1)
    } else {
        Rational::new(sign * mantissa, 1i128 << (-e).min(127))
    }
}

/// 纯策略组合迭代器。
pub struct ProfileIter<'a> {
    game: &'a NormalFormGame,
    idx: usize,
}

impl Iterator for ProfileIter<'_> {
    type Item = Vec<usize>;
    fn next(&mut self) -> Option<Vec<usize>> {
        if self.idx >= self.game.profile_count() {
            return None;
        }
        let profile = self.game.profile_from_index(self.idx);
        self.idx += 1;
        Some(profile)
    }
}

/// 从纯策略组合生成全部等大小支撑组合（`combinations`）。
pub fn combinations(n: usize, k: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    if k == 0 || k > n {
        return out;
    }
    let mut cur: Vec<usize> = (0..k).collect();
    loop {
        out.push(cur.clone());
        // 找最右的可提升位
        let mut i = k;
        while i > 0 {
            i -= 1;
            if cur[i] != i + n - k {
                cur[i] += 1;
                for j in i + 1..k {
                    cur[j] = cur[j - 1] + 1;
                }
                break;
            }
            if i == 0 {
                return out;
            }
        }
    }
}

/// 全支撑组合（每个玩家 2^n − 1 个非空子集）。
pub fn all_supports(count: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    for mask in 1usize..(1usize << count) {
        let s: Vec<usize> = (0..count).filter(|i| mask & (1 << i) != 0).collect();
        out.push(s);
    }
    out
}

/// 判定支付表是否可精确表示（黄金用例全部是整数支付）。
pub fn is_integral(r: Rational) -> bool {
    r.denom() == 1
}

/// 供上层报告使用的、对人类可读的博弈摘要。
pub fn describe(game: &NormalFormGame) -> String {
    format!(
        "{} 人博弈，策略数 {:?}",
        game.players(),
        (0..game.players()).map(|p| game.strategy_count(p)).collect::<Vec<_>>()
    )
}

/// `ParamSource` 的再导出，便于调用方做"铁律 2"检查而不必依赖 gamespec。
pub use shangyi_gamespec::ParamSource as PayoffParamSource;

#[cfg(test)]
mod tests {
    use super::*;

    fn bos() -> NormalFormGame {
        NormalFormGame::from_bimatrix(
            &[vec![3, 0], vec![0, 2]],
            &[vec![2, 0], vec![0, 3]],
        )
        .unwrap()
    }

    #[test]
    fn profile_index_roundtrip_3p() {
        let g = NormalFormGame::new(vec![2, 3, 2]).unwrap();
        assert_eq!(g.profile_count(), 12);
        for idx in 0..g.profile_count() {
            let p = g.profile_from_index(idx);
            assert_eq!(g.profile_index(&p), idx);
        }
    }

    #[test]
    fn profile_index_is_row_major() {
        let g = NormalFormGame::new(vec![2, 3]).unwrap();
        assert_eq!(g.profile_index(&[0, 0]), 0);
        assert_eq!(g.profile_index(&[0, 2]), 2);
        assert_eq!(g.profile_index(&[1, 0]), 3);
        assert_eq!(g.profile_index(&[1, 2]), 5);
    }

    #[test]
    fn expected_payoffs_against_mixed() {
        let g = bos();
        // 对手（列玩家）以 (1/2, 1/2) 混合：行玩家 PF 得 3·1/2 = 3/2，Ballet 得 2·1/2 = 1
        let v = g.pure_payoffs_against(
            0,
            &[vec![Rational::new(1, 2), Rational::new(1, 2)]],
        );
        assert_eq!(v, vec![Rational::new(3, 2), Rational::ONE]);
    }

    #[test]
    fn symmetry_detection() {
        assert!(!bos().is_symmetric());
        let stag = NormalFormGame::from_bimatrix(
            &[vec![5, 0], vec![4, 2]],
            &[vec![5, 4], vec![0, 2]],
        )
        .unwrap();
        assert!(stag.is_symmetric());
    }

    #[test]
    fn missing_cell_is_rejected() {
        let cells = vec![
            (vec![0, 0], vec![1, 1]),
            (vec![0, 1], vec![0, 0]),
            (vec![1, 0], vec![0, 0]),
            // 缺 (1,1)
        ];
        assert!(matches!(
            NormalFormGame::from_profiles(vec![2, 2], &cells),
            Err(GameError::MissingProfile(_))
        ));
    }

    #[test]
    fn duplicate_cell_is_rejected() {
        let cells = vec![
            (vec![0, 0], vec![1, 1]),
            (vec![0, 0], vec![1, 1]),
            (vec![0, 1], vec![0, 0]),
            (vec![1, 0], vec![0, 0]),
            (vec![1, 1], vec![0, 0]),
        ];
        assert!(matches!(
            NormalFormGame::from_profiles(vec![2, 2], &cells),
            Err(GameError::DuplicateProfile(_))
        ));
    }

    #[test]
    fn combinations_are_lexicographic() {
        assert_eq!(combinations(3, 2), vec![vec![0, 1], vec![0, 2], vec![1, 2]]);
        assert_eq!(combinations(2, 2), vec![vec![0, 1]]);
        assert_eq!(combinations(2, 1), vec![vec![0], vec![1]]);
        assert!(combinations(2, 3).is_empty());
    }

    #[test]
    fn all_supports_covers_nonempty_subsets() {
        assert_eq!(
            all_supports(3),
            vec![
                vec![0],
                vec![1],
                vec![0, 1],
                vec![2],
                vec![0, 2],
                vec![1, 2],
                vec![0, 1, 2]
            ]
        );
    }

    #[test]
    fn f64_exact_conversion() {
        assert_eq!(f64_to_exact_rational(0.5), Rational::new(1, 2));
        assert_eq!(f64_to_exact_rational(-2.0), Rational::from_i64(-2));
        assert_eq!(f64_to_exact_rational(0.25) + f64_to_exact_rational(0.5), Rational::new(3, 4));
    }

    #[test]
    fn gamespec_adapter_reads_cells() {
        use shangyi_gamespec::{Participant, ValueNetRole};
        let mut strategies = std::collections::BTreeMap::new();
        strategies.insert("focal".to_string(), vec!["H".to_string(), "G".to_string()]);
        strategies.insert("rival".to_string(), vec!["H".to_string(), "G".to_string()]);
        let mut cells: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Expr>> =
            std::collections::BTreeMap::new();
        let table = [("H,H", 5, 5), ("H,G", 0, 4), ("G,H", 4, 0), ("G,G", 2, 2)];
        for (key, a, b) in table {
            let mut m = std::collections::BTreeMap::new();
            m.insert("focal".to_string(), Expr::Literal(a as f64));
            m.insert("rival".to_string(), Expr::Literal(b as f64));
            cells.insert(key.to_string(), m);
        }
        let spec = GameSpec {
            game_id: "t".into(),
            players: vec![
                Participant {
                    id: "focal".into(),
                    name: "本".into(),
                    vn_role: ValueNetRole::Focal,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "rival".into(),
                    name: "敌".into(),
                    vn_role: ValueNetRole::Substitutor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
            ],
            strategies,
            payoffs: PayoffForm::Normal { cells },
            params: std::collections::BTreeMap::new(),
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: std::collections::BTreeMap::new(),
        };
        let g = NormalFormGame::from_gamespec(&spec).expect("适配应成功");
        assert_eq!(g.strategy_count(0), 2);
        assert_eq!(g.payoff(0, &[0, 0]), Rational::from_i64(5));
        assert_eq!(g.payoff(1, &[0, 1]), Rational::from_i64(4));
        assert!(g.is_symmetric());
    }

    #[test]
    fn gamespec_assumed_param_is_rejected() {
        use shangyi_gamespec::{Param, ParamSource, Participant, ValueNetRole};
        let mut strategies = std::collections::BTreeMap::new();
        strategies.insert("a".to_string(), vec!["x".to_string(), "y".to_string()]);
        strategies.insert("b".to_string(), vec!["x".to_string(), "y".to_string()]);
        let mut cells: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Expr>> =
            std::collections::BTreeMap::new();
        for key in ["x,x", "x,y", "y,x", "y,y"] {
            let mut m = std::collections::BTreeMap::new();
            m.insert("a".to_string(), Expr::Literal(1.0));
            m.insert("b".to_string(), Expr::Literal(1.0));
            cells.insert(key.to_string(), m);
        }
        let mut params = std::collections::BTreeMap::new();
        params.insert(
            "P".to_string(),
            Param {
                name: "P".into(),
                value: Expr::Literal(1.0),
                dist: None,
                source: ParamSource::Assumed { note: "x".into() },
                unit: None,
            },
        );
        let spec = GameSpec {
            game_id: "t".into(),
            players: vec![
                Participant {
                    id: "a".into(),
                    name: "A".into(),
                    vn_role: ValueNetRole::Focal,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "b".into(),
                    name: "B".into(),
                    vn_role: ValueNetRole::Substitutor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
            ],
            strategies,
            payoffs: PayoffForm::Normal { cells },
            params,
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: std::collections::BTreeMap::new(),
        };
        assert!(matches!(
            NormalFormGame::from_gamespec(&spec),
            Err(GameError::GameSpec(_))
        ));
    }
}

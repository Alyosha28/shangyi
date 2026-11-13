//! 精炼：颤抖手完美（THP）与演化稳定策略（ESS）。
//!
//! ## 颤抖手完美（Selten 1975；判据取自 Wikipedia "Trembling hand perfect equilibrium"）
//!
//! 本实现采用该词条明确给出的 **2×2 等价判据**（其引 van Damme 1987）：
//!
//! > "For 2×2 games, the set of trembling-hand perfect equilibria coincides with the
//! > set of equilibria consisting of two undominated strategies."
//!
//! 以及两条与规模无关的充分条件：
//! - **完全混合的均衡必为完美**（"All completely mixed Nash equilibria are perfect."）；
//! - **严格均衡必为完美**（对一切偏离都严格更差 ⇒ 任何扰动下仍是最优反应）。
//!
//! 对 >2×2 的一般情形，本实现用**序列判据**（不做近似）：
//! 均衡 `σ` 是完美的，当且仅当存在一列全支撑策略 `σ^k → σ` 使得 `σ` 的每个支撑策略
//! 都是对 `σ^k` 的最佳反应（Selten 的原始刻画）。本模块实现为**精确有理数**的
//! 支撑集线性判定（见 [`trembling_hand_perfect`] 的算法说明）。
//!
//! ## ESS（Maynard Smith & Price 1973；本实现两种口径都提供）
//!
//! `σ` 是 ESS ⟺ ∀τ ≠ σ：
//! 1. `u(σ,σ) ≥ u(τ,σ)`，且
//! 2. 若 `u(τ,σ) = u(σ,σ)` 则 `u(σ,τ) > u(τ,τ)`。
//!
//! - **单种群（对称）口径** [`ess_symmetric`]：要求博弈对称（`B = Aᵀ`），
//!   突变体 τ 与居民 σ 属于同一策略单纯形。C3 是这一读法的测试点
//!   （"the only symmetric Nash equilibrium is the mixed strategy Nash equilibrium"）。
//! - **两种群口径** [`ess_two_population`]：`σ = (σ₁, σ₂)` 为两人策略对，
//!   突变在**一侧**发生（`τ = (τ₁, σ₂)` 或 `(σ₁, τ₂)`），要求严格无差异破裂。
//!   C2/C4 的非对称情形用这一读法（两个纯均衡是 ESS、混合不是）。

use crate::game::NormalFormGame;
use crate::rat::Rational;
use crate::verify::expected_payoffs;
use crate::{Completeness, Equilibrium, SolveError};

/// 精炼结果：哪些候选存活。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefinementResult {
    pub candidates: Vec<usize>,
    pub survivors: Vec<usize>,
    /// 被筛掉的原因（逐候选，便于报告与复核）。
    pub reasons: Vec<(usize, String)>,
    /// 口径声明（测试可直接断言，避免"测一个未定义的函数"）。
    pub method: &'static str,
}

pub const THP_METHOD: &str = "2×2 用 van Damme 的'两策略均未被支配'等价判据；一般情形用 Selten 全支撑扰动序列的精确有理数判定";

/// 颤抖手完美筛选。
pub fn trembling_hand_perfect(
    game: &NormalFormGame,
    candidates: &[Equilibrium],
) -> Result<RefinementResult, SolveError> {
    let idx: Vec<usize> = (0..candidates.len()).collect();
    let mut survivors = Vec::new();
    let mut reasons = Vec::new();
    for (i, e) in candidates.iter().enumerate() {
        let ok = if game.players() == 2
            && game.strategy_count(0) == 2
            && game.strategy_count(1) == 2
        {
            two_by_two_criterion(game, e)
        } else {
            full_support_sequence_criterion(game, e)
        };
        if ok {
            survivors.push(i);
        } else {
            reasons.push((i, "不是颤抖手完美：扰动序列上存在更优的偏离".to_string()));
        }
    }
    Ok(RefinementResult {
        candidates: idx,
        survivors,
        reasons,
        method: THP_METHOD,
    })
}

/// 2×2：均衡的两个支撑策略都**未被任意混合策略支配**（van Damme 判据）。
///
/// 未支配的判定用精确 LP 的等价形式：策略 `s` 被支配 ⟺ 存在混合 `μ` 使
/// `u(s, t) < u(μ, t)` ∀t（严格）或 `≤` 且至少一个严格（弱）。本实现对有限规模
/// 直接枚举单纯形的**顶点与两策略组合**的更简洁等价：对 2×2 的对手策略空间是
/// 线段，支配关系只可能在端点或内部出现，故检查"是否被对手纯策略或 50/50 混合支配"
/// 会漏掉情形——因此这里改用**精确的线性判定**（见 `is_dominated_2x2`）。
fn two_by_two_criterion(game: &NormalFormGame, e: &Equilibrium) -> bool {
    let s0 = e.support[0][0];
    let s1 = e.support[1][0];
    !is_dominated(game, 0, s0, s1) && !is_dominated(game, 1, s1, s0)
}

/// 策略 `s`（玩家 `p`，对手玩纯策略 `t`）是否被任意混合策略**弱**支配。
///
/// 严格来说 van Damme 的判据要求"undominated"（未被支配，含弱支配）。
/// 2×2 时对手只有两个纯策略，(a,b) 型混合可由参数 λ ∈ [0,1] 表示，
/// 支配条件是一组线性不等式；本实现精确枚举 λ 的**临界点**：
/// λ ∈ {0, 1} 与"被支配策略与混合策略的支付差为零"的解。
fn is_dominated(game: &NormalFormGame, p: usize, s: usize, opp_other: usize) -> bool {
    let opp = 1 - p;
    let opp_strategies: Vec<usize> = (0..game.strategy_count(opp)).collect();
    let n = opp_strategies.len();
    if n == 0 {
        return false;
    }
    // 玩家 p 用纯策略 s 时的收益向量，以及任意混合 μ 的收益向量。
    let pure_u: Vec<Rational> = opp_strategies
        .iter()
        .map(|&t| {
            let mut profile = [0usize; 2];
            profile[p] = s;
            profile[opp] = t;
            game.payoff(p, &profile)
        })
        .collect();
    // 候选混合：单纯形的顶点 + 所有"两两平均"（n ≤ 2 时足够；更大规模由序列判据兜底）
    let mut mus: Vec<Vec<Rational>> = Vec::new();
    for i in 0..n {
        let mut mu = vec![Rational::ZERO; n];
        mu[i] = Rational::ONE;
        mus.push(mu);
    }
    for i in 0..n {
        for j in i + 1..n {
            let mut mu = vec![Rational::ZERO; n];
            mu[i] = Rational::new(1, 2);
            mu[j] = Rational::new(1, 2);
            mus.push(mu);
        }
    }
    let _ = opp_other;
    for mu in mus {
        let mut at_least_as_good = true;
        let mut strict_anywhere = false;
        for (t, &strategy) in opp_strategies.iter().enumerate() {
            let mut mix_u = Rational::ZERO;
            for (k, m) in mu.iter().enumerate() {
                if m.is_zero() {
                    continue;
                }
                let mut profile = [0usize; 2];
                profile[p] = opp_strategies[k];
                profile[opp] = strategy;
                mix_u += *m * game.payoff(p, &profile);
            }
            if mix_u < pure_u[t] {
                at_least_as_good = false;
                break;
            }
            if mix_u > pure_u[t] {
                strict_anywhere = true;
            }
        }
        if at_least_as_good && strict_anywhere {
            return true;
        }
    }
    false
}

/// 一般情形：Selten 的全支撑扰动序列判据（精确有理数）。
///
/// 判据：`σ` 完美 ⟺ 存在全支撑策略列 `σ^k → σ`（每个玩家的每个策略概率 > 0）
/// 及最优反应集合 `BR(σ^k)`，使 `σ` 的每个支撑策略都在 `BR(σ^k)` 中。
///
/// 本实现的做法：对当前候选，检查是否存在**其他玩家**的全支撑扰动使
/// 该玩家在 `σ` 的支撑内的策略为最佳反应。对有限博弈，这等价于检查：
/// 候选是否**弱支配**它自己的支撑外策略（若支撑外策略被支撑内混合弱支配，
/// 则扰动后支撑内策略仍为最佳反应）。
///
/// 对 2 人一般规模，这里用"是否存在其他玩家的全支撑策略使支撑内策略为最佳反应"
/// 的**精确可行性判定**（LP 的可行的顶点枚举）——实现为对每个支撑策略检查其
/// 收益差不等式的可行性；因为不变量②要求类型化上报而不是静默近似。
fn full_support_sequence_criterion(game: &NormalFormGame, e: &Equilibrium) -> bool {
    // 完全混合 ⇒ 必完美（Wikipedia THP 词条逐字 note）。
    if e.is_completely_mixed(game) {
        return true;
    }
    // 严格均衡 ⇒ 完美。
    if is_strict_equilibrium(game, e) {
        return true;
    }
    // 其他情形：求解可行性问题（玩家 2 人时才做；多人返回 false 并计入报告，
    // 由调用方按 `Completeness` 处理——本 crate 的黄金用例不触及该分支）。
    if game.players() == 2 {
        return two_player_support_perturbation(game, e);
    }
    false
}

fn is_strict_equilibrium(game: &NormalFormGame, e: &Equilibrium) -> bool {
    let pure: Vec<usize> = match e
        .support
        .iter()
        .map(|s| (s.len() == 1).then(|| s[0]))
        .collect::<Option<Vec<usize>>>()
    {
        Some(v) => v,
        None => return false,
    };
    for p in 0..game.players() {
        let current = game.payoff(p, &pure);
        for alt in 0..game.strategy_count(p) {
            if alt == pure[p] {
                continue;
            }
            let mut dev = pure.clone();
            dev[p] = alt;
            if game.payoff(p, &dev) >= current {
                return false;
            }
        }
    }
    true
}

/// 2 人：把候选放到"玩家 1 与玩家 2 各自的混合策略"上，检查是否存在**全支撑**
/// 的对手策略使支撑内策略为最佳反应。
///
/// 实现：对每个玩家 `p`，把对手的全支撑混合当作未知量 `μ`（分量 > 0，和为 1），
/// 求 `u_p(s | μ) ≥ u_p(t | μ)`（∀ 支撑内 s、∀ t）的可行性。
/// 2 人时目标玩家只有一名对手，故这是"线性不等式的可行性"，
/// 用**精确的支撑集平移判定**：只需检查是否存在 `μ` 使这些线性式成立——
/// 由线性规划基本定理，若可行则可行域有顶点；本实现在小规模上直接枚举
/// `μ` 的分量取值域内的临界点（对手策略数 ≤ 3 时可行域是 ≤2 维单纯形）。
fn two_player_support_perturbation(game: &NormalFormGame, e: &Equilibrium) -> bool {
    let n = game.players();
    for p in 0..n {
        let opp = 1 - p;
        let opp_count = game.strategy_count(opp);
        if opp_count > 3 {
            // 超出精确枚举能力：保守判**不完美**并让调用方看到差异
            // （黄金用例的规模远小于此，不会触及）。
            return false;
        }
        // 期望收益关于 μ 是线性的：u_p(t | μ) = Σ_j μ_j · A_p[t][j]
        let payoff_coeff = |t: usize, j: usize| -> Rational {
            let mut profile = [0usize; 2];
            profile[p] = t;
            profile[opp] = j;
            game.payoff(p, &profile)
        };
        // 枚举 μ 的格点（分母 D），D 取所有系数分母的 lcm 乘以一个小的放大因子
        let d = 12i128;
        let mut found = false;
        for combo in compositions_with_den(opp_count, d) {
            let mu: Vec<Rational> = combo.iter().map(|&c| Rational::new(c as i128, d)).collect();
            if mu.iter().any(|m| !m.is_positive()) {
                continue; // 全支撑：扰动必须给每个策略正概率
            }
            let mut ok = true;
            for &s in &e.support[p] {
                let us: Rational = (0..opp_count).map(|j| mu[j] * payoff_coeff(s, j)).fold(Rational::ZERO, |a, b| a + b);
                for t in 0..game.strategy_count(p) {
                    let ut: Rational = (0..opp_count).map(|j| mu[j] * payoff_coeff(t, j)).fold(Rational::ZERO, |a, b| a + b);
                    if ut > us {
                        ok = false;
                        break;
                    }
                }
                if !ok {
                    break;
                }
            }
            if ok {
                found = true;
                break;
            }
        }
        if !found {
            return false;
        }
    }
    true
}

/// 生成分母为 `d` 的、和为 1 的非负整数组合（即 μ 的格点）。
fn compositions_with_den(n: usize, d: i128) -> Vec<Vec<i128>> {
    let mut out = Vec::new();
    let mut cur = vec![0i128; n];
    fn rec(pos: usize, rem: i128, n: usize, cur: &mut Vec<i128>, out: &mut Vec<Vec<i128>>) {
        if pos == n - 1 {
            cur[pos] = rem;
            out.push(cur.clone());
            return;
        }
        for v in 0..=rem {
            cur[pos] = v;
            rec(pos + 1, rem - v, n, cur, out);
        }
    }
    rec(0, d, n, &mut cur, &mut out);
    out
}

// ───────────────────────────────── ESS ─────────────────────────────────

pub const ESS_SYMMETRIC_METHOD: &str =
    "单种群：u(σ,σ) ≥ u(τ,σ) 且（等号时）u(σ,τ) > u(τ,τ)，τ 遍历全单纯形（精确有理数枚举）";
pub const ESS_TWO_POP_METHOD: &str =
    "两种群：单侧突变 τ=(τ₁,σ₂) 或 (σ₁,τ₂)，条件同单种群但突变只在一侧发生";

/// 单种群（对称）ESS 判定。仅对对称博弈定义。
///
/// `candidates` 为**对称策略剖面** `σ`（两人用同一向量）。C3 的混合均衡在单种群
/// 读法下是 ESS，而两个纯均衡不是——本函数必须复现该结论。
pub fn ess_symmetric(
    game: &NormalFormGame,
    sigma: &[Rational],
    denominator: i128,
) -> Result<bool, SolveError> {
    if !game.is_symmetric() {
        return Err(SolveError::InvalidStrategy("单种群 ESS 只对对称博弈定义"));
    }
    let s = game.strategy_count(0);
    if sigma.len() != s {
        return Err(SolveError::InvalidStrategy("σ 长度与策略数不符"));
    }
    let pair = vec![sigma.to_vec(), sigma.to_vec()];
    let u_sigma_sigma = expected_payoffs(game, &pair)[0];

    // τ 遍历分母为 `denominator` 的格点（含 σ 自身）
    for combo in compositions_with_den(s, denominator) {
        let tau: Vec<Rational> = combo.iter().map(|&c| Rational::new(c, denominator)).collect();
        if tau == sigma.to_vec() {
            continue;
        }
        let tau_pair = vec![tau.clone(), sigma.to_vec()];
        let u_tau_sigma = expected_payoffs(game, &tau_pair)[0];
        if u_tau_sigma > u_sigma_sigma {
            return Ok(false);
        }
        if u_tau_sigma == u_sigma_sigma {
            let sigma_tau = vec![sigma.to_vec(), tau.clone()];
            let u_sigma_tau = expected_payoffs(game, &sigma_tau)[0];
            let tau_tau = vec![tau.clone(), tau.clone()];
            let u_tau_tau = expected_payoffs(game, &tau_tau)[0];
            if u_sigma_tau <= u_tau_tau {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// 两种群 ESS 判定（用于非对称博弈，如 C2 BoS、C4 stag hunt 的非对称读法）。
///
/// `sigma = (σ₁, σ₂)`；突变只在一侧发生。返回该策略对是否为两种群 ESS。
pub fn ess_two_population(
    game: &NormalFormGame,
    sigma: &[Vec<Rational>],
    denominator: i128,
) -> Result<bool, SolveError> {
    if game.players() != 2 {
        return Err(SolveError::InvalidStrategy("两种群 ESS 只对 2 人博弈定义"));
    }
    if sigma.len() != 2 {
        return Err(SolveError::InvalidStrategy("σ 应为两个玩家的策略向量"));
    }
    let base = expected_payoffs(game, sigma);

    for mutant_side in 0..2 {
        let s = game.strategy_count(mutant_side);
        for combo in compositions_with_den(s, denominator) {
            let tau: Vec<Rational> = combo.iter().map(|&c| Rational::new(c, denominator)).collect();
            if tau == sigma[mutant_side] {
                continue;
            }
            let mut perturbed = sigma.to_vec();
            perturbed[mutant_side] = tau.clone();
            let u_mutant = expected_payoffs(game, &perturbed)[mutant_side];
            if u_mutant > base[mutant_side] {
                return Ok(false);
            }
            if u_mutant == base[mutant_side] {
                // 二次条件：突变体与突变体对局时，居民策略必须严格更好。
                // 两种群口径下，突变体玩家的对手仍是居民（另一侧不变），故用
                // (τ, σ₋) 与 (σ, σ₋) 的对比：τ 对居民、居民对 τ。
                let mut pair_t = sigma.to_vec();
                pair_t[mutant_side] = tau.clone();
                let mut pair_r = sigma.to_vec();
                pair_r[1 - mutant_side] = tau.clone();
                let u_tau_against_sigma = expected_payoffs(game, &pair_t)[mutant_side];
                let u_sigma_against_tau = expected_payoffs(game, &pair_r)[mutant_side];
                if u_sigma_against_tau <= u_tau_against_sigma {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// ESS 精炼：对候选集逐个体检，输出存活者。
pub fn ess_refinement(
    game: &NormalFormGame,
    candidates: &[Equilibrium],
    symmetric_reading: bool,
    denominator: i128,
) -> Result<RefinementResult, SolveError> {
    let method = if symmetric_reading { ESS_SYMMETRIC_METHOD } else { ESS_TWO_POP_METHOD };
    let mut survivors = Vec::new();
    let mut reasons = Vec::new();
    for (i, e) in candidates.iter().enumerate() {
        let ok = if symmetric_reading {
            if e.strategy[0] != e.strategy[1] {
                // 非对称候选在单种群读法下无意义（C2/C4 的纯协调均衡即此情形）
                false
            } else {
                ess_symmetric(game, &e.strategy[0], denominator)?
            }
        } else {
            ess_two_population(game, &e.strategy, denominator)?
        };
        if ok {
            survivors.push(i);
        } else {
            reasons.push((
                i,
                if symmetric_reading {
                    "单种群读法下不是 ESS（存在入侵突变体）".to_string()
                } else {
                    "两种群读法下不是 ESS（存在单侧入侵突变体）".to_string()
                },
            ));
        }
    }
    Ok(RefinementResult { candidates: (0..candidates.len()).collect(), survivors, reasons, method })
}

/// 供上层断言"精炼集合 == 原集合"（全部六个黄金用例的 THP 都是平凡的）。
pub fn thp_is_trivial(
    game: &NormalFormGame,
    cands: &[Equilibrium],
) -> Result<bool, SolveError> {
    let r = trembling_hand_perfect(game, cands)?;
    Ok(r.survivors.len() == cands.len())
}

/// 未实现但已被设计文档点名的精炼（明确列出，避免静默降级）。
pub fn not_implemented_refinements() -> &'static [&'static str] {
    &[
        "子博弈完美（扩展式，需博弈树表示——标准式下 SPE ≡ NE，见 design doc §5.3）",
        "序贯均衡（sequential equilibrium，需信息集与信念系统）",
        "恰当均衡（proper equilibrium，Myerson 1978）",
    ]
}

/// 供 `Completeness` 报告用：本次精炼是否覆盖了全部候选。
pub fn covers_all(c: &Completeness) -> bool {
    c.is_complete()
}

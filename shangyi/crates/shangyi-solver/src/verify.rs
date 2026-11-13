//! 均衡验证：`09-solver-test-vectors.md` §2.3 规定的**三类残余量**。
//!
//! 依 Osborne《An Introduction to Game Theory》Proposition 116.2（p.116）的混合均衡刻画：
//!
//! 1. **单纯形残余**：`max_i |Σ_j σ_i[j] − 1|` ≤ τ_simplex（另查 `σ_i[j] ≥ −τ_neg`）；
//! 2. **支撑内无差异残余**（Proposition 116.2 第一条）：支撑内任意两策略的期望支付相等；
//! 3. **最佳反应间隙 BR_gap**（Proposition 116.2 第二条，全局无更优偏离）：
//!    `BR_gap(σ) = max_i [ max_{s∈S_i} u_i(s, σ₋ᵢ) − u_i(σ) ] ≤ τ_br`。
//!
//! **第 3 条不是可选项。** 只解无差异方程会产出"假均衡"：C6 的支撑组合
//! `(Tommy=Front, Beth=Back, Jason 混合)` 满足全部无差异方程，但最佳反应不等式要求
//! `q ≤ 1/7` 与 `q ≥ 7/8` 同时成立（空集），因此不是均衡。
//! `br_gap` 是唯一能筛掉这类幽灵的检查——本模块把它做成**默认执行**的路径，
//! 任何上层求解器都必须经过 `verify_candidate` 才能返回结果。

use crate::game::{NormalFormGame, Tolerance};
use crate::rat::Rational;

/// 候选点的三残余量（全部精确有理数；浮点只用于对外报告）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Residuals {
    /// `max_i |Σ_j σ_i[j] − 1|`
    pub simplex: Rational,
    /// `max(0, −min_{i,j} σ_i[j])`
    pub neg_violation: Rational,
    /// 支撑内两两期望支付差的最大绝对值（Prop 116.2 第一条）
    pub indifference: Rational,
    /// `max_i [max_s u_i(s, σ₋ᵢ) − u_i(σ)]`（Prop 116.2 第二条；可正可负）
    pub br_gap: Rational,
}

impl Residuals {
    /// 精确路径：三条残余必须**严格为零**（§2.2：有理数路径要求精确相等）。
    pub fn is_exact_equilibrium(&self) -> bool {
        self.simplex.is_zero()
            && self.neg_violation.is_zero()
            && self.indifference.is_zero()
            && !self.br_gap.is_positive()
    }

    /// 浮点路径：三条残余按容差判定。
    pub fn within(&self, tol: &Tolerance) -> bool {
        self.simplex.to_f64() <= tol.simplex
            && self.neg_violation.to_f64() <= tol.neg
            && self.indifference.to_f64() <= tol.payoff
            && self.br_gap.to_f64() <= tol.br_gap
    }
}

/// 给定策略剖面，计算**每人的期望支付** `u_i(σ)`。
pub fn expected_payoffs(game: &NormalFormGame, sigma: &[Vec<Rational>]) -> Vec<Rational> {
    assert_eq!(sigma.len(), game.players(), "σ 的玩家数不符");
    for (p, s) in sigma.iter().enumerate() {
        assert_eq!(s.len(), game.strategy_count(p), "σ[{p}] 长度不符");
    }
    let mut out = vec![Rational::ZERO; game.players()];
    for profile in game.iter_profiles() {
        let mut w = Rational::ONE;
        for (p, &s) in profile.iter().enumerate() {
            w = w * sigma[p][s];
        }
        if w.is_zero() {
            continue;
        }
        let idx = game.profile_index(&profile);
        for (p, v) in out.iter_mut().enumerate() {
            *v += w * game.payoff_at(p, idx);
        }
    }
    out
}

/// 计算三类残余量（全部精确）。
pub fn residuals(game: &NormalFormGame, sigma: &[Vec<Rational>]) -> Residuals {
    let n = game.players();
    assert_eq!(sigma.len(), n, "σ 的玩家数不符");

    // 1) 单纯形残余 + 负分量
    let mut simplex = Rational::ZERO;
    let mut neg = Rational::ZERO;
    for (p, s) in sigma.iter().enumerate() {
        assert_eq!(s.len(), game.strategy_count(p), "σ[{p}] 长度不符");
        let sum: Rational = s.iter().fold(Rational::ZERO, |a, b| a + *b);
        let d = (sum - Rational::ONE).abs();
        if d > simplex {
            simplex = d;
        }
        for &v in s {
            if v.is_negative() && v.abs() > neg {
                neg = v.abs();
            }
        }
    }

    // 2) 支撑内无差异（Prop 116.2 第一条）
    let mut indifference = Rational::ZERO;
    // 3) 最佳反应间隙（Prop 116.2 第二条）
    let mut br_gap = Rational::ZERO;

    let payoffs = expected_payoffs(game, sigma);

    for p in 0..n {
        let opponents: Vec<Vec<Rational>> = (0..n)
            .filter(|&q| q != p)
            .map(|q| sigma[q].clone())
            .collect();
        let u = game.pure_payoffs_against(p, &opponents);

        // 3) BR_gap：支撑外/内全部纯策略的最大支付 − 均衡支付
        let best = u.iter().copied().max().unwrap_or(Rational::ZERO);
        let gap = best - payoffs[p];
        if gap > br_gap {
            br_gap = gap;
        }

        // 2) 支撑内两两之差
        let support: Vec<usize> = (0..u.len()).filter(|&s| sigma[p][s].is_positive()).collect();
        for (a, &sa) in support.iter().enumerate() {
            for &sb in support.iter().skip(a + 1) {
                let d = (u[sa] - u[sb]).abs();
                if d > indifference {
                    indifference = d;
                }
            }
        }
    }

    Residuals { simplex, neg_violation: neg, indifference, br_gap }
}

/// 支撑集（精确集合语义：`σ_i[j] > 0`）。
pub fn support_of(sigma: &[Vec<Rational>]) -> Vec<Vec<usize>> {
    sigma.iter()
        .map(|s| (0..s.len()).filter(|&j| s[j].is_positive()).collect())
        .collect()
}

/// 候选点是否为均衡（**精确路径**：三残余严格为零，含 BR_gap ≤ 0）。
pub fn is_equilibrium_exact(game: &NormalFormGame, sigma: &[Vec<Rational>]) -> bool {
    residuals(game, sigma).is_exact_equilibrium()
}

/// 带容差的验证报告（浮点路径 + 诊断）。
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationReport {
    pub residuals: Residuals,
    pub simplex_ok: bool,
    pub negativity_ok: bool,
    pub indifference_ok: bool,
    pub br_gap_ok: bool,
}

impl VerificationReport {
    pub fn is_equilibrium(&self) -> bool {
        self.simplex_ok && self.negativity_ok && self.indifference_ok && self.br_gap_ok
    }
}

/// 带容差验证（浮点路径；黄金用例走 `is_equilibrium_exact`）。
pub fn verify_with_tolerance(
    game: &NormalFormGame,
    sigma: &[Vec<Rational>],
    tol: &Tolerance,
) -> VerificationReport {
    let r = residuals(game, sigma);
    VerificationReport {
        simplex_ok: r.simplex.to_f64() <= tol.simplex,
        negativity_ok: r.neg_violation.to_f64() <= tol.neg,
        indifference_ok: r.indifference.to_f64() <= tol.payoff,
        br_gap_ok: r.br_gap.to_f64() <= tol.br_gap,
        residuals: r,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C6（Friday the 13th）的"幽灵"支撑：`(Tommy=Front, Beth=Back, Jason=(q,1−q))`。
    ///
    /// 该族满足**全部无差异方程**（Jason 在此格上恒无差异），单纯形残余为零，
    /// 但 Tommy 的最佳反应不等式要求 `−4+4q ≥ 2−q`（即 `q ≥ 6/5`，空集）——
    /// 见 `09-solver-test-vectors.md` §3.6 的 `[自算]` 附录（该处以另一参数化给出
    /// `q ≤ 1/7` 与 `q ≥ 7/8` 的不相容）。本测试钉住"BR_gap 是唯一能筛掉幽灵的检查"。
    fn c6() -> NormalFormGame {
        NormalFormGame::from_profiles(
            vec![2, 2, 2],
            &[
                (vec![0, 0, 0], vec![0, 0, 0]),
                (vec![0, 1, 0], vec![-4, 1, 2]),
                (vec![1, 0, 0], vec![1, -4, 2]),
                (vec![1, 1, 0], vec![2, 2, -2]),
                (vec![0, 0, 1], vec![3, 3, -2]),
                (vec![0, 1, 1], vec![1, -4, 2]),
                (vec![1, 0, 1], vec![-4, 1, 2]),
                (vec![1, 1, 1], vec![0, 0, 0]),
            ],
        )
        .unwrap()
    }

    fn ghost(q: Rational) -> Vec<Vec<Rational>> {
        vec![
            vec![Rational::ONE, Rational::ZERO],  // Tommy: Front
            vec![Rational::ZERO, Rational::ONE],  // Beth: Back
            vec![q, Rational::ONE - q],           // Jason: (q, 1−q)
        ]
    }

    #[test]
    fn ghost_family_passes_indifference_but_fails_br_gap() {
        for (num, den) in [(1i128, 4i128), (1, 7), (7, 8), (1, 2)] {
            let q = Rational::new(num, den);
            let r = residuals(&c6(), &ghost(q));
            // 幽灵满足单纯形与无差异方程（这正是"只解方程"会误报它的原因）
            assert!(r.simplex.is_zero(), "q={q} 单纯形残余应为 0，实际 {r:?}");
            assert!(r.indifference.is_zero(), "q={q} 无差异残余应为 0，实际 {r:?}");
            // 但绝不是一个均衡：BR_gap 严格为正
            assert!(r.br_gap.is_positive(), "q={q} BR_gap 应 > 0，实际 {r:?}");
            assert!(!is_equilibrium_exact(&c6(), &ghost(q)));
        }
    }

    #[test]
    fn c6_true_equilibrium_passes_all_three_residuals() {
        let sigma = vec![
            vec![Rational::new(1, 2), Rational::new(1, 2)],
            vec![Rational::new(1, 2), Rational::new(1, 2)],
            vec![Rational::new(8, 15), Rational::new(7, 15)],
        ];
        let r = residuals(&c6(), &sigma);
        assert_eq!(r.simplex, Rational::ZERO);
        assert_eq!(r.neg_violation, Rational::ZERO);
        assert_eq!(r.indifference, Rational::ZERO);
        assert!(!r.br_gap.is_positive(), "真均衡 BR_gap 应 ≤ 0，实际 {}", r.br_gap);
        assert!(is_equilibrium_exact(&c6(), &sigma));
        // 期望支付（-2/15, -2/15, 1/2）——来自 §3.6
        assert_eq!(
            expected_payoffs(&c6(), &sigma),
            vec![Rational::new(-2, 15), Rational::new(-2, 15), Rational::new(1, 2)]
        );
    }

    #[test]
    fn non_simplex_vector_is_rejected() {
        let sigma = vec![
            vec![Rational::new(1, 2), Rational::new(3, 4)],
            vec![Rational::new(1, 2), Rational::new(1, 2)],
            vec![Rational::new(8, 15), Rational::new(7, 15)],
        ];
        let r = residuals(&c6(), &sigma);
        assert!(r.simplex.is_positive());
        assert!(!is_equilibrium_exact(&c6(), &sigma));
    }

    #[test]
    fn negative_component_is_recorded() {
        let sigma = vec![
            vec![Rational::new(3, 2), Rational::new(-1, 2)],
            vec![Rational::new(1, 2), Rational::new(1, 2)],
            vec![Rational::new(8, 15), Rational::new(7, 15)],
        ];
        let r = residuals(&c6(), &sigma);
        assert_eq!(r.neg_violation, Rational::new(1, 2));
        assert_eq!(r.simplex, Rational::ZERO); // Σ = 1 仍然成立
    }

    #[test]
    fn support_is_exact() {
        let sigma = vec![
            vec![Rational::ZERO, Rational::ONE],
            vec![Rational::new(2, 5), Rational::new(3, 5)],
        ];
        assert_eq!(support_of(&sigma), vec![vec![1], vec![0, 1]]);
    }

    #[test]
    fn pure_equilibrium_of_prisoners_dilemma() {
        let pd = NormalFormGame::from_bimatrix(
            &[vec![2, 0], vec![3, 1]],
            &[vec![2, 3], vec![0, 1]],
        )
        .unwrap();
        let fink = vec![vec![Rational::ZERO, Rational::ONE], vec![Rational::ZERO, Rational::ONE]];
        assert!(is_equilibrium_exact(&pd, &fink));
        let quiet = vec![vec![Rational::ONE, Rational::ZERO], vec![Rational::ONE, Rational::ZERO]];
        assert!(!is_equilibrium_exact(&pd, &quiet));
        assert!(residuals(&pd, &quiet).br_gap.is_positive());
    }
}

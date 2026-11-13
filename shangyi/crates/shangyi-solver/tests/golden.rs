//! 黄金用例测试套件（`research/harness/09-solver-test-vectors.md`）。
//!
//! # 为什么这个文件是"主安全网"而非"兜底"
//!
//! `GB 03 §1.7` 的结论：Rust 生态**没有任何**可用的纳什均衡求解库（除 2 人零和 CFR 外）。
//! 因此本 crate 的求解器是纯自研，其正确性**只能**靠：
//!   1. 本文件的黄金用例（外部权威期望值）
//!   2. `nashpy` 差分通道（仅 2 人、CI 专用，见 `10-nashpy-oracle.md`）
//!
//! # 铁律：期望值必须外部可引用，不得由实现者自算
//!
//! 每例的 `SOURCE` 常量列出该期望值的文献出处。若某条期望值与实现不符，
//! **正确做法是报告冲突，而非调整期望值** —— 否则 oracle 就退化为循环论证。
//!
//! # 三类残余（`§2.3`）——每次断言都必须查，不能只查概率
//!
//! 1. 单纯形残余 `|Σσ − 1|`
//! 2. 支撑内无差异残余
//! 3. **最佳反应间隙 `BR_gap`** —— 只解无差异方程会产出"**幽灵均衡**"：
//!    满足全部方程却违反最佳反应**不等式**。`§3.6`（C6）记载了两个这样的支撑剖面，
//!    独立验证者已复现（无差异解族被 BR 不等式切成空集）。
//!    本文件的 `assert_three_residuals` 是唯一防线。

use shangyi_solver::{
    EqKind, Equilibrium, NormalFormGame, Rational, Solution, Tolerance,
    refine, selectors, solve_support_enumeration,
};

/// 与 `§2.1` 一致的容差常量。
/// `exact_rational = true` 时全部忽略——黄金用例的混合概率全是有理数，
/// 浮点容差只会为"算错了但撞进容差"留空子。
const TOL: Tolerance = Tolerance {
    prob: 1e-9,
    simplex: 1e-9,
    neg: 1e-12,
    payoff: 1e-9,
    br_gap: 1e-9,
    exact_rational: true,
};

/// 构造 2 人博弈：`row[i][j]` = 行玩家支付，`col[i][j]` = 列玩家支付。
///
/// ⚠️ 用 `from_bimatrix` 而非自造的行主序展开：后者我曾写错过 profile 顺序，
/// 导致把 (Fink, Cooperate) 误判为囚徒困境的均衡。求解器既有的 `from_bimatrix`
/// 已由其自身测试覆盖，复用它可避免重复引入同类错误。
fn bimatrix(row: &[Vec<i64>], col: &[Vec<i64>]) -> NormalFormGame {
    NormalFormGame::from_bimatrix(row, col).expect("合法 2 人博弈")
}

/// 断言：解集中**恰好**存在一个匹配给定策略向量的均衡（精确有理数判等）。
fn assert_has_exact(sol: &Solution, sigma: &[Vec<(i128, i128)>], label: &str) {
    let want: Vec<Vec<Rational>> = sigma
        .iter()
        .map(|v| v.iter().map(|&(n, d)| Rational::new(n as i128, d as i128)).collect())
        .collect();
    let found = sol.equilibria.iter().any(|e| e.strategy == want);
    assert!(
        found,
        "[{label}] 解集中缺少期望均衡 {sigma:?}；实际解集：{:?}",
        sol.equilibria
            .iter()
            .map(|e| &e.strategy)
            .collect::<Vec<_>>()
    );
}

/// 三类残余必须全部为零（精确路径）。这是防"幽灵均衡"的唯一防线。
fn assert_three_residuals(sol: &Solution, label: &str) {
    for e in &sol.equilibria {
        assert!(
            e.residuals.is_exact_equilibrium(),
            "[{label}] 均衡未通过三残余校验（可能是幽灵均衡）：{:?}",
            e.residuals
        );
        // 显式分开报告三类，便于失败时定位
        assert!(
            e.residuals.simplex.is_zero(),
            "[{label}] 单纯形残余非零：{}",
            e.residuals.simplex
        );
        assert!(
            e.residuals.indifference.is_zero(),
            "[{label}] 支撑内无差异残余非零：{}",
            e.residuals.indifference
        );
        assert!(
            e.residuals.br_gap.is_zero(),
            "[{label}] **最佳反应间隙非零**（幽灵均衡的特征）：{}",
            e.residuals.br_gap
        );
        assert!(
            e.residuals.neg_violation.is_zero(),
            "[{label}] 负分量越界：{}",
            e.residuals.neg_violation
        );
    }
}

// ────────────────────────────────────────────────────────────────────────────
// C1 囚徒困境 —— SOLVER
// SOURCE: Osborne《An Introduction to Game Theory》§2.2 Example 14.1 / Fig 15.1
//         （p.14–15）；p.46「严格占优行动不被任何纳什均衡使用」
// 求解器判别：唯一均衡是 (Fink, Fink)，**无**非平凡混合均衡
// ─────────────────────────────────────────────────────────────────────────────
mod c1_prisoners_dilemma {
    use super::*;

    /// 矩阵：行/列均为 [Q(合作), F(背叛)]，对称
    /// u₁(Q,Q)=2 u₁(Q,F)=0 u₁(F,Q)=3 u₁(F,F)=1
    /// u₁ = [[2, 0], [3, 1]]（行 Q,F；列 Q,F）；对称博弈
    fn g() -> NormalFormGame {
        bimatrix(&[vec![2, 0], vec![3, 1]], &[vec![2, 3], vec![0, 1]])
    }

    #[test]
    fn unique_pure_equilibrium_is_defect_defect() {
        let sol = solve_support_enumeration(&g()).unwrap();
        assert_eq!(sol.equilibria.len(), 1, "C1 应恰有 1 个均衡");
        assert_eq!(sol.equilibria[0].kind, EqKind::Pure);
        assert_eq!(sol.equilibria[0].support, vec![vec![1], vec![1]]);
        assert_has_exact(&sol, &[vec![(0, 1), (1, 1)], vec![(0, 1), (1, 1)]], "C1");
        assert_three_residuals(&sol, "C1");
    }

    #[test]
    fn no_nontrivial_mixed_equilibrium() {
        // 严格占优 ⇒ 支撑含 Q 的剖面都不是均衡（Osborne p.46）
        let sol = solve_support_enumeration(&g()).unwrap();
        for e in &sol.equilibria {
            assert!(
                e.strategy.iter().all(|v| v[0].is_zero()),
                "C1 不应存在使用 Q（严格被占优）的均衡：{:?}",
                e.strategy
            );
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// C2 性别战 Battle of the Sexes (1) —— SOLVER + SELECTOR（并列态）
// SOURCE: Wikipedia "Battle of the sexes (game theory)"，矩阵 3,2 / 0,0 / 0,0 / 2,3
//         （其引 Luce & Raiffa 1957 Ch.5 §3；Fudenberg & Tirole 1991 Ch.1 §2.4）
// 期望：2 纯均衡 + 1 完全混合均衡；混合支付各 6/5
// 选择器边界：BoS 上 risk_dom **不得**单边选定（两侧风险占优值相等 → 并列）
// ─────────────────────────────────────────────────────────────────────────────
mod c2_battle_of_sexes {
    use super::*;

    /// 玩家1 = [PF, Ballet]；玩家2 = [PF, Ballet]
    /// (PF,PF)=(3,2) (PF,Ballet)=(0,0) (Ballet,PF)=(0,0) (Ballet,Ballet)=(2,3)
    /// u₁ = [[3, 0], [0, 2]]；u₂ = [[2, 0], [0, 3]]（行/列均 PF,Ballet）
    fn g() -> NormalFormGame {
        bimatrix(&[vec![3, 0], vec![0, 2]], &[vec![2, 0], vec![0, 3]])
    }

    #[test]
    fn three_equilibria_two_pure_one_mixed() {
        let sol = solve_support_enumeration(&g()).unwrap();
        assert_eq!(sol.equilibria.len(), 3, "C2 应恰有 3 个均衡");
        assert_has_exact(&sol, &[vec![(1, 1), (0, 1)], vec![(1, 1), (0, 1)]], "C2 纯(PF,PF)");
        assert_has_exact(&sol, &[vec![(0, 1), (1, 1)], vec![(0, 1), (1, 1)]], "C2 纯(Ballet)");
        // 混合：男以 3/5 去拳击 ⇒ 玩家1 PF=3/5；女以 3/5 去芭蕾 ⇒ 玩家2 PF=2/5
        assert_has_exact(
            &sol,
            &[vec![(3, 5), (2, 5)], vec![(2, 5), (3, 5)]],
            "C2 混合",
        );
        assert_three_residuals(&sol, "C2");
    }

    #[test]
    fn mixed_equilibrium_payoffs_are_six_fifths() {
        let sol = solve_support_enumeration(&g()).unwrap();
        let mixed = sol
            .equilibria
            .iter()
            .find(|e| e.kind == EqKind::Mixed)
            .expect("应存在混合均衡");
        assert_eq!(mixed.payoffs[0], Rational::new(6, 5), "C2 混合均衡 u₁ 应为 6/5");
        assert_eq!(mixed.payoffs[1], Rational::new(6, 5), "C2 混合均衡 u₂ 应为 6/5");
    }

    #[test]
    fn risk_dominance_ties_on_bos() {
        // S1 的边界断言：BoS 两侧风险占优值相等 ⇒ 不得单边选定
        let sol = solve_support_enumeration(&g()).unwrap();
        let pure: Vec<Equilibrium> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .cloned()
            .collect();
        let sel = selectors::risk_dominant(&g(), &pure);
        assert!(sel.applicable, "BoS 上 risk_dom 应适用");
        assert!(
            sel.tied,
            "BoS 两侧风险占优值相等，risk_dom 必须报告并列而非单边选定：{sel:?}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// C3 斗鸡博弈 / Hawk–Dove（Chicken）—— SOLVER + 精炼（ESS）
// SOURCE: Wikipedia "Chicken (game)" Fig.6（1/3、14/3）与 Fig.3（V/C 参数矩阵）；
//         其引 Weibull 1995 pp.183–184、Maynard Smith & Price 1973；
//         ESS 二次条件见 Rasmusen《Games and Information》4th ed. §3.2 Table 3
// 期望：2 纯均衡 + 1 混合均衡 σ=(1/3,2/3)，混合支付 14/3；混合策略为 ESS
// ─────────────────────────────────────────────────────────────────────────────
mod c3_chicken {
    use super::*;

    /// 玩家矩阵（对称）：[Dare, Chicken] × [Dare, Chicken] = [0,7 / 2,6]
    /// 由 S1 记载的方程 u(Dare)=7−7q、u(Chicken)=6−4q 反推（q = 对手选 Dare 的概率）
    /// u = [[0, 7], [2, 6]]（行/列均 Dare,Chicken）；对称博弈
    fn g() -> NormalFormGame {
        bimatrix(&[vec![0, 7], vec![2, 6]], &[vec![0, 2], vec![7, 6]])
    }

    #[test]
    fn two_pure_one_mixed() {
        let sol = solve_support_enumeration(&g()).unwrap();
        assert_eq!(sol.equilibria.len(), 3, "C3 应恰有 3 个均衡");
        assert_has_exact(&sol, &[vec![(1, 1), (0, 1)], vec![(0, 1), (1, 1)]], "C3 纯(Dare,Chicken)");
        assert_has_exact(&sol, &[vec![(0, 1), (1, 1)], vec![(1, 1), (0, 1)]], "C3 纯(Chicken,Dare)");
        assert_has_exact(&sol, &[vec![(1, 3), (2, 3)], vec![(1, 3), (2, 3)]], "C3 混合");
        assert_three_residuals(&sol, "C3");
    }

    #[test]
    fn mixed_payoff_is_14_over_3() {
        let sol = solve_support_enumeration(&g()).unwrap();
        let mixed = sol
            .equilibria
            .iter()
            .find(|e| e.kind == EqKind::Mixed)
            .expect("应存在混合均衡");
        assert_eq!(mixed.payoffs[0], Rational::new(14, 3), "C3 混合均衡 u₁ 应为 14/3");
    }

    #[test]
    fn mixed_is_ess() {
        // Rasmusen §3.2 Table 3：混合 (1/3,2/3) 通过二次条件 ⇒ ESS
        let sol = solve_support_enumeration(&g()).unwrap();
        let mixed = sol
            .equilibria
            .iter()
            .find(|e| e.kind == EqKind::Mixed)
            .expect("应存在混合均衡");
        // denominator = 3：σ 的分量为 1/3 与 2/3，取 3 的格点即覆盖
        let is_ess = refine::ess_symmetric(&g(), &mixed.strategy[0], 3)
            .expect("对称博弈上 ESS 应有定义");
        assert!(
            is_ess,
            "C3 的混合策略应为 ESS（二次条件 u(σ,Dare)>u(Dare,Dare) 成立）"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// C4 2×2 协调博弈（stag hunt 形式）—— SOLVER + SELECTOR（**方向断言**）
// SOURCE: Wikipedia "Risk dominance" Figure 1（stag hunt 例）：
//         矩阵 5,5 | 0,4 / 4,0 | 2,2；混合公式引同页 Figure 2；
//         风险占优判据引 Harsanyi & Selten 1988 Lemma 5.4.4
// 期望：2 纯均衡 + 1 混合 σ=(2/3,1/3)，混合支付 10/3
// **门 ② 的直接落点**：risk_dom → (Gather,Gather)；payoff_dom → (Hunt,Hunt)。
// 两者**方向相反**——只断言"能选出均衡"是没用的，必须断言"选对哪一个"。
// ─────────────────────────────────────────────────────────────────────────────
mod c4_stag_hunt {
    use super::*;

    /// 玩家1: [[5,0],[4,2]]（行 Hunt,Gather）; 玩家2: [[5,4],[0,2]]
    /// u₁ = [[5, 0], [4, 2]]；u₂ = [[5, 4], [0, 2]]（行/列均 Hunt,Gather）
    fn g() -> NormalFormGame {
        bimatrix(&[vec![5, 0], vec![4, 2]], &[vec![5, 4], vec![0, 2]])
    }

    #[test]
    fn two_pure_one_mixed() {
        let sol = solve_support_enumeration(&g()).unwrap();
        assert_eq!(sol.equilibria.len(), 3, "C4 应恰有 3 个均衡");
        assert_has_exact(&sol, &[vec![(1, 1), (0, 1)], vec![(1, 1), (0, 1)]], "C4 纯(Hunt)");
        assert_has_exact(&sol, &[vec![(0, 1), (1, 1)], vec![(0, 1), (1, 1)]], "C4 纯(Gather)");
        assert_has_exact(&sol, &[vec![(2, 3), (1, 3)], vec![(2, 3), (1, 3)]], "C4 混合");
        assert_three_residuals(&sol, "C4");
    }

    #[test]
    fn mixed_payoff_is_10_over_3() {
        // §C4.3 的浮点陷阱：u(σ,σ)=10/3 与 u(Hunt,σ)=10/3 需**精确**相等；
        // ESS 二次条件则依赖**严格**不等（10/3 < 5 故失败）。宽松容差会两者皆错。
        let sol = solve_support_enumeration(&g()).unwrap();
        let mixed = sol
            .equilibria
            .iter()
            .find(|e| e.kind == EqKind::Mixed)
            .expect("应存在混合均衡");
        assert_eq!(mixed.payoffs[0], Rational::new(10, 3), "C4 混合均衡 u₁ 应为 10/3");
    }

    #[test]
    fn risk_dom_selects_gather_gather() {
        // 判据 (C−D)(c−d) ≥ (B−A)(b−a)：代入得 4 > 1 ⇒ 严格风险占优
        // A=u₁(H,H)=5, B=u₁(G,H)=4, C=u₁(H,G)=0, D=u₁(G,G)=2
        let g = g();
        let sol = solve_support_enumeration(&g).unwrap();
        let pure: Vec<Equilibrium> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .cloned()
            .collect();
        let sel = selectors::risk_dominant(&g, &pure);
        assert!(sel.applicable, "C4 上 risk_dom 应适用");
        assert!(!sel.tied, "C4 风险占优应**严格**（4 > 1），不得并列：{sel:?}");
        assert_eq!(sel.selected.len(), 1);
        let chosen = &pure[sel.selected[0]];
        assert_eq!(
            chosen.support,
            vec![vec![1], vec![1]],
            "risk_dom 必须选 (Gather,Gather)，而非 (Hunt,Hunt)"
        );
    }

    #[test]
    fn payoff_dom_selects_hunt_hunt() {
        let g = g();
        let sol = solve_support_enumeration(&g).unwrap();
        let pure: Vec<Equilibrium> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .cloned()
            .collect();
        let sel = selectors::payoff_dominant(&g, &pure);
        assert!(sel.applicable, "C4 上 payoff_dom 应适用");
        assert_eq!(sel.selected.len(), 1);
        let chosen = &pure[sel.selected[0]];
        assert_eq!(
            chosen.support,
            vec![vec![0], vec![0]],
            "payoff_dom 必须选 (Hunt,Hunt)"
        );
    }

    #[test]
    fn the_two_selectors_disagree() {
        // 这是门 ② 的核心：若两者同向，说明实现退化为"同一算法换名字"
        let g = g();
        let sol = solve_support_enumeration(&g).unwrap();
        let pure: Vec<Equilibrium> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .cloned()
            .collect();
        let rd = selectors::risk_dominant(&g, &pure);
        let pd = selectors::payoff_dominant(&g, &pure);
        assert_ne!(
            (rd.selected.clone(), rd.tied),
            (pd.selected.clone(), pd.tied),
            "C4 上 risk_dom 与 payoff_dom **必须**给出不同答案"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// C6 三人标准式博弈（Friday the 13th）—— SOLVER（n=3 路径，**无库可差分**）
//
// ⚠️ 本用例是本套件里**唯一没有第二独立来源**的：n≥3 无法用 nashpy 差分
// （nashpy 上游自述 "A python library for 2 player games"），而教材
// Harrington pp.201–204 未能直读（付费墙）。表格来自 WSU Munoz-Garcia 讲义
// （标题明确标注来源 "Harrington pp 201-204"）→ `[二级引用]`。
// **落笔前应由能访问教材者抽查 pp.201–204**（S1 §6-1 已登记该待办）。
//
// 期望：**0 个纯策略均衡**；**恰好 1 个完全混合均衡**：
//   σ_Tommy = σ_Beth = (1/2, 1/2)，σ_Jason = (8/15, 7/15)
// ─────────────────────────────────────────────────────────────────────────────
mod c6_friday_the_13th {
    use super::*;

    /// profile = (i_tommy, i_beth, i_jason)，0=Front, 1=Back
    /// 按 S1 §C6.1 的八条逐字列举
    fn g() -> NormalFormGame {
        let cells: Vec<(Vec<usize>, Vec<i64>)> = vec![
            (vec![0, 0, 0], vec![0, 0, 0]),
            (vec![0, 1, 0], vec![-4, 1, 2]),
            (vec![1, 0, 0], vec![1, -4, 2]),
            (vec![1, 1, 0], vec![2, 2, -2]),
            (vec![0, 0, 1], vec![3, 3, -2]),
            (vec![0, 1, 1], vec![1, -4, 2]),
            (vec![1, 0, 1], vec![-4, 1, 2]),
            (vec![1, 1, 1], vec![0, 0, 0]),
        ];
        NormalFormGame::from_profiles(vec![2, 2, 2], &cells).expect("合法三人博弈")
    }

    #[test]
    fn no_pure_equilibrium() {
        let sol = solve_support_enumeration(&g()).unwrap();
        let pure: Vec<_> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .collect();
        assert!(
            pure.is_empty(),
            "C6 应无纯策略均衡（讲义：'let's check for psNE (none)'），却得到 {pure:?}"
        );
    }

    #[test]
    fn exactly_one_completely_mixed_equilibrium() {
        let sol = solve_support_enumeration(&g()).unwrap();
        assert_eq!(sol.equilibria.len(), 1, "C6 应恰有 1 个均衡");
        let eq = &sol.equilibria[0];
        assert_eq!(eq.kind, EqKind::Mixed);
        assert!(eq.is_completely_mixed(&g()), "C6 的唯一均衡应完全混合");
        assert_has_exact(
            &sol,
            &[vec![(1, 2), (1, 2)], vec![(1, 2), (1, 2)], vec![(8, 15), (7, 15)]],
            "C6",
        );
        assert_three_residuals(&sol, "C6");
    }

    #[test]
    fn no_ghost_equilibria_survive() {
        // S1 §C6.3 第 3 条记载的**假阳性陷阱**：支撑
        //   (Tommy={Front}, Beth={Back}, Jason={Front,Back})
        //   (Tommy={Back}, Beth={Front}, Jason={Front,Back})
        // 上会解出单参数解族，但被 BR 不等式切成空集（独立验证者已复现）。
        // 本断言确保求解器**没有**把这类幽灵计入解集——解集必须恰为 1。
        let sol = solve_support_enumeration(&g()).unwrap();
        assert_eq!(
            sol.equilibria.len(),
            1,
            "幽灵均衡未被 BR 间隙检查拦住（解集个数 > 1）"
        );
        assert_three_residuals(&sol, "C6 幽灵检查");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// S2 Young 惯例博弈的随机稳定结果 —— SELECTOR（**与风险占优分歧**）
//
// SOURCE: H. P. Young, "The Evolution of Conventions", Econometrica 61(1) 1993,
//         Theorem 3 (p.72) + §8 Example 3 (pp.73–74)
// 矩阵：[[6,0,0],[5,7,5],[0,5,8]]（行/列同序，对称）
// 期望：三个严格纯均衡 (1,1)、(2,2)、(3,3)；
//       **随机稳定 = 策略 2**，而风险占优 = 策略 3 → 两者**必须不同**
//
// 这条同时挡住两种常见实现错误：
//   ① 把 stochastic 实现成"再算一遍风险占优"
//   ② 把随机稳定误当成支付占优/帕累托的同义词（策略 3 支付最高 8）
// ─────────────────────────────────────────────────────────────────────────────
mod s2_young_convention {
    use super::*;

    fn g() -> NormalFormGame {
        bimatrix(
            &[vec![6, 0, 0], vec![5, 7, 5], vec![0, 5, 8]],
            &[vec![6, 5, 0], vec![0, 7, 5], vec![0, 5, 8]],
        )
    }

    #[test]
    fn three_strict_pure_equilibria() {
        let sol = solve_support_enumeration(&g()).unwrap();
        for (i, s) in [(0usize, 0usize), (1, 1), (2, 2)] {
            assert_has_exact(
                &sol,
                &[
                    (0..3).map(|j| if j == s { (1, 1) } else { (0, 1) }).collect(),
                    (0..3).map(|j| if j == s { (1, 1) } else { (0, 1) }).collect(),
                ],
                &format!("S2 纯({i},{i})"),
            );
        }
        assert_three_residuals(&sol, "S2");
    }

    #[test]
    fn stochastic_picks_strategy_2_but_risk_dom_picks_3() {
        // 这是本套件最有区分力的一条断言（S1 §S2）
        let g = g();
        let sol = solve_support_enumeration(&g).unwrap();
        let pure: Vec<Equilibrium> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .cloned()
            .collect();

        // ⚠️ 种群规模必须足够大：Young 的阻力是 (1/8)k 与 (3/8)k 的量级，
        // k 太小时（如 2）两者都塌缩为 1，分离消失 ⇒ 会误报并列。
        // 这是随机稳定理论的**渐近性质**（k → ∞），非实现缺陷。
        let st = selectors::stochastically_stable(&g, &pure, 16);
        assert!(st.applicable, "S2 上 stochastic 应适用");
        assert_eq!(st.selected.len(), 1, "S2 应有唯一随机稳定约定：{st:?}");
        let st_choice = &pure[st.selected[0]];
        assert_eq!(
            st_choice.support,
            vec![vec![1], vec![1]],
            "S2 的随机稳定约定必须是**策略 2**，而实得 {:?}",
            st_choice.support
        );

        let rd = selectors::risk_dominant(&g, &pure);
        assert!(rd.applicable, "S2 上 risk_dom 应适用");
        assert_eq!(rd.selected.len(), 1);
        let rd_choice = &pure[rd.selected[0]];
        assert_eq!(
            rd_choice.support,
            vec![vec![2], vec![2]],
            "S2 的风险占优均衡必须是**策略 3**，而实得 {:?}",
            rd_choice.support
        );

        assert_ne!(
            st_choice.support, rd_choice.support,
            "S2 上 stochastic 与 risk_dom **必须**给出不同答案——同向即说明实现退化"
        );
    }

    #[test]
    fn stochastic_is_not_payoff_dominance() {
        // 策略 3 的支付最高（8），若 stochastic 选了 3，说明它退化为支付占优
        let g = g();
        let sol = solve_support_enumeration(&g).unwrap();
        let pure: Vec<Equilibrium> = sol
            .equilibria
            .iter()
            .filter(|e| e.kind == EqKind::Pure)
            .cloned()
            .collect();
        // ⚠️ 种群规模必须足够大：Young 的阻力是 (1/8)k 与 (3/8)k 的量级，
        // k 太小时（如 2）两者都塌缩为 1，分离消失 ⇒ 会误报并列。
        // 这是随机稳定理论的**渐近性质**（k → ∞），非实现缺陷。
        let st = selectors::stochastically_stable(&g, &pure, 16);
        let chosen = &pure[st.selected[0]];
        assert_ne!(
            chosen.support,
            vec![vec![2], vec![2]],
            "stochastic 不得选支付最高的策略 3 —— 那意味着它被实现成了支付占优"
        );
    }
}

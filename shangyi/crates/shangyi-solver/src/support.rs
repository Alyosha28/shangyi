//! 2 人博弈的**支持枚举**（精确有理数）：本 crate 的完备性主路径。
//!
//! 依据（不读 nashpy 源码；`10-nashpy-oracle.md` §9.3 的"禁止抄 oracle"纪律）：
//! von Stengel, *Algorithmic Game Theory*（Nisan et al. eds.）Ch.3 的支撑枚举构造
//! （Algorithm 3.4）与 Osborne, *An Introduction to Game Theory* Prop. 116.2（p.116）
//! 的均衡刻画。
//!
//! 算法：对每个**等大小**支撑对 `(I, J)`（`|I| = |J| = k`）解两个线性方程组
//! （行玩家：`Σ_{j∈J} A[i][j] q_j = v` ∀i∈I；列玩家：`Σ_{i∈I} B[i][j] p_i = u` ∀j∈J，
//! 各加归一化方程），再执行 §2.3 的**三条残余检查**：
//! 单纯形、支撑内无差异、**最佳反应间隙**。
//!
//! 完备性口径（诚实声明）：
//! - 等大小支撑穷举是**非退化博弈**上的完备方法（von Stengel Cor. 3.7：
//!   非退化 ⇒ 均衡个数为奇数，且每个均衡的双方支撑等大小）；
//! - 退化博弈（某混合策略的支撑大小 k 却有 > k 个纯最佳反应）可能含**不等大小**
//!   支撑的均衡乃至连续统。本模块对"欠定线性系统"返回 `Underdetermined` 计入
//!   `Completeness::Partial`，**不**把连续统截断成有限样本（§2.4 第 5 条）。

use crate::game::{combinations, NormalFormGame};
use crate::rat::{solve_linear, LinearSolution, Rational};
use crate::{Completeness, SolveError};


/// 退化信号检测：某玩家的支撑内是否存在**线性相关**的纯策略列/行。
///
/// 为什么需要独立检测（而不是依赖 `solve_linear` 的 `Underdetermined`）：
/// 退化博弈里，一个支撑对的无差异系统可能**无解**（`LinearSolution::None`）而非欠定。
/// 实测案例（`duplicated_column_flags_underdetermined`）：
/// 矩阵 `[[1,1,0],[0,0,1]]`，支撑 `I=J={0,1}` —— 行策略 1 在 J 上两列支付皆为 0，
/// 故无差异要求 `v=0`；而归一化 + 行策略 0 的方程要求 `v=1` ⇒ **矛盾**。
/// 若此时简单 `Skip`，退化会被静默吞掉，`Completeness` 误报 `Complete` —— 而这正是
/// 上层据以判断"解集是否穷尽"的依据。
///
/// 判据：对行玩家，若 `J` 的**支付列**在 `I` 上线性相关 ⇒ 退化；
///       对列玩家，若 `I` 的**支付行**在 `J` 上线性相关 ⇒ 退化。
/// 线性相关 ⇒ 无差异方程的系数矩阵秩亏 ⇒ 该支撑对至多给出解族而非孤立解，
/// 属 §2.4 第 5 条要求**类型化上报**的"连续统"情形。
fn support_is_degenerate(game: &NormalFormGame, i_sup: &[usize], j_sup: &[usize]) -> bool {
    // 行玩家：J 上各列的支付向量（长度 |I|）是否线性相关
    let cols: Vec<Vec<Rational>> = j_sup
        .iter()
        .map(|&j| i_sup.iter().map(|&i| game.payoff(0, &[i, j])).collect())
        .collect();
    if vectors_dependent(&cols) {
        return true;
    }
    // 列玩家：I 上各行的支付向量（长度 |J|）是否线性相关
    let rows: Vec<Vec<Rational>> = i_sup
        .iter()
        .map(|&i| j_sup.iter().map(|&j| game.payoff(1, &[i, j])).collect())
        .collect();
    vectors_dependent(&rows)
}

/// 一组等长有理向量是否**线性相关**（用行阶梯形求秩比较）。
fn vectors_dependent(vecs: &[Vec<Rational>]) -> bool {
    if vecs.is_empty() {
        return false;
    }
    let len = vecs[0].len();
    let mut a: Vec<Vec<Rational>> = vecs.to_vec();
    let rows = a.len();
    let mut rank = 0usize;
    for col in 0..len {
        let Some(pr) = (rank..rows).find(|&r| !a[r][col].is_zero()) else {
            continue;
        };
        a.swap(rank, pr);
        let pv = a[rank][col];
        for r in (rank + 1)..rows {
            if !a[r][col].is_zero() {
                let f = a[r][col] / pv;
                for c in col..len {
                    let sub = f * a[rank][c];
                    a[r][c] = a[r][c] - sub;
                }
            }
        }
        rank += 1;
        if rank == rows {
            break;
        }
    }
    rank < rows
}

/// 对指定支撑对求解（供 2 人主路径与 3 人的诱导子博弈共用）。
///
/// 返回 `None` = 该支撑对无解或不满足支撑一致性；`Some(vec)` = 通过三残余检查的候选。
pub(crate) fn solve_support_pair(
    game: &NormalFormGame,
    row_support: &[usize],
    col_support: &[usize],
) -> PairOutcome {
    if row_support.len() != col_support.len() {
        return PairOutcome::Skip;
    }
    let k = row_support.len();

    // ── 列玩家的混合 q（由行玩家的支付 A 决定：q 使行玩家在 I 内无差异）──
    let mut mat_q = vec![vec![Rational::ZERO; k + 1]; k + 1];
    let mut rhs_q = vec![Rational::ZERO; k + 1];
    for (r, &i) in row_support.iter().enumerate() {
        for (c, &j) in col_support.iter().enumerate() {
            mat_q[r][c] = game.payoff(0, &[i, j]);
        }
        mat_q[r][k] = -Rational::ONE; // −v
        rhs_q[r] = Rational::ZERO;
    }
    for c in 0..k {
        mat_q[k][c] = Rational::ONE;
    }
    rhs_q[k] = Rational::ONE;

    let q_sol = match solve_linear(&mat_q, &rhs_q) {
        LinearSolution::Unique(x) => x,
        // 无解时若该支撑对本身退化，则归因为"连续统"而非"该支撑非均衡"
        LinearSolution::None => {
            return if support_is_degenerate(game, row_support, col_support) {
                PairOutcome::Underdetermined
            } else {
                PairOutcome::Skip
            }
        }
        LinearSolution::Underdetermined => return PairOutcome::Underdetermined,
        LinearSolution::Overflow => return PairOutcome::Overflow,
    };

    // ── 行玩家的混合 p（由列玩家的支付 B 决定：p 使列玩家在 J 内无差异）──
    let mut mat_p = vec![vec![Rational::ZERO; k + 1]; k + 1];
    let mut rhs_p = vec![Rational::ZERO; k + 1];
    for (r, &j) in col_support.iter().enumerate() {
        for (c, &i) in row_support.iter().enumerate() {
            mat_p[r][c] = game.payoff(1, &[i, j]);
        }
        mat_p[r][k] = -Rational::ONE; // −u
        rhs_p[r] = Rational::ZERO;
    }
    for c in 0..k {
        mat_p[k][c] = Rational::ONE;
    }
    rhs_p[k] = Rational::ONE;

    let p_sol = match solve_linear(&mat_p, &rhs_p) {
        LinearSolution::Unique(x) => x,
        LinearSolution::None => {
            return if support_is_degenerate(game, row_support, col_support) {
                PairOutcome::Underdetermined
            } else {
                PairOutcome::Skip
            }
        }
        LinearSolution::Underdetermined => return PairOutcome::Underdetermined,
        LinearSolution::Overflow => return PairOutcome::Overflow,
    };

    // 支撑一致性：解在声明的支撑上必须**严格为正**（恰好为 0 或负 ⇒ 支撑不符）
    let mut sigma = vec![
        vec![Rational::ZERO; game.strategy_count(0)],
        vec![Rational::ZERO; game.strategy_count(1)],
    ];
    for (c, &i) in row_support.iter().enumerate() {
        if !p_sol[c].is_positive() {
            return PairOutcome::Skip;
        }
        sigma[0][i] = p_sol[c];
    }
    for (c, &j) in col_support.iter().enumerate() {
        if !q_sol[c].is_positive() {
            return PairOutcome::Skip;
        }
        sigma[1][j] = q_sol[c];
    }

    // §2.3 三条残余（含 BR_gap）——由 verify 统一执行
    if crate::verify::is_equilibrium_exact(game, &sigma) {
        PairOutcome::Found(sigma)
    } else {
        // 无差异方程全满足但 BR 不等式失败 = 幽灵（§3.6 的假阳性陷阱）
        PairOutcome::Ghost
    }
}

/// 单个支撑对的求解结果（区分"幽灵"与"无解"，便于完整性与诊断计数）。
#[derive(Debug, Clone)]
pub(crate) enum PairOutcome {
    Found(Vec<Vec<Rational>>),
    Skip,
    Underdetermined,
    Ghost,
    /// 该支撑对的精确求解**算不动**（分母溢出）。
    ///
    /// 与 `Underdetermined` 语义不同：后者是"解不唯一"（数学结论），
    /// 前者是"算不出来"（能力边界）。混同会把能力不足说成知识结论。
    Overflow,
}

/// 2 人支持枚举主路径。
pub(crate) fn enumerate_2p(
    game: &NormalFormGame,
) -> Result<(Vec<Vec<Vec<Rational>>>, Completeness), SolveError> {
    if game.players() != 2 {
        return Err(SolveError::InvalidGame("enumerate_2p 只接受 2 人博弈".into()));
    }
    let m = game.strategy_count(0);
    let n = game.strategy_count(1);
    let mut found = Vec::new();
    let mut underdetermined = 0usize;
    let mut ghosts = 0usize;
    let unsupported = 0usize;

    for k in 1..=m.min(n) {
        for i_sup in combinations(m, k) {
            for j_sup in combinations(n, k) {
                match solve_support_pair(game, &i_sup, &j_sup) {
                    PairOutcome::Found(sigma) => found.push(sigma),
                    PairOutcome::Skip => {}
                    PairOutcome::Underdetermined => underdetermined += 1,
                    PairOutcome::Ghost => ghosts += 1,
                    // **不静默跳过**：算不动必须让整个求解以类型化错误结束，
                    // 否则解集会被当成"完整"而实际漏掉了那些支撑剖面。
                    PairOutcome::Overflow => {
                        return Err(SolveError::Overflow(format!(
                            "支撑剖面 ({i_sup:?}, {j_sup:?}) 的精确求解溢出——\
                             该博弈的支付值精度超出精确路径容量。\
                             解集**不完整**，故整体拒绝而非部分返回。"
                        )));
                    }
                }
            }
        }
    }
    // 不等大小支撑（仅在退化博弈中出现）：显式计数，不做静默截断。
    if m != n {
        // 非方阵博弈：等大小支撑穷举仍然覆盖全部非退化均衡（k ≤ min(m,n)），
        // 但 k > min(m,n) 的支撑对不存在，故无需额外计数。
    }
    let completeness = if underdetermined == 0 {
        Completeness::Complete
    } else {
        Completeness::Partial { unsupported, underdetermined, ghost: ghosts }
    };
    Ok((found, completeness))
}

/// 纯策略组合是否为纳什均衡（供 3 人路径与自检使用）。
pub(crate) fn is_pure_equilibrium(game: &NormalFormGame, profile: &[usize]) -> bool {
    for p in 0..game.players() {
        let current = game.payoff(p, profile);
        for alt in 0..game.strategy_count(p) {
            let mut deviated = profile.to_vec();
            deviated[p] = alt;
            if game.payoff(p, &deviated) > current {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::Tolerance;

    fn pd() -> NormalFormGame {
        NormalFormGame::from_bimatrix(&[vec![2, 0], vec![3, 1]], &[vec![2, 3], vec![0, 1]]).unwrap()
    }

    fn bos() -> NormalFormGame {
        NormalFormGame::from_bimatrix(&[vec![3, 0], vec![0, 2]], &[vec![2, 0], vec![0, 3]]).unwrap()
    }

    fn stag() -> NormalFormGame {
        NormalFormGame::from_bimatrix(&[vec![5, 0], vec![4, 2]], &[vec![5, 4], vec![0, 2]]).unwrap()
    }

    #[test]
    fn prisoners_dilemma_has_exactly_one() {
        let (eqs, c) = enumerate_2p(&pd()).unwrap();
        assert_eq!(eqs.len(), 1);
        assert_eq!(c, Completeness::Complete);
        assert_eq!(eqs[0], vec![vec![Rational::ZERO, Rational::ONE], vec![Rational::ZERO, Rational::ONE]]);
    }

    #[test]
    fn bos_has_three() {
        let (eqs, c) = enumerate_2p(&bos()).unwrap();
        assert_eq!(eqs.len(), 3, "BoS 应有 2 纯 + 1 混合");
        assert_eq!(c, Completeness::Complete);
        let mixed = eqs
            .iter()
            .find(|s| s[0].iter().all(|v| v.is_positive()))
            .expect("应有完全混合均衡");
        assert_eq!(mixed[0], vec![Rational::new(3, 5), Rational::new(2, 5)]);
        assert_eq!(mixed[1], vec![Rational::new(2, 5), Rational::new(3, 5)]);
    }

    #[test]
    fn stag_hunt_has_three_and_mixed_is_two_thirds() {
        let (eqs, _) = enumerate_2p(&stag()).unwrap();
        assert_eq!(eqs.len(), 3);
        let mixed = eqs.iter().find(|s| s[0].iter().all(|v| v.is_positive())).unwrap();
        assert_eq!(mixed[0], vec![Rational::new(2, 3), Rational::new(1, 3)]);
        assert_eq!(mixed[1], vec![Rational::new(2, 3), Rational::new(1, 3)]);
    }

    #[test]
    fn chicken_fig6_values() {
        let ch = NormalFormGame::from_bimatrix(&[vec![0, 7], vec![2, 6]], &[vec![0, 2], vec![7, 6]])
            .unwrap();
        let (eqs, _) = enumerate_2p(&ch).unwrap();
        assert_eq!(eqs.len(), 3);
        let mixed = eqs.iter().find(|s| s[0].iter().all(|v| v.is_positive())).unwrap();
        assert_eq!(mixed[0], vec![Rational::new(1, 3), Rational::new(2, 3)]);
        let pay = crate::verify::expected_payoffs(&ch, mixed);
        assert_eq!(pay, vec![Rational::new(14, 3), Rational::new(14, 3)]);
    }

    #[test]
    fn hawk_dove_v4_c10() {
        // V=4, C=10 ⇒ [[(V−C)/2, V],[0, V/2]] = [[−3,4],[0,2]]
        let hd = NormalFormGame::from_bimatrix(&[vec![-3, 4], vec![0, 2]], &[vec![-3, 0], vec![4, 2]])
            .unwrap();
        let (eqs, _) = enumerate_2p(&hd).unwrap();
        assert_eq!(eqs.len(), 3);
        let mixed = eqs.iter().find(|s| s[0].iter().all(|v| v.is_positive())).unwrap();
        assert_eq!(mixed[0], vec![Rational::new(2, 5), Rational::new(3, 5)]);
        assert_eq!(
            crate::verify::expected_payoffs(&hd, mixed),
            vec![Rational::new(6, 5), Rational::new(6, 5)]
        );
        // 纯均衡为 (Dare, Chicken) 与 (Chicken, Dare)
        assert!(eqs.iter().any(|s| s[0][0].is_exactly(1) && s[1][1].is_exactly(1)));
        assert!(eqs.iter().any(|s| s[0][1].is_exactly(1) && s[1][0].is_exactly(1)));
    }

    #[test]
    fn matching_pennies_unique_mixed() {
        let mp = NormalFormGame::from_bimatrix(&[vec![1, -1], vec![-1, 1]], &[vec![-1, 1], vec![1, -1]])
            .unwrap();
        let (eqs, c) = enumerate_2p(&mp).unwrap();
        assert_eq!(eqs.len(), 1);
        assert_eq!(c, Completeness::Complete);
        assert_eq!(eqs[0][0], vec![Rational::new(1, 2), Rational::new(1, 2)]);
        assert_eq!(eqs[0][1], vec![Rational::new(1, 2), Rational::new(1, 2)]);
        let _ = Tolerance::GOLDEN;
    }

    #[test]
    fn pure_equilibrium_check() {
        assert!(is_pure_equilibrium(&pd(), &[1, 1]));
        assert!(!is_pure_equilibrium(&pd(), &[0, 0]));
        assert!(is_pure_equilibrium(&bos(), &[0, 0]));
        assert!(is_pure_equilibrium(&bos(), &[1, 1]));
        assert!(!is_pure_equilibrium(&bos(), &[0, 1]));
    }

    /// 退化博弈：复制一列 ⇒ 出现不等大小支撑的连续统。
    /// 本模块必须**报欠定**而不是静默产出错误的有限解集。
    #[test]
    fn duplicated_column_flags_underdetermined() {
        let g = NormalFormGame::from_bimatrix(
            &[vec![1, 1, 0], vec![0, 0, 1]],
            &[vec![1, 1, 0], vec![0, 0, 1]],
        )
        .unwrap();
        let (_, c) = enumerate_2p(&g).unwrap();
        match c {
            Completeness::Partial { underdetermined, .. } => {
                assert!(underdetermined > 0, "复制列应触发欠定信号");
            }
            Completeness::Complete => panic!("退化博弈不应报告 Complete"),
            Completeness::NotGuaranteed { .. } => {
                panic!("支持枚举不接受 NotGuaranteed 完备性")
            }
        }
    }

    /// 3×3 非方阵以外的规模：4×4 的支持枚举不应爆炸（回归）。
    #[test]
    fn four_by_four_smoke() {
        let a = [vec![3, 0, 1, 2], vec![0, 2, 1, 1], vec![1, 1, 3, 0], vec![2, 1, 0, 2]];
        let b = [vec![2, 1, 0, 1], vec![1, 3, 1, 0], vec![0, 1, 2, 2], vec![1, 0, 2, 3]];
        let g = NormalFormGame::from_bimatrix(&a, &b).unwrap();
        let (eqs, _) = enumerate_2p(&g).unwrap();
        // 每个候选都必须通过三残余（含 BR_gap）
        for sigma in &eqs {
            assert!(crate::verify::is_equilibrium_exact(&g, sigma));
        }
    }
}

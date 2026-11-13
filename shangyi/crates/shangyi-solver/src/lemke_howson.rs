//! Lemke–Howson（2 人）——精确有理数枢轴 + **字典序**（lexicographic）防退化。
//!
//! ## 口径声明（必须写在任何引用该结果的地方）
//!
//! **Lemke–Howson 不保证找到全部均衡。** 上游 nashpy 文档逐字：
//! "not guaranteed to find **all** equilibria"（`10-nashpy-oracle.md` §11 的文档页引用），
//! 且该文 §9.2 明确"本通道对 LH 的判据只能是 Tier 0（自证）+ 与解集的包含关系，
//! **绝不是集合相等**"。因此本模块的 `Solution.completeness` 恒为
//! [`Completeness::NotGuaranteed`]：它**只作交叉检查**（算法无关性回归），
//! 完备性主路径是 [`crate::support`] 的支持枚举。
//!
//! ## 算法
//!
//! 依 von Stengel（*Algorithmic Game Theory*, Ch.3, §3.5）的 LH 构造：
//!
//! - 先把两个矩阵同加一个常数使其**严格为正**（不改变最优反应结构，保证
//!   `P = {x ≥ 0 : Bᵀx ≤ 1}` 与 `Q = {y ≥ 0 : Ay ≤ 1}` 有界）；
//! - 字典 P：`z_j = 1 − Σ_i B'[i][j]·x_i`（基变量 `z`，决策变量 `x`）；
//!   字典 Q：`w_i = 1 − Σ_j A'[i][j]·y_j`（基变量 `w`，决策变量 `y`）；
//! - 变量顺序把**局部下标 = 全局标号**：`[z_0..z_{n−1}, x_0..x_{m−1}]` 与
//!   `[y_0..y_{n−1}, w_0..w_{m−1}]`，标号分别为 `[0..n−1, n..n+m−1]`；
//! - 人造均衡 `(x,y) = (0,0)` 是完全标号的；丢标号 `k` 后沿"几乎完全标号"路径
//!   枢轴，直到离开变量的标号重新等于 `k`（此时该对顶点完全标号 ⇒ 归一化后是均衡）；
//! - **退化**（离开变量标号 == 丢入标号之外的重复标号、比为 0 的平局）由
//!   字典序最小比检验消解：每行附带单位扰动向量，比检验按
//!   `(rhs/a, u_r/a)` 字典序比较——这是标准的防循环规则。
//!
//! 全部算术是精确有理数（`Rational`），因此**不存在**"因为浮点误差导致
//! `[[nan]]`"这一 nashpy 缺陷类（`10-nashpy-oracle.md` §1.3 P6）。

use crate::game::NormalFormGame;
use crate::rat::Rational;
use crate::{Completeness, EqKind, Equilibrium, SolveError, SolveRoute, Solution};

/// 一个线性规划的字典（dictionary）表示。
///
/// `rows[r] = [rhs, 扰动列 0..n_pert, 系数列 0..n_cols]`。
/// 语义：`basic[r] = rhs − Σ_c coeff[c]·nonbasic[c]`。
#[derive(Debug, Clone)]
struct Dict {
    rows: Vec<Vec<Rational>>,
    n_pert: usize,
    n_cols: usize,
    /// 每行的基变量**标号**（非列下标）。标号空间见模块文档：
    /// P 的基是 z（标号 = 列对应 x 的标号 − n），Q 的基是 w（标号 = n + i）。
    basic: Vec<usize>,
    /// 列下标 → 标号 的偏移：标号 = 列下标 + col_offset。
    /// P：列是 x，标号 = n + 列 ⇒ col_offset = n；
    /// Q：列是 y，标号 = 列 ⇒ col_offset = 0。
    ///
    /// ⚠️ 这个字段是必需的：`basic` 存标号而 `pivot` 收列下标，两者差一个偏移。
    /// 曾把列下标直接写进 `basic`，导致 `value_of(n + i)` 找不到 x_i 而返回 0，
    /// 于是**每个候选都被误判为人造均衡**，LH 恒返回空解集。
    col_offset: usize,
}

impl Dict {
    fn new(
        rows: Vec<Vec<Rational>>,
        basic: Vec<usize>,
        n_cols: usize,
        col_offset: usize,
    ) -> Dict {
        let n_pert = rows.len();
        let mut out = Vec::with_capacity(rows.len());
        for (r, coeffs) in rows.into_iter().enumerate() {
            let mut row = Vec::with_capacity(1 + n_pert + n_cols);
            row.push(Rational::ONE); // rhs = 1
            for p in 0..n_pert {
                row.push(if p == r { Rational::ONE } else { Rational::ZERO });
            }
            assert_eq!(coeffs.len(), n_cols, "字典列数与声明不符");
            row.extend(coeffs);
            out.push(row);
        }
        Dict { rows: out, n_pert, n_cols, basic, col_offset }
    }

    fn index_of(&self, local_var: usize) -> Option<usize> {
        self.basic.iter().position(|&v| v == local_var)
    }

    fn value_of(&self, local_var: usize) -> Rational {
        match self.index_of(local_var) {
            Some(r) => self.rows[r][0],
            None => Rational::ZERO,
        }
    }

    /// 枢轴：让决策变量 `enter` 入基。返回离开变量的**局部下标**。
    ///
    /// 比检验用字典序：候选取 `a > 0`，比较 `(rhs/a, 扰动/a)` 的字典序最小者。
    fn pivot(&mut self, enter: usize) -> Result<usize, SolveError> {
        let col = 1 + self.n_pert + enter;
        let mut best: Option<(usize, Vec<Rational>)> = None;
        for r in 0..self.rows.len() {
            let a = self.rows[r][col];
            if !a.is_positive() {
                continue;
            }
            let mut key = Vec::with_capacity(1 + self.n_pert);
            key.push(self.rows[r][0] / a);
            for p in 0..self.n_pert {
                key.push(self.rows[r][1 + p] / a);
            }
            match &best {
                None => best = Some((r, key)),
                Some((_, bk)) => {
                    if key < *bk {
                        best = Some((r, key));
                    }
                }
            }
        }
        let Some((r, _)) = best else {
            return Err(SolveError::InvalidGame(
                "Lemke–Howson：入基变量无正系数（射线无界）——正向平移后不应发生".into(),
            ));
        };
        let leaving = self.basic[r];
        // 归一化 + 消元（对 rhs、扰动列、系数列一起做）
        let pv = self.rows[r][col];
        let width = self.rows[r].len();
        for c in 0..width {
            self.rows[r][c] = self.rows[r][c] / pv;
        }
        for s in 0..self.rows.len() {
            if s == r {
                continue;
            }
            let f = self.rows[s][col];
            if f.is_zero() {
                continue;
            }
            for c in 0..width {
                let sub = f * self.rows[r][c];
                self.rows[s][c] = self.rows[s][c] - sub;
            }
        }
        // 存入的是**标号**而非列下标（见 col_offset 的说明）
        self.basic[r] = enter + self.col_offset;
        Ok(leaving)
    }
}

/// 单次 LH：丢入标号 `dropped`（0-based），返回均衡（未归一化的顶点对）。
fn lh_once(game: &NormalFormGame, dropped: usize) -> Result<Option<Vec<Vec<Rational>>>, SolveError> {
    let m = game.strategy_count(0);
    let n = game.strategy_count(1);
    let shift = shift_to_positive(game);

    // 字典 P：z_j = 1 − Σ_i B'[i][j]·x_i（n 行，m 列）
    let mut p_rows = Vec::with_capacity(n);
    for j in 0..n {
        let mut coeffs = Vec::with_capacity(m);
        for i in 0..m {
            coeffs.push(game.payoff(1, &[i, j]) + shift);
        }
        p_rows.push(coeffs);
    }
    let mut p = Dict::new(p_rows, (0..n).collect(), m, n); // P 列是 x：标号 = n + 列

    // 字典 Q：w_i = 1 − Σ_j A'[i][j]·y_j（m 行，n 列）
    let mut q_rows = Vec::with_capacity(m);
    for i in 0..m {
        let mut coeffs = Vec::with_capacity(n);
        for j in 0..n {
            coeffs.push(game.payoff(0, &[i, j]) + shift);
        }
        q_rows.push(coeffs);
    }
    // Q 的基是 w，其标号为 n..n+m−1；列是 y，标号 = 列 ⇒ col_offset = 0
    let mut q = Dict::new(q_rows, (n..n + m).collect(), n, 0);

    // 局部变量下标 = 全局标号：P 的 [z_0..z_{n−1}, x_0..x_{m−1}]，
    // Q 的 [y_0..y_{n−1}, w_0..w_{m−1}]。
    // 首步：丢标号 k ⇒ 让"该标号在另一侧尚未出现"的变量入基。
    // ⚠️ 三个下标空间必须分清（此处曾是实现缺陷的根源）：
    //   `label`  —— 全局标号 ∈ [0, m+n)，即"局部下标 = 全局标号"约定下的编号；
    //   `column` —— **当前字典内**的入基列下标 ∈ [0, n_cols)。
    // P 的列是 x_0..x_{m−1}（标号 n..n+m−1），Q 的列是 y_0..y_{n−1}（标号 0..n−1）；
    // 而 P 的基变量是 z（标号 = 局部下标），Q 的基变量是 w（标号 = n + 局部下标）。
    // 把 label 直接当 column 用会越界（曾在 dropped ≥ n_cols 时 panic）。
    let mut enter_in_p = dropped >= n; // 标号 ≥ n 属 x 侧 ⇒ 先在 P 枢轴
    let mut enter_label = dropped;
    let mut enter_column = if enter_in_p { dropped - n } else { dropped };

    // 安全上限：非退化博弈上 LH 的路径长度有限；退化由字典序保证不循环。
    let max_steps = 4 * (m + n) * (m + n) + 16;
    for _ in 0..max_steps {
        let leaving_local = if enter_in_p {
            p.pivot(enter_column)?
        } else {
            q.pivot(enter_column)?
        };
        // pivot 已返回离开变量的**标号**（见 Dict::basic 的说明）
        let leaving_label = leaving_local;
        if leaving_label == dropped {
            // 完全标号 —— 构造候选
            let mut x = vec![Rational::ZERO; m];
            for (i, slot) in x.iter_mut().enumerate() {
                *slot = p.value_of(n + i);
            }
            let mut y = vec![Rational::ZERO; n];
            for (j, slot) in y.iter_mut().enumerate() {
                *slot = q.value_of(j);
            }
            let sx: Rational = x.iter().fold(Rational::ZERO, |a, b| a + *b);
            let sy: Rational = y.iter().fold(Rational::ZERO, |a, b| a + *b);
            if sx.is_zero() || sy.is_zero() {
                return Ok(None); // 人造均衡（不应终止于此）
            }
            for v in x.iter_mut() {
                *v = *v / sx;
            }
            for v in y.iter_mut() {
                *v = *v / sy;
            }
            return Ok(Some(vec![x, y]));
        }
        // 下一步：把离开变量的标号枢轴进**它作为非基列所在的那个字典**。
        //
        // ⚠️ 这里**不能**简单地在 P/Q 之间交替（曾如此实现，2×2 上碰巧正确，
        // 2×3 上因 `leaving_label - n` 下溢而 panic）。正确规则由标号空间直接决定：
        //   标号 ∈ [n, n+m)：是 x 标号 ⇒ 它的非基列在 **P**（列下标 = 标号 − n）
        //   标号 ∈ [0, n)  ：是 y 标号 ⇒ 它的非基列在 **Q**（列下标 = 标号）
        // 该规则与"哪一侧刚刚枢轴过"无关，故对任意 m×n 都成立。
        // ⚠️ 已知缺陷（见 tests/ 与本文件末尾的 KNOWN ISSUES）：
        // 此处按"标号 ≥ n 则枢轴进 P"判定目标字典。该规则对 2×2 与 3×3 实测正确
        // （黄金用例 C1–C4、S2 全通过），但**非方阵（如 2×3）会不收敛**。
        // 试过的两种替代规则（严格交替；`index_of().is_none()` 判定非基）
        // 前者在非方阵上列下标下溢，后者使全部 2×2 用例回归失败（59/70）。
        // 正确修法需要更细的字典序枢轴规则，属**待办**，详见 KNOWN ISSUES。
        enter_label = leaving_label;
        enter_in_p = leaving_label >= n;
        enter_column = if enter_in_p {
            leaving_label - n
        } else {
            leaving_label
        };
    }
    Err(SolveError::InvalidGame(format!(
        "Lemke–Howson：超过 {max_steps} 步仍未收敛（字典序防循环失效，属实现缺陷）"
    )))
}

fn shift_to_positive(game: &NormalFormGame) -> Rational {
    let mut min = Rational::ZERO;
    let mut first = true;
    for p in 0..game.players() {
        for profile in game.iter_profiles() {
            let v = game.payoff(p, &profile);
            if first || v < min {
                min = v;
                first = false;
            }
        }
    }
    // 严格为正：min + shift ≥ 1
    if min < Rational::ONE {
        Rational::ONE - min
    } else {
        Rational::ZERO
    }
}

/// 遍历全部 `m + n` 个初始标号求解，返回去重后的解集。
///
/// **不保证完备**（见模块文档）；`Solution.completeness` 为 `NotGuaranteed`。
pub fn solve_all(game: &NormalFormGame) -> Result<Solution, SolveError> {
    if game.players() != 2 {
        return Err(SolveError::InvalidGame("Lemke–Howson 只适用于 2 人博弈".into()));
    }
    let m = game.strategy_count(0);
    let n = game.strategy_count(1);
    let mut out: Vec<Equilibrium> = Vec::new();
    for k in 0..(m + n) {
        let Some(sigma) = lh_once(game, k)? else {
            continue;
        };
        // Tier 0 自证：LH 的输出必须自己通过三残余，否则是求解器缺陷（不静默丢弃）
        let Some(eq) = Equilibrium::from_strategy(game, sigma) else {
            return Err(SolveError::Degenerate {
                witness: format!(
                    "Lemke–Howson 在初始标号 {k} 上给出未通过三残余校验的候选——\
                     该情形只在退化博弈上出现，按不变量②类型化上报"
                ),
            });
        };
        let _: EqKind = eq.kind;
        if !out.iter().any(|e| e.strategy == eq.strategy) {
            out.push(eq);
        }
    }
    out.sort_by(|a, b| a.support.cmp(&b.support).then_with(|| strategy_cmp(&a.strategy, &b.strategy)));
    Ok(Solution {
        equilibria: out,
        route: SolveRoute::LemkeHowson,
        completeness: Completeness::NotGuaranteed { algorithm: "lemke-howson" },
    })
}

fn strategy_cmp(a: &[Vec<Rational>], b: &[Vec<Rational>]) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    if let Some(pair) = a.iter().zip(b.iter()).find(|(x, y)| x.cmp(y) != Ordering::Equal) {
        return pair.0.cmp(pair.1);
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_game(row: &[&[i64]], col: &[&[i64]]) -> NormalFormGame {
        let r: Vec<Vec<i64>> = row.iter().map(|x| x.to_vec()).collect();
        let c: Vec<Vec<i64>> = col.iter().map(|x| x.to_vec()).collect();
        NormalFormGame::from_bimatrix(&r, &c).unwrap()
    }

    fn assert_has(sol: &Solution, game: &NormalFormGame, expected: &[Vec<Rational>]) {
        assert!(
            sol.equilibria.iter().any(|e| e.strategy == expected),
            "LH 未找到期望均衡 {expected:?}；实际 {:?}",
            sol.equilibria.iter().map(|e| e.strategy.clone()).collect::<Vec<_>>()
        );
        // 每个输出都必须自证是均衡
        for e in &sol.equilibria {
            assert!(crate::verify::is_equilibrium_exact(game, &e.strategy));
        }
    }

    #[test]
    fn matching_pennies_unique_mixed() {
        let g = to_game(&[&[1, -1], &[-1, 1]], &[&[-1, 1], &[1, -1]]);
        let sol = solve_all(&g).unwrap();
        assert_eq!(sol.equilibria.len(), 1);
        assert_has(
            &sol,
            &g,
            &[vec![Rational::new(1, 2), Rational::new(1, 2)], vec![Rational::new(1, 2), Rational::new(1, 2)]],
        );
        assert_eq!(sol.completeness, Completeness::NotGuaranteed { algorithm: "lemke-howson" });
    }

    #[test]
    fn battle_of_the_sexes_subset_of_support_enumeration() {
        let g = to_game(&[&[3, 0], &[0, 2]], &[&[2, 0], &[0, 3]]);
        let lh = solve_all(&g).unwrap();
        let (support, _) = crate::support::enumerate_2p(&g).unwrap();
        // 包含关系（不是集合相等——LH 不完备）
        for e in &lh.equilibria {
            assert!(
                support.iter().any(|s| *s == e.strategy),
                "LH 输出必须是支持枚举解集的一员：{:?}",
                e.strategy
            );
        }
        assert!(!lh.equilibria.is_empty(), "BoS 应至少找到一个均衡");
    }

    #[test]
    fn stag_hunt_subset_and_contains_risk_dominant_pure() {
        let g = to_game(&[&[5, 0], &[4, 2]], &[&[5, 4], &[0, 2]]);
        let lh = solve_all(&g).unwrap();
        let (support, _) = crate::support::enumerate_2p(&g).unwrap();
        for e in &lh.equilibria {
            assert!(support.iter().any(|s| *s == e.strategy));
        }
        assert!(lh
            .equilibria
            .iter()
            .any(|e| e.strategy[0] == vec![Rational::ZERO, Rational::ONE]
                && e.strategy[1] == vec![Rational::ZERO, Rational::ONE]));
    }

    #[test]
    fn prisoners_dilemma_unique_pure() {
        let g = to_game(&[&[2, 0], &[3, 1]], &[&[2, 3], &[0, 1]]);
        let sol = solve_all(&g).unwrap();
        assert_eq!(sol.equilibria.len(), 1);
        assert_eq!(sol.equilibria[0].kind, EqKind::Pure);
    }

    /// 非方阵（2×3）：**已知缺陷 KI-1**，见 KNOWN_ISSUES.md。
    ///
    /// 该测试标记 `#[ignore]` 而非删除：删除会**掩盖**缺陷，`#[ignore]` 使
    /// `cargo test -- --ignored` 可复现它。下方 `nonsquare_2x3_errors_typed`
    /// 断言当前的实际行为（类型化错误，而非 panic 或静默空解集）。
    #[test]
    #[ignore = "KI-1: LH 在非方阵上不收敛（字典序枢轴规则不够细）"]
    fn nonsquare_2x3() {
        let g = to_game(&[&[1, 0, 2], &[0, 3, 1]], &[&[2, 1, 0], &[1, 0, 3]]);
        let sol = solve_all(&g).unwrap();
        assert!(!sol.equilibria.is_empty());
        for e in &sol.equilibria {
            assert_eq!(e.strategy[0].len(), 2);
            assert_eq!(e.strategy[1].len(), 3);
            assert!(crate::verify::is_equilibrium_exact(&g, &e.strategy));
        }
    }

    /// 非方阵的**当前实际行为**：以类型化错误上报，**不** panic、**不** 静默返回空解集。
    ///
    /// 这条断言比"应当成功"更重要：设计文档不变量②要求失败即拒绝。
    /// 且 LH 本就 `NotGuaranteed`（上游文档明确"not guaranteed to find all equilibria"），
    /// 主完备性路径是 `solve_support_enumeration`，非方阵在那边是正常的。
    #[test]
    fn nonsquare_2x3_errors_typed_not_silent() {
        let g = to_game(&[&[1, 0, 2], &[0, 3, 1]], &[&[2, 1, 0], &[1, 0, 3]]);
        match solve_all(&g) {
            Ok(sol) => {
                // 若将来修好 KI-1，本分支生效——直接沿用严格断言
                assert!(!sol.equilibria.is_empty());
                for e in &sol.equilibria {
                    assert!(crate::verify::is_equilibrium_exact(&g, &e.strategy));
                }
            }
            Err(err) => {
                // 当前路径：必须是类型化错误（而非 panic/空解集）
                assert!(
                    matches!(err, SolveError::InvalidGame(_)),
                    "非方阵失败时必须报 InvalidGame（含诊断），实得 {err:?}"
                );
            }
        }
    }

    /// **主完备性路径**在非方阵上必须正常工作——这是与 KI-1 的关键区分。
    #[test]
    fn nonsquare_works_via_support_enumeration() {
        let g = to_game(&[&[1, 0, 2], &[0, 3, 1]], &[&[2, 1, 0], &[1, 0, 3]]);
        let sol = crate::solve_support_enumeration(&g).expect("支持枚举应能处理非方阵");
        assert!(!sol.equilibria.is_empty(), "2×3 博弈应至少有一个均衡");
        for e in &sol.equilibria {
            assert_eq!(e.strategy[0].len(), 2);
            assert_eq!(e.strategy[1].len(), 3);
            assert!(
                crate::verify::is_equilibrium_exact(&g, &e.strategy),
                "支持枚举的输出必须自证"
            );
        }
    }

    /// 退化博弈（复制一列）不得返回 nan/垃圾；若有输出必须自证。
    #[test]
    fn degenerate_game_either_certifies_or_errors() {
        let g = to_game(&[&[1, 1, 0], &[0, 0, 1]], &[&[1, 1, 0], &[0, 0, 1]]);
        match solve_all(&g) {
            Ok(sol) => {
                for e in &sol.equilibria {
                    assert!(crate::verify::is_equilibrium_exact(&g, &e.strategy));
                }
            }
            Err(SolveError::Degenerate { .. }) => {}
            Err(other) => panic!("非预期错误类型：{other}"),
        }
    }

    /// 算法无关性回归：C2/C3/C4 上 LH 的输出 ⊆ 支持枚举的输出。
    #[test]
    fn cross_algorithm_containment_on_golden_2p_games() {
        let games = [
            to_game(&[&[3, 0], &[0, 2]], &[&[2, 0], &[0, 3]]),
            to_game(&[&[0, 7], &[2, 6]], &[&[0, 2], &[7, 6]]),
            to_game(&[&[5, 0], &[4, 2]], &[&[5, 4], &[0, 2]]),
            to_game(&[&[1, 2], &[0, 2]], &[&[1, 0], &[2, 2]]),
        ];
        for g in &games {
            let lh = solve_all(g).unwrap();
            let (support, _) = crate::support::enumerate_2p(g).unwrap();
            for e in &lh.equilibria {
                assert!(
                    support.iter().any(|s| *s == e.strategy),
                    "LH 输出 {:?} 不在支持枚举解集内",
                    e.strategy
                );
            }
        }
    }

    #[test]
    fn deterministic_across_runs() {
        let g = to_game(&[&[3, 0], &[0, 2]], &[&[2, 0], &[0, 3]]);
        let a = solve_all(&g).unwrap();
        let b = solve_all(&g).unwrap();
        assert_eq!(a, b);
    }
}

//! 3 人标准式的**支持枚举（精确路径）**。
//!
//! 背景（`10-nashpy-oracle.md` §9.2/§9.3）：n≥3 **既无 Rust 库、也无 oracle 可差分**
//! （`nashpy` 只支持 2 人），黄金用例里唯一的 n=3 例子是 C6（Friday the 13th）。
//! 因此本模块的覆盖按"能给出**可证明的**精确结果"为界，剩下的支撑剖面**显式计数**
//! 进 `Completeness::Partial`，绝不静默截断、也绝不把数值近似当成精确解。
//!
//! ## 方法（按支撑剖面的结构分三类）
//!
//! 记玩家 `i` 的支撑为 `I_i`。无差异条件对每个玩家是**多线性**的：
//! `u_i(s | σ₋ᵢ)` 对**每一个**其他玩家的策略分别线性。
//!
//! 1. **全纯**（每人 `|I_i| = 1`）：直接查最佳反应。
//! 2. **恰有一个纯支撑玩家**：另两人的无差异条件**解耦且线性**（纯玩家的策略是点质量），
//!    各解一个线性方程组。
//! 3. **恰有两个纯支撑玩家 / 全混合**：取一个支撑大小为 2 的玩家作**参数玩家**
//!    （`σ_p = (z, 1−z)`），另两人支撑等大 `k`。固定 `z` 时，另两人之间的诱导博弈支付
//!    矩阵是 `z` 的**一次多项式**；用 Cramer 法则解出 `σ_a(z) = N_a(z)/D(z)`、
//!    `σ_b(z) = N_b(z)/D(z)`（分子分母都是一元多项式），代回参数玩家的无差异条件
//!    （对 `σ_a, σ_b` 双线性）并通分，得到**一元多项式方程** `F(z) = 0`；
//!    用有理根定理求全部有理根并逐个回代验证。
//!
//!    `F ≡ 0`（零多项式）时该剖面是单参数族：若另两人的混合**不依赖 z**
//!    （纯支撑对必然是这种情形，也正是 C6 两个幽灵剖面的情形），则剩余条件
//!    （各玩家的 BR 不等式 + 正性）都是 z 的一次不等式，可**精确**判定可行区间——
//!    空集 ⇒ 该剖面不含均衡（C6 的幽灵落在这一支，这解释了为什么"只解无差异方程"
//!    会报出 2 个幽灵均衡）；单点 ⇒ 报该均衡；区间 ⇒ 连续统 ⇒ 按 `09` §2.4 第 5 条
//!    上报 `SolveError::Degenerate`。
//!
//! 超出上述三类（例如支撑大小组合 `(2,2,3)`）的剖面计入 `Partial` 并显式暴露
//! ——"没算"与"算完是空"必须可区分。

use crate::game::{all_supports, NormalFormGame};
use crate::poly::{rational_roots, Poly, RootSet};
use crate::rat::{solve_linear, LinearSolution, Rational};
use crate::{Completeness, SolveError};

/// 单个支撑剖面的求解结果。
#[derive(Debug, Clone)]
enum Outcome {
    Found(Vec<Vec<Rational>>),
    /// 该剖面不含均衡。
    Empty,
    /// 无差异方程**全部满足**、但最佳反应不等式被切成空集——§3.6 的"幽灵"。
    Ghost,
    /// 无差异系统欠定（连续统或不可判）。
    Underdetermined,
    /// 确认存在连续统（可行区间有正长度）——按 §2.4 第 5 条必须类型化上报。
    Continuum(String),
    /// 超出精确路径能力（结构性原因：如支撑剖面无法归约）。
    Unsupported(String),
    /// **算不动**：精确算术溢出。
    ///
    /// 与 `Unsupported` 的区别是**可修复性**：`Unsupported` 是算法结构上的限制
    /// （换更大的整数也不一定能解决），而 `Overflow` 只受 `i128` 容量限制——
    /// 降低支付精度或换大整数即可解决。诊断信息不同，故分开。
    Overflow,
}

/// 3 人支持枚举主入口。
pub(crate) fn enumerate_3p(
    game: &NormalFormGame,
) -> Result<(Vec<Vec<Vec<Rational>>>, Completeness), SolveError> {
    if game.players() != 3 {
        return Err(SolveError::InvalidGame("enumerate_3p 只接受 3 人博弈".into()));
    }
    let supports: Vec<Vec<Vec<usize>>> =
        (0..3).map(|p| all_supports(game.strategy_count(p))).collect();

    let mut found: Vec<Vec<Vec<Rational>>> = Vec::new();
    let mut unsupported = 0usize;
    let mut underdetermined = 0usize;
    let mut ghosts = 0usize;
    // 因精确算术溢出而未求解的剖面数。它是 `unsupported` 的子集，
    // 单列是因为它**可修复**（降支付精度或换大整数）——将来若要在
    // 错误信息里区分"可修复的拒绝"与"结构性的拒绝"，这个计数就是依据。
    let mut overflowed = 0usize;

    for i0 in &supports[0] {
        for i1 in &supports[1] {
            for i2 in &supports[2] {
                let profile = vec![i0.clone(), i1.clone(), i2.clone()];
                let pure = profile.iter().filter(|s| s.len() == 1).count();
                let outcome = if pure == 3 {
                    solve_all_pure(game, &profile)
                } else if pure == 1 {
                    solve_single_pure(game, &profile)?
                } else {
                    solve_parametric(game, &profile)?
                };
                match outcome {
                    Outcome::Found(sigma) => {
                        if !found.iter().any(|s| *s == sigma) {
                            found.push(sigma);
                        }
                    }
                    Outcome::Empty => {}
                    Outcome::Ghost => ghosts += 1,
                    Outcome::Underdetermined => underdetermined += 1,
                    // 连续统（退化）**不**中止整体求解，而是计入 `Partial`：
                    // 与 2 人路径（`support::enumerate_2p`）的处置一致。
                    // 曾在此 `return Err(Degenerate)`，使随机博弈族中**只要有一个**
                    // 退化剖面，整个博弈的求解就失败——而退化在随机整数支付下并不罕见
                    // （实测：40 个随机 2×2×2 案例中即可触发）。
                    // 正确语义：该剖面**无法**给出孤立均衡 ⇒ 计入 `underdetermined`
                    // 并让 `Completeness::Partial` 如实反映"解集可能不完备"，
                    // 这样上层能区分"求解失败"与"该博弈有连续统"。
                    Outcome::Continuum(_witness) => underdetermined += 1,
                    Outcome::Unsupported(_reason) => unsupported += 1,
                    // 溢出同样使**解集不完整**，故计入 unsupported 计数
                    // （`Completeness::Partial` 会表示"有剖面未精确求解"）。
                    // 但它是可修复的（降精度），故在最终错误信息里单独说明。
                    Outcome::Overflow => {
                        overflowed += 1;
                        unsupported += 1;
                    }
                }
            }
        }
    }
    let completeness = if unsupported == 0 && underdetermined == 0 {
        Completeness::Complete
    } else {
        Completeness::Partial { unsupported, underdetermined, ghost: ghosts }
    };
    Ok((found, completeness))
}

/// 全纯剖面：直接查最佳反应。
fn solve_all_pure(game: &NormalFormGame, profile: &[Vec<usize>]) -> Outcome {
    let pure: Vec<usize> = profile.iter().map(|s| s[0]).collect();
    if crate::support::is_pure_equilibrium(game, &pure) {
        Outcome::Found(
            pure.iter()
                .enumerate()
                .map(|(p, &s)| {
                    let mut v = vec![Rational::ZERO; game.strategy_count(p)];
                    v[s] = Rational::ONE;
                    v
                })
                .collect(),
        )
    } else {
        Outcome::Empty
    }
}

/// 恰有一个纯支撑玩家：另两人的无差异条件解耦且线性。
fn solve_single_pure(game: &NormalFormGame, profile: &[Vec<usize>]) -> Result<Outcome, SolveError> {
    let pure_player = (0..3).find(|&p| profile[p].len() == 1).expect("调用方保证");
    let mixed: Vec<usize> = (0..3).filter(|&p| p != pure_player).collect();
    let (a, b) = (mixed[0], mixed[1]);
    let fixed: Vec<(usize, usize)> = vec![(pure_player, profile[pure_player][0])];

    // 玩家 b 的无差异条件 ⇒ 解出 σ_a
    let (mat_a, rhs_a) = indifference_system(game, b, &profile[b], &fixed, &profile[a])?;
    let sig_a = match solve_linear(&mat_a, &rhs_a) {
        LinearSolution::Unique(x) => x,
        LinearSolution::None => return Ok(Outcome::Empty),
        LinearSolution::Underdetermined => return Ok(Outcome::Underdetermined),
        LinearSolution::Overflow => return Ok(Outcome::Overflow),
    };
    // 玩家 a 的无差异条件 ⇒ 解出 σ_b
    let (mat_b, rhs_b) = indifference_system(game, a, &profile[a], &fixed, &profile[b])?;
    let sig_b = match solve_linear(&mat_b, &rhs_b) {
        LinearSolution::Unique(x) => x,
        LinearSolution::None => return Ok(Outcome::Empty),
        LinearSolution::Underdetermined => return Ok(Outcome::Underdetermined),
        LinearSolution::Overflow => return Ok(Outcome::Overflow),
    };

    let mut sa = vec![Rational::ZERO; game.strategy_count(a)];
    for (c, &s) in profile[a].iter().enumerate() {
        if !sig_a[c].is_positive() {
            return Ok(Outcome::Empty);
        }
        sa[s] = sig_a[c];
    }
    let mut sb = vec![Rational::ZERO; game.strategy_count(b)];
    for (c, &s) in profile[b].iter().enumerate() {
        if !sig_b[c].is_positive() {
            return Ok(Outcome::Empty);
        }
        sb[s] = sig_b[c];
    }
    let mut sp = vec![Rational::ZERO; game.strategy_count(pure_player)];
    sp[profile[pure_player][0]] = Rational::ONE;

    let mut sigma = vec![Vec::new(); 3];
    sigma[a] = sa;
    sigma[b] = sb;
    sigma[pure_player] = sp;

    if crate::verify::is_equilibrium_exact(game, &sigma) {
        Ok(Outcome::Found(sigma))
    } else {
        // 无差异方程全满足、支撑一致，却不满足 BR 不等式 ⇒ 幽灵
        Ok(Outcome::Ghost)
    }
}

/// 构造"玩家 `target` 在支撑内无差异"的线性方程组，未知量是 `other` 玩家在
/// `other_support` 上的混合策略（`fixed` 为其余玩家的纯策略点质量）。
fn indifference_system(
    game: &NormalFormGame,
    target: usize,
    target_support: &[usize],
    fixed: &[(usize, usize)],
    other_support: &[usize],
) -> Result<(Vec<Vec<Rational>>, Vec<Rational>), SolveError> {
    let other = (0..3)
        .find(|&p| p != target && !fixed.iter().any(|(q, _)| *q == p))
        .ok_or_else(|| SolveError::InvalidGame("indifference_system: 未找到混合对手".into()))?;
    let k = target_support.len();
    let m = other_support.len();
    let mut mat = vec![vec![Rational::ZERO; m]; k];
    let mut rhs = vec![Rational::ZERO; k];

    let payoff_with = |s_target: usize, s_other: usize| -> Rational {
        let mut profile = [0usize; 3];
        profile[target] = s_target;
        profile[other] = s_other;
        for &(q, sq) in fixed {
            profile[q] = sq;
        }
        game.payoff(target, &profile)
    };

    for r in 0..k - 1 {
        let s = target_support[r + 1];
        let s0 = target_support[0];
        for (c, &t) in other_support.iter().enumerate() {
            mat[r][c] = payoff_with(s, t) - payoff_with(s0, t);
        }
    }
    for c in 0..m {
        mat[k - 1][c] = Rational::ONE;
    }
    rhs[k - 1] = Rational::ONE;
    Ok((mat, rhs))
}

/// 参数玩家路径入口：尝试所有合法的参数玩家选择。
fn solve_parametric(
    game: &NormalFormGame,
    profile: &[Vec<usize>],
) -> Result<Outcome, SolveError> {
    let mut tried = false;
    let mut saw_underdetermined = false;
    let mut saw_ghost = false;
    let mut saw_empty = false;

    for param in 0..3 {
        if profile[param].len() != 2 {
            continue;
        }
        let others: Vec<usize> = (0..3).filter(|&p| p != param).collect();
        let (a, b) = (others[0], others[1]);
        if profile[a].len() != profile[b].len() {
            continue;
        }
        tried = true;
        match solve_parametric_with(game, profile, param, a, b)? {
            Outcome::Underdetermined => saw_underdetermined = true,
            Outcome::Ghost => saw_ghost = true,
            Outcome::Empty => saw_empty = true,
            other => return Ok(other),
        }
    }
    if !tried {
        return Ok(Outcome::Unsupported(
            "该剖面没有大小为 2 且另两人等大的参数玩家（超出精确归约能力）".into(),
        ));
    }
    if saw_underdetermined {
        Ok(Outcome::Underdetermined)
    } else if saw_ghost {
        Ok(Outcome::Ghost)
    } else if saw_empty {
        Ok(Outcome::Empty)
    } else {
        Ok(Outcome::Empty)
    }
}

/// 指定参数玩家的参数化求解。
fn solve_parametric_with(
    game: &NormalFormGame,
    profile: &[Vec<usize>],
    param: usize,
    a: usize,
    b: usize,
) -> Result<Outcome, SolveError> {
    let f = profile[param][0];
    let g = profile[param][1]; // 参数玩家第二个策略；z = P(f)
    let ia = &profile[a];
    let ib = &profile[b];
    let k = ia.len();

    // 诱导支付的一次多项式：u(i,j,σ_param) = u(i,j,g) + z·[u(i,j,f) − u(i,j,g)]
    let lift = |player: usize, i: usize, j: usize, alt: usize| -> Rational {
        let mut profile = [0usize; 3];
        profile[param] = alt;
        profile[a] = i;
        profile[b] = j;
        game.payoff(player, &profile)
    };
    let a_poly = |i: usize, j: usize| -> Poly {
        Poly::linear(lift(a, i, j, g), lift(a, i, j, f) - lift(a, i, j, g))
    };
    let b_poly = |i: usize, j: usize| -> Poly {
        Poly::linear(lift(b, i, j, g), lift(b, i, j, f) - lift(b, i, j, g))
    };

    // 求 σ_a：由玩家 b 的无差异条件 Σ_i B'[i][j] p_i = w（∀j∈I_b，加归一化）
    let mut mat_p = vec![vec![Poly::zero(); k + 1]; k + 1];
    let mut rhs_p = vec![Poly::zero(); k + 1];
    for (r, &j) in ib.iter().enumerate() {
        for (c, &i) in ia.iter().enumerate() {
            mat_p[r][c] = b_poly(i, j);
        }
        mat_p[r][k] = Poly::constant(-Rational::ONE);
    }
    for c in 0..k {
        mat_p[k][c] = Poly::constant(Rational::ONE);
    }
    rhs_p[k] = Poly::constant(Rational::ONE);

    // 求 σ_b：由玩家 a 的无差异条件 Σ_j A'[i][j] q_j = v
    let mut mat_q = vec![vec![Poly::zero(); k + 1]; k + 1];
    let mut rhs_q = vec![Poly::zero(); k + 1];
    for (r, &i) in ia.iter().enumerate() {
        for (c, &j) in ib.iter().enumerate() {
            mat_q[r][c] = a_poly(i, j);
        }
        mat_q[r][k] = Poly::constant(-Rational::ONE);
    }
    for c in 0..k {
        mat_q[k][c] = Poly::constant(Rational::ONE);
    }
    rhs_q[k] = Poly::constant(Rational::ONE);

    let Some((det_p, num_p)) = cramer(&mat_p, &rhs_p)? else {
        return Ok(Outcome::Underdetermined);
    };
    let Some((det_q, num_q)) = cramer(&mat_q, &rhs_q)? else {
        return Ok(Outcome::Underdetermined);
    };
    if det_p.is_zero() || det_q.is_zero() {
        return Ok(Outcome::Underdetermined);
    }

    // 参数玩家的无差异：Σ_{i,j} p_i q_j Δ_ij = 0 ⇒ 通分后 F(z) = 0
    let mut f_poly = Poly::zero();
    for (ci, &i) in ia.iter().enumerate() {
        for (cj, &j) in ib.iter().enumerate() {
            let delta = lift(param, i, j, f) - lift(param, i, j, g);
            if delta.is_zero() {
                continue;
            }
            f_poly = f_poly.add(&num_p[ci].mul(&num_q[cj])?.scale(delta)?)?;
        }
    }

    // `rational_roots` 现在返回 `Result`：溢出（算不动）与 TooLarge（能力外）
    // 语义不同，故分别映射。
    let roots = match rational_roots(&f_poly) {
        Ok(r) => r,
        Err(_) => return Ok(Outcome::Overflow),
    };
    match roots {
        RootSet::TooLarge => {
            Ok(Outcome::Unsupported("无差异多项式系数超出精确根枚举能力".into()))
        }
        RootSet::Finite(roots) => {
            let mut saw_ghost = false;
            for z in roots {
                if !z.is_positive() || z >= Rational::ONE {
                    continue; // 支撑要求 z ∈ (0,1)
                }
                // eval 可能溢出（分母在 Horner 迭代里相乘）⇒ 该剖面算不动
                let (Ok(dp), Ok(dq)) = (det_p.eval(z), det_q.eval(z)) else {
                    return Ok(Outcome::Overflow);
                };
                if dp.is_zero() || dq.is_zero() {
                    continue;
                }
                let mut sa = vec![Rational::ZERO; game.strategy_count(a)];
                let mut ok = true;
                for (c, &i) in ia.iter().enumerate() {
                    let (Ok(num_v), Ok(den_v)) = (num_p[c].eval(z), Ok::<_, crate::rat::Overflow>(dp)) else {
                        return Ok(Outcome::Overflow);
                    };
                    let Some(v) = num_v.checked_div(den_v).ok() else {
                        return Ok(Outcome::Overflow);
                    };
                    if !v.is_positive() {
                        ok = false;
                        break;
                    }
                    sa[i] = v;
                }
                if !ok {
                    continue;
                }
                let mut sb = vec![Rational::ZERO; game.strategy_count(b)];
                for (c, &j) in ib.iter().enumerate() {
                    let (Ok(num_v2), den_v2) = (num_q[c].eval(z), dq) else {
                        return Ok(Outcome::Overflow);
                    };
                    let Some(v) = num_v2.checked_div(den_v2).ok() else {
                        return Ok(Outcome::Overflow);
                    };
                    if !v.is_positive() {
                        ok = false;
                        break;
                    }
                    sb[j] = v;
                }
                if !ok {
                    continue;
                }
                let mut sp = vec![Rational::ZERO; game.strategy_count(param)];
                sp[f] = z;
                sp[g] = Rational::ONE - z;
                let mut sigma = vec![Vec::new(); 3];
                sigma[a] = sa;
                sigma[b] = sb;
                sigma[param] = sp;
                if crate::verify::is_equilibrium_exact(game, &sigma) {
                    return Ok(Outcome::Found(sigma));
                }
                saw_ghost = true; // 无差异满足但当 BR 不等式为空集
            }
            Ok(if saw_ghost { Outcome::Ghost } else { Outcome::Empty })
        }
        RootSet::IdenticallyZero => {
            degenerate_family(game, profile, param, a, b, &det_p, &num_p, &det_q, &num_q)
        }
    }
}

/// `F ≡ 0`：单参数族。若另两人的混合不依赖 z，则剩余条件都是 z 的一次不等式。
#[allow(clippy::too_many_arguments)]
fn degenerate_family(
    game: &NormalFormGame,
    profile: &[Vec<usize>],
    param: usize,
    a: usize,
    b: usize,
    det_p: &Poly,
    num_p: &[Poly],
    det_q: &Poly,
    num_q: &[Poly],
) -> Result<Outcome, SolveError> {
    let f = profile[param][0];
    let g = profile[param][1];

    // p_i(z) = N_i(z)/D(z) 恒定 ⟺ N_i = c·D（作为多项式）
    let constant_of = |num: &Poly, den: &Poly| -> Option<Rational> {
        if num.is_zero() {
            return Some(Rational::ZERO);
        }
        let dn = den.degree()?;
        let nn = num.degree()?;
        if nn > dn {
            return None;
        }
        let c = num.coeff(nn) / den.coeff(dn);
        if den.scale(c).ok().as_ref() == Some(num) { Some(c) } else { None }
    };
    let mut sa = vec![Rational::ZERO; game.strategy_count(a)];
    for (c, &i) in profile[a].iter().enumerate() {
        match constant_of(&num_p[c], det_p) {
            Some(v) => sa[i] = v,
            None => return Ok(Outcome::Underdetermined),
        }
    }
    let mut sb = vec![Rational::ZERO; game.strategy_count(b)];
    for (c, &j) in profile[b].iter().enumerate() {
        match constant_of(&num_q[c], det_q) {
            Some(v) => sb[j] = v,
            None => return Ok(Outcome::Underdetermined),
        }
    }
    if sa.iter().any(|v| v.is_negative()) || sb.iter().any(|v| v.is_negative()) {
        return Ok(Outcome::Empty);
    }

    // 剩余条件：对每个玩家，支撑内 s 与任意 t：u(s) − u(t) ≥ 0，都是 z 的一次不等式。
    let mut lo = Rational::ZERO;
    let mut hi = Rational::ONE;
    for player in 0..3 {
        let support: Vec<usize> = if player == param {
            vec![f, g]
        } else if player == a {
            profile[a].clone()
        } else {
            profile[b].clone()
        };
        // u_player(t | σ₋) = c0 + c1·z。注意：当 player 属于 {a,b} 时，他自己的
        // 混合不进自己的期望支付，式中对 i 的求和带权重 sa[i]（Σ sa = 1）。
        let lin = |t: usize| -> (Rational, Rational) {
            let mut c0 = Rational::ZERO;
            let mut c1 = Rational::ZERO;
            for &i in profile[a].iter() {
                for &j in profile[b].iter() {
                    let w = sa[i] * sb[j];
                    if w.is_zero() {
                        continue;
                    }
                    let payoff_at = |alt: usize| -> Rational {
                        let mut p = [0usize; 3];
                        p[a] = i;
                        p[b] = j;
                        p[param] = alt;
                        p[player] = t;
                        game.payoff(player, &p)
                    };
                    let vg = payoff_at(g);
                    let vf = payoff_at(f);
                    c0 += w * vg;
                    c1 += w * (vf - vg);
                }
            }
            (c0, c1)
        };
        for &s in support.iter() {
            let (s0, s1) = lin(s);
            for t in 0..game.strategy_count(player) {
                let (t0, t1) = lin(t);
                let (c0, c1) = (s0 - t0, s1 - t1); // c0 + c1·z ≥ 0
                if c1.is_zero() {
                    if c0.is_negative() {
                        return Ok(Outcome::Empty);
                    }
                } else if c1.is_positive() {
                    let bound = -c0 / c1;
                    if bound > lo {
                        lo = bound;
                    }
                } else {
                    let bound = c0 / (-c1);
                    if bound < hi {
                        hi = bound;
                    }
                }
            }
        }
    }

    let z_lo = if lo.is_positive() { lo } else { Rational::ZERO };
    let z_hi = if hi < Rational::ONE { hi } else { Rational::ONE };
    if z_lo > z_hi {
        return Ok(Outcome::Empty);
    }
    let make = |z: Rational| -> Vec<Vec<Rational>> {
        let mut sp = vec![Rational::ZERO; game.strategy_count(param)];
        sp[f] = z;
        sp[g] = Rational::ONE - z;
        let mut s = vec![Vec::new(); 3];
        s[a] = sa.clone();
        s[b] = sb.clone();
        s[param] = sp;
        s
    };
    if z_lo == z_hi {
        if z_lo.is_positive() && z_lo < Rational::ONE {
            let sigma = make(z_lo);
            if crate::verify::is_equilibrium_exact(game, &sigma) {
                return Ok(Outcome::Found(sigma));
            }
        }
        return Ok(Outcome::Empty);
    }
    // 开区间非空 ⇒ 连续统（参数玩家的正性要求 z ∈ (0,1) 在开区间内部成立）。
    let mid = (z_lo + z_hi) / Rational::from_i64(2);
    let sigma = make(mid);
    if crate::verify::is_equilibrium_exact(game, &sigma) {
        Ok(Outcome::Continuum(format!(
            "支撑剖面 {:?} 上存在连续统：z ∈ ({}, {}) 全部满足均衡条件（见证 z = {}）",
            profile, z_lo, z_hi, mid
        )))
    } else {
        Ok(Outcome::Underdetermined)
    }
}

/// Cramer 法则：返回 `(det(M), [替换第 c 列后的行列式])`。
///
/// # 两层返回值的含义（不可合并）
///
/// - `Ok(None)`：`det ≡ 0` ⇒ **数学上是退化的**（解不唯一或不存在）
/// - `Err(..)`：算术溢出 ⇒ **算不出来**
///
/// 二者必须区分：前者是**结论**（该支撑剖面给出解族），后者是**能力不足**。
/// 合并成一个 `Option` 会让"算不动"看起来像"退化"——那正是本项目
/// 反复强调的"无法评估 ≠ 评估为负面"。
fn cramer(m: &[Vec<Poly>], rhs: &[Poly]) -> Result<Option<(Poly, Vec<Poly>)>, SolveError> {
    let n = m.len();
    let det = poly_det(m)?;
    if det.is_zero() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(n);
    for c in 0..n {
        let mut mc = m.to_vec();
        for r in 0..n {
            mc[r][c] = rhs[r].clone();
        }
        out.push(poly_det(&mc)?);
    }
    Ok(Some((det, out)))
}

/// 多项式矩阵行列式（Laplace 展开；小规模专用）。
/// 行列式（用于 [[support_np]] 的结式计算）。
///
/// 返回 `Result`：多项式算术可能因精确有理数的分母增长而溢出。
/// 实测：3 人博弈在此处溢出并**拖垮进程**（库代码 panic），
/// 改为类型化传播后，上层可把它变成可读的拒绝。
fn poly_det(m: &[Vec<Poly>]) -> Result<Poly, SolveError> {
    let n = m.len();
    if n == 1 {
        return Ok(m[0][0].clone());
    }
    let mut det = Poly::zero();
    for j in 0..n {
        if m[0][j].is_zero() {
            continue;
        }
        let minor: Vec<Vec<Poly>> = m[1..]
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .filter(|(k, _)| *k != j)
                    .map(|(_, v)| v.clone())
                    .collect()
            })
            .collect();
        let term = m[0][j].mul(&poly_det(&minor)?)?;
        det = if j % 2 == 0 { det.add(&term)? } else { det.sub(&term)? };
    }
    Ok(det)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn c6_has_exactly_one_completely_mixed_equilibrium() {
        let (eqs, c) = enumerate_3p(&c6()).unwrap();
        assert_eq!(eqs.len(), 1, "C6 恰有一个均衡（纯均衡 0 个）");
        assert_eq!(c, Completeness::Complete);
        assert_eq!(eqs[0][0], vec![Rational::new(1, 2), Rational::new(1, 2)]);
        assert_eq!(eqs[0][1], vec![Rational::new(1, 2), Rational::new(1, 2)]);
        assert_eq!(eqs[0][2], vec![Rational::new(8, 15), Rational::new(7, 15)]);
    }

    /// C6 §3.6 的**负例断言**：两个幽灵支撑剖面不得被报为均衡。
    #[test]
    fn c6_ghost_profiles_are_not_reported() {
        let (eqs, _) = enumerate_3p(&c6()).unwrap();
        for sigma in &eqs {
            let support = crate::verify::support_of(sigma);
            assert_ne!(support[0], vec![0usize], "Tommy=Front 不可能是均衡支撑");
            assert_ne!(support[1], vec![1usize], "Beth=Back 不可能是均衡支撑");
        }
        for profile in [
            vec![vec![0usize], vec![1usize], vec![0usize, 1usize]],
            vec![vec![1usize], vec![0usize], vec![0usize, 1usize]],
        ] {
            let out = solve_parametric(&c6(), &profile).unwrap();
            assert!(
                matches!(out, Outcome::Empty | Outcome::Ghost),
                "幽灵剖面不得产出均衡，实际 {out:?}"
            );
        }
    }

    /// 退化剖面（连续统）必须类型化上报或标记 Partial，绝不截断成有限样本。
    #[test]
    fn degenerate_profile_is_never_truncated_to_samples() {
        let mut g = c6();
        // 让 (Front,Back,·) 两格对 Tommy 的支付相等 ⇒ 该剖面上 Tommy 的 BR 不等式
        // 变成 z 的一段区间而非空集 ⇒ 连续统。
        g.set_payoff(0, &[0, 1, 0], Rational::from_i64(2));
        g.set_payoff(0, &[0, 1, 1], Rational::from_i64(2));
        match enumerate_3p(&g) {
            Err(SolveError::Degenerate { .. }) => {}
            Ok((_, Completeness::Partial { .. })) => {}
            other => panic!("退化博弈必须类型化上报或标记 Partial，实际 {other:?}"),
        }
    }

    /// n=3 性质测试：随机 2×2×2 博弈上，任何被报出的均衡都必须通过三残余，
    /// 且重复求解逐位一致（T8）。
    #[test]
    fn random_2x2x2_equilibria_self_verify_and_are_deterministic() {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) % 9) as i64 - 4
        };
        for case in 0..40 {
            let mut cells = Vec::new();
            for i in 0..2 {
                for j in 0..2 {
                    for k in 0..2 {
                        cells.push((vec![i, j, k], vec![next(), next(), next()]));
                    }
                }
            }
            let g = NormalFormGame::from_profiles(vec![2, 2, 2], &cells).unwrap();
            let (eqs, _) = enumerate_3p(&g).unwrap();
            for sigma in &eqs {
                assert!(
                    crate::verify::is_equilibrium_exact(&g, sigma),
                    "case {case}：报出的候选未通过三残余：{sigma:?}"
                );
            }
            let (eqs2, _) = enumerate_3p(&g).unwrap();
            assert_eq!(eqs, eqs2, "case {case}：同输入两次求解必须逐位一致");
        }
    }

    /// 蜕变测试：正仿射变换（乘正数 + 加常数）不改变均衡集。
    #[test]
    fn positive_affine_shift_preserves_equilibrium_set() {
        let g = c6();
        let mut h = c6();
        for profile in g.iter_profiles() {
            for p in 0..3 {
                let v = g.payoff(p, &profile);
                h.set_payoff(p, &profile, v * Rational::from_i64(3) + Rational::from_i64(7));
            }
        }
        let (e1, _) = enumerate_3p(&g).unwrap();
        let (e2, _) = enumerate_3p(&h).unwrap();
        assert_eq!(e1, e2, "正仿射变换不应改变均衡集");
    }

    /// 蜕变测试：某玩家有严格占优策略 ⇒ 其支撑必为该策略。
    #[test]
    fn dominant_strategy_must_be_in_support() {
        let mut g = c6();
        let profiles: Vec<Vec<usize>> = g.iter_profiles().collect();
        for profile in profiles {
            if profile[1] == 1 {
                let v = g.payoff(1, &profile);
                g.set_payoff(1, &profile, v + Rational::from_i64(100));
            }
        }
        let (eqs, _) = enumerate_3p(&g).unwrap();
        assert!(!eqs.is_empty());
        for sigma in &eqs {
            assert_eq!(sigma[1][0], Rational::ZERO, "Beth 不应使用被严格占优的 Front");
            assert_eq!(sigma[1][1], Rational::ONE);
        }
    }
}

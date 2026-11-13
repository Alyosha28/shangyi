//! 解选择器：支付占优、风险占优、随机稳定（KMR/Young）。
//!
//! # 算法口径声明（**读测试前必读**）
//!
//! `09-solver-test-vectors.md` S2 备注明确要求："**实现方必须先声明算法口径，
//! 测试再断言方向**，否则这条断言在测一个未定义的函数"。本模块的口径如下：
//!
//! | 选择器 | 口径 | 判据出处 |
//! |---|---|---|
//! | `payoff_dom` | 逐对比较：p 支付占优 p′ ⟺ ∀i `u_i(p) ≥ u_i(p′)` 且至少一个严格 | Wikipedia "Risk dominance" 的 `A ≥ D, a ≥ d` 判据 |
//! | `risk_dom` | **逐对受限子博弈**：对每对纯候选 `(p, p′)`，在只含策略 `{p_i, p′_i}` 的受限博弈上算偏离损失之积，大者严格风险占优 | Harsanyi & Selten 1988 Lemma 5.4.4；**Young 1993 §8 的核验方式**："To verify risk dominance, one checks that strategy 3 risk dominates strategy 2 in the subgame restricted to strategies 2 and 3, and that strategy 3 risk dominates strategy 1 in the subgame restricted to strategies 1 and 3."（`09` S2 逐字引用） |
//! | `stochastic` | 单种群 k 个参与者、最佳反应 + 变异模型：状态 = 计数向量，**阻力** = 路径上变异的最少次数（0-1 最短路）；随**最小阻力生成树**（arborescence）取根 | Young 1993 §8 例（其阻力数字 `[(1/8)k]`、`[(3/8)k]` 由本实现在下文复现）；KMR 1993；Young 1993 Theorem 3（2×2：随机稳定 = 弱风险占优） |
//!
//! ## 两处必须显式声明的限制
//!
//! 1. **`risk_dom` 不是 H–S 的"13 矩阵 tracing/缩减"**。`09` S2 备注已提示两者
//!    在 3×3 上**不保证等价**。本实现取逐对受限子博弈口径，理由是：
//!    (a) 它是 S2 期望值（⇒ 策略 3）的**唯一**可核对依据——Young 原文就是用这个
//!    方法核验的；(b) tracing 需要完整的 H–S 缩减理论（含"13 矩阵逼近"），
//!    本工作项无法在不引原书页码的前提下给出可复核的实现。若日后改用 tracing，
//!    **必须**重跑 S2 并更新该断言的依据（S1/S2 的期望值不变，但实现路径变）。
//! 2. **`stochastic` 只对对称博弈定义**（单种群模型的前提）。对非对称博弈（如 C2 BoS）
//!    返回 `Selection::unsupported`，而不是硬套一个错误的两种群模型——这满足
//!    "不得静默降级"：调用方拿到的是明确的"不适用"，而非一个看似合理的错答案。

use crate::game::NormalFormGame;
use crate::rat::Rational;
use crate::{Equilibrium, EqKind};

/// 选择器类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectorKind {
    RiskDominant,
    PayoffDominant,
    StochasticStable,
}

/// 选择结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub selector: SelectorKind,
    /// 该选择器对该输入是否**适用**。false 时 `selected` 必为空——调用方必须区分
    /// "不适用"与"适用但无胜者"，不得把两者混为一谈。
    pub applicable: bool,
    /// 参与比较的候选下标（相对输入候选列表）。
    pub considered: Vec<usize>,
    /// 被选中的候选下标（并列时多于一个；不适用时为空）。
    pub selected: Vec<usize>,
    /// 是否存在并列/无严格胜负。
    pub tied: bool,
    /// 算法口径（可被测试直接断言，避免"测一个未定义的函数"）。
    pub method: &'static str,
    /// 人可读的细节（含逐对比较的数值），用于报告与复核。
    pub detail: String,
}

impl Selection {
    fn unsupported(selector: SelectorKind, method: &'static str, why: &str) -> Selection {
        Selection {
            selector,
            applicable: false,
            considered: vec![],
            selected: vec![],
            tied: false,
            method,
            detail: format!("不适用：{why}"),
        }
    }

    /// 恰好选中一个候选时返回其下标。
    pub fn single(&self) -> Option<usize> {
        if self.selected.len() == 1 { Some(self.selected[0]) } else { None }
    }

    /// 唯一且未并列地选出一个候选时返回其下标（最常用的断言形态）。
    pub fn unique_winner(&self) -> Option<usize> {
        if self.applicable && !self.tied { self.single() } else { None }
    }
}

/// 候选均衡的（纯）策略剖面。
fn pure_profile(e: &Equilibrium) -> Option<Vec<usize>> {
    if e.kind != EqKind::Pure {
        return None;
    }
    Some(e.support.iter().map(|s| s[0]).collect())
}

// ─────────────────────────── 支付占优（payoff dominance） ───────────────────────────

pub const PAYOFF_DOM_METHOD: &str =
    "逐对：p 支付占优 p′ ⟺ ∀i u_i(p) ≥ u_i(p′) 且至少一个严格（Wikipedia Risk dominance）";

/// 支付占优方向选择：返回**未被任何其他纯候选支付占优**的候选集合。
///
/// 说明：支付占优是**偏序**而非全序，多个候选可互不占优（C2 BoS 即如此）。
/// 当恰好一个候选 strict 占优所有其他候选时 `tied = false` 且 `selected` 唯一。
pub fn payoff_dominant(game: &NormalFormGame, candidates: &[Equilibrium]) -> Selection {
    let pure: Vec<(usize, Vec<usize>)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, e)| pure_profile(e).map(|p| (i, p)))
        .collect();
    if pure.is_empty() {
        return Selection::unsupported(
            SelectorKind::PayoffDominant,
            PAYOFF_DOM_METHOD,
            "候选集中没有纯策略均衡（支付占优只在纯候选之间定义）",
        );
    }
    let mut detail = String::new();
    let mut undominated: Vec<usize> = Vec::new();
    for &(i, ref pi) in &pure {
        let mut dominated_by_some = false;
        for &(j, ref pj) in &pure {
            if i == j {
                continue;
            }
            if payoff_dominates(game, pj, pi) {
                detail.push_str(&format!("{:?} 支付占优 {:?}；", pj, pi));
                dominated_by_some = true;
            }
        }
        if !dominated_by_some {
            undominated.push(i);
        }
    }
    Selection {
        selector: SelectorKind::PayoffDominant,
        applicable: true,
        considered: pure.iter().map(|(i, _)| *i).collect(),
        tied: undominated.len() > 1,
        selected: undominated,
        method: PAYOFF_DOM_METHOD,
        detail,
    }
}

/// `p` 是否支付占优 `p′`（∀i：`u_i(p) ≥ u_i(p′)`，且至少一个严格）。
pub fn payoff_dominates(game: &NormalFormGame, p: &[usize], q: &[usize]) -> bool {
    let mut strict = false;
    for i in 0..game.players() {
        let (up, uq) = (game.payoff(i, p), game.payoff(i, q));
        if up < uq {
            return false;
        }
        if up > uq {
            strict = true;
        }
    }
    strict
}

// ─────────────────────────── 风险占优（risk dominance） ───────────────────────────

pub const RISK_DOM_METHOD: &str = "逐对受限子博弈：偏离损失之积（Harsanyi & Selten 1988 Lemma 5.4.4 / Young 1993 §8 核验方式）";

/// 一对纯候选之间的风险占优比较（**受限子博弈口径**）。
///
/// 返回 `Some(Ordering)`：`Greater` = `p` 严格风险占优 `q`；`Less` = 反之；`Equal` = 并列。
/// `None` = 该对不可比较（两候选在某玩家上策略相同，或损失之积非正）。
pub fn risk_dominance_pair(
    game: &NormalFormGame,
    p: &[usize],
    q: &[usize],
) -> Option<std::cmp::Ordering> {
    let mut prod_p = Rational::ONE;
    let mut prod_q = Rational::ONE;
    for i in 0..game.players() {
        if p[i] == q[i] {
            return None; // 该对在玩家 i 上没有真正的"两个策略"子博弈
        }
        let mut dev = p.to_vec();
        dev[i] = q[i];
        let loss_p = game.payoff(i, p) - game.payoff(i, &dev);
        let mut dev2 = q.to_vec();
        dev2[i] = p[i];
        let loss_q = game.payoff(i, q) - game.payoff(i, &dev2);
        if !loss_p.is_positive() || !loss_q.is_positive() {
            return None; // 不是协调型的一对（至少一方在该对上是弱/严格占优），不可比较
        }
        prod_p = prod_p * loss_p;
        prod_q = prod_q * loss_q;
    }
    Some(prod_p.cmp(&prod_q))
}

/// 风险占优方向选择。
///
/// **口径**：逐对受限子博弈（见模块文档与 [`RISK_DOM_METHOD`]）。返回
/// **严格风险占优所有其他纯候选**的那一个；若不存在严格胜者（并列或循环），
/// 则返回"未被任何一个候选严格风险占优"的候选集合且 `tied = true`。
///
/// C2 BoS(1) 即并列情形：两侧偏离损失之积均为 6（`09` §3.2.5 `[自算代入]`），
/// 故必须报并列，不得单边选定。
pub fn risk_dominant(game: &NormalFormGame, candidates: &[Equilibrium]) -> Selection {
    let pure: Vec<(usize, Vec<usize>)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(i, e)| pure_profile(e).map(|p| (i, p)))
        .collect();
    if pure.is_empty() {
        return Selection::unsupported(
            SelectorKind::RiskDominant,
            RISK_DOM_METHOD,
            "候选集中没有纯策略均衡（风险占优只在纯候选之间定义）",
        );
    }
    let mut detail = String::new();
    let mut strict_winners: Vec<usize> = Vec::new();
    for &(i, ref pi) in &pure {
        let mut beats_all = true;
        for &(j, ref pj) in &pure {
            if i == j {
                continue;
            }
            match risk_dominance_pair(game, pi, pj) {
                Some(std::cmp::Ordering::Greater) => {
                    detail.push_str(&format!("风险占优：{:?} ≻ {:?}；", pi, pj));
                }
                _ => {
                    beats_all = false;
                    if let Some(cmp) = risk_dominance_pair(game, pi, pj) {
                        if cmp == std::cmp::Ordering::Equal {
                            detail.push_str(&format!("并列：{:?} ~ {:?}；", pi, pj));
                        }
                    }
                }
            }
        }
        if beats_all {
            strict_winners.push(i);
        }
    }
    if strict_winners.len() == 1 {
        return Selection {
            selector: SelectorKind::RiskDominant,
            applicable: true,
            considered: pure.iter().map(|(i, _)| *i).collect(),
            selected: strict_winners,
            tied: false,
            method: RISK_DOM_METHOD,
            detail,
        };
    }
    // 无严格胜者：返回未被严格风险占优的候选（并列）
    let mut undominated: Vec<usize> = vec![];
    for &(i, ref pi) in &pure {
        let mut lost = false;
        for &(j, ref pj) in &pure {
            if i != j && risk_dominance_pair(game, pj, pi) == Some(std::cmp::Ordering::Greater) {
                lost = true;
                break;
            }
        }
        if !lost {
            undominated.push(i);
        }
    }
    Selection {
        selector: SelectorKind::RiskDominant,
        applicable: true,
        considered: pure.iter().map(|(i, _)| *i).collect(),
        tied: true,
        selected: undominated,
        method: RISK_DOM_METHOD,
        detail,
    }
}

// ─────────────────────────── 随机稳定（KMR/Young） ───────────────────────────

pub const STOCHASTIC_METHOD: &str = "单种群最佳反应+变异模型的最小阻力生成树（Young 1993 §8 / KMR 1993）";

/// 默认种群规模。取 240 的理由：Young §8 例中的阻力按 `k` 成比例，
/// 且 `240` 同时是 `8`、`3`、`5`、`2`、`6` 的整数倍——使 C4 与 S2 的阻力
/// 恰好落在整数上（不引入取整歧义）。测试另断言结果对 `k ∈ {120, 240, 480}` 不变。
pub const DEFAULT_POPULATION: usize = 240;

/// 随机稳定选择（单种群对称博弈）。
///
/// 模型（Young 1993 §8 的可执行化）：
/// - 状态 = `k` 个参与者各自的纯策略计数向量；
/// - 每期随机选一名参与者按**当前经验分布**的最佳反应改策略（平局任选）；
/// - **变异** = 该参与者选了非最佳反应；阻力 = 路径上变异次数的最小值（0-1 最短路）；
/// - 候选 = 严格纯策略均衡（惯例 `h_i`）；取"最小阻力生成树"总阻力最小的根。
pub fn stochastically_stable(
    game: &NormalFormGame,
    candidates: &[Equilibrium],
    population: usize,
) -> Selection {
    if !game.is_symmetric() {
        return Selection::unsupported(
            SelectorKind::StochasticStable,
            STOCHASTIC_METHOD,
            "本实现是单种群模型，只对对称博弈定义（非对称博弈需两种群模型，未实现）",
        );
    }
    let s = game.strategy_count(0);
    let mut conventions: Vec<(usize, usize)> = Vec::new(); // (candidate idx, strategy)
    for (i, e) in candidates.iter().enumerate() {
        if e.kind != EqKind::Pure {
            continue;
        }
        let strategy = match e.support[0].first() {
            Some(&v) => v,
            None => continue,
        };
        // 严格：对自身是唯一最佳反应
        let mut strict = true;
        for t in 0..s {
            if t != strategy {
                let mut profile = vec![strategy; 2];
                profile[0] = t;
                if game.payoff(0, &profile) >= game.payoff(0, &[strategy, strategy]) {
                    strict = false;
                    break;
                }
            }
        }
        if strict {
            conventions.push((i, strategy));
        }
    }
    if conventions.is_empty() {
        return Selection::unsupported(
            SelectorKind::StochasticStable,
            STOCHASTIC_METHOD,
            "没有严格纯策略均衡（惯例）⇒ 随机稳定无从谈起",
        );
    }

    // 阻力矩阵
    let c = conventions.len();
    let mut weights = vec![vec![u64::MAX; c]; c];
    let mut detail = String::new();
    for a in 0..c {
        let dist = resistance_from(game, conventions[a].1, population);
        for b in 0..c {
            if a == b {
                continue;
            }
            let idx = conventions[b].1;
            weights[a][b] = dist[idx];
        }
        detail.push_str(&format!(
            "从惯例 h{} 出发的阻力：{:?}；",
            conventions[a].1 + 1,
            (0..c).map(|b| (b, weights[a][b])).collect::<Vec<_>>()
        ));
    }
    // 最小阻力生成树（arborescence）：对每个根，枚举 Prüfer 序列给出的全部生成树
    let mut totals: Vec<(usize, u64)> = (0..c)
        .map(|root| (root, min_arborescence(&weights, root)))
        .collect();
    totals.sort_by_key(|&(_, w)| w);
    let best = totals[0].1;
    let winners: Vec<usize> = totals
        .iter()
        .filter(|&&(_, w)| w == best)
        .map(|&(r, _)| conventions[r].0)
        .collect();
    detail.push_str(&format!("最小阻力生成树总阻力：{totals:?}（最小 = {best}）"));
    Selection {
        selector: SelectorKind::StochasticStable,
        applicable: true,
        considered: conventions.iter().map(|(i, _)| *i).collect(),
        tied: winners.len() > 1,
        selected: winners,
        method: STOCHASTIC_METHOD,
        detail,
    }
}

/// 从"全员玩 `start`"出发，到每个状态的最小变异次数（0-1 最短路 / 双端队列 BFS）。
///
/// 返回长度为 `s` 的向量：`out[t]` = 到达惯例 `h_t`（全员玩 `t`）的最小变异次数。
///
/// **用精确有理数**比较期望支付。这不是洁癖：Young §8 例中的阻力恰好在
/// `q = 3/8` 这类**弱**最佳反应边界上取到（该点上 `u₂ = u₃` 相等），
/// 浮点 `>= best` 的判定会在边界上随舍入翻转，从而把阻力从 `3k/8` 变成 `3k/8 + 1`
/// 或反之——而那正是 S2 用来区分"随机稳定"与"风险占优"的数字。
fn resistance_from(game: &NormalFormGame, start: usize, k: usize) -> Vec<u64> {
    let s = game.strategy_count(0);
    let states = compositions(k, s);
    let index: std::collections::HashMap<Vec<usize>, usize> = states
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, v)| (v, i))
        .collect();

    let mut dist = vec![u64::MAX; states.len()];
    let mut start_counts = vec![0usize; s];
    start_counts[start] = k;
    let start_idx = index[&start_counts];
    dist[start_idx] = 0;
    let mut queue = std::collections::VecDeque::new();
    queue.push_front(start_idx);
    while let Some(cur) = queue.pop_front() {
        let d = dist[cur];
        let counts = &states[cur];
        let total = Rational::from_usize(k);
        let sigma: Vec<Rational> = counts.iter().map(|&c| Rational::from_usize(c) / total).collect();
        // 对当前经验分布的各纯策略期望支付（对称博弈：用行玩家矩阵）
        let mut u = vec![Rational::ZERO; s];
        for (t, slot) in u.iter_mut().enumerate() {
            let mut acc = Rational::ZERO;
            for (r, p) in sigma.iter().enumerate() {
                if p.is_zero() {
                    continue;
                }
                acc += *p * game.payoff(0, &[t, r]);
            }
            *slot = acc;
        }
        let best = u.iter().copied().max().unwrap_or(Rational::ZERO);
        for from in 0..s {
            if counts[from] == 0 {
                continue;
            }
            for to in 0..s {
                if to == from {
                    continue;
                }
                let mut nxt = counts.clone();
                nxt[from] -= 1;
                nxt[to] += 1;
                let ni = index[&nxt];
                let cost = if u[to] >= best { 0u64 } else { 1 };
                let nd = d + cost;
                if nd < dist[ni] {
                    dist[ni] = nd;
                    if cost == 0 {
                        queue.push_front(ni);
                    } else {
                        queue.push_back(ni);
                    }
                }
            }
        }
    }
    let mut out = vec![0u64; s];
    for t in 0..s {
        let mut counts = vec![0usize; s];
        counts[t] = k;
        out[t] = dist[index[&counts]];
    }
    out
}

/// 全部 `k` 元非负整数组合（长度 `s`），按字典序生成。
fn compositions(k: usize, s: usize) -> Vec<Vec<usize>> {
    let mut out = Vec::new();
    let mut cur = vec![0usize; s];
    fn rec(pos: usize, rem: usize, s: usize, cur: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        if pos == s - 1 {
            cur[pos] = rem;
            out.push(cur.clone());
            return;
        }
        for v in 0..=rem {
            cur[pos] = v;
            rec(pos + 1, rem - v, s, cur, out);
        }
    }
    rec(0, k, s, &mut cur, &mut out);
    out
}

/// 把 `rem` 分给 `n` 个非负整数的方案数（组合数 C(rem+n−1, n−1)）。
fn compositions_count(rem: usize, n: usize) -> usize {
    if n == 0 {
        return if rem == 0 { 1 } else { 0 };
    }
    let mut res = 1usize;
    for i in 0..n - 1 {
        res = res * (rem + n - 1 - i) / (i + 1);
    }
    res
}

/// 公开给测试的阻力计算（用于 S2 的阻力数字断言）。
pub fn resistance_profile(game: &NormalFormGame, population: usize) -> Vec<Vec<u64>> {
    let s = game.strategy_count(0);
    (0..s).map(|start| resistance_from(game, start, population)).collect()
}

/// 最小阻力生成树（arborescence）：所有边指向 `root`。
///
/// 对候选数 ≤ 6 直接用 Prüfer 序列枚举全部生成树（Cayley：`c^(c−2)` 个），
/// 精确且无近似；更大规模时应改用 Chu–Liu/Edmonds（本 crate 不涉及该规模）。
fn min_arborescence(weights: &[Vec<u64>], root: usize) -> u64 {
    let c = weights.len();
    if c == 1 {
        return 0;
    }
    let mut best = u64::MAX;
    for tree in labeled_trees(c) {
        // 无向生成树 → 定向为"每条边指向根"（BFS 距离小的一端是父节点）
        let dist = tree_dist(&tree, root);
        let mut total = 0u64;
        let mut ok = true;
        for &(u, v) in &tree {
            let (child, parent) = if dist[u] > dist[v] { (u, v) } else { (v, u) };
            let w = weights[child][parent];
            if w == u64::MAX {
                ok = false;
                break;
            }
            total = total.saturating_add(w);
        }
        if ok && total < best {
            best = total;
        }
    }
    best
}

fn tree_dist(tree: &[(usize, usize)], root: usize) -> Vec<usize> {
    let n = tree.len() + 1;
    let mut adj = vec![vec![]; n];
    for &(a, b) in tree {
        adj[a].push(b);
        adj[b].push(a);
    }
    let mut dist = vec![usize::MAX; n];
    dist[root] = 0;
    let mut q = std::collections::VecDeque::from([root]);
    while let Some(x) = q.pop_front() {
        for &y in &adj[x] {
            if dist[y] == usize::MAX {
                dist[y] = dist[x] + 1;
                q.push_back(y);
            }
        }
    }
    dist
}

/// 用 Prüfer 序列枚举 `n` 个顶点的全部带标号树（返回无向边表）。
fn labeled_trees(n: usize) -> Vec<Vec<(usize, usize)>> {
    if n == 2 {
        return vec![vec![(0, 1)]];
    }
    let len = n - 2;
    let total = n.pow(len as u32);
    let mut out = Vec::with_capacity(total);
    for code in 0..total {
        let mut seq = vec![0usize; len];
        let mut x = code;
        for slot in seq.iter_mut() {
            *slot = x % n;
            x /= n;
        }
        out.push(prufer_to_tree(&seq, n));
    }
    out
}

fn prufer_to_tree(seq: &[usize], n: usize) -> Vec<(usize, usize)> {
    let mut degree = vec![1usize; n];
    for &x in seq {
        degree[x] += 1;
    }
    let mut edges = Vec::with_capacity(n - 1);
    for &x in seq {
        let leaf = (0..n).find(|&i| degree[i] == 1).expect("Prüfer：必有叶子");
        edges.push((leaf, x));
        degree[leaf] -= 1;
        degree[x] -= 1;
    }
    let rest: Vec<usize> = (0..n).filter(|&i| degree[i] == 1).collect();
    assert_eq!(rest.len(), 2);
    edges.push((rest[0], rest[1]));
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tolerance;

    fn equilibria(g: &NormalFormGame) -> Vec<Equilibrium> {
        crate::solve_support_enumeration(g).unwrap().equilibria
    }

    /// C4 / S1：stag hunt（对称）
    fn stag_hunt() -> NormalFormGame {
        NormalFormGame::from_bimatrix(&[vec![5, 0], vec![4, 2]], &[vec![5, 4], vec![0, 2]]).unwrap()
    }

    /// C2：BoS(1)（非对称）
    fn bos1() -> NormalFormGame {
        NormalFormGame::from_bimatrix(&[vec![3, 0], vec![0, 2]], &[vec![2, 0], vec![0, 3]]).unwrap()
    }

    #[test]
    fn payoff_dominance_direction_stag_hunt() {
        let g = stag_hunt();
        let eqs = equilibria(&g);
        let sel = payoff_dominant(&g, &eqs);
        let winner = sel.single().expect("应恰有一个支付占优候选");
        assert_eq!(eqs[winner].support, vec![vec![0], vec![0]], "(Hunt,Hunt) 支付占优");
    }

    #[test]
    fn risk_dominance_direction_stag_hunt() {
        let g = stag_hunt();
        let eqs = equilibria(&g);
        let sel = risk_dominant(&g, &eqs);
        let winner = sel.single().expect("应恰有一个风险占优候选");
        assert_eq!(eqs[winner].support, vec![vec![1], vec![1]], "(Gather,Gather) 风险占优");
        assert!(!sel.tied);
    }

    /// C2 BoS(1)：偏离损失之积两侧均为 6 ⇒ **必须**报并列，不得单边选定。
    #[test]
    fn bos_risk_dominance_is_a_tie() {
        let g = bos1();
        let eqs = equilibria(&g);
        let sel = risk_dominant(&g, &eqs);
        assert!(sel.tied, "BoS 风险占优必须并列：{sel:?}");
        assert!(sel.selected.len() >= 2, "并列时应返回两者（等权）：{sel:?}");
    }

    #[test]
    fn stochastic_agrees_with_risk_dominance_on_2x2() {
        let g = stag_hunt();
        let eqs = equilibria(&g);
        let sel = stochastically_stable(&g, &eqs, DEFAULT_POPULATION);
        let winner = sel.single().expect("应恰有一个随机稳定惯例");
        assert_eq!(eqs[winner].support, vec![vec![1], vec![1]], "2×2 随机稳定 = 风险占优");
    }

    #[test]
    fn risk_dominance_pair_values() {
        let g = stag_hunt();
        // (Gather,Gather) 风险占优 (Hunt,Hunt)：4 > 1
        assert_eq!(
            risk_dominance_pair(&g, &[1, 1], &[0, 0]),
            Some(std::cmp::Ordering::Greater)
        );
        // BoS：并列
        let b = bos1();
        assert_eq!(risk_dominance_pair(&b, &[0, 0], &[1, 1]), Some(std::cmp::Ordering::Equal));
    }

    #[test]
    fn composition_ranking_matches_generation_order() {
        for s in 2..=4usize {
            for k in 1..=5usize {
                let states = compositions(k, s);
                assert_eq!(states.len(), compositions_count(k, s));
                // 每个状态的和 = k
                for st in &states {
                    assert_eq!(st.iter().sum::<usize>(), k);
                }
                // 生成顺序严格递增（逐位比较）
                for w in states.windows(2) {
                    assert!(w[0] < w[1], "组合必须按字典序生成");
                }
            }
        }
    }

    #[test]
    fn arborescence_matches_brute_force_on_three_nodes() {
        let w = vec![
            vec![u64::MAX, 3, 14],
            vec![u64::MAX, u64::MAX, u64::MAX],
            vec![u64::MAX, 9, u64::MAX],
        ];
        // 根 1（0-based 索引 1 = h2）：树 {0→1, 2→1} = 3 + 9 = 12；
        // 另两种树：{0→1, 2→0} = 3 + 14 = 17；{0→2, 2→1} = 14 + 9 = 23
        assert_eq!(min_arborescence(&w, 1), 12);
    }

    #[test]
    fn prufer_generates_expected_tree_count() {
        assert_eq!(labeled_trees(3).len(), 3);
        assert_eq!(labeled_trees(4).len(), 16);
        assert_eq!(labeled_trees(5).len(), 125);
    }

    #[test]
    fn stochastic_is_unsupported_on_asymmetric_games() {
        let g = bos1();
        let eqs = equilibria(&g);
        let sel = stochastically_stable(&g, &eqs, DEFAULT_POPULATION);
        assert!(!sel.applicable, "非对称博弈上单种群模型不适用");
        assert!(sel.selected.is_empty());
        assert!(sel.detail.contains("不适用"), "必须明确报'不适用'而不是给出错答案");
        let _ = Tolerance::GOLDEN;
    }

    /// C4 的 payoff/risk 选择方向必须**相反**（这是门②的判据强度要求）。
    #[test]
    fn payoff_and_risk_dominance_point_opposite_ways() {
        let g = stag_hunt();
        let eqs = equilibria(&g);
        let pd = payoff_dominant(&g, &eqs).unique_winner().unwrap();
        let rd = risk_dominant(&g, &eqs).unique_winner().unwrap();
        assert_ne!(pd, rd, "(Hunt,Hunt) 支付占优而 (Gather,Gather) 风险占优，方向必须相反");
    }
}

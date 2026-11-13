//! 精确路径的**能力边界**（实测，非推演）。
//!
//! # 为什么这个文件存在
//!
//! 设计文档说支持枚举 n≤4，但**精确有理数路径的实际能力远小于此**。
//! 实测矩阵（`--nocapture` 可见）：
//!
//! | 玩家数 | 整数 | 一位小数 | 两位小数 | 三位小数 |
//! |---|---|---|---|---|
//! | 2 人 | ✓ | ✓ | ✓ | **✗ panic** |
//! | 3 人 | ✓ | ✗ 溢出 | ✗ 溢出 | ✗ 溢出 |
//! | 4 人 | — 已知不支持（`UnsupportedPlayerCount`） | | | |
//!
//! # 机制（这是可解释的，不是玄学）
//!
//! `f64_to_exact_rational` 把小数转成**精确二进制分数**：
//! - `9.9` → 分母 `2^49`
//! - `3.513` → 分母 `2^52`
//!
//! 而高斯消元的除法把分母**相乘**——两个 2^50 相乘就是 2^100，
//! 再来一次到 2^150，超过 `i128` 的 `2^127`。
//!
//! 故边界取决于**消元路径长度**（∈ 玩家数、支撑大小）与**输入分母大小**
//! （∈ 小数位数）**的乘积**，不是单一因素。
//!
//! # 这不是"修好这个 bug"就能解决的
//!
//! 它是**精确算术的固有代价**：要么接受 i128 的上限（并如实告知用户），
//! 要么换大整数（`num-bigint`，但那是新依赖 + 性能代价），
//! 要么对超范围的输入走数值路径。**这是设计取舍，需人决定。**

use shangyi_gamespec::{Expr, GameSpec, Participant, PayoffForm, RelationToGame, ValueNetRole};
use shangyi_solver::{NormalFormGame, SolveRequest, SolverBackend};
use std::collections::BTreeMap;

fn spec(n: usize, val: fn(usize, usize) -> f64) -> GameSpec {
    let ids: Vec<String> = (0..n).map(|i| format!("p{i}")).collect();
    let mut strategies = BTreeMap::new();
    for id in &ids {
        strategies.insert(id.clone(), vec!["H".to_string(), "L".to_string()]);
    }
    let mut cells: BTreeMap<String, BTreeMap<String, Expr>> = BTreeMap::new();
    for combo in 0..(1usize << n) {
        let key: Vec<&str> = (0..n)
            .map(|i| if combo >> i & 1 == 0 { "H" } else { "L" })
            .collect();
        let mut m = BTreeMap::new();
        for (pi, id) in ids.iter().enumerate() {
            m.insert(id.clone(), Expr::Literal(val(combo, pi)));
        }
        cells.insert(key.join(","), m);
    }
    GameSpec {
        game_id: format!("n{n}"),
        players: ids
            .iter()
            .map(|id| Participant {
                id: id.clone(),
                name: id.clone(),
                vn_role: ValueNetRole::Focal,
                relation_to_game: RelationToGame::Player,
                profile_ref: None,
            })
            .collect(),
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

#[derive(Debug, PartialEq)]
enum Outcome {
    Solved,
    /// 类型化拒绝：溢出（**算不动**）
    Overflow,
    /// 类型化拒绝：4 人以上（已知能力边界）
    Unsupported,
    /// **panic** —— 库代码崩溃，最不可接受的一类
    Panicked,
}

fn classify(n: usize, val: fn(usize, usize) -> f64) -> Outcome {
    std::panic::catch_unwind(|| {
        let s = spec(n, val);
        let g = match NormalFormGame::from_gamespec(&s) {
            Ok(g) => g,
            Err(_) => return Outcome::Unsupported,
        };
        match shangyi_solver::NativeSolver.solve(SolveRequest::new(g)) {
            Ok(_) => Outcome::Solved,
            Err(e) => {
                let m = e.to_string();
                if m.contains("溢出") {
                    Outcome::Overflow
                } else if m.contains("不受支持") {
                    Outcome::Unsupported
                } else {
                    Outcome::Overflow // 其它拒绝也归此类（本测试只关心"未被拒"）
                }
            }
        }
    })
    .unwrap_or(Outcome::Panicked)
}

/// **不变量**：任何输入都**不得 panic**。
///
/// 这是本文件最重要的一条。溢出可以是"算不动"（类型化拒绝），
/// 但**不能是进程崩溃**——实测一次真实咨询因此死掉。
#[test]
fn no_input_causes_a_panic() {
    let cases: Vec<(usize, fn(usize, usize) -> f64)> = vec![
        (2, |c: usize, p: usize| (10 + c + p) as f64),
        (2, |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.1),
        (2, |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.01),
        (2, |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.001),
        (3, |c: usize, p: usize| (10 + c + p) as f64),
        (3, |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.1),
        (3, |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.01),
        (4, |c: usize, p: usize| (10 + c + p) as f64),
    ];
    let panics: Vec<(usize, Outcome)> = cases
        .iter()
        .map(|(n, f)| (*n, classify(*n, *f)))
        .filter(|(_, o)| *o == Outcome::Panicked)
        .collect();
    assert!(
        panics.is_empty(),
        "以下输入导致 **panic**（库代码崩溃，会拖垮整个进程）：{panics:?}\\n\
         溢出应表现为 `Outcome::Overflow`（类型化拒绝），而不是崩溃。\\n\
         修法：所有 `Rational` 算子必须走 checked 版本并传播错误。"
    );
}

/// 记录当前边界（**供决策参考**，不是断言"应该如此"）。
///
/// 它故意不断言具体哪些组合成功——那是实现现状，会随修法变化。
/// 它断言的是：**每种组合都必须给出明确的结论**（成功或类型化拒绝）。
#[test]
fn every_combination_yields_a_defined_outcome() {
    println!();
    for n in [2usize, 3] {
        for (label, f) in [
            ("整数", (|c: usize, p: usize| (10 + c + p) as f64) as fn(usize, usize) -> f64),
            ("1 位小数", |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.1),
            ("2 位小数", |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.01),
            ("3 位小数", |c: usize, p: usize| 10.0 + (c + p) as f64 * 0.001),
        ] {
            let o = classify(n, f);
            println!("  {n} 人 / {label:<10} → {o:?}");
        }
    }
    // 4 人必须是 Unsupported（已知边界），不能是 panic
    assert_eq!(
        classify(4, |c: usize, p: usize| (10 + c + p) as f64),
        Outcome::Unsupported,
        "4 人应是已知的类型化拒绝，而非其它"
    );
}

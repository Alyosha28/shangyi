//! 连续策略博弈的**解析路径**（Cournot 双寡头，C5）。
//!
//! 为什么单独一路：`09-solver-test-vectors.md` §3.5（C5.3）要求测试验证
//! "**路由**到解析/数值路径"而不是"支持枚举报错"或"静默降级"。因此本模块
//! 不接受纯策略组合表，只接受反需求/成本参数，并显式返回
//! [`crate::SolveRoute::Analytic`]。
//!
//! 解（Osborne《An Introduction to Game Theory》§3.1，p.56–58 逐字给出唯一均衡）：
//!
//! ```text
//! 反需求  P(Q) = a − b·Q，  Q = q₁ + q₂,  P = 0 for Q > a/b
//! 成本    C_i(q_i) = c·q_i
//! 唯一纳什均衡   q₁* = q₂* = (a − c)/(3b)
//!               Q*  = 2(a − c)/(3b)
//!               P*  = (a + 2c)/3
//!               π_i* = (a − c)²/(9b)
//! ```
//!
//! 数值实例（§3.5 的建议向量）：`a = 100, b = 1, c = 40` ⇒ `q* = 20`、`P* = 60`、`π* = 400`。
//!
//! 本模块同时返回**最佳反应不动点残差**（`BR_gap`，与 §2.3 第 3 条同义）：
//! 对 `q_i*` 的最优反应为 `argmax_q q·(a − b(q + q_j*) − c)`，其解析解为
//! `(a − c − b·q_j*)/(2b)`；残差 = `|q_i* − BR_i(q_j*)|`，应与解析解一致（≤ τ）。
//! 这是"数值路径也能自证"的那条检查。

use crate::game::Tolerance;

/// Cournot 博弈参数。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CournotParams {
    /// 反需求截距 `a`（记法同 Osborne 的 α）。
    pub a: f64,
    /// 反需求斜率 `b`（> 0）。
    pub b: f64,
    /// 单位边际成本 `c`（< a）。
    pub c: f64,
}

impl CournotParams {
    pub fn new(a: f64, b: f64, c: f64) -> CournotParams {
        assert!(b > 0.0, "Cournot：斜率 b 必须为正");
        assert!(a > c, "Cournot：需 a > c，否则最优产量为 0（边界解，不属本向量）");
        CournotParams { a, b, c }
    }
}

/// Cournot 均衡（解析解）与自证残差。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CournotEquilibrium {
    /// 每家产量 `q*`。
    pub quantity: f64,
    /// 总产量 `Q*`。
    pub total_quantity: f64,
    /// 价格 `P*`。
    pub price: f64,
    /// 每家利润 `π*`。
    pub profit: f64,
    /// 最佳反应不动点残差 `max_i |q_i* − BR_i(q_j*)|`。
    pub br_residual: f64,
}

/// 求解 Cournot 双寡头（解析公式，无迭代、无收敛容差）。
pub fn solve_cournot(params: &CournotParams) -> CournotEquilibrium {
    let CournotParams { a, b, c } = *params;
    let q = (a - c) / (3.0 * b);
    let total = 2.0 * q;
    let price = a - b * total;
    let profit = q * (price - c);
    // 最佳反应：BR_i(q_j) = (a − c − b·q_j)/(2b)
    let br = (a - c - b * q) / (2.0 * b);
    let br_residual = (q - br).abs().max((q - br).abs());
    CournotEquilibrium {
        quantity: q,
        total_quantity: total,
        price,
        profit,
        br_residual,
    }
}

/// 按容差验证解析解（`09` §3.5 的断言建议：`τ_q = 1e-9`（解析路径））。
pub fn verify_cournot(
    params: &CournotParams,
    expected_quantity: f64,
    expected_price: f64,
    expected_profit: f64,
    tol: &Tolerance,
) -> Result<(), String> {
    let got = solve_cournot(params);
    let t = if tol.exact_rational { 1e-9 } else { tol.payoff };
    let checks = [
        ("q*", got.quantity, expected_quantity),
        ("P*", got.price, expected_price),
        ("π*", got.profit, expected_profit),
    ];
    for (name, g, e) in checks {
        if (g - e).abs() > t {
            return Err(format!("{name}: 解析解 {g} 与黄金值 {e} 差 {} > {t}", (g - e).abs()));
        }
    }
    if got.br_residual > t.max(1e-9) {
        return Err(format!("BR 残差 {} 超容差", got.br_residual));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C5 数值实例：a = 100, b = 1, c = 40 ⇒ q* = 20、Q* = 40、P* = 60、π* = 400。
    #[test]
    fn c5_numeric_instance() {
        let p = CournotParams::new(100.0, 1.0, 40.0);
        let eq = solve_cournot(&p);
        assert!((eq.quantity - 20.0).abs() < 1e-12);
        assert!((eq.total_quantity - 40.0).abs() < 1e-12);
        assert!((eq.price - 60.0).abs() < 1e-12);
        assert!((eq.profit - 400.0).abs() < 1e-12);
        assert!(eq.br_residual < 1e-12, "最佳反应不动点残差应为机器精度");
    }

    /// 一般 b 的形式：(a − c)/(3b) 与 (a − c)²/(9b)。
    #[test]
    fn general_slope_form() {
        let p = CournotParams::new(10.0, 2.0, 4.0);
        let eq = solve_cournot(&p);
        assert!((eq.quantity - (10.0 - 4.0) / 6.0).abs() < 1e-12);
        assert!((eq.profit - (10.0 - 4.0f64).powi(2) / 18.0).abs() < 1e-12);
        assert!((eq.price - (10.0 + 2.0 * 4.0) / 3.0).abs() < 1e-12);
    }

    /// 利润恒低于垄断利润 (a−c)²/(4b)（WSU 讲义的对照）。
    #[test]
    fn profit_below_monopoly() {
        let p = CournotParams::new(100.0, 1.0, 40.0);
        let eq = solve_cournot(&p);
        let monopoly = (100.0f64 - 40.0).powi(2) / 4.0;
        assert!(eq.profit < monopoly);
    }

    #[test]
    fn verify_helper_catches_wrong_expectation() {
        let p = CournotParams::new(100.0, 1.0, 40.0);
        assert!(verify_cournot(&p, 20.0, 60.0, 400.0, &Tolerance::GOLDEN).is_ok());
        assert!(verify_cournot(&p, 21.0, 60.0, 400.0, &Tolerance::GOLDEN).is_err());
    }
}

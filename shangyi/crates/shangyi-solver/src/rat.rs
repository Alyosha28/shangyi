//! 精确有理数算术与精确线性代数。
//!
//! 为什么自研而不引第三方 crate：黄金用例（`09-solver-test-vectors.md` §2.2）把
//! **精确有理数路径定为默认路径**，理由是"浮点比较会把'求解器算错了但恰好落在容差内'
//! 误判为通过"。本模块用 `i128` 存储、每次运算后按 gcd 约分，并对每一次算术做溢出检查：
//! 溢出即 panic（带诊断信息）而不是静默取近似——对齐设计文档不变量②（"崩溃而非静默"）。
//!
//! 全部有序：`Ord` 用交叉相乘的精确比较（不做浮点近似）。

use std::cmp::Ordering;
use std::fmt;
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub};

/// 精确有理数。恒为**约分后**存储，且 `den > 0`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rational {
    num: i128,
    den: i128,
}

/// 算术溢出（精确路径的容量上限被突破）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overflow;

impl std::fmt::Display for Overflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("精确有理数算术溢出（i128）")
    }
}

impl std::error::Error for Overflow {}

impl Rational {
    pub const ZERO: Rational = Rational { num: 0, den: 1 };
    pub const ONE: Rational = Rational { num: 1, den: 1 };

    /// 构造并约分。`den == 0` 时 panic（编程错误，不是数据错误）。
    pub fn new(num: i128, den: i128) -> Rational {
        assert!(den != 0, "Rational::new: 分母为零");
        let (num, den) = if den < 0 { (-num, -den) } else { (num, den) };
        let g = gcd(num.unsigned_abs(), den.unsigned_abs()) as i128;
        let g = if g == 0 { 1 } else { g };
        Rational { num: num / g, den: den / g }
    }

    pub fn from_i64(v: i64) -> Rational {
        Rational { num: v as i128, den: 1 }
    }

    pub fn from_usize(v: usize) -> Rational {
        Rational { num: v as i128, den: 1 }
    }

    pub fn numer(&self) -> i128 {
        self.num
    }

    pub fn denom(&self) -> i128 {
        self.den
    }

    /// `(分子, 分母)` 的 i64 视图；超出 i64 时返回 `None`。
    pub fn as_i64_pair(&self) -> Option<(i64, i64)> {
        Some((i64::try_from(self.num).ok()?, i64::try_from(self.den).ok()?))
    }

    pub fn is_zero(&self) -> bool {
        self.num == 0
    }

    pub fn is_positive(&self) -> bool {
        self.num > 0
    }

    pub fn is_negative(&self) -> bool {
        self.num < 0
    }

    pub fn abs(&self) -> Rational {
        Rational { num: self.num.abs(), den: self.den }
    }

    pub fn signum(&self) -> i32 {
        self.num.signum() as i32
    }

    pub fn to_f64(&self) -> f64 {
        self.num as f64 / self.den as f64
    }

    pub fn checked_add(self, o: Rational) -> Result<Rational, Overflow> {
        let g = gcd(self.den.unsigned_abs(), o.den.unsigned_abs()) as i128;
        let (a, b) = (self.den / g, o.den / g);
        let num = self
            .num
            .checked_mul(b)
            .and_then(|v| o.num.checked_mul(a).and_then(|w| v.checked_add(w)))
            .ok_or(Overflow)?;
        let den = self.den.checked_mul(b).ok_or(Overflow)?;
        Ok(Rational::new(num, den))
    }

    pub fn checked_mul(self, o: Rational) -> Result<Rational, Overflow> {
        // 先约分再相乘，尽量抑制增长。
        let g1 = gcd(self.num.unsigned_abs(), o.den.unsigned_abs()) as i128;
        let g2 = gcd(o.num.unsigned_abs(), self.den.unsigned_abs()) as i128;
        let (n1, d2) = (self.num / g1.max(1), o.den / g1.max(1));
        let (n2, d1) = (o.num / g2.max(1), self.den / g2.max(1));
        let num = n1.checked_mul(n2).ok_or(Overflow)?;
        let den = d1.checked_mul(d2).ok_or(Overflow)?;
        Ok(Rational::new(num, den))
    }

    pub fn checked_sub(self, o: Rational) -> Result<Rational, Overflow> {
        self.checked_add(-o)
    }

    pub fn checked_div(self, o: Rational) -> Result<Rational, Overflow> {
        assert!(!o.is_zero(), "Rational 除法：除数为零");
        self.checked_mul(Rational { num: o.den, den: o.num })
    }

    /// 精确比较：两侧通分后比较分子（不经过浮点）。
    pub fn checked_cmp(self, o: Rational) -> Result<Ordering, Overflow> {
        let g = gcd(self.den.unsigned_abs(), o.den.unsigned_abs()) as i128;
        let (a, b) = (self.den / g, o.den / g);
        let lhs = self.num.checked_mul(b).ok_or(Overflow)?;
        let rhs = o.num.checked_mul(a).ok_or(Overflow)?;
        Ok(lhs.cmp(&rhs))
    }

    /// 精确等于 0 的判定（用于支撑外/支撑内判定）。
    pub fn is_exactly(self, v: i128) -> bool {
        self.num == v && self.den == 1
    }
}

fn gcd(mut a: u128, mut b: u128) -> u128 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn overflow_panic() -> ! {
    // 这是**库代码 panic**——它会拖垮整个进程（实测：一次真实咨询在 solve
    // 阶段因此崩溃，而 CLI 没有任何机会把它变成可读的错误）。
    //
    // ⚠️ 已知待修：应改为让调用方走 `checked_*` 并返回 `SolveError::Overflow`。
    // 保留 panic 的理由是 `Add`/`Mul` 的 `type Output` 签名不允许返回错误；
    // 改成 checked 需要改所有调用点的签名（是一次重构而非补丁）。
    // 此处先让消息**带上足够信息**以便定位（哪两个数）。
    panic!(
        "精确有理数算术溢出（i128）：输入规模超出精确路径容量；\
         请缩短支撑或改用浮点路径。（这说明博弈的支付值过大——\
         常见原因：把金额直接写进支付而未做单位归约）"
    );
}

impl Add for Rational {
    type Output = Rational;
    fn add(self, o: Rational) -> Rational {
        self.checked_add(o).unwrap_or_else(|_| overflow_panic())
    }
}

impl AddAssign for Rational {
    fn add_assign(&mut self, o: Rational) {
        *self = *self + o;
    }
}

impl Sub for Rational {
    type Output = Rational;
    fn sub(self, o: Rational) -> Rational {
        self.checked_sub(o).unwrap_or_else(|_| overflow_panic())
    }
}

impl Mul for Rational {
    type Output = Rational;
    fn mul(self, o: Rational) -> Rational {
        self.checked_mul(o).unwrap_or_else(|_| overflow_panic())
    }
}

impl Div for Rational {
    type Output = Rational;
    fn div(self, o: Rational) -> Rational {
        self.checked_div(o).unwrap_or_else(|_| overflow_panic())
    }
}

impl Neg for Rational {
    type Output = Rational;
    fn neg(self) -> Rational {
        Rational { num: -self.num, den: self.den }
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        self.checked_cmp(*other).unwrap_or_else(|_| overflow_panic())
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}", self.num)
        } else {
            write!(f, "{}/{}", self.num, self.den)
        }
    }
}

impl From<i64> for Rational {
    fn from(v: i64) -> Rational {
        Rational::from_i64(v)
    }
}

impl From<i32> for Rational {
    fn from(v: i32) -> Rational {
        Rational::from_i64(v as i64)
    }
}

// ───────────────────────────── 精确线性代数 ─────────────────────────────

/// 线性方程组 `M x = b` 的解况。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinearSolution {
    /// 唯一解。
    Unique(Vec<Rational>),
    /// 无解（不相容）。
    None,
    /// 解不唯一（秩亏，含连续统）——退化信号。
    Underdetermined,
    /// **算不动**：精确算术溢出。
    ///
    /// # 为什么必须是独立的第四态（不能并进任何一个）
    ///
    /// 前三态都是**数学结论**（解是什么）；这一态是**能力边界**（算不出来）。
    /// 合并会让"算不动"被读成"无解"或"退化"——一个错误的知识主张。
    /// 这正是本项目反复强调的「无法评估 ≠ 评估为负面」。
    ///
    /// 实测触发条件（`tests/exact_path_limits.rs`）：3 人博弈 + 6 位小数的
    /// 支付值（如 `0.828571`）时，消元的除法把分母放大到超过 `i128`。
    Overflow,
}

/// 精确高斯消元（带部分主元：选第一个非零主元）。
///
/// 返回值语义：`Underdetermined` 只表示"秩 < 未知数个数且相容"，
/// 调用方必须把它当作**退化信号**处理，不得当作零解。
pub fn solve_linear(mat: &[Vec<Rational>], rhs: &[Rational]) -> LinearSolution {
    let n = mat.len();
    assert_eq!(rhs.len(), n, "solve_linear: 行数与右端项长度不符");
    if n == 0 {
        return LinearSolution::Unique(Vec::new());
    }
    let m = mat[0].len();
    // 增广矩阵
    let mut a: Vec<Vec<Rational>> = mat
        .iter()
        .zip(rhs.iter())
        .map(|(row, b)| {
            let mut r = row.clone();
            r.push(*b);
            r
        })
        .collect();

    let mut pivot_row = 0usize;
    let mut pivot_cols: Vec<usize> = Vec::new();
    for col in 0..m {
        // 找主元
        let Some(pr) = (pivot_row..n).find(|&r| !a[r][col].is_zero()) else {
            continue;
        };
        a.swap(pivot_row, pr);
        // 消元
        let pv = a[pivot_row][col];
        for r in 0..n {
            if r != pivot_row && !a[r][col].is_zero() {
                // ⚠️ 这里是精确路径的**唯一溢出源**（消元必然做除法，
                // 而除法把两个分母相乘）。用 checked 并返回第四态，
                // 而不是让 panic 拖垮进程——实测一次真实咨询因此崩溃。
                let Some(factor) = a[r][col].checked_div(pv).ok() else {
                    return LinearSolution::Overflow;
                };
                for c in col..=m {
                    let Some(sub) = factor.checked_mul(a[pivot_row][c]).ok() else {
                        return LinearSolution::Overflow;
                    };
                    let Some(diff) = a[r][c].checked_sub(sub).ok() else {
                        return LinearSolution::Overflow;
                    };
                    a[r][c] = diff;
                }
            }
        }
        pivot_cols.push(col);
        pivot_row += 1;
        if pivot_row == n {
            break;
        }
    }

    // 相容性：是否存在 0 = 非零 的行
    for row in a.iter() {
        if (0..m).all(|c| row[c].is_zero()) && !row[m].is_zero() {
            return LinearSolution::None;
        }
    }
    if pivot_cols.len() < m {
        return LinearSolution::Underdetermined;
    }
    let mut x = vec![Rational::ZERO; m];
    for (r, &c) in pivot_cols.iter().enumerate() {
        x[c] = a[r][m] / a[r][c];
    }
    LinearSolution::Unique(x)
}

/// 方阵行列式（用于 Cramer 法则；仅小规模调用）。
pub fn determinant(mat: &[Vec<Rational>]) -> Rational {
    let n = mat.len();
    if n == 1 {
        return mat[0][0];
    }
    let mut det = Rational::ZERO;
    for j in 0..n {
        if mat[0][j].is_zero() {
            continue;
        }
        let minor: Vec<Vec<Rational>> = mat[1..]
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .filter(|(k, _)| *k != j)
                    .map(|(_, v)| *v)
                    .collect()
            })
            .collect();
        let term = mat[0][j] * determinant(&minor);
        det = if j % 2 == 0 { det + term } else { det - term };
    }
    det
}

/// 两个有理数间的最大绝对值差（L∞ 距离的标量版本）。
pub fn max_abs_diff(a: &[Rational], b: &[Rational]) -> Rational {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (*x - *y).abs())
        .max()
        .unwrap_or(Rational::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reduction_and_sign_convention() {
        assert_eq!(Rational::new(2, 4), Rational::new(1, 2));
        assert_eq!(Rational::new(-2, -4), Rational::new(1, 2));
        assert_eq!(Rational::new(2, -4), Rational::new(-1, 2));
        assert_eq!(Rational::new(0, -5), Rational::ZERO);
    }

    #[test]
    fn arithmetic_is_exact() {
        let a = Rational::new(1, 3);
        let b = Rational::new(1, 6);
        assert_eq!(a + b, Rational::new(1, 2));
        assert_eq!(a - b, Rational::new(1, 6));
        assert_eq!(a * b, Rational::new(1, 18));
        assert_eq!(a / b, Rational::new(2, 1));
        assert_eq!((a * Rational::from_i64(3)), Rational::ONE);
    }

    #[test]
    fn ordering_is_exact() {
        assert!(Rational::new(1, 3) > Rational::new(1, 4));
        assert!(Rational::new(-1, 3) < Rational::new(-1, 4));
        assert!(Rational::new(1, 3) < Rational::new(2, 3));
    }

    #[test]
    fn exact_decimal_constants() {
        // 容差常量按十进制精确表示（1e-9 = 1/10^9），不经过浮点。
        let tau = Rational::new(1, 1_000_000_000);
        assert_eq!(tau.to_f64(), 1e-9);
        assert!(Rational::new(1, 1_000_000_001) < tau);
    }

    #[test]
    fn linear_system_unique() {
        let m = vec![
            vec![Rational::from_i64(2), Rational::from_i64(1)],
            vec![Rational::from_i64(1), Rational::from_i64(-1)],
        ];
        let b = vec![Rational::from_i64(5), Rational::from_i64(1)];
        let LinearSolution::Unique(x) = solve_linear(&m, &b) else {
            panic!("应唯一解");
        };
        assert_eq!(x, vec![Rational::from_i64(2), Rational::from_i64(1)]);
    }

    #[test]
    fn linear_system_none_and_underdetermined() {
        let m = vec![
            vec![Rational::from_i64(1), Rational::from_i64(1)],
            vec![Rational::from_i64(2), Rational::from_i64(2)],
        ];
        let b = vec![Rational::from_i64(1), Rational::from_i64(3)];
        assert_eq!(solve_linear(&m, &b), LinearSolution::None);
        let b2 = vec![Rational::from_i64(1), Rational::from_i64(2)];
        assert_eq!(solve_linear(&m, &b2), LinearSolution::Underdetermined);
    }

    #[test]
    fn determinant_of_singular_is_zero() {
        let m = vec![
            vec![Rational::from_i64(1), Rational::from_i64(2)],
            vec![Rational::from_i64(2), Rational::from_i64(4)],
        ];
        assert!(determinant(&m).is_zero());
    }
}

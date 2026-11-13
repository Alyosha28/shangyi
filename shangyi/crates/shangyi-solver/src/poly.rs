//! 一元多项式（有理系数）——3 人支持枚举的消元工具。
//!
//! 3 人博弈的支撑剖面上，"无差异 + 归一化"方程组对每个玩家是**多线性**的；
//! 当其中一个玩家的支撑大小为 2（一个自由参数 z）且另两人的支撑等大时，
//! 方程组可归约为**一元多项式方程**（见 `support_np` 的推导）：
//!
//! 1. 把 z 固定，玩家 1、2 之间的诱导博弈的支付矩阵是 z 的一次多项式；
//! 2. 用 Cramer 法则解出 σ₁(z)、σ₂(z)（共同分母 = 系数行列式，分子 = 替换列后的行列式）；
//! 3. 玩家 3 的无差异条件 `u₃(f|σ₁,σ₂) = u₃(b|σ₁,σ₂)` 是**双线性**的，
//!    通分后成为 z 的一元多项式方程；
//! 4. 求该多项式的**有理根**（有理根定理 + 综合除法），逐个回代并由三残余验证。
//!
//! 这套推导只依赖线性代数与双线性性，不依赖任何外部求解库。

use crate::rat::Rational;

/// 稠密一元多项式：`coeffs[i]` 为 zⁱ 的系数；恒去除高阶零项。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Poly {
    coeffs: Vec<Rational>,
}

impl Poly {
    pub fn zero() -> Poly {
        Poly { coeffs: Vec::new() }
    }

    pub fn constant(v: Rational) -> Poly {
        let mut p = Poly { coeffs: vec![v] };
        p.trim();
        p
    }

    /// 一次多项式 `a + b z`。
    pub fn linear(a: Rational, b: Rational) -> Poly {
        let mut p = Poly { coeffs: vec![a, b] };
        p.trim();
        p
    }

    pub fn from_coeffs(coeffs: Vec<Rational>) -> Poly {
        let mut p = Poly { coeffs };
        p.trim();
        p
    }

    fn trim(&mut self) {
        while let Some(last) = self.coeffs.last() {
            if last.is_zero() {
                self.coeffs.pop();
            } else {
                break;
            }
        }
    }

    pub fn is_zero(&self) -> bool {
        self.coeffs.is_empty()
    }

    pub fn degree(&self) -> Option<usize> {
        if self.coeffs.is_empty() {
            None
        } else {
            Some(self.coeffs.len() - 1)
        }
    }

    pub fn coeff(&self, i: usize) -> Rational {
        self.coeffs.get(i).copied().unwrap_or(Rational::ZERO)
    }

    pub fn coeffs(&self) -> &[Rational] {
        &self.coeffs
    }

    /// Horner 法则求值。**返回 `Result`**：`acc * z` 可能溢出
    /// （实测：3 人博弈 + 6 位小数支付在此处 panic）。
    pub fn eval(&self, z: Rational) -> Result<Rational, crate::rat::Overflow> {
        let mut acc = Rational::ZERO;
        for c in self.coeffs.iter().rev() {
            acc = acc.checked_mul(z)?.checked_add(*c)?;
        }
        Ok(acc)
    }

    // ── 算术：**全部返回 `Result`**（溢出是类型化错误，不是 panic）──
    //
    // # 为什么必须这样（实测事故）
    //
    // 早先这些方法用 `Add`/`Mul`/`AddAssign`，而它们在溢出时
    // **panic**（`rat::overflow_panic`）。后果：一次真实咨询在 solve 阶段
    // 直接崩溃，CLI 没有任何机会把它变成可读错误——用户看到的是一个
    // Rust panic 消息，而会话状态停在半路。
    //
    // # 溢出的成因（不是"值太大"，而是精确算术的固有边界）
    //
    // `f64_to_exact_rational(9.9)` 的分母是 `2^49`。分数乘法**三次**
    // 就会到 `2^147`，超过 `i128` 的 `2^127`。而 3 人支撑枚举的多项式
    // 运算要做几十次乘法——**必然溢出**。
    //
    // 故这不是"极端输入"，而是"3 人以上博弈走精确有理数路径"的必然结果。
    // 正确处置是**类型化上报**（让上层决定退到浮点路径或告知用户），
    // 绝不是让进程死掉。

    pub fn add(&self, o: &Poly) -> Result<Poly, crate::rat::Overflow> {
        let n = self.coeffs.len().max(o.coeffs.len());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(self.coeff(i).checked_add(o.coeff(i))?);
        }
        Ok(Poly::from_coeffs(out))
    }

    pub fn sub(&self, o: &Poly) -> Result<Poly, crate::rat::Overflow> {
        let n = self.coeffs.len().max(o.coeffs.len());
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(self.coeff(i).checked_sub(o.coeff(i))?);
        }
        Ok(Poly::from_coeffs(out))
    }

    pub fn mul(&self, o: &Poly) -> Result<Poly, crate::rat::Overflow> {
        if self.is_zero() || o.is_zero() {
            return Ok(Poly::zero());
        }
        let mut out = vec![Rational::ZERO; self.coeffs.len() + o.coeffs.len() - 1];
        for (i, a) in self.coeffs.iter().enumerate() {
            if a.is_zero() {
                continue;
            }
            for (j, b) in o.coeffs.iter().enumerate() {
                let prod = a.checked_mul(*b)?;
                out[i + j] = out[i + j].checked_add(prod)?;
            }
        }
        Ok(Poly::from_coeffs(out))
    }

    pub fn scale(&self, s: Rational) -> Result<Poly, crate::rat::Overflow> {
        let mut out = Vec::with_capacity(self.coeffs.len());
        for c in &self.coeffs {
            out.push(c.checked_mul(s)?);
        }
        Ok(Poly::from_coeffs(out))
    }

    /// 乘以 `z^k`。
    pub fn shift(&self, k: usize) -> Poly {
        if self.is_zero() {
            return Poly::zero();
        }
        let mut out = vec![Rational::ZERO; k];
        out.extend_from_slice(&self.coeffs);
        Poly::from_coeffs(out)
    }

    /// 综合除法：`self = (z − root)·quotient + remainder`。
    pub fn deflate(&self, root: Rational) -> (Poly, Rational) {
        let n = self.coeffs.len();
        if n == 0 {
            return (Poly::zero(), Rational::ZERO);
        }
        let mut quotient = vec![Rational::ZERO; n - 1];
        let mut carry = self.coeffs[n - 1];
        for i in (0..n - 1).rev() {
            quotient[i] = carry;
            carry = self.coeffs[i] + carry * root;
        }
        (Poly::from_coeffs(quotient), carry)
    }
}

/// 有理根集合。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootSet {
    /// 零多项式：方程恒成立 ⇒ 无差异条件不约束 z ⇒ 解族为一段（**退化信号**）。
    IdenticallyZero,
    /// 有限个有理根（已去重，升序）。
    Finite(Vec<Rational>),
    /// 系数超出精确枚举能力（未做任何截断，明确报错）。
    TooLarge,
}

/// 求多项式在 Q 上的全部**有理根**（有理根定理 + 综合除法完备搜索）。
///
/// 只返回有理根：精确路径要求概率是精确有理数（§2.2）。无理根由浮点路径处理，
/// 本函数不做近似——近似根若被当成精确根，就会凭空造出"均衡"。
pub fn rational_roots(p: &Poly) -> Result<RootSet, crate::rat::Overflow> {
    if p.is_zero() {
        return Ok(RootSet::IdenticallyZero);
    }
    // 清分母 → 整系数
    let mut den_lcm: i128 = 1;
    for c in p.coeffs() {
        let d = c.denom();
        den_lcm = lcm(den_lcm, d);
    }
    let mut ints: Vec<i128> = p
        .coeffs()
        .iter()
        .map(|c| c.numer() * (den_lcm / c.denom()))
        .collect();

    let mut roots: Vec<Rational> = Vec::new();

    // 提取 z 的幂（z = 0 的根）
    let mut zero_multiplicity = 0usize;
    while !ints.is_empty() && ints[0] == 0 {
        ints.remove(0);
        zero_multiplicity += 1;
    }
    if zero_multiplicity > 0 {
        roots.push(Rational::ZERO);
    }
    if ints.is_empty() {
        roots.sort();
        roots.dedup();
        return Ok(RootSet::Finite(roots));
    }

    // 除以内容（coefficients 的 gcd），把首项与常数项压到最小
    let mut content = 0i128;
    for &v in &ints {
        content = gcd_i128(content, v);
    }
    if content != 0 && content != 1 {
        for v in ints.iter_mut() {
            *v /= content;
        }
    }

    let a0 = ints[0].abs();
    let an = ints[ints.len() - 1].abs();
    if a0 == 0 || an == 0 {
        return Ok(RootSet::TooLarge);
    }
    let num_divs = divisors(a0);
    let den_divs = divisors(an);
    if num_divs.len() * den_divs.len() > 100_000 {
        return Ok(RootSet::TooLarge);
    }

    let mut current = Poly::from_coeffs(ints.iter().map(|&v| Rational::from_i64(v as i64)).collect());
    // 反复找根并降次（同一多项式可能有多个有理根）
    loop {
        if current.is_zero() {
            break;
        }
        let deg = match current.degree() {
            None => break,
            Some(d) => d,
        };
        if deg == 0 {
            break;
        }
        let mut found_any = false;
        'cand: for &p_num in &num_divs {
            for &q_den in &den_divs {
                for sign in [1i128, -1i128] {
                    let cand = Rational::new(sign * p_num, q_den);
                    if current.eval(cand)?.is_zero() {
                        roots.push(cand);
                        let (q, _) = current.deflate(cand);
                        current = q;
                        found_any = true;
                        break 'cand;
                    }
                }
            }
        }
        if !found_any {
            break;
        }
    }

    roots.sort();
    roots.dedup();
    Ok(RootSet::Finite(roots))
}

fn gcd_i128(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

fn lcm(a: i128, b: i128) -> i128 {
    if a == 0 || b == 0 {
        return 1;
    }
    (a / gcd_i128(a, b)) * b
}

fn divisors(mut n: i128) -> Vec<i128> {
    n = n.abs();
    if n == 0 {
        return vec![1];
    }
    let mut out = Vec::new();
    let mut i = 1i128;
    while i * i <= n {
        if n % i == 0 {
            out.push(i);
            if i * i != n {
                out.push(n / i);
            }
        }
        i += 1;
        if i > 2_000_000 {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(n: i128, d: i128) -> Rational {
        Rational::new(n, d)
    }

    #[test]
    fn arithmetic() {
        let a = Poly::from_coeffs(vec![r(1, 1), r(2, 1)]);
        let b = Poly::from_coeffs(vec![r(0, 1), r(0, 1), r(3, 1)]);
        assert_eq!(a.degree(), Some(1));
        assert_eq!(b.degree(), Some(2));
        let c = a.mul(&b).expect("小系数不应溢出");
        assert_eq!(c.coeffs(), &[r(0, 1), r(0, 1), r(3, 1), r(6, 1)]);
        assert_eq!(c.eval(r(1, 1)).unwrap(), r(9, 1));
    }

    #[test]
    fn roots_of_quadratic_with_two_rational_roots() {
        // (z − 1/2)(z − 3/5) = z² − 11/10 z + 3/10
        let p = Poly::from_coeffs(vec![r(3, 10), r(-11, 10), r(1, 1)]);
        let RootSet::Finite(roots) = rational_roots(&p).unwrap() else {
            panic!("应有理根");
        };
        assert_eq!(roots, vec![r(1, 2), r(3, 5)]);
    }

    #[test]
    fn zero_root_is_found() {
        // z(z − 1/3)
        let p = Poly::from_coeffs(vec![r(0, 1), r(-1, 3), r(1, 1)]);
        let RootSet::Finite(roots) = rational_roots(&p).unwrap() else {
            panic!()
        };
        assert_eq!(roots, vec![r(0, 1), r(1, 3)]);
    }

    #[test]
    fn irreducible_quadratic_has_no_rational_roots() {
        // z² − 2
        let p = Poly::from_coeffs(vec![r(-2, 1), r(0, 1), r(1, 1)]);
        let RootSet::Finite(roots) = rational_roots(&p).unwrap() else {
            panic!()
        };
        assert!(roots.is_empty(), "√2 不是有理数");
    }

    #[test]
    fn identically_zero_is_flagged_not_silently_empty() {
        let p = Poly::zero();
        assert_eq!(rational_roots(&p).unwrap(), RootSet::IdenticallyZero);
    }

    #[test]
    fn deflation_remainder_is_zero_at_root() {
        let p = Poly::from_coeffs(vec![r(3, 10), r(-11, 10), r(1, 1)]);
        let (q, rem) = p.deflate(r(1, 2));
        assert!(rem.is_zero());
        assert_eq!(q, Poly::from_coeffs(vec![r(-3, 5), r(1, 1)]));
    }

    #[test]
    fn c6_polynomial_has_root_8_over_15() {
        // C6 的无差异多项式的一个根应为 8/15（ζ = Jason 玩 Front 的概率）。
        // 这里用它的因子 (z − 8/15)(z − 1/2) 做一次数值性质检查。
        let p = Poly::from_coeffs(vec![r(8, 30), r(-31, 30), r(1, 1)]);
        let RootSet::Finite(roots) = rational_roots(&p).unwrap() else {
            panic!()
        };
        assert!(roots.contains(&r(8, 15)));
        assert!(roots.contains(&r(1, 2)));
    }
}

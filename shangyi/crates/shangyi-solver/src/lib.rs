//! # 商奕求解器（`shangyi-solver`）
//!
//! 标准式博弈的纳什均衡求解、解选择器与精炼。全部算法自研——
//! `03-rust-solvers-and-licensing.md` §1.7 已证明 Rust 生态**没有**成熟的纳什均衡库
//! （除 2 人零和扩展式 CFR 外），因此本 crate 不引入任何第三方数值依赖。
//!
//! ## 算法清单（口径声明）
//!
//! | 算法 | 适用范围 | 完备性 |
//! |---|---|---|
//! | 支持枚举（精确有理数） | 2 人（任意规模） | **完备**（非退化博弈；退化由 `Completeness` 标记） |
//! | 支持枚举（精确有理数） | 3 人（消元到一元多项式） | **对可归约支撑剖面完备**，不可归约者计入 `Completeness::Partial` |
//! | Lemke–Howson（精确有理数枢轴 + 字典序防退化） | 2 人 | **不完备**（上游文档明确 "not guaranteed to find all equilibria"）；只作交叉检查 |
//! | 解析路径 | Cournot 连续策略 | 唯一均衡（解析解） |
//!
//! ## 验证（本 crate 最重要的部分）
//!
//! 所有候选点都必须通过 [`verify::residuals`] 的三条检查（单纯形残余、支撑内无差异、
//! **最佳反应间隙**）才能进入解集。第三条不是可选项：只解无差异方程会产出
//! "幽灵均衡"（`09-solver-test-vectors.md` §3.6 的 C6 例），见 [`verify`] 的模块文档。
//!
//! ## 明确未实现（待办，见 README 与本 crate 报告）
//!
//! - 扩展式博弈 / 子博弈完美 / 逆向归纳 / 序贯均衡（标准式下 SPE ≡ NE，见 [`refine`]）；
//! - QRE、level-k、虚构对局、复制者动态（`09` §8 已登记为缺向量区）；
//! - 稀疏/大规模博弈（策略数上限按小规模设计，精确路径的容量受 i128 限制）。

pub mod analytic;
pub mod game;
pub mod lemke_howson;
pub mod poly;
pub mod rat;
pub mod refine;
pub mod selectors;
pub mod support;
pub mod support_np;
pub mod verify;

pub use game::{GameError, NormalFormGame, Tolerance};
pub use rat::{LinearSolution, Rational};
pub use verify::{Residuals, VerificationReport};

use std::fmt;

/// 求解方法（请求侧的选择）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveMethod {
    /// 支持枚举（2 人完备；3 人按可归约支撑剖面）。
    SupportEnumeration,
    /// Lemke–Howson（2 人；不完备，仅作交叉检查）。
    LemkeHowson,
    /// 解析/数值路径（连续策略，如 Cournot）。
    Analytic,
}

/// 实际走通的求解路径（响应侧的事实，用于"路由不得静默降级"的断言）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SolveRoute {
    SupportEnumeration,
    LemkeHowson,
    Analytic,
}

/// 解集完备性。**必须**由调用方检查：`Partial` 表示该博弈的某些支撑剖面
/// 落在精确路径的能力之外（已计入计数），此时"解集完整"这一主张不成立。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completeness {
    /// 全部支撑剖面都被精确穷举并通过验证。
    Complete,
    /// 存在未被精确求解的支撑剖面（退化/欠定/超出归约能力）。
    Partial {
        /// 超出精确路径能力的支撑剖面数
        unsupported: usize,
        /// 无差异系统欠定（连续统信号）的支撑剖面数
        underdetermined: usize,
        /// 无差异系统解出但 BR 不等式为空集（幽灵）的支撑剖面数
        ghost: usize,
    },
    /// 该算法**本身**不保证完备（Lemke–Howson：上游文档明确
    /// "not guaranteed to find all equilibria"）。此时解集只能作交叉检查用。
    NotGuaranteed { algorithm: &'static str },
}

impl Completeness {
    pub fn is_complete(&self) -> bool {
        matches!(self, Completeness::Complete)
    }
}

/// 均衡类型标签（派生量，不是自报量：`Pure` ⟺ 全部分量 ∈ {0,1}）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EqKind {
    Pure,
    Mixed,
}

/// 一个求出的均衡。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Equilibrium {
    pub kind: EqKind,
    /// 每个玩家的支撑（策略下标升序）。
    pub support: Vec<Vec<usize>>,
    /// 每个玩家的混合策略（长度 = 策略数；非支撑分量为精确 0）。
    pub strategy: Vec<Vec<Rational>>,
    /// 每人的期望支付。
    pub payoffs: Vec<Rational>,
    /// 三残余量（精确值，供断言与报告）。
    pub residuals: Residuals,
}

impl Equilibrium {
    pub fn from_strategy(
        game: &NormalFormGame,
        sigma: Vec<Vec<Rational>>,
    ) -> Option<Equilibrium> {
        let r = verify::residuals(game, &sigma);
        if !r.is_exact_equilibrium() {
            return None;
        }
        let support = verify::support_of(&sigma);
        let kind = if support
            .iter()
            .zip(sigma.iter())
            .all(|(s, v)| s.len() == 1 && v.iter().filter(|x| !x.is_zero()).count() == 1)
        {
            EqKind::Pure
        } else {
            EqKind::Mixed
        };
        let payoffs = verify::expected_payoffs(game, &sigma);
        Some(Equilibrium { kind, support, strategy: sigma, payoffs, residuals: r })
    }

    /// 是否完全混合（每个玩家在其全部策略上都给正概率）。C6 的关键断言。
    pub fn is_completely_mixed(&self, game: &NormalFormGame) -> bool {
        (0..game.players()).all(|p| self.strategy[p].iter().all(|v| v.is_positive()))
    }
}

/// 求解结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Solution {
    pub equilibria: Vec<Equilibrium>,
    pub route: SolveRoute,
    pub completeness: Completeness,
}

/// 求解错误。全部为**类型化**错误——对齐设计文档不变量②"崩溃而非静默"：
/// 退化、不收敛、支撑集为空都必须以类型化错误上报，禁止返回空解集掩盖失败。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SolveError {
    /// 博弈定义非法。
    InvalidGame(String),
    /// 玩家数不受支持（本 crate 只做 2 人完备与 3 人可归约；4 人待办）。
    UnsupportedPlayerCount(usize),
    /// 3 人支撑剖面无法归约为 1 元多项式（见 `support_np` 文档）。
    UnsupportedSupportProfile { supports: Vec<Vec<usize>>, reason: String },
    /// 无差异系统的解不唯一 ⇒ 连续统（退化）。按 §2.4 第 5 条**必须**类型化上报。
    Degenerate { witness: String },
    /// 请求的方法不适用于该博弈（例如对连续策略走支持枚举）。
    MethodNotApplicable { method: SolveMethod, reason: String },
    /// 输入的策略向量与博弈规模不符，或该判定对该博弈类别未定义。
    InvalidStrategy(&'static str),
    /// 尚未实现（明确列出，避免"A 静默降级为 B"）。
    NotImplemented(&'static str),
    /// 精确路径能力溢出。
    Overflow(String),
}

impl fmt::Display for SolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SolveError::InvalidGame(m) => write!(f, "博弈定义非法：{m}"),
            SolveError::UnsupportedPlayerCount(n) => {
                write!(f, "玩家数 {n} 不受支持（2 人完备 / 3 人可归约；4 人待办）")
            }
            SolveError::UnsupportedSupportProfile { supports, reason } => {
                write!(f, "支撑剖面 {supports:?} 超出精确路径能力：{reason}")
            }
            SolveError::Degenerate { witness } => write!(f, "退化博弈（解集非有限）：{witness}"),
            SolveError::MethodNotApplicable { method, reason } => {
                write!(f, "方法 {method:?} 不适用：{reason}")
            }
            SolveError::InvalidStrategy(m) => write!(f, "策略向量非法/判定未定义：{m}"),
            SolveError::NotImplemented(what) => write!(f, "尚未实现：{what}"),
            SolveError::Overflow(m) => write!(f, "精确算术溢出：{m}"),
        }
    }
}

impl std::error::Error for SolveError {}

/// 精确算术溢出 ⇒ `SolveError::Overflow`。
///
/// 有了这个转换，`Poly` 的 checked 算子可以用 `?` 直接传播——
/// 溢出从**进程崩溃**变成**类型化拒绝**（实测事故：一次真实咨询在 solve
/// 阶段因 panic 直接死掉，CLI 没有机会给出可读错误）。
impl From<crate::rat::Overflow> for SolveError {
    fn from(o: crate::rat::Overflow) -> Self {
        SolveError::Overflow(format!(
            "{o}。\n\
             成因：精确有理数的分母在多次乘法后超出 i128（如 0.1 的精确分母是 2^52，\
             三次相乘即到 2^156）。**这不是数值大小问题，是精确路径的固有边界。**\n\
             ▸ 2 人博弈通常不会触发；**3 人及以上**的支撑枚举极易触发。\n\
             ▸ 可行做法：减小支付值的精度（用整数或短小数），或改用数值路径。"
        ))
    }
}

/// 求解请求。
#[derive(Debug, Clone)]
pub struct SolveRequest {
    pub game: NormalFormGame,
    pub tolerance: Tolerance,
    pub method: SolveMethod,
}

impl SolveRequest {
    pub fn new(game: NormalFormGame) -> Self {
        SolveRequest {
            game,
            tolerance: Tolerance::GOLDEN,
            method: SolveMethod::SupportEnumeration,
        }
    }

    pub fn with_method(mut self, method: SolveMethod) -> Self {
        self.method = method;
        self
    }

    pub fn with_tolerance(mut self, tol: Tolerance) -> Self {
        self.tolerance = tol;
        self
    }
}

/// 求解后端（`09` §7 断言骨架里的 `SolverBackend`）。
pub trait SolverBackend {
    fn solve(&self, request: SolveRequest) -> Result<Solution, SolveError>;
}

/// 参考实现：按请求方法路由到支持枚举 / Lemke–Howson / 解析路径。
#[derive(Debug, Default, Clone, Copy)]
pub struct NativeSolver;

impl SolverBackend for NativeSolver {
    fn solve(&self, request: SolveRequest) -> Result<Solution, SolveError> {
        // ⚠️ **必须 catch panic**——`Rational` 的算术溢出以 panic 上抛
        // （`Add`/`Mul` 的签名不允许返回错误，见 `overflow_panic` 的文档），
        // 而它发生在**求解深处的乘加**里，调用方无法提前预判。
        //
        // 实测（批量样本第 6 条）：模型把"60–90 万"直接写进支付矩阵，
        // 乘加几次就溢出 i128 ⇒ 进程 panic ⇒ 整个咨询死亡，
        // 用户只看到进程退出。而 `SolveError::Overflow` 变体早就存在，
        // 只是没人把 panic 接过来。
        //
        // # 为什么用 catch_unwind 而不是全面重构 checked 算术
        //
        // 把每个 `+`/`*` 换成 checked 是数百个调用点的重构，且会让
        // 数学表达被错误处理噪声淹没。catch_unwind 在**边界**拦截：
        // 求解内部继续用自然的数学写法，溢出在边界变成类型化错误。
        //
        // # 前提（重要）
        //
        // panic 点**不持有需要清理的资源**（Rational 是纯值，
        // 无 File/Mutex）⇒ unwinding 安全。若将来在求解路径上加了
        // 需要清理的资源，这里要重新评估。
        //
        // `panic = "abort"` 的构建下本函数无效——workspace 的 profile
        // 未设 abort（默认 unwind），但这是一条**依赖**，写进测试。
        let method = request.method;
        let game = request.game;
        let inner = move || match method {
            SolveMethod::SupportEnumeration => solve_support_enumeration(&game),
            SolveMethod::LemkeHowson => lemke_howson::solve_all(&game),
            SolveMethod::Analytic => Err(SolveError::MethodNotApplicable {
                method: SolveMethod::Analytic,
                reason: "标准式离散博弈没有解析路径；Cournot 走 analytic::solve_cournot".into(),
            }),
        };
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(inner)) {
            Ok(r) => r,
            Err(payload) => {
                // 尽力提取 panic 消息（它带定位信息），失败则给通用文案
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "精确算术溢出（无详细消息）".into());
                Err(SolveError::Overflow(format!(
                    "{msg}\n\
                     → 这是**输入规模问题**，不是「没有均衡」：\
                     支付值过大（常见：把金额直接写进支付）。\
                     请把支付做**单位归约**（如用「利润占营收的比例」而非元），\
                     或缩小数值量级后重试。"
                )))
            }
        }
    }
}

/// 支持枚举入口：2 人走完备路径，3 人走可归约路径，4 人明确报"待办"。
pub fn solve_support_enumeration(game: &NormalFormGame) -> Result<Solution, SolveError> {
    let (eqs, completeness) = match game.players() {
        2 => support::enumerate_2p(game)?,
        3 => support_np::enumerate_3p(game)?,
        n => return Err(SolveError::UnsupportedPlayerCount(n)),
    };
    Ok(Solution {
        equilibria: finish(game, eqs),
        route: SolveRoute::SupportEnumeration,
        completeness,
    })
}

/// 去重 + 确定性排序。T8（可复现性）要求同一输入两次求解给出**逐位相同**的序列。
pub(crate) fn finish(game: &NormalFormGame, mut eqs: Vec<Vec<Vec<Rational>>>) -> Vec<Equilibrium> {
    let mut out: Vec<Equilibrium> = Vec::new();
    for sigma in eqs.drain(..) {
        let Some(eq) = Equilibrium::from_strategy(game, sigma) else {
            // 求解器内部产出的候选必须自证；否则是求解器缺陷，直接丢弃由调用方计数。
            continue;
        };
        if out.iter().any(|e| e.strategy == eq.strategy) {
            continue;
        }
        out.push(eq);
    }
    out.sort_by(|a, b| {
        a.support
            .cmp(&b.support)
            .then_with(|| strategy_cmp(&a.strategy, &b.strategy))
    });
    out
}

fn strategy_cmp(a: &[Vec<Rational>], b: &[Vec<Rational>]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let c = x.cmp(y);
        if c != std::cmp::Ordering::Equal {
            return c;
        }
    }
    std::cmp::Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equilibrium_kind_is_derived_not_reported() {
        let g = NormalFormGame::from_bimatrix(
            &[vec![3, 0], vec![0, 2]],
            &[vec![2, 0], vec![0, 3]],
        )
        .unwrap();
        let pure = Equilibrium::from_strategy(
            &g,
            vec![vec![Rational::ONE, Rational::ZERO], vec![Rational::ONE, Rational::ZERO]],
        )
        .unwrap();
        assert_eq!(pure.kind, EqKind::Pure);
        let mixed = Equilibrium::from_strategy(
            &g,
            vec![
                vec![Rational::new(3, 5), Rational::new(2, 5)],
                vec![Rational::new(2, 5), Rational::new(3, 5)],
            ],
        )
        .unwrap();
        assert_eq!(mixed.kind, EqKind::Mixed);
        assert!(mixed.is_completely_mixed(&g));
        // 非均衡不得被构造成 Equilibrium（BR 检查把关）
        assert!(Equilibrium::from_strategy(
            &g,
            vec![vec![Rational::ONE, Rational::ZERO], vec![Rational::ZERO, Rational::ONE]]
        )
        .is_none());
    }

    #[test]
    fn dedup_and_deterministic_order() {
        let g = NormalFormGame::from_bimatrix(
            &[vec![5, 0], vec![4, 2]],
            &[vec![5, 4], vec![0, 2]],
        )
        .unwrap();
        let a = solve_support_enumeration(&g).unwrap();
        let b = solve_support_enumeration(&g).unwrap();
        assert_eq!(a.equilibria.len(), 3);
        assert_eq!(a, b, "同一输入两次求解必须逐位一致（T8）");
    }
}

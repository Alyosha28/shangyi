//! `shangyi-consequence` 的测试。
//!
//! # 最重要的一个测试：`xingyu_regression`
//!
//! 设计文档 §11.3 / §16.2 的门⑤要求：**以星宇股份事件（2026-08）为原型构造
//! "弱势方 + 压力传导链"案例，验证开放行动空间能产出"向对手客户/监管投诉"类
//! 候选并通过三重过滤**。
//!
//! 这个测试是**回归护栏**：它锁住"系统不会因为重构而退回封闭策略空间"。
//! 若有人为了简化而移除非常规反应生成，本测试会失败。

use std::collections::BTreeMap;

use shangyi_consequence::{
    ConsequenceReport, FilterError, FilterVerdict, Interval, Lcg, NovelMoveProposal, Percentiles,
    ReactionOutcome, TripleFilter, filter_and_register, monte_carlo_percentiles,
    percentiles_from_samples,
};
use shangyi_gamespec::{GameSpec, Participant, RelationToGame, ValueNetRole};

fn participant(id: &str, name: &str, role: ValueNetRole) -> Participant {
    Participant {
        id: id.to_string(),
        name: name.to_string(),
        vn_role: role,
        relation_to_game: RelationToGame::Player,
        profile_ref: None,
    }
}

/// 一个最小 GameSpec：本企业 + 一名对手。
fn minimal_spec() -> GameSpec {
    let mut strategies = BTreeMap::new();
    strategies.insert("focal".to_string(), vec!["维持价".into(), "降价10%".into()]);
    strategies.insert("comp_a".to_string(), vec!["跟随".into(), "不动".into()]);
    let mut params = BTreeMap::new();
    params.insert(
        "P1".to_string(),
        shangyi_gamespec::Param {
            name: "P1".into(),
            value: shangyi_gamespec::Expr::Literal(42.0),
            dist: None,
            source: shangyi_gamespec::ParamSource::UserConfirmed {
                checkpoint: "cp2".into(),
            },
            unit: None,
        },
    );
    GameSpec {
        game_id: "case-whatif".into(),
        players: vec![
            participant("focal", "本企业", ValueNetRole::Focal),
            participant("comp_a", "竞争对手A", ValueNetRole::Substitutor),
        ],
        strategies,
        payoffs: shangyi_gamespec::PayoffForm::Normal {
            cells: BTreeMap::new(),
        },
        params,
        information: None,
        dynamics: None,
        external_actors: vec![],
        preference: None,
        meta: BTreeMap::new(),
    }
}

fn iv(lo: f64, hi: f64) -> Interval {
    Interval::new(lo, hi).expect("合法区间")
}

// ─────────────────────────────────────────────────────────────────────────────
// 门⑤：星宇事件回归测试
// ─────────────────────────────────────────────────────────────────────────────

/// **设计文档 §11.3 门⑤ 的回归护栏。**
///
/// 原型：2026-08 星宇股份批量解约 107 名应届生。学生的致命反击**不在劳资博弈
/// 策略矩阵内**——向该公司的**客户与海外供应商**发函投诉、诉至**港交所**，
/// 借新参与者的 ESG / IPO 声誉激励改变支付结构。
///
/// 本测试验证：
/// 1. 系统能**接受**此类矩阵外候选（若封闭策略空间，则第一项就会失败）
/// 2. 通过三重过滤后，**新参与者被注册进 `external_actors`**（这是"改变棋盘"的机制）
/// 3. 被拒候选**如实返回**而非静默丢弃
#[test]
fn xingyu_regression_open_action_space() {
    let mut spec = minimal_spec();
    assert!(spec.external_actors.is_empty(), "前置：初始无环境方");

    // 学生方（博弈中的弱势方）的矩阵外反击提议。
    // 关键：指向"对手的客户"，并引入"交易所"作为新参与者。
    let complaint_to_customers = NovelMoveProposal {
        actor_id: "students".into(),
        description: "向对手的大客户与海外供应商发函投诉其 ESG 记录".into(),
        targets: Some("comp_a 的大客户".into()),
        introduces_actors: vec![Participant {
            id: "major_customer".into(),
            name: "对手的大客户".into(),
            vn_role: ValueNetRole::Customer,
            relation_to_game: RelationToGame::ExternalActor,
            profile_ref: None,
        }],
        payoff_channel:
            "客户的 ESG 采购政策 → 对手面临订单流失风险 → 其声誉激励改变支付结构".into(),
        expected_benefit: iv(8.0, 15.0),
        expected_cost: iv(1.0, 3.0),
        legality_basis: vec!["民事投诉权".into(), "公开信息举报".into()],
        feasibility_basis: vec!["公开邮箱与官网投诉渠道".into(), "已发生先例".into()],
    };

    // 引入交易所（IPO 进程受威胁）
    let complaint_to_exchange = NovelMoveProposal {
        actor_id: "students".into(),
        description: "向港交所投诉其在 IPO 进程中的用工合规问题".into(),
        targets: Some("港交所".into()),
        introduces_actors: vec![Participant {
            id: "exchange".into(),
            name: "港交所".into(),
            vn_role: ValueNetRole::Complementor,
            relation_to_game: RelationToGame::ExternalActor,
            profile_ref: None,
        }],
        payoff_channel: "交易所上市规则对用工合规有审查要求 → IPO 进程受威胁".into(),
        expected_benefit: iv(10.0, 20.0),
        expected_cost: iv(0.5, 2.0),
        legality_basis: vec!["交易所上市规则投诉机制".into()],
        feasibility_basis: vec!["交易所公开投诉入口".into()],
    };

    let outcome = filter_and_register(
        &mut spec,
        &[complaint_to_customers, complaint_to_exchange],
        &TripleFilter::default(),
    );

    // ① 两个候选都应通过（它们是有效的、可执行的、激励相容的反击）
    assert_eq!(
        outcome.accepted.len(),
        2,
        "向客户/交易所投诉类候选必须通过三重过滤；被拒原因：{:?}",
        outcome
            .rejected
            .iter()
            .map(|(p, v)| (p.description.clone(), v.rejections.clone()))
            .collect::<Vec<_>>()
    );

    // ② 新参与者必须被注册进 external_actors（这是"改变棋盘"的机制）
    let ids: Vec<&str> = spec
        .external_actors
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert!(
        ids.contains(&"major_customer"),
        "对手的大客户必须被注册为环境方，实得 {ids:?}"
    );
    assert!(
        ids.contains(&"exchange"),
        "交易所必须被注册为环境方，实得 {ids:?}"
    );

    // ③ 被引入者的定位必须被强制为 ExternalActor（不参与原始策略组合）
    for a in &spec.external_actors {
        assert_eq!(
            a.relation_to_game,
            RelationToGame::ExternalActor,
            "被引入者的定位必须为环境方"
        );
    }
}

/// 门⑤ 的反面：**封闭策略空间会对这类反击失明**——本测试固定这一认识。
///
/// 若只枚举既定策略集（`focal`/`comp_a` 的价格策略），"向客户投诉"这类行动
/// 根本不会出现。这是星宇事件的核心教训，写成测试以防后人"简化"掉开放行动空间。
#[test]
fn closed_strategy_space_would_miss_novel_moves() {
    let spec = minimal_spec();
    // 既定策略集里没有任何"引入第三方"的行动
    for (_player, strategies) in &spec.strategies {
        for s in strategies {
            assert!(
                !s.contains("投诉") && !s.contains("监管") && !s.contains("交易所"),
                "既定策略集不应含非常规反击（这正是需要开放行动空间的原因）：{s}"
            );
        }
    }
    // 且初始没有环境方——若不注册，博弈结构永远看不到"客户/交易所"这一层
    assert!(spec.external_actors.is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// 三重过滤的三项判据
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn filter_rejects_missing_legality_basis() {
    let f = TripleFilter::default();
    let p = NovelMoveProposal {
        actor_id: "a".into(),
        description: "散布对手负面谣言".into(),
        targets: None,
        introduces_actors: vec![],
        payoff_channel: "打击声誉".into(),
        expected_benefit: iv(10.0, 20.0),
        expected_cost: iv(1.0, 2.0),
        legality_basis: vec![], // 无依据
        feasibility_basis: vec!["有渠道".into()],
    };
    let v = f.evaluate(&p);
    assert!(!v.legal, "无合法性依据必须被拒");
    assert!(!v.passes());
    assert!(
        v.rejections.iter().any(|r| r.contains("合法性")),
        "拒绝原因须点明合法性：{:?}",
        v.rejections
    );
}

#[test]
fn filter_rejects_missing_feasibility_basis() {
    let f = TripleFilter::default();
    let p = NovelMoveProposal {
        actor_id: "a".into(),
        description: "起诉对手".into(),
        targets: None,
        introduces_actors: vec![],
        payoff_channel: "司法救济".into(),
        expected_benefit: iv(50.0, 100.0),
        expected_cost: iv(1.0, 2.0),
        legality_basis: vec!["民事诉讼法".into()],
        feasibility_basis: vec![], // 虽然合法，但没有执行路径依据
    };
    let v = f.evaluate(&p);
    assert!(!v.feasible, "无执行路径依据必须被拒");
    assert!(
        v.rejections.iter().any(|r| r.contains("资源可得性")),
        "{:?}",
        v.rejections
    );
}

#[test]
fn filter_rejects_incentive_incompatible() {
    let f = TripleFilter::default();
    let p = NovelMoveProposal {
        actor_id: "a".into(),
        description: "代价极高的对抗行动".into(),
        targets: None,
        introduces_actors: vec![],
        payoff_channel: "x".into(),
        // 成本远超收益：必然亏损
        expected_benefit: iv(1.0, 2.0),
        expected_cost: iv(50.0, 100.0),
        legality_basis: vec!["合法".into()],
        feasibility_basis: vec!["可行".into()],
    };
    let v = f.evaluate(&p);
    assert!(!v.incentive_compatible, "必然亏损必须被判为激励不相容");
    assert!((v.incentive_probability - 0.0).abs() < 1e-12);
}

#[test]
fn incentive_probability_is_one_when_certainly_profitable() {
    let f = TripleFilter::default();
    // 收益下界 > 成本上界 ⇒ 必然盈利
    let p = f.incentive_probability(&iv(10.0, 20.0), &iv(1.0, 2.0));
    assert!((p - 1.0).abs() < 1e-12, "实得 {p}");
}

#[test]
fn incentive_probability_is_zero_when_certainly_lossy() {
    let f = TripleFilter::default();
    let p = f.incentive_probability(&iv(1.0, 2.0), &iv(10.0, 20.0));
    assert!(p.abs() < 1e-12, "实得 {p}");
}

#[test]
fn incentive_probability_uses_probability_not_expectation() {
    // 这是 R18 的口径修订：原设计写"预期收益 > 成本"，
    // 现改为 P(收益 > 成本) ≥ 阈值。
    // 构造一个"期望为正但概率不足"的情形：
    //   收益 ∈ [0, 100]（期望 50），成本 = 60（点估计）
    //   期望收益 50 < 60 ⇒ 旧口径判"不相容"
    //   而 P(收益 > 60) = (100-60)/(100-0) = 0.4 < 0.6 ⇒ 新口径也判"不相容"
    // 两者同向，但新口径给出了**概率值**，可进报告。
    let f = TripleFilter::default();
    let prob = f.incentive_probability(&iv(0.0, 100.0), &iv(60.0, 60.0));
    assert!(
        (prob - 0.4).abs() < 1e-9,
        "P(收益 > 成本) 应为 0.4，实得 {prob}"
    );
    let p = NovelMoveProposal {
        actor_id: "a".into(),
        description: "x".into(),
        targets: None,
        introduces_actors: vec![],
        payoff_channel: "x".into(),
        expected_benefit: iv(0.0, 100.0),
        expected_cost: iv(60.0, 60.0),
        legality_basis: vec!["合法".into()],
        feasibility_basis: vec!["可行".into()],
    };
    let v = f.evaluate(&p);
    assert!(
        !v.incentive_compatible,
        "P=0.4 < 阈值 0.6 应判不相容，实得 {:?}",
        v
    );
}

#[test]
fn wider_uncertainty_pushes_probability_toward_half() {
    // 信息越少越接近 0.5——这是本估计式的设计意图。
    //
    // ⚠️ 测试设计要点：估计式的解析形式为
    //     P = 0.5 + (收益中点 − 成本中点) / (2·(收益半宽 + 成本半宽))
    //   **中点差为 0 时无论区间多宽 P 都恰为 0.5**，测不出该性质。
    //   （我第一版测试就踩了这个坑：两组输入的中点差都是 0。）
    //   故须**固定中点差、只改宽度**。
    let f = TripleFilter::default();

    // 中点差固定 = 100 − 90 = 10；窄区间（半宽各 5）
    let narrow = f.incentive_probability(&iv(95.0, 105.0), &iv(85.0, 95.0));
    // 同中点差，宽区间（半宽各 50）
    let wide = f.incentive_probability(&iv(50.0, 150.0), &iv(40.0, 140.0));

    assert!(
        (narrow - 0.5).abs() > (wide - 0.5).abs(),
        "同中点差下，窄区间应给出更极端的概率（更确定）：narrow={narrow} wide={wide}"
    );
    // 窄区间下"必然盈利"（收益下界 95 > 成本上界 95 不成立，故为上界情形）
    assert!((narrow - 1.0).abs() < 1e-9, "实得 {narrow}");
    // 宽区间因信息不足被拉向 0.5
    assert!((wide - 0.55).abs() < 1e-9, "实得 {wide}");
}

#[test]
fn probability_analytic_form_matches_implementation() {
    // 把推导出的解析式写成测试，锁住实现不被改动：
    //   P = 0.5 + (m_b − m_c) / (2·(hb + hc))
    // 在多组随机输入上与实现比对（用固定序列保证可复现）。
    let f = TripleFilter::default();
    let mut rng = Lcg::new(20260918);
    for _ in 0..200 {
        let m_b = rng.next_uniform(-100.0, 100.0);
        let m_c = rng.next_uniform(-100.0, 100.0);
        let hb = rng.next_uniform(0.0, 50.0);
        let hc = rng.next_uniform(0.0, 50.0);
        let b = iv(m_b - hb, m_b + hb);
        let c = iv(m_c - hc, m_c + hc);

        let got = f.incentive_probability(&b, &c);

        // 仅在中点差能被宽度"消化"（无区间吞并）时用解析式比对
        let analytic = if b.lo > c.hi {
            1.0
        } else if b.hi <= c.lo {
            0.0
        } else {
            0.5 + (m_b - m_c) / (2.0 * (hb + hc))
        };
        let analytic = analytic.clamp(0.0, 1.0);
        assert!(
            (got - analytic).abs() < 1e-9,
            "b={b:?} c={c:?} got={got} analytic={analytic}"
        );
    }
}

#[test]
fn threshold_is_configurable() {
    let strict = TripleFilter::new(0.9).unwrap();
    let lenient = TripleFilter::new(0.3).unwrap();
    let b = iv(0.0, 100.0);
    let c = iv(60.0, 60.0); // P = 0.4
    assert!(strict.incentive_probability(&b, &c) < strict.incentive_threshold);
    assert!(lenient.incentive_probability(&b, &c) >= lenient.incentive_threshold);
    assert!(TripleFilter::new(1.5).is_err(), "阈值越界应报错");
    assert!(TripleFilter::new(f64::NAN).is_err());
}

#[test]
fn interval_rejects_inverted_and_nonfinite() {
    assert!(matches!(
        Interval::new(10.0, 1.0),
        Err(FilterError::InvertedInterval { .. })
    ));
    assert!(matches!(
        Interval::new(f64::NAN, 1.0),
        Err(FilterError::NonFiniteInterval)
    ));
    assert!(matches!(
        Interval::new(0.0, f64::INFINITY),
        Err(FilterError::NonFiniteInterval)
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// 注册行为
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn passing_move_is_registered_as_strategy() {
    let mut spec = minimal_spec();
    let p = NovelMoveProposal {
        actor_id: "focal".into(), // 本企业自己采取非常规行动
        description: "联合互补者推出联合会员".into(),
        targets: None,
        introduces_actors: vec![],
        payoff_channel: "提高切换成本".into(),
        expected_benefit: iv(20.0, 30.0),
        expected_cost: iv(5.0, 8.0),
        legality_basis: vec!["反不正当竞争法允许".into()],
        feasibility_basis: vec!["已有互补方接洽".into()],
    };
    let before = spec.strategies["focal"].len();
    let out = filter_and_register(&mut spec, &[p], &TripleFilter::default());
    assert_eq!(out.accepted.len(), 1);
    assert_eq!(
        spec.strategies["focal"].len(),
        before + 1,
        "通过的非常规行动应注册为策略"
    );
    assert!(
        spec.strategies["focal"]
            .iter()
            .any(|s| s.starts_with("[非常规]")),
        "注册的策略名应带 [非常规] 前缀以便报告区分：{:?}",
        spec.strategies["focal"]
    );
}

#[test]
fn non_player_actor_does_not_add_strategy() {
    // 行动者不是本博弈的直接玩家（如"学生"不在原始博弈里）⇒ 只注册新参与者，不加策略
    let mut spec = minimal_spec();
    let p = NovelMoveProposal {
        actor_id: "students".into(), // 非玩家
        description: "投诉".into(),
        targets: None,
        introduces_actors: vec![Participant {
            id: "watchdog".into(),
            name: "媒体".into(),
            vn_role: ValueNetRole::Complementor,
            relation_to_game: RelationToGame::ExternalActor,
            profile_ref: None,
        }],
        payoff_channel: "舆论压力".into(),
        expected_benefit: iv(10.0, 20.0),
        expected_cost: iv(1.0, 2.0),
        legality_basis: vec!["言论自由".into()],
        feasibility_basis: vec!["社交媒体".into()],
    };
    let players_before: Vec<usize> = spec.strategies.values().map(|v| v.len()).collect();
    let out = filter_and_register(&mut spec, &[p], &TripleFilter::default());
    assert_eq!(out.accepted.len(), 1);
    let players_after: Vec<usize> = spec.strategies.values().map(|v| v.len()).collect();
    assert_eq!(
        players_before, players_after,
        "非玩家行动者不应改变既有玩家的策略集"
    );
    assert_eq!(spec.external_actors.len(), 1, "但新参与者应被注册");
}

#[test]
fn duplicate_external_actor_not_registered_twice() {
    let mut spec = minimal_spec();
    let mk = || NovelMoveProposal {
        actor_id: "students".into(),
        description: "同一参与者的第二个行动".into(),
        targets: None,
        introduces_actors: vec![Participant {
            id: "exchange".into(),
            name: "港交所".into(),
            vn_role: ValueNetRole::Complementor,
            relation_to_game: RelationToGame::ExternalActor,
            profile_ref: None,
        }],
        payoff_channel: "x".into(),
        expected_benefit: iv(10.0, 20.0),
        expected_cost: iv(1.0, 2.0),
        legality_basis: vec!["依据".into()],
        feasibility_basis: vec!["路径".into()],
    };
    filter_and_register(&mut spec, &[mk(), mk()], &TripleFilter::default());
    assert_eq!(
        spec.external_actors.len(),
        1,
        "同一外部参与者不得被重复注册"
    );
}

#[test]
fn rejected_moves_are_returned_not_silently_dropped() {
    // 被拒候选进报告③层——用户应能看到"系统考虑过但排除了哪些路径"
    let mut spec = minimal_spec();
    let bad = NovelMoveProposal {
        actor_id: "a".into(),
        description: "无依据的行动".into(),
        targets: None,
        introduces_actors: vec![],
        payoff_channel: "x".into(),
        expected_benefit: iv(1.0, 2.0),
        expected_cost: iv(1.0, 2.0),
        legality_basis: vec![],
        feasibility_basis: vec![],
    };
    let out = filter_and_register(&mut spec, &[bad], &TripleFilter::default());
    assert!(out.accepted.is_empty());
    assert_eq!(out.rejected.len(), 1, "被拒候选必须如实返回");
    let (_, verdict) = &out.rejected[0];
    assert!(
        verdict.rejections.len() >= 2,
        "应列出全部未通过项：{:?}",
        verdict.rejections
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Monte Carlo 与分位数
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn percentiles_match_hand_computation() {
    // 1..=10 的线性插值分位数
    let samples: Vec<f64> = (1..=10).map(|v| v as f64).collect();
    let p = percentiles_from_samples(samples).unwrap();
    // P50: pos = 0.5*9 = 4.5 ⇒ 在 5 与 6 之间 ⇒ 5.5
    assert!((p.p50 - 5.5).abs() < 1e-12, "实得 {}", p.p50);
    // P10: pos = 0.9 ⇒ 1 与 2 之间 ⇒ 1.9
    assert!((p.p10 - 1.9).abs() < 1e-12, "实得 {}", p.p10);
    // P90: pos = 8.1 ⇒ 9 与 10 之间 ⇒ 9.1
    assert!((p.p90 - 9.1).abs() < 1e-12, "实得 {}", p.p90);
}

#[test]
fn percentiles_reject_empty_and_all_nonfinite() {
    assert!(matches!(
        percentiles_from_samples(vec![]),
        Err(FilterError::ZeroSamples)
    ));
    assert!(matches!(
        percentiles_from_samples(vec![f64::NAN, f64::INFINITY]),
        Err(FilterError::ZeroSamples)
    ));
}

#[test]
fn percentiles_ordering_is_invariant_to_input_order() {
    let a = percentiles_from_samples(vec![3.0, 1.0, 4.0, 1.5, 9.0, 2.0]).unwrap();
    let b = percentiles_from_samples(vec![9.0, 2.0, 1.5, 4.0, 1.0, 3.0]).unwrap();
    assert_eq!(a, b, "分位数不应依赖输入顺序");
    assert!(a.p10 <= a.p50 && a.p50 <= a.p90);
}

#[test]
fn lcg_is_deterministic_across_instances() {
    let mut a = Lcg::new(12345);
    let mut b = Lcg::new(12345);
    for _ in 0..20 {
        assert_eq!(a.next_u64(), b.next_u64(), "同种子必须给同序列");
    }
}

#[test]
fn lcg_different_seeds_differ() {
    let mut a = Lcg::new(1);
    let mut b = Lcg::new(2);
    let sa: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
    let sb: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
    assert_ne!(sa, sb);
}

#[test]
fn lcg_handles_zero_seed() {
    // 0 是 LCG 的吸收态，必须被替换掉
    let mut a = Lcg::new(0);
    let s1 = a.next_u64();
    let s2 = a.next_u64();
    assert_ne!(s1, 0);
    assert_ne!(s1, s2, "0 种子不应导致序列退化");
}

#[test]
fn lcg_uniform_is_in_range() {
    let mut r = Lcg::new(42);
    for _ in 0..1000 {
        let v = r.next_f64();
        assert!((0.0..1.0).contains(&v), "实得 {v}");
        let u = r.next_uniform(-5.0, 5.0);
        assert!((-5.0..5.0).contains(&u), "实得 {u}");
    }
}

#[test]
fn lcg_normal_has_expected_moments() {
    // 大样本下均值 ≈ 0、方差 ≈ 1（宽松容差，只验证没写错公式）
    let mut r = Lcg::new(7);
    let n = 20_000;
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for _ in 0..n {
        let v = r.next_normal();
        sum += v;
        sum_sq += v * v;
    }
    let mean = sum / n as f64;
    let var = sum_sq / n as f64 - mean * mean;
    assert!(mean.abs() < 0.05, "均值应接近 0，实得 {mean}");
    assert!((var - 1.0).abs() < 0.1, "方差应接近 1，实得 {var}");
}

#[test]
fn monte_carlo_is_reproducible() {
    let f = |rng: &mut Lcg| rng.next_normal() * 10.0 + 100.0;
    let a = monte_carlo_percentiles(500, 999, f).unwrap();
    let b = monte_carlo_percentiles(500, 999, f).unwrap();
    assert_eq!(a, b, "同 (seed, samples) 必须给同一结果——报告数字须可复现");
}

#[test]
fn monte_carlo_rejects_zero_samples() {
    assert!(matches!(
        monte_carlo_percentiles(0, 1, |r| r.next_f64()),
        Err(FilterError::ZeroSamples)
    ));
}

#[test]
fn monte_carlo_propagates_uncertainty_to_wide_interval() {
    // 参数服从 N(100, 10) ⇒ 结果的 P10/P90 应明显分开
    let p = monte_carlo_percentiles(2000, 2024, |rng| 100.0 + 10.0 * rng.next_normal()).unwrap();
    assert!(p.p10 < p.p50 && p.p50 < p.p90);
    assert!(
        p.p90 - p.p10 > 15.0,
        "σ=10 时 P10–P90 跨度应约 25，实得 {}",
        p.p90 - p.p10
    );
    // 中位数应接近均值
    assert!((p.p50 - 100.0).abs() < 2.0, "实得 {}", p.p50);
}

// ─────────────────────────────────────────────────────────────────────────────
// 报告结构与自检
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn report_normalization_check_works() {
    let mk = |prob: f64| ReactionOutcome {
        reaction: "跟随降价".into(),
        probability: prob,
        outcomes: BTreeMap::new(),
        drivers: vec!["stochastic".into()],
        is_novel: false,
    };
    let ok = ConsequenceReport {
        decision: "降价10%".into(),
        reactions: vec![mk(0.6), mk(0.4)],
        rejected_novel_moves: vec![],
        note: "".into(),
    };
    assert!(ok.probabilities_normalized());

    let bad = ConsequenceReport {
        decision: "降价10%".into(),
        reactions: vec![mk(0.6), mk(0.6)],
        rejected_novel_moves: vec![],
        note: "".into(),
    };
    assert!(
        !bad.probabilities_normalized(),
        "未归一化必须被自检发现（报告生成前的护栏）"
    );
}

#[test]
fn report_sorts_and_marks_novel_reactions() {
    let mk = |name: &str, prob: f64, novel: bool| ReactionOutcome {
        reaction: name.to_string(),
        probability: prob,
        outcomes: BTreeMap::<String, Percentiles>::new(),
        drivers: vec![],
        is_novel: novel,
    };
    let r = ConsequenceReport {
        decision: "降价10%".into(),
        reactions: vec![
            mk("部分跟随", 0.3, false),
            mk("[非常规] 向我的客户投诉", 0.15, true),
            mk("跟随降价", 0.55, false),
        ],
        rejected_novel_moves: vec![],
        note: "".into(),
    };
    let sorted = r.by_probability();
    assert_eq!(sorted[0].reaction, "跟随降价");
    assert_eq!(sorted[2].reaction, "[非常规] 向我的客户投诉");

    let novel = r.novel_reactions();
    assert_eq!(novel.len(), 1, "老板页需单独标注非常规反应");
    assert!(novel[0].is_novel);
}

#[test]
fn role_labels_are_chinese() {
    // 报告面向中文用户，角色名必须可读
    assert_eq!(shangyi_consequence::role_label(ValueNetRole::Focal), "本企业");
    assert_eq!(shangyi_consequence::role_label(ValueNetRole::Customer), "顾客");
    assert_eq!(shangyi_consequence::role_label(ValueNetRole::Supplier), "供应商");
    assert_eq!(
        shangyi_consequence::role_label(ValueNetRole::Substitutor),
        "替代者"
    );
    assert_eq!(
        shangyi_consequence::role_label(ValueNetRole::Complementor),
        "互补者"
    );
}

#[test]
fn verdict_passes_requires_all_three() {
    let v = FilterVerdict {
        legal: true,
        feasible: true,
        incentive_compatible: false,
        incentive_probability: 0.1,
        rejections: vec!["x".into()],
    };
    assert!(!v.passes(), "缺任一项都不得通过");

    let all = FilterVerdict {
        legal: true,
        feasible: true,
        incentive_compatible: true,
        incentive_probability: 0.9,
        rejections: vec![],
    };
    assert!(all.passes());
}

//! `shangyi-swarm` 的测试。
//!
//! 核心验证三件事（对应 §5.7 的三条承重设计）：
//! 1. **fail loud**：能力缺失必须返回类型化错误，绝不静默降级
//! 2. **提议 ≠ 结论**：persona 的矩阵外提议必须经引擎三重过滤才算数
//! 3. **可恢复性**：persona 集合与种子必须持久化，使重跑得到同一分布

use std::collections::BTreeMap;

use shangyi_consequence::TripleFilter;
use shangyi_gamespec::{
    Expr, GameSpec, Param, ParamSource, Participant, PayoffForm, RelationToGame, ValueNetRole,
};
use shangyi_swarm::orchestrator::{
    OrchestratorConfig, PersonaActor, evaluate_candidates, register_accepted_novel_moves, rollout,
    rollout_n,
};
use shangyi_swarm::{
    Action, CapabilitySet, Move, Persona, PersonaSet, RequiredCapabilities, SwarmDescriptor,
    SwarmError, validate_actors,
};

fn participant(id: &str, role: ValueNetRole) -> Participant {
    Participant {
        id: id.to_string(),
        name: id.to_string(),
        vn_role: role,
        relation_to_game: RelationToGame::Player,
        profile_ref: None,
    }
}

fn spec() -> GameSpec {
    let mut strategies = BTreeMap::new();
    strategies.insert("focal".to_string(), vec!["维持价".into(), "降价10%".into()]);
    strategies.insert("comp_a".to_string(), vec!["跟随".into(), "不动".into()]);
    let mut params = BTreeMap::new();
    params.insert(
        "P1".to_string(),
        Param {
            name: "P1".into(),
            value: Expr::Literal(42.0),
            dist: None,
            source: ParamSource::UserConfirmed {
                checkpoint: "cp2".into(),
            },
            unit: None,
        },
    );
    GameSpec {
        game_id: "case-swarm".into(),
        players: vec![
            participant("focal", ValueNetRole::Focal),
            participant("comp_a", ValueNetRole::Substitutor),
        ],
        strategies,
        payoffs: PayoffForm::Normal {
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

fn persona(id: &str) -> Persona {
    Persona {
        participant_id: id.to_string(),
        display_name: format!("{id} 的 persona"),
        decision_style: "激进".into(),
        role_card: format!("你是 {id}，只代表其立场行事。"),
        tool_allowlist: vec!["game_solve".into()],
    }
}

fn personas() -> PersonaSet {
    PersonaSet::new(vec![persona("focal"), persona("comp_a")])
}

/// 规则驱动的 persona 实现（测试用，不依赖 LLM）。
struct FixedActor {
    /// 固定给出矩阵内行动。
    strategy: String,
}

impl PersonaActor for FixedActor {
    fn act(
        &self,
        _spec: &GameSpec,
        participant_id: &str,
        _history: &[Move],
        _perturbation: f64,
    ) -> Option<Move> {
        Some(Move::new(
            participant_id,
            Action::InMatrix {
                strategy: self.strategy.clone(),
            },
            "固定策略（测试用）",
        ))
    }
}

/// 给出矩阵外提议的 persona，可配置是否带依据。
struct NovelActor {
    with_basis: bool,
}

impl PersonaActor for NovelActor {
    fn act(
        &self,
        _spec: &GameSpec,
        participant_id: &str,
        history: &[Move],
        _perturbation: f64,
    ) -> Option<Move> {
        // 只提议一次（避免无限重复）
        if !history.is_empty() {
            return None;
        }
        let (legal, feas) = if self.with_basis {
            (
                vec!["民法典投诉权".to_string()],
                vec!["公开投诉渠道".to_string()],
            )
        } else {
            (vec![], vec![])
        };
        Some(Move::new(
            participant_id,
            Action::Novel {
                description: "向对手客户投诉".into(),
                introduces: vec!["major_customer".into()],
                legality_basis: legal,
                feasibility_basis: feas,
                expected_benefit: (10.0, 20.0),
                expected_cost: (1.0, 2.0),
            },
            "弱势方反击（测试用）",
        ))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// fail loud：能力预检（§5.7 的承重设计）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn missing_capability_returns_typed_error_not_silent_degradation() {
    // 这是本 crate 最重要的断言。若"对抗性监管者"persona 因后端不支持
    // 而被静默忽略、退化成"乐于助人的助手"，仿真结论就是**错的**——
    // 而这种错误在报告里看不出来。
    let caps = CapabilitySet {
        persona: false, // 不支持 persona 覆盖
        ..CapabilitySet::full()
    };
    let err = caps
        .require("persona", "weak-provider")
        .expect_err("缺 persona 能力必须报错，绝不静默降级");
    assert!(
        matches!(err, SwarmError::CapabilityMissing { ref provider, ref capability }
            if provider == "weak-provider" && capability == "persona"),
        "实得 {err:?}"
    );
}

#[test]
fn full_capabilities_pass_all_checks() {
    let caps = CapabilitySet::full();
    for c in ["persona", "tool_filter", "output_schema", "depth_limit", "continuable"] {
        assert!(caps.require(c, "full-provider").is_ok(), "{c} 应通过");
    }
    assert!(caps.missing().is_empty());
}

#[test]
fn missing_lists_all_absent_capabilities() {
    let caps = CapabilitySet {
        persona: false,
        output_schema: false,
        ..CapabilitySet::full()
    };
    let m = caps.missing();
    assert_eq!(m.len(), 2, "应列出两项缺失：{m:?}");
    assert!(m.contains(&"persona") && m.contains(&"output_schema"));
}

#[test]
fn required_capabilities_check_rejects_incomplete_provider() {
    let req = RequiredCapabilities::all();
    let caps = CapabilitySet {
        tool_filter: false,
        ..CapabilitySet::full()
    };
    let err = req
        .check(&caps, "p")
        .expect_err("所需能力缺失必须报错");
    assert!(
        matches!(err, SwarmError::CapabilityMissing { ref capability, .. } if capability == "tool_filter"),
        "实得 {err:?}"
    );
}

#[test]
fn unknown_capability_name_is_rejected() {
    // 拼错的能力名不得被当作"具备"——否则会绕过预检
    let caps = CapabilitySet::full();
    assert!(caps.require("nonexistent_capability", "p").is_err());
}

#[test]
fn empty_capability_set_requires_nothing() {
    let caps = CapabilitySet::default();
    assert_eq!(caps.missing().len(), 5);
    assert!(RequiredCapabilities::default().check(&caps, "p").is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// 深度预算持久化（§5.7："否则恢复后预算被重置"）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn depth_budget_is_enforced() {
    let mut ps = personas();
    ps.max_depth = 2;
    ps.delegation_depth = 3;
    let err = ps.check_depth().expect_err("超限必须报错");
    assert!(matches!(err, SwarmError::DepthExceeded { current: 3, limit: 2 }));
}

#[test]
fn depth_budget_survives_serialization() {
    // §5.7 原文：仅存运行时的深度在崩溃恢复后会被重置，
    // 导致重组出与崩溃前不同的 persona。故深度必须在 PersonaSet 内持久化。
    let mut ps = personas();
    ps.delegation_depth = 2;
    ps.max_depth = 3;
    let json = serde_json::to_string(&ps).unwrap();
    let back: PersonaSet = serde_json::from_str(&json).unwrap();
    assert_eq!(back.delegation_depth, 2, "深度必须随序列化保留");
    assert_eq!(back.max_depth, 3);
}

#[test]
fn swarm_descriptor_round_trips_with_personas_and_seed() {
    // 恢复后须能重建**同一批** persona 与同一种子 ⇒ 同分布
    let d = SwarmDescriptor::new("provider-x", personas(), 16, 20260918);
    let json = serde_json::to_string(&d).unwrap();
    let back: SwarmDescriptor = serde_json::from_str(&json).unwrap();
    assert_eq!(d, back);
    assert_eq!(back.personas.personas.len(), 2);
    assert_eq!(back.seed, 20260918);
    assert!(back.is_compatible());
}

#[test]
fn descriptor_version_mismatch_detected() {
    let mut d = SwarmDescriptor::new("p", personas(), 1, 1);
    d.version = 999;
    assert!(!d.is_compatible(), "版本不符须能被检出");
}

// ─────────────────────────────────────────────────────────────────────────────
// rollout 与终止条件
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn rollout_terminates_on_stability() {
    // 固定 actor 每回合给同一行动 ⇒ 应在 stable_rounds 后终止
    let cfg = OrchestratorConfig {
        max_rounds: 100,
        perturb: false,
        stable_rounds: 3,
    };
    let t = rollout(&spec(), &personas(), &FixedActor { strategy: "维持价".into() }, &cfg, 1).unwrap();
    assert_eq!(
        t.stop_reason,
        shangyi_swarm::StopReason::Stable,
        "固定行动应在稳定后终止，而非跑满回合"
    );
    assert!(t.moves.len() < 100 * 2, "不应跑满上限");
}

#[test]
fn rollout_terminates_on_max_rounds_when_never_stable() {
    // actor 每次给不同行动 ⇒ 永不稳定 ⇒ 由回合上限终止
    struct Alternating {
        round: std::cell::Cell<usize>,
    }
    impl PersonaActor for Alternating {
        fn act(&self, _: &GameSpec, id: &str, _: &[Move], _: f64) -> Option<Move> {
            let r = self.round.get();
            self.round.set(r + 1);
            Some(Move::new(
                id,
                Action::InMatrix {
                    strategy: format!("s{r}"),
                },
                "交替",
            ))
        }
    }
    let cfg = OrchestratorConfig {
        max_rounds: 5,
        perturb: false,
        stable_rounds: 3,
    };
    let a = Alternating {
        round: std::cell::Cell::new(0),
    };
    let t = rollout(&spec(), &personas(), &a, &cfg, 1).unwrap();
    assert_eq!(t.stop_reason, shangyi_swarm::StopReason::MaxRounds);
}

#[test]
fn rollout_stops_when_actor_returns_none() {
    struct OnceOnly;
    impl PersonaActor for OnceOnly {
        fn act(&self, _: &GameSpec, id: &str, history: &[Move], _: f64) -> Option<Move> {
            if history.is_empty() {
                Some(Move::new(id, Action::InMatrix { strategy: "x".into() }, "一次"))
            } else {
                None
            }
        }
    }
    let cfg = OrchestratorConfig::default();
    let t = rollout(&spec(), &personas(), &OnceOnly, &cfg, 1).unwrap();
    assert_eq!(t.stop_reason, shangyi_swarm::StopReason::Stable);
    assert_eq!(t.moves.len(), 2, "两位 persona 各提议一次");
}

#[test]
fn rollout_is_deterministic_for_same_seed() {
    let cfg = OrchestratorConfig {
        perturb: true,
        ..Default::default()
    };
    let t1 = rollout(&spec(), &personas(), &FixedActor { strategy: "a".into() }, &cfg, 42).unwrap();
    let t2 = rollout(&spec(), &personas(), &FixedActor { strategy: "a".into() }, &cfg, 42).unwrap();
    assert_eq!(t1, t2, "同种子必须给同轨迹");
}

#[test]
fn rollout_rejects_empty_personas() {
    let empty = PersonaSet::new(vec![]);
    assert!(matches!(
        rollout(&spec(), &empty, &FixedActor { strategy: "a".into() }, &OrchestratorConfig::default(), 1),
        Err(SwarmError::EmptyPersonas)
    ));
}

#[test]
fn rollout_rejects_depth_over_budget() {
    let mut ps = personas();
    ps.max_depth = 1;
    ps.delegation_depth = 5;
    let err = rollout(
        &spec(),
        &ps,
        &FixedActor { strategy: "a".into() },
        &OrchestratorConfig::default(),
        1,
    )
    .expect_err("深度超限必须报错");
    assert!(matches!(err, SwarmError::DepthExceeded { .. }));
}

#[test]
fn rollout_n_rejects_zero() {
    let err = rollout_n(
        &spec(),
        &personas(),
        &FixedActor { strategy: "a".into() },
        &OrchestratorConfig::default(),
        0,
        1,
    )
    .expect_err("零次 rollout 必须报错");
    assert_eq!(err, SwarmError::ZeroRollouts);
}

#[test]
fn rollout_n_uses_distinct_seeds() {
    // 每次 rollout 用不同种子（确定性派生），使扰动多样
    let cfg = OrchestratorConfig {
        perturb: true,
        ..Default::default()
    };
    let ts = rollout_n(&spec(), &personas(), &FixedActor { strategy: "a".into() }, &cfg, 4, 100).unwrap();
    let seeds: Vec<u64> = ts.iter().map(|t| t.seed).collect();
    assert_eq!(seeds, vec![100, 101, 102, 103], "种子应依次派生");
}

#[test]
fn rollout_n_is_reproducible() {
    let cfg = OrchestratorConfig::default();
    let a = rollout_n(&spec(), &personas(), &FixedActor { strategy: "a".into() }, &cfg, 3, 7).unwrap();
    let b = rollout_n(&spec(), &personas(), &FixedActor { strategy: "a".into() }, &cfg, 3, 7).unwrap();
    assert_eq!(a, b, "同 (rollouts, seed) 必须给同一批轨迹");
}

// ─────────────────────────────────────────────────────────────────────────────
// actor 校验：拦住 persona 幻觉
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn unknown_actor_is_rejected_not_silently_dropped() {
    // 若静默丢弃，分布会失真（分母含被丢的 move 而分子不含）
    let s = spec();
    let bad = vec![Move::new(
        "hallucinated_actor",
        Action::InMatrix { strategy: "x".into() },
        "幻觉出的参与者",
    )];
    let err = validate_actors(&s, &bad).expect_err("未知 actor 必须报错");
    assert!(
        matches!(err, SwarmError::UnknownActor(ref a) if a == "hallucinated_actor"),
        "实得 {err:?}"
    );
}

#[test]
fn known_actors_validate() {
    let s = spec();
    let ok = vec![
        Move::new("focal", Action::InMatrix { strategy: "维持价".into() }, "r"),
        Move::new("comp_a", Action::InMatrix { strategy: "跟随".into() }, "r"),
    ];
    assert!(validate_actors(&s, &ok).is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// 提议 ≠ 结论：引擎评估是唯一通道（§5.7 的铁律）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn novel_move_without_basis_is_rejected_by_engine() {
    // persona 提议了矩阵外行动但**没带依据** ⇒ 引擎过滤必须拒绝
    let s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &NovelActor { with_basis: false }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();

    let novel: Vec<_> = dist.novel_actions();
    assert!(!novel.is_empty(), "应有矩阵外候选被记录（如实返回而非丢弃）");
    for e in &novel {
        assert!(
            !e.engine_accepted,
            "无依据的矩阵外候选不得被引擎接受：{e:?}"
        );
        assert!(!e.rejection.is_empty(), "须给出拒绝原因：{e:?}");
    }
}

#[test]
fn novel_move_with_basis_is_accepted_by_engine() {
    // 带完整依据 ⇒ 引擎接受
    let s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &NovelActor { with_basis: true }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();

    let novel: Vec<_> = dist.novel_actions();
    assert!(!novel.is_empty());
    let accepted = novel.iter().filter(|e| e.engine_accepted).count();
    assert!(
        accepted > 0,
        "带完整依据的矩阵外候选应被引擎接受：{novel:?}"
    );
}

#[test]
fn in_matrix_moves_are_accepted_without_filtering() {
    // 矩阵内行动不需过三重过滤（过滤只针对矩阵外）
    let s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &FixedActor { strategy: "维持价".into() }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();
    assert!(
        dist.entries.iter().all(|e| !e.is_novel && e.engine_accepted),
        "矩阵内行动应全部被接受：{:?}",
        dist.entries
    );
}

#[test]
fn distribution_probabilities_sum_to_one() {
    let s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &FixedActor { strategy: "a".into() }, &cfg, 3, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();
    let sum: f64 = dist.entries.iter().map(|e| e.probability).sum();
    assert!((sum - 1.0).abs() < 1e-9, "概率和应为 1，实得 {sum}");
}

#[test]
fn evaluate_rejects_empty_trajectories() {
    let s = spec();
    assert!(matches!(
        evaluate_candidates(&s, &[], &TripleFilter::default()),
        Err(SwarmError::ZeroRollouts)
    ));
}

#[test]
fn distribution_output_is_deterministic() {
    let s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &FixedActor { strategy: "a".into() }, &cfg, 3, 1).unwrap();
    let d1 = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();
    let d2 = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();
    assert_eq!(d1, d2, "同轨迹必须给同分布");
}

#[test]
fn for_actor_filters_and_sorts() {
    let s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &FixedActor { strategy: "a".into() }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();
    let focal = dist.for_actor("focal");
    assert!(!focal.is_empty());
    assert!(focal.iter().all(|e| e.actor == "focal"));
    // 概率和应约等于 focal 的份额（此处两位 persona 均等 ⇒ 0.5）
    assert!(
        (dist.probability_sum_for("focal") - 0.5).abs() < 1e-9,
        "实得 {}",
        dist.probability_sum_for("focal")
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 注册回博弈（§5.7.1 步骤 3）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn accepted_novel_moves_are_registered_back_to_gamespec() {
    let mut s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &NovelActor { with_basis: true }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();

    assert!(s.external_actors.is_empty(), "前置：初始无环境方");
    let n = register_accepted_novel_moves(&mut s, &dist, &ts, &TripleFilter::default());
    assert!(n > 0, "应有候选被注册");
    assert!(
        !s.external_actors.is_empty(),
        "被引入的新参与者应进 external_actors——这是「改变棋盘」的机制"
    );
    assert!(
        s.external_actors
            .iter()
            .any(|a| a.relation_to_game == RelationToGame::ExternalActor),
        "被引入者的定位必须为环境方"
    );
}

#[test]
fn rejected_novel_moves_are_not_registered() {
    // 只接受 engine_accepted 的候选——未过过滤者不得进博弈
    let mut s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &NovelActor { with_basis: false }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();

    let n = register_accepted_novel_moves(&mut s, &dist, &ts, &TripleFilter::default());
    assert_eq!(n, 0, "无依据的候选不应被注册");
    assert!(
        s.external_actors.is_empty(),
        "被拒候选不得引入新参与者——否则博弈结构被污染"
    );
}

#[test]
fn registering_twice_does_not_duplicate_actors() {
    let mut s = spec();
    let cfg = OrchestratorConfig::default();
    let ts = rollout_n(&s, &personas(), &NovelActor { with_basis: true }, &cfg, 2, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();

    register_accepted_novel_moves(&mut s, &dist, &ts, &TripleFilter::default());
    let after_first = s.external_actors.len();
    register_accepted_novel_moves(&mut s, &dist, &ts, &TripleFilter::default());
    assert_eq!(
        s.external_actors.len(),
        after_first,
        "重复注册不得产生重复参与者"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 多样性采样
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn perturbation_is_bounded_and_symmetric() {
    let mut rng = shangyi_swarm::orchestrator::SwarmRng::new(12345);
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for _ in 0..10_000 {
        let p = rng.next_perturbation();
        assert!((-1.0..1.0).contains(&p), "扰动须在 [-1,1)，实得 {p}");
        lo = lo.min(p);
        hi = hi.max(p);
    }
    // 大样本下应覆盖两侧
    assert!(lo < -0.9, "应能取到接近 -1 的值，实得 {lo}");
    assert!(hi > 0.9, "应能取到接近 1 的值，实得 {hi}");
}

#[test]
fn swarm_rng_handles_zero_seed() {
    // 0 是 LCG 吸收态，必须被替换
    let mut r = shangyi_swarm::orchestrator::SwarmRng::new(0);
    let a = r.next_u64();
    let b = r.next_u64();
    assert_ne!(a, 0);
    assert_ne!(a, b, "零种子不应致序列退化");
}

#[test]
fn perturbed_rollouts_differ_from_unperturbed_when_actor_uses_perturbation() {
    // actor 依扰动改变行动 ⇒ 开启扰动应产生多种行动
    struct PerturbSensitive;
    impl PersonaActor for PerturbSensitive {
        fn act(&self, _: &GameSpec, id: &str, _: &[Move], perturbation: f64) -> Option<Move> {
            Some(Move::new(
                id,
                Action::InMatrix {
                    strategy: if perturbation > 0.0 {
                        "激进".into()
                    } else {
                        "保守".into()
                    },
                },
                "依扰动行动",
            ))
        }
    }
    let s = spec();
    let cfg = OrchestratorConfig {
        perturb: true,
        max_rounds: 1,
        stable_rounds: 1,
    };
    let ts = rollout_n(&s, &personas(), &PerturbSensitive, &cfg, 8, 1).unwrap();
    let dist = evaluate_candidates(&s, &ts, &TripleFilter::default()).unwrap();
    let strategies: std::collections::BTreeSet<&str> =
        dist.entries.iter().map(|e| e.action.as_str()).collect();
    assert!(
        strategies.len() > 1,
        "扰动采样应产生多种行动，实得 {strategies:?}"
    );
}

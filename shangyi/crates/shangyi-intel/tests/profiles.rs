//! `shangyi-intel` 的测试。
//!
//! 核心验证三件事：
//! 1. **合规红线是类型级的**：个人信息与未确认来源在构造时即被拒
//! 2. **画像角色一致性**：差异化字段块必须与 `value_net_role` 匹配
//! 3. **贝叶斯推断的正确性与诚实性**：后验归一化、与观测顺序无关、
//!    "类型假设集不完备"必须被标记而非静默吞掉

use std::collections::BTreeMap;

use shangyi_intel::fetch::FetchError;

use shangyi_gamespec::{RelationToGame, ValueNetRole};
use shangyi_intel::{
    BehaviorModel, BusinessSource, DataSource, EventType, IntelError, ParticipantProfile,
    RoleSpecific, SourceKind, TimelineEvent, apply_inference, decision_style_summary, infer_types};

fn ev(date: &str, ty: EventType, mag: f64, conf: f64) -> TimelineEvent {
    TimelineEvent::new(
        date,
        ty,
        mag,
        DataSource::business(BusinessSource::Platform, "https://example.com/p", 1_700_000_000_000),
        conf,
    )
    .expect("合法观测")
}

fn model(name: &str, weights: &[(EventType, f64)]) -> BehaviorModel {
    BehaviorModel::new(name, weights.iter().copied().collect())
}

// ─────────────────────────────────────────────────────────────────────────────
// 合规红线（§6.1）—— 类型级强制
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn personal_data_source_is_rejected_at_construction() {
    // §6.1 红线："不采集个人信息"。若只写在文档里，赶工期时最容易被绕过，
    // 而"已采集"是不可逆事故（数据已落盘）。故在构造层面拒绝。
    let err = DataSource::new(SourceKind::Personal, "某处", 1)
        .expect_err("个人信息来源必须被拒绝");
    assert_eq!(err, IntelError::PersonalDataForbidden);
}

#[test]
fn unverified_source_is_rejected() {
    let err = DataSource::new(SourceKind::Unverified, "某处", 1)
        .expect_err("来源不明必须被拒绝");
    assert_eq!(err, IntelError::UnverifiedSource);
}

#[test]
fn business_source_is_accepted() {
    let ok = DataSource::new(SourceKind::Business, "https://example.com", 1).unwrap();
    assert_eq!(ok.kind, SourceKind::Business);
    assert_eq!(ok.locator, "https://example.com");
}

#[test]
fn all_documented_business_sources_are_representable() {
    // §6.1 清单的 6 类公开数据源都应可表达
    let sources = [
        (BusinessSource::Platform, "价格竞争核心数据"),
        (BusinessSource::Registry, "进入/扩张信号"),
        (BusinessSource::JobPosting, "竞对类型推断的富矿"),
        (BusinessSource::Filing, "payoff 估计"),
        (BusinessSource::Report, "市场规模与事件"),
        (BusinessSource::Poi, "空间竞争"),
    ];
    for (s, want_use) in sources {
        assert_eq!(s.primary_use(), want_use, "{s:?}");
        let ds = DataSource::business(s, "loc", 1);
        assert_eq!(ds.kind, SourceKind::Business);
        assert_eq!(ds.business, Some(s));
    }
}

#[test]
fn every_timeline_event_carries_a_source() {
    // 无来源的观测无法审计——时间线条目必须带 source（类型上强制）
    let e = ev("2026-01-01", EventType::PriceCut, 0.15, 0.9);
    assert!(e.source.locator.contains("example.com"));
}

#[test]
fn negative_or_nonfinite_magnitude_rejected() {
    let src = DataSource::business(BusinessSource::Platform, "l", 1);
    assert!(matches!(
        TimelineEvent::new("d", EventType::PriceCut, -0.1, src.clone(), 0.9),
        Err(IntelError::InvalidMagnitude(_))
    ));
    assert!(matches!(
        TimelineEvent::new("d", EventType::PriceCut, f64::NAN, src, 0.9),
        Err(IntelError::InvalidMagnitude(_))
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// 画像角色一致性（§6.2）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn role_specific_block_must_match_declared_role() {
    // 声明是供应商，却填了替代者的字段块 ⇒ 下游会按错误角色口径解读数字
    let mut p = ParticipantProfile::new(
        "supplier_x",
        ValueNetRole::Supplier,
        RelationToGame::Player,
    );
    p.role_specific = Some(RoleSpecific::Substitutor {
        lambda_qre: Some(1.0),
        level_k: Some(1),
        aggressiveness: Some(0.5),
        cost_mu: Some(50.0),
        cost_sigma: Some(5.0),
    });
    let err = p.validate().expect_err("角色不匹配必须报错");
    assert!(matches!(err, IntelError::MissingRoleSpecific(ValueNetRole::Supplier)));
}

#[test]
fn missing_role_specific_block_is_accepted() {
    // ⚠️ **语义已变更**（本次修正）。
    //
    // 早先断言"缺字段块必须报错"。而字段类型是 `Option`，且本项目反复确立
    // 「未取得」必须有合法表达形式（见 `RoleSpecific::Substitutor` 那次修复：
    // 模型如实写"未取得"却被类型拒绝，只能编数）。
    //
    // SME 场景里"对手的攻击性参数"经常拿不到，而**拿不到就不该编**。
    // 故：缺失 ⇒ 通过（下游按"无该角色参数"处理）；
    // **给了就必须与声明的角色一致**——那才是本条要防的错。
    let p = ParticipantProfile::new("x", ValueNetRole::Customer, RelationToGame::Player);
    assert!(
        p.validate().is_ok(),
        "缺字段块是「未取得」，不是错误——那正是 Option 的含义"
    );
}

/// **给了字段块就必须与声明的角色一致**——这才是校验要防的错。
#[test]
fn mismatched_role_specific_block_is_still_rejected() {
    let mut p = ParticipantProfile::new("s", ValueNetRole::Supplier, RelationToGame::Player);
    // 声明供应商，却填替代者的字段块 ⇒ 下游会按错误角色口径解读数字
    p.role_specific = Some(RoleSpecific::Substitutor {
        lambda_qre: Some(1.0),
        level_k: Some(1),
        aggressiveness: Some(0.5),
        cost_mu: Some(50.0),
        cost_sigma: Some(5.0),
    });
    assert!(
        matches!(
            p.validate(),
            Err(IntelError::MissingRoleSpecific(ValueNetRole::Supplier))
        ),
        "角色不匹配必须被拒——那会让下游按错误口径解读"
    );
}

#[test]
fn matching_role_block_validates() {
    let mut p = ParticipantProfile::new("s", ValueNetRole::Supplier, RelationToGame::Player);
    p.role_specific = Some(RoleSpecific::Supplier {
        supply_concentration: 0.7,
        switching_cost: shangyi_intel::Level::High,
        our_batna: "替代供应商2家（认证周期3个月）".into(),
        holdup_risk: shangyi_intel::Level::Medium,
    });
    assert!(p.validate().is_ok());
}

#[test]
fn supplier_concentration_out_of_range_rejected() {
    let bad = RoleSpecific::Supplier {
        supply_concentration: 1.5,
        switching_cost: shangyi_intel::Level::High,
        our_batna: "x".into(),
        holdup_risk: shangyi_intel::Level::Low,
    };
    assert!(bad.validate().is_err());
}

#[test]
fn customer_wtp_range_order_enforced() {
    // wtp_low > wtp_high 说明数据录入有误，不得静默接受
    let bad = RoleSpecific::Customer {
        segment: "价格敏感型".into(),
        wtp_low: 120.0,
        wtp_high: 80.0,
        elasticity: -1.8,
        concentration: "前3大客户占60%".into(),
    };
    assert!(bad.validate().is_err());

    let ok = RoleSpecific::Customer {
        segment: "价格敏感型".into(),
        wtp_low: 80.0,
        wtp_high: 120.0,
        elasticity: -1.8,
        concentration: "前3大客户占60%".into(),
    };
    assert!(ok.validate().is_ok());
}

#[test]
fn substitutor_parameter_ranges_enforced() {
    let bad_aggr = RoleSpecific::Substitutor {
        lambda_qre: Some(1.2),
        level_k: Some(1),
        aggressiveness: Some(1.5), // 越界
        cost_mu: Some(55.0),
        cost_sigma: Some(6.0),
    };
    assert!(bad_aggr.validate().is_err());

    let bad_sigma = RoleSpecific::Substitutor {
        lambda_qre: Some(1.2),
        level_k: Some(1),
        aggressiveness: Some(0.8),
        cost_mu: Some(55.0),
        cost_sigma: Some(-1.0), // 标准差不可为负
    };
    assert!(bad_sigma.validate().is_err());
}

#[test]
fn every_role_has_a_distinct_rationale() {
    // 四种角色的决策风格摘要必须各不相同——否则"差异化"是假的
    let cases = vec![
        RoleSpecific::Supplier {
            supply_concentration: 0.7,
            switching_cost: shangyi_intel::Level::High,
            our_batna: "x".into(),
            holdup_risk: shangyi_intel::Level::Medium,
        },
        RoleSpecific::Customer {
            segment: "价格敏感型".into(),
            wtp_low: 80.0,
            wtp_high: 120.0,
            elasticity: -1.8,
            concentration: "c".into(),
        },
        RoleSpecific::Substitutor {
            lambda_qre: Some(1.2),
            level_k: Some(1),
            aggressiveness: Some(0.8),
            cost_mu: Some(55.0),
            cost_sigma: Some(6.0),
        },
        RoleSpecific::Complementor {
            coupling: 0.6,
            synergy_note: "联合会员".into(),
        },
    ];
    let summaries: Vec<String> = cases.iter().map(decision_style_summary).collect();
    let unique: std::collections::BTreeSet<&String> = summaries.iter().collect();
    assert_eq!(unique.len(), 4, "四种角色摘要应互不相同：{summaries:?}");
    for s in &summaries {
        assert!(!s.is_empty());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 贝叶斯类型推断（§5.1 竞对研判的核心）
// ─────────────────────────────────────────────────────────────────────────────

fn priors(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
}

/// 构造两个对立的行为模型：
/// - `dumping`（清库存型）：激进降价、频繁促销
/// - `strategic`（长期战略型）：稳健、扩张与招聘
fn two_models() -> Vec<BehaviorModel> {
    vec![
        model(
            "dumping",
            &[(EventType::PriceCut, 1.0), (EventType::Promotion, 0.8)],
        ),
        model(
            "strategic",
            &[(EventType::Hiring, 1.0), (EventType::NewStore, 0.9)],
        ),
    ]
}

#[test]
fn aggressive_price_cuts_shift_posterior_toward_dumping() {
    // 观测到多次大幅降价 ⇒ 后验应向 dumping 偏移
    let obs = vec![
        ev("2026-01-01", EventType::PriceCut, 0.20, 0.9),
        ev("2026-02-01", EventType::PriceCut, 0.15, 0.9),
        ev("2026-03-01", EventType::Promotion, 0.30, 0.8),
    ];
    let inf = infer_types(&obs, &two_models(), &priors(&[("dumping", 0.5), ("strategic", 0.5)]))
        .unwrap();
    assert!(
        inf.posterior["dumping"] > 0.95,
        "多次降价后 dumping 后验应显著上升：{:?}",
        inf.posterior
    );
    assert!(inf.posterior_normalized_ok());
}

#[test]
fn hiring_and_expansion_shift_posterior_toward_strategic() {
    let obs = vec![
        ev("2026-01-01", EventType::Hiring, 0.5, 0.9),
        ev("2026-02-01", EventType::NewStore, 0.4, 0.9),
    ];
    let inf = infer_types(&obs, &two_models(), &priors(&[("dumping", 0.5), ("strategic", 0.5)]))
        .unwrap();
    assert!(
        inf.posterior["strategic"] > 0.95,
        "扩张信号后 strategic 后验应显著上升：{:?}",
        inf.posterior
    );
}

#[test]
fn posterior_always_normalized() {
    let obs = vec![ev("2026-01-01", EventType::PriceCut, 0.1, 0.5)];
    let inf = infer_types(&obs, &two_models(), &priors(&[("dumping", 0.3), ("strategic", 0.7)]))
        .unwrap();
    let s: f64 = inf.posterior.values().sum();
    assert!((s - 1.0).abs() < 1e-9, "后验须归一化，实得 {s}");
}

#[test]
fn inference_is_independent_of_observation_order() {
    // 输入顺序不同但日期相同 ⇒ 结果必须一致（按日期排序处理）
    let a = vec![
        ev("2026-01-01", EventType::PriceCut, 0.2, 0.9),
        ev("2026-02-01", EventType::Hiring, 0.3, 0.9),
    ];
    let b = vec![
        ev("2026-02-01", EventType::Hiring, 0.3, 0.9),
        ev("2026-01-01", EventType::PriceCut, 0.2, 0.9),
    ];
    let inf_a = infer_types(&a, &two_models(), &priors(&[("dumping", 0.5), ("strategic", 0.5)])).unwrap();
    let inf_b = infer_types(&b, &two_models(), &priors(&[("dumping", 0.5), ("strategic", 0.5)])).unwrap();
    assert_eq!(inf_a.posterior, inf_b.posterior, "结果不应依赖输入顺序");
}

#[test]
fn confidence_scales_observation_influence() {
    // ⚠️ 测试设计要点：两个类型都必须对该事件有**非零**似然，否则观测置信度
    // 会被归一化约掉——"该类型根本不会做这事"（似然 0）比"置信度低"是更强的信息，
    // conf 的差异被前者淹没。（我第一版测试就踩了这个坑。）
    //
    // 故构造两个类型都对 PriceCut 有非零权重、但倾向强度不同：
    //   dumping:   PriceCut w=1.0（强烈倾向）
    //   strategic: PriceCut w=0.3（偶尔为之）
    let models = vec![
        model("dumping", &[(EventType::PriceCut, 1.0)]),
        model("strategic", &[(EventType::PriceCut, 0.3)]),
    ];
    let p = priors(&[("dumping", 0.5), ("strategic", 0.5)]);

    // 低置信度观测对后验的推动应更小（更接近先验 0.5）
    let high = vec![ev("2026-01-01", EventType::PriceCut, 0.2, 1.0)];
    let low = vec![ev("2026-01-01", EventType::PriceCut, 0.2, 0.05)];

    let inf_high = infer_types(&high, &models, &p).unwrap();
    let inf_low = infer_types(&low, &models, &p).unwrap();

    assert!(
        inf_high.posterior["dumping"] > inf_low.posterior["dumping"],
        "高置信观测应更强地推动后验：high={:?} low={:?}",
        inf_high.posterior,
        inf_low.posterior
    );
    assert!(
        inf_low.posterior["dumping"] > 0.5,
        "低置信观测仍应略偏向 dumping，但幅度小：{:?}",
        inf_low.posterior
    );
}

#[test]
fn zero_observations_leave_prior_unchanged() {
    let p = priors(&[("dumping", 0.3), ("strategic", 0.7)]);
    let inf = infer_types(&[], &two_models(), &p).unwrap();
    assert_eq!(inf.posterior, p, "无观测时后验应等于先验");
    assert_eq!(inf.observations, 0);
    assert!(!inf.model_incomplete);
}

#[test]
fn incompatible_observation_flags_incomplete_model_set() {
    // 观测到两个模型都不支持的事件类型（如 SupplyChange）
    // ⇒ 类型假设集不完备。必须**标记**而非静默吞掉。
    let obs = vec![ev("2026-01-01", EventType::SupplyChange, 0.5, 0.9)];
    let inf = infer_types(&obs, &two_models(), &priors(&[("dumping", 0.5), ("strategic", 0.5)]))
        .unwrap();
    assert!(
        inf.model_incomplete,
        "观测与全部类型不相容时必须标记 model_incomplete"
    );
    // 后验保持不变（未静默改动）
    assert_eq!(inf.posterior, priors(&[("dumping", 0.5), ("strategic", 0.5)]));
}

#[test]
fn invalid_priors_rejected() {
    // 先验和不为 1
    assert!(matches!(
        infer_types(&[], &two_models(), &priors(&[("dumping", 0.3), ("strategic", 0.3)])),
        Err(IntelError::InvalidPriors(_))
    ));
    // 先验含 0 或负值
    assert!(matches!(
        infer_types(&[], &two_models(), &priors(&[("dumping", 0.0), ("strategic", 1.0)])),
        Err(IntelError::InvalidPriors(_))
    ));
    // 空先验
    assert!(matches!(
        infer_types(&[], &two_models(), &BTreeMap::new()),
        Err(IntelError::InvalidPriors(_))
    ));
}

#[test]
fn missing_likelihood_model_rejected() {
    // 先验里有某类型但没有对应行为模型 ⇒ 报错，不得静默忽略该类型
    let p = priors(&[("dumping", 0.5), ("unknown_type", 0.5)]);
    let err = infer_types(&[], &two_models(), &p).expect_err("缺似然模型必须报错");
    assert!(matches!(err, IntelError::MissingLikelihood(ref t) if t == "unknown_type"));
}

#[test]
fn apply_inference_updates_profile_and_timestamp() {
    let mut prof = ParticipantProfile::new("comp_a", ValueNetRole::Substitutor, RelationToGame::Player);
    let obs = vec![ev("2026-01-01", EventType::PriceCut, 0.2, 0.9)];
    let inf = infer_types(&obs, &two_models(), &priors(&[("dumping", 0.5), ("strategic", 0.5)])).unwrap();
    apply_inference(&mut prof, &inf, 1_700_000_000_000);
    assert_eq!(prof.posterior_types, inf.posterior);
    assert_eq!(prof.updated_at_ms, 1_700_000_000_000);
    assert!(prof.posterior_normalized());
}

#[test]
fn map_estimate_picks_highest_posterior() {
    let mut prof = ParticipantProfile::new("x", ValueNetRole::Substitutor, RelationToGame::Player);
    prof.posterior_types = priors(&[("dumping", 0.35), ("strategic", 0.65)]);
    let (t, p) = prof.most_likely_type().unwrap();
    assert_eq!(t, "strategic");
    assert!((p - 0.65).abs() < 1e-12);
}

#[test]
fn map_estimate_is_deterministic_on_ties() {
    // 并列时按字典序取小者——否则同一输入两次调用可能给不同答案
    let mut prof = ParticipantProfile::new("x", ValueNetRole::Substitutor, RelationToGame::Player);
    prof.posterior_types = priors(&[("b_type", 0.5), ("a_type", 0.5)]);
    let (t, _) = prof.most_likely_type().unwrap();
    assert_eq!(t, "a_type", "并列时应取字典序小者以保证确定性");
}

#[test]
fn magnitude_sensitivity_amplifies_effect() {
    // 同上：两个类型都须对该事件有非零似然，否则幅度影响被约掉。
    // 构造"对幅度敏感度不同"的两个类型：
    //   dumping:   sensitivity = 2.0（大幅降价很符合其风格）
    //   strategic: sensitivity = 0.1（对幅度不敏感）
    let dumping = BehaviorModel {
        type_name: "dumping".into(),
        event_weights: [(EventType::PriceCut, 1.0)].into_iter().collect(),
        magnitude_sensitivity: 2.0,
    };
    let strategic = BehaviorModel {
        type_name: "strategic".into(),
        event_weights: [(EventType::PriceCut, 1.0)].into_iter().collect(),
        magnitude_sensitivity: 0.1,
    };
    let models = vec![dumping, strategic];
    let p = priors(&[("dumping", 0.5), ("strategic", 0.5)]);

    let small = vec![ev("2026-01-01", EventType::PriceCut, 0.02, 1.0)];
    let large = vec![ev("2026-01-01", EventType::PriceCut, 0.50, 1.0)];

    let inf_small = infer_types(&small, &models, &p).unwrap();
    let inf_large = infer_types(&large, &models, &p).unwrap();

    assert!(
        inf_large.posterior["dumping"] > inf_small.posterior["dumping"],
        "大幅降价应更强推动 dumping 后验：large={:?} small={:?}",
        inf_large.posterior,
        inf_small.posterior
    );
}

#[test]
fn profile_serializes_round_trip() {
    let mut prof = ParticipantProfile::new("supplier_x", ValueNetRole::Supplier, RelationToGame::Player);
    prof.common.interests = vec!["稳定订单量".into(), "缩短账期".into()];
    prof.common.resources = vec!["独家产能".into()];
    prof.role_specific = Some(RoleSpecific::Supplier {
        supply_concentration: 0.7,
        switching_cost: shangyi_intel::Level::High,
        our_batna: "替代供应商2家".into(),
        holdup_risk: shangyi_intel::Level::Medium,
    });
    prof.timeline.push(ev("2026-06-01", EventType::PriceCut, 0.15, 0.9));
    prof.posterior_types = priors(&[("dumping", 0.35), ("strategic", 0.65)]);
    prof.evidence_quality = "B（两条独立来源交叉）".into();

    // ⚠️ 本测试是 `arbitrary_precision` 兼容性的**回归护栏**：
    // 本 crate 的 `[dev-dependencies]` 显式启用了 `serde_json/arbitrary_precision`
    // （R1 前该 feature 由上游 vendor crate 带入全图，摘除后只能这样保住覆盖），
    // 故**无论 `-p shangyi-intel` 还是全量构建，本测试都跑在该 feature 下**。
    // 若 `RoleSpecific` 退回 internally-tagged，本测试会立即失败。
    // 见 shangyi/README.md 的序列化规则章节。
    let json = serde_json::to_string(&prof).unwrap();
    let back: ParticipantProfile =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("反序列化失败（{e}）；JSON={json}"));
    assert_eq!(prof, back);
    assert!(back.validate().is_ok());
}

// ───────────── 竞对参数可以为「未取得」（实测逼出来的设计修正）─────────────

/// **`null` 必须是合法的**——它表达「未取得」，而这在 SME 场景里是常态。
///
/// # 这条测试的来历（一次真实咨询的 7 次连续失败）
///
/// 早先 `Substitutor` 的五个字段是必填 `f64`/`u32`。一次真实运行里模型
/// 连续失败 7 次，错误依次是：
///
/// ```text
/// missing field `lambda_qre`
/// missing field `level_k`
/// missing field `aggressiveness`
/// invalid type: string "未取得", expected f64
/// ```
///
/// （serde 只报**第一个**缺失字段，故模型只能逐个试出来。）
///
/// 最后那条尤其说明问题：模型**如实写了「未取得」**，却被类型拒绝——
/// 于是它要么编一个数（违反铁律 1），要么一直试到能够构造为止。
///
/// 而这些量（对手的 QRE λ、推理层级、成本分布）在中小企业场景里
/// 几乎不可能有来源。**逼模型为无来源的字段编数字，就是让报告长出一批
/// 看起来像证据的假数。**
#[test]
fn substitutor_accepts_null_for_unknown_params() {
    let json = serde_json::json!({
        "participant_id": "rival",
        "value_net_role": "substitutor",
        "relation_to_game": "player",
        "common": {"interests": [], "resources": [],
                   "decision_style": "", "power_asymmetry": ""},
        "role_specific": {
            "role": "substitutor",
            "details": {
                "lambda_qre": null,
                "level_k": null,
                "aggressiveness": null,
                "cost_mu": null,
                "cost_sigma": null
            }
        },
        "updated_at_ms": 0
    });
    let p: ParticipantProfile =
        serde_json::from_value(json).expect("未取得必须是合法输入（实测被拒过）");
    assert!(p.role_specific.is_some(), "角色块应被解析出来");
    p.validate().expect("全 null 应通过校验");
}

/// 提供了值就必须合法——「可以不知道」**不等于**「可以乱填」。
///
/// 两条边界必须同时成立，否则放宽会退化成"什么都收"：
/// - 缺失 ⇒ 允许（未取得）
/// - 给了越界值 ⇒ 拒绝（如攻击性 1.5）
#[test]
fn substitutor_still_validates_provided_values() {
    let mk = |agg: f64| {
        serde_json::json!({
            "participant_id": "rival",
            "value_net_role": "substitutor",
            "relation_to_game": "player",
            "common": {"interests": [], "resources": [],
                       "decision_style": "", "power_asymmetry": ""},
            "role_specific": {
                "role": "substitutor",
                "details": {"aggressiveness": agg, "cost_sigma": 1.0}
            },
            "updated_at_ms": 0
        })
    };
    let ok: ParticipantProfile =
        serde_json::from_value(mk(0.5)).expect("合法值应可解析");
    ok.validate().expect("0.5 在 [0,1] 内，应通过");

    let bad: ParticipantProfile =
        serde_json::from_value(mk(1.5)).expect("解析层不拦越界，校验层拦");
    assert!(
        bad.validate().is_err(),
        "攻击性 1.5 越界，必须被拒——放宽「缺值」不得放宽「错值」"
    );
}

/// 缺值在渲染文本里必须显示「未取得」，**不能显示 0**。
///
/// 用 0 顶替会让读者以为「攻击性为零」——那是一个**关于对手的主张**；
/// 而事实是我们不知道。两者对建模的含义完全相反：前者说对手不会进攻，
/// 后者说我们没有依据。报告会照抄这个字符串，故它必须诚实。
#[test]
fn missing_params_display_as_unknown_not_zero() {
    let rs = RoleSpecific::Substitutor {
        lambda_qre: None,
        level_k: None,
        aggressiveness: None,
        cost_mu: None,
        cost_sigma: None,
    };
    let text = decision_style_summary(&rs);
    assert!(
        text.contains("未取得"),
        "缺值应显示「未取得」，实得：{text}"
    );
    assert!(
        !text.contains("0.00") && !text.contains("k=0"),
        "缺值**不得**显示为 0——那是关于对手的主张，而事实是不知道：{text}"
    );
}

// ───────────── 抓取失败的可重试性（类型层面分类，非文本匹配）─────────────

/// **404 与 429 必须被分成两类**——它们的处置方向相反。
///
/// # 这条测试的来历（run24：同一个 404 URL 被试了 4 次）
///
/// 模型对同一批 `stats.gov.cn` URL 连续重试。它的行为在信息不足时是
/// **理性的**："抓取失败"这句话没有回答它真正的问题——换个时间再试有用吗？
///
/// 两类失败的差别是决定性的：
///
/// | 失败 | 重试 | 正确动作 |
/// |---|---|---|
/// | 404 / 证书过期 / 人机验证 / robots | ❌ | 换数据源 |
/// | 429 / 5xx / 网络超时 | ✅ | 稍后重试 |
///
/// 把"重试无用"说得像"暂时失败"，就是在鼓励浪费 turn（真实成本）。
#[test]
fn fetch_errors_are_classified_by_retryability() {
    let not_retryable = vec![
        FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status: 404,
        },
        FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status: 410,
        },
        FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status: 403,
        },
        FetchError::BotChallenge {
            url: "https://x.invalid/a".into(),
            status: 200,
        },
        FetchError::DisallowedByRobots {
            url: "https://x.invalid/a".into(),
            rule: "Disallow: /a".into(),
        },
        FetchError::InvalidUrl("不是 URL".into()),
        FetchError::InvalidResponse {
            url: "https://api.invalid/search".into(),
            detail: "返回结构不符合接口约定".into(),
        },
    ];
    for e in not_retryable {
        assert!(
            !e.retryable(),
            "`{e}` 重试无用（应引导模型换数据源），但被判为可重试"
        );
    }

    let retryable = vec![
        FetchError::RobotsUnavailable {
            url: "https://x.invalid/a".into(),
            detail: "io: unexpected end of file".into(),
        },
        FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status: 429,
        },
        FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status: 500,
        },
        FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status: 503,
        },
        FetchError::Transport {
            url: "https://x.invalid/a".into(),
            detail: "connection timed out".into(),
        },
    ];
    for e in retryable {
        assert!(
            e.retryable(),
            "`{e}` 是临时性故障（稍后重试可能成功），但被判为不可重试"
        );
    }
}

/// **未知状态码一律判为不可重试**——这个方向的取舍是有意的。
///
/// 若未知的可重试，模型会浪费 turn 去试；若未知的不可重试，最坏情况是
/// "本来能成功但没试"，而模型会换数据源（**仍有进展**）。
/// 两害相权，取后者。
#[test]
fn unknown_status_is_not_retryable_by_default() {
    for status in [400, 401, 418, 451, 599] {
        let e = FetchError::HttpStatus {
            url: "https://x.invalid/a".into(),
            status,
        };
        // 5xx 里的 599 是可重试的（服务端故障）；其余未知客户端错误不可重试
        let expected = (500..600).contains(&status);
        assert_eq!(
            e.retryable(),
            expected,
            "status={status} 的可重试性判定与预期不符"
        );
    }
}

/// **`actionable()` 必须真的可操作**：回答"我该怎么办"，
/// 而不只是重复一遍错误。
///
/// 这条断言的是形态而非文案：不可重试的说明里必须出现"换数据源"
/// 这类**动作指示**；可重试的必须明说"稍后重试"。
/// 若有人把它改成只回一句错误原文，这条测试会失败。
#[test]
fn actionable_text_gives_a_next_step() {
    let hard = FetchError::HttpStatus {
        url: "https://x.invalid/a".into(),
        status: 404,
    };
    let t = hard.actionable();
    assert!(
        t.contains("换数据源") || t.contains("重试无用"),
        "不可重试的失败必须给出「换来源」这类动作指示，实得：{t}"
    );

    let soft = FetchError::HttpStatus {
        url: "https://x.invalid/a".into(),
        status: 429,
    };
    let t2 = soft.actionable();
    assert!(
        t2.contains("重试"),
        "可重试的失败应明说「稍后重试可能成功」，实得：{t2}"
    );

    // robots 被拒的说明必须**不含**绕过暗示——那是站点政策
    let robots = FetchError::DisallowedByRobots {
        url: "https://x.invalid/a".into(),
        rule: "Disallow: /".into(),
    };
    let t3 = robots.actionable();
    assert!(
        t3.contains("不绕过") || t3.contains("政策"),
        "robots 被拒时应说明这是站点政策且不绕过，实得：{t3}"
    );
}

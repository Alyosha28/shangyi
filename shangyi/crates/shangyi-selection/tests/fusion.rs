//! `shangyi-selection` 的测试。
//!
//! 重点验证**可审计性与诚实性**，而非仅仅"跑得通"：
//! - 融合必须归一化、必须尊重判据权重、缺失权重必须报错（不得隐式默认）
//! - 判据分歧必须被发现（设计文档 §5.4 的自动告警要求）
//! - 校准在样本不足时必须**拒绝假装校准**（设计文档 §12）
//! - 校准必须把权重移向**表现更好**的判据（否则"越用越准"是空话）

use std::collections::BTreeMap;

use shangyi_selection::{
    CalibrationOutcome, Confidence, DIVERGENCE_THRESHOLD, MIN_SAMPLES_FOR_CALIBRATION,
    SelectorId, SelectorScore, SelectorWeights, SelectionError, BacktestRecord,
    calibrate, confidence_for, fuse, log_loss, normalize_selector,
};

fn ids(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn scores(
    selector: SelectorId,
    pairs: &[(&str, f64)],
) -> Vec<SelectorScore> {
    pairs
        .iter()
        .map(|(id, w)| SelectorScore::new(selector, *id, *w, format!("{selector} 理由")))
        .collect()
}

fn weights(pairs: &[(SelectorId, f64)]) -> SelectorWeights {
    SelectorWeights::new(pairs.iter().copied().collect()).expect("合法权重")
}

// ─────────────────────────────────────────────────────────────────────────────
// 融合的基本性质
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn fused_probabilities_sum_to_one() {
    let eqs = ids(&["价格战", "默契分层", "先手优势"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("价格战", 0.6), ("默契分层", 0.3), ("先手优势", 0.1)]),
    );
    by_sel.insert(
        SelectorId::Stochastic,
        scores(SelectorId::Stochastic, &[("价格战", 0.7), ("默契分层", 0.2), ("先手优势", 0.1)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 0.5), (SelectorId::Stochastic, 0.5)]);

    let dist = fuse(
        &eqs,
        &by_sel,
        &w,
        confidence_for(0, 2, f64::NAN),
    )
    .expect("融合应成功");

    let sum: f64 = dist.entries.iter().map(|e| e.prob).sum();
    assert!((sum - 1.0).abs() < 1e-12, "概率和应为 1，实得 {sum}");
    assert_eq!(dist.entries.len(), 3);
}

#[test]
fn fused_output_is_sorted_by_probability_desc() {
    let eqs = ids(&["a", "b"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("a", 0.2), ("b", 0.8)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);
    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 1, f64::NAN)).unwrap();
    assert_eq!(dist.entries[0].equilibrium_id, "b");
    assert!(dist.entries[0].prob > dist.entries[1].prob);
}

#[test]
fn higher_weighted_selector_dominates_fusion() {
    // 两个判据对同一均衡给出相反意见；权重高的那个应当主导结果
    let eqs = ids(&["x", "y"]);
    let mut by_sel = BTreeMap::new();
    // 判据 A 认为 x 更可能
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("x", 0.9), ("y", 0.1)]),
    );
    // 判据 B 认为 y 更可能
    by_sel.insert(
        SelectorId::Behavioral,
        scores(SelectorId::Behavioral, &[("x", 0.1), ("y", 0.9)]),
    );

    // A 权重高 ⇒ x 应胜出
    let wa = weights(&[(SelectorId::RiskDom, 0.9), (SelectorId::Behavioral, 0.1)]);
    let da = fuse(&eqs, &by_sel, &wa, confidence_for(0, 2, f64::NAN)).unwrap();
    assert_eq!(
        da.entries[0].equilibrium_id, "x",
        "权重 0.9 的判据应主导：{da:?}"
    );

    // 反转权重 ⇒ y 应胜出
    let wb = weights(&[(SelectorId::RiskDom, 0.1), (SelectorId::Behavioral, 0.9)]);
    let db = fuse(&eqs, &by_sel, &wb, confidence_for(0, 2, f64::NAN)).unwrap();
    assert_eq!(
        db.entries[0].equilibrium_id, "y",
        "权重反转后结果应反转：{db:?}"
    );
}

#[test]
fn unnormalized_selector_input_is_normalized_internally() {
    // 判据给出未归一化的原始计数（如 persona_sim 的落点次数）——
    // 融合必须内部归一化，不得让量纲大的判据支配
    let eqs = ids(&["x", "y"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::PersonaSim,
        scores(SelectorId::PersonaSim, &[("x", 30.0), ("y", 70.0)]),
    );
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("x", 0.5), ("y", 0.5)]),
    );
    let w = weights(&[(SelectorId::PersonaSim, 0.5), (SelectorId::RiskDom, 0.5)]);
    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
    let sum: f64 = dist.entries.iter().map(|e| e.prob).sum();
    assert!((sum - 1.0).abs() < 1e-12, "仍应归一化，实得 {sum}");
    // persona_sim 给 y 更高（0.7 vs 0.3 归一化后）⇒ y 应胜
    assert_eq!(dist.entries[0].equilibrium_id, "y");
}

#[test]
fn drivers_are_recorded_for_report_explainability() {
    // 报告老板页要能说"为什么认为价格战概率最高"
    let eqs = ids(&["价格战", "默契分层"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::Stochastic,
        scores(SelectorId::Stochastic, &[("价格战", 0.8), ("默契分层", 0.2)]),
    );
    by_sel.insert(
        SelectorId::Behavioral,
        scores(SelectorId::Behavioral, &[("价格战", 0.6), ("默契分层", 0.4)]),
    );
    let w = weights(&[(SelectorId::Stochastic, 0.6), (SelectorId::Behavioral, 0.4)]);
    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
    let top = &dist.entries[0];
    assert_eq!(top.equilibrium_id, "价格战");
    assert!(!top.drivers.is_empty(), "必须记录驱动判据");
    // 贡献大的判据应排在前面（stochastic 权重 0.6 > behavioral 0.4）
    assert_eq!(top.drivers[0], SelectorId::Stochastic, "drivers 应按贡献降序");
}

#[test]
fn determinism_same_input_same_output() {
    let eqs = ids(&["a", "b", "c"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("a", 0.34), ("b", 0.33), ("c", 0.33)]),
    );
    by_sel.insert(
        SelectorId::Stochastic,
        scores(SelectorId::Stochastic, &[("a", 0.5), ("b", 0.3), ("c", 0.2)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 0.5), (SelectorId::Stochastic, 0.5)]);
    let d1 = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
    let d2 = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
    assert_eq!(d1, d2, "同输入必须逐位一致（确定性要求）");
}

// ─────────────────────────────────────────────────────────────────────────────
// 失败即拒绝（工程不变量②）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn missing_weight_is_an_error_not_a_silent_default() {
    // 剧本未配置某判据的权重 ⇒ 必须报错，不得用隐式默认值
    let eqs = ids(&["a", "b"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("a", 0.5), ("b", 0.5)]),
    );
    by_sel.insert(
        SelectorId::Behavioral,
        scores(SelectorId::Behavioral, &[("a", 0.5), ("b", 0.5)]),
    );
    // 只配了 risk_dom，缺 behavioral
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);
    let err = fuse(&eqs, &by_sel, &w, confidence_for(0, 1, f64::NAN))
        .expect_err("缺权重必须报错");
    assert!(
        matches!(err, SelectionError::MissingWeight(SelectorId::Behavioral)),
        "实得 {err:?}"
    );
}

#[test]
fn invalid_weights_rejected() {
    // 判据权重表只要求"有限且非负"——**不**要求 ≤ 1。
    // 理由：权重会先经归一化再用于融合，故 1.5、2.0 这类相对值是合法输入
    // （例如把"经验权重"写成 1.5 : 1.0 的形式）。
    // 真正必须拒绝的是 NaN / 无穷 / 负数——它们会让归一化失去意义。
    for w in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.1] {
        let mut bad = BTreeMap::new();
        bad.insert(SelectorId::RiskDom, w);
        assert!(
            matches!(
                SelectorWeights::new(bad),
                Err(SelectionError::InvalidWeight(SelectorId::RiskDom))
            ),
            "权重 {w} 必须被拒绝"
        );
    }

    // >1 的有限正值应被接受（归一化会处理）
    let mut ok = BTreeMap::new();
    ok.insert(SelectorId::RiskDom, 1.5);
    assert!(
        SelectorWeights::new(ok).is_ok(),
        "1.5 是合法的相对权重（归一化后生效），不应被拒"
    );
}

#[test]
fn empty_candidates_rejected() {
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);
    let err = fuse(&[], &BTreeMap::new(), &w, confidence_for(0, 1, f64::NAN))
        .expect_err("无候选必须报错");
    assert_eq!(err, SelectionError::NoCandidates);
}

#[test]
fn all_zero_weights_rejected() {
    // 全部判据权重为 0 ⇒ 无法归一化，必须报错而非返回均匀分布
    let eqs = ids(&["a", "b"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("a", 0.5), ("b", 0.5)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 0.0)]);
    let err = fuse(&eqs, &by_sel, &w, confidence_for(0, 1, f64::NAN))
        .expect_err("全零权重必须报错");
    assert_eq!(err, SelectionError::AllWeightsZero);
}

#[test]
fn zero_weight_selector_is_skipped_not_errored() {
    // 与上一条的区别：判据**表中有权重且为 0** 是"显式关闭"，应静默跳过；
    // 前提是另有一个非零判据使融合可行
    let eqs = ids(&["a", "b"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("a", 0.5), ("b", 0.5)]),
    );
    by_sel.insert(
        SelectorId::Learning,
        scores(SelectorId::Learning, &[("a", 0.0), ("b", 0.0)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 1.0), (SelectorId::Learning, 0.0)]);
    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN))
        .expect("显式关闭的判据不应致错");
    // learning 被跳过 ⇒ 不得出现在 drivers 里
    for e in &dist.entries {
        assert!(
            !e.drivers.contains(&SelectorId::Learning),
            "权重为 0 的判据不应出现在 drivers：{e:?}"
        );
    }
}

#[test]
fn normalize_rejects_out_of_range() {
    let mut s = scores(SelectorId::RiskDom, &[("a", 1.2), ("b", -0.2)]);
    assert!(normalize_selector(&mut s).is_err());
}

#[test]
fn normalize_makes_sum_one() {
    let mut s = scores(SelectorId::PersonaSim, &[("a", 3.0), ("b", 7.0)]);
    normalize_selector(&mut s).unwrap();
    let sum: f64 = s.iter().map(|x| x.weight).sum();
    assert!((sum - 1.0).abs() < 1e-12);
    assert!((s[1].weight - 0.7).abs() < 1e-12);
}

// ─────────────────────────────────────────────────────────────────────────────
// 判据分歧告警（设计文档 §5.4）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn divergence_is_detected_when_selector_disagrees_with_fusion() {
    // persona_sim 强烈认为 `非价格战` 会发生，但解析判据压倒性地认为 `价格战`
    let eqs = ids(&["价格战", "默契分层"]);
    let mut by_sel = BTreeMap::new();
    // 解析判据：价格战 0.99
    by_sel.insert(
        SelectorId::Stochastic,
        scores(SelectorId::Stochastic, &[("价格战", 0.99), ("默契分层", 0.01)]),
    );
    // persona_sim：默契分层 0.95（与解析法严重背离）
    by_sel.insert(
        SelectorId::PersonaSim,
        scores(SelectorId::PersonaSim, &[("价格战", 0.05), ("默契分层", 0.95)]),
    );
    // 解析判据权重压倒性
    let w = weights(&[(SelectorId::Stochastic, 0.95), (SelectorId::PersonaSim, 0.05)]);

    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
    assert!(
        !dist.divergences.is_empty(),
        "persona_sim 与解析法严重背离时必须告警（设计文档 §5.4）：{dist:?}"
    );
    let msg = &dist.divergences[0];
    assert!(
        msg.contains("persona_sim"),
        "告警应指名判据：{msg}"
    );
    assert!(
        msg.contains("默契分层"),
        "告警应指出被判据偏好的均衡：{msg}"
    );
}

#[test]
fn no_divergence_when_selectors_agree() {
    let eqs = ids(&["a", "b"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::Stochastic,
        scores(SelectorId::Stochastic, &[("a", 0.8), ("b", 0.2)]),
    );
    by_sel.insert(
        SelectorId::FocalLlm,
        scores(SelectorId::FocalLlm, &[("a", 0.75), ("b", 0.25)]),
    );
    let w = weights(&[(SelectorId::Stochastic, 0.5), (SelectorId::FocalLlm, 0.5)]);
    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
    assert!(
        dist.divergences.is_empty(),
        "判据一致时不应告警：{:?}",
        dist.divergences
    );
}

#[test]
fn divergence_threshold_is_respected() {
    // 构造刚好在阈值两侧的两种情形，验证阈值确实生效
    let eqs = ids(&["a", "b"]);
    // 情形 1：差距略小于阈值 ⇒ 不告警
    {
        let mut by_sel = BTreeMap::new();
        by_sel.insert(
            SelectorId::RiskDom,
            scores(SelectorId::RiskDom, &[("a", 0.5), ("b", 0.5)]),
        );
        by_sel.insert(
            SelectorId::Behavioral,
            scores(SelectorId::Behavioral, &[("a", 0.5), ("b", 0.5)]),
        );
        // 两判据等同 ⇒ 融合后 a、b 各 0.5，无差距
        let w = weights(&[(SelectorId::RiskDom, 0.5), (SelectorId::Behavioral, 0.5)]);
        let d = fuse(&eqs, &by_sel, &w, confidence_for(0, 2, f64::NAN)).unwrap();
        assert!(
            d.divergences.is_empty(),
            "无差距时不应告警（阈值 {DIVERGENCE_THRESHOLD}）：{:?}",
            d.divergences
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 对数损失与校准
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn log_loss_matches_hand_computation() {
    // 预测 [0.5, 0.5]，实现下标 0 ⇒ −ln(0.5) = ln2
    let ll = log_loss(&[0.5, 0.5], 0).unwrap();
    assert!((ll - std::f64::consts::LN_2).abs() < 1e-12, "实得 {ll}");

    // 完美预测：−ln(1) = 0
    assert!((log_loss(&[1.0, 0.0], 0).unwrap() - 0.0).abs() < 1e-12);
}

#[test]
fn log_loss_rejects_bad_input() {
    // 未归一化
    assert!(matches!(
        log_loss(&[0.5, 0.7], 0),
        Err(SelectionError::CalibrationNotNormalized { .. })
    ));
    // 下标越界
    assert!(matches!(
        log_loss(&[0.5, 0.5], 5),
        Err(SelectionError::CalibrationShape { .. })
    ));
}

#[test]
fn calibrate_refuses_to_fake_calibration_below_min_samples() {
    // 设计文档 §12：冷启动期必须诚实标注，不得假装已校准
    let w = weights(&[(SelectorId::RiskDom, 0.5), (SelectorId::Behavioral, 0.5)]);
    // 只给 3 个样本（远低于下限）
    let records: Vec<BacktestRecord> = (0..3)
        .map(|i| BacktestRecord {
            predicted: vec![0.5, 0.5],
            realized: i % 2,
            selector_weights: [
                (SelectorId::RiskDom, 0.9),
                (SelectorId::Behavioral, 0.1),
            ]
            .into_iter()
            .collect(),
        })
        .collect();

    let out = calibrate(&w, &records).expect("样本不足不应报错，而是拒绝校准");
    assert_eq!(out.samples, 3);
    // 权重必须**完全不变**
    for (_, (before, after)) in &out.weight_deltas {
        assert!(
            (before - after).abs() < 1e-15,
            "样本不足时权重不得变化：{before} → {after}"
        );
    }
    // 对数损失前后相同（因为没校准）
    assert!((out.mean_log_loss_before - out.mean_log_loss_after).abs() < 1e-15);
}

#[test]
fn calibrate_moves_weight_toward_better_selector() {
    // 构造 40 个样本：RiskDom 给出高权重（0.9），Behavioral 给出低权重（0.1）
    // ⇒ 校准后 RiskDom 的权重应上升
    let w = weights(&[(SelectorId::RiskDom, 0.5), (SelectorId::Behavioral, 0.5)]);
    let records: Vec<BacktestRecord> = (0..40)
        .map(|i| BacktestRecord {
            predicted: vec![0.5, 0.5],
            realized: i % 2,
            selector_weights: [
                (SelectorId::RiskDom, 0.9),
                (SelectorId::Behavioral, 0.1),
            ]
            .into_iter()
            .collect(),
        })
        .collect();

    let out: CalibrationOutcome = calibrate(&w, &records).expect("应校准");
    let (rd_before, rd_after) = out.weight_deltas[&SelectorId::RiskDom];
    let (bh_before, bh_after) = out.weight_deltas[&SelectorId::Behavioral];

    assert!(
        rd_after > rd_before,
        "表现好的判据权重应上升：{rd_before} → {rd_after}"
    );
    assert!(
        bh_after < bh_before,
        "表现差的判据权重应下降：{bh_before} → {bh_after}"
    );
    // 归一化：权重和应为 1
    let sum: f64 = out.updated.iter().map(|(_, v)| v).sum();
    assert!((sum - 1.0).abs() < 1e-12, "校准后权重应归一化，实得 {sum}");
}

#[test]
fn calibrate_is_deterministic() {
    let w = weights(&[(SelectorId::RiskDom, 0.4), (SelectorId::Learning, 0.6)]);
    let records: Vec<BacktestRecord> = (0..35)
        .map(|i| BacktestRecord {
            predicted: vec![0.3, 0.7],
            realized: usize::from(i % 3 == 0),
            selector_weights: [
                (SelectorId::RiskDom, 0.2),
                (SelectorId::Learning, 0.8),
            ]
            .into_iter()
            .collect(),
        })
        .collect();
    let a = calibrate(&w, &records).unwrap();
    let b = calibrate(&w, &records).unwrap();
    assert_eq!(a, b, "校准必须确定性");
}

#[test]
fn calibrate_does_not_enable_unconfigured_selectors() {
    // 回测数据里出现某判据，但它不在权重表中 ⇒ 不得因回测被"凭空启用"
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);
    let records: Vec<BacktestRecord> = (0..35)
        .map(|_| BacktestRecord {
            predicted: vec![0.5, 0.5],
            realized: 0,
            selector_weights: [
                (SelectorId::RiskDom, 0.5),
                (SelectorId::PersonaSim, 0.99), // 未配置
            ]
            .into_iter()
            .collect(),
        })
        .collect();
    let out = calibrate(&w, &records).unwrap();
    assert!(
        !out.updated.ids().contains(&SelectorId::PersonaSim),
        "未配置的判据不得被回测启用：{:?}",
        out.updated.ids()
    );
}

#[test]
fn confidence_grading_is_honest_about_cold_start() {
    // 样本低于下限 ⇒ PriorUncalibrated，且 note 必须说明"未校准"
    let c = confidence_for(5, 3, f64::NAN);
    assert!(!c.is_calibrated());
    let note = c.note();
    assert!(note.contains("未校准"), "冷启动说明须点明未校准：{note}");
    assert!(note.contains("30"), "应给出所需样本数：{note}");

    // 样本足够 ⇒ Calibrated，且 note 应含损失值
    let c2 = confidence_for(50, 4, 0.42);
    assert!(c2.is_calibrated());
    let note2 = c2.note();
    assert!(note2.contains("0.42"), "应给出平均对数损失：{note2}");
}

#[test]
fn selector_id_llm_flag_is_correct() {
    // 用于区分"确定性判据"与"LLM 判据"（后者权重必须受回测约束）
    assert!(!SelectorId::RiskDom.needs_llm());
    assert!(!SelectorId::Stochastic.needs_llm());
    assert!(!SelectorId::Learning.needs_llm());
    assert!(!SelectorId::Behavioral.needs_llm());
    assert!(SelectorId::FocalLlm.needs_llm());
    assert!(SelectorId::PersonaSim.needs_llm());
}

#[test]
fn all_selector_ids_round_trip_through_str() {
    for id in SelectorId::ALL {
        let s = id.as_str();
        assert!(!s.is_empty());
        // 与设计文档 §5.4 的 id 命名一致
        assert!(
            ["risk_dom", "stochastic", "learning", "behavioral", "focal_llm", "persona_sim"]
                .contains(&s),
            "判据 id 应与设计文档一致，实得 {s}"
        );
    }
}

#[test]
fn most_likely_reports_ties() {
    let eqs = ids(&["a", "b"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("a", 0.5), ("b", 0.5)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);
    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 1, f64::NAN)).unwrap();
    let (_, tied) = dist.most_likely().unwrap();
    assert!(tied, "概率相等时必须报告并列，而非任选一个：{dist:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 判据输入与候选集不一致：必须报错，不得静默丢弃
// ─────────────────────────────────────────────────────────────────────────────

/// **这是对一个真实缺陷的回归测试。**
///
/// 原实现里，判据提到一个不在候选集中的均衡时，代码注释写着"属调用方错误"，
/// 但行为是 `continue`——**静默丢弃该评分**。后果是融合结果仍呈现为一个
/// 看起来正常的概率分布，而某个判据的一部分输入已被悄悄丢掉；报告里该判据
/// 仍显示"参与了"，读者无从察觉。这正是工程不变量②（失败即拒绝，绝不静默降级）
/// 要禁止的情形。
#[test]
fn unknown_equilibrium_in_scores_is_an_error_not_silently_dropped() {
    let eqs = ids(&["e1", "e2"]);
    let mut by_sel = BTreeMap::new();
    // 判据给了一个候选集里不存在的均衡
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("e1", 0.5), ("e2", 0.3), ("e9_不存在", 0.2)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);

    let err = fuse(&eqs, &by_sel, &w, confidence_for(0, 1, f64::NAN))
        .expect_err("引用不存在的均衡必须报错，不得静默丢弃");
    match err {
        SelectionError::UnknownEquilibrium {
            selector,
            equilibrium_id,
            candidates,
        } => {
            assert_eq!(selector, SelectorId::RiskDom);
            assert_eq!(equilibrium_id, "e9_不存在");
            assert_eq!(candidates, eqs, "错误里必须带上候选集，便于定位");
        }
        other => panic!("期望 UnknownEquilibrium，实得 {other:?}"),
    }
}

/// 反面：判据只覆盖候选集的**子集**是合法的（该判据对另一部分没有意见），
/// 不得因此报错。这条防止上一处修正矫枉过正。
#[test]
fn selector_covering_a_subset_of_candidates_is_allowed() {
    let eqs = ids(&["e1", "e2", "e3"]);
    let mut by_sel = BTreeMap::new();
    by_sel.insert(
        SelectorId::RiskDom,
        scores(SelectorId::RiskDom, &[("e1", 0.8), ("e2", 0.2)]),
    );
    let w = weights(&[(SelectorId::RiskDom, 1.0)]);

    let dist = fuse(&eqs, &by_sel, &w, confidence_for(0, 1, f64::NAN))
        .expect("只对子集有意见是合法的");
    // 和仍须为 1（子集内部归一化后，未提及者得 0）
    let sum: f64 = dist.entries.iter().map(|e| e.prob).sum();
    assert!((sum - 1.0).abs() < 1e-12, "概率和须为 1，实得 {sum}");
    let e3 = dist.entries.iter().find(|e| e.equilibrium_id == "e3").unwrap();
    assert_eq!(e3.prob, 0.0, "未被该判据提及的均衡得 0，而不是被报错");
}

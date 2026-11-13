//! `shangyi-discover` 的测试。
//!
//! 重点验证 Simon 满意化的**两个核心行为**：
//! 1. 达标筛选正确（不是"最优解"，而是"够好的解"）
//! 2. 满意集为空时**必须给出抱负水平放宽建议**——只说"无解"对决策者无用

use std::collections::BTreeMap;

use shangyi_consequence::Percentiles;
use shangyi_discover::{
    Aspiration, Candidate, DEFAULT_ACHIEVEMENT_THRESHOLD, DiscoverError, MAX_SATISFIED,
    achievement_probability, discover, evaluate_candidate, pareto_front, propose_relaxation,
    robustness_margin,
};

fn pct(p10: f64, p50: f64, p90: f64) -> Percentiles {
    Percentiles { p10, p50, p90 }
}

fn cand(id: &str, profit: Percentiles, share: Percentiles) -> Candidate {
    let mut outcomes = BTreeMap::new();
    outcomes.insert("利润".to_string(), profit);
    outcomes.insert("份额".to_string(), share);
    Candidate {
        id: id.to_string(),
        description: format!("策略 {id}"),
        outcomes,
        llm_proposed: false,
    }
}

fn aspiration(profit: f64, share: f64) -> Aspiration {
    let mut levels = BTreeMap::new();
    levels.insert("利润".to_string(), profit);
    levels.insert("份额".to_string(), share);
    Aspiration::new(levels).expect("合法抱负水平")
}

// ─────────────────────────────────────────────────────────────────────────────
// 达标概率估计
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn achievement_probability_at_quantile_points() {
    let p = pct(10.0, 50.0, 90.0);
    // 抱负 ≤ P10 ⇒ 0.9（截断，不报 1.0——三点分位数无法确定尾部）
    assert!((achievement_probability(&p, 5.0) - 0.9).abs() < 1e-12);
    assert!((achievement_probability(&p, 10.0) - 0.9).abs() < 1e-12);
    // 在 P50 ⇒ 0.5
    assert!((achievement_probability(&p, 50.0) - 0.5).abs() < 1e-12);
    // 在 P90 ⇒ 0.1
    assert!((achievement_probability(&p, 90.0) - 0.1).abs() < 1e-12);
    // 超过 P90 ⇒ 0.1
    assert!((achievement_probability(&p, 1000.0) - 0.1).abs() < 1e-12);
}

#[test]
fn achievement_probability_interpolates_monotonically() {
    let p = pct(0.0, 50.0, 100.0);
    let mut prev = 1.0;
    for a in 0..=100 {
        let prob = achievement_probability(&p, a as f64);
        assert!(
            prob <= prev + 1e-12,
            "达标概率应随抱负水平单调不增：a={a} prob={prob} prev={prev}"
        );
        prev = prob;
    }
}

#[test]
fn achievement_probability_endpoints_not_certain() {
    // 端点截断在 0.9/0.1 而非 1.0/0.0：三点分位数无法确定尾部，
    // 报 1.0 会把"信息不足"伪装成"确定"
    let p = pct(0.0, 50.0, 100.0);
    assert!(
        achievement_probability(&p, -1000.0) < 1.0,
        "下界不得报 1.0"
    );
    assert!(
        achievement_probability(&p, 1e9) > 0.0,
        "上界不得报 0.0（否则会把「几乎不可能」与「不可能」混同）"
    );
}

#[test]
fn achievement_probability_handles_degenerate_distribution() {
    // P10 == P50 == P90（点估计）
    let p = pct(50.0, 50.0, 50.0);
    assert!((achievement_probability(&p, 40.0) - 0.9).abs() < 1e-12);
    assert!((achievement_probability(&p, 50.0) - 0.9).abs() < 1e-12);
    assert!((achievement_probability(&p, 60.0) - 0.1).abs() < 1e-12);
}

#[test]
fn achievement_probability_rejects_nonfinite_aspiration() {
    // 非有限抱负水平是**调用方错误**（不是"极高标竿"），故一律返回 0.0。
    // 注意 `+∞` 也走这条路径：`is_finite()` 是第一个 guard，
    // 所以它不会落到"超过 P90 ⇒ 0.1"的分支。这是刻意的：
    // 把"输入错误"与"标竿过高"区分开，避免调用方把 NaN 误读为"几乎不可能达标"。
    let p = pct(0.0, 1.0, 2.0);
    assert_eq!(achievement_probability(&p, f64::NAN), 0.0);
    assert_eq!(achievement_probability(&p, f64::INFINITY), 0.0);
    assert_eq!(achievement_probability(&p, f64::NEG_INFINITY), 0.0);
}

// ─────────────────────────────────────────────────────────────────────────────
// 达标筛选（Simon 满意化的核心）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn candidate_is_satisfied_only_when_all_criteria_met() {
    let asp = aspiration(100.0, 20.0);

    // 两个准则都远超抱负 ⇒ 满意
    let good = cand("good", pct(200.0, 300.0, 400.0), pct(30.0, 40.0, 50.0));
    let ev = evaluate_candidate(&good, &asp).unwrap();
    assert!(ev.satisfied, "两准则均达标应满意：{ev:?}");
    assert!(ev.unmet.is_empty());

    // 利润达标但份额不达标 ⇒ 不满意（**全部**准则都要达标）
    let partial = cand("partial", pct(200.0, 300.0, 400.0), pct(0.0, 5.0, 10.0));
    let ev2 = evaluate_candidate(&partial, &asp).unwrap();
    assert!(!ev2.satisfied, "仅部分准则达标不得算满意");
    assert!(ev2.unmet.contains(&"份额".to_string()));
    assert_eq!(ev2.unmet.len(), 1, "未达标项应精确列出：{:?}", ev2.unmet);
}

#[test]
fn missing_criterion_prediction_counts_as_unmet() {
    // 候选未给出某准则的预测 ⇒ 按未达标处理并记入 unmet，
    // 不得静默跳过（那会让"无法评估"伪装成"达标"）
    let asp = aspiration(100.0, 20.0);
    let mut outcomes = BTreeMap::new();
    outcomes.insert("利润".to_string(), pct(200.0, 300.0, 400.0));
    // 故意不给"份额"
    let c = Candidate {
        id: "incomplete".into(),
        description: "缺份额预测".into(),
        outcomes,
        llm_proposed: false,
    };
    let ev = evaluate_candidate(&c, &asp).unwrap();
    assert!(!ev.satisfied, "缺准则预测不得算满意");
    assert!(ev.unmet.contains(&"份额".to_string()));
    let sc = ev.criteria.iter().find(|c| c.criterion == "份额").unwrap();
    assert_eq!(sc.achieved_probability, 0.0);
    assert!(sc.p10.is_nan(), "缺失的准则其分位数应为 NaN 而非 0");
}

#[test]
fn discover_returns_satisfied_only() {
    let asp = aspiration(100.0, 20.0);
    let candidates = vec![
        cand("a", pct(200.0, 300.0, 400.0), pct(30.0, 40.0, 50.0)), // 满意
        cand("b", pct(0.0, 10.0, 20.0), pct(30.0, 40.0, 50.0)),     // 利润不达标
        cand("c", pct(500.0, 600.0, 700.0), pct(0.0, 5.0, 10.0)),   // 份额不达标
    ];
    let report = discover(&candidates, &asp).unwrap();
    assert_eq!(report.satisfied.len(), 1);
    assert_eq!(report.satisfied[0].candidate.id, "a");
    assert!(report.has_satisfied);
    assert!(report.relaxation.is_empty(), "有满意策略时不应给放宽建议");
}

#[test]
fn discover_rejects_empty_candidates() {
    let asp = aspiration(100.0, 20.0);
    assert!(matches!(
        discover(&[], &asp),
        Err(DiscoverError::NoCandidates)
    ));
}

#[test]
fn discover_caps_satisfied_list_at_five() {
    // 设计文档 §5.6.2：满意策略清单 ≤5 个
    let asp = aspiration(1.0, 1.0);
    let candidates: Vec<Candidate> = (0..10)
        .map(|i| {
            let base = 100.0 + i as f64;
            cand(
                &format!("c{i}"),
                pct(base, base + 10.0, base + 20.0),
                pct(base, base + 10.0, base + 20.0),
            )
        })
        .collect();
    let report = discover(&candidates, &asp).unwrap();
    assert_eq!(
        report.satisfied.len(),
        MAX_SATISFIED,
        "满意清单应被截取到 {MAX_SATISFIED} 个"
    );
}

#[test]
fn discover_is_deterministic() {
    let asp = aspiration(100.0, 20.0);
    let candidates = vec![
        cand("b", pct(200.0, 300.0, 400.0), pct(30.0, 40.0, 50.0)),
        cand("a", pct(200.0, 300.0, 400.0), pct(30.0, 40.0, 50.0)),
    ];
    let r1 = discover(&candidates, &asp).unwrap();
    let r2 = discover(&candidates, &asp).unwrap();
    assert_eq!(r1, r2, "同输入必须逐位一致");
    // 同稳健性时按 id 升序 ⇒ a 在 b 前
    assert_eq!(r1.satisfied[0].candidate.id, "a");
}

// ─────────────────────────────────────────────────────────────────────────────
// 抱负水平放宽（Simon 动态调整）—— 本 crate 最重要的行为
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn empty_satisfied_set_produces_relaxation_proposals() {
    // 抱负水平定得过高 ⇒ 无候选达标 ⇒ **必须**给出放宽建议
    let asp = aspiration(1000.0, 1000.0);
    let candidates = vec![
        cand("a", pct(100.0, 200.0, 300.0), pct(10.0, 20.0, 30.0)),
        cand("b", pct(150.0, 250.0, 350.0), pct(15.0, 25.0, 35.0)),
    ];
    let report = discover(&candidates, &asp).unwrap();

    assert!(!report.has_satisfied);
    assert!(report.satisfied.is_empty());
    assert!(
        !report.relaxation.is_empty(),
        "满意集为空时必须给出放宽建议，而非只说「无解」——\
         这是 Simon 抱负水平动态调整的落地（§5.6.2 步骤 4）"
    );

    // 每条建议必须**确有效果**（candidates_meeting > 0）
    for r in &report.relaxation {
        assert!(
            r.candidates_meeting > 0,
            "放宽建议必须真的能让候选达标：{r:?}"
        );
        assert!(
            r.suggested < r.current,
            "放宽意味着**降低**抱负水平：{r:?}"
        );
        assert!(
            r.rationale.contains("Simon"),
            "说明应点明理论依据（供报告引用）：{}",
            r.rationale
        );
    }
    // note 应说明放宽建议的数量
    assert!(report.note.contains("放宽"), "note 应提示放宽建议：{}", report.note);
}

#[test]
fn relaxation_covers_each_unmet_criterion() {
    // 两个准则都不达标 ⇒ 两条建议
    let asp = aspiration(1000.0, 1000.0);
    let candidates = vec![cand("a", pct(100.0, 200.0, 300.0), pct(10.0, 20.0, 30.0))];
    let report = discover(&candidates, &asp).unwrap();
    let criteria: Vec<&str> = report
        .relaxation
        .iter()
        .map(|r| r.criterion.as_str())
        .collect();
    assert!(criteria.contains(&"利润"), "应包含利润准则：{criteria:?}");
    assert!(criteria.contains(&"份额"), "应包含份额准则：{criteria:?}");
}

#[test]
fn no_relaxation_for_criteria_that_some_candidate_meets() {
    // 利润达标、份额不达标 ⇒ 只对份额给建议
    let asp = aspiration(50.0, 1000.0);
    let candidates = vec![cand("a", pct(100.0, 200.0, 300.0), pct(10.0, 20.0, 30.0))];
    let report = discover(&candidates, &asp).unwrap();
    assert_eq!(report.relaxation.len(), 1, "{:?}", report.relaxation);
    assert_eq!(report.relaxation[0].criterion, "份额");
}

#[test]
fn relaxation_rationale_includes_quantified_effect() {
    // 建议必须量化效果（"放宽到 X 后 Y 个候选达标"），否则用户无法判断
    let asp = aspiration(1000.0, 1000.0);
    let candidates = vec![
        cand("a", pct(100.0, 200.0, 300.0), pct(10.0, 20.0, 30.0)),
        cand("b", pct(150.0, 250.0, 350.0), pct(15.0, 25.0, 35.0)),
    ];
    let report = discover(&candidates, &asp).unwrap();
    let r = &report.relaxation[0];
    assert!(
        r.rationale.contains(&r.suggested.to_string()),
        "说明应含建议值：{}",
        r.rationale
    );
    assert!(
        r.rationale.contains(&r.candidates_meeting.to_string()),
        "说明应含可达标候选数：{}",
        r.rationale
    );
}

#[test]
fn propose_relaxation_returns_empty_for_empty_evaluations() {
    let asp = aspiration(100.0, 20.0);
    assert!(propose_relaxation(&[], &asp).is_empty());
}

// ─────────────────────────────────────────────────────────────────────────────
// 帕累托前沿
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn pareto_front_excludes_dominated() {
    // a 在两准则上都优于 b ⇒ b 被支配，不在前沿
    let asp = aspiration(1.0, 1.0);
    let candidates = vec![
        cand("a", pct(100.0, 200.0, 300.0), pct(20.0, 30.0, 40.0)),
        cand("b", pct(10.0, 20.0, 30.0), pct(1.0, 2.0, 3.0)),
    ];
    let evals: Vec<_> = candidates
        .iter()
        .map(|c| evaluate_candidate(c, &asp).unwrap())
        .collect();
    let front = pareto_front(&evals);
    assert!(front.contains(&"a".to_string()));
    assert!(!front.contains(&"b".to_string()), "被支配的候选不应在前沿");
}

#[test]
fn pareto_front_keeps_tradeoffs() {
    // a 利润高份额低；b 利润低份额高 ⇒ 互不支配，都在前沿
    let asp = aspiration(1.0, 1.0);
    let candidates = vec![
        cand("a", pct(90.0, 100.0, 110.0), pct(1.0, 2.0, 3.0)),
        cand("b", pct(9.0, 10.0, 11.0), pct(20.0, 30.0, 40.0)),
    ];
    let evals: Vec<_> = candidates
        .iter()
        .map(|c| evaluate_candidate(c, &asp).unwrap())
        .collect();
    let front = pareto_front(&evals);
    assert_eq!(front.len(), 2, "权衡关系上的候选应都保留：{front:?}");
    assert!(front.contains(&"a".to_string()) && front.contains(&"b".to_string()));
}

#[test]
fn identical_candidates_are_both_on_front() {
    // 完全相同 ⇒ 互不严格支配 ⇒ 都在前沿（不得任选一个丢掉）
    let asp = aspiration(1.0, 1.0);
    let candidates = vec![
        cand("a", pct(100.0, 200.0, 300.0), pct(20.0, 30.0, 40.0)),
        cand("b", pct(100.0, 200.0, 300.0), pct(20.0, 30.0, 40.0)),
    ];
    let evals: Vec<_> = candidates
        .iter()
        .map(|c| evaluate_candidate(c, &asp).unwrap())
        .collect();
    let front = pareto_front(&evals);
    assert_eq!(front.len(), 2, "相同候选不得被互相排除：{front:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 稳健满意度
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn robustness_is_positive_when_comfortably_satisfied() {
    // 抱负远低于 P10 ⇒ 即使用最差情形也能达标 ⇒ 高稳健
    let asp = aspiration(10.0, 1.0);
    let c = cand("safe", pct(100.0, 200.0, 300.0), pct(20.0, 30.0, 40.0));
    let ev = evaluate_candidate(&c, &asp).unwrap();
    assert!(ev.satisfied);
    assert!(
        ev.robustness > 0.9,
        "抱负远低于 P10 时余量应为 1（即使用最差情形也达标），实得 {}",
        ev.robustness
    );
}

#[test]
fn robustness_is_small_when_barely_satisfied() {
    // 抱负紧贴翻转点 ⇒ 稳健余量应很小。
    // 取抱负 = 160：P=[100,200,300]、阈值 0.6 ⇒ 翻转点 175，
    // 达标概率 = 0.9 − 0.4*(60/100) = 0.66 ≥ 0.6 ⇒ 达标，但余量小。
    let asp = aspiration(160.0, 30.0);
    let c = cand(
        "marginal",
        pct(100.0, 200.0, 300.0),
        pct(100.0, 200.0, 300.0), // 份额也给宽分布，避免它成为限制项
    );
    let ev = evaluate_candidate(&c, &asp).unwrap();
    assert!(ev.satisfied, "达标概率应 ≥ 0.6：{:?}", ev.unmet);
    assert!(
        ev.robustness < 0.3,
        "紧贴翻转点时余量应小，实得 {}",
        ev.robustness
    );
}

#[test]
fn robustness_takes_worst_criterion() {
    // 一个准则很稳（利润远超）、另一个紧贴标竿（份额恰好等于抱负）⇒
    // 整体稳健性由最脆弱的份额决定。
    let asp = aspiration(10.0, 30.0);
    // 份额取退化分布 P10=P50=P90=30：抱负 30 ⇒ 达标概率 0.9（≥0.6）但缓冲为 0
    let c = cand("mixed", pct(1000.0, 2000.0, 3000.0), pct(30.0, 30.0, 30.0));
    let ev = evaluate_candidate(&c, &asp).unwrap();
    assert!(ev.satisfied, "份额 0.9 ≥ 0.6 应达标");
    assert!(
        ev.robustness < 0.5,
        "稳健性应由最脆弱的准则决定（份额无缓冲），实得 {}",
        ev.robustness
    );
}

#[test]
fn robustness_is_zero_for_empty_criteria() {
    assert_eq!(robustness_margin(&[]), 0.0);
}

// ─────────────────────────────────────────────────────────────────────────────
// 抱负水平校验
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn aspiration_rejects_negative_and_invalid() {
    let mut bad = BTreeMap::new();
    bad.insert("利润".to_string(), -5.0);
    assert!(matches!(
        Aspiration::new(bad),
        Err(DiscoverError::NegativeAspiration(_))
    ));

    let mut nan = BTreeMap::new();
    nan.insert("利润".to_string(), f64::NAN);
    assert!(matches!(
        Aspiration::new(nan),
        Err(DiscoverError::NegativeAspiration(_))
    ));
}

#[test]
fn aspiration_rejects_invalid_threshold() {
    let mut a = aspiration(100.0, 20.0);
    a.default_threshold = 1.5;
    assert!(matches!(
        a.validate(),
        Err(DiscoverError::InvalidThreshold(_))
    ));

    let mut b = aspiration(100.0, 20.0);
    b.thresholds.insert("利润".to_string(), -0.1);
    assert!(matches!(
        b.validate(),
        Err(DiscoverError::InvalidThreshold(_))
    ));
}

#[test]
fn default_threshold_matches_design_doc() {
    // 设计文档 §5.6.2：达标概率阈值默认 0.6
    assert!((DEFAULT_ACHIEVEMENT_THRESHOLD - 0.6).abs() < 1e-12);
    let a = aspiration(100.0, 20.0);
    assert!((a.threshold_for("未知准则") - 0.6).abs() < 1e-12);
}

#[test]
fn per_criterion_threshold_override_works() {
    let mut a = aspiration(100.0, 20.0);
    a.thresholds.insert("利润".to_string(), 0.9); // 对利润更严格
    // 达标概率 0.5（抱负=P50）⇒ 0.5 < 0.9 ⇒ 不达标
    let c = cand("x", pct(0.0, 100.0, 200.0), pct(100.0, 200.0, 300.0));
    let ev = evaluate_candidate(&c, &a).unwrap();
    assert!(
        ev.unmet.contains(&"利润".to_string()),
        "更严格的阈值应使利润不达标：{:?}",
        ev.unmet
    );
}

#[test]
fn discover_ranks_by_robustness_descending() {
    // 稳健余量能区分"贴边"与"宽松"——本测试锁定这一**实际分辨率**。
    //
    // ⚠️ 数值是**算出来的**，不是凑的：翻转点 flip = P10 + ((0.9−0.6)/0.4)·(P50−P10)，
    // 余量 = (flip − 抱负)/(flip − P10)。
    //   tight：抱负 170 贴近 flip 175 ⇒ 余量 = 5/75 ≈ 0.067（贴边）
    //   loose：抱负 20 远低于 P10 100 ⇒ 余量饱和为 1.0
    // 两者严格可分，故能测出排序。
    //
    // 不测"宽松 vs 极宽松"的排序：两者都会饱和（见 robustness_margin 的分辨率边界说明）。
    // 断言一个不存在的分辨率会让测试虚假地"更严格"。
    let asp = aspiration(170.0, 17.0);

    let tight = cand("tight", pct(100.0, 200.0, 300.0), pct(10.0, 20.0, 30.0));
    let loose = cand("loose", pct(100.0, 200.0, 300.0), pct(10.0, 20.0, 30.0));

    // tight 用高抱负（170/17），loose 用低抱负（20/5）——
    // 为让两者用同一分布但不同抱负，分别评估
    let asp_tight = aspiration(170.0, 17.0);
    let asp_loose = aspiration(20.0, 5.0);
    let ev_tight = evaluate_candidate(&tight, &asp_tight).unwrap();
    let ev_loose = evaluate_candidate(&loose, &asp_loose).unwrap();

    assert!(ev_tight.satisfied, "tight 应达标（prob 0.62 ≥ 0.6）：{:?}", ev_tight.unmet);
    assert!(ev_loose.satisfied, "loose 应达标");

    assert!(
        ev_tight.robustness < 0.2,
        "贴边候选的余量应很小，实得 {}",
        ev_tight.robustness
    );
    assert!(
        (ev_loose.robustness - 1.0).abs() < 1e-12,
        "宽松候选的余量应饱和为 1.0，实得 {}",
        ev_loose.robustness
    );
    assert!(
        ev_loose.robustness > ev_tight.robustness,
        "宽松候选余量应严格大于贴边候选"
    );

    // 整条 discover 管线：同一抱负下两者都满意，且按余量降序
    let asp_mid = aspiration(170.0, 17.0);
    let report = discover(&[tight.clone(), loose.clone()], &asp_mid).unwrap();
    assert_eq!(report.satisfied.len(), 2, "两者都应达标");
    assert!(
        report.satisfied[0].robustness >= report.satisfied[1].robustness,
        "应按稳健余量降序：{:?}",
        report
            .satisfied
            .iter()
            .map(|e| (&e.candidate.id, e.robustness))
            .collect::<Vec<_>>()
    );
}

#[test]
fn robustness_saturates_when_aspiration_below_p10() {
    // 明确锁定"饱和"这一已知行为，使后人有据可依：
    // 若将来引入 P01 而消除饱和，本测试会失败——那时应更新它并说明改进。
    let asp = aspiration(20.0, 5.0);
    let a = cand("a", pct(100.0, 200.0, 300.0), pct(50.0, 100.0, 150.0));
    let b = cand("b", pct(1000.0, 2000.0, 3000.0), pct(500.0, 1000.0, 1500.0));
    let ea = evaluate_candidate(&a, &asp).unwrap();
    let eb = evaluate_candidate(&b, &asp).unwrap();
    assert_eq!(
        ea.robustness, eb.robustness,
        "抱负远低于 P10 时两者都应饱和为 1.0（信息不足，无分辨力）"
    );
    assert!((ea.robustness - 1.0).abs() < 1e-12);
}

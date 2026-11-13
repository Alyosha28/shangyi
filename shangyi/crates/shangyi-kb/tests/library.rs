//! `shangyi-kb` 的测试。
//!
//! 核心验证两件事：
//! 1. **理论库的四要素契约**：`fails_when` 与 `applicable_when` 同等重要，
//!    缺一不可——一个说不出"何时不适用"的条目会让 Agent 在错误场景引用它
//! 2. **案例库的回测诚实性**：未回填的案例**不得**参与校准（拿没答案的题调权重
//!    会奖励错误的判据）

use shangyi_kb::{
    CaseAssumption, CaseLibrary, CaseOutcome, CasePrediction, CaseRecord, KbError, TheoryEntry,
    TheoryLibrary, parse_theory_md,
};

fn prediction(candidates: &[(&str, f64)]) -> CasePrediction {
    CasePrediction {
        kind: shangyi_kb::PredictionKind::EquilibriumDistribution,
        candidates: candidates.iter().map(|(c, _)| c.to_string()).collect(),
        distribution: candidates.iter().map(|(_, p)| *p).collect(),
        selector_weights_snapshot: [("risk_dom".to_string(), 0.5), ("stochastic".to_string(), 0.5)]
            .into_iter()
            .collect(),
    }
}

/// 构造一条**真实委托**的案例，用于验证校准语义。
///
/// 与 `mk_case` 的区别：来源标为 `RealEngagement`。校准只采纳真实委托
/// （演练案例的"结果"是假设情景下的产物，拿它调权重等于用假数据拟合可信度）。
fn real_case(
    id: &str,
    date: &str,
    scenario: &str,
    pred: CasePrediction,
) -> CaseRecord {
    let mut c = CaseRecord::new(id, date, scenario, pred).expect("案例构造应成功");
    c.provenance = shangyi_kb::CaseProvenance::RealEngagement;
    c
}

/// 构造一条待回填的案例（用上面的 `prediction` helper）。
///
/// 来源标为 `RealEngagement`：本文件的测试验证的是**校准语义**
/// （哪些案例该进、哪些该排除），而只有真实委托才参与校准。
/// 「演练案例被排除」另有一条专门的测试（见 `domain_exec.rs`）。
fn mk_case(id: &str, candidates: &[(&str, f64)]) -> CaseRecord {
    let mut c = CaseRecord::new(id, "2026-05-01", "pricing_response", prediction(candidates))
        .expect("案例构造应成功");
    c.provenance = shangyi_kb::CaseProvenance::RealEngagement;
    c
}

const THEORY_MD: &str = r#"---
theory_id: bertrand-differentiated
name: 差异化产品价格竞争
family: noncooperative
applicable_when: [同业竞对, 可观察价格, 差异化产品]
fails_when: [容量约束强, 拍卖/招投标]
data_needs: [价格轨迹, 需求交叉弹性]
outputs: [均衡价格, 反应函数]
version: "1.0"
---

## 直觉

差异化产品下，价格战不会把价格打到边际成本——因为顾客不把两者视为完全替代。
"#;

const THEORY_MD_2: &str = r#"---
theory_id: rubinstein-bargaining
name: Rubinstein 轮流出价议价
family: cooperative
applicable_when: [双边谈判, 供应商议价, 大客户合同]
fails_when: [多方同时谈判, 完全信息不成立]
data_needs: [贴现因子, BATNA 估值]
outputs: [均衡分割比例, 议价力评估]
version: "1.0"
---

贴现因子决定议价力的分配。
"#;

// ─────────────────────────────────────────────────────────────────────────────
// 理论库（§7.1）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn theory_md_parses_with_frontmatter_and_body() {
    let e = parse_theory_md(THEORY_MD).expect("应能解析 §7.1 的条目形态");
    assert_eq!(e.theory_id, "bertrand-differentiated");
    assert_eq!(e.name, "差异化产品价格竞争");
    assert_eq!(e.family, "noncooperative");
    assert_eq!(e.applicable_when.len(), 3);
    assert_eq!(e.fails_when.len(), 2);
    assert_eq!(e.data_needs.len(), 2);
    assert_eq!(e.outputs.len(), 2);
    assert!(
        e.body.contains("直觉"),
        "正文应被解析进 body：{:?}",
        e.body
    );
}

#[test]
fn theory_without_fails_when_rejected() {
    // 一个说不出"何时不适用"的条目会让 Agent 在错误场景引用它
    let bad = r#"---
theory_id: x
name: X 理论
applicable_when: [场景A]
---
正文
"#;
    let err = parse_theory_md(bad).expect_err("缺 fails_when 必须报错");
    assert_eq!(err, KbError::MissingConditions);
}

#[test]
fn theory_without_applicable_when_rejected() {
    let bad = r#"---
theory_id: x
name: X 理论
fails_when: [场景B]
---
"#;
    assert!(matches!(
        parse_theory_md(bad),
        Err(KbError::MissingConditions)
    ));
}

#[test]
fn theory_without_frontmatter_rejected() {
    let err = parse_theory_md("# 没有 frontmatter 的 markdown").expect_err("必须报错");
    assert!(matches!(err, KbError::Frontmatter(_)), "实得 {err:?}");
}

#[test]
fn unclosed_frontmatter_rejected() {
    let err = parse_theory_md("---\ntheory_id: x\nname: X\n").expect_err("未闭合必须报错");
    assert!(matches!(err, KbError::Frontmatter(_)), "实得 {err:?}");
}

#[test]
fn bom_is_tolerated() {
    // 带 BOM 的文件（Windows 编辑器常见）应能解析——否则会误报"无 frontmatter"
    let with_bom = format!("\u{feff}{THEORY_MD}");
    assert!(parse_theory_md(&with_bom).is_ok(), "BOM 应被容忍");
}

#[test]
fn library_loads_multiple_entries() {
    let mut lib = TheoryLibrary::new();
    let id1 = lib.insert_md(THEORY_MD).unwrap();
    let id2 = lib.insert_md(THEORY_MD_2).unwrap();
    assert_eq!(id1, "bertrand-differentiated");
    assert_eq!(id2, "rubinstein-bargaining");
    assert_eq!(lib.len(), 2);
}

#[test]
fn duplicate_theory_id_rejected() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    let err = lib.insert_md(THEORY_MD).expect_err("重复 id 必须报错");
    assert!(matches!(err, KbError::DuplicateTheory(ref t) if t == "bertrand-differentiated"));
}

#[test]
fn library_get_returns_entry() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    let e = lib.get("bertrand-differentiated").unwrap();
    assert_eq!(e.name, "差异化产品价格竞争");
    assert!(lib.get("nonexistent").is_none());
}

#[test]
fn programmatic_insert_validates() {
    let mut lib = TheoryLibrary::new();
    let bad = TheoryEntry {
        theory_id: "x".into(),
        name: "X".into(),
        family: String::new(),
        applicable_when: vec!["a".into()],
        fails_when: vec![], // 缺
        data_needs: vec![],
        outputs: vec![],
        body: String::new(),
        version: "1.0".into(),
    };
    assert!(matches!(lib.insert(bad), Err(KbError::MissingConditions)));
}

// ─────────────────────────────────────────────────────────────────────────────
// 关键词粗筛（侧车不可用时的降级路径）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn keyword_candidates_matches_applicable_conditions() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    lib.insert_md(THEORY_MD_2).unwrap();

    let hits = lib.keyword_candidates(&["价格".to_string()]);
    assert_eq!(hits.len(), 1, "应只命中 Bertrand 条目：{hits:?}");
    assert_eq!(hits[0].theory_id, "bertrand-differentiated");
    assert!(!hits[0].has_fail_warning());
}

#[test]
fn keyword_candidates_flags_fail_condition_hits() {
    // 同时命中失效条件时应**标记警示**而非直接排除——
    // "容量约束只在小规模时强"这类判断需人来做
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    let hits = lib.keyword_candidates(&["价格".to_string(), "容量约束强".to_string()]);
    assert_eq!(hits.len(), 1);
    assert!(
        hits[0].has_fail_warning(),
        "命中失效条件必须标记：{hits:?}"
    );
    let note = hits[0].note();
    assert!(note.contains("需确认"), "说明应提示需确认：{note}");
}

#[test]
fn keyword_candidates_is_deterministic() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    lib.insert_md(THEORY_MD_2).unwrap();
    let kw = vec!["谈判".to_string(), "价格".to_string()];
    let a = lib.keyword_candidates(&kw);
    let b = lib.keyword_candidates(&kw);
    assert_eq!(a, b);
}

#[test]
fn keyword_candidates_returns_empty_for_no_match() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    assert!(lib.keyword_candidates(&["完全无关的词".to_string()]).is_empty());
}

#[test]
fn keyword_match_note_is_readable() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    let hits = lib.keyword_candidates(&["同业竞对".to_string()]);
    let note = hits[0].note();
    assert!(note.contains("差异化产品价格竞争"), "说明应含理论名：{note}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 案例库（§7.2）——回测闭环
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn case_requires_normalized_aligned_prediction() {
    // 预测分布长度与候选数不符
    let bad = CasePrediction {
        kind: shangyi_kb::PredictionKind::EquilibriumDistribution,
        candidates: vec!["a".into(), "b".into()],
        distribution: vec![0.5], // 长度不符
        selector_weights_snapshot: Default::default(),
    };
    assert!(matches!(
        CaseRecord::new("c1", "2026-01-01", "pricing_response", bad),
        Err(KbError::MissingPrediction(_))
    ));

    // 未归一化
    let bad2 = CasePrediction {
        kind: shangyi_kb::PredictionKind::EquilibriumDistribution,
        candidates: vec!["a".into(), "b".into()],
        distribution: vec![0.5, 0.7],
        selector_weights_snapshot: Default::default(),
    };
    assert!(matches!(
        CaseRecord::new("c2", "2026-01-01", "pricing_response", bad2),
        Err(KbError::MissingPrediction(_))
    ));
}

#[test]
fn pending_case_is_not_calibratable() {
    // **本 crate 最重要的断言**：未回填的案例不得参与校准。
    // 拿没有答案的题去调权重会奖励错误的判据。
    let mut lib = CaseLibrary::new();
    lib.insert(
        CaseRecord::new(
            "c1",
            "2026-01-01",
            "pricing_response",
            prediction(&[("价格战", 0.6), ("默契分层", 0.4)]),
        )
        .unwrap(),
    )
    .unwrap();

    assert_eq!(lib.len(), 1);
    assert_eq!(lib.pending().len(), 1, "应处于待回填状态");
    assert!(
        lib.calibratable().is_empty(),
        "未回填的案例不得进入校准集"
    );
    assert!(lib.mean_log_loss().is_none(), "无已回填样本时不得报损失值");
    assert!(!lib.calibration_ready(1));
}

#[test]
fn resolved_case_becomes_calibratable() {
    let mut lib = CaseLibrary::new();
    let mut c = real_case(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("价格战", 0.6), ("默契分层", 0.4)]),
    );
    c.resolve(CaseOutcome {
        date: "2026-03-01".into(),
        what_happened: "对手跟随降价，价格战爆发".into(),
        equilibrium_realized: "价格战".into(),
    })
    .unwrap();
    lib.insert(c).unwrap();

    assert_eq!(lib.calibratable().len(), 1);
    assert!(lib.pending().is_empty());
    assert!(lib.calibration_ready(1));
}

#[test]
fn outcome_must_be_among_predicted_candidates() {
    // 预测漏掉真相说明建模有缺陷——不得硬塞进对数损失里，
    // 那会奖励"蒙对"的判据。必须作为类型化错误上报。
    let mut c = CaseRecord::new(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("价格战", 0.6), ("默契分层", 0.4)]),
    )
    .unwrap();
    let err = c
        .resolve(CaseOutcome {
            date: "2026-03-01".into(),
            what_happened: "出现了完全没预料到的局面".into(),
            equilibrium_realized: "监管介入".into(), // 不在候选中
        })
        .expect_err("真相不在候选中必须报错");
    assert!(
        matches!(err, KbError::OutcomeNotInCandidates(ref e) if e == "监管介入"),
        "实得 {err:?}"
    );
    assert!(!c.is_resolved(), "失败的回填不应改变状态");
}

#[test]
fn log_loss_matches_hand_computation() {
    let mut c = CaseRecord::new(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("价格战", 0.5), ("默契分层", 0.5)]),
    )
    .unwrap();
    assert!(c.log_loss().is_none(), "未回填时无损失值");
    c.resolve(CaseOutcome {
        date: "d".into(),
        what_happened: "x".into(),
        equilibrium_realized: "价格战".into(),
    })
    .unwrap();
    let ll = c.log_loss().unwrap();
    assert!(
        (ll - std::f64::consts::LN_2).abs() < 1e-12,
        "预测 0.5 实现 ⇒ −ln0.5 = ln2，实得 {ll}"
    );
}

#[test]
fn perfect_prediction_has_zero_log_loss() {
    let mut c = CaseRecord::new(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("价格战", 1.0), ("默契分层", 0.0)]),
    )
    .unwrap();
    c.resolve(CaseOutcome {
        date: "d".into(),
        what_happened: "x".into(),
        equilibrium_realized: "价格战".into(),
    })
    .unwrap();
    assert!(c.log_loss().unwrap().abs() < 1e-12, "完美预测损失为 0");
}

#[test]
fn mean_log_loss_averages_resolved_only() {
    let mut lib = CaseLibrary::new();
    // 已回填：完美预测（损失 0）
    let mut c1 = real_case(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("A", 1.0), ("B", 0.0)]),
    );
    c1.resolve(CaseOutcome {
        date: "d".into(),
        what_happened: "x".into(),
        equilibrium_realized: "A".into(),
    })
    .unwrap();
    lib.insert(c1).unwrap();

    // 未回填：损失未知，不应影响均值
    lib.insert(
        CaseRecord::new(
            "c2",
            "2026-01-02",
            "pricing_response",
            prediction(&[("A", 0.1), ("B", 0.9)]),
        )
        .unwrap(),
    )
    .unwrap();

    let mean = lib.mean_log_loss().expect("应有一个已回填样本");
    assert!(
        mean.abs() < 1e-12,
        "均值只应统计已回填者（此处损失为 0），实得 {mean}"
    );
}

#[test]
fn scenario_filtered_calibration() {
    // 不同剧本的判据权重应独立校准——混在一起会让 pricing 的规律影响 negotiation
    let mut lib = CaseLibrary::new();
    for (id, scenario) in [("p1", "pricing_response"), ("n1", "negotiation")] {
        let mut c = real_case(id, "2026-01-01", scenario, prediction(&[("A", 0.5), ("B", 0.5)]));
        c.resolve(CaseOutcome {
            date: "d".into(),
            what_happened: "x".into(),
            equilibrium_realized: "A".into(),
        })
        .unwrap();
        lib.insert(c).unwrap();
    }
    assert_eq!(lib.calibratable().len(), 2);
    assert_eq!(lib.calibratable_in_scenario("pricing_response").len(), 1);
    assert_eq!(lib.calibratable_in_scenario("negotiation").len(), 1);
    assert_eq!(lib.calibratable_in_scenario("market_entry").len(), 0);
}

#[test]
fn duplicate_case_id_rejected() {
    let mut lib = CaseLibrary::new();
    let mk = || {
        CaseRecord::new(
            "c1",
            "2026-01-01",
            "pricing_response",
            prediction(&[("A", 1.0)]),
        )
        .unwrap()
    };
    lib.insert(mk()).unwrap();
    let err = lib.insert(mk()).expect_err("重复 case_id 必须报错——覆盖会丢失预测留档");
    assert!(matches!(err, KbError::DuplicateTheory(ref i) if i == "c1"));
}

#[test]
fn prediction_most_likely_is_deterministic_on_ties() {
    let p = prediction(&[("b_eq", 0.5), ("a_eq", 0.5)]);
    let (c, _) = p.most_likely().unwrap();
    assert_eq!(c, "a_eq", "并列时取字典序小者以保证确定性");
}

#[test]
fn prediction_probability_lookup() {
    let p = prediction(&[("A", 0.3), ("B", 0.7)]);
    assert!((p.probability_of("B").unwrap() - 0.7).abs() < 1e-12);
    assert!(p.probability_of("C").is_none());
}

#[test]
fn assumptions_carry_source() {
    // 铁律 2：假设必须带来源。类型上强制（source 非 Option）。
    let mut c = CaseRecord::new(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("A", 1.0)]),
    )
    .unwrap();
    c.assumptions.push(CaseAssumption {
        id: "A1".into(),
        content: "毛利率约 35%".into(),
        source: "user_confirmed".into(),
    });
    c.assumptions.push(CaseAssumption {
        id: "A2".into(),
        content: "对手成本与己方接近".into(),
        source: "industry_prior".into(),
    });
    let json = serde_json::to_string(&c).unwrap();
    let back: CaseRecord = serde_json::from_str(&json).unwrap();
    assert_eq!(c, back);
    assert_eq!(back.assumptions.len(), 2);
    assert!(back.assumptions.iter().all(|a| !a.source.is_empty()));
}

#[test]
fn library_serializes_round_trip() {
    let mut lib = CaseLibrary::new();
    let mut c = real_case(
        "c1",
        "2026-01-01",
        "pricing_response",
        prediction(&[("价格战", 0.6), ("默契分层", 0.4)]),
    );
    c.resolve(CaseOutcome {
        date: "2026-03-01".into(),
        what_happened: "价格战爆发".into(),
        equilibrium_realized: "价格战".into(),
    })
    .unwrap();
    lib.insert(c).unwrap();

    let json = serde_json::to_string(&lib).unwrap();
    let back: CaseLibrary = serde_json::from_str(&json).unwrap();
    assert_eq!(lib, back);
    assert!(back.calibration_ready(1));
}

#[test]
fn case_ids_are_deterministic() {
    let mut lib = CaseLibrary::new();
    for id in ["z", "a", "m"] {
        lib.insert(
            CaseRecord::new(id, "2026-01-01", "s", prediction(&[("A", 1.0)])).unwrap(),
        )
        .unwrap();
    }
    let ids: Vec<&str> = lib.ids().into_iter().collect();
    assert_eq!(ids, vec!["a", "m", "z"], "id 列表应有序");
}

#[test]
fn theory_library_serializes_round_trip() {
    let mut lib = TheoryLibrary::new();
    lib.insert_md(THEORY_MD).unwrap();
    let json = serde_json::to_string(&lib).unwrap();
    let back: TheoryLibrary = serde_json::from_str(&json).unwrap();
    assert_eq!(lib, back);
    assert_eq!(back.len(), 1);
}

#[test]
fn cold_start_reports_none_not_nan() {
    // 冷启动期必须返回 None 而非 NaN/0——
    // 报 0 会让读者误以为"预测很准"
    let lib = CaseLibrary::new();
    assert_eq!(lib.mean_log_loss(), None);
    assert!(!lib.calibration_ready(1));
}

// ───────────── 案例库持久化与校准数据供给（此前完全没有）─────────────

/// **案例库必须能存盘与读回**——否则校准闭环从地基上不成立。
///
/// # 为什么这条测试重要（实测审计发现的根本缺口）
///
/// `CaseLibrary` 有 `Serialize/Deserialize`，但**没有任何存盘/读盘代码**
/// ⇒ 它是纯内存的：`case_store_write` 归档的案例，进程退出即丢。
///
/// 后果链：
/// ```text
/// 案例丢失 → 永远积累不到 30 个样本 → calibration_ready 恒 false
///          → 判据权重永远未校准 → 报告里那句"待校准"永远不变
/// ```
///
/// 这比"`backtest` 命令未实现"更根本——后者是入口，这个是**存储**：
/// 没有它，即使 backtest 实现了也无数据可用。
#[test]
fn case_library_round_trips_through_a_file() {
    let dir = std::env::temp_dir().join(format!(
        "shangyi-kb-persist-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let path = dir.join("library.json");

    let mut lib = CaseLibrary::new();
    lib.insert(mk_case("c1", &[("e1", 0.7), ("e2", 0.3)]))
        .expect("入库");
    lib.save(&path).expect("存盘应成功");

    let back = CaseLibrary::load(&path).expect("读回应成功");
    assert_eq!(back.len(), 1, "存盘后读回应得到同一条案例");
    let rec = back.get("c1").expect("案例应在");
    assert_eq!(rec.prediction.candidates, vec!["e1", "e2"]);

    // 清理
    let _ = std::fs::remove_dir_all(&dir);
}

/// **文件不存在 ⇒ 空库**（冷启动正常）；**文件损坏 ⇒ 报错**（不当空库）。
///
/// # 为什么必须区分这两者
///
/// 把"损坏"当"空库"，会让用户以为"还没有积累样本"而继续归档——
/// 而**后续归档会覆盖掉仅有的那份证据**。
/// 案例库是几个月积累的、无法重建的样本。
#[test]
fn load_distinguishes_missing_from_corrupt() {
    let dir = std::env::temp_dir().join(format!(
        "shangyi-kb-corrupt-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");

    // ① 不存在 ⇒ 空库
    let missing = dir.join("nope.json");
    let lib = CaseLibrary::load(&missing).expect("文件不存在应返回空库（冷启动正常）");
    assert_eq!(lib.len(), 0);

    // ② 损坏 ⇒ 报错（**不当作空库**）
    let broken = dir.join("broken.json");
    std::fs::write(&broken, "{ 这不是合法 JSON").expect("写坏文件");
    let r = CaseLibrary::load(&broken);
    assert!(
        r.is_err(),
        "损坏的案例库必须**报错**——当作空库会让后续归档覆盖掉仅有的证据"
    );
    let msg = r.unwrap_err().to_string();
    assert!(
        msg.contains("损坏") && msg.contains("不当作空库"),
        "错误信息应说清这一点及处置建议：{msg}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **已回填的案例能转成校准记录**——校准闭环的数据供给。
///
/// 此前 `BacktestRecord` 只在 `shangyi-selection` 的**测试**里被构造过，
/// 生产路径上没有任何地方从案例库生成它 ⇒ 校准**永远无法启动**。
#[test]
fn backfillable_cases_convert_to_backtest_records() {
    let mut lib = CaseLibrary::new();

    // ① 已回填的（可校准）
    let mut c1 = mk_case("c1", &[("e1", 0.6), ("e2", 0.4)]);
    c1.resolve(CaseOutcome {
        date: "2026-05-01".into(),
        what_happened: "对手跟进降价".into(),
        equilibrium_realized: "e1".into(),
    })
    .expect("回填应成功");
    lib.insert(c1).expect("入库");

    // ② 未回填的（不可校准）
    lib.insert(mk_case("c2", &[("e1", 0.5), ("e2", 0.5)]))
        .expect("入库");

    let (records, skipped) = lib.backtest_records();
    assert_eq!(
        records.len(),
        1,
        "只有**已回填**的案例能转成校准记录（实得 {}）",
        records.len()
    );
    assert_eq!(
        skipped, 0,
        "`skipped` 只统计**已回填但形态不适配**的——未回填的不在其列\n\
         （它们由 `pending()` 单独提供，两者的处置完全不同）"
    );
    assert_eq!(
        lib.pending().len(),
        1,
        "未回填的案例应由 `pending()` 报出——那是「待办」，不是「不适用」"
    );
    assert_eq!(records[0].predicted, vec![0.6, 0.4]);
    assert_eq!(records[0].realized, 0, "e1 在候选中的下标是 0");
}

/// **不做对数损失的预测形态不参与校准**（如 discover 的达标概率）。
///
/// 达标概率**不归一化**（多个候选可同时达标，其和不等于 1），
/// 故 `-ln(p)` 不是可校准的分数——拿它调权重会奖励错误的判据。
#[test]
fn satisficing_predictions_are_excluded_from_calibration() {
    let mut lib = CaseLibrary::new();
    // ⚠️ `kind` 必须在**构造前**设好：`CaseRecord::new` 会按它决定
    // 是否校验归一化（达标概率本就不该归一化）。
    // 我第一版先构造再改 kind ⇒ 构造时就因"未归一化"被拒——
    // 那恰好说明了那条校验在工作。
    let mut pred = prediction(&[("e1", 0.8), ("e2", 0.6)]);
    pred.kind = shangyi_kb::PredictionKind::SatisficingAchievement;
    let mut c = real_case("c1", "2026-05-01", "pricing_response", pred);
    c.resolve(CaseOutcome {
        date: "2026-05-01".into(),
        what_happened: "候选 c1 达标".into(),
        equilibrium_realized: "e1".into(),
    })
    .expect("回填");
    lib.insert(c).expect("入库");

    let (records, skipped) = lib.backtest_records();
    assert!(
        records.is_empty(),
        "达标概率形态**不该**参与对数损失校准（它的和不等于 1，-ln(p) 无意义）"
    );
    assert_eq!(skipped, 1, "应如实计入跳过");
}

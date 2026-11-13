//! `shangyi-report` 的测试。
//!
//! 重点验证**审计链的强制力**：本 crate 存在的意义就是让"每个数字可溯源"
//! 成为机器可检查的约束，而非文档里的一句口号。
//! 故测试的核心是"缺源时必须拒绝"，而非"排版好看"。

use std::collections::BTreeMap;

use shangyi_report::{
    ContentHash, FigureSpec, LAYER_APPENDIX, LAYER_BOSS, Number, ProvenanceError, ProvenanceId,
    ProvenanceKind, ProvenanceLedger, ProvenanceRecord, Report, Section,
};

fn rec(id: &str, kind: ProvenanceKind, seq: u64) -> ProvenanceRecord {
    ProvenanceRecord::new(id, kind, seq)
}

fn pid(id: &str) -> ProvenanceId {
    ProvenanceId(id.to_string())
}

// ─────────────────────────────────────────────────────────────────────────────
// 铁律 1 的类型级强制
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn number_requires_registered_provenance() {
    let mut r = Report::new("case-1");
    // 未登记 provenance 的数字必须被拒——这是铁律 1 的核心强制点
    let n = Number::new("profit", 42.0, "预期利润", pid("prov-missing"));
    let err = r.add_number(n).expect_err("未登记 provenance 的数字必须被拒");
    assert!(
        matches!(err, ProvenanceError::MissingProvenance(ref k) if k == "profit"),
        "实得 {err:?}"
    );
}

#[test]
fn number_accepted_when_provenance_registered() {
    let mut r = Report::new("case-1");
    r.register_provenance(rec("p1", ProvenanceKind::ToolOutput, 42))
        .unwrap();
    r.add_number(Number::new("profit", 42.0, "预期利润", pid("p1")))
        .unwrap();
    assert_eq!(r.numbers.len(), 1);
}

#[test]
fn empty_report_fails_selfcheck() {
    // 空报告不应通过自检——否则"出个空报告"会绕过 provenance 要求
    let r = Report::new("case-empty");
    assert!(matches!(
        r.check_provenance(),
        Err(ProvenanceError::EmptyReport)
    ));
}

#[test]
fn report_with_numbers_and_sources_passes_selfcheck() {
    let mut r = Report::new("case-ok");
    r.register_provenance(rec("p1", ProvenanceKind::CodeComputed, 10))
        .unwrap();
    r.add_number(Number::new("profit", 42.0, "预期利润", pid("p1")))
        .unwrap();
    r.add_section(
        Section::new(LAYER_BOSS, "老板页")
            .with_paragraph("三句话结论")
            .with_numbers(vec!["profit".into()]),
    );
    assert!(r.check_provenance().is_ok());
}

// ─────────────────────────────────────────────────────────────────────────────
// provenance DAG 一致性
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn duplicate_provenance_id_rejected() {
    let mut l = ProvenanceLedger::new();
    l.register(rec("p1", ProvenanceKind::ToolOutput, 1)).unwrap();
    let err = l
        .register(rec("p1", ProvenanceKind::ToolOutput, 2))
        .expect_err("重复 id 必须报错——静默覆盖会丢失审计历史");
    assert!(matches!(err, ProvenanceError::DuplicateId(_)));
}

#[test]
fn dangling_derives_from_detected() {
    let mut l = ProvenanceLedger::new();
    l.register(
        rec("downstream", ProvenanceKind::CodeComputed, 5)
            .derived_from(vec![pid("nonexistent")]),
    )
    .unwrap();
    let err = l.check_refs().expect_err("悬空上游引用必须被发现");
    assert!(
        matches!(err, ProvenanceError::DanglingRef { ref upstream, .. } if upstream.0 == "nonexistent"),
        "实得 {err:?}"
    );
}

#[test]
fn cycle_in_provenance_detected() {
    // 有环则"溯源"会无限递归，必须拦住
    let mut l = ProvenanceLedger::new();
    l.register(rec("a", ProvenanceKind::CodeComputed, 1).derived_from(vec![pid("b")]))
        .unwrap();
    l.register(rec("b", ProvenanceKind::CodeComputed, 2).derived_from(vec![pid("a")]))
        .unwrap();
    let err = l.check_acyclic().expect_err("环必须被发现");
    assert!(matches!(err, ProvenanceError::Cycle(_)), "实得 {err:?}");
}

#[test]
fn self_loop_is_a_cycle() {
    let mut l = ProvenanceLedger::new();
    l.register(rec("a", ProvenanceKind::CodeComputed, 1).derived_from(vec![pid("a")]))
        .unwrap();
    assert!(l.check_acyclic().is_err());
}

#[test]
fn trace_returns_full_upstream_closure() {
    // 构造 DAG: c ← b ← a（c 由 b 派生，b 由 a 派生）
    let mut l = ProvenanceLedger::new();
    l.register(rec("a", ProvenanceKind::DataFile, 1)).unwrap();
    l.register(
        rec("b", ProvenanceKind::CodeComputed, 2).derived_from(vec![pid("a")]),
    )
    .unwrap();
    l.register(
        rec("c", ProvenanceKind::CodeComputed, 3).derived_from(vec![pid("b")]),
    )
    .unwrap();

    let t = l.trace(&pid("c"));
    assert_eq!(t.len(), 3, "应含自身与全部上游：{t:?}");
    assert!(t.contains(&pid("a")) && t.contains(&pid("b")) && t.contains(&pid("c")));
}

#[test]
fn trace_handles_diamond_dependency() {
    // 菱形：d ← b, c；b ← a；c ← a。不得重复访问 a，也不得死循环
    let mut l = ProvenanceLedger::new();
    l.register(rec("a", ProvenanceKind::DataFile, 1)).unwrap();
    l.register(rec("b", ProvenanceKind::CodeComputed, 2).derived_from(vec![pid("a")])).unwrap();
    l.register(rec("c", ProvenanceKind::CodeComputed, 3).derived_from(vec![pid("a")])).unwrap();
    l.register(
        rec("d", ProvenanceKind::CodeComputed, 4).derived_from(vec![pid("b"), pid("c")]),
    )
    .unwrap();
    let t = l.trace(&pid("d"));
    assert_eq!(t.len(), 4, "菱形依赖应得 4 个唯一节点：{t:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 图表与章节的引用完整性
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn figure_referencing_unknown_number_rejected() {
    let mut r = Report::new("case-fig");
    r.register_provenance(rec("p1", ProvenanceKind::ToolOutput, 1)).unwrap();
    r.add_number(Number::new("a", 1.0, "A", pid("p1"))).unwrap();
    // 图引用了不存在的数字
    r.add_figure(
        FigureSpec::new("f1", "bar", "概率分布").with_series("概率", vec!["missing".into()]),
    );
    let err = r.check_provenance().expect_err("图引用未知数字必须被发现");
    assert!(
        matches!(err, ProvenanceError::UnknownNumberRef(ref f, ref k) if f == "f1" && k == "missing"),
        "实得 {err:?}"
    );
}

#[test]
fn section_referencing_unknown_number_rejected() {
    let mut r = Report::new("case-sec");
    r.register_provenance(rec("p1", ProvenanceKind::ToolOutput, 1)).unwrap();
    r.add_number(Number::new("a", 1.0, "A", pid("p1"))).unwrap();
    r.add_section(Section::new(LAYER_BOSS, "老板页").with_numbers(vec!["ghost".into()]));
    assert!(r.check_provenance().is_err());
}

#[test]
fn section_referencing_unknown_figure_rejected() {
    let mut r = Report::new("case-secfig");
    r.register_provenance(rec("p1", ProvenanceKind::ToolOutput, 1)).unwrap();
    r.add_number(Number::new("a", 1.0, "A", pid("p1"))).unwrap();
    r.add_section(Section::new(LAYER_BOSS, "老板页").with_figures(vec!["nofig".into()]));
    assert!(r.check_provenance().is_err());
}

#[test]
fn figure_spec_references_numbers_not_inline_values() {
    // 图的 series 存的是**数字 key**，不是内联数值——
    // 这是"图上每个点都能反查到 provenance"的前提
    let f = FigureSpec::new("f1", "bar", "均衡概率分布")
        .with_series("概率", vec!["p_price_war".into(), "p_tacit".into()]);
    assert_eq!(f.referenced_keys().len(), 2);
    assert!(f.referenced_keys().contains(&"p_price_war"));
}

// ─────────────────────────────────────────────────────────────────────────────
// 数据文件哈希校验
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn file_hash_verification_catches_tampering() {
    // 数据在报告生成后被改动 ⇒ 报告数字不再可由该文件复现 ⇒ 审计链断裂
    let bytes = b"price,share\n10,0.3\n";
    let hash = ContentHash::of(bytes);

    let mut l = ProvenanceLedger::new();
    l.register(
        rec("p-file", ProvenanceKind::DataFile, 7)
            .with_source_file("data/prices.csv", hash.clone()),
    )
    .unwrap();

    // 原样 ⇒ 通过
    assert!(l.verify_file_hash(&pid("p-file"), bytes).is_ok());

    // 被改过 ⇒ 报错
    let tampered = b"price,share\n10,0.9\n";
    let err = l
        .verify_file_hash(&pid("p-file"), tampered)
        .expect_err("文件被改动必须被发现");
    assert!(matches!(err, ProvenanceError::HashMismatch { .. }), "实得 {err:?}");
}

#[test]
fn content_hash_is_stable_and_distinguishing() {
    let a = ContentHash::of(b"hello");
    let b = ContentHash::of(b"hello");
    let c = ContentHash::of(b"hello!");
    assert_eq!(a, b, "同内容必须同哈希");
    assert_ne!(a, c, "不同内容必须不同哈希");
    assert_eq!(a.0.len(), 64, "blake3 十六进制应为 64 字符");
}

// ─────────────────────────────────────────────────────────────────────────────
// 来源标注（PriorBorrowed 必须显式提醒）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn borrowed_prior_requires_caveat() {
    // 设计文档 §12：先验库标注 borrowed——否则读者会把行业先验
    // 误当成本对手的实证数据
    assert!(ProvenanceKind::PriorBorrowed.requires_caveat().is_some());
    let c = ProvenanceKind::PriorBorrowed.requires_caveat().unwrap();
    assert!(c.contains("borrowed"), "标注须含 borrowed 字样：{c}");
    assert!(c.contains("非本对手实证"), "标注须点明非实证：{c}");
}

#[test]
fn llm_proposal_requires_caveat() {
    // LLM 提案须标注"已经引擎评估"——区分"LLM 想到"与"引擎算出"
    let c = ProvenanceKind::LlmProposal.requires_caveat().unwrap();
    assert!(c.contains("引擎评估"), "实得 {c}");
}

#[test]
fn plain_sources_need_no_caveat() {
    for k in [
        ProvenanceKind::ToolOutput,
        ProvenanceKind::DataFile,
        ProvenanceKind::UserAnswer,
        ProvenanceKind::CodeComputed,
    ] {
        assert!(k.requires_caveat().is_none(), "{k:?} 不应要求标注");
    }
}

#[test]
fn appendix_lists_caveats_for_borrowed_and_llm() {
    let mut r = Report::new("case-caveat");
    r.register_provenance(rec("p1", ProvenanceKind::PriorBorrowed, 1)).unwrap();
    r.register_provenance(rec("p2", ProvenanceKind::LlmProposal, 2).with_llm("glm-5.3", "call-9")).unwrap();
    r.register_provenance(rec("p3", ProvenanceKind::CodeComputed, 3)).unwrap();
    r.add_number(Number::new("a", 1.0, "先验参数", pid("p1"))).unwrap();
    r.add_number(Number::new("b", 2.0, "聚点权重", pid("p2"))).unwrap();
    r.add_number(Number::new("c", 3.0, "均衡概率", pid("p3"))).unwrap();

    let appendix = r.provenance_appendix();
    assert!(appendix.contains("先验参数"), "应列出数字");
    assert!(appendix.contains("borrowed"), "先验须标注 borrowed");
    assert!(appendix.contains("LLM 提案"), "LLM 提案须标注");
    // 普通来源不应出现在"需注意的标注"段
    let caveat_section = appendix.split("## 需注意的来源标注").nth(1).unwrap_or("");
    assert!(!caveat_section.contains("均衡概率"), "普通来源不应列入标注段");
}

// ─────────────────────────────────────────────────────────────────────────────
// 附录与溯源出口
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn appendix_is_valid_markdown_table() {
    let mut r = Report::new("case-md");
    r.register_provenance(
        rec("p1", ProvenanceKind::ToolOutput, 42).with_tool("game_solve", "call-1"),
    )
    .unwrap();
    r.add_number(Number::new("prob", 0.6, "价格战概率", pid("p1")).with_unit("概率"))
        .unwrap();
    let md = r.provenance_appendix();
    assert!(md.starts_with("# 数字溯源清单"));
    assert!(md.contains("| 数字 | 值 | 来源类别 | 事件序号 | 上游依赖 |"));
    assert!(md.contains("价格战概率"));
    assert!(md.contains("0.6 概率"), "应含单位：{md}");
    assert!(md.contains("| 42 |"), "应含事件序号锚点：{md}");
}

#[test]
fn appendix_shows_missing_provenance_rather_than_hiding_it() {
    // 构造一个畸形状态（Number 存在但 provenance 未登记）——
    // 正常路径会被 add_number 拦住，此处直接操作字段以验证附录的保底显示。
    // 目的：宁可显示"缺少 provenance"，也不静默隐藏该数字。
    let mut r = Report::new("case-broken");
    r.register_provenance(rec("p1", ProvenanceKind::ToolOutput, 1)).unwrap();
    r.add_number(Number::new("ok", 1.0, "正常数字", pid("p1"))).unwrap();
    // 绕过 add_number 强行插入
    r.numbers.insert(
        "bad".to_string(),
        Number::new("bad", 2.0, "缺源数字", pid("p-never-registered")),
    );

    // 自检必须拦住它
    assert!(r.check_provenance().is_err());
    // 附录保底显示，不隐藏
    let md = r.provenance_appendix();
    assert!(md.contains("缺少 provenance"), "附录应显式标出缺源：{md}");
}

#[test]
fn trace_number_returns_ordered_chain() {
    let mut r = Report::new("case-trace");
    r.register_provenance(rec("raw", ProvenanceKind::DataFile, 1)).unwrap();
    r.register_provenance(
        rec("mid", ProvenanceKind::CodeComputed, 2).derived_from(vec![pid("raw")]),
    )
    .unwrap();
    r.register_provenance(
        rec("final", ProvenanceKind::CodeComputed, 3).derived_from(vec![pid("mid")]),
    )
    .unwrap();
    r.add_number(Number::new("result", 9.9, "最终结果", pid("final"))).unwrap();

    let chain = r.trace_number("result").expect("应能溯源");
    assert_eq!(chain.len(), 3);
    // BTreeSet 迭代 ⇒ 按 id 字典序（确定性）
    assert_eq!(chain[0].0, "final");
    assert_eq!(chain[2].0, "raw");
}

#[test]
fn trace_number_returns_none_for_unknown_key() {
    let r = Report::new("case-none");
    assert!(r.trace_number("nope").is_none());
}

// ─────────────────────────────────────────────────────────────────────────────
// 四层结构
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn four_layer_structure_supported() {
    // 设计文档 §9.1 的四层
    let mut r = Report::new("case-layers");
    r.register_provenance(rec("p1", ProvenanceKind::CodeComputed, 1)).unwrap();
    r.add_number(Number::new("n", 1.0, "数字", pid("p1"))).unwrap();
    for (layer, title) in [
        (LAYER_BOSS, "老板页"),
        ("scenario", "情景推演"),
        ("profile", "参与者画像与证据"),
        (LAYER_APPENDIX, "技术附录"),
    ] {
        r.add_section(Section::new(layer, title).with_numbers(vec!["n".into()]));
    }
    assert_eq!(r.sections.len(), 4);
    assert!(r.check_provenance().is_ok());
}

#[test]
fn report_serializes_round_trip() {
    // 报告需能序列化落盘（案例库）并读回——provenance 链不得在序列化中丢失
    let mut r = Report::new("case-ser");
    r.register_provenance(
        rec("p1", ProvenanceKind::ToolOutput, 42).with_tool("game_solve", "c1"),
    )
    .unwrap();
    r.add_number(Number::new("a", 1.5, "A", pid("p1"))).unwrap();
    r.add_figure(FigureSpec::new("f1", "bar", "图").with_series("s", vec!["a".into()]));

    let json = serde_json::to_string(&r).expect("应可序列化");
    let back: Report = serde_json::from_str(&json).expect("应可反序列化");
    assert_eq!(r, back, "序列化往返不得丢字段（含 provenance）");
    assert!(back.check_provenance().is_ok());
}

#[test]
fn number_display_includes_unit_when_present() {
    let n = Number::new("a", 42.5, "利润", pid("p1")).with_unit("万元");
    assert_eq!(n.display(), "42.5 万元");
    let n2 = Number::new("b", 0.6, "概率", pid("p1"));
    assert_eq!(n2.display(), "0.6");
}

#[test]
fn empty_ledger_checks_pass() {
    // 空台账本身是合法的（无引用可悬空、无环）——
    // "报告非空"由 check_provenance 的 EmptyReport 分支负责
    let l = ProvenanceLedger::new();
    assert!(l.is_empty());
    assert!(l.check_refs().is_ok());
    assert!(l.check_acyclic().is_ok());
}

#[test]
fn ledger_len_counts_records() {
    let mut l = ProvenanceLedger::new();
    for i in 0..5 {
        l.register(rec(&format!("p{i}"), ProvenanceKind::CodeComputed, i)).unwrap();
    }
    assert_eq!(l.len(), 5);
}

#[test]
fn all_provenance_kinds_serialize_kebab_case() {
    // 与 Harness §15.4 的 serde 命名一致（kebab-case），便于跨语言读取
    let cases = [
        (ProvenanceKind::ToolOutput, "\"tool-output\""),
        (ProvenanceKind::DataFile, "\"data-file\""),
        (ProvenanceKind::UserAnswer, "\"user-answer\""),
        (ProvenanceKind::PriorBorrowed, "\"prior-borrowed\""),
        (ProvenanceKind::LlmProposal, "\"llm-proposal\""),
        (ProvenanceKind::CodeComputed, "\"code-computed\""),
    ];
    for (k, want) in cases {
        assert_eq!(serde_json::to_string(&k).unwrap(), want, "{k:?}");
    }
}

#[test]
fn code_rev_records_param_hash() {
    // 只记 crate 版本无法复现具体结果；参数哈希使"这份代码在这组参数下算出"可验证
    let r = rec("p1", ProvenanceKind::CodeComputed, 1).with_code(
        "shangyi-solver",
        "0.1.0",
        "params:a1b2c3",
    );
    let cr = r.code_rev.expect("应记录代码版本");
    assert_eq!(cr.crate_name, "shangyi-solver");
    assert_eq!(cr.param_hash, "params:a1b2c3");
}

#[test]
fn tool_call_ref_is_recorded() {
    let r = rec("p1", ProvenanceKind::ToolOutput, 1).with_tool("monte_carlo", "call-42");
    let t = r.tool_call.expect("应记录工具调用");
    assert_eq!(t.tool, "monte_carlo");
    assert_eq!(t.call_id, "call-42");
}

#[test]
fn produced_at_ms_is_recorded_when_set() {
    let r = rec("p1", ProvenanceKind::ToolOutput, 1).at(1_700_000_000_000);
    assert_eq!(r.produced_at_ms, 1_700_000_000_000);
}

#[test]
fn series_map_is_deterministic() {
    // BTreeMap 保证序列顺序确定 ⇒ 图规格的序列化可复现
    let mut m = BTreeMap::new();
    m.insert("z", vec!["1"]);
    m.insert("a", vec!["2"]);
    let keys: Vec<&&str> = m.keys().collect();
    assert_eq!(keys, vec![&"a", &"z"]);
}
// **探索性产出不得进报告**——设计文档 §5.3 / Harness R8 的强制点。
//
// # 为什么这个文件存在（实测审计发现的缺口）
//
// 规则本身早就写了：`TrustLevel::Exploratory`（沙箱产出）**不得进报告**，
// 任何要进报告的结论须由确定性 Rust 重算。
//
// 但审计发现**它在生产路径上没有强制点**：
// - `ToolRegistry::check_reportable` 只在**测试**里被调用
// - `ProvenanceKind` 枚举**根本没有表达"探索性"的变体** ⇒ 沙箱结果
//   只能被标成 `ToolOutput` 或 `CodeComputed`，而**两者都会让它合法进报告**
//
// 故加 `ProvenanceKind::Exploratory` 并在 `check_provenance` 里拒绝它。
// 本文件锁住那条拒绝。

fn report_with(kind: ProvenanceKind) -> Report {
    let mut r = Report::new("probe");
    r.register_provenance(ProvenanceRecord::new("p1", kind, 0))
        .expect("注册 provenance");
    r.add_number(Number::new("k1", 1.0, "一个数", ProvenanceId("p1".into())))
        .expect("登记数字");
    r.add_section(Section::new("s", "节").with_numbers(vec!["k1".into()]));
    r
}

/// **探索性来源的数字必须被自检拒绝。**
#[test]
fn exploratory_numbers_are_rejected_by_self_check() {
    let r = report_with(ProvenanceKind::Exploratory);
    let err = r
        .check_provenance()
        .expect_err("探索性产出**不得**通过自检——它只能用于探索");
    let msg = err.to_string();
    assert!(
        msg.contains("确定性") || msg.contains("探索性"),
        "拒绝理由应说清「须由确定性代码重算」：{msg}"
    );
    assert!(
        msg.contains("k1"),
        "应指出是哪个数字有问题：{msg}"
    );
}

/// **引擎算出的数字必须被接受**（否则禁止就变成了"什么都不让进"）。
#[test]
fn code_computed_numbers_pass() {
    for kind in [
        ProvenanceKind::CodeComputed,
        ProvenanceKind::ToolOutput,
        ProvenanceKind::DataFile,
        ProvenanceKind::UserAnswer,
        ProvenanceKind::PriorBorrowed,
        ProvenanceKind::LlmProposal,
    ] {
        report_with(kind)
            .check_provenance()
            .unwrap_or_else(|e| panic!("{kind:?} 应通过自检：{e}"));
    }
}

/// **`reportable()` 的分类必须与变体一一对应**——新增变体时忘了分类，
/// 会静默把它当作"可进报告"（`matches!` 的默认方向取决于写法）。
///
/// 本测试枚举全部变体并断言已知的不可报告集合**恰好**是 `{Exploratory}`。
/// 有人加新变体而没想清它该不该进报告时，这条会失败并逼他回答那个问题。
#[test]
fn every_kind_has_an_explicit_reportability_decision() {
    let all = [
        ProvenanceKind::ToolOutput,
        ProvenanceKind::DataFile,
        ProvenanceKind::UserAnswer,
        ProvenanceKind::PriorBorrowed,
        ProvenanceKind::LlmProposal,
        ProvenanceKind::CodeComputed,
        ProvenanceKind::Exploratory,
    ];
    let not_reportable: Vec<ProvenanceKind> = all
        .iter()
        .copied()
        .filter(|k| !k.reportable())
        .collect();
    assert_eq!(
        not_reportable,
        vec![ProvenanceKind::Exploratory],
        "不可进报告的来源应当**恰好**是探索性那一类。\
         若你新增了一个变体，请在此明确它该不该进报告——\
         而不是让它默默落进 matches! 的默认分支。"
    );
}

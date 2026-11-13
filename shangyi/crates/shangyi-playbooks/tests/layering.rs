//! `shangyi-playbooks` 的测试。
//!
//! 核心验证**整行替换语义**（§2.4 工程不变量③）：
//! 同 id 的行被完全替换、绝不深合并；且每行的来源层可查（可检视性）。

use std::collections::BTreeMap;

use shangyi_playbooks::{
    Layer, Playbook, PlaybookError, PlaybookMeta, PlaybookRoot, Row, ScanDepth, compose,
    select_playbook,
};

fn row(id: &str, plugin: &str, yaml: &str) -> Row {
    let mut r = Row::new(id, plugin);
    r.config = serde_yaml::from_str(yaml).expect("合法 YAML");
    r
}

// ─────────────────────────────────────────────────────────────────────────────
// 整行替换语义 —— 本 crate 的核心
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn later_layer_replaces_whole_row() {
    let base = Layer::with_rows(
        "bundled",
        vec![row("sidecar", "tool.python-sidecar", "{timeout: 30, network: false}")],
    );
    let over = Layer::with_rows(
        "cli",
        vec![row("sidecar", "tool.python-sidecar", "{timeout: 60}")],
    );
    let c = compose(&[base, over]).unwrap();

    let r = c.get("sidecar").unwrap();
    // **关键断言**：`network` 字段必须**消失**——整行替换的代价与好处都在此。
    // 若实现改成深合并，`network: false` 会存活，本断言失败。
    let map = r.config.as_mapping().unwrap();
    assert!(
        map.get(&serde_yaml::Value::String("network".into())).is_none(),
        "整行替换必须丢弃未重述的字段（深合并会让 network 存活）：{:?}",
        r.config
    );
    assert_eq!(
        map.get(&serde_yaml::Value::String("timeout".into())).unwrap().as_i64(),
        Some(60)
    );
}

#[test]
fn origin_tracks_which_layer_won() {
    // 可检视性的关键：能回答"这行是谁定的"——深合并无法回答这一点
    let base = Layer::with_rows("bundled", vec![row("a", "p.a", "{x: 1}"), row("b", "p.b", "{x: 2}")]);
    let firm = Layer::with_rows("firm", vec![row("a", "p.a", "{x: 10}")]);
    let c = compose(&[base, firm]).unwrap();

    assert_eq!(c.origin_of("a"), Some("firm"), "a 被 firm 层覆盖");
    assert_eq!(c.origin_of("b"), Some("bundled"), "b 仍来自 bundled");
}

#[test]
fn history_records_all_overrides() {
    // 不只记最终值，还记全部覆盖历史——审计需要
    let l1 = Layer::with_rows("bundled", vec![row("a", "p.a", "{x: 1}")]);
    let l2 = Layer::with_rows("firm", vec![row("a", "p.a", "{x: 2}")]);
    let l3 = Layer::with_rows("cli", vec![row("a", "p.a", "{x: 3}")]);
    let c = compose(&[l1, l2, l3]).unwrap();

    assert_eq!(c.history.len(), 2, "应记录两次覆盖：{:?}", c.history);
    assert_eq!(c.history[0], ("a".to_string(), "firm".to_string()));
    assert_eq!(c.history[1], ("a".to_string(), "cli".to_string()));
    assert_eq!(c.origin_of("a"), Some("cli"));
}

#[test]
fn new_row_in_later_layer_is_appended() {
    let l1 = Layer::with_rows("bundled", vec![row("a", "p.a", "{x: 1}")]);
    let l2 = Layer::with_rows("firm", vec![row("b", "p.b", "{y: 2}")]);
    let c = compose(&[l1, l2]).unwrap();
    assert_eq!(c.rows.len(), 2);
    assert_eq!(c.origin_of("b"), Some("firm"));
}

#[test]
fn rows_are_sorted_by_id_for_determinism() {
    let l = Layer::with_rows(
        "bundled",
        vec![row("z", "p.z", "{}"), row("a", "p.a", "{}"), row("m", "p.m", "{}")],
    );
    let c = compose(&[l]).unwrap();
    let ids: Vec<&str> = c.rows.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "m", "z"], "应按 id 排序保证确定性");
}

#[test]
fn composition_is_deterministic() {
    let layers = vec![
        Layer::with_rows("bundled", vec![row("a", "p.a", "{x: 1}"), row("b", "p.b", "{y: 2}")]),
        Layer::with_rows("firm", vec![row("a", "p.a", "{x: 9}")]),
    ];
    let c1 = compose(&layers).unwrap();
    let c2 = compose(&layers).unwrap();
    assert_eq!(c1, c2);
}

#[test]
fn duplicate_row_id_within_layer_is_error() {
    // 层内重复 id：报错而非静默后者胜——静默会让"我明明写了两行"变成难查的错误
    let l = Layer::with_rows("bundled", vec![row("a", "p.a", "{x: 1}"), row("a", "p.a", "{x: 2}")]);
    let err = compose(&[l]).expect_err("层内重复 id 必须报错");
    assert!(matches!(err, PlaybookError::DuplicateRowId { .. }), "实得 {err:?}");
}

#[test]
fn empty_plugin_name_rejected() {
    let l = Layer::with_rows("bundled", vec![Row::new("a", "")]);
    assert!(matches!(
        compose(&[l]),
        Err(PlaybookError::EmptyPlugin { .. })
    ));
}

#[test]
fn empty_result_rejected() {
    assert!(matches!(compose(&[]), Err(PlaybookError::EmptyResult)));
    assert!(matches!(
        compose(&[Layer::new("empty")]),
        Err(PlaybookError::EmptyResult)
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// 平台条件（替代 dsh 的 !!js）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn platform_condition_filters_rows() {
    let mut mac_only = row("m", "p.m", "{}");
    mac_only.platform = Some("macos".into());
    let mut win_only = row("w", "p.w", "{}");
    win_only.platform = Some("windows".into());
    let always = row("a", "p.a", "{}");

    let l = Layer::with_rows("bundled", vec![mac_only, win_only, always]);
    let c = compose(&[l]).unwrap();

    let on_mac: Vec<&str> = c.active_on("macos").iter().map(|r| r.id.as_str()).collect();
    assert_eq!(on_mac, vec!["a", "m"], "macos 上应生效 a 与 m：{on_mac:?}");

    let on_win: Vec<&str> = c.active_on("windows").iter().map(|r| r.id.as_str()).collect();
    assert_eq!(on_win, vec!["a", "w"]);
}

#[test]
fn disabled_row_is_inactive_but_still_present() {
    // 禁用行仍参与替换（可被上层重新启用），只是不生效——
    // 这与"删除该行"不同：后者会让上层无法启用它
    let mut r = row("x", "p.x", "{}");
    r.disabled = true;
    let l = Layer::with_rows("bundled", vec![r, row("y", "p.y", "{}")]);
    let c = compose(&[l]).unwrap();

    assert_eq!(c.rows.len(), 2, "禁用行仍应在配置中（可被上层启用）");
    assert_eq!(c.active_on("macos").len(), 1, "只有 y 生效");
}

#[test]
fn later_layer_can_reenable_disabled_row() {
    let mut disabled = row("x", "p.x", "{}");
    disabled.disabled = true;
    let l1 = Layer::with_rows("bundled", vec![disabled]);
    let l2 = Layer::with_rows("firm", vec![row("x", "p.x", "{}")]); // 完整重述并启用
    let c = compose(&[l1, l2]).unwrap();
    assert_eq!(c.active_on("macos").len(), 1, "上层可重新启用");
}

// ─────────────────────────────────────────────────────────────────────────────
// 剧本解析与校验
// ─────────────────────────────────────────────────────────────────────────────

const PRICING_YAML: &str = r#"
meta:
  id: pricing_response
  name: 调价/促销响应
  description: 差异化 Bertrand + 重复博弈
  order: 10
  version: 1
applicable_when:
  - 决策涉及本企业对最终顾客的价格或促销力度
  - 相同或邻近商圈存在 1–3 个可观察价格的直接竞对
fails_when:
  - 强容量约束（此时 Bertrand 结论失真，转 Cournot 数值路径）
required_inputs: [本企业当前价格带, 主要竞对名单]
value_net:
  priority: [substitutor, customer, supplier]
  scan_depth:
    substitutor: deep
    customer: medium
    supplier: shallow
    complementor: skip
intake:
  - id: q1
    text: "如果对手下个月在你同商圈降价 15%，你会？"
    kind: choice
    options: ["跟着降", "加大促销", "提升服务差异化", "按兵不动"]
models: [differentiated_bertrand, repeated_game]
selectors:
  risk_dom: 0.3
  stochastic: 0.3
  behavioral: 0.2
  focal_llm: 0.2
tool_allowlist: [game_solve, equilibrium_select, web_search]
report_vars:
  title: 调价响应分析
"#;

#[test]
fn pricing_playbook_parses_from_yaml() {
    // 用设计文档 §13.3 的实际 YAML 形态验证解析
    let p = Playbook::from_yaml(PRICING_YAML).expect("应能解析 §13.3 的剧本");
    assert_eq!(p.meta.id, "pricing_response");
    assert_eq!(p.meta.name, "调价/促销响应");
    assert_eq!(p.applicable_when.len(), 2);
    assert_eq!(p.fails_when.len(), 1);
    assert_eq!(p.intake.len(), 1);
    assert_eq!(p.intake[0].options.len(), 4);
    assert!(p.validate().is_ok());
}

#[test]
fn scan_depth_parses_all_variants() {
    let p = Playbook::from_yaml(PRICING_YAML).unwrap();
    let vn = p.value_net.expect("应有价值网规格");
    assert_eq!(vn.scan_depth.get("substitutor"), Some(&ScanDepth::Deep));
    assert_eq!(vn.scan_depth.get("customer"), Some(&ScanDepth::Medium));
    assert_eq!(vn.scan_depth.get("supplier"), Some(&ScanDepth::Shallow));
    assert_eq!(vn.scan_depth.get("complementor"), Some(&ScanDepth::Skip));
}

#[test]
fn playbook_yaml_round_trip() {
    let p = Playbook::from_yaml(PRICING_YAML).unwrap();
    let y = p.to_yaml().unwrap();
    let back = Playbook::from_yaml(&y).unwrap();
    assert_eq!(p, back, "序列化往返不得丢字段");
}

#[test]
fn choice_question_without_options_rejected() {
    let bad = r#"
meta: {id: t, name: T}
applicable_when: [x]
fails_when: [y]
intake:
  - id: q1
    text: "选一个"
    kind: choice
"#;
    let p = Playbook::from_yaml(bad).unwrap();
    let err = p.validate().expect_err("choice 型问题无选项必须报错");
    assert!(format!("{err}").contains("无从选择"), "实得 {err}");
}

#[test]
fn duplicate_intake_question_id_rejected() {
    let bad = r#"
meta: {id: t, name: T}
applicable_when: [x]
fails_when: [y]
intake:
  - {id: q1, text: "A", kind: text}
  - {id: q1, text: "B", kind: text}
"#;
    let p = Playbook::from_yaml(bad).unwrap();
    assert!(matches!(
        p.validate(),
        Err(PlaybookError::DuplicateRowId { .. })
    ));
}

#[test]
fn invalid_selector_weight_rejected() {
    let bad = r#"
meta: {id: t, name: T}
applicable_when: [x]
fails_when: [y]
selectors:
  risk_dom: -0.5
"#;
    let p = Playbook::from_yaml(bad).unwrap();
    let err = p.validate().expect_err("负权重必须报错");
    assert!(format!("{err}").contains("risk_dom"), "实得 {err}");
}

#[test]
fn applicable_without_fails_when_rejected() {
    // 声明"何时该用"却不声明"何时不该用"——后者同等重要
    let bad = r#"
meta: {id: t, name: T}
applicable_when: [适用]
"#;
    let p = Playbook::from_yaml(bad).unwrap();
    let err = p.validate().expect_err("缺 fails_when 必须报错");
    assert!(format!("{err}").contains("不该用"), "实得 {err}");
}

#[test]
fn tool_allowlist_is_enforced() {
    let p = Playbook::from_yaml(PRICING_YAML).unwrap();
    assert!(p.allows_tool("game_solve"));
    assert!(p.allows_tool("web_search"));
    assert!(
        !p.allows_tool("exec_command"),
        "编码工具不应在咨询剧本的白名单内"
    );
    assert!(!p.allows_tool("python_sidecar_exec"));
}

#[test]
fn malformed_yaml_reports_error_not_panic() {
    let err = Playbook::from_yaml("meta: {id: t, name: T\n  bad indent ]").unwrap_err();
    assert!(matches!(err, PlaybookError::Yaml(_)), "实得 {err:?}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 剧本遮蔽（rank 规则）
// ─────────────────────────────────────────────────────────────────────────────

fn minimal_pb(id: &str, name: &str) -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: id.to_string(),
            name: name.to_string(),
            description: String::new(),
            order: 0,
            version: 1,
        },
        applicable_when: vec![],
        fails_when: vec![],
        required_inputs: vec![],
        value_net: None,
        intake: vec![],
        models: vec![],
        selectors: BTreeMap::new(),
        tool_allowlist: vec![],
        report_vars: BTreeMap::new(),
    }
}

#[test]
fn project_playbook_shadows_bundled() {
    // rank 越小越优先：Project(100) > User(400) > Bundled(600)
    let candidates = vec![
        (PlaybookRoot::Bundled, minimal_pb("pricing_response", "内置版")),
        (PlaybookRoot::Project, minimal_pb("pricing_response", "项目定制版")),
    ];
    let sel = select_playbook(&candidates, "pricing_response").unwrap();
    assert_eq!(sel.playbook.meta.name, "项目定制版", "项目级应遮蔽内置");
    assert_eq!(sel.root, PlaybookRoot::Project);
    assert_eq!(sel.shadowed, vec![PlaybookRoot::Bundled], "应记录被遮蔽者");
}

#[test]
fn user_playbook_shadows_bundled_but_not_project() {
    let candidates = vec![
        (PlaybookRoot::Bundled, minimal_pb("x", "内置")),
        (PlaybookRoot::User, minimal_pb("x", "用户")),
    ];
    let sel = select_playbook(&candidates, "x").unwrap();
    assert_eq!(sel.playbook.meta.name, "用户");
    assert!(sel.shadowed.contains(&PlaybookRoot::Bundled));
    assert!(!sel.shadowed.contains(&PlaybookRoot::Project));
}

#[test]
fn select_returns_none_for_unknown_id() {
    let candidates = vec![(PlaybookRoot::Bundled, minimal_pb("a", "A"))];
    assert!(select_playbook(&candidates, "nonexistent").is_none());
}

#[test]
fn rank_ordering_matches_design() {
    // 与 §13.7 的 rank 表同构（越小越优先）
    assert!(PlaybookRoot::Project.rank() < PlaybookRoot::User.rank());
    assert!(PlaybookRoot::User.rank() < PlaybookRoot::Bundled.rank());
    assert_eq!(PlaybookRoot::Bundled.rank(), 600);
}

#[test]
fn multiple_shadowed_roots_all_recorded() {
    let candidates = vec![
        (PlaybookRoot::Bundled, minimal_pb("x", "内置")),
        (PlaybookRoot::User, minimal_pb("x", "用户")),
        (PlaybookRoot::Project, minimal_pb("x", "项目")),
    ];
    let sel = select_playbook(&candidates, "x").unwrap();
    assert_eq!(sel.playbook.meta.name, "项目");
    assert_eq!(sel.shadowed.len(), 2, "应记录两个被遮蔽根：{:?}", sel.shadowed);
}

//! Issue #6 (`report` / `kb`) 与 Issue #7 (`start` / 持久化工作区) 端到端集成测试。
//!
//! 全部走真实编译产物 `env!("CARGO_BIN_EXE_shangyi")` 与临时 `SHANGYI_HOME`，不触碰真实 `~/.shangyi`。

use std::path::PathBuf;
use std::process::Command;

use shangyi_cli::{parse, Command as CliCommand, KbOp};
use shangyi_cli::start_cmd::ensure_persistent_workspace;
use shangyi_report::{Number, ProvenanceId, ProvenanceKind, ProvenanceRecord, Report, Section};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_shangyi")
}

fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "shangyi-cli-e2e-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn args(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn parse_supports_start_ui_and_kb_show_list() {
    assert_eq!(
        parse(&args(&["start"])).unwrap(),
        CliCommand::Start {
            port: None,
            no_open: false
        }
    );
    assert_eq!(
        parse(&args(&["ui", "--port", "9001", "--no-open"])).unwrap(),
        CliCommand::Start {
            port: Some(9001),
            no_open: true
        }
    );
    assert!(parse(&args(&["start", "--port", "0"])).is_err());
    assert!(parse(&args(&["start", "--bogus"])).is_err());

    assert_eq!(
        parse(&args(&["kb", "show", "gamespec-build"])).unwrap(),
        CliCommand::Kb {
            op: KbOp::Show {
                id: "gamespec-build".into()
            }
        }
    );
    assert_eq!(
        parse(&args(&["kb", "list"])).unwrap(),
        CliCommand::Kb { op: KbOp::List }
    );
    assert!(!parse(&args(&["kb", "show", "gamespec-build"])).unwrap().is_mutating());
    assert!(!parse(&args(&["kb", "list"])).unwrap().is_mutating());
}

#[test]
fn kb_list_search_show_and_add_work_end_to_end() {
    let home = tmp_dir("kb");

    let list_out = Command::new(bin())
        .args(["kb", "list"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        list_out.status.success(),
        "kb list stderr: {}",
        String::from_utf8_lossy(&list_out.stderr)
    );
    let list_stdout = String::from_utf8_lossy(&list_out.stdout);
    assert!(list_stdout.contains("35 篇"), "应包含全部 35 篇内置知识库：{list_stdout}");

    let search_out = Command::new(bin())
        .args(["kb", "search", "差异化", "价格竞争"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(search_out.status.success());
    let search_stdout = String::from_utf8_lossy(&search_out.stdout);
    assert!(
        search_stdout.contains("differentiated-bertrand"),
        "应命中 differentiated-bertrand：{search_stdout}"
    );
    assert!(!search_stdout.contains("还没实装"));

    let show_out = Command::new(bin())
        .args(["kb", "show", "gamespec_build.md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(show_out.status.success());
    let show_stdout = String::from_utf8_lossy(&show_out.stdout);
    assert!(show_stdout.contains("gamespec-build"));

    let bad_show = Command::new(bin())
        .args(["kb", "show", "definitely-nonexistent-entry"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(!bad_show.status.success());

    // 导入有效扩展知识页，且不遮蔽内置 35 篇
    let custom_md = home.join("custom-note.md");
    std::fs::write(
        &custom_md,
        "---\nid: tea-margin-check\nname: 茶饮毛利复核\nkind: method\nsummary: 用于测算加盟店毛利边界\ntags: [茶饮, 毛利]\n---\n\n正文内容。\n",
    )
    .unwrap();
    let add_ok = Command::new(bin())
        .args(["kb", "add", custom_md.to_str().unwrap()])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        add_ok.status.success(),
        "kb add stderr: {}",
        String::from_utf8_lossy(&add_ok.stderr)
    );

    let show_added = Command::new(bin())
        .args(["kb", "show", "tea-margin-check"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(show_added.status.success());
    assert!(String::from_utf8_lossy(&show_added.stdout).contains("茶饮毛利复核"));

    // Built-in page must STILL be accessible after adding user KB entry!
    let show_builtin_still = Command::new(bin())
        .args(["kb", "show", "hotelling-location"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(show_builtin_still.status.success());

    // Malformed KB file (missing id) must fail without leaving junk behind
    let bad_md = home.join("bad.md");
    std::fs::write(&bad_md, "---\nname: 缺id\n---\n\n内容。\n").unwrap();
    let add_bad = Command::new(bin())
        .args(["kb", "add", bad_md.to_str().unwrap()])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(!add_bad.status.success());

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn report_renders_engine_artifacts_verifies_provenance_and_rejects_empty_or_exploratory_cases() {
    let home = tmp_dir("report");
    let case_dir = home.join("cases/2026-11-14/case-20261114-scenario-demo");
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::write(
        case_dir.join("header.json"),
        serde_json::json!({
            "case_id": "case-20261114-scenario-demo",
            "mode": "scenario",
            "playbook_id": "pricing_response",
            "created_at": "2026-11-14T08:00:00Z",
            "schema_version": 1
        })
        .to_string(),
    )
    .unwrap();

    // Build a valid shangyi_report::Report JSON for report_render output
    let mut report_obj = Report::new("case-20261114-scenario-demo");
    let prov = ProvenanceRecord::new("prov_dist", ProvenanceKind::CodeComputed, 0).with_code(
        "shangyi-selection",
        "0.1.0",
        "fuse",
    );
    report_obj.register_provenance(prov).unwrap();
    report_obj
        .add_number(
            Number::new(
                "prob_top",
                0.74,
                "均衡 eq_keep 概率",
                ProvenanceId("prov_dist".into()),
            )
            .with_unit("概率"),
        )
        .unwrap();
    let mut sec = Section::new("boss_page", "一页结论")
        .with_paragraph("最可能的均衡是「eq_keep」，概率 74.0%。");
    sec.number_keys.push("prob_top".to_string());
    report_obj.add_section(sec);
    report_obj
        .check_provenance()
        .expect("fixture report should satisfy provenance invariant");
    let valid_report = serde_json::to_value(&report_obj).unwrap();

    let events = vec![
        serde_json::json!({"seq": 1, "event": {"type": "task_scope_selected", "scope": "consultation", "reason": "定价响应推演", "at": "2026-11-14T08:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "phase_entered", "from": "intake", "to": "model_build", "playbook": "pricing_response", "reason": "done", "at": "2026-11-14T08:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "checkpoint_opened", "id": "cp2", "phase": "model_build", "items": ["确认毛利率与客单价"], "at": "2026-11-14T08:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "checkpoint_answered", "id": "cp2", "answers": ["确认：毛利率按公开报价与我方台账"], "user_actor": "user", "at": "2026-11-14T08:00:04Z"}}),
        // Exploratory sandbox call whose number 999888 MUST NOT enter report findings
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_sb", "name": "sandbox_exec", "arguments": "{\"code\":\"print(999888)\"}", "at": "2026-11-14T08:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "tool_call_finished", "call_id": "c_sb", "outcome": {"status": "ok", "output": "999888"}, "at": "2026-11-14T08:00:06Z"}}),
        // GameSpec build
        serde_json::json!({"seq": 7, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 2}, "call_id": "c_gs", "name": "gamespec_build", "arguments": "{\"game_id\":\"g1\",\"players\":[{\"id\":\"us\",\"name\":\"我方\",\"vn_role\":\"focal\",\"relation_to_game\":\"player\"},{\"id\":\"comp\",\"name\":\"对手\",\"vn_role\":\"substitutor\",\"relation_to_game\":\"player\"}],\"strategies\":{\"us\":[\"稳价\",\"降价\"],\"comp\":[\"跟随\",\"不动\"]},\"params\":{\"margin\":{\"name\":\"单杯毛利\",\"value\":{\"kind\":\"literal\",\"value\":12.5},\"source\":{\"kind\":\"user_confirmed\",\"checkpoint\":\"cp2\"},\"unit\":\"元\"}}}", "at": "2026-11-14T08:00:07Z"}}),
        serde_json::json!({"seq": 8, "event": {"type": "tool_call_finished", "call_id": "c_gs", "outcome": {"status": "ok", "output": "{\"game_id\":\"g1\"}"}, "at": "2026-11-14T08:00:08Z"}}),
        // Business cost with one null item (must show 未取得, never 0)
        serde_json::json!({"seq": 9, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 3}, "call_id": "c_cost", "name": "business_cost", "arguments": "{\"scenario\":\"方案A\"}", "at": "2026-11-14T08:00:09Z"}}),
        serde_json::json!({"seq": 10, "event": {"type": "tool_call_finished", "call_id": "c_cost", "outcome": {"status": "ok", "output": "{\"scenario_id\":\"s_a\",\"scenario\":\"方案A\",\"currency\":\"CNY\",\"listed_upfront_cash\":150000,\"listed_upfront_cash_expense\":120000,\"listed_refundable_deposits\":30000,\"listed_monthly_fixed_cost\":18000,\"amounts_complete\":false,\"cash_classification_complete\":true,\"costs_complete\":false,\"items\":[{\"name\":\"月租\",\"treatment\":\"cash_expense\",\"normalized_period\":\"month\",\"normalized_amount\":18000,\"source\":\"房东报价\"},{\"name\":\"转让费\",\"treatment\":\"cash_expense\",\"normalized_period\":\"once\",\"normalized_amount\":null,\"source\":\"待核对\",\"missing_amount_reason\":\"尚未议价确认\"}],\"note\":\"仅计已知现金支出\"}"}, "at": "2026-11-14T08:00:10Z"}}),
        // Recommend build
        serde_json::json!({"seq": 11, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 4}, "call_id": "c_rec", "name": "recommend_build", "arguments": "{}", "at": "2026-11-14T08:00:11Z"}}),
        serde_json::json!({"seq": 12, "event": {"type": "tool_call_finished", "call_id": "c_rec", "outcome": {"status": "ok", "output": "{\"headline\":\"优先稳价并强化复购，不主动发起九块九价格战\",\"actions\":[{\"action\":\"维持主推款原价 18 元\",\"rationale\":\"均衡 eq_keep 概率 74%\"}],\"do_not\":[\"避免无底线补贴\"],\"caveats\":[\"以上基于 cp2 核定毛利\"] }"}, "at": "2026-11-14T08:00:12Z"}}),
        // Report render
        serde_json::json!({"seq": 13, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 5}, "call_id": "c_rep", "name": "report_render", "arguments": "{}", "at": "2026-11-14T08:00:13Z"}}),
        serde_json::json!({"seq": 14, "event": {"type": "tool_call_finished", "call_id": "c_rep", "outcome": {"status": "ok", "output": serde_json::json!({"report": valid_report, "sections": 1, "provenance_records": 1}).to_string()}, "at": "2026-11-14T08:00:14Z"}}),
    ];

    let log_content = events
        .into_iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(case_dir.join("case.jsonl"), log_content).unwrap();

    // Test via `latest` inside SHANGYI_HOME
    let out = Command::new(bin())
        .args(["report", "latest", "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "report stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("优先稳价并强化复购"));
    assert!(md.contains("18000"));
    assert!(md.contains("尚未议价确认") && md.contains("未取得"));
    assert!(md.contains("用户已确认"));
    assert!(md.contains("sandbox_exec") && md.contains("隔离排除"));
    assert!(!md.contains("999888"), "探索性沙箱结果不得进入报告：{md}");
    assert!(md.contains("prov_dist") && md.contains("prob_top"), "missing provenance info in md:\n{md}");

    // Test direct file path argument (`case.jsonl`)
    let out_file = Command::new(bin())
        .args(["report", case_dir.join("case.jsonl").to_str().unwrap(), "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(out_file.status.success());

    // Default format omits --format and prints Markdown to stdout
    let out_default = Command::new(bin())
        .args(["report", "latest"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        out_default.status.success(),
        "default report stderr: {}",
        String::from_utf8_lossy(&out_default.stderr)
    );
    assert!(String::from_utf8_lossy(&out_default.stdout).contains("优先稳价并强化复购"));

    // PDF format must honestly refuse without pandoc on PATH (no fake pdf binary output)
    let out_pdf = Command::new(bin())
        .args(["report", "latest", "--format", "pdf"])
        .env("SHANGYI_HOME", &home)
        .env("PATH", "/nonexistent-bin-dir")
        .current_dir(&home)
        .output()
        .unwrap();
    assert!(!out_pdf.status.success());
    assert!(String::from_utf8_lossy(&out_pdf.stderr).contains("--format md"));

    // Exploratory-only case must be refused per Iron Law 1
    let bad_case = home.join("cases/2026-11-14/case-exploratory-only");
    std::fs::create_dir_all(&bad_case).unwrap();
    std::fs::write(
        bad_case.join("case.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"seq": 1, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c1", "name": "sandbox_exec", "arguments": "{}", "at": "2026-11-14T08:00:00Z"}}),
            serde_json::json!({"seq": 2, "event": {"type": "tool_call_finished", "call_id": "c1", "outcome": {"status": "ok", "output": "42"}, "at": "2026-11-14T08:00:01Z"}}),
        ),
    )
    .unwrap();
    let refused = Command::new(bin())
        .args(["report", bad_case.to_str().unwrap(), "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("TrustLevel::Engine"));

    // Rewound phase must clear stale downstream solve/recommend, keeping earlier valid business_cost
    let rewind_case = home.join("cases/2026-11-14/case-rewind-and-cost");
    std::fs::create_dir_all(&rewind_case).unwrap();
    let rewind_log = [
        serde_json::json!({"seq": 1, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_cost", "name": "business_cost", "arguments": "{}", "at": "2026-11-14T08:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "tool_call_finished", "call_id": "c_cost", "outcome": {"status": "ok", "output": "{\"scenario_id\":\"s_b\",\"scenario\":\"保底方案\",\"currency\":\"CNY\",\"listed_upfront_cash\":50000,\"listed_upfront_cash_expense\":50000,\"listed_refundable_deposits\":0,\"listed_monthly_fixed_cost\":8000,\"amounts_complete\":false,\"cash_classification_complete\":true,\"costs_complete\":false,\"items\":[{\"name\":\"人工\",\"treatment\":\"cash_expense\",\"normalized_period\":\"month\",\"normalized_amount\":null,\"source\":\"待定\",\"missing_amount_reason\":\"尚未排班\"}]}"}, "at": "2026-11-14T08:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 2}, "call_id": "c_solve", "name": "solve_equilibria", "arguments": "{}", "at": "2026-11-14T08:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "tool_call_finished", "call_id": "c_solve", "outcome": {"status": "ok", "output": "{\"completeness\":\"exact\",\"route\":\"support_enum\",\"equilibria\":[{\"id\":\"eq_stale_999\",\"payoffs\":{\"us\":99}}]}"}, "at": "2026-11-14T08:00:04Z"}}),
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 3}, "call_id": "c_rec", "name": "recommend_build", "arguments": "{}", "at": "2026-11-14T08:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "tool_call_finished", "call_id": "c_rec", "outcome": {"status": "ok", "output": "{\"headline\":\"陈旧过期建议MUST_NOT_APPEAR\",\"actions\":[]}"}, "at": "2026-11-14T08:00:06Z"}}),
        serde_json::json!({"seq": 7, "event": {"type": "phase_rewound", "from": "deliver", "to": "model_build", "due_to": "用户修正参数", "at": "2026-11-14T08:00:07Z"}}),
    ]
    .into_iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    std::fs::write(rewind_case.join("case.jsonl"), rewind_log).unwrap();
    let out_rw = Command::new(bin())
        .args(["report", rewind_case.to_str().unwrap(), "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(out_rw.status.success(), "rewind stderr: {}", String::from_utf8_lossy(&out_rw.stderr));
    let rw_md = String::from_utf8_lossy(&out_rw.stdout);
    assert!(rw_md.contains("保底方案") && rw_md.contains("尚未排班") && rw_md.contains("未取得"));
    assert!(!rw_md.contains("eq_stale_999"), "phase_rewound must clear downstream solve");
    assert!(!rw_md.contains("MUST_NOT_APPEAR"), "phase_rewound must clear downstream recommend");

    // Broken provenance report_render must fail check_provenance gate
    let mut bad_report = Report::new("case-bad-prov");
    bad_report.numbers.insert(
        "x".into(),
        Number::new("x", 1.0, "broken", ProvenanceId("missing_id".into())),
    );
    let bad_prov_case = home.join("cases/2026-11-14/case-bad-prov");
    std::fs::create_dir_all(&bad_prov_case).unwrap();
    std::fs::write(
        bad_prov_case.join("case.jsonl"),
        format!(
            "{}\n{}\n",
            serde_json::json!({"seq": 1, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_r", "name": "report_render", "arguments": "{}", "at": "2026-11-14T08:00:01Z"}}),
            serde_json::json!({"seq": 2, "event": {"type": "tool_call_finished", "call_id": "c_r", "outcome": {"status": "ok", "output": serde_json::json!({"report": bad_report}).to_string()}, "at": "2026-11-14T08:00:02Z"}}),
        ),
    )
    .unwrap();
    let bad_prov_out = Command::new(bin())
        .args(["report", bad_prov_case.to_str().unwrap(), "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(!bad_prov_out.status.success());
    assert!(String::from_utf8_lossy(&bad_prov_out.stderr).contains("溯源校验"));

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn start_initializes_persistent_workspace_and_fails_loudly_without_dsh() {
    let home = tmp_dir("start");
    let prev = std::env::var_os("SHANGYI_HOME");
    // Use process subprocess so env mutation stays isolated to child
    let out = Command::new(bin())
        .args(["start", "--no-open", "--port", "8799"])
        .env("SHANGYI_HOME", &home)
        .env("PATH", "/nonexistent-bin-dir")
        .output()
        .unwrap();
    assert!(!out.status.success(), "missing dsh must fail loudly");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("dsh"), "expected clear dsh hint: {stderr}");
    assert!(home.join("cases").is_dir(), "cases workspace should persist under SHANGYI_HOME");
    assert!(home.join("runtime").is_dir());
    assert!(home.join("kb").is_dir());
    let _ = prev;
    let _ = ensure_persistent_workspace;
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
fn write_exec_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    let mut perms = std::fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).unwrap();
    path
}

fn write_case_fixture(
    home: &std::path::Path,
    date: &str,
    case_id: &str,
    events: Vec<serde_json::Value>,
) -> PathBuf {
    let case_dir = home.join("cases").join(date).join(case_id);
    std::fs::create_dir_all(&case_dir).unwrap();
    std::fs::write(
        case_dir.join("header.json"),
        serde_json::json!({
            "case_id": case_id,
            "mode": "scenario",
            "playbook_id": "pricing_response",
            "created_at": "2026-11-14T08:00:00Z",
            "schema_version": 1
        })
        .to_string(),
    )
    .unwrap();
    let content = events
        .into_iter()
        .map(|e| e.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(case_dir.join("case.jsonl"), content).unwrap();
    case_dir
}

fn minimal_engine_events(case_id: &str, headline: &str) -> Vec<serde_json::Value> {
    let mut rep = Report::new(case_id);
    rep.register_provenance(
        ProvenanceRecord::new("prov_solve", ProvenanceKind::CodeComputed, 0).with_code(
            "shangyi-solver",
            "0.1.0",
            "solve",
        ),
    )
    .unwrap();
    rep.add_number(
        Number::new("cost_m", 18000.0, "月租固定成本", ProvenanceId("prov_solve".into()))
            .with_unit("元/月"),
    )
    .unwrap();
    let mut sec = Section::new("boss_page", "一页结论").with_paragraph(headline);
    sec.number_keys.push("cost_m".into());
    rep.add_section(sec);
    rep.check_provenance().unwrap();
    let rep_val = serde_json::to_value(&rep).unwrap();

    vec![
        serde_json::json!({"seq": 1, "event": {"type": "task_scope_selected", "scope": "consultation", "reason": "定价推演", "at": "2026-11-14T08:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_cost", "name": "business_cost", "arguments": "{\"scenario\":\"方案A\"}", "at": "2026-11-14T08:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "tool_call_finished", "call_id": "c_cost", "outcome": {"status": "ok", "output": "{\"scenario_id\":\"s_a\",\"scenario\":\"方案A\",\"currency\":\"CNY\",\"listed_upfront_cash\":120000,\"listed_upfront_cash_expense\":120000,\"listed_refundable_deposits\":0,\"listed_monthly_fixed_cost\":18000,\"amounts_complete\":true,\"cash_classification_complete\":true,\"costs_complete\":true,\"items\":[{\"name\":\"月租\",\"treatment\":\"cash_expense\",\"normalized_period\":\"month\",\"normalized_amount\":18000,\"source\":\"合同\"}]}"}, "at": "2026-11-14T08:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 2}, "call_id": "c_rec", "name": "recommend_build", "arguments": "{}", "at": "2026-11-14T08:00:04Z"}}),
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_finished", "call_id": "c_rec", "outcome": {"status": "ok", "output": serde_json::json!({"headline": headline, "actions": []}).to_string()}, "at": "2026-11-14T08:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 3}, "call_id": "c_rep", "name": "report_render", "arguments": "{}", "at": "2026-11-14T08:00:06Z"}}),
        serde_json::json!({"seq": 7, "event": {"type": "tool_call_finished", "call_id": "c_rep", "outcome": {"status": "ok", "output": serde_json::json!({"report": rep_val}).to_string()}, "at": "2026-11-14T08:00:07Z"}}),
    ]
}

#[test]
fn report_defaults_to_md_and_exports_md_to_file_atomically() {
    let home = tmp_dir("report-md-export");
    write_case_fixture(
        &home,
        "2026-11-14",
        "case-md-demo",
        minimal_engine_events("case-md-demo", "优先稳价并强化复购"),
    );

    // 省略 --format 与 -o：默认输出 Markdown 到 stdout
    let out_stdout = Command::new(bin())
        .args(["report", "latest"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        out_stdout.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_stdout.stderr)
    );
    let md_stdout = String::from_utf8_lossy(&out_stdout.stdout);
    assert!(md_stdout.contains("# 商奕决策报告：case-md-demo"));
    assert!(md_stdout.contains("18000"));
    assert!(md_stdout.contains("优先稳价并强化复购"));

    // 指定 -o 输出到文件：stdout 保持干净为空，文件完整落地
    let out_file = home.join("exports/sub/final-report.md");
    let out_saved = Command::new(bin())
        .args(["report", "latest", "-o", out_file.to_str().unwrap()])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        out_saved.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_saved.stderr)
    );
    assert!(
        out_saved.stdout.is_empty(),
        "指定 -o 导出 md 时 stdout 必须为空：{}",
        String::from_utf8_lossy(&out_saved.stdout)
    );
    assert!(String::from_utf8_lossy(&out_saved.stderr).contains("已导出 Markdown 报告"));
    let written = std::fs::read_to_string(&out_file).unwrap();
    assert_eq!(written, md_stdout.as_ref());

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn report_pdf_and_docx_without_pandoc_fail_cleanly_without_leaving_files() {
    let home = tmp_dir("report-no-pandoc");
    write_case_fixture(
        &home,
        "2026-11-14",
        "case-no-pandoc",
        minimal_engine_events("case-no-pandoc", "稳价策略"),
    );
    let empty_bin = home.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();

    for (fmt, fname) in [("pdf", "out.pdf"), ("docx", "out.docx")] {
        let target = home.join("out").join(fname);
        let res = Command::new(bin())
            .args([
                "report",
                "latest",
                "--format",
                fmt,
                "-o",
                target.to_str().unwrap(),
            ])
            .env("SHANGYI_HOME", &home)
            .env("PATH", &empty_bin)
            .output()
            .unwrap();
        assert!(!res.status.success(), "{fmt} 缺少 pandoc 必须失败");
        let err = String::from_utf8_lossy(&res.stderr);
        assert!(err.contains("pandoc"), "{fmt} 错误应提及 pandoc：{err}");
        assert!(err.contains("--format md"), "{fmt} 错误应给 md 降级路径：{err}");
        assert!(err.contains("shangyi start"), "{fmt} 错误应提及 shangyi start：{err}");
        assert!(!target.exists(), "不得生成伪造 `{}`", target.display());
        assert!(!target.parent().unwrap().exists(), "缺少 pandoc 时不应提前建出空输出目录");
    }

    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn report_pdf_with_pandoc_only_fails_before_invoking_pandoc() {
    let home = tmp_dir("report-no-pdf-engine");
    write_case_fixture(
        &home,
        "2026-11-14",
        "case-no-engine",
        minimal_engine_events("case-no-engine", "稳价策略"),
    );
    let fake_bin = home.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let sentinel = home.join("pandoc-was-invoked");
    write_exec_script(
        &fake_bin,
        "pandoc",
        &format!(": > \"{}\"\nexit 99", sentinel.display()),
    );

    let target = home.join("out/blocked.pdf");
    let res = Command::new(bin())
        .args([
            "report",
            "latest",
            "--format",
            "pdf",
            "-o",
            target.to_str().unwrap(),
        ])
        .env("SHANGYI_HOME", &home)
        .env("PATH", &fake_bin)
        .output()
        .unwrap();
    assert!(!res.status.success());
    let err = String::from_utf8_lossy(&res.stderr);
    assert!(err.contains("PDF 排版引擎"), "应明示缺少 PDF 引擎：{err}");
    assert!(err.contains("xelatex"), "应列出 xelatex：{err}");
    assert!(err.contains("--format md"), "应提示 --format md：{err}");
    assert!(!sentinel.exists(), "缺少 PDF 引擎时不得启动 pandoc");
    assert!(!target.exists(), "不得写伪造 pdf");

    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn report_pdf_and_docx_with_stub_pandoc_succeed_and_pass_verbatim_markdown() {
    let home = tmp_dir("report-stub-pandoc-ok");
    write_case_fixture(
        &home,
        "2026-11-14",
        "case-pandoc-ok",
        minimal_engine_events("case-pandoc-ok", "优先稳价并强化复购"),
    );

    let fake_bin = home.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let args_log = home.join("pandoc-args.log");
    let stdin_log = home.join("pandoc-stdin.md");
    write_exec_script(
        &fake_bin,
        "pandoc",
        &format!(
            r#"out=""
args_log="{args_log}"
stdin_log="{stdin_log}"
: > "$args_log"
while [ "$#" -gt 0 ]; do
  printf '%s\n' "$1" >> "$args_log"
  if [ "$1" = "-o" ]; then
    shift
    printf '%s\n' "$1" >> "$args_log"
    out="$1"
  fi
  shift
done
: > "$stdin_log"
while IFS= read -r line || [ -n "$line" ]; do
  printf '%s\n' "$line" >> "$stdin_log"
done
printf '%s' 'FAKE_DOC_BYTES' > "$out""#,
            args_log = args_log.display(),
            stdin_log = stdin_log.display()
        ),
    );

    // 1. DOCX：仅需 pandoc，且省略 -o 时在 cwd 生成 <case_id>.docx
    let work_cwd = home.join("work-cwd");
    std::fs::create_dir_all(&work_cwd).unwrap();
    let docx_res = Command::new(bin())
        .args(["report", "latest", "--format", "docx"])
        .env("SHANGYI_HOME", &home)
        .env("PATH", &fake_bin)
        .current_dir(&work_cwd)
        .output()
        .unwrap();
    assert!(
        docx_res.status.success(),
        "docx stderr: {}",
        String::from_utf8_lossy(&docx_res.stderr)
    );
    let default_docx = work_cwd.join("case-pandoc-ok.docx");
    assert_eq!(std::fs::read(&default_docx).unwrap(), b"FAKE_DOC_BYTES");
    let docx_args = std::fs::read_to_string(&args_log).unwrap();
    assert!(docx_args.contains("--from=markdown+pipe_tables+strikeout\n"));
    assert!(docx_args.contains("-t\ndocx\n"));
    let captured_md = std::fs::read_to_string(&stdin_log).unwrap();
    assert!(captured_md.contains("18000"));
    assert!(captured_md.contains("优先稳价并强化复购"));

    // 2. PDF：加上 xelatex 与 SHANGYI_PDF_FONT，显式 -o 到深层目录
    write_exec_script(&fake_bin, "xelatex", "exit 0");
    let pdf_target = home.join("nested/dir/report.pdf");
    let pdf_res = Command::new(bin())
        .args([
            "report",
            "latest",
            "--format",
            "pdf",
            "-o",
            pdf_target.to_str().unwrap(),
        ])
        .env("SHANGYI_HOME", &home)
        .env("PATH", &fake_bin)
        .env("SHANGYI_PDF_FONT", "PingFang SC")
        .output()
        .unwrap();
    assert!(
        pdf_res.status.success(),
        "pdf stderr: {}",
        String::from_utf8_lossy(&pdf_res.stderr)
    );
    assert_eq!(std::fs::read(&pdf_target).unwrap(), b"FAKE_DOC_BYTES");
    let pdf_args = std::fs::read_to_string(&args_log).unwrap();
    assert!(pdf_args.contains("-t\npdf\n"));
    assert!(pdf_args.contains("--pdf-engine=xelatex\n"));
    assert!(pdf_args.contains("CJKmainfont=PingFang SC\n"));
    assert!(pdf_args.contains("mainfont=PingFang SC\n"));

    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn report_pdf_rejects_failing_or_empty_pandoc_without_leaving_target_or_temp_files() {
    let home = tmp_dir("report-pandoc-fail-empty");
    write_case_fixture(
        &home,
        "2026-11-14",
        "case-pandoc-fail",
        minimal_engine_events("case-pandoc-fail", "稳价"),
    );

    let fake_bin = home.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    write_exec_script(&fake_bin, "xelatex", "exit 0");

    // Subcase A: pandoc writes partial bytes then exits 1
    write_exec_script(
        &fake_bin,
        "pandoc",
        r#"out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then shift; out="$1"; fi
  shift
done
[ -n "$out" ] && printf '%s' 'partial' > "$out"
printf '%s\n' 'latex error' >&2
exit 1"#,
    );
    let out_dir_a = home.join("out-a");
    let fail_target = out_dir_a.join("fail.pdf");
    let res_a = Command::new(bin())
        .args([
            "report",
            "latest",
            "--format",
            "pdf",
            "-o",
            fail_target.to_str().unwrap(),
        ])
        .env("SHANGYI_HOME", &home)
        .env("PATH", &fake_bin)
        .output()
        .unwrap();
    assert!(!res_a.status.success());
    let err_a = String::from_utf8_lossy(&res_a.stderr);
    assert!(err_a.contains("pandoc") && err_a.contains("--format md"), "err: {err_a}");
    assert!(!fail_target.exists());
    let leftovers_a: Vec<_> = std::fs::read_dir(&out_dir_a)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(leftovers_a.is_empty(), "失败后不得残留临时文件: {leftovers_a:?}");

    // Subcase B: pandoc exits 0 but writes 0-byte output
    write_exec_script(
        &fake_bin,
        "pandoc",
        r#"out=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "-o" ]; then shift; out="$1"; fi
  shift
done
[ -n "$out" ] && : > "$out"
exit 0"#,
    );
    let out_dir_b = home.join("out-b");
    let empty_target = out_dir_b.join("empty.pdf");
    let res_b = Command::new(bin())
        .args([
            "report",
            "latest",
            "--format",
            "pdf",
            "-o",
            empty_target.to_str().unwrap(),
        ])
        .env("SHANGYI_HOME", &home)
        .env("PATH", &fake_bin)
        .output()
        .unwrap();
    assert!(!res_b.status.success());
    assert!(!empty_target.exists());
    let leftovers_b: Vec<_> = std::fs::read_dir(&out_dir_b)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(leftovers_b.is_empty(), "空产物后不得残留临时文件: {leftovers_b:?}");

    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn report_pdf_rejects_stdout_dash_and_exploratory_before_running_pandoc() {
    let home = tmp_dir("report-iron-gates");
    write_case_fixture(
        &home,
        "2026-11-14",
        "case-valid",
        minimal_engine_events("case-valid", "稳价"),
    );
    write_case_fixture(
        &home,
        "2026-11-13",
        "case-exploratory",
        vec![
            serde_json::json!({"seq": 1, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c1", "name": "sandbox_exec", "arguments": "{}", "at": "2026-11-13T08:00:00Z"}}),
            serde_json::json!({"seq": 2, "event": {"type": "tool_call_finished", "call_id": "c1", "outcome": {"status": "ok", "output": "42"}, "at": "2026-11-13T08:00:01Z"}}),
        ],
    );

    let fake_bin = home.join("fake-bin");
    std::fs::create_dir_all(&fake_bin).unwrap();
    let sentinel = home.join("pandoc-invoked");
    write_exec_script(
        &fake_bin,
        "pandoc",
        &format!(": > \"{}\"\nexit 0", sentinel.display()),
    );
    write_exec_script(&fake_bin, "xelatex", "exit 0");

    // 1. --format pdf/docx with -o - must fail cleanly before pandoc and keep stdout empty
    for fmt in ["pdf", "docx"] {
        let dash = Command::new(bin())
            .args(["report", "case-valid", "--format", fmt, "-o", "-"])
            .env("SHANGYI_HOME", &home)
            .env("PATH", &fake_bin)
            .output()
            .unwrap();
        assert!(!dash.status.success());
        assert!(dash.stdout.is_empty());
        assert!(String::from_utf8_lossy(&dash.stderr).contains("标准输出"));
        assert!(!sentinel.exists(), "{fmt} `-o -` 不得调用 pandoc");
    }

    // 2. Exploratory-only case must fail Iron Law 1 gate BEFORE pandoc
    let bad_pdf = home.join("bad.pdf");
    let exp = Command::new(bin())
        .args([
            "report",
            "case-exploratory",
            "--format",
            "pdf",
            "-o",
            bad_pdf.to_str().unwrap(),
        ])
        .env("SHANGYI_HOME", &home)
        .env("PATH", &fake_bin)
        .output()
        .unwrap();
    assert!(!exp.status.success());
    assert!(String::from_utf8_lossy(&exp.stderr).contains("TrustLevel::Engine"));
    assert!(!sentinel.exists(), "仅有探索工具时不得触发 pandoc");
    assert!(!bad_pdf.exists());

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn report_gamespec_merge_true_updates_parameters_and_rewind_clears_checkpoints() {
    let home = tmp_dir("report-merge-and-rewind");
    let events_a = vec![
        serde_json::json!({"seq": 1, "event": {"type": "checkpoint_opened", "id": "cp2", "phase": "model_build", "items": ["核对客单价"], "at": "2026-11-14T08:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "checkpoint_answered", "id": "cp2", "answers": ["确认"], "user_actor": "user", "at": "2026-11-14T08:00:02Z"}}),
        // Initial gamespec_build sets k_aa_1 = 40
        serde_json::json!({"seq": 3, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "g1", "name": "gamespec_build", "arguments": "{\"game_id\":\"g1\",\"players\":[{\"id\":\"us\",\"name\":\"我方\",\"vn_role\":\"focal\",\"relation_to_game\":\"player\"}],\"strategies\":{\"us\":[\"A\"]},\"params\":{\"k_aa_1\":{\"name\":\"单杯定价\",\"value\":{\"kind\":\"literal\",\"value\":40},\"unit\":\"元\"}}}", "at": "2026-11-14T08:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "tool_call_finished", "call_id": "g1", "outcome": {"status": "ok", "output": "{\"game_id\":\"g1\"}"}, "at": "2026-11-14T08:00:04Z"}}),
        // Followup gamespec_build with merge=true (omits players/strategies) updates k_aa_1 = 42
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 2}, "call_id": "g2", "name": "gamespec_build", "arguments": "{\"merge\":true,\"params\":{\"k_aa_1\":{\"name\":\"单杯定价\",\"value\":{\"kind\":\"literal\",\"value\":42},\"unit\":\"元\"}}}", "at": "2026-11-14T08:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "tool_call_finished", "call_id": "g2", "outcome": {"status": "ok", "output": "{\"game_id\":\"g1\"}"}, "at": "2026-11-14T08:00:06Z"}}),
        // Solve reflecting updated value 42
        serde_json::json!({"seq": 7, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 3}, "call_id": "s1", "name": "solve_equilibria", "arguments": "{}", "at": "2026-11-14T08:00:07Z"}}),
        serde_json::json!({"seq": 8, "event": {"type": "tool_call_finished", "call_id": "s1", "outcome": {"status": "ok", "output": "{\"completeness\":\"exact\",\"route\":\"support_enum\",\"equilibria\":[{\"id\":\"eq_42\",\"payoffs\":{\"us\":42}}]}"}, "at": "2026-11-14T08:00:08Z"}}),
    ];
    write_case_fixture(&home, "2026-11-14", "case-merge-a", events_a);

    let out_a = Command::new(bin())
        .args(["report", "case-merge-a"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(out_a.status.success(), "stderr: {}", String::from_utf8_lossy(&out_a.stderr));
    let md_a = String::from_utf8_lossy(&out_a.stdout);
    let sec4 = md_a.split("## 四、").nth(1).expect("报告应含 §四");
    assert!(sec4.contains("42"), "§四 必须反映 merge 更新后的 42：\n{sec4}");
    assert!(!sec4.contains("| 40 "), "§四 不得残留旧参数 40：\n{sec4}");

    // Also test wrapped {"merge": true, "spec": {...}} + followup preservation + phase_rewound checkpoint invalidation
    let events_b = vec![
        serde_json::json!({"seq": 1, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_cost", "name": "business_cost", "arguments": "{}", "at": "2026-11-14T09:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "tool_call_finished", "call_id": "c_cost", "outcome": {"status": "ok", "output": "{\"scenario_id\":\"s_keep\",\"scenario\":\"延续测算\",\"currency\":\"CNY\",\"listed_upfront_cash\":10000,\"listed_upfront_cash_expense\":10000,\"listed_refundable_deposits\":0,\"listed_monthly_fixed_cost\":12000,\"amounts_complete\":true,\"cash_classification_complete\":true,\"costs_complete\":true,\"items\":[]}"}, "at": "2026-11-14T09:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "followup_task_started", "prompt": "顺便复核定价", "prior_scope": "consultation", "at": "2026-11-14T09:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "checkpoint_opened", "id": "cp2", "phase": "model_build", "items": ["核对假设"], "at": "2026-11-14T09:00:04Z"}}),
        serde_json::json!({"seq": 5, "event": {"type": "checkpoint_answered", "id": "cp2", "answers": ["确认旧版"], "user_actor": "user", "at": "2026-11-14T09:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "phase_rewound", "from": "solve", "to": "model_build", "due_to": "重设参数", "at": "2026-11-14T09:00:06Z"}}),
        serde_json::json!({"seq": 7, "event": {"type": "tool_call_started", "step": {"turn": 2, "step": 1}, "call_id": "g_init", "name": "gamespec_build", "arguments": "{\"game_id\":\"g2\",\"players\":[{\"id\":\"us\",\"name\":\"我方\",\"vn_role\":\"focal\",\"relation_to_game\":\"player\"}],\"strategies\":{\"us\":[\"A\"]},\"params\":{\"k_aa_1\":{\"name\":\"定价\",\"value\":{\"kind\":\"literal\",\"value\":40}}}}", "at": "2026-11-14T09:00:07Z"}}),
        serde_json::json!({"seq": 8, "event": {"type": "tool_call_finished", "call_id": "g_init", "outcome": {"status": "ok", "output": "{\"game_id\":\"g2\"}"}, "at": "2026-11-14T09:00:08Z"}}),
        serde_json::json!({"seq": 9, "event": {"type": "tool_call_started", "step": {"turn": 2, "step": 2}, "call_id": "g_wrap", "name": "gamespec_build", "arguments": "{\"merge\":true,\"spec\":{\"params\":{\"k_aa_1\":{\"name\":\"定价\",\"value\":{\"kind\":\"literal\",\"value\":45}}}}}", "at": "2026-11-14T09:00:09Z"}}),
        serde_json::json!({"seq": 10, "event": {"type": "tool_call_finished", "call_id": "g_wrap", "outcome": {"status": "ok", "output": "{\"game_id\":\"g2\"}"}, "at": "2026-11-14T09:00:10Z"}}),
    ];
    write_case_fixture(&home, "2026-11-14", "case-merge-b", events_b);

    let out_b = Command::new(bin())
        .args(["report", "case-merge-b"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(out_b.status.success(), "stderr: {}", String::from_utf8_lossy(&out_b.stderr));
    let md_b = String::from_utf8_lossy(&out_b.stdout);
    assert!(md_b.contains("45"), "wrapped spec merge 应生效：\n{md_b}");
    assert!(!md_b.contains("| 40 "), "wrapped merge 不得保留旧值 40：\n{md_b}");
    assert!(md_b.contains("延续测算") && md_b.contains("12000"), "非条件 followup 后成本精算应保留：\n{md_b}");
    assert!(
        md_b.contains("状态 `rewound`") && !md_b.contains("状态 `answered`"),
        "phase_rewound 到 model_build 后旧 cp2 必须标为 rewound：\n{md_b}"
    );

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn report_request_rewind_to_model_build_preserves_gamespec_for_merge_and_cp2() {
    let home = tmp_dir("report-request-rewind-merge");
    let events = vec![
        serde_json::json!({"seq": 1, "event": {"type": "checkpoint_opened", "id": "cp1", "phase": "intel", "items": ["对手列表"], "at": "2026-11-14T10:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "checkpoint_answered", "id": "cp1", "answers": ["确认"], "user_actor": "user", "at": "2026-11-14T10:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "phase_entered", "from": "intel", "to": "model_build", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T10:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "g1", "name": "gamespec_build", "arguments": "{\"game_id\":\"g_req_rw\",\"players\":[{\"id\":\"us\",\"name\":\"我方\",\"vn_role\":\"focal\",\"relation_to_game\":\"player\"}],\"strategies\":{\"us\":[\"稳价\",\"跟降\"]},\"params\":{\"k_base\":{\"name\":\"基准客单\",\"value\":{\"kind\":\"literal\",\"value\":32},\"unit\":\"元\",\"source\":{\"kind\":\"public_data\",\"provenance\":\"菜单\"}}}}", "at": "2026-11-14T10:00:04Z"}}),
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_finished", "call_id": "g1", "outcome": {"status": "ok", "output": "{\"game_id\":\"g_req_rw\"}"}, "at": "2026-11-14T10:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "checkpoint_opened", "id": "cp2", "phase": "model_build", "items": ["核对基准客单"], "at": "2026-11-14T10:00:06Z"}}),
        serde_json::json!({"seq": 7, "event": {"type": "checkpoint_answered", "id": "cp2", "answers": ["确认"], "user_actor": "user", "at": "2026-11-14T10:00:07Z"}}),
        serde_json::json!({"seq": 8, "event": {"type": "phase_entered", "from": "model_build", "to": "solve", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T10:00:08Z"}}),
        serde_json::json!({"seq": 9, "event": {"type": "tool_call_started", "step": {"turn": 2, "step": 1}, "call_id": "s1", "name": "solve_equilibria", "arguments": "{}", "at": "2026-11-14T10:00:09Z"}}),
        serde_json::json!({"seq": 10, "event": {"type": "tool_call_finished", "call_id": "s1", "outcome": {"status": "ok", "output": "{\"completeness\":\"exact\",\"route\":\"support_enum\",\"equilibria\":[{\"id\":\"eq_stale_rr\",\"payoffs\":{\"us\":32}}]}"}, "at": "2026-11-14T10:00:10Z"}}),
        serde_json::json!({"seq": 11, "event": {"type": "phase_entered", "from": "solve", "to": "select", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T10:00:11Z"}}),
        serde_json::json!({"seq": 12, "event": {"type": "checkpoint_opened", "id": "cp3", "phase": "select", "items": ["选择均衡"], "at": "2026-11-14T10:00:12Z"}}),
        // request_rewind(to=model_build) keeps model_build's gamespec and cp2 while clearing solve & cp3
        serde_json::json!({"seq": 13, "event": {"type": "phase_rewound", "from": "select", "to": "model_build", "due_to": "需要补充成本参数（用户确认：同意回退到 model_build 修改参数）", "at": "2026-11-14T10:00:13Z"}}),
        // Follow-up gamespec_build(merge=true) without players/strategies must merge cleanly onto preserved gamespec
        serde_json::json!({"seq": 14, "event": {"type": "tool_call_started", "step": {"turn": 3, "step": 1}, "call_id": "g2", "name": "gamespec_build", "arguments": "{\"merge\":true,\"params\":{\"k_base\":{\"source\":{\"kind\":\"user_confirmed\",\"checkpoint\":\"cp2\"}},\"k_margin\":{\"name\":\"边际毛利\",\"value\":{\"kind\":\"literal\",\"value\":18},\"unit\":\"元\",\"source\":{\"kind\":\"user_confirmed\",\"checkpoint\":\"cp2\"}}}}", "at": "2026-11-14T10:00:14Z"}}),
        serde_json::json!({"seq": 15, "event": {"type": "tool_call_finished", "call_id": "g2", "outcome": {"status": "ok", "output": "{\"game_id\":\"g_req_rw\"}"}, "at": "2026-11-14T10:00:15Z"}}),
        // New solve after merge so report has authoritative solve artifact
        serde_json::json!({"seq": 16, "event": {"type": "tool_call_started", "step": {"turn": 3, "step": 2}, "call_id": "s2", "name": "solve_equilibria", "arguments": "{}", "at": "2026-11-14T10:00:16Z"}}),
        serde_json::json!({"seq": 17, "event": {"type": "tool_call_finished", "call_id": "s2", "outcome": {"status": "ok", "output": "{\"completeness\":\"exact\",\"route\":\"support_enum\",\"equilibria\":[{\"id\":\"eq_fresh_18\",\"payoffs\":{\"us\":18}}]}"}, "at": "2026-11-14T10:00:17Z"}}),
    ];
    write_case_fixture(&home, "2026-11-14", "case-request-rewind-merge", events);

    let out = Command::new(bin())
        .args(["report", "case-request-rewind-merge"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let md = String::from_utf8_lossy(&out.stdout);
    assert!(md.contains("`k_base`") && md.contains("32"), "request_rewind(to=model_build) 必须保留原 gamespec 供 merge=true 使用：\n{md}");
    assert!(md.contains("`k_margin`") && md.contains("18"), "merge=true 新增参数应生效：\n{md}");
    assert!(!md.contains("eq_stale_rr"), "下游旧 solve 不得残留：\n{md}");
    assert!(md.contains("eq_fresh_18"), "新 solve 应保留：\n{md}");
    assert!(md.contains("`cp2`") && md.contains("状态 `answered`"), "request_rewind(to=model_build) 保留 cp2：\n{md}");
    assert!(md.contains("`cp3`") && md.contains("状态 `rewound`"), "request_rewind(to=model_build) 作废下游 cp3：\n{md}");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn new_without_api_key_leaves_no_orphan_dir_and_resume_latest_resolves() {
    let home = tmp_dir("new-and-resume-latest");
    let fake_user_home = home.join("user-home");
    let sh_home = home.join("shangyi-home");
    std::fs::create_dir_all(&fake_user_home).unwrap();
    std::fs::create_dir_all(&sh_home).unwrap();

    // 1. shangyi new without API key fails with exit 3 and leaves NO orphan case dir
    let new_out = Command::new(bin())
        .args(["new", "--mode", "scenario"])
        .env("HOME", &fake_user_home)
        .env("SHANGYI_HOME", &sh_home)
        .env("SHANGYI_PROVIDER", "deepseek")
        .env_remove("DEEPSEEK_API_KEY")
        .output()
        .unwrap();
    assert_eq!(
        new_out.status.code(),
        Some(3),
        "无密钥应以退出码 3 失败，stderr={}",
        String::from_utf8_lossy(&new_out.stderr)
    );
    let cases_dir = sh_home.join("cases");
    let has_any_case_dir = cases_dir.exists()
        && std::fs::read_dir(&cases_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|d| d.path().is_dir());
    assert!(!has_any_case_dir, "凭证失败不得留下空案例目录");

    // 2. shangyi resume latest resolves latest case cleanly
    write_case_fixture(
        &sh_home,
        "2026-11-13",
        "case-older",
        vec![serde_json::json!({"seq": 1, "event": {"type": "task_scope_selected", "scope": "consultation", "reason": "older", "at": "2026-11-13T08:00:00Z"}})],
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    write_case_fixture(
        &sh_home,
        "2026-11-14",
        "case-newer",
        vec![
            serde_json::json!({"seq": 1, "event": {"type": "task_scope_selected", "scope": "consultation", "reason": "newer", "at": "2026-11-14T08:00:00Z"}}),
            serde_json::json!({"seq": 2, "event": {"type": "phase_entered", "from": "intake", "to": "intel", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T08:00:01Z"}}),
        ],
    );

    let res_out = Command::new(bin())
        .args(["resume", "latest"])
        .env("SHANGYI_HOME", &sh_home)
        .output()
        .unwrap();
    assert!(
        res_out.status.success(),
        "resume latest stderr: {}",
        String::from_utf8_lossy(&res_out.stderr)
    );
    let stdout = String::from_utf8_lossy(&res_out.stdout);
    assert!(stdout.contains("case-newer"), "应解析最新案例 case-newer：{stdout}");
    assert!(stdout.contains("事件 = 2 条"), "应显示事件条数：{stdout}");

    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn report_conditional_followup_rewinds_answered_cp1_while_non_conditional_preserves() {
    let home = tmp_dir("report-cond-followup-cp1");

    // 1. Answered cp1 -> conditional_analysis -> followup_task_started must mark cp1 rewound
    //    and clear stale gamespec/solve/conditional while preserving business_cost evidence.
    let cond_events = vec![
        serde_json::json!({"seq": 1, "event": {"type": "task_scope_selected", "scope": "consultation", "reason": "定价推演", "at": "2026-11-14T11:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "phase_entered", "from": "intake", "to": "intel", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T11:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "checkpoint_opened", "id": "cp1", "phase": "intel", "items": ["核对竞对客单价"], "at": "2026-11-14T11:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "checkpoint_answered", "id": "cp1", "answers": ["确认：18元"], "user_actor": "user", "at": "2026-11-14T11:00:04Z"}}),
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_cost", "name": "business_cost", "arguments": "{}", "at": "2026-11-14T11:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "tool_call_finished", "call_id": "c_cost", "outcome": {"status": "ok", "output": "{\"scenario_id\":\"s_keep_cond\",\"scenario\":\"保留方案A\",\"currency\":\"CNY\",\"listed_upfront_cash\":60000,\"listed_upfront_cash_expense\":60000,\"listed_refundable_deposits\":0,\"listed_monthly_fixed_cost\":15000,\"amounts_complete\":true,\"cash_classification_complete\":true,\"costs_complete\":true,\"items\":[]}"}, "at": "2026-11-14T11:00:06Z"}}),
        serde_json::json!({"seq": 7, "event": {"type": "phase_entered", "from": "intel", "to": "model_build", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T11:00:07Z"}}),
        serde_json::json!({"seq": 8, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 2}, "call_id": "c_gs", "name": "gamespec_build", "arguments": "{\"game_id\":\"g_stale\",\"players\":[{\"id\":\"us\",\"name\":\"我方\",\"vn_role\":\"focal\",\"relation_to_game\":\"player\"}],\"strategies\":{\"us\":[\"稳价\"]},\"params\":{\"k_stale_cond\":{\"name\":\"陈旧参数\",\"value\":{\"kind\":\"literal\",\"value\":777},\"unit\":\"元\"}}}", "at": "2026-11-14T11:00:08Z"}}),
        serde_json::json!({"seq": 9, "event": {"type": "tool_call_finished", "call_id": "c_gs", "outcome": {"status": "ok", "output": "{\"game_id\":\"g_stale\"}"}, "at": "2026-11-14T11:00:09Z"}}),
        serde_json::json!({"seq": 10, "event": {"type": "checkpoint_opened", "id": "cp2", "phase": "model_build", "items": ["核对博弈矩阵"], "at": "2026-11-14T11:00:10Z"}}),
        serde_json::json!({"seq": 11, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 3}, "call_id": "c_solve", "name": "solve_equilibria", "arguments": "{}", "at": "2026-11-14T11:00:11Z"}}),
        serde_json::json!({"seq": 12, "event": {"type": "tool_call_finished", "call_id": "c_solve", "outcome": {"status": "ok", "output": "{\"completeness\":\"exact\",\"route\":\"support_enum\",\"equilibria\":[{\"id\":\"eq_stale_cond\",\"payoffs\":{\"us\":777}}]}"}, "at": "2026-11-14T11:00:12Z"}}),
        serde_json::json!({"seq": 13, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 4}, "call_id": "c_ca", "name": "conditional_analysis", "arguments": "{}", "at": "2026-11-14T11:00:13Z"}}),
        serde_json::json!({"seq": 14, "event": {"type": "tool_call_finished", "call_id": "c_ca", "outcome": {"status": "ok", "output": "{\"scope\":\"conditional\",\"summary\":\"STALE_COND_SUMMARY\",\"conditional\":{\"limitations\":[\"缺少竞对毛利\"]}}"}, "at": "2026-11-14T11:00:14Z"}}),
        serde_json::json!({"seq": 15, "event": {"type": "followup_task_started", "at": "2026-11-14T11:00:15Z"}}),
    ];
    write_case_fixture(&home, "2026-11-14", "case-cond-followup-cp1", cond_events);

    let out_cond = Command::new(bin())
        .args(["report", "case-cond-followup-cp1", "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        out_cond.status.success(),
        "conditional followup report stderr: {}",
        String::from_utf8_lossy(&out_cond.stderr)
    );
    let md_cond = String::from_utf8_lossy(&out_cond.stdout);
    assert!(!md_cond.contains("损坏 JSONL"), "测试日志不得有损坏行：\n{md_cond}");
    assert!(
        md_cond.contains("保留方案A") && md_cond.contains("15000"),
        "条件 followup 后成本精算证据应保留：\n{md_cond}"
    );
    assert!(!md_cond.contains("k_stale_cond"), "条件 followup 应清空陈旧博弈参数：\n{md_cond}");
    assert!(!md_cond.contains("eq_stale_cond"), "条件 followup 应清空陈旧求解结果：\n{md_cond}");
    assert!(!md_cond.contains("STALE_COND_SUMMARY"), "条件 followup 应清空陈旧条件结论：\n{md_cond}");
    assert!(
        md_cond.contains("关口 `cp1`（阶段 `intel` / 状态 `rewound`"),
        "条件 followup 必须将已确认的 cp1 标为 rewound：\n{md_cond}"
    );
    assert!(
        md_cond.contains("关口 `cp2`（阶段 `model_build` / 状态 `rewound`"),
        "条件 followup 必须将待决 cp2 标为 rewound：\n{md_cond}"
    );
    assert!(
        !md_cond.contains("状态 `answered`"),
        "条件 followup 后不得残留 answered 关口状态：\n{md_cond}"
    );

    // 2. Non-conditional followup must preserve prior answered cp1 as answered
    let noncond_events = vec![
        serde_json::json!({"seq": 1, "event": {"type": "phase_entered", "from": "intake", "to": "intel", "playbook": "pricing_response", "reason": "ok", "at": "2026-11-14T12:00:01Z"}}),
        serde_json::json!({"seq": 2, "event": {"type": "checkpoint_opened", "id": "cp1", "phase": "intel", "items": ["核对门面租金"], "at": "2026-11-14T12:00:02Z"}}),
        serde_json::json!({"seq": 3, "event": {"type": "checkpoint_answered", "id": "cp1", "answers": ["确认：月租12000"], "user_actor": "user", "at": "2026-11-14T12:00:03Z"}}),
        serde_json::json!({"seq": 4, "event": {"type": "task_scope_selected", "scope": "cost", "reason": "费用测算", "at": "2026-11-14T12:00:04Z"}}),
        serde_json::json!({"seq": 5, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "c_cost2", "name": "business_cost", "arguments": "{}", "at": "2026-11-14T12:00:05Z"}}),
        serde_json::json!({"seq": 6, "event": {"type": "tool_call_finished", "call_id": "c_cost2", "outcome": {"status": "ok", "output": "{\"scenario_id\":\"s_noncond\",\"scenario\":\"非条件方案B\",\"currency\":\"CNY\",\"listed_upfront_cash\":40000,\"listed_upfront_cash_expense\":40000,\"listed_refundable_deposits\":0,\"listed_monthly_fixed_cost\":12000,\"amounts_complete\":true,\"cash_classification_complete\":true,\"costs_complete\":true,\"items\":[]}"}, "at": "2026-11-14T12:00:06Z"}}),
        serde_json::json!({"seq": 7, "event": {"type": "scoped_task_completed", "result": {"scope": "cost", "status": "complete", "summary": "非条件费用完成", "missing": [], "source_urls": [], "retrieval_attempts": 0, "cost_scenarios": []}, "at": "2026-11-14T12:00:07Z"}}),
        serde_json::json!({"seq": 8, "event": {"type": "followup_task_started", "at": "2026-11-14T12:00:08Z"}}),
    ];
    write_case_fixture(&home, "2026-11-14", "case-noncond-followup-cp1", noncond_events);

    let out_noncond = Command::new(bin())
        .args(["report", "case-noncond-followup-cp1", "--format", "md"])
        .env("SHANGYI_HOME", &home)
        .output()
        .unwrap();
    assert!(
        out_noncond.status.success(),
        "non-conditional followup report stderr: {}",
        String::from_utf8_lossy(&out_noncond.stderr)
    );
    let md_noncond = String::from_utf8_lossy(&out_noncond.stdout);
    assert!(!md_noncond.contains("损坏 JSONL"), "测试日志不得有损坏行：\n{md_noncond}");
    assert!(md_noncond.contains("非条件方案B") && md_noncond.contains("12000"));
    assert!(
        md_noncond.contains("关口 `cp1`（阶段 `intel` / 状态 `answered`"),
        "非条件 followup 必须保留已确认的 cp1 为 answered：\n{md_noncond}"
    );
    assert!(!md_noncond.contains("状态 `rewound`"), "非条件 followup 不得撤销 cp1：\n{md_noncond}");

    let _ = std::fs::remove_dir_all(&home);
}

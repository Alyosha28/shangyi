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

    // PDF format must honestly refuse (no fake pdf binary output)
    let out_pdf = Command::new(bin())
        .args(["report", "latest", "--format", "pdf"])
        .env("SHANGYI_HOME", &home)
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

//! `shangyi-cli` 的测试：命令解析与自检。
//!
//! 重点验证**校验时机**——所有非法组合应在解析阶段被拒，
//! 而非留到业务流程里（那会让用户在投入时间后才失败）。

use std::collections::BTreeMap;

use shangyi_cli::{self_check, help, parse, CliError, Command, KbOp, Mode, ReportFormat};
use shangyi_core::SessionPhase;
use shangyi_playbooks::{Playbook, PlaybookMeta};
use shangyi_tools::ToolRegistry;

fn args(s: &[&str]) -> Vec<String> {
    s.iter().map(|x| x.to_string()).collect()
}

fn playbook_allow(tools: &[&str]) -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: "pricing_response".into(),
            name: "调价响应".into(),
            description: String::new(),
            order: 10,
            version: 1,
        },
        applicable_when: vec!["x".into()],
        fails_when: vec!["y".into()],
        required_inputs: vec![],
        value_net: None,
        intake: vec![],
        models: vec![],
        selectors: BTreeMap::new(),
        tool_allowlist: tools.iter().map(|s| s.to_string()).collect(),
        report_vars: BTreeMap::new(),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 基本命令解析
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn new_defaults_to_scenario_mode() {
    // §10：`shangyi new` 默认 scenario
    let c = parse(&args(&["new"])).unwrap();
    assert_eq!(
        c,
        Command::New {
            mode: Mode::Scenario,
            action: None,
            playbook: None
        }
    );
}

#[test]
fn new_parses_explicit_mode() {
    let c = parse(&args(&["new", "--mode", "discover"])).unwrap();
    match c {
        Command::New { mode, .. } => assert_eq!(mode, Mode::Discover),
        other => panic!("实得 {other:?}"),
    }
}

#[test]
fn new_parses_short_flags() {
    let c = parse(&args(&["new", "-m", "whatif", "-a", "降价15%"])).unwrap();
    match c {
        Command::New { mode, action, .. } => {
            assert_eq!(mode, Mode::Whatif);
            assert_eq!(action.as_deref(), Some("降价15%"));
        }
        other => panic!("实得 {other:?}"),
    }
}

#[test]
fn new_parses_playbook() {
    let c = parse(&args(&["new", "--playbook", "pricing_response"])).unwrap();
    match c {
        Command::New { playbook, .. } => assert_eq!(playbook.as_deref(), Some("pricing_response")),
        other => panic!("实得 {other:?}"),
    }
}

#[test]
fn resume_requires_case_id() {
    let c = parse(&args(&["resume", "case-2026-001"])).unwrap();
    assert_eq!(
        c,
        Command::Resume {
            case: "case-2026-001".into()
        }
    );
    assert!(matches!(
        parse(&args(&["resume"])),
        Err(CliError::MissingArg { .. })
    ));
}

#[test]
fn report_defaults_to_md() {
    let c = parse(&args(&["report", "case-1"])).unwrap();
    assert_eq!(
        c,
        Command::Report {
            case: "case-1".into(),
            format: ReportFormat::Md,
            output: None,
        }
    );
}

#[test]
fn report_parses_all_formats_and_output_flags() {
    for (s, want) in [
        ("md", ReportFormat::Md),
        ("docx", ReportFormat::Docx),
        ("pdf", ReportFormat::Pdf),
    ] {
        let c = parse(&args(&["report", "c", "--format", s])).unwrap();
        match c {
            Command::Report { format, output, .. } => {
                assert_eq!(format, want, "{s}");
                assert_eq!(output, None);
            }
            other => panic!("实得 {other:?}"),
        }
    }

    let c1 = parse(&args(&["report", "case-1", "--output", "out/r.md"])).unwrap();
    assert_eq!(
        c1,
        Command::Report {
            case: "case-1".into(),
            format: ReportFormat::Md,
            output: Some("out/r.md".into()),
        }
    );

    let c2 = parse(&args(&["report", "-f", "pdf", "-o", "/tmp/r.pdf", "case-2"])).unwrap();
    assert_eq!(
        c2,
        Command::Report {
            case: "case-2".into(),
            format: ReportFormat::Pdf,
            output: Some("/tmp/r.pdf".into()),
        }
    );
}

#[test]
fn report_rejects_unknown_format_and_empty_output() {
    let err = parse(&args(&["report", "c", "--format", "html"])).expect_err("未知格式必须被拒");
    assert!(
        matches!(err, CliError::InvalidValue { ref value, .. } if value == "html"),
        "实得 {err:?}"
    );
    let err_empty =
        parse(&args(&["report", "c", "--output", "   "])).expect_err("空 --output 必须被拒");
    assert!(
        matches!(err_empty, CliError::InvalidValue { .. }),
        "实得 {err_empty:?}"
    );
}

#[test]
fn backtest_accepts_optional_scenario() {
    assert_eq!(
        parse(&args(&["backtest"])).unwrap(),
        Command::Backtest { scenario: None }
    );
    assert_eq!(
        parse(&args(&["backtest", "pricing_response"])).unwrap(),
        Command::Backtest {
            scenario: Some("pricing_response".into())
        }
    );
    assert_eq!(
        parse(&args(&["backtest", "--scenario", "negotiation"])).unwrap(),
        Command::Backtest {
            scenario: Some("negotiation".into())
        }
    );
}

#[test]
fn kb_add_and_search() {
    assert_eq!(
        parse(&args(&["kb", "add", "theory/bertrand.md"])).unwrap(),
        Command::Kb {
            op: KbOp::Add {
                path: "theory/bertrand.md".into()
            }
        }
    );
    // 查询可含空格（拼接剩余参数）
    let c = parse(&args(&["kb", "search", "差异化", "价格竞争"])).unwrap();
    assert_eq!(
        c,
        Command::Kb {
            op: KbOp::Search {
                query: "差异化 价格竞争".into()
            }
        }
    );
}

#[test]
fn kb_requires_subcommand() {
    assert!(matches!(
        parse(&args(&["kb"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["kb", "delete", "x"])),
        Err(CliError::InvalidValue { .. })
    ));
}

#[test]
fn kb_search_requires_query() {
    assert!(matches!(
        parse(&args(&["kb", "search"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["kb", "search", "  "])),
        Err(CliError::MissingArg { .. })
    ));
}

// ─────────────────────────────────────────────────────────────────────────────
// 校验时机：非法组合在解析阶段即被拒
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn whatif_without_action_is_rejected_at_parse_time() {
    // **本 crate 最重要的断言**：模式与选项的耦合在解析阶段校验，
    // 否则每个入口都得重复一遍。whatif 不给动作 ⇒ 推演什么？
    let err = parse(&args(&["new", "--mode", "whatif"])).expect_err("whatif 缺 action 必须被拒");
    assert!(
        matches!(err, CliError::ModeConflict { ref mode, ref reason }
            if mode == "whatif" && reason.contains("--action")),
        "实得 {err:?}"
    );
}

#[test]
fn action_with_non_whatif_mode_is_rejected() {
    // `--action` 只对 whatif 有意义——在其他模式下静默忽略是危险的
    // （用户以为推演了，实际没推演）
    let err = parse(&args(&["new", "--mode", "scenario", "--action", "降价"])).expect_err("必须被拒");
    assert!(
        matches!(err, CliError::ModeConflict { ref mode, .. } if mode == "scenario"),
        "实得 {err:?}"
    );
}

#[test]
fn unknown_command_rejected() {
    let err = parse(&args(&["analyse"])).expect_err("未知命令必须被拒");
    assert!(matches!(err, CliError::UnknownCommand(ref c) if c == "analyse"));
}

#[test]
fn empty_args_rejected_with_hint() {
    let err = parse(&args(&[])).expect_err("无命令必须被拒");
    let msg = format!("{err}");
    // 错误信息应完整列出全部 9 个可用子命令
    for cmd in [
        "start",
        "ui",
        "new",
        "resume",
        "solve",
        "report",
        "backtest",
        "kb",
        "checkpoint",
        "mcp-serve",
    ] {
        assert!(msg.contains(cmd), "空参数提示缺少 `{cmd}`：{msg}");
    }
}

#[test]
fn unknown_option_rejected() {
    assert!(matches!(
        parse(&args(&["new", "--verbose"])),
        Err(CliError::UnknownOption { .. })
    ));
    assert!(matches!(
        parse(&args(&["resume", "c", "--force"])),
        Err(CliError::UnknownOption { .. })
    ));
}

#[test]
fn missing_option_value_rejected() {
    assert!(matches!(
        parse(&args(&["new", "--mode"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["report", "c", "--format"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["report", "c", "--output"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["report", "c", "-o"])),
        Err(CliError::MissingArg { .. })
    ));
}

#[test]
fn invalid_mode_value_rejected() {
    let err = parse(&args(&["new", "--mode", "explore"])).expect_err("非法模式必须被拒");
    assert!(
        matches!(err, CliError::InvalidValue { ref allowed, .. }
            if allowed.contains(&"scenario".into()) && allowed.contains(&"whatif".into())),
        "错误应列出合法取值：{err:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 模式语义（§13.6）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn mode_requires_flags_are_correct() {
    assert!(Mode::Whatif.requires_action());
    assert!(!Mode::Scenario.requires_action());
    assert!(!Mode::Discover.requires_action());
    assert!(Mode::Discover.requires_aspiration());
    assert!(!Mode::Scenario.requires_aspiration());
}

#[test]
fn all_modes_have_phase_sequences_starting_with_intake() {
    for m in [Mode::Scenario, Mode::Whatif, Mode::Discover] {
        let ph = m.phases();
        assert_eq!(ph[0], SessionPhase::Intake, "{:?}", m);
        assert_eq!(*ph.last().unwrap(), SessionPhase::Archived, "{:?}", m);
        assert!(ph.contains(&SessionPhase::Report), "{:?}", m);
    }
}

/// 三种模式的阶段序列必须**互不相同**。
///
/// 这条测试针对一个真实缺陷的回归：`Mode::phases()` 曾是同一知识的第二份拷贝，
/// 三个分支返回**同一个数组**——于是 `whatif` 声称自己会有 `select` 阶段
/// （其路径上根本没有），而 `discover` 与 `scenario` 完全一样。旧写法只断言
/// 首尾与"包含 report"，**发现不了**这种塌缩。
#[test]
fn mode_phase_sequences_are_distinct_and_pin_the_tail() {
    let sc = Mode::Scenario.phases();
    let wi = Mode::Whatif.phases();
    let di = Mode::Discover.phases();

    assert_eq!(
        sc,
        vec![
            SessionPhase::Intake, SessionPhase::Intel, SessionPhase::ModelBuild,
            SessionPhase::Solve, SessionPhase::Select, SessionPhase::Recommend,
            SessionPhase::Report, SessionPhase::Archived,
        ]
    );
    assert_eq!(
        wi,
        vec![
            SessionPhase::Intake, SessionPhase::Intel, SessionPhase::ModelBuild,
            SessionPhase::Solve, SessionPhase::Consequence, SessionPhase::Recommend,
            SessionPhase::Report, SessionPhase::Archived,
        ],
        "whatif：consequence 取代 select"
    );
    assert_eq!(
        di,
        vec![
            SessionPhase::Intake, SessionPhase::Intel, SessionPhase::ModelBuild,
            SessionPhase::Solve, SessionPhase::Discover, SessionPhase::Recommend,
            SessionPhase::Report, SessionPhase::Archived,
        ],
        "discover：discover 取代 select"
    );

    assert_ne!(sc, wi);
    assert_ne!(sc, di);
    assert_ne!(wi, di);
    assert!(!wi.contains(&SessionPhase::Select), "whatif 路径上没有 select 阶段");
    assert!(!di.contains(&SessionPhase::Select), "discover 路径上没有 select 阶段");
}

#[test]
fn cli_mode_converts_to_core_mode() {
    use shangyi_core::ConsultationMode;
    assert_eq!(Mode::Scenario.to_consultation_mode(), ConsultationMode::Scenario);
    assert_eq!(Mode::Whatif.to_consultation_mode(), ConsultationMode::Whatif);
    assert_eq!(Mode::Discover.to_consultation_mode(), ConsultationMode::Discover);
    assert_eq!(ConsultationMode::from(Mode::Whatif), ConsultationMode::Whatif);
}

#[test]
fn mode_str_round_trip() {
    for m in [Mode::Scenario, Mode::Whatif, Mode::Discover] {
        assert_eq!(Mode::parse(m.as_str()).unwrap(), m);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 命令属性
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn mutating_commands_are_identified() {
    // 只有非只读命令才需要写权限与审批
    assert!(parse(&args(&["new"])).unwrap().is_mutating());
    assert!(parse(&args(&["resume", "c"])).unwrap().is_mutating());
    assert!(parse(&args(&["report", "c"])).unwrap().is_mutating());
    assert!(parse(&args(&["backtest"])).unwrap().is_mutating());
    assert!(parse(&args(&["kb", "add", "p"])).unwrap().is_mutating());
    // 只读操作
    assert!(!parse(&args(&["kb", "search", "q"])).unwrap().is_mutating());
}

#[test]
fn report_command_maps_to_report_phase() {
    let c = parse(&args(&["report", "c"])).unwrap();
    assert_eq!(c.phases(), vec![SessionPhase::Report]);
}

#[test]
fn backtest_command_maps_to_archived_phase() {
    let c = parse(&args(&["backtest"])).unwrap();
    assert_eq!(c.phases(), vec![SessionPhase::Archived]);
}

#[test]
fn kb_command_has_no_phase_binding() {
    let c = parse(&args(&["kb", "search", "q"])).unwrap();
    assert!(c.phases().is_empty(), "kb 操作不绑定会话阶段");
}

// ─────────────────────────────────────────────────────────────────────────────
// 自检（命令与领域配置交叉校验）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn self_check_passes_when_playbook_supports_mode() {
    let r = ToolRegistry::standard();
    let pb = playbook_allow(&["solve_equilibria", "consequence_predict", "discover_satisfice"]);
    let c = parse(&args(&["new", "--mode", "scenario"])).unwrap();
    assert!(self_check(&c, &r, Some(&pb)).is_ok());
}

#[test]
fn self_check_rejects_playbook_without_mode_primary_tool() {
    // 剧本白名单不含该模式的主工具 ⇒ 该剧本不支持此模式
    let r = ToolRegistry::standard();
    let pb = playbook_allow(&["vn_map", "pars_check"]); // 只有 intake 工具
    let c = parse(&args(&["new", "--mode", "scenario"])).unwrap();
    let err = self_check(&c, &r, Some(&pb)).expect_err("剧本不支持该模式必须被拒");
    assert!(
        matches!(err, CliError::SelfCheck(ref m) if m.contains("solve_equilibria")),
        "实得 {err:?}"
    );
}

#[test]
fn self_check_covers_all_three_modes_primary_tools() {
    let r = ToolRegistry::standard();
    for (mode, tool) in [
        ("scenario", "solve_equilibria"),
        ("whatif", "consequence_predict"),
        ("discover", "discover_satisfice"),
    ] {
        // 只放该模式的主工具 ⇒ 应通过
        let pb = playbook_allow(&[tool]);
        let c = if mode == "whatif" {
            parse(&args(&["new", "--mode", "whatif", "--action", "降价"])).unwrap()
        } else {
            parse(&args(&["new", "--mode", mode])).unwrap()
        };
        assert!(self_check(&c, &r, Some(&pb)).is_ok(), "{mode} 应通过自检");
    }
}

#[test]
fn self_check_skips_non_new_commands() {
    let r = ToolRegistry::standard();
    let pb = playbook_allow(&[]);
    for a in [
        vec!["resume", "c"],
        vec!["report", "c"],
        vec!["backtest"],
        vec!["kb", "search", "q"],
    ] {
        let c = parse(&args(&a)).unwrap();
        assert!(self_check(&c, &r, Some(&pb)).is_ok(), "{a:?} 不涉及模式-剧本耦合");
    }
}

#[test]
fn self_check_skips_when_no_playbook_given() {
    let r = ToolRegistry::standard();
    let c = parse(&args(&["new"])).unwrap();
    assert!(self_check(&c, &r, None).is_ok(), "未指定剧本时由后续推荐流程决定");
}

// ─────────────────────────────────────────────────────────────────────────────
// 帮助文本
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn help_lists_all_commands_and_modes() {
    let h = help();
    for cmd in [
        "start",
        "ui",
        "new",
        "resume",
        "report",
        "kb",
        "backtest",
        "solve",
        "checkpoint",
        "mcp-serve",
    ] {
        assert!(h.contains(cmd), "帮助应含命令 `{cmd}`：{h}");
    }
    for m in ["scenario", "whatif", "discover"] {
        assert!(h.contains(m), "帮助应含模式 `{m}`");
    }
    // 关键提示：whatif 必须给 action，report 说明默认 md + --output + pandoc
    assert!(h.contains("--action"), "帮助应说明 whatif 需要 --action");
    assert!(h.contains("--output"), "帮助应说明 report 支持 --output");
    assert!(h.contains("默认 md"), "帮助应说明 report 默认 md");
    assert!(h.contains("pandoc"), "帮助应说明 pdf/docx 按需调用 pandoc");
}

// ─────────────────────────────────────────────────────────────────────────────
// solve：离线求解（机器接口）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn solve_requires_in() {
    // --in 是唯一必填项：缺它必须在解析层被拒（机器调用方依赖快速失败）
    let err = parse(&args(&["solve"])).expect_err("缺 --in 必须被拒");
    match err {
        CliError::MissingArg { command, arg } => {
            assert_eq!(command, "solve");
            assert!(arg.contains("--in"), "提示应指向 --in：{arg}");
        }
        other => panic!("实得 {other:?}"),
    }
}

#[test]
fn solve_parses_all_options() {
    let c = parse(&args(&[
        "solve",
        "--in",
        "spec.json",
        "--method",
        "lemke_howson",
        "--refine",
        "trembling_hand_perfect",
        "--refine-denominator",
        "12",
    ]))
    .unwrap();
    assert_eq!(
        c,
        Command::Solve {
            input: "spec.json".into(),
            method: Some("lemke_howson".into()),
            refine: Some("trembling_hand_perfect".into()),
            refine_denominator: Some(12),
        }
    );
}

#[test]
fn solve_parses_short_flags() {
    let c = parse(&args(&["solve", "-i", "spec.json", "-m", "lh", "-r", "thp", "-d", "6"])).unwrap();
    match c {
        Command::Solve { input, method, refine, refine_denominator } => {
            assert_eq!(input, "spec.json");
            assert_eq!(method.as_deref(), Some("lh"));
            assert_eq!(refine.as_deref(), Some("thp"));
            assert_eq!(refine_denominator, Some(6));
        }
        other => panic!("实得 {other:?}"),
    }
}

#[test]
fn solve_option_value_missing_is_rejected() {
    // `--method` 后没有值：不是"默认值"，而是残缺命令行
    let err = parse(&args(&["solve", "--in", "s.json", "--method"])).expect_err("必须被拒");
    assert!(matches!(err, CliError::MissingArg { .. }), "实得 {err:?}");
}

#[test]
fn solve_unknown_option_rejected() {
    let err = parse(&args(&["solve", "--in", "s.json", "--verbose"])).expect_err("必须被拒");
    assert!(matches!(err, CliError::UnknownOption { .. }), "实得 {err:?}");
}

#[test]
fn solve_denominator_bounds_enforced_at_parse_time() {
    // 越界值必须在解析层被拒：静默回退默认 6 会让"写错参数"看起来像"用了默认"
    for bad in ["1", "65", "abc"] {
        let err = parse(&args(&["solve", "-i", "s.json", "-d", bad])).expect_err("必须被拒");
        assert!(
            matches!(err, CliError::InvalidValue { .. }),
            "`{bad}` 实得 {err:?}"
        );
    }
    // 边界值合法
    for ok in ["2", "64"] {
        let c = parse(&args(&["solve", "-i", "s.json", "-d", ok])).unwrap();
        assert!(matches!(c, Command::Solve { .. }), "`{ok}` 应合法");
    }
}

#[test]
fn solve_is_read_only() {
    // solve 只读输入文件、只写 stdout——只读命令可在只读沙箱下运行（is_mutating 契约）
    let c = parse(&args(&["solve", "-i", "s.json"])).unwrap();
    assert!(!c.is_mutating(), "solve 不得标记为 mutating");
    assert_eq!(c.phases(), vec![SessionPhase::Solve]);
}

#[test]
fn help_mentions_solve() {
    let h = help();
    assert!(h.contains("solve"), "帮助应含 solve：{h}");
    // 机器接口的定位说明（调用方据此判断 stdout 可否直接管道给 JSON 解析器）
    assert!(h.contains("离线机器接口"), "帮助应说明 solve 是离线机器接口");
}

#[test]
fn start_and_ui_alias_parse_correctly() {
    let s1 = parse(&args(&["start"])).unwrap();
    assert_eq!(
        s1,
        Command::Start {
            port: None,
            no_open: false,
        }
    );
    assert!(s1.is_mutating());
    assert!(s1.phases().is_empty());

    let s2 = parse(&args(&["ui", "--port", "9001", "--no-open"])).unwrap();
    assert_eq!(
        s2,
        Command::Start {
            port: Some(9001),
            no_open: true,
        }
    );

    assert!(matches!(
        parse(&args(&["start", "--port", "0"])),
        Err(CliError::InvalidValue { .. })
    ));
    assert!(matches!(
        parse(&args(&["start", "--port"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["start", "--unknown"])),
        Err(CliError::UnknownOption { .. })
    ));
}

#[test]
fn checkpoint_parses_approve_reject_and_options() {
    let c1 = parse(&args(&[
        "checkpoint",
        "latest",
        "--approve",
        "--note",
        "确认假设",
        "--home",
        "/tmp/sh",
    ]))
    .unwrap();
    assert_eq!(
        c1,
        Command::Checkpoint {
            case: "latest".into(),
            approve: true,
            note: Some("确认假设".into()),
            home: Some("/tmp/sh".into()),
        }
    );
    assert!(c1.is_mutating());
    assert!(c1.phases().is_empty());

    let c2 = parse(&args(&["checkpoint", "case-9", "--reject"])).unwrap();
    assert_eq!(
        c2,
        Command::Checkpoint {
            case: "case-9".into(),
            approve: false,
            note: None,
            home: None,
        }
    );

    assert!(matches!(
        parse(&args(&["checkpoint", "latest"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["checkpoint", "--approve"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["checkpoint", "latest", "--approve", "--reject"])),
        Err(CliError::InvalidValue { .. })
    ));
}

#[test]
fn mcp_serve_parses_options_and_contracts() {
    let m1 = parse(&args(&["mcp-serve"])).unwrap();
    assert_eq!(
        m1,
        Command::McpServe {
            playbook: None,
            mode: None,
            home: None,
            auto_approve_checkpoints: false,
            resume: None,
        }
    );
    assert!(m1.is_mutating());
    assert_eq!(m1.phases(), Mode::Scenario.phases());

    let m2 = parse(&args(&[
        "mcp-serve",
        "--playbook",
        "pricing_response",
        "--mode",
        "discover",
        "--home",
        "/tmp/h",
        "--auto-approve-checkpoints",
        "--resume",
        "latest",
    ]))
    .unwrap();
    assert_eq!(
        m2,
        Command::McpServe {
            playbook: Some("pricing_response".into()),
            mode: Some(Mode::Discover),
            home: Some("/tmp/h".into()),
            auto_approve_checkpoints: true,
            resume: Some("latest".into()),
        }
    );
    assert_eq!(m2.phases(), Mode::Discover.phases());

    assert!(matches!(
        parse(&args(&["mcp-serve", "--mode", "bad"])),
        Err(CliError::InvalidValue { .. })
    ));
    assert!(matches!(
        parse(&args(&["mcp-serve", "--home"])),
        Err(CliError::MissingArg { .. })
    ));
    assert!(matches!(
        parse(&args(&["mcp-serve", "--foo"])),
        Err(CliError::UnknownOption { .. })
    ));
}

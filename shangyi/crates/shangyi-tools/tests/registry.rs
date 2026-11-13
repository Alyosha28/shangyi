//! `shangyi-tools` 的测试。
//!
//! 核心验证三项约束（对应 §4 与 Harness §9.3）：
//! 1. **阶段白名单**：在 intake 阶段调 solve_equilibria 必须被拒
//! 2. **剧本白名单**：工具不在剧本白名单内必须被拒
//! 3. **可信度分级**：`sandbox_exec`（Exploratory）的产出**不得进报告**（铁律 1）

use std::collections::BTreeMap;

use shangyi_playbooks::{Playbook, PlaybookMeta};
use shangyi_tools::{ToolError, ToolRegistry, ToolSpec, TrustLevel};

fn playbook_with_allowlist(tools: &[&str]) -> Playbook {
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
// 标准注册表（Harness §9.3；2026-11-10 实测 38 条，旧注释写 24）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn standard_registry_has_all_documented_tools() {
    // 与 Harness §9.3 的工具表逐行对应
    let r = ToolRegistry::standard();
    let expected = [
        "select_task_scope", "complete_task", "begin_followup_task",
        "constraint_register", "constraint_check", "conditional_analysis",
        "research_task_plan", "research_task_update", "research_document_lookup",
        "research_record", "business_cost", "vn_map", "pars_check", "ask_user",
        // 周边门店计数（高德 POI）：把"周边多少家同类"变成坐标级事实
        "poi_survey", "web_search", "web_fetch", "data_ingest",
        "participant_profile_update", "gamespec_build", "gamespec_validate",
        "solve_equilibria", "select_equilibria", "select_fuse", "swarm_run",
        "novel_move_filter", "consequence_predict", "monte_carlo", "sensitivity_scan",
        "discover_satisfice", "preference_inject", "report_render",
        "case_store_read", "case_store_write", "kb_retrieve", "checkpoint", "sandbox_exec",
        // R23：模型向用户**提议**回退（决定权在用户）
        "request_rewind",
        // R24：recommend 阶段的产物工具（曾漏注册 ⇒ report 阶段不可达）
        "recommend_build",
    ];
    assert_eq!(r.len(), expected.len(), "工具数应为 {}", expected.len());
    for name in expected {
        assert!(r.get(name).is_some(), "缺少工具 `{name}`");
    }
}

#[test]
fn duplicate_registration_rejected() {
    let mut r = ToolRegistry::new();
    r.register(ToolSpec::new("t", "d", "owner", TrustLevel::Engine, true, &[]))
        .unwrap();
    assert!(r
        .register(ToolSpec::new("t", "d2", "owner", TrustLevel::Engine, true, &[]))
        .is_err());
}

#[test]
fn intake_scope_controls_are_available_without_expanding_domain_permissions() {
    let registry=ToolRegistry::standard();let pb=playbook_with_allowlist(&["web_fetch"]);
    for name in ["select_task_scope","complete_task"] {
        assert!(registry.check_call(name,"intake",Some(&pb)).is_ok());
        assert!(registry.tools_for("intake",&pb).iter().any(|tool|tool.name==name));
        assert!(registry.check_call(name,"solve",Some(&pb)).is_err());
    }
    assert!(registry.check_call("business_cost","intake",Some(&pb)).is_err());
}

// ─────────────────────────────────────────────────────────────────────────────
// 阶段白名单（Harness §9.3 表）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn solve_not_allowed_in_intake_phase() {
    // 在 intake 阶段调 solve_equilibria 是逻辑错误——博弈尚未建模，求解什么？
    let r = ToolRegistry::standard();
    let err = r
        .check_call("solve_equilibria", "intake", None)
        .expect_err("intake 阶段不得求解");
    assert!(
        matches!(err, ToolError::PhaseNotAllowed { ref tool, ref phase, .. }
            if tool == "solve_equilibria" && phase == "intake"),
        "实得 {err:?}"
    );
}

#[test]
fn solve_allowed_in_solve_phase() {
    let r = ToolRegistry::standard();
    assert!(r.check_call("solve_equilibria", "solve", None).is_ok());
}

#[test]
fn tools_for_phase_filters_correctly() {
    let r = ToolRegistry::standard();
    let intake = r.tools_for_phase("intake");
    let names: Vec<&str> = intake.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"vn_map"), "intake 应有 vn_map：{names:?}");
    assert!(names.contains(&"pars_check"));
    assert!(
        !names.contains(&"solve_equilibria"),
        "intake 不应暴露求解工具——否则模型可能过早尝试求解"
    );

    let solve = r.tools_for_phase("solve");
    let solve_names: Vec<&str> = solve.iter().map(|t| t.name.as_str()).collect();
    assert!(solve_names.contains(&"solve_equilibria"));
    assert!(solve_names.contains(&"gamespec_validate"));
}

#[test]
fn empty_phases_means_any_phase() {
    // kb_retrieve / case_store_read / checkpoint 声明"全阶段"
    let r = ToolRegistry::standard();
    for phase in ["intake", "intel", "model_build", "solve", "select", "recommend", "report", "archived"] {
        assert!(
            r.check_call("kb_retrieve", phase, None).is_ok(),
            "kb_retrieve 应在 {phase} 可用"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 剧本白名单（§13.3）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn tool_outside_playbook_allowlist_rejected() {
    let r = ToolRegistry::standard();
    let pb = playbook_with_allowlist(&["game_solve", "web_search"]);
    // `vn_map` 在注册表里且在 intake 阶段可用，但不在该剧本白名单
    let err = r
        .check_call("vn_map", "intake", Some(&pb))
        .expect_err("不在剧本白名单必须被拒");
    assert!(
        matches!(err, ToolError::NotInPlaybookAllowlist { ref tool, ref playbook }
            if tool == "vn_map" && playbook == "pricing_response"),
        "实得 {err:?}"
    );
}

#[test]
fn tool_inside_playbook_allowlist_accepted() {
    let r = ToolRegistry::standard();
    let pb = playbook_with_allowlist(&["vn_map", "pars_check"]);
    assert!(r.check_call("vn_map", "intake", Some(&pb)).is_ok());
}

#[test]
fn empty_playbook_allowlist_means_no_restriction() {
    // 白名单为空 ⇒ 不施加额外限制（只有阶段限制生效）
    let r = ToolRegistry::standard();
    let pb = playbook_with_allowlist(&[]);
    assert!(r.check_call("vn_map", "intake", Some(&pb)).is_ok());
}

#[test]
fn tools_for_combines_phase_and_playbook_filters() {
    let r = ToolRegistry::standard();
    let pb = playbook_with_allowlist(&["web_search", "solve_equilibria"]);
    // solve 阶段 + 该剧本 ⇒ 只有 solve_equilibria（web_search 不在 solve 阶段的允许列表里）
    let got = r.tools_for("solve", &pb);
    let names: Vec<&str> = got.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"solve_equilibria"));
    assert!(
        !names.contains(&"web_search"),
        "web_search 不在 solve 阶段的允许列表内：{names:?}"
    );
}

#[test]
fn unknown_tool_rejected() {
    let r = ToolRegistry::standard();
    let err = r.check_call("exec_command", "intake", None).expect_err("未知工具必须被拒");
    assert!(matches!(err, ToolError::Unknown(ref t) if t == "exec_command"));
}

// ─────────────────────────────────────────────────────────────────────────────
// 可信度分级（铁律 1 的最后一道闸）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn sandbox_exec_results_are_not_reportable() {
    // **本 crate 最重要的断言**。Harness §9.3 对 sandbox_exec 的收窄（R8）：
    // 其产出标注 exploratory，报告生成器**拒绝读取**。
    let r = ToolRegistry::standard();
    let err = r
        .check_reportable("sandbox_exec")
        .expect_err("sandbox_exec 的产出不得进报告");
    assert!(
        matches!(err, ToolError::InsufficientTrust(_)),
        "实得 {err:?}"
    );

    let spec = r.get("sandbox_exec").unwrap();
    assert_eq!(spec.trust, TrustLevel::Exploratory);
    assert!(!spec.trust.reportable_directly());
}

#[test]
fn engine_tools_are_reportable() {
    let r = ToolRegistry::standard();
    for name in ["solve_equilibria", "consequence_predict", "monte_carlo", "select_fuse"] {
        assert!(
            r.check_reportable(name).is_ok(),
            "`{name}` 是 Engine 类，应可进报告"
        );
        assert_eq!(r.get(name).unwrap().trust, TrustLevel::Engine);
    }
}

#[test]
fn proposal_tools_are_not_directly_reportable() {
    // swarm_run 与 ask_user 的结果是"提议"，须经引擎评估
    let r = ToolRegistry::standard();
    for name in ["swarm_run", "ask_user"] {
        assert!(
            r.check_reportable(name).is_err(),
            "`{name}` 是 Proposal 类，不得直接进报告"
        );
        assert_eq!(r.get(name).unwrap().trust, TrustLevel::Proposal);
    }
}

#[test]
fn sidecar_tools_do_not_produce_reportable_numbers() {
    // web_search / web_fetch / data_ingest 是 Sidecar 类：
    // 它们提供原料与检索，但**数字须由 Rust 算出**
    let r = ToolRegistry::standard();
    for name in ["web_search", "web_fetch", "data_ingest"] {
        let spec = r.get(name).unwrap();
        assert_eq!(spec.trust, TrustLevel::Sidecar, "`{name}`");
        assert!(
            !spec.produces_reportable_numbers,
            "`{name}` 不应声明产生可进报告的数字"
        );
    }
}

#[test]
fn trust_levels_ordering_is_by_increasing_distrust() {
    // Engine 最可信托，Exploratory 最不可信——顺序用于任何"取最保守"的场合
    assert!(TrustLevel::Engine < TrustLevel::Sidecar);
    assert!(TrustLevel::Sidecar < TrustLevel::Proposal);
    assert!(TrustLevel::Proposal < TrustLevel::Exploratory);
}

#[test]
fn only_engine_level_is_directly_reportable() {
    assert!(TrustLevel::Engine.reportable_directly());
    for t in [TrustLevel::Sidecar, TrustLevel::Proposal, TrustLevel::Exploratory] {
        assert!(!t.reportable_directly(), "{t:?} 不应可直接进报告");
    }
}

#[test]
fn engine_tools_list_is_nonempty_and_excludes_sandbox() {
    let r = ToolRegistry::standard();
    let engines = r.engine_tools();
    assert!(!engines.is_empty());
    let names: Vec<&str> = engines.iter().map(|t| t.name.as_str()).collect();
    assert!(!names.contains(&"sandbox_exec"), "sandbox_exec 不属于 Engine 类");
    assert!(names.contains(&"solve_equilibria"));
}

// ─────────────────────────────────────────────────────────────────────────────
// 幂等性
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn non_idempotent_tools_are_identifiable() {
    // 非幂等工具不得在只读/可重放上下文调用
    let r = ToolRegistry::standard();
    let non_idem: Vec<&str> = r.non_idempotent().iter().map(|t| t.name.as_str()).collect();
    assert!(non_idem.contains(&"sandbox_exec"), "sandbox_exec 非幂等");
    assert!(non_idem.contains(&"report_render"), "report_render 非幂等");
    assert!(non_idem.contains(&"swarm_run"), "swarm_run 非幂等");
    // 求解是纯函数 ⇒ 幂等
    assert!(!non_idem.contains(&"solve_equilibria"));
    assert!(!non_idem.contains(&"monte_carlo"));
}

#[test]
fn idempotent_tools_include_deterministic_engines() {
    let r = ToolRegistry::standard();
    for name in ["solve_equilibria", "select_fuse", "consequence_predict", "monte_carlo", "gamespec_validate"] {
        assert!(
            r.get(name).unwrap().idempotent,
            "`{name}` 是确定性计算，应声明幂等"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 工具规格本身的约束
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn spec_requires_owner() {
    let mut r = ToolRegistry::new();
    let spec = ToolSpec::new("t", "d", "", TrustLevel::Engine, true, &[]);
    assert!(matches!(r.register(spec), Err(ToolError::MissingOwner(_))));
}

#[test]
fn engine_level_specs_declare_reportable_numbers() {
    // Engine 类的语义就是"产出可进报告的数字"
    let s = ToolSpec::new("x", "d", "owner", TrustLevel::Engine, true, &["solve"]);
    assert!(s.produces_reportable_numbers);

    let side = ToolSpec::new("y", "d", "owner", TrustLevel::Sidecar, true, &["intel"]);
    assert!(!side.produces_reportable_numbers);
}

#[test]
fn registry_serializes_round_trip() {
    let r = ToolRegistry::standard();
    let json = serde_json::to_string(&r).unwrap();
    let back: ToolRegistry = serde_json::from_str(&json).unwrap();
    assert_eq!(r, back);
    assert_eq!(back.len(), r.len());
}

#[test]
fn trust_level_serializes_kebab_case() {
    for (t, want) in [
        (TrustLevel::Engine, "\"engine\""),
        (TrustLevel::Sidecar, "\"sidecar\""),
        (TrustLevel::Proposal, "\"proposal\""),
        (TrustLevel::Exploratory, "\"exploratory\""),
    ] {
        assert_eq!(serde_json::to_string(&t).unwrap(), want, "{t:?}");
    }
}

#[test]
fn tool_names_are_snake_case_wire_names() {
    // wire 名须与 Harness §9.3 表一致（LLM 看到的名字）
    let r = ToolRegistry::standard();
    for name in r.names() {
        assert!(
            name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
            "wire 名应为 snake_case：`{name}`"
        );
    }
}

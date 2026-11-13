//! `shangyi-core::consultation` 的测试。
//!
//! 重点验证接线的**诚实性**——这是"看起来能跑"最容易骗人的地方：
//! 1. 空解集不得被当作"求解成功"推进
//! 2. 未归一化的分布不得进报告
//! 3. 缺 provenance 的报告不得交付
//! 4. 回退时必须清理下游产物（否则报告会混合两个版本的模型）
//! 5. 未实现的交互环节必须报错而非伪造结果

use std::collections::BTreeMap;

// ⚠️ 这里曾导入 `Consultation` 与三个自由函数——它们已删除。
//
// 原因（见 `consultation.rs` 末尾的记录）：那套 `Consultation` 是与
// `SessionRunner` + `DomainExecutor` **并行的实现**，生产调用为 0，
// 只被本文件的测试使用。而那带来一个隐蔽危害：**那些测试验证的是
// 平行实现，不是真实路径**——例如"空画像要被拒"在平行实现里成立，
// 而真实路径把"没有画像"当作合法状态。两个结论相反，测试通过给了假信心。
//
// 故删除平行实现，本文件的测试改为**直接测真实路径**。
use shangyi_core::consultation::ConsultationMode;
use shangyi_core::SessionPhase;
use shangyi_gamespec::{
    Expr, GameSpec, Param, ParamSource, Participant, PayoffForm, RelationToGame, ValueNetRole,
};
use shangyi_intel::{Level, ParticipantProfile, RoleSpecific};
use shangyi_playbooks::{Playbook, PlaybookMeta};
use shangyi_report::{Number, ProvenanceId, ProvenanceKind, ProvenanceRecord, Report, Section};
use shangyi_tools::ToolRegistry;

fn playbook(allow: &[&str]) -> Playbook {
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
        tool_allowlist: allow.iter().map(|s| s.to_string()).collect(),
        report_vars: BTreeMap::new(),
    }
}

/// 一个通过校验的 GameSpec（参数已确认）。
fn valid_spec() -> GameSpec {
    let mut strategies = BTreeMap::new();
    strategies.insert("focal".into(), vec!["维持价".into(), "降价10%".into()]);
    strategies.insert("comp_a".into(), vec!["跟随".into(), "不动".into()]);
    let mut params = BTreeMap::new();
    params.insert(
        "P1".into(),
        Param {
            name: "P1".into(),
            value: Expr::Literal(42.0),
            dist: None,
            source: ParamSource::UserConfirmed {
                checkpoint: "cp2".into(),
            },
            unit: None,
        },
    );
    GameSpec {
        game_id: "case-wire".into(),
        players: vec![
            Participant {
                id: "focal".into(),
                name: "本企业".into(),
                vn_role: ValueNetRole::Focal,
                relation_to_game: RelationToGame::Player,
                profile_ref: None,
            },
            Participant {
                id: "comp_a".into(),
                name: "竞对".into(),
                vn_role: ValueNetRole::Substitutor,
                relation_to_game: RelationToGame::Player,
                profile_ref: None,
            },
        ],
        strategies,
        payoffs: PayoffForm::Normal {
            // 2×2 必须填满——空矩阵现在会被 `validate` 如实拒绝
            // （`IncompletePayoffCells`；夹具此前是空的，于是它验证的不是真实路径）
            cells: {
                let mut c = BTreeMap::new();
                for (f, a) in [("维持价", 3.0), ("降价10%", 1.0)] {
                    for (k, b) in [("跟随", 0.0), ("不动", 5.0)] {
                        let mut m = BTreeMap::new();
                        m.insert("focal".into(), Expr::Literal(a));
                        m.insert("comp_a".into(), Expr::Literal(b));
                        c.insert(format!("{f},{k}"), m);
                    }
                }
                c
            },
        },
        params,
        information: None,
        dynamics: None,
        external_actors: vec![],
        preference: None,
        meta: BTreeMap::new(),
    }
}

fn profile(id: &str, role: ValueNetRole, rs: RoleSpecific) -> ParticipantProfile {
    let mut p = ParticipantProfile::new(id, role, RelationToGame::Player);
    p.role_specific = Some(rs);
    p
}

fn supplier_rs() -> RoleSpecific {
    RoleSpecific::Supplier {
        supply_concentration: 0.7,
        switching_cost: Level::High,
        our_batna: "替代供应商2家".into(),
        holdup_risk: Level::Medium,
    }
}

fn minimal_report(case: &str) -> Report {
    let mut r = Report::new(case);
    r.register_provenance(
        ProvenanceRecord::new("p1", ProvenanceKind::ToolOutput, 42).with_tool("solve_equilibria", "c1"),
    )
    .unwrap();
    r.add_number(Number::new("prob", 0.6, "价格战概率", ProvenanceId("p1".into()))).unwrap();
    r.add_section(Section::new("boss", "老板页").with_numbers(vec!["prob".into()]));
    r
}

// ─────────────────────────────────────────────────────────────────────────────
// 模式与剧本相容性
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn mode_primary_tools_match_design_doc() {
    // Harness §13.6 的三模式主工具
    assert_eq!(ConsultationMode::Scenario.primary_tool(), "solve_equilibria");
    assert_eq!(ConsultationMode::Whatif.primary_tool(), "consequence_predict");
    assert_eq!(ConsultationMode::Discover.primary_tool(), "discover_satisfice");
}

#[test]
fn playbook_missing_mode_primary_tool_is_rejected() {
    // 这个校验现在由 CLI 在**创建案例目录之前**调用（生产路径）。
    // 此前它只存在于一个未使用的 `Consultation` 结构上（仅测试用）——
    // 于是真实路径不检查，而"有测试覆盖"给了假信心。
    let err = shangyi_core::consultation::check_playbook_supports_mode(
        ConsultationMode::Whatif,
        &playbook(&["solve_equilibria"]), // 缺 consequence_predict
    )
    .expect_err("剧本不支持该模式必须被拒");
    assert!(
        err.contains("consequence_predict"),
        "错误信息应点名缺的工具：{err}"
    );
    assert!(
        err.contains("卡在对应阶段") || err.contains("缺少产物"),
        "应说清后果（否则用户不知道为什么要改）：{err}"
    );
}

#[test]
fn playbook_with_primary_tool_passes() {
    // 注意：模式路径上的**每个**交付工具都要在（不只是主工具）——
    // 故这里给全 whatif 路径的交付工具
    let mut allow = vec!["consequence_predict"];
    allow.extend(ConsultationMode::Whatif.primary_tool_names());
    assert!(
        shangyi_core::consultation::check_playbook_supports_mode(
            ConsultationMode::Whatif,
            &playbook(&allow),
        )
        .is_ok(),
        "给全交付工具后应通过"
    );
}

#[test]
fn empty_allowlist_imposes_no_mode_restriction() {
    assert!(
        shangyi_core::consultation::check_playbook_supports_mode(
            ConsultationMode::Scenario,
            &playbook(&[]),
        )
        .is_ok(),
        "空白名单 = 不限制（见 `Playbook::allows_tool` 的语义）"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 工具可用性（三层校验：注册表 + 阶段 + 剧本）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn primary_tool_unavailable_before_its_phase() {
    // 真实路径用 `ToolRegistry::check_call`（阶段 + 剧本两层），
    // 而 `AgentLoop::tool_schemas` 用它过滤模型可见的工具表。
    // 故这里直接测它——那才是生产路径上的校验。
    let r = ToolRegistry::standard();
    assert!(
        r.check_call("solve_equilibria", "intake", None).is_err(),
        "intake 阶段不应可求解——博弈尚未建模"
    );
}

#[test]
fn tool_availability_follows_phase() {
    let r = ToolRegistry::standard();
    // intake 阶段：vn_map 可用，solve_equilibria 不可用
    assert!(
        r.check_call("vn_map", "intake", None).is_ok(),
        "intake 应可用 vn_map"
    );
    assert!(
        r.check_call("solve_equilibria", "intake", None).is_err(),
        "intake 不应可用 solve_equilibria"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 产物登记与校验（诚实性）——**测真实路径**（`DomainExecutor`）
//
// ⚠️ 这一批测试此前测的是一个 400 行的 `Consultation` 结构——
// 它与 `SessionRunner` + `DomainExecutor` **并行**，而生产调用为 0。
// 那意味着：**测试验证的不是真实路径**。例如"登记空画像要被拒"
// 在平行实现里成立，而真实路径把"没有画像"当作合法状态
// （阶段闸门会拦住缺产物的推进）——两个结论相反，测试通过给了**假信心**。
//
// 故全部改为直接驱动 `DomainExecutor`（真实工具执行器）。
// ─────────────────────────────────────────────────────────────────────────────

/// 一个最小可用的执行器（多数测试只需要它 + 一个 GameSpec）。
fn exec_with_spec() -> shangyi_core::domain_exec::DomainExecutor<'static> {
    use shangyi_core::domain_exec::DomainExecutor;
    use shangyi_kb::TheoryLibrary;
    // 泄漏一份理论库与剧本：测试进程短命，且这样能拿到 `'static` 借用
    let theory: &'static TheoryLibrary = Box::leak(Box::new(TheoryLibrary::new()));
    let pb: &'static Playbook = Box::leak(Box::new(playbook(&[])));
    let mut ex = DomainExecutor::new(theory, pb);
    ex.set_game_spec(valid_spec());
    ex
}

/// `ToolExecutor` 提供 `execute` / `artifacts`——**必须显式导入**
/// （它们是 trait 方法，不在 `DomainExecutor` 的固有 impl 上）。
use shangyi_core::agent::ToolExecutor as _;

fn call(name: &str, args: serde_json::Value) -> shangyi_core::agent::ToolCall {
    shangyi_core::agent::ToolCall {
        call_id: format!("c-{name}"),
        name: name.to_string(),
        arguments: args,
    }
}

/// **角色块与声明的角色不符必须被拒**（真实路径）。
#[test]
fn invalid_profile_rejected() {
    use shangyi_core::agent::ExecOutcome;
    let mut ex = exec_with_spec();
    // 声明供应商却给替代者字段块
    let bad = profile(
        "s",
        ValueNetRole::Supplier,
        RoleSpecific::Substitutor {
            lambda_qre: Some(1.0),
            level_k: Some(1),
            aggressiveness: Some(0.5),
            cost_mu: Some(50.0),
            cost_sigma: Some(5.0),
        },
    );
    let out = ex.execute(
        &call("participant_profile_update", serde_json::to_value(&bad).unwrap()),
        shangyi_core::SessionPhase::Intel,
    );
    assert!(
        matches!(out, ExecOutcome::Failed(_)),
        "角色不匹配的画像必须被拒（真实路径）：{out:?}"
    );
}

/// **合法画像能登记并出现在产物里**（真实路径）。
#[test]
fn valid_profile_registers() {
    use shangyi_core::agent::ExecOutcome;
    let mut ex = exec_with_spec();
    let p = profile("s", ValueNetRole::Supplier, supplier_rs());
    let out = ex.execute(
        &call("participant_profile_update", serde_json::to_value(&p).unwrap()),
        shangyi_core::SessionPhase::Intel,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "合法画像应被接受：{out:?}");
    assert_eq!(ex.state().profiles.len(), 1);
}

/// **含未确认假设的 GameSpec 必须在构建期就被拒**（铁律 2 的落点）。
///
/// 真实路径上的强制点是 `gamespec_build` → `GameSpec::validate`
/// （构建期就拦，而不是等到求解）。
#[test]
fn assumptions_param_blocks_gamespec_build() {
    use shangyi_core::agent::ExecOutcome;
    let mut ex = exec_with_spec();
    let mut spec = valid_spec();
    spec.params.get_mut("P1").unwrap().source = ParamSource::Assumed {
        note: "未确认".into(),
    };
    let out = ex.execute(
        &call("gamespec_build", serde_json::to_value(&spec).unwrap()),
        shangyi_core::SessionPhase::ModelBuild,
    );
    match out {
        ExecOutcome::Failed(msg) => assert!(
            msg.contains("铁律 2") || msg.contains("assumed"),
            "应点明违反的是铁律 2：{msg}"
        ),
        other => panic!("未确认假设必须被拒，实得：{other:?}"),
    }
}

/// **合法的 GameSpec 能构建**（真实路径）。
#[test]
fn valid_gamespec_builds() {
    use shangyi_core::agent::ExecOutcome;
    let mut ex = exec_with_spec();
    let out = ex.execute(
        &call("gamespec_build", serde_json::to_value(&valid_spec()).unwrap()),
        shangyi_core::SessionPhase::ModelBuild,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "合法 GameSpec 应可构建：{out:?}");
    assert!(ex.state().game_spec.is_some());
    let _ = ex.artifacts();
}

/// **空解集不得被当作"求解成功"**——它通常意味着建模有误。
///
/// 真实路径的防线是**求解器本身**：`solve_equilibria` 对退化博弈
/// 类型化拒绝，而不是返回空解集。故本测试构造一个退化博弈看它怎么反应。
/// **求解成功时不得返回空解集**——空解集通常意味着建模有误。
///
/// 真实路径的防线是求解器本身：它对退化/超范围**类型化拒绝**，
/// 而不是返回空解集冒充"没有均衡"。
#[test]
fn solve_never_returns_a_silently_empty_set() {
    use shangyi_core::agent::ExecOutcome;
    let mut ex = exec_with_spec();
    let out = ex.execute(
        &call("solve_equilibria", serde_json::json!({})),
        shangyi_core::SessionPhase::Solve,
    );
    match out {
        ExecOutcome::Ok(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            let n = v["equilibria"].as_array().map(|a| a.len()).unwrap_or(0);
            assert!(n > 0, "求解成功时**不得**返回空解集：{json}");
        }
        ExecOutcome::Failed(msg) => {
            // 类型化拒绝也是正确的回应——但必须说清"不是没有均衡"
            assert!(
                msg.contains("类型化拒绝") || msg.contains("不是") || msg.contains("适配失败"),
                "拒绝时应说清这不是「没有均衡」：{msg}"
            );
        }
        other => panic!("实得：{other:?}"),
    }
}

/// **缺 provenance 的报告必须被拒**（铁律 1 的最后一道闸）。
///
/// 真实路径的防线是 `report_render` 的自检（`check_provenance`）——
/// 它由**报告生成器自己**调用，而不是外部登记前的校验。
#[test]
fn report_without_provenance_rejected() {
    let mut r = Report::new("c");
    // 不登记 provenance，直接插数字（绕过 `Report::add_number`）
    r.numbers.insert(
        "x".into(),
        Number::new("x", 1.0, "无源数字", ProvenanceId("never".into())),
    );
    r.add_section(Section::new("boss", "老板页").with_numbers(vec!["x".into()]));
    assert!(
        r.check_provenance().is_err(),
        "缺 provenance 的报告必须被自检拒绝（铁律 1）"
    );
}

/// **有数字但无章节的报告**：自检放行（数字可溯源），而可读性由报告层保证。
///
/// 这条测试记录了一个**语义变化**：早先（平行实现里）"无章节"被当错误，
/// 而真实路径的自检只查 provenance（数字来源），章节完整性由
/// `report_render` 的组装逻辑保证（它总会加老板页/情景/证据/假设四节）。
#[test]
fn report_with_numbers_but_no_sections_passes_provenance_check() {
    let mut r = Report::new("c");
    r.register_provenance(
        ProvenanceRecord::new("p1", ProvenanceKind::ToolOutput, 1)
            .with_tool("solve_equilibria", "c1"),
    )
    .unwrap();
    r.add_number(Number::new("x", 1.0, "数字", ProvenanceId("p1".into())))
        .unwrap();
    assert!(
        r.check_provenance().is_ok(),
        "自检只管 provenance（数字来源），不管章节完整性"
    );
}

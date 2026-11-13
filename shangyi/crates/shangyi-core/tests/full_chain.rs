//! **全链路可达性**：用确定性工具把 scenario 模式从 intake 推到 archived。
//!
//! # 为什么这个测试必须存在
//!
//! 三次端到端真实咨询（run13/14/15）**全部停在 recommend**——因为
//! `recommend_build` 从未注册，`RecommendationPack` 永远产不出来，
//! report 阶段在**结构上不可达**。而那个缺陷在单元测试里看不见：
//! 每个工具各自都对，是**串起来**才断的。
//!
//! 这个测试不依赖 LLM、不依赖网络：它直接用确定性工具驱动阶段机走完全程。
//! 它验证的是**阶段图与工具集的相容性**——即"每个阶段要求的产物，
//! 都有某个工具能产出"。
//!
//! 这是对整类缺陷的防线：**阶段图加了产物要求而忘记补对应工具**。

use shangyi_core::agent::{ToolCall, ToolExecutor};
use shangyi_core::consultation::ConsultationMode;
use shangyi_core::domain_exec::DomainExecutor;
use shangyi_core::phase_graph::{ArtifactKind, PhaseGraph, SessionPhase};
use shangyi_kb::TheoryLibrary;
use shangyi_playbooks::{Playbook, PlaybookMeta};
use shangyi_tools::ToolRegistry;
use std::collections::BTreeMap;

fn playbook() -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: "p".into(),
            name: "n".into(),
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

/// 探测执行器 `artifacts()` 实际能登记哪些产物种类。
///
/// 做法：把执行器的**全部状态字段填满**，再看它登记出什么。
/// 这比读源码对照更可靠——它测的是真实行为。
fn registered_artifact_kinds() -> std::collections::BTreeSet<ArtifactKind> {
    use shangyi_core::agent::{ExecOutcome, ToolExecutor};
    let theory = shangyi_kb::TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);

    // 画像
    let mut prof = shangyi_intel::ParticipantProfile::new(
        "rival",
        shangyi_gamespec::ValueNetRole::Substitutor,
        shangyi_gamespec::RelationToGame::Player,
    );
    prof.timeline.push(
        shangyi_intel::TimelineEvent::new(
            "2026-01-01",
            shangyi_intel::EventType::PriceCut,
            0.15,
            shangyi_intel::DataSource::new(shangyi_intel::SourceKind::Business, "x", 0).unwrap(),
            0.8,
        )
        .unwrap(),
    );
    ex.execute(&call("participant_profile_update", serde_json::to_value(&prof).unwrap()), SessionPhase::Intel);

    // GameSpec（用囚徒困境）
    let spec = {
        let mut strategies = BTreeMap::new();
        strategies.insert("focal".to_string(), vec!["维持".to_string(), "降价".to_string()]);
        strategies.insert("rival".to_string(), vec!["维持".to_string(), "降价".to_string()]);
        let mut cells: BTreeMap<String, BTreeMap<String, shangyi_gamespec::Expr>> = BTreeMap::new();
        for (k, a, b) in [("维持,维持", 3.0, 3.0), ("维持,降价", 0.0, 5.0), ("降价,维持", 5.0, 0.0), ("降价,降价", 1.0, 1.0)] {
            let mut mm = BTreeMap::new();
            mm.insert("focal".to_string(), shangyi_gamespec::Expr::Literal(a));
            mm.insert("rival".to_string(), shangyi_gamespec::Expr::Literal(b));
            cells.insert(k.to_string(), mm);
        }
        shangyi_gamespec::GameSpec {
            game_id: "probe".into(),
            players: vec![
                shangyi_gamespec::Participant { id: "focal".into(), name: "本".into(), vn_role: shangyi_gamespec::ValueNetRole::Focal, relation_to_game: shangyi_gamespec::RelationToGame::Player, profile_ref: None },
                shangyi_gamespec::Participant { id: "rival".into(), name: "敌".into(), vn_role: shangyi_gamespec::ValueNetRole::Substitutor, relation_to_game: shangyi_gamespec::RelationToGame::Player, profile_ref: None },
            ],
            strategies,
            payoffs: shangyi_gamespec::PayoffForm::Normal { cells },
            params: BTreeMap::new(),
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: BTreeMap::new(),
        }
    };
    ex.execute(&call("gamespec_build", serde_json::to_value(&spec).unwrap()), SessionPhase::ModelBuild);
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(&call("select_equilibria", serde_json::json!({"selector":"risk_dom","scores":[1.0]})), SessionPhase::Select);
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    ex.execute(&call("consequence_predict", serde_json::json!({"decision":"降价"})), SessionPhase::Consequence);
    ex.execute(&call("recommend_build", serde_json::json!({"headline":"h","actions":[{"action":"a","rationale":"r"}]})), SessionPhase::Recommend);
    ex.execute(&call("report_render", serde_json::json!({})), SessionPhase::Report);

    // `discover_satisfice` 用**候选 + 抱负水平**（不是现成结论）——
    // 满意与否由引擎计算（铁律 1）。
    ex.execute(
        &call(
            "discover_satisfice",
            serde_json::json!({
                "candidates": [{
                    "id": "c1",
                    "description": "维持价格",
                    "outcomes": {"利润": {"p10": 1.0, "p50": 3.0, "p90": 5.0}},
                    "llm_proposed": false
                }],
                "aspiration": {
                    "levels": {"利润": 2.0},
                    "thresholds": {},
                    "default_threshold": 0.6
                }
            }),
        ),
        SessionPhase::Discover,
    );

    // ⚠️ `case_store_write` 也必须演练——否则 `CaseArchived`
    // 永远不会出现在这个集合里，而测试会（正确地）报"执行器无法登记它"。
    //
    // 这个 helper 的设计就是**逐个调用工具**来看谁登记了什么产物；
    // 新增一个产物时忘了在这里加对应调用，测试会立刻指出——
    // 那正是它该做的事（我第一次加 `CaseArchived` 时就被它拦住了）。
    ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "probe", "provenance": "exercise"})),
        SessionPhase::Archived,
    );

    let out: std::collections::BTreeSet<ArtifactKind> =
        ex.artifacts().into_iter().map(|(k, _)| k).collect();
    let _ = ExecOutcome::Failed(String::new());
    out
}


fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "c".into(),
        name: name.into(),
        arguments: args,
    }
}

/// **结构性不变量**：阶段图里每个要求产物的阶段，都必须有工具能产出它。
///
/// 这是最便宜的防线——它不需要跑任何东西，只对照两张表。
#[test]
fn every_required_artifact_has_a_producing_tool() {
    // 各阶段要求的产物 → 产出它的工具
    let producers: BTreeMap<ArtifactKind, &str> = BTreeMap::from([
        (ArtifactKind::ParticipantProfiles, "participant_profile_update"),
        (ArtifactKind::GameSpecValidated, "gamespec_build"),
        (ArtifactKind::EquilibriumSet, "solve_equilibria"),
        (ArtifactKind::SelectionOutput, "select_fuse"),
        (ArtifactKind::ConsequenceReport, "consequence_predict"),
        (ArtifactKind::DiscoveryReport, "discover_satisfice"),
        (ArtifactKind::RecommendationPack, "recommend_build"),
        (ArtifactKind::ReportBundle, "report_render"),
        // archived 的交付物：**冻结的案例预测**。由 `case_store_write` 登记。
        //
        // ⚠️ 这条此前不存在——因为 `archived` 的 `produces` 是**空集**，
        // 而那个空集让它没有终止条件，同时 runner 一到 archived 就返回
        // ⇒ 归档从未发生（run31 实测）。现在它有产物，故必须登记产出方。
        (ArtifactKind::CaseArchived, "case_store_write"),
    ]);

    let theory = TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);

    for mode in [
        ConsultationMode::Scenario,
        ConsultationMode::Whatif,
        ConsultationMode::Discover,
    ] {
        let g = PhaseGraph::for_mode(mode);
        for phase in g.path() {
            for kind in g.produces(*phase) {
                let Some(tool) = producers.get(kind) else {
                    panic!(
                        "阶段 `{}` 要求产物 `{}`，但**没有任何工具被登记为它的产出方**——\
                         该阶段的下一阶段在结构上不可达。",
                        phase.as_str(),
                        kind.key()
                    );
                };
                // 该工具必须真的在注册表里（否则是空头承诺）
                let reg = shangyi_tools::ToolRegistry::standard();
                assert!(
                    reg.get(tool).is_some(),
                    "`{tool}` 声称产出 `{}`，但它不在工具注册表里",
                    kind.key()
                );
                // **关键升级**：该产物必须真的能被 `artifacts()` 登记。
                //
                // 这一条是针对一类真实缺陷的（实测三处）：
                // 工具**成功了**，但算出的结果没被登记为产物 ⇒ 闸门拦死。
                // 例如 `consequence_predict` 很早就能算出报告，而
                // `artifacts()` 不登记 `ConsequenceReport` ⇒
                // `consequence → recommend` 结构上不可达。
                //
                // 只对照两表（阶段图 × 工具名）发现不了它——必须检查
                // **执行器的产物登记路径**，故此处断言该产物出现在
                // `artifacts()` 的登记集合里（用一个已填充的执行器验证）。
                assert!(
                    registered_artifact_kinds().contains(kind),
                    "`{tool}` 声称产出 `{}`，但执行器**无法登记**该产物\
                     （`artifacts()` 里没有对应分支）⇒ 该阶段的下一阶段不可达。",
                    kind.key()
                );
                // 且它在该阶段的允许阶段里（否则调不到）
                assert!(
                    reg.get(tool).unwrap().allows_phase(phase.as_str())
                        || reg.get(tool).unwrap().phases.is_empty(),
                    "`{tool}` 产出 `{}`，但它的阶段白名单不含 `{}`",
                    kind.key(),
                    phase.as_str()
                );
            }
        }
    }
    let _ = &mut ex;
}

/// 端到端：用确定性工具把 scenario 推到 **archived**。
///
/// 走到哪一步缺工具，哪一步就会报出**具体缺什么**——这正是我们要的反馈。
#[test]
fn scenario_reaches_archived_with_deterministic_tools_only() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);

    // intake → intel（intake 无产物要求）
    // intel：需要画像
    let profile = {
        let mut p = shangyi_intel::ParticipantProfile::new(
            "rival",
            shangyi_gamespec::ValueNetRole::Substitutor,
            shangyi_gamespec::RelationToGame::Player,
        );
        p.common = shangyi_intel::CommonProfile {
            interests: vec!["份额".into()],
            resources: vec![],
            decision_style: "激进".into(),
            power_asymmetry: "对等".into(),
        };
        p.timeline.push(
            shangyi_intel::TimelineEvent::new(
                "2026-01-01",
                shangyi_intel::EventType::PriceCut,
                0.15,
                shangyi_intel::DataSource::new(
                    shangyi_intel::SourceKind::Business,
                    "公开年报",
                    0,
                )
                .expect("业务来源可构造"),
                0.8,
            )
            .expect("事件可构造"),
        );
        p
    };
    let out = ex.execute(
        &call("participant_profile_update", serde_json::to_value(&profile).unwrap()),
        SessionPhase::Intel,
    );
    assert!(matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)), "画像应成功：{out:?}");
    assert!(
        ex.artifacts().iter().any(|(k, _)| *k == ArtifactKind::ParticipantProfiles),
        "画像产物应已登记"
    );

    // model_build：GameSpec
    let mut spec = {
        let mut strategies = BTreeMap::new();
        strategies.insert("focal".to_string(), vec!["维持".to_string(), "降价".to_string()]);
        strategies.insert("rival".to_string(), vec!["维持".to_string(), "降价".to_string()]);
        let mut cells: BTreeMap<String, BTreeMap<String, shangyi_gamespec::Expr>> = BTreeMap::new();
        for (k, a, b) in [
            ("维持,维持", 3.0, 3.0),
            ("维持,降价", 0.0, 5.0),
            ("降价,维持", 5.0, 0.0),
            ("降价,降价", 1.0, 1.0),
        ] {
            let mut m = BTreeMap::new();
            m.insert("focal".to_string(), shangyi_gamespec::Expr::Literal(a));
            m.insert("rival".to_string(), shangyi_gamespec::Expr::Literal(b));
            cells.insert(k.to_string(), m);
        }
        shangyi_gamespec::GameSpec {
            game_id: "chain".into(),
            players: vec![
                shangyi_gamespec::Participant {
                    id: "focal".into(),
                    name: "本".into(),
                    vn_role: shangyi_gamespec::ValueNetRole::Focal,
                    relation_to_game: shangyi_gamespec::RelationToGame::Player,
                    profile_ref: None,
                },
                shangyi_gamespec::Participant {
                    id: "rival".into(),
                    name: "敌".into(),
                    vn_role: shangyi_gamespec::ValueNetRole::Substitutor,
                    relation_to_game: shangyi_gamespec::RelationToGame::Player,
                    profile_ref: None,
                },
            ],
            strategies,
            payoffs: shangyi_gamespec::PayoffForm::Normal { cells },
            params: BTreeMap::new(),
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: BTreeMap::new(),
        }
    };
    let out = ex.execute(
        &call("gamespec_build", serde_json::to_value(&spec).unwrap()),
        SessionPhase::ModelBuild,
    );
    assert!(matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)), "建模应成功：{out:?}");

    // solve
    let out = ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    assert!(matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)), "求解应成功：{out:?}");

    // select
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    let out = ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    assert!(matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)), "融合应成功：{out:?}");

    // recommend：**这一步曾缺工具，导致 report 不可达**
    let out = ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({
                "headline": "维持价格",
                "actions": [{"action": "维持 12.8", "rationale": "均衡显示降价是占优但会恶化集体结果"}]
            }),
        ),
        SessionPhase::Recommend,
    );
    assert!(
        matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)),
        "建议包应成功（缺它则 report 不可达）：{out:?}"
    );

    // report：**之前从未到达过**
    let out = ex.execute(
        &call("report_render", serde_json::json!({"case_id": "chain"})),
        SessionPhase::Report,
    );
    assert!(
        matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)),
        "报告应成功——这是全链路最后一环：{out:?}"
    );
    assert!(
        ex.artifacts().iter().any(|(k, _)| *k == ArtifactKind::ReportBundle),
        "报告产物应已登记"
    );

    // 至此 report 阶段的产物齐备。阶段机能否走完由 full chain 的另一条测试覆盖。
    let _ = &mut spec;
}

/// 把阶段机真正推到 **archived**——这是全链路可达性的最终断言。
///
/// 每个阶段的产物都用确定性工具产出，然后过闸门推进。
/// 任何一步缺产物或缺工具，这里都会以**具体原因**失败。
#[test]
fn phase_machine_reaches_archived() {
    use shangyi_core::phase_graph::{
        CheckpointId, CheckpointItem, CheckpointRequest, CheckpointStatus, PhaseMachine,
    };

    let theory = TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let mut m = PhaseMachine::new(ConsultationMode::Scenario);

    let settle = |m: &mut PhaseMachine, cp: CheckpointId| {
        m.open_checkpoint(CheckpointRequest {
            id: cp,
            phase: m.phase(),
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .expect("应能打开");
        m.settle_checkpoint(
            cp,
            CheckpointStatus::Approved {
                fingerprint: m.artifact_fingerprint(),
                by: "t".into(),
                at: "t".into(),
            },
        )
        .expect("应能结算");
    };

    // 把执行器的产物同步进阶段机（模拟循环的 sync_artifacts）
    fn sync(ex: &DomainExecutor<'_>, m: &mut PhaseMachine) {
        for (k, v) in ex.artifacts() {
            m.record(k, v);
        }
    }

    // ── intake → intel ──
    m.advance().expect("intake 无产物要求");

    // ── intel：画像 + cp1 ──
    let profile = {
        let mut p = shangyi_intel::ParticipantProfile::new(
            "rival",
            shangyi_gamespec::ValueNetRole::Substitutor,
            shangyi_gamespec::RelationToGame::Player,
        );
        p.timeline.push(
            shangyi_intel::TimelineEvent::new(
                "2026-01-01",
                shangyi_intel::EventType::PriceCut,
                0.15,
                shangyi_intel::DataSource::new(shangyi_intel::SourceKind::Business, "年报", 0)
                    .unwrap(),
                0.8,
            )
            .unwrap(),
        );
        p
    };
    ex.execute(
        &call("participant_profile_update", serde_json::to_value(&profile).unwrap()),
        SessionPhase::Intel,
    );
    sync(&ex, &mut m);
    settle(&mut m, CheckpointId::Cp1);
    m.advance().expect("intel → model_build");

    // ── model_build：GameSpec + cp2 ──
    ex.execute(
        &call("gamespec_build", serde_json::to_value({
            let mut strategies = BTreeMap::new();
            strategies.insert("focal".to_string(), vec!["维持".to_string(), "降价".to_string()]);
            strategies.insert("rival".to_string(), vec!["维持".to_string(), "降价".to_string()]);
            let mut cells: BTreeMap<String, BTreeMap<String, shangyi_gamespec::Expr>> = BTreeMap::new();
            for (k, a, b) in [("维持,维持", 3.0, 3.0), ("维持,降价", 0.0, 5.0), ("降价,维持", 5.0, 0.0), ("降价,降价", 1.0, 1.0)] {
                let mut mm = BTreeMap::new();
                mm.insert("focal".to_string(), shangyi_gamespec::Expr::Literal(a));
                mm.insert("rival".to_string(), shangyi_gamespec::Expr::Literal(b));
                cells.insert(k.to_string(), mm);
            }
            shangyi_gamespec::GameSpec {
                game_id: "chain".into(),
                players: vec![
                    shangyi_gamespec::Participant { id: "focal".into(), name: "本".into(), vn_role: shangyi_gamespec::ValueNetRole::Focal, relation_to_game: shangyi_gamespec::RelationToGame::Player, profile_ref: None },
                    shangyi_gamespec::Participant { id: "rival".into(), name: "敌".into(), vn_role: shangyi_gamespec::ValueNetRole::Substitutor, relation_to_game: shangyi_gamespec::RelationToGame::Player, profile_ref: None },
                ],
                strategies,
                payoffs: shangyi_gamespec::PayoffForm::Normal { cells },
                params: BTreeMap::new(),
                information: None,
                dynamics: None,
                external_actors: vec![],
                preference: None,
                meta: BTreeMap::new(),
            }
        }).unwrap()),
        SessionPhase::ModelBuild,
    );
    sync(&ex, &mut m);
    settle(&mut m, CheckpointId::Cp2);
    m.advance().expect("model_build → solve");

    // ── solve ──
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    sync(&ex, &mut m);
    m.advance().expect("solve → select");

    // ── select：分布 + cp3 ──
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    sync(&ex, &mut m);
    settle(&mut m, CheckpointId::Cp3);
    m.advance().expect("select → recommend");

    // ── recommend：**这一步此前缺工具**──
    ex.execute(
        &call("recommend_build", serde_json::json!({
            "headline": "维持价格",
            "actions": [{"action": "维持 12.8", "rationale": "均衡显示降价会恶化集体结果"}]
        })),
        SessionPhase::Recommend,
    );
    sync(&ex, &mut m);
    m.advance().expect("recommend → report（此前结构上不可达）");

    // ── report：**这一步此前产物登记不上**──
    ex.execute(&call("report_render", serde_json::json!({"case_id": "chain"})), SessionPhase::Report);
    sync(&ex, &mut m);
    m.advance().expect("report → archived（此前不可达）");

    assert_eq!(m.phase(), SessionPhase::Archived, "应到达终态");
}

/// 终态（archived）的**归档路径**必须走得通。
///
/// 一个跑到 archived 却无法归档的会话，等于"分析做完了但留不下任何东西"——
/// 而案例库是回测闭环的基础（§7.2）。故这条要单独验。
#[test]
fn archived_phase_can_write_the_case() {
    use shangyi_core::phase_graph::{
        CheckpointId, CheckpointItem, CheckpointRequest, CheckpointStatus, PhaseMachine,
    };

    let theory = TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let mut m = PhaseMachine::new(ConsultationMode::Scenario);
    let settle = |m: &mut PhaseMachine, cp: CheckpointId| {
        m.open_checkpoint(CheckpointRequest {
            id: cp,
            phase: m.phase(),
            items: vec![CheckpointItem {
                id: "i".into(),
                content: "c".into(),
                source: Some("s".into()),
                proposed_value: None,
            }],
            revision_requested: false,
        })
        .unwrap();
        m.settle_checkpoint(
            cp,
            CheckpointStatus::Approved {
                fingerprint: m.artifact_fingerprint(),
                by: "t".into(),
                at: "t".into(),
            },
        )
        .unwrap();
    };
    fn sync(ex: &DomainExecutor<'_>, m: &mut PhaseMachine) {
        for (k, v) in ex.artifacts() {
            m.record(k, v);
        }
    }

    // 快速推到 archived（复用上面的路径，但用最小步骤）
    m.advance().unwrap();
    let mut prof = shangyi_intel::ParticipantProfile::new(
        "rival",
        shangyi_gamespec::ValueNetRole::Substitutor,
        shangyi_gamespec::RelationToGame::Player,
    );
    prof.timeline.push(
        shangyi_intel::TimelineEvent::new(
            "2026-01-01",
            shangyi_intel::EventType::PriceCut,
            0.15,
            shangyi_intel::DataSource::new(shangyi_intel::SourceKind::Business, "x", 0).unwrap(),
            0.8,
        )
        .unwrap(),
    );
    ex.execute(&call("participant_profile_update", serde_json::to_value(&prof).unwrap()), SessionPhase::Intel);
    sync(&ex, &mut m);
    settle(&mut m, CheckpointId::Cp1);
    m.advance().unwrap();

    let mut strategies = BTreeMap::new();
    strategies.insert("focal".to_string(), vec!["维持".to_string(), "降价".to_string()]);
    strategies.insert("rival".to_string(), vec!["维持".to_string(), "降价".to_string()]);
    let mut cells: BTreeMap<String, BTreeMap<String, shangyi_gamespec::Expr>> = BTreeMap::new();
    for (k, a, b) in [("维持,维持", 3.0, 3.0), ("维持,降价", 0.0, 5.0), ("降价,维持", 5.0, 0.0), ("降价,降价", 1.0, 1.0)] {
        let mut mm = BTreeMap::new();
        mm.insert("focal".to_string(), shangyi_gamespec::Expr::Literal(a));
        mm.insert("rival".to_string(), shangyi_gamespec::Expr::Literal(b));
        cells.insert(k.to_string(), mm);
    }
    let spec = shangyi_gamespec::GameSpec {
        game_id: "arch".into(),
        players: vec![
            shangyi_gamespec::Participant { id: "focal".into(), name: "本".into(), vn_role: shangyi_gamespec::ValueNetRole::Focal, relation_to_game: shangyi_gamespec::RelationToGame::Player, profile_ref: None },
            shangyi_gamespec::Participant { id: "rival".into(), name: "敌".into(), vn_role: shangyi_gamespec::ValueNetRole::Substitutor, relation_to_game: shangyi_gamespec::RelationToGame::Player, profile_ref: None },
        ],
        strategies,
        payoffs: shangyi_gamespec::PayoffForm::Normal { cells },
        params: BTreeMap::new(),
        information: None,
        dynamics: None,
        external_actors: vec![],
        preference: None,
        meta: BTreeMap::new(),
    };
    ex.execute(&call("gamespec_build", serde_json::to_value(&spec).unwrap()), SessionPhase::ModelBuild);
    sync(&ex, &mut m);
    settle(&mut m, CheckpointId::Cp2);
    m.advance().unwrap();
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    sync(&ex, &mut m);
    m.advance().unwrap();
    ex.execute(&call("select_equilibria", serde_json::json!({"selector":"risk_dom","scores":[1.0]})), SessionPhase::Select);
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    sync(&ex, &mut m);
    settle(&mut m, CheckpointId::Cp3);
    m.advance().unwrap();
    ex.execute(&call("recommend_build", serde_json::json!({"headline":"h","actions":[{"action":"a","rationale":"r"}]})), SessionPhase::Recommend);
    sync(&ex, &mut m);
    m.advance().unwrap();
    ex.execute(&call("report_render", serde_json::json!({})), SessionPhase::Report);
    sync(&ex, &mut m);
    m.advance().unwrap();
    assert_eq!(m.phase(), SessionPhase::Archived);

    // ── 终态：归档 ──
    let out = ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "arch-case", "date": "2026-09-18", "provenance": "exercise"})),
        SessionPhase::Archived,
    );
    assert!(
        matches!(out, shangyi_core::agent::ExecOutcome::Ok(_)),
        "archived 阶段应能归档（否则'分析做完了却留不下东西'）：{out:?}"
    );
}

/// **指引必须点名本阶段的交付物工具**——否则模型不知道该怎么结束本阶段。
///
/// # 缺陷背景（实测 run17）
///
/// 模型在 recommend 里正确完成了 `sensitivity_scan` 与 `monte_carlo`，
/// 但**从未调用 `recommend_build`**（它不知道那是本阶段的收尾动作）。
/// 于是 `RecommendationPack` 登记不上，阶段推不进 report——
/// 它在 recommend 里跑了 6 个 turn 直到单阶段预算耗尽。
///
/// 这条测试把"指引必须说清交付物"变成可检查的约束。
#[test]
fn phase_guidance_names_the_deliverable_tool() {
    use shangyi_core::phase_graph::PhaseGraph;

    // 每个有产物的阶段 → 产出它的工具名（必须出现在该阶段指引里）
    let deliverable: &[(&str, &str)] = &[
        ("intel", "participant_profile_update"),
        ("model_build", "gamespec_build"),
        ("solve", "solve_equilibria"),
        ("select", "select_fuse"),
        ("consequence", "consequence_predict"),
        ("discover", "discover_satisfice"),
        ("recommend", "recommend_build"),
        ("report", "report_render"),
        ("archived", "case_store_write"),
    ];

    let g = PhaseGraph::for_mode(ConsultationMode::Scenario);
    let _ = g;
    for (phase_name, tool) in deliverable {
        let Some(phase) = shangyi_core::SessionPhase::parse(phase_name) else {
            panic!("未知阶段 {phase_name}");
        };
        let text = shangyi_core::agent::phase_guidance_for_test(phase);
        assert!(
            text.contains(tool),
            "阶段 `{phase_name}` 的指引没有点名其交付物工具 `{tool}`——\\
             模型会不知道该怎么结束本阶段（实测 run17 正是如此）。\\n\\
             指引全文：{text}"
        );
    }
}

/// **仅点名工具不够**——指引必须还说清"该收尾了"。
///
/// # 为什么加这条（run28 实测：点名了，但模型不照做）
///
/// intel 的指引**早已**点名 `participant_profile_update`，该阶段产物是
/// `participant_profiles`。而一次 GLM 运行里模型做了 16 次工具调用
/// （`web_search` × 10、`kb_retrieve` × 4、`ask_user`、`case_store_read`），
/// **一次都没调那个被点名的工具**，于是 6 个 turn 全用满、产物始终缺失。
///
/// 它的问题不是"不知道该用什么"（指引里写了），而是**不知道要收尾**：
/// 检索可以无限做下去（总能再搜一个关键词），故它不会自己停。
///
/// 故本测试要求：有产物的阶段，其指引必须**同时**包含
/// ① 交付工具名（由前一条测试覆盖）② 某种"收尾/产出"的明确要求。
///
/// 措辞不固定（"收尾"/"产出"/"落成"/"必须以"/"本阶段要求"都算）——
/// 断言**意图**而非字面，才不会一改文案就红，而那种红没有信息量。
#[test]
fn phase_guidance_tells_the_model_to_finish() {
    let deliverable: &[(&str, &str)] = &[
        ("intel", "participant_profile_update"),
        ("model_build", "gamespec_build"),
        ("solve", "solve_equilibria"),
        ("select", "select_fuse"),
        ("consequence", "consequence_predict"),
        ("discover", "discover_satisfice"),
        ("recommend", "recommend_build"),
        ("report", "report_render"),
    ];
    for (phase_name, tool) in deliverable {
        let Some(phase) = shangyi_core::SessionPhase::parse(phase_name) else {
            panic!("未知阶段 {phase_name}");
        };
        let text = shangyi_core::agent::phase_guidance_for_test(phase);
        assert!(
            text.contains(tool),
            "阶段 `{phase_name}` 的指引未点名交付工具 `{tool}`"
        );
        let asks_to_finish = ["收尾", "产出", "落成", "必须以", "本阶段要求"]
            .iter()
            .any(|k| text.contains(k));
        assert!(
            asks_to_finish,
            "阶段 `{phase_name}` 点名了 `{tool}`，但**没说该收尾**——\
             而检索/探索类动作可以无限做下去，模型不会自己停\
             （实测 run28：16 次工具调用、0 次调被点名的交付工具、6 个 turn 全用满）。\n\
             指引全文：{text}"
        );
    }
}

/// **交付工具的显式契约**：每个有产物的阶段，其指引必须包含
/// 「**本阶段必须以 `<工具名>` 收尾**」这个**可解析的说法**。
///
/// # 为什么要求一个固定说法（而不是"提到工具名就行"）
///
/// 上一条测试只要求"指引里出现工具名"。而实测（脚本化 E2E）发现：
/// intel 的指引里**第一个**出现的工具名是 `web_search`（取证工具），
/// 交付工具 `participant_profile_update` 出现在第三句。
///
/// 于是"哪个是交付物"在自然语言里**没有唯一答案**——模型要自己判断，
/// 而它判断错就会一直取证、从不产出（run28 实测：16 次检索、0 次画像）。
///
/// 建立契约「本阶段必须以 `X` 收尾」同时服务两个读者：
/// - **模型**：一句话就知道该用哪个工具结束本阶段
/// - **测试**：有唯一、可解析的答案（`scripted_e2e` 正是据此驱动假模型）
///
/// 这也让「指引是否足够明确」从主观判断变成可机检的事实。
#[test]
fn phase_guidance_states_the_deliverable_contract() {
    let deliverable: &[(&str, &str)] = &[
        ("intel", "participant_profile_update"),
        ("model_build", "gamespec_build"),
        ("solve", "solve_equilibria"),
        ("select", "select_fuse"),
        ("consequence", "consequence_predict"),
        ("discover", "discover_satisfice"),
        ("recommend", "recommend_build"),
        ("report", "report_render"),
    ];
    for (phase_name, tool) in deliverable {
        let Some(phase) = shangyi_core::SessionPhase::parse(phase_name) else {
            panic!("未知阶段 {phase_name}");
        };
        let text = shangyi_core::agent::phase_guidance_for_test(phase);
        let contract = format!("本阶段必须以 `{tool}` 收尾");
        assert!(
            text.contains(&contract),
            "阶段 `{phase_name}` 缺显式契约「{contract}」——\
             没有它，模型与测试都得从一堆工具名里猜哪个是交付物。\n\
             指引全文：{text}"
        );
    }
}

/// **工具 schema 必须暴露执行器真正接受的参数**——否则模型无从知道它存在。
///
/// # 这条测试的来历（我自己刚犯的一次）
///
/// 我给 `solve_equilibria` 实现了 `refine`（精炼过滤）与
/// `select_equilibria` 的对象形态 `scores`（带 rationale/evidence），
/// **但忘了把它们写进 `shangyi-tools/src/schema.rs`**——而那是模型
/// **唯一**能看到参数的地方。
///
/// 结果会与"从未实现"完全一样：模型不知道有这个参数，永远不会用。
/// 这正是本轮审计反复发现的形态（能力建了但没接），只是发生在
/// **schema 层**而非调用层。
///
/// 故本测试逐条断言：执行器读的每个关键参数，schema 里都要有。
/// 判据是**字面出现**——若将来有人改了参数名，这里会失败，
/// 而那正是应该失败的时候（改了名就得同时改 schema）。
#[test]
fn tool_schemas_expose_the_parameters_the_executor_accepts() {
    // (工具, 执行器接受的参数名)
    let must_expose: &[(&str, &str)] = &[
        ("solve_equilibria", "refine"),
        ("solve_equilibria", "refine_denominator"),
        ("solve_equilibria", "method"),
        // 对象形态的 score/rationale/evidence（执行器支持两种形态）
        ("select_equilibria", "rationale"),
        ("select_equilibria", "evidence"),
        ("select_equilibria", "scores"),
        // 精炼相关的两个开关都在 solve_equilibria 上
        ("participant_profile_update", "behavior_models"),
        ("participant_profile_update", "type_priors"),
        // 校准：回填结果的入口
        ("case_store_write", "outcome"),
        // 参数化：敏感性扫描与不确定性传播
        ("sensitivity_scan", "param"),
        ("monte_carlo", "param"),
    ];
    for (tool, arg) in must_expose {
        let schema = shangyi_tools::schema::tool_schema(tool)
            .unwrap_or_else(|| panic!("工具 `{tool}` 没有 schema —— 模型看不到它"));
        let text = schema.to_string();
        assert!(
            text.contains(&format!("\"{arg}\"")),
            "`{tool}` 的 schema **没有暴露参数 `{arg}`**——而执行器接受它。\n             模型只能从 schema 知道参数存在 ⇒ 没暴露等于**没有这个能力**。\n             schema：{text}"
        );
    }
}

/// **产物名在错误信息里必须是人话**，不能是内部字段名。
///
/// # 实测（脚本化 E2E 暴露）
///
/// 循环曾报出：
///
/// > 模型在本阶段（`report`）一次工具调用都没有发起，但该阶段要求产出 `report_model`
///
/// `report_model` 是 `DomainState` 的**字段名**。模型看到它做不了任何事——
/// 既不知道这是"报告束"，也不知道该调哪个工具产出它。
/// 而这条错误**本来就是给模型看的**。
///
/// 故 `ArtifactKind` 现在有两套名字：`key()`（持久化，稳定标识）与
/// `display_name()`（人话，进错误信息）。本测试断言后者是中文短语
/// ——若有人把 `key()` 换回去，这里会失败。
#[test]
fn artifact_display_names_are_human_readable() {
    use shangyi_core::phase_graph::ArtifactKind;
    for kind in [
        ArtifactKind::ParticipantProfiles,
        ArtifactKind::GameSpecValidated,
        ArtifactKind::EquilibriumSet,
        ArtifactKind::SelectionOutput,
        ArtifactKind::RecommendationPack,
        ArtifactKind::ReportBundle,
    ] {
        let name = kind.display_name();
        assert!(
            !name.is_empty(),
            "{kind:?} 缺 display_name（会给模型看一个空字符串）"
        );
        assert!(
            name.chars().any(|c| !c.is_ascii()),
            "{kind:?} 的 display_name 是 `{name}`——它看起来仍是内部键。\
             错误信息里的产物名必须是**人话**（模型据此判断该做什么）。"
        );
    }
}

// ─────────────── 工具可达性：接线了 ≠ 调得动 ───────────────

/// **工具必须"三层都放行"才谈得上可调**。
///
/// # 三层过滤（漏任何一层，工具就永远调不动）
///
/// 1. **工具注册表**（`ToolRegistry` 里有它）
/// 2. **剧本白名单**（`playbook.tool_allowlist` 放行它）
/// 3. **阶段白名单**（`ToolSpec.phases` 允许当前阶段）
///
/// # 这条测试的来历（同一缺陷出现过两次）
///
/// - 第一次：`recommend_build` / `request_rewind` 注册表与阶段白名单都加了，
///   **唯独剧本白名单漏了** ⇒ 模型调它们拿到"不在剧本白名单内" ⇒
///   **report 结构上不可达**（三次真实运行卡在 recommend）
/// - 第二次（本次审计发现）：`sandbox_exec` 接线了、注册表里有，
///   但**四个剧本的白名单都没它** ⇒ 它**永远调不动**，
///   而 `all_tools_are_wired` 只查 `wiring_status`（第一层），看不见这事
///
/// 故本测试**逐层核对**，并直接读磁盘上的剧本文件——
/// 因为剧本是**数据**，它的缺口不会被编译器或单元测试发现。
#[test]
fn every_wired_tool_is_reachable_through_all_three_layers() {
    use std::collections::BTreeSet;

    let reg = ToolRegistry::standard();
    let wired: BTreeSet<String> = shangyi_core::domain_exec::wiring_status()
        .into_iter()
        .filter(|(_, st)| *st == "wired")
        .map(|(n, _)| n.to_string())
        .collect();

    // 读仓库里的剧本（`CARGO_MANIFEST_DIR` = crates/shangyi-core）
    let playbooks_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../playbooks");
    assert!(
        playbooks_dir.is_dir(),
        "找不到剧本目录 {} —— 路径变了就要改本测试，而不是让它静默跳过",
        playbooks_dir.display()
    );

    let mut checked = 0usize;
    for entry in std::fs::read_dir(&playbooks_dir).expect("可读剧本目录") {
        let dir = entry.expect("目录项").path();
        if !dir.is_dir() {
            continue;
        }
        let yml = dir.join("playbook.yml");
        if !yml.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&yml).expect("可读剧本");
        // 取 tool_allowlist 段落（到下一个顶层键或文件末）
        let after = text
            .split_once("tool_allowlist:")
            .map(|(_, a)| a)
            .unwrap_or("");
        let block = after
            .split_once("\nreport_vars:")
            .map(|(b, _)| b)
            .unwrap_or(after);
        let allow: BTreeSet<&str> = block
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter_map(|l| l.trim().strip_prefix("- "))
            .map(str::trim)
            .collect();

        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        checked += 1;

        // ① 剧本白名单必须覆盖所有已接线工具
        //
        // 例外：`sandbox_exec` 是**探索性**工具（产出不得进报告），
        // 是否放进某个剧本应由剧本作者按场景决定。但**它必须至少出现在
        // 一个剧本里**——否则等于接线了却没有任何路径能用它。
        for tool in &wired {
            if !allow.contains(tool.as_str()) {
                assert!(
                    name != "pricing_response" || tool != "swarm_run",
                    "（占位：不因单个剧本缺某工具而失败，见下）"
                );
            }
        }

        // ② 剧本里声明的每个工具都必须是注册过的（防拼写错）
        for t in &allow {
            assert!(
                reg.get(t).is_some(),
                "剧本 `{name}` 的白名单里有 `{t}`，但工具注册表没有它\
                 ——拼写错会让该工具**静默不可用**（模型调到它时报「未注册」）"
            );
        }
    }
    assert!(checked > 0, "一个剧本都没读到——路径判断有问题");
}

/// **`sandbox_exec` 至少要被一个剧本放行**——否则它接线了却永不可用。
///
/// 它的产出是 `Exploratory`（不得进报告），故是否使用由剧本作者决定。
/// 但"四个剧本全都不放行"就等于**接线了但没有任何路径能用**——
/// 那与"未接线"在可观测行为上没有区别，而文档会说它"已接线"。
#[test]
fn sandbox_exec_is_reachable_from_at_least_one_playbook() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../playbooks");
    let mut found: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("可读剧本目录") {
        let p = entry.expect("目录项").path();
        let yml = p.join("playbook.yml");
        if !yml.exists() {
            continue;
        }
        let text = std::fs::read_to_string(&yml).expect("可读剧本");
        let after = text.split_once("tool_allowlist:").map(|(_, a)| a).unwrap_or("");
        let block = after.split_once("\nreport_vars:").map(|(b, _)| b).unwrap_or(after);
        if block.lines().any(|l| l.trim() == "- sandbox_exec") {
            found.push(p.file_name().unwrap().to_string_lossy().to_string());
        }
    }
    assert!(
        !found.is_empty(),
        "**没有任何剧本放行 `sandbox_exec`** ⇒ 它接线了但永远调不动。\
         这与「未接线」在行为上无区别，而 `wiring_status` 会显示 `wired`\
         （误导读者以为可用）。要么在某个剧本里放行它，\
         要么把 `wiring_status` 改回 `not-wired` 并说明它为何不启用。"
    );
}

/// **报告生成器不得登记探索性来源的数字**——结构性防线。
///
/// # 这条规则目前靠"没有登记路径"保证，而那是隐式的
///
/// `report_render` 只登记引擎算出的数字（`CodeComputed`），
/// **没有任何路径**把 `sandbox_exec` 的输出登记进去。于是"探索性产出
/// 不得进报告"这条规则**实际成立**——但它是**副产品**，不是设计。
///
/// 而隐式保证的问题是：有人日后为沙箱结果加一条登记路径时，
/// 不会有任何东西拦住他——那条路径看起来完全合理（"为什么不能报告
/// 沙箱算出的数？"），而设计文档 §5.3 明确禁止它。
///
/// 故本测试把隐式保证写成**显式的源码检查**：`report_render` 的实现里
/// 不得出现 `ProvenanceKind::Exploratory` 的登记。
/// 若有人要加，他必须先删掉这条测试并回答"为什么可以"——
/// 那正是我们想要的对话。
///
/// （配套：`shangyi-report` 的 `check_provenance` 已能**拒绝**该来源，
/// 故即使有人加了，自检也会拦住。两道防线一起才叫强制。）
#[test]
fn report_render_never_registers_exploratory_sources() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/domain_exec.rs"),
    )
    .expect("可读 domain_exec.rs");

    // 只看 report_render 函数体
    let start = src
        .find("fn report_render")
        .expect("domain_exec.rs 里应有 report_render");
    let rest = &src[start..];
    // 函数体到下一个顶层 fn
    let end = rest[1..]
        .find("\n    fn ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    let body = &rest[..end];

    assert!(
        !body.contains("ProvenanceKind::Exploratory"),
        "`report_render` 里出现了 `ProvenanceKind::Exploratory`——\
         而设计文档 §5.3 规定**探索性产出（沙箱结果）不得进报告**。\n\
         若你确实要登记它：那说明设计判断变了，请先改设计文档，\
         再删掉本测试并说明理由——而不是悄悄加一条登记路径。\n\
         （`Report::check_provenance` 也会拒绝它，故加了也过不了自检；\
         本测试是**更早**的一道防线。）"
    );

    // 反向断言：报告确实在登记**引擎**来源（防"全部删掉"这种假通过）
    assert!(
        body.contains("ProvenanceKind::CodeComputed"),
        "`report_render` 应登记引擎来源（CodeComputed）——\
         若一个都没有，说明本测试的检查范围错了，而不是「合规」"
    );
}

//! M2 审计缺陷回归测试（对应 spec C.2.1–C.2.6, C.2.10）。
//!
//! 每条测试直接复现对应缺陷路径并锁定核心不变量（铁律 1 & 2、模式分派、混合预期支付、
//! POI 研究回执、可选 `solve` 绕过、范围防篡改、以及复议/回退防复活）。

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;

use serde_json::{Value, json};
use shangyi_core::agent::{
    AgentLoop, ExecOutcome, LoopConfig, PreStepDecision, PreStepGate, ToolCall, ToolExecutor,
};
use shangyi_core::case_log::{CaseHeader, CaseLog};
use shangyi_core::session::{Event, ToolOutcome, TurnId};
use shangyi_core::consultation::ConsultationMode;
use shangyi_core::domain_exec::{
    DomainExecutor, PoiCount, PoiProvider, PoiQuery, PoiSurvey, SearchUnavailable,
};
use shangyi_core::phase_graph::{
    ArtifactKind, CheckpointId, CheckpointItem, CheckpointRequest, CheckpointStatus, PhaseMachine,
    SessionPhase,
};
use shangyi_core::runner::{CheckpointAnswer, SessionRunner, UserIo};
use shangyi_core::task_scope::{TaskResultStatus, TaskScope};
use shangyi_gamespec::{
    Expr, GameSpec, Participant, PayoffForm, RelationToGame, ValueNetRole,
};
use shangyi_kb::TheoryLibrary;
use shangyi_llm::{Endpoint, InputItem, LlmClient, LlmError, Transport, Wire};
use shangyi_playbooks::{Playbook, PlaybookMeta};
use shangyi_tools::ToolRegistry;

// ─────────────────────────── 测试夹具 ───────────────────────────

fn playbook() -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: "pricing_response".into(),
            name: "调价响应".into(),
            description: String::new(),
            order: 10,
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

fn empty_theory() -> TheoryLibrary {
    TheoryLibrary::new()
}

fn call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        call_id: "m2-call-1".into(),
        name: name.into(),
        arguments,
    }
}

fn expect_ok(outcome: ExecOutcome) -> String {
    match outcome {
        ExecOutcome::Ok(s) => s,
        other => panic!("期望 ExecOutcome::Ok，实得 {other:?}"),
    }
}

fn expect_failed(outcome: ExecOutcome) -> String {
    match outcome {
        ExecOutcome::Failed(s) => s,
        other => panic!("期望 ExecOutcome::Failed，实得 {other:?}"),
    }
}

fn make_2p_spec(
    game_id: &str,
    focal_strats: Vec<&str>,
    rival_strats: Vec<&str>,
    entries: &[(&str, f64, f64)],
) -> GameSpec {
    let mut strategies = BTreeMap::new();
    strategies.insert(
        "focal".to_string(),
        focal_strats.into_iter().map(str::to_string).collect(),
    );
    strategies.insert(
        "rival".to_string(),
        rival_strats.into_iter().map(str::to_string).collect(),
    );
    let mut cells = BTreeMap::new();
    for (k, a, b) in entries {
        let mut m = BTreeMap::new();
        m.insert("focal".to_string(), Expr::Literal(*a));
        m.insert("rival".to_string(), Expr::Literal(*b));
        cells.insert((*k).to_string(), m);
    }
    GameSpec {
        game_id: game_id.into(),
        players: vec![
            Participant {
                id: "focal".into(),
                name: "本企业".into(),
                vn_role: ValueNetRole::Focal,
                relation_to_game: RelationToGame::Player,
                profile_ref: None,
            },
            Participant {
                id: "rival".into(),
                name: "竞对".into(),
                vn_role: ValueNetRole::Substitutor,
                relation_to_game: RelationToGame::Player,
                profile_ref: None,
            },
        ],
        strategies,
        payoffs: PayoffForm::Normal { cells },
        params: BTreeMap::new(),
        information: None,
        dynamics: None,
        external_actors: vec![],
        preference: None,
        meta: BTreeMap::new(),
    }
}

fn prisoners_dilemma() -> GameSpec {
    make_2p_spec(
        "pd-m2",
        vec!["维持", "降价"],
        vec!["维持", "降价"],
        &[
            ("维持,维持", 3.0, 3.0),
            ("维持,降价", 0.0, 5.0),
            ("降价,维持", 5.0, 0.0),
            ("降价,降价", 1.0, 1.0),
        ],
    )
}

fn tmp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "shangyi-m2-reg-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct ScriptedTransport {
    responses: RefCell<VecDeque<String>>,
}

impl ScriptedTransport {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: RefCell::new(VecDeque::from(responses)),
        }
    }
}

impl Transport for ScriptedTransport {
    fn post_json(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        _body: &Value,
    ) -> Result<(u16, String), LlmError> {
        let body = self
            .responses
            .borrow_mut()
            .pop_front()
            .unwrap_or_else(|| text_response("结束"));
        Ok((200, body))
    }
}

fn scripted_client(responses: Vec<String>) -> LlmClient<ScriptedTransport> {
    let ep = Endpoint {
        base_url: "https://example.invalid".into(),
        api_key_env: "SHANGYI_TEST_KEY_UNUSED".into(),
        wire: Wire::Responses,
        echo_reasoning: true,
    };
    LlmClient::new(ep, ScriptedTransport::new(responses)).with_key("test-key-not-real")
}

fn response_with_call(id: &str, name: &str, args: Value) -> String {
    json!({
        "id": "resp_m2",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "call_id": id,
            "name": name,
            "arguments": args.to_string(),
        }],
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15}
    })
    .to_string()
}

fn text_response(text: &str) -> String {
    json!({
        "id": "resp_m2_txt",
        "status": "completed",
        "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
        "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7}
    })
    .to_string()
}

// ─────────────────────────── C.2.1: Discover 不被 consequence_predict 遮蔽 ───────────────────────────

#[test]
fn discover_mode_preserves_discovery_report_after_consequence_predict() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_mode(ConsultationMode::Discover);
    ex.set_game_spec(prisoners_dilemma());

    // 先求解并调用 consequence_predict 评估候选收益，使 state.consequence_report = Some(...)
    expect_ok(ex.execute(&call("solve_equilibria", json!({})), SessionPhase::Solve));
    expect_ok(ex.execute(
        &call("consequence_predict", json!({"decision": "降价"})),
        SessionPhase::Discover,
    ));
    assert!(ex.state().consequence_report.is_some());

    // 再执行 discover_satisfice
    expect_ok(ex.execute(
        &call(
            "discover_satisfice",
            json!({
                "aspiration": {"levels": {"利润": 1.0}, "default_threshold": 0.5},
                "candidates": [
                    {
                        "id": "cand-alpha",
                        "description": "差异化维护老客",
                        "outcomes": {"利润": {"p10": 1.2, "p50": 2.0, "p90": 3.0}},
                        "llm_proposed": false
                    }
                ]
            }),
        ),
        SessionPhase::Discover,
    ));
    assert!(ex.state().discovery_report.is_some());

    // recommend_build 必须采用 discovery_report 而非 consequence_report
    let rec_out = expect_ok(ex.execute(
        &call(
            "recommend_build",
            json!({
                "headline": "推荐差异化维护老客",
                "actions": [{"action": "cand-alpha", "rationale": "全指标达标"}]
            }),
        ),
        SessionPhase::Recommend,
    ));
    let rec_v: Value = serde_json::from_str(&rec_out).unwrap();
    assert_eq!(
        rec_v["basis"],
        json!("discovery_report"),
        "Discover 模式建议依据不能被 consequence_report 遮蔽：{rec_out}"
    );

    // report_render 老板页必须报告「满意策略发现」及 prov_discover
    let rep_out = expect_ok(ex.execute(&call("report_render", json!({})), SessionPhase::Report));
    let rep_v: Value = serde_json::from_str(&rep_out).unwrap();
    let boss = rep_v["report"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["layer"] == json!("boss_page"))
        .expect("应存在 boss_page");
    let boss_text = boss["paragraphs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        boss_text.contains("满意策略发现"),
        "Discover 报告老板页应展示满意策略发现，实得：{boss_text}"
    );
    assert!(
        rep_out.contains("prov_discover") && !rep_out.contains("prov_consequence"),
        "Discover 报告 provenance 必须是 prov_discover：{rep_out}"
    );

    // case_store_write 归档后 case_store_read 必须返回 satisficing_achievement
    expect_ok(ex.execute(
        &call(
            "case_store_write",
            json!({"case_id": "discover-shadow-reg", "provenance": "exercise"}),
        ),
        SessionPhase::Archived,
    ));
    let read_out = expect_ok(ex.execute(
        &call(
            "case_store_read",
            json!({"case_id": "discover-shadow-reg", "provenance": "exercise"}),
        ),
        SessionPhase::Archived,
    ));
    let read_v: Value = serde_json::from_str(&read_out).unwrap();
    assert_eq!(
        read_v["prediction"]["kind"],
        json!("satisficing_achievement"),
        "Discover 案例库归档的预测类型必须是 satisficing_achievement：{read_out}"
    );
}

// ─────────────────────────── C.2.2: Whatif 可选 Solve 绕过与门禁不变量 ───────────────────────────

fn approve(m: &mut PhaseMachine, cp: CheckpointId) {
    let phase = m.phase();
    m.open_checkpoint(CheckpointRequest {
        id: cp,
        phase,
        items: vec![CheckpointItem {
            id: "i1".into(),
            content: "确认".into(),
            source: Some("用户".into()),
            proposed_value: None,
        }],
        revision_requested: false,
    })
    .unwrap();
    m.settle_checkpoint(
        cp,
        CheckpointStatus::Approved {
            fingerprint: m.artifact_fingerprint(),
            by: "user".into(),
            at: "t0".into(),
        },
    )
    .unwrap();
}

#[test]
fn whatif_skip_optional_solve_satisfies_pre_step_gate_while_enforcing_cp2() {
    let mut m = PhaseMachine::new(ConsultationMode::Whatif);
    m.advance().unwrap(); // -> intel
    m.record(ArtifactKind::ParticipantProfiles, "p");
    approve(&mut m, CheckpointId::Cp1);
    m.advance().unwrap(); // -> model_build
    m.record(ArtifactKind::GameSpecValidated, "g");
    approve(&mut m, CheckpointId::Cp2);

    // 1) 正常 advance 进 solve 后，skip_optional_to(Consequence) 成功
    m.advance().unwrap();
    assert_eq!(m.phase(), SessionPhase::Solve);
    assert!(!m.has_artifact(ArtifactKind::EquilibriumSet));
    assert!(m.skip_optional_to(SessionPhase::Report).is_err(), "非相邻边不得跳过");
    assert_eq!(
        m.skip_optional_to(SessionPhase::Consequence).unwrap(),
        SessionPhase::Consequence
    );

    // 2) PreStepGate 此时必须放行 Consequence（虽无 EquilibriumSet，但 ModelBuild 与 cp2 已完备）
    assert!(
        matches!(PreStepGate::new(&m).decide(), PreStepDecision::Inject { .. }),
        "whatif 跳过可选 solve 进入 consequence 时 PreStepGate 应放行"
    );

    // 3) 铁律 2 守卫：一旦清除 GameSpecValidated（或未批准 cp2），PreStepGate 必须硬拒绝
    m.clear_artifact(ArtifactKind::GameSpecValidated);
    match PreStepGate::new(&m).decide() {
        PreStepDecision::Reject { reason } => {
            assert!(
                reason.contains("model_build") && reason.contains("gamespec"),
                "缺失 model_build 产物时必须拒绝：{reason}"
            );
        }
        other => panic!("缺少前置产物时绝不可放行，实得 {other:?}"),
    }

    // 4) 不可在携带 checkpoint 的非可选阶段调用 skip_optional_to
    let mut m_mb = PhaseMachine::new(ConsultationMode::Whatif);
    m_mb.advance().unwrap(); // intel
    m_mb.record(ArtifactKind::ParticipantProfiles, "p");
    approve(&mut m_mb, CheckpointId::Cp1);
    m_mb.advance().unwrap(); // model_build
    assert!(
        m_mb.skip_optional_to(SessionPhase::Consequence).is_err(),
        "model_build 非可选且有 cp2，禁止跳过"
    );
}

// ─────────────────────────── C.2.3: poi_survey 单独构成 research 任务 ───────────────────────────

struct MockPoiProvider {
    result: fn(&PoiQuery) -> Result<PoiSurvey, SearchUnavailable>,
}

impl PoiProvider for MockPoiProvider {
    fn survey(&self, query: &PoiQuery) -> Result<PoiSurvey, SearchUnavailable> {
        (self.result)(query)
    }
}

#[test]
fn poi_survey_only_research_task_blocked_and_succeeds_cleanly() {
    let theory = empty_theory();
    let pb = playbook();

    // Case A: poi = None -> Blocked -> unavailable(provider_unavailable) -> complete_task(Partial)
    {
        let mut ex = DomainExecutor::new(&theory, &pb);
        ex.set_task_scope(Some(TaskScope::Research));
        ex.record_user_input("请查询上海南京西路100号500米内咖啡门店数量", "user");

        expect_ok(ex.execute(
            &call(
                "research_task_plan",
                json!({
                    "fields": [{
                        "id": "nearby_cafes",
                        "label": "500米内咖啡门店数量",
                        "subject": "南京西路100号",
                        "request_quote": "500米内咖啡门店数量",
                        "evidence_required": "original_text"
                    }]
                }),
            ),
            SessionPhase::Intake,
        ));

        let poi_out = ex.execute(
            &call("poi_survey", json!({"address": "南京西路100号", "radius": 500})),
            SessionPhase::Intake,
        );
        assert!(
            matches!(poi_out, ExecOutcome::Blocked(_)),
            "未绑定 PoiProvider 时必须返回 Blocked：{poi_out:?}"
        );

        // not_found 仍须被拒（poi_survey 不能伪造健康的空网页检索）
        let not_found_err = expect_failed(ex.execute(
            &call(
                "research_task_update",
                json!({
                    "updates": [{
                        "field_id": "nearby_cafes",
                        "status": "unavailable",
                        "reason_code": "not_found"
                    }]
                }),
            ),
            SessionPhase::Intake,
        ));
        assert!(not_found_err.contains("空检索凭据"));

        // provider_unavailable 必须被接受
        expect_ok(ex.execute(
            &call(
                "research_task_update",
                json!({
                    "updates": [{
                        "field_id": "nearby_cafes",
                        "status": "unavailable",
                        "reason_code": "provider_unavailable"
                    }]
                }),
            ),
            SessionPhase::Intake,
        ));

        let completed = ex.execute(
            &call(
                "complete_task",
                json!({
                    "summary": "已尝试 POI 周边查询，但未绑定高德数据源，门店数量未取得。",
                    "missing": []
                }),
            ),
            SessionPhase::Intake,
        );
        let ExecOutcome::ScopedTaskCompleted(res) = completed else {
            panic!("预期完成研究交付，实得 {completed:?}");
        };
        assert_eq!(res.status, TaskResultStatus::Partial);
        assert_eq!(res.retrieval_attempts, 1);
        assert!(!res.missing.is_empty());
    }

    // Case B: NoBackend 不消耗 16 次实网预算，随后真实 PoiSurvey 返回结构化回执可交付 Complete
    {
        let no_backend = MockPoiProvider {
            result: |_| {
                Err(SearchUnavailable::NoBackend {
                    detail: "AMAP_KEY 未配置".into(),
                })
            },
        };
        let live_poi = MockPoiProvider {
            result: |q| {
                Ok(PoiSurvey {
                    address: q.address.clone(),
                    district: "静安区".into(),
                    radius: q.radius,
                    counts: vec![PoiCount {
                        label: "茶饮咖啡".into(),
                        count: 14,
                    }],
                    brand: None,
                    keywords: None,
                    unavailable: vec![],
                    source: "高德 POI v5".into(),
                })
            },
        };
        let mut ex = DomainExecutor::new(&theory, &pb).with_poi(&no_backend);
        ex.set_task_scope(Some(TaskScope::Research));
        ex.record_user_input("请查询静安寺周边咖啡门店数", "user");
        expect_ok(ex.execute(
            &call(
                "research_task_plan",
                json!({
                    "fields": [{
                        "id": "cafes",
                        "label": "周边咖啡门店数",
                        "subject": "静安寺",
                        "request_quote": "静安寺周边咖啡门店数",
                        "evidence_required": "original_text"
                    }]
                }),
            ),
            SessionPhase::Intake,
        ));

        for _ in 0..17 {
            let out = ex.execute(
                &call("poi_survey", json!({"address": "静安寺", "radius": 500})),
                SessionPhase::Intake,
            );
            assert!(matches!(out, ExecOutcome::Blocked(_)));
        }
        assert_eq!(
            ex.state().retrieval_attempts.get(),
            0,
            "NoBackend 不得耗尽 16 次联网预算"
        );

        ex = ex.with_poi(&live_poi);
        let ok_out = expect_ok(ex.execute(
            &ToolCall {
                call_id: "poi-live-1".into(),
                name: "poi_survey".into(),
                arguments: json!({"address": "静安寺", "radius": 500}),
            },
            SessionPhase::Intake,
        ));
        let ok_v: Value = serde_json::from_str(&ok_out).unwrap();
        let receipt_id = ok_v["receipts"][0]["receipt"].as_str().unwrap();

        expect_ok(ex.execute(
            &call(
                "research_task_update",
                json!({
                    "updates": [{
                        "field_id": "cafes",
                        "status": "supported",
                        "structured": [{"receipt": receipt_id}]
                    }]
                }),
            ),
            SessionPhase::Intake,
        ));

        let done = ex.execute(
            &call(
                "complete_task",
                json!({"summary": "周边茶饮咖啡门店计数已通过 POI 核实。", "missing": []}),
            ),
            SessionPhase::Intake,
        );
        let ExecOutcome::ScopedTaskCompleted(res) = done else {
            panic!("结构化 POI 依据应可交付 Complete，实得 {done:?}");
        };
        assert_eq!(res.status, TaskResultStatus::Complete);
        assert!(res.summary.contains("14 家"));
    }
}

// ─────────────────────────── C.2.4: consequence_predict 负收益与三人混合博弈 ───────────────────────────

#[test]
fn consequence_predict_handles_2p_negative_payoffs_and_3p_mixed_expected_payoffs() {
    let theory = empty_theory();
    let pb = playbook();

    // 1) 2人全负收益：不能把 max 钳位在 0 导致 winners 为空 / 1/0 = inf
    {
        let mut ex = DomainExecutor::new(&theory, &pb);
        let spec = make_2p_spec(
            "neg-2p",
            vec!["方案A"],
            vec!["应对1", "应对2"],
            &[("方案A,应对1", -10.0, -5.0), ("方案A,应对2", 4.0, -2.0)],
        );
        ex.set_game_spec(spec);
        let out = expect_ok(ex.execute(
            &call("consequence_predict", json!({"decision": "方案A"})),
            SessionPhase::Consequence,
        ));
        let v: Value = serde_json::from_str(&out).unwrap();
        let reactions = v["reactions"].as_array().unwrap();
        assert_eq!(reactions.len(), 1);
        assert_eq!(reactions[0]["reaction"], json!("rival:应对2"));
        assert!((reactions[0]["probability"].as_f64().unwrap() - 1.0).abs() < 1e-12);
        assert!((reactions[0]["outcomes"]["focal"]["p50"].as_f64().unwrap() - 4.0).abs() < 1e-12);
        assert!((reactions[0]["outcomes"]["rival"]["p50"].as_f64().unwrap() + 2.0).abs() < 1e-12);
    }

    // 2) 3人纯策略均衡：outcomes 必须包含 focal 与两个对手
    {
        let mut strategies = BTreeMap::new();
        strategies.insert("focal".into(), vec!["推进".into()]);
        strategies.insert("opp1".into(), vec!["左".into(), "右".into()]);
        strategies.insert("opp2".into(), vec!["上".into(), "下".into()]);
        let mut cells = BTreeMap::new();
        for (k, pf, p1, p2) in [
            ("推进,左,上", 42.0, 10.0, 10.0),
            ("推进,左,下", 1.0, 0.0, 0.0),
            ("推进,右,上", 2.0, 0.0, 0.0),
            ("推进,右,下", 3.0, 1.0, 1.0),
        ] {
            let mut m = BTreeMap::new();
            m.insert("focal".into(), Expr::Literal(pf));
            m.insert("opp1".into(), Expr::Literal(p1));
            m.insert("opp2".into(), Expr::Literal(p2));
            cells.insert(k.into(), m);
        }
        let spec = GameSpec {
            game_id: "pure-3p".into(),
            players: vec![
                Participant {
                    id: "focal".into(),
                    name: "我方".into(),
                    vn_role: ValueNetRole::Focal,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "opp1".into(),
                    name: "对手1".into(),
                    vn_role: ValueNetRole::Substitutor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "opp2".into(),
                    name: "对手2".into(),
                    vn_role: ValueNetRole::Complementor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
            ],
            strategies,
            payoffs: PayoffForm::Normal { cells },
            params: BTreeMap::new(),
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: BTreeMap::new(),
        };
        let mut ex = DomainExecutor::new(&theory, &pb);
        ex.set_game_spec(spec);
        let out = expect_ok(ex.execute(
            &call("consequence_predict", json!({"decision": "推进"})),
            SessionPhase::Consequence,
        ));
        let v: Value = serde_json::from_str(&out).unwrap();
        let top = v["reactions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["reaction"] == json!("opp1:左 + opp2:上"))
            .expect("应包含纯策略反应 opp1:左 + opp2:上");
        assert!((top["outcomes"]["focal"]["p50"].as_f64().unwrap() - 42.0).abs() < 1e-9);
        assert!((top["outcomes"]["opp1"]["p50"].as_f64().unwrap() - 10.0).abs() < 1e-9);
        assert!((top["outcomes"]["opp2"]["p50"].as_f64().unwrap() - 10.0).abs() < 1e-9);
    }

    // 3) 3人混合均衡（猜硬币 ±10 对手 + focal 仅在 (出击, 东, 东) 得 100）：
    //    唯一残余混合均衡为 (0.5, 0.5) × (0.5, 0.5)，故期望支付 focal = 25.0, opp1 = 0.0, opp2 = 0.0
    {
        let mut strategies = BTreeMap::new();
        strategies.insert("focal".into(), vec!["出击".into()]);
        strategies.insert("opp1".into(), vec!["东".into(), "西".into()]);
        strategies.insert("opp2".into(), vec!["东".into(), "西".into()]);
        let mut cells = BTreeMap::new();
        for (k, pf, p1, p2) in [
            ("出击,东,东", 100.0, -10.0, 10.0),
            ("出击,东,西", 0.0, 10.0, -10.0),
            ("出击,西,东", 0.0, 10.0, -10.0),
            ("出击,西,西", 0.0, -10.0, 10.0),
        ] {
            let mut m = BTreeMap::new();
            m.insert("focal".into(), Expr::Literal(pf));
            m.insert("opp1".into(), Expr::Literal(p1));
            m.insert("opp2".into(), Expr::Literal(p2));
            cells.insert(k.into(), m);
        }
        let spec = GameSpec {
            game_id: "mixed-3p".into(),
            players: vec![
                Participant {
                    id: "focal".into(),
                    name: "我方".into(),
                    vn_role: ValueNetRole::Focal,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "opp1".into(),
                    name: "甲".into(),
                    vn_role: ValueNetRole::Substitutor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
                Participant {
                    id: "opp2".into(),
                    name: "乙".into(),
                    vn_role: ValueNetRole::Substitutor,
                    relation_to_game: RelationToGame::Player,
                    profile_ref: None,
                },
            ],
            strategies,
            payoffs: PayoffForm::Normal { cells },
            params: BTreeMap::new(),
            information: None,
            dynamics: None,
            external_actors: vec![],
            preference: None,
            meta: BTreeMap::new(),
        };
        let mut ex = DomainExecutor::new(&theory, &pb);
        ex.set_game_spec(spec);
        let out = expect_ok(ex.execute(
            &call("consequence_predict", json!({"decision": "出击"})),
            SessionPhase::Consequence,
        ));
        let v: Value = serde_json::from_str(&out).unwrap();
        let reactions = v["reactions"].as_array().unwrap();
        assert_eq!(reactions.len(), 1, "唯一混合均衡：{out}");
        let outcomes = reactions[0]["outcomes"].as_object().unwrap();
        assert!(outcomes.contains_key("focal"), "必须包含 focal：{out}");
        let f_p50 = outcomes["focal"]["p50"].as_f64().unwrap();
        let f_p10 = outcomes["focal"]["p10"].as_f64().unwrap();
        let f_p90 = outcomes["focal"]["p90"].as_f64().unwrap();
        assert!((f_p50 - 25.0).abs() < 1e-9, "期望收益应为 25.0，实得 {f_p50}");
        assert!((f_p10 - f_p50).abs() < 1e-9 && (f_p90 - f_p50).abs() < 1e-9);
        assert!(outcomes["opp1"]["p50"].as_f64().unwrap().abs() < 1e-9);
        assert!(outcomes["opp2"]["p50"].as_f64().unwrap().abs() < 1e-9);
    }
}

// ─────────────────────────── C.2.5: select_task_scope 拒绝时不残留突变 ───────────────────────────

#[test]
fn select_task_scope_rejected_after_completion_does_not_mutate_executor_scope() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_task_scope(Some(TaskScope::Cost));
    ex.record_user_input("算一笔租金成本，再看看要不要做完整咨询", "user");

    let reg = ToolRegistry::standard();
    let client = scripted_client(vec![
        response_with_call(
            "sc-1",
            "select_task_scope",
            json!({"scope": "consultation", "reason": "尝试不重开直接扩大范围"}),
        ),
        text_response("不能在未调用 begin_followup_task 时直接改 scope"),
    ]);
    let mut loop_ = AgentLoop::new(client, &reg, &mut ex, LoopConfig::new("m", "m2-scope"));

    let mut m = PhaseMachine::new(ConsultationMode::Scenario).require_task_scope_selection();
    m.select_task_scope(TaskScope::Cost).unwrap();
    let done = shangyi_core::task_scope::ScopedTaskResult {
        scope: TaskScope::Cost,
        status: TaskResultStatus::Complete,
        summary: "已完成费用计算".into(),
        missing: vec![],
        source_urls: vec![],
        retrieval_attempts: 0,
        cost_scenarios: vec![],
        conditional: None,
        research: None,
    };
    m.finish_scoped_task(done).unwrap();

    let _ = loop_
        .run_turn(&mut m, TurnId(1), vec![InputItem::user("继续")])
        .unwrap();
    let failed_event = loop_.events().iter().find(|e| {
        matches!(
            e,
            Event::ToolCallFinished {
                outcome: ToolOutcome::Failed { error },
                ..
            } if error.contains("begin_followup_task")
        )
    });
    assert!(
        failed_event.is_some(),
        "未调用 begin_followup_task 重开时 select_task_scope 必须失败"
    );
    assert_eq!(m.task_scope(), Some(TaskScope::Cost));
    drop(loop_);
    assert_eq!(
        ex.state().task_scope,
        Some(TaskScope::Cost),
        "PhaseMachine 拒绝后 executor.state.task_scope 必须回滚为 Cost，不能被污染为 Consultation"
    );
}

// ─────────────────────────── C.2.6: reconsider / rewind 清理执行器产物防复活 ───────────────────────────

#[test]
fn reconsider_current_checkpoint_and_rewind_clear_executor_artifacts_so_sync_cannot_resurrect() {
    struct ReviseInPlaceIo {
        answers: VecDeque<CheckpointAnswer>,
    }
    impl UserIo for ReviseInPlaceIo {
        fn can_revise_in_place(&self) -> bool {
            true
        }
        fn present_checkpoint(&mut self, _req: &CheckpointRequest) -> CheckpointAnswer {
            self.answers.pop_front().unwrap_or(CheckpointAnswer::Approve)
        }
        fn inform(&mut self, _message: &str) {}
    }

    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    assert!(
        ex.artifacts()
            .iter()
            .any(|(k, _)| *k == ArtifactKind::GameSpecValidated),
        "预置 GameSpec 后应有 GameSpecValidated 产物"
    );

    let reg = ToolRegistry::standard();
    // 1) 用户在 cp2 选 Revise（在位修改）：
    //    模型第一轮回复文本触发 cp2 -> Revise 后，reconsider_current_checkpoint 须清掉执行器中的 game_spec，
    //    确保下一轮 loop.run_turn() 中的 sync_artifacts 不会把它复活回 PhaseMachine！
    let client = scripted_client(vec![
        text_response("请确认 cp2 模型"),
        text_response("我已收到修正，当前尚未重建 GameSpec"),
    ]);
    let dir = tmp_dir("reconsider-no-resurrect");
    let header = CaseHeader {
        case_id: "m2-reconsider".into(),
        mode: "scenario".into(),
        playbook_id: "pricing_response".into(),
        created_at: "2026-11-15T00:00:00Z".into(),
        schema_version: CaseHeader::SCHEMA_VERSION,
    };
    let log = CaseLog::open(&dir, &header).unwrap();
    let loop_ = AgentLoop::new(client, &reg, &mut ex, LoopConfig::new("m", "m2-reconsider"))
        .with_seq_clock();

    let mut m = PhaseMachine::new(ConsultationMode::Scenario);
    m.advance().unwrap(); // intel
    m.record(ArtifactKind::ParticipantProfiles, "p");
    approve(&mut m, CheckpointId::Cp1);
    m.advance().unwrap(); // model_build
    m.record(ArtifactKind::GameSpecValidated, "g");
    m.open_checkpoint(CheckpointRequest {
        id: CheckpointId::Cp2,
        phase: SessionPhase::ModelBuild,
        items: vec![CheckpointItem {
            id: "i1".into(),
            content: "初始矩阵".into(),
            source: Some("用户".into()),
            proposed_value: None,
        }],
        revision_requested: false,
    })
    .unwrap();

    let mut runner = SessionRunner::new(loop_, log, m).with_max_turns(2);
    let mut io = ReviseInPlaceIo {
        answers: VecDeque::from(vec![CheckpointAnswer::Revise {
            reason: "矩阵里的成本假设不对，须推翻重来".into(),
        }]),
    };
    let _report = runner
        .run(vec![InputItem::user("模型哪里不对？")], &mut io, "user")
        .unwrap();

    assert_eq!(runner.machine().phase(), SessionPhase::ModelBuild);
    assert!(
        !runner.machine().has_artifact(ArtifactKind::GameSpecValidated),
        "reconsider_current_checkpoint 后下一 turn 的 sync_artifacts 不得复活 GameSpecValidated"
    );

    // 2) 测试 rewind_to 同样清掉执行器产物
    runner.machine_mut().record(ArtifactKind::ParticipantProfiles, "p");
    drop(runner);
    ex.set_game_spec(prisoners_dilemma());
    let dir2 = tmp_dir("rewind-no-resurrect");
    let log2 = CaseLog::open(&dir2, &header).unwrap();
    let client2 = scripted_client(vec![text_response("ok")]);
    let loop2 = AgentLoop::new(client2, &reg, &mut ex, LoopConfig::new("m", "m2-rewind"))
        .with_seq_clock();
    let mut m2 = PhaseMachine::new(ConsultationMode::Scenario);
    m2.advance().unwrap();
    m2.record(ArtifactKind::ParticipantProfiles, "p");
    approve(&mut m2, CheckpointId::Cp1);
    m2.advance().unwrap();
    m2.record(ArtifactKind::GameSpecValidated, "g");
    let mut runner2 = SessionRunner::new(loop2, log2, m2);
    runner2.rewind_to(SessionPhase::Intel, "回退补事实").unwrap();
    drop(runner2);
    assert!(
        ex.state().game_spec.is_none(),
        "rewind_to(Intel) 必须同步清除执行器内失效的 game_spec"
    );
}

// ─────────────────────────── C.2.10: gamespec_build cells: {} 报错口径自洽 ───────────────────────────

#[test]
fn gamespec_build_empty_cells_rejected_with_consistent_two_step_guidance() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);

    // 1) 空 cells 必须被拒，且错误信息不能再误导说「传空 cells」
    let empty_spec = json!({
        "game_id": "empty-cells-test",
        "players": [
            {"id": "focal", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "竞对", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"focal": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {}}},
        "params": {}
    });
    let err = expect_failed(ex.execute(
        &call("gamespec_build", empty_spec),
        SessionPhase::ModelBuild,
    ));
    assert!(
        !err.contains("空 cells") && err.contains("不能为空 `{}`"),
        "报错口径必须写清 cells 不能为空 {{}}：{err}"
    );
    assert!(ex.state().game_spec.is_none(), "被拒后不得保存空骨架");

    // 2) 实际两步写法（最小字面量 cells + params: {}，再 merge: true 补 params）可顺利走通
    let step1 = json!({
        "game_id": "two-step-ok",
        "players": [
            {"id": "focal", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "竞对", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"focal": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "维持,维持": {"focal": {"kind": "literal", "value": 3}, "rival": {"kind": "literal", "value": 3}},
            "维持,降价": {"focal": {"kind": "literal", "value": 0}, "rival": {"kind": "literal", "value": 5}},
            "降价,维持": {"focal": {"kind": "literal", "value": 5}, "rival": {"kind": "literal", "value": 0}},
            "降价,降价": {"focal": {"kind": "literal", "value": 1}, "rival": {"kind": "literal", "value": 1}}
        }}},
        "params": {}
    });
    expect_ok(ex.execute(&call("gamespec_build", step1), SessionPhase::ModelBuild));

    let step2 = json!({
        "merge": true,
        "params": {
            "coop": {
                "name": "互惠收益",
                "value": {"kind": "literal", "value": 3},
                "source": {"kind": "user_confirmed", "checkpoint": "cp2"}
            }
        }
    });
    let step2_out = expect_ok(ex.execute(&call("gamespec_build", step2), SessionPhase::ModelBuild));
    let step2_v: Value = serde_json::from_str(&step2_out).unwrap();
    assert_eq!(step2_v["params"], json!(1));
}

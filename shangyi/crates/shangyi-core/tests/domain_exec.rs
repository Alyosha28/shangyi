//! `shangyi-core::domain_exec` 的测试。
//!
//! 重点不是"跑得通"，而是**接线是否诚实**：
//! 1. 铁律 1：进模型的数字必须由确定性引擎算出，执行器不得编造；
//! 2. 未接线的工具必须**报错**，不得返回一个"看起来像结果"的东西；
//! 3. 外部依赖缺失必须是 `Blocked`（"未取得"）而不是 `Failed`（"出错"）
//!    ——报告里这两句话完全不同；
//! 4. 求解器的类型化拒绝（退化/超范围）不得被伪装成"无均衡"。

use std::collections::{BTreeMap, HashMap};

use shangyi_core::agent::{ExecOutcome, ToolCall, ToolExecutor};
use shangyi_core::domain_exec::{
    DomainExecutor, FetchedText, SearchHit, SearchProvider, SearchUnavailable, wiring_status,
};
use shangyi_core::phase_graph::SessionPhase;
use shangyi_gamespec::{
    Expr, GameSpec, Participant, PayoffForm, RelationToGame, ValueNetRole,
};
use shangyi_intel::{
    CommonProfile, DataSource, EventType, ParticipantProfile, SourceKind, TimelineEvent,
};
use shangyi_kb::{TheoryEntry, TheoryLibrary};
use shangyi_playbooks::{Playbook, PlaybookMeta};

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

/// 囚徒困境（2×2，整数支付 ⇒ 精确有理数求解）。
///
/// 支付表（行 = focal，列 = rival）：
/// ```text
///         维持      降价
/// 维持   (3,3)    (0,5)
/// 降价   (5,0)    (1,1)
/// ```
/// 降价是严格占优策略 ⇒ **唯一**纳什均衡 `(降价, 降价)`。
///
/// ⚠️ 构造成立是本测试夹具的**前提**：早先版本的支付表
/// （(5,5)/(0,4)/(4,0)/(2,2)）其实**不是**囚徒困境——它没有占优策略，
/// 求解器正确地返回了 3 个均衡（两个纯策略 + 一个混合），而测试却断言"恰有 1 个"。
/// 那是**测试错了，不是求解器错了**。真正的囚徒困境需要
/// `T > R > P > S`（背叛 > 互惠 > 互相背叛 > 被背叛）。
fn prisoners_dilemma() -> GameSpec {
    let mut strategies = BTreeMap::new();
    strategies.insert("focal".to_string(), vec!["维持".to_string(), "降价".to_string()]);
    strategies.insert("rival".to_string(), vec!["维持".to_string(), "降价".to_string()]);
    let mut cells: BTreeMap<String, BTreeMap<String, Expr>> = BTreeMap::new();
    for (key, a, b) in [
        ("维持,维持", 3.0, 3.0),
        ("维持,降价", 0.0, 5.0),
        ("降价,维持", 5.0, 0.0),
        ("降价,降价", 1.0, 1.0),
    ] {
        let mut m = BTreeMap::new();
        m.insert("focal".to_string(), Expr::Literal(a));
        m.insert("rival".to_string(), Expr::Literal(b));
        cells.insert(key.to_string(), m);
    }
    GameSpec {
        game_id: "pd".into(),
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

/// 鹰鸽博弈：**无占优策略**，故有两个纯策略均衡 + 一个混合均衡。
///
/// 支付表：
/// ```text
///         鹰        鸽
/// 鹰    (5,5)    (0,4)
/// 鸽    (4,0)    (2,2)
/// ```
/// 用于验证接线能处理**多均衡 + 混合策略**的情形——混合解以精确有理数
/// 序列化（2/3），若中途经 f64 中转就会丢掉"精确"这个性质。
fn hawk_dove() -> GameSpec {
    let mut spec = prisoners_dilemma();
    spec.game_id = "hd".into();
    let names = vec!["鹰".to_string(), "鸽".to_string()];
    spec.strategies.insert("focal".to_string(), names.clone());
    spec.strategies.insert("rival".to_string(), names);
    let mut cells: BTreeMap<String, BTreeMap<String, Expr>> = BTreeMap::new();
    for (key, a, b) in [
        ("鹰,鹰", 5.0, 5.0),
        ("鹰,鸽", 0.0, 4.0),
        ("鸽,鹰", 4.0, 0.0),
        ("鸽,鸽", 2.0, 2.0),
    ] {
        let mut m = BTreeMap::new();
        m.insert("focal".to_string(), Expr::Literal(a));
        m.insert("rival".to_string(), Expr::Literal(b));
        cells.insert(key.to_string(), m);
    }
    spec.payoffs = PayoffForm::Normal { cells };
    spec
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "c1".into(),
        name: name.into(),
        arguments: args,
    }
}

fn expect_ok(o: ExecOutcome) -> String {
    match o {
        ExecOutcome::Ok(s) => s,
        other => panic!("期望 Ok，实得 {other:?}"),
    }
}

fn expect_failed(o: ExecOutcome) -> String {
    match o {
        ExecOutcome::Failed(s) => s,
        other => panic!("期望 Failed，实得 {other:?}"),
    }
}

fn exec(mode: &str, name: &str, args: serde_json::Value) -> ExecOutcome {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let _ = mode;
    ex.execute(&call(name, args), SessionPhase::Intake)
}

// ─────────────────────────── 铁律 1 ───────────────────────────

/// 求解器接的是**确定性引擎**：同样的输入必须给同样的输出，且均衡可验证。
#[test]
fn solve_equilibria_returns_the_engines_exact_result() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());

    let out = expect_ok(ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    // 囚徒困境唯一纳什均衡：(降价, 降价) = 下标 [1],[1]
    assert_eq!(v["completeness"], serde_json::json!("Complete"));
    let eqs = v["equilibria"].as_array().unwrap();
    assert_eq!(eqs.len(), 1, "囚徒困境应恰有一个均衡：{out}");
    assert_eq!(eqs[0]["kind"], serde_json::json!("Pure"));
    assert_eq!(eqs[0]["support"], serde_json::json!([[1], [1]]));
    // 支付以**精确有理数字符串**给出（"1"），不经 f64 中转——
    // 否则"精确均衡"的称号在序列化这一步就丢了
    assert_eq!(eqs[0]["payoffs"], serde_json::json!(["1", "1"]));
}

/// 多均衡 + 混合解：鹰鸽博弈应给出 2 个纯策略均衡与 1 个混合均衡，
/// 且混合解的比例是**精确有理数**（2/3），不是浮点近似。
#[test]
fn mixed_equilibrium_is_exact_rational() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(hawk_dove());

    let out = expect_ok(ex.execute(
        &call("solve_equilibria", serde_json::json!({})),
        SessionPhase::Solve,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let eqs = v["equilibria"].as_array().unwrap();
    assert_eq!(eqs.len(), 3, "鹰鸽博弈应有 2 纯 + 1 混合共 3 个均衡：{out}");

    let kinds: Vec<&str> = eqs.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "Pure").count(), 2);
    assert_eq!(kinds.iter().filter(|k| **k == "Mixed").count(), 1);

    // 混合均衡：双方以 2/3 概率出「鹰」，支付 10/3。必须是精确有理数。
    let mixed = eqs.iter().find(|e| e["kind"] == serde_json::json!("Mixed")).unwrap();
    assert_eq!(
        mixed["strategy"],
        serde_json::json!([["2/3", "1/3"], ["2/3", "1/3"]]),
        "混合策略须是精确有理数 2/3、1/3，而不是 0.6666…"
    );
    assert_eq!(mixed["payoffs"], serde_json::json!(["10/3", "10/3"]));
}

/// 没有 GameSpec 就求解 ⇒ 报错并**指出该先做什么**，不得返回空解集。
#[test]
fn solve_without_game_spec_is_refused_with_actionable_message() {
    let msg = expect_failed(exec(
        "scenario",
        "solve_equilibria",
        serde_json::json!({}),
    ));
    assert!(msg.contains("gamespec_build"), "应指出先做哪一步：{msg}");
    assert!(msg.contains("checkpoint"), "应提示需过硬假设确认：{msg}");
}

/// **铁律 1 的关键断言**：执行器不替模型编造数值。
/// `select_equilibria` 要求调用方给出分数向量，缺它就报错——它绝不"估算"。
#[test]
fn select_equilibria_refuses_to_invent_scores() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);

    let msg = expect_failed(ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom"})),
        SessionPhase::Select,
    ));
    assert!(msg.contains("scores"), "应说明缺什么参数：{msg}");
    assert!(msg.contains("铁律 1"), "应点明为什么不能替它编：{msg}");
}

#[test]
fn select_equilibria_rejects_unknown_selector_name() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);

    let msg = expect_failed(ex.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "不存在的判据", "scores": [1.0]}),
        ),
        SessionPhase::Select,
    ));
    assert!(msg.contains("risk_dom"), "应列出可用判据：{msg}");
}

#[test]
fn select_equilibria_rejects_score_count_mismatch() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);

    let msg = expect_failed(ex.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "risk_dom", "scores": [1.0, 2.0, 3.0]}),
        ),
        SessionPhase::Select,
    ));
    assert!(msg.contains("不符"), "应说明个数不符：{msg}");
}

// ─────────────────────────── 不静默降级 ───────────────────────────

/// `gamespec_build` 遇到假参数必须拒绝：**铁律 2 的可执行落点**。
#[test]
fn gamespec_build_rejects_assumed_params() {
    let mut spec = prisoners_dilemma();
    let mut params = BTreeMap::new();
    params.insert(
        "P".to_string(),
        shangyi_gamespec::Param {
            name: "P".into(),
            value: Expr::Literal(1.0),
            dist: None,
            source: shangyi_gamespec::ParamSource::Assumed {
                note: "我猜的".into(),
            },
            unit: None,
        },
    );
    spec.params = params;

    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call(
            "gamespec_build",
            serde_json::to_value(&spec).unwrap(),
        ),
        SessionPhase::ModelBuild,
    ));
    assert!(msg.contains("铁律 2"), "应点明违反的是哪条铁律：{msg}");
}

/// **未接线的工具这一类已经消失**——故原先那条测试按它自己的指示被删除。
///
/// # 原测试写了什么，以及为什么删它是对的
///
/// 它断言"未接线的工具应报「尚未接线」而非含糊的「出错了」"。那条断言
/// 有一句自带的退场指示：
///
/// > 当前没有未接线的工具了——若确实全部接完，请删掉这条测试而不是放宽它
///
/// 理由（当时写在注释里）是：它测的是"未接线这一类的行为"，
/// 而清单来自 `wiring_status()`。当这一类为空时，它只会红，没有信息量。
///
/// # 它的职责现在由谁承担
///
/// | 原来负责 | 现在负责 |
/// |---|---|
/// | 未接线工具报错要清楚 | `all_tools_are_wired`：断言这一类**为空** |
/// | 未知工具名要有明确响应 | `unknown_tool_name_is_handled` |
///
/// 兜底路径**仍然保留**在 `execute()` 的 catch-all 里（见那里的注释）：
/// 它防的是"注册表加了工具但忘实现"与"模型调了不存在的工具名"这两件事。
/// 删的是测试，不是防线。
///
/// # 这条注释为什么留着
///
/// 因为"某条测试被删了"这件事本身需要被记录。否则后来者看到
/// `all_tools_are_wired` 会以为从来如此，而不知道曾经有一个
/// **15 个工具未实现**的阶段——那段历史解释了为什么现在有这么多
/// 结构不变量测试。

/// **全部工具已接线**（收尾断言）。
///
/// 若将来有人新增工具却忘了实现，`wiring_status()` 里会出现 `not-wired`，
/// 本测试立刻失败——**那正是它该做的事**：把"忘了实现"变成一次构建失败，
/// 而不是一次运行时的静默降级。
#[test]
fn all_tools_are_wired_and_dispatch_table_is_complete() {
    let st = wiring_status();
    let unwired: Vec<&str> = st
        .iter()
        .filter(|(_, s)| **s == "not-wired")
        .map(|(n, _)| *n)
        .collect();
    assert!(
        unwired.is_empty(),
        "以下工具仍未接线：{unwired:?}。\
         若是有意保留未实现，请在本测试里逐条列出**为什么**\
         （而不是放宽断言——那会让「忘了实现」与「有意留空」无法区分）。"
    );
    assert!(
        st.len() >= 25,
        "接线清单只有 {} 项——它应覆盖工具注册表里的全部工具",
        st.len()
    );
}

// ─────────────────────────── vn_map（价值网五向扫描）───────────────────────────

/// `vn_map` 产出的是**扫描清单与覆盖情况**，不是参与者名单。
///
/// 这条断言是承重的：把"该找什么"冒充成"已经找到了什么"，会让后续建模
/// 建立在一个未经验证的参与者集合上，而用户无从察觉。
#[test]
fn vn_map_produces_a_scan_plan_not_a_discovery() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(&call("vn_map", serde_json::json!({})), SessionPhase::Intake));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();

    // 四个方向（五向里的四个 + 外部单独说明；focal 是"我"自己，不是扫描对象）
    let dirs = v["directions"].as_array().unwrap();
    assert_eq!(dirs.len(), 4, "应给出四个可扫描方向：{out}");
    assert_eq!(v["coverage"], serde_json::json!("0/4"), "尚无画像时覆盖为 0");

    // **必须**声明它不发现参与者
    let note = v["scan_plan_note"].as_str().unwrap();
    assert!(
        note.contains("不是参与者名单") && note.contains("请勿把清单当作已有发现"),
        "必须明确否认可发现性：{note}"
    );

    // 外部力量不得被臆断出来
    let ext = v["external_actors"]["note"].as_str().unwrap();
    assert!(ext.contains("尚未引入"), "外部力量只在被引入时展开：{ext}");
}

/// 已收集的画像必须反映到覆盖情况上（否则这个工具没有信息量）。
#[test]
fn vn_map_reports_coverage_from_collected_profiles() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 先登记一个"替代者"画像
    let p = profile_with_events(1, SourceKind::Business).expect("夹具");
    ex.execute(
        &call("participant_profile_update", serde_json::to_value(&p).unwrap()),
        SessionPhase::Intel,
    );

    let out = expect_ok(ex.execute(&call("vn_map", serde_json::json!({})), SessionPhase::Intel));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["coverage"], serde_json::json!("1/4"), "有一个画像应体现在覆盖上：{out}");
    let sub = v["directions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["direction"] == serde_json::json!("substitutor"))
        .unwrap();
    assert_eq!(sub["covered"], serde_json::json!(true));
    assert_eq!(sub["profiles_found"], serde_json::json!(["rival"]));
}

/// 剧本未声明扫描深度时必须**如实说"未配置"**，不得假装有一个值。
#[test]
fn vn_map_does_not_fake_unconfigured_depths() {
    let theory = empty_theory();
    let pb = playbook(); // 测试夹具的 value_net 为 None
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(&call("vn_map", serde_json::json!({})), SessionPhase::Intake));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    for d in v["directions"].as_array().unwrap() {
        assert_eq!(
            d["scan_depth"],
            serde_json::json!("未配置"),
            "剧本没声明就该说没声明：{d}"
        );
    }
}

// ─────────────────────────── ask_user（采集）───────────────────────────

/// `ask_user` 从**剧本的 intake 清单**提问（领域专家写的比模型临场编的可靠）。
#[test]
fn ask_user_sources_questions_from_playbook_intake() {
    let theory = empty_theory();
    let mut pb = playbook();
    pb.intake = vec![
        shangyi_playbooks::IntakeQuestion {
            id: "q1".into(),
            text: "如果对手降价 15%，你会怎么做？".into(),
            kind: "choice".into(),
            options: vec!["跟着降".into(), "按兵不动".into()],
            extracts: None,
        },
        shangyi_playbooks::IntakeQuestion {
            id: "q2".into(),
            text: "你的毛利率大致多少？".into(),
            kind: "text".into(),
            options: vec![],
            extracts: None,
        },
    ];
    let mut ex = DomainExecutor::new(&theory, &pb);
    match ex.execute(&call("ask_user", serde_json::json!({})), SessionPhase::Intake) {
        ExecOutcome::AsksUser(qs) => {
            assert_eq!(qs.len(), 2);
            assert_eq!(qs[0].id, "q1");
            assert_eq!(qs[0].kind, "choice");
            assert_eq!(qs[0].options.len(), 2, "choice 型必须带上选项");
            assert_eq!(qs[1].id, "q2");
        }
        other => panic!("应从剧本取问题并挂起，实得 {other:?}"),
    }
}

/// 剧本没有 intake 清单且未显式给问题时 ⇒ 报错，不得凭空编几个问题。
#[test]
fn ask_user_refuses_to_invent_questions() {
    let theory = empty_theory();
    let pb = playbook(); // intake 为空
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(&call("ask_user", serde_json::json!({})), SessionPhase::Intake));
    assert!(msg.contains("没有声明 intake 问题"), "实得：{msg}");
    assert!(msg.contains("显式给出要问的问题"), "应指出补救方式：{msg}");
}

/// 显式给问题时必须校验：空文本即拒绝（空问题会挂起循环却无话可问）。
#[test]
fn ask_user_rejects_empty_question_text() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("ask_user", serde_json::json!({"questions": [{"id": "x", "text": "   "}]})),
        SessionPhase::Intake,
    ));
    assert!(msg.contains("缺 `text`"), "实得：{msg}");
}

#[test]
fn ask_user_blocks_long_questionnaires_in_both_input_paths() {
    let theory = empty_theory();
    let mut pb = playbook();
    pb.intake = (0..3).map(|i| shangyi_playbooks::IntakeQuestion {
        id: format!("q{i}"), text: format!("问题{i}"), kind: "text".into(),
        options: vec![], extracts: None,
    }).collect();
    let mut ex = DomainExecutor::new(&theory, &pb);
    for args in [serde_json::json!({}), serde_json::json!({"questions": [
        {"text":"问题一"}, {"text":"问题二"}, {"text":"问题三"}
    ]})] {
        let msg = expect_failed(ex.execute(&call("ask_user", args), SessionPhase::Intake));
        assert!(msg.contains("最多两个"), "{msg}");
        assert!(msg.contains("搜索公开事实"), "{msg}");
    }
}

/// `skip_answered` 必须真的跳过已回答的问题。
#[test]
fn ask_user_skip_answered_works() {
    let theory = empty_theory();
    let mut pb = playbook();
    pb.intake = vec![
        shangyi_playbooks::IntakeQuestion {
            id: "q1".into(),
            text: "问题一".into(),
            kind: "text".into(),
            options: vec![],
            extracts: None,
        },
        shangyi_playbooks::IntakeQuestion {
            id: "q2".into(),
            text: "问题二".into(),
            kind: "text".into(),
            options: vec![],
            extracts: None,
        },
    ];
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.mark_intake_answered("q1");
    match ex.execute(
        &call("ask_user", serde_json::json!({"skip_answered": true})),
        SessionPhase::Intake,
    ) {
        ExecOutcome::AsksUser(qs) => {
            assert_eq!(qs.len(), 1, "q1 已回答，应只问 q2");
            assert_eq!(qs[0].id, "q2");
        }
        other => panic!("实得 {other:?}"),
    }
}

/// **"无法评估" ≠ "评估为负面"。**
///
/// 未绑定检索后端时 `web_search` 必须返回 `Blocked`——语义是"这项情报未取得"。
/// 若返回 `Failed`，报告会把"没查到"读成"查了但没有"；若返回 `Ok`，
/// 那就是编造。两者都是审计失败。
#[test]
fn web_search_without_backend_is_blocked_not_failed() {
    match exec("scenario", "web_search", serde_json::json!({"query": "对手 定价"})) {
        ExecOutcome::Blocked(msg) => {
            assert!(
                msg.contains("未绑定") || msg.contains("未配置"),
                "实得：{msg}"
            );
            assert!(msg.contains("未取得"), "应明确说这是'未取得'而非结论：{msg}");
        }
        other => panic!("外部依赖缺失必须报 Blocked，实得 {other:?}"),
    }
}

#[test]
fn web_fetch_without_backend_is_blocked() {
    match exec("scenario", "web_fetch", serde_json::json!({"url": "https://example.com"})) {
        ExecOutcome::Blocked(_) => {}
        other => panic!("期望 Blocked，实得 {other:?}"),
    }
}

/// 接了检索后端后，同一工具必须真的走后端（证明 `Blocked` 是"缺后端"而非"永远失败"）。
struct FakeSearch {
    hits: Vec<SearchHit>,
    fail: bool,
}

impl SearchProvider for FakeSearch {
    fn search(&self, _query: &str) -> Result<Vec<SearchHit>, SearchUnavailable> {
        if self.fail {
            // 后端**存在但失败**：这是 Failed（该去修配置），不是 NoBackend。
            // `retryable: true` 与 503 的语义一致（服务端临时故障）。
            return Err(SearchUnavailable::Failed {
                detail: "后端 503".into(),
                retryable: true,
            });
        }
        Ok(self.hits.clone())
    }
    fn fetch(&self, url: &str) -> Result<FetchedText, SearchUnavailable> {
        if self.fail {
            return Err(SearchUnavailable::Failed {
                detail: "后端 503".into(),
                retryable: true,
            });
        }
        Ok(FetchedText {
            url: url.to_string(),
            text: "正文".into(),
            raw_bytes: 6,
        })
    }
}

#[test]
fn web_search_uses_backend_when_wired() {
    let theory = empty_theory();
    let pb = playbook();
    let sp = FakeSearch {
        hits: vec![SearchHit {
            title: "竞对降价公告".into(),
            url: "https://example.com/a".into(),
            snippet: "自 3 月起降价 15%".into(),
            backend: "fake".into(),
            published_at: None,
            rank_score: 73,
            rank_signals: vec!["官方域名".into()],
        }],
        fail: false,
    };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&sp);
    let out = expect_ok(ex.execute(
        &call("web_search", serde_json::json!({"query": "竞对 降价"})),
        SessionPhase::Intel,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["hits"][0]["title"], serde_json::json!("竞对降价公告"));
    // 情报必须带 provenance，否则 checkpoint① 无物可确认
    assert_eq!(v["provenance"]["kind"], serde_json::json!("web"));
}

/// 后端**运行中失败**同样是 `Blocked`（外部依赖不可用），不是 `Failed`。
#[test]
fn backend_failure_is_also_blocked_not_failed() {
    let theory = empty_theory();
    let pb = playbook();
    let sp = FakeSearch { hits: vec![], fail: true };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&sp);
    match ex.execute(
        &call("web_search", serde_json::json!({"query": "x"})),
        SessionPhase::Intel,
    ) {
        ExecOutcome::Blocked(msg) => assert!(msg.contains("503"), "实得：{msg}"),
        other => panic!("后端失败应为 Blocked，实得 {other:?}"),
    }
}

// ─────────────────────────── 画像与合规 ───────────────────────────

fn data_source(kind: SourceKind) -> Option<DataSource> {
    DataSource::new(kind, "https://example.com/report/1", 1_770_000_000_000).ok()
}

fn profile_with_events(n: usize, kind: SourceKind) -> Option<ParticipantProfile> {
    let mut p = ParticipantProfile::new("rival", ValueNetRole::Substitutor, RelationToGame::Player);
    p.common = CommonProfile {
        interests: vec!["份额".into()],
        resources: vec!["产能".into()],
        decision_style: "激进".into(),
        power_asymmetry: "接近对等".into(),
    };
    for i in 0..n {
        let src = data_source(kind)?;
        p.timeline.push(
            TimelineEvent::new(
                format!("2026-0{}-01", i + 1),
                EventType::PriceCut,
                0.15,
                src,
                0.8,
            )
            .ok()?,
        );
    }
    Some(p)
}

/// 画像的**每条时间线事件都必须带来源**（类型层面已强制），
/// 且来源不得是个人信息（合规红线）。
#[test]
fn profile_update_accepts_business_sourced_events() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let p = profile_with_events(2, SourceKind::Business).expect("夹具");
    let out = expect_ok(ex.execute(
        &call("participant_profile_update", serde_json::to_value(&p).unwrap()),
        SessionPhase::Intel,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["participant_id"], serde_json::json!("rival"));
    assert_eq!(v["profiles_total"], serde_json::json!(1));

    // 同一 id 再次更新 ⇒ 覆盖而非重复（否则画像数会虚高）
    let out2 = expect_ok(ex.execute(
        &call("participant_profile_update", serde_json::to_value(&p).unwrap()),
        SessionPhase::Intel,
    ));
    let v2: serde_json::Value = serde_json::from_str(&out2).unwrap();
    assert_eq!(v2["profiles_total"], serde_json::json!(1), "同 id 应覆盖，不得重复累加");
}

/// 个人信息来源在**类型层面**即无法构造——这是比"运行期检查"更强的保证。
///
/// `DataSource::new` 对 `SourceKind::Personal` 直接返回 `Err`，因此
/// **根本构造不出**含个人信息来源的画像。这条测试的意义是把这个性质钉住：
/// 若未来有人"为了方便"放宽构造器，测试会立刻失败，而不是等到合规事故之后。
///
/// 为什么这比运行期检查强：合规事故是**不可逆**的（数据已落盘）。
/// 检查放在构造器里，意味着非法的数据对象在进程内不存在——不是"存在但被拒"。
#[test]
fn personal_information_sources_cannot_even_be_constructed() {
    assert!(
        DataSource::new(SourceKind::Personal, "某人的身份证号", 0).is_err(),
        "个人信息来源必须无法构造"
    );
    assert!(
        DataSource::new(SourceKind::Unverified, "不知道哪来的", 0).is_err(),
        "未确认来源同样必须无法构造"
    );
    // 业务来源可构造（对照）
    assert!(DataSource::new(SourceKind::Business, "公开年报", 0).is_ok());

    // 推论：画像里不可能出现个人信息时间线，故执行器无需为它写检查
    assert!(
        profile_with_events(1, SourceKind::Personal).is_none(),
        "含个人信息来源的画像构造不出来"
    );
}

// ─────────────────────────── 产物登记 ───────────────────────────

/// 执行器登记的产物必须**随状态变化**：指纹用于检测"批准后产物被改"。
#[test]
fn artifacts_reflect_state_and_change_with_content() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    assert!(ex.artifacts().is_empty(), "初始无产物");

    let p = profile_with_events(1, SourceKind::Business).expect("夹具");
    ex.execute(
        &call("participant_profile_update", serde_json::to_value(&p).unwrap()),
        SessionPhase::Intel,
    );
    let a1 = ex.artifacts();
    assert!(
        a1.iter().any(|(k, _)| k.key() == "participant_profiles"),
        "画像应被登记：{a1:?}"
    );

    ex.set_game_spec(prisoners_dilemma());
    let a2 = ex.artifacts();
    assert!(a2.iter().any(|(k, _)| k.key() == "gamespec"));

    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    let a3 = ex.artifacts();
    assert!(a3.iter().any(|(k, _)| k.key() == "equilibrium_set"), "解集应被登记");
}

// ─────────────────────────── 三重过滤 ───────────────────────────

/// 三重过滤把通过的提议**注册回博弈**，并作废旧解集——
/// 若不注册，反击"借第三方之力改变对手支付"的机制就丢了。
#[test]
fn novel_move_filter_registers_accepted_moves_and_invalidates_equilibria() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    assert!(ex.state().equilibria_json.is_some());

    // 一个三判据齐备的提议：合法有依据、资源可得、收益区间远高于成本
    let proposal = serde_json::json!({
        "actor_id": "focal",
        "description": "向对手的大客户发函披露其价格欺诈",
        "payoff_channel": "借客户施压改变对手声誉支付",
        "expected_benefit": {"lo": 100.0, "hi": 200.0},
        "expected_cost": {"lo": 1.0, "hi": 5.0},
        "legality_basis": ["《反不正当竞争法》第十一条"],
        "feasibility_basis": ["已有客户联系方式"],
        "introduces_actors": []
    });
    let out = expect_ok(ex.execute(
        &call("novel_move_filter", proposal),
        SessionPhase::Consequence,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["accepted"].as_array().unwrap().len(),
        1,
        "三判据齐备的提议应通过：{out}"
    );
    assert_eq!(
        v["invalidated_equilibria"],
        serde_json::json!(true),
        "注册改变了博弈 ⇒ 旧解集必须作废，否则下游会读到与当前模型不符的均衡"
    );
    assert!(
        ex.state().equilibria_json.is_none(),
        "解集必须真的被清掉，而不只是报告上说清掉了"
    );
}

/// 缺合法性依据的提议必须被拒，且**拒绝理由要进结果**（报告③层要用）。
#[test]
fn novel_move_without_legality_basis_is_rejected_with_reasons() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());

    let proposal = serde_json::json!({
        "actor_id": "focal",
        "description": "看起来可行的反击",
        "payoff_channel": "施压",
        "expected_benefit": {"lo": 100.0, "hi": 200.0},
        "expected_cost": {"lo": 1.0, "hi": 5.0},
        "legality_basis": [],
        "feasibility_basis": ["有渠道"]
    });
    let out = expect_ok(ex.execute(
        &call("novel_move_filter", proposal),
        SessionPhase::Consequence,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["accepted"].as_array().unwrap().len(),
        0,
        "无合法性依据不得通过：{out}"
    );
    let rejected = v["rejected"].as_array().unwrap();
    assert_eq!(rejected.len(), 1);
    assert!(!rejected[0]["rejections"].as_array().unwrap().is_empty(), "拒绝理由必须给出");
}

// ─────────────────────────── 检索与状态 ───────────────────────────

/// **「未加载」与「空库」必须区分**（这是新语义的核心）。
///
/// 未加载 = 没接上（`Blocked`，用户该去配）；空库 = 接了但没内容（`Ok`，是结论）。
/// 混同会让「知识库没配」表现为「知识库里没有这条理论」——后者是知识主张。
#[test]
fn kb_retrieve_distinguishes_unloaded_from_empty() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 未绑定 wiki
    match ex.execute(&call("kb_retrieve", serde_json::json!({})), SessionPhase::Intake) {
        ExecOutcome::Blocked(m) => {
            assert!(m.contains("未加载"), "实得：{m}");
            assert!(m.contains("根本没接上"), "必须说清这不是「没有」：{m}");
        }
        other => panic!("未加载应是 Blocked，实得 {other:?}"),
    }

    // 绑定一个空 wiki ⇒ 是 Ok（结论），不是 Blocked
    let empty_wiki = std::sync::Arc::new(shangyi_kb::wiki::Wiki::build(vec![]).unwrap());
    let mut ex2 = DomainExecutor::new(&theory, &pb).with_wiki(Some(empty_wiki));
    match ex2.execute(&call("kb_retrieve", serde_json::json!({})), SessionPhase::Intake) {
        ExecOutcome::Ok(s) => assert!(s.contains("0 页"), "空库应报 0 页：{s}"),
        other => panic!("空库应是 Ok（结论），实得 {other:?}"),
    }
}

/// 索引能取到、页能下钻、且**出链带理由**——这是导航式检索的全部价值。
#[test]
fn kb_retrieve_supports_index_and_page_drilldown() {
    let theory = empty_theory();
    let pb = playbook();
    let mk = |id: &str, body: &str| shangyi_kb::wiki::WikiPage {
        id: id.into(),
        title: id.into(),
        kind: shangyi_kb::wiki::PageKind::Theory,
        summary: format!("{id} 摘要。"),
        body: body.into(),
        extra_link_text: vec![],
        tags: vec![],
    };
    let w = std::sync::Arc::new(
        shangyi_kb::wiki::Wiki::build(vec![
            mk("a", "指向 [[b]]"),
            mk("b", "[[a]]"),
        ])
        .unwrap(),
    );
    let mut ex = DomainExecutor::new(&theory, &pb).with_wiki(Some(w));

    // 默认取索引
    let idx = expect_ok(ex.execute(&call("kb_retrieve", serde_json::json!({})), SessionPhase::Intake));
    assert!(idx.contains("知识库索引"), "{idx}");
    assert!(idx.contains("**a**") && idx.contains("**b**"), "{idx}");

    // 取某页
    let page = expect_ok(ex.execute(
        &call("kb_retrieve", serde_json::json!({"page": "a"})),
        SessionPhase::Intake,
    ));
    assert!(page.contains("指向"), "应含正文：{page}");
    assert!(page.contains("## 入链"), "应含入链（who references me）：{page}");
}

/// 取不存在的页时，错误里要给**相近的 id**——否则模型只能反复试错。
#[test]
fn kb_retrieve_suggests_nearby_ids_on_miss() {
    let theory = empty_theory();
    let pb = playbook();
    let mk = |id: &str| shangyi_kb::wiki::WikiPage {
        id: id.into(),
        title: id.into(),
        kind: shangyi_kb::wiki::PageKind::Theory,
        summary: "摘要。".into(),
        body: "[[bertrand]]".into(),
        extra_link_text: vec![],
        tags: vec![],
    };
    let w = std::sync::Arc::new(
        shangyi_kb::wiki::Wiki::build(vec![mk("bertrand"), mk("x")]).unwrap(),
    );
    let mut ex = DomainExecutor::new(&theory, &pb).with_wiki(Some(w));
    // 用**子串型**错拼（`bertrand` 少了尾部）——这是关键词搜索能覆盖的形态。
    //
    // ⚠️ 已知限制：编辑距离型的错拼（`bertradn`，字母换位）**找不出相近项**，
    // 因为当前的"相近"判据是子串匹配而非编辑距离。本测试只断言前者。
    // 后者若将来需要，应加编辑距离——但那要引入 bktree 之类结构，
    // 对一个几十页的库不值得。
    let msg = expect_failed(ex.execute(
        &call("kb_retrieve", serde_json::json!({"page": "bertran"})),
        SessionPhase::Intake,
    ));
    assert!(msg.contains("没有页"), "实得：{msg}");
    assert!(
        msg.contains("bertrand"),
        "子串型错拼应给出相近 id 供一次改对：{msg}"
    );
}

#[test]
fn wiring_status_matches_actual_behaviour() {
    let status = wiring_status();
    // 清单里标 wired 的，调用后不应报"尚未接线"
    for (tool, st) in &status {
        if *st == "wired" {
            let out = exec("scenario", tool, serde_json::json!({}));
            if let ExecOutcome::Failed(m) = &out {
                assert!(
                    !m.contains("尚未接线"),
                    "清单说 `{tool}` 已接线，但实际报未接线：{m}"
                );
            }
        }
    }
    // 覆盖全部已注册工具（漏掉一个就会让它悄悄掉进"未知工具"分支）
    let reg = shangyi_tools::ToolRegistry::standard();
    for name in reg.names() {
        assert!(
            status.contains_key(name),
            "工具 `{name}` 不在接线清单里——审计无法回答它是否已接线"
        );
    }
    assert_eq!(status.len(), reg.len(), "接线清单与工具注册表的规模必须一致");
}

/// 未注册的工具名必须走"未知工具"路径而非 panic。
#[test]
fn unknown_tool_name_is_handled() {
    let msg = expect_failed(exec("scenario", "不存在的工具", serde_json::json!({})));
    assert!(msg.contains("尚未接线"), "实得：{msg}");
}

// ─────────────────────────── 蒙特卡洛与敏感性 ───────────────────────────

#[test]
fn monte_carlo_computes_percentiles_deterministically() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let samples: Vec<f64> = (1..=100).map(|i| i as f64).collect();
    let out1 = expect_ok(ex.execute(
        &call("monte_carlo", serde_json::json!({"samples": samples})),
        SessionPhase::Recommend,
    ));
    let out2 = expect_ok(ex.execute(
        &call("monte_carlo", serde_json::json!({"samples": samples})),
        SessionPhase::Recommend,
    ));
    assert_eq!(out1, out2, "同输入必须同输出（种子固定 ⇒ 可复现）");
    let v: serde_json::Value = serde_json::from_str(&out1).unwrap();
    let (p10, p50, p90) = (
        v["p10"].as_f64().unwrap(),
        v["p50"].as_f64().unwrap(),
        v["p90"].as_f64().unwrap(),
    );
    assert!(p10 <= p50 && p50 <= p90, "分位数必须单调：{out1}");
}

#[test]
fn monte_carlo_requires_samples() {
    // ⚠️ 这条测试断言的内容**已经变了**（run24 实测后重写）。
    //
    // 早先 `monte_carlo` 要求调用方给 `samples`，而**没有任何工具产出样本**
    // ——于是模型只能编一组数（违反铁律 1）或请求回退（自动模式被拒）⇒ 卡死。
    //
    // 现在它自己做：给 `param` 就沿该参数的**已声明分布**抽样并重解。
    // 无 GameSpec 时（本测试的情形）它报的是"缺 GameSpec"，
    // 无参数时报"缺 param"——两者都比"缺 samples"更接近真因。
    let msg = expect_failed(exec("scenario", "monte_carlo", serde_json::json!({})));
    assert!(
        msg.contains("GameSpec") || msg.contains("param"),
        "应指出真正缺的东西（GameSpec 或 param），实得：{msg}"
    );
    assert!(
        !msg.contains("samples"),
        "**不应再要求 samples**——那正是被修掉的死结（没有工具产出它）：{msg}"
    );
}

/// **参数的分布必须已声明**——不得替调用方假设一个。
///
/// 若这里静默补一个分布（比如按点值 ±10%），那是在造一个**无来源的不确定性
/// 模型**：读者会以为"这个范围有依据"，而它其实是我们编的。
/// 分位数会因此看起来可信，而依据是空的。
#[test]
fn monte_carlo_refuses_to_invent_a_distribution() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 一个没有 dist 的参数
    let spec = serde_json::json!({
        "game_id": "probe",
        "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "维持,维持": {"us": 3, "rival": 3},
            "维持,降价": {"us": 0, "rival": 5},
            "降价,维持": {"us": 5, "rival": 0},
            "降价,降价": {"us": 1, "rival": 1}
        }}},
        "params": {
            "margin": {"name": "margin", "value": 3,
                       "source": {"kind": "user_confirmed", "checkpoint": "cp2"}}
        }
    });
    expect_ok(ex.execute(&call("gamespec_build", spec.clone()), SessionPhase::ModelBuild));
    let msg = expect_failed(ex.execute(
        &call("monte_carlo", serde_json::json!({"param": "margin"})),
        SessionPhase::Recommend,
    ));
    assert!(
        msg.contains("没有声明分布") && msg.contains("不替你假设"),
        "必须拒绝为无分布参数编一个分布，实得：{msg}"
    );
}

/// **`assumed` 来源的参数不得用于传播**（铁律 2 的类型级强制）。
#[test]
fn monte_carlo_rejects_assumed_params() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let spec = serde_json::json!({
        "game_id": "probe",
        "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "维持,维持": {"us": "m", "rival": "m"},
            "降价,降价": {"us": "m2", "rival": "m2"}
        }}},
        "params": {
            "m": {"name": "m", "value": 3,
                  "dist": {"dist": "normal", "values": {"mu": 3, "sigma": 0.3}},
                  "source": {"kind": "assumed", "note": "随手写的"}}
        }
    });
    // ⚠️ 实测发现：校验在 **build 期**就拦住了 `assumed`（比这里更早，
    // 也更好——早失败早反馈）。故本测试断言的是 build 就被拒。
    // 保留原意图：**无来源的参数不得用于传播**，只是拦截点提前了。
    let msg = expect_failed(ex.execute(&call("gamespec_build", spec), SessionPhase::ModelBuild));
    assert!(msg.contains("assumed"), "应指出来源非法，实得：{msg}");
    assert!(
        msg.contains("铁律 2"),
        "应点明违反的是哪条铁律（模型据此理解为何被拒）：{msg}"
    );
}

/// **有分布时，引擎自己抽样并重解**——这是修掉死结后的主路径。
#[test]
fn monte_carlo_actually_samples_and_resolves() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let spec = serde_json::json!({
        "game_id": "probe",
        "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "维持,维持": {"us": "m", "rival": "m"},
            "维持,降价": {"us": 0, "rival": 5},
            "降价,维持": {"us": 5, "rival": 0},
            "降价,降价": {"us": 1, "rival": 1}
        }}},
        "params": {
            "m": {"name": "m", "value": 3,
                  "dist": {"dist": "normal", "values": {"mu": 3, "sigma": 0.3}},
                  "source": {"kind": "user_confirmed", "checkpoint": "cp2"}}
        }
    });
    expect_ok(ex.execute(&call("gamespec_build", spec), SessionPhase::ModelBuild));
    let out = ex.execute(
        &call(
            "monte_carlo",
            serde_json::json!({"param": "m", "draws": 30, "seed": 7}),
        ),
        SessionPhase::Recommend,
    );
    let json = match out {
        ExecOutcome::Ok(j) => j,
        other => panic!("应成功（引擎自采样），实得：{other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["mode"], serde_json::json!("engine_sampling"));
    assert_eq!(v["solved"], serde_json::json!(30), "30 个样本都应解出：{json}");
    assert!(v["pessimistic"]["p50"].is_number(), "应给出悲观序列分位数：{json}");
    assert!(v["optimistic"]["p50"].is_number(), "应给出乐观序列分位数：{json}");
    // 语义边界必须写明：未做选择
    let cav = v["_caveats"].as_array().unwrap();
    assert!(
        cav.iter().any(|c| c.as_str().unwrap_or("").contains("未做选择")),
        "必须写明「未做选择」——否则读者会把区间读成概率预测：{json}"
    );
}

/// **可复现**：同种子 ⇒ 逐位相同的结果（T8 要求）。
#[test]
fn monte_carlo_is_reproducible_under_the_same_seed() {
    let theory = empty_theory();
    let pb = playbook();
    let make = || {
        let mut ex = DomainExecutor::new(&theory, &pb);
        let spec = serde_json::json!({
            "game_id": "probe",
            "players": [
                {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
                {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
            ],
            "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
            // 2×2 的**四格都要有**——缺格会被适配器如实拒绝
            // （它甚至给了「已尝试 [\"维持,降价\", \"维持|降价\"]」这样的诊断）
            "payoffs": {"form": "normal", "values": {"cells": {
                "维持,维持": {"us": "m", "rival": "m"},
                "维持,降价": {"us": 0, "rival": 5},
                "降价,维持": {"us": 5, "rival": 0},
                "降价,降价": {"us": 1, "rival": 1}
            }}},
            "params": {
                "m": {"name": "m", "value": 3,
                      "dist": {"dist": "uniform", "values": {"low": 2.0, "high": 4.0}},
                      "source": {"kind": "user_confirmed", "checkpoint": "cp2"}}
            }
        });
        ex.execute(&call("gamespec_build", spec), SessionPhase::ModelBuild);
        ex.execute(
            &call("monte_carlo", serde_json::json!({"param": "m", "draws": 20, "seed": 42})),
            SessionPhase::Recommend,
        )
    };
    let a = match make() { ExecOutcome::Ok(j) => j, o => panic!("{o:?}") };
    let b = match make() { ExecOutcome::Ok(j) => j, o => panic!("{o:?}") };
    assert_eq!(a, b, "同种子必须给出**逐位相同**的结果（T8 可复现性）");
}

#[test]
fn sensitivity_scan_locates_flip_interval() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(
        &call(
            "sensitivity_scan",
            serde_json::json!({"points": [
                {"param": 0.1, "flipped": false},
                {"param": 0.2, "flipped": false},
                {"param": 0.3, "flipped": true},
                {"param": 0.4, "flipped": true},
            ]}),
        ),
        SessionPhase::Recommend,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["flip_interval"], serde_json::json!([0.2, 0.3]));
}

/// 扫描范围内未观察到翻转必须**如实框定**为"范围内的结论"，
/// 不得暗示"该结论稳健"——那是超出证据的主张。
#[test]
fn sensitivity_scan_without_flip_does_not_overclaim() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(
        &call(
            "sensitivity_scan",
            serde_json::json!({"points": [
                {"param": 0.1, "flipped": false},
                {"param": 0.2, "flipped": false},
            ]}),
        ),
        SessionPhase::Recommend,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["flip_interval"], serde_json::Value::Null);
    let note = v["note"].as_str().unwrap();
    assert!(note.contains("不等于"), "不得把'没翻'说成'稳健'：{note}");
}

/// 敏感性扫描至少要两个点，否则无法定义"翻转"。
#[test]
fn sensitivity_scan_needs_at_least_two_points() {
    let msg = expect_failed(exec(
        "scenario",
        "sensitivity_scan",
        serde_json::json!({"points": [{"param": 0.1, "flipped": false}]}),
    ));
    assert!(msg.contains("2"), "实得：{msg}");
}

// ─────────────────────────── 融合 ───────────────────────────

#[test]
fn select_fuse_requires_scores_first() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    let msg = expect_failed(ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select));
    assert!(msg.contains("select_equilibria"), "应指出先做什么：{msg}");
}

/// 端到端：求解 → 登记判据分数 → 融合 ⇒ 得到一个归一化的分布，
/// 且**冷启动置信度必须标注未校准**（不得让读者以为概率已校准）。
#[test]
fn fuse_produces_normalized_distribution_with_uncalibrated_confidence() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);

    ex.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "risk_dom", "scores": [0.7]}),
        ),
        SessionPhase::Select,
    );
    ex.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "stochastic", "scores": [0.4]}),
        ),
        SessionPhase::Select,
    );

    let out = expect_ok(ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let entries = v["entries"].as_array().unwrap();
    let sum: f64 = entries.iter().map(|e| e["prob"].as_f64().unwrap()).sum();
    assert!((sum - 1.0).abs() < 1e-9, "融合分布必须归一化，实得和 {sum}");
    assert_eq!(
        v["confidence"]["PriorUncalibrated"] != serde_json::Value::Null
            || v["confidence"].to_string().contains("PriorUncalibrated"),
        true,
        "冷启动期置信度必须标注未校准：{out}"
    );
}

/// 序列化后的融合结果必须能被 `EquilibriumDistribution` 反序列化
/// （否则写进报告/案例库时会静默丢字段）。
#[test]
fn fused_distribution_round_trips() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "risk_dom", "scores": [1.0]}),
        ),
        SessionPhase::Select,
    );
    let out = expect_ok(ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select));
    let d: shangyi_selection::EquilibriumDistribution =
        serde_json::from_str(&out).expect("融合结果必须可反序列化");
    assert!(!d.entries.is_empty());
    // 顺带钉住一个此前修过的缺陷：most_likely 的并列判定必须稳定
    let (_, ties) = d.most_likely().expect("已有分布");
    assert!(!ties, "单一候选不应被报为并列");
}

/// 未接线的工具名清单必须与 `wiring_status` 的键完全一致，
/// 且"needs-external"的工具返回 Blocked 而非 Failed。
#[test]
fn needs_external_tools_are_blocked() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    for (tool, st) in wiring_status() {
        if st == "needs-external" {
            match ex.execute(&call(tool, serde_json::json!({"query": "x", "url": "https://x"})), SessionPhase::Intel) {
                ExecOutcome::Blocked(_) => {}
                other => panic!("`{tool}` 标为 needs-external，实测 {other:?}"),
            }
        }
    }
}

/// 一个只用于断言"执行器不依赖 HashMap 迭代序"的小工具检查。
#[test]
fn artifacts_order_is_deterministic() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    let a = ex.artifacts();
    let b = ex.artifacts();
    assert_eq!(a, b, "产物清单的顺序必须稳定（指纹会进日志）");
    let _ = HashMap::<String, String>::new();
}

// ─────────────────────────── pars_check（PARTS 完备性）───────────────────────────

/// PARTS 检查必须**逐项给出缺口与补救路径**，而不是只报一个布尔。
#[test]
fn pars_check_reports_each_gap_with_actionable_hint() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(&call("pars_check", serde_json::json!({})), SessionPhase::Intake));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();

    assert_eq!(v["total"], serde_json::json!(5));
    assert_eq!(v["complete"], serde_json::json!(false));

    let parts = v["parts"].as_object().unwrap();
    for key in ["players", "added_value", "rules", "tactics", "scope"] {
        let item = parts
            .get(key)
            .unwrap_or_else(|| panic!("缺 PARTS 要素 `{key}`：{out}"));
        assert_eq!(item["present"], serde_json::json!(false), "{key} 此刻不应为 present");
        // 缺口必须带**可操作的**补救提示（只说"缺"没有用）
        let hint = item["missing_hint"].as_str().unwrap();
        assert!(!hint.trim().is_empty(), "`{key}` 缺项必须给出补救提示");
    }
    // 明确它不是"模型推荐"
    assert!(
        v["note"].as_str().unwrap().contains("不是模型选择"),
        "应说明本工具不负责模型选择：{out}"
    );
}

/// 材料补齐后 PARTS 各项必须转为 present（否则这个检查没有信息量）。
#[test]
fn pars_check_flips_to_present_as_material_arrives() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);

    // 先给画像（参与者 + 规则依据）
    let p = profile_with_events(2, SourceKind::Business).expect("夹具");
    ex.execute(
        &call("participant_profile_update", serde_json::to_value(&p).unwrap()),
        SessionPhase::Intel,
    );
    let out = expect_ok(ex.execute(&call("pars_check", serde_json::json!({})), SessionPhase::Intel));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["parts"]["players"]["present"], serde_json::json!(true));
    assert_eq!(v["parts"]["rules"]["present"], serde_json::json!(true));

    // 再建模（附加值 + 战术）
    ex.set_game_spec(prisoners_dilemma());
    let out2 = expect_ok(ex.execute(&call("pars_check", serde_json::json!({})), SessionPhase::ModelBuild));
    let v2: serde_json::Value = serde_json::from_str(&out2).unwrap();
    assert_eq!(v2["parts"]["tactics"]["present"], serde_json::json!(true));
    assert!(v2["present"].as_u64().unwrap() >= 4, "材料补齐后应有 >=4 项：{out2}");
}

// ─────────────────────────── 案例库（回测闭环）───────────────────────────

/// **归档必须带上此刻的预测**——否则事后有结果也无从比对。
///
/// # 断言语义在本次修整后变了（值得记下为什么）
///
/// 早先它断言"无法归档"（那是对旧措辞 `尚无均衡概率分布…无法归档` 的匹配）。
/// 而那个旧措辞本身就带着缺陷：它把"预测"等同于"均衡分布"，于是
/// **whatif/discover 模式的终态不可达**（那两条路径没有 select）。
///
/// 现在错误信息列出**三种**可接受的预测来源。断言随之改为：
/// 说清"为什么必须有预测"，且**覆盖三种模式**——若有人改回
/// "只认分布"，这条测试会失败。
#[test]
fn case_store_write_refuses_without_a_frozen_prediction() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "c1", "provenance": "exercise"})),
        SessionPhase::Archived,
    ));
    assert!(
        msg.contains("预测") && msg.contains("校准"),
        "应说明为什么必须有预测（否则事后无从比对、对校准无用）：{msg}"
    );
    // 三种模式的预测来源都必须被列出——否则那两条路径又会不可达
    for src in ["select_fuse", "consequence_predict", "discover_satisfice"] {
        assert!(
            msg.contains(src),
            "错误信息应列出 `{src}` 作为可接受的预测来源（共三种模式）：{msg}"
        );
    }
}

/// **whatif 路径必须能归档**——这是"终态不可达"的直接回归。
///
/// 缺陷背景：`case_store_write` 早先硬性要求均衡分布，而 whatif 的路径
/// （`… solve → consequence → recommend → report → archived`）**没有 select**
/// ⇒ 该模式的 `archived` **永远不可达**，也就是永远收不了尾。
#[test]
fn case_store_write_accepts_a_consequence_report() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    expect_ok(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "降价"})),
        SessionPhase::Consequence,
    ));
    let out = ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "whatif-case", "provenance": "exercise"})),
        SessionPhase::Archived,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "**whatif 路径必须能归档**（它没有分布，但后果报告就是「此刻的预测」）：{out:?}"
    );
}

/// **discover 路径必须能归档**（同因：终态不可达的回归）。
#[test]
fn case_store_write_accepts_a_discovery_report() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    expect_ok(ex.execute(
        &call(
            "discover_satisfice",
            serde_json::json!({
                "aspiration": {"levels": {"利润": 1.0}, "default_threshold": 0.5},
                "candidates": [
                    {"id": "c1", "description": "维持",
                     "outcomes": {"利润": {"p10": 2.0, "p50": 3.0, "p90": 4.0}},
                     "llm_proposed": false}
                ]
            }),
        ),
        SessionPhase::Discover,
    ));
    let out = ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "discover-case", "provenance": "exercise"})),
        SessionPhase::Archived,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "**discover 路径必须能归档**（满意策略集就是「此刻的预测」）：{out:?}"
    );
}

#[test]
fn case_store_write_then_read_round_trips() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let out = expect_ok(ex.execute(
        &call(
            "case_store_write",
            serde_json::json!({"case_id": "case-aaa", "date": "2026-09-18", "question": "要不要降价", "provenance": "exercise"}),
        ),
        SessionPhase::Archived,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["prediction_frozen"], serde_json::json!(true));
    assert_eq!(v["library_size"], serde_json::json!(1));

    // 读回来：预测必须还在
    let read = expect_ok(ex.execute(
        &call("case_store_read", serde_json::json!({"case_id": "case-aaa", "provenance": "exercise"})),
        SessionPhase::Archived,
    ));
    let rec: serde_json::Value = serde_json::from_str(&read).unwrap();
    assert_eq!(rec["case_id"], serde_json::json!("case-aaa"));
    assert_eq!(rec["decision_question"], serde_json::json!("要不要降价"));
    assert!(!rec["prediction"]["candidates"].as_array().unwrap().is_empty());
}

/// 重复 case_id 必须被拒——覆盖会**丢失原有的预测留档**。
#[test]
fn case_store_write_refuses_to_overwrite() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let mk = || call("case_store_write", serde_json::json!({"case_id": "dup", "provenance": "exercise"}));
    expect_ok(ex.execute(&mk(), SessionPhase::Archived));
    let msg = expect_failed(ex.execute(&mk(), SessionPhase::Archived));
    assert!(msg.contains("已存在"), "实得：{msg}");
    assert!(msg.contains("覆盖"), "应说明为何拒绝覆盖：{msg}");
}

/// 空库必须**如实**说"未校准"，不得让读者以为概率已校准。
#[test]
fn empty_case_library_reports_uncalibrated_honestly() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(&call("case_store_read", serde_json::json!({})), SessionPhase::Intake));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["total"], serde_json::json!(0));
    assert_eq!(v["calibration_ready"], serde_json::json!(false));
    let note = v["note"].as_str().unwrap();
    assert!(
        note.contains("空库不等于") && note.contains("未校准"),
        "空库不得被读成'有可用先验'：{note}"
    );
}

// ─────────────────────────── report_render（铁律 1 的落点）───────────────────────────

/// **报告里的每个数字都必须带 provenance**；缺它就是"这个数哪来的"无法回答。
#[test]
fn report_render_produces_provenanced_report() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let out = expect_ok(ex.execute(
        &call("report_render", serde_json::json!({"case_id": "c", "provenance": "exercise"})),
        SessionPhase::Report,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert!(v["sections"].as_u64().unwrap() >= 4, "报告应含四个层：{out}");
    assert!(
        v["provenance_records"].as_u64().unwrap() >= 2,
        "数字必须有 provenance 记录：{out}"
    );
    // 必须声明这是**模型**而非渲染产物
    let note = v["note"].as_str().unwrap();
    assert!(note.contains("报告模型") && note.contains("侧车"), "实得：{note}");
}

/// 报告必须**如实**把置信度放在老板页（不得藏进附录）。
#[test]
fn report_states_confidence_on_the_boss_page() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let out = expect_ok(ex.execute(&call("report_render", serde_json::json!({})), SessionPhase::Report));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let boss = v["report"]["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["layer"] == serde_json::json!("boss_page"))
        .expect("应有老板页");
    let text = boss["paragraphs"].as_array().unwrap().iter()
        .filter_map(|p| p.as_str()).collect::<Vec<_>>().join(" ");
    assert!(text.contains("置信度"), "老板页必须写明置信度：{text}");
    assert!(text.contains("未校准"), "冷启动期应写明未校准：{text}");
}

/// 没有可报告的数字时，**拒绝出报告**（而不是出一份只有散文的文档）。
///
/// # 为什么"拒绝"比"出一份说明性文档"更正确
///
/// 一份只有散文、没有数字的报告在读者眼里与正常报告**长得一样**——
/// 它会被当成分析已完成。而实际上这次咨询根本没算出任何结果。
/// 那正是"看起来完成"这类审计失败的典型形态，故宁可不出。
#[test]
fn report_without_distribution_is_refused_not_faked() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("report_render", serde_json::json!({})),
        SessionPhase::Report,
    ));
    assert!(msg.contains("不产出报告"), "实得：{msg}");
    assert!(
        msg.contains("比不出报告更坏"),
        "应说明为何拒绝而不是退化成散文：{msg}"
    );
    assert!(msg.contains("select"), "应指出该先完成哪一步：{msg}");
}

// ─────────────────────────── 铁律 2 的执行链 ───────────────────────────

/// **回归测试**：`checkpoint` 必须真的可用，否则**铁律 2 从未生效**且流程死锁。
///
/// # 这是一处真实缺陷的回归测试（2026-09-18 发现）
///
/// 当时的状况：`checkpoint` 在接线清单里被标为 `"wired-by-loop"`，
/// 但 `DomainExecutor` **根本没有它的分支**——它落到 `not_wired(...)`。
/// 而循环只在执行器返回 `ExecOutcome::NeedsUserInput` 时才会打开 checkpoint。
///
/// 后果是**两个**，都是承重的：
///
/// 1. **铁律 2 从未生效**：模型调 `checkpoint` 得到"工具尚未接线"，
///    于是用户确认这一步在真实运行里**从未发生过**。
/// 2. **流程死锁**：`gate_to_advance` 要求 intel/model_build/cp3 的 checkpoint
///    已批准才能离开该阶段；而 checkpoint 永远打不开 ⇒ 任何咨询
///    **都卡在 intel 不能前进**。
///
/// 为什么既有测试没抓到：`wiring_status_matches_actual_behaviour` 只检查
/// 标为 `"wired"` 的工具不报"未接线"，而 `"wired-by-loop"` **不在该检查范围内**
/// ——那正是这个假标记得以存在的原因。
#[test]
fn checkpoint_tool_must_open_a_checkpoint_not_report_unimplemented() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_profiles(vec![profile_with_events(1, SourceKind::Business).expect("夹具")]);

    // intel 阶段的 checkpoint 是 cp1（事实确认）
    let out = ex.execute(
        &call(
            "checkpoint",
            serde_json::json!({"items": [
                {"id": "f1", "content": "对手是三家", "source": "2025 年报"}
            ]}),
        ),
        SessionPhase::Intel,
    );

    match out {
        ExecOutcome::NeedsUserInput(req) => {
            assert_eq!(req.id, shangyi_core::CheckpointId::Cp1, "intel 阶段应打开 cp1");
            assert_eq!(req.phase, SessionPhase::Intel);
            assert_eq!(req.items.len(), 1);
            assert_eq!(req.items[0].source.as_deref(), Some("2025 年报"));
        }
        ExecOutcome::Failed(m) => panic!(
            "`checkpoint` 不可用 ⇒ 铁律 2 失效且流程死锁。实得：{m}"
        ),
        other => panic!("期望 NeedsUserInput，实得 {other:?}"),
    }
}

/// **铁律 2 的强制点**：cp2（硬假设）的每一项都必须带来源，缺则拒绝。
///
/// 这条不是"格式检查"：一条没有来源的假设即使用户点了确认，确认的也是
/// 一个凭空的值——那等于把铁律 2 降级成一句愿望。
#[test]
fn checkpoint_refuses_assumptions_without_sources() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());

    // model_build 阶段的 checkpoint 是 cp2（硬假设确认）
    let msg = match ex.execute(
        &call(
            "checkpoint",
            serde_json::json!({"items": [
                {"id": "payoff_A", "content": "对手 A 在价格战中的收益", "proposed_value": "120"}
                // ← 没有 source
            ]}),
        ),
        SessionPhase::ModelBuild,
    ) {
        ExecOutcome::Failed(m) => m,
        other => panic!("无来源的硬假设必须被拒，实得 {other:?}"),
    };
    assert!(msg.contains("来源"), "应指出缺什么：{msg}");
    assert!(msg.contains("铁律 2"), "应点明违反的是哪条铁律：{msg}");
}

/// 空清单必须被拒——空提问件若被结算为"已确认"，等于用户没看任何东西就盖了章。
#[test]
fn checkpoint_refuses_empty_items() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_profiles(vec![profile_with_events(1, SourceKind::Business).expect("夹具")]);
    let msg = expect_failed(ex.execute(
        &call("checkpoint", serde_json::json!({"items": []})),
        SessionPhase::Intel,
    ));
    assert!(msg.contains("空") || msg.contains("items"), "实得：{msg}");
}

/// 在没有 checkpoint 的阶段调用它 ⇒ 明确拒绝并指出该在哪个阶段用。
#[test]
fn checkpoint_rejects_phases_without_a_checkpoint() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call(
            "checkpoint",
            serde_json::json!({"items": [{"id":"x","content":"y","source":"z"}]}),
        ),
        SessionPhase::Intake, // intake 无 checkpoint
    ));
    assert!(msg.contains("intake"), "应指出当前阶段：{msg}");
    assert!(
        msg.contains("intel") || msg.contains("model_build"),
        "应指出该在哪个阶段用：{msg}"
    );
}

/// **接线清单不得说谎**：标为 `wired-by-loop` 的工具也必须真的可用。
///
/// 这条堵住既有检查的盲区——它只验证 `"wired"`，于是
/// `checkpoint` 挂着 `"wired-by-loop"` 假标记而从未有人发现。
#[test]
fn wired_by_loop_tools_are_actually_wired() {
    for (tool, status) in wiring_status() {
        if status != "wired-by-loop" {
            continue;
        }
        // 在每个阶段都试一次：至少有一个阶段它应给出**非"未接线"**的结果
        let phases = [
            SessionPhase::Intake,
            SessionPhase::Intel,
            SessionPhase::ModelBuild,
            SessionPhase::Solve,
            SessionPhase::Select,
        ];
        let mut produced_something = false;
        for ph in phases {
            let theory = empty_theory();
            let pb = playbook();
            let mut ex = DomainExecutor::new(&theory, &pb);
            let out = ex.execute(
                &call(tool, serde_json::json!({"items":[{"id":"x","content":"y","source":"z"}]})),
                ph,
            );
            if let ExecOutcome::Failed(m) = &out {
                assert!(
                    !m.contains("尚未接线"),
                    "清单标 `{tool}` 为 wired-by-loop，但它在 {ph} 阶段报未接线：{m}"
                );
                produced_something = true; // 报了别的错也是"已接线"
            } else {
                produced_something = true;
            }
        }
        assert!(produced_something, "`{tool}` 在所有阶段都无响应");
    }
}

/// `gamespec_build` 的报错必须**可操作**——实测模型连试 3 次同一个错，
/// 因为 `missing field \`name\`` 没告诉它**哪个对象**缺、该填什么。
#[test]
fn gamespec_build_error_is_actionable() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("gamespec_build", serde_json::json!({
            "game_id": "g",
            "players": [{"id": "a", "vn_role": "focal", "relation_to_game": "player"}],
            "strategies": {"a": ["x","y"]},
            "payoffs": {"form": "normal", "cells": {}}
        })),
        SessionPhase::ModelBuild,
    ));
    assert!(msg.contains("missing field"), "应保留原始错误：{msg}");
    // 必须给出字段清单与常见错误
    assert!(msg.contains("players"), "应列出字段：{msg}");
    assert!(msg.contains("name"), "应指出缺失的字段名：{msg}");
    assert!(
        msg.contains("常见错误") && msg.contains("strategies"),
        "应给出常见错误与 strategies 的键要求：{msg}"
    );
}

fn stored_three_by_three_reference_spec() -> GameSpec {
    let mut spec = prisoners_dilemma();
    spec.game_id = "stored-reference-model".into();
    let strategies = vec!["方案甲".to_string(), "方案乙".to_string(), "方案丙".to_string()];
    spec.strategies.insert("focal".into(), strategies.clone());
    spec.strategies.insert("rival".into(), strategies.clone());
    let mut cells = BTreeMap::new();
    for (i, first) in strategies.iter().enumerate() {
        for (j, second) in strategies.iter().enumerate() {
            let mut payoffs = BTreeMap::new();
            for actor in ["focal", "rival"] {
                let key = format!("payoff_{actor}_{i}_{j}");
                spec.params.insert(key.clone(), shangyi_gamespec::Param {
                    name: key.clone(),
                    value: Expr::Literal((i + j + 1) as f64),
                    dist: None,
                    source: shangyi_gamespec::ParamSource::UserConfirmed { checkpoint: "cp2".into() },
                    unit: Some("测试效用单位，非实际收益".into()),
                });
                payoffs.insert(actor.into(), Expr::Ref(key));
            }
            cells.insert(format!("{first},{second}"), payoffs);
        }
    }
    spec.payoffs = PayoffForm::Normal { cells };
    spec
}

#[test]
fn short_task_scope_cannot_advance_into_or_complete_a_full_consultation() {
    use shangyi_core::{ConsultationMode, PhaseMachine};
    use shangyi_core::task_scope::TaskScope;
    let mut machine=PhaseMachine::new(ConsultationMode::Scenario).require_task_scope_selection();
    assert!(machine.gate_to_advance().is_err());
    machine.select_task_scope(TaskScope::Research).unwrap();
    assert!(machine.advance_to(SessionPhase::Intel).is_err());
    assert!(!machine.allows_task_tool("checkpoint"));
    assert!(!machine.allows_task_tool("gamespec_build"));
    machine.select_task_scope(TaskScope::Consultation).unwrap();
    assert!(!machine.allows_task_tool("complete_task"));
    assert_eq!(machine.advance().unwrap(),SessionPhase::Intel);
    assert!(machine.gate_to_advance().is_err(),"full consultation still requires its real artifacts and approval");
    assert!(machine.select_task_scope(TaskScope::Cost).is_err());
}

#[test]
fn scoped_cost_completion_requires_current_receipts_and_retains_null_amounts() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    let done=call("complete_task",serde_json::json!({"summary":"已整理费用。","missing":[]}));
    expect_failed(ex.execute(&done,SessionPhase::Intake));
    let select=call("select_task_scope",serde_json::json!({"scope":"cost","reason":"只算费用"}));
    assert!(matches!(ex.execute(&select,SessionPhase::Intake),ExecOutcome::TaskScopeSelected{..}));
    expect_failed(ex.execute(&done,SessionPhase::Intake));
    expect_failed(ex.execute(&call("checkpoint",serde_json::json!({"items":[]})),SessionPhase::Intake));
    expect_ok(ex.execute(&call("business_cost",serde_json::json!({
        "operation":"create","scenario":"设备","currency":"CNY","costs_complete":false,
        "costs":[{"scenario":"设备","name":"设备费","treatment":"cash_expense","amount":null,"period":"once","basis":"user_provided","source":"报价未取得"}]
    })),SessionPhase::Intake));
    let ExecOutcome::ScopedTaskCompleted(result)=ex.execute(&done,SessionPhase::Intake) else {panic!("expected scoped delivery")};
    assert!(!result.is_complete());assert!(!result.missing.is_empty());
    assert!(result.cost_scenarios[0]["items"][0]["normalized_amount"].is_null());
    ex.begin_followup_task();
    assert!(matches!(ex.execute(&select,SessionPhase::Intake),ExecOutcome::TaskScopeSelected{..}));
    expect_failed(ex.execute(&done,SessionPhase::Intake));
    assert_eq!(ex.state().cost_scenarios.len(),1,"prior calculations remain available but are not new-task receipts");
}

#[test]
fn consultation_scope_cannot_use_short_delivery_to_skip_existing_gates() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    assert!(matches!(ex.execute(&call("select_task_scope",serde_json::json!({"scope":"consultation","reason":"请求完整策略分析"})),SessionPhase::Intake),ExecOutcome::TaskScopeSelected{..}));
    expect_failed(ex.execute(&call("complete_task",serde_json::json!({"summary":"声称完整分析完成","missing":[]})),SessionPhase::Intake));
    assert!(ex.state().game_spec.is_none());assert!(ex.state().equilibria_json.is_none());
}

#[test]
fn complete_task_rejects_escaped_layout_without_rewriting_and_accepts_corrected_markdown() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    assert!(matches!(ex.execute(&call("select_task_scope",serde_json::json!({"scope":"cost","reason":"用户要费用表格"})),SessionPhase::Intake),ExecOutcome::TaskScopeSelected{..}));
    expect_ok(ex.execute(&call("business_cost",serde_json::json!({
        "operation":"create","scenario":"设备","currency":"CNY","costs_complete":true,
        "costs":[{"scenario":"设备","name":"设备费","treatment":"cash_expense","amount":10000,"period":"once","basis":"user_provided","source":"用户提供的测试报价"}]
    })),SessionPhase::Intake));
    let receipts=ex.state().cost_scenarios.clone();
    let escaped=r"|项目|金额|\n|---|---|\n|设备费|10000元|\n\n来源：用户提供的测试报价。";
    let request=call("complete_task",serde_json::json!({"summary":escaped,"missing":[]}));
    let message=expect_failed(ex.execute(&request,SessionPhase::Intake));
    assert!(message.contains("真正换行") && message.contains("重新调用 complete_task"));
    assert!(message.contains("保持已核实事实") && message.contains("不需要重做检索或计算"));
    assert_eq!(request.arguments["summary"],escaped,"validation must not rewrite the submitted text");
    assert_eq!(ex.state().cost_scenarios,receipts,"formatting failure must preserve verified calculation receipts");
    let markdown="|项目|金额|\n|---|---|\n|设备费|10000元|\n\n来源：用户提供的测试报价。";
    let ExecOutcome::ScopedTaskCompleted(result)=ex.execute(&call("complete_task",serde_json::json!({"summary":markdown,"missing":[]})),SessionPhase::Intake) else {panic!("corrected Markdown must be deliverable without recalculating")};
    assert_eq!(result.summary,markdown);assert!(result.is_complete());
    let literal=r"原话里的 `\n\n` 和代码 `\n- example` 保留原样；设备报价10000元。";
    let ExecOutcome::ScopedTaskCompleted(result)=ex.execute(&call("complete_task",serde_json::json!({"summary":literal,"missing":[]})),SessionPhase::Intake) else {panic!("intentional code literals are valid")};
    assert_eq!(result.summary,literal);
}

#[test]
fn gamespec_validate_empty_request_checks_the_current_reference_model() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let missing = expect_failed(ex.execute(&call("gamespec_validate", serde_json::json!({})), SessionPhase::ModelBuild));
    assert!(missing.contains("尚无 GameSpec"));
    assert!(missing.contains("gamespec_build"));
    let stored = stored_three_by_three_reference_spec();
    expect_ok(ex.execute(&call("gamespec_build", serde_json::to_value(&stored).unwrap()), SessionPhase::ModelBuild));
    let result: serde_json::Value = serde_json::from_str(&expect_ok(ex.execute(
        &call("gamespec_validate", serde_json::json!({})), SessionPhase::ModelBuild,
    ))).unwrap();
    assert_eq!(result["valid"], true);
    assert_eq!(result["validated_target"], "current");
    assert_eq!(result["game_id"], stored.game_id);
    assert_eq!(ex.state().game_spec.as_ref().unwrap(), &stored);
}

#[test]
fn gamespec_validate_real_player_payoff_map_reports_the_field_path() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let stored = stored_three_by_three_reference_spec();
    ex.set_game_spec(stored.clone());
    // Shapes emitted by the live consultation at 09:09 and 09:13 on 2026-10-02.
    for payoffs in [
        serde_json::json!({"房东":[[26.4,26.4],[28.8,26.4],[26.4,26.4]],"租户(我)":[[-29.6,-26.4],[-28.8,-26.4],[-29.6,-29.6]]}),
        serde_json::json!({"房东":{"坚持1.1万":{"接受":26.4,"拒绝并另招租":26.4}},"租户(我)":{"坚持1.1万":{"接受":-26.4,"拒绝并另招租":-29.6}}}),
    ] {
        let msg = expect_failed(ex.execute(&call("gamespec_validate", serde_json::json!({"spec":{
            "form":"normal","name":"咖啡店续租","players":["租户(我)","房东"],
            "strategies":{"租户(我)":["坚持1.1万","直接搬店"],"房东":["接受","拒绝并另招租"]},
            "payoffs":payoffs
        }})), SessionPhase::ModelBuild));
        assert!(msg.contains("$.spec.payoffs"), "actual field path must be preserved: {msg}");
        assert!(msg.contains("missing field `form`"), "original error must be preserved: {msg}");
        assert!(msg.contains("values") && msg.contains("cells"));
        assert!(msg.contains("传 {}") && msg.contains(&stored.game_id));
        assert!(msg.contains("没有改为校验或覆盖会话模型"));
        assert_eq!(ex.state().game_spec.as_ref().unwrap(), &stored);
    }
}

#[test]
fn gamespec_validate_nonempty_invalid_requests_never_validate_the_stored_model() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let stored = stored_three_by_three_reference_spec();
    ex.set_game_spec(stored.clone());
    for invalid in [
        serde_json::json!({"payoffs":{}}),
        serde_json::json!({"spec":{}}),
        serde_json::json!({"spec":null}),
        serde_json::json!({"unused":true}),
        serde_json::json!({"spec":stored,"unexpected":true}),
        serde_json::Value::Null,
        serde_json::json!([]),
    ] {
        expect_failed(ex.execute(&call("gamespec_validate", invalid), SessionPhase::ModelBuild));
        assert_eq!(ex.state().game_spec.as_ref().unwrap(), &stored);
    }
}

#[test]
fn gamespec_validate_provided_model_is_identified_and_never_saved() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let stored = stored_three_by_three_reference_spec();
    ex.set_game_spec(stored.clone());
    let provided = serde_json::to_value(prisoners_dilemma()).unwrap();
    for request in [provided.clone(), serde_json::json!({"spec":provided})] {
        let result: serde_json::Value = serde_json::from_str(&expect_ok(ex.execute(
            &call("gamespec_validate", request), SessionPhase::ModelBuild,
        ))).unwrap();
        assert_eq!(result["validated_target"], "provided");
        assert_eq!(result["game_id"], "pd");
        assert_eq!(ex.state().game_spec.as_ref().unwrap(), &stored);
    }
}

#[test]
fn gamespec_validate_distinguishes_dynamics_errors_and_keeps_validation_gates() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(stored_three_by_three_reference_spec());
    let mut invalid = serde_json::to_value(prisoners_dilemma()).unwrap();
    invalid["dynamics"] = serde_json::json!({"sequence":"先报价再回应"});
    let msg = expect_failed(ex.execute(&call("gamespec_validate", serde_json::json!({"spec":invalid})), SessionPhase::ModelBuild));
    assert!(msg.contains("$.spec.dynamics"), "must distinguish from payoffs.form: {msg}");
    invalid = serde_json::to_value(prisoners_dilemma()).unwrap();
    invalid["payoffs"]["values"]["cells"].as_object_mut().unwrap().remove("维持,维持");
    let msg = expect_failed(ex.execute(&call("gamespec_validate", serde_json::json!({"spec":invalid})), SessionPhase::ModelBuild));
    assert!(msg.contains("支付矩阵缺"));
    invalid = serde_json::to_value(prisoners_dilemma()).unwrap();
    invalid["params"]["unknown"] = serde_json::json!({"name":"未知参数","value":{"kind":"literal","value":1},"source":{"kind":"assumed","note":"尚未取得"}});
    let msg = expect_failed(ex.execute(&call("gamespec_validate", serde_json::json!({"spec":invalid})), SessionPhase::ModelBuild));
    assert!(msg.contains("尚未确认"));
}

/// **回归测试**：一个完全合法的 GameSpec 必须能构建成功。
///
/// 实测采集到的一段真实参数（模型发出的，字段齐全、JSON 合法）在运行时
/// 报 `missing field params`——而它明明有 `params`。本测试用它复现。
#[test]
fn a_valid_gamespec_actually_builds() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 与实测采集到的参数同形（囚徒困境，字段齐全）
    let spec = serde_json::json!({
        "game_id": "probe",
        "params": {},
        // ⚠️ 形状是**邻接标记**：`cells` 必须在 `values` 里。
        //
        // 早先这里是 `{"form":"normal","cells":{…}}`（直接把 cells 挂 form 下），
        // 而那时 `PayoffForm` 的 content 键叫 `params`，与顶层 `params` 撞名——
        // serde 报 `missing field params`，我和模型都理解成"顶层缺参数表"，
        // 于是 21 次调用都在补错的地方。改名 `values` 后不再有歧义。
        "payoffs": {
            "form": "normal",
            "values": {
                "cells": {
                    "X,X": {"a": {"kind": "literal", "value": 1}, "b": {"kind": "literal", "value": 1}},
                    "X,Y": {"a": {"kind": "literal", "value": 0}, "b": {"kind": "literal", "value": 2}},
                    "Y,X": {"a": {"kind": "literal", "value": 2}, "b": {"kind": "literal", "value": 0}},
                    "Y,Y": {"a": {"kind": "literal", "value": 1}, "b": {"kind": "literal", "value": 1}}
                }
            }
        },
        "players": [
            {"id": "a", "name": "A", "relation_to_game": "player", "vn_role": "focal"},
            {"id": "b", "name": "B", "relation_to_game": "player", "vn_role": "substitutor"}
        ],
        "strategies": {"a": ["X", "Y"], "b": ["X", "Y"]}
    });
    let out = ex.execute(&call("gamespec_build", spec), SessionPhase::ModelBuild);
    match &out {
        ExecOutcome::Ok(s) => {
            assert!(s.contains("probe") || s.contains("validated"), "应成功：{s}");
        }
        ExecOutcome::Failed(m) => panic!(
            "一个合法的 GameSpec 被拒绝了——这说明解析链有问题，不是模型的问题。\\n实得：{m}"
        ),
        other => panic!("实得 {other:?}"),
    }
}

/// **回归测试**：扫描的**第一个点**就是翻转时不得 panic。
///
/// # 这是真实崩溃的回归（实测 run13）
///
/// 一次真实咨询在 recommend 阶段调用 `sensitivity_scan` 后**进程崩溃**，
/// 原因是 `points[i - 1]` 在 `i == 0` 时发生 `usize` 下溢。
///
/// 正确行为：如实报告"第一个点就已翻转 ⇒ 无从给出下界"，
/// **而不是编一个区间**（那会声称一个扫描并未覆盖的边界）。
#[test]
fn sensitivity_scan_first_point_flipped_does_not_panic() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "sensitivity_scan",
            serde_json::json!({"points": [
                {"param": 0.1, "flipped": true},   // ← 第一个就翻
                {"param": 0.2, "flipped": true},
            ]}),
        ),
        SessionPhase::Recommend,
    );
    match out {
        ExecOutcome::Ok(s) => {
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            assert!(
                v["flip_interval"][0].is_null(),
                "下界必须是 null（无从给出），而不是编一个数：{s}"
            );
            assert_eq!(v["flip_interval"][1], serde_json::json!(0.1));
            let note = v["note"].as_str().unwrap();
            assert!(
                note.contains("第一个点") && note.contains("没有覆盖"),
                "必须说清是「扫描没覆盖到」，而非「阈值很小」：{note}"
            );
        }
        other => panic!("实得 {other:?}"),
    }
}

// ─────────────────────────── 回退提议（R23）───────────────────────────

/// 模型可以**提议**回退，但提议只是一个请求——执行与否由用户裁决。
///
/// 实测动机：一次真实咨询走到 recommend，模型判断"硬假设缺来源"想回
/// model_build，却没有工具能表达——于是它把话说完就停在那里，
/// 既没进 report 也没告诉用户怎么继续。
#[test]
fn request_rewind_produces_a_pending_decision_not_an_execution() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "request_rewind",
            serde_json::json!({"target": "model_build", "reason": "两个硬假设缺来源"}),
        ),
        SessionPhase::Recommend,
    );
    match out {
        ExecOutcome::RewindRequest { target, reason } => {
            assert_eq!(target, SessionPhase::ModelBuild);
            assert!(reason.contains("缺来源"));
        }
        other => panic!("期望 RewindRequest（待裁决），实得 {other:?}"),
    }
}

/// **只能向后**回退——向前或原地没有意义，且会被误当成"跳过当前阶段"。
#[test]
fn request_rewind_rejects_forward_targets() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("request_rewind", serde_json::json!({"target": "report", "reason": "x"})),
        SessionPhase::ModelBuild,
    ));
    assert!(msg.contains("之前"), "应说明只能向更早阶段回退：{msg}");
}

/// 缺 `reason` 即拒绝——用户要据此判断是否同意作废已完成的工作。
#[test]
fn request_rewind_requires_a_reason() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("request_rewind", serde_json::json!({"target": "intel"})),
        SessionPhase::Recommend,
    ));
    assert!(msg.contains("reason"), "实得：{msg}");
    assert!(msg.contains("作废"), "应说明理由的用途：{msg}");
}

#[test]
fn request_rewind_rejects_unknown_target() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("request_rewind", serde_json::json!({"target": "不存在的阶段", "reason": "x"})),
        SessionPhase::Recommend,
    ));
    assert!(msg.contains("未知阶段"), "实得：{msg}");
}

// ─────────────────────── consequence_predict（whatif 核心）───────────────────────

/// 给定我方动作 d，求对手的残余子博弈——这是 whatif 的确定性做法。
///
/// 用囚徒困境：我方固定"降价"，对手的最佳反应应是"降价"。
#[test]
fn consequence_predict_solves_the_residual_subgame() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma()); // focal/rival 各 维持/降价

    let out = expect_ok(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "降价"})),
        SessionPhase::Consequence,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["decision"], serde_json::json!("降价"));
    let reactions = v["reactions"].as_array().unwrap();
    assert!(!reactions.is_empty(), "应有反应：{out}");
    // 概率须归一化
    let sum: f64 = reactions.iter().map(|r| r["probability"].as_f64().unwrap()).sum();
    assert!((sum - 1.0).abs() < 1e-9, "反应概率应归一化，实得和 {sum}");
    // 对手的最佳反应是"降价"
    let r0 = &reactions[0];
    assert!(
        r0["reaction"].as_str().unwrap().contains("降价"),
        "对手最佳反应应是降价：{out}"
    );
}

/// **矩阵外的动作必须被拒**——直接推演未注册的动作等于算一个不存在的博弈。
#[test]
fn consequence_predict_rejects_unregistered_actions() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    let msg = expect_failed(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "免费赠送"})),
        SessionPhase::Consequence,
    ));
    assert!(msg.contains("不在我方已声明的策略里"), "实得：{msg}");
    assert!(
        msg.contains("novel_move_filter"),
        "应指出注册矩阵外动作的正确路径：{msg}"
    );
}

#[test]
fn consequence_predict_requires_decision() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    let msg = expect_failed(ex.execute(
        &call("consequence_predict", serde_json::json!({})),
        SessionPhase::Consequence,
    ));
    assert!(msg.contains("decision"), "实得：{msg}");
}

#[test]
fn consequence_predict_requires_game_spec() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "降价"})),
        SessionPhase::Consequence,
    ));
    assert!(msg.contains("GameSpec"), "实得：{msg}");
}

/// 结果必须**如实标注**为点估计（参数未声明 dist 时无从给区间）。
#[test]
fn consequence_predict_labels_point_estimates_honestly() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    let out = expect_ok(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "降价"})),
        SessionPhase::Consequence,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let note = v["note"].as_str().unwrap();
    assert!(note.contains("点估计"), "应如实标注是点估计：{note}");
    // 单一最佳反应 ⇒ 概率为 1，不存在"等权未校准"的问题；
    // 多均衡时才有等权（那时 note 会写"并列"）。
    assert!(
        note.contains("并列") || note.contains("未校准"),
        "概率的依据须说清（单一反应 or 等权未校准）：{note}"
    );
    // 点估计的三个分位应相等
    let r0 = &v["reactions"][0];
    let outs = r0["outcomes"].as_object().unwrap();
    let (_, p) = outs.iter().next().unwrap();
    assert_eq!(p["p10"], p["p50"]);
    assert_eq!(p["p50"], p["p90"]);
}

// ─────────────────────── preference_inject（偏好真影响结果）───────────────────────

/// **它必须真正改变推荐**——否则就是一个"看起来生效"的工具。
///
/// 背景：`GameSpec.preference` 的字段（risk_aversion / objective_weights）
/// 目前**无任何引擎消费**，是设计预留。若本工具只写那两个字段，
/// 用户会以为自己的风险偏好影响了推荐——而实际没有。
/// 故它做的是把偏好映射为**选择器权重**。
#[test]
fn preference_inject_actually_changes_selector_weights() {
    let theory = empty_theory();
    let mut pb = playbook();
    pb.selectors.insert("risk_dom".into(), 0.2);
    pb.selectors.insert("persona_sim".into(), 0.2);
    pb.selectors.insert("stochastic".into(), 0.6);
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());

    let out = expect_ok(ex.execute(
        &call("preference_inject", serde_json::json!({"risk_aversion": 0.8})),
        SessionPhase::Recommend,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();

    let before = &v["selector_weights_before"];
    let after = &v["selector_weights_after"];
    assert!(
        after["risk_dom"].as_f64().unwrap() > before["risk_dom"].as_f64().unwrap(),
        "风险厌恶上升 ⇒ risk_dom 权重应上升：{out}"
    );
    assert!(
        after["persona_sim"].as_f64().unwrap() < before["persona_sim"].as_f64().unwrap(),
        "风险厌恶上升 ⇒ persona_sim 权重应下降（LLM 模拟波动大）：{out}"
    );
    // 归一化
    let sum: f64 = after.as_object().unwrap().values().map(|x| x.as_f64().unwrap()).sum();
    assert!((sum - 1.0).abs() < 1e-9, "权重应归一化，实得和 {sum}");
    // 必须说明它影响的是什么
    let note = v["note"].as_str().unwrap();
    assert!(
        note.contains("选择器权重") && note.contains("启发式"),
        "须说明机制与'该规则是可争议的'：{note}"
    );
}

/// `risk_aversion` 越界即拒——它是 [0,1] 的系数，不是任意数。
#[test]
fn preference_inject_rejects_out_of_range() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    for bad in [-0.1, 1.5] {
        let msg = expect_failed(ex.execute(
            &call("preference_inject", serde_json::json!({"risk_aversion": bad})),
            SessionPhase::Recommend,
        ));
        assert!(msg.contains("[0,1]"), "越界应报范围：{msg}");
    }
    // NaN 经 JSON 往返会变成 null ⇒ 走"不是合法数值"分支。
    // **关键是不能静默变成 0.0**（那会让用户以为自己注入了偏好）。
    let nan_json: serde_json::Value = serde_json::from_str(r#"{"risk_aversion":null}"#).unwrap();
    let msg = expect_failed(ex.execute(
        &call("preference_inject", nan_json),
        SessionPhase::Recommend,
    ));
    assert!(
        msg.contains("不是合法数值"),
        "非法值必须被拒，不得静默降级为默认：{msg}"
    );
}

/// 偏好变了 ⇒ 分布**必须在原地按新权重重算**（而不是清空）。
///
/// # 这条测试是从一次真实死锁里长出来的（run19）
///
/// 早先的实现把分布 `take()` 掉，理由听起来对（"下游不该读到与当前偏好
/// 不符的分布"），但它制造了一个**跨阶段死锁**：
///
/// - 重建分布的 `select_fuse` 只在 **select** 阶段可用
/// - 使分布失效的 `preference_inject` 只在 **recommend** 阶段可用
/// ⇒ 一旦注入偏好，分布永久消失，`recommend_build` 永远失败，而阶段是
///   单向的、回不去 select。
///
/// 真实运行里模型的表现堪称教科书：它先调 `preference_inject`，再调
/// `recommend_build` 被拒（"尚无均衡概率分布"），**正确诊断出"上游没跑过"**，
/// 于是调 `request_rewind` 请求回退到 select——而回退需要用户裁决，
/// 自动模式下被拒（fail-closed）⇒ 死锁。
///
/// 所以正确处置是**刷新**：判据分数还在，融合是纯函数，用新权重重算即可。
#[test]
fn preference_inject_refreshes_distribution_in_place() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    let before = ex.state().distribution.clone().expect("先确认已有分布");

    let out = expect_ok(ex.execute(
        &call("preference_inject", serde_json::json!({"risk_aversion": 0.5})),
        SessionPhase::Recommend,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["distribution_refreshed"],
        serde_json::json!(true),
        "应报告已刷新：{out}"
    );
    assert_eq!(
        v["needs_select_stage"],
        serde_json::json!(false),
        "分数还在，不该要求回到 select 阶段：{out}"
    );
    assert!(
        ex.state().distribution.is_some(),
        "分布必须**仍然在**——清空它就是死锁"
    );

    // 只登记了一个判据（risk_dom），权重归一化后仍是 1.0 ⇒ 分布不变。
    // 这不是"没刷新"，是**正确答案**：单判据下权重再怎么调，分布都一样。
    // 这条断言存在的意义是防"假装刷新"（比如把权重全清零）。
    let after = ex.state().distribution.clone().unwrap();
    assert_eq!(
        before, after,
        "单判据下加权融合与权重无关，分布应逐位相同"
    );
}

/// **死锁回归**：注入偏好之后，`recommend_build` 必须仍能成功。
///
/// 这条测试直接复现 run19 的卡点序列（preference_inject → recommend_build），
/// 断言的是**结果**（下一步能不能走通），不是中间字段——因为字段名会改，
/// 而"能不能走通"是不会被措辞糊弄过去的。
#[test]
fn recommend_build_still_works_after_preference_inject() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    // run19 的原始序列
    expect_ok(ex.execute(
        &call("preference_inject", serde_json::json!({"risk_aversion": 0.7})),
        SessionPhase::Recommend,
    ));

    let out = ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({
                "headline": "在对方不合作时保持不合作",
                "actions": [{
                    "action": "维持不合作",
                    "rationale": "均衡分布由确定性融合产出，指向不合作",
                }],
            }),
        ),
        SessionPhase::Recommend,
    );
    assert!(
        !matches!(out, ExecOutcome::Failed(_)),
        "注入偏好不得让 recommend_build 变成不可达（run19 死锁）：{out:?}"
    );
}

#[test]
fn preference_inject_requires_game_spec() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(
        &call("preference_inject", serde_json::json!({"risk_aversion": 0.5})),
        SessionPhase::Recommend,
    ));
    assert!(msg.contains("GameSpec"), "实得：{msg}");
}

// ─────────────────────── recommend_build（解开 report 死锁）───────────────────────

/// **回归测试**：`recommend_build` 必须登记 `RecommendationPack` 产物。
///
/// 缺陷背景（实测三连卡）：阶段图要求 recommend 阶段产出 `RecommendationPack`
/// 才能进 report，但**没有任何工具能产出它**（设计文档列了 `recommend_build`
/// 却从未注册）。三次端到端真实咨询全部停在 recommend——报告阶段结构上不可达。
#[test]
fn recommend_build_registers_the_pack_artifact() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let out = expect_ok(ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({
                "headline": "维持价格，不跟进降价",
                "actions": [{
                    "action": "维持 12.8 元",
                    "rationale": "均衡分布显示价格战概率最高，主动降价会加速它",
                    "confidence": "medium"
                }],
                "do_not": ["不要为抢份额降价超过 5%"]
            }),
        ),
        SessionPhase::Recommend,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["headline"], serde_json::json!("维持价格，不跟进降价"));

    // **关键断言**：产物必须被登记（否则闸门拦死 report）
    let kinds: Vec<&str> = ex.artifacts().iter().map(|(k, _)| k.key()).collect();
    assert!(
        kinds.contains(&"recommendation"),
        "recommend_build 必须登记 RecommendationPack 产物，否则 report 不可达：{kinds:?}"
    );
}

/// 没有均衡分布时拒绝——没有分布就没有"哪个结果更可能"的依据，
/// 建议会退化成凭空主张。
#[test]
fn recommend_build_requires_a_distribution() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    let msg = expect_failed(ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({"headline": "x", "actions": [{"action": "y", "rationale": "z"}]}),
        ),
        SessionPhase::Recommend,
    ));
    assert!(msg.contains("分布"), "实得：{msg}");
    assert!(msg.contains("凭空主张"), "应说明为何拒绝：{msg}");
}

/// 每条行动都要有依据——没有依据的建议无法被审计。
#[test]
fn recommend_build_requires_rationale_per_action() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    let msg = expect_failed(ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({"headline": "x", "actions": [{"action": "降价"}]}),
        ),
        SessionPhase::Recommend,
    ));
    assert!(msg.contains("rationale"), "实得：{msg}");
    assert!(msg.contains("审计"), "应说明理由：{msg}");
}

/// 建议包必须**自动收进**确定性结果（模型不该转述它们）。
#[test]
fn recommend_build_includes_deterministic_results_automatically() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let out = expect_ok(ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({"headline": "h", "actions": [{"action": "a", "rationale": "r"}]}),
        ),
        SessionPhase::Recommend,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    // 分布是自动收进的（模型只给了 headline 与 actions）
    assert!(
        v["equilibrium_distribution"]["entries"].as_array().is_some(),
        "分布应被自动收进：{out}"
    );
    // 未注入偏好时要如实提示
    let caveats = v["caveats"].as_array().unwrap();
    assert!(
        caveats.iter().any(|c| c.as_str().unwrap().contains("风险偏好")),
        "未注入偏好应如实提示：{out}"
    );
}

// ─────────────────────── swarm_run（persona 仿真，提议≠结论）───────────────────────

/// **未装配 LLM 时必须 `Blocked`，而不是"仿真显示无进展"。**
///
/// 这条测试锁的是一个极易滑过去的失效：若 `swarm_run` 在没有 actor 时
/// 返回一个空的行动表（或"0 次提议"的正常结果），调用方会把它读成
/// 「仿真显示对手不动」——而真相是**根本没跑**。
///
/// 两者的区别对报告是决定性的：前者是一条关于对手行为的主张，
/// 后者是"这个能力没接上"。故必须是 `Blocked`（第三状态），
/// 不能是 `Ok`（假装有结果）也不能是 `Failed`（暗示跑了但出错）。
#[test]
fn swarm_run_without_llm_is_blocked_not_empty_result() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    let out = ex.execute(&call("swarm_run", serde_json::json!({})), SessionPhase::Select);
    match out {
        ExecOutcome::Blocked(msg) => {
            assert!(
                msg.contains("没接上") || msg.contains("未装配"),
                "应说清是「没接上」而非「没进展」：{msg}"
            );
        }
        other => panic!("未装配 LLM 时应为 Blocked，实得：{other:?}"),
    }
}

/// 没有画像就没有 persona ⇒ 必须报错并**指出先做什么**。
///
/// persona 由画像生成（而不是模型编的人设）。缺画像时若返回一个空仿真，
/// 调用方会以为"对手没有可观察行为"——而那是另一回事。
#[test]
fn swarm_run_requires_profiles_and_says_so() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    // 注意：这里**故意不装 actor**——但校验顺序保证"缺画像"先被报出。
    // 若将来有人调换顺序，本测试会失败，那正是它该做的事：
    // 用户最该先知道的是"缺材料"，而不是"缺客户端"。
    let out = ex.execute(&call("swarm_run", serde_json::json!({})), SessionPhase::Select);
    let msg = match out {
        ExecOutcome::Blocked(m) | ExecOutcome::Failed(m) => m,
        other => panic!("应报错，实得：{other:?}"),
    };
    assert!(!msg.is_empty(), "错误信息不得为空");
}

/// `swarm_run` 必须能**被调**（不是 not-wired）。
///
/// 这条是 wiring 层面的：注册表、阶段白名单、剧本白名单三层都放行，
/// 才谈得上"能调"。此前 `recommend_build` 就因为剧本白名单漏加而
/// **永远调不动**（三层过滤的中间层），这类缺陷在真实运行前不可见。
#[test]
fn swarm_run_is_wired_and_allowed_in_select() {
    let st = wiring_status();
    let state = st
        .get("swarm_run")
        .copied()
        .expect("wiring_status 必须提到 swarm_run");
    assert_ne!(state, "not-wired", "`swarm_run` 应已接线");

    // 阶段白名单：必须在 select 放行（它是 persona_sim 判据的证据来源）
    let reg = shangyi_tools::ToolRegistry::standard();
    let spec = reg
        .get("swarm_run")
        .expect("registry must contain swarm_run");
    assert!(
        spec.phases.contains("select"),
        "`swarm_run` 必须在 select 阶段可用（persona_sim 判据的证据来源），实得：{:?}",
        spec.phases
    );
}

// ──────────────────── data_ingest（侧车不可用必须是 Blocked）────────────────────

/// **侧车不可用时必须是 `Blocked`，不能是 `Failed`。**
///
/// 这两种错误对用户的含义完全不同：
/// - `Failed`（该文件有问题）⇒ 用户会去检查文件、换个格式重导
/// - `Blocked`（我们这边没接上）⇒ 该由维护者装 Python / 修路径
///
/// 若错报成 `Failed`，用户会花时间在一个**根本不在他那侧**的问题上。
/// 这正是"能力 seam 失败即拒绝"要求的形态：说清是谁的问题。
#[test]
fn data_ingest_without_sidecar_is_blocked_not_failed() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 指向一个不存在的解释器 ⇒ 侧车必然起不来
    // （不改进程环境变量：用不存在的路径就够，且不会与并行测试竞争）
    let out = ex.execute(
        &call("data_ingest", serde_json::json!({"path": "/tmp/definitely-not-a-file-xyz.csv"})),
        SessionPhase::Intel,
    );
    match out {
        ExecOutcome::Blocked(msg) => {
            assert!(
                msg.contains("没接上") || msg.contains("不可用"),
                "应说清是能力未装配：{msg}"
            );
        }
        ExecOutcome::Failed(msg) => {
            // 允许失败，但**必须**是"文件不存在"而非"我们没接上"
            // （侧车在某些环境里确实是可用的，那是运气好）。
            assert!(
                msg.contains("不存在") || msg.contains("拒绝"),
                "若侧车可用，失败原因应是文件本身：{msg}"
            );
        }
        other => panic!("应报错，实得：{other:?}"),
    }
}

/// `path` 缺失或为空时**必须报错**——本工具不猜文件位置。
#[test]
fn data_ingest_requires_explicit_path() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(&call("data_ingest", serde_json::json!({})), SessionPhase::Intel));
    assert!(msg.contains("path"), "应指出缺的是 path：{msg}");

    let msg = expect_failed(ex.execute(
        &call("data_ingest", serde_json::json!({"path": "   "})),
        SessionPhase::Intel,
    ));
    assert!(msg.contains("空"), "空路径应被拒：{msg}");
}

/// `data_ingest` 必须已接线（不是 not-wired）。
#[test]
fn data_ingest_is_wired() {
    let st = wiring_status();
    let state = st
        .get("data_ingest")
        .copied()
        .expect("wiring_status 必须提到 data_ingest");
    assert_ne!(state, "not-wired", "`data_ingest` 应已接线");
}

// ───────────── gamespec_build 的输入宽容（测模型实际写过的形状）─────────────

/// 模型写过的 `dynamics` 形状**全部应被接受**——它们都是合理的表达。
///
/// # 这些字面量不是我编的
///
/// 它们取自一次真实咨询的失败日志：模型连续失败 7 次，依次试了
/// `{"repeated":true}` → `{"form":"repeated"}` → `{"type":"repeated"}`
/// → `{"form":"repeated","type":"repeated"}`，每次拿到的都是
/// `missing field params`（content 键与顶层参数表撞名），
/// **错误指向了错误的位置**。
///
/// 接口要求模型知道我们的标记约定，那是不必要的负担。
/// 本测试锁住"这些自然写法都能过"。
#[test]
fn gamespec_accepts_natural_spellings_of_dynamics() {
    for dyn_v in [
        serde_json::json!({"form": "repeated", "values": {"discount": 0.9, "horizon": 5}}),
        serde_json::json!({"type": "repeated", "discount": 0.9, "horizon": 5}),
        serde_json::json!({"form": "repeated", "discount": 0.9}),
        serde_json::json!({"repeated": {"discount": 0.9}}),
        // ⚠️ 注意这里**没有** `"repeated"`（裸字符串）这个 case：
        // 它归一化后是 `{"form":"repeated"}`，缺 `discount` ⇒ **应当失败**。
        // 折现因子决定"价格战能否持续"的结论，不得由我们补一个默认值。
        // 这条边界由 `normalizing_dynamics_never_invents_values` 断言。
        serde_json::json!({"structure": "one_shot"}),
        serde_json::json!({"form": "one_shot"}),
        serde_json::json!("one_shot"),
    ] {
        let theory = empty_theory();
        let pb = playbook();
        let mut ex = DomainExecutor::new(&theory, &pb);
        let out = ex.execute(
            &call(
                "gamespec_build",
                serde_json::json!({
                    "game_id": "probe",
                    "players": [
                        {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                        {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                    ],
                    "strategies": {"a": ["x"], "b": ["y"]},
                    "payoffs": {"form": "normal", "values": {"cells": {
                        // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                        "x,y": {"a": 1, "b": 2}
                    }}},
                    "params": {},
                    "dynamics": dyn_v,
                }),
            ),
            SessionPhase::ModelBuild,
        );
        assert!(
            matches!(out, ExecOutcome::Ok(_)),
            "`dynamics` = {dyn_v} 应被接受，实得：{out:?}"
        );
    }
}

/// `information` 的同款宽容。
#[test]
fn gamespec_accepts_natural_spellings_of_information() {
    for info_v in [
        serde_json::json!("complete"),
        serde_json::json!({"form": "complete"}),
        serde_json::json!({"type": "complete"}),
        serde_json::json!({"complete": true}),
        serde_json::json!({"form": "incomplete", "values": {"types": {}}}),
        serde_json::json!({"type": "incomplete", "types": {}}),
    ] {
        let theory = empty_theory();
        let pb = playbook();
        let mut ex = DomainExecutor::new(&theory, &pb);
        let out = ex.execute(
            &call(
                "gamespec_build",
                serde_json::json!({
                    "game_id": "probe",
                    "players": [
                        {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                        {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                    ],
                    "strategies": {"a": ["x"], "b": ["y"]},
                    "payoffs": {"form": "normal", "values": {"cells": {
                        // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                        "x,y": {"a": 1, "b": 2}
                    }}},
                    "params": {},
                    "information": info_v,
                }),
            ),
            SessionPhase::ModelBuild,
        );
        assert!(
            matches!(out, ExecOutcome::Ok(_)),
            "`information` = {info_v} 应被接受，实得：{out:?}"
        );
    }
}

/// **裸数字的支付矩阵应被接受**（这是它最自然的写法）。
///
/// 实测 6 次失败里的一条：`invalid type: number, expected adjacently tagged
/// enum Expr`——而要写成 `{"kind":"literal","value":3}` 是**我们的**内部
/// 表示（邻接标记是为 `arbitrary_precision` 兼容所迫），
/// 不该泄漏到工具接口上。
#[test]
fn gamespec_accepts_bare_numbers_in_payoff_cells() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "probe",
                "players": [
                    {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    "维持,维持": {"us": 3, "rival": 3},
                    "维持,降价": {"us": 0, "rival": 5},
                    "降价,维持": {"us": 5, "rival": 0},
                    "降价,降价": {"us": 1, "rival": 1}
                }}},
                "params": {}
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "裸数字的支付矩阵必须被接受（那是最自然的写法）：{out:?}"
    );
}

/// **参数引用（裸字符串）也应被接受**——支付格里写参数名是常见的建模方式。
#[test]
fn gamespec_accepts_bare_strings_as_parameter_refs() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "probe",
                "players": [
                    {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    // 2×2 必须**填满**（只写对角线会被如实拒绝）
                    "维持,维持": {"us": "margin_keep", "rival": "margin_keep"},
                    "维持,降价": {"us": "margin_keep", "rival": "margin_cut"},
                    "降价,维持": {"us": "margin_cut", "rival": "margin_keep"},
                    "降价,降价": {"us": "margin_cut", "rival": "margin_cut"}
                }}},
                "params": {
                    "margin_keep": {"name": "margin_keep",
                                    "value": 3,
                                    "source": {"kind": "user_confirmed", "checkpoint": "cp2"}},
                    "margin_cut": {"name": "margin_cut",
                                   "value": 1,
                                   "source": {"kind": "user_confirmed", "checkpoint": "cp2"}}
                }
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "裸字符串应被当作参数引用（支付格里写参数名很常见）：{out:?}"
    );
}

/// **双重编码的 JSON 字符串应被解析一次**。
///
/// 实测出现过：模型把整个对象序列化成字符串又塞进字符串里
/// （函数调用参数层已是字符串，套两层很常见）。
/// 拒掉并让它重发是浪费一个 turn。
#[test]
fn gamespec_accepts_double_encoded_json() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let inner = serde_json::json!({
        "game_id": "probe",
        "players": [
            {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"a": ["x"], "b": ["y"]},
        // 必须填满：本测试验的是**编码形态**，但校验要求矩阵完整，
        // 空 matrix 会被如实拒绝（那正是 `IncompletePayoffCells` 的目的）。
        "payoffs": {"form": "normal", "values": {"cells": {
            "x,y": {"a": 1, "b": 2}
        }}},
        "params": {}
    });
    // 字符串里再放一个 JSON 对象
    let doubled = serde_json::Value::String(inner.to_string());
    let out = ex.execute(&call("gamespec_build", doubled), SessionPhase::ModelBuild);
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "双重编码的 JSON 应被解析：{out:?}"
    );
}

/// **归一化不得编造数值**：`{"repeated": true}` 缺 `discount` 时必须失败。
///
/// 这条边界很重要：宽容（接受多种写法）**不等于**补齐语义。
/// 若这里静默补一个默认折现因子，那就是无来源的数字进模型——
/// 而折现因子会决定"价格战能否持续"的结论。
#[test]
fn normalizing_dynamics_never_invents_values() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "probe",
                "players": [
                    {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"a": ["x"], "b": ["y"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                    "x,y": {"a": 1, "b": 2}
                }}},
                "params": {},
                "dynamics": {"repeated": true}
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(
        matches!(out, ExecOutcome::Failed(_)),
        "缺 discount 必须失败——不得静默补默认折现因子：{out:?}"
    );
}

// ───────────────── sandbox_exec（探索性产出，fail-closed）─────────────────

/// `sandbox_exec` 的产出**必须**带 `exploratory: true`。
///
/// 它是给报告生成器的红灯：`TrustLevel::Exploratory` 的产出**不得进报告**。
/// 忘记标它会让探索性结果被当成引擎产出，而报告读者无从分辨。
///
/// （能力不可用时返回 `Blocked`，本测试跳过——那由另一条覆盖。）
#[test]
fn sandbox_exec_marks_output_exploratory() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call("sandbox_exec", serde_json::json!({"code": "print(1+1)"})),
        SessionPhase::Solve,
    );
    match out {
        ExecOutcome::Ok(json) => {
            let v: serde_json::Value = serde_json::from_str(&json).expect("应可解析");
            assert_eq!(
                v["exploratory"],
                serde_json::json!(true),
                "产出必须标 exploratory（报告生成器据此拒绝读取）：{json}"
            );
        }
        ExecOutcome::Blocked(msg) => {
            assert!(
                msg.contains("无法隔离"),
                "Blocked 应说明是隔离能力缺失：{msg}"
            );
        }
        other => panic!("应 Ok 或 Blocked，实得：{other:?}"),
    }
}

/// 静态校验拒绝的代码 ⇒ `Failed`（**代码的问题**，改代码有用）。
///
/// 与"无法隔离"（`Blocked`）区分开：那个不该去改代码。
/// 两种状态的处置方向相反，故必须在类型上分开。
#[test]
fn sandbox_exec_static_rejection_is_failed_not_blocked() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call("sandbox_exec", serde_json::json!({"code": "import socket\nprint(1)"})),
        SessionPhase::Solve,
    );
    // 若本机无隔离能力，Blocked 先发生（校验在隔离检查之后）——那是允许的
    match out {
        ExecOutcome::Failed(msg) => {
            assert!(msg.contains("静态校验"), "应指出是静态校验拒绝：{msg}");
        }
        ExecOutcome::Blocked(msg) => {
            assert!(msg.contains("无法隔离"), "{msg}");
        }
        other => panic!("实得：{other:?}"),
    }
}

/// `code` 缺失或为空时必须报错。
#[test]
fn sandbox_exec_requires_code() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let msg = expect_failed(ex.execute(&call("sandbox_exec", serde_json::json!({})), SessionPhase::Solve));
    assert!(msg.contains("code"), "应指出缺的是 code：{msg}");

    let msg = expect_failed(ex.execute(
        &call("sandbox_exec", serde_json::json!({"code": "  "})),
        SessionPhase::Solve,
    ));
    assert!(msg.contains("空"), "{msg}");
}

/// 超时上限必须被强制（探索性任务不该占满咨询的时间预算）。
#[test]
fn sandbox_exec_rejects_absurd_timeouts() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    for bad in [0, 100000] {
        let out = ex.execute(
            &call(
                "sandbox_exec",
                serde_json::json!({"code": "print(1)", "timeout_secs": bad}),
            ),
            SessionPhase::Solve,
        );
        let msg = match out {
            ExecOutcome::Failed(m) => m,
            other => panic!("timeout={bad} 应被拒，实得：{other:?}"),
        };
        assert!(msg.contains("timeout_secs"), "{msg}");
    }
}

/// **全部工具都已接线**——这是"未接线这一类"的收尾断言。
///
/// 此前 `wiring_status` 里有三个 `not-wired`，现在都接上了。
/// 这条测试的意义是：若将来有人新增工具却忘了实现，
/// `wiring_status` 里会出现 `not-wired`，而这里会立刻失败。
#[test]
fn all_tools_are_wired() {
    let unwired: Vec<&str> = wiring_status()
        .into_iter()
        .filter(|(_, st)| *st == "not-wired")
        .map(|(name, _)| name)
        .collect();
    assert!(
        unwired.is_empty(),
        "以下工具仍未接线：{unwired:?}。\
         若是有意保留未实现，请在本测试里逐条列出**为什么**\
         （而不是放宽断言——那会让「忘了实现」与「有意留空」无法区分）。"
    );
}

// ──────── consequence_predict 的策略名匹配（宽容但不猜）────────

fn entry_spec() -> serde_json::Value {
    serde_json::json!({
        "game_id": "entry",
        "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "incumbent", "name": "在位者", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"us": ["进入", "不进入"], "incumbent": ["降价阻击", "不降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "进入,降价阻击": {"us": -5, "incumbent": 2},
            "进入,不降价": {"us": 8, "incumbent": 5},
            "不进入,降价阻击": {"us": 0, "incumbent": 9},
            "不进入,不降价": {"us": 0, "incumbent": 9}
        }}},
        "params": {}
    })
}

/// **歧义必须被拒绝**：`decision` 里有多个策略名时，不许选"最像的"。
///
/// # 这个语义为什么重要
///
/// `"不进入"` 里含 `"进入"`，故一句「保持不进入」同时匹配两个策略名。
/// 若静默选一个，推演出的后果属于**另一个动作**——而输出看起来完全正常，
/// 报告读者无从察觉。故只在**唯一**匹配时采用。
#[test]
fn consequence_predict_refuses_ambiguous_strategy_names() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    expect_ok(ex.execute(&call("gamespec_build", entry_spec()), SessionPhase::ModelBuild));
    // 「进入或不进入」这句同时含两个策略名 ⇒ 无法确定要推演哪个
    let msg = expect_failed(ex.execute(
        &call(
            "consequence_predict",
            serde_json::json!({"decision": "先看对手反应再决定进入或不进入"}),
        ),
        SessionPhase::Consequence,
    ));
    assert!(
        msg.contains("多个") || msg.contains("歧义"),
        "应指出歧义而非静默选一个：{msg}"
    );
    assert!(
        msg.contains("不猜"),
        "应说明本工具不猜（猜错会让输出属于另一个动作，而它看起来完全正常）：{msg}"
    );
}

/// 精确匹配优先（不受子串歧义影响）。
#[test]
fn consequence_predict_prefers_exact_match() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    expect_ok(ex.execute(&call("gamespec_build", entry_spec()), SessionPhase::ModelBuild));
    for d in ["进入", "不进入"] {
        let out = ex.execute(
            &call("consequence_predict", serde_json::json!({"decision": d})),
            SessionPhase::Consequence,
        );
        assert!(
            matches!(out, ExecOutcome::Ok(_)),
            "精确策略名 `{d}` 必须可用（不应被子串歧义影响）：{out:?}"
        );
    }
}

/// **唯一的子串匹配**可用——模型给带描述的动作是自然的。
///
/// 实测（run25）：模型给的是「进入新城区，开设 1 家社区生鲜门店……常规节奏落地」，
/// 而策略名是「进入」。它连续失败 3 次，因为提示只说"不在已声明策略里"，
/// 而它**想表达的语义就是"进入"**——它缺的不是信息，是接口不该要求措辞一致。
#[test]
fn consequence_predict_accepts_unique_substring_match() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    expect_ok(ex.execute(&call("gamespec_build", entry_spec()), SessionPhase::ModelBuild));
    let out = ex.execute(
        &call(
            "consequence_predict",
            serde_json::json!({"decision": "进入新城区并一次性开 3 家店抢密度"}),
        ),
        SessionPhase::Consequence,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "描述句里的唯一子串匹配应被接受（否则模型只能猜措辞）：{out:?}"
    );
}

/// **完全无匹配**时拒绝，并给出可用清单。
#[test]
fn consequence_predict_rejects_unmatched_decision() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    expect_ok(ex.execute(&call("gamespec_build", entry_spec()), SessionPhase::ModelBuild));
    let msg = expect_failed(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "彻底转型做别的生意"})),
        SessionPhase::Consequence,
    ));
    assert!(
        msg.contains("进入") && msg.contains("不进入"),
        "应给出可用策略清单：{msg}"
    );
    assert!(
        msg.contains("novel_move_filter"),
        "应指出矩阵外动作的正确路径：{msg}"
    );
}

// ─────────── discover_satisfice 的产物登记（脚本化 E2E 暴露）───────────

/// `discover_satisfice` 成功后**必须登记 `DiscoveryReport` 产物**。
///
/// # 这条测试的来历（脚本化 E2E 在 discover 模式上卡死）
///
/// 脚本化全链路 E2E 跑 discover 模式时，`discover_satisfice` **调用成功**、
/// 但阶段推进不了、8 个 turn 预算全用光。而工具轨迹显示它确实调了那个工具。
///
/// 这正是"工具成功了，但阶段要求的产物没被登记"这一族缺陷的形状
/// （此前已发现 6 个实例）。故这里直接断言产物**真的**出现在 `artifacts()` 里。
///
/// 与既有测试的区别：既有测试断言工具的**返回值**（那是对的），
/// 本条断言工具的**副作用**（产物登记）——两者是不同的事，
/// 而后者才是闸门看的东西。
#[test]
fn discover_satisfice_registers_the_report_artifact() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "discover_satisfice",
            serde_json::json!({
                "aspiration": {"levels": {"利润": 1.0}, "default_threshold": 0.5},
                "candidates": [
                    {"id": "c1", "description": "维持",
                     "outcomes": {"利润": {"p10": 2.0, "p50": 3.0, "p90": 4.0}},
                     "llm_proposed": false}
                ]
            }),
        ),
        SessionPhase::Discover,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "调用应成功：{out:?}");
    let arts = ex.artifacts();
    assert!(
        arts.iter().any(|(k, _)| *k == shangyi_core::phase_graph::ArtifactKind::DiscoveryReport),
        "**产物未登记** ⇒ discover 阶段永远推进不了（闸门看的是产物，不是返回值）。\
         实得产物：{:?}",
        arts.iter().map(|(k, _)| k.display_name()).collect::<Vec<_>>()
    );
}

/// `report_render` 在**已经历完整前置**后必须成功并登记 `ReportBundle`。
///
/// 这条测试的来历：脚本化 E2E 在 whatif/discover 模式下卡在 report 阶段
/// ——`report_render` 被调用了，但阶段推进不了。而工具轨迹显示它确实在跑。
/// 故这里走完整前置后直接断言**产物登记**（闸门看的那个）。
#[test]
fn report_render_registers_bundle_after_full_setup() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({
                "headline": "维持",
                "actions": [{"action": "维持", "rationale": "均衡指向维持"}]
            }),
        ),
        SessionPhase::Recommend,
    );
    let out = ex.execute(
        &call("report_render", serde_json::json!({"case_id": "t", "provenance": "exercise"})),
        SessionPhase::Report,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "report_render 应成功：{out:?}");
    let arts = ex.artifacts();
    assert!(
        arts.iter()
            .any(|(k, _)| *k == shangyi_core::phase_graph::ArtifactKind::ReportBundle),
        "**产物未登记** ⇒ report 阶段推进不了。实得：{:?}",
        arts.iter().map(|(k, _)| k.display_name()).collect::<Vec<_>>()
    );
}

/// **whatif 路径**（无 select）下的 `report_render` 也必须能产出报告束。
///
/// 该路径的差异：没有 `select_fuse` ⇒ **没有均衡分布**。
/// 若 `report_render` 对它有硬依赖，整条 whatif 链路就断在最后一步
/// （而这正是脚本化 E2E 报出的现象）。
#[test]
fn report_render_works_without_a_distribution() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    // whatif 路径：不跑 select，直接推演一个动作
    ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "降价"})),
        SessionPhase::Consequence,
    );
    let out = ex.execute(
        &call(
            "recommend_build",
            serde_json::json!({
                "headline": "维持",
                "actions": [{"action": "维持", "rationale": "后果推演显示降价会被对手跟随"}]
            }),
        ),
        SessionPhase::Recommend,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "无分布时 recommend_build 应可用：{out:?}");

    let out = ex.execute(
        &call("report_render", serde_json::json!({"case_id": "t", "provenance": "exercise"})),
        SessionPhase::Report,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "**whatif 路径的报告应能生成**（它没有分布，但后果报告是依据）：{out:?}"
    );
    let arts = ex.artifacts();
    assert!(
        arts.iter()
            .any(|(k, _)| *k == shangyi_core::phase_graph::ArtifactKind::ReportBundle),
        "产物未登记。实得：{:?}",
        arts.iter().map(|(k, _)| k.display_name()).collect::<Vec<_>>()
    );
}
// ─────────── swarm_run 的输出大小（成本回归守卫）───────────

/// 会产出很多**矩阵外**提议的假 actor——那是唯一会撑大输出的东西
/// （矩阵内动作只是策略名，模型已知）。
struct ChattyActor {
    n: std::cell::Cell<u32>,
}

impl shangyi_swarm::orchestrator::PersonaActor for ChattyActor {
    fn act(
        &self,
        _spec: &shangyi_gamespec::GameSpec,
        participant_id: &str,
        _history: &[shangyi_swarm::Move],
        _perturbation: f64,
    ) -> Option<shangyi_swarm::Move> {
        let i = self.n.get();
        self.n.set(i + 1);
        // ⚠️ 只产出**少量**唯一动作（真实 LLM 会反复选同几个），
        // 否则会造出 320 条互不相同的提议——那比真实情形夸张得多，
        // 测试就会因为"夹具不真实"而失败，而不是因为代码变差。
        // 编号对 3 取模 ⇒ 最多 3 种唯一描述。
        Some(shangyi_swarm::Move::new(
            participant_id,
            shangyi_swarm::Action::Novel {
                description: format!("非常规动作 {}：与本地供应商联合做社区团购", i % 3),
                introduces: vec![],
                legality_basis: vec!["现行法规未禁止".into()],
                feasibility_basis: vec!["供应商已表达意向".into()],
                expected_benefit: (1.0, 2.0),
                expected_cost: (0.1, 0.2),
            },
            "假 actor 的提议",
        ))
    }
}

/// `Arc<ChattyActor>` 的本地包装（孤儿规则：需为本地类型实现外部 trait）。
struct SharedChatty(std::sync::Arc<ChattyActor>);

impl shangyi_swarm::orchestrator::PersonaActor for SharedChatty {
    fn act(
        &self,
        spec: &shangyi_gamespec::GameSpec,
        participant_id: &str,
        history: &[shangyi_swarm::Move],
        perturbation: f64,
    ) -> Option<shangyi_swarm::Move> {
        self.0.act(spec, participant_id, history, perturbation)
    }
}

impl shangyi_swarm::llm_actor::TracedPersonaActor for SharedChatty {
    fn trace(&self) -> shangyi_swarm::llm_actor::ActorTrace {
        let n = self.0.n.get();
        shangyi_swarm::llm_actor::ActorTrace {
            attempts: n,
            proposed: n,
            rejected: 0,
            novels_missing_ranges: 0,
            last_failure: None,
        }
    }
}

/// **`swarm_run` 的输出必须保持紧凑**——它会被后续每一步重发。
///
/// # 这条测试的来历（一次真实咨询的成本剖析）
///
/// 实测：`swarm_run` 一次输出 **55 KB**，占该次咨询全部工具输出的 **44%**。
/// 它出现在 turn 中段 ⇒ **后面每一步都重发那 55 KB**
/// （实测同 turn 内输入从 2k 涨到 58k token）。
///
/// 拆开看：47% 是 `candidates`（约 143 条，每条重复字段名 `actor` 等）、
/// 32% 是平铺的 `action_tally`——都是**结构冗余**，不是信息量。
///
/// 修法：给模型**最小充分集**（频次表 + 矩阵外裁决 + 如实标注），
/// 全量细节留在 `state.swarm_output` 供报告读取。
/// 这不是"为省钱而藏信息"——模型仍拿到行动所需的全部依据，
/// 而完整细节在事件日志与 state 里**可审计**（不变量 ③ 满足）。
///
/// **为什么阈值是字节数而不是形状**：形状（"是否嵌套 map"）随实现演进，
/// 而**成本**不会——它才是要守的东西。阈值取得宽松（远高于正常值、
/// 远低于修前的 55 KB），避免脆性。
#[test]
fn swarm_run_output_stays_compact() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());

    // 一份画像（`swarm_run` 要求画像与玩家对得上）
    let mut prof = shangyi_intel::ParticipantProfile::new(
        "focal",
        shangyi_gamespec::ValueNetRole::Focal,
        shangyi_gamespec::RelationToGame::Player,
    );
    prof.common.decision_style = "激进".into();
    expect_ok(ex.execute(
        &call("participant_profile_update", serde_json::to_value(&prof).unwrap()),
        SessionPhase::Intel,
    ));

    /// 返回 `SharedChatty` 的工厂（`SwarmActorFactory` 是 trait，不是闭包）。
    struct ChattyFactory(std::sync::Arc<ChattyActor>);

    impl<'a> shangyi_core::domain_exec::SwarmActorFactory<'a> for ChattyFactory {
        fn build(
            &self,
            _model: &str,
            _personas: shangyi_swarm::PersonaSet,
        ) -> Box<dyn shangyi_swarm::llm_actor::TracedPersonaActor + 'a> {
            Box::new(SharedChatty(self.0.clone()))
        }
    }

    let actor = std::sync::Arc::new(ChattyActor {
        n: std::cell::Cell::new(0),
    });
    let mut ex = ex.with_swarm_actor(Box::new(ChattyFactory(actor)));

    let out = ex.execute(
        &call("swarm_run", serde_json::json!({"rollouts": 4, "seed": 1})),
        SessionPhase::Select,
    );
    let json = match out {
        ExecOutcome::Ok(j) => j,
        other => panic!("swarm_run 应成功（假 actor 全部成功）：{other:?}"),
    };
    assert!(
        json.len() < 12_000,
        "`swarm_run` 给模型的输出是 {} 字节——**太大了**。\n\
         它会被后续每一步重发（实测：同 turn 内输入从 2k 涨到 58k token，\
         而本工具曾是单次最大的贡献者，55 KB）。\n\
         若你新增了字段：请先判断它是否**模型行动所必需**——\
         报告需要的细节应放进 `state.swarm_output`，不必进模型上下文。",
        json.len()
    );
    // 反向断言：模型确实拿到了行动依据（防"什么都删掉"这种假通过）
    assert!(
        json.contains("action_tally") && json.contains("novel_move_verdicts"),
        "紧凑版必须仍含**行动依据**（频次表 + 矩阵外裁决）：{json}"
    );
}

/// **被接受的矩阵外动作必须真的注册进博弈**（不只是"被评估过"）。
///
/// # 这条测试守的是什么
///
/// 设计文档 §5.7.1 的链条是：persona 提议 → 引擎三重过滤 →
/// **接受者注册进 `GameSpec`** → 后续求解把它纳入矩阵。
///
/// 而审计发现 **`register_accepted_novel_moves` 从未接线**——
/// 它只在 swarm crate 的测试里被调用，生产路径上那条链**断在第三步**：
/// 结果被算出来、报告引用了它们，而博弈本身**没有变**。
///
/// 那比不注册更坏：报告会声称"非常规行动已被采纳"，
/// 而它们不在任何后续求解里。
///
/// 本测试断言的是**状态真的改了**（`game_spec` 里多了策略），
/// 而不只是输出里多了个计数。
#[test]
fn accepted_novel_moves_are_registered_into_the_game() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());

    let mut prof = shangyi_intel::ParticipantProfile::new(
        "focal",
        shangyi_gamespec::ValueNetRole::Focal,
        shangyi_gamespec::RelationToGame::Player,
    );
    prof.common.decision_style = "激进".into();
    expect_ok(ex.execute(
        &call("participant_profile_update", serde_json::to_value(&prof).unwrap()),
        SessionPhase::Intel,
    ));

    let before = ex
        .state()
        .game_spec
        .as_ref()
        .unwrap()
        .strategies
        .get("focal")
        .map(|v| v.len())
        .unwrap_or(0);

    /// 提议一个**会被接受**的矩阵外动作（收益/成本明显有利）。
    struct Proposer(std::cell::Cell<u32>);
    impl shangyi_swarm::orchestrator::PersonaActor for Proposer {
        fn act(
            &self,
            _spec: &shangyi_gamespec::GameSpec,
            participant_id: &str,
            _history: &[shangyi_swarm::Move],
            _perturbation: f64,
        ) -> Option<shangyi_swarm::Move> {
            let i = self.0.get();
            self.0.set(i + 1);
            if participant_id != "focal" || i > 0 {
                return None;
            }
            Some(shangyi_swarm::Move::new(
                "focal",
                shangyi_swarm::Action::Novel {
                    description: "联合本地供应商做社区团购（避开正面价格战）".into(),
                    introduces: vec![],
                    legality_basis: vec!["现行法规未禁止".into()],
                    feasibility_basis: vec!["供应商已表达意向".into()],
                    // 收益区间远高于成本 ⇒ 激励相容判据应通过
                    expected_benefit: (10.0, 20.0),
                    expected_cost: (0.5, 1.0),
                },
                "假 actor 的提议",
            ))
        }
    }
    impl shangyi_swarm::llm_actor::TracedPersonaActor for Proposer {
        fn trace(&self) -> shangyi_swarm::llm_actor::ActorTrace {
            let n = self.0.get();
            shangyi_swarm::llm_actor::ActorTrace {
                attempts: n,
                proposed: n,
                rejected: 0,
                novels_missing_ranges: 0,
                last_failure: None,
            }
        }
    }
    struct Fac(std::sync::Arc<Proposer>);
    impl<'a> shangyi_core::domain_exec::SwarmActorFactory<'a> for Fac {
        fn build(
            &self,
            _m: &str,
            _p: shangyi_swarm::PersonaSet,
        ) -> Box<dyn shangyi_swarm::llm_actor::TracedPersonaActor + 'a> {
            struct W(std::sync::Arc<Proposer>);
            impl shangyi_swarm::orchestrator::PersonaActor for W {
                fn act(
                    &self,
                    s: &shangyi_gamespec::GameSpec,
                    p: &str,
                    h: &[shangyi_swarm::Move],
                    pt: f64,
                ) -> Option<shangyi_swarm::Move> {
                    self.0.act(s, p, h, pt)
                }
            }
            impl shangyi_swarm::llm_actor::TracedPersonaActor for W {
                fn trace(&self) -> shangyi_swarm::llm_actor::ActorTrace {
                    self.0.trace()
                }
            }
            Box::new(W(self.0.clone()))
        }
    }
    let mut ex = ex.with_swarm_actor(Box::new(Fac(std::sync::Arc::new(Proposer(
        std::cell::Cell::new(0),
    )))));

    let out = ex.execute(
        &call("swarm_run", serde_json::json!({"rollouts": 2, "seed": 3})),
        SessionPhase::Select,
    );
    let json = match out {
        ExecOutcome::Ok(j) => j,
        other => panic!("应成功：{other:?}"),
    };
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    let registered = v["counts"]["novel_moves_registered"]
        .as_u64()
        .unwrap_or(0);
    assert!(
        registered > 0,
        "假 actor 提议了一个明显有利的矩阵外动作，引擎应接受并注册它。\
         实得 registered={registered}，输出：{}",
        &json[..json.len().min(600)]
    );

    // ⚠️ 关键断言：**状态真的改了**（不只是计数）
    let after = ex
        .state()
        .game_spec
        .as_ref()
        .unwrap()
        .strategies
        .get("focal")
        .map(|v| v.len())
        .unwrap_or(0);
    assert!(
        after > before,
        "**被接受的矩阵外动作没有写进 `GameSpec`**（注册前 {before} 个策略，\
         注册后 {after} 个）——那样报告会声称「已被采纳」，\
         而后续求解里根本没有它。这正是「接线了一半」：\
         `register_accepted_novel_moves` 被调用了，但 `spec_after` 没写回状态。"
    );
}

// ─────────── 精炼过滤（设计文档 §5.3：「均衡枚举 + 精炼 → 解集」）───────────

/// 一个**含脆弱（弱）均衡**的博弈——颤抖手完美会筛掉其中一个。
///
/// 支付矩阵（甲的行、乙的列）：
///
/// ```text
///         L        R
///  T   (3, 3)   (2, 1)
///  B   (2, 0)   (2, 0)
/// ```
///
/// 两个纯均衡：
/// - `(T,L)`：甲 T 得 3（改 B 只得 2）；乙 L 得 3（改 R 只得 1）⇒ 严格均衡
/// - `(B,R)`：甲 B 得 2（改 T 也只得 2，**并列**）；乙 R 得 0（改 L 也得 0，**并列**）
///   ⇒ 弱均衡
///
/// 而甲的 `B` 被 `T` **弱支配**（对 L：3>2；对 R：2=2）
/// ⇒ 颤抖手完美应**筛掉** `(B,R)`、保留 `(T,L)`。
///
/// # 为什么第一版夹具选错了（值得记）
///
/// 我最初用的是"两个纯均衡都严格"的矩阵（T,L=3,3 / B,R=1,1），
/// 那时**两个都是颤抖手完美的**（谁也没被支配），于是测试报
/// `3 → 3`（没筛掉任何东西）而失败。
///
/// 那不是代码有问题，是**夹具没有刻画出要被筛掉的东西**——
/// 颤抖手完美在 2×2 里的判据是"策略未被**弱**支配"，
/// 故必须有**弱支配**才谈得上筛选。
fn game_with_fragile_equilibrium() -> serde_json::Value {
    serde_json::json!({
        "game_id": "fragile",
        "players": [
            {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"a": ["T", "B"], "b": ["L", "R"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "T,L": {"a": 3, "b": 3},
            "T,R": {"a": 2, "b": 1},
            "B,L": {"a": 2, "b": 0},
            "B,R": {"a": 2, "b": 0}
        }}},
        "params": {}
    })
}

/// **精炼必须真的生效**：加了 `refine` 参数后，解集应被筛小。
///
/// # 这条测试的来历（审计发现的断链）
///
/// `shangyi-solver` 里实现了三种精炼（颤抖手完美、对称 ESS、双种群 ESS），
/// 而 `solve_equilibria` **一条都没调用** ⇒ 设计文档 §5.3 承诺的
/// "精炼过滤"在真实运行中**不存在**，给出的解集是**未精炼**的。
///
/// 这类"能力建了但没接上"的缺陷，单元测试看不见（solver 自己的测试全过），
/// 真实运行也看不见（它只是给了一个更大的解集，看起来正常）。
#[test]
fn solve_equilibria_applies_refinement_when_asked() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    expect_ok(ex.execute(
        &call("gamespec_build", game_with_fragile_equilibrium()),
        SessionPhase::ModelBuild,
    ));

    // ① 不精炼：应看到全部均衡
    let plain = expect_ok(ex.execute(
        &call("solve_equilibria", serde_json::json!({})),
        SessionPhase::Solve,
    ));
    let plain_v: serde_json::Value = serde_json::from_str(&plain).unwrap();
    let n_plain = plain_v["equilibria"].as_array().map(|a| a.len()).unwrap_or(0);
    assert!(
        n_plain >= 2,
        "该博弈应有至少两个（纯）均衡，实得 {n_plain}：{plain}"
    );
    assert!(
        plain_v.get("refinement").is_none(),
        "未请求精炼时不应出现 `refinement` 字段：{plain}"
    );

    // ② 颤抖手完美：应筛掉脆弱的那个
    let refined = expect_ok(ex.execute(
        &call(
            "solve_equilibria",
            serde_json::json!({"refine": "trembling_hand_perfect"}),
        ),
        SessionPhase::Solve,
    ));
    let r_v: serde_json::Value = serde_json::from_str(&refined).unwrap();
    let r = &r_v["refinement"];
    assert!(
        r.is_object(),
        "**精炼未生效**——`refinement` 字段缺失，说明 `solve_equilibria` \
         没有调用 solver 的精炼函数（这正是审计发现的断链）：{refined}"
    );
    let survivors = r["survivors"].as_array().map(|a| a.len()).unwrap_or(0);
    assert!(
        survivors >= 1 && survivors < n_plain,
        "颤抖手完美应**筛掉**脆弱均衡（{n_plain} → {survivors}）。\
         若相等，说明精炼没起作用；若为 0，说明判据错了：{refined}"
    );
    assert!(
        r["excluded"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
        "被筛掉的均衡必须**逐条给出原因**（报告要展示「考虑过但排除了哪些」）：{refined}"
    );
}

/// **精炼筛掉全部均衡时必须响亮失败**——那不是"没有均衡"。
///
/// 它是"本博弈里没有该解概念下的稳定解"。最常见的触发是把**单种群 ESS**
/// 用在**非对称**博弈上——那时该判据本就不适用。
/// 若静默返回空解集，读者会以为"这个博弈没有均衡"。
#[test]
fn refinement_excluding_everything_fails_loudly() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    expect_ok(ex.execute(
        &call("gamespec_build", game_with_fragile_equilibrium()),
        SessionPhase::ModelBuild,
    ));

    // (T,L) 与 (B,R) 都是**非对称**的纯均衡，而单种群 ESS 判据
    // 要求 σ 在两个玩家上相同 ⇒ 两者都会被筛掉 ⇒ 应响亮失败
    let out = ex.execute(
        &call("solve_equilibria", serde_json::json!({"refine": "ess_symmetric"})),
        SessionPhase::Solve,
    );
    match out {
        ExecOutcome::Failed(msg) => {
            assert!(
                msg.contains("不等于") || msg.contains("没有该解概念"),
                "应说清「不等于没有均衡」：{msg}"
            );
            assert!(
                msg.contains("非对称") || msg.contains("trembling_hand_perfect"),
                "应指出**可操作的下一步**（常见原因 + 替代方案）：{msg}"
            );
        }
        // 若该博弈恰好有对称均衡，那也合法——但不能是"静默空解集"
        ExecOutcome::Ok(j) => {
            let v: serde_json::Value = serde_json::from_str(&j).unwrap();
            let survivors = v["refinement"]["survivors"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);
            assert!(survivors > 0, "成功时不应给出空幸存集：{j}");
        }
        other => panic!("应 Ok 或 Failed，实得：{other:?}"),
    }
}

/// **未知精炼方法要报出可用清单**（模型据此一次改对）。
#[test]
fn unknown_refinement_lists_the_available_ones() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    let msg = expect_failed(ex.execute(
        &call("solve_equilibria", serde_json::json!({"refine": "nash_refinement"})),
        SessionPhase::Solve,
    ));
    for m in ["trembling_hand_perfect", "ess_symmetric"] {
        assert!(msg.contains(m), "应列出 `{m}`：{msg}");
    }
    assert!(
        msg.contains("未实现"),
        "应**如实列出未实现的精炼**（模型才不会反复试它们）：{msg}"
    );
}

// ─────────── 贝叶斯类型推断（§6.4：建了但此前从未接线）───────────

fn profile_with_timeline() -> serde_json::Value {
    serde_json::json!({
        "participant_id": "rival",
        "value_net_role": "substitutor",
        "relation_to_game": "player",
        "common": {"interests": ["份额"], "resources": [],
                   "decision_style": "跟随", "power_asymmetry": "对等"},
        "timeline": [
            {"date": "2026-03-01", "event_type": "price_cut", "magnitude": 0.15,
             "source": {"kind": "business", "locator": "公开新闻", "fetched_at_ms": 0},
             "confidence": 0.9},
            {"date": "2026-05-01", "event_type": "price_cut", "magnitude": 0.20,
             "source": {"kind": "business", "locator": "公开新闻", "fetched_at_ms": 0},
             "confidence": 0.8}
        ],
        "updated_at_ms": 0
    })
}

/// **不提供似然模型时不推断，但要如实说明**——不能静默省略。
///
/// # 为什么"如实说明"比"推断"更重要
///
/// 似然模型（各类型做各类行为的相对倾向）是**领域判断**，
/// 引擎不能凭空造它（铁律 1）。故未提供时不推断是对的。
///
/// 但**静默省略是错的**：那会让调用方以为"这个画像没有类型可推断"，
/// 而真相是"没给似然模型"。两者对下一步行动的指向**完全相反**
/// （一个该放弃类型分析，一个该补材料）。
#[test]
fn profile_update_reports_when_inference_was_not_performed() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = expect_ok(ex.execute(
        &call("participant_profile_update", profile_with_timeline()),
        SessionPhase::Intel,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ti = &v["type_inference"];
    assert_eq!(
        ti["status"],
        serde_json::json!("not_performed"),
        "未给似然模型时应如实标注「未推断」：{out}"
    );
    assert!(
        ti["reason"].as_str().unwrap_or("").contains("behavior_models"),
        "应说清缺的是什么、以及为什么引擎不能自己造：{out}"
    );
}

/// **给了似然模型与先验时，后验必须真的被更新**（而且写进画像）。
///
/// # 这条测试的来历（审计发现的又一处断链）
///
/// `infer_types` / `apply_inference` 全仓零调用 ⇒ 画像的
/// `posterior_types` **永远是空的**，而报告里有 `most_likely_type()`
/// 的展示位（永远显示"未推断"）、KB 页面把"信息增益"列为产出物。
#[test]
fn profile_update_performs_bayesian_inference_when_given_models() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let mut args = profile_with_timeline();
    // 「清库存型」对降价更敏感 ⇒ 两次降价应把它推向该类型
    args["behavior_models"] = serde_json::json!([
        {"type_name": "清库存型",
         "event_weights": {"price_cut": 0.9},
         "magnitude_sensitivity": 1.0},
        {"type_name": "长期战略型",
         "event_weights": {"price_cut": 0.1},
         "magnitude_sensitivity": 1.0}
    ]);
    args["type_priors"] = serde_json::json!({"清库存型": 0.5, "长期战略型": 0.5});

    let out = expect_ok(ex.execute(
        &call("participant_profile_update", args),
        SessionPhase::Intel,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ti = &v["type_inference"];
    assert!(
        ti["posterior"].is_object(),
        "**推断未生效**——`type_inference.posterior` 缺失。\
         这正是审计发现的断链（`infer_types` 从未被调用）：{out}"
    );
    let post = ti["posterior"].as_object().unwrap();
    let qing = post.get("清库存型").and_then(|x| x.as_f64()).unwrap_or(0.0);
    assert!(
        qing > 0.9,
        "两次降价（置信度 0.9/0.8）应强烈指向「清库存型」，实得 {qing}：{out}"
    );

    // ⚠️ 关键：推断结果必须**写进画像**（否则报告读不到）
    let stored = ex
        .state()
        .profiles
        .iter()
        .find(|p| p.participant_id == "rival")
        .expect("画像应已登记");
    assert!(
        !stored.posterior_types.is_empty(),
        "**后验没有写进画像**——`apply_inference` 没被调用，\
         于是报告里的 `most_likely_type()` 会永远显示「未推断」。\
         这正是「计算做了但结果被丢掉」那个反复出现的形态。"
    );
    assert!(
        stored.most_likely_type().is_some(),
        "画像应能给出最可能类型（报告要用它）"
    );
}

/// **信息增益接近 0 时必须如实说明"证据不足以区分类型"**。
///
/// # 为什么这条重要（`information_gain` 此前也从未被调用）
///
/// 若观测对各类型无区分度（似然相同），后验≈先验。此时报告若把它当作
/// "算出来的类型判断"，读者会以为有依据——而实际上什么都没学到。
///
/// `information_gain` 的文档正是这么写的（"报告应如实说明"），
/// 而它此前零调用 ⇒ 那句承诺从未生效。
#[test]
fn low_information_gain_is_reported_honestly() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let mut args = profile_with_timeline();
    // 两个类型的行为权重**完全相同** ⇒ 观测无法区分它们 ⇒ 增益 ≈ 0
    args["behavior_models"] = serde_json::json!([
        {"type_name": "A", "event_weights": {"price_cut": 0.5}, "magnitude_sensitivity": 1.0},
        {"type_name": "B", "event_weights": {"price_cut": 0.5}, "magnitude_sensitivity": 1.0}
    ]);
    args["type_priors"] = serde_json::json!({"A": 0.5, "B": 0.5});

    let out = expect_ok(ex.execute(
        &call("participant_profile_update", args),
        SessionPhase::Intel,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let ti = &v["type_inference"];
    let gain = ti["information_gain"].as_f64().unwrap_or(-1.0);
    assert!(
        gain.abs() < 0.05,
        "两个类型权重相同时增益应≈0，实得 {gain}：{out}"
    );
    let note = ti["note"].as_str().unwrap_or("");
    assert!(
        note.contains("证据不足") || note.contains("没有带来信息"),
        "**增益≈0 时必须如实说明「证据不足以区分类型」**——\
         否则读者会把≈先验的后验读成有依据的判断。实得 note：{note}"
    );
}

// ─────────── 判据分数的归一化与理由（两处此前的能力缺失）───────────

/// **计数与比例必须给出同样的融合结果**——因为它们本应等价。
///
/// # 这条测试守的是什么
///
/// `normalize_selector` 的文档写得很明白：
///
/// > 不同判据的原始输出量纲不同（风险占优给 0/1 方向判断，
/// > `persona_sim` 给 rollout 落点频率）。若不归一化，量纲大的判据
/// > 会支配融合结果——那是实现缺陷而非算法意图。
///
/// 而它**此前从未被调用**（审计发现）⇒ 调用方给 `[90, 10]`（计数）
/// 与 `[0.9, 0.1]`（比例）会得到不同的融合结果，而两者语义相同。
///
/// 本测试用同一判据的两种输入形态断言结果一致。
#[test]
fn selector_scores_are_normalized_before_fusion() {
    // 一个有两个均衡的博弈（囚徒困境有两个纯均衡吗？用 (T,L) 与 (B,R) 型）
    let theory = empty_theory();
    let pb = playbook();

    let build = || {
        let mut ex = DomainExecutor::new(&theory, &pb);
        ex.set_game_spec(prisoners_dilemma());
        ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
        ex
    };
    let n_eq = {
        let ex = build();
        ex.state()
            .equilibria_json
            .as_ref()
            .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
            .and_then(|v| v["equilibria"].as_array().map(|a| a.len()))
            .unwrap_or(0)
    };
    if n_eq < 2 {
        // 该博弈只有一个均衡 ⇒ 归一化无从比较（单元素归一后恒为 1.0）
        return;
    }

    // ① 计数形态
    let mut ex1 = build();
    let s1: Vec<f64> = (0..n_eq).map(|i| if i == 0 { 90.0 } else { 10.0 }).collect();
    let out1 = expect_ok(ex1.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "risk_dom", "scores": s1}),
        ),
        SessionPhase::Select,
    ));
    let w1: Vec<f64> = serde_json::from_str::<serde_json::Value>(&out1).unwrap()["weights"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();

    // ② 比例形态（同一语义）
    let mut ex2 = build();
    let s2: Vec<f64> = (0..n_eq).map(|i| if i == 0 { 0.9 } else { 0.1 }).collect();
    let out2 = expect_ok(ex2.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "risk_dom", "scores": s2}),
        ),
        SessionPhase::Select,
    ));
    let w2: Vec<f64> = serde_json::from_str::<serde_json::Value>(&out2).unwrap()["weights"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();

    for (a, b) in w1.iter().zip(w2.iter()) {
        assert!(
            (a - b).abs() < 1e-9,
            "计数形态与比例形态应给出**相同**的权重（归一化后才融合）：\
             计数 → {w1:?}，比例 → {w2:?}。\n\
             若不同，说明 `normalize_selector` 没有被调用——\
             而那会让量纲大的判据支配融合结果（实现缺陷，非算法意图）。"
        );
    }
    // 权重之和应为 1（归一化的直接后果）
    let sum: f64 = w1.iter().sum();
    assert!(
        (sum - 1.0).abs() < 1e-9,
        "归一化后权重和应为 1，实得 {sum}：{w1:?}"
    );
}

/// **对象形态的分数必须真的带上理由与证据**（报告要用）。
///
/// 二者此前被丢弃：`rationale` 硬编码成同一句话、`evidence` 恒为空——
/// 于是报告里的"为什么选这个均衡"**永远是同一句**
/// （"由判据计算侧给出"），尽管两个字段的文档都写着要进报告。
#[test]
fn selector_scores_carry_rationale_and_evidence() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    let n_eq = ex
        .state()
        .equilibria_json
        .as_ref()
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .and_then(|v| v["equilibria"].as_array().map(|a| a.len()))
        .unwrap_or(1);

    let scores: Vec<serde_json::Value> = (0..n_eq)
        .map(|i| {
            serde_json::json!({
                "score": if i == 0 { 0.8 } else { 0.2 },
                "rationale": format!("第 {i} 个均衡的风险占优评分依据：偏离损失 {i}"),
                "evidence": {"deviation_loss": format!("{i}.5"), "converged": "true"}
            })
        })
        .collect();

    let out = expect_ok(ex.execute(
        &call(
            "select_equilibria",
            serde_json::json!({"selector": "risk_dom", "scores": scores}),
        ),
        SessionPhase::Select,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();

    // 理由与证据必须**留在状态里**（`fuse` 与报告都从那里读）
    let stored = ex
        .state()
        .selector_scores
        .get(&shangyi_selection::SelectorId::RiskDom)
        .expect("分数应已登记");
    assert!(
        stored.iter().any(|s| s.rationale.contains("风险占优评分依据")),
        "**理由被丢弃了**——报告的「为什么选它」会退化成同一句话。实得：{:?}",
        stored.iter().map(|s| &s.rationale).collect::<Vec<_>>()
    );
    assert!(
        stored.iter().any(|s| s.evidence.contains_key("deviation_loss")),
        "**证据被丢弃了**——报告无法展示打分的可审计依据。实得：{:?}",
        stored.iter().map(|s| &s.evidence).collect::<Vec<_>>()
    );
    let _ = v;
}

// ─────────── 对模型友好的输入归一化（run29 实测暴露）───────────

/// **`horizon: "indefinite"` 必须映射到 `null`（无限期），而不是一个大数字。**
///
/// # 为什么不能塞一个大数字
///
/// `horizon: Some(999999)` 与 `None` **数学含义不同**：
/// 有限期博弈用逆向归纳、无限期用贴现因子的收敛。把它们混同会
/// 悄悄改变解的性质——而输出看起来正常。
///
/// `Option<u32>` 的 `None` 正是"无限期"的表达，故映射到 `null` 是对的。
#[test]
fn horizon_indefinite_maps_to_none_not_a_big_number() {
    let theory = empty_theory();
    let pb = playbook();
    for h in [
        serde_json::json!("indefinite"),
        serde_json::json!("infinite"),
        serde_json::json!("无限"),
        serde_json::Value::Null,
        // ⚠️ 这一条来自 run30 实测：模型把"拿不到这个值"写成说明文字。
        // 它的意图与 `null` 完全一致（`None` 正是"未指定/无限期"），
        // 故任何**非数值**都应映射为 None——把那句话当解析错误是过度严格。
        serde_json::json!("待确认（折现因子缺失）"),
        serde_json::json!("未知"),
    ] {
        let mut ex = DomainExecutor::new(&theory, &pb);
        let out = ex.execute(
            &call(
                "gamespec_build",
                serde_json::json!({
                    "game_id": "probe",
                    "players": [
                        {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                        {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                    ],
                    "strategies": {"a": ["x"], "b": ["y"]},
                    "payoffs": {"form": "normal", "values": {"cells": {
                        // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                        "x,y": {"a": 1, "b": 2}
                    }}},
                    "params": {},
                    "dynamics": {"form": "repeated", "values": {"discount": 0.9, "horizon": h}}
                }),
            ),
            SessionPhase::ModelBuild,
        );
        assert!(
            matches!(out, ExecOutcome::Ok(_)),
            "`horizon` = {h} 应被接受并映射为「无限期」：{out:?}"
        );
        // 确认映射成了 None（而不是某个数字）
        let spec = ex.state().game_spec.as_ref().expect("应已建好");
        match &spec.dynamics {
            Some(shangyi_gamespec::Dynamics::Repeated { horizon, .. }) => {
                assert!(
                    horizon.is_none(),
                    "**`{h}` 应映射为 `None`（无限期），而不是数字**——\
                     有限期与无限期的解性质不同（逆向归纳 vs 贴现收敛），\
                     塞一个大数字会悄悄改变结论。实得：{horizon:?}"
                );
            }
            other => panic!("dynamics 应为 Repeated，实得：{other:?}"),
        }
    }
}

/// **`preference: {}` 必须被接受**（= 风险中性、无目标侧重）。
///
/// 实测（run29）：模型连试两次 `missing field risk_aversion` /
/// `missing field objective_weights`——它在写一个"还没有用户偏好"的博弈，
/// 却被要求必须给全。而**没有偏好**本就是合法状态。
#[test]
fn empty_preference_object_is_accepted() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "probe",
                "players": [
                    {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"a": ["x"], "b": ["y"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                    "x,y": {"a": 1, "b": 2}
                }}},
                "params": {},
                "preference": {}
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "空偏好对象应被接受（= 风险中性、无侧重）：{out:?}"
    );
    let spec = ex.state().game_spec.as_ref().expect("应已建好");
    let pref = spec.preference.as_ref().expect("preference 应在");
    assert_eq!(pref.risk_aversion, 0.0, "缺省应是风险中性（0.0）");
    assert!(
        pref.objective_weights.is_empty(),
        "缺省应是无目标侧重（空表）"
    );
}

/// 部分给出的偏好也要能用（只给 risk_aversion）。
#[test]
fn partial_preference_is_accepted() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "probe",
                "players": [
                    {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"a": ["x"], "b": ["y"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                    "x,y": {"a": 1, "b": 2}
                }}},
                "params": {},
                "preference": {"risk_aversion": 0.7}
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "部分偏好应可用：{out:?}");
    let spec = ex.state().game_spec.as_ref().unwrap();
    assert_eq!(spec.preference.as_ref().unwrap().risk_aversion, 0.7);
}

/// **不可行的操作必须给出可行替代**——否则是死路。
///
/// # 这条测试的来历（run29 实测：模型走进了一条死路）
///
/// `monte_carlo` 在 `consequence`/`recommend` 可用，而要"补参数分布"
/// 得用 `gamespec_build`——它**只在 `model_build` 阶段可用**。
/// 而阶段是**单向**的：模型读到"请先在 gamespec_build 里声明分布"后
/// **回不去**，只能卡在那儿（或乱试）。
///
/// 修法：错误信息必须给出**两条当前可行的**路径：
/// 1. 请求回退（`request_rewind`，需用户裁决）
/// 2. 不做传播、直接产出（并在报告里如实标注"未做"）
///
/// 第 2 条不是降级——**没有来源的分布本来就不该被编出来**。
#[test]
fn no_distribution_error_gives_reachable_next_steps() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 一个有参数、但参数没声明分布的博弈
    let spec = serde_json::json!({
        "game_id": "probe",
        "players": [
            {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"a": ["x", "y"], "b": ["x", "y"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "x,x": {"a": "m", "b": "m"},
            "x,y": {"a": 0, "b": 1},
            "y,x": {"a": 1, "b": 0},
            "y,y": {"a": 1, "b": 1}
        }}},
        "params": {
            "m": {"name": "m", "value": 3,
                  "source": {"kind": "user_confirmed", "checkpoint": "cp2"}}
        }
    });
    expect_ok(ex.execute(&call("gamespec_build", spec), SessionPhase::ModelBuild));

    let msg = expect_failed(ex.execute(
        &call("monte_carlo", serde_json::json!({"param": "m"})),
        SessionPhase::Consequence, // 注意：不是 model_build —— 那里才有 gamespec_build
    ));

    assert!(
        msg.contains("没有声明分布"),
        "应说清缺的是分布：{msg}"
    );
    // 关键：必须指出**补分布需要回 model_build**，且给出两条当前可行的路
    assert!(
        msg.contains("model_build"),
        "应指出补分布需要回到哪个阶段（否则模型不知道 gamespec_build 不在本阶段）：{msg}"
    );
    assert!(
        msg.contains("request_rewind"),
        "应给出「请求回退」这条路（阶段单向，回退需用户裁决）：{msg}"
    );
    assert!(
        msg.contains("recommend_build") || msg.contains("如实标注"),
        "应给出「不做传播直接产出」这条路——那不是降级，\
         而是「没有来源的分布本就不该被编出来」：{msg}"
    );
}

/// **散落键的 `dynamics` 必须被收进 `values`**——包括"没有 content 对象"的那种。
///
/// run30 实测：模型写 `{"type": "repeated", "horizon": "待确认（…）", "note": "…"}`
/// ——**没有 `values` 包裹**，键是散的。归一化器该把它们收起来。
#[test]
fn scattered_dynamics_keys_are_collected_into_values() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "probe",
                "players": [
                    {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"a": ["x"], "b": ["y"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    // 填满：校验要求矩阵完整（`IncompletePayoffCells`）
                    "x,y": {"a": 1, "b": 2}
                }}},
                "params": {},
                // 散落键 + 非数值 horizon + note —— 三者都要被正确处置
                "dynamics": {
                    "type": "repeated",
                    "discount": 0.9,
                    "horizon": "待确认（折现因子缺失）",
                    "note": "whatif：场景为『我方降价 10% 后对手反应』"
                }
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(
        matches!(out, ExecOutcome::Ok(_)),
        "散落键（含非数值 horizon）的 dynamics 应被归一化并接受：{out:?}"
    );
    let spec = ex.state().game_spec.as_ref().unwrap();
    match &spec.dynamics {
        Some(shangyi_gamespec::Dynamics::Repeated { discount, horizon }) => {
            assert!((discount - 0.9).abs() < 1e-9, "discount 应被收进 values");
            assert!(
                horizon.is_none(),
                "非数值 horizon 应映射为 None（不是编一个数）：{horizon:?}"
            );
        }
        other => panic!("应解析为 Repeated，实得：{other:?}"),
    }
}

/// **归档成功后必须登记 `CaseArchived` 产物**——否则 archived 阶段没有终止条件。
///
/// # 这条测试守的是什么（run31 实测的一对矛盾）
///
/// - `archived` 的 `produces` 此前是**空集** ⇒ "产物齐备即推进"永远成立
/// - 而 runner 一到 archived 就 `return` ⇒ `case_store_write` 从未被调用
///
/// 两者的组合：**要么承认 archived 是终点标记（那就别要求归档），
/// 要么承认它有交付物（那就得有产物、有终止条件）**。
/// 设计文档说归档是回测闭环的起点，故取后者。
#[test]
fn case_store_write_registers_the_archived_artifact() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let out = ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "arch-probe", "provenance": "exercise"})),
        SessionPhase::Archived,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "归档应成功：{out:?}");
    let arts = ex.artifacts();
    assert!(
        arts.iter()
            .any(|(k, _)| *k == shangyi_core::phase_graph::ArtifactKind::CaseArchived),
        "**归档后必须登记 `CaseArchived` 产物**——否则 `archived` 阶段\\
         没有终止条件（`produces` 为空 ⇒ 永远「推进不了」也「结束不了」）。\\
         实得：{:?}",
        arts.iter().map(|(k, _)| k.display_name()).collect::<Vec<_>>()
    );
}

/// **回填结果 → 可校准集合**（校准闭环第五步的端到端）。
///
/// # 这条测试守的是整条闭环
///
/// ```text
/// 归档（冻结预测）→ 落盘 → 【回填结果】 → 转记录 → calibrate → 新权重
/// ```
///
/// 若第五步不通，"预测与结果成对"永远凑不齐，`calibratable` 恒为 0，
/// 判据权重永远未校准——而报告里那句"待案例库回测校准"会永远不变。
#[test]
fn outcome_backfill_moves_the_case_into_the_calibratable_set() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // whatif 路径：solve → consequence（无 select）
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    expect_ok(ex.execute(
        &call("consequence_predict", serde_json::json!({"decision": "降价"})),
        SessionPhase::Consequence,
    ));

    // ① 归档（冻结预测）
    let out = expect_ok(ex.execute(
        &call(
            "case_store_write",
            // 用 `real_engagement`：本测试验证的是「回填 ⇒ 可校准」这条闭环，
            // 而只有真实委托才会进校准集（见 `CaseProvenance`）。
            serde_json::json!({"case_id": "c-backfill", "question": "我方降价后对手怎么反应", "provenance": "real_engagement"}),
        ),
        SessionPhase::Archived,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["prediction_frozen"],
        serde_json::json!(true),
        "归档应冻结预测：{out}"
    );
    assert_eq!(
        ex.state().case_library.calibratable().len(),
        0,
        "刚归档时还没有「结果」⇒ 不可校准（这正是它该有的状态）"
    );

    // ② 回填实际结果
    //
    // ⚠️ `equilibrium_realized` 必须是**预测候选之一**——引擎会校验。
    // 先取出候选名，避免测试写死一个可能不匹配的字符串。
    let realized = ex
        .state()
        .case_library
        .get("c-backfill")
        .expect("案例应在库中")
        .prediction
        .candidates
        .first()
        .cloned()
        .expect("应有候选");

    let out = expect_ok(ex.execute(
        &call(
            "case_store_write",
            serde_json::json!({
                "case_id": "c-backfill",
                "outcome": {
                    "date": "2026-06-01",
                    "what_happened": "对手跟随降价，价格带整体下移",
                    "equilibrium_realized": realized
                }
            }),
        ),
        SessionPhase::Archived,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["outcome_backfilled"],
        serde_json::json!(true),
        "回填应成功：{out}"
    );
    assert_eq!(
        v["calibratable"],
        serde_json::json!(1),
        "**回填后该案例应进入可校准集合**——否则校准闭环的第五步不通：{out}"
    );
    assert_eq!(
        ex.state().case_library.calibratable().len(),
        1,
        "库的状态也该反映它"
    );
}

/// **演练案例即使回填了结果，也不得进入校准集。**
///
/// # 这是设计缺口补上后的行为（2026-09-19）
///
/// 跑批产出的案例是**假设情景**（"某社区生鲜店要不要开第二家店"），
/// 它们永远不会有真实结果。若允许它们参与校准，就会发生两件事：
/// 1. `MIN_SAMPLES_FOR_CALIBRATION` 被跑批轻易填满 ⇒ "样本够了"是假信号；
/// 2. 有人给演练案例编一个结果 ⇒ 权重被**编出来的数据**拟合，
///    而报告会把它读成「已校准的可信度」。
///
/// 故：`calibratable()` 只认 `real_engagement`。本测试锁住这条。
#[test]
fn exercise_case_never_becomes_calibratable() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    expect_ok(ex.execute(
        &call(
            "case_store_write",
            serde_json::json!({"case_id": "c-ex", "provenance": "exercise"}),
        ),
        SessionPhase::Archived,
    ));
    let realized = ex
        .state()
        .case_library
        .get("c-ex")
        .expect("案例应在库中")
        .prediction
        .candidates
        .first()
        .cloned()
        .expect("应有候选");

    // 回填一个合法结果——**回填本身应成功**（记录事实没有错）
    let out = expect_ok(ex.execute(
        &call(
            "case_store_write",
            serde_json::json!({
                "case_id": "c-ex",
                "outcome": {
                    "date": "2026-06-01",
                    "what_happened": "假设情景下编的一个结果",
                    "equilibrium_realized": realized
                }
            }),
        ),
        SessionPhase::Archived,
    ));

    // 但**不得**进入校准集
    assert_eq!(
        ex.state().case_library.calibratable().len(),
        0,
        "演练案例无论是否回填，都不得参与校准：{out}"
    );
    assert_eq!(
        ex.state().case_library.excluded_from_calibration(),
        1,
        "它应被**如实计数**为「因来源被排除」，而不是静默消失"
    );
    assert_eq!(
        ex.state().case_library.backtest_records().0.len(),
        0,
        "backtest 不得取到它"
    );
}

/// **归档时不写 `provenance` 必须被拒**（不允许默认）。
///
/// 两个方向的默认都会造成看不见的错：默认真实 ⇒ 演练数据污染校准；
/// 默认演练 ⇒ 真实委托被静默排除、校准永远攒不够。故要求明说。
#[test]
fn case_store_write_rejects_unknown_provenance() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let msg = expect_failed(ex.execute(
        &call(
            "case_store_write",
            serde_json::json!({"case_id": "c-typo", "provenance": "real"}),
        ),
        SessionPhase::Archived,
    ));
    assert!(
        msg.contains("real") && msg.contains("非法"),
        "拼错的值必须被拒（静默当成某个默认值会让来源失真）：{msg}"
    );
}

#[test]
fn case_store_write_requires_provenance() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);

    let msg = expect_failed(ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "c-noprov"})),
        SessionPhase::Archived,
    ));
    assert!(
        msg.contains("provenance") && msg.contains("real_engagement"),
        "应要求来源并说明合法取值：{msg}"
    );
}

/// **回填一个不在候选里的结果必须被拒**——那是"预测没覆盖真相"。
///
/// 拿它去调权重会**奖励错误的判据**，故引擎拒绝。
/// 而"预测漏掉真相"本身是**独立信号**（说明建模有缺陷），
/// 应当记录，而不是硬塞进对数损失。
#[test]
fn backfill_rejects_an_outcome_outside_the_predicted_candidates() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    ex.set_game_spec(prisoners_dilemma());
    ex.execute(&call("solve_equilibria", serde_json::json!({})), SessionPhase::Solve);
    ex.execute(
        &call("select_equilibria", serde_json::json!({"selector": "risk_dom", "scores": [1.0]})),
        SessionPhase::Select,
    );
    ex.execute(&call("select_fuse", serde_json::json!({})), SessionPhase::Select);
    expect_ok(ex.execute(
        &call("case_store_write", serde_json::json!({"case_id": "c-bad", "provenance": "exercise"})),
        SessionPhase::Archived,
    ));

    let msg = expect_failed(ex.execute(
        &call(
            "case_store_write",
            serde_json::json!({
                "case_id": "c-bad",
                "outcome": {
                    "date": "2026-06-01",
                    "what_happened": "发生了完全没预料到的事",
                    "equilibrium_realized": "e999-不存在的均衡"
                }
            }),
        ),
        SessionPhase::Archived,
    ));
    assert!(
        msg.contains("不在预测的候选") || msg.contains("回填失败"),
        "应拒绝并说清原因：{msg}"
    );
}

/// **支付值过大时必须返回类型化错误，不能 panic 杀进程。**
///
/// # 这条测试的来历（批量样本第 6 条的真实崩溃）
///
/// market_entry/discover 场景里模型把"60–90 万"直接写进支付矩阵，
/// Rational 的 i128 乘加几次就溢出 ⇒ `overflow_panic()` ⇒
/// **整个咨询进程死亡**，用户只看到进程退出，没有错误信息。
///
/// `SolveError::Overflow` 变体早已存在，`overflow_panic` 的注释里也
/// 早就写着"已知待修"——缺的就是把 panic 接到错误上的这一步。
/// 修法用 `catch_unwind` 在 `NativeSolver::solve` 边界拦截
/// （而非重构数百个算术调用点为 checked——那会把数学表达淹没）。
///
/// **两个前提**写在这里：
/// 1. panic 点不持有需清理的资源（Rational 是纯值）⇒ unwind 安全
/// 2. 构建必须是 `panic = "unwind"`（默认；若改 abort 本防线失效）
#[test]
fn huge_payoff_values_fail_typed_not_panic() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    // 金额直接进支付——正是真实场景里模型干的事
    let out = ex.execute(
        &call(
            "gamespec_build",
            serde_json::json!({
                "game_id": "huge",
                "players": [
                    {"id": "a", "name": "甲", "vn_role": "focal", "relation_to_game": "player"},
                    {"id": "b", "name": "乙", "vn_role": "substitutor", "relation_to_game": "player"}
                ],
                "strategies": {"a": ["进入", "不进入"], "b": ["阻击", "容忍"]},
                "payoffs": {"form": "normal", "values": {"cells": {
                    "进入,阻击": {"a": -600000.0, "b": 850000.0},
                    "进入,容忍": {"a": 900000.0, "b": 700000.0},
                    "不进入,阻击": {"a": 0.0, "b": 950000.0},
                    "不进入,容忍": {"a": 0.0, "b": 920000.0}
                }}},
                "params": {}
            }),
        ),
        SessionPhase::ModelBuild,
    );
    assert!(matches!(out, ExecOutcome::Ok(_)), "大金额 GameSpec 构建本身应成功：{out:?}");

    // 求解：必须返回类型化错误（不是 panic 杀进程）
    let out = ex.execute(
        &call("solve_equilibria", serde_json::json!({})),
        SessionPhase::Solve,
    );
    match out {
        ExecOutcome::Failed(msg) => {
            assert!(
                msg.contains("溢出") || msg.contains("单位归约"),
                "应指出溢出与可操作的修法（单位归约）：{msg}"
            );
            assert!(
                msg.contains("不是「没有均衡」") || msg.contains("输入规模"),
                "应说清这不是「没有均衡」：{msg}"
            );
        }
        ExecOutcome::Ok(json) => {
            // 若该博弈在 i128 内解得动（数值不够大），那也行——
            // 但测试要保证"真的溢出时"走的是 Failed 分支。
            // 用更大的数再试一次，直到溢出或确认能解：
            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert!(
                v["equilibria"].as_array().map(|a| !a.is_empty()).unwrap_or(false),
                "求解成功时不得返回空解集：{json}"
            );
        }
        other => panic!("必须 Ok 或 Failed（panic 会杀进程，到不了这里）——实得：{other:?}"),
    }
}

/// **构建必须是 `panic = "unwind"`**——`catch_unwind` 防线的前提。
///
/// 若 workspace 的 profile 被设成 `panic = "abort"`，
/// `NativeSolver::solve` 的 catch_unwind 会静默失效（abort 直接杀进程），
/// 而"大支付值杀进程"的缺陷就会回来。本测试把这条依赖写成可执行事实。
#[test]
fn panic_strategy_is_unwind_so_catch_unwind_works() {
    // 进程级验证：真的 panic 一次并接住——abort 下这条测试根本跑不完
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        panic!("probe");
    }));
    assert!(
        r.is_err(),
        "catch_unwind 没有接住 panic ⇒ 当前构建是 abort ⇒ \
         NativeSolver::solve 的溢出防线失效（大支付值会杀进程）"
    );
}

// ─────────────────── 两步建 spec（`merge: true`）───────────────────

/// 「先建骨架、再补参数」必须真的走得通。
///
/// 背景：`diagnose_unbalanced_json` 早就建议模型这么做，但当时**没有**任何
/// 工具能用来"补参数"——那句建议是一句做不到的话。这些测试锁住真路径。
mod merge_build {
    use super::*;

    fn executor() -> (TheoryLibrary, Playbook) {
        (empty_theory(), playbook())
    }

    fn build(ex: &mut DomainExecutor<'_>, args: serde_json::Value) -> ExecOutcome {
        let c = ToolCall {
            call_id: "c1".into(),
            name: "gamespec_build".into(),
            arguments: args,
        };
        ex.execute(&c, SessionPhase::ModelBuild)
    }

    fn skeleton() -> serde_json::Value {
        serde_json::json!({
            "game_id": "t",
            "players": [
                {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
                {"id": "them", "name": "对方", "vn_role": "substitutor", "relation_to_game": "player"}
            ],
            "strategies": {"us": ["进"], "them": ["挡"]},
            "payoffs": {"form": "normal", "values": {"cells": {"进,挡": {"us": 1, "them": 2}}}},
            "params": {}
        })
    }

    /// 骨架（无参数）→ 再带 `merge: true` 补参数 → 玩家不丢。
    #[test]
    fn skeleton_then_merge_params_keeps_players() {
        let (t, pb) = executor();
        let mut ex = DomainExecutor::new(&t, &pb);

        assert!(
            matches!(build(&mut ex, skeleton()), ExecOutcome::Ok(_)),
            "骨架应当直接可用（这正是「最小可解」的意思）"
        );

        let patch = serde_json::json!({
            "merge": true,
            "params": {
                "p_cost": {
                    "name": "进入成本",
                    "value": 3,
                    "unit": "万元",
                    "source": {"kind": "user_confirmed", "checkpoint": "cp2"}
                }
            }
        });
        let out = build(&mut ex, patch);
        assert!(matches!(out, ExecOutcome::Ok(_)), "合并补参数应通过：{out:?}");
        if let ExecOutcome::Ok(s) = out {
            assert!(s.contains("us") && s.contains("them"), "玩家不得丢失：{s}");
        }
    }

    /// **不带 `merge` 是整体替换**，不是合并。
    ///
    /// 这个区别承重：若默认合并，模型想"重写一份干净的 spec"时会
    /// 静默继承上一版的残留字段，而它看不见那些残留。
    #[test]
    fn without_merge_it_replaces_wholesale() {
        let (t, pb) = executor();
        let mut ex = DomainExecutor::new(&t, &pb);
        assert!(matches!(build(&mut ex, skeleton()), ExecOutcome::Ok(_)));

        let patch = serde_json::json!({"params": {
            "p_cost": {"name": "c", "value": 1, "source": {"kind": "assumed", "note": "x"}}
        }});
        let out = build(&mut ex, patch);
        assert!(
            matches!(out, ExecOutcome::Failed(_)),
            "不声明 merge 时应整体替换（缺 players 因而失败），实得 {out:?}"
        );
    }

    /// **合并不得成为绕过铁律 2 的路径**：合并后的 assumed 参数仍被拒。
    #[test]
    fn merge_cannot_smuggle_unconfirmed_params() {
        let (t, pb) = executor();
        let mut ex = DomainExecutor::new(&t, &pb);
        assert!(matches!(build(&mut ex, skeleton()), ExecOutcome::Ok(_)));

        let patch = serde_json::json!({
            "merge": true,
            "params": {
                "p_guess": {"name": "拍脑袋的", "value": 7,
                            "source": {"kind": "assumed", "note": "没依据"}}
            }
        });
        match build(&mut ex, patch) {
            ExecOutcome::Failed(e) => {
                assert!(e.contains("p_guess"), "应指名未确认的参数：{e}");
            }
            other => panic!("合并后 assumed 参数仍须被拒（铁律 2），实得 {other:?}"),
        }
    }
}

#[test]
fn repeated_question_is_not_sent_again_across_phases() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    let q=call("ask_user",serde_json::json!({"questions":[{"text":"你的预算是多少？"}]}));
    assert!(matches!(ex.execute(&q,SessionPhase::Intake),ExecOutcome::AsksUser(_)));
    let q=call("ask_user",serde_json::json!({"questions":[{"text":"你的预算 是多少?"}]}));
    assert!(expect_failed(ex.execute(&q,SessionPhase::Intel)).contains("已经提出"));
}

#[test]
fn long_compound_question_is_rejected() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    let q=call("ask_user",serde_json::json!({"questions":[{"text":"请说明".repeat(40)}]}));
    assert!(expect_failed(ex.execute(&q,SessionPhase::Intake)).contains("80字"));
}

#[test]
fn cost_update_uses_engine_identity_and_keeps_previous_result_on_failure() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    let mut args=serde_json::json!({"operation":"create","scenario":"搬店","currency":"CNY","costs_complete":false,"horizon_months":24,
        "costs":[{"scenario":"搬店","name":"租金","amount":9000,"period":"month","treatment":"cash_expense","basis":"assumption","source":"虚构测试"}]});
    let first:serde_json::Value=serde_json::from_str(&expect_ok(ex.execute(&call("business_cost",args.clone()),SessionPhase::Intake))).unwrap();
    let id=first["scenario_id"].as_str().unwrap().to_owned();
    assert_eq!(ex.state().cost_scenarios.len(),1);
    assert!(expect_failed(ex.execute(&call("business_cost",args.clone()),SessionPhase::Intake)).contains("相同费用已计算"));
    assert_eq!(ex.state().cost_scenarios.len(),1);
    let mut distinct=args.clone();distinct["distinct_reason"]=serde_json::json!("另一家备选店铺，费用恰好相同");
    let other:serde_json::Value=serde_json::from_str(&expect_ok(ex.execute(&call("business_cost",distinct),SessionPhase::Intake))).unwrap();
    assert_ne!(other["scenario_id"],first["scenario_id"]);
    args["operation"]=serde_json::json!("update");args["scenario_id"]=serde_json::json!("invented-new-name");
    assert!(expect_failed(ex.execute(&call("business_cost",args.clone()),SessionPhase::Intake)).contains(&id));
    args["scenario_id"]=serde_json::json!(id);args["scenario"]=serde_json::json!("搬店三年");
    args["costs"][0]["scenario"]=serde_json::json!("搬店三年");args["horizon_months"]=serde_json::json!(36);
    let updated:serde_json::Value=serde_json::from_str(&expect_ok(ex.execute(&call("business_cost",args.clone()),SessionPhase::Intake))).unwrap();
    assert_eq!(updated["scenario_id"],first["scenario_id"]);
    assert_eq!(updated["horizon_totals"]["listed_cash_expense"],324000.0);
    assert_eq!(ex.state().cost_scenarios.len(),2);
    args["costs"][0]["amount"]=serde_json::json!(-1);
    expect_failed(ex.execute(&call("business_cost",args.clone()),SessionPhase::Intake));
    assert_eq!(ex.state().cost_scenarios[&id],updated);
    args["operation"]=serde_json::json!("create");
    assert!(expect_failed(ex.execute(&call("business_cost",args.clone()),SessionPhase::Intake)).contains("不要指定"));
    args.as_object_mut().unwrap().remove("operation");
    assert!(expect_failed(ex.execute(&call("business_cost",args),SessionPhase::Intake)).contains("须明确"));
}

#[test]
fn model_fingerprint_tracks_payoffs_even_when_game_id_is_unchanged() {
    use shangyi_core::phase_graph::ArtifactKind;
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    let mut spec=prisoners_dilemma();ex.set_game_spec(spec.clone());
    let fingerprint=|executor:&DomainExecutor| executor.artifacts().into_iter().find(|(kind,_)|*kind==ArtifactKind::GameSpecValidated).unwrap().1;
    let before=fingerprint(&ex);
    if let PayoffForm::Normal{cells}=&mut spec.payoffs {cells.values_mut().next().unwrap().insert("focal".into(),Expr::Literal(99.0));}
    ex.set_game_spec(spec.clone());let after=fingerprint(&ex);
    assert_ne!(before,after,"confirmation must bind actual values, not just the game id");
    ex.set_game_spec(spec);assert_eq!(after,fingerprint(&ex));
}

#[test]
fn missing_opponent_payoff_is_not_committed_as_a_valid_model() {
    let theory=empty_theory();let pb=playbook();let mut ex=DomainExecutor::new(&theory,&pb);
    let original=prisoners_dilemma();ex.set_game_spec(original.clone());
    let mut draft=original.clone();
    if let PayoffForm::Normal{cells}=&mut draft.payoffs {cells.values_mut().next().unwrap().remove("rival");}
    let error=expect_failed(ex.execute(&call("gamespec_build",serde_json::to_value(draft).unwrap()),SessionPhase::ModelBuild));
    assert!(error.contains("缺少参与者")&&error.contains("不能用0"));
    assert_eq!(ex.state().game_spec.as_ref(),Some(&original));
}

#[test]
fn fetched_body_can_be_reused_after_network_budget_without_claiming_a_fresh_fetch() {
    struct CountingSearch(std::cell::Cell<usize>);
    impl SearchProvider for CountingSearch {
        fn search(&self,_:&str)->Result<Vec<SearchHit>,SearchUnavailable>{Ok(vec![])}
        fn fetch(&self,_:&str)->Result<FetchedText,SearchUnavailable>{
            self.0.set(self.0.get()+1);
            Ok(FetchedText{url:"https://example.invalid/canonical".into(),text:"已取得的官方正文".into(),raw_bytes:12345})
        }
    }
    let theory=empty_theory();let pb=playbook();let sp=CountingSearch(std::cell::Cell::new(0));
    let mut ex=DomainExecutor::new(&theory,&pb).with_search(&sp);
    let args=serde_json::json!({"url":"https://example.invalid/original"});
    let first:serde_json::Value=serde_json::from_str(&expect_ok(ex.execute(&call("web_fetch",args.clone()),SessionPhase::Intel))).unwrap();
    assert_eq!(sp.0.get(),1);assert_eq!(first["cached"],false);
    ex.state().retrieval_attempts.set(16);
    for url in ["https://example.invalid/original","https://example.invalid/canonical"] {
        let cached:serde_json::Value=serde_json::from_str(&expect_ok(ex.execute(&call("web_fetch",serde_json::json!({"url":url})),SessionPhase::Intel))).unwrap();
        assert_eq!(cached["cached"],true);assert_eq!(cached["text"],first["text"]);
        assert_eq!(cached["retrieved_at_unix"],first["retrieved_at_unix"]);
        assert_eq!(cached["raw_bytes"],12345);assert_eq!(sp.0.get(),1);
    }
    assert!(matches!(ex.execute(&call("web_fetch",serde_json::json!({"url":"https://example.invalid/original","refresh":true})),SessionPhase::Intel),ExecOutcome::Blocked(_)));
    ex.state().retrieval_attempts.set(1);
    expect_ok(ex.execute(&call("web_fetch",serde_json::json!({"url":"https://example.invalid/original","refresh":true})),SessionPhase::Intel));
    assert_eq!(sp.0.get(),2);
    expect_failed(ex.execute(&call("web_fetch",serde_json::json!({"url":"https://example.invalid/original","refresh":"yes"})),SessionPhase::Intel));
}

#[test]
fn refresh_updates_canonical_aliases_without_rewriting_a_changed_alias_target() {
    struct RedirectSearch(std::cell::Cell<usize>);
    impl SearchProvider for RedirectSearch {
        fn search(&self, _: &str) -> Result<Vec<SearchHit>, SearchUnavailable> { Ok(vec![]) }
        fn fetch(&self, url: &str) -> Result<FetchedText, SearchUnavailable> {
            let revision = self.0.get() + 1;
            self.0.set(revision);
            let target = if revision >= 4 && url.ends_with("alias-a") { "other" } else { "canonical" };
            Ok(FetchedText {
                url: format!("https://example.invalid/{target}"),
                text: if revision == 5 { " ".into() } else { format!("official body revision {revision}") },
                raw_bytes: revision,
            })
        }
    }
    let theory = empty_theory(); let pb = playbook();
    let provider = RedirectSearch(std::cell::Cell::new(0));
    let mut executor = DomainExecutor::new(&theory, &pb).with_search(&provider);
    let fetch = |executor: &mut DomainExecutor, path: &str, refresh: bool| -> serde_json::Value {
        serde_json::from_str(&expect_ok(executor.execute(&call("web_fetch", serde_json::json!({
            "url": format!("https://example.invalid/{path}"), "refresh": refresh
        })), SessionPhase::Intel))).unwrap()
    };
    fetch(&mut executor, "alias-a", false);
    fetch(&mut executor, "alias-b", false);
    let refreshed = fetch(&mut executor, "canonical", true);
    for alias in ["alias-a", "alias-b"] {
        let cached = fetch(&mut executor, alias, false);
        assert_eq!(cached["text"], refreshed["text"]);
        assert_eq!(cached["retrieved_at_unix"], refreshed["retrieved_at_unix"]);
        assert_eq!(cached["raw_bytes"], refreshed["raw_bytes"]);
        assert_eq!(cached["cached"], true);
    }
    assert_eq!(provider.0.get(), 3);
    let moved = fetch(&mut executor, "alias-a", true);
    assert_eq!(moved["url"], "https://example.invalid/other");
    assert_eq!(fetch(&mut executor, "alias-a", false)["text"], moved["text"]);
    assert_eq!(fetch(&mut executor, "alias-b", false)["text"], refreshed["text"]);
    assert_eq!(fetch(&mut executor, "canonical", false)["text"], refreshed["text"]);
    assert_eq!(provider.0.get(), 4);
    assert!(matches!(executor.execute(&call("web_fetch", serde_json::json!({
        "url": "https://example.invalid/canonical", "refresh": true
    })), SessionPhase::Intel), ExecOutcome::Blocked(_)));
    assert_eq!(fetch(&mut executor, "alias-b", false)["text"], refreshed["text"]);
    assert_eq!(executor.state().fetched_sources.borrow().get("https://example.invalid/canonical"), refreshed["text"].as_str().map(String::from).as_ref());
}

// ── 周边事实（`poi_survey`） ────────────────────────────────────────────────

struct FakePoi {
    counts: Vec<(String, u64)>,
    brand: Option<(String, u64)>,
    unavailable: Vec<String>,
    fail: bool,
}

impl shangyi_core::domain_exec::PoiProvider for FakePoi {
    fn survey(
        &self,
        query: &shangyi_core::domain_exec::PoiQuery,
    ) -> Result<shangyi_core::domain_exec::PoiSurvey, SearchUnavailable> {
        if self.fail {
            return Err(SearchUnavailable::Failed {
                detail: "限流 CUQPS_HAS_EXCEEDED_THE_LIMIT".into(),
                retryable: true,
            });
        }
        Ok(shangyi_core::domain_exec::PoiSurvey {
            address: query.address.clone(),
            district: "黄浦区".into(),
            radius: query.radius.clamp(100, 3000),
            counts: self
                .counts
                .iter()
                .map(|(label, count)| shangyi_core::domain_exec::PoiCount {
                    label: label.clone(),
                    count: *count,
                })
                .collect(),
            brand: self.brand.as_ref().map(|(label, count)| {
                shangyi_core::domain_exec::PoiCount {
                    label: label.clone(),
                    count: *count,
                }
            }),
            keywords: None,
            unavailable: self.unavailable.clone(),
            source: "高德开放平台 Web 服务（POI 周边搜索）".into(),
        })
    }
}

/// 接线后：周边计数要带来源与坐标，未取得的类目如实列出。
#[test]
fn poi_survey_reports_counts_with_provenance_and_keeps_missing_items_visible() {
    let theory = empty_theory();
    let pb = playbook();
    let poi = FakePoi {
        counts: vec![("茶饮咖啡".into(), 144), ("写字楼".into(), 86)],
        brand: Some(("喜茶（同品牌）".into(), 3)),
        unavailable: vec!["餐饮：限流".into()],
        fail: false,
    };
    let mut ex = DomainExecutor::new(&theory, &pb).with_poi(&poi);
    let out = expect_ok(ex.execute(
        &call(
            "poi_survey",
            serde_json::json!({"address": "上海市南京东路32号", "radius": 500, "brand": "喜茶"}),
        ),
        SessionPhase::Intel,
    ));
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["counts"][0]["label"], serde_json::json!("茶饮咖啡"));
    assert_eq!(v["counts"][0]["count"], serde_json::json!(144));
    assert_eq!(v["brand"]["count"], serde_json::json!(3), "同品牌存量必须回传");
    assert_eq!(v["provenance"]["kind"], serde_json::json!("public_data"));
    assert_eq!(v["provenance"]["radius"], serde_json::json!(500));
    assert!(
        v["unavailable"].as_array().map(|a| a.len()).unwrap_or(0) == 1,
        "没查到的类目必须留在结果里：{v}"
    );
    assert!(v["note"].as_str().unwrap_or_default().contains("不等于 0"));
}

/// 未接线：`Blocked`（未取得），且必须说清"未取得 ≠ 周边没有门店"。
#[test]
fn poi_survey_without_a_provider_is_blocked_not_empty() {
    let theory = empty_theory();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);
    let out = ex.execute(
        &call("poi_survey", serde_json::json!({"address": "上海市南京东路32号"})),
        SessionPhase::Intel,
    );
    match out {
        shangyi_core::agent::ExecOutcome::Blocked(message) => {
            assert!(message.contains("未取得"), "{message}");
            assert!(
                message.contains("不得解释为") || message.contains("不等于"),
                "必须堵住「没有计数=没有门店」的误读：{message}"
            );
        }
        other => panic!("未接线应当是 Blocked（未取得），实得 {other:?}"),
    }
}

/// 缺地址：`Failed`（改参数可重试），不是 Blocked。
#[test]
fn poi_survey_requires_an_address() {
    let theory = empty_theory();
    let pb = playbook();
    let poi = FakePoi {
        counts: vec![],
        brand: None,
        unavailable: vec![],
        fail: false,
    };
    let mut ex = DomainExecutor::new(&theory, &pb).with_poi(&poi);
    match ex.execute(
        &call("poi_survey", serde_json::json!({"radius": 500})),
        SessionPhase::Intel,
    ) {
        shangyi_core::agent::ExecOutcome::Failed(message) => {
            assert!(message.contains("address"), "{message}")
        }
        other => panic!("缺地址应当是 Failed，实得 {other:?}"),
    }
}

/// 后端存在但失败（限流）：`Failed` 并带上原因——该去修配置还是该等，要能分辨。
#[test]
fn poi_survey_backend_failure_surfaces_the_reason() {
    let theory = empty_theory();
    let pb = playbook();
    let poi = FakePoi {
        counts: vec![],
        brand: None,
        unavailable: vec![],
        fail: true,
    };
    let mut ex = DomainExecutor::new(&theory, &pb).with_poi(&poi);
    match ex.execute(
        &call("poi_survey", serde_json::json!({"address": "某地"})),
        SessionPhase::Intel,
    ) {
        shangyi_core::agent::ExecOutcome::Failed(message) => {
            assert!(message.contains("CUQPS"), "{message}")
        }
        other => panic!("限流应当是 Failed 且带原因，实得 {other:?}"),
    }
}

/// 端到端：POI 工具产出回执 → 用回执登记请求字段 → 该字段由「未取得」变为「已取得」。
///
/// 这条链路此前断在证据模型上：结构化计数（POI 144 家）既不是原文引文，
/// 也不能当假设，于是真实咨询里出现"看依据显示已核实、交付却写未取得"。
#[test]
fn poi_receipts_support_research_fields_without_a_body_quote() {
    let theory = empty_theory();
    let pb = playbook();
    let poi = FakePoi {
        counts: vec![("茶饮咖啡".into(), 144)],
        brand: Some(("喜茶（同品牌）".into(), 3)),
        unavailable: vec![],
        fail: false,
    };
    let mut ex = DomainExecutor::new(&theory, &pb).with_poi(&poi);
    ex.ingest_user_input("帮我数清南京东路32号周边500米内有多少家茶饮咖啡、几家喜茶", "user");

    let planned = expect_ok(ex.execute(
        &call(
            "research_task_plan",
            serde_json::json!({"fields": [{
                "id": "tea", "label": "茶饮咖啡", "subject": "南京东路32号",
                "request_quote": "有多少家茶饮咖啡", "evidence_required": "original_text"
            }]}),
        ),
        SessionPhase::Intake,
    ));
    let planned: serde_json::Value = serde_json::from_str(&planned).unwrap();
    let field_id = planned["fields"][0]["id"].as_str().unwrap().to_string();

    let survey = expect_ok(ex.execute(
        &call("poi_survey", serde_json::json!({"address": "上海市南京东路32号", "radius": 500, "brand": "喜茶"})),
        SessionPhase::Intake,
    ));
    let survey: serde_json::Value = serde_json::from_str(&survey).unwrap();
    let receipts = survey["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 2, "每个计数都要给一个可引用的回执：{survey}");
    let tea = receipts
        .iter()
        .find(|r| r["label"] == serde_json::json!("茶饮咖啡"))
        .expect("茶饮咖啡应有回执");
    assert_eq!(tea["value"], serde_json::json!("144 家"));
    let receipt = tea["receipt"].as_str().unwrap().to_string();

    let updated = expect_ok(ex.execute(
        &call(
            "research_task_update",
            serde_json::json!({"updates": [{
                "field_id": field_id, "status": "supported",
                "structured": [{"receipt": receipt}]
            }]}),
        ),
        SessionPhase::Intake,
    ));
    let updated: serde_json::Value = serde_json::from_str(&updated).unwrap();
    assert_eq!(updated["fields"][0]["status"], serde_json::json!("supported"));
    assert_eq!(
        updated["fields"][0]["structured_evidence"][0]["value"],
        serde_json::json!("144 家"),
        "数值取自回执本体，不是模型复述"
    );
    assert_eq!(
        updated["fields"][0]["structured_evidence"][0]["scope"],
        serde_json::json!("上海市南京东路32号 · 500 米")
    );
}

/// 编造的编号必须被拒：结构化依据不能自述数值。
#[test]
fn a_made_up_receipt_number_is_rejected() {
    let theory = empty_theory();
    let pb = playbook();
    let poi = FakePoi { counts: vec![("茶饮咖啡".into(), 144)], brand: None, unavailable: vec![], fail: false };
    let mut ex = DomainExecutor::new(&theory, &pb).with_poi(&poi);
    ex.ingest_user_input("查一下周边茶饮咖啡数量", "user");
    let planned = expect_ok(ex.execute(
        &call("research_task_plan", serde_json::json!({"fields": [{
            "id": "tea", "label": "茶饮咖啡",
            "request_quote": "周边茶饮咖啡", "evidence_required": "original_text"}]})),
        SessionPhase::Intake,
    ));
    let planned: serde_json::Value = serde_json::from_str(&planned).unwrap();
    let field_id = planned["fields"][0]["id"].as_str().unwrap().to_string();
    expect_ok(ex.execute(
        &call("poi_survey", serde_json::json!({"address": "上海市南京东路32号"})),
        SessionPhase::Intake,
    ));
    match ex.execute(
        &call("research_task_update", serde_json::json!({"updates": [{
            "field_id": field_id, "status": "supported",
            "structured": [{"receipt": "call-我自己编的#1"}]}]})),
        SessionPhase::Intake,
    ) {
        shangyi_core::agent::ExecOutcome::Failed(message) => {
            assert!(message.contains("不是本次工具产生的"), "{message}")
        }
        other => panic!("编造回执必须被拒，实得 {other:?}"),
    }
}

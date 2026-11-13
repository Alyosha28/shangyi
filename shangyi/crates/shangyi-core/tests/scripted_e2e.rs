//! **脚本化 LLM 的全链路 E2E**——无需 API 即可验证整条流水线。
//!
//! # 为什么需要它（这是本项目最缺的一块验证能力）
//!
//! 整轮开发里发现的**结构性缺陷**（阶段无法推进、产物登记不上、工具调不动）
//! 有一个共同点：**它们只在真实 LLM 运行时才暴露**，而真实运行：
//! - 花钱（一次完整咨询实测 $0.0005–0.01）
//! - 慢（5–15 分钟）
//! - 噪声大（模型的措辞每次不同，失败原因常与缺陷无关）
//! - **可能被外部因素阻塞**（实测：DeepSeek 余额为负、GLM 免费档能力不足）
//!
//! 而单元测试又看不见它们——每个工具各自都对、各自的断言都过，
//! 是**串起来**才断的。
//!
//! 本文件填上这个缺口：用**假模型 + 真执行器**跑完整链路。
//!
//! # 假模型的设计（这是本文件的核心）
//!
//! 它不是"按固定序列回放响应"——那种脚本一改流程就失效，而且**测不出
//! 指引是否充分**（脚本自己知道该调什么，不需要指引）。
//!
//! 它**读请求里的阶段指引**，按指引行事——就像一个听话的真实模型：
//!
//! 1. 从最后一条 `system` 消息解析出当前阶段
//! 2. 看该阶段要求的产物是否已在执行器里——不在，就调**指引里点名的那个工具**
//! 3. 产物齐了、该阶段又有未结算的检查点——就调 `checkpoint` 提交清单
//! 4. 都做完了——回一段文本收尾
//!
//! 于是这个测试**同时验证三件事**：
//! - 指引点名了正确的工具（若点名错，第 2 步会调到不存在的工具）
//! - 工具的**最小合法输入**真的能产出所需产物（若不能，第 2 步失败）
//! - 闸门、检查点、阶段推进三者串起来走得通（否则到不了 archived）
//!
//! **这正是此前 6 个结构性缺陷的形态**——它们都能被这个测试抓住。

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use shangyi_core::agent::{AgentLoop, LoopConfig};
use shangyi_core::case_log::{CaseHeader, CaseLog};
use shangyi_core::domain_exec::DomainExecutor;
use shangyi_core::phase_graph::{
    CheckpointId, CheckpointRequest, PhaseMachine, SessionPhase,
};
use shangyi_core::runner::{CheckpointAnswer, RunReport, SessionRunner, UserIo};
use shangyi_core::consultation::ConsultationMode;
use shangyi_kb::TheoryLibrary;
use shangyi_llm::{Endpoint, InputItem, LlmError, LlmClient, Transport};
use shangyi_playbooks::{Playbook, PlaybookMeta};
use shangyi_tools::ToolRegistry;

// ─────────────────────────── 假模型 ───────────────────────────

/// 一个**按指引行事**的假模型。
///
/// 它从请求里读出阶段指引，并据此决定下一步——因此它同时也是
/// 「指引是否足够」的检验器。
struct PolicyModel {
    /// 本阶段内已问过几次（用于决定"产出产物"还是"提交检查点"）。
    step_in_phase: Cell<u32>,
    last_phase: RefCell<String>,

    /// 记录每一阶段的工具调用序列（供断言与诊断）。
    log: RefCell<Vec<(String, String)>>,
}

impl PolicyModel {
    fn new() -> Self {
        Self {
            step_in_phase: Cell::new(0),
            last_phase: RefCell::new(String::new()),
            log: RefCell::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<(String, String)> {
        self.log.borrow().clone()
    }
}

impl Transport for PolicyModel {
    fn post_json(
        &self,
        _url: &str,
        _headers: &[(String, String)],
        body: &serde_json::Value,
    ) -> Result<(u16, String), LlmError> {
        // ── 从请求里读阶段指引 ──
        //
        // 真实模型也是这样知道自己在哪个阶段的——故这里必须走同一条路。
        let guidance = body
            .get("input")
            .and_then(|i| i.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter(|it| it.get("role").and_then(|r| r.as_str()) == Some("system"))
                    .filter_map(|it| it.get("content").and_then(|c| c.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        let phase = parse_phase(&guidance);
        // 阶段变了 ⇒ 步数归零
        if *self.last_phase.borrow() != phase {
            *self.last_phase.borrow_mut() = phase.clone();
            self.step_in_phase.set(0);
        }
        let step = self.step_in_phase.get();
        self.step_in_phase.set(step + 1);

        // ── 按阶段决定动作 ──
        // ── 策略：**按指引行事** —— 工具名从指引解析，参数形状按阶段 ──
        //
        // 「工具名从指引解析」是刻意的：它让本测试真的能验证
        // **指引点名的工具是不是能用**。若指引写错工具名，
        // 假模型会调到不存在的工具而失败（已用变异验证过）。
        //
        // 「参数形状硬编码」也是刻意的：参数必须符合工具的 schema，
        // 而那**无法从指引推导**（指引不该、也不必复述每个工具的字段）。
        // 真实模型是从工具的 JSON schema 得知字段的——本测试用固定形状模拟它。
        let deliverable = tool_named_in(&guidance);
        let d = deliverable.as_deref().unwrap_or("");

        let (tool, args): (&str, serde_json::Value) = match (phase.as_str(), step) {
            // intake 无产物要求（见 `PhaseGraph::produces` 的注释），
            // 故直接收尾——这也验证了"产出为空的阶段不该被闸门卡住"。
            ("intake", _) => ("", serde_json::json!({})),

            // intel：两份画像（用指引点名的那个工具）+ cp1
            //
            // 两份画像都是刻意的：`intel` 的产物是**画像集合**，
            // 一份也够，但两份能验证"同一工具多步调用"这条常见路径。
            ("intel", 0) => (d, focal_profile()),
            ("intel", 1) => (d, rival_profile()),
            ("intel", 2) => ("checkpoint", cp_items("cp1", "情报与画像")),
            ("intel", _) => ("", serde_json::json!({})),

            // model_build：博弈（指引点名的工具）+ cp2
            ("model_build", 0) => (d, minimal_game()),
            ("model_build", 1) => ("checkpoint", cp_items("cp2", "博弈结构")),
            ("model_build", _) => ("", serde_json::json!({})),

            // solve：产出一解集即收尾
            //
            // ⚠️ 必须"一次"：初版写成每步都调，于是假模型在 solve 阶段连调 10 次
            // 同一个工具直到触顶 `max_steps_per_turn`。而那次失败**恰好演示了
            // 本测试的价值**——它精确指出"这个阶段的策略让模型停不下来"，
            // 而真实运行里同类现象是"反复调同一个幂等工具却从不收尾"
            // （实测出现过：recommend 里调 9 次 `sensitivity_scan`）。
            ("solve", 0) => (d, serde_json::json!({})),
            ("solve", _) => ("", serde_json::json!({})),

            // select：登记判据 → 融合 → cp3
            //
            // 这里两步用的是**不同**工具（先 `select_equilibria` 后 `select_fuse`），
            // 故只能在指引里找到"交付工具"（`select_fuse`）；判据登记那步
            // 用硬编码名（它也在指引里，但解析器只取第一个）。
            ("select", 0) => (
                "select_equilibria",
                serde_json::json!({"selector": "risk_dom", "scores": [1.0]}),
            ),
            ("select", 1) => (d, serde_json::json!({})),
            ("select", 2) => ("checkpoint", cp_items("cp3", "结果预览")),
            ("select", _) => ("", serde_json::json!({})),

            // consequence（whatif 模式）：推演用户给定的动作 ⇒ 产出后果报告
            //
            // 动作必须是**已声明的策略名**——`consequence_predict` 会做
            // 精确 → 唯一子串 → 拒绝的三级匹配（见该方法文档）。
            ("consequence", 0) => (d, serde_json::json!({"decision": "维持"})),
            // ⚠️ cp3 落在**该模式的"结果产出阶段"**（见 `PhaseGraph` 的映射）：
            // scenario → select、whatif → consequence、discover → discover。
            // 本测试覆盖三个模式，故这三处都要提交 cp3——
            // 而缺失它正是脚本化 E2E 在 whatif/discover 上卡死的真因：
            // **产物齐了但检查点未结算，闸门同样不放行。**
            ("consequence", 1) => ("checkpoint", cp_items("cp3", "结果预览")),
            ("consequence", _) => ("", serde_json::json!({})),

            // discover（discover 模式）：候选 + 抱负水平 ⇒ 引擎判定满意与否
            //
            // ⚠️ `outcomes` 的 P10/P50/P90 **由确定性管线产出**（现实中是
            // `consequence_predict` 的结果）。本测试直接给出这些数是为了
            // 保持脚本可读——它们是**输入**而非结论，引擎仍会自己做判定。
            ("discover", 0) => (d, discover_input()),
            // cp3 在 discover 模式落在 discover 阶段（同 consequence 那条注释）
            ("discover", 1) => ("checkpoint", cp_items("cp3", "结果预览")),
            ("discover", _) => ("", serde_json::json!({})),

            ("recommend", 0) => (
                d,
                serde_json::json!({
                    "headline": "维持价格带",
                    "actions": [{
                        "action": "维持当前价格",
                        "rationale": "引擎解出的均衡指向维持"
                    }]
                }),
            ),
            ("recommend", _) => ("", serde_json::json!({})),

            ("report", 0) => (d, serde_json::json!({"case_id": "scripted-e2e"})),
            ("report", _) => ("", serde_json::json!({})),

            // archived：归档进案例库（**终态的产物**）。
            //
            // 这一步此前没有被假模型覆盖，于是 `case_store_write` 里
            // "硬性要求 select 分布"的缺陷躲过了第一轮——
            // 而它让 whatif/discover 模式的 **archived 不可达**（终态收不了尾）。
            // 现在覆盖它：三个模式都必须能归档。
            ("archived", 0) => (
                d,
                serde_json::json!({
                    "case_id": "scripted-e2e",
                    "client": "脚本化客户",
                    "question": "脚本化决策问题",
                    // 必填，且**假模型必须如实填**——这也顺带验证了
                    // 指引里那句"必须给 provenance"足够让模型知道该填什么。
                    "provenance": "exercise"
                }),
            ),
            ("archived", _) => ("", serde_json::json!({})),

            // 未预期的阶段 ⇒ 空文本
            _ => ("", serde_json::json!({})),
        };

        if tool.is_empty() {
            self.log.borrow_mut().push((phase.clone(), "(text)".into()));
            return Ok((200, text_response("本阶段完成。")));
        }
        self.log.borrow_mut().push((phase.clone(), tool.to_string()));
        // 诊断开关：`SHANGYI_SCRIPTED_TRACE=1` 时打印每步（默认关，不污染输出）
        if std::env::var("SHANGYI_SCRIPTED_TRACE").is_ok() {
            eprintln!("[scripted] {phase} → {tool} args={}", args);
        }
        Ok((200, tool_call_response(tool, &args)))
    }
}

/// 从指引里**解析出被点名的工具名**（反引号包裹的第一个工具名）。
///
/// # 为什么必须从指引里读，而不是硬编码（这是我踩过的坑）
///
/// 本文件开头的文档声称"假模型读指引行事，若指引点名错会调到不存在的工具"。
/// 而**第一版的实现里工具名是硬编码的**——它只从指引解析阶段名，
/// 工具名写死在 match 分支里。于是那句声称是**假的**：
/// 我做了个变异（把 intel 指引点名的工具改成不存在的名字），
/// 结构断言抓住了，**脚本化 E2E 却通过了**——因为它压根没读那个名字。
///
/// 这是"文档描述了未实现的行为"，与我在别处批评过的
/// `LiveSearch` 注释（写着"逐类转述"而实现只转字符串）同型。
/// 修法只有一条：**让实现追上声称**。
///
/// 于是本函数把工具名从指引里解析出来——假模型因此真的在"按指引行事"，
/// 指引点名错工具它就会调到不存在的工具并失败。
fn tool_named_in(guidance: &str) -> Option<String> {
    // **优先匹配显式契约**：`本阶段必须以 \`工具名\` 收尾`。
    //
    // # 为什么要有这个显式契约（而不是"取第一个反引号里的工具名"）
    //
    // 第一版解析器取的是"指引里第一个已知工具名"——而 intel 的指引第一句是
    // "用 `web_search`/`web_fetch` 取证"，于是它解析出 `web_search`，
    // 假模型就去调检索，而**检索需要真实后端** ⇒ `Blocked` ⇒ 测试失败。
    //
    // 那个失败说明了一件事：**指引里出现的第一个工具名不是交付工具**。
    // 取证工具、校验工具都可能先出现，而"哪个是交付物"在自然语言里
    // **没有唯一答案**——除非我们**明确规定一个说法**。
    //
    // 故建立契约：所有阶段的指引都用同一句话点明交付工具——
    // 「**本阶段必须以 `<工具名>` 收尾**」。
    // 它同时服务两个读者：
    // - **模型**：一句话就知道该用哪个工具结束本阶段（而不是从一堆工具名里猜）
    // - **本测试**：有唯一、可解析的答案
    //
    // 契约由 `full_chain` 的 `phase_guidance_states_the_deliverable_contract`
    // 断言（每个有产物的阶段都必须有这句话）。
    const MARK: &str = "本阶段必须以";
    if let Some(pos) = guidance.find(MARK) {
        let tail = &guidance[pos + MARK.len()..];
        if let Some(start) = tail.find('`') {
            let after = &tail[start + 1..];
            if let Some(end) = after.find('`') {
                let name = &after[..end];
                if KNOWN_TOOLS.contains(&name) {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// 已知的工具名（与 `ToolRegistry::standard()` 对齐）。
///
/// 保留它是为了让上面的解析器**只认真正的工具名**：指引里也会出现
/// 别的反引号内容（如 `` `select` `` 这门阶段名、`` `equilibrium_set` `` 产物名），
/// 而把它们当工具名会调出不存在的工具——那是**测试自己的假失败**。
const KNOWN_TOOLS: &[&str] = &[
    "participant_profile_update",
    "gamespec_build",
    "gamespec_validate",
    "solve_equilibria",
    "select_equilibria",
    "select_fuse",
    "consequence_predict",
    "discover_satisfice",
    "recommend_build",
    "report_render",
    "checkpoint",
    "vn_map",
    "pars_check",
    "ask_user",
    "web_search",
    "web_fetch",
    "case_store_write",
    "kb_retrieve",
];

fn parse_phase(guidance: &str) -> String {
    // 指引形如 "当前阶段：intel（公开数据搜集与参与者画像）。…"
    if let Some(rest) = guidance.split("当前阶段：").nth(1) {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            return name;
        }
    }
    // 解析不出阶段时返回空串 ⇒ 假模型回文本 ⇒ 测试会失败并暴露问题
    String::new()
}

// ── 各工具的最小合法输入 ──

fn focal_profile() -> serde_json::Value {
    serde_json::json!({
        "participant_id": "us",
        "value_net_role": "focal",
        "relation_to_game": "player",
        "common": {
            "interests": ["毛利率"],
            "resources": [],
            "decision_style": "稳健",
            "power_asymmetry": "对等"
        },
        "role_specific": {"role": "substitutor", "details": {
            "lambda_qre": null, "level_k": null, "aggressiveness": null,
            "cost_mu": null, "cost_sigma": null
        }},
        "updated_at_ms": 0
    })
}

fn rival_profile() -> serde_json::Value {
    serde_json::json!({
        "participant_id": "rival",
        "value_net_role": "substitutor",
        "relation_to_game": "player",
        "common": {
            "interests": ["份额"],
            "resources": [],
            "decision_style": "跟随",
            "power_asymmetry": "对等"
        },
        "role_specific": {"role": "substitutor", "details": {
            "lambda_qre": null, "level_k": null, "aggressiveness": null,
            "cost_mu": null, "cost_sigma": null
        }},
        "updated_at_ms": 0
    })
}

/// 最小可解的 2×2 博弈（囚徒困境式的支付）。
fn minimal_game() -> serde_json::Value {
    serde_json::json!({
        "game_id": "scripted",
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
    })
}

/// `discover_satisfice` 的最小合法输入。
///
/// 两个候选各带准则上的 P10/P50/P90 分位数与一个抱负水平——
/// 引擎据此**计算**达标概率并判定满意与否（不是我们下结论）。
fn discover_input() -> serde_json::Value {
    serde_json::json!({
        "aspiration": {
            "levels": {"利润": 1.0},
            "default_threshold": 0.5
        },
        "candidates": [
            {
                "id": "c1",
                "description": "维持价格",
                "outcomes": {"利润": {"p10": 2.0, "p50": 3.0, "p90": 4.0}},
                "llm_proposed": false
            },
            {
                "id": "c2",
                "description": "降价换量",
                "outcomes": {"利润": {"p10": 0.5, "p50": 1.5, "p90": 2.5}},
                "llm_proposed": true
            }
        ]
    })
}

fn cp_items(id: &str, label: &str) -> serde_json::Value {
    serde_json::json!({
        "items": [{
            "id": format!("{id}-item"),
            "content": format!("{label}已核对"),
            "source": "脚本化 E2E",
            "proposed_value": "确认"
        }]
    })
}

fn tool_call_response(name: &str, args: &serde_json::Value) -> String {
    serde_json::json!({
        "id": "resp",
        "status": "completed",
        "output": [{
            "type": "function_call",
            "call_id": format!("call_{name}"),
            "name": name,
            "arguments": args.to_string(),
        }],
        "usage": {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120}
    })
    .to_string()
}

fn text_response(text: &str) -> String {
    serde_json::json!({
        "id": "resp",
        "status": "completed",
        "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
        "usage": {"input_tokens": 100, "output_tokens": 20, "total_tokens": 120}
    })
    .to_string()
}

// ─────────────────────────── 用户交互的替身 ───────────────────────────

/// 自动批准检查点的用户替身。
///
/// # 与生产里的 `AutoApprove` 同款语义
///
/// `user_actor` 必须是 `"auto"` 而非 `"user"`——否则日志会把"无人审阅"
/// 记成"用户已批准"，而那是审计上的伪造（设计文档 §7.6 的红线）。
struct ApprovingIo {
    asked: RefCell<Vec<CheckpointId>>,
}

impl UserIo for ApprovingIo {
    fn present_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer {
        self.asked.borrow_mut().push(req.id);
        CheckpointAnswer::Approve
    }

    fn ask(
        &mut self,
        questions: &[shangyi_core::agent::UserQuestion],
    ) -> Vec<(String, String)> {
        // 每个问题给一个**明确非空**的答案：空答案会让上游标「未取得」，
        // 而本测试要验证的是"信息齐备时链路能走通"。
        questions
            .iter()
            .map(|q| (q.id.clone(), "脚本化答案".to_string()))
            .collect()
    }

    fn inform(&mut self, _msg: &str) {}

    fn is_automated(&self) -> bool {
        true
    }
}

// ─────────────────────────── 测试 ───────────────────────────

fn tmp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!(
        "shangyi-scripted-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&d).expect("创建临时目录");
    d
}

fn playbook() -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: "scripted".into(),
            name: "脚本化".into(),
            description: String::new(),
            order: 0,
            version: 1,
        },
        applicable_when: vec![],
        fails_when: vec![],
        required_inputs: vec![],
        value_net: None,
        intake: vec![],
        models: vec!["differentiated_bertrand".into()],
        selectors: BTreeMap::new(),
        // 白名单留空 = 不额外限制（本测试要验证的是链路，不是剧本过滤）
        tool_allowlist: vec![],
        report_vars: BTreeMap::new(),
    }
}

/// **全链路走到 archived**——脚本化 LLM + 真实执行器。
///
/// 这是本项目最便宜的"整条流水线是否还活着"的检验：
/// 它不需要 API、不需要网络、秒级完成，且失败时会指出**卡在哪个阶段**。
/// **三种模式的全链路**——它们的阶段路径不同，而此前只有 scenario 被验证过。
///
/// | 模式 | 路径特点 |
/// |---|---|
/// | `scenario` | 标准路径：… → select → recommend → report |
/// | `discover` | **跳过 select**，走 discover（找满意解而非选均衡） |
/// | `whatif` | 走 consequence（条件后果推演） |
///
/// 这三条路径由 `PhaseGraph::for_mode` 给出，而"某条路径上的阶段缺少产出工具"
/// 正是此前 `report` 不可达的真因。故每个模式都必须走一遍。
fn run_mode_to_completion(mode: ConsultationMode, tag: &str) -> Vec<(String, String)> {
    let dir = tmp_dir(tag);
    let header = CaseHeader {
        case_id: format!("scripted-{tag}"),
        mode: mode.as_str().into(),
        playbook_id: "scripted".into(),
        created_at: "2026-09-19T00:00:00Z".into(),
        schema_version: CaseHeader::SCHEMA_VERSION,
    };
    let log = CaseLog::open(&dir, &header).expect("案例日志可开");
    let reg = ToolRegistry::standard();
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb).with_mode(mode);

    let transport = std::sync::Arc::new(PolicyModel::new());
    let ep = Endpoint {
        base_url: "https://example.invalid".into(),
        api_key_env: "SHANGYI_TEST_KEY_UNUSED".into(),
        wire: shangyi_llm::Wire::Responses,
        echo_reasoning: false,
    };
    let client = LlmClient::new(ep, SharedPolicy(transport.clone())).with_key("test");

    let machine = PhaseMachine::new(mode);
    let mut cfg = LoopConfig::new("scripted", &format!("scripted-{tag}"));
    cfg.max_steps_per_turn = 12;
    cfg.context_warn_tokens = u64::MAX;

    let agent = AgentLoop::new(client, &reg, &mut ex, cfg).with_playbook(&pb);
    let mut runner = SessionRunner::new(agent, log, machine)
        .with_stage_turn_budget(8)
        .with_auto_advance(true);
    let mut io = ApprovingIo {
        asked: RefCell::new(Vec::new()),
    };

    let report = runner
        .run(
            vec![InputItem::user(format!("开始一次 {} 模式咨询", mode.as_str()))],
            &mut io,
            "auto",
        )
        .expect("runner 不应返回 Err");

    let calls = transport.calls();
    match &report {
        RunReport::Completed { phase, .. } => {
            assert_eq!(
                *phase,
                SessionPhase::Archived,
                "[{mode:?}] Completed 应停在 archived。轨迹：{calls:?}"
            );
        }
        RunReport::StageBudgetExhausted { phase, .. } => panic!(
            "[{mode:?}] 阶段 `{}` 预算耗尽——**产物没登记上或闸门没过**。\n\
             工具轨迹：{calls:?}\n\
             （这类失败通常意味着：工具的产物没被 `artifacts()` 登记、\
             或检查点未结算、或该阶段在 `PhaseGraph` 里缺产出工具）",
            phase.as_str()
        ),
        other => panic!(
            "[{mode:?}] **未到达终态**：{other:?}\n工具调用轨迹：{calls:?}"
        ),
    }
    calls
}

#[test]
fn scripted_model_drives_the_whole_chain_to_archived() {
    let dir = tmp_dir("full");
    let header = CaseHeader {
        case_id: "scripted-e2e".into(),
        mode: "scenario".into(),
        playbook_id: "scripted".into(),
        created_at: "2026-09-19T00:00:00Z".into(),
        schema_version: CaseHeader::SCHEMA_VERSION,
    };
    let log = CaseLog::open(&dir, &header).expect("案例日志可开");
    let reg = ToolRegistry::standard();
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let mut ex = DomainExecutor::new(&theory, &pb);

    // 模型：按指引行事的假模型（**读指引**，不是回放固定序列）
    let transport = std::sync::Arc::new(PolicyModel::new());
    let ep = Endpoint {
        base_url: "https://example.invalid".into(),
        api_key_env: "SHANGYI_TEST_KEY_UNUSED".into(),
        wire: shangyi_llm::Wire::Responses,
        echo_reasoning: false,
    };
    let client = LlmClient::new(ep, SharedPolicy(transport.clone())).with_key("test");

    let machine = PhaseMachine::new(ConsultationMode::Scenario);
    let mut cfg = LoopConfig::new("scripted", "scripted-e2e");
    cfg.max_steps_per_turn = 12;
    // 假模型的每步很快，但每阶段要走 2–4 步；给足预算
    cfg.context_warn_tokens = u64::MAX; // 本测试不测上下文预警

    let agent = AgentLoop::new(client, &reg, &mut ex, cfg).with_playbook(&pb);
    let mut runner = SessionRunner::new(agent, log, machine)
        .with_stage_turn_budget(8)
        .with_auto_advance(true);
    let mut io = ApprovingIo {
        asked: RefCell::new(Vec::new()),
    };

    let report = runner
        .run(vec![InputItem::user("开始一次脚本化咨询")], &mut io, "auto")
        .expect("runner 不应返回 Err（那表示部件缺失）");

    // ── 断言：到达终态 ──
    let calls = transport.calls();
    let phases: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for (p, _) in &calls {
            if seen.last() != Some(p) {
                seen.push(p.clone());
            }
        }
        seen
    };
    match &report {
        RunReport::Completed { phase, .. } => {
            assert_eq!(
                *phase,
                SessionPhase::Archived,
                "Completed 应停在 archived。工具调用轨迹：{calls:?}"
            );
        }
        other => panic!(
            "**未到达终态**：{other:?}\n\
             工具调用轨迹：{calls:?}\n\
             （失败在这一步上通常意味着：某个阶段的指引点名了错误的工具、\
             或某个工具的最小合法输入产不出所需产物、或闸门/检查点串不起来）"
        ),
    }

    // ── 断言：走过了应有的阶段 ──
    for expected in ["intel", "model_build", "solve", "select", "recommend", "report"] {
        assert!(
            phases.iter().any(|p| p == expected),
            "阶段 `{expected}` 未被走到。实际路径：{phases:?}"
        );
    }

    // ── 断言：**archived 阶段真的跑了一轮归档** ──
    //
    // ⚠️ 这条断言的来历（run31 实测）：
    // runner 此前一到 `archived` 就 `return Completed`，于是
    // `case_store_write` **一次都没被调用**——而该阶段的指引写着
    // "本阶段必须以它收尾"。后果：案例从不入库 ⇒ 校准闭环拿不到样本。
    //
    // 脚本化 E2E 当时**没抓到**它——因为假模型在 archived 阶段会调
    // `case_store_write`（我写策略时加了），而 runner 在推进到 archived
    // 的那一刻就返回了，那个调用根本没机会发生。
    //
    // 现在断言"归档真的发生过"，使这条路径被守住。
    assert!(
        calls.iter().any(|(p, t)| p == "archived" && t == "case_store_write"),
        "**archived 阶段必须真的跑一轮归档**（调 `case_store_write`）。\n\
         若缺失：案例不会入库 ⇒ 校准闭环永远拿不到样本。\n\
         常见原因：runner 在推进到 archived 时**立即返回**，没有给模型\
         调用归档工具的机会。\n工具调用轨迹：{calls:?}"
    );

    // ── 断言：三个检查点都被问过（铁律 2 在链路里真的生效） ──
    let asked = io.asked.borrow();
    for cp in [CheckpointId::Cp1, CheckpointId::Cp2, CheckpointId::Cp3] {
        assert!(
            asked.contains(&cp),
            "检查点 `{}` 未被问到——那意味着铁律 2 在真实链路里没有生效。\
             实际问过：{asked:?}",
            cp.as_str()
        );
    }
}

/// `Arc<PolicyModel>` 的本地包装（孤儿规则要求本地类型）。
struct SharedPolicy(std::sync::Arc<PolicyModel>);

impl Transport for SharedPolicy {
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &serde_json::Value,
    ) -> Result<(u16, String), LlmError> {
        self.0.post_json(url, headers, body)
    }
}

/// **discover 模式的路径**（跳过 select）也必须走通。
///
/// 该模式的路径是 `intake → intel → model_build → solve → discover →
/// recommend → report → archived`——**没有 select**，因为 discover 问的是
/// "哪些策略够好"而非"哪个均衡更可能"。
///
/// 而"路径上某个阶段缺产出工具"正是此前 `report` 结构性不可达的真因
/// （`recommend_build` 未注册）。故每条路径都要走一遍。
#[test]
fn discover_mode_also_reaches_archived() {
    let calls = run_mode_to_completion(ConsultationMode::Discover, "discover");
    let phases: Vec<&str> = calls.iter().map(|(p, _)| p.as_str()).collect();
    assert!(
        phases.contains(&"discover"),
        "discover 模式必须走到 discover 阶段。轨迹：{calls:?}"
    );
    assert!(
        !phases.contains(&"select"),
        "discover 模式**不应**走 select（它找满意解，不选均衡）。轨迹：{calls:?}"
    );
}

/// **whatif 模式的路径**必须走通。
///
/// 该模式走 `consequence`（条件后果推演：用户给定一个动作，推演各方反应），
/// 而不是 select。这是三条路径里**唯一此前从未被任何测试走过**的。
#[test]
fn whatif_mode_also_reaches_archived() {
    let calls = run_mode_to_completion(ConsultationMode::Whatif, "whatif");
    let phases: Vec<&str> = calls.iter().map(|(p, _)| p.as_str()).collect();
    assert!(
        phases.contains(&"consequence"),
        "whatif 模式必须走到 consequence 阶段。轨迹：{calls:?}"
    );
}

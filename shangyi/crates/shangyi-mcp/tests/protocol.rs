//! MCP 协议层与宿主的离线测试（**不 spawn 进程、不联网、不调模型**）。
//!
//! 端到端（spawn 真二进制）在 `shangyi-cli/tests/mcp_serve_e2e.rs`——
//! `CARGO_BIN_EXE_<bin>` 只在定义该 bin 的 crate 的集成测试里注入，
//! 而 `shangyi` 这个 bin 属于 `shangyi-cli`。
//!
//! 覆盖清单（与 task-4 的验收项逐条对应）：
//! 1. 协议形状：initialize / tools/list / ping
//! 2. 工具齐备：`tools/list` 数量 == `ToolRegistry::standard().len()`，且逐名对齐
//! 3. schema 覆盖率：有/无 schema 各多少，无 schema 的**不伪造** properties
//! 4. 真实工具调用：`solve_equilibria` 返回可解析的均衡 JSON
//! 5. 阶段闸门：intake 阶段调 `solve_equilibria` 被拒
//! 6. 未知工具 / 未知方法 / 非法 JSON
//! 7. 通知不回复
//! 8. 串行请求的顺序与 id 保持
//! 9. Blocked（未取得）与 Failed（执行失败）可区分
//! 10. checkpoint 在无 `--auto-approve-checkpoints` 时**不被**服务端确认

use std::io::Cursor;
use std::path::PathBuf;

use serde_json::{Value, json};
use shangyi_core::ConsultationMode;
use shangyi_mcp::host::{HostConfig, SessionHost};
use shangyi_mcp::protocol::PARSE_ERROR;
use shangyi_mcp::server::{
    PREFERRED_PROTOCOL_VERSION, SERVER_NAME, SUPPORTED_PROTOCOL_VERSIONS, Server,
};
use shangyi_mcp::tools::{CLIENT_SCHEMA_KEYWORDS, UNCONSTRAINED_NOTE, schema_coverage, wire_tools};
use shangyi_tools::ToolRegistry;

type TestServer = Server<Cursor<Vec<u8>>, Vec<u8>>;

// ─────────────────────────── 夹具 ───────────────────────────

/// 唯一临时目录（不引第三方 tempfile crate：本项目对依赖很节制）。
fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("时钟")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "shangyi-mcp-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn new_server(tag: &str, auto_approve: bool) -> TestServer {
    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(temp_home(tag)),
        auto_approve_checkpoints: auto_approve,
        resume_case: None,
    })
    .expect("宿主应能打开（仓库内剧本必须存在）");
    Server::new(Cursor::new(Vec::new()), Vec::new(), host)
}

fn request(srv: &mut TestServer, id: Value, method: &str, params: Value) -> Value {
    let line = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
    srv.handle_line(&line)
        .unwrap_or_else(|| panic!("请求 `{method}` 必须有响应"))
}

fn notification(srv: &mut TestServer, method: &str) -> Option<Value> {
    let line = json!({"jsonrpc":"2.0","method":method}).to_string();
    srv.handle_line(&line)
}

/// 一次 `tools/call`，返回 MCP 的 result 对象（`{content, isError}`）。
fn call(srv: &mut TestServer, id: Value, name: &str, args: Value) -> Value {
    let response = request(srv, id, "tools/call", json!({"name": name, "arguments": args}));
    assert!(
        response.get("error").is_none(),
        "工具级失败不应变成 RPC 错误：{response}"
    );
    response["result"].clone()
}

fn text_of(result: &Value) -> String {
    let blocks = result["content"]
        .as_array()
        .unwrap_or_else(|| panic!("content 必须是数组：{result}"));
    blocks
        .iter()
        .map(|b| b["text"].as_str().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `content[0]`（工具本身的结论，不含会话状态块）。
fn payload_of(result: &Value) -> String {
    result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("content[0] 必须是文本：{result}"))
        .to_string()
}

fn is_error(result: &Value) -> bool {
    result["isError"].as_bool().unwrap_or(false)
}

/// 合法的最小 GameSpec（与 `shangyi-core/tests/domain_exec.rs` 的夹具同形）。
fn minimal_gamespec() -> Value {
    json!({
        "game_id": "mcp-probe",
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
    })
}

// ─────────────────────────── 1. initialize ───────────────────────────

#[test]
fn initialize_echoes_supported_version_and_advertises_tools() {
    let mut s = new_server("init", false);
    let r = request(
        &mut s,
        json!(1),
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "test-client", "version": "0"}
        }),
    );
    assert_eq!(r["jsonrpc"], json!("2.0"));
    assert_eq!(r["id"], json!(1), "id 必须原样回显");
    let result = &r["result"];
    assert_eq!(
        result["protocolVersion"],
        json!("2025-11-25"),
        "客户端给的版本在支持列表里时必须回显"
    );
    assert!(
        result["capabilities"]["tools"].is_object(),
        "必须声明 capabilities.tools：{result}"
    );
    assert_eq!(result["serverInfo"]["name"], json!(SERVER_NAME));
    assert!(
        !result["serverInfo"]["version"].as_str().unwrap_or("").is_empty(),
        "serverInfo.version 不得为空"
    );
    assert!(
        result["instructions"].as_str().unwrap_or("").contains("阶段"),
        "instructions 必须说明阶段白名单"
    );

    // 协议版本协商的兜底：客户端版本不认识时，回我们声明的版本并写清。
    let r2 = request(
        &mut s,
        json!(2),
        "initialize",
        json!({"protocolVersion": "1999-01-01", "capabilities": {}}),
    );
    assert_eq!(r2["result"]["protocolVersion"], json!(PREFERRED_PROTOCOL_VERSION));
    assert!(
        r2["result"]["instructions"]
            .as_str()
            .unwrap()
            .contains("1999-01-01"),
        "未知版本必须在 instructions 里说清，而不是静默替换"
    );
    assert!(
        SUPPORTED_PROTOCOL_VERSIONS.contains(&"2024-11-05"),
        "必须保留最老的被广泛实现的版本"
    );
}

// ─────────────────────────── 2. tools/list 齐备 ───────────────────────────

#[test]
fn tools_list_matches_the_registry_exactly() {
    let mut s = new_server("list", false);
    let r = request(&mut s, json!(1), "tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().expect("tools 数组");
    let registry = ToolRegistry::standard();

    // 数目：与 [`shangyi_tools::STANDARD_TOOL_COUNT`] 及 `ToolRegistry::standard()` 严格一致（39 个工具）。
    assert_eq!(
        registry.len(),
        shangyi_tools::STANDARD_TOOL_COUNT,
        "注册表工具数变了：请同步 STANDARD_TOOL_COUNT、本断言与文档"
    );
    assert_eq!(
        tools.len(),
        registry.len(),
        "tools/list 数量必须等于注册表（实得 {} vs {}）",
        tools.len(),
        registry.len()
    );

    // 名字逐一对齐（顺序：注册表是 BTreeMap，天然字典序）
    let listed: Vec<String> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name").to_string())
        .collect();
    let expected: Vec<String> = registry.names().into_iter().map(str::to_string).collect();
    assert_eq!(listed, expected, "工具名集合/顺序必须与注册表一致");

    // 每个工具都必须是 MCP 的合法 Tool 对象
    for t in tools {
        let name = t["name"].as_str().unwrap();
        assert!(!t["description"].as_str().unwrap_or("").is_empty(), "`{name}` 描述为空");
        assert!(
            t["inputSchema"].is_object(),
            "`{name}` 的 inputSchema 必须是对象"
        );
    }

    // 阶段白名单的可见性：注册表里声明的阶段必须真的在 spec 里（抽查一条承重的）
    let solve = registry.get("solve_equilibria").expect("solve_equilibria 必须注册");
    assert!(solve.allows_phase("solve"));
    assert!(!solve.allows_phase("intake"), "求解不得在 intake 放行");
}

// ─────────────────────────── 3. schema 覆盖率（不伪造）───────────────────────────

/// 收集一个 schema 里出现在**关键字位置**的键（`properties` 里的是字段名，不算）。
fn keyword_keys(node: &Value, out: &mut Vec<String>) {
    let Some(obj) = node.as_object() else { return };
    for (key, value) in obj {
        match key.as_str() {
            "properties" => {
                if let Some(props) = value.as_object() {
                    for (_, sub) in props {
                        keyword_keys(sub, out);
                    }
                }
            }
            "items" | "additionalProperties" => keyword_keys(value, out),
            "oneOf" => {
                if let Some(arr) = value.as_array() {
                    for sub in arr {
                        keyword_keys(sub, out);
                    }
                }
            }
            other => out.push(other.to_string()),
        }
    }
}

#[test]
fn schema_coverage_is_reported_and_never_fabricated() {
    let registry = ToolRegistry::standard();
    let coverage = schema_coverage(&registry);

    // 两份清单必须恰好覆盖全部工具，且不重叠
    assert_eq!(
        coverage.with_schema.len() + coverage.without_schema.len(),
        registry.len(),
        "schema 覆盖率统计漏了工具"
    );
    for name in &coverage.with_schema {
        assert!(!coverage.without_schema.contains(name));
    }
    // 实测：36 有（含被客户端子集降级的）、3 无
    assert_eq!(coverage.with_schema.len(), 36, "有显式 schema 的工具数变了");
    assert_eq!(coverage.without_schema.len(), 3, "无 schema 的工具数变了");
    assert_eq!(
        coverage.without_schema,
        vec![
            "preference_inject".to_string(),
            "sandbox_exec".to_string(),
            "swarm_run".to_string()
        ],
        "无 schema 的工具清单变了：必须与 schema.rs 的 match 分支持平"
    );

    // 无 schema 的工具：**不得伪造** properties，且描述里如实标注
    for tool in wire_tools(&registry) {
        if tool.has_explicit_schema {
            continue;
        }
        assert_eq!(
            tool.input_schema,
            json!({"type": "object"}),
            "`{}` 没有 schema，只能退回自由对象（不许伪造 properties）",
            tool.name
        );
        assert!(
            tool.description.contains(UNCONSTRAINED_NOTE),
            "`{}` 的描述必须如实标注「参数结构未逐一约束」",
            tool.name
        );
    }

    // 有 schema 的工具：线上 schema 里不得残留客户端子集外的关键字
    // （残留会让 dsh 把整条 schema 的类型渲染降级成 unknown）
    for tool in wire_tools(&registry) {
        if !tool.has_explicit_schema {
            continue;
        }
        let mut keys = Vec::new();
        keyword_keys(&tool.input_schema, &mut keys);
        let unsupported: Vec<&String> = keys
            .iter()
            .filter(|k| !CLIENT_SCHEMA_KEYWORDS.contains(&k.as_str()))
            .collect();
        assert!(
            unsupported.is_empty(),
            "`{}` 的线上 schema 仍有关键字 {:?}（应被降级并留痕）",
            tool.name,
            unsupported
        );
        // 降级必须留痕：description 里点名省略了什么
        if !tool.downgraded.is_empty() {
            for kw in &tool.downgraded {
                assert!(
                    tool.description.contains(kw.as_str()),
                    "`{}` 省略了关键字却没在 description 里说明",
                    tool.name
                );
            }
        }
    }
    // 至少有一个工具真的被降级过——否则这条适配代码是死的（会让断言空转）
    assert!(
        !coverage.downgraded.is_empty(),
        "预期有工具使用了客户端子集外的关键字（如 minimum），这个断言空了说明上游 schema 变了"
    );
}

// ─────────────────────────── 4. 真实工具调用 ───────────────────────────

#[test]
fn solve_equilibria_real_call_returns_parseable_json() {
    // 打开自动结算：完整咨询路径要过 cp1/cp2 两道确认闸门，而 MCP 目前没有
    // 把用户答复回传的通道（见报告缺口清单）。这里走的是与
    // `SHANGYI_ASSUME_YES=1` 完全相同的自动结算路径（日志记 actor=auto）。
    let mut s = new_server("solve", true);
    assert_eq!(s.host().phase().as_str(), "intake");

    // intake：先选范围（与 `shangyi new` 一致，intake 不许默认进完整咨询）
    let r = call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "用户要完整评估这次调价的应对"}),
    );
    assert!(!is_error(&r), "选范围应成功：{}", text_of(&r));
    let scope: Value = serde_json::from_str(&payload_of(&r)).expect("选范围结果应是 JSON");
    assert_eq!(scope["scope"], json!("consultation"));
    assert_eq!(s.host().phase().as_str(), "intake", "控制类工具不推进阶段");

    // vn_map 是 intake 的域工具：成功后闸门放行 intake → intel
    let r = call(&mut s, json!(2), "vn_map", json!({}));
    assert!(!is_error(&r), "vn_map 应成功：{}", text_of(&r));
    serde_json::from_str::<Value>(&payload_of(&r)).expect("vn_map 输出应是 JSON");
    assert_eq!(s.host().phase().as_str(), "intel", "intake 闸门通过后应进入 intel");

    // intel 需要参与者画像 + cp1；先交产物，再确认
    let r = call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({
            "participant_id": "rival",
            "value_net_role": "substitutor",
            "relation_to_game": "player",
            "common": {"interests": ["保住份额"], "resources": ["本地渠道"]}
        }),
    );
    assert!(!is_error(&r), "画像应成功：{}", text_of(&r));
    assert_eq!(s.host().phase().as_str(), "intel", "cp1 未结算时不得推进");
    assert!(
        text_of(&r).contains("cp1"),
        "闸门拒绝的理由必须带进工具结果（让模型知道缺什么）：{}",
        text_of(&r)
    );

    // cp1（事实确认）：自动结算 → 推进到 model_build
    let r = call(
        &mut s,
        json!(4),
        "checkpoint",
        json!({"items": [{"id": "i1", "content": "直接竞对只有一家（rival）"}]}),
    );
    assert!(!is_error(&r), "checkpoint 应成功：{}", text_of(&r));
    let cp: Value = serde_json::from_str(&payload_of(&r)).expect("checkpoint 输出应是 JSON");
    assert_eq!(cp["needs_user_input"], json!(false), "自动结算时应为 false");
    assert_eq!(cp["approved_by"], json!("auto"));
    assert_eq!(
        s.host().phase().as_str(),
        "model_build",
        "cp1 结算后应推进到 model_build"
    );

    // model_build：交 GameSpec，再走 cp2（硬假设确认，每项必须带来源）
    let r = call(&mut s, json!(5), "gamespec_build", minimal_gamespec());
    assert!(!is_error(&r), "建模应成功：{}", text_of(&r));
    let r = call(
        &mut s,
        json!(6),
        "checkpoint",
        json!({"items": [{"id": "p1", "content": "margin = 3", "source": "用户口述毛利率区间", "proposed_value": "3"}]}),
    );
    assert!(!is_error(&r), "cp2 应成功：{}", text_of(&r));
    assert_eq!(s.host().phase().as_str(), "solve", "cp2 结算后应推进到 solve");

    // ── 真正要验的：solve_equilibria 在 solve 阶段返回可解析的均衡 JSON ──
    let r = call(&mut s, json!(7), "solve_equilibria", json!({}));
    assert!(!is_error(&r), "求解应成功：{}", text_of(&r));
    let solved: Value =
        serde_json::from_str(&payload_of(&r)).expect("求解结果必须是可解析 JSON（不是散文）");
    let equilibria = solved["equilibria"]
        .as_array()
        .or_else(|| solved["cells"].as_array())
        .unwrap_or_else(|| panic!("解集 JSON 里应有 equilibria：{solved}"));
    assert!(!equilibria.is_empty(), "囚徒困境式夹具至少有混合/纯策略均衡");
}

// ─────────────────────────── 5. 阶段闸门 ───────────────────────────

#[test]
fn phase_gate_rejects_solve_at_intake() {
    let mut s = new_server("gate", false);

    // 第一道闸：范围未选 ⇒ 拒绝并指向 select_task_scope（与 AgentLoop 同序）
    let r = call(&mut s, json!(1), "solve_equilibria", json!({}));
    assert!(is_error(&r), "未选范围时调 solver 必须失败：{}", text_of(&r));
    assert!(
        text_of(&r).contains("select_task_scope"),
        "范围闸门的拒绝必须指出下一步：{}",
        text_of(&r)
    );

    // 第二道闸：选了范围（完整咨询）之后，intake 阶段仍不能调 solve 工具
    let scoped = call(
        &mut s,
        json!(2),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "完整咨询"}),
    );
    assert!(!is_error(&scoped), "{}", text_of(&scoped));
    assert_eq!(s.host().phase().as_str(), "intake", "控制类工具不推进阶段");

    let r = call(&mut s, json!(3), "solve_equilibria", json!({}));
    assert!(is_error(&r), "intake 阶段调 solver 必须是失败：{}", text_of(&r));
    let msg = text_of(&r);
    assert!(
        msg.contains("不允许在阶段") && msg.contains("solve"),
        "阶段闸门的拒绝必须带工具阶段白名单（`check_call` 的原文）：{msg}"
    );
    assert_eq!(s.host().phase().as_str(), "intake", "被拒的调用不得推动阶段");
}

#[test]
fn task_scope_gate_points_at_select_task_scope() {
    let mut s = new_server("scope", false);
    // 未选范围时，除 select_task_scope / ask_user 外的工具都该被拒
    let r = call(&mut s, json!(1), "vn_map", json!({}));
    assert!(is_error(&r), "未选范围时 vn_map 应被拒：{}", text_of(&r));
    assert!(
        text_of(&r).contains("select_task_scope"),
        "拒绝信息必须指出下一步动作：{}",
        text_of(&r)
    );
}

// ─────────────────────────── 6. 未知工具 / 未知方法 / 非法 JSON ───────────────────────────

#[test]
fn unknown_tool_is_a_tool_level_error_not_an_rpc_error() {
    let mut s = new_server("unknown-tool", false);
    let response = request(
        &mut s,
        json!(9),
        "tools/call",
        json!({"name": "no_such_tool", "arguments": {}}),
    );
    assert!(response.get("error").is_none(), "未知工具是工具级失败，不是 RPC 错误");
    let result = &response["result"];
    assert!(is_error(result));
    assert!(
        text_of(result).contains("no_such_tool") || text_of(result).contains("未注册"),
        "错误里必须报出名字：{}",
        text_of(result)
    );
}

#[test]
fn unknown_method_and_malformed_messages_use_rpc_errors() {
    let mut s = new_server("rpc-errors", false);

    // 未知方法 → -32601
    let r = request(&mut s, json!(1), "tools/teleport", json!({}));
    assert_eq!(r["error"]["code"], json!(-32601));

    // 非法 JSON → -32700 且 id 为 null
    let bad = s.handle_line("{not json").expect("解析错误也要回一条");
    assert_eq!(bad["error"]["code"], json!(PARSE_ERROR));
    assert_eq!(bad["id"], Value::Null);

    // 合法 JSON 但不是对象 → -32600
    let not_object = s.handle_line("[1,2,3]").expect("非法请求也要回一条");
    assert_eq!(not_object["error"]["code"], json!(-32600));

    // 缺 jsonrpc 版本 → -32600
    let no_version = s.handle_line(r#"{"id":5,"method":"ping"}"#).expect("回一条");
    assert_eq!(no_version["error"]["code"], json!(-32600));

    // tools/call 参数不成话 → -32602（**协议级**，不是工具级）
    let no_name = request(&mut s, json!(2), "tools/call", json!({"arguments": {}}));
    assert_eq!(no_name["error"]["code"], json!(-32602));
    let bad_args = request(
        &mut s,
        json!(3),
        "tools/call",
        json!({"name": "vn_map", "arguments": "不是对象"}),
    );
    assert_eq!(bad_args["error"]["code"], json!(-32602));
}

// ─────────────────────────── 7. 通知不回复 ───────────────────────────

#[test]
fn notifications_are_never_answered() {
    let mut s = new_server("notify", false);
    assert!(
        notification(&mut s, "notifications/initialized").is_none(),
        "initialized 通知绝不能有响应（规范要求）"
    );
    assert!(
        notification(&mut s, "notifications/something_else").is_none(),
        "不认识的通知也不得回复"
    );
    // 之后仍能正常应答（证明没有把后续输出吞掉）
    let r = request(&mut s, json!(1), "ping", json!({}));
    assert_eq!(r["result"], json!({}));
    assert_eq!(s.stats().notifications, 2);
}

// ─────────────────────────── 8. 串行请求的顺序与 id ───────────────────────────

#[test]
fn serial_requests_keep_order_and_ids() {
    let mut s = new_server("serial", false);
    let ids = [json!("a"), json!(2), json!(3.5)];
    let mut seen = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        let method = if i == 0 { "ping" } else if i == 1 { "tools/list" } else { "initialize" };
        let params = if method == "initialize" {
            json!({"protocolVersion": "2025-06-18"})
        } else {
            json!({})
        };
        let r = request(&mut s, id.clone(), method, params);
        seen.push(r["id"].clone());
    }
    assert_eq!(seen, ids, "响应顺序与 id 类型必须与请求一一对应（数字仍是数字）");
}

// ─────────────────────────── 9. Blocked vs Failed ───────────────────────────

#[test]
fn blocked_is_not_an_error_but_failed_is() {
    let mut s = new_server("blocked", true);
    // 先选范围 + 推进到 intel（web_search 在 intake 也允许，但 intel 更贴近真实用法）
    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "完整咨询"}),
    );
    call(&mut s, json!(2), "vn_map", json!({}));

    // 检索后端未接线 ⇒ Blocked（"未取得"），不是 isError
    let blocked = call(&mut s, json!(3), "web_search", json!({"query": "竞争对手调价"}));
    assert!(
        !is_error(&blocked),
        "未取得（Blocked）不得报成 isError——那会让模型以为是自己调错了：{}",
        text_of(&blocked)
    );
    let payload: Value = serde_json::from_str(&payload_of(&blocked)).expect("Blocked 应是结构化 JSON");
    assert_eq!(payload["blocked"], json!(true));
    assert_eq!(payload["tool"], json!("web_search"));
    assert!(payload["reason"].as_str().unwrap_or("").contains("后端"));

    // 参数错 ⇒ Failed（可纠正的执行失败），必须是 isError
    let failed = call(
        &mut s,
        json!(4),
        "participant_profile_update",
        json!({"participant_id": "rival"}),
    );
    assert!(is_error(&failed), "执行失败必须是 isError：{}", text_of(&failed));
    assert!(
        text_of(&failed).contains("调用失败"),
        "失败措辞要区分于未取得：{}",
        text_of(&failed)
    );
}

// ─────────────────────────── 10. checkpoint 不被服务端确认 ───────────────────────────

#[test]
fn checkpoint_without_auto_approve_waits_for_the_user() {
    let mut s = new_server("checkpoint", false);
    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "完整咨询"}),
    );
    call(&mut s, json!(2), "vn_map", json!({}));
    call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({"participant_id": "rival", "value_net_role": "substitutor", "relation_to_game": "player"}),
    );
    let r = call(
        &mut s,
        json!(4),
        "checkpoint",
        json!({"items": [{"id": "i1", "content": "直接竞对只有一家"}]}),
    );
    assert!(!is_error(&r), "挂起不是失败：{}", text_of(&r));
    let payload: Value = serde_json::from_str(&payload_of(&r)).expect("应为结构化 JSON");
    assert_eq!(payload["needs_user_input"], json!(true));
    assert_eq!(payload["checkpoint"], json!("cp1"));
    assert_eq!(
        payload["items"].as_array().unwrap().len(),
        1,
        "提问件必须原样转达（用户要看得到要确认什么）"
    );
    assert_eq!(
        s.host().phase().as_str(),
        "intel",
        "没有用户答复时，阶段必须停在原处（闸门仍然拦着）"
    );

    // 再问一次同一 checkpoint：内容未变 ⇒ 幂等，不得报"已结算"
    let again = call(
        &mut s,
        json!(5),
        "checkpoint",
        json!({"items": [{"id": "i1", "content": "直接竞对只有一家"}]}),
    );
    assert!(!is_error(&again), "重复提问同一内容不得失败：{}", text_of(&again));
}

// ─────────────────────────── 11. 留档目录不可写时优雅降级 ───────────────────────────

/// `--home` 指向一个**普通文件**（而不是目录）⇒ 留档必然失败。
///
/// 断言：服务端仍然能起、协议仍然正确、并且**如实**报告"未落盘"
/// （而不是静默当成记录了）。dsh profile 里 `args` 只有 `["mcp-serve"]`，
/// 即默认走 `$HOME/.shangyi`——那个目录在某些环境里不可写，这条路径必须活着。
#[test]
fn unwritable_archive_dir_degrades_gracefully() {
    let blocker = std::env::temp_dir().join(format!(
        "shangyi-mcp-blocked-file-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("时钟")
            .as_nanos()
    ));
    std::fs::write(&blocker, b"not a directory").expect("写文件");

    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        // home 是个文件 ⇒ $home/cases/<date>/<case> 建不出来
        home: Some(blocker.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("留档失败不得阻止会话启动");
    assert!(
        !host.archiving(),
        "写不进去时必须如实报「未落盘」，不能假装记录了"
    );
    let mut srv = Server::new(Cursor::new(Vec::new()), Vec::new(), host);
    let r = request(&mut srv, json!(1), "ping", json!({}));
    assert_eq!(r["result"], json!({}), "协议面不得受影响");
    let _ = std::fs::remove_file(blocker);
}

// ─────────────────────────── 12. 惰性建案、MCP Resources、双通道关口与恢复 ───────────────────────────

#[test]
fn lazy_caselog_does_not_create_case_dir_until_first_tool_call() {
    let home = temp_home("lazy-log");
    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    assert!(host.archiving(), "可写目录应报 archiving=true");
    let case_dir = host.case_dir().expect("case_dir").to_path_buf();
    let mut srv = Server::new(Cursor::new(Vec::new()), Vec::new(), host);

    let init = request(
        &mut srv,
        json!(1),
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
    );
    assert!(init["result"]["capabilities"]["resources"].is_object());
    let _ = request(&mut srv, json!(2), "tools/list", json!({}));
    let res_list = request(&mut srv, json!(3), "resources/list", json!({}));
    let resources = res_list["result"]["resources"].as_array().expect("resources");
    assert!(resources.len() >= 40, "应列出 ≥40 项资源：{}", resources.len());

    // 仅握手/列工具/列资源（探测进程模式）不得落盘空案例目录
    assert!(
        !case_dir.exists(),
        "首次 tools/call 前不得创建空案例目录：{}",
        case_dir.display()
    );

    // 首次真实调用工具后，立即创建 header.json 与 case.jsonl
    let scoped = call(
        &mut srv,
        json!(4),
        "select_task_scope",
        json!({"scope": "consultation", "mode": "whatif", "playbook": "market_entry", "action": "先开单店试水", "reason": "动态选型测试"}),
    );
    assert!(!is_error(&scoped), "动态选型应成功：{}", text_of(&scoped));
    assert_eq!(srv.host().mode(), ConsultationMode::Whatif);
    assert_eq!(srv.host().playbook().meta.id, "market_entry");
    assert_eq!(srv.host().action(), Some("先开单店试水"));
    assert!(case_dir.join("header.json").is_file(), "首次调用后应写入 header.json");
    assert!(case_dir.join("case.jsonl").is_file(), "首次调用后应写入 case.jsonl");
}

#[test]
fn resources_protocol_handles_list_templates_read_and_security_errors() {
    let mut s = new_server("resources-rpc", false);
    let templates = request(&mut s, json!(1), "resources/templates/list", json!({}));
    assert_eq!(
        templates["result"]["resourceTemplates"].as_array().unwrap().len(),
        2
    );

    let kb = request(
        &mut s,
        json!(2),
        "resources/read",
        json!({"uri": "shangyi://kb/adverse-selection"}),
    );
    assert!(
        kb["result"]["contents"][0]["text"]
            .as_str()
            .unwrap_or("")
            .contains("Akerlof")
    );

    let traversal = request(
        &mut s,
        json!(3),
        "resources/read",
        json!({"uri": "shangyi://kb/../secrets"}),
    );
    assert_eq!(traversal["error"]["code"], json!(-32602));

    let missing = request(
        &mut s,
        json!(4),
        "resources/read",
        json!({"uri": "shangyi://kb/nonexistent-slug"}),
    );
    assert_eq!(missing["error"]["code"], json!(-32002));
}

#[test]
fn conversational_checkpoint_rejects_forgery_and_settles_with_verified_post_checkpoint_quote() {
    let home = temp_home("conv-cp");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");

    // 关口打开前的首轮用户消息（可用于 constraint_register，但绝不可用于伪造后续关口确认）
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        &ledger,
        format!(
            "{{\"text\":\"月租金上限25000元，对手只有一家瑞幸\",\"user_actor\":\"user\",\"unix_secs\":{now_secs},\"at\":\"2026-11-12T10:00:00Z\"}}\n"
        ),
    )
    .expect("write initial ledger");

    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), host);

    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "测试双通道确认"}),
    );
    // 验证首轮真实用户原文可用于登记硬约束
    let cr = call(
        &mut s,
        json!(2),
        "constraint_register",
        json!({
            "quote": "月租金上限25000元"
        }),
    );
    assert!(!is_error(&cr), "真实用户原文登记约束应成功：{}", text_of(&cr));

    call(&mut s, json!(3), "vn_map", json!({}));
    assert_eq!(s.host().phase().as_str(), "intel");
    let pp = call(
        &mut s,
        json!(4),
        "participant_profile_update",
        json!({
            "participant_id": "rival",
            "value_net_role": "substitutor",
            "relation_to_game": "player",
            "common": {"interests": ["保住份额"], "resources": ["本地渠道"]}
        }),
    );
    assert!(!is_error(&pp), "画像更新应成功：{}", text_of(&pp));
    let opened = call(
        &mut s,
        json!(5),
        "checkpoint",
        json!({"items": [{"id": "i1", "content": "直接竞对只有一家瑞幸"}]}),
    );
    assert!(!is_error(&opened));
    assert_eq!(s.host().phase().as_str(), "intel");

    // 企图 1：模型不经用户回复直接伪造确认 ⇒ 必须被铁律 2 拒绝
    let forged = call(
        &mut s,
        json!(6),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "确认无误"}),
    );
    assert!(is_error(&forged), "无真实用户回复时自批必须被拒绝");
    assert_eq!(s.host().phase().as_str(), "intel");

    // 企图 2：模型拿关口打开**之前**的旧消息充当确认凭据 ⇒ 同样必须被拒绝
    let replayed = call(
        &mut s,
        json!(7),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "月租金上限25000元"}),
    );
    assert!(is_error(&replayed), "重放关口前旧消息必须被拒绝");
    assert_eq!(s.host().phase().as_str(), "intel");

    // 真实用户在关口打开后回复确认（写入可信账本）
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    use std::io::Write;
    writeln!(
        f,
        "{{\"text\":\"核对无误，同意竞对画像，继续建模\",\"user_actor\":\"user\",\"at\":\"2026-11-12T10:01:00Z\"}}"
    )
    .unwrap();
    drop(f);

    let approved = call(
        &mut s,
        json!(8),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "同意竞对画像"}),
    );
    assert!(
        !is_error(&approved),
        "真实关口后用户消息应成功结算：{}",
        text_of(&approved)
    );
    assert_eq!(
        s.host().phase().as_str(),
        "model_build",
        "结算 cp1 后应自动推进到 model_build"
    );

    // 再测试 --resume 跨进程恢复（阶段与产物完整保留）
    let case_id = s.host().case_id().to_string();
    drop(s);

    let resumed_host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id.clone()),
    })
    .expect("resume open");
    assert_eq!(resumed_host.case_id(), case_id);
    assert_eq!(resumed_host.phase().as_str(), "model_build");
}

#[test]
fn followup_task_reopens_scope_selection_and_survives_resume() {
    use std::io::Write;

    let home = temp_home("followup-scope");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        &ledger,
        format!(
            "{{\"text\":\"先帮我算设备费（报价还没拿到）\",\"user_actor\":\"user\",\"unix_secs\":{now_secs},\"at\":\"2026-11-13T01:00:00Z\"}}\n"
        ),
    )
    .expect("write ledger");

    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), host);

    // 1. 尚在首轮进行中，禁止误调 begin_followup_task
    let premature = call(&mut s, json!(1), "begin_followup_task", json!({}));
    assert!(is_error(&premature), "未完成任务前不得调用 begin_followup_task");

    // 2. 完成一轮 cost 短任务
    let sc1 = call(
        &mut s,
        json!(2),
        "select_task_scope",
        json!({"scope": "cost", "reason": "首轮先做设备成本测算"}),
    );
    assert!(!is_error(&sc1), "选择 cost 范围应成功：{}", text_of(&sc1));
    let cost = call(
        &mut s,
        json!(3),
        "business_cost",
        json!({
            "operation": "create",
            "scenario": "设备方案",
            "currency": "CNY",
            "costs_complete": false,
            "costs": [{
                "scenario": "设备方案",
                "name": "设备采购",
                "treatment": "cash_expense",
                "amount": null,
                "period": "once",
                "basis": "user_provided",
                "source": "报价未取得"
            }]
        }),
    );
    assert!(!is_error(&cost), "business_cost 应成功：{}", text_of(&cost));
    let done = call(
        &mut s,
        json!(4),
        "complete_task",
        json!({
            "summary": "已登记设备采购待补正式报价。",
            "missing": ["设备采购正式报价"]
        }),
    );
    assert!(!is_error(&done), "complete_task 应成功：{}", text_of(&done));

    // 3. 交付后直接再调 select_task_scope 必须被拒，并明确指向 begin_followup_task
    let blocked_select = call(
        &mut s,
        json!(5),
        "select_task_scope",
        json!({"scope": "research", "reason": "追问竞对"}),
    );
    assert!(is_error(&blocked_select), "未先 begin_followup_task 不得重选范围");
    assert!(
        text_of(&blocked_select).contains("begin_followup_task"),
        "拒绝文案应指明先调 begin_followup_task：{}",
        text_of(&blocked_select)
    );

    // 4. 调用真实工具 begin_followup_task（无需调用不存在的 reopen_task）
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(
        f,
        "{{\"text\":\"再查一下瑞幸同商圈最新门店动态\",\"user_actor\":\"user\",\"at\":\"2026-11-13T01:02:00Z\"}}"
    )
    .unwrap();
    drop(f);

    let reopened = call(
        &mut s,
        json!(6),
        "begin_followup_task",
        json!({"reason": "用户追问同商圈竞对公开信息"}),
    );
    assert!(!is_error(&reopened), "begin_followup_task 应成功：{}", text_of(&reopened));
    let reopened_json: Value = serde_json::from_str(&payload_of(&reopened)).unwrap();
    assert_eq!(reopened_json["reopened"], json!(true));
    assert_eq!(reopened_json["phase"], json!("intake"));
    assert_eq!(reopened_json["evidence_preserved"], json!(true));

    // 5. 重开后即可重新选择范围（例如 research）
    let sc2 = call(
        &mut s,
        json!(7),
        "select_task_scope",
        json!({"scope": "research", "reason": "追问竞对公开动态"}),
    );
    assert!(!is_error(&sc2), "begin_followup_task 之后重新 select_task_scope 应成功：{}", text_of(&sc2));

    let case_id = s.host().case_id().to_string();
    drop(s);

    // 6. --resume 恢复后应完整保持新开启的 research 范围，且不得误退回 scoped_result 锁定
    let resumed_host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id),
    })
    .expect("resume open");
    let mut s2 = Server::new(Cursor::new(Vec::new()), Vec::new(), resumed_host);
    let plan = call(
        &mut s2,
        json!(8),
        "research_task_plan",
        json!({
            "fields": [{
                "id": "store_news",
                "label": "门店动态",
                "request_quote": "瑞幸同商圈最新门店动态",
                "evidence_required": "source_link"
            }]
        }),
    );
    assert!(!is_error(&plan), "resume 后应处于新追问的 research 范围并可登记字段：{}", text_of(&plan));
}

#[test]
fn conversational_request_rewind_enforces_iron_law_2_across_resume() {
    use std::io::Write;

    let home = temp_home("conv-rewind");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        &ledger,
        format!(
            "{{\"text\":\"对手只有一家瑞幸。我方收益为3。对手收益为2\",\"user_actor\":\"user\",\"unix_secs\":{now_secs},\"at\":\"2026-11-13T02:00:00Z\"}}\n"
        ),
    )
    .expect("write ledger");

    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), host);

    call(&mut s, json!(1), "select_task_scope", json!({"scope": "consultation", "reason": "完整咨询"}));
    call(&mut s, json!(2), "vn_map", json!({}));
    call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({
            "participant_id": "us",
            "value_net_role": "focal",
            "relation_to_game": "player",
            "common": {"interests": ["利润最大化"], "resources": ["门店选址"]}
        }),
    );
    call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({
            "participant_id": "rival",
            "value_net_role": "substitutor",
            "relation_to_game": "player",
            "common": {"interests": ["保住份额"], "resources": ["本地渠道"]}
        }),
    );
    call(
        &mut s,
        json!(4),
        "checkpoint",
        json!({"items": [{"id": "i1", "content": "对手为瑞幸"}]}),
    );
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"同意竞对事实，继续\",\"user_actor\":\"user\",\"at\":\"2026-11-13T02:01:00Z\"}}").unwrap();
    drop(f);
    let cp1 = call(&mut s, json!(5), "checkpoint", json!({"decision": "approve", "user_quote": "同意竞对事实"}));
    assert!(!is_error(&cp1));
    assert_eq!(s.host().phase().as_str(), "model_build");

    let verified_spec = json!({
        "game_id": "mcp-rewind-probe",
        "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "维持,维持": {"us": {"kind": "ref", "value": "p_us"}, "rival": {"kind": "ref", "value": "p_rival"}},
            "维持,降价": {"us": {"kind": "ref", "value": "p_us"}, "rival": {"kind": "ref", "value": "p_rival"}},
            "降价,维持": {"us": {"kind": "ref", "value": "p_us"}, "rival": {"kind": "ref", "value": "p_rival"}},
            "降价,降价": {"us": {"kind": "ref", "value": "p_us"}, "rival": {"kind": "ref", "value": "p_rival"}}
        }}},
        "params": {
            "p_us": {"name": "p_us", "value": 3, "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "我方收益为3"}},
            "p_rival": {"name": "p_rival", "value": 2, "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "对手收益为2"}}
        }
    });
    let gs = call(&mut s, json!(6), "gamespec_build", verified_spec);
    assert!(!is_error(&gs), "gamespec_build 应成功：{}", text_of(&gs));
    assert!(!text_of(&gs).contains("insufficient_evidence"), "gamespec_build 不应报证据不足：{}", text_of(&gs));
    let open_cp2 = call(
        &mut s,
        json!(7),
        "checkpoint",
        json!({"items": [{"id": "p1", "content": "p_us = 3, p_rival = 2", "source": "用户明确说明收益", "proposed_value": "3 / 2"}]}),
    );
    assert!(!is_error(&open_cp2), "open cp2 应成功：{}", text_of(&open_cp2));
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"同意建模假设，开始求解\",\"user_actor\":\"user\",\"at\":\"2026-11-13T02:02:00Z\"}}").unwrap();
    drop(f);
    let cp2 = call(&mut s, json!(8), "checkpoint", json!({"decision": "approve", "user_quote": "同意建模假设"}));
    assert!(!is_error(&cp2), "cp2 批准应成功：{}", text_of(&cp2));
    assert_eq!(
        s.host().phase().as_str(),
        "solve",
        "cp2 后应进入 solve；gs={}；open_cp2={}；cp2={}",
        text_of(&gs),
        text_of(&open_cp2),
        text_of(&cp2)
    );

    // 产出 solve 阶段的均衡解集（推进到 select），以便后续检验严格下游产物 equilibrium_set 被清
    let solved = call(&mut s, json!(9), "solve_equilibria", json!({}));
    assert!(!is_error(&solved), "solve_equilibria 应成功：{}", text_of(&solved));
    assert_eq!(s.host().phase().as_str(), "select");

    // 阶段一：模型发起回退提议，此时不得立即回退
    let prop1 = call(
        &mut s,
        json!(10),
        "request_rewind",
        json!({"target": "model_build", "reason": "需要修改支付矩阵结构"}),
    );
    assert!(!is_error(&prop1), "提议回退应成功：{}", text_of(&prop1));
    assert_eq!(s.host().phase().as_str(), "select", "未获用户确认前不得提前回退");

    // 铁律 2 门禁 1：未收到用户回退后回复，模型直接伪造 approve 必须被拒
    let forged = call(
        &mut s,
        json!(11),
        "request_rewind",
        json!({"decision": "approve", "user_quote": "同意回退"}),
    );
    assert!(is_error(&forged));
    assert_eq!(s.host().phase().as_str(), "select");

    // 铁律 2 门禁 2：重放提议前的旧用户消息也必须被拒
    let replayed = call(
        &mut s,
        json!(12),
        "request_rewind",
        json!({"decision": "approve", "user_quote": "同意建模假设"}),
    );
    assert!(is_error(&replayed), "重放回退提议前的旧消息必须被拒");
    assert_eq!(s.host().phase().as_str(), "select");

    // 铁律 2 门禁 3：否定/疑问句不得充当批准
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"不要同意退回到建模，先继续\",\"user_actor\":\"user\",\"at\":\"2026-11-13T02:03:00Z\"}}").unwrap();
    drop(f);
    let negated = call(
        &mut s,
        json!(13),
        "request_rewind",
        json!({"decision": "approve", "user_quote": "同意退回到建模"}),
    );
    assert!(is_error(&negated), "含否定前缀的话语不得用于 approve 回退");
    assert_eq!(s.host().phase().as_str(), "select");

    // 用户拒绝回退 => 清空待决回退请求并维持当前阶段
    let rejected = call(
        &mut s,
        json!(14),
        "request_rewind",
        json!({"decision": "reject", "user_quote": "先继续"}),
    );
    assert!(!is_error(&rejected), "用户拒绝回退应平稳结算：{}", text_of(&rejected));
    assert_eq!(s.host().phase().as_str(), "select");

    // 再次提出回退，并模拟用户真实同意
    let prop2 = call(
        &mut s,
        json!(15),
        "request_rewind",
        json!({"target": "model_build", "reason": "用户确认改模型"}),
    );
    assert!(!is_error(&prop2));
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"好的，确定退回到建模阶段改一下参数\",\"user_actor\":\"user\",\"at\":\"2026-11-13T02:04:00Z\"}}").unwrap();
    drop(f);
    let approved = call(
        &mut s,
        json!(16),
        "request_rewind",
        json!({"decision": "approve", "user_quote": "确定退回到建模阶段"}),
    );
    assert!(!is_error(&approved), "真实用户确认后回退应成功：{}", text_of(&approved));
    assert_eq!(s.host().phase().as_str(), "model_build", "回退后应停留在目标阶段 model_build 而不得自动推进");
    let approved_json: Value = serde_json::from_str(&payload_of(&approved)).unwrap();
    assert!(
        approved_json["cleared_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "equilibrium_set"),
        "回退到 model_build 必须清理严格下游产物 equilibrium_set：{approved_json}"
    );
    let blocked_solve = call(&mut s, json!(17), "solve_equilibria", json!({}));
    assert!(is_error(&blocked_solve), "回退至 model_build 后 solve_equilibria 必须被阶段闸门拒绝");

    // 进一步测试：在 model_build 提议回退至 intel 后，进程退出 --resume 再确认回退！
    let prop3 = call(
        &mut s,
        json!(18),
        "request_rewind",
        json!({"target": "intel", "reason": "要补充一条竞对情报"}),
    );
    assert!(!is_error(&prop3));
    let case_id = s.host().case_id().to_string();
    drop(s);

    let resumed_host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id.clone()),
    })
    .expect("resume open");
    assert_eq!(resumed_host.phase().as_str(), "model_build");
    let mut s2 = Server::new(Cursor::new(Vec::new()), Vec::new(), resumed_host);

    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"同意退回情报阶段补充竞对\",\"user_actor\":\"user\",\"at\":\"2026-11-13T02:05:00Z\"}}").unwrap();
    drop(f);
    let approved_intel = call(
        &mut s2,
        json!(19),
        "request_rewind",
        json!({"decision": "approve", "user_quote": "同意退回情报阶段"}),
    );
    assert!(
        !is_error(&approved_intel),
        "跨 --resume 恢复待决回退并由用户确认应成功：{}",
        text_of(&approved_intel)
    );
    assert_eq!(s2.host().phase().as_str(), "intel");
    let intel_json: Value = serde_json::from_str(&payload_of(&approved_intel)).unwrap();
    assert!(
        intel_json["cleared_artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "gamespec"),
        "回退到 intel 必须清理严格下游产物 gamespec：{intel_json}"
    );
    drop(s2);

    // 再次 --resume 确认回退至 intel 及作废的 gamespec 在重放后保持已清理状态（不会泄漏回 executor）
    let resumed_intel = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id),
    })
    .expect("resume intel");
    assert_eq!(resumed_intel.phase().as_str(), "intel");
}

#[test]
fn select_task_scope_dynamic_overrides_are_atomic_on_error() {
    let home = temp_home("atomic-intake");
    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: Some("pricing_response".to_string()),
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), host);
    assert_eq!(s.host().mode(), ConsultationMode::Scenario);
    assert_eq!(s.host().playbook().meta.id, "pricing_response");

    // 1) playbook 合法，但 mode 非法：不得留下已切换的 playbook。
    let bad_mode = call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({
            "scope": "consultation",
            "reason": "切换剧本后测试回滚原子性",
            "playbook": "market_entry",
            "mode": "not_a_mode"
        }),
    );
    assert!(is_error(&bad_mode));
    assert_eq!(s.host().playbook().meta.id, "pricing_response");
    assert_eq!(s.host().mode(), ConsultationMode::Scenario);

    // 2) playbook + mode 合法，但 scope 非法：不得切换 mode/playbook 或写 PlaybookSelected。
    let bad_scope = call(
        &mut s,
        json!(2),
        "select_task_scope",
        json!({
            "scope": "bogus_scope",
            "reason": "测试无效 scope 原子回滚",
            "playbook": "market_entry",
            "mode": "whatif",
            "action": "test_act"
        }),
    );
    assert!(is_error(&bad_scope));
    assert_eq!(s.host().playbook().meta.id, "pricing_response");
    assert_eq!(s.host().mode(), ConsultationMode::Scenario);
    assert_eq!(s.host().action(), None);

    // 3) playbook + mode + action 合法但 reason 缺失：仍不得切换，随后合法调用仍按原配置走。
    let bad_reason = call(
        &mut s,
        json!(3),
        "select_task_scope",
        json!({
            "scope": "cost",
            "playbook": "market_entry",
            "mode": "discover"
        }),
    );
    assert!(is_error(&bad_reason));
    assert_eq!(s.host().playbook().meta.id, "pricing_response");
    assert_eq!(s.host().mode(), ConsultationMode::Scenario);

    let ok = call(
        &mut s,
        json!(4),
        "select_task_scope",
        json!({"scope": "cost", "reason": "仅算门店月租金"}),
    );
    assert!(!is_error(&ok), "{}", text_of(&ok));
    let out: Value = serde_json::from_str(&payload_of(&ok)).unwrap();
    assert_eq!(out["mode"], "scenario");
    assert_eq!(out["playbook"], "pricing_response");
}

#[test]
fn rewind_and_checkpoint_block_clause_negation_question_and_duplicate_rewind() {
    use std::io::Write;

    let home = temp_home("iron2-bypass");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).expect("runtime dir");
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        &ledger,
        format!(
            "{{\"text\":\"对手只有一家瑞幸。\",\"user_actor\":\"user\",\"unix_secs\":{now_secs},\"at\":\"2026-11-13T03:00:00Z\"}}\n"
        ),
    )
    .expect("write ledger");

    let host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), host);

    call(&mut s, json!(1), "select_task_scope", json!({"scope": "consultation", "reason": "完整咨询"}));
    call(&mut s, json!(2), "vn_map", json!({}));
    call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({
            "participant_id": "us",
            "value_net_role": "focal",
            "relation_to_game": "player",
            "common": {"interests": ["利润"], "resources": ["资金"]}
        }),
    );
    call(
        &mut s,
        json!(4),
        "participant_profile_update",
        json!({
            "participant_id": "rival",
            "value_net_role": "substitutor",
            "relation_to_game": "player",
            "common": {"interests": ["份额"], "resources": ["渠道"]}
        }),
    );
    call(&mut s, json!(5), "checkpoint", json!({"items": [{"id": "i1", "content": "对手为瑞幸"}]}));

    // 否定/疑问绕过企图不得通过 checkpoint 审批
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"先别急着同意，能先不确认吗？\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:00:10Z\"}}").unwrap();
    writeln!(f, "{{\"text\":\"同意方向，不过先别确认\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:00:20Z\"}}").unwrap();
    drop(f);
    let bypass1 = call(&mut s, json!(6), "checkpoint", json!({"decision": "approve", "user_quote": "同意"}));
    assert!(is_error(&bypass1), "先别急着同意不得被误判为批准：{}", text_of(&bypass1));
    let bypass2 = call(&mut s, json!(7), "checkpoint", json!({"decision": "approve", "user_quote": "同意方向"}));
    assert!(is_error(&bypass2), "后半句「不过先别确认」反悔不得通过：{}", text_of(&bypass2));

    // 合法含正面惯用语的确认应通过
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"不用怀疑，同意推进\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:00:30Z\"}}").unwrap();
    drop(f);
    let ok_cp1 = call(&mut s, json!(8), "checkpoint", json!({"decision": "approve", "user_quote": "同意推进"}));
    assert!(!is_error(&ok_cp1), "{}", text_of(&ok_cp1));
    assert_eq!(s.host().phase().as_str(), "model_build");

    // 提议回退后，未结算前不得再次调用 request_rewind 覆盖待决提议
    let prop1 = call(&mut s, json!(9), "request_rewind", json!({"target": "intake", "reason": "重确认玩家"}));
    assert!(!is_error(&prop1));
    let dup = call(&mut s, json!(10), "request_rewind", json!({"target": "intel", "reason": "改成回退到intel"}));
    assert!(is_error(&dup), "未结算已有 pending_rewind 时不得覆盖：{}", text_of(&dup));

    // 否定/疑问句不得批准 rewind
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"先不用退回\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:01:00Z\"}}").unwrap();
    writeln!(f, "{{\"text\":\"能退回吗？\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:01:10Z\"}}").unwrap();
    writeln!(f, "{{\"text\":\"我不同意现在回退\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:01:20Z\"}}").unwrap();
    drop(f);
    for (id, q) in [(11, "退回"), (12, "现在回退")] {
        let bad = call(&mut s, json!(id), "request_rewind", json!({"decision": "approve", "user_quote": q}));
        assert!(is_error(&bad), "否定/疑问片段 `{q}` 不得批准回退：{}", text_of(&bad));
    }

    // 真实批准退回后，旧提议已结算
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"没问题，同意退回\",\"user_actor\":\"user\",\"at\":\"2026-11-13T03:01:30Z\"}}").unwrap();
    drop(f);
    let ok_rew = call(&mut s, json!(13), "request_rewind", json!({"decision": "approve", "user_quote": "同意退回"}));
    assert!(!is_error(&ok_rew), "{}", text_of(&ok_rew));
    assert_eq!(s.host().phase().as_str(), "intake");
}

#[test]
fn task_scope_and_conditional_followup_survive_resume_cleanly() {
    use std::io::Write;

    // 1) Cost -> --resume -> complete_task -> begin_followup_task -> Consultation
    let home1 = temp_home("resume-scope-cost");
    let h1 = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home1.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open");
    let mut s1 = Server::new(Cursor::new(Vec::new()), Vec::new(), h1);
    assert!(!is_error(&call(
        &mut s1,
        json!(1),
        "select_task_scope",
        json!({"scope": "cost", "reason": "先算租金"}),
    )));
    assert!(!is_error(&call(
        &mut s1,
        json!(2),
        "business_cost",
        json!({
            "operation": "create",
            "scenario": "A店",
            "currency": "CNY",
            "costs_complete": false,
            "costs": [{
                "scenario": "A店",
                "name": "租金",
                "treatment": "cash_expense",
                "amount": null,
                "period": "month",
                "basis": "user_provided",
                "source": "待报价"
            }]
        }),
    )));
    let case_id = s1.host().case_id().to_string();
    drop(s1);

    let h1_resumed = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home1),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id),
    })
    .expect("resume cost");
    let mut s1_res = Server::new(Cursor::new(Vec::new()), Vec::new(), h1_resumed);
    let comp = call(
        &mut s1_res,
        json!(3),
        "complete_task",
        json!({"summary": "已登记A店待报价月租。", "missing": ["月租报价"]}),
    );
    assert!(!is_error(&comp), "--resume 后 complete_task 必须恢复 task_scope：{}", text_of(&comp));
    let fu = call(
        &mut s1_res,
        json!(4),
        "begin_followup_task",
        json!({"reason": "继续做完整竞争推演"}),
    );
    assert!(!is_error(&fu), "--resume 后 begin_followup_task 必须成功：{}", text_of(&fu));
    let consult = call(
        &mut s1_res,
        json!(5),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "开启完整推演"}),
    );
    assert!(!is_error(&consult), "{}", text_of(&consult));

    // 2) Conditional -> --resume -> begin_followup_task 必须清理下游建模产物，不得回流到 intake
    let home2 = temp_home("resume-cond-followup");
    let runtime_dir = home2.join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        &ledger,
        format!("{{\"text\":\"我方收益为3，对手收益不知道\",\"user_actor\":\"user\",\"unix_secs\":{now_secs},\"at\":\"2026-11-13T04:00:00Z\"}}\n"),
    )
    .unwrap();

    let h2 = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home2.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open2");
    let mut s2 = Server::new(Cursor::new(Vec::new()), Vec::new(), h2);
    call(&mut s2, json!(1), "select_task_scope", json!({"scope": "consultation", "reason": "完整分析"}));
    call(&mut s2, json!(2), "vn_map", json!({}));
    call(
        &mut s2,
        json!(3),
        "participant_profile_update",
        json!({"participant_id": "us", "value_net_role": "focal", "relation_to_game": "player", "common": {"interests": ["利润"], "resources": ["门店"]}}),
    );
    call(
        &mut s2,
        json!(4),
        "participant_profile_update",
        json!({"participant_id": "rival", "value_net_role": "substitutor", "relation_to_game": "player", "common": {"interests": ["份额"], "resources": ["品牌"]}}),
    );
    call(&mut s2, json!(5), "checkpoint", json!({"items": [{"id": "i1", "content": "双方边界"}]}));
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"同意双方边界\",\"user_actor\":\"user\",\"at\":\"2026-11-13T04:00:10Z\"}}").unwrap();
    drop(f);
    assert!(!is_error(&call(&mut s2, json!(6), "checkpoint", json!({"decision": "approve", "user_quote": "同意双方边界"}))));
    assert_eq!(s2.host().phase().as_str(), "model_build");

    // 显式交付条件分析
    let cond = call(&mut s2, json!(7), "conditional_analysis", json!({"reason": "对手收益尚未取得"}));
    assert!(!is_error(&cond), "{}", text_of(&cond));
    let cond_case = s2.host().case_id().to_string();
    drop(s2);

    // 跨 --resume 重开后续任务：必须成功回到 intake
    let h2_res = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home2),
        auto_approve_checkpoints: false,
        resume_case: Some(cond_case),
    })
    .expect("resume cond");
    let mut s2_res = Server::new(Cursor::new(Vec::new()), Vec::new(), h2_res);
    let reopen = call(
        &mut s2_res,
        json!(8),
        "begin_followup_task",
        json!({"reason": "先单算房租预算"}),
    );
    assert!(!is_error(&reopen), "条件分析 --resume 后 begin_followup_task 必须成功：{}", text_of(&reopen));
    assert_eq!(s2_res.host().phase().as_str(), "intake");
    assert!(!is_error(&call(
        &mut s2_res,
        json!(9),
        "select_task_scope",
        json!({"scope": "cost", "reason": "单算房租预算"}),
    )));
}

fn evidenced_pd_ledger_clauses() -> &'static str {
    "我方维持同得收益为3元。我方受损收益为0元。我方抢占收益为5元。我方互降收益为1元。\
     对手维持同得收益为3元。对手抢占收益为5元。对手受损收益为0元。对手互降收益为1元。"
}

fn evidenced_pd_gamespec() -> Value {
    json!({
        "game_id": "mcp-evidenced-pd",
        "players": [
            {"id": "us", "name": "我方", "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": "对手", "vn_role": "substitutor", "relation_to_game": "player"}
        ],
        "strategies": {"us": ["维持", "降价"], "rival": ["维持", "降价"]},
        "payoffs": {"form": "normal", "values": {"cells": {
            "维持,维持": {"us": {"kind": "ref", "value": "u_mm"}, "rival": {"kind": "ref", "value": "r_mm"}},
            "维持,降价": {"us": {"kind": "ref", "value": "u_md"}, "rival": {"kind": "ref", "value": "r_md"}},
            "降价,维持": {"us": {"kind": "ref", "value": "u_dm"}, "rival": {"kind": "ref", "value": "r_dm"}},
            "降价,降价": {"us": {"kind": "ref", "value": "u_dd"}, "rival": {"kind": "ref", "value": "r_dd"}}
        }}},
        "params": {
            "u_mm": {"name": "我方维持收益", "value": {"kind": "literal", "value": 3}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "我方维持同得收益为3元"}},
            "u_md": {"name": "我方受损收益", "value": {"kind": "literal", "value": 0}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "我方受损收益为0元"}},
            "u_dm": {"name": "我方抢占收益", "value": {"kind": "literal", "value": 5}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "我方抢占收益为5元"}},
            "u_dd": {"name": "我方互降收益", "value": {"kind": "literal", "value": 1}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "我方互降收益为1元"}},
            "r_mm": {"name": "对手维持收益", "value": {"kind": "literal", "value": 3}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "对手维持同得收益为3元"}},
            "r_md": {"name": "对手抢占收益", "value": {"kind": "literal", "value": 5}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "对手抢占收益为5元"}},
            "r_dm": {"name": "对手受损收益", "value": {"kind": "literal", "value": 0}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "对手受损收益为0元"}},
            "r_dd": {"name": "对手互降收益", "value": {"kind": "literal", "value": 1}, "unit": "元", "source": {"kind": "user_confirmed", "checkpoint": "cp2", "user_quote": "对手互降收益为1元"}}
        }
    })
}

#[test]
fn whatif_mode_can_skip_optional_solve_into_consequence_and_survives_resume() {
    use std::io::Write;
    let home = temp_home("whatif-skip-solve");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    std::fs::write(
        &ledger,
        format!("{{\"text\":\"{}\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:00Z\"}}\n", evidenced_pd_ledger_clauses()),
    )
    .unwrap();

    let h = SessionHost::open(HostConfig {
        mode: ConsultationMode::Whatif,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open whatif host");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), h);

    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "直接假设我方降价后对手如何反应", "mode": "whatif"}),
    );
    call(&mut s, json!(2), "vn_map", json!({}));
    call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({"participant_id": "us", "value_net_role": "focal", "relation_to_game": "player", "common": {"interests": ["利润"]}}),
    );
    call(
        &mut s,
        json!(4),
        "participant_profile_update",
        json!({"participant_id": "rival", "value_net_role": "substitutor", "relation_to_game": "player", "common": {"interests": ["份额"]}}),
    );
    call(
        &mut s,
        json!(5),
        "checkpoint",
        json!({"items": [{"id": "c1", "content": "确认参与者边界"}]}),
    );
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"同意参与者边界\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:05Z\"}}").unwrap();
    drop(f);
    assert!(!is_error(&call(
        &mut s,
        json!(6),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "同意参与者边界"}),
    )));
    assert_eq!(s.host().phase().as_str(), "model_build");

    let build = call(&mut s, json!(7), "gamespec_build", evidenced_pd_gamespec());
    assert!(!is_error(&build), "gamespec_build 失败：{}", text_of(&build));
    let cp2_open = call(
        &mut s,
        json!(8),
        "checkpoint",
        json!({"items": [{"id": "c2", "content": "确认博弈矩阵与假设", "source": "用户明确说明收益", "proposed_value": "3 / 0 / 5 / 1"}]}),
    );
    assert!(!is_error(&cp2_open), "打开 cp2 失败：{}", text_of(&cp2_open));
    let mut f2 = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f2, "{{\"text\":\"确认博弈模型\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:10Z\"}}").unwrap();
    drop(f2);
    assert!(!is_error(&call(
        &mut s,
        json!(9),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "确认博弈模型"}),
    )));
    assert_eq!(s.host().phase().as_str(), "solve");

    // 不得借绕过跳过非 consequence 工具（例如在 solve 直接调 report_render 仍须被拒）
    let wrong = call(&mut s, json!(10), "report_render", json!({}));
    assert!(is_error(&wrong), "无关阶段工具不得触发 solve 跳过");
    assert_eq!(s.host().phase().as_str(), "solve");

    // 直接调 consequence_predict：必须自动跳过可选 solve 进入 consequence 执行，并在 cp3 前驻留
    let conseq = call(&mut s, json!(11), "consequence_predict", json!({"decision": "降价"}));
    assert!(
        !is_error(&conseq),
        "whatif 在 solve 直接调用 consequence_predict 应自动跳过 solve：{}",
        text_of(&conseq)
    );
    assert_eq!(s.host().phase().as_str(), "consequence");

    let case_id = s.host().case_id().to_string();
    drop(s);

    // --resume 重放 PhaseEntered(solve -> consequence) 必须成功恢复 consequence 与推演产物；
    // 随后经真实 cp3 确认推进至 recommend 并顺利生成建议。
    let h_res = SessionHost::open(HostConfig {
        mode: ConsultationMode::Whatif,
        playbook_id: None,
        home: Some(home),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id),
    })
    .expect("resume whatif host");
    let mut s_res = Server::new(Cursor::new(Vec::new()), Vec::new(), h_res);
    assert_eq!(s_res.host().phase().as_str(), "consequence");

    let cp3_open = call(
        &mut s_res,
        json!(12),
        "checkpoint",
        json!({"items": [{"id": "c3", "content": "确认推演条件与对手最佳反应", "source": "consequence_predict 确定性推演", "proposed_value": "对手降价"}]}),
    );
    assert!(!is_error(&cp3_open), "打开 cp3 失败：{}", text_of(&cp3_open));
    let mut f3 = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f3, "{{\"text\":\"同意推演结果，进入建议阶段\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:15Z\"}}").unwrap();
    drop(f3);
    assert!(!is_error(&call(
        &mut s_res,
        json!(13),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "同意推演结果"}),
    )));
    assert_eq!(s_res.host().phase().as_str(), "recommend");

    let rec = call(
        &mut s_res,
        json!(14),
        "recommend_build",
        json!({"headline": "降价后对手跟进降价", "actions": [{"action": "降价", "rationale": "纳什最佳反应"}]}),
    );
    assert!(
        !is_error(&rec),
        "whatif --resume 后 recommend_build 必须保留 consequence_report：{}",
        text_of(&rec)
    );
}

#[test]
fn open_resumed_replays_constraint_check_novel_move_filter_and_in_place_rewind_cleanly() {
    use std::io::Write;
    let home = temp_home("replay-m2-tools");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    std::fs::write(
        &ledger,
        format!(
            "{{\"text\":\"我的月租上限25000元。{}\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:00Z\"}}\n",
            evidenced_pd_ledger_clauses()
        ),
    )
    .unwrap();

    // Part 1: constraint_check + novel_move_filter 跨 --resume 恢复状态
    let h = SessionHost::open(HostConfig {
        mode: ConsultationMode::Whatif,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open replay test");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), h);
    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "检验约束与新策略重放", "mode": "whatif"}),
    );
    let cr = call(&mut s, json!(2), "constraint_register", json!({"quote": "我的月租上限25000元"}));
    assert!(!is_error(&cr), "constraint_register 失败：{}", text_of(&cr));
    let cost = call(
        &mut s,
        json!(3),
        "business_cost",
        json!({
            "operation": "create",
            "scenario": "稳妥选址",
            "currency": "CNY",
            "commitment_months": 12,
            "horizon_months": 12,
            "costs_complete": true,
            "costs": [{"scenario": "稳妥选址", "name": "租金", "constraint_metric": "rent", "treatment": "cash_expense", "amount": 20000, "period": "month", "basis": "user_provided", "source": "房东报价"}]
        }),
    );
    assert!(!is_error(&cost), "{}", text_of(&cost));
    call(&mut s, json!(4), "vn_map", json!({}));
    call(
        &mut s,
        json!(5),
        "participant_profile_update",
        json!({"participant_id": "us", "value_net_role": "focal", "relation_to_game": "player", "common": {"interests": ["利润"]}}),
    );
    call(
        &mut s,
        json!(6),
        "participant_profile_update",
        json!({"participant_id": "rival", "value_net_role": "substitutor", "relation_to_game": "player", "common": {"interests": ["份额"]}}),
    );
    call(&mut s, json!(7), "checkpoint", json!({"items": [{"id": "c1", "content": "确认情报"}]}));
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f, "{{\"text\":\"同意情报\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:05Z\"}}").unwrap();
    drop(f);
    assert!(!is_error(&call(&mut s, json!(8), "checkpoint", json!({"decision": "approve", "user_quote": "同意情报"}))));
    assert_eq!(s.host().phase().as_str(), "model_build");

    let cc = call(&mut s, json!(9), "constraint_check", json!({}));
    assert!(!is_error(&cc), "{}", text_of(&cc));
    let cc_v: Value = serde_json::from_str(&payload_of(&cc)).unwrap();
    let rev = cc_v["constraint_revision"].as_u64().unwrap();
    let sid = cc_v["eligible_scenario_ids"][0]
        .as_str()
        .unwrap_or_else(|| panic!("cc_v 缺少 eligible_scenario_ids：{cc_v}"))
        .to_string();

    let mut spec = evidenced_pd_gamespec();
    spec["constraint_revision"] = json!(rev);
    spec["strategy_bindings"] = json!({"us": {"维持": sid.clone(), "降价": sid}});
    let build = call(&mut s, json!(10), "gamespec_build", spec);
    assert!(!is_error(&build), "gamespec_build 失败：{}", text_of(&build));

    let cp2_open = call(&mut s, json!(11), "checkpoint", json!({"items": [{"id": "c2", "content": "确认模型", "source": "用户明确说明收益", "proposed_value": "3 / 0 / 5 / 1"}]}));
    assert!(!is_error(&cp2_open), "打开 cp2 失败：{}", text_of(&cp2_open));
    let mut f2 = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(f2, "{{\"text\":\"确认模型\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:10Z\"}}").unwrap();
    drop(f2);
    assert!(!is_error(&call(&mut s, json!(12), "checkpoint", json!({"decision": "approve", "user_quote": "确认模型"}))));
    assert_eq!(s.host().phase().as_str(), "solve");

    // 跳过 solve 后在 consequence 注册非常规动作
    let nmf = call(
        &mut s,
        json!(13),
        "novel_move_filter",
        json!({
            "actor_id": "us",
            "description": "向大客户披露合规承诺",
            "payoff_channel": "巩固长期签约留存",
            "expected_benefit": {"lo": 100.0, "hi": 200.0},
            "expected_cost": {"lo": 1.0, "hi": 5.0},
            "legality_basis": ["《反不正当竞争法》第十一条"],
            "feasibility_basis": ["已有客户渠道"],
            "introduces_actors": []
        }),
    );
    assert!(!is_error(&nmf), "{}", text_of(&nmf));
    assert_eq!(s.host().phase().as_str(), "consequence");

    let case_id = s.host().case_id().to_string();
    drop(s);

    // --resume 后必须能直接推演刚通过 novel_move_filter 注册的新策略，且 constraint_check 审阅态已恢复
    let h_res = SessionHost::open(HostConfig {
        mode: ConsultationMode::Whatif,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id),
    })
    .expect("resume replay test");
    let mut s_res = Server::new(Cursor::new(Vec::new()), Vec::new(), h_res);
    assert_eq!(s_res.host().phase().as_str(), "consequence");
    let pred_novel = call(
        &mut s_res,
        json!(14),
        "consequence_predict",
        json!({"decision": "[非常规] 向大客户披露合规承诺"}),
    );
    let pred_msg = text_of(&pred_novel);
    assert!(
        pred_msg.contains("[非常规] 向大客户披露合规承诺") && pred_msg.contains("支付矩阵缺 2 个策略组合"),
        "--resume 后 GameSpec 必须保留 novel_move_filter 注册的新策略（不能回退成未声明策略）：{pred_msg}"
    );

    // Part 2: 在位修订 PhaseRewound { from: "model_build", to: "model_build" } 重放后不得复活 GameSpecValidated
    let home2 = temp_home("replay-in-place-rewind");
    let rt2 = home2.join("runtime");
    std::fs::create_dir_all(&rt2).unwrap();
    let ledger2 = rt2.join("trusted_user_inputs.jsonl");
    std::fs::write(
        &ledger2,
        format!("{{\"text\":\"{}\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:00Z\"}}\n", evidenced_pd_ledger_clauses()),
    )
    .unwrap();
    let h2 = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home2.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .unwrap();
    let mut s2 = Server::new(Cursor::new(Vec::new()), Vec::new(), h2);
    call(&mut s2, json!(1), "select_task_scope", json!({"scope": "consultation", "reason": "完整分析"}));
    call(&mut s2, json!(2), "vn_map", json!({}));
    call(&mut s2, json!(3), "participant_profile_update", json!({"participant_id": "us", "value_net_role": "focal", "relation_to_game": "player", "common": {"interests": ["利润"]}}));
    call(&mut s2, json!(4), "participant_profile_update", json!({"participant_id": "rival", "value_net_role": "substitutor", "relation_to_game": "player", "common": {"interests": ["份额"]}}));
    call(&mut s2, json!(5), "checkpoint", json!({"items": [{"id": "c1", "content": "确认情报"}]}));
    let mut f3 = std::fs::OpenOptions::new().append(true).open(&ledger2).unwrap();
    writeln!(f3, "{{\"text\":\"同意情报\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:05Z\"}}").unwrap();
    drop(f3);
    assert!(!is_error(&call(&mut s2, json!(6), "checkpoint", json!({"decision": "approve", "user_quote": "同意情报"}))));
    assert!(!is_error(&call(&mut s2, json!(7), "gamespec_build", evidenced_pd_gamespec())));
    assert!(!is_error(&call(&mut s2, json!(8), "checkpoint", json!({"items": [{"id": "c2", "content": "确认模型", "source": "用户明确说明收益", "proposed_value": "3 / 0 / 5 / 1"}]}))));
    let case_id2 = s2.host().case_id().to_string();
    let case_dir2 = s2.host().case_dir().expect("case_dir2").to_path_buf();
    drop(s2);

    // 模拟在位修订写入 CheckpointAnswered("修改：...") + PhaseRewound(model_build -> model_build)
    let log_path = case_dir2.join("case.jsonl");
    let existing = std::fs::read_to_string(&log_path).unwrap();
    let last_seq = existing
        .lines()
        .filter(|l| !l.trim().is_empty())
        .last()
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["seq"].as_u64())
        .unwrap_or(0);
    let mut lf = std::fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(
        lf,
        "{}",
        json!({
            "seq": last_seq + 1,
            "event": {
                "type": "checkpoint_answered",
                "id": "cp2",
                "answers": ["修改：我方月租金上限改为15000元"],
                "user_actor": "user",
                "at": "2026-11-15T00:00:20Z"
            }
        })
    )
    .unwrap();
    writeln!(
        lf,
        "{}",
        json!({
            "seq": last_seq + 2,
            "event": {
                "type": "phase_rewound",
                "from": "model_build",
                "to": "model_build",
                "due_to": "用户修订本轮模型",
                "at": "2026-11-15T00:00:21Z"
            }
        })
    )
    .unwrap();
    drop(lf);

    let h2_res = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home2),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id2),
    })
    .expect("resume in-place rewind");
    let mut s2_res = Server::new(Cursor::new(Vec::new()), Vec::new(), h2_res);
    assert_eq!(s2_res.host().phase().as_str(), "model_build");
    let val = call(&mut s2_res, json!(9), "gamespec_validate", json!({}));
    assert!(
        is_error(&val),
        "在位 PhaseRewound(model_build -> model_build) 重放后不得复活已清除的 GameSpec：{}",
        text_of(&val)
    );
}

// ─────────────────────────── 13. swarm_run / data_ingest 离线 --resume 恢复 ───────────────────────────

fn read_http_request_body(stream: &mut std::net::TcpStream) -> String {
    use std::io::Read;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        match stream.read(&mut chunk) {
            Ok(0) => return String::new(),
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos + 4;
                }
            }
            Err(_) => return String::new(),
        }
    };
    let header_str = String::from_utf8_lossy(&buf[..header_end]);
    let content_length = header_str
        .lines()
        .find_map(|line| {
            let (k, v) = line.split_once(':')?;
            if k.trim().eq_ignore_ascii_case("content-length") {
                v.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let body_end = (header_end + content_length).min(buf.len());
    String::from_utf8_lossy(&buf[header_end..body_end]).to_string()
}

struct MockSwarmHttpServer {
    base_url: String,
    hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MockSwarmHttpServer {
    fn start() -> Self {
        use std::io::Write;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind mock LLM port");
        let addr = listener.local_addr().expect("local addr");
        listener.set_nonblocking(true).expect("nonblocking listener");

        let hits = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_bg = hits.clone();
        let stop_bg = stop.clone();

        let handle = std::thread::spawn(move || {
            while !stop_bg.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        hits_bg.fetch_add(1, Ordering::SeqCst);
                        let body = read_http_request_body(&mut stream);
                        let content = if body.contains("你（us）") {
                            json!({
                                "kind": "novel",
                                "description": "向大客户披露合规承诺",
                                "rationale": "巩固大客户留存",
                                "introduces": [],
                                "legality_basis": ["《反不正当竞争法》第十一条"],
                                "feasibility_basis": ["已有客户渠道"],
                                "expected_benefit": [100, 200],
                                "expected_cost": [1, 5]
                            })
                            .to_string()
                        } else {
                            json!({
                                "kind": "in_matrix",
                                "strategy": "维持",
                                "rationale": "暂维持原价观望"
                            })
                            .to_string()
                        };
                        let resp_json = json!({
                            "id": "chatcmpl-mock",
                            "object": "chat.completion",
                            "model": "test-model",
                            "choices": [{
                                "index": 0,
                                "message": {
                                    "role": "assistant",
                                    "content": content
                                },
                                "finish_reason": "stop"
                            }]
                        })
                        .to_string();
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            resp_json.len(),
                            resp_json
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            base_url: format!("http://{addr}/v1"),
            hits,
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for MockSwarmHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[test]
fn resume_swarm_run_makes_zero_llm_http_calls_and_restores_state() {
    use std::io::Write;
    use std::sync::atomic::Ordering;

    let mock = MockSwarmHttpServer::start();
    let home = temp_home("resume-swarm-offline");
    let runtime_dir = home.join("runtime");
    std::fs::create_dir_all(&runtime_dir).unwrap();
    let ledger = runtime_dir.join("trusted_user_inputs.jsonl");
    std::fs::write(
        &ledger,
        format!(
            "{{\"text\":\"{}\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:00Z\"}}\n",
            evidenced_pd_ledger_clauses()
        ),
    )
    .unwrap();

    let h = SessionHost::open_with_swarm_override(
        HostConfig {
            mode: ConsultationMode::Whatif,
            playbook_id: None,
            home: Some(home.clone()),
            auto_approve_checkpoints: false,
            resume_case: None,
        },
        Some(shangyi_core::domain_exec::openai_swarm_actor_factory_for_test(
            &mock.base_url,
            "test-model",
        )),
    )
    .expect("open initial host");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), h);

    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "检验 swarm_run 离线重放", "mode": "whatif"}),
    );
    call(&mut s, json!(2), "vn_map", json!({}));
    call(
        &mut s,
        json!(3),
        "participant_profile_update",
        json!({"participant_id": "us", "value_net_role": "focal", "relation_to_game": "player", "common": {"interests": ["利润"]}}),
    );
    call(
        &mut s,
        json!(4),
        "participant_profile_update",
        json!({"participant_id": "rival", "value_net_role": "substitutor", "relation_to_game": "player", "common": {"interests": ["份额"]}}),
    );
    call(
        &mut s,
        json!(5),
        "checkpoint",
        json!({"items": [{"id": "c1", "content": "确认参与者"}]}),
    );
    let mut f = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(
        f,
        "{{\"text\":\"同意参与者\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:05Z\"}}"
    )
    .unwrap();
    drop(f);
    assert!(!is_error(&call(
        &mut s,
        json!(6),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "同意参与者"}),
    )));
    assert!(!is_error(&call(
        &mut s,
        json!(7),
        "gamespec_build",
        evidenced_pd_gamespec(),
    )));
    assert!(!is_error(&call(
        &mut s,
        json!(8),
        "checkpoint",
        json!({"items": [{"id": "c2", "content": "确认模型", "source": "用户明确说明收益", "proposed_value": "3 / 0 / 5 / 1"}]}),
    )));
    let mut f2 = std::fs::OpenOptions::new().append(true).open(&ledger).unwrap();
    writeln!(
        f2,
        "{{\"text\":\"确认模型\",\"user_actor\":\"user\",\"at\":\"2026-11-15T00:00:10Z\"}}"
    )
    .unwrap();
    drop(f2);
    assert!(!is_error(&call(
        &mut s,
        json!(9),
        "checkpoint",
        json!({"decision": "approve", "user_quote": "确认模型"}),
    )));
    assert_eq!(s.host().phase().as_str(), "solve");

    // 首次在真实（mock HTTP）LLM 工厂下运行 swarm_run，应命中 mock HTTP 并注册非常规策略
    let sw = call(&mut s, json!(10), "swarm_run", json!({"rollouts": 1}));
    assert!(!is_error(&sw), "初始 swarm_run 应成功：{}", text_of(&sw));
    assert!(mock.hits.load(Ordering::SeqCst) > 0, "首次 swarm_run 应调用 mock LLM HTTP");
    assert_eq!(s.host().phase().as_str(), "consequence");
    assert!(s.host().domain_state().swarm_output.is_some());
    assert!(
        s.host()
            .domain_state()
            .game_spec
            .as_ref()
            .unwrap()
            .strategies["us"]
            .contains(&"[非常规] 向大客户披露合规承诺".to_string())
    );

    let case_id = s.host().case_id().to_string();
    drop(s);

    // 清零计数器并执行 --resume（仍传入活跃的 mock HTTP SwarmActorFactory）
    mock.hits.store(0, Ordering::SeqCst);
    let resumed_host = SessionHost::open_with_swarm_override(
        HostConfig {
            mode: ConsultationMode::Whatif,
            playbook_id: None,
            home: Some(home.clone()),
            auto_approve_checkpoints: false,
            resume_case: Some(case_id),
        },
        Some(shangyi_core::domain_exec::openai_swarm_actor_factory_for_test(
            &mock.base_url,
            "test-model",
        )),
    )
    .expect("resume with swarm override");

    assert_eq!(
        mock.hits.load(Ordering::SeqCst),
        0,
        "--resume 重放历史 swarm_run 绝不得发起任何 LLM HTTP 请求"
    );
    assert_eq!(resumed_host.phase().as_str(), "consequence");
    let restored_swarm = resumed_host
        .domain_state()
        .swarm_output
        .as_ref()
        .expect("--resume 应从日志恢复 swarm_output");
    assert_eq!(
        restored_swarm["counts"]["novel_moves_registered"],
        json!(1)
    );
    assert!(
        resumed_host
            .domain_state()
            .game_spec
            .as_ref()
            .unwrap()
            .strategies["us"]
            .contains(&"[非常规] 向大客户披露合规承诺".to_string()),
        "--resume 应将已接受的非常规策略恢复进 GameSpec"
    );

    // 下游 consequence_predict 应能识别恢复出的 [非常规] 策略，且仍保持 0 次 LLM HTTP 请求
    let mut s_res = Server::new(Cursor::new(Vec::new()), Vec::new(), resumed_host);
    let pred_novel = call(
        &mut s_res,
        json!(11),
        "consequence_predict",
        json!({"decision": "[非常规] 向大客户披露合规承诺"}),
    );
    let pred_msg = text_of(&pred_novel);
    assert!(
        pred_msg.contains("[非常规] 向大客户披露合规承诺") && pred_msg.contains("支付矩阵缺 2 个策略组合"),
        "--resume 后 GameSpec 必须保留 swarm_run 注册的非常规策略：{pred_msg}"
    );
    assert_eq!(mock.hits.load(Ordering::SeqCst), 0);

    // --resume 后新发起的 swarm_run 仍可正常走已装配的 LLM factory
    let live_post_resume = call(&mut s_res, json!(12), "swarm_run", json!({"rollouts": 1}));
    assert!(
        !is_error(&live_post_resume),
        "--resume 后新调用 swarm_run 应继续可用：{}",
        text_of(&live_post_resume)
    );
    assert!(
        mock.hits.load(Ordering::SeqCst) > 0,
        "--resume 后新调用 swarm_run 应正常访问装配的 LLM factory"
    );
    drop(s_res);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn resume_data_ingest_restores_ingested_report_without_sidecar_or_csv() {
    use std::io::Write;
    use shangyi_intel::sidecar::{
        ColumnHint, ComputationBoundary, EncodingDetail, FileInfo, HeaderInfo, IngestReport,
    };

    let home = temp_home("resume-data-ingest");
    let h = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: None,
    })
    .expect("open initial host");
    let mut s = Server::new(Cursor::new(Vec::new()), Vec::new(), h);
    call(
        &mut s,
        json!(1),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "检验 data_ingest 离线重放"}),
    );
    call(&mut s, json!(2), "vn_map", json!({}));
    assert_eq!(s.host().phase().as_str(), "intel");

    let case_id = s.host().case_id().to_string();
    let case_dir = s.host().case_dir().expect("case_dir").to_path_buf();
    drop(s);

    // 构造指向磁盘不存在 CSV 路径的历史 data_ingest 日志记录（模拟原 CSV 已删或无侧车环境）
    let missing_csv = "/nonexistent/deleted-after-ingest/sales.csv";
    let report = IngestReport {
        file: FileInfo {
            path: missing_csv.to_string(),
            size_bytes: 256,
            mtime_ms: 1_700_000_000_000,
            sha256: Some("deadbeef".to_string()),
            digest_note: None,
        },
        encoding: "utf-8".to_string(),
        encoding_detail: EncodingDetail {
            method: "utf8_strict".to_string(),
            candidates_tried: vec!["utf-8".to_string()],
            pinned: false,
            note: String::new(),
        },
        delimiter: ",".to_string(),
        delimiter_source: "sniffer".to_string(),
        columns: vec!["日期".to_string(), "单价".to_string(), "销量".to_string()],
        columns_raw: vec!["日期".to_string(), "单价".to_string(), "销量".to_string()],
        header: HeaderInfo {
            has_header: true,
            confidence: "high".to_string(),
            evidence: "首行全为文本表头".to_string(),
            first_row_raw: vec!["日期".to_string(), "单价".to_string(), "销量".to_string()],
        },
        row_count: 42,
        row_count_exact: true,
        scanned_rows: 42,
        scan_limit: 100_000,
        sample_rows: Vec::new(),
        sample_rows_truncated: false,
        column_hints: vec![ColumnHint {
            column: "单价".to_string(),
            hint: "number".to_string(),
            evidence: "抽样全部可解析为数值".to_string(),
            non_empty: 42,
            empty: 0,
            examples: vec!["18.0".to_string()],
            alternatives: Vec::new(),
        }],
        column_count: 3,
        ragged_rows: 0,
        blank_lines: 0,
        notes: Vec::new(),
        computation_boundary: ComputationBoundary::default(),
    };
    let report_json = serde_json::to_string_pretty(&report).expect("serialize IngestReport");

    let log_path = case_dir.join("case.jsonl");
    let existing = std::fs::read_to_string(&log_path).unwrap();
    let last_seq = existing
        .lines()
        .filter(|l| !l.trim().is_empty())
        .last()
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["seq"].as_u64())
        .unwrap_or(0);
    let mut lf = std::fs::OpenOptions::new().append(true).open(&log_path).unwrap();
    writeln!(
        lf,
        "{}",
        json!({
            "seq": last_seq + 1,
            "event": {
                "type": "tool_call_started",
                "step": {"turn": 1, "step": 3},
                "call_id": "mcp-0003",
                "name": "data_ingest",
                "arguments": json!({"path": missing_csv}).to_string(),
                "at": "2026-11-15T00:00:05Z"
            }
        })
    )
    .unwrap();
    writeln!(
        lf,
        "{}",
        json!({
            "seq": last_seq + 2,
            "event": {
                "type": "tool_call_finished",
                "call_id": "mcp-0003",
                "outcome": {
                    "status": "ok",
                    "output": report_json
                },
                "at": "2026-11-15T00:00:06Z"
            }
        })
    )
    .unwrap();
    drop(lf);

    let resumed_host = SessionHost::open(HostConfig {
        mode: ConsultationMode::Scenario,
        playbook_id: None,
        home: Some(home.clone()),
        auto_approve_checkpoints: false,
        resume_case: Some(case_id),
    })
    .expect("resume with data_ingest snapshot");

    assert_eq!(resumed_host.phase().as_str(), "intel");
    let ingested = &resumed_host.domain_state().ingested;
    assert_eq!(
        ingested.len(),
        1,
        "--resume 应离线恢复 data_ingest 报告而不依赖原 CSV 或 Python 侧车"
    );
    assert_eq!(ingested[0].file.path, missing_csv);
    assert_eq!(ingested[0].columns, vec!["日期", "单价", "销量"]);
    assert_eq!(ingested[0].row_count, 42);
    assert!(ingested[0].row_count_exact);
    drop(resumed_host);
    let _ = std::fs::remove_dir_all(&home);
}

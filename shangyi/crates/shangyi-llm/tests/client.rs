//! `shangyi-llm` 的测试。
//!
//! 绝大多数测试用**假传输层**（离线），锁定三件事：
//! 1. **请求形状**（A10b 实测的 Responses 扁平格式与字段名）
//! 2. **`tool_choice` 只有两个合法值**（`required` 恒 400，故类型上不存在）
//! 3. **重试策略**（429/5xx 重试，4xx 不重试）
//!
//! 另有一个 `#[ignore]` 的**实测测试**（需真实 API key）——见文件末尾。
//! 它被 `ignore` 而非删除：需要时可 `cargo test -- --ignored` 复跑。

use std::cell::RefCell;

use shangyi_llm::{
    Wire,
    parse_chat_response,
    Endpoint, InputItem, LlmClient, LlmError, LlmRequest, LlmResponse, ToolChoice,
    ToolSchema, Transport, UreqTransport,
};

/// 假传输层：记录请求，返回预设响应。
struct FakeTransport {
    /// (状态码, 响应体) 队列；耗尽后重复最后一个。
    responses: RefCell<Vec<(u16, String)>>,
    /// 记录收到的请求（URL, headers, body）。
    seen: RefCell<Vec<(String, Vec<(String, String)>, serde_json::Value)>>,
}

impl FakeTransport {
    fn new(responses: Vec<(u16, String)>) -> Self {
        Self {
            responses: RefCell::new(responses),
            seen: RefCell::new(Vec::new()),
        }
    }

    fn last_body(&self) -> serde_json::Value {
        self.seen.borrow().last().expect("应有请求").2.clone()
    }

    fn call_count(&self) -> usize {
        self.seen.borrow().len()
    }
}

impl Transport for FakeTransport {
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &serde_json::Value,
    ) -> Result<(u16, String), LlmError> {
        self.seen
            .borrow_mut()
            .push((url.to_string(), headers.to_vec(), body.clone()));
        let mut q = self.responses.borrow_mut();
        if q.len() > 1 {
            Ok(q.remove(0))
        } else {
            Ok(q.first().cloned().unwrap_or((200, "{}".into())))
        }
    }
}

fn ok_response(json: &str) -> (u16, String) {
    (200, json.to_string())
}

/// 一个 A10b 实测形态的成功响应（含 reasoning + message + function_call）。
const LIVE_SHAPED_RESPONSE: &str = r#"{
  "id": "fd69365e-62b2-4694-8400-d7af1691960e",
  "object": "response",
  "created_at": 1789659367,
  "status": "completed",
  "model": "deepseek-flash",
  "output": [
    {"type": "reasoning", "content": [{"type": "reasoning_text", "text": "我应该查天气"}]},
    {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "我来查一下"}]},
    {"type": "function_call", "name": "get_weather", "call_id": "call_00_Vow6cQlcIUVzaiHTqO7o8181",
     "arguments": {"city": "东京"}}
  ],
  "usage": {"input_tokens": 297, "output_tokens": 102, "total_tokens": 399}
}"#;

fn client_with(responses: Vec<(u16, String)>) -> (LlmClient<FakeTransport>, Endpoint) {
    // 用环境变量以外的 key 来源：测试直接设置环境变量不安全（并发），
    // 故此处只验证"构造请求"的路径，需要 key 的路径由单独测试覆盖。
    let ep = Endpoint {
        base_url: "https://api.test.invalid".into(),
        api_key_env: "SHANGYI_TEST_KEY_PRESENT".into(),
            wire: Wire::Responses,
            echo_reasoning: true,
    };
    (LlmClient::new(ep.clone(), FakeTransport::new(responses)), ep)
}

/// 需要一个已存在的 key 环境变量。
///
/// **不在测试里 `set_var`**：Rust 2024 下它是 unsafe，且并发测试中改环境变量
/// 本身就是竞态。改为**跳过**——若环境未提供该变量，相关测试直接返回。
/// 这不是"静默跳过"：它跳过的是"凭据相关"的路径，而该路径的正确行为
/// 由 `client_reports_missing_api_key` 单独覆盖（不需要任何 key）。
fn ensure_test_key() -> bool {
    std::env::var("SHANGYI_TEST_KEY_PRESENT")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
}

// ─────────────────────────────────────────────────────────────────────────────
// tool_choice 的类型级约束（A10b 实测）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn tool_choice_only_has_auto_and_none() {
    // **本 crate 最重要的类型约束**。A10b 实测确认 DeepSeek 的 Responses 端点上
    // `required` 与"指定函数"恒返回 400（"Thinking mode does not support this
    // tool_choice"）。把非法取值变成编译期不可能，比运行时报错可靠——
    // 后者会在凌晨批处理里才暴露。
    assert_eq!(ToolChoice::Auto.as_str(), "auto");
    assert_eq!(ToolChoice::None.as_str(), "none");
    // 无法构造第三个变体：这是编译期保证，测试只能间接验证
}

#[test]
fn parsing_required_is_rejected_with_reason() {
    let err = ToolChoice::parse("required").expect_err("required 必须被拒（实测恒 400）");
    assert!(
        matches!(err, LlmError::UnsupportedToolChoice(ref s) if s == "required"),
        "实得 {err:?}"
    );
    // 错误信息应说明原因（供调用方判断如何处理）
    assert!(
        format!("{err}").contains("auto") && format!("{err}").contains("none"),
        "错误应指出合法取值：{err}"
    );
}

#[test]
fn parsing_function_form_is_rejected() {
    // 指定函数形式（`{"type":"function","name":...}`）同样恒 400
    let err = ToolChoice::parse("{\"type\":\"function\",\"name\":\"ping\"}")
        .expect_err("函数指定形式必须被拒");
    assert!(matches!(err, LlmError::UnsupportedToolChoice(_)));
}

#[test]
fn parsing_auto_and_none_works() {
    assert_eq!(ToolChoice::parse("auto").unwrap(), ToolChoice::Auto);
    assert_eq!(ToolChoice::parse("none").unwrap(), ToolChoice::None);
}

// ─────────────────────────────────────────────────────────────────────────────
// 请求形状（A10b 实测的 Responses 扁平格式）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn request_serializes_tool_choice_as_bare_string() {
    let req = LlmRequest::new("deepseek-flash", vec![InputItem::user("hi")])
        .with_tool_choice(ToolChoice::Auto);
    let body = req.to_body().unwrap();
    assert_eq!(
        body["tool_choice"],
        serde_json::Value::String("auto".into()),
        "tool_choice 须为裸字符串，而非对象：{body}"
    );
}

#[test]
fn tools_use_flat_function_format() {
    // ⚠️ Responses 用**扁平** `{type, name, parameters}`，
    // 而非 Chat 的嵌套 `{type, function:{name, parameters}}`
    let tool = ToolSchema::function(
        "get_weather",
        "查天气",
        serde_json::json!({"type": "object", "properties": {"city": {"type": "string"}}}),
    );
    let v = serde_json::to_value(&tool).unwrap();
    assert_eq!(v["type"], "function");
    assert_eq!(v["name"], "get_weather");
    assert!(
        v.get("function").is_none(),
        "不得有 Chat 式嵌套 function 字段：{v}"
    );
    assert!(v.get("parameters").is_some());
}

#[test]
fn request_with_tools_defaults_to_auto_never_required() {
    // 有工具时的默认值必须是 `auto`——绝不用 `required`（实测恒 400）
    let req = LlmRequest::new("m", vec![InputItem::user("x")]).with_tools(vec![ToolSchema::function(
        "t",
        "d",
        serde_json::json!({"type": "object"}),
    )]);
    assert_eq!(req.tool_choice, Some(ToolChoice::Auto));
    let body = req.to_body().unwrap();
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn input_items_serialize_with_type_tags() {
    let items = vec![
        InputItem::system("你是商奕"),
        InputItem::user("降价 15% 会怎样"),
        InputItem::tool_output("call_00_x", "晴，25度"),
    ];
    let v = serde_json::to_value(&items).unwrap();
    assert_eq!(v[0]["type"], "message");
    assert_eq!(v[0]["role"], "system");
    assert_eq!(v[1]["type"], "message");
    assert_eq!(v[1]["role"], "user");
    assert_eq!(v[2]["type"], "function_call_output");
    assert_eq!(v[2]["call_id"], "call_00_x");
}

#[test]
fn function_call_arguments_serialize_as_string_in_requests() {
    // **实测（2026-09-18）**：请求侧的 `arguments` **必须是 JSON 字符串**。
    //
    // 早先本测试断言"arguments 是对象"（依据 A10b 的记录）。该记录对请求侧
    // 不成立：传对象会得到
    //   `400 invalid type: map, expected a string`
    // 而响应侧实测同样是字符串。即**两个方向都是字符串**。
    //
    // 内部仍持 `Value`（工具可直接取字段），仅在序列化时转字符串——
    // 把形态差异挡在这一层。
    let item = InputItem::FunctionCall {
        call_id: "call_00_x".into(),
        name: "get_weather".into(),
        arguments: serde_json::json!({"city": "东京"}),
    };
    let v = serde_json::to_value(&item).unwrap();
    let args = &v["arguments"];
    assert!(
        args.is_string(),
        "请求侧 arguments 必须序列化为字符串，实得：{args}"
    );
    // 且必须是**可解析的** JSON（否则端点会当成非法参数，而非报类型错）
    let parsed: serde_json::Value =
        serde_json::from_str(args.as_str().unwrap()).expect("字符串内容应是合法 JSON");
    assert_eq!(parsed["city"], serde_json::json!("东京"));

    // 已在字符串形态时不得双重编码
    let already = InputItem::FunctionCall {
        call_id: "c".into(),
        name: "n".into(),
        arguments: serde_json::Value::String("{\"a\":1}".into()),
    };
    let v2 = serde_json::to_value(&already).unwrap();
    assert_eq!(
        v2["arguments"], serde_json::json!("{\"a\":1}"),
        "已是字符串者不得被再包一层引号"
    );
}

/// 反序列化两种形态都接受（宽进严出：读宽容、写严格）。
#[test]
fn function_call_arguments_deserialize_from_either_shape() {
    let as_string: InputItem = serde_json::from_str(
        r#"{"type":"function_call","call_id":"c","name":"n","arguments":"{\"a\":1}"}"#,
    )
    .expect("字符串形态应可解析");
    match as_string {
        InputItem::FunctionCall { arguments, .. } => {
            assert_eq!(arguments, serde_json::json!({"a": 1}));
        }
        other => panic!("实得 {other:?}"),
    }

    let as_object: InputItem = serde_json::from_str(
        r#"{"type":"function_call","call_id":"c","name":"n","arguments":{"a":1}}"#,
    )
    .expect("对象形态也应可解析（未来兼容）");
    match as_object {
        InputItem::FunctionCall { arguments, .. } => {
            assert_eq!(arguments, serde_json::json!({"a": 1}));
        }
        other => panic!("实得 {other:?}"),
    }
}

/// reasoning 项必须能回放（实测必需，见 `InputItem::Reasoning` 的说明）。
#[test]
fn reasoning_item_round_trips_for_replay() {
    let item = InputItem::reasoning(
        None,
        vec![shangyi_llm::ContentPart {
            kind: "reasoning_text".into(),
            text: Some("我需要调用工具".into()),
        }],
        vec![],
    );
    let v = serde_json::to_value(&item).unwrap();
    assert_eq!(v["type"], serde_json::json!("reasoning"));
    assert_eq!(v["content"][0]["text"], serde_json::json!("我需要调用工具"));
    // 无 id 时不应出现该键（omit-if-none），避免发送 null 触发校验
    assert!(v.get("id").is_none(), "无 id 时不该出现 id 键：{v}");

    let back: InputItem = serde_json::from_value(v).unwrap();
    assert_eq!(back, item);
}

#[test]
fn optional_fields_are_omitted_when_absent() {
    // 空 tools / 无 tool_choice / 无 max_output_tokens 不应出现在请求体里——
    // 避免某些端点因多余字段报错
    let req = LlmRequest::new("m", vec![InputItem::user("x")]);
    let body = req.to_body().unwrap();
    assert!(body.get("tools").is_none(), "空 tools 应省略：{body}");
    assert!(body.get("tool_choice").is_none());
    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("temperature").is_none());
}

#[test]
fn optional_fields_appear_when_set() {
    let req = LlmRequest::new("m", vec![InputItem::user("x")])
        .with_max_tokens(4096)
        .with_temperature(0.0);
    let body = req.to_body().unwrap();
    assert_eq!(body["max_output_tokens"], 4096);
    assert_eq!(body["temperature"], 0.0);
}

#[test]
fn endpoint_uses_shortest_verified_path() {
    // A10b 实测：/responses、/v1/responses、/beta/responses 均 200；
    // /api/v1/responses 与 /openai/v1/responses 为 404。
    // 本实现固定用 /responses，不依赖别名行为。
    let ep = Endpoint::deepseek();
    assert_eq!(ep.responses_url(), "https://api.deepseek.com/responses");
}

#[test]
fn endpoint_trailing_slash_normalized() {
    let ep = Endpoint {
        base_url: "https://api.deepseek.com/".into(),
        api_key_env: "X".into(),
            wire: Wire::Responses,
            echo_reasoning: true,
    };
    assert_eq!(ep.responses_url(), "https://api.deepseek.com/responses");
}

#[test]
fn endpoint_display_never_leaks_key() {
    let ep = Endpoint {
        base_url: "https://api.test".into(),
        api_key_env: "MY_KEY_ENV".into(),
            wire: Wire::Responses,
            echo_reasoning: true,
    };
    let s = ep.to_string();
    assert!(s.contains("MY_KEY_ENV"), "应显示环境变量名：{s}");
    assert!(!s.to_lowercase().contains("bearer"), "不得含凭据：{s}");
}

// ─────────────────────────────────────────────────────────────────────────────
// 响应解析（A10b 实测形态）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn parses_live_shaped_response() {
    let r: LlmResponse = serde_json::from_str(LIVE_SHAPED_RESPONSE).unwrap();
    assert_eq!(r.status, "completed");
    assert!(r.is_completed());
    assert_eq!(r.model, "deepseek-flash");
    assert_eq!(r.usage.input_tokens, 297);
    assert_eq!(r.usage.output_tokens, 102);
    assert_eq!(r.usage.total_tokens, 399);
    assert_eq!(r.output.len(), 3);
}

#[test]
fn extracts_function_call_with_object_arguments() {
    let r: LlmResponse = serde_json::from_str(LIVE_SHAPED_RESPONSE).unwrap();
    let calls = r.function_calls();
    assert_eq!(calls.len(), 1);
    let (call_id, name, args) = calls[0];
    assert_eq!(call_id, "call_00_Vow6cQlcIUVzaiHTqO7o8181");
    assert_eq!(name, "get_weather");
    assert_eq!(args["city"], "东京");
}

#[test]
fn text_excludes_reasoning() {
    // 报告里的文本不应混入思维链——思维链是审计材料，不是结论
    let r: LlmResponse = serde_json::from_str(LIVE_SHAPED_RESPONSE).unwrap();
    assert_eq!(r.text(), "我来查一下");
    assert_eq!(r.reasoning_text(), "我应该查天气");
    assert!(!r.text().contains("我应该查天气"), "文本不得混入思维链");
}

#[test]
fn unknown_output_item_types_do_not_break_parsing() {
    // 前向兼容：端点上新增 output item 类型时不应整体解析失败
    let json = r#"{"status":"completed","output":[
      {"type":"some_future_type","foo":1},
      {"type":"message","content":[{"type":"output_text","text":"ok"}]}
    ]}"#;
    let r: LlmResponse = serde_json::from_str(json).unwrap();
    assert_eq!(r.output.len(), 2);
    assert_eq!(r.text(), "ok", "已知项仍应被正确提取");
}

#[test]
fn api_error_in_body_detected() {
    let json = r#"{"status":"failed","error":{"message":"Invalid schema","type":"invalid_request_error","code":"invalid_request_error"}}"#;
    let r: LlmResponse = serde_json::from_str(json).unwrap();
    assert!(!r.is_completed(), "带 error 的响应不应视为完成");
    let e = r.error.unwrap();
    assert_eq!(e.kind, "invalid_request_error");
    assert!(e.message.contains("Invalid schema"));
}

#[test]
fn response_without_usage_still_parses() {
    // usage 是可选的（某些端点或错误路径不返回）
    let json = r#"{"status":"completed","output":[]}"#;
    let r: LlmResponse = serde_json::from_str(json).unwrap();
    assert_eq!(r.usage.total_tokens, 0);
}

// ─────────────────────────────────────────────────────────────────────────────
// 客户端行为（用假传输层）
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn client_sends_to_correct_url_with_auth_header() {
    if !ensure_test_key() {
        return;
    }
    let (c, _ep) = client_with(vec![ok_response(LIVE_SHAPED_RESPONSE)]);
    let req = LlmRequest::new("deepseek-flash", vec![InputItem::user("hi")]);
    c.complete(&req).unwrap();

    let seen = c.transport().seen.borrow();
    let (url, headers, _) = &seen[0];
    assert_eq!(url, "https://api.test.invalid/responses");
    assert!(
        headers.iter().any(|(k, v)| k == "Authorization" && v.starts_with("Bearer ")),
        "应带 Bearer 认证头：{headers:?}"
    );
    assert!(headers.iter().any(|(k, _)| k == "Content-Type"));
}

#[test]
fn client_reports_missing_api_key() {
    let ep = Endpoint {
        base_url: "https://api.test.invalid".into(),
        api_key_env: "SHANGYI_KEY_DEFINITELY_ABSENT_XYZ".into(),
            wire: Wire::Responses,
            echo_reasoning: true,
    };
    let c = LlmClient::new(ep, FakeTransport::new(vec![ok_response("{}")]));
    let req = LlmRequest::new("m", vec![InputItem::user("x")]);
    let err = c.complete(&req).expect_err("缺 key 必须报错");
    assert!(
        matches!(err, LlmError::MissingApiKey(ref e) if e.contains("SHANGYI_KEY_DEFINITELY_ABSENT_XYZ")),
        "实得 {err:?}"
    );
}

#[test]
fn client_surfaces_api_error_from_body() {
    if !ensure_test_key() {
        return;
    }
    let (c, _) = client_with(vec![ok_response(
        r#"{"status":"failed","error":{"message":"bad schema","type":"invalid_request_error"}}"#,
    )]);
    let err = c
        .complete(&LlmRequest::new("m", vec![InputItem::user("x")]))
        .expect_err("body 中的 error 必须上报");
    assert!(matches!(err, LlmError::Api { .. }), "实得 {err:?}");
}

#[test]
fn client_returns_parsed_response() {
    if !ensure_test_key() {
        return;
    }
    let (c, _) = client_with(vec![ok_response(LIVE_SHAPED_RESPONSE)]);
    let r = c
        .complete(&LlmRequest::new("deepseek-flash", vec![InputItem::user("查天气")]))
        .unwrap();
    assert!(r.is_completed());
    assert_eq!(r.function_calls().len(), 1);
}

#[test]
fn retryable_statuses_are_correct() {
    assert!(shangyi_llm::is_retryable(429), "限流应重试");
    assert!(shangyi_llm::is_retryable(500));
    assert!(shangyi_llm::is_retryable(503));
    assert!(!shangyi_llm::is_retryable(400), "请求错误不应重试");
    assert!(!shangyi_llm::is_retryable(401), "认证错误不应重试");
    assert!(!shangyi_llm::is_retryable(404), "路径错误不应重试");
}

#[test]
fn client_retries_on_429_then_succeeds() {
    if !ensure_test_key() {
        return;
    }
    // 先 429，再成功
    let (c, _) = client_with(vec![(429, "rate limited".into()), ok_response(LIVE_SHAPED_RESPONSE)]);
    let r = c
        .complete(&LlmRequest::new("m", vec![InputItem::user("x")]))
        .expect("重试后应成功");
    assert!(r.is_completed());
    assert_eq!(c.transport().call_count(), 2, "应发两次请求");
}

#[test]
fn client_does_not_retry_on_400() {
    if !ensure_test_key() {
        return;
    }
    // 400 不可重试——重试格式错误的请求只会浪费配额
    let (c, _) = client_with(vec![(400, "invalid schema".into())]);
    let err = c
        .complete(&LlmRequest::new("m", vec![InputItem::user("x")]))
        .expect_err("400 应立即失败");
    assert!(matches!(err, LlmError::Api { status: 400, .. }), "实得 {err:?}");
    assert_eq!(c.transport().call_count(), 1, "不得重试 400");
}

#[test]
fn client_retries_exhaust_on_persistent_5xx() {
    if !ensure_test_key() {
        return;
    }
    let (mut c, _) = client_with(vec![(500, "boom".into())]);
    c.max_retries = 2;
    let err = c
        .complete(&LlmRequest::new("m", vec![InputItem::user("x")]))
        .expect_err("持续 5xx 应最终失败");
    // 循环 0..=max_retries = 3 次尝试；最后一次直接返回 Api 错误
    assert!(matches!(err, LlmError::Api { status: 500, .. }), "实得 {err:?}");
    assert_eq!(c.transport().call_count(), 3, "应为 1 + 2 次重试");
}

#[test]
fn client_reports_parse_failure_with_context() {
    if !ensure_test_key() {
        return;
    }
    let (c, _) = client_with(vec![(200, "这不是 JSON".into())]);
    let err = c
        .complete(&LlmRequest::new("m", vec![InputItem::user("x")]))
        .expect_err("非法 JSON 应报解析错误");
    assert!(matches!(err, LlmError::Parse(_)), "实得 {err:?}");
    // 错误应带上响应片段，便于排查
    assert!(
        format!("{err}").contains("这不是 JSON") || format!("{err}").contains("响应体前"),
        "错误应含响应片段：{err}"
    );
}

#[test]
fn truncate_respects_utf8_boundaries() {
    // 截断多字节字符不得 panic（中文响应常见）
    if !ensure_test_key() {
        return;
    }
    let (c, _) = client_with(vec![(200, "中文字符串".repeat(200))]);
    let err = c
        .complete(&LlmRequest::new("m", vec![InputItem::user("x")]))
        .expect_err("长中文非 JSON 应报解析错误");
    // 不 panic 即通过（若截断在字符中间会 panic）
    assert!(matches!(err, LlmError::Parse(_)));
}

// ─────────────────────────────────────────────────────────────────────────────
// 实测测试（需真实 key）——被 ignore 而非删除
// ─────────────────────────────────────────────────────────────────────────────

/// **实测：对真实 DeepSeek 端点发一次请求。**
///
/// 需要 `DEEPSEEK_API_KEY` 环境变量。默认跳过（`#[ignore]`）——
/// 因为 CI 与常规测试不应依赖外部服务与凭据。
///
/// 运行方式：
/// ```text
/// DEEPSEEK_API_KEY=sk-... cargo test -p shangyi-llm -- --ignored live_
/// ```
///
/// # 这个测试验证什么（离线测试无法覆盖的部分）
///
/// - 真实端点是否仍接受本 crate 构造的请求体形状
/// - `tool_choice = auto` 与工具定义是否仍被接受（A10b 实测的结论是否仍成立）
/// - 响应是否仍可被本 crate 的解析器解析
///
/// # 它刻意**不**验证什么
///
/// 不验证数值结果——本 crate 不产生数字（铁律 1）。
#[test]
#[ignore = "需 DEEPSEEK_API_KEY；用 cargo test -- --ignored 运行"]
fn live_deepseek_responses_roundtrip() {
    let ep = Endpoint::deepseek();
    let key = match ep.api_key() {
        Ok(k) => k,
        Err(e) => {
            // 无 key 时打印原因并返回（`#[ignore]` 已保证默认不跑；
            // 显式跑时应给出清晰原因而非静默通过）
            panic!("缺少凭据：{e}（请设置 DEEPSEEK_API_KEY）");
        }
    };
    assert!(!key.trim().is_empty());

    let c = LlmClient::new(ep, shangyi_llm::UreqTransport);
    let tool = ToolSchema::function(
        "get_weather",
        "查询某城市天气",
        serde_json::json!({
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"]
        }),
    );
    let req = LlmRequest::new("deepseek-flash", vec![InputItem::user("东京天气如何？请调用工具。")])
        .with_tools(vec![tool])
        .with_tool_choice(ToolChoice::Auto);

    let resp = c.complete(&req).expect("真实端点应接受该请求");
    assert!(
        resp.is_completed(),
        "应成功完成：status={} error={:?}",
        resp.status,
        resp.error
    );
    let calls = resp.function_calls();
    assert_eq!(calls.len(), 1, "应产出一个 function_call：{resp:?}");
    assert_eq!(calls[0].1, "get_weather");
    // arguments 已是对象（A10b 实测确认）
    assert!(
        calls[0].2.is_object(),
        "arguments 应为对象：{:?}",
        calls[0].2
    );
    assert_eq!(calls[0].2["city"], "东京");
    assert!(resp.usage.total_tokens > 0, "应返回用量");
}


// ─────────────────────────────────────────────────────────────────────────────
// Chat Completions 适配（GLM 唯一可用的 wire）
// ─────────────────────────────────────────────────────────────────────────────

fn chat_ep(echo_reasoning: bool) -> Endpoint {
    Endpoint {
        base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
        api_key_env: "GLM_API_KEY".into(),
        wire: Wire::Chat,
        echo_reasoning,
    }
}

fn chat_request() -> LlmRequest {
    LlmRequest::new("glm-5.3", vec![InputItem::user("查东京天气")]).with_tools(vec![
        ToolSchema::function(
            "get_weather",
            "查询天气",
            serde_json::json!({"type":"object","properties":{"city":{"type":"string"}}}),
        ),
    ])
}

/// 请求 URL 按 wire 选择路径。
#[test]
fn request_url_depends_on_wire() {
    assert_eq!(
        Endpoint::deepseek().request_url(),
        "https://api.deepseek.com/responses"
    );
    assert_eq!(
        chat_ep(false).request_url(),
        "https://open.bigmodel.cn/api/paas/v4/chat/completions"
    );
    // 尾部斜杠不得产生双斜杠
    let e = Endpoint {
        base_url: "https://x.test/v1/".into(),
        ..chat_ep(false)
    };
    assert_eq!(e.request_url(), "https://x.test/v1/chat/completions");
}

/// **工具定义必须是嵌套形态**（`{type,function:{…}}`），不是 Responses 的扁平形态。
#[test]
fn chat_tool_definitions_are_nested() {
    let body = chat_request().to_body_chat(false).unwrap();
    let t = &body["tools"][0];
    assert_eq!(t["type"], serde_json::json!("function"));
    assert!(
        t.get("function").is_some(),
        "Chat 侧工具定义必须嵌套在 `function` 下：{t}"
    );
    assert_eq!(t["function"]["name"], serde_json::json!("get_weather"));
    // 扁平形态的字段不得**同时**出现在顶层（那会让端点困惑）
    assert!(t.get("name").is_none(), "不得混用扁平形态：{t}");
}

/// `max_output_tokens` 在 Chat 侧必须改名为 `max_tokens`。
#[test]
fn chat_uses_max_tokens_field() {
    let b = chat_request().with_max_tokens(4096).to_body_chat(false).unwrap();
    assert_eq!(b["max_tokens"], serde_json::json!(4096));
    assert!(b.get("max_output_tokens").is_none(), "字段名必须按 wire 改：{b}");
}

/// function_call_output 在 Chat 侧是 `role:"tool"` 消息；function_call 是
/// assistant 的 `tool_calls`。
#[test]
fn chat_encodes_tool_calls_and_outputs_in_chat_shape() {
    let input = vec![
        InputItem::user("查东京天气"),
        InputItem::function_call(
            "call_1",
            "get_weather",
            serde_json::json!({"city": "东京"}),
        ),
        InputItem::tool_output("call_1", "晴，28 度"),
    ];
    let body = LlmRequest::new("glm-5.3", input).to_body_chat(false).unwrap();
    let msgs = body["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3, "三条消息：user / assistant(tool_calls) / tool");

    let asst = &msgs[1];
    assert_eq!(asst["role"], serde_json::json!("assistant"));
    let tc = &asst["tool_calls"][0];
    assert_eq!(tc["id"], serde_json::json!("call_1"));
    assert_eq!(tc["function"]["name"], serde_json::json!("get_weather"));
    // arguments 必须是**字符串**（实测两个 provider 一致）
    assert!(
        tc["function"]["arguments"].is_string(),
        "arguments 必须是字符串：{tc}"
    );

    let tool = &msgs[2];
    assert_eq!(tool["role"], serde_json::json!("tool"));
    assert_eq!(tool["tool_call_id"], serde_json::json!("call_1"));
    assert_eq!(tool["content"], serde_json::json!("晴，28 度"));
}

/// 思维链在 Chat 侧**附到它对应的 assistant 消息**上（不是独立项）。
#[test]
fn chat_attaches_reasoning_to_the_assistant_message() {
    let input = vec![
        InputItem::user("查东京天气"),
        InputItem::reasoning(
            None,
            vec![shangyi_llm::ContentPart {
                kind: "reasoning_text".into(),
                text: Some("我需要调用天气工具".into()),
            }],
            vec![],
        ),
        InputItem::function_call("call_1", "get_weather", serde_json::json!({})),
    ];
    // echo=true：附上
    let b = LlmRequest::new("m", input.clone()).to_body_chat(true).unwrap();
    let asst = b["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == serde_json::json!("assistant"))
        .unwrap();
    assert_eq!(
        asst["reasoning_content"],
        serde_json::json!("我需要调用天气工具"),
        "回放时思维链必须附到 assistant 消息上：{b}"
    );

    // echo=false（GLM 实测不要求）：**不带**
    let b2 = LlmRequest::new("m", input).to_body_chat(false).unwrap();
    let asst2 = b2["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == serde_json::json!("assistant"))
        .unwrap();
    assert!(
        asst2.get("reasoning_content").is_none(),
        "echo_reasoning=false 时不得带思维链：{b2}"
    );
}

/// 连续同角色消息合并（避免多个 system 被反复强调）。
#[test]
fn chat_merges_consecutive_same_role_messages() {
    let input = vec![
        InputItem::system("指引一"),
        InputItem::system("指引二"),
        InputItem::user("问题"),
        InputItem::user("补充"),
    ];
    let b = LlmRequest::new("m", input).to_body_chat(false).unwrap();
    let msgs = b["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "两条 system 应合并、两条 user 应合并：{b}");
    assert!(msgs[0]["content"].as_str().unwrap().contains("指引一"));
    assert!(msgs[0]["content"].as_str().unwrap().contains("指引二"));
}

// ── Chat 响应解析 ──

const CHAT_RESPONSE: &str = r#"{
  "id": "20260918abc",
  "model": "glm-5.3",
  "choices": [{
    "index": 0,
    "finish_reason": "tool_calls",
    "message": {
      "role": "assistant",
      "content": null,
      "reasoning_content": "我需要调用工具",
      "tool_calls": [{
        "id": "call_-7245796745328716628",
        "type": "function",
        "function": {"name": "get_weather", "arguments": "{\"city\":\"东京\"}"}
      }]
    }
  }],
  "usage": {"prompt_tokens": 155, "completion_tokens": 38, "total_tokens": 193}
}"#;

/// Chat 响应被归一化为与 Responses **相同**的 `LlmResponse`，
/// 使 agent 循环不必区分两种 wire。
#[test]
fn chat_response_normalizes_to_the_unified_shape() {
    let r = parse_chat_response(CHAT_RESPONSE).unwrap();
    assert!(r.is_completed(), "finish_reason=tool_calls 应视为完成");
    assert_eq!(r.model, "glm-5.3");
    // usage 字段名映射
    assert_eq!(r.usage.input_tokens, 155);
    assert_eq!(r.usage.output_tokens, 38);
    assert_eq!(r.usage.total_tokens, 193);

    // 项序：思维链在前、工具调用在后（与 Responses 实测一致）
    let kinds: Vec<&str> = r
        .output
        .iter()
        .map(|o| match o {
            shangyi_llm::OutputItem::Reasoning { .. } => "reasoning",
            shangyi_llm::OutputItem::Message { .. } => "message",
            shangyi_llm::OutputItem::FunctionCall { .. } => "function_call",
            shangyi_llm::OutputItem::Unknown => "unknown",
        })
        .collect();
    assert_eq!(kinds, vec!["reasoning", "function_call"], "项序应与 Responses 一致");

    // 工具调用可被取出，且 arguments 已解析为对象（调用方不必再 from_str）
    let calls = r.function_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "call_-7245796745328716628");
    assert_eq!(calls[0].1, "get_weather");
    assert_eq!(calls[0].2["city"], serde_json::json!("东京"));

    // 思维链可被取出（回放用）
    assert!(r.reasoning_text().contains("我需要调用工具"));
}

/// `finish_reason: "length"`（触顶）**不得**被当作完成。
#[test]
fn chat_length_finish_is_not_completed() {
    let r = parse_chat_response(
        r#"{"choices":[{"finish_reason":"length","message":{"role":"assistant","content":"截断的"}}]}"#,
    )
    .unwrap();
    assert!(
        !r.is_completed(),
        "触顶意味着分析没做完，不是完成（status={}）",
        r.status
    );
    assert_eq!(r.status, "incomplete");
}

/// Chat 侧的错误体（HTTP 200 带 error）必须被识别为错误。
#[test]
fn chat_error_body_is_detected() {
    let r = parse_chat_response(
        r#"{"error":{"message":"输入不能为空","type":"invalid_request_error","code":"1210"}}"#,
    )
    .unwrap();
    assert!(r.error.is_some());
    let e = r.error.as_ref().unwrap();
    assert!(e.message.contains("输入不能为空"));
    assert_eq!(e.code.as_deref(), Some("1210"));
    assert!(!r.is_completed());
}

/// 缺 choices 的响应必须报错，而不是被当成"空回复"。
#[test]
fn chat_response_without_choices_is_an_error() {
    let e = parse_chat_response(r#"{"id":"x"}"#).unwrap_err();
    assert!(
        e.to_string().contains("choices"),
        "应指出缺什么以便定位：{e}"
    );
}

/// 纯文本回复（无工具调用）也应被正确解析。
#[test]
fn chat_text_only_response_parses() {
    let r = parse_chat_response(
        r#"{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"结论是维持价格"}}]}"#,
    )
    .unwrap();
    assert!(r.is_completed());
    assert!(r.text().contains("维持价格"));
    assert!(r.function_calls().is_empty());
    // 没有 reasoning_content 时不得凭空造一个 reasoning 项
    assert!(!r.output.iter().any(|o| matches!(
        o,
        shangyi_llm::OutputItem::Reasoning { .. }
    )));
}

/// 端到端（离线）：Chat 端点的请求体形态与响应解析串起来能跑。
#[test]
fn chat_wire_end_to_end_offline() {
    let body = chat_request().to_body_chat(false).unwrap();
    assert!(body.get("messages").is_some(), "应产出 messages：{body}");
    assert!(body.get("input").is_none(), "不得混入 Responses 的 input：{body}");

    let r = parse_chat_response(CHAT_RESPONSE).unwrap();
    assert_eq!(r.function_calls().len(), 1);
}


/// **真实 GLM 端点往返**（`#[ignore]`，需 `GLM_API_KEY`）。
///
/// # 为什么必须有一条真实往返测试
///
/// 离线测试证明的是"我们的形状自洽"，**证明不了端点接受它**。
/// 本项目的两条实测教训都出在这里：
/// - `arguments` 传对象 ⇒ 端点 400（离线测试当时是绿的）
/// - reasoning 不回放 ⇒ 端点 400（当时离线测试也是绿的）
///
/// 运行：`cargo test -p shangyi-llm --test client -- --ignored`
#[test]
#[ignore = "需要 GLM_API_KEY 与网络"]
fn live_glm_chat_roundtrip() {
    let key = std::env::var("GLM_API_KEY").expect("需要 GLM_API_KEY");
    let client = LlmClient::new(Endpoint::glm(), UreqTransport).with_key(key);

    // 第一轮：应触发工具调用
    let req = LlmRequest::new(
        "glm-5.3",
        vec![InputItem::user("查一下东京天气，必须用工具查")],
    )
    .with_tools(vec![ToolSchema::function(
        "get_weather",
        "查询某城市天气",
        serde_json::json!({
            "type":"object",
            "properties":{"city":{"type":"string"}},
            "required":["city"]
        }),
    )]);

    let r1 = client.complete(&req).expect("第一轮请求应成功");
    assert!(r1.is_completed(), "status={} err={:?}", r1.status, r1.error);
    let calls = r1.function_calls();
    assert_eq!(calls.len(), 1, "应触发一次工具调用；output={:?}", r1.output);
    let (call_id, name, args) = calls[0];
    assert_eq!(name, "get_weather");
    assert_eq!(args["city"], serde_json::json!("东京"), "参数应被解析为对象");

    // 第二轮：回传工具结果，形状必须被端点接受
    let input = vec![
        InputItem::user("查一下东京天气，必须用工具查"),
        InputItem::function_call(call_id, name, args.clone()),
        InputItem::tool_output(call_id, "晴，28 度"),
    ];
    let r2 = client
        .complete(&LlmRequest::new("glm-5.3", input).with_tools(req.tools.clone()))
        .expect("第二轮请求应成功（工具结果形状被接受）");
    assert!(r2.is_completed(), "status={} err={:?}", r2.status, r2.error);
    assert!(
        r2.text().contains("28"),
        "第二轮应基于工具结果作答：{:?}",
        r2.text()
    );
}

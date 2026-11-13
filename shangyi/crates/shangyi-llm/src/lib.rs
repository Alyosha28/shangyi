//! 商奕 LLM 接入层（设计文档 §3.3、Harness §10）。
//!
//! # 本 crate 的定位：只做传输与序列化
//!
//! 它构造请求、解析响应、处理重试——**不做任何数值计算**。
//! 所有进入报告的数字来自确定性引擎（`shangyi-solver` 等）。
//! LLM 的输出永远是"提议"或"文本"，不是报告里的数字（铁律 1）。
//!
//! # 为何自建而非复用 vendor 的 `codex-api`
//!
//! Codex 基座只支持 Responses API，而商奕的目标端点（DeepSeek/GLM）实测支持
//! Responses，理论上可复用。但 `codex-api` 把请求构造与 `reqwest` 传输**耦合**，
//! 难以注入假传输层做离线测试。自建一个薄客户端使"请求形状是否正确"
//! 可以在无网络下被单元测试锁定——**这是本 crate 存在的主要理由**。
//!
//! # A10b 实测确认的三条约束（编码在设计中）
//!
//! 1. **`tool_choice` 仅支持 `auto` / `none`**：`required` 与指定函数**恒 400**
//!    （`"Thinking mode does not support this tool_choice"`）。
//!    故 [`ToolChoice`] 类型**只定义这两个变体**——把非法的第三个值
//!    变成编译期不可能。
//! 2. **省略 `reasoning` 回传不会 400**（A10 的相反断言已被实测推翻）。
//! 3. **模型名用 `/models` 实际返回的**（`deepseek-flash` / `deepseek-v4-pro`），
//!    而非别名 `deepseek-chat`。

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LlmError {
    #[error("HTTP 请求失败：{0}")]
    Transport(String),
    #[error("API 返回错误 {status}：{body}")]
    Api { status: u16, body: String },
    #[error("响应解析失败：{0}")]
    Parse(String),
    #[error("端点不支持 `{0}`——A10b 实测：tool_choice 仅支持 auto 与 none")]
    UnsupportedToolChoice(String),
    #[error("缺少 API key（环境变量 `{0}` 未设置）")]
    MissingApiKey(String),
    #[error("重试次数耗尽（{0} 次）：{1}")]
    RetriesExhausted(u32, String),
    #[error("流式响应的结束标记缺失（收到 {0} 个字节后中断）")]
    StreamTruncated(usize),
}

/// 工具选择方式。
///
/// ⚠️ **只有两个变体是刻意的**：A10b 实测确认 DeepSeek 的 Responses 端点上
/// `required` 与"指定函数"**恒返回 400**（`Thinking mode does not support
/// this tool_choice`）。把非法取值变成**编译期不可能**，比运行时报错更可靠——
/// 后者会在凌晨的批处理里才暴露。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    /// 由模型自行决定是否调用工具。
    Auto,
    /// 不允许调用工具。
    None,
}

impl ToolChoice {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolChoice::Auto => "auto",
            ToolChoice::None => "none",
        }
    }

    /// 从字符串解析——**拒绝** `required` 与函数指定形式，并给出可读原因。
    pub fn parse(s: &str) -> Result<Self, LlmError> {
        match s {
            "auto" => Ok(ToolChoice::Auto),
            "none" => Ok(ToolChoice::None),
            other => Err(LlmError::UnsupportedToolChoice(other.to_string())),
        }
    }
}

/// 一个工具的函数签名（Responses API 的扁平 function 格式）。
///
/// ⚠️ 注意形态：Responses 用**扁平**的 `{type, name, parameters}`，
/// 而非 Chat Completions 的嵌套 `{type, function: {name, parameters}}`。
/// A10b 实测确认 DeepSeek 的 Responses 端点接受扁平形式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSchema {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub description: String,
    /// JSON Schema。A10b 实测支持 `anyOf`、嵌套 `object`、`enum`、
    /// `required`、`additionalProperties: false`——**无需拍平**。
    pub parameters: serde_json::Value,
}

impl ToolSchema {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".to_string(),
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }
}

/// 把参数序列化为 JSON **字符串**（请求侧实测要求）。
fn serialize_args_as_string<S: serde::Serializer>(
    v: &serde_json::Value,
    s: S,
) -> Result<S::Ok, S::Error> {
    // 已是字符串就原样传（避免双重编码）
    if let serde_json::Value::String(inner) = v {
        return s.serialize_str(inner);
    }
    s.serialize_str(&v.to_string())
}

/// 接受两种形态（宽容解析）。
///
/// # 内层 JSON 非法时**必须原样返回字符串**，绝不硬失败
///
/// 早先的写法是 `serde_json::from_str(&s).map_err(...)`——内层解析失败即
/// 让**整条响应**反序列化失败。后果（实测遇到）：
///
/// - 输出被上限截断时（`high` 思考档很容易发生），`arguments` 停在半句
/// - 于是外层 `serde_json::from_str::<LlmResponse>` 失败
/// - 错误信息写成「响应解析失败：expected `:` at line 1 column 432」
/// - **而真因是"参数被截断"**，与"响应格式"毫无关系
///
/// 这个诊断指向完全错误的方向：排查者会去查传输层与序列化，而该做的是
/// 看输出预算。改成原样返回字符串后，坏参数会流到工具那里，
/// 由工具报「参数非法」——**那是可操作的**。
fn deserialize_args_from_either<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<serde_json::Value, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    match v {
        // 能解析就解析（正常路径）；不能就原样保留（由工具报参数非法）
        serde_json::Value::String(s) => {
            Ok(serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s)))
        }
        other => Ok(other),
    }
}

/// 输入项（Responses API 的 `input` 数组元素形态）。
///
/// 与 A10b 实测到的响应形态对应：`message` / `function_call` / `function_call_output`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputItem {
    /// 一条消息（用户或助手）。
    Message {
        role: String,
        content: String,
    },
    /// 模型发起的函数调用。
    FunctionCall {
        /// 调用 id（实测格式 `call_00_...`）。
        call_id: String,
        name: String,
        /// ⚠️ **序列化为 JSON 字符串**，但内部持 `Value`。
        ///
        /// # 两处实测结论（2026-09-18，与 A10b 记录相反）
        ///
        /// | 方向 | 形态 | 证据 |
        /// |---|---|---|
        /// | 响应里的 `arguments` | **字符串** `"{\"city\": \"东京\"}"` | 实测响应原文 |
        /// | 请求里的 `arguments` | **必须字符串** | 传对象 ⇒ `400 invalid type: map, expected a string` |
        ///
        /// A10b 记录的是"arguments 是对象（已解析）"。**该结论已不成立**：
        /// 现在两个方向都是字符串。内部仍持 `Value`（便于工具直接取字段），
        /// 仅在序列化时转字符串——把形态差异挡在这一层。
        #[serde(serialize_with = "serialize_args_as_string", deserialize_with = "deserialize_args_from_either")]
        arguments: serde_json::Value,
    },
    /// 函数调用结果（回传用）。
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    /// 思维链回放。
    ///
    /// # 为什么必须回放（实测，推翻 A10b 的相反结论）
    ///
    /// A10b 记录"省略 reasoning 回传不会 400"。**2026-09-18 实测已推翻**：
    /// 只要上一轮响应含 `reasoning` 项（带工具调用时必有），下一轮回传的
    /// `input` 里就必须带上它，否则：
    ///
    /// ```text
    /// HTTP 400 The `reasoning_text` in the thinking mode must be passed back to the API.
    /// ```
    ///
    /// 该错误**具有误导性**：它出现在与 reasoning 无关的字段改动之后（例如
    /// `arguments` 形态变一次、或去重几条 system 消息），让人以为是自己刚改的那处
    /// 引起的。实测三种改法（去掉重复 system / 只回 function_call / 改写 name）
    /// 全部报同一条错——排除法之后才定位到 reasoning。
    ///
    /// 故：**收到什么就回放什么**，且必须**保序**（reasoning 在它对应的
    /// function_call 之前）。
    Reasoning {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default)]
        content: Vec<ContentPart>,
        #[serde(default)]
        summary: Vec<ContentPart>,
    },
}

impl InputItem {
    pub fn user(content: impl Into<String>) -> Self {
        InputItem::Message {
            role: "user".into(),
            content: content.into(),
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        InputItem::Message {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn tool_output(call_id: impl Into<String>, output: impl Into<String>) -> Self {
        InputItem::FunctionCallOutput {
            call_id: call_id.into(),
            output: output.into(),
        }
    }

    /// 把上一轮的思维链**回放进输入历史**（实测必需，见 [`InputItem::Reasoning`]）。
    pub fn reasoning(
        id: Option<String>,
        content: Vec<ContentPart>,
        summary: Vec<ContentPart>,
    ) -> Self {
        InputItem::Reasoning {
            id,
            content,
            summary,
        }
    }

    /// 把模型发起的函数调用**回放进输入历史**。
    ///
    /// # 为什么回放是必需的（实测）
    ///
    /// Responses API 在无状态调用下要求 `function_call` 与它的
    /// `function_call_output` **成对出现在 `input` 里**。只给输出会得到：
    ///
    /// ```text
    /// HTTP 400 No tool call found for tool output with call_id call_00_...
    /// ```
    ///
    /// 这与 Chat Completions 不同（那边只需回 `role: "tool"` 的消息），
    /// 故它是一处**容易漏掉且离线测不出来**的差异：假传输层不校验配对。
    pub fn function_call(
        call_id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        InputItem::FunctionCall {
            call_id: call_id.into(),
            name: name.into(),
            arguments,
        }
    }
}

/// 一次请求。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub model: String,
    pub input: Vec<InputItem>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// 思考量控制（Responses 侧字段 `reasoning: {effort: ...}`）。
    ///
    /// # 为什么它直接影响成本（实测）
    ///
    /// DeepSeek 上带工具调用时实测：
    ///
    /// | effort | 输出 tokens | 思考 tokens | 工具调用 |
    /// |---|---|---|---|
    /// | 默认（未设） | 58 | 19（32%） | ✓ |
    /// | `none` | **38** | **0（0%）** | **✓** |
    /// | `low` | 51 | 12（23%） | ✓ |
    ///
    /// 即 **`none` 把思考归零、输出降约 1/3，而工具调用照常工作**。
    /// 价格按输出 token 计，故这是直接的降本手段。
    ///
    /// # 但默认**不设**它
    ///
    /// 思考对**分析类**任务有实际价值（商奕的场景是"对手会怎么反应"，
    /// 不是查天气）。关闭思考换来的是**质量**的潜在损失，而那是产品判断，
    /// 不是工程默认值该替用户决定的事。故：默认保持供应商行为，
    /// 由 `SHANGYI_REASONING_EFFORT` 显式开启降本。
    #[serde(default, skip_serializing_if = "Option::is_none", skip_serializing)]
    pub reasoning_effort: Option<String>,
}

impl LlmRequest {
    pub fn new(model: impl Into<String>, input: Vec<InputItem>) -> Self {
        Self {
            model: model.into(),
            input,
            tools: Vec::new(),
            tool_choice: None,
            max_output_tokens: None,
            temperature: None,
            reasoning_effort: None,
        }
    }

    /// 设置思考量（`none` / `low` / `medium` / `high` / `max`）。
    ///
    /// **传什么由供应商决定**：不同实现接受的取值不同，商奕不在此校验
    /// （校验会把它写死成一个子集），而是**原样透传**并在自检里验证是否被接受。
    pub fn with_reasoning_effort(mut self, e: impl Into<String>) -> Self {
        self.reasoning_effort = Some(e.into());
        self
    }

    pub fn with_tools(mut self, tools: Vec<ToolSchema>) -> Self {
        self.tools = tools;
        if !self.tools.is_empty() && self.tool_choice.is_none() {
            // 有工具时的默认值是 `auto`——**绝不用 `required`**（实测恒 400）
            self.tool_choice = Some(ToolChoice::Auto);
        }
        self
    }

    pub fn with_tool_choice(mut self, tc: ToolChoice) -> Self {
        self.tool_choice = Some(tc);
        self
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_output_tokens = Some(n);
        self
    }

    pub fn with_temperature(mut self, t: f64) -> Self {
        self.temperature = Some(t);
        self
    }

    /// 序列化为指定 wire 的请求体。
    ///
    /// `echo_reasoning` 决定是否把思维链带回下一轮（见 [`Endpoint::echo_reasoning`]）。
    pub fn to_body_for(
        &self,
        wire: Wire,
        echo_reasoning: bool,
    ) -> Result<serde_json::Value, LlmError> {
        match wire {
            Wire::Responses => self.to_body(),
            Wire::Chat => self.to_body_chat(echo_reasoning),
        }
    }

    /// 序列化为 Chat Completions 请求体。
    ///
    /// # 与 Responses 的四处形态差异（逐条对应实测）
    ///
    /// 1. `input` 数组 → `messages` 数组，项由 `type` 判别改为 `role` 判别
    /// 2. 工具结果：`function_call_output` 项 → `{"role":"tool","tool_call_id":…}`
    /// 3. 工具定义：扁平 → 嵌套 `{"type":"function","function":{…}}`
    /// 4. `max_output_tokens` → `max_tokens`
    ///
    /// # 思维链的处理（`echo_reasoning`）
    ///
    /// Chat 里思维链是 **assistant 消息的一个字段**（`reasoning_content`），
    /// 不是独立项。故回放时它必须**附到它对应的那条 assistant 消息上**——
    /// 而不是像 Responses 那样插一个独立项。这正是"两种 wire 不能只换字段名"
    /// 的具体例子：同一份中立历史，编码到两种 wire 的结构不同。
    pub fn to_body_chat(&self, echo_reasoning: bool) -> Result<serde_json::Value, LlmError> {
        let mut messages: Vec<serde_json::Value> = Vec::new();
        // 暂存待回放的思维链文本（附到紧随其后的 assistant 消息上）
        let mut pending_reasoning: Option<String> = None;

        // 连续的同角色普通消息合并（Chat 侧对多 system 的容忍度不如 Responses，
        // 且合并能减少"阶段指引被重复强调"的副作用）
        let flush_text = |messages: &mut Vec<serde_json::Value>, role: &str, text: String| {
            if text.trim().is_empty() {
                return;
            }
            if let Some(last) = messages.last_mut() {
                if last["role"] == role && last["tool_calls"].is_null() {
                    let merged = format!(
                        "{}\n{}",
                        last["content"].as_str().unwrap_or_default(),
                        text
                    );
                    last["content"] = serde_json::Value::String(merged);
                    return;
                }
            }
            messages.push(serde_json::json!({"role": role, "content": text}));
        };

        for item in &self.input {
            match item {
                InputItem::Message { role, content } => {
                    flush_text(&mut messages, role, content.clone());
                }
                InputItem::Reasoning { content, .. } => {
                    // 收敛为纯文本；Chat 侧的思维链就是一个字符串字段
                    let text: String = content.iter().filter_map(|p| p.text.as_deref()).collect();
                    if !text.is_empty() {
                        pending_reasoning = Some(text);
                    }
                }
                InputItem::FunctionCall {
                    call_id,
                    name,
                    arguments,
                } => {
                    // 模型发起的调用：作为 assistant 消息的 `tool_calls`
                    let args_str = match arguments {
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    let mut msg = serde_json::json!({
                        "role": "assistant",
                        "content": serde_json::Value::Null,
                        "tool_calls": [{
                            "id": call_id,
                            "type": "function",
                            "function": {"name": name, "arguments": args_str}
                        }]
                    });
                    if echo_reasoning {
                        if let Some(r) = pending_reasoning.take() {
                            msg["reasoning_content"] = serde_json::Value::String(r);
                        }
                    } else {
                        pending_reasoning = None;
                    }
                    messages.push(msg);
                }
                InputItem::FunctionCallOutput { call_id, output } => {
                    messages.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
            }
        }

        let mut v = serde_json::json!({
            "model": self.model,
            "messages": messages,
        });
        if !self.tools.is_empty() {
            // 工具定义：**嵌套**形态（与 Responses 的扁平形态不同）
            v["tools"] = serde_json::Value::Array(
                self.tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
            if let Some(tc) = self.tool_choice {
                v["tool_choice"] = serde_json::Value::String(tc.as_str().to_string());
            }
        }
        if let Some(n) = self.max_output_tokens {
            // Chat 侧的字段名不同
            v["max_tokens"] = serde_json::json!(n);
        }
        if let Some(t) = self.temperature {
            v["temperature"] = serde_json::json!(t);
        }
        // Chat 侧的思考控制字段名与 Responses 不同（实测两家的文档口径一致）：
        // `thinking` 是**请求侧开关**，`reasoning_effort` 是档位。
        // `none` ⇒ 显式关思考（同时给两个字段，兼容两种实现口径）。
        if let Some(e) = &self.reasoning_effort {
            v["reasoning_effort"] = serde_json::json!(e);
            if e == "none" {
                v["thinking"] = serde_json::json!({"type": "disabled"});
            } else {
                v["thinking"] = serde_json::json!({"type": "enabled"});
            }
        }
        Ok(v)
    }

    /// 序列化为 Responses API 的请求体。
    ///
    /// `tool_choice` 用字符串枚举序列化（`"auto"` / `"none"`）。
    pub fn to_body(&self) -> Result<serde_json::Value, LlmError> {
        let mut v = serde_json::to_value(self).map_err(|e| LlmError::Parse(e.to_string()))?;
        // tool_choice 需序列化为裸字符串，而非 {"auto": null}
        if let Some(tc) = self.tool_choice {
            v["tool_choice"] = serde_json::Value::String(tc.as_str().to_string());
        }
        // Responses 的思考量是嵌套对象 `reasoning: {effort: ...}`
        // （实测接受 `reasoning.effort`；`reasoning_effort` 顶层字段不生效）
        if let Some(e) = &self.reasoning_effort {
            v["reasoning"] = serde_json::json!({"effort": e});
        }
        Ok(v)
    }
}

/// 响应中的一个输出项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    /// 文本回答。
    Message {
        #[serde(default)]
        content: Vec<ContentPart>,
    },
    /// 函数调用。
    FunctionCall {
        call_id: String,
        name: String,
        /// 参数。**响应侧实测为字符串**（`"{\"city\": \"东京\"}"`），但此处
        /// 反序列化时**自动解析成对象**（见 `deserialize_args_from_either`）。
        ///
        /// # 为什么在这里解析（而不是让调用方自己 from_str）
        ///
        /// 实测发现：若不在这一层解析，Responses 路径下 `arguments` 会以
        /// **字符串**形态流到调用方，而调用方（agent 循环）按对象取字段
        /// （`args["city"]`）就会失败——表现为"工具每次都收到空参数"。
        ///
        /// Chat 路径的 `parse_chat_response` 已做同样处理（早先它做了、
        /// Responses 没做，这个**不对称**正是问题的来源）。两条 wire 必须在
        /// 这一层收敛到同一形态，否则下游要为每条 wire 写一遍判断。
        #[serde(deserialize_with = "deserialize_args_from_either")]
        arguments: serde_json::Value,
    },
    /// 思维链（实测 `reasoning` 项含 `content`/`summary`/`encrypted_content`）。
    ///
    /// **商奕不需要回传它**（A10b 实测：省略不回传不会 400），
    /// 但保留字段以便日志与审计。
    Reasoning {
        #[serde(default)]
        content: Vec<ContentPart>,
        #[serde(default)]
        summary: Vec<ContentPart>,
    },
    /// 未知项类型（前向兼容：不因新增类型而整体解析失败）。
    #[serde(other)]
    Unknown,
}

impl OutputItem {
    /// 提取函数调用信息。
    pub fn as_function_call(&self) -> Option<(&str, &str, &serde_json::Value)> {
        match self {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => Some((call_id, name, arguments)),
            _ => None,
        }
    }

    /// 提取文本内容（拼接全部 text part）。
    pub fn text(&self) -> String {
        match self {
            OutputItem::Message { content } => content
                .iter()
                .filter_map(|p| p.text.as_deref())
                .collect::<Vec<_>>()
                .join(""),
            OutputItem::Reasoning { content, summary } => {
                let a: String = content.iter().filter_map(|p| p.text.as_deref()).collect();
                let b: String = summary.iter().filter_map(|p| p.text.as_deref()).collect();
                format!("{a}{b}")
            }
            _ => String::new(),
        }
    }
}

/// 内容片段。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

/// 响应用量（字段名与 Codex 内部 `TokenUsage` 语义一致，A10b 实测）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(from = "RawUsage")]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
    /// 输入里**命中上下文缓存**的部分（实测字段 `input_tokens_details.cached_tokens`）。
    ///
    /// # 为什么必须单独记（成本差 50 倍）
    ///
    /// DeepSeek 实测价（本机核实自官方定价页，USD/百万 tokens）：
    /// 缓存命中输入 `$0.003`–`0.006`，未命中输入 `$0.15`–`0.3`，
    /// **输出 `$0.6`–`1.2`**。命中与未命中差 **50 倍**。
    ///
    /// 若只记 `input_tokens`，成本根本算不出来——而商奕的架构**每一步都要重发
    /// 整段对话历史**，命中率是决定成本的头号变量。不记它，"这个模型贵不贵"
    /// 就只能靠外部文档猜。
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// 输出里**思考**的部分（实测字段 `output_tokens_details.reasoning_tokens`）。
    ///
    /// 实测一例：`output_tokens=63` 而其中 `reasoning_tokens=61`——
    /// **思考占了输出的 97%**。这印证了"强制思考模型贵"的机制：
    /// 价格按输出 token 计，而思考 token 也算输出。
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// `Usage` 的线上形态：缓存/思考 token 在**嵌套的 details 对象**里
/// （实测：`input_tokens_details.cached_tokens`、`output_tokens_details.reasoning_tokens`）。
///
/// 用中间结构而不是在 `Usage` 上直接写 `#[serde(flatten)]`：嵌套两层的字段
/// 用 flatten 需要中间 map 类型，反而更绕；显式摊平让 wire 形态与内部形态
/// 的对应关系一眼可见。
#[derive(Deserialize)]
struct RawUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
    #[serde(default)]
    input_tokens_details: Option<RawInputDetails>,
    #[serde(default)]
    output_tokens_details: Option<RawOutputDetails>,
}

#[derive(Deserialize)]
struct RawInputDetails {
    #[serde(default)]
    cached_tokens: u64,
}

#[derive(Deserialize)]
struct RawOutputDetails {
    #[serde(default)]
    reasoning_tokens: u64,
}

impl From<RawUsage> for Usage {
    fn from(r: RawUsage) -> Self {
        Usage {
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            total_tokens: r.total_tokens,
            cached_input_tokens: r.input_tokens_details.map(|d| d.cached_tokens).unwrap_or(0),
            reasoning_tokens: r.output_tokens_details.map(|d| d.reasoning_tokens).unwrap_or(0),
        }
    }
}

impl Usage {
    /// 未命中缓存的输入（= 全额计价的输入）。
    pub fn uncached_input_tokens(&self) -> u64 {
        self.input_tokens.saturating_sub(self.cached_input_tokens)
    }

    /// 缓存命中率（供日志与成本分析；无输入时为 0）。
    ///
    /// 它是**成本的主导变量**而非性能指标：命中率从 0 提到 80% 能把输入成本
    /// 降到约 1/10（按 50 倍价差算）。
    pub fn cache_hit_ratio(&self) -> f64 {
        if self.input_tokens == 0 {
            return 0.0;
        }
        self.cached_input_tokens as f64 / self.input_tokens as f64
    }

    /// 思考 token 占输出的比例——判断"这个模型是否值得用"的直接依据。
    pub fn reasoning_ratio(&self) -> f64 {
        if self.output_tokens == 0 {
            return 0.0;
        }
        self.reasoning_tokens as f64 / self.output_tokens as f64
    }
}

/// 把 Chat Completions 的响应体解析为统一的 [`LlmResponse`]。
///
/// # 为什么要有这一层转换（而不是让调用方认两种响应）
///
/// agent 循环只应该认**一种**响应表示。若让它自己判断"这是 Responses 还是
/// Chat"，那么每个使用点都要重复一次 wire 判断——而漏掉一处就会得到
/// "工具调用被静默丢弃"这类难以定位的故障（表现为模型一直在空转）。
///
/// # 转换要点（与请求侧对称）
///
/// - `choices[0].message.tool_calls` → 若干个 [`OutputItem::FunctionCall`]
/// - `choices[0].message.content` → [`OutputItem::Message`]
/// - `choices[0].message.reasoning_content` → [`OutputItem::Reasoning`]
/// - `finish_reason` → `status`（`tool_calls` → `"completed"`）
/// - `usage.completion_tokens`/`prompt_tokens` → `output_tokens`/`input_tokens`
///
/// **顺序**：思维链在前、工具调用在后——与 Responses 实测的项序一致，
/// 使下游（回放逻辑）不必区分两种 wire。
pub fn parse_chat_response(text: &str) -> Result<LlmResponse, LlmError> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| {
        LlmError::Parse(format!(
            "Chat 响应不是合法 JSON：{e}；响应体前 500 字节：{}",
            truncate(text, 500)
        ))
    })?;

    // Chat 侧的错误体：`{"error": {"message", "type", "code"}}`
    if let Some(err) = v.get("error") {
        let message = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("（无 message）");
        let kind = err.get("type").and_then(|t| t.as_str()).unwrap_or("error");
        return Ok(LlmResponse {
            id: String::new(),
            model: String::new(),
            status: "failed".to_string(),
            output: Vec::new(),
            usage: Usage::default(),
            incomplete_reason: None,
            raw_incomplete: None,
            error: Some(ApiError {
                message: message.to_string(),
                kind: kind.to_string(),
                code: err.get("code").and_then(|c| c.as_str()).map(str::to_string),
            }),
        });
    }

    let choice = v
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| {
            LlmError::Parse(format!(
                "Chat 响应没有 choices[0]；响应体前 500 字节：{}",
                truncate(text, 500)
            ))
        })?;
    let msg = choice.get("message").ok_or_else(|| {
        LlmError::Parse(format!(
            "Chat 响应没有 choices[0].message；响应体前 500 字节：{}",
            truncate(text, 500)
        ))
    })?;

    let mut output: Vec<OutputItem> = Vec::new();

    // 思维链（GLM 与 DeepSeek 都用 `reasoning_content`）——放在最前，与
    // Responses 实测的项序一致
    if let Some(rc) = msg.get("reasoning_content").and_then(|r| r.as_str()) {
        if !rc.is_empty() {
            output.push(OutputItem::Reasoning {
                content: vec![ContentPart {
                    kind: "reasoning_text".into(),
                    text: Some(rc.to_string()),
                }],
                summary: Vec::new(),
            });
        }
    }

    // 文本内容
    if let Some(c) = msg.get("content").and_then(|c| c.as_str()) {
        if !c.is_empty() {
            output.push(OutputItem::Message {
                content: vec![ContentPart {
                    kind: "output_text".into(),
                    text: Some(c.to_string()),
                }],
            });
        }
    }

    // 工具调用
    if let Some(calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
        for call in calls {
            let id = call.get("id").and_then(|x| x.as_str()).unwrap_or_default();
            let f = call.get("function").unwrap_or(&serde_json::Value::Null);
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or_default();
            // Chat 侧的 `arguments` 是**字符串**（实测两个 provider 一致）；
            // 但宽容解析对象形态（某些实现会给已解析的对象）
            let arguments = match f.get("arguments") {
                Some(serde_json::Value::String(s)) => serde_json::from_str(s)
                    .unwrap_or_else(|_| serde_json::Value::String(s.clone())),
                Some(other) => other.clone(),
                None => serde_json::Value::Null,
            };
            output.push(OutputItem::FunctionCall {
                call_id: id.to_string(),
                name: name.to_string(),
                arguments,
            });
        }
    }

    let finish = choice
        .get("finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or("");
    // 归一化状态：下游只看 `is_completed()`，故此处把 Chat 的 finish_reason
    // 映射到 Responses 的 status 语义。`length` 映射为 `incomplete`
    //（触顶 ≠ 完成——分析没做完，只是没报错）。
    let status = match finish {
        "stop" | "tool_calls" | "function_call" => "completed",
        "length" => "incomplete",
        "content_filter" => "failed",
        "" => "completed", // 未给 finish_reason 的实现在此处宽容，但不假装是别的
        _ => "completed",
    };

    let usage = v.get("usage");
    let g = |k: &str| usage.and_then(|u| u.get(k)).and_then(|x| x.as_u64());
    // 嵌套 details（实测两家 provider 同形）：缓存命中与思考 token 都要取出来，
    // 否则成本无法计算（见 `Usage` 的字段说明）
    let gi = |outer: &str, inner: &str| {
        usage
            .and_then(|u| u.get(outer))
            .and_then(|d| d.get(inner))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    Ok(LlmResponse {
        id: v.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        model: v.get("model").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
        status: status.to_string(),
        output,
        incomplete_reason: None,
        raw_incomplete: None,
        usage: Usage {
            input_tokens: g("prompt_tokens").unwrap_or(0),
            output_tokens: g("completion_tokens").unwrap_or(0),
            total_tokens: g("total_tokens").unwrap_or(0),
            cached_input_tokens: gi("prompt_tokens_details", "cached_tokens"),
            reasoning_tokens: gi("completion_tokens_details", "reasoning_tokens"),
        },
        error: None,
    })
}

/// 一次响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    #[serde(default)]
    pub id: String,

    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub output: Vec<OutputItem>,
    #[serde(default)]
    pub usage: Usage,
    /// API 级错误（实测形态 `{"error": {"message", "type", "code"}}`）。
    #[serde(default)]
    pub error: Option<ApiError>,
    /// 未完成的原因（实测形态 `incomplete_details: {"reason": "max_output_tokens"}`）。
    ///
    /// # 为什么必须暴露它（实测教训）
    ///
    /// 输出被 `max_output_tokens` 截断时，端点返回的是 **HTTP 200** 且
    /// `status: "incomplete"`——**不是错误**。而 `function_call.arguments`
    /// 会是**半截 JSON**。
    ///
    /// 实测遇到：思考档 `high` 下，一次 256 token 上限的调用把 234 个 token
    /// 花在思考上，`arguments` 只剩 `{"additionalProperties"`（未闭合）。
    /// 若调用方不检查这个字段，看到的现象是"JSON 解析失败"——
    /// **一个指向错误方向的诊断**（会让人去查解析代码，而真正的原因是预算不足）。
    #[serde(default)]
    pub incomplete_reason: Option<String>,
    /// 线上形态的 `incomplete_details: {reason: "..."}`（内部用，解析时摊平）。
    #[serde(default, rename = "incomplete_details")]
    raw_incomplete: Option<RawIncomplete>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawIncomplete {
    #[serde(default)]
    pub reason: Option<String>,
}

/// API 错误体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub message: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub code: Option<String>,
}

impl LlmResponse {
    /// 响应是否**被截断**（输出触到上限）。
    ///
    /// 截断的响应**不可当正常结果使用**：工具参数可能是半截 JSON，
    /// 文本可能停在句中。调用方应先检查它。
    pub fn is_truncated(&self) -> bool {
        self.status == "incomplete" || self.incomplete_reason.is_some()
    }

    /// 截断原因的可读描述（供错误消息）。
    pub fn truncation_note(&self) -> String {
        match &self.incomplete_reason {
            Some(r) => format!("原因：{r}"),
            None => format!("status={}", self.status),
        }
    }

    /// 是否成功完成（实测成功时为 `status = "completed"`）。
    pub fn is_completed(&self) -> bool {
        self.error.is_none() && self.status == "completed"
    }

    /// 全部函数调用。
    pub fn function_calls(&self) -> Vec<(&str, &str, &serde_json::Value)> {
        self.output
            .iter()
            .filter_map(|o| o.as_function_call())
            .collect()
    }

    /// 拼接全部文本输出（不含思维链）。
    pub fn text(&self) -> String {
        self.output
            .iter()
            .filter(|o| matches!(o, OutputItem::Message { .. }))
            .map(|o| o.text())
            .collect::<Vec<_>>()
            .join("")
    }

    /// 拼接思维链文本（供审计，**不进报告数字**）。
    pub fn reasoning_text(&self) -> String {
        self.output
            .iter()
            .filter(|o| matches!(o, OutputItem::Reasoning { .. }))
            .map(|o| o.text())
            .collect::<Vec<_>>()
            .join("")
    }
}

/// Wire 协议。
///
/// # 为什么必须支持两种（而不只是一种更"先进"的）
///
/// 实测（2026-09-18）：
/// - **DeepSeek** 支持 Responses（且要求回放思维链）
/// - **GLM 没有 Responses 端点**（穷举 5 条路径全 404，详见
///   `research/harness/12-provider-live-probe.md` 的 A10d 节）
///
/// 设计文档 §17-D2 定的是"GLM 为主（国内合规 + 中文）、DeepSeek 备选"，
/// 而它同时把"必须先完成 Chat Completions 适配层"列为**阻塞项**。
/// 故两种 wire 都要有——这不是兼容性洁癖，是模型可替换性的前提。
///
/// # 两者的差异**不止于字段名**
///
/// | 维度 | Responses | Chat |
/// |---|---|---|
/// | 输入 | `input` 数组，项带 `type` | `messages` 数组，项带 `role` |
/// | 工具结果 | `function_call_output` 项 | `role:"tool"` 消息 |
/// | 工具定义 | 扁平 `{type,name,parameters}` | 嵌套 `{type,function:{…}}` |
/// | 输出 | `output` 数组（多项） | `choices[0].message`（单个） |
/// | 思维链 | 独立 `reasoning` 项，**DeepSeek 强制回放** | `reasoning_content` 字段，GLM 不要求 |
/// | token 上限字段 | `max_output_tokens` | `max_tokens` |
///
/// 把自己的 `InputItem` 当"中立表示"、按 wire 编码，比让两套形状各自流到
/// 循环里要安全：调用方（agent 循环）只认一种表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI Responses 形态（DeepSeek 实测可用）。
    Responses,
    /// OpenAI Chat Completions 形态（GLM 唯一可用的形态）。
    Chat,
}

impl Wire {
    pub fn as_str(self) -> &'static str {
        match self {
            Wire::Responses => "responses",
            Wire::Chat => "chat",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "responses" => Some(Wire::Responses),
            "chat" | "chat_completions" => Some(Wire::Chat),
            _ => None,
        }
    }
}

/// 端点配置。
#[derive(Debug, Clone, PartialEq)]
pub struct Endpoint {
    /// 基础 URL（含 API 版本前缀，如 `https://api.deepseek.com` 或
    /// `https://open.bigmodel.cn/api/paas/v4`）。
    pub base_url: String,
    /// 环境变量名（**不存明文 key**——与 Harness §11 的凭据纪律一致）。
    pub api_key_env: String,
    /// wire 协议。
    pub wire: Wire,
    /// 下一轮**是否必须回放思维链**。
    ///
    /// # 为什么这是端点的属性而不是 wire 的属性
    ///
    /// 实测两侧**相反**：
    /// - DeepSeek（Responses）：**强制**，且每个 `function_call` 前各要一份，
    ///   缺则 400 `The reasoning_text … must be passed back`。
    /// - GLM（Chat）：**不要求**（回放与否都 200）。
    ///
    /// 同一 wire 的不同实现可能不同（例如另一个 Responses 端点未必强制），
    /// 故它属于**端点**而非协议。默认按实测值给：DeepSeek true、GLM false。
    pub echo_reasoning: bool,
}

impl Endpoint {
    /// DeepSeek 端点（A10b 实测确认支持 Responses）。
    ///
    /// ⚠️ 实测可达路径为 `/responses`、`/v1/responses`、`/beta/responses`
    /// （均 200），而 `/api/v1/responses` 与 `/openai/v1/responses` 为 404。
    /// 本实现固定用 `/responses`（最短且实测可用），**不依赖别名行为**。
    pub fn deepseek() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            wire: Wire::Responses,
            echo_reasoning: true,
        }
    }

    /// DeepSeek 的 Chat 端点（备选路径；实测其 Responses 可用，故默认不用它）。
    pub fn deepseek_chat() -> Self {
        Self {
            base_url: "https://api.deepseek.com".into(),
            api_key_env: "DEEPSEEK_API_KEY".into(),
            wire: Wire::Chat,
            // Chat 侧未实测；保守取 true（多回放不会 400，少回放会）
            echo_reasoning: true,
        }
    }

    /// GLM（智谱）端点。
    ///
    /// ⚠️ **只有 Chat**：实测穷举 5 条 Responses 路径全 404
    /// （`/api/paas/v4/responses`、`/v1/responses`、`/beta/responses`、
    /// `/openai/responses`、`/api/paas/v4/v1/responses`）。
    /// 另注意 `/api/paas/v4/chat/completions` 与 `/api/coding/paas/v4/chat/completions`
    /// 都可用。
    ///
    /// `echo_reasoning: false` 依据实测：GLM 回放与不回放 `reasoning_content`
    /// **都是 200**，故不必回放（少传一些上下文）。
    pub fn glm() -> Self {
        Self {
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            api_key_env: "GLM_API_KEY".into(),
            wire: Wire::Chat,
            echo_reasoning: false,
        }
    }

    /// 通用 OpenAI Chat Completions 兼容端点（供本机网关或自定义兼容服务接入）。
    pub fn openai_chat(
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
        echo_reasoning: bool,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key_env: api_key_env.into(),
            wire: Wire::Chat,
            echo_reasoning,
        }
    }

    /// 本机 Magpie Gateway 端点（默认 `http://127.0.0.1:3425/v1`，可用 `MAGPIE_BASE_URL` 覆盖）。
    pub fn magpie_local() -> Self {
        let base_url = std::env::var("MAGPIE_BASE_URL")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "http://127.0.0.1:3425/v1".to_string());
        Self::openai_chat(base_url, "MAGPIE_GATEWAY_KEY", false)
    }

    /// 请求 URL（按 wire 选路径）。
    pub fn request_url(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        match self.wire {
            Wire::Responses => format!("{base}/responses"),
            Wire::Chat => format!("{base}/chat/completions"),
        }
    }

    /// 兼容旧名（等价于 `request_url`，仅 Responses 时语义准确）。
    pub fn responses_url(&self) -> String {
        self.request_url()
    }

    /// 从环境变量取 key。缺失时返回类型化错误（不 panic，也不静默用空串）。
    pub fn api_key(&self) -> Result<String, LlmError> {
        std::env::var(&self.api_key_env)
            .ok()
            .filter(|k| !k.trim().is_empty())
            .ok_or_else(|| LlmError::MissingApiKey(self.api_key_env.clone()))
    }
}

/// 传输层抽象——**这是本 crate 可测试性的关键**。
///
/// 生产实现走 ureq；测试实现返回固定响应。有了它，
/// "请求形状是否正确"可在无网络下被锁定。
pub trait Transport {
    /// 发送 JSON 请求，返回 `(状态码, 响应体)`。
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &serde_json::Value,
    ) -> Result<(u16, String), LlmError>;
}

/// 从环境变量取全局超时（秒），非法或缺失时用 [`DEFAULT_TIMEOUT_SECS`]。
///
/// 抽成函数是为了**可测**：超时配置错了不会有任何症状，直到某天
/// 服务器不响应、进程永久挂起。把它做成纯函数，测试就能直接断言取值。
pub fn timeout_secs_from_env() -> u64 {
    std::env::var("SHANGYI_LLM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

/// 构造 ureq 配置——**超时在这里被强制设上**。
///
/// 见 `post_json` 里那段注释：ureq 3.x 的默认超时全是 `None`，
/// 不设就等于永久等待。本函数是"绝不会忘"的落点。
pub fn agent_config(global_secs: u64) -> ureq::config::Config {
    ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_global(Some(std::time::Duration::from_secs(global_secs)))
        .timeout_connect(Some(std::time::Duration::from_secs(CONNECT_TIMEOUT_SECS)))
        .build()
}

/// TCP 连接超时（秒）。握手本该秒级完成，故取得比全局超时小得多——
/// 这样"网络不通"能更快地与"模型在长推理"区分开。
pub const CONNECT_TIMEOUT_SECS: u64 = 30;

/// LLM 请求的默认全局超时（秒）。
///
/// # 取值依据（两次实测调整）
///
/// 起初取 600 秒，理由是"思考档 high 的长推理是合法负载，而超时的目的是
/// 兜住挂起，不是限制推理长度"。理由对，但**数值定得太宽**：
///
/// 实测（run26）一次 intel 阶段的请求挂在本地代理上 **10 分钟**才超时，
/// 而那次挂起不产生任何信息——它只是让流水线停了 10 分钟。
/// 对 5 小时自动循环，这类静默停机是主要的时间浪费。
///
/// 现在取 300 秒，依据是**观测到的合法上界**：实测运行里最长的成功请求
/// 是 76 秒（深度情报检索、思考档 high、含 3 次工具调用）。
/// 300 秒是它的 **4 倍**——足够覆盖长推理，又不至于让一次挂起拖掉
/// 整个阶段的时间预算。
///
/// 需要更长可设 `SHANGYI_LLM_TIMEOUT_SECS`（如确实遇到超长推理）。
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// 基于 ureq 的生产传输。
///
/// `Clone` 是无成本的（零大小类型）：它**不持有任何状态**——
/// 每次调用新建 `ureq::Agent`（连接池因此每次重建，那是当前的取舍：
/// 简单、无跨请求状态，代价是失去 keep-alive。若将来要复用连接，
/// 应改为持有 `Agent` 并让它可克隆，而不是让本类型变成有状态的）。
#[derive(Clone)]
pub struct UreqTransport;

impl UreqTransport {
    /// 发一次请求，**显式指定**全局超时（秒）。
    ///
    /// 与 [`Transport::post_json`] 分开是为了**可测**：`post_json` 从环境变量
    /// 取超时（生产路径），而测试要能在 1 秒内验证「超时真的会触发」。
    /// 只能靠环境变量配超时的话，测试就得改进程级变量——那会与并行测试竞争，
    /// 是个不稳定的测试，而"不稳定"会让它被忽略，于是防线形同虚设。
    pub fn post_json_with_timeout(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &serde_json::Value,
        global_secs: u64,
    ) -> Result<(u16, String), LlmError> {
        // **必须关掉 `http_status_as_error`**（ureq 默认为 true）。
        //
        // 打开时，4xx/5xx 会变成 `Error::StatusCode(code)` —— 而那个变体**只带状态码、
        // 不带响应体**。于是 API 的实际错误说明（`{"error":{"message":...}}`）被丢弃，
        // 上层只能看到「HTTP 400：（空）」这种无法定位的错误。
        //
        // 实测代价：一次真实运行的 400 因此无从诊断，只能靠手工 curl 逐个变量二分。
        // 关掉它之后，非 2xx 也会走 `Ok(response)`，体可读。
        // ⚠️ **必须显式设超时**——ureq 3.x 的默认值**全是 `None`**：
        //
        // 实测（`ureq-3.1.4/src/config.rs` 的 `impl Default for Timeouts`）：
        // `global` / `per_call` / `resolve` / `connect` / `send_request` /
        // `recv_response` / `send_body` / `recv_body` **一个都没有默认值**，
        // 只有 `await_100` 是 1 秒。
        //
        // 后果不是"慢"，是**永久挂起**：服务器接受了 TCP 连接却不发响应时，
        // 客户端会一直等。实测一次真实运行在此卡死 **17 分钟**、CPU 时间仅
        // 0.7 秒（纯等待），进程再也不会推进——而它是自动循环的一环，
        // 于是整条流水线停摆，且没有任何报错。
        //
        // # 为什么不用一个小超时
        //
        // 思考档 `high` 的合法调用本来就可能跑几分钟。超时设小了会把
        // **正常的长推理**误判为网络故障——那是一种更坏的失效：它悄悄改变了
        // 系统行为（换模型、断会话），而用户以为只是"网络抖了一下"。
        //
        // 故：**默认值取得足够宽**（10 分钟），且**可配置**
        // （`SHANGYI_LLM_TIMEOUT_SECS`）——真正遇到长推理的用户能自己调大，
        // 而不是被迫放弃思考档。
        //
        // 连接超时单独设小（30 秒）：TCP 握手本该在秒级完成，等 10 分钟
        // 没有意义，且能更快地把"网络不通"与"模型在想"区分开。
        // ⚠️ 这里**不要**再写 `let global_secs = timeout_secs_from_env();`。
        //
        // 我犯过这个错，代价是超时测试假通了 20 分钟：那一行会**遮蔽参数**，
        // 于是调用方传的显式超时被静默忽略（测试传 1 秒、实际用默认 600 秒）。
        // 症状极具误导性——测试失败像是"ureq 的 global 超时不生效"，
        // 而真相是"我传的值根本没被用上"。
        //
        // 生产路径的超时来自环境变量，但那由 `Transport::post_json` 负责传入；
        // 本方法只认参数。
        let agent = ureq::Agent::new_with_config(agent_config(global_secs));
        let mut req = agent.post(url);
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        // 诊断开关：`SHANGYI_DUMP_REQUEST=<path>` 会把请求体写到该文件。
        //
        // 为什么需要它：**请求形状的 bug 在离线测试里看不见**。假传输层不问对错，
        // 而真实端点只会给一句可能误导的 400（本次实测：`arguments` 传对象时报
        // "invalid type: map"，传字符串但字段不配对时报 "reasoning_text must be
        // passed back"——后者与真正的原因无关）。有原始请求体就能与 curl 逐字节比对。
        //
        // 只在设置了环境变量时生效；默认不写文件（不引入 I/O 副作用）。
        if let Ok(path) = std::env::var("SHANGYI_DUMP_REQUEST") {
            let _ = std::fs::write(&path, body.to_string());
        }

        match req.send_json(body.clone()) {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                // 限长读取：错误响应体通常很小，但**不设上限就等于让远端决定**
                // 我们分配多少内存。4xx/5xx 的体尤其要读——诊断信息都在里面。
                let text = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| LlmError::Transport(e.to_string()))?;
                Ok((status, text))
            }
            // 传输层错误（连接失败、TLS、超时）：这才是真正的 `Err`。
            // `Error::StatusCode` 在关闭 `http_status_as_error` 后不再出现；
            // 若未来有人改回默认值，这里会退化成"状态码无体"——故保留分支并
            // 在注释里点明它是一条**降级**路径。
            Err(ureq::Error::StatusCode(code)) => Ok((
                code,
                format!(
                    "<响应体不可得：ureq 的 http_status_as_error 被打开了，                     它只带状态码不带体。请检查 UreqTransport 的配置。>"
                ),
            )),
            Err(e) => {
                // 超时的报错必须**可操作**：单纯回一句 "timed out" 会让人以为是
                // 网络问题而去查网络，而实际原因可能是"思考档太高的合法长推理"
                // ——两者的处置方向相反（一个重试，一个调大超时）。
                let msg = e.to_string();
                if msg.contains("timeout") || msg.contains("timed out") {
                    return Err(LlmError::Transport(format!(
                        "{msg}（全局超时 {global_secs} 秒）。                         若是长推理被误杀，请调大 `SHANGYI_LLM_TIMEOUT_SECS`；                         若是网络故障，重试即可。"
                    )));
                }
                Err(LlmError::Transport(msg))
            }
        }
    }
}

impl Transport for UreqTransport {
    fn post_json(
        &self,
        url: &str,
        headers: &[(String, String)],
        body: &serde_json::Value,
    ) -> Result<(u16, String), LlmError> {
        self.post_json_with_timeout(url, headers, body, timeout_secs_from_env())
    }
}

/// 凭据来源。
///
/// # 为什么不是"只读环境变量"
///
/// 产品部署里 key 的实际来源是 **secrets 文件**（`~/.shangyi-secrets/*.env`），
/// 由启动脚本 `export` 进环境——即"环境变量"是**其中一种**承载方式，而不是
/// 唯一契约。把它写成唯一来源会带来两个具体问题：
///
/// 1. 调用方无法用一个已解析出来的 key 构造客户端（例如未来接 keyring、
///    或从 KMS 取）。
/// 2. 测试被迫去改**进程全局环境变量**——在 Rust 2024 下那是 `unsafe`，
///    且并发测试下是真实数据竞态。用"跳过测试"回避它，等于让测试静默失效。
///
/// 故凭据是显式注入的。[`KeySource::Env`] 只是其中一种便捷取值方式。
#[derive(Debug, Clone, PartialEq)]
pub enum KeySource {
    /// 从环境变量取（缺失时返回 [`LlmError::MissingApiKey`]）。
    Env(String),
    /// 直接给一个 key（调用方负责它从哪来：文件、keyring、KMS）。
    ///
    /// `Debug` 输出**不打印 key 本身**（见 `impl fmt::Debug for KeySource`）——
    /// 凭据不得因一次日志而泄漏。
    Explicit(String),
}

impl KeySource {
    /// 解析出本次请求用的 key。
    pub fn resolve(&self) -> Result<String, LlmError> {
        match self {
            KeySource::Env(var) => std::env::var(var)
                .ok()
                .filter(|k| !k.trim().is_empty())
                .ok_or_else(|| LlmError::MissingApiKey(var.clone())),
            KeySource::Explicit(k) => {
                if k.trim().is_empty() {
                    return Err(LlmError::MissingApiKey("<explicit>".into()));
                }
                Ok(k.clone())
            }
        }
    }
}

/// LLM 客户端。
///
/// # 为什么可 `Clone`（这不是随手加的）
///
/// 一个会话里有两处需要它：主 agent 循环（`AgentLoop` 取所有权）与
/// `swarm_run` 的 persona actor（借用）。而 agent 循环必须**持有**客户端，
/// 于是借用方与持有方冲突。
///
/// 三条出路里选 `Clone`：
/// - 把 `AgentLoop` 改成借用 ⇒ 要动 22 个构造点，且它本来就是"会话的所有者"
/// - 用 `Arc<LlmClient>` ⇒ 给所有调用点加一层包装，收益仅是省一次克隆
/// - `Clone` ⇒ 零改动，代价是一次克隆（`Endpoint` 是几个 String、
///   `KeySource` 是一个 String、`UreqTransport` 是零大小）
///
/// 克隆**不复制任何连接或状态**：`UreqTransport` 无状态，每次调用新建
/// `ureq::Agent`。故两个副本完全等价，不存在"两个客户端不同步"的风险。
#[derive(Clone)]
pub struct LlmClient<T: Transport> {
    endpoint: Endpoint,
    key_source: KeySource,
    transport: T,
    /// 重试上限（仅对"可重试"错误生效，见 [`is_retryable`]）。
    pub max_retries: u32,
}

/// 该状态码是否值得重试。
///
/// 429（限流）与 5xx（服务端）可重试；4xx（请求错误）不可——
/// 重试一个格式错误的请求只会浪费配额。
pub fn is_retryable(status: u16) -> bool {
    status == 429 || (500..600).contains(&status)
}

impl<T: Transport> LlmClient<T> {
    /// 构造：凭据默认从 `endpoint.api_key_env` 指定的环境变量取。
    pub fn new(endpoint: Endpoint, transport: T) -> Self {
        let key_source = KeySource::Env(endpoint.api_key_env.clone());
        Self {
            endpoint,
            key_source,
            transport,
            max_retries: 2,
        }
    }

    /// 构造：显式给出 key（用于 secrets 文件 / keyring / 测试）。
    ///
    /// 这是**测试不碰环境变量**的关键：并发测试改环境变量是数据竞态，
    /// 而"跳过测试"会让覆盖静默消失。显式注入让两条路都不必走。
    pub fn with_key(mut self, key: impl Into<String>) -> Self {
        self.key_source = KeySource::Explicit(key.into());
        self
    }

    /// 构造：显式给出凭据来源（最通用）。
    pub fn with_key_source(mut self, src: KeySource) -> Self {
        self.key_source = src;
        self
    }

    /// 当前凭据来源（不含 key 明文）。
    pub fn key_source(&self) -> &KeySource {
        &self.key_source
    }

    /// 当前端点（供 CLI 打印"用的是哪条 wire"）。
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// 访问底层传输（供测试与诊断——例如查看实际发出的请求形状）。
    ///
    /// 暴露它而非让测试用反射/复制：使"请求形状"可被离线断言，
    /// 这是本 crate 可测试性设计的核心。
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// 发送一次请求。
    ///
    /// # 重试策略
    ///
    /// 仅对 429/5xx 重试；4xx 立即返回错误（重试格式错误无意义）。
    /// **不做指数退避的实际 sleep**——退避应由调用方（agent 循环）编排，
    /// 使库函数保持可测试与无副作用。这是刻意的：库里的 sleep
    /// 会让单元测试变慢且不可控。
    pub fn complete(&self, request: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let key = self.key_source.resolve()?;
        let url = self.endpoint.request_url();
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {key}")),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        // 按端点的 wire 编码；`echo_reasoning` 也取自端点（两侧要求相反，见其文档）
        let body = request.to_body_for(self.endpoint.wire, self.endpoint.echo_reasoning)?;

        let mut last_err: Option<LlmError> = None;
        for attempt in 0..=self.max_retries {
            match self.transport.post_json(&url, &headers, &body) {
                Ok((status, text)) if (200..300).contains(&status) => {
                    // 诊断开关（与 `SHANGYI_DUMP_REQUEST` 同族）：
                    // **解析失败时**把原始响应落盘，供逐字节比对。
                    //
                    // 为什么需要它：serde 的报错只给出行列号，而"第 373 列"
                    // 在中文/转义字符混排的内容里无法靠肉眼定位；
                    // 且错误消息里的"前 500 字节"是**转义后**的文本，
                    // 列号映射不上去。有原文才能确定是"端点给了坏 JSON"
                    // 还是"我们的解析方式不对"。
                    if let Ok(path) = std::env::var("SHANGYI_DUMP_RESPONSE") {
                        let _ = std::fs::write(&path, &text);
                    }
                    // 响应也按 wire 解析：Chat 的 choices/嵌套 tool_calls 在此
                    // 归一化为与 Responses 相同的 `LlmResponse`
                    let mut resp: LlmResponse = match self.endpoint.wire {
                        Wire::Responses => serde_json::from_str(&text).map_err(|e| {
                            LlmError::Parse(format!(
                                "{e}；响应体前 500 字节：{}",
                                truncate(&text, 500)
                            ))
                        })?,
                        Wire::Chat => parse_chat_response(&text)?,
                    };
                    // 摊平 `incomplete_details.reason`（见该字段的说明）
                    if resp.incomplete_reason.is_none() {
                        resp.incomplete_reason =
                            resp.raw_incomplete.as_ref().and_then(|r| r.reason.clone());
                    }
                    // API 级错误（HTTP 200 但 body 带 error）
                    if let Some(err) = &resp.error {
                        return Err(LlmError::Api {
                            status,
                            body: format!("{}: {}", err.kind, err.message),
                        });
                    }
                    return Ok(resp);
                }
                Ok((status, text)) => {
                    let e = LlmError::Api {
                        status,
                        body: truncate(&text, 800),
                    };
                    if !is_retryable(status) || attempt == self.max_retries {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
                Err(e) => {
                    if attempt == self.max_retries {
                        return Err(e);
                    }
                    last_err = Some(e);
                }
            }
        }
        Err(LlmError::RetriesExhausted(
            self.max_retries,
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "未知".into()),
        ))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…（截断，原长 {} 字节）", &s[..end], s.len())
}

/// 供报告与日志引用的端点摘要（**不含 key**）。
/// `Debug` **不泄漏 key**：只打印端点与凭据来源的类型，不打印 key 明文。
///
/// 这是有意的：`#[derive(Debug)]` 会让任何一次 `{:?}` 打印（包括 panic 消息
/// 与日志）把凭据写进可能被提交的文件里。凭据纪律不能靠"记得别打印"。
impl<T: Transport> fmt::Debug for LlmClient<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let source = match &self.key_source {
            KeySource::Env(v) => format!("env:{v}"),
            KeySource::Explicit(_) => "explicit:<redacted>".to_string(),
        };
        f.debug_struct("LlmClient")
            .field("endpoint", &self.endpoint)
            .field("key_source", &source)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (key from ${})", self.responses_url(), self.api_key_env)
    }
}

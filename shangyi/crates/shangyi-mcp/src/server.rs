//! stdio 服务端：JSON-RPC 2.0 over 换行分隔 stdin/stdout。
//!
//! # 铁律：stdout **只**有协议消息
//!
//! 一个字节的诊断混进 stdout，客户端就会把整条流读成坏 JSON 并判定"服务端坏了"。
//! 故本模块：
//!
//! - 只通过注入的 `Write` 写协议消息（因此测试可以用内存管道断言**每一行**）；
//! - 所有诊断走 `eprintln!`；
//! - 领域层的 `println!` 不存在（`shangyi-core`/`shangyi-solver` 的
//!   `print_stdout` 在 solver 里是 deny，其余路径也不用它）。
//!
//! # 方法面（8 个）
//!
//! | 方法 | 有无 id | 行为 |
//! |---|---|---|
//! | `initialize` | 有 | 协商 protocolVersion，返回 `capabilities.{tools,resources}` + `serverInfo` |
//! | `notifications/initialized` | 无 | 忽略（规范：通知不回复） |
//! | `ping` | 有 | `{}` |
//! | `tools/list` | 有 | 全部工具（name/description/inputSchema） |
//! | `tools/call` | 有 | 阶段闸门 → 执行 → 结果映射 |
//! | `resources/list` | 有 | 35 篇知识库 + 4 套剧本 + 门店选址规程 |
//! | `resources/templates/list` | 有 | `shangyi://kb/{slug}`、`shangyi://playbook/{id}` |
//! | `resources/read` | 有 | 按 URI 读取逐字原文 |
//!
//! 其余方法一律 `-32601`（未知方法）。**工具级失败不走 JSON-RPC error**：
//! 那是 `tools/call` 结果里的 `isError`（见 [`crate::host`]）。

use std::io::{BufRead, Write};

use serde_json::{Value, json};

use crate::host::{HostConfig, HostError, SessionHost};
use crate::protocol::{self, Incoming, RpcError};
use crate::tools;

/// 本服务端声明支持的 MCP 协议版本（新→旧）。
///
/// 与 dsh 0.2.1-rc.2 所带的 `@modelcontextprotocol/client` 2.2.0 实际可协商的
/// 版本对齐（该 SDK 的 `LATEST_PROTOCOL_VERSION = "2025-11-25"`，
/// 支持列表含 2025-06-18 / 2025-03-26 / 2024-11-05）。
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// 客户端请求了不认识的版本时，本服务端会回这个版本。
pub const PREFERRED_PROTOCOL_VERSION: &str = "2025-11-25";

/// `serverInfo.name`。
pub const SERVER_NAME: &str = "shangyi-mcp";

/// 运行统计（供测试与诊断；不参与协议）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub requests: usize,
    pub notifications: usize,
    pub responses_ignored: usize,
    pub parse_errors: usize,
    pub tool_calls: usize,
}

/// 服务端。
pub struct Server<R: BufRead, W: Write> {
    input: R,
    output: W,
    host: SessionHost,
    initialized: bool,
    client_protocol_version: Option<String>,
    stats: Stats,
}

impl<R: BufRead, W: Write> Server<R, W> {
    pub fn new(input: R, output: W, host: SessionHost) -> Self {
        Self {
            input,
            output,
            host,
            initialized: false,
            client_protocol_version: None,
            stats: Stats::default(),
        }
    }

    pub fn host(&self) -> &SessionHost {
        &self.host
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// 跑到 EOF。返回最终统计。
    pub fn run(&mut self) -> std::io::Result<Stats> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.input.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            // 纯空白行不是消息：回 -32700 只会让客户端收到它没提问的错误。
            if line.trim().is_empty() {
                continue;
            }
            if let Some(response) = self.handle_line(line.trim_end_matches(['\n', '\r'])) {
                self.write_message(&response)?;
            }
        }
        Ok(self.stats.clone())
    }

    /// 处理一行，返回要写出去的响应（`None` = 不回复）。
    ///
    /// **公开给测试**：这样协议形状可以在内存里逐条断言，不必 spawn 进程。
    pub fn handle_line(&mut self, line: &str) -> Option<Value> {
        self.handle(protocol::parse_line(line))
    }

    fn handle(&mut self, incoming: Incoming) -> Option<Value> {
        match incoming {
            Incoming::Request { id, method, params } => {
                self.stats.requests += 1;
                Some(self.handle_request(id, &method, params))
            }
            Incoming::Notification { method, .. } => {
                self.stats.notifications += 1;
                if method == "notifications/initialized" {
                    self.initialized = true;
                }
                // 规范：通知**绝不**回复（包括不认识的通知）。
                None
            }
            Incoming::Response { id } => {
                self.stats.responses_ignored += 1;
                eprintln!("商奕[mcp]：忽略客户端响应（本服务端不发起请求）：id={id}");
                None
            }
            Incoming::Malformed { id, error } => {
                self.stats.parse_errors += 1;
                Some(protocol::error_response(id, &error))
            }
        }
    }

    fn handle_request(&mut self, id: Value, method: &str, params: Value) -> Value {
        match method {
            "initialize" => protocol::result_response(id, self.initialize_result(&params)),
            "ping" => protocol::result_response(id, json!({})),
            "tools/list" => protocol::result_response(id, json!({"tools": self.tool_list()})),
            "resources/list" => protocol::result_response(
                id,
                crate::resources::list_resources(self.host.explicit_home()),
            ),
            "resources/templates/list" => {
                protocol::result_response(id, crate::resources::list_resource_templates())
            }
            "resources/read" => match self.parse_resource_read(&params) {
                Ok(uri) => match crate::resources::read_resource(&uri, self.host.explicit_home()) {
                    Ok(value) => protocol::result_response(id, value),
                    Err(crate::resources::ResourceReadError::InvalidParams(msg)) => {
                        protocol::error_response(id, &RpcError::new(protocol::INVALID_PARAMS, msg))
                    }
                    Err(crate::resources::ResourceReadError::NotFound(msg)) => {
                        protocol::error_response(id, &RpcError::new(-32002, msg))
                    }
                    Err(crate::resources::ResourceReadError::Io(msg)) => {
                        protocol::error_response(id, &RpcError::new(-32603, msg))
                    }
                },
                Err(e) => protocol::error_response(id, &e),
            },
            "tools/call" => match self.parse_tool_call(&params) {
                Ok((name, arguments)) => {
                    self.stats.tool_calls += 1;
                    let result = self.host.call(&name, arguments);
                    let content: Vec<Value> = result
                        .content
                        .iter()
                        .map(|text| json!({"type": "text", "text": text}))
                        .collect();
                    protocol::result_response(
                        id,
                        json!({"content": content, "isError": result.is_error}),
                    )
                }
                // 参数非法是**协议级**错误（-32602）：它不是"工具跑失败了"，
                // 而是"这条请求不成话"（比如 name 不是字符串）。
                Err(e) => protocol::error_response(id, &e),
            },
            other => protocol::error_response(
                id,
                &RpcError::new(
                    protocol::METHOD_NOT_FOUND,
                    format!(
                        "未知方法 `{other}`（本服务端实现：initialize / ping / tools/list / tools/call / resources/list / resources/templates/list / resources/read）"
                    ),
                ),
            ),
        }
    }

    fn parse_resource_read(&self, params: &Value) -> Result<String, RpcError> {
        let Some(obj) = params.as_object() else {
            return Err(RpcError::new(
                protocol::INVALID_PARAMS,
                "`resources/read` 的 params 必须是对象：{\"uri\":\"shangyi://...\"}",
            ));
        };
        let Some(uri) = obj.get("uri").and_then(Value::as_str) else {
            return Err(RpcError::new(
                protocol::INVALID_PARAMS,
                "`resources/read` 缺少字符串参数 `uri`",
            ));
        };
        Ok(uri.to_string())
    }

    fn parse_tool_call(&self, params: &Value) -> Result<(String, Value), RpcError> {
        let Some(obj) = params.as_object() else {
            return Err(RpcError::new(
                protocol::INVALID_PARAMS,
                "`tools/call` 的 params 必须是对象：{\"name\":\"<工具名>\",\"arguments\":{...}}",
            ));
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            return Err(RpcError::new(
                protocol::INVALID_PARAMS,
                "`tools/call` 缺少字符串参数 `name`",
            ));
        };
        if name.trim().is_empty() {
            return Err(RpcError::new(
                protocol::INVALID_PARAMS,
                "`tools/call` 的 `name` 不得为空",
            ));
        }
        let arguments = match obj.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(v @ Value::Object(_)) => v.clone(),
            Some(_) => {
                return Err(RpcError::new(
                    protocol::INVALID_PARAMS,
                    "`tools/call` 的 `arguments` 必须是 JSON 对象（缺省视为 {}）",
                ));
            }
        };
        Ok((name.to_string(), arguments))
    }

    fn initialize_result(&mut self, params: &Value) -> Value {
        let requested = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .map(str::to_string);
        let negotiated = match requested.as_deref() {
            Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v.to_string(),
            // 客户端版本未知：回我们声明的版本（规范允许），并在 instructions 说清。
            _ => PREFERRED_PROTOCOL_VERSION.to_string(),
        };
        self.client_protocol_version = requested.clone();

        let mut instructions = String::new();
        instructions.push_str(
            "商奕（ShangYi）博弈论金融决策内核。这里的每个工具都由确定性 Rust 引擎执行，\
             模型不产生任何进入结论的数字。\n",
        );
        instructions.push_str(&format!(
            "本次会话：模式 `{}`（{}），剧本 `{}`，案例 `{}`。会话从 `intake` 阶段开始。\n",
            self.host.mode().as_str(),
            phase_path(self.host.mode()),
            self.host.playbook().meta.id,
            self.host.case_id(),
        ));
        instructions.push_str(
            "工具按**会话阶段白名单**放行：`tools/list` 会列出全部工具，但阶段不匹配的调用\
             返回 `isError: true` 并列出当前阶段允许的工具。每个工具结果的末尾都有一行 \
             `[session]` 说明当前阶段与推进情况。\n",
        );
        instructions.push_str(
            "`intake` 阶段先调用 `select_task_scope` 选择任务范围（可同时传入 `mode`、`playbook`、`action` \
             动态选定问法与行业剧本）；在此之前除 `ask_user` 外的域工具都会被拒绝。短任务结案（`complete_task`）\
             或条件分析交付后若用户继续追问，先调用 `begin_followup_task` 再调用 `select_task_scope`。\n",
        );
        instructions.push_str(
            "本服务端同时通过 MCP Resources（`resources/list` / `resources/read`）暴露 35 篇博弈论知识库\
             （`shangyi://kb/<slug>`）、4 套剧本（`shangyi://playbook/<id>`）及门店选址规程\
             （`shangyi://guide/store_location_research`）原文。\n",
        );
        instructions.push_str(
            "工具结果里出现 `\"needs_user_input\": true` 表示需要**转达用户并等待回答**\
             （确认 / 采集 / 提议回退），那不是失败。关口确认（cp1/cp2/cp3）可由用户在工作台点击核准\
             （服务端自动同步），或在真实用户回复确认后再次调用 `checkpoint`（传 `decision` + 逐字 `user_quote`）\
             结算；回退提议（`request_rewind`）同样须在真实用户回复后以 `decision` + `user_quote` 结算。\
             严禁模型在用户未回复前自行代批。`\"blocked\": true` 表示外部依赖不可用（未取得），\
             也不是失败，不得用推测填补。\n",
        );
        if let Some(v) = &requested {
            if v != &negotiated {
                instructions.push_str(&format!(
                    "协议版本：客户端请求 `{v}`，本服务端支持 {SUPPORTED_PROTOCOL_VERSIONS:?}，\
                     故按 `{negotiated}` 协商。若客户端不接受该版本，应断开连接。\n"
                ));
            }
        }

        json!({
            "protocolVersion": negotiated,
            "capabilities": {
                "tools": {"listChanged": false},
                "resources": {"subscribe": false, "listChanged": false},
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "version": env!("CARGO_PKG_VERSION"),
                "title": format!("商奕 MCP 服务端（{} 个领域工具，阶段白名单生效）", self.host.registry().len()),
            },
            "instructions": instructions,
        })
    }

    fn tool_list(&self) -> Vec<Value> {
        tools::wire_tools(self.host.registry())
            .iter()
            .map(|t| t.to_mcp())
            .collect()
    }

    fn write_message(&mut self, message: &Value) -> std::io::Result<()> {
        let text = serde_json::to_string(message)
            .map_err(|e| std::io::Error::other(format!("协议消息序列化失败：{e}")))?;
        self.output.write_all(text.as_bytes())?;
        self.output.write_all(b"\n")?;
        // 立刻 flush：stdio 另一头是交互式的，攒缓冲会让客户端觉得"服务端没响应"。
        self.output.flush()
    }
}

/// 阶段路径的简写（`intake → intel → …`）。
fn phase_path(mode: shangyi_core::ConsultationMode) -> String {
    shangyi_core::PhaseGraph::for_mode(mode)
        .path()
        .iter()
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(" → ")
}

/// 用真实 stdin/stdout 起服务（CLI 的唯一入口）。
///
/// 返回值只表示**传输层**的结局（EOF 正常返回 `Ok`）。
pub fn serve_stdio(config: HostConfig) -> Result<Stats, HostError> {
    let host = SessionHost::open(config)?;
    // 横幅只写事实：目录算出来了但写不进去时，必须说"未启用"而不是宣称已记录。
    let archiving = if host.archiving() {
        match host.case_dir() {
            Some(dir) => format!("留档={}", dir.display()),
            None => "留档=未启用".to_string(),
        }
    } else {
        "留档=未启用（目录不可写）".to_string()
    };
    eprintln!(
        "商奕[mcp]：就绪（模式={}，剧本={}，阶段={}，案例={}，{archiving}）。协议消息走 stdout，诊断走 stderr。",
        host.mode().as_str(),
        host.playbook().meta.id,
        host.phase().as_str(),
        host.case_id(),
    );
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = Server::new(stdin.lock(), stdout.lock(), host);
    let stats = server.run()?;
    eprintln!(
        "商奕[mcp]：stdin 已关闭，退出。请求 {} / 通知 {} / 工具调用 {} / 解析错误 {}。",
        stats.requests, stats.notifications, stats.tool_calls, stats.parse_errors
    );
    Ok(stats)
}

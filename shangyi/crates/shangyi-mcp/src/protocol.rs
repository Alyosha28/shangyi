//! JSON-RPC 2.0 线格式（MCP 的传输层）。
//!
//! # 为什么手写而不引第三方 MCP/JSON-RPC crate
//!
//! 服务端只需要 5 个方法与 5 个错误码（见 [`crate::server`]），而协议面的
//! **每一个字节都由我们自己写出去**——多一个依赖只会让"stdout 里到底有没有
//! 非协议字节"更难证明。本项目对外部依赖的既有判据是"能否离线锁定行为"，
//! 而这里的行为面小到可以用测试穷举（见 `tests/protocol.rs`）。
//!
//! # 消息形态（换行分隔，一行一条）
//!
//! | 形态 | 判别 | 是否回复 |
//! |---|---|---|
//! | 请求 request | 有 `method` 且有 `id` | 必须回复（结果或错误） |
//! | 通知 notification | 有 `method` 但**没有** `id` | **绝不回复**（规范要求） |
//! | 响应 response | 有 `id` 且有 `result`/`error`，无 `method` | 本服务端不发请求，故忽略 |
//!
//! # 错误码只用标准的五个
//!
//! `-32700` 解析错误 / `-32600` 非法请求 / `-32601` 方法不存在 /
//! `-32602` 参数非法 / `-32603` 内部错误。
//!
//! **工具级失败不是 RPC 错误**（见 [`crate::host`]）：协议错误与领域失败的
//! 处置完全不同——前者说明"这次对话不成话"，后者说明"这个工具这次没成"。
//! 把二者混成一个通道，模型会把"参数写错"读成"服务端坏了"。

use serde_json::{Value, json};

/// 本服务端实现的 JSON-RPC 版本（唯一合法值）。
pub const JSONRPC: &str = "2.0";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

/// 一个 JSON-RPC 错误对象。
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    /// 结构化补充信息（可选）。**只放诊断**，不放业务结论。
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn to_value(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("code".into(), json!(self.code));
        map.insert("message".into(), json!(self.message));
        if let Some(d) = &self.data {
            map.insert("data".into(), d.clone());
        }
        Value::Object(map)
    }
}

/// 解析一条入站消息的结论。
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// 必须回复。
    Request {
        id: Value,
        method: String,
        /// 缺省为 `null`（规范允许 params 缺失）。
        params: Value,
    },
    /// **绝不可回复**（包括 `notifications/initialized`）。
    Notification { method: String, params: Value },
    /// 客户端对我们发起的请求的回复。本服务端当前**不发起**任何请求，
    /// 故收到它属于异常但无害的情形：记 stderr、不回。
    Response { id: Value },
    /// 消息本身不成话：回复一个 JSON-RPC 错误。
    Malformed { id: Value, error: RpcError },
}

/// 解析一行文本。
///
/// 空行/纯空白行由调用方跳过（见 [`crate::server`]）——它们不是消息，
/// 回复 `-32700` 只会让客户端收到一条它没提问的错误。
pub fn parse_line(line: &str) -> Incoming {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Incoming::Malformed {
                id: Value::Null,
                error: RpcError::new(PARSE_ERROR, format!("JSON 解析失败：{e}")),
            };
        }
    };
    let Some(obj) = value.as_object() else {
        return Incoming::Malformed {
            id: Value::Null,
            error: RpcError::new(INVALID_REQUEST, "消息必须是 JSON 对象"),
        };
    };

    let id = obj.get("id").cloned();
    if obj.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC) {
        return Incoming::Malformed {
            id: id.unwrap_or(Value::Null),
            error: RpcError::new(
                INVALID_REQUEST,
                format!("`jsonrpc` 必须是 \"{JSONRPC}\"（缺失或取值不对）"),
            ),
        };
    }

    if obj.contains_key("method") {
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            return Incoming::Malformed {
                id: id.unwrap_or(Value::Null),
                error: RpcError::new(INVALID_REQUEST, "`method` 必须是字符串"),
            };
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return match id {
            Some(id) => Incoming::Request {
                id,
                method: method.to_string(),
                params,
            },
            // 无 id ⇒ 通知。规范：通知**不得**被回复，即使方法不认识。
            None => Incoming::Notification {
                method: method.to_string(),
                params,
            },
        };
    }

    if obj.contains_key("result") || obj.contains_key("error") {
        return match id {
            Some(id) => Incoming::Response { id },
            None => Incoming::Malformed {
                id: Value::Null,
                error: RpcError::new(INVALID_REQUEST, "响应必须带 `id`"),
            },
        };
    }

    Incoming::Malformed {
        id: id.unwrap_or(Value::Null),
        error: RpcError::new(INVALID_REQUEST, "既没有 `method`，也不是响应（缺 `result`/`error`）"),
    }
}

/// 构造一条成功响应。`id` 原样回显（**不得**改写类型：数字仍是数字）。
pub fn result_response(id: Value, result: Value) -> Value {
    json!({"jsonrpc": JSONRPC, "id": id, "result": result})
}

/// 构造一条错误响应。
pub fn error_response(id: Value, error: &RpcError) -> Value {
    json!({"jsonrpc": JSONRPC, "id": id, "error": error.to_value()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_notification_are_distinguished_by_id_presence() {
        let req = parse_line(r#"{"jsonrpc":"2.0","id":7,"method":"ping"}"#);
        assert!(matches!(req, Incoming::Request { id, ref method, .. } if id == json!(7) && method == "ping"));

        let note = parse_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
        assert!(matches!(note, Incoming::Notification { ref method, .. } if method == "notifications/initialized"));
    }

    #[test]
    fn bad_json_is_a_parse_error_with_null_id() {
        match parse_line("{oops") {
            Incoming::Malformed { id, error } => {
                assert!(id.is_null(), "解析错误的 id 必须是 null（规范要求）");
                assert_eq!(error.code, PARSE_ERROR);
            }
            other => panic!("应为 Malformed，实得 {other:?}"),
        }
    }

    #[test]
    fn wrong_jsonrpc_version_is_invalid_request() {
        match parse_line(r#"{"id":1,"method":"ping","jsonrpc":"1.0"}"#) {
            Incoming::Malformed { id, error } => {
                assert_eq!(id, json!(1));
                assert_eq!(error.code, INVALID_REQUEST);
            }
            other => panic!("应为 Malformed，实得 {other:?}"),
        }
    }

    #[test]
    fn response_is_not_a_request() {
        let out = parse_line(r#"{"jsonrpc":"2.0","id":3,"result":{}}"#);
        assert_eq!(out, Incoming::Response { id: json!(3) });
    }

    #[test]
    fn string_ids_are_echoed_unchanged() {
        assert_eq!(
            result_response(json!("abc"), json!({})),
            json!({"jsonrpc":"2.0","id":"abc","result":{}})
        );
    }
}

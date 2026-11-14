//! 商奕 MCP（Model Context Protocol）stdio 服务端。
//!
//! # 存在理由：让 agent 运行时段（DeepSeek Harness）真正驱动商奕内核
//!
//! 商奕的领域能力是进程内的确定性 Rust 代码（求解器、选择引擎、后果预测…），
//! 而 DSH 是 TypeScript，二者不可能靠 `use` 互相链接。MCP 是它们之间唯一的
//! 标准缝隙：**DSH 作 agent 运行时段（模型、会话、工具编排、审批），
//! 商奕作领域引擎（每个数字都由 Rust 算出）**。
//!
//! ```text
//!   dsh（TS，MIT）                          shangyi（Rust，本 workspace）
//!   ┌────────────────────┐   MCP stdio    ┌──────────────────────────────┐
//!   │ agent loop         │◀──────────────▶│ mcp-serve                    │
//!   │ system prompt      │  JSON-RPC 2.0  │  ├ ToolRegistry（39 个工具） │
//!   │ 会话事件 / 审批    │  换行分隔      │  ├ DomainExecutor（执行 seam）│
//!   └────────────────────┘                │  ├ PhaseMachine（产物闸门）   │
//!                                         │  └ CaseLog（留档）            │
//!                                         └──────────────────────────────┘
//! ```
//!
//! # 分工边界（谁拥有什么）
//!
//! | 问题 | 唯一真相源 | 本 crate 的角色 |
//! |---|---|---|
//! | 有哪些工具、允许在哪个阶段 | [`shangyi_tools::ToolRegistry::standard`] | 搬运（`tools` 模块） |
//! | 工具参数长什么样 | `shangyi_tools::schema::tool_schema` | 搬运 + 客户端子集降级（留痕） |
//! | 工具怎么执行 | [`shangyi_core::DomainExecutor::execute`] | 调用（`host` 模块） |
//! | 阶段能否推进 | [`shangyi_core::PhaseMachine::advance`] | 调用并把拒绝理由原文回传给模型 |
//! | 线格式 | MCP / JSON-RPC 2.0 | 实现（`protocol` + `server`） |
//!
//! 本 crate **不含任何领域判断**：没有第二个阶段表、没有第二个工具清单、
//! 没有"模型说的数字算数"这种捷径。
//!
//! # 三类失败必须分开（这是本服务端最容易被写坏的地方）
//!
//! | 失败 | 通道 | 语义 |
//! |---|---|---|
//! | 协议错误（坏 JSON、未知方法、参数不成话） | JSON-RPC `error` | "这次对话不成话" |
//! | 工具失败（执行报错、错阶段调用） | `result.isError = true` | "这个工具这次没成" |
//! | 未取得 / 需用户裁决 | `result.isError = false` + 结构化 JSON | "世界没给数据 / 等人决定" |
//!
//! 第三类是设计文档反复强调的区分（`ExecOutcome::Blocked` 与 `Failed`）：
//! 把"查不到"报成"出错"，模型会重试一个必然失败的调用；把"要用户确认"
//! 报成错误，模型会以为闸门坏了。
//!
//! # 用法
//!
//! ```text
//! shangyi mcp-serve [--playbook <id>] [--mode scenario|whatif|discover] [--home <dir>]
//!                   [--auto-approve-checkpoints]
//! ```
//!
//! 参数缺省值与 `shangyi new` 完全一致（模式 `scenario`、剧本 `pricing_response`、
//! 主目录 `$SHANGYI_HOME` → `$HOME/.shangyi`）。`--auto-approve-checkpoints`
//! 默认关闭；打开后 checkpoint 由服务端结算并记 `actor = auto`（与
//! `SHANGYI_ASSUME_YES=1` 同源，**不是**用户确认）。

pub mod host;
pub mod protocol;
pub mod resources;
pub mod server;
pub mod tools;

pub use host::{DEFAULT_PLAYBOOK_ID, HostConfig, HostError, SessionHost, ToolCallResult};
pub use protocol::{Incoming, RpcError};
pub use server::{
    PREFERRED_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS, SERVER_NAME, Server, Stats,
    serve_stdio,
};

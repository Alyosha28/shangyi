//! 商奕命令行入口（设计文档 §10 与 Harness §13.6）。
//!
//! # 为何自解析而非用 clap
//!
//! 参数面很小（5 个子命令 + 少量选项），自解析可避免为一层薄封装引入依赖树。
//! **代价是失去自动 help 与补全**——若参数面增长应改用 clap。此权衡已写入
//! `Cargo.toml` 的注释。
//!
//! # 本模块的核心职责
//!
//! 把命令行**解析为类型化的命令枚举**，使"未知命令 / 缺参数 / 非法组合"
//! 在进入业务逻辑前即被拒绝。这比"各子命令自己校验"更可靠：
//! 后者容易漏掉某个组合。

pub mod kb_cmd;
pub mod report_cmd;
pub mod run;
pub mod start_cmd;
pub mod web_io;

pub use run::search_provider_from_env;

use shangyi_core::SessionPhase;
use shangyi_playbooks::Playbook;
use shangyi_tools::ToolRegistry;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CliError {
    #[error("没这个命令：`{0}`")]
    UnknownCommand(String),
    #[error("`{command}` 还缺 `{arg}`")]
    MissingArg { command: String, arg: String },
    #[error("`{command}` 不认得选项 `{opt}`")]
    UnknownOption { command: String, opt: String },
    #[error("`{value}` 用不了；只能填 {allowed:?}")]
    InvalidValue { value: String, allowed: Vec<String> },
    #[error("问法 `{mode}` 和这个选项不搭：{reason}")]
    ModeConflict { mode: String, reason: String },
    #[error("启动前检查没过：{0}")]
    SelfCheck(String),
}

/// 三种咨询模式（§1.3 / §13.6）。
///
/// 三者共用同一引擎栈，差别在**阶段序列**与**工具白名单**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// 情景分析：回答"这局博弈的均衡在哪"。
    Scenario,
    /// 决策推演：用户给定动作 `d`，回答"做了会怎样"。
    Whatif,
    /// 策略发现：用户给满意标准，回答"哪些策略达标"。
    Discover,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Scenario => "scenario",
            Mode::Whatif => "whatif",
            Mode::Discover => "discover",
        }
    }

    pub fn parse(s: &str) -> Result<Self, CliError> {
        match s {
            "scenario" => Ok(Mode::Scenario),
            "whatif" => Ok(Mode::Whatif),
            "discover" => Ok(Mode::Discover),
            other => Err(CliError::InvalidValue {
                value: other.to_string(),
                allowed: vec!["scenario".into(), "whatif".into(), "discover".into()],
            }),
        }
    }

    /// 该模式的阶段序列（Harness §13.6 逐行对应）。
    ///
    /// **委托**给 [`shangyi_core::PhaseGraph`]：阶段路径只有一处定义。
    /// 本方法此前是同一知识的第二份拷贝，且三处分支返回了**同一个数组**
    /// （`whatif` 里错误地含 `select`、`discover` 与 `scenario` 完全相同）。
    /// 拷贝会漂移，路径不能。
    pub fn phases(self) -> Vec<SessionPhase> {
        shangyi_core::PhaseGraph::for_mode(self.into())
            .path()
            .to_vec()
    }

    /// 与 `shangyi_core::ConsultationMode` 的转换（两者语义相同，分属交付层与接线层）。
    pub fn to_consultation_mode(self) -> shangyi_core::ConsultationMode {
        match self {
            Mode::Scenario => shangyi_core::ConsultationMode::Scenario,
            Mode::Whatif => shangyi_core::ConsultationMode::Whatif,
            Mode::Discover => shangyi_core::ConsultationMode::Discover,
        }
    }

    /// `whatif` 是否**需要**用户给定动作 `d`。
    pub fn requires_action(self) -> bool {
        matches!(self, Mode::Whatif)
    }

    /// `discover` 是否**需要**满意标准。
    pub fn requires_aspiration(self) -> bool {
        matches!(self, Mode::Discover)
    }
}

impl From<Mode> for shangyi_core::ConsultationMode {
    fn from(m: Mode) -> Self {
        m.to_consultation_mode()
    }
}

/// 报告输出格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportFormat {
    Pdf,
    Docx,
    Md,
}

impl ReportFormat {
    pub fn parse(s: &str) -> Result<Self, CliError> {
        match s {
            "pdf" => Ok(ReportFormat::Pdf),
            "docx" => Ok(ReportFormat::Docx),
            "md" => Ok(ReportFormat::Md),
            other => Err(CliError::InvalidValue {
                value: other.to_string(),
                allowed: vec!["pdf".into(), "docx".into(), "md".into()],
            }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ReportFormat::Pdf => "pdf",
            ReportFormat::Docx => "docx",
            ReportFormat::Md => "md",
        }
    }
}

/// 类型化的命令。
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// 新建咨询。
    New {
        mode: Mode,
        /// `whatif` 模式必需：用户给定的决策动作。
        action: Option<String>,
        /// 剧本 id（缺省则按场景推荐）。
        playbook: Option<String>,
    },
    /// 恢复既有咨询。
    Resume { case: String },
    /// 离线求解：读入 GameSpec JSON 文件，输出解集 JSON（不建会话、不调 LLM）。
    ///
    /// 这是给程序化调用（Web 控制台、脚本、测试）的**机器接口**：
    /// stdin/stdout 均为 JSON，路径与工具 `solve_equilibria` 完全同源
    /// （`shangyi_core::domain_exec::solve_spec_to_json`）。
    Solve {
        /// GameSpec JSON 文件路径（`--in`，必填）。
        input: String,
        /// 求解方法：support_enumeration / lemke_howson / analytic。
        ///
        /// 取值域校验留给运行时（solver 的报错含完整可用清单与别名，
        /// 且会随引擎演进——解析层重复一份只会过期）。
        method: Option<String>,
        /// 精炼方法：trembling_hand_perfect / ess_symmetric / ess_two_population。
        refine: Option<String>,
        /// ESS 扰动格点分辨率（2..=64）。范围校验在解析层做：
        /// 这是命令行自身的语法约束，且静默回退默认值会让
        /// "写错参数"看起来像"用了默认"。
        refine_denominator: Option<i64>,
    },
    /// 启动 Web 工作台与 DSH 宿主（推荐新手入口）。
    Start {
        port: Option<u16>,
        no_open: bool,
    },
    /// 渲染报告。
    Report { case: String, format: ReportFormat },
    /// 回测校准。
    Backtest { scenario: Option<String> },
    /// 知识库操作。
    Kb { op: KbOp },
    /// 人工核准或驳回挂起中的阶段确认点（`user_actor = "user"`）。
    Checkpoint {
        /// 案例 id 或 `latest`。
        case: String,
        /// `true` = 核准通过（`--approve`），`false` = 驳回要求修订（`--reject`）。
        approve: bool,
        /// 可选确认/修订备注。
        note: Option<String>,
        /// 商奕主目录覆盖（缺省 `$SHANGYI_HOME` → `$HOME/.shangyi`）。
        home: Option<String>,
    },
    /// MCP（Model Context Protocol）stdio 服务：把领域工具暴露给 agent 运行时段。
    ///
    /// 一个进程 = 一次咨询会话；stdin/stdout 只走 JSON-RPC 2.0（换行分隔），
    /// 诊断走 stderr。协议与宿主的实现都在 `shangyi-mcp`，本层只做参数解析。
    McpServe {
        /// 剧本 id（缺省与 `new` 一致：`pricing_response`）。
        playbook: Option<String>,
        /// 咨询模式（缺省与 `new` 一致：`scenario`）。
        mode: Option<Mode>,
        /// 商奕主目录（缺省 `$SHANGYI_HOME` → `$HOME/.shangyi`）。
        home: Option<String>,
        /// 自动结算 checkpoint（默认关闭；语义同 `SHANGYI_ASSUME_YES=1`，日志记 actor=auto）。
        auto_approve_checkpoints: bool,
        /// 续接既有案例（`<case_id>` 或 `latest`）。
        resume: Option<String>,
    },
}

/// 知识库子操作。
#[derive(Debug, Clone, PartialEq)]
pub enum KbOp {
    Add { path: String },
    Search { query: String },
    Show { id: String },
    List,
}

impl Command {
    /// 该命令是否**修改**状态。
    ///
    /// 用途：只有非只读命令才需要写权限与审批；只读命令可在只读沙箱下运行。
    pub fn is_mutating(&self) -> bool {
        match self {
            Command::New { .. } => true,
            Command::Resume { .. } => true,
            // solve 只读输入文件、只写 stdout——无状态、可在只读沙箱下运行
            Command::Solve { .. } => false,
            Command::Start { .. } => true,
            Command::Report { .. } => true, // 产出文件
            Command::Backtest { .. } => true, // 更新判据权重
            Command::Kb { op } => matches!(op, KbOp::Add { .. }),
            Command::Checkpoint { .. } => true,
            // mcp-serve 会写案例日志（`$SHANGYI_HOME/cases/...`）
            Command::McpServe { .. } => true,
        }
    }

    /// 该命令涉及的会话阶段（用于工具白名单与阶段校验）。
    pub fn phases(&self) -> Vec<SessionPhase> {
        match self {
            Command::New { mode, .. } => mode.phases(),
            Command::Resume { .. } => vec![], // 阶段由已存案例状态决定
            Command::Solve { .. } => vec![SessionPhase::Solve],
            Command::Start { .. } => vec![],
            Command::Report { .. } => vec![SessionPhase::Report],
            Command::Backtest { .. } => vec![SessionPhase::Archived],
            Command::Kb { .. } => vec![],
            Command::Checkpoint { .. } => vec![],
            // 阶段由会话进程内的阶段机决定；此处给出该模式的完整路径（与 `new` 同口径）
            Command::McpServe { mode, .. } => mode.unwrap_or(Mode::Scenario).phases(),
        }
    }
}

/// 解析命令行参数（不含程序名）。
///
/// # 校验时机
///
/// 全部校验在解析阶段完成——包括"whatif 必须给动作"这类**模式与选项的耦合**。
/// 若留到业务逻辑里校验，每个入口都得重复一遍。
pub fn parse(args: &[String]) -> Result<Command, CliError> {
    if args.is_empty() {
        return Err(CliError::UnknownCommand(
            "（没给命令。可以：new 开聊 / resume 接着聊 / solve 算 / report 出报告 / backtest 校准 / kb 知识库）".into(),
        ));
    }
    let cmd = args[0].as_str();
    let rest = &args[1..];

    match cmd {
        "new" => parse_new(rest),
        "resume" => {
            let case = take_positional(rest, "resume", "case")?;
            reject_unknown_options(&rest[1..], "resume")?;
            Ok(Command::Resume { case })
        }
        "solve" => parse_solve(rest),
        "checkpoint" => parse_checkpoint(rest),
        "mcp-serve" => parse_mcp_serve(rest),
        "report" => {
            let case = take_positional(rest, "report", "case")?;
            let mut format = None;
            let mut it = rest[1..].iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--format" | "-f" => {
                        let v = it.next().ok_or_else(|| CliError::MissingArg {
                            command: "report".into(),
                            arg: "--format".into(),
                        })?;
                        format = Some(ReportFormat::parse(v)?);
                    }
                    other => {
                        return Err(CliError::UnknownOption {
                            command: "report".into(),
                            opt: other.to_string(),
                        })
                    }
                }
            }
            Ok(Command::Report {
                case,
                format: format.unwrap_or(ReportFormat::Pdf),
            })
        }
        "backtest" => {
            let mut scenario = None;
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--scenario" | "-s" => {
                        let v = it.next().ok_or_else(|| CliError::MissingArg {
                            command: "backtest".into(),
                            arg: "--scenario".into(),
                        })?;
                        scenario = Some(v.clone());
                    }
                    // 同名位置参数形式：`shangyi backtest pricing_response`
                    other if !other.starts_with('-') && scenario.is_none() => {
                        scenario = Some(other.to_string());
                    }
                    other => {
                        return Err(CliError::UnknownOption {
                            command: "backtest".into(),
                            opt: other.to_string(),
                        })
                    }
                }
            }
            Ok(Command::Backtest { scenario })
        }
        "start" | "ui" => parse_start(rest),
        "kb" => {
            if rest.is_empty() {
                return Err(CliError::MissingArg {
                    command: "kb".into(),
                    arg: "add|search|show|list".into(),
                });
            }
            match rest[0].as_str() {
                "add" => {
                    let path = take_positional(&rest[1..], "kb add", "path")?;
                    Ok(Command::Kb {
                        op: KbOp::Add { path },
                    })
                }
                "search" => {
                    // 查询可能含空格：把剩余参数拼起来
                    let query = rest[1..].join(" ");
                    if query.trim().is_empty() {
                        return Err(CliError::MissingArg {
                            command: "kb search".into(),
                            arg: "query".into(),
                        });
                    }
                    Ok(Command::Kb {
                        op: KbOp::Search { query },
                    })
                }
                "show" => {
                    let id = take_positional(&rest[1..], "kb show", "id")?;
                    Ok(Command::Kb {
                        op: KbOp::Show { id },
                    })
                }
                "list" => {
                    reject_unknown_options(&rest[1..], "kb list")?;
                    Ok(Command::Kb { op: KbOp::List })
                }
                other => Err(CliError::InvalidValue {
                    value: other.to_string(),
                    allowed: vec!["add".into(), "search".into(), "show".into(), "list".into()],
                }),
            }
        }
        other => Err(CliError::UnknownCommand(other.to_string())),
    }
}

fn parse_start(rest: &[String]) -> Result<Command, CliError> {
    let mut port: Option<u16> = None;
    let mut no_open = false;

    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--port" | "-p" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "start".into(),
                    arg: "--port".into(),
                })?;
                let parsed: u16 = v.parse().map_err(|_| CliError::InvalidValue {
                    value: v.clone(),
                    allowed: vec!["1..=65535 的端口号".into()],
                })?;
                if parsed == 0 {
                    return Err(CliError::InvalidValue {
                        value: v.clone(),
                        allowed: vec!["1..=65535 的端口号".into()],
                    });
                }
                port = Some(parsed);
            }
            "--no-open" => no_open = true,
            other => {
                return Err(CliError::UnknownOption {
                    command: "start".into(),
                    opt: other.to_string(),
                });
            }
        }
    }

    Ok(Command::Start { port, no_open })
}

fn parse_checkpoint(rest: &[String]) -> Result<Command, CliError> {
    let case = take_positional(rest, "checkpoint", "case")?;
    let mut approve: Option<bool> = None;
    let mut note: Option<String> = None;
    let mut home: Option<String> = None;

    let mut it = rest[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--approve" => {
                if approve == Some(false) {
                    return Err(CliError::InvalidValue {
                        value: "--approve + --reject".into(),
                        allowed: vec!["--approve".into(), "--reject".into()],
                    });
                }
                approve = Some(true);
            }
            "--reject" => {
                if approve == Some(true) {
                    return Err(CliError::InvalidValue {
                        value: "--approve + --reject".into(),
                        allowed: vec!["--approve".into(), "--reject".into()],
                    });
                }
                approve = Some(false);
            }
            "--note" | "-n" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "checkpoint".into(),
                    arg: "--note".into(),
                })?;
                note = Some(v.clone());
            }
            "--home" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "checkpoint".into(),
                    arg: "--home".into(),
                })?;
                home = Some(v.clone());
            }
            other => {
                return Err(CliError::UnknownOption {
                    command: "checkpoint".into(),
                    opt: other.to_string(),
                });
            }
        }
    }

    let approve = approve.ok_or_else(|| CliError::MissingArg {
        command: "checkpoint".into(),
        arg: "--approve|--reject".into(),
    })?;
    Ok(Command::Checkpoint {
        case,
        approve,
        note,
        home,
    })
}

/// 解析 `mcp-serve`。
fn parse_mcp_serve(rest: &[String]) -> Result<Command, CliError> {
    let mut playbook: Option<String> = None;
    let mut mode: Option<Mode> = None;
    let mut home: Option<String> = None;
    let mut auto_approve_checkpoints = false;
    let mut resume: Option<String> = None;

    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--playbook" | "-p" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "mcp-serve".into(),
                    arg: "--playbook".into(),
                })?;
                playbook = Some(v.clone());
            }
            "--mode" | "-m" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "mcp-serve".into(),
                    arg: "--mode".into(),
                })?;
                mode = Some(Mode::parse(v)?);
            }
            "--home" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "mcp-serve".into(),
                    arg: "--home".into(),
                })?;
                home = Some(v.clone());
            }
            "--resume" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "mcp-serve".into(),
                    arg: "--resume".into(),
                })?;
                resume = Some(v.clone());
            }
            // 与 `SHANGYI_ASSUME_YES=1` 同源：把 checkpoint 结算成 actor=auto。
            // 默认关闭——"替用户确认"必须是显式选择，不能是默认行为。
            "--auto-approve-checkpoints" => auto_approve_checkpoints = true,
            other => {
                return Err(CliError::UnknownOption {
                    command: "mcp-serve".into(),
                    opt: other.to_string(),
                })
            }
        }
    }

    Ok(Command::McpServe {
        playbook,
        mode,
        home,
        auto_approve_checkpoints,
        resume,
    })
}

fn parse_solve(rest: &[String]) -> Result<Command, CliError> {
    let mut input: Option<String> = None;
    let mut method: Option<String> = None;
    let mut refine: Option<String> = None;
    let mut refine_denominator: Option<i64> = None;

    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let require_value = |it: &mut std::slice::Iter<'_, String>, arg: &str| -> Result<String, CliError> {
            it.next().cloned().ok_or_else(|| CliError::MissingArg {
                command: "solve".into(),
                arg: arg.into(),
            })
        };
        match a.as_str() {
            "--in" | "-i" => input = Some(require_value(&mut it, "--in")?),
            "--method" | "-m" => method = Some(require_value(&mut it, "--method")?),
            "--refine" | "-r" => refine = Some(require_value(&mut it, "--refine")?),
            "--refine-denominator" | "-d" => {
                let v = require_value(&mut it, "--refine-denominator")?;
                let n: i64 = v.parse().map_err(|_| CliError::InvalidValue {
                    value: v.clone(),
                    allowed: vec!["2..=64 的整数".into()],
                })?;
                if !(2..=64).contains(&n) {
                    return Err(CliError::InvalidValue {
                        value: v,
                        allowed: vec!["2..=64 的整数".into()],
                    });
                }
                refine_denominator = Some(n);
            }
            other => {
                return Err(CliError::UnknownOption {
                    command: "solve".into(),
                    opt: other.to_string(),
                })
            }
        }
    }

    let input = input.ok_or_else(|| CliError::MissingArg {
        command: "solve".into(),
        arg: "--in <GameSpec JSON 文件>".into(),
    })?;
    Ok(Command::Solve {
        input,
        method,
        refine,
        refine_denominator,
    })
}

fn parse_new(rest: &[String]) -> Result<Command, CliError> {
    let mut mode = Mode::Scenario; // 默认 scenario（§10）
    let mut action: Option<String> = None;
    let mut playbook: Option<String> = None;

    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--mode" | "-m" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "new".into(),
                    arg: "--mode".into(),
                })?;
                mode = Mode::parse(v)?;
            }
            "--action" | "-a" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "new".into(),
                    arg: "--action".into(),
                })?;
                action = Some(v.clone());
            }
            "--playbook" | "-p" => {
                let v = it.next().ok_or_else(|| CliError::MissingArg {
                    command: "new".into(),
                    arg: "--playbook".into(),
                })?;
                playbook = Some(v.clone());
            }
            other => {
                return Err(CliError::UnknownOption {
                    command: "new".into(),
                    opt: other.to_string(),
                })
            }
        }
    }

    // **模式与选项的耦合校验**（放在解析阶段，避免各入口重复）
    if mode.requires_action() && action.is_none() {
        return Err(CliError::ModeConflict {
            mode: mode.as_str().into(),
            reason: "试做法必须给 --action <你打算怎么做>（不然推演啥？）".into(),
        });
    }
    if !mode.requires_action() && action.is_some() {
        return Err(CliError::ModeConflict {
            mode: mode.as_str().into(),
            reason: format!(
                "`--action` 只有试做法用得上；{} 问法直接描述问题就行",
                mode.as_str()
            ),
        });
    }

    Ok(Command::New {
        mode,
        action,
        playbook,
    })
}

fn take_positional(args: &[String], command: &str, arg: &str) -> Result<String, CliError> {
    args.first()
        .filter(|s| !s.starts_with('-'))
        .cloned()
        .ok_or_else(|| CliError::MissingArg {
            command: command.into(),
            arg: arg.into(),
        })
}

fn reject_unknown_options(args: &[String], command: &str) -> Result<(), CliError> {
    for a in args {
        if a.starts_with('-') {
            return Err(CliError::UnknownOption {
                command: command.into(),
                opt: a.clone(),
            });
        }
    }
    Ok(())
}

/// 启动前的**自检**：把命令与剧本、工具注册表交叉校验。
///
/// # 为何要这一步
///
/// 解析阶段只能校验"命令行自身是否合法"；还需校验**命令与领域配置是否相容**：
/// - 剧本是否存在
/// - 该模式所需阶段是否都在剧本支持范围内
/// - 该模式的主工具是否在剧本白名单内
///
/// 这些若留到业务流程里，会在用户已经投入时间后才失败。
pub fn self_check(
    cmd: &Command,
    registry: &ToolRegistry,
    playbook: Option<&Playbook>,
) -> Result<(), CliError> {
    let Command::New { mode, .. } = cmd else {
        return Ok(()); // 其余命令不涉及模式-剧本耦合
    };
    let Some(pb) = playbook else {
        return Ok(()); // 未指定剧本 ⇒ 由后续推荐流程决定
    };

    // 该模式的主工具必须在剧本白名单内
    let primary = match mode {
        Mode::Scenario => "solve_equilibria",
        Mode::Whatif => "consequence_predict",
        Mode::Discover => "discover_satisfice",
    };
    if !registry.get(primary).is_some() {
        return Err(CliError::SelfCheck(format!(
            "注册表中缺少工具 `{primary}`（模式 {} 的主工具）",
            mode.as_str()
        )));
    }
    if !pb.tool_allowlist.is_empty() && !pb.allows_tool(primary) {
        return Err(CliError::SelfCheck(format!(
            "剧本 `{}` 的白名单不含模式 `{}` 的主工具 `{primary}`——\
             该剧本不支持此模式，请换剧本或改模式",
            pb.meta.id,
            mode.as_str()
        )));
    }
    Ok(())
}

/// 渲染帮助文本（供 `--help`）。
pub fn help() -> String {
    let mut s = String::new();
    s.push_str("商奕 —— 帮你把经营里的两难想清楚\n\n");
    s.push_str("推荐新手入口（自动初始化 ~/.shangyi/cases 并拉起宣纸·墨玉工作台）：\n");
    s.push_str("  shangyi start [--port 8791] [--no-open]   （别名：shangyi ui）\n\n");
    s.push_str("用法：\n");
    s.push_str("  shangyi start [--port <端口>] [--no-open]\n");
    s.push_str("  shangyi new [--mode scenario|whatif|discover] [--playbook <id>] [--action <你打算怎么做>]\n");
    s.push_str("  shangyi resume <案例>\n");
    s.push_str("  shangyi report <案例ID|latest|目录|case.jsonl> [--format md|pdf|docx]\n");
    s.push_str("  shangyi kb list | kb search <关键词> | kb show <词条id> | kb add <路径>\n");
    s.push_str("  shangyi backtest [--scenario <id>]\n");
    s.push_str("  shangyi solve --in <gamespec.json> [--method support_enumeration|lemke_howson|analytic]\n");
    s.push_str("               [--refine trembling_hand_perfect|ess_symmetric|ess_two_population] [--refine-denominator 6]\n");
    s.push_str("  shangyi checkpoint <案例|latest> --approve|--reject [--note <备注>] [--home <目录>]\n");
    s.push_str("  shangyi mcp-serve [--playbook <id>] [--mode scenario|whatif|discover] [--home <目录>]\n");
    s.push_str("                    [--resume <案例|latest>] [--auto-approve-checkpoints]\n\n");
    s.push_str("三种问法：\n");
    s.push_str("  scenario  看可能 —— 两边各有哪几种做法、哪种最稳（默认）\n");
    s.push_str("  whatif    试做法 —— 我这么做，对方会怎样，结果如何（必须 --action）\n");
    s.push_str("  discover  找做法 —— 按我定的目标，哪些做法够得上\n\n");
    s.push_str("mcp-serve 是给 agent 运行时段（DeepSeek Harness）用的 MCP stdio 服务：\n");
    s.push_str("  stdin/stdout 只走 JSON-RPC 2.0 协议消息（换行分隔），诊断一律走 stderr；\n");
    s.push_str("  一个进程 = 一次咨询会话，阶段白名单与产物闸门仍然生效。\n\n");
    s.push_str("solve 是离线机器接口：读 GameSpec JSON 文件、stdout 输出解集 JSON，\n");
    s.push_str("不建会话、不调 LLM、不写任何文件（供 Web 控制台/脚本程序化调用）。\n");
    s
}

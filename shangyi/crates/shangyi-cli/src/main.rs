//! 商奕 CLI 入口。
//!
//! **本二进制只做三件事**：解析命令 → 自检 → 分派。
//! 业务逻辑在各自的 crate 中——入口保持极薄，使"命令行行为"可被
//! 单元测试覆盖（见 `tests/parsing.rs`），而无需启动完整流程。

use std::process::ExitCode;

use shangyi_cli::{self_check, help, parse, Command, KbOp};
use shangyi_tools::ToolRegistry;

/// `SHANGYI_ASSUME_YES` 的取值判据（与 `shangyi_cli::run::start_consultation`
/// 里那段完全一致：非空且不是 "0"）。
fn assume_yes_from_env() -> bool {
    std::env::var("SHANGYI_ASSUME_YES")
        .map(|v| !v.trim().is_empty() && v.trim() != "0")
        .unwrap_or(false)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.as_slice() == ["--web-protocol-version"] {
        println!("{}", shangyi_cli::web_io::PROTOCOL_VERSION);
        return ExitCode::SUCCESS;
    }

    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        print!("{}", help());
        // 无参数视为用法提示：非零退出码（便于脚本判断），但 help 本身为零
        return if args.is_empty() {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }

    let cmd = match parse(&args) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("商奕：{e}");
            eprintln!("\n打 `shangyi --help` 看咋用。");
            return ExitCode::from(2);
        }
    };

    // 启动前自检（命令与领域配置交叉校验）
    let registry = ToolRegistry::standard();
    // 注：剧本加载需读文件系统；此处以 None 表示"由后续推荐流程决定"。
    // 真正的剧本解析在 `shangyi-playbooks`，此处不重复实现 I/O。
    if let Err(e) = self_check(&cmd, &registry, None) {
        eprintln!("商奕：{e}");
        return ExitCode::from(3);
    }

    // 分派：把命令接到**真实流程**（`shangyi_cli::run`）。
    //
    // 这里不再有"已解析但未实现"的占位分支——占位分支是危险的：
    // 它让管道的每一段都"看起来能跑"，从而把"功能缺失"推迟到用户
    // 真正依赖它的那一刻才暴露。仍未接线的环节改为**明确报错**。
    match &cmd {
        Command::New { mode, action, playbook } => {
            let core_mode = mode.to_consultation_mode();
            match shangyi_cli::run::start_consultation(
                core_mode,
                playbook.as_deref(),
                action.as_deref(),
            ) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("商奕：{e}");
                    // 凭据/剧本缺失是**配置问题**（退出码 3），不是崩溃
                    ExitCode::from(3)
                }
            }
        }
        Command::Resume { case } => match shangyi_cli::run::resume_consultation(case) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("商奕：{e}");
                ExitCode::from(3)
            }
        },
        Command::Solve {
            input,
            method,
            refine,
            refine_denominator,
        } => match shangyi_cli::run::run_solve(
            input,
            method.as_deref(),
            refine.as_deref(),
            *refine_denominator,
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("商奕：{e}");
                ExitCode::from(3)
            }
        },
        Command::Start { port, no_open } => {
            match shangyi_cli::start_cmd::run_start(*port, *no_open) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("商奕：{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Report {
            case,
            format,
            output,
        } => match shangyi_cli::report_cmd::run_report(case, *format, output.as_deref()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("商奕：{e}");
                ExitCode::FAILURE
            }
        },
        Command::Backtest { scenario } => {
            // `--scenario` 目前只用于**筛选展示**，不改变数据来源。
            // 如实说明这一点，而不是让它看起来"按场景过滤了"。
            // ⚠️ 用 CLI 自己的 `Mode::parse`（它已有校验），而不是
            // 给 `ConsultationMode` 加一个 parse——后者的合法值域与
            // 命令行语法是两件事，混在一起会让"命令行接受什么"
            // 与"领域里有哪几种模式"互相牵扯。
            let mode = match scenario.as_deref() {
                None => None,
                Some(s) => match shangyi_cli::Mode::parse(s) {
                    Ok(m) => Some(m.into()),
                    Err(_) => {
                        eprintln!(
                            "商奕：没这个问法 `{s}`（有：scenario / whatif / discover）\
                             ——这次按全部的算"
                        );
                        None
                    }
                },
            };
            match shangyi_cli::run::run_backtest(mode) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("商奕：{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Kb { op } => {
            let res = match op {
                KbOp::Add { path } => shangyi_cli::kb_cmd::run_kb_add(&path),
                KbOp::Search { query } => shangyi_cli::kb_cmd::run_kb_search(&query),
                KbOp::Show { id } => shangyi_cli::kb_cmd::run_kb_show(&id),
                KbOp::List => shangyi_cli::kb_cmd::run_kb_list(),
            };
            match res {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("商奕：{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Command::Checkpoint {
            case,
            approve,
            note,
            home,
        } => match shangyi_cli::run::settle_case_checkpoint(
            case,
            *approve,
            note.as_deref(),
            home.as_deref().map(std::path::Path::new),
        ) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("商奕：{e}");
                ExitCode::from(3)
            }
        },
        Command::McpServe {
            playbook,
            mode,
            home,
            auto_approve_checkpoints,
            resume,
        } => {
            // ⚠️ 本分支**绝不能**往 stdout 写任何东西：stdout 是 JSON-RPC 通道，
            // 一个字节的诊断就会让客户端把整条流读成坏 JSON。诊断一律 stderr。
            let config = shangyi_mcp::HostConfig {
                // 缺省与 `shangyi new` 一致：scenario
                mode: mode.unwrap_or(shangyi_cli::Mode::Scenario).to_consultation_mode(),
                playbook_id: playbook.clone(),
                home: home.as_ref().map(std::path::PathBuf::from),
                // 显式开关 ∨ 环境变量（与 `shangyi new` 的 SHANGYI_ASSUME_YES 同源）。
                // 语义一致很重要：两条路径都不能"悄悄替用户确认"。
                auto_approve_checkpoints: *auto_approve_checkpoints || assume_yes_from_env(),
                resume_case: resume.clone().or_else(|| {
                    std::env::var("SHANGYI_RESUME_CASE")
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                }),
            };
            match shangyi_mcp::serve_stdio(config) {
                Ok(_) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("商奕：mcp-serve 启动失败：{e}");
                    // 配置问题（剧本缺失等）不是崩溃：退出码 3，与其它子命令一致
                    ExitCode::from(3)
                }
            }
        }
    }
}

//! 端到端：**spawn 真二进制** `shangyi mcp-serve`，走一遍 JSON-RPC 2.0 stdio 握手。
//!
//! 为什么这条必须在 `shangyi-cli` 里：`CARGO_BIN_EXE_<bin>` 只对**定义该 bin 的
//! crate** 的集成测试注入，而 `shangyi` 这个 bin 属于 `shangyi-cli`。
//! `shangyi-mcp` 的测试覆盖协议形状与宿主逻辑（注入内存管道），这里覆盖
//! "真的作为一个子进程被拉起、真的能说话"——两件事都不可省：前者证明逻辑对，
//! 后者证明 CLI 接线、参数解析、stdout 纯净性都对。
//!
//! 断言清单：
//! 1. `initialize` → protocolVersion / capabilities.tools / serverInfo
//! 2. `notifications/initialized` **不产生任何输出**（紧跟着 ping 的响应就是证明）
//! 3. `tools/list` 数量与名字与 `ToolRegistry::standard()` 一致
//! 4. `tools/call` 正常路径（select_task_scope）与错阶段路径（intake 调求解器）
//! 5. 未知方法 → `-32601`
//! 6. stdout 每一行都是合法 JSON（诊断只能在 stderr），且 stdin EOF 后正常退出
//! 7. 剧本缺失时**快速失败**且 stdout 为空（不吐半个协议消息）
//! 8. 参数解析：缺省值、未知选项、取值域、缺值
//! 9. `--mode` 真的改变握手里声明的阶段路径

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

// ─────────────────────────── 子进程夹具 ───────────────────────────

/// 一条挂住的 stdio 会话：`read_line` 没有超时，故带看门狗（60s 后强杀），
/// 使"协议不同步"表现为**失败**而不是 CI 挂死。
struct McpChild {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    alive: Arc<AtomicBool>,
}

impl Drop for McpChild {
    fn drop(&mut self) {
        // 先让看门狗收手，再杀子进程（顺序反了会留下一个等 60s 的线程）
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl McpChild {
    fn spawn(args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_shangyi"))
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("应能 spawn `shangyi` 二进制");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let alive = Arc::new(AtomicBool::new(true));
        let flag = alive.clone();
        std::thread::spawn(move || {
            for _ in 0..600 {
                if !flag.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("商奕[mcp] 测试看门狗：60s 未结束，强杀子进程");
            std::process::exit(70);
        });
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            alive,
        }
    }

    fn send(&mut self, message: Value) {
        let line = serde_json::to_string(&message).expect("序列化");
        self.send_raw(&line);
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin 已关闭");
        writeln!(stdin, "{line}").expect("写 stdin");
        stdin.flush().expect("flush stdin");
    }

    /// 读一条响应。**每一行都必须是合法 JSON**——这本身就断言了 stdout 纯净。
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).expect("读 stdout");
        assert!(n > 0, "子进程在给出响应前关闭了 stdout");
        let trimmed = line.trim();
        serde_json::from_str(trimmed).unwrap_or_else(|e| {
            panic!("stdout 出现了非协议内容（诊断必须走 stderr）：{trimmed:?} / {e}")
        })
    }

    fn request(&mut self, id: Value, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}));
        let response = self.recv();
        assert_eq!(response["id"], id, "响应 id 必须与请求一致：{response}");
        response
    }

    fn call_tool(&mut self, id: Value, name: &str, args: Value) -> Value {
        let response = self.request(id, "tools/call", json!({"name": name, "arguments": args}));
        assert!(
            response.get("error").is_none(),
            "工具级结果不应变成 RPC 错误：{response}"
        );
        response["result"].clone()
    }

    /// 关掉 stdin（子进程应当读到 EOF 并正常退出）。
    fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// 等退出并取回 stderr（诊断全在这里）。
    fn wait_for_exit(&mut self) -> (ExitStatus, String) {
        let status = self.child.wait().expect("wait");
        let mut stderr = String::new();
        if let Some(mut e) = self.child.stderr.take() {
            let _ = e.read_to_string(&mut stderr);
        }
        (status, stderr)
    }
}

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("时钟")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "shangyi-mcp-e2e-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("建临时目录");
    dir
}

fn home_str(home: &Path) -> String {
    home.display().to_string()
}

// ─────────────────────────── 1~6. 真二进制握手 ───────────────────────────

#[test]
fn real_binary_handshake_lists_tools_and_serves_a_call() {
    let home = temp_home("handshake");
    let home = home_str(&home);
    let mut child = McpChild::spawn(&["mcp-serve", "--home", home.as_str()]);

    // ── initialize ──
    let init = child.request(
        json!(1),
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "e2e", "version": "0"}
        }),
    );
    let result = &init["result"];
    assert_eq!(result["protocolVersion"], json!("2025-11-25"));
    assert!(result["capabilities"]["tools"].is_object());
    assert_eq!(result["serverInfo"]["name"], json!("shangyi-mcp"));

    // ── notification 不回复：紧接着的 ping 必须是**下一条**输出 ──
    child.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
    let ping = child.request(json!(2), "ping", json!({}));
    assert_eq!(
        ping["result"],
        json!({}),
        "通知之后的第一条输出必须是 ping 的响应（说明通知没有产生输出）"
    );

    // ── tools/list：与注册表逐名对齐 ──
    let listed = child.request(json!(3), "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().expect("tools 数组");
    let registry = shangyi_tools::ToolRegistry::standard();
    assert_eq!(tools.len(), registry.len(), "tools/list 数量必须等于注册表");
    assert_eq!(
        registry.len(),
        shangyi_tools::STANDARD_TOOL_COUNT,
        "注册表工具数变了：同步本断言"
    );
    let listed_names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    let expected: Vec<&str> = registry.names().into_iter().collect();
    assert_eq!(listed_names, expected);

    // ── tools/call 正常路径：intake 选范围 ──
    let scoped = child.call_tool(
        json!(4),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "端到端测试：完整咨询"}),
    );
    assert_eq!(scoped["isError"], json!(false), "选范围应成功：{scoped}");

    // ── tools/call 错阶段：intake 调求解器必须被拒，且给出去处 ──
    let rejected = child.call_tool(json!(5), "solve_equilibria", json!({}));
    assert_eq!(rejected["isError"], json!(true), "错阶段必须是工具级失败");
    let text = rejected["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("solve"), "拒绝信息要含允许阶段：{text}");

    // ── 未知方法 → -32601 ──
    let unknown = child.request(json!(6), "tools/teleport", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601));

    // ── stdin 关闭 ⇒ 进程正常退出（EOF 是正常终止，不是错误） ──
    child.close_stdin();
    let (status, stderr) = child.wait_for_exit();
    assert!(status.success(), "EOF 后应正常退出，实得 {status:?}；stderr={stderr}");
    assert!(
        stderr.contains("就绪"),
        "诊断信息必须在 stderr（实得 stderr={stderr:?}）"
    );

    // 案例留档真的落盘了（`--home` 生效）
    let cases = PathBuf::from(&home).join("cases");
    assert!(
        cases.is_dir(),
        "`--home` 下应产生 cases/ 目录：{}",
        cases.display()
    );
}

// ─────────────────────────── 7. 启动失败要干净 ───────────────────────────

#[test]
fn missing_playbook_fails_fast_without_writing_protocol_bytes() {
    let home = temp_home("no-playbook");
    let home = home_str(&home);
    let output = Command::new(env!("CARGO_BIN_EXE_shangyi"))
        .args([
            "mcp-serve",
            "--home",
            home.as_str(),
            "--playbook",
            "does-not-exist",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("应能跑完 `shangyi` 二进制");
    assert!(!output.status.success(), "剧本缺失必须非零退出");
    assert!(
        output.stdout.is_empty(),
        "启动失败时 stdout 必须是空字节（不许吐半个协议消息）：{:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does-not-exist"),
        "错误必须报出缺失的剧本 id：{stderr}"
    );
}

// ─────────────────────────── 8. 参数解析 ───────────────────────────

#[test]
fn mcp_serve_argument_parsing_has_the_documented_shape() {
    use shangyi_cli::{Command, Mode, parse};

    let argv = |parts: &[&str]| -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    };

    // 无参数：全部缺省（dsh profile 里写的正是 `args: ["mcp-serve"]`）
    assert_eq!(
        parse(&argv(&["mcp-serve"])).expect("无参数也应合法"),
        Command::McpServe {
            playbook: None,
            mode: None,
            home: None,
            auto_approve_checkpoints: false,
            resume: None,
        }
    );

    // 四个文档化参数（含 --resume）
    assert_eq!(
        parse(&argv(&[
            "mcp-serve",
            "--playbook",
            "market_entry",
            "--mode",
            "whatif",
            "--home",
            "/tmp/x",
            "--resume",
            "latest",
        ]))
        .expect("应合法"),
        Command::McpServe {
            playbook: Some("market_entry".into()),
            mode: Some(Mode::Whatif),
            home: Some("/tmp/x".into()),
            auto_approve_checkpoints: false,
            resume: Some("latest".into()),
        }
    );

    // 默认关闭的自动结算开关（显式打开才生效）
    assert_eq!(
        parse(&argv(&["mcp-serve", "--auto-approve-checkpoints"])).expect("应合法"),
        Command::McpServe {
            playbook: None,
            mode: None,
            home: None,
            auto_approve_checkpoints: true,
            resume: None,
        }
    );

    // 未知选项必须被拒（不能静默忽略）
    assert!(parse(&argv(&["mcp-serve", "--nope"])).is_err(), "未知选项必须报错");
    // `--mode` 的取值域由解析层把关
    assert!(
        parse(&argv(&["mcp-serve", "--mode", "nonsense"])).is_err(),
        "非法模式必须报错"
    );
    // 缺值也要报错，而不是当默认值
    assert!(parse(&argv(&["mcp-serve", "--home"])).is_err(), "缺参数值必须报错");
}

// ─────────────────────────── 9. `--mode` 真的改变阶段路径 ───────────────────────────

#[test]
fn mode_flag_changes_the_phase_path_in_the_handshake() {
    let home = temp_home("mode");
    let home = home_str(&home);
    let mut child = McpChild::spawn(&["mcp-serve", "--home", home.as_str(), "--mode", "discover"]);
    let init = child.request(
        json!(1),
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}}),
    );
    let instructions = init["result"]["instructions"].as_str().unwrap_or("");
    assert!(
        instructions.contains("discover"),
        "instructions 应写明本次模式：{instructions}"
    );
    assert!(
        instructions.contains("solve → discover"),
        "discover 模式的阶段路径应出现在 instructions 里：{instructions}"
    );
    // Drop 负责收尾（关 stdin → 子进程 EOF 退出前会被 kill，二者都正常）
}

// ─────────────────────────── 10. 外部 CLI `shangyi checkpoint` 同步推进真子进程 ───────────────────────────

#[test]
fn external_cli_checkpoint_approval_advances_live_mcp_serve_process() {
    let home_dir = temp_home("ext-cp");
    let home = home_str(&home_dir);
    let mut child = McpChild::spawn(&["mcp-serve", "--home", home.as_str()]);

    let _ = child.request(
        json!(1),
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
    );
    child.call_tool(
        json!(2),
        "select_task_scope",
        json!({"scope": "consultation", "reason": "测试外部真人审批"}),
    );
    child.call_tool(json!(3), "vn_map", json!({}));
    child.call_tool(
        json!(4),
        "participant_profile_update",
        json!({
            "participant_id": "rival",
            "value_net_role": "substitutor",
            "relation_to_game": "player",
            "common": {"interests": ["保住份额"], "resources": ["本地渠道"]}
        }),
    );
    let cp_res = child.call_tool(
        json!(5),
        "checkpoint",
        json!({"items": [{"id": "i1", "content": "主要竞争对手仅有一家"}]}),
    );
    let cp_payload: Value =
        serde_json::from_str(cp_res["content"][0]["text"].as_str().unwrap()).expect("cp json");
    assert_eq!(cp_payload["needs_user_input"], json!(true));
    let case_id = cp_payload["case_id"].as_str().expect("case_id").to_string();

    // 模拟真人在另一个终端执行 `shangyi checkpoint <case_id> --approve`
    let cli_out = Command::new(env!("CARGO_BIN_EXE_shangyi"))
        .args([
            "checkpoint",
            case_id.as_str(),
            "--approve",
            "--note",
            "确认无误",
            "--home",
            home.as_str(),
        ])
        .output()
        .expect("run shangyi checkpoint");
    assert!(
        cli_out.status.success(),
        "CLI checkpoint 审批应成功：stderr={}",
        String::from_utf8_lossy(&cli_out.stderr)
    );

    // 仍在运行中的 mcp-serve 下一次收到 model_build 专属工具调用时，前置自动同步并成功放行！
    let gs_res = child.call_tool(
        json!(6),
        "gamespec_validate",
        json!({}),
    );
    let status_text = gs_res["content"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["text"]
        .as_str()
        .unwrap_or("");
    assert!(
        status_text.contains("阶段=model_build"),
        "外部确认后 mcp-serve 应已自动推进至 model_build：{status_text}"
    );
}

#[test]
fn followup_task_lifecycle_works_end_to_end_in_mcp_serve() {
    let home_dir = temp_home("e2e-followup");
    let home = home_str(&home_dir);
    let mut child = McpChild::spawn(&["mcp-serve", "--home", home.as_str()]);

    let _ = child.request(
        json!(1),
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
    );
    let s1 = child.call_tool(
        json!(2),
        "select_task_scope",
        json!({"scope": "cost", "reason": "首轮先做场地租金成本试算"}),
    );
    assert_eq!(s1["isError"], json!(false), "首轮选范围应成功：{s1}");

    let cost = child.call_tool(
        json!(3),
        "business_cost",
        json!({
            "operation": "create",
            "scenario": "新店月租",
            "currency": "CNY",
            "costs_complete": false,
            "costs": [{
                "scenario": "新店月租",
                "name": "租金",
                "treatment": "cash_expense",
                "amount": null,
                "period": "month",
                "basis": "user_provided",
                "source": "房东报价未取得"
            }]
        }),
    );
    assert_eq!(cost["isError"], json!(false), "business_cost 应成功：{cost}");

    let done = child.call_tool(
        json!(4),
        "complete_task",
        json!({
            "summary": "已登记新店月租待确认报价。",
            "missing": ["房东正式租金报价"]
        }),
    );
    assert_eq!(done["isError"], json!(false), "complete_task 应成功：{done}");

    let blocked = child.call_tool(
        json!(5),
        "select_task_scope",
        json!({"scope": "research", "reason": "用户追问竞对动态"}),
    );
    assert_eq!(blocked["isError"], json!(true), "交付后未经 begin_followup_task 不得直接重选范围");
    let blocked_text = blocked["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        blocked_text.contains("begin_followup_task"),
        "应提示调用 begin_followup_task：{blocked_text}"
    );

    let reopen = child.call_tool(
        json!(6),
        "begin_followup_task",
        json!({"reason": "用户追问竞对公开动态"}),
    );
    assert_eq!(reopen["isError"], json!(false), "begin_followup_task 应成功：{reopen}");

    let s2 = child.call_tool(
        json!(7),
        "select_task_scope",
        json!({"scope": "research", "reason": "用户追问竞对公开动态"}),
    );
    assert_eq!(s2["isError"], json!(false), "重开后重新选定 research 范围应成功：{s2}");
}

#[test]
fn followup_and_task_scope_survive_mcp_serve_resume_e2e() {
    let home_dir = temp_home("e2e-resume-scope");
    let home = home_str(&home_dir);

    {
        let mut child1 = McpChild::spawn(&["mcp-serve", "--home", home.as_str()]);
        let _ = child1.request(
            json!(1),
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
        );
        let s1 = child1.call_tool(
            json!(2),
            "select_task_scope",
            json!({"scope": "cost", "reason": "首轮先算门店成本"}),
        );
        assert_eq!(s1["isError"], json!(false), "{s1}");
        let c1 = child1.call_tool(
            json!(3),
            "business_cost",
            json!({
                "operation": "create",
                "scenario": "A店成本",
                "currency": "CNY",
                "costs_complete": false,
                "costs": [{
                    "scenario": "A店成本",
                    "name": "租金",
                    "treatment": "cash_expense",
                    "amount": null,
                    "period": "month",
                    "basis": "user_provided",
                    "source": "待报价"
                }]
            }),
        );
        assert_eq!(c1["isError"], json!(false), "{c1}");
    }

    // 重启子进程并 --resume latest，验证 complete_task → begin_followup_task → consultation 多轮串联
    let mut child2 = McpChild::spawn(&["mcp-serve", "--home", home.as_str(), "--resume", "latest"]);
    let _ = child2.request(
        json!(1),
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
    );
    let done1 = child2.call_tool(
        json!(2),
        "complete_task",
        json!({"summary": "已登记A店待报价租金。", "missing": ["租金报价"]}),
    );
    assert_eq!(done1["isError"], json!(false), "--resume 后 complete_task 应成功：{done1}");

    let reopen = child2.call_tool(
        json!(3),
        "begin_followup_task",
        json!({"reason": "继续切换为完整博弈推演"}),
    );
    assert_eq!(reopen["isError"], json!(false), "--resume 后 begin_followup_task 应成功：{reopen}");

    let consult = child2.call_tool(
        json!(4),
        "select_task_scope",
        json!({
            "scope": "consultation",
            "mode": "whatif",
            "playbook": "pricing_response",
            "reason": "按完整博弈推演价格战后果"
        }),
    );
    assert_eq!(consult["isError"], json!(false), "{consult}");
    let out_text = consult["content"][0]["text"].as_str().unwrap_or("");
    assert!(out_text.contains("\"mode\":\"whatif\""), "应切换至 whatif：{out_text}");
}

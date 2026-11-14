//! 把 CLI 命令接到真实的咨询流程（设计文档 §7、§8.3）。
//!
//! # 这一层做什么
//!
//! 组装：`$SHANGYI_HOME` 下的案例目录 + 事件日志 + 阶段机 + 领域执行器 +
//! LLM 客户端 → [`SessionRunner`]，然后在终端上与用户交互。
//!
//! # 它**不**做什么
//!
//! 不含任何领域逻辑，也不含任何数值计算——它只把已经就位的部件接起来。
//! 这一层的代码若有"业务判断"，那一定是放错了地方。

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use shangyi_core::agent::{AgentLoop, LoopConfig, ToolExecutor};
use shangyi_core::case_log::{CaseHeader, CaseLog};
use shangyi_core::clock::{Clock, SystemClock};
use shangyi_core::consultation::ConsultationMode;
use shangyi_core::domain_exec::{DomainExecutor, PoiProvider};
use shangyi_core::phase_graph::{CheckpointId, CheckpointRequest, PhaseMachine, SessionPhase};
pub use shangyi_core::providers::{AmapPoi, LiveSearch, LlmSwarmActorFactory};
use shangyi_core::runner::{
    CheckpointAnswer, RewindDecision, RunError, RunReport, SessionRunner, UserIo, progress,
};
use shangyi_intel::bocha::backends_from;
pub use shangyi_intel::secrets::load_api_key;
use shangyi_intel::secrets::load_magpie_gateway_key;
use shangyi_kb::TheoryLibrary;
use shangyi_llm::{Endpoint, LlmClient, UreqTransport};
use shangyi_playbooks::Playbook;
use shangyi_tools::ToolRegistry;

/// 运行期错误（与解析期错误分开：解析错误在 `lib.rs` 的 `CliError`）。
#[derive(Debug, thiserror::Error)]
pub enum RunFailure {
    #[error("无法定位商奕主目录：既没有 $SHANGYI_HOME，也取不到 $HOME")]
    NoHome,
    #[error("案例目录不可用（{path}）：{detail}")]
    CaseDir { path: String, detail: String },
    #[error("凭据不可用：{0}")]
    Credentials(String),
    #[error("剧本加载失败：{0}")]
    Playbook(String),
    #[error("会话错误：{0}")]
    Session(String),
    #[error("离线求解失败：{0}")]
    Solve(String),
    #[error("终端交互失败：{0}")]
    Io(String),
}

/// 商奕主目录：`$SHANGYI_HOME` → `$HOME/.shangyi`。
///
/// 与设计 §4.3 的 D-A15 一致：**不读 `$CODEX_HOME`**——商奕与 codex 并存时
/// 不得互相污染。
pub fn shangyi_home() -> Result<PathBuf, RunFailure> {
    if let Ok(p) = std::env::var("SHANGYI_HOME") {
        let p = p.trim();
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").map_err(|_| RunFailure::NoHome)?;
    Ok(Path::new(&home).join(".shangyi"))
}

/// 案例目录：`$SHANGYI_HOME/cases/<日期>/<case_id>`（§8.3 的日期分片）。
///
/// # ⚠️ 日期分片按 **UTC**（不是本地时区）——这是刻意的，但有个副作用
///
/// 时间戳恒为 UTC（`SystemClock` 的设计），故分片目录也是 UTC 日期。
/// 审计上这是对的：**UTC 无歧义**，跨时区协作或换机器时不会出现
/// "同一份日志被归到两个日期"。
///
/// 副作用是中国用户会发现：**晚上 8 点（CST）之后跑的案例落在前一天的目录**。
/// 实测：本地 2026-09-19 01:20 启动的咨询写进了 `cases/2026-09-18/`。
///
/// 这不影响正确性（`case_id` 自带 HHMMSS 后缀，唯一性由它保证），
/// 但会让用户困惑。**故在此明说，而不是把分片改成 UTC+8**——
/// 后者会把"无歧义"换掉，而那是拿审计正确性换界面的顺眼。
/// 若将来要做本地分片，正确做法是在**保留 UTC 时间戳**的前提下，
/// 另设一个 `local_date` 字段用于分片。
pub fn case_dir(case_id: &str, created_at: &str) -> Result<PathBuf, RunFailure> {
    let home = shangyi_home()?;
    // 取日期的 YYYY-MM-DD 部分（created_at 形如 2026-09-17T08:30:00Z）
    let date = created_at.get(..10).unwrap_or("unknown-date");
    Ok(home.join("cases").join(date).join(case_id))
}

/// 终端用户交互（真实实现）。
///
/// # 为什么持有**一个**输入锁
///
/// 早先的实现每读一行就 `io::stdin().lock()` 一次。`StdinLock` 是**带缓冲**的：
/// 每次新建一个锁，它会把管道里的后续内容读进自己的缓冲区；锁一释放，
/// **缓冲区里没被消费的那部分就丢了**。
///
/// 实测表现：脚本用管道喂入多行答案时，只有第一行可能被读到，其余全变成
/// "跳过（未取得）"——而界面上看不出任何异常，只表现为"用户什么都没答"。
/// 这是最坏的一类 bug：**输入被静默丢弃**。
///
/// 修法：整个会话持有一个锁（`StdinLock<'static>`，Rust 1.61+ 支持）。
pub struct TerminalIo {
    /// 非交互模式（`--yes`）：自动确认，但**在日志里记成 auto**。
    pub assume_yes: bool,
    stdin: io::StdinLock<'static>,
}

impl TerminalIo {
    pub fn new(assume_yes: bool) -> Self {
        Self {
            assume_yes,
            stdin: io::stdin().lock(),
        }
    }

    /// 读一行（末尾换行已去）。返回 `None` = EOF 或读失败。
    fn read_line(&mut self) -> Option<String> {
        let mut line = String::new();
        match self.stdin.read_line(&mut line) {
            Ok(0) => None, // EOF
            Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_string()),
            Err(_) => None,
        }
    }
}

impl TerminalIo {
    fn ask_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer {
        if self.assume_yes {
            return CheckpointAnswer::Approve;
        }
        println!();
        println!("═══ 核对：{}（{}）═══", req.id.as_str(), phase_label(req.phase));
        for (i, item) in req.items.iter().enumerate() {
            println!("  {}. {}", i + 1, item.content);
            match &item.proposed_value {
                Some(v) => println!("     等你定：{v}"),
                None => {}
            }
            match &item.source {
                Some(s) => println!("     来源：{s}"),
                // 无来源必须显式说出：cp2 的硬假设本不该走到这里
                None => println!("     来源：（无）"),
            }
        }
        println!();
        println!(" [回车] 就这样    [m] 要改    [?] 先问清楚");

        let Some(line) = self.read_line() else {
            return CheckpointAnswer::Revise {
                reason: "无法读取终端输入（EOF）".into(),
            };
        };
        match line.trim() {
            "" | "y" | "Y" => CheckpointAnswer::Approve,
            "m" | "M" => {
                println!("说说哪儿要改（会记下你没同意的原因）：");
                let reason = self.read_line().unwrap_or_default();
                CheckpointAnswer::Revise {
                    reason: reason.trim().to_string(),
                }
            }
            "?" | "？" => {
                println!("请输入你的问题：");
                let q = self.read_line().unwrap_or_default();
                CheckpointAnswer::AskFollowUp {
                    question: q.trim().to_string(),
                }
            }
            other => CheckpointAnswer::Revise {
                reason: format!("没看懂 `{other}`（先按要改处理）"),
            },
        }
    }
}

fn phase_label(p: SessionPhase) -> &'static str {
    match p {
        SessionPhase::Intake => "intake 问背景",
        SessionPhase::Intel => "intel 摸底",
        SessionPhase::ModelBuild => "model_build 摆局",
        SessionPhase::Solve => "solve 算稳的",
        SessionPhase::Select => "select 挑一个",
        SessionPhase::Consequence => "consequence 做了会怎样",
        SessionPhase::Discover => "discover 找做法",
        SessionPhase::Recommend => "recommend 建议",
        SessionPhase::Report => "report 报告",
        SessionPhase::Archived => "archived 已完成",
    }
}

impl UserIo for TerminalIo {
    fn is_automated(&self) -> bool {
        self.assume_yes
    }

    /// 裁决模型的回退提议。
    ///
    /// # 为什么这里必须真的问（不能自动同意）
    ///
    /// 回退会**作废已完成的工作**（清空下游产物）。自动同意等于让模型
    /// 单方面撤销用户已经看过的成果——那与铁律 2 的精神相反。
    ///
    /// `assume_yes` 时**拒绝**（不是同意）：作废是不可逆的，而"跳过确认"
    /// 的含义是"别问我琐事"，不是"你可以替我毁掉成果"。
    fn decide_rewind(
        &mut self,
        target: shangyi_core::SessionPhase,
        reason: &str,
    ) -> RewindDecision {
        if self.assume_yes {
            println!("  （自动模式：不回退——作废成果得人定）");
            return RewindDecision::Reject;
        }
        println!();
        println!("═══ 想退回重做 ═══");
        println!("  回到：{}", target.as_str());
        println!("  理由：{reason}");
        println!();
        println!("  回去会作废这步之后已产出的（算出的、选的、报告）。");
        println!("  [回车/r] 不了，接着走    [y] 回去重做");
        let Some(line) = self.read_line() else {
            return RewindDecision::Reject;
        };
        if matches!(line.trim(), "y" | "Y" | "yes") {
            RewindDecision::Approve
        } else {
            RewindDecision::Reject
        }
    }

    fn present_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer {
        self.ask_checkpoint(req)
    }
    /// 采集提问（`ask_user`）：逐题问，逐题读。
    ///
    /// 用户敲空行 ⇒ 该题**未回答**（不写入答案表）。这与"回答了空字符串"
    /// 是两件事：未回答表示"我不知道/不想说"，会让上游如实标注"未取得"；
    /// 而空字符串会被当作一个（空的）陈述流进建模。
    fn ask(&mut self, questions: &[shangyi_core::agent::UserQuestion]) -> Vec<(String, String)> {
        // ⚠️ **不因 `assume_yes` 短路**。
        //
        // 早先这里在自动模式下直接返回空答案，本意是"不编造答案"（对的），
        // 但它把两件不同的事耦合在一起了：
        //   ① 确认点自动通过（"别问我确认"）
        //   ② 采集问题也不问（"什么也别问我"）
        //
        // 实测后果：无人值守模式下**一个问题都答不上**，而 stdin 里其实
        // 有答案。且这使"脚本化驱动一次完整咨询"做不到——而那既是自动化
        // 流程的真实需求，也是端到端测试的前提。
        //
        // 现在 `assume_yes` 只表示①。②的情形由 EOF 自然覆盖：
        // 输入耗尽时下面的循环会 break 并如实标注"其余问题标记为未取得"
        // （仍不编造答案）。
        println!();
        println!("─── 想问你 {} 件事（不答的直接回车）───", questions.len());
        let mut out = Vec::new();
        for q in questions {
            println!();
            println!("▸ {}", q.text);
            if q.kind == "choice" && !q.options.is_empty() {
                for (i, o) in q.options.iter().enumerate() {
                    println!("    {}. {}", i + 1, o);
                }
            }
            print!("  你答（回车跳过）：");
            let _ = io::stdout().flush();
            let Some(line) = self.read_line() else {
                // EOF：把剩下的问题当作"未回答"（不编造、也不丢弃已答部分）
                println!("  （不答了：剩下的记「没问到」）");
                break;
            };
            let ans = line.trim();
            if ans.is_empty() {
                println!("  （跳过了：这条记「没问到」）");
                continue;
            }
            // choice 型允许按序号回答，转成选项文本（让下游拿到的是选项而不是数字）
            let ans = if q.kind == "choice" && !q.options.is_empty() {
                ans.parse::<usize>()
                    .ok()
                    .and_then(|i| q.options.get(i.wrapping_sub(1)).cloned())
                    .unwrap_or_else(|| ans.to_string())
            } else {
                ans.to_string()
            };
            out.push((q.id.clone(), ans));
        }
        out
    }

    fn inform(&mut self, message: &str) {
        println!("· {message}");
    }
}

/// 启动一次真实咨询。
///
/// # 诚实原则
///
/// 在**任何**部件缺失时（无凭据、无剧本文件），本函数**报错退出**，
/// 不退回"模拟结果"。理由是决定性的：一个"看起来在分析"的模拟流程，
/// 比一个明确报错的流程危险得多——用户会拿它的输出做商业决策。
pub fn start_consultation(
    mode: ConsultationMode,
    playbook_id: Option<&str>,
    action: Option<&str>,
) -> Result<(), RunFailure> {
    let created_at = SystemClock.now_iso8601();

    // ⚠️ case_id **必须逐次唯一**——只含「日期+模式」是个会造成日志交叠的缺陷。
    //
    // # 实测（run20 / run21）
    //
    // 早先是 `case-{日期}-{模式}`。于是同一天第二次跑同一模式时，两次咨询
    // **写进了同一个 case.jsonl**：
    //
    // - run20（01:08 启动）被杀后，run21（01:16 启动）在 `CaseLog::open` 里
    //   看到已有日志，**续写**它（那是为 `resume` 设计的行为，被 `new` 误用）
    // - 更糟的是 `repair_if_crashed` 会把上一次运行的未闭合 turn 当成
    //   「本会话崩溃」来修复——**两次咨询的记录就此混成一条时间线**
    //
    // 对 5 小时自动循环来说这是致命的：每一轮都撞同一目录，
    // 而审计链（哪条结论出自哪次咨询）彻底失效。
    //
    // 加时分秒后，同一天不同轮次天然分开；`resume` 不受影响——它按完整
    // case_id 在日期分片下查找，而 CLI 会把 case_id 打印给用户。
    let case_seq = if std::env::var_os("SHANGYI_WEB_EVENTS_PATH").is_some() {
        format!("{}-{}", seq_from_clock(&created_at), std::process::id())
    } else {
        seq_from_clock(&created_at)
    };
    let case_id = format!(
        "case-{}-{}-{}",
        date_compact(&created_at),
        mode.as_str(),
        case_seq
    );

    // ── 剧本：从 $SHANGYI_HOME/playbooks 或仓库内 shangyi/playbooks 载入 ──
    let playbook = load_playbook(playbook_id)?;

    // ── 启动前校验：剧本必须支持该模式 ──
    //
    // ⚠️ 这项检查此前**只在一个未使用的结构上**存在（`consultation::Consultation`，
    // 仅测试使用）⇒ 真实路径上不检查。后果：用不匹配的剧本跑某模式时，
    // 会话会**卡在对应阶段直到预算耗尽**，而错误信息只说"缺少产物"——
    // 离真因（剧本缺那个工具）很远。
    //
    // 放在这里（**创建案例目录之前**）：与 `preflight` 同理——
    // 校验失败时不该留下一个空的案例目录（那会让"cases/ 下有个目录"
    // 变成"跑过一次"的假证据）。
    // Task scope is chosen during intake. The full consultation capability check
    // now runs when select_task_scope chooses consultation; a research-only
    // request does not require a game solver in its playbook.

    // ── LLM 端点与凭据：缺失即**拒绝启动**，不退回模拟 ──
    //
    // 供应商选择（`SHANGYI_PROVIDER`）：
    // - `deepseek`（默认）：走 Responses
    // - `glm`：走 Chat Completions（**GLM 没有 Responses**，见 A10d）
    //
    // 两条 wire 都已实测可用（`cargo test -p shangyi-llm -- --ignored`）。
    let provider = std::env::var("SHANGYI_PROVIDER")
        .unwrap_or_else(|_| "deepseek".to_string())
        .to_lowercase();
    // `(端点, 供应商标识, 默认模型, 默认思考档)`
    //
    // 思考档是**按供应商**给的，因为接受的值域可能不同（GLM 实测对不支持的档位
    // 直接报错而非降级，见 research/harness/08-chat-wire-spec.md）。
    // DeepSeek 已实测 `high` 被接受且工具调用正常。
    let (endpoint, provider_name, default_model, default_effort) = match provider.as_str() {
        // 默认用同代**最轻档**而非满配：`glm-5.3` 是**强制思考**模型
        // （`reasoning_effort` 默认 `max`，见 research/harness/08-chat-wire-spec.md），
        // 一次工具调用就会产生大量思考 token——这对一个要多轮调用工具的 agent
        // 是主要的成本来源。同代的 flashx 是轻量档，能力面够用而成本低得多。
        //
        // ⚠️ **价格未实测**：官网定价页是动态渲染，抓不到静态文本；且探测期间
        // 账户余额已耗尽（429），无法用实测反推。若你的账户计价与此判断不符，
        // 用 `SHANGYI_MODEL=<模型名>` 覆盖即可（一行环境变量）。
        // GLM 的思考档**不预设**：其可接受值域未实测（账户余额耗尽，探测无法完成），
        // 而它对不支持的档位是**报错**而非静默降级——预设一个错的会让它直接起不来。
        // 需要时用 SHANGYI_REASONING_EFFORT 显式指定。
        "glm" | "zhipu" => (Endpoint::glm(), "glm", "glm-5.3-flashx", None),
        "deepseek" => (Endpoint::deepseek(), "deepseek", "deepseek-flash", Some("high")),
        "magpie" => (
            Endpoint::magpie_local(),
            "magpie",
            "opencode-go/deepseek-v4.1-flash",
            None,
        ),
        other => {
            return Err(RunFailure::Credentials(format!(
                "未知的供应商 `{other}`。可用：deepseek（默认，走 Responses）、glm（走 Chat）、magpie（走本机 Magpie Gateway Chat）。"
            )));
        }
    };
    let key = if provider_name == "magpie" {
        load_magpie_gateway_key()
    } else {
        load_api_key(provider_name)
    }
    .ok_or_else(|| {
        RunFailure::Credentials(format!(
            "未找到 {} 凭据。请设置环境变量，或写入 \
             ~/.shangyi-secrets/{provider_name}.env（Magpie 可通过 MAGPIE_GATEWAY_KEY 或 ~/.config/magpie/caller-keys.json 自动发现）。\
             商奕不会在无凭据时退回模拟结果——那会产出看起来可信但无依据的结论。",
            provider_name.to_uppercase(),
        ))
    })?;

    let model = std::env::var("SHANGYI_MODEL").unwrap_or_else(|_| default_model.to_string());
    // 思考档：供应商默认 → 环境变量覆盖 → 空值表示"用供应商行为"
    let reasoning_effort = match std::env::var("SHANGYI_REASONING_EFFORT") {
        Ok(v) if v.trim().is_empty() => None, // 显式设空串 = 关掉默认档
        Ok(v) => Some(v.trim().to_string()),
        Err(_) => default_effort.map(str::to_string),
    };
    let wire = endpoint.wire;
    let client = LlmClient::new(endpoint, UreqTransport).with_key(key);

    // ── 引擎与执行器 ──
    let registry = ToolRegistry::standard();

    // ── 启动前自检（**在创建案例目录之前**）──
    //
    // 为什么必须在建目录之前：自检失败时不该留下一个空的案例目录——
    // 那会让「cases/ 下有个目录」变成「跑过一次」的假证据。
    if let Err(e) = preflight(&client, &model, &registry) {
        return Err(e);
    }

    let dir = case_dir(&case_id, &created_at)?;
    let header = CaseHeader {
        case_id: case_id.clone(),
        mode: mode.as_str().to_string(),
        playbook_id: playbook.meta.id.clone(),
        created_at: created_at.clone(),
        schema_version: CaseHeader::SCHEMA_VERSION,
    };
    let log = CaseLog::open(&dir, &header).map_err(|e| RunFailure::CaseDir {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;

    let theory = TheoryLibrary::new();
    // 加载知识库（R22 的 LLM-Wiki：页 + 链接图）
    let wiki = load_wiki(&theory);
    // 检索/抓取后端：抓取真实可用；检索后端按环境变量装配
    let search = LiveSearch::new().with_backends_from_env();
    if !search.has_backends() {
        println!("       搜索   = 没配（网页能读；搜资料会报「没查到」）");
    } else {
        println!("       搜索   = {:?}（几个后端一起用）", search.backend_names());
    }
    match &wiki {
        Some(w) => {
            let issues = w.issues().len();
            if issues == 0 {
                println!("       知识库 = {} 页（都连上了）", w.len());
            } else {
                // 有问题就说出来——藏起来会让"断链"被读成"没有后续"
                println!("       知识库 = {} 页（⚠ {} 处有问题）", w.len(), issues);
            }
        }
        None => println!("       知识库 = 没读（查知识库会报「没接上」）"),
    }
    // 绑定模式：`checkpoint` 的"该打开哪个检查点"依赖它
    // （cp1/cp2 三种模式一致，cp3 分别落在 select / consequence / discover）
    // ── persona actor 工厂（`swarm_run` 用）──
    //
    // 闭包在**每次调用**时构造一个新的 actor：`LlmPersonaActor` 持有 trace
    // （累计统计），一次会话里重复调用 `swarm_run` 时若复用同一个 actor，
    // 第二次的 trace 会混进第一次的失败计数 ⇒ "本次仿真有没有故障"就答不清了。
    //
    // 代价是每次新建（无状态开销，client 仍是借用）——这个取舍偏向可解释性。
    //
    // 注意返回的 actor 借用 `&client`：闭包捕获引用，故它不能活得比
    // `client` 久。二者都在本函数栈上，`executor` 用完即弃，借用关系成立。
    let swarm_actor_factory = LlmSwarmActorFactory::new(&client, model.clone());

    // 案例库路径（校准闭环的存储）。位置：`$SHANGYI_HOME/cases/library.json`
    //
    // ⚠️ 这一步**必须存在**：`CaseLibrary` 此前是纯内存的，
    // `case_store_write` 归档的案例进程退出即丢 ⇒ 案例库**永远积累不到**
    // 校准所需的 30 个样本 ⇒ 判据权重永远未校准。
    let library_path = shangyi_home()?.join("cases").join("library.json");
    // 周边数据源（可选）：没有 AMAP_KEY 就不接线，`poi_survey` 如实报未取得。
    let poi = AmapPoi::from_env();
    let mut executor = DomainExecutor::new(&theory, &playbook)
        .with_mode(mode)
        .with_search(&search)
        .with_poi_opt(poi.as_ref().map(|p| p as &dyn PoiProvider))
        .with_wiki(wiki.clone())
        .with_swarm_actor(Box::new(swarm_actor_factory))
        .with_case_library_path(library_path.clone())
        .map_err(|e| {
            RunFailure::CaseDir {
                path: library_path.display().to_string(),
                detail: format!(
                    "案例库加载失败：{e}。**不当作空库继续**——\
                     那会让后续归档覆盖掉仅有的样本（它们是几个月积累的、无法重建）。"
                ),
            }
        })?;

    let web_events = std::env::var("SHANGYI_WEB_EVENTS_PATH").ok();
    let brief = web_events.as_ref().and_then(|_| std::env::var("SHANGYI_WEB_BRIEF").ok());
    let assume_yes = std::env::var("SHANGYI_ASSUME_YES")
        .map(|v| !v.trim().is_empty() && v.trim() != "0")
        .unwrap_or(false);
    if let Some(raw)=brief.as_deref() {executor.record_user_input(raw,if assume_yes {"auto"} else {"user"});}
    // Only raw input enters the trusted ledger. Generated intake instructions do not.
    let machine = PhaseMachine::new(mode).require_task_scope_selection();
    let mut loop_config = LoopConfig::new(model.clone(), case_id.clone());
    // 思考量：默认不设（用供应商行为）。设 `SHANGYI_REASONING_EFFORT=none`
    // 可把思考 token 归零（实测输出降约 1/3 且工具调用照常），
    // 代价是分析质量可能下降——**故不默认开启**，由你按任务权衡。
    if let Some(e) = &reasoning_effort {
        println!("       想多深 = {e}");
        loop_config.reasoning_effort = Some(e.clone());
    }
    // `client` 克隆一份给 agent 循环：swarm actor 借用的是原件
    // （见 `LlmClient` 的 Clone 文档——克隆不复制连接或状态）。
    let mut agent =
        AgentLoop::new(client.clone(), &registry, &mut executor, loop_config).with_playbook(&playbook);
    let progress_path=std::env::var("SHANGYI_WEB_PROGRESS_PATH").ok();
    let scope_path=std::env::var("SHANGYI_WEB_EVENTS_PATH").ok();
    if progress_path.is_some() || scope_path.is_some() {
        let mut output=progress_path.map(|path|
            std::fs::OpenOptions::new().create(true).append(true).open(path)
        ).transpose().map_err(|e|RunFailure::Io(e.to_string()))?;
        let mut scope_output=scope_path.map(|path|
            std::fs::OpenOptions::new().create(true).append(true).open(path)
        ).transpose().map_err(|e|RunFailure::Io(e.to_string()))?;
        agent=agent.with_event_sink(Box::new(move |event| {
            let line=serde_json::to_string(event).expect("progress serialization failed")+"\n";
            if let Some(sink)=&mut output {
                std::io::Write::write_all(sink,line.as_bytes()).expect("progress write failed");
                std::io::Write::flush(sink).expect("progress flush failed");
            }
            if matches!(event,shangyi_core::Event::TaskScopeSelected {..}) {
                if let Some(sink)=&mut scope_output {
                    std::io::Write::write_all(sink,line.as_bytes()).expect("scope event write failed");
                    std::io::Write::flush(sink).expect("scope event flush failed");
                }
            }
        }));
    }
    let mut runner = SessionRunner::new(agent, log, machine);

    println!("商奕：开聊 {case_id}");
    println!("       问法   = {}", mode.as_str());
    println!("       方案   = {}", playbook.meta.name);
    println!("       供应商 = {provider_name}（wire = {}）", wire.as_str());
    println!("       模型   = {model}");
    println!("       记录放哪 = {}", dir.display());
    println!();
    // 显示"上次会话是否异常结束"——用户有权知道历史是否完整
    println!("这次干嘛：查资料、算钱，还是完整聊一轮；要完整分析时才请你核对。");

    // 非交互模式：`SHANGYI_ASSUME_YES=1` 时自动确认 checkpoint。
    //
    // # 为什么需要它（两个真实用途，都是产品需求而非测试便利）
    //
    // 1. **批处理/脚本化**：把商奕接进自动化流程时无人坐在终端前
    // 2. **自动化测试**：驱动一次完整咨询需要它——否则管道里的答案会落到
    //    确认提示上（实测：这是此前三次端到端尝试全部卡在 intake/intel 的原因）
    //
    // # 它是否削弱铁律 2
    //
    // 不削弱，但**必须可审计**：`SessionRunner` 把自动确认记成
    // `user_actor = "auto"`（不是 "user"），故"这份分析有没有人看过"
    // 在日志里是可回答的。这正是 `AutoApprove` 的设计意图。
    if assume_yes {
        println!("       核对   = 自动（都记成你点的）");
    }
    let initial_user_msg = build_intake_message(mode, action, &playbook, brief.as_deref());
    let input = vec![shangyi_llm::InputItem::user(initial_user_msg)];
    let report = if let Some(path) = web_events {
        let file = std::fs::OpenOptions::new().create(true).append(true).open(path)
            .map_err(|e| RunFailure::Io(e.to_string()))?;
        let mailbox = std::env::var("SHANGYI_WEB_MAILBOX_PATH").ok().map(PathBuf::from);
        let mut io = crate::web_io::WebIo::new(io::stdin().lock(), file, mailbox);
        if let Ok(path) = std::env::var("SHANGYI_WEB_RESPONSES_PATH") {
            let path=PathBuf::from(path);
            io=io.with_responses(path.clone());
            let folder=path.parent().ok_or_else(|| RunFailure::Io("response path has no parent".into()))?.to_path_buf();
            std::thread::spawn(move || loop {
                if folder.join("stop.request").exists() { std::process::exit(130); }
                let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock before epoch").as_secs_f64();
                let tmp=folder.join("heartbeat.tmp");
                std::fs::write(&tmp, now.to_string()).expect("heartbeat write failed");
                std::fs::rename(tmp,folder.join("heartbeat")).expect("heartbeat rename failed");
                std::thread::sleep(std::time::Duration::from_secs(1));
            });
        }
        io.emit(serde_json::json!({"type":"ready","case_id":case_id,
            "date":created_at.get(..10),"protocol":crate::web_io::PROTOCOL_VERSION}));
        let report = runner.run(input, &mut io, "user")
            .map_err(|e| RunFailure::Session(e.to_string()))?;
        io.emit(serde_json::json!({"type":"finished",
            "completed":report.is_finished(),
            "scope":runner.machine().task_scope(),
            "completion_kind":if matches!(&report,RunReport::TaskCompleted{result,..} if result.scope==shangyi_core::task_scope::TaskScope::Conditional){"conditional_analysis"}else if matches!(report,RunReport::TaskCompleted{..}){"scoped_task"}else{"consultation"},
            "phase":report.phase().as_str(),"turns":report.turns()}));
        report
    } else {
        let mut io = TerminalIo::new(assume_yes);
        runner.run(input, &mut io, "user")
            .map_err(|e| RunFailure::Session(e.to_string()))?
    };

    print_report(&report, runner.machine(), &case_id);
    Ok(())
}

/// 启动前自检：确认**这个模型真的能干活**。
///
/// # 为什么值得花一次极小的调用
///
/// 没有它时，配置错误会以**难以归因**的方式出现：
///
/// | 实际原因 | 表面现象 |
/// |---|---|
/// | 余额不足（429） | 会话跑一个 turn 后失败，错误里只有 "HTTP 429" |
/// | 模型名写错（404） | 同上 |
/// | **模型不支持工具调用** | 模型正常回一段文本 ⇒ 循环认为"turn 完成" ⇒ 但没有产物 ⇒ 闸门拦下 ⇒ 用户看到"会话已暂停，等待你提供信息"——**完全误导** |
///
/// 第三行是最坏的一种：它对模型的**能力缺陷**给出了一个看起来像
/// "需要你补充信息"的结论。而一次带工具的自检能在 2 秒内点明真正原因。
///
/// # 它检查什么
///
/// 1. 凭据可用、端点可达、模型名有效（HTTP 层）
/// 2. **模型确实会发起工具调用**（能力层）——这是 agent 的硬前提
///
/// 跳过：设 `SHANGYI_SKIP_PREFLIGHT=1`（例如离线开发或已知可用时）。
fn preflight<T: shangyi_llm::Transport>(
    client: &LlmClient<T>,
    model: &str,
    registry: &ToolRegistry,
) -> Result<(), RunFailure> {
    if std::env::var("SHANGYI_SKIP_PREFLIGHT").is_ok_and(|v| !v.trim().is_empty()) {
        println!("       启动检查 = 跳过了");
        return Ok(());
    }

    // 用注册表里第一个真实工具，而不是自造一个假工具——
    // 这样同时验证了"工具 schema 能被该端点接受"（两种 wire 的 schema 形态不同）
    let probe_tool = registry
        .tools_for_phase("intake")
        .into_iter()
        .next()
        .map(|t| {
            shangyi_llm::ToolSchema::function(
                &t.name,
                &t.description,
                serde_json::json!({"type":"object","properties":{},"additionalProperties":true}),
            )
        })
        .ok_or_else(|| RunFailure::Credentials("工具注册表为空，无法自检".into()))?;
    let probe_tool_name = probe_tool.name.clone();

    let req = shangyi_llm::LlmRequest::new(
        model,
        vec![shangyi_llm::InputItem::user(format!(
            "这是启动自检。请调用工具 `{probe_tool_name}`，参数为空对象。不要只回文字。"
        ))],
    )
    .with_tools(vec![probe_tool])
    // 输出预算必须**够思考 + 参数**。
    //
    // 实测教训：早先设 256，而思考档 `high` 下**光思考就用了 234 个 token**，
    // 参数只剩 22 个 —— 结果 `arguments` 被截成 `{"additionalProperties"`
    // （半截 JSON），自检报"JSON 解析失败"。
    // **那个诊断指向了错误的方向**：真正的原因是预算不足，不是解析问题。
    // 4096 给足了思考空间；自检本身仍是"一次极小的调用"（单 turn、单工具）。
    .with_max_tokens(4096);

    // 重试一次：**工具调用是概率性的**，不是确定性的。
    //
    // 实测：同一条自检提示在 `effort=none` 下连续 8 次里 7 次成功、1 次返回文字。
    // 若一次失败就判定"该模型不支持工具调用"，会**误杀一个可用模型**——
    // 而用户看到的是一句斩钉截铁的"该模型无法用于本系统"，无从怀疑。
    //
    // 故：失败时重试一次；两次都失败才判定。这是"减少误判"与"不掩盖缺陷"的平衡
    // ——真正的能力缺失会两次都失败。
    let started = std::time::Instant::now();
    let mut last_text = String::new();
    let mut ok = false;
    for attempt in 1..=2 {
        match client.complete(&req) {
            Ok(r) => {
                if !r.function_calls().is_empty() {
                    ok = true;
                    break;
                }
                last_text = r.text();
                if attempt == 1 {
                    println!("       启动检查 = 第 1 次没调工具，再试一次…");
                }
            }
            Err(e) => {
                return Err(credentials_error_from_api(&e.to_string(), model, attempt));
            }
        }
    }

    if !ok {
        return Err(RunFailure::Credentials(format!(
            "启动自检未通过（连续 2 次）：模型 `{model}` 收到带工具的请求后\
             **没有发起工具调用**，只回了文字。\n\
             商奕的 agent 循环依赖工具调用来产出结果，故该模型无法用于本系统。\n\
             常见原因：① 该模型不支持 function calling；② 它是一个「精简档」被关闭了工具能力；\
             ③ 思考量设置过低（`SHANGYI_REASONING_EFFORT=none` 会显著降低工具调用倾向）；\
             ④ 端点的工具 schema 形态不被接受。\n\
             请改用支持工具调用的模型（`SHANGYI_MODEL=<模型名>`），\
             或若你确认该模型可用，设 `SHANGYI_SKIP_PREFLIGHT=1` 跳过自检。\n\
             模型的文字回复（前 200 字）：{}",
            last_text.chars().take(200).collect::<String>()
        )));
    }

    println!(
        "       自检   = 通过（模型会发起工具调用，{} ms）",
        started.elapsed().as_millis()
    );
    Ok(())
}

/// 把 API 错误翻译成**可操作的**诊断。
///
/// 抽出来是因为它现在有多个调用点（自检的两次尝试），而"从 HTTP 码猜病因"
/// 的逻辑散在多处时，迟早有一处漏掉某个码。
fn credentials_error_from_api(msg: &str, model: &str, attempt: u32) -> RunFailure {
    let hint = if msg.contains("429") || msg.contains("余额") || msg.contains("quota") {
        "看起来是**账户余额或配额**问题。请到供应商控制台充值/确认资源包；\
         换更便宜的模型也许能绕开（`SHANGYI_MODEL=<模型名>`）。"
    } else if msg.contains("401") || msg.contains("403") {
        "看起来是**凭据**问题。请确认 API key 有效且未过期。"
    } else if msg.contains("404") {
        "看起来是**模型名或端点路径**问题。用该端点的 `/models` 接口确认可用模型名，\
         再用 `SHANGYI_MODEL=<模型名>` 指定。"
    } else if msg.contains("400") {
        "端点拒绝了请求体（HTTP 400）。常见原因：某字段形态不被接受。\
         可设 `SHANGYI_DUMP_REQUEST=<路径>` 把请求体落盘以逐字段比对。"
    } else {
        "请检查网络、端点地址与凭据。"
    };
    RunFailure::Credentials(format!(
        "启动自检失败（模型 `{model}`，第 {attempt} 次尝试）。{hint}\n原始错误：{}",
        msg.chars().take(300).collect::<String>()
    ))
}

/// 加载知识库。
///
/// 查找顺序：`$SHANGYI_KB_DIR` → `$SHANGYI_HOME/kb` → 仓库内 `shangyi/kb`。
///
/// # 为什么加载失败时返回 `None` 而不是报错退出
///
/// 知识库缺失**不构成启动失败**：一次咨询的核心流程（求解、选择、报告）
/// 不依赖它。但它也不该被静默忽略——故加载结果（含失败原因）会打印出来，
/// 且 `kb_retrieve` 会明确区分「未加载」与「库里没有」。
///
/// 这与「检索后端缺失」的处置一致：**缺一个可选能力不等于流程不能跑，
/// 但绝不能让"缺"看起来像"没有"**。
fn load_wiki(fallback: &TheoryLibrary) -> Option<std::sync::Arc<shangyi_kb::wiki::Wiki>> {
    let _ = fallback;
    crate::kb_cmd::load_combined_wiki_for_runtime()
}

fn date_compact(iso: &str) -> String {
    iso.chars().filter(|c| c.is_ascii_digit()).take(8).collect()
}

/// 从 ISO8601 时间戳取 `HHMMSS`——用作 case_id 的**唯一性后缀**。
///
/// 见 `start_consultation` 里那段注释：不加它，同一天的重复运行会撞同一个
/// 案例目录，日志交叠且崩溃修复互相干扰。
///
/// 取不到时分秒时返回 `000000`：那会让 case_id 退化成"按日期唯一"，
/// **但仍然可用**（不是 panic，也不是空串——空串会让所有案例_id 变成
/// `case-20260918-scenario-`，那是更坏的静默合并）。
fn seq_from_clock(iso: &str) -> String {
    let digits: String = iso.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 14 {
        digits[8..14].to_string()
    } else {
        "000000".to_string()
    }
}

/// 构造 intake 的初始用户消息。
fn build_intake_message(mode: ConsultationMode, action: Option<&str>, pb: &Playbook, brief: Option<&str>) -> String {
    let mut s = String::new();
    s.push_str(&format!("请处理用户这次的经营咨询。当前流程配置：{}；以用户描述的范围为准。\n", mode.as_str()));
    if !pb.meta.description.is_empty() {
        s.push_str(&format!("剧本参考（仅供查漏，不扩大任务）：{}\n", pb.meta.description));
    }
    if let Some(a) = action {
        s.push_str(&format!("用户指定的决策动作：{a}\n"));
    }
    if let Some(brief) = brief.filter(|s| !s.trim().is_empty()) {
        s.push_str("\n交互方式：用户已经自由描述背景。先结合全部上下文理解问题，简短复述关键理解。\
            剧本中的 intake 清单仅供查漏，不是必须逐项填写的问卷；已提供的信息不得重复提问。\
            只用 ask_user 追问会改变判断的最关键缺口，每轮优先一个、最多两个问题，解释为何需要。\
            允许用户一次回答多个问题、补充背景、纠正先前说法或表示不知道；不要要求按题号回答。\
            用户补充或纠正后，先更新理解，未回答的信息保持未取得，不能作为确认过的假设。\
            信息足够时继续分析；事实、硬假设和结果确认仍须通过 checkpoint，不得自动确认。\n");
        s.push_str("\n用户的原始描述：\n");
        s.push_str(brief);
    } else if !pb.intake.is_empty() {
        s.push_str("\n剧本的 intake 清单仅供查漏，不要逐项向用户提问。先了解用户要做的决策与最小检索定位信息。\n");
    }
    s.push_str(
        "\n注意：计算结果必须来自确定性工具，不得自行估算后声称已经验证；用户提供的数字和公开原文数值如实引用并注明来源。\
         本次目标需要核验且用户未排除的公开事实缺失时，先用 `web_search` 主动搜索，再用 `web_fetch` 核对原文，\
         保留来源、日期与适用范围，不要询问用户能够公开查到的信息。\
         只有检索主体或地区无法确定时，先问最小定位信息。\
         `ask_user` 只问无法公开取得且会改变决策的内部信息、目标、偏好或约束，\
         每轮优先一个、最多两个问题，不把多个信息点塞进一道题；已回答或表示不知道的内容不重复问。\
         检索失败如实标为未取得，不得用常识补，也不得把全部搜索缺口变成问卷。\
         非关键缺口保留未知继续分析；关键缺口才说明影响并询问用户是否掌握。\
         搜索得到的事实不等于用户已确认的建模假设，必要的 checkpoint 仍须保留。",
    );
    // 按用户意图适用，不能仅依赖剧本 ID：开店也可能被路由到竞争研判或调价。
    s.push_str(include_str!("../../../playbooks/store_location_research.md"));
    s
}

/// 从磁盘加载剧本。
///
/// 查找顺序：`$SHANGYI_HOME/playbooks/<id>/playbook.yml` →
/// 仓库内 `shangyi/playbooks/<id>/playbook.yml`。
/// 都找不到 ⇒ **报错**（不构造一个"占位剧本"——那会让运行结果无法解释）。
fn load_playbook(id: Option<&str>) -> Result<Playbook, RunFailure> {
    let id = id.unwrap_or("pricing_response");
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(home) = shangyi_home() {
        candidates.push(home.join("playbooks").join(id).join("playbook.yml"));
    }
    // 开发时从仓库根运行：相对路径候选
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../playbooks")
            .join(id)
            .join("playbook.yml"),
    );

    for p in &candidates {
        if let Ok(text) = std::fs::read_to_string(p) {
            let pb = Playbook::from_yaml(&text).map_err(|e| {
                RunFailure::Playbook(format!("{} 解析失败：{e}", p.display()))
            })?;
            pb.validate().map_err(|e| {
                RunFailure::Playbook(format!("{} 校验失败：{e}", p.display()))
            })?;
            return Ok(pb);
        }
    }
    Err(RunFailure::Playbook(format!(
        "找不到剧本 `{id}`。已查找：{}。\
         请把剧本放在 $SHANGYI_HOME/playbooks/{id}/playbook.yml。",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("、")
    )))
}

fn format_report_lines(report: &RunReport, machine: &PhaseMachine, case_id: &str) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(String::new());
    lines.push("─── 现在啥情况 ───".to_string());
    let p = progress(machine);
    lines.push(format!("哪一步：{}（{}）", p.phase.as_str(), phase_label(p.phase)));
    lines.push(format!(
        "产出了：{}",
        if p.missing_artifacts.is_empty() {
            "齐备".to_string()
        } else {
            format!(
                "缺 {}",
                p.missing_artifacts
                    .iter()
                    .map(|k| k.key())
                    .collect::<Vec<_>>()
                    .join("、")
            )
        }
    ));
    if let Some(cp) = p.pending_checkpoint {
        lines.push(format!("核对点：{} — {}", cp.as_str(), p.checkpoint_state));
    }
    lines.push(format!("为啥往前走：{}", p.reason));

    lines.push(String::new());
    match report {
        RunReport::TaskCompleted { result, .. } => {
            lines.push(format!(
                "✓ 这次的{}交了（{} 轮，{}）。",
                result.scope.as_str(),
                report.turns(),
                if result.is_complete() {
                    "要的资料和钱都齐了"
                } else {
                    "还有资料或金额没到手"
                }
            ));
            lines.push("只对这次要求的范围；不代表完整咨询做完了。".to_string());
        }
        RunReport::Completed { .. } => {
            lines.push(format!("✓ 全聊完了（{} 轮）。", report.turns()));
        }
        RunReport::AwaitingUser { checkpoint, .. } => {
            lines.push(format!(
                "⏸ 停着等你核对 {}（{} 轮）。",
                checkpoint.as_str(),
                report.turns()
            ));
            lines.push(format!("   用 `shangyi resume {case_id}` 继续。"));
        }
        RunReport::AwaitingAnswers { .. } => {
            lines.push(format!("⏸ 停着等你补充（{} 轮）。", report.turns()));
            lines.push("   没答的记「没问到」——不猜、不编。".to_string());
            lines.push(format!("   用 `shangyi resume {case_id}` 继续。"));
        }
        RunReport::Blocked { detail, .. } => {
            lines.push(format!("⛔ 外部东西用不了，先停了（{} 轮）。", report.turns()));
            lines.push(format!("   原因：{detail}"));
            lines.push("   这不是结论——查不到的记「没查到」，不当成「没有」。".to_string());
            lines.push(format!("   等能用了，`shangyi resume {case_id}` 接着来。"));
        }
        RunReport::Failed { detail, .. } => {
            lines.push(format!("✗ 这轮没成（{} 轮）。", report.turns()));
            lines.push(format!("   {detail}"));
        }
        RunReport::Aborted { .. } => {
            lines.push(format!(
                "■ 被叫停了（{} 轮）。做完的还在日志里。",
                report.turns()
            ));
        }
        RunReport::StageBudgetExhausted {
            phase,
            turns_in_stage,
            ..
        } => {
            lines.push(format!(
                "⚠ 在“{}”这一步用满了 {} 轮，还没往前走。",
                phase.as_str(),
                turns_in_stage
            ));
            lines.push("   不算失败——是「不再连环问」。".to_string());
            lines.push(format!(
                "   补点信息，`shangyi resume {case_id}` 接着来（重新计数）。"
            ));
        }
        RunReport::BudgetExhausted { .. } => {
            lines.push(format!(
                "⚠ 到轮数上限了，还没往前走（{} 轮）。",
                report.turns()
            ));
            lines.push("   多半是该产出的没产出。日志在，能查。".to_string());
        }
    }
    lines
}

/// 打印运行结局。
///
/// **不把非完成说成完成**：挂起/阻塞/失败各自有明确措辞与"下一步"。
fn print_report(report: &RunReport, machine: &PhaseMachine, case_id: &str) {
    for line in format_report_lines(report, machine, case_id) {
        println!("{line}");
    }
}

/// 恢复一次会话（读日志 + 崩溃修复 + 打印状态）。
pub fn resume_consultation(case: &str) -> Result<(), RunFailure> {
    let trimmed = case.trim();
    if trimmed.is_empty() {
        return Err(RunFailure::Session("案例标识不能为空".into()));
    }
    let home = shangyi_home()?;
    let cases = home.join("cases");
    let dir = find_case_dir_in(&cases, trimmed).ok_or_else(|| RunFailure::CaseDir {
        path: cases.display().to_string(),
        detail: format!("找不到案例 `{trimmed}` 的目录（支持案例 ID 或 `latest`）"),
    })?;

    let outcome = shangyi_core::case_log::resume(&dir).map_err(|e| RunFailure::CaseDir {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;

    let resolved_id = std::fs::read_to_string(dir.join("header.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<CaseHeader>(&s).ok())
        .map(|h| h.case_id)
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| trimmed.to_string());

    println!("商奕：接着聊 {resolved_id}");
    println!("       目录 = {}", dir.display());
    println!("       事件 = {} 条", outcome.events.len());
    if outcome.crashed() {
        println!(
            "       ⚠ 上次会话**异常结束**（未闭合的 turn）：已算出 {} 条中断收尾事件。",
            outcome.repairs.len()
        );
        println!("         这轮的结论会标「断了」——不算一次完整分析。");
    } else {
        println!("       上次会话正常结束。");
    }
    // 结局如实打印
    if let Some(last) = outcome.events.last() {
        println!("       最后事件 = {}", event_label(last));
    }
    println!();
    println!("（接着跑要接模型和你互动；现在只恢复和如实报告。）");
    Ok(())
}

/// 给事件一个可读的标签。
fn event_label(e: &shangyi_core::session::Event) -> &'static str {
    {
        use shangyi_core::session::Event::*;
        match e {
            TaskScopeSelected { .. } => "task_scope_selected",
            ScopedTaskCompleted { .. } => "scoped_task_completed",
            FollowupTaskStarted { .. } => "followup_task_started",
            UserInputRecorded { .. } => "user_input_recorded",
            TurnStarted { .. } => "turn_started",
            TurnEnded { .. } => "turn_ended",
            StepStarted { .. } => "step_started",
            StepEnded { .. } => "step_ended",
            ToolCallStarted { .. } => "tool_call_started",
            ToolCallFinished { .. } => "tool_call_finished",
            TokenUsage { .. } => "token_usage",
            Warning { .. } => "warning",
            PhaseEntered { .. } => "phase_entered",
            PhaseApproved { .. } => "phase_approved",
            PhaseRewound { .. } => "phase_rewound",
            PlaybookSelected { .. } => "playbook_selected",
            CheckpointOpened { .. } => "checkpoint_opened",
            CheckpointAnswered { .. } => "checkpoint_answered",
        }
    }
}

/// 保留：`RunError` 的使用点（避免它只在类型签名里出现）。
pub fn describe_run_error(e: &RunError) -> String {
    e.to_string()
}

/// 保留：`CheckpointId` 使用点。
pub fn cp_label(id: CheckpointId) -> &'static str {
    id.as_str()
}

/// 让 stdout 及时刷新（交互提示不能因为缓冲而看不见）。
pub fn flush_stdout() {
    let _ = io::stdout().flush();
}

#[cfg(test)]
mod tests {
    /// 装配逻辑：有凭据就装博查，没有就**空着**（不许悄悄兜底成别的后端）。
    ///
    /// SearXNG 删除后这里只剩一个后端，故这条测试同时钉住"删除是彻底的"：
    /// 装配结果只由本事决定，不受 `SHANGYI_SEARXNG_URL` 之类残留环境影响。
    #[test]
    fn backend_assembly_is_bocha_only_and_never_falls_back_silently() {
        let none = super::backends_from(None);
        assert!(none.is_empty(), "没有凭据时不得装配任何后端");
        let some = super::backends_from(Some("sk-test-not-used".into()));
        assert_eq!(some.len(), 1, "有凭据时只装配一个后端");
        assert_eq!(some[0].name(), "bocha");
    }

    use super::*;

    #[test]
    fn api_key_format_is_derived_from_provider() {
        // 只验证命名规则（不读真实凭据）
        assert_eq!(format!("{}_API_KEY", "deepseek".to_uppercase()), "DEEPSEEK_API_KEY");
    }

    #[test]
    fn date_compact_takes_eight_digits() {
        assert_eq!(date_compact("2026-09-17T08:30:00Z"), "20260917");
        assert_eq!(date_compact("unknown"), "");
    }

    /// case_id 的唯一性后缀：同一天的不同运行必须得到**不同**的 case_id。
    ///
    /// 这条测试的来历是一次真实事故：run20 与 run21 写进了同一个
    /// case.jsonl，两次咨询的记录混成一条时间线（详见 `seq_from_clock` 注释）。
    #[test]
    fn seq_from_clock_separates_runs_on_the_same_day() {
        // 同一天、不同时刻 ⇒ 必须不同
        let a = seq_from_clock("2026-09-17T08:30:00Z");
        let b = seq_from_clock("2026-09-17T19:45:12Z");
        assert_eq!(a, "083000");
        assert_eq!(b, "194512");
        assert_ne!(a, b, "同一日期分片下的两次运行必须可区分");

        // 取不到时分秒时退化为固定值——**不是 panic，也不是空串**。
        // 空串会让所有 case_id 变成 `case-20260917-scenario-`，
        // 那是比"按日期唯一"更坏的静默合并。
        assert_eq!(seq_from_clock("2026-09-17"), "000000");
        assert_eq!(seq_from_clock("unknown"), "000000");
    }

    #[test]
    fn case_dir_uses_date_sharding() {
        // 依赖 $SHANGYI_HOME 或 $HOME；测试里显式设前者不存在时跳过
        if std::env::var("SHANGYI_HOME").is_err() && std::env::var("HOME").is_err() {
            return;
        }
        let d = case_dir("case-x", "2026-09-17T00:00:00Z").expect("应可定位");
        let s = d.display().to_string();
        assert!(s.contains("2026-09-17"), "应按日期分片：{s}");
        assert!(s.ends_with("case-x"));
    }

    /// 缺剧本必须**报错**，不得构造占位剧本——占位剧本会让运行结果无法解释。
    #[test]
    fn missing_playbook_is_an_error_not_a_placeholder() {
        match load_playbook(Some("绝不存在的剧本xyz")) {
            Err(RunFailure::Playbook(msg)) => {
                assert!(msg.contains("找不到剧本"), "实得：{msg}");
                assert!(msg.contains("playbooks"), "应告诉用户放哪里：{msg}");
            }
            Err(e) => panic!("期望 Playbook 错误，实得 {e:?}"),
            Ok(_) => panic!("不得为不存在的剧本构造占位"),
        }
    }

    #[test]
    fn print_report_hints_actual_case_id_instead_of_mode() {
        let machine = PhaseMachine::new(ConsultationMode::Scenario);
        let case_id = "case-20261114-scenario-123456";
        for rep in [
            RunReport::AwaitingUser {
                phase: SessionPhase::ModelBuild,
                checkpoint: CheckpointId::Cp2,
                turns: 2,
            },
            RunReport::AwaitingAnswers {
                phase: SessionPhase::Intake,
                turns: 1,
            },
            RunReport::Blocked {
                phase: SessionPhase::Intel,
                detail: "无搜索密钥".into(),
                turns: 1,
            },
            RunReport::StageBudgetExhausted {
                phase: SessionPhase::Intake,
                turns_in_stage: 4,
                turns: 4,
            },
        ] {
            let joined = format_report_lines(&rep, &machine, case_id).join("\n");
            assert!(
                joined.contains(&format!("shangyi resume {case_id}")),
                "应提示具体 case_id：{joined}"
            );
            assert!(
                !joined.contains("`shangyi resume scenario`"),
                "不得把 mode 当成 case_id：{joined}"
            );
        }
    }

    #[test]
    fn find_case_dir_in_resolves_latest_deterministically() {
        let root = std::env::temp_dir().join(format!(
            "shangyi-cli-latest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let c1 = root.join("2026-11-14/case-20261114-scenario-010000");
        let c2 = root.join("2026-11-14/case-20261114-scenario-020000");
        std::fs::create_dir_all(&c1).unwrap();
        std::fs::create_dir_all(&c2).unwrap();
        std::fs::write(c1.join("case.jsonl"), "{}\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(c2.join("case.jsonl"), "{}\n").unwrap();

        assert_eq!(find_case_dir_in(&root, "latest"), Some(c2.clone()));
        assert_eq!(
            find_case_dir_in(&root, "case-20261114-scenario-010000"),
            Some(c1)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

}


/// 测试支持：按环境变量装配检索后端。
///
/// 单独放在这里而不是让测试自己拼：**装配逻辑只应有一处**。
/// 若测试自己拼一个 provider，它验证的就是"测试拼的那个能跑"，
/// 而不是"产品用的那个能跑"——那是两种不同的断言。
pub fn search_provider_from_env() -> LiveSearch {
    LiveSearch::new().with_backends_from_env()
}

/// `shangyi backtest`：**校准判据权重**——回测闭环的最后一环。
///
/// # 为什么这个命令必须真的做事（实测审计发现它是占位符）
///
/// 它此前只打印 `[已解析] 回测校准：全部场景` 就退出，而
/// `Command::is_mutating()` 却声称它"更新判据权重"——**声明与实现不符**。
///
/// 而它是校准闭环的**唯一入口**。缺了它，整条闭环（§7.2）不成立：
/// 案例库积累再多样本也换不来更好的权重。
///
/// # 四步
///
/// 1. 加载案例库
/// 2. 把**已回填结果**的案例转成校准记录（`backtest_records`）
/// 3. 样本够（≥ `MIN_SAMPLES_FOR_CALIBRATION`）才校准；不够就**如实报告**
/// 4. 写回新的权重
///
/// 第 3 步的"不够就不校准"是刻意的（`shangyi-selection::calibrate`
/// 的文档说明了理由）：**不假装"越用越准"**——样本不足时的任何权重调整
/// 都是噪声拟合，而报告会把它当成"已校准"。
pub fn run_backtest(mode: Option<ConsultationMode>) -> Result<(), RunFailure> {
    let home = shangyi_home()?;
    let path = home.join("cases").join("library.json");

    let lib = shangyi_kb::CaseLibrary::load(&path).map_err(|e| RunFailure::CaseDir {
        path: path.display().to_string(),
        detail: format!("{e}"),
    })?;

    let total = lib.len();
    let calibratable = lib.calibratable().len();
    let excluded = lib.excluded_from_calibration();
    println!("商奕：回测校准");
    println!("       案例库 = {}", path.display());
    println!("       案例总数 = {total}，其中**可参与校准**（已回填且来源为真实委托）= {calibratable}");
    if excluded > 0 {
        // **必须说出来**：静默排除会让"只有 0 条可校准"看起来像是库是空的，
        // 而真相是"有 N 条演练案例回填了结果，但它们不算数"。
        println!(
            "       ⚠ 另有 {excluded} 条已回填结果、但因**来源是演练/未标注**而被排除——\
             演练案例的结果是假设情景下的产物，用它调权重会产出「用假数据拟合的可信度」。"
        );
    }

    let (records, skipped) = lib.backtest_records();
    if skipped > 0 {
        // 跳过**如实报告**——静默跳过会让"样本不够"看起来像"没有案例"
        println!("       ⚠ 跳过 {skipped} 条（没填结果，或形状不对）");
    }

    let need = shangyi_selection::MIN_SAMPLES_FOR_CALIBRATION;
    if records.len() < need {
        // **不假装越用越准**：样本不足时不做任何调整，并说清还差多少
        println!();
        println!(
            "样本不足（{}/{}），**不做校准**。",
            records.len(),
            need
        );
        println!(
            "       理由：样本不足时的权重调整是**噪声拟合**，\
             而报告会把调整后的权重当成「已校准」。"
        );
        println!(
            "       下一步：用 `shangyi new` 继续积累案例，\
             事后用 `case_store_write` 的 `outcome` 回填实际结果。"
        );
        return Ok(());
    }

    // 基准权重：从剧本读（未校准时是剧本声明的先验）
    let playbook = load_playbook(None)?;
    let weights = shangyi_selection::SelectorWeights::uniform(
        &playbook
            .selectors
            .keys()
            .filter_map(|k| {
                shangyi_selection::SelectorId::ALL
                    .iter()
                    .find(|s| s.as_str() == k.as_str())
                    .copied()
            })
            .collect::<Vec<_>>(),
    );

    match shangyi_selection::calibrate(&weights, &records) {
        Ok(outcome) => {
            println!();
            println!("校准完了（用了 {} 个）", outcome.samples);
            println!(
                "       对数损失：{:.4} → {:.4}",
                outcome.mean_log_loss_before, outcome.mean_log_loss_after
            );
            let mut changed = 0usize;
            for (id, (before, after)) in &outcome.weight_deltas {
                if (before - after).abs() > 1e-9 {
                    println!("       {} : {:.4} → {:.4}", id.as_str(), before, after);
                    changed += 1;
                }
            }
            if changed == 0 {
                println!("       （无权重变化）");
            }
            println!();
            println!(
                "⚠ 新权重**未自动写回剧本**——那是刻意的：\
                 剧本是可审计的声明文件，应由人审阅后修改。"
            );
        }
        Err(e) => {
            return Err(RunFailure::Session(format!("校准失败：{e}")));
        }
    }
    Ok(())
}

/// `shangyi solve`：离线确定性求解（无会话、无 LLM、不写文件）。
///
/// 机器接口契约（供 Web 控制台 / 脚本调用）：
/// - 输入：GameSpec JSON **文件**（路径经 `--in` 给出）；
/// - 输出：解集 pretty JSON 打到 **stdout**（与工具 `solve_equilibria`
///   的产物完全同源——同一段 [`shangyi_core::domain_exec::solve_spec_to_json`]）；
/// - 失败：中文错误信息打到 **stderr**，退出码非零，stdout 无输出。
///
/// 校验在求解前显式执行（`validate()`）：`solve_equilibria` 在会话里
/// 依赖上游 `gamespec_build` 已保证这一点，而本入口读的是任意文件——
/// 「未确认假设不得进入求解」（铁律 2 的文件版）在这里同样成立：
/// 结构非法的 spec 要在进入求解器之前被拒。
pub fn run_solve(
    input: &str,
    method: Option<&str>,
    refine: Option<&str>,
    refine_denominator: Option<i64>,
) -> Result<(), RunFailure> {
    let text = std::fs::read_to_string(input)
        .map_err(|e| RunFailure::Solve(format!("无法读取 GameSpec 文件 `{input}`：{e}")))?;
    let spec: shangyi_gamespec::GameSpec = serde_json::from_str(&text)
        .map_err(|e| RunFailure::Solve(format!("GameSpec JSON 解析失败：{e}")))?;
    if let Err(errs) = spec.validate() {
        let detail = errs
            .iter()
            .map(|e| format!("  - {e}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(RunFailure::Solve(format!(
            "GameSpec 未通过校验（{} 处）：\n{detail}",
            errs.len()
        )));
    }

    // 与工具参数同构的 args（只放显式给出的键，保持默认值语义一致）
    let mut args = serde_json::json!({});
    if let Some(m) = method {
        args["method"] = serde_json::json!(m);
    }
    if let Some(r) = refine {
        args["refine"] = serde_json::json!(r);
    }
    if let Some(d) = refine_denominator {
        args["refine_denominator"] = serde_json::json!(d);
    }

    let json = shangyi_core::domain_exec::solve_spec_to_json(&spec, &args)
        .map_err(RunFailure::Solve)?;
    println!("{json}");
    Ok(())
}

/// `shangyi checkpoint <case|latest> --approve|--reject [--note <备注>] [--home <目录>]`：
/// 由真人通过命令行对挂起中的关口落盘作答（`user_actor = "user"`）。
pub fn settle_case_checkpoint(
    case: &str,
    approve: bool,
    note: Option<&str>,
    home_override: Option<&Path>,
) -> Result<(), RunFailure> {
    let home = match home_override {
        Some(p) => p.to_path_buf(),
        None => shangyi_home()?,
    };
    let cases_root = home.join("cases");
    let dir = find_case_dir_in(&cases_root, case).ok_or_else(|| RunFailure::CaseDir {
        path: cases_root.display().to_string(),
        detail: format!("找不到案例 `{case}` 的目录"),
    })?;
    let header_path = dir.join("header.json");
    let header_text =
        std::fs::read_to_string(&header_path).map_err(|e| RunFailure::CaseDir {
            path: header_path.display().to_string(),
            detail: format!("读取 header.json 失败：{e}"),
        })?;
    let header: CaseHeader =
        serde_json::from_str(&header_text).map_err(|e| RunFailure::CaseDir {
            path: header_path.display().to_string(),
            detail: format!("解析 header.json 失败：{e}"),
        })?;

    let mut log = CaseLog::open(&dir, &header).map_err(|e| RunFailure::CaseDir {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;
    let events = log.events().map_err(|e| RunFailure::CaseDir {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;

    let mut open_cp: Option<String> = None;
    for ev in &events {
        match ev {
            shangyi_core::Event::CheckpointOpened { id, .. } => open_cp = Some(id.clone()),
            shangyi_core::Event::CheckpointAnswered { id, answers, .. } => {
                let first = answers.first().map(String::as_str).unwrap_or("");
                if !first.starts_with("追问：") && open_cp.as_deref() == Some(id.as_str()) {
                    open_cp = None;
                }
            }
            _ => {}
        }
    }
    let Some(cp_id) = open_cp else {
        return Err(RunFailure::Session(format!(
            "案例 `{}` 当前没有处于待确认状态的关口（checkpoint）",
            header.case_id
        )));
    };

    let at = SystemClock.now_iso8601();
    let trimmed_note = note.map(str::trim).filter(|s| !s.is_empty());
    if let Some(n) = trimmed_note {
        log.append_durable(shangyi_core::Event::UserInputRecorded {
            text: n.to_string(),
            user_actor: "user".to_string(),
            at: at.clone(),
        })
        .map_err(|e| RunFailure::CaseDir {
            path: dir.display().to_string(),
            detail: e.to_string(),
        })?;
    }

    let answer_line = if approve {
        match trimmed_note {
            Some(n) => format!("确认：{n}"),
            None => "确认".to_string(),
        }
    } else {
        match trimmed_note {
            Some(n) => format!("修改：{n}"),
            None => "修改：用户要求修订当前阶段内容".to_string(),
        }
    };

    log.append_durable(shangyi_core::Event::CheckpointAnswered {
        id: cp_id.clone(),
        answers: vec![answer_line],
        user_actor: "user".to_string(),
        at,
    })
    .map_err(|e| RunFailure::CaseDir {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;

    println!(
        "✓ 已记录真人{}：案例={}，关口={}（actor=user）",
        if approve { "核准通过" } else { "驳回修订" },
        header.case_id,
        cp_id
    );
    Ok(())
}

pub(crate) fn find_case_dir_in(cases_root: &Path, case_ref: &str) -> Option<PathBuf> {
    if case_ref == "latest" {
        if let Ok(active_text) = std::fs::read_to_string(cases_root.join("active.json")) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&active_text) {
                if let Some(dir_str) = val.get("case_dir").and_then(|v| v.as_str()) {
                    let cand = PathBuf::from(dir_str);
                    if cand.join("case.jsonl").is_file() {
                        return Some(cand);
                    }
                }
            }
        }
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        let days = std::fs::read_dir(cases_root).ok()?;
        for day in days.flatten() {
            if !day.path().is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(day.path()) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                let log_path = dir.join("case.jsonl");
                if let Ok(meta) = std::fs::metadata(&log_path) {
                    if meta.len() > 0 {
                        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        if best
                            .as_ref()
                            .is_none_or(|(prev_t, prev_d)| (mtime, &dir) > (*prev_t, prev_d))
                        {
                            best = Some((mtime, dir));
                        }
                    }
                }
            }
        }
        return best.map(|(_, d)| d);
    }

    let days = std::fs::read_dir(cases_root).ok()?;
    for day in days.flatten() {
        if !day.path().is_dir() {
            continue;
        }
        let cand = day.path().join(case_ref);
        if cand.join("case.jsonl").is_file() || cand.join("header.json").is_file() {
            return Some(cand);
        }
    }
    None
}

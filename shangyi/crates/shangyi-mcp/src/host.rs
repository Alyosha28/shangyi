//! 会话宿主：**一个进程 = 一次咨询会话**（或通过 `--resume` 续接既有会话）。
//!
//! # 这个模块在整条链上的位置
//!
//! ```text
//! dsh ──JSON-RPC──▶ server ──▶ host ──▶ ToolRegistry::check_call（阶段/剧本白名单）
//!                               │              │
//!                               │              ├─▶ DomainExecutor::execute（唯一执行 seam）
//!                               │              ├─▶ PhaseMachine（产物闸门 / checkpoint）
//!                               │              └─▶ CaseLog（留档 & 外部确认同步）
//!                               ▼
//!                        MCP tools/call 结果（isError + content）
//! ```
//!
//! # 双通道关口确认与可信用户输入账本
//!
//! 1. **工作台 / CLI 外部确认通道**：用户在 Web 工作台（`POST /api/shangyi/checkpoint`）
//!    或命令行（`shangyi checkpoint <case_id> --approve|--reject`）确认后，向当前案例
//!    `case.jsonl` 追加 `CheckpointAnswered`（`user_actor = "user"`）。每次 `tools/call`
//!    前置执行 `sync_external_inputs()`，自动结算阶段机并推进阶段。
//! 2. **对话确认通道**：DSH 宿主插件监听真实 `user/message` 事件写入
//!    `$SHANGYI_HOME/runtime/trusted_user_inputs.jsonl`；用户在当前关口打开**之后**回复确认时，
//!    模型可调用 `checkpoint(decision="approve"|"reject", user_quote="...")`，由服务端核验
//!    `user_quote` 确系当前关口打开后的真实用户消息子串后以 `actor = "user"` 结算。
//!    严禁在无关口后真实用户消息时自行代批。
//! 3. **惰性建案（Lazy `CaseLog`）**：仅握手/列工具（如 DSH 探测进程）不落盘空案例目录；
//!    首次实际执行 `tools/call` 时才创建 `<case_dir>/header.json` 与 `case.jsonl`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use shangyi_core::case_log::CaseHeader;
use shangyi_core::clock::Clock;
use shangyi_core::domain_exec::PoiProvider;
use shangyi_core::providers::{LiveSearch, AmapPoi, poi_for_mcp, search_for_mcp, swarm_for_mcp};
use shangyi_core::task_scope::TaskScope;
use shangyi_core::{
    CaseLog, CheckpointId, CheckpointItem, CheckpointRequest, CheckpointStatus, ConsultationMode,
    DomainExecutor, Event, ExecOutcome, SessionPhase, StepId, SystemClock, ToolCall, ToolExecutor,
    ToolOutcome, TurnId,
};
use shangyi_kb::TheoryLibrary;
use shangyi_playbooks::Playbook;
use shangyi_tools::ToolRegistry;

/// 默认剧本 id（与 `shangyi new` 一致：`pricing_response`）。
pub const DEFAULT_PLAYBOOK_ID: &str = "pricing_response";

/// 启动参数。
#[derive(Debug, Clone, PartialEq)]
pub struct HostConfig {
    /// 咨询模式（缺省与 `shangyi new` 一致：scenario）。
    pub mode: ConsultationMode,
    /// 剧本 id；`None` ⇒ [`DEFAULT_PLAYBOOK_ID`]。
    pub playbook_id: Option<String>,
    /// 商奕主目录（案例留档落在这里）。`None` ⇒ `$SHANGYI_HOME` → `$HOME/.shangyi`。
    pub home: Option<PathBuf>,
    /// 自动结算 checkpoint（与 `SHANGYI_ASSUME_YES=1` 同源，日志记 actor=auto）。
    pub auto_approve_checkpoints: bool,
    /// 续接既有案例（`case_id` 或 `"latest"`）。`None` ⇒ 新建会话。
    pub resume_case: Option<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            mode: ConsultationMode::Scenario,
            playbook_id: None,
            home: None,
            auto_approve_checkpoints: false,
            resume_case: None,
        }
    }
}

/// 宿主启动失败。**启动失败即拒绝服务**——不带病进会话。
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(
        "找不到剧本 `{id}`。已查找：{searched}。请把剧本放在 $SHANGYI_HOME/playbooks/{id}/playbook.yml。"
    )]
    PlaybookNotFound { id: String, searched: String },
    #[error("剧本 {path} 解析或校验失败：{detail}")]
    PlaybookInvalid { path: String, detail: String },
    #[error(
        "案例库加载失败（{path}）：{detail}。**不当作空库继续**——那会让后续归档覆盖掉仅有的样本。"
    )]
    CaseLibrary { path: String, detail: String },
    #[error("案例恢复失败（`{case}`）：{detail}")]
    CaseResume { case: String, detail: String },
    #[error("标准 IO 失败：{0}")]
    Io(#[from] std::io::Error),
}

/// 一次工具调用的结论（MCP `tools/call` 的 `content` 已组装好）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallResult {
    /// MCP 的 `isError`。**协议错误不走这里**（见 [`crate::server`]）。
    pub is_error: bool,
    /// 文本块。`content[0]` 是工具本身的结论；`content[1]` 是会话状态块。
    pub content: Vec<String>,
    /// 调用后所在阶段。
    pub phase: SessionPhase,
    /// 若本次调用推进了阶段，这里是从哪个阶段推进的。
    pub advanced_from: Option<SessionPhase>,
    /// 若本次调用尝试推进但被闸门拦下，这里是 `PhaseGateError` 的原文。
    pub gate_error: Option<String>,
}

impl ToolCallResult {
    /// 纯文本形态（供日志/诊断）。
    pub fn text(&self) -> String {
        self.content.join("\n\n")
    }
}

/// 一次结局的渲染结果（`call` 的内部结构）。
struct Rendered {
    is_error: bool,
    content: Vec<String>,
    /// 是否需要挂起（不推进阶段）。
    suspend: bool,
    log_outcome: ToolOutcome,
}

/// 会话宿主。
pub struct SessionHost {
    #[allow(dead_code)]
    theory: &'static TheoryLibrary,
    playbook: &'static Playbook,
    #[allow(dead_code)]
    search: &'static LiveSearch,
    #[allow(dead_code)]
    poi: Option<&'static AmapPoi>,
    registry: ToolRegistry,
    executor: DomainExecutor<'static>,
    machine: shangyi_core::PhaseMachine,
    explicit_home: Option<PathBuf>,
    resolved_home: Option<PathBuf>,
    archive_enabled: bool,
    pending_header: Option<CaseHeader>,
    pending_events: Vec<Event>,
    log: Option<CaseLog>,
    case_id: String,
    case_dir: Option<PathBuf>,
    clock: SystemClock,
    call_seq: u64,
    auto_approve_checkpoints: bool,
    mode: ConsultationMode,
    action: Option<String>,
    trusted_inputs_lines_read: usize,
    post_checkpoint_user_messages: Vec<String>,
}

impl SessionHost {
    /// 打开一个咨询会话（新建或恢复）。
    pub fn open(config: HostConfig) -> Result<Self, HostError> {
        let explicit_home = config.home.clone();
        let resolved_home = resolve_home(explicit_home.as_deref());

        if let Some(resume_ref) = config.resume_case.clone() {
            return Self::open_resumed(config, resume_ref, explicit_home, resolved_home);
        }

        let mode = config.mode;
        let playbook = load_playbook(config.playbook_id.as_deref(), explicit_home.as_deref())?;
        let playbook: &'static Playbook = Box::leak(Box::new(playbook));
        let theory: &'static TheoryLibrary = Box::leak(Box::new(TheoryLibrary::new()));
        let search: &'static LiveSearch =
            Box::leak(Box::new(search_for_mcp(explicit_home.as_deref())));
        let poi: Option<&'static AmapPoi> = poi_for_mcp(explicit_home.as_deref())
            .map(|p| -> &'static AmapPoi { Box::leak(Box::new(p)) });

        let mut executor = DomainExecutor::new(theory, playbook)
            .with_mode(mode)
            .with_search(search)
            .with_poi_opt(poi.map(|p| p as &dyn PoiProvider))
            .with_wiki(load_wiki(resolved_home.as_deref()));
        if let Some(swarm_factory) = swarm_for_mcp(explicit_home.as_deref()) {
            executor = executor.with_swarm_actor(swarm_factory);
        }
        if let Some(home) = &resolved_home {
            let library = home.join("cases").join("library.json");
            executor = executor
                .with_case_library_path(library.clone())
                .map_err(|e| HostError::CaseLibrary {
                    path: library.display().to_string(),
                    detail: e,
                })?;
        }

        let machine = shangyi_core::PhaseMachine::new(mode).require_task_scope_selection();
        let clock = SystemClock;
        let created_at = clock.now_iso8601();
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_millis())
            .unwrap_or(0);
        let case_id = format!(
            "case-{}-{}-{}{:03}-{}",
            date_compact(&created_at),
            mode.as_str(),
            seq_from_clock(&created_at),
            millis,
            std::process::id(),
        );

        // 启动阶段只探测 `$home/cases` 是否可建可写，不立即建 `<case_dir>` 空壳；
        // 首次实际执行 `tools/call` 时再惰性打开 `CaseLog`。
        let (archive_enabled, pending_header, case_dir) = match &resolved_home {
            Some(home) => {
                let cases_root = home.join("cases");
                let dir = cases_root
                    .join(created_at.get(..10).unwrap_or("0000-00-00"))
                    .join(&case_id);
                match std::fs::create_dir_all(&cases_root) {
                    Ok(()) => {
                        let header = CaseHeader {
                            case_id: case_id.clone(),
                            mode: mode.as_str().to_string(),
                            playbook_id: playbook.meta.id.clone(),
                            created_at: created_at.clone(),
                            schema_version: CaseHeader::SCHEMA_VERSION,
                        };
                        (true, Some(header), Some(dir))
                    }
                    Err(e) => {
                        eprintln!(
                            "商奕[mcp]：案例留档目录不可用（{}）：{e}；本次会话不落盘。",
                            dir.display()
                        );
                        (false, None, Some(dir))
                    }
                }
            }
            None => (false, None, None),
        };

        // 若生产环境已有 `trusted_user_inputs.jsonl` 历史行，启动时把游标定位在
        // 「会话启动前最近 5 分钟之前的历史已跳过」处：既能接住 DSH 首轮刚发出的用户消息，
        // 又不会把数小时前的旧会话消息混入本次会话。显式 `--home` 隔离测试目录从 0 行开始读。
        let initial_offset = match (&explicit_home, &resolved_home) {
            (Some(_), _) => 0,
            (None, Some(home)) => initial_trusted_inputs_offset(home),
            (None, None) => 0,
        };

        Ok(Self {
            theory,
            playbook,
            search,
            poi,
            registry: ToolRegistry::standard(),
            executor,
            machine,
            explicit_home,
            resolved_home,
            archive_enabled,
            pending_header,
            pending_events: Vec::new(),
            log: None,
            case_id,
            case_dir,
            clock,
            call_seq: 0,
            auto_approve_checkpoints: config.auto_approve_checkpoints,
            mode,
            action: None,
            trusted_inputs_lines_read: initial_offset,
            post_checkpoint_user_messages: Vec::new(),
        })
    }

    fn open_resumed(
        config: HostConfig,
        resume_ref: String,
        explicit_home: Option<PathBuf>,
        resolved_home: Option<PathBuf>,
    ) -> Result<Self, HostError> {
        let home = resolved_home.clone().ok_or_else(|| HostError::CaseResume {
            case: resume_ref.clone(),
            detail: "无法定位商奕主目录（缺 --home / $SHANGYI_HOME / $HOME）".to_string(),
        })?;
        let dir = locate_case_dir(&home, &resume_ref).ok_or_else(|| HostError::CaseResume {
            case: resume_ref.clone(),
            detail: format!("在 {} 下未找到该案例目录", home.join("cases").display()),
        })?;
        let header_path = dir.join("header.json");
        let header_text =
            std::fs::read_to_string(&header_path).map_err(|e| HostError::CaseResume {
                case: resume_ref.clone(),
                detail: format!("读取 {} 失败：{e}", header_path.display()),
            })?;
        let header: CaseHeader =
            serde_json::from_str(&header_text).map_err(|e| HostError::CaseResume {
                case: resume_ref.clone(),
                detail: format!("解析 {} 失败：{e}", header_path.display()),
            })?;

        let mut mode = parse_consultation_mode(&header.mode).unwrap_or(config.mode);
        let playbook_id = config
            .playbook_id
            .clone()
            .unwrap_or_else(|| header.playbook_id.clone());
        let playbook = load_playbook(Some(&playbook_id), explicit_home.as_deref())?;
        let mut playbook_ref: &'static Playbook = Box::leak(Box::new(playbook));
        let theory: &'static TheoryLibrary = Box::leak(Box::new(TheoryLibrary::new()));
        let search: &'static LiveSearch =
            Box::leak(Box::new(search_for_mcp(explicit_home.as_deref())));
        let poi: Option<&'static AmapPoi> = poi_for_mcp(explicit_home.as_deref())
            .map(|p| -> &'static AmapPoi { Box::leak(Box::new(p)) });

        let mut executor = DomainExecutor::new(theory, playbook_ref)
            .with_mode(mode)
            .with_search(search)
            .with_poi_opt(poi.map(|p| p as &dyn PoiProvider))
            .with_wiki(load_wiki(Some(&home)));
        if let Some(swarm_factory) = swarm_for_mcp(explicit_home.as_deref()) {
            executor = executor.with_swarm_actor(swarm_factory);
        }
        let library = home.join("cases").join("library.json");
        executor = executor
            .with_case_library_path(library.clone())
            .map_err(|e| HostError::CaseLibrary {
                path: library.display().to_string(),
                detail: e,
            })?;

        let resume_out = shangyi_core::resume(&dir).map_err(|e| HostError::CaseResume {
            case: resume_ref.clone(),
            detail: e.to_string(),
        })?;
        let mut log = CaseLog::open(&dir, &header).map_err(|e| HostError::CaseResume {
            case: resume_ref.clone(),
            detail: e.to_string(),
        })?;
        if resume_out.crashed() {
            let _ = shangyi_core::apply_repairs(&mut log, &resume_out.repairs);
        }

        let mut machine = shangyi_core::PhaseMachine::new(mode).require_task_scope_selection();
        let mut call_seq = 0u64;
        let mut action: Option<String> = None;
        let mut post_checkpoint_user_messages: Vec<String> = Vec::new();
        let mut recorded_user_texts: Vec<String> = Vec::new();

        // 离线确定性重放历史事件以重建 DomainExecutor 与 PhaseMachine 状态
        let mut started_calls: BTreeMap<String, (String, Value)> = BTreeMap::new();
        for ev in &resume_out.events {
            match ev {
                Event::UserInputRecorded {
                    text, user_actor, ..
                } => {
                    if user_actor == "user" {
                        recorded_user_texts.push(text.trim().to_string());
                    }
                    executor.record_trusted_user_input(text, user_actor);
                    let invalidated = ToolExecutor::take_invalidated_artifacts(&mut executor);
                    if !invalidated.is_empty() {
                        let _ = machine.invalidate_user_dependent_artifacts(&invalidated);
                    }
                    if user_actor == "user" && has_open_checkpoint(&machine) {
                        post_checkpoint_user_messages.push(text.clone());
                    }
                }
                Event::PlaybookSelected { id, .. } => {
                    if let Ok(pb) = load_playbook(Some(id), explicit_home.as_deref()) {
                        playbook_ref = Box::leak(Box::new(pb));
                        executor.set_playbook(playbook_ref);
                    }
                }
                Event::TaskScopeSelected { scope, .. } => {
                    if machine.phase() == SessionPhase::Intake && machine.scoped_result().is_none()
                    {
                        let _ = machine.select_task_scope(*scope);
                    }
                }
                Event::ToolCallStarted {
                    call_id,
                    name,
                    arguments,
                    step,
                    ..
                } => {
                    if step.step > call_seq {
                        call_seq = step.step;
                    } else {
                        call_seq += 1;
                    }
                    let parsed = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| json!({}));
                    started_calls.insert(call_id.clone(), (name.clone(), parsed));
                }
                Event::ToolCallFinished {
                    call_id, outcome, ..
                } => {
                    if let Some((tool_name, args)) = started_calls.remove(call_id) {
                        if tool_name == "select_task_scope"
                            && matches!(outcome, ToolOutcome::Ok { .. })
                        {
                            if let Some(m_str) = args.get("mode").and_then(Value::as_str) {
                                if let Some(new_mode) = parse_consultation_mode(m_str) {
                                    mode = new_mode;
                                    executor.set_mode(new_mode);
                                    let prev_scope = machine.task_scope();
                                    machine = shangyi_core::PhaseMachine::new(new_mode)
                                        .require_task_scope_selection();
                                    if let Some(sc) = prev_scope {
                                        let _ = machine.select_task_scope(sc);
                                    }
                                }
                            }
                            if let Some(pb_id) = args.get("playbook").and_then(Value::as_str) {
                                if let Ok(pb) = load_playbook(Some(pb_id), explicit_home.as_deref())
                                {
                                    playbook_ref = Box::leak(Box::new(pb));
                                    executor.set_playbook(playbook_ref);
                                }
                            }
                            if let Some(act) = args.get("action").and_then(Value::as_str) {
                                if !act.trim().is_empty() {
                                    action = Some(act.trim().to_string());
                                }
                            }
                        }
                        if let ToolOutcome::Ok { output } = outcome {
                            if tool_name == "web_fetch" {
                                let req_url =
                                    args.get("url").and_then(Value::as_str).unwrap_or_default();
                                executor.restore_fetched_page(req_url, output);
                            } else if is_deterministic_replay_tool(&tool_name) {
                                let replay_call = ToolCall {
                                    call_id: call_id.clone(),
                                    name: tool_name,
                                    arguments: args,
                                };
                                let _ = executor.execute(&replay_call, machine.phase());
                                for (kind, fp) in executor.artifacts() {
                                    machine.record(kind, fp);
                                }
                            }
                        }
                    }
                }
                Event::ScopedTaskCompleted { result, .. } => {
                    if result.scope == TaskScope::Conditional {
                        let _ = machine.finish_conditional_task(result.clone());
                    } else {
                        let _ = machine.finish_scoped_task(result.clone());
                    }
                }
                Event::FollowupTaskStarted { .. } => {
                    let _ = machine.begin_followup_task();
                }
                Event::CheckpointOpened {
                    id, phase, items, ..
                } => {
                    if let (Some(cp_id), Some(ph)) =
                        (parse_checkpoint_id(id), parse_session_phase(phase))
                    {
                        let cp_items: Vec<CheckpointItem> = items
                            .iter()
                            .enumerate()
                            .map(|(idx, text)| CheckpointItem {
                                id: format!("item-{}", idx + 1),
                                content: text.clone(),
                                source: Some("case_log_replay".to_string()),
                                proposed_value: None,
                            })
                            .collect();
                        if !cp_items.is_empty() {
                            match machine.checkpoint_status(cp_id) {
                                Some(_) => {
                                    let _ = machine.reopen_checkpoint(cp_id, cp_items);
                                }
                                None => {
                                    let req = CheckpointRequest {
                                        id: cp_id,
                                        phase: ph,
                                        items: cp_items,
                                        revision_requested: false,
                                    };
                                    let _ = machine.open_checkpoint(req);
                                }
                            }
                            post_checkpoint_user_messages.clear();
                        }
                    }
                }
                Event::CheckpointAnswered {
                    id,
                    answers,
                    user_actor,
                    at,
                } => {
                    if let Some(cp_id) = parse_checkpoint_id(id) {
                        let first = answers.first().map(String::as_str).unwrap_or("");
                        if first.starts_with("追问：") {
                            // 追问不结算关口
                        } else if first.starts_with("修改：") || first.starts_with("驳回：")
                        {
                            let reason = first
                                .strip_prefix("修改：")
                                .or_else(|| first.strip_prefix("驳回："))
                                .unwrap_or(first)
                                .to_string();
                            let _ = machine.settle_checkpoint(
                                cp_id,
                                CheckpointStatus::Rejected {
                                    reason,
                                    at: at.clone(),
                                },
                            );
                            post_checkpoint_user_messages.clear();
                        } else {
                            let fp = machine.artifact_fingerprint();
                            let _ = machine.settle_checkpoint(
                                cp_id,
                                CheckpointStatus::Approved {
                                    fingerprint: fp,
                                    by: user_actor.clone(),
                                    at: at.clone(),
                                },
                            );
                            post_checkpoint_user_messages.clear();
                        }
                    }
                }
                Event::PhaseEntered { to, .. } => {
                    if let Some(target) = parse_session_phase(to) {
                        if machine.phase() != target {
                            let _ = machine.advance_to(target).or_else(|_| machine.advance());
                        }
                    }
                }
                Event::PhaseRewound { to, .. } => {
                    if let Some(target) = parse_session_phase(to) {
                        let _ = machine.rollback_to(target);
                    }
                }
                _ => {}
            }
        }

        // 若恢复前外部（Web UI / CLI）刚刚追加了 CheckpointAnswered 但尚未写 PhaseEntered，
        // 在恢复完成时自动补齐推进。
        if let Some(cp_id) = machine.graph().checkpoint_at(machine.phase()) {
            if matches!(
                machine.checkpoint_status(cp_id),
                Some(CheckpointStatus::Approved { .. })
            ) {
                let from = machine.phase();
                if let Ok(to) = machine.advance() {
                    let now = SystemClock.now_iso8601();
                    let _ = log.append_durable(Event::PhaseEntered {
                        from: from.as_str().to_string(),
                        to: to.as_str().to_string(),
                        playbook: mode.as_str().to_string(),
                        reason: "闸门通过（外部确认恢复推进）".to_string(),
                        at: now,
                    });
                }
            }
        }

        write_active_pointer(&home, &header.case_id, &dir);
        let initial_offset = resumed_trusted_inputs_offset(&home, &recorded_user_texts);

        Ok(Self {
            theory,
            playbook: playbook_ref,
            search,
            poi,
            registry: ToolRegistry::standard(),
            executor,
            machine,
            explicit_home,
            resolved_home: Some(home),
            archive_enabled: true,
            pending_header: None,
            pending_events: Vec::new(),
            log: Some(log),
            case_id: header.case_id,
            case_dir: Some(dir),
            clock: SystemClock,
            call_seq,
            auto_approve_checkpoints: config.auto_approve_checkpoints,
            mode,
            action,
            trusted_inputs_lines_read: initial_offset,
            post_checkpoint_user_messages,
        })
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn playbook(&self) -> &Playbook {
        self.playbook
    }

    pub fn phase(&self) -> SessionPhase {
        self.machine.phase()
    }

    pub fn mode(&self) -> ConsultationMode {
        self.mode
    }

    pub fn action(&self) -> Option<&str> {
        self.action.as_deref()
    }

    pub fn case_id(&self) -> &str {
        &self.case_id
    }

    pub fn case_dir(&self) -> Option<&Path> {
        self.case_dir.as_deref()
    }

    pub fn explicit_home(&self) -> Option<&Path> {
        self.explicit_home.as_deref()
    }

    /// 本次会话是否可落盘留档。
    pub fn archiving(&self) -> bool {
        self.archive_enabled
    }

    /// 本阶段允许调用的工具名（阶段白名单 ∧ 任务范围）。
    pub fn allowed_tools(&self) -> Vec<String> {
        let phase = self.machine.phase();
        let mut names: Vec<String> = self
            .registry
            .tools_for(phase.as_str(), self.playbook)
            .into_iter()
            .filter(|spec| self.machine.allows_task_tool(&spec.name))
            .map(|spec| spec.name.clone())
            .collect();
        names.sort();
        names
    }

    /// 执行一次工具调用。
    pub fn call(&mut self, name: &str, arguments: Value) -> ToolCallResult {
        let at = self.clock.now_iso8601();
        self.sync_external_inputs(&at);

        // 若在 intake 首次调用 select_task_scope 时动态指定了 mode / playbook / action，
        // 先校验并应用（同时更新尚未落盘的 pending_header，保证 header.json 口径一致）。
        let mut dynamic_playbook_switched: Option<String> = None;
        let mut dynamic_override_err: Option<String> = None;
        if name == "select_task_scope"
            && self.machine.phase() == SessionPhase::Intake
            && self.machine.scoped_result().is_none()
        {
            if let Err(err) =
                self.apply_dynamic_intake_selection(&arguments, &mut dynamic_playbook_switched)
            {
                dynamic_override_err = Some(err);
            }
        }

        self.ensure_log_open();
        if let Some(pb_id) = dynamic_playbook_switched {
            self.append_event(Event::PlaybookSelected {
                id: pb_id,
                source: "select_task_scope".to_string(),
                at: at.clone(),
            });
        }

        let phase_before = self.machine.phase();
        self.call_seq += 1;
        let call_id = format!("mcp-{:04}", self.call_seq);
        self.append_event(Event::ToolCallStarted {
            step: StepId::new(TurnId(1), self.call_seq),
            call_id: call_id.clone(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            at: at.clone(),
        });

        if let Some(err_msg) = dynamic_override_err {
            let rendered = Rendered {
                is_error: true,
                content: vec![format!("工具 `{name}` 调用失败：{err_msg}")],
                suspend: true,
                log_outcome: ToolOutcome::Failed { error: err_msg },
            };
            self.append_event(Event::ToolCallFinished {
                call_id,
                outcome: rendered.log_outcome,
                at,
            });
            let mut content = rendered.content;
            content.push(self.status_block(phase_before, phase_before, None, &None));
            return ToolCallResult {
                is_error: true,
                content,
                phase: phase_before,
                advanced_from: None,
                gate_error: None,
            };
        }

        // ── 闸门 1：任务范围 ──
        let rejection: Option<String> = if !self.machine.allows_task_tool(name) {
            Some(
                "该工具不属于用户当前任务范围；仅查资料/算费用时不得扩大为完整咨询。\
                 未选择范围时先用 select_task_scope。"
                    .to_string(),
            )
        } else {
            // ── 闸门 2：注册表（阶段白名单 + 剧本白名单） ──
            match self
                .registry
                .check_call(name, phase_before.as_str(), Some(self.playbook))
            {
                Err(e) => Some(e.to_string()),
                Ok(_) => None,
            }
        };

        let rendered = match rejection {
            Some(reason) => {
                let mut msg = format!("工具 `{name}` 被拒绝：{reason}");
                msg.push_str(&format!(
                    "\n（这是**调用前置校验**的拒绝，不是执行失败。当前阶段 `{}`；本阶段允许的工具：{}）",
                    phase_before.as_str(),
                    self.allowed_tools().join(" / ")
                ));
                Rendered {
                    is_error: true,
                    content: vec![msg],
                    suspend: true,
                    log_outcome: ToolOutcome::Failed { error: reason },
                }
            }
            None if name == "checkpoint" && arguments.get("decision").is_some() => {
                self.settle_conversational_checkpoint(&arguments, phase_before, &at)
            }
            None => {
                let call = ToolCall {
                    call_id: call_id.clone(),
                    name: name.to_string(),
                    arguments,
                };
                let outcome = self.executor.execute(&call, phase_before);
                self.sync_artifacts();
                self.apply_executor_invalidations(&at);
                self.render_outcome(name, outcome, &at)
            }
        };

        self.append_event(Event::ToolCallFinished {
            call_id,
            outcome: rendered.log_outcome.clone(),
            at: at.clone(),
        });

        // ── 推进 ──
        let mut gate_error = None;
        let mut advanced = None;
        if !rendered.suspend && !shangyi_tools::is_session_control(name) {
            match self.machine.advance() {
                Ok(to) => {
                    self.append_event(Event::PhaseEntered {
                        from: phase_before.as_str().to_string(),
                        to: to.as_str().to_string(),
                        playbook: self.mode.as_str().to_string(),
                        reason: "闸门通过".to_string(),
                        at: at.clone(),
                    });
                    advanced = Some(to);
                }
                Err(e) => gate_error = Some(e.to_string()),
            }
        }

        let phase_after = self.machine.phase();
        let mut content = rendered.content;
        content.push(self.status_block(phase_before, phase_after, advanced, &gate_error));

        ToolCallResult {
            is_error: rendered.is_error,
            content,
            phase: phase_after,
            advanced_from: advanced.map(|_| phase_before),
            gate_error,
        }
    }

    fn apply_dynamic_intake_selection(
        &mut self,
        arguments: &Value,
        switched_playbook: &mut Option<String>,
    ) -> Result<(), String> {
        if let Some(pb_val) = arguments.get("playbook") {
            let pb_id = pb_val
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| "`select_task_scope.playbook` 必须是非空字符串".to_string())?;
            if pb_id != self.playbook.meta.id {
                let pb = load_playbook(Some(pb_id), self.explicit_home.as_deref())
                    .map_err(|e| e.to_string())?;
                let pb_static: &'static Playbook = Box::leak(Box::new(pb));
                self.playbook = pb_static;
                self.executor.set_playbook(pb_static);
                if let Some(header) = &mut self.pending_header {
                    header.playbook_id = pb_static.meta.id.clone();
                }
                *switched_playbook = Some(pb_static.meta.id.clone());
            }
        }

        if let Some(mode_val) = arguments.get("mode") {
            let mode_str = mode_val
                .as_str()
                .map(str::trim)
                .ok_or_else(|| "`select_task_scope.mode` 必须是字符串".to_string())?;
            let new_mode = parse_consultation_mode(mode_str).ok_or_else(|| {
                format!(
                    "未知咨询模式 `{mode_str}`（允许：scenario / whatif / discover）"
                )
            })?;
            if new_mode != self.mode {
                let prev_scope = self.machine.task_scope();
                self.mode = new_mode;
                self.executor.set_mode(new_mode);
                self.machine =
                    shangyi_core::PhaseMachine::new(new_mode).require_task_scope_selection();
                if let Some(sc) = prev_scope {
                    let _ = self.machine.select_task_scope(sc);
                }
                if let Some(header) = &mut self.pending_header {
                    header.mode = new_mode.as_str().to_string();
                }
            }
        }

        if let Some(action_val) = arguments.get("action") {
            if !action_val.is_null() {
                let act = action_val
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| "`select_task_scope.action` 必须是非空字符串".to_string())?;
                self.action = Some(act.to_string());
            }
        }
        Ok(())
    }

    fn settle_conversational_checkpoint(
        &mut self,
        arguments: &Value,
        phase: SessionPhase,
        at: &str,
    ) -> Rendered {
        let decision = match arguments.get("decision").and_then(Value::as_str) {
            Some(d @ ("approve" | "reject")) => d,
            _ => {
                let err = "`checkpoint.decision` 只能为 `\"approve\"` 或 `\"reject\"`".to_string();
                return Rendered {
                    is_error: true,
                    content: vec![format!("工具 `checkpoint` 调用失败：{err}")],
                    suspend: true,
                    log_outcome: ToolOutcome::Failed { error: err },
                };
            }
        };

        let user_quote = match arguments
            .get("user_quote")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| s.chars().filter(|c| !c.is_whitespace()).count() >= 2)
        {
            Some(q) => q.to_string(),
            None => {
                let err = "通过对话结算 `checkpoint` 时必须提供不少于 2 个非空字符的 `user_quote`（用户在关口打开后回复的真实原文片段）".to_string();
                return Rendered {
                    is_error: true,
                    content: vec![format!("工具 `checkpoint` 调用失败：{err}")],
                    suspend: true,
                    log_outcome: ToolOutcome::Failed { error: err },
                };
            }
        };
        if user_quote.contains(['?', '？', '吗']) {
            let err = "疑问句不能作为关口结算确认凭据（`user_quote` 含疑问词/问号）".to_string();
            return Rendered {
                is_error: true,
                content: vec![format!("工具 `checkpoint` 调用失败：{err}")],
                suspend: true,
                log_outcome: ToolOutcome::Failed { error: err },
            };
        }

        let Some(cp_id) = self.machine.graph().checkpoint_at(phase) else {
            let err = format!("当前阶段 `{}` 没有需要结算的 checkpoint", phase.as_str());
            return Rendered {
                is_error: true,
                content: vec![format!("工具 `checkpoint` 调用失败：{err}")],
                suspend: true,
                log_outcome: ToolOutcome::Failed { error: err },
            };
        };

        if let Some(explicit_cp) = arguments.get("checkpoint_id").and_then(Value::as_str) {
            if explicit_cp.trim() != cp_id.as_str() {
                let err = format!(
                    "当前阶段 `{}` 对应的关口是 `{}`，而非 `{explicit_cp}`",
                    phase.as_str(),
                    cp_id.as_str()
                );
                return Rendered {
                    is_error: true,
                    content: vec![format!("工具 `checkpoint` 调用失败：{err}")],
                    suspend: true,
                    log_outcome: ToolOutcome::Failed { error: err },
                };
            }
        }

        if !matches!(
            self.machine.checkpoint_status(cp_id),
            Some(CheckpointStatus::Open { .. })
        ) {
            let err = format!(
                "关口 `{}` 当前未处于待确认（Open）状态；请先携带 `items` 打开确认单并等待用户回复",
                cp_id.as_str()
            );
            return Rendered {
                is_error: true,
                content: vec![format!("工具 `checkpoint` 调用失败：{err}")],
                suspend: true,
                log_outcome: ToolOutcome::Failed { error: err },
            };
        }

        let verified = self.post_checkpoint_user_messages.iter().any(|msg| {
            msg.match_indices(&user_quote).any(|(pos, _)| {
                if decision != "approve" {
                    return true;
                }
                let prefix = msg[..pos].trim_end();
                !["不", "未", "别", "勿", "没", "并非", "不要", "拒绝", "驳回"]
                    .iter()
                    .any(|neg| prefix.ends_with(neg))
            })
        });
        if !verified {
            let err = format!(
                "拒绝结算 checkpoint `{}`（铁律 2 强制门禁）：未在当前关口打开之后的真实用户消息中找到原文片段 `{user_quote}`。\
                 严禁模型自行代批或引用关口打开前的旧消息；请先等待用户真实回复，或请用户在工作台点击「核准通过」。",
                cp_id.as_str()
            );
            return Rendered {
                is_error: true,
                content: vec![err.clone()],
                suspend: true,
                log_outcome: ToolOutcome::Failed { error: err },
            };
        }

        match decision {
            "approve" => {
                let fingerprint = self.machine.artifact_fingerprint();
                match self.machine.settle_checkpoint(
                    cp_id,
                    CheckpointStatus::Approved {
                        fingerprint: fingerprint.clone(),
                        by: "user".to_string(),
                        at: at.to_string(),
                    },
                ) {
                    Ok(()) => {
                        self.post_checkpoint_user_messages.clear();
                        self.append_event(Event::CheckpointAnswered {
                            id: cp_id.as_str().to_string(),
                            answers: vec![format!("确认：{user_quote}")],
                            user_actor: "user".to_string(),
                            at: at.to_string(),
                        });
                        self.append_event(Event::PhaseApproved {
                            phase: phase.as_str().to_string(),
                            fingerprint,
                            at: at.to_string(),
                        });
                        let out = json!({
                            "kind": "checkpoint",
                            "checkpoint": cp_id.as_str(),
                            "phase": phase.as_str(),
                            "needs_user_input": false,
                            "approved_by": "user",
                            "user_quote": user_quote,
                        })
                        .to_string();
                        Rendered {
                            is_error: false,
                            content: vec![out.clone()],
                            suspend: false,
                            log_outcome: ToolOutcome::Ok { output: out },
                        }
                    }
                    Err(e) => Rendered {
                        is_error: true,
                        content: vec![format!("工具 `checkpoint` 结算失败：{e}")],
                        suspend: true,
                        log_outcome: ToolOutcome::Failed {
                            error: e.to_string(),
                        },
                    },
                }
            }
            "reject" => {
                self.executor.record_trusted_user_input(&user_quote, "user");
                self.apply_executor_invalidations(at);
                if matches!(
                    self.machine.checkpoint_status(cp_id),
                    Some(CheckpointStatus::Open { .. })
                ) {
                    let _ = self.machine.settle_checkpoint(
                        cp_id,
                        CheckpointStatus::Rejected {
                            reason: user_quote.clone(),
                            at: at.to_string(),
                        },
                    );
                }
                self.post_checkpoint_user_messages.clear();
                self.append_event(Event::CheckpointAnswered {
                    id: cp_id.as_str().to_string(),
                    answers: vec![format!("修改：{user_quote}")],
                    user_actor: "user".to_string(),
                    at: at.to_string(),
                });
                let out = json!({
                    "kind": "checkpoint",
                    "checkpoint": cp_id.as_str(),
                    "phase": phase.as_str(),
                    "needs_user_input": false,
                    "rejected_by": "user",
                    "reason": user_quote,
                    "note": "用户要求修订当前阶段内容；请按用户反馈更新产物后重新调用 checkpoint(items=...)。",
                })
                .to_string();
                Rendered {
                    is_error: false,
                    content: vec![out.clone()],
                    suspend: true,
                    log_outcome: ToolOutcome::Ok { output: out },
                }
            }
            _ => unreachable!(),
        }
    }

    // ── 外部事件同步（DSH 用户消息账本 + Web/CLI 关口确认） ──

    fn sync_external_inputs(&mut self, now_at: &str) {
        // 1) 同步 `$home/runtime/trusted_user_inputs.jsonl` 新追加的真实用户消息
        if let Some(home) = self.resolved_home.clone() {
            let ledger_path = home.join("runtime").join("trusted_user_inputs.jsonl");
            if let Ok(content) = std::fs::read_to_string(&ledger_path) {
                let lines: Vec<&str> = content.lines().collect();
                if lines.len() < self.trusted_inputs_lines_read {
                    // 文件被截断重建时归零重读
                    self.trusted_inputs_lines_read = 0;
                }
                let new_lines: Vec<String> = lines[self.trusted_inputs_lines_read..]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                self.trusted_inputs_lines_read = lines.len();

                for raw in new_lines {
                    let trimmed = raw.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
                        continue;
                    };
                    let actor = val
                        .get("user_actor")
                        .and_then(Value::as_str)
                        .unwrap_or("user");
                    if actor != "user" {
                        continue;
                    }
                    let Some(text) = val
                        .get("text")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                    else {
                        continue;
                    };
                    let entry_at = val
                        .get("at")
                        .and_then(Value::as_str)
                        .unwrap_or(now_at)
                        .to_string();

                    self.executor.record_trusted_user_input(text, "user");
                    self.apply_executor_invalidations(&entry_at);
                    if has_open_checkpoint(&self.machine) {
                        self.post_checkpoint_user_messages.push(text.to_string());
                    }
                    self.append_or_buffer_event(Event::UserInputRecorded {
                        text: text.to_string(),
                        user_actor: "user".to_string(),
                        at: entry_at,
                    });
                }
            }
        }

        // 2) 同步外部进程（Web 工作台 `/api/shangyi/checkpoint` 或 CLI `shangyi checkpoint`）
        //    直接写入当前案例 `case.jsonl` 的事件
        let external_records = match &mut self.log {
            Some(log) => match log.sync_external() {
                Ok(recs) => recs,
                Err(e) => {
                    eprintln!("商奕[mcp]：读取外部同步日志失败：{e}");
                    Vec::new()
                }
            },
            None => Vec::new(),
        };

        for rec in external_records {
            match rec.event {
                Event::UserInputRecorded {
                    text, user_actor, at,
                } => {
                    self.executor.record_trusted_user_input(&text, &user_actor);
                    self.apply_executor_invalidations(&at);
                    if user_actor == "user" && has_open_checkpoint(&self.machine) {
                        self.post_checkpoint_user_messages.push(text);
                    }
                }
                Event::CheckpointAnswered {
                    id,
                    answers,
                    user_actor,
                    at,
                } => {
                    let Some(cp_id) = parse_checkpoint_id(&id) else {
                        continue;
                    };
                    if !matches!(
                        self.machine.checkpoint_status(cp_id),
                        Some(CheckpointStatus::Open { .. })
                    ) {
                        continue;
                    }
                    let first = answers.first().map(String::as_str).unwrap_or("");
                    if first.starts_with("追问：") {
                        continue;
                    }
                    if first.starts_with("修改：") || first.starts_with("驳回：") {
                        let reason = first
                            .strip_prefix("修改：")
                            .or_else(|| first.strip_prefix("驳回："))
                            .unwrap_or(first)
                            .to_string();
                        self.executor.record_trusted_user_input(&reason, &user_actor);
                        self.apply_executor_invalidations(&at);
                        if matches!(
                            self.machine.checkpoint_status(cp_id),
                            Some(CheckpointStatus::Open { .. })
                        ) {
                            let _ = self.machine.settle_checkpoint(
                                cp_id,
                                CheckpointStatus::Rejected {
                                    reason,
                                    at: at.clone(),
                                },
                            );
                        }
                        self.post_checkpoint_user_messages.clear();
                    } else {
                        let phase_before = self.machine.phase();
                        let fp = self.machine.artifact_fingerprint();
                        if self
                            .machine
                            .settle_checkpoint(
                                cp_id,
                                CheckpointStatus::Approved {
                                    fingerprint: fp.clone(),
                                    by: user_actor,
                                    at: at.clone(),
                                },
                            )
                            .is_ok()
                        {
                            self.post_checkpoint_user_messages.clear();
                            self.append_event(Event::PhaseApproved {
                                phase: phase_before.as_str().to_string(),
                                fingerprint: fp,
                                at: at.clone(),
                            });
                            if let Ok(to) = self.machine.advance() {
                                self.append_event(Event::PhaseEntered {
                                    from: phase_before.as_str().to_string(),
                                    to: to.as_str().to_string(),
                                    playbook: self.mode.as_str().to_string(),
                                    reason: "闸门通过（外部确认）".to_string(),
                                    at,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn apply_executor_invalidations(&mut self, at: &str) {
        let invalidated = ToolExecutor::take_invalidated_artifacts(&mut self.executor);
        if invalidated.is_empty() {
            return;
        }
        let from = self.machine.phase();
        if let Some(to) = self.machine.invalidate_user_dependent_artifacts(&invalidated) {
            self.append_or_buffer_event(Event::PhaseRewound {
                from: from.as_str().to_string(),
                to: to.as_str().to_string(),
                due_to: "用户更新约束或上下文，受影响的下游产物与关口确认已作废".to_string(),
                at: at.to_string(),
            });
        }
    }

    fn ensure_log_open(&mut self) {
        if self.log.is_some() || !self.archive_enabled {
            return;
        }
        let (Some(dir), Some(header)) = (self.case_dir.clone(), self.pending_header.take()) else {
            return;
        };
        match CaseLog::open(&dir, &header) {
            Ok(log) => {
                if let Some(home) = &self.resolved_home {
                    write_active_pointer(home, &self.case_id, &dir);
                }
                self.log = Some(log);
                let buffered = std::mem::take(&mut self.pending_events);
                for ev in buffered {
                    self.append_event(ev);
                }
            }
            Err(e) => {
                self.archive_enabled = false;
                self.pending_events.clear();
                eprintln!(
                    "商奕[mcp]：案例留档目录不可用（{}）：{e}；本次会话不落盘。",
                    dir.display()
                );
            }
        }
    }

    fn append_or_buffer_event(&mut self, event: Event) {
        if self.log.is_some() {
            self.append_event(event);
        } else if self.archive_enabled {
            self.pending_events.push(event);
        }
    }

    // ── 结局渲染 ──

    fn render_outcome(&mut self, name: &str, outcome: ExecOutcome, at: &str) -> Rendered {
        match outcome {
            ExecOutcome::Ok(text) => Rendered {
                is_error: false,
                content: vec![text.clone()],
                suspend: false,
                log_outcome: ToolOutcome::Ok { output: text },
            },
            ExecOutcome::Failed(e) => Rendered {
                is_error: true,
                content: vec![format!(
                    "工具 `{name}` 调用失败：{e}\n\
                     （这是可纠正的错误：检查参数或改用本阶段允许的工具后重试。）"
                )],
                suspend: true,
                log_outcome: ToolOutcome::Failed { error: e },
            },
            ExecOutcome::Blocked(d) => Rendered {
                is_error: false,
                content: vec![json!({
                    "blocked": true,
                    "tool": name,
                    "reason": d,
                    "guidance": "这是**外部依赖不可用/未取得**，不是结论、也不是执行失败。\
                                 请勿用推测填补该缺口；资料/费用短任务应把缺口列入 \
                                 complete_task.missing，完整咨询如无法绕过则如实标为未取得。"
                })
                .to_string()],
                suspend: false,
                log_outcome: ToolOutcome::Blocked { detail: d },
            },
            ExecOutcome::InsufficientEvidence { gaps } => Rendered {
                is_error: false,
                content: vec![json!({
                    "insufficient_evidence": true,
                    "tool": name,
                    "gaps": gaps,
                    "guidance": "关键证据未取得：不能填零、删除对手或改成背景方以通过校验；\
                                 可补真实来源，或调用 conditional_analysis 正式交付已有费用、\
                                 条件比较及信息收集步骤。相同缺口最多三次。"
                })
                .to_string()],
                suspend: false,
                log_outcome: ToolOutcome::InsufficientEvidence { gaps },
            },
            ExecOutcome::TaskScopeSelected { scope, reason } => {
                match self.machine.select_task_scope(scope) {
                    Ok(()) => {
                        self.append_event(Event::TaskScopeSelected {
                            scope,
                            reason: reason.clone(),
                            at: at.to_string(),
                        });
                        let mut payload = json!({
                            "scope": scope,
                            "phase": "intake",
                            "mode": self.mode.as_str(),
                            "playbook": self.playbook.meta.id,
                            "requires_game_analysis": !scope.is_brief(),
                            "reason": reason,
                        });
                        if let Some(act) = &self.action {
                            payload["action"] = json!(act);
                        }
                        let out = payload.to_string();
                        Rendered {
                            is_error: false,
                            content: vec![out.clone()],
                            suspend: false,
                            log_outcome: ToolOutcome::Ok { output: out },
                        }
                    }
                    Err(e) => Rendered {
                        is_error: true,
                        content: vec![format!("工具 `{name}` 调用失败：{e}")],
                        suspend: true,
                        log_outcome: ToolOutcome::Failed {
                            error: e.to_string(),
                        },
                    },
                }
            }
            ExecOutcome::ScopedTaskCompleted(result) => {
                let applied = if result.scope == TaskScope::Conditional {
                    self.machine.finish_conditional_task(result.clone())
                } else {
                    self.machine.finish_scoped_task(result.clone())
                };
                match applied {
                    Ok(()) => {
                        let receipt = serde_json::to_string(&result)
                            .unwrap_or_else(|e| format!("{{\"serialization_error\":\"{e}\"}}"));
                        self.append_event(Event::ScopedTaskCompleted {
                            result: result.clone(),
                            at: at.to_string(),
                        });
                        Rendered {
                            is_error: false,
                            content: vec![receipt.clone()],
                            suspend: false,
                            log_outcome: ToolOutcome::Ok { output: receipt },
                        }
                    }
                    Err(e) => Rendered {
                        is_error: true,
                        content: vec![format!("工具 `{name}` 调用失败：{e}")],
                        suspend: true,
                        log_outcome: ToolOutcome::Failed {
                            error: e.to_string(),
                        },
                    },
                }
            }
            ExecOutcome::AsksUser(questions) => {
                let payload: Vec<Value> = questions
                    .iter()
                    .map(|q| {
                        json!({
                            "id": q.id,
                            "text": q.text,
                            "kind": q.kind,
                            "options": q.options,
                        })
                    })
                    .collect();
                Rendered {
                    is_error: false,
                    content: vec![json!({
                        "needs_user_input": true,
                        "kind": "question",
                        "questions": payload,
                        "note": "这是向用户**采集**信息（不是失败）。请把问题逐条转达用户并等待回答；\
                                 服务端不把采集到的东西当产物，也**不**因此推进阶段。",
                    })
                    .to_string()],
                    suspend: true,
                    log_outcome: ToolOutcome::Ok {
                        output: format!("PENDING_USER_ANSWER:{}", questions.len()),
                    },
                }
            }
            ExecOutcome::RewindRequest { target, reason } => Rendered {
                is_error: false,
                content: vec![json!({
                    "needs_user_input": true,
                    "kind": "rewind_request",
                    "target_phase": target.as_str(),
                    "reason": reason,
                    "note": "模型**提议**回退到更早阶段，决定权在用户。\
                             服务端本轮不执行回退（回退会作废已完成的工作，须用户知情同意）。",
                })
                .to_string()],
                suspend: true,
                log_outcome: ToolOutcome::Ok {
                    output: format!("PENDING_REWIND_DECISION:{}", target.as_str()),
                },
            },
            ExecOutcome::NeedsUserInput(req) => self.bookkeep_checkpoint(req, at),
        }
    }

    fn bookkeep_checkpoint(&mut self, req: CheckpointRequest, at: &str) -> Rendered {
        let id = req.id;
        let already_approved_same = matches!(
            self.machine.checkpoint_status(id),
            Some(CheckpointStatus::Approved { fingerprint, .. })
                if *fingerprint == self.machine.artifact_fingerprint()
        );
        if already_approved_same {
            let out = json!({
                "needs_user_input": false,
                "kind": "checkpoint",
                "checkpoint": id.as_str(),
                "phase": req.phase.as_str(),
                "already_approved": true,
                "note": "同一内容已确认过，无需重复；服务端未重开。",
            })
            .to_string();
            return Rendered {
                is_error: false,
                content: vec![out],
                suspend: true,
                log_outcome: ToolOutcome::Ok {
                    output: format!("ALREADY_APPROVED:{}", id.as_str()),
                },
            };
        }

        let reopened_or_opened = match self.machine.checkpoint_status(id).cloned() {
            Some(CheckpointStatus::Open { .. })
            | Some(CheckpointStatus::Approved { .. })
            | Some(CheckpointStatus::Rejected { .. }) => self
                .machine
                .reopen_checkpoint(id, req.items.clone())
                .map(|()| "reopened"),
            None => self.machine.open_checkpoint(req.clone()).map(|()| "opened"),
        };
        if let Err(e) = reopened_or_opened {
            return Rendered {
                is_error: true,
                content: vec![format!("工具 `checkpoint` 调用失败（无法挂载提问件）：{e}")],
                suspend: true,
                log_outcome: ToolOutcome::Failed {
                    error: e.to_string(),
                },
            };
        }

        // 关口打开/重开时清空关口后用户消息缓冲：只有在此之后到达的真实用户回复才可作为确认凭据。
        self.post_checkpoint_user_messages.clear();

        self.append_event(Event::CheckpointOpened {
            id: id.as_str().to_string(),
            phase: req.phase.as_str().to_string(),
            items: req.items.iter().map(|i| i.content.clone()).collect(),
            at: at.to_string(),
        });

        let items: Vec<Value> = req
            .items
            .iter()
            .map(|i| {
                json!({
                    "id": i.id,
                    "content": i.content,
                    "source": i.source,
                    "proposed_value": i.proposed_value,
                })
            })
            .collect();
        let base = json!({
            "kind": "checkpoint",
            "checkpoint": id.as_str(),
            "checkpoint_kind": id.kind_str_zh(),
            "phase": req.phase.as_str(),
            "case_id": self.case_id,
            "items": items,
        });

        if self.auto_approve_checkpoints {
            let fingerprint = self.machine.artifact_fingerprint();
            let settled = self.machine.settle_checkpoint(
                id,
                CheckpointStatus::Approved {
                    fingerprint: fingerprint.clone(),
                    by: "auto".to_string(),
                    at: at.to_string(),
                },
            );
            match settled {
                Ok(()) => {
                    self.append_event(Event::CheckpointAnswered {
                        id: id.as_str().to_string(),
                        answers: vec!["确认（服务端自动结算，非真人确认）".to_string()],
                        user_actor: "auto".to_string(),
                        at: at.to_string(),
                    });
                    self.append_event(Event::PhaseApproved {
                        phase: req.phase.as_str().to_string(),
                        fingerprint,
                        at: at.to_string(),
                    });
                    let mut payload = base;
                    payload["needs_user_input"] = json!(false);
                    payload["auto_approved"] = json!(true);
                    payload["approved_by"] = json!("auto");
                    payload["note"] = json!(
                        "服务端按 `--auto-approve-checkpoints` 自动结算；**未经过真人确认**\
                         （案例日志里 actor=auto）。若本进程没有这个开关，这里会返回 \
                         needs_user_input=true 并停在原阶段。"
                    );
                    let out = payload.to_string();
                    Rendered {
                        is_error: false,
                        content: vec![out.clone()],
                        suspend: false,
                        log_outcome: ToolOutcome::Ok { output: out },
                    }
                }
                Err(e) => Rendered {
                    is_error: true,
                    content: vec![format!("工具 `checkpoint` 调用失败（无法结算）：{e}")],
                    suspend: true,
                    log_outcome: ToolOutcome::Failed {
                        error: e.to_string(),
                    },
                },
            }
        } else {
            let mut payload = base;
            payload["needs_user_input"] = json!(true);
            payload["note"] = json!(
                "这是铁律 2 的**确认闸门**：请把 items 逐条转达用户并暂停等待。\
                 用户可在工作台点击「核准通过 / 驳回修订」（服务端会自动同步），\
                 或在对话中直接明确确认；收到真实用户回复后可再次调用 `checkpoint`\
                 （传 `decision` 与逐字 `user_quote`）结算。严禁模型自行确认。"
            );
            Rendered {
                is_error: false,
                content: vec![payload.to_string()],
                suspend: true,
                log_outcome: ToolOutcome::Ok {
                    output: format!("PENDING_CHECKPOINT:{}", id.as_str()),
                },
            }
        }
    }

    fn sync_artifacts(&mut self) {
        for (kind, fingerprint) in self.executor.artifacts() {
            self.machine.record(kind, fingerprint);
        }
    }

    fn status_block(
        &self,
        before: SessionPhase,
        after: SessionPhase,
        advanced: Option<SessionPhase>,
        gate_error: &Option<String>,
    ) -> String {
        let mut s = format!(
            "[session] 阶段={} 模式={} 剧本={} 案例={}",
            after.as_str(),
            self.mode.as_str(),
            self.playbook.meta.id,
            self.case_id
        );
        if let Some(to) = advanced {
            s.push_str(&format!(
                "\n[session] 阶段推进：{} → {}（闸门通过）。本阶段允许的工具：{}",
                before.as_str(),
                to.as_str(),
                self.allowed_tools().join(" / ")
            ));
        } else if let Some(err) = gate_error {
            s.push_str(&format!(
                "\n[session] 阶段未推进（仍在 {}）：{err}",
                after.as_str()
            ));
        }
        if self.machine.task_scope().is_none() {
            s.push_str(
                "\n[session] 任务范围尚未选择：先调用 select_task_scope（在此之前除 ask_user 外的域工具都会被拒绝）。",
            );
        }
        s
    }

    fn append_event(&mut self, event: Event) {
        let durable = shangyi_core::is_durable(&event);
        if let Some(log) = &mut self.log {
            let result = if durable {
                log.append_durable(event)
            } else {
                log.append(event)
            };
            if let Err(e) = result {
                eprintln!("商奕[mcp]：案例日志写入失败：{e}");
            }
        }
    }
}

// ─────────────────────────── 辅助函数 ───────────────────────────

fn has_open_checkpoint(machine: &shangyi_core::PhaseMachine) -> bool {
    machine
        .graph()
        .checkpoint_at(machine.phase())
        .and_then(|cp| machine.checkpoint_status(cp))
        .is_some_and(CheckpointStatus::is_open)
}

fn parse_consultation_mode(s: &str) -> Option<ConsultationMode> {
    match s.trim() {
        "scenario" => Some(ConsultationMode::Scenario),
        "whatif" => Some(ConsultationMode::Whatif),
        "discover" => Some(ConsultationMode::Discover),
        _ => None,
    }
}

fn parse_checkpoint_id(s: &str) -> Option<CheckpointId> {
    match s.trim() {
        "cp1" => Some(CheckpointId::Cp1),
        "cp2" => Some(CheckpointId::Cp2),
        "cp3" => Some(CheckpointId::Cp3),
        _ => None,
    }
}

fn parse_session_phase(s: &str) -> Option<SessionPhase> {
    match s.trim() {
        "intake" => Some(SessionPhase::Intake),
        "intel" => Some(SessionPhase::Intel),
        "model_build" => Some(SessionPhase::ModelBuild),
        "solve" => Some(SessionPhase::Solve),
        "select" => Some(SessionPhase::Select),
        "consequence" => Some(SessionPhase::Consequence),
        "discover" => Some(SessionPhase::Discover),
        "recommend" => Some(SessionPhase::Recommend),
        "report" => Some(SessionPhase::Report),
        "archived" => Some(SessionPhase::Archived),
        _ => None,
    }
}

fn is_deterministic_replay_tool(name: &str) -> bool {
    matches!(
        name,
        "research_task_plan"
            | "research_task_update"
            | "research_record"
            | "constraint_register"
            | "business_cost"
            | "vn_map"
            | "pars_check"
            | "participant_profile_update"
            | "gamespec_build"
            | "solve_equilibria"
            | "select_equilibria"
            | "select_fuse"
            | "preference_inject"
            | "consequence_predict"
            | "discover_satisfice"
            | "recommend_build"
            | "report_render"
    )
}

fn locate_case_dir(home: &Path, case_ref: &str) -> Option<PathBuf> {
    let cases_root = home.join("cases");
    if case_ref == "latest" {
        if let Ok(active_text) = std::fs::read_to_string(cases_root.join("active.json")) {
            if let Ok(val) = serde_json::from_str::<Value>(&active_text) {
                if let Some(dir_str) = val.get("case_dir").and_then(Value::as_str) {
                    let cand = PathBuf::from(dir_str);
                    if cand.join("header.json").is_file() {
                        return Some(cand);
                    }
                }
            }
        }
        let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
        let Ok(days) = std::fs::read_dir(&cases_root) else {
            return None;
        };
        for day in days.flatten() {
            let day_path = day.path();
            if !day_path.is_dir() {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&day_path) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                let log_path = dir.join("case.jsonl");
                if let Ok(meta) = std::fs::metadata(&log_path) {
                    if meta.len() > 0 {
                        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                        if best.as_ref().is_none_or(|(prev, _)| mtime > *prev) {
                            best = Some((mtime, dir));
                        }
                    }
                }
            }
        }
        return best.map(|(_, d)| d);
    }

    if !is_safe_id_segment(case_ref) {
        return None;
    }
    let Ok(days) = std::fs::read_dir(&cases_root) else {
        return None;
    };
    for day in days.flatten() {
        let cand = day.path().join(case_ref);
        if cand.join("header.json").is_file() || cand.join("case.jsonl").is_file() {
            return Some(cand);
        }
    }
    None
}

fn is_safe_id_segment(seg: &str) -> bool {
    !seg.is_empty()
        && seg.len() <= 128
        && seg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn write_active_pointer(home: &Path, case_id: &str, case_dir: &Path) {
    let cases_root = home.join("cases");
    if std::fs::create_dir_all(&cases_root).is_ok() {
        let payload = json!({
            "case_id": case_id,
            "case_dir": case_dir.display().to_string(),
            "updated_at": SystemClock.now_iso8601(),
        });
        if let Ok(text) = serde_json::to_string_pretty(&payload) {
            let _ = std::fs::write(cases_root.join("active.json"), text);
        }
    }
}

/// 跨进程恢复会话时定位 `trusted_user_inputs.jsonl` 的起始行号：
/// 精确跳过本案例已在 `case.jsonl` 登记过的 `UserInputRecorded` 消息，
/// 但保留在上一轮退出后、本轮子进程拉起前刚追加的新用户消息（避免恢复瞬间漏读当轮输入）。
fn resumed_trusted_inputs_offset(home: &Path, recorded_user_texts: &[String]) -> usize {
    if recorded_user_texts.is_empty() {
        return initial_trusted_inputs_offset(home);
    }
    let path = home.join("runtime").join("trusted_user_inputs.jsonl");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let lines: Vec<&str> = content.lines().collect();
    let parsed_texts: Vec<Option<String>> = lines
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|v| {
                    if v.get("user_actor").and_then(Value::as_str) == Some("user") {
                        v.get("text").and_then(Value::as_str).map(|s| s.trim().to_string())
                    } else {
                        None
                    }
                })
        })
        .collect();

    // 从后向前找最近一条在账本中出现过的已落盘用户消息（兼容 Web/CLI 关口备注仅入 case.jsonl 的场景）
    for rec in recorded_user_texts.iter().rev() {
        for idx in (0..parsed_texts.len()).rev() {
            if parsed_texts[idx].as_deref() == Some(rec.as_str()) {
                return idx + 1;
            }
        }
    }
    initial_trusted_inputs_offset(home)
}

/// 生产启动时定位 `trusted_user_inputs.jsonl` 的起始行号：
/// 只接纳最近 120 秒内写入的消息（覆盖 DSH 首轮先发 `user/message`、再拉起 MCP 子进程的时序差），
/// 跳过更早历史会话的消息。
fn initial_trusted_inputs_offset(home: &Path) -> usize {
    let path = home.join("runtime").join("trusted_user_inputs.jsonl");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let lines: Vec<&str> = content.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        if let Ok(val) = serde_json::from_str::<Value>(line) {
            if let Some(ts) = val.get("unix_secs").and_then(Value::as_u64) {
                if now_secs.saturating_sub(ts) <= 120 {
                    return idx;
                }
            }
        }
    }
    lines.len()
}

/// `--home` → `$SHANGYI_HOME` → `$HOME/.shangyi`（与 `shangyi-cli` 同序）。
fn resolve_home(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Ok(v) = std::env::var("SHANGYI_HOME") {
        if !v.trim().is_empty() {
            return Some(PathBuf::from(v.trim()));
        }
    }
    std::env::var("HOME")
        .ok()
        .map(|h| Path::new(&h).join(".shangyi"))
}

/// 从磁盘加载并校验剧本。
fn load_playbook(id: Option<&str>, home: Option<&Path>) -> Result<Playbook, HostError> {
    let id = id.unwrap_or(DEFAULT_PLAYBOOK_ID);
    if !is_safe_id_segment(id) {
        return Err(HostError::PlaybookNotFound {
            id: id.to_string(),
            searched: "（非法剧本标识符：仅允许 ASCII 字母数字、下划线与连字符）".to_string(),
        });
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join("playbooks").join(id).join("playbook.yml"));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../playbooks")
            .join(id)
            .join("playbook.yml"),
    );
    for path in &candidates {
        if let Ok(text) = std::fs::read_to_string(path) {
            let pb = Playbook::from_yaml(&text).map_err(|e| HostError::PlaybookInvalid {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
            pb.validate().map_err(|e| HostError::PlaybookInvalid {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
            return Ok(pb);
        }
    }
    Err(HostError::PlaybookNotFound {
        id: id.to_string(),
        searched: candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("、"),
    })
}

/// 加载知识库（`kb_retrieve` 的后端）。
fn load_wiki(home: Option<&Path>) -> Option<std::sync::Arc<shangyi_kb::wiki::Wiki>> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("SHANGYI_KB_DIR") {
        if !dir.trim().is_empty() {
            candidates.push(PathBuf::from(dir.trim()));
        }
    }
    if let Some(home) = home {
        candidates.push(home.join("kb"));
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kb"));
    for dir in candidates {
        if !dir.is_dir() {
            continue;
        }
        match shangyi_kb::wiki::load_dir(&dir) {
            Ok(wiki) => return Some(std::sync::Arc::new(wiki)),
            Err(e) => {
                eprintln!("商奕[mcp]：知识库 {dir:?} 加载失败：{e}（本次以无知识库运行）");
                return None;
            }
        }
    }
    None
}

fn date_compact(iso: &str) -> String {
    iso.chars().filter(char::is_ascii_digit).take(8).collect()
}

fn seq_from_clock(iso: &str) -> String {
    let digits: String = iso.chars().filter(char::is_ascii_digit).collect();
    if digits.len() >= 14 {
        digits[8..14].to_string()
    } else {
        "000000".to_string()
    }
}

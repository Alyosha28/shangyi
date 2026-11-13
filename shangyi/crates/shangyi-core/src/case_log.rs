//! 案例事件日志：追加式 JSONL + 崩溃修复（设计文档 §8.3、§7.1）。
//!
//! # 布局
//!
//! ```text
//! <case_dir>/
//! ├── header.json    # 不可变会话头（写一次，之后只读）
//! └── case.jsonl     # 事件日志：每行一个 Recorded（append-only）
//! ```
//!
//! # 三条硬约束
//!
//! 1. **只追加**。[`CaseLog::append`] 不提供任何改写或删除历史的方法。
//!    要"作废"某个决定，就追加一条新事件（§8.4 的遮蔽语义）——
//!    审计问的是"当时发生了什么"，不是"现在只剩什么"。
//! 2. **关键事件必须落盘（fsync）**。checkpoint 的作答是**不可逆的**
//!    （用户已经点了确认），掉电丢失会让用户以为没确认过。故
//!    [`CaseLog::append_durable`] 对其强制 fsync；普通事件走缓冲追加。
//! 3. **损坏必须显式**。日志中间出现解析不了的行，绝不"跳过并继续"——
//!    那等于让审计看到一份**缺了几段**却看起来完整的历史。读不出即报错，
//!    由人决定怎么办。

use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::session::{Event, interrupted_turn_closers};

/// 日志中的一行。
///
/// `seq` 是**排序的权威**（不是时间戳）：同一秒内可能有多个事件，
/// 而跨机器、跨时钟调整的日志只能靠单调序号还原真实发生顺序。
///
/// # 为什么外层不重复存一份 `timestamp`
///
/// 设计文档 §8.3 借鉴 Codex 的 `RolloutLine`（外带 `timestamp`）。此处
/// **不**照搬那一层，因为商奕的 [`Event`] 自身已带 `at`（崩溃修复函数
/// §7.1 需要它）。若外层再存一份，同一事实就有两个副本——两份时间戳
/// 迟早会不一致，而审计无法判断该信哪个。故外层只加**真正新**的信息：`seq`。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recorded {
    /// 单调递增序号，从 1 开始。
    pub seq: u64,
    pub event: Event,
}

/// 会话头（写一次，不可变）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaseHeader {
    pub case_id: String,
    pub mode: String,
    pub playbook_id: String,
    /// 会话创建时刻。
    pub created_at: String,
    /// schemas 版本——日志格式是**契约**，升级须显式登记。
    pub schema_version: u32,
}

impl CaseHeader {
    pub const SCHEMA_VERSION: u32 = 1;
}

/// 日志错误。**每一种都不可被忽略**。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LogError {
    #[error("IO 失败（{path}）：{detail}")]
    Io { path: String, detail: String },
    #[error("第 {line} 行不是合法的事件记录：{detail}")]
    CorruptLine { line: usize, detail: String },
    #[error("日志已有 header.json，且其内容与本次写入不同——会话头不可变")]
    HeaderImmutable,
    #[error("缺少 header.json：{0}")]
    MissingHeader(String),
}

/// 追加式案例日志。
pub struct CaseLog {
    dir: PathBuf,
    file: File,
    next_seq: u64,
}

impl CaseLog {
    /// 打开（或创建）某案例目录的日志。
    ///
    /// 已存在则**续写**：序号接着已有日志的最大值继续，避免重号。
    pub fn open(dir: impl AsRef<Path>, header: &CaseHeader) -> Result<Self, LogError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir).map_err(|e| LogError::Io {
            path: dir.display().to_string(),
            detail: e.to_string(),
        })?;

        // 会话头：写一次；已存在则校验其一致性（不可变，不得被悄悄改写）
        let header_path = dir.join("header.json");
        if header_path.exists() {
            let existing: CaseHeader = serde_json::from_str(
                &fs::read_to_string(&header_path).map_err(|e| LogError::Io {
                    path: header_path.display().to_string(),
                    detail: e.to_string(),
                })?,
            )
            .map_err(|e| LogError::CorruptLine {
                line: 0,
                detail: format!("header.json 解析失败：{e}"),
            })?;
            if existing.case_id != header.case_id || existing.mode != header.mode {
                return Err(LogError::HeaderImmutable);
            }
        } else {
            let text = serde_json::to_string_pretty(header).map_err(|e| LogError::Io {
                path: header_path.display().to_string(),
                detail: e.to_string(),
            })?;
            write_new_file(&header_path, &text)?;
        }

        let log_path = dir.join("case.jsonl");
        // 续写：从已有内容推出 next_seq
        let next_seq = if log_path.exists() {
            let (records, _) = read_records(&log_path)?;
            records.last().map(|r| r.seq + 1).unwrap_or(1)
        } else {
            1
        };

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| LogError::Io {
                path: log_path.display().to_string(),
                detail: e.to_string(),
            })?;

        Ok(Self {
            dir,
            file,
            next_seq,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 下一个将要写入的序号。
    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// 追加一条事件（缓冲写入）。
    pub fn append(&mut self, event: Event) -> Result<u64, LogError> {
        self.append_inner(event, false)
    }

    /// 追加一条**不可丢失**的事件（强制 fsync）。
    ///
    /// # 何时必须用 durable
    ///
    /// 判据是"这件事发生后再丢，用户会认为自己没做过"：
    /// - **checkpoint 的作答**（§8.3 明确标注的强制写点）：用户点了确认，
    ///   掉电丢失会让他以为没确认过，而重新问一遍是**可接受的**；
    ///   但反过来——用户以为确认了、系统以为没确认——会让流程停在一个
    ///   "用户已经做过动作却看不到进展"的状态，那是更坏的失效。
    /// - `PhaseApproved` / `PhaseRewound`：阶段状态机的位置由它们决定，
    ///   丢了会让恢复出的阶段与用户认知不符。
    ///
    /// 普通 step 事件**不需要**：它们可被重跑，且每次 fsync 会显著拖慢循环。
    pub fn append_durable(&mut self, event: Event) -> Result<u64, LogError> {
        self.append_inner(event, true)
    }

    fn append_inner(&mut self, event: Event, durable: bool) -> Result<u64, LogError> {
        let seq = self.next_seq;
        let rec = Recorded {
            seq,
            event: event.clone(),
        };
        let line = serde_json::to_string(&rec).map_err(|e| LogError::Io {
            path: self.dir.join("case.jsonl").display().to_string(),
            detail: format!("事件序列化失败：{e}"),
        })?;
        writeln!(self.file, "{line}").map_err(|e| LogError::Io {
            path: self.dir.join("case.jsonl").display().to_string(),
            detail: e.to_string(),
        })?;
        if durable {
            self.file.sync_data().map_err(|e| LogError::Io {
                path: self.dir.join("case.jsonl").display().to_string(),
                detail: format!("fsync 失败（durable 事件不得丢）：{e}"),
            })?;
        }
        self.next_seq += 1;
        Ok(seq)
    }

    /// 读回全部记录（只读，不修改日志）。
    pub fn read_all(&self) -> Result<Vec<Recorded>, LogError> {
        let (records, _) = read_records(&self.dir.join("case.jsonl"))?;
        Ok(records)
    }

    /// 仅取事件序列（供崩溃修复与投影）。
    pub fn events(&self) -> Result<Vec<Event>, LogError> {
        Ok(self.read_all()?.into_iter().map(|r| r.event).collect())
    }

    /// 同步外部进程（如 Web 工作台关口确认或 CLI `shangyi checkpoint`）追加的新事件，
    /// 并推进本实例的 `next_seq`，防止多进程追加时出现重号。
    pub fn sync_external(&mut self) -> Result<Vec<Recorded>, LogError> {
        let records = self.read_all()?;
        let watermark = self.next_seq;
        if let Some(last) = records.last() {
            if last.seq + 1 > self.next_seq {
                self.next_seq = last.seq + 1;
            }
        }
        Ok(records
            .into_iter()
            .filter(|r| r.seq >= watermark)
            .collect())
    }
}

/// 读取一个 JSONL 日志文件。
///
/// 返回 `(记录, 行数)`。**任何一行解析失败即整体失败**——理由见模块文档第 3 条。
pub fn read_records(path: &Path) -> Result<(Vec<Recorded>, usize), LogError> {
    if !path.exists() {
        return Ok((Vec::new(), 0));
    }
    let f = File::open(path).map_err(|e| LogError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let mut out = Vec::new();
    let mut n = 0usize;
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line.map_err(|e| LogError::Io {
            path: path.display().to_string(),
            detail: e.to_string(),
        })?;
        n = i + 1;
        if line.trim().is_empty() {
            // 空行：容忍（编辑器与并发追加可能留下），但不计入记录
            continue;
        }
        let rec: Recorded = serde_json::from_str(&line).map_err(|e| LogError::CorruptLine {
            line: i + 1,
            detail: format!("{e}；该行前 200 字节：{}", truncate(&line, 200)),
        })?;
        out.push(rec);
    }
    // 序号必须严格递增：乱序或重号说明文件被外部改过，不能当成正常日志
    for w in out.windows(2) {
        if w[1].seq <= w[0].seq {
            return Err(LogError::CorruptLine {
                line: 0,
                detail: format!(
                    "日志序号非严格递增：{} 之后出现 {}（文件可能被外部修改或拼接错了）",
                    w[0].seq, w[1].seq
                ),
            });
        }
    }
    Ok((out, n))
}

/// 恢复结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeOutcome {
    /// 日志里的全部事件（未被修改）。
    pub events: Vec<Event>,
    /// 崩溃修复**应追加**的事件（可能为空）。
    ///
    /// 它**不由本函数写入**：写不写、何时写是调用方的决定。
    /// 分离的理由：修复本身是纯函数（可测试、可比较），而"把补丁落盘"
    /// 是一个有副作用的动作——两者混在一起就没法在测试里断言"补丁内容正确"
    /// 而不污染测试目录。
    pub repairs: Vec<Event>,
}

impl ResumeOutcome {
    /// 该日志是否需要一个 `Interrupted` 收尾（即上次是崩溃退出）。
    pub fn crashed(&self) -> bool {
        !self.repairs.is_empty()
    }
}

/// 读日志并算出崩溃修复（纯逻辑，不落盘）。
pub fn resume(dir: impl AsRef<Path>) -> Result<ResumeOutcome, LogError> {
    let path = dir.as_ref().join("case.jsonl");
    let (records, _) = read_records(&path)?;
    let events: Vec<Event> = records.into_iter().map(|r| r.event).collect();
    let repairs = interrupted_turn_closers(&events);
    Ok(ResumeOutcome { events, repairs })
}

/// 把修复事件追加进日志（**显式**的落盘动作）。
pub fn apply_repairs(log: &mut CaseLog, repairs: &[Event]) -> Result<usize, LogError> {
    for e in repairs {
        // 修复事件改变了"上一个 turn 是怎么结束的"这一事实 ⇒ durable
        log.append_durable(e.clone())?;
    }
    Ok(repairs.len())
}

fn write_new_file(path: &Path, text: &str) -> Result<(), LogError> {
    let mut f = File::create(path).map_err(|e| LogError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    f.write_all(text.as_bytes()).map_err(|e| LogError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    f.sync_data().map_err(|e| LogError::Io {
        path: path.display().to_string(),
        detail: format!("fsync 失败：{e}"),
    })?;
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{StepId, ToolOutcome, TurnEndReason, TurnId};

    /// 每个测试用独立的临时目录（避免并发互扰）。
    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shangyi-caselog-{tag}-{}-{}",
            std::process::id(),
            // 用纳秒做唯一后缀；不需要密码学强度
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).expect("建临时目录");
        p
    }

    fn header(id: &str) -> CaseHeader {
        CaseHeader {
            case_id: id.into(),
            mode: "scenario".into(),
            playbook_id: "pricing_response".into(),
            created_at: "2026-09-17T00:00:00Z".into(),
            schema_version: CaseHeader::SCHEMA_VERSION,
        }
    }

    fn started(n: u64) -> Event {
        Event::TurnStarted {
            turn: TurnId(n),
            at: format!("2026-09-17T00:00:0{n}Z"),
        }
    }

    // ── 追加与读回 ──

    #[test]
    fn append_then_read_round_trips() {
        let dir = tmp_dir("roundtrip");
        let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
        assert_eq!(log.append(started(1)).unwrap(), 1);
        assert_eq!(log.append(started(2)).unwrap(), 2);

        let recs = log.read_all().unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].seq, 1);
        assert_eq!(recs[1].seq, 2);
        assert_eq!(recs[0].event, started(1));
    }

    #[test]
    fn header_is_written_once_and_is_immutable() {
        let dir = tmp_dir("header");
        let _ = CaseLog::open(&dir, &header("c1")).unwrap();
        assert!(dir.join("header.json").exists());

        // 同 id/模式重开：允许（续写）
        assert!(CaseLog::open(&dir, &header("c1")).is_ok());

        // 改 id：拒绝——会话头是会话的身份，不得被改写
        let mut other = header("c2");
        other.created_at = "2030-01-01T00:00:00Z".into();
        match CaseLog::open(&dir, &other) {
            Err(LogError::HeaderImmutable) => {}
            Err(e) => panic!("期望 HeaderImmutable，实得 {e:?}"),
            Ok(_) => panic!("改写会话头必须被拒"),
        }
    }

    #[test]
    fn reopen_continues_seq_without_reuse() {
        let dir = tmp_dir("reopen");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(started(1)).unwrap();
            log.append(started(2)).unwrap();
        }
        // 重开 ⇒ 序号接着来，**不复用**（复用会让审计无法区分两段历史）
        let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
        assert_eq!(log.next_seq(), 3);
        assert_eq!(log.append(started(3)).unwrap(), 3);
        assert_eq!(log.read_all().unwrap().len(), 3);
    }

    // ── 损坏必须显式 ──

    #[test]
    fn corrupt_line_is_an_error_not_skipped() {
        let dir = tmp_dir("corrupt");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(started(1)).unwrap();
        }
        // 手工插入一行坏数据（模拟磁盘损坏/外部改动）
        let p = dir.join("case.jsonl");
        let mut s = fs::read_to_string(&p).unwrap();
        s.push_str("{这不是 JSON}\n");
        fs::write(&p, s).unwrap();

        // 错误在 **open** 阶段就暴露：诊断损坏的最后时机是"开始用这份日志之前"，
        // 若等到 read_all 才发现，中间可能已经基于残缺历史做了判断。
        match CaseLog::open(&dir, &header("c1")) {
            Err(LogError::CorruptLine { line, .. }) => {
                assert_eq!(line, 2, "应指出是第几行（便于人工修复）：{line}");
            }
            Err(e) => panic!("期望 CorruptLine，实得 {e:?}"),
            Ok(_) => panic!("坏行必须报错而非跳过"),
        }
    }

    /// 序号非递增 ⇒ 报错。这能发现"两个日志被拼接"这类静默损坏。
    #[test]
    fn non_monotonic_seq_is_detected() {
        let dir = tmp_dir("seq");
        let p = dir.join("case.jsonl");
        fs::create_dir_all(&dir).unwrap();
        let _ = CaseLog::open(&dir, &header("c1")).unwrap();
        fs::write(
            &p,
            "{\"seq\":1,\"event\":{\"type\":\"turn_started\",\"turn\":1,\"at\":\"t\"}}\n\
             {\"seq\":1,\"event\":{\"type\":\"turn_started\",\"turn\":2,\"at\":\"t\"}}\n",
        )
        .unwrap();
        match CaseLog::open(&dir, &header("c1")) {
            Err(LogError::CorruptLine { detail, .. }) => {
                assert!(
                    detail.contains("非严格递增"),
                    "错误信息应说明哪里不对：{detail}"
                );
            }
            Err(e) => panic!("期望 CorruptLine，实得 {e:?}"),
            Ok(_) => panic!("序号非递增必须被检出"),
        }
    }

    #[test]
    fn empty_log_reads_as_empty_not_error() {
        let dir = tmp_dir("empty");
        let log = CaseLog::open(&dir, &header("c1")).unwrap();
        assert!(log.read_all().unwrap().is_empty());
        assert_eq!(log.next_seq(), 1);
    }

    // ── 崩溃修复 ──

    /// 崩溃遗留的 turn：resume 必须算出 `Interrupted` 收尾，且**不在读取时落盘**。
    #[test]
    fn resume_detects_crashed_turn_without_writing() {
        let dir = tmp_dir("crash");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(started(1)).unwrap();
            log.append(Event::StepStarted {
                step: StepId::new(TurnId(1), 0),
                at: "2026-09-17T00:00:01Z".into(),
            })
            .unwrap();
            // 崩溃：没有 TurnEnded
        }
        let out = resume(&dir).unwrap();
        assert!(out.crashed(), "缺 TurnEnded 应被识别为崩溃");
        assert!(out.repairs.iter().any(|e| matches!(
            e,
            Event::TurnEnded {
                reason: TurnEndReason::Interrupted,
                ..
            }
        )));

        // 读操作**不得**改写日志：修复必须由调用方显式落盘
        let after = CaseLog::open(&dir, &header("c1")).unwrap();
        assert_eq!(
            after.read_all().unwrap().len(),
            2,
            "resume 是只读的——落盘与否由调用方决定"
        );
    }

    #[test]
    fn apply_repairs_writes_and_makes_resume_clean() {
        let dir = tmp_dir("repair");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(started(1)).unwrap();
        }
        let out = resume(&dir).unwrap();
        assert!(out.crashed());

        let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
        let n = apply_repairs(&mut log, &out.repairs).unwrap();
        assert!(n > 0);

        // 修复后：再 resume 应干净（幂等——否则每次开启都会追加一遍）
        let out2 = resume(&dir).unwrap();
        assert!(!out2.crashed(), "修复后再修复不得产生新事件：{:?}", out2.repairs);
    }

    #[test]
    fn resume_on_clean_log_is_a_no_op() {
        let dir = tmp_dir("clean");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(started(1)).unwrap();
            log.append(Event::TurnEnded {
                turn: TurnId(1),
                reason: TurnEndReason::Completed,
                at: "2026-09-17T00:00:02Z".into(),
            })
            .unwrap();
        }
        let out = resume(&dir).unwrap();
        assert!(!out.crashed(), "正常结束的会话不应被补丁污染");
        assert_eq!(out.events.len(), 2);
    }

    /// 崩溃点的工具调用必须标为**结局未知**（不得是"未开始"）——
    /// 标错会诱导恢复逻辑重放一个已产生副作用的非幂等工具。
    #[test]
    fn crashed_tool_call_is_marked_unknown_through_resume() {
        let dir = tmp_dir("toolunknown");
        {
            let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
            log.append(started(1)).unwrap();
            log.append(Event::ToolCallStarted {
                step: StepId::new(TurnId(1), 0),
                call_id: "call_x".into(),
                name: "case_store_write".into(),
                arguments: "{}".into(),
                at: "2026-09-17T00:00:01Z".into(),
            })
            .unwrap();
        }
        let out = resume(&dir).unwrap();
        let fixed: Vec<&Event> = out
            .repairs
            .iter()
            .filter(|e| matches!(e, Event::ToolCallFinished { .. }))
            .collect();
        assert_eq!(fixed.len(), 1);
        match fixed[0] {
            Event::ToolCallFinished { outcome, call_id, .. } => {
                assert_eq!(call_id, "call_x");
                assert_eq!(
                    *outcome,
                    ToolOutcome::Unknown,
                    "崩溃点的工具必须标为结局未知"
                );
            }
            _ => unreachable!(),
        }
    }

    // ── durable 写 ──

    /// `append_durable` 必须真的落盘：进程级验证方式是把文件读回来
    /// （fsync 的语义无法在单进程测试里完全验证，但至少钉住"内容已写入"）。
    #[test]
    fn durable_append_is_readable_from_disk_immediately() {
        let dir = tmp_dir("durable");
        let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
        log.append_durable(Event::CheckpointAnswered {
            id: "cp1".into(),
            answers: vec!["确认".into()],
            user_actor: "user".into(),
            at: "2026-09-17T00:00:05Z".into(),
        })
        .unwrap();

        // 不经过 CaseLog 对象，直接从磁盘读
        let text = fs::read_to_string(dir.join("case.jsonl")).unwrap();
        assert!(text.contains("checkpoint_answered"), "durable 事件必须已在磁盘上");
        assert!(text.contains("确认"));
    }

    /// checkpoint 的作答是**不可逆的用户动作**，必须走 durable 路径。
    /// 这条测试把"哪些事件必须 durable"钉住，防止将来有人图省事改成缓冲追加。
    #[test]
    fn checkpoint_answers_are_durable_by_contract() {
        let dir = tmp_dir("cpdurable");
        let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
        // 依次记下用户的关键动作，全部走 durable
        log.append_durable(Event::CheckpointOpened {
            id: "cp1".into(),
            phase: "intel".into(),
            items: vec!["对手是三家".into()],
            at: "2026-09-17T00:00:01Z".into(),
        })
        .unwrap();
        log.append_durable(Event::CheckpointAnswered {
            id: "cp1".into(),
            answers: vec!["确认".into()],
            user_actor: "user".into(),
            at: "2026-09-17T00:00:02Z".into(),
        })
        .unwrap();
        log.append_durable(Event::PhaseApproved {
            phase: "intel".into(),
            fingerprint: "abc".into(),
            at: "2026-09-17T00:00:03Z".into(),
        })
        .unwrap();
        assert_eq!(log.read_all().unwrap().len(), 3);
    }

    #[test]
    fn jsonl_format_is_one_event_per_line() {
        let dir = tmp_dir("jsonl");
        let mut log = CaseLog::open(&dir, &header("c1")).unwrap();
        log.append(started(1)).unwrap();
        log.append(started(2)).unwrap();
        let text = fs::read_to_string(dir.join("case.jsonl")).unwrap();
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        assert_eq!(lines.len(), 2, "每行一个事件：{text}");
        for l in lines {
            let v: serde_json::Value = serde_json::from_str(l).expect("每行应是合法 JSON");
            assert!(v.get("seq").is_some());
            assert!(v.get("event").is_some());
        }
    }
}

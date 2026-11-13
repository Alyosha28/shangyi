//! `data_ingest` 侧车客户端：起一个 Python 子进程，把 CSV 解析成结构化描述。
//!
//! # 本模块负责的三件事，以及为什么它们必须在这里
//!
//! ## 1. 进程边界（可信度边界）
//!
//! `data_ingest` 在注册表里的可信度是 [`shangyi_tools::TrustLevel::Sidecar`]：
//! 「进程隔离的 Python 侧车，仅渲染/检索，**不产生数字**」。进程隔离意味着
//! 两件事，本模块都要负责：
//!
//! - **输入输出只走 JSON**（stdin/stdout），不解环境、不共享内存；
//! - **sidecar 算出来的东西不构成数值证据**。故对侧车结果里的每一个「值」，
//!   本模块的类型都只接受**字符串**（见 [`CellValue`]）。
//!
//! 最后一条不是苛求：侧车给出的行数、比例、样本值都是**解析过程的自述**，
//! 需要参与计算时必须由确定性 Rust 引擎从这些原样字符串出发另行计算，
//! 并带上 provenance（铁律 1）。若本模块把 `"12345.67"` 收成 `f64`，
//! 就等于把「侧车产生的数字」混进了数值通路——那正是 `Sidecar` 级别要拦的事。
//!
//! ## 2. 失败即拒绝，且**分层**（不变量②）
//!
//! 失败分三层，混作一团就无法决定应对：
//!
//! | 层 | 类型 | 含义 | 应对 |
//! |---|---|---|---|
//! | 进程/协议 | [`IngestError`] | 起不来、超时、退出码非 0、stdout 不是 JSON | 修环境或修接线 |
//! | 业务拒绝 | [`IngestOutcome::Refused`] | 侧车**正常应答**「这个文件我不解析」 | 换文件 / 显式指定编码 |
//! | 成功 | [`IngestOutcome::Parsed`] | 结构化结果 | 用 |
//!
//! 「文件不存在」属第二层——侧车如实回答了「不存在」，这不是传输故障。
//! 把它并进第一层会让调用方以为侧车坏了，从而去查环境而不是查路径。
//!
//! ## 3. 不许挂死
//!
//! 子进程没有超时保护就会永久挂住整个咨询。本模块**自己**做超时
//! （stdin 写入与 stdout/stderr 读取各起一个线程，主线程轮询 `try_wait`）
//! 而不是依赖 `wait_timeout` 之类的额外依赖：这个边界值得用二十行代码换掉
//! 一个依赖，而且它使超时行为**可测**（见 `tests` 里的 `timeout_kills_a_hanging_sidecar`）。
//!
//! # 已知边界（如实记录）
//!
//! - **超时后子进程被 `kill`**。它可能留下半写的临时文件——本侧车不写文件，
//!   故当前无此风险；若将来 sidecar 要落盘，需在此处加清理或改用进程组。
//! - **解释器只按名字启动**（默认 `python3`），不解析 shebang、不查 PATH
//!   之外的候选。用户机器上若是别的名字（`python`、`py`、conda 环境内的绝对
//!   路径），用 `SHANGYI_PYTHON` 覆盖——这是刻意的单一出口，避免「它到底用了
//!   哪个 Python」在出问题时无法回答。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

// ────────────────────────────────────────────────────────────────────────────
// 错误：进程/协议层
// ────────────────────────────────────────────────────────────────────────────

/// 侧车**未能给出可用应答**（进程或协议层）。
///
/// 与 [`SidecarRefusal`] 的分工：本类型表示「没拿到答案」，那个表示
/// 「拿到了一个『不行』的答案」。前者要修环境，后者要改输入。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IngestError {
    /// 解释器不存在或不可执行。**这是必须类型化的情形**：不少环境里没有
    /// `python3`（只有 `python`、或只有某个虚拟环境里的绝对路径）。
    #[error("找不到 Python 解释器 `{program}`：{detail}。可用环境变量 SHANGYI_PYTHON 指定实际路径")]
    PythonNotFound { program: String, detail: String },

    /// 侧车脚本不存在。若不在启动前检查，`python3 /不存在.py` 只会给出一句
    /// 笼统的 stderr，而真正的原因（部署时漏拷脚本）会被掩盖。
    #[error("侧车脚本不存在：{path}。可用环境变量 SHANGYI_SIDECAR_DIR 指定脚本所在目录")]
    ScriptMissing { path: String },

    /// 启动失败（非「找不到」的其他 io 错误，如权限）。
    #[error("启动侧车失败（{program}）：{detail}")]
    Spawn { program: String, detail: String },

    /// 超时。**不含部分输出**是刻意的：半份输出不该被当成结果。
    #[error("侧车超过 {seconds} 秒未返回，已终止。path={path}")]
    Timeout {
        seconds: u64,
        path: String,
        stderr_tail: String,
    },

    /// 退出码非 0（协议层错误的标志：句柄坏、解释器崩、脚本自身报错）。
    #[error("侧车退出码 {code}（path={path}）：{stderr_tail}")]
    NonZeroExit {
        code: i32,
        path: String,
        stderr_tail: String,
    },

    /// stdout 不是合法 JSON，或形状与契约不符。
    #[error("侧车响应无法解析（path={path}）：{detail}。原始输出前 200 字符：{stdout_head}")]
    BadResponse {
        detail: String,
        path: String,
        stdout_head: String,
    },

    /// 路径含非 UTF-8 字节，无法放进 JSON 请求。
    ///
    /// **不做 lossy 转换**：那会让请求指向另一个文件（或干脆不存在），
    /// 而调用方以为解析的是原目标——这种错位无法从结果里看出来。
    #[error("路径不是合法 UTF-8，无法作为 JSON 传给侧车：{}", .path.display())]
    NonUtf8Path { path: PathBuf },
}

// ────────────────────────────────────────────────────────────────────────────
// 业务拒绝（sidecar 正常应答的 ok:false）
// ────────────────────────────────────────────────────────────────────────────

/// 侧车给出的**类型化拒绝理由**。
///
/// 每一种对应不同的动作，故不能合并（合并后全部退化为「换个文件试试」）：
///
/// | 变体 | 动作 |
/// |---|---|
/// | `FileNotFound` / `NotAFile` | 改路径 |
/// | `PermissionDenied` | 改权限或换文件 |
/// | `UnsupportedFormat` | 换格式（xlsx → 另存为 CSV） |
/// | `EncodingUndetectable` | **问用户要编码**，或用 `encoding` 显式指定 |
/// | `EncodingNotSupported` | 换一个受支持的编码名 |
/// | `MalformedCsv` | 修文件（引号不成对等） |
/// | `BadRequest` | 改参数 |
///
/// `Unknown` 是给「上游新增了 kind 而 Rust 侧还没跟上」留的口子：
/// **如实透传**而不是丢弃——丢掉未知 kind 会让新情形的信息在边界处消失。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidecarErrorKind {
    FileNotFound,
    NotAFile,
    PermissionDenied,
    EmptyFile,
    UnsupportedFormat,
    EncodingUndetectable,
    EncodingNotSupported,
    MalformedCsv,
    BadRequest,
    IoError,
    InternalError,
    #[serde(other)]
    Unknown,
}

/// 一条来自侧车的拒绝。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("侧车拒绝解析（{kind:?}）：{detail}")]
pub struct SidecarRefusal {
    pub kind: SidecarErrorKind,
    /// 侧车写的中文说明（含应对建议）。原样保留——它比任何本地改写都更贴近实际原因。
    pub detail: String,
}

// ────────────────────────────────────────────────────────────────────────────
// 成功结果
// ────────────────────────────────────────────────────────────────────────────

/// 一个样本单元格。
///
/// # 为什么只有 `Text` 与 `Absent` 两个变体
///
/// 这是 `Sidecar` 可信度在类型上的落点：侧车不得产生数字，故本类型**没有**
/// 数值变体。若侧车哪天把样本值发成 JSON 数字，反序列化会**失败**并报
/// [`IngestError::BadResponse`]——错误发生在边界，而不是悄悄流进计算。
///
/// `Absent`（JSON `null`）表示该行**缺这一列**（ragged 行），与空字符串
/// （`Text("")`，表示这一格是空的）是两件事，不做归一。
///
/// 序列化用 `untagged`：`Text(s)` ↔ JSON 字符串、`Absent` ↔ `null`，
/// 与 Python 侧发出的形状一致（便于把结果原样回传给其他环节）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CellValue {
    Text(String),
    Absent,
}

impl CellValue {
    /// 取文本；`Absent` 返回 `None`。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            CellValue::Text(s) => Some(s.as_str()),
            CellValue::Absent => None,
        }
    }

    /// 是否为空（缺列或空串）。
    pub fn is_blank(&self) -> bool {
        match self {
            CellValue::Text(s) => s.trim().is_empty(),
            CellValue::Absent => true,
        }
    }
}

/// 一行样本。**按位置与 [`IngestReport::columns`] 对齐**：`cells[j]` 对应 `columns[j]`。
///
/// # 为什么是位置数组而不是「列名 → 值」的映射（实测否决了映射形态）
///
/// ① 映射的键序**不被 JSON 保证**。Rust 侧若经 `serde_json::Value` 中转，
///    键会按 `BTreeMap` 重排——实测列序被打乱，人眼核对样本行时与表头对不上。
///    数组的顺序由 JSON 语法保证，不依赖任何实现的善意。
/// ② 重复列名在映射里只能容纳一个键，会**静默丢列**；数组形态下该风险不存在。
///
/// 这两条都不是理论顾虑：第一版协议用的就是映射，测试当场打出了乱序的列。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SampleRow {
    pub cells: Vec<CellValue>,
}

impl SampleRow {
    /// 按列名取值（需要 `columns` 才能把位置翻译成名）。
    pub fn get<'a>(&'a self, columns: &[String], column: &str) -> Option<&'a CellValue> {
        columns
            .iter()
            .position(|c| c == column)
            .and_then(|i| self.cells.get(i))
    }

    /// 与 `columns` 配对（供打印/回传；列数不一致时以较短者为准）。
    pub fn zip<'a>(&'a self, columns: &'a [String]) -> Vec<(&'a str, &'a CellValue)> {
        columns
            .iter()
            .zip(self.cells.iter())
            .map(|(c, v)| (c.as_str(), v))
            .collect()
    }
}

impl<'de> Deserialize<'de> for SampleRow {
    /// 期望形状：`["原样字符串" | null, ...]`。
    ///
    /// 元素只接受字符串或 `null`——**数字会在此报错**（见 [`CellValue`]）。
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let cells = Vec::<CellValue>::deserialize(deserializer)?;
        Ok(SampleRow { cells })
    }
}

impl Serialize for SampleRow {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.cells.serialize(serializer)
    }
}

/// 一列的**格式启发式建议**。注意措辞：是建议，不是判定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnHint {
    pub column: String,
    /// `date` / `number` / `text` / `empty` / `ambiguous`。
    pub hint: String,
    /// **依据**（如「在抽样的 3 个非空值中 100.0%（3/3）可解析为日期」）。
    /// 有依据才谈得上「可被模型/用户否决」——没有依据的建议只能被当作结论。
    pub evidence: String,
    #[serde(default)]
    pub non_empty: u64,
    #[serde(default)]
    pub empty: u64,
    #[serde(default)]
    pub examples: Vec<String>,
    /// 同一列的其他成立解读（`ambiguous` 时非空）。
    #[serde(default)]
    pub alternatives: Vec<String>,
}

/// 表头判定（同样是建议 + 依据）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderInfo {
    pub has_header: bool,
    #[serde(default)]
    pub confidence: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub first_row_raw: Vec<String>,
}

/// 文件身份（审计用：这份报告读的是哪个版本的哪个文件）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileInfo {
    pub path: String,
    pub size_bytes: u64,
    pub mtime_ms: i64,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub digest_note: Option<String>,
}

/// 编码判定的过程说明。**方法比结论重要**：`bom` 是文件自己声明的，
/// `sequential_probe` 是试出来的，两者的可信度不同。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncodingDetail {
    pub method: String,
    #[serde(default)]
    pub candidates_tried: Vec<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub note: String,
}

/// 侧车自述的**计算边界**（哪些是它算的、哪些它不做）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ComputationBoundary {
    #[serde(default)]
    pub trust_level: String,
    #[serde(default)]
    pub computed: Vec<String>,
    #[serde(default)]
    pub not_computed: Vec<String>,
    #[serde(default)]
    pub reason: String,
}

/// 解析结果。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IngestReport {
    pub file: FileInfo,
    /// 实际用于解码的编码（`utf-8` / `utf-8-sig` / `gb18030` / …）。
    pub encoding: String,
    pub encoding_detail: EncodingDetail,
    pub delimiter: String,
    /// `sniffer` / `fallback_scan` / `default_comma` / `pinned`——分隔符判定的
    /// 来源。`default_comma` 意味着没有任何候选成立（单列文件），可信度最低。
    pub delimiter_source: String,
    /// 最终列名（已去重、已补空名）。
    pub columns: Vec<String>,
    /// 原始表头单元格（改名前）。两份都留：`columns` 供程序用，原始供人核对。
    #[serde(default)]
    pub columns_raw: Vec<String>,
    pub header: HeaderInfo,
    /// 数据行数。`row_count_exact=false` 时它是**下界**（扫描被上限截断）。
    pub row_count: u64,
    pub row_count_exact: bool,
    #[serde(default)]
    pub scanned_rows: u64,
    #[serde(default)]
    pub scan_limit: u64,
    #[serde(default)]
    pub sample_rows: Vec<SampleRow>,
    #[serde(default)]
    pub sample_rows_truncated: bool,
    #[serde(default)]
    pub column_hints: Vec<ColumnHint>,
    #[serde(default)]
    pub column_count: usize,
    #[serde(default)]
    pub ragged_rows: u64,
    #[serde(default)]
    pub blank_lines: u64,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub computation_boundary: ComputationBoundary,
}

/// 一次解析的**完整结果**：要么成功，要么是侧车的类型化拒绝。
///
/// 用两层枚举而不是 `Result` 包 `Refused`：`Refused` 是一个**成功的应答**，
/// 只是内容说「不行」。若把它塞进 `Err`，调用方会在 `?` 处把它当异常吞掉，
/// 而它携带的信息（编码不可判定 → 该去问用户要编码）恰恰是必须传下去的东西。
#[derive(Debug, Clone, PartialEq)]
pub enum IngestOutcome {
    Parsed(Box<IngestReport>),
    Refused(SidecarRefusal),
}

impl IngestOutcome {
    /// 是否成功解析。
    pub fn is_parsed(&self) -> bool {
        matches!(self, IngestOutcome::Parsed(_))
    }
}

// ────────────────────────────────────────────────────────────────────────────
// 请求
// ────────────────────────────────────────────────────────────────────────────

/// 一次解析请求。
///
/// `path` 存 [`PathBuf`] 而不是 `String`：序列化成 JSON 时才做 UTF-8 检查，
/// 非 UTF-8 路径**报错**而非 `to_string_lossy`。有意的——有损转换会让请求
/// 指向另一个文件（或干脆不存在的路径），而调用方以为解析的是原目标；
/// 这种错位从结果里看不出来。
#[derive(Debug, Clone, PartialEq)]
pub struct IngestRequest {
    pub path: PathBuf,
    pub max_rows: usize,
    pub max_scan_rows: usize,
    /// 显式指定编码（`None` 表示让侧车顺序试探）。
    ///
    /// 为什么允许调用方指定：用户往往**知道**文件是哪个系统导出的，
    /// 这时指定比探测可靠。侧车对指定编码不做自动改写——指定失败即报错。
    pub encoding: Option<String>,
    pub delimiter: Option<String>,
}

impl IngestRequest {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_rows: 20,
            max_scan_rows: 200_000,
            encoding: None,
            delimiter: None,
        }
    }

    pub fn with_max_rows(mut self, n: usize) -> Self {
        self.max_rows = n;
        self
    }

    pub fn with_max_scan_rows(mut self, n: usize) -> Self {
        self.max_scan_rows = n;
        self
    }

    pub fn with_encoding(mut self, enc: impl Into<String>) -> Self {
        self.encoding = Some(enc.into());
        self
    }

    pub fn with_delimiter(mut self, d: impl Into<String>) -> Self {
        self.delimiter = Some(d.into());
        self
    }

    /// 转成待发送的 JSON 对象。**非 UTF-8 路径在此失败**。
    fn to_json(&self) -> Result<serde_json::Value, IngestError> {
        let path = self
            .path
            .to_str()
            .ok_or_else(|| IngestError::NonUtf8Path {
                path: self.path.clone(),
            })?
            .to_string();
        let mut obj = serde_json::json!({
            "path": path,
            "max_rows": self.max_rows,
            "max_scan_rows": self.max_scan_rows,
        });
        // 只有显式指定的键才发出：侧车把「键存在」当作「调用方明确要求」
        if let Some(enc) = &self.encoding {
            obj["encoding"] = serde_json::Value::String(enc.clone());
        }
        if let Some(d) = &self.delimiter {
            obj["delimiter"] = serde_json::Value::String(d.clone());
        }
        Ok(obj)
    }
}

// ────────────────────────────────────────────────────────────────────────────
// 客户端
// ────────────────────────────────────────────────────────────────────────────

/// 默认超时。CSV 解析是本地纯 CPU 工作，60 秒足够解析很大的文件；
/// 超过这个数几乎一定是挂死（例如 stdin 未收到 EOF），而不是「算得慢」。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// 超时轮询间隔。20 ms 在响应速度与 CPU 占用之间取平衡：本地进程的往返
/// 通常在毫秒级，这个粒度让「正常路径」几乎不付额外延迟。
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// 读取 stdout/stderr 的线程栈大小无特殊要求，用标准线程即可。
#[derive(Debug, Clone)]
pub struct IngestClient {
    /// 解释器（默认 `python3`；`SHANGYI_PYTHON` 可覆盖）。
    python: String,
    /// 侧车脚本路径（默认按编译期 `CARGO_MANIFEST_DIR` 推仓库内的 `shangyi/sidecar/`）。
    script: PathBuf,
    timeout: Duration,
}

impl Default for IngestClient {
    fn default() -> Self {
        Self::from_env()
    }
}

impl IngestClient {
    /// 按环境解析解释器与脚本路径。
    ///
    /// 顺序：显式构造器 > `SHANGYI_PYTHON` / `SHANGYI_SIDECAR_DIR` 环境变量 >
    /// 默认值。两个环境变量都读同一处，便于「解释器对了但脚本找不到」时定位。
    pub fn from_env() -> Self {
        let python = std::env::var("SHANGYI_PYTHON")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "python3".to_string());
        let script = match std::env::var("SHANGYI_SIDECAR_DIR") {
            Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir).join("data_ingest.py"),
            _ => default_script_path(),
        };
        Self {
            python,
            script,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// 显式指定解释器与脚本（**测试用**：避免改环境变量带来的竞态）。
    pub fn new(python: impl Into<String>, script: impl Into<PathBuf>) -> Self {
        Self {
            python: python.into(),
            script: script.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn python(&self) -> &str {
        &self.python
    }

    pub fn script(&self) -> &Path {
        &self.script
    }

    /// 解析一个文件。
    ///
    /// 返回 `Err` 表示**没拿到可用答案**（进程/协议层），`Ok(Refused)` 表示
    /// 侧车正常回答「这个文件我不解析」。区分见 [`IngestError`] 的文档。
    pub fn ingest(&self, request: &IngestRequest) -> Result<IngestOutcome, IngestError> {
        // 先查脚本存在性：否则 Python 报的是「can't open file」，
        // 那句 stderr 会把真正的原因（部署漏拷）盖住。
        if !self.script.is_file() {
            return Err(IngestError::ScriptMissing {
                path: self.script.display().to_string(),
            });
        }
        let payload = self.to_request_json(request)?;
        let path_for_err = request.path.display().to_string();

        let raw = self.run_process(&payload, &path_for_err)?;
        let stdout = String::from_utf8_lossy(&raw.stdout).into_owned();
        let head: String = stdout.chars().take(200).collect();

        let value: serde_json::Value =
            serde_json::from_str(&stdout).map_err(|e| IngestError::BadResponse {
                detail: format!("stdout 不是合法 JSON：{e}"),
                path: path_for_err.clone(),
                stdout_head: head.clone(),
            })?;

        let ok = value.get("ok").and_then(|v| v.as_bool()).ok_or_else(|| {
            IngestError::BadResponse {
                detail: "响应缺少布尔字段 `ok`".to_string(),
                path: path_for_err.clone(),
                stdout_head: head.clone(),
            }
        })?;

        if ok {
            let report: IngestReport =
                serde_json::from_value(value).map_err(|e| IngestError::BadResponse {
                    detail: format!("成功响应与契约不符：{e}"),
                    path: path_for_err.clone(),
                    stdout_head: head.clone(),
                })?;
            Ok(IngestOutcome::Parsed(Box::new(report)))
        } else {
            #[derive(Deserialize)]
            struct ErrBody {
                kind: SidecarErrorKind,
                detail: String,
            }
            #[derive(Deserialize)]
            struct ErrEnvelope {
                error: ErrBody,
            }
            let env: ErrEnvelope =
                serde_json::from_value(value).map_err(|e| IngestError::BadResponse {
                    detail: format!("失败响应与契约不符（须为 {{error: {{kind, detail}}}}）：{e}"),
                    path: path_for_err.clone(),
                    stdout_head: head.clone(),
                })?;
            Ok(IngestOutcome::Refused(SidecarRefusal {
                kind: env.error.kind,
                detail: env.error.detail,
            }))
        }
    }

    /// 请求对象 → JSON 文本（非 UTF-8 路径在此失败，见 [`IngestRequest::to_json`]）。
    fn to_request_json(&self, request: &IngestRequest) -> Result<String, IngestError> {
        let value = request.to_json()?;
        serde_json::to_string(&value).map_err(|e| IngestError::BadResponse {
            detail: format!("请求序列化失败：{e}"),
            path: request.path.display().to_string(),
            stdout_head: String::new(),
        })
    }

    /// 起子进程、送 stdin、读回 stdout/stderr，带超时。
    fn run_process(&self, payload: &str, path_for_err: &str) -> Result<RawOutput, IngestError> {
        let mut child = Command::new(&self.python)
            .arg(&self.script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => IngestError::PythonNotFound {
                    program: self.python.clone(),
                    detail: e.to_string(),
                },
                _ => IngestError::Spawn {
                    program: self.python.clone(),
                    detail: e.to_string(),
                },
            })?;

        // stdin 的写入**必须在独立线程**：若子进程不读 stdin（超时用例正是
        // 这种情形），主线程会阻塞在 write 上，于是超时逻辑永远走不到——
        // 「带超时」就成了摆设。
        let stdin = child.stdin.take();
        let payload_owned = payload.to_string();
        let writer = std::thread::spawn(move || {
            if let Some(mut si) = stdin {
                let _ = si.write_all(payload_owned.as_bytes());
                // 显式 drop 以发出 EOF：侧车 `sys.stdin.buffer.read()` 靠它返回。
                let _ = si.flush();
            }
        });

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let out_reader = std::thread::spawn(move || read_all(stdout));
        let err_reader = std::thread::spawn(move || read_all(stderr));

        let exited = wait_with_timeout(&mut child, self.timeout);
        if !exited {
            // 超时：杀掉子进程并回收，否则会留下僵尸进程。
            let _ = child.kill();
            let _ = child.wait();
        }
        // 无论超时与否都要 join：子进程已退出或已被杀，管道会关闭，
        // 读线程随即结束（不会永久挂住）。
        let _ = writer.join();
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();

        if !exited {
            return Err(IngestError::Timeout {
                seconds: self.timeout.as_secs(),
                path: path_for_err.to_string(),
                stderr_tail: tail(&String::from_utf8_lossy(&stderr)),
            });
        }

        let status = child.wait().map_err(|e| IngestError::Spawn {
            program: self.python.clone(),
            detail: format!("回收子进程失败：{e}"),
        })?;
        if !status.success() {
            return Err(IngestError::NonZeroExit {
                code: status.code().unwrap_or(-1),
                path: path_for_err.to_string(),
                stderr_tail: tail(&String::from_utf8_lossy(&stderr)),
            });
        }
        Ok(RawOutput { stdout, stderr })
    }
}

struct RawOutput {
    stdout: Vec<u8>,
    #[allow(dead_code)] // 保留：排查时值得看一眼侧车的告警输出
    stderr: Vec<u8>,
}

/// 把子进程的某个管道读到 EOF。
fn read_all<R: Read>(reader: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut r) = reader {
        let _ = r.read_to_end(&mut buf);
    }
    buf
}

/// 轮询等待子进程退出；返回是否在超时前退出。
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            // try_wait 自身出错（极少见）：按「未能正常结束」处理，
            // 让调用方拿到 Timeout/退出码错误而不是 panic。
            Err(_) => return true,
        }
    }
}

/// 取文本尾部若干字符（stderr 可能很长，塞进错误信息前要收窄）。
fn tail(text: &str) -> String {
    const LIMIT: usize = 500;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let skip = trimmed.chars().count() - LIMIT;
    let tail: String = trimmed.chars().skip(skip).collect();
    format!("…{tail}")
}

/// 默认的侧车脚本位置。
///
/// # 为什么用编译期 `CARGO_MANIFEST_DIR`
///
/// 侧车不是编译产物（它是被解释执行的 .py），故必须**定位**而不是嵌入。
/// 用编译期常量推出仓库内的固定相对路径 `<仓库>/shangyi/sidecar/data_ingest.py`：
/// 本 crate 在 `<仓库>/shangyi/crates/shangyi-intel`，向上一级到 `crates`、
/// 再一级到 `shangyi/`。
///
/// 这也意味着**安装到别处后必须用 `SHANGYI_SIDECAR_DIR` 指定**——
/// 该路径会指向构建机的源码树。这条限制写在文档里而不是靠猜。
pub fn default_script_path() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // <repo>/shangyi/crates/shangyi-intel → <repo>/shangyi
    let shangyi_root = manifest
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.clone());
    shangyi_root.join("sidecar").join("data_ingest.py")
}

/// 便捷入口：按环境配置解析一个文件。
pub fn ingest(request: &IngestRequest) -> Result<IngestOutcome, IngestError> {
    IngestClient::from_env().ingest(request)
}

// ────────────────────────────────────────────────────────────────────────────
// 测试
// ────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// 临时目录（不引入 `tempfile` 依赖：三个用例不值得多一个 crate）。
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            // 进程 id + 纳秒时间戳：并发跑测试的多个进程互不冲突
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            path.push(format!("shangyi-ingest-{}-{}-{}", tag, std::process::id(), nanos));
            std::fs::create_dir_all(&path).expect("建临时目录");
            Self { path }
        }

        fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
            let p = self.path.join(name);
            std::fs::write(&p, bytes).expect("写临时文件");
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// 本机可用的解释器与真实脚本（都来自环境，故测试在别的机器上也不需要改）。
    fn client() -> IngestClient {
        IngestClient::from_env()
    }

    const UTF8_CSV: &str = "日期,销售额,客户名称\n2024-01-05,12345.67,北京某某商贸有限公司\n2024-02-05,23456.78,天津某某科技有限公司\n2024-03-05,34567.89,上海某某贸易有限公司\n";

    /// 默认脚本路径应指向仓库内的侧车（本 crate 位于 <repo>/shangyi/crates/shangyi-intel）。
    #[test]
    fn default_script_path_points_at_repo_sidecar() {
        let p = default_script_path();
        assert!(
            p.ends_with("shangyi/sidecar/data_ingest.py"),
            "实得 {}",
            p.display()
        );
        assert!(p.is_file(), "侧车脚本应在仓库内：{}", p.display());
    }

    /// 正常解析：列名是正确的中文（不是乱码）、值保持原样字符串。
    #[test]
    fn parses_a_utf8_csv_with_chinese_columns() {
        let dir = TempDir::new("ok");
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());

        let outcome = client()
            .ingest(&IngestRequest::new(&csv).with_max_rows(5))
            .expect("正常文件不应是进程/协议层错误");
        let report = match outcome {
            IngestOutcome::Parsed(r) => r,
            IngestOutcome::Refused(r) => panic!("不该被拒绝：{r}"),
        };

        assert_eq!(report.encoding, "utf-8");
        assert_eq!(report.delimiter, ",");
        assert_eq!(report.columns, vec!["日期", "销售额", "客户名称"]);
        assert_eq!(report.row_count, 3);
        assert!(report.row_count_exact);
        assert!(report.header.has_header);

        let hints: BTreeMap<&str, &str> = report
            .column_hints
            .iter()
            .map(|h| (h.column.as_str(), h.hint.as_str()))
            .collect();
        assert_eq!(hints.get("日期"), Some(&"date"));
        assert_eq!(hints.get("销售额"), Some(&"number"));
        assert_eq!(hints.get("客户名称"), Some(&"text"));
        // 每条建议都必须带依据——否则模型/用户无从否决
        for h in &report.column_hints {
            assert!(!h.evidence.is_empty(), "列建议缺依据：{h:?}");
        }

        // **可信度边界**：样本值必须是原样字符串，不被转成数字
        let row = &report.sample_rows[0];
        assert_eq!(row.cells.len(), report.columns.len(), "样本行应与列数对齐");
        assert_eq!(
            row.get(&report.columns, "销售额"),
            Some(&CellValue::Text("12345.67".into()))
        );
        assert_eq!(
            row.get(&report.columns, "客户名称").and_then(|v| v.as_text()),
            Some("北京某某商贸有限公司")
        );
        // 位置对齐：cells[j] 对应 columns[j]（这正是被 Value 中转打乱、故改用数组的那条）
        assert_eq!(
            row.zip(&report.columns)[0],
            ("日期", &CellValue::Text("2024-01-05".into()))
        );
        assert_eq!(report.computation_boundary.trust_level, "sidecar");
    }

    /// GBK 编码的 CSV（中文中小企业导出的常见形态）要解出正确中文。
    #[test]
    fn parses_a_gbk_csv_without_mojibake() {
        let dir = TempDir::new("gbk");
        // 手工构造 GBK 字节：避免依赖测试机上是否有 GBK 编码器。
        // 「日期」= C8 D5 C6 DA，「销售额」= CF FA CA DB B6 EE，「备注」= B1 B8 D7 A2
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\xc8\xd5\xc6\xda,\xcf\xfa\xca\xdb\xb6\xee\n");
        bytes.extend_from_slice(b"2024-01-05,12345.67\n");
        bytes.extend_from_slice(b"2024-02-05,23456.78\n");
        let csv = dir.write("gbk.csv", &bytes);

        let outcome = client()
            .ingest(&IngestRequest::new(&csv))
            .expect("GBK 文件不应是进程层错误");
        let report = match outcome {
            IngestOutcome::Parsed(r) => r,
            IngestOutcome::Refused(r) => panic!("GBK 应能被探测出来：{r}"),
        };
        assert_eq!(report.encoding, "gb18030");
        assert_eq!(report.encoding_detail.method, "sequential_probe");
        assert_eq!(report.columns, vec!["日期", "销售额"]);
        assert!(
            report.encoding_detail.note.contains("不是"),
            "探测结论必须自带「这不是确定结论」的说明：{}",
            report.encoding_detail.note
        );
    }

    /// 文件不存在：这是**侧车的类型化拒绝**，不是进程层错误。
    ///
    /// 这条区分是有意的：`Err` 意味着「去查环境」，`Refused` 意味着「去查路径」。
    #[test]
    fn missing_file_is_a_typed_refusal_not_a_transport_error() {
        let dir = TempDir::new("missing");
        let missing = dir.path.join("真的不存在.csv");

        let outcome = client()
            .ingest(&IngestRequest::new(&missing))
            .expect("侧车正常应答「不存在」，不该是进程层错误");
        match outcome {
            IngestOutcome::Refused(r) => {
                assert_eq!(r.kind, SidecarErrorKind::FileNotFound);
                assert!(r.detail.contains("不存在"), "详情：{}", r.detail);
            }
            IngestOutcome::Parsed(_) => panic!("不存在的文件不该解析成功"),
        }
    }

    /// xlsx（ZIP 容器）：必须报**不支持**，而不是给出空数据或猜列名。
    #[test]
    fn xlsx_container_is_refused_as_unsupported_format() {
        let dir = TempDir::new("xlsx");
        let mut bytes = b"PK\x03\x04".to_vec();
        bytes.extend_from_slice(&[0u8; 64]);
        let path = dir.write("表.xlsx", &bytes);

        let outcome = client().ingest(&IngestRequest::new(&path)).expect("应是拒绝而非崩溃");
        match outcome {
            IngestOutcome::Refused(r) => {
                assert_eq!(r.kind, SidecarErrorKind::UnsupportedFormat);
                assert!(r.detail.contains("CSV"), "拒绝理由须给出应对：{}", r.detail);
            }
            IngestOutcome::Parsed(_) => panic!("xlsx 不该被当成 CSV 解析成功"),
        }
    }

    /// **Python 不存在**必须返回类型化错误，不得 panic。
    #[test]
    fn missing_python_is_a_typed_error_not_a_panic() {
        let dir = TempDir::new("nopy");
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let c = IngestClient::new("/nonexistent/python3", default_script_path());
        let err = c
            .ingest(&IngestRequest::new(&csv))
            .expect_err("解释器不存在时必须失败");
        match err {
            IngestError::PythonNotFound { program, .. } => {
                assert_eq!(program, "/nonexistent/python3");
            }
            other => panic!("应报 PythonNotFound，实得 {other:?}"),
        }
        // 错误信息要指出出口（环境变量），否则用户只能猜
        assert!(format!("{}", IngestError::PythonNotFound {
            program: "/nonexistent/python3".into(),
            detail: "No such file".into()
        })
        .contains("SHANGYI_PYTHON"));
    }

    /// 脚本不存在：要在启动子进程**之前**就被识别（否则只有一句含糊的 stderr）。
    #[test]
    fn missing_script_is_reported_before_spawning() {
        let dir = TempDir::new("noscript");
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let c = IngestClient::new("python3", dir.path.join("没有这个脚本.py"));
        let err = c.ingest(&IngestRequest::new(&csv)).expect_err("脚本缺失必须失败");
        assert!(matches!(err, IngestError::ScriptMissing { .. }), "实得 {err:?}");
    }

    /// 超时：挂死的侧车必须被终止，且报 `Timeout`。
    ///
    /// 用一个「读 stdin 后睡 300 秒」的脚本模拟挂死。若超时逻辑失效，
    /// 本测试会明显变慢（而不是静默通过），这比断言时间更可靠。
    #[test]
    fn timeout_kills_a_hanging_sidecar() {
        let dir = TempDir::new("timeout");
        let script = dir.write(
            "hang.py",
            b"import sys, time\nsys.stdin.buffer.read()\ntime.sleep(300)\n",
        );
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let c = IngestClient::new("python3", script).with_timeout(Duration::from_millis(400));

        let started = Instant::now();
        let err = c.ingest(&IngestRequest::new(&csv)).expect_err("挂死必须超时");
        assert!(matches!(err, IngestError::Timeout { .. }), "实得 {err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "超时应及时返回，实际耗时 {:?}",
            started.elapsed()
        );
    }

    /// 退出码非 0：报 `NonZeroExit` 并带上 stderr 尾部（供排查）。
    #[test]
    fn non_zero_exit_is_surfaced_with_stderr() {
        let dir = TempDir::new("crash");
        let script = dir.write(
            "crash.py",
            b"import sys\nsys.stderr.write('boom\\n')\nsys.exit(3)\n",
        );
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let c = IngestClient::new("python3", script);
        let err = c.ingest(&IngestRequest::new(&csv)).expect_err("退出码 3 必须失败");
        match err {
            IngestError::NonZeroExit { code, stderr_tail, .. } => {
                assert_eq!(code, 3);
                assert!(stderr_tail.contains("boom"), "stderr 应被带回：{stderr_tail}");
            }
            other => panic!("实得 {other:?}"),
        }
    }

    /// stdout 不是 JSON：报 `BadResponse`（协议层），不猜测。
    #[test]
    fn non_json_stdout_is_a_bad_response() {
        let dir = TempDir::new("garbage");
        let script = dir.write("garbage.py", b"print('not json')\n");
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let c = IngestClient::new("python3", script);
        let err = c.ingest(&IngestRequest::new(&csv)).expect_err("非 JSON 输出必须失败");
        match err {
            IngestError::BadResponse { detail, stdout_head, .. } => {
                assert!(stdout_head.contains("not json"), "原始输出应被带回");
                assert!(detail.contains("JSON"), "{detail}");
            }
            other => panic!("实得 {other:?}"),
        }
    }

    /// **样本值不得是数字**：若侧车违约发出数字，反序列化必须在边界失败。
    ///
    /// 这是 `Sidecar` 可信度（「不产生数字」）在类型层面的可执行断言。
    #[test]
    fn numeric_sample_values_are_rejected_at_the_boundary() {
        let dir = TempDir::new("numeric");
        let script = dir.write(
            "bad_sidecar.py",
            r#"import sys, json
sys.stdin.buffer.read()
sys.stdout.write(json.dumps({"ok": True, "file": {"path": "/x", "size_bytes": 1, "mtime_ms": 0},
  "encoding": "utf-8", "encoding_detail": {"method": "bom"}, "delimiter": ",",
  "delimiter_source": "sniffer", "columns": ["金额"], "header": {"has_header": True},
  "row_count": 1, "row_count_exact": True, "sample_rows": [[12345.67]]}))
"#
            .as_bytes(),
        );
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let c = IngestClient::new("python3", script);
        let err = c.ingest(&IngestRequest::new(&csv)).expect_err("数字样本值必须被拒绝");
        assert!(
            matches!(err, IngestError::BadResponse { .. }),
            "应在边界报 BadResponse，实得 {err:?}"
        );
    }

    /// 位置数组的列序必须与 `columns` 一致。
    ///
    /// # 这条测试的由来（一个被实测抓出的协议缺陷）
    ///
    /// 第一版协议把样本行发成「列名 → 值」的 JSON 对象，本模块用自定义
    /// `Deserialize` 走 `MapAccess` 以求保序。看起来无懈可击——但**没通过**：
    /// 响应先被解析成 `serde_json::Value` 再 `from_value`，而 `Value::Object`
    /// 底层是 `BTreeMap`，键在那一跳就被按码点排序了，自定义 Visitor 拿到的
    /// 已经是排好序的序列。
    ///
    /// 修法不是在 Rust 侧绕过 `Value`（那只是把问题藏起来），而是**改协议**：
    /// 样本行改成位置数组，顺序由 JSON 语法保证。本测试用**多个列**来钉住它
    /// ——单列文件无论怎么排序都看不出问题。
    #[test]
    fn sample_row_order_follows_the_columns_field() {
        let dir = TempDir::new("order");
        // 用**三列以上**的文件：单列文件无论怎么重排都看不出问题。
        let csv = dir.write("sales.csv", UTF8_CSV.as_bytes());
        let outcome = client().ingest(&IngestRequest::new(&csv)).expect("不该是进程层错误");
        let report = match outcome {
            IngestOutcome::Parsed(r) => r,
            IngestOutcome::Refused(r) => panic!("不该被拒绝：{r}"),
        };
        let ordered = report.sample_rows[0].zip(&report.columns);
        let names: Vec<&str> = ordered.iter().map(|(c, _)| *c).collect();
        assert_eq!(names, report.columns, "样本行的取值顺序必须与 columns 一致");
        // 再钉一层内容：日期在第一格、客户名在第三格
        assert_eq!(ordered[0].1.as_text(), Some("2024-01-05"));
        assert_eq!(ordered[2].1.as_text(), Some("北京某某商贸有限公司"));
    }

    /// 可选参数**只在显式给出时**才出现在请求 JSON 里。
    ///
    /// 侧车把「键存在」当作「调用方明确要求」：`encoding: null` 与
    /// 「没有 encoding 键」在它那边是同一件事（都走探测），但 `delimiter`
    /// 一旦发成 `null` 会撞上它「非 null 且非法即拒绝」的检查。故这里钉住
    /// 「不给就不发键」，避免调用方在不知情时改变了侧车的判定路径。
    #[test]
    fn optional_request_keys_are_omitted_when_not_set() {
        let req = IngestRequest::new("/tmp/x.csv");
        let json = req.to_json().expect("UTF-8 路径应能序列化");
        assert!(json.get("encoding").is_none(), "未指定编码时不应发 encoding 键");
        assert!(json.get("delimiter").is_none(), "未指定分隔符时不应发 delimiter 键");
        assert_eq!(json["max_rows"], 20);
        assert_eq!(json["max_scan_rows"], 200_000);

        let req2 = req.with_encoding("gbk").with_delimiter(";");
        let json2 = req2.to_json().expect("应能序列化");
        assert_eq!(json2["encoding"], "gbk");
        assert_eq!(json2["delimiter"], ";");
    }

    /// 非 UTF-8 路径必须**报错**，不得有损转换后送到另一个文件上去。
    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_refused_instead_of_lossy_converted() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let bad = PathBuf::from(OsStr::from_bytes(b"/tmp/\xff\xfe.csv"));
        let req = IngestRequest::new(bad);
        let err = req.to_json().expect_err("非 UTF-8 路径必须失败");
        assert!(matches!(err, IngestError::NonUtf8Path { .. }), "实得 {err:?}");
    }

    /// 显式指定编码的路径（用户给了领域知识时用）。
    #[test]
    fn pinned_encoding_is_forwarded() {
        let dir = TempDir::new("pinned");
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\xc8\xd5\xc6\xda\n");
        bytes.extend_from_slice(b"2024-01-05\n");
        let csv = dir.write("gbk.csv", &bytes);

        let outcome = client()
            .ingest(&IngestRequest::new(&csv).with_encoding("gbk"))
            .expect("指定编码不应是进程层错误");
        match outcome {
            IngestOutcome::Parsed(r) => {
                assert_eq!(r.encoding, "gbk");
                assert_eq!(r.encoding_detail.method, "pinned");
                assert_eq!(r.columns, vec!["日期"]);
            }
            IngestOutcome::Refused(r) => panic!("指定 gbk 应成功：{r}"),
        }
    }

    /// 扫描上限生效时，`row_count_exact` 必须为 false（**行数诚实**）。
    #[test]
    fn truncated_scan_reports_inexact_row_count() {
        let dir = TempDir::new("trunc");
        let mut csv_text = String::from("日期,值\n");
        for i in 0..50 {
            csv_text.push_str(&format!("2024-01-{:02}, {}\n", i % 28 + 1, i));
        }
        let csv = dir.write("long.csv", csv_text.as_bytes());

        let outcome = client()
            .ingest(&IngestRequest::new(&csv).with_max_rows(2).with_max_scan_rows(10))
            .expect("大文件不应是进程层错误");
        match outcome {
            IngestOutcome::Parsed(r) => {
                assert!(!r.row_count_exact, "扫描被截断时不得声称行数精确");
                assert!(r.row_count < 50);
                assert_eq!(r.sample_rows.len(), 2);
                assert!(
                    r.notes.iter().any(|n| n.contains("下界")),
                    "notes 应说明 row_count 是下界：{:?}",
                    r.notes
                );
            }
            IngestOutcome::Refused(r) => panic!("不该被拒绝：{r}"),
        }
    }
}

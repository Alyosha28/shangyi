//! `shangyi report` 子命令实现：从 `case.jsonl` 严格读取 `TrustLevel::Engine` 产物渲染 Markdown 报告。
//!
//! 严格遵守铁律 1 与铁律 2：
//! - 绝不重算、合并、推算任何数字；
//! - 绝不把 `sandbox_exec`（`TrustLevel::Exploratory`）输出当作报告事实；
//! - 绝不为缺少引擎产物的空案或纯对话案例生成散文报告；
//! - 若存在 `report_render` 产物，必须通过 [`shangyi_report::Report::check_provenance`] 自检。

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;
use shangyi_core::case_log::{CaseHeader, Recorded};
use shangyi_core::session::{Event, ToolOutcome};
use shangyi_report::Report;

use crate::ReportFormat;
use crate::run::{RunFailure, find_case_dir_in, shangyi_home};

const PDF_ENGINES: &[&str] = &[
    "xelatex",
    "lualatex",
    "tectonic",
    "typst",
    "weasyprint",
    "wkhtmltopdf",
    "pdflatex",
];

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

struct TempCleanup(Option<PathBuf>);

impl TempCleanup {
    fn new(path: PathBuf) -> Self {
        Self(Some(path))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for TempCleanup {
    fn drop(&mut self) {
        if let Some(ref p) = self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[derive(Debug)]
struct CaseSource {
    case_id: String,
    log_path: PathBuf,
    header: Option<CaseHeader>,
}

#[derive(Debug, Clone)]
struct ToolInvocation {
    name: String,
    arguments: Value,
    at: String,
}

#[derive(Debug, Clone)]
struct CheckpointEntry {
    id: String,
    phase: String,
    items: Vec<String>,
    status: &'static str,
    answers: Vec<String>,
    actor: Option<String>,
    at: String,
}

#[derive(Debug, Default)]
struct ExtractedReportState {
    scope: Option<String>,
    scope_reason: Option<String>,
    phases: Vec<String>,
    checkpoints: Vec<CheckpointEntry>,
    gamespec_args: Option<Value>,
    solve: Option<Value>,
    select: Option<Value>,
    consequence: Option<Value>,
    discovery: Option<Value>,
    recommend: Option<Value>,
    report_model: Option<Report>,
    scoped_result: Option<Value>,
    conditional: Option<Value>,
    constraints: Vec<Value>,
    constraint_check: Option<Value>,
    costs: BTreeMap<String, Value>,
    research_records: Vec<Value>,
    poi_surveys: Vec<Value>,
    web_sources: BTreeSet<String>,
    kb_queries: Vec<String>,
    exploratory_calls: usize,
    failed_tools: Vec<(String, String)>,
    bad_lines: usize,
}

pub fn run_report(
    case_ref: &str,
    format: ReportFormat,
    output: Option<&str>,
) -> Result<(), RunFailure> {
    let source = resolve_case_source(case_ref)?;
    let state = extract_case_state(&source)?;

    if let Some(ref rep) = state.report_model {
        rep.check_provenance().map_err(|e| {
            RunFailure::Session(format!(
                "案例 `{}` 的报告模型未通过数字溯源校验（{e}），依据铁律 1 拒绝交付。",
                source.case_id
            ))
        })?;
    }

    if !has_authoritative_artifacts(&state) {
        let extra = if state.exploratory_calls > 0 {
            format!(
                "（日志中仅有 {} 次 `sandbox_exec` 探索性沙箱调用，属于 `TrustLevel::Exploratory`，严禁作为报告依据。）",
                state.exploratory_calls
            )
        } else {
            String::new()
        };
        return Err(RunFailure::Session(format!(
            "案例 `{}` 尚无任何 `TrustLevel::Engine` 确定性工具产出（无求解/选择/成本精算/条件交付/报告模型），依据铁律 1 拒绝仅凭对话生成报告。{extra}",
            source.case_id
        )));
    }

    let md = format_markdown_report(&source, &state);
    match format {
        ReportFormat::Md => match output.map(str::trim) {
            None | Some("-") => {
                print!("{md}");
                Ok(())
            }
            Some("") => Err(RunFailure::Session("输出路径不能为空".into())),
            Some(path_str) => {
                let out_path = PathBuf::from(path_str);
                if out_path.is_dir() {
                    return Err(RunFailure::Io(format!(
                        "输出路径 `{}` 是目录，请指定具体文件路径",
                        out_path.display()
                    )));
                }
                write_atomic_bytes(&out_path, "md", md.as_bytes())?;
                eprintln!("已导出 Markdown 报告：{}", out_path.display());
                Ok(())
            }
        },
        ReportFormat::Pdf | ReportFormat::Docx => {
            if output.map(str::trim) == Some("-") {
                return Err(RunFailure::Session(format!(
                    "拒绝向标准输出写入二进制 {} 文件；请指定 `--output <文件路径>`，或省略 `--output` 默认保存到 `./{}.{}`。",
                    format.as_str(),
                    sanitize_filename(&source.case_id),
                    format.as_str()
                )));
            }
            let out_path = match output.map(str::trim) {
                Some("") => return Err(RunFailure::Session("输出路径不能为空".into())),
                Some(p) => PathBuf::from(p),
                None => PathBuf::from(format!(
                    "{}.{}",
                    sanitize_filename(&source.case_id),
                    format.as_str()
                )),
            };
            export_via_pandoc(case_ref, &source.case_id, format, &out_path, &md)
        }
    }
}

fn sanitize_filename(raw: &str) -> String {
    let mut cleaned: String = raw
        .trim()
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '\0') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    while cleaned.contains("..") {
        cleaned = cleaned.replace("..", "__");
    }
    if cleaned.trim_matches(['_', '.']).is_empty() {
        "shangyi-report".to_string()
    } else {
        cleaned
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn probe_program(bin_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join(bin_name);
        if is_executable_file(&cand) {
            return Some(cand);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{bin_name}.exe"));
            if is_executable_file(&exe) {
                return Some(exe);
            }
        }
    }
    None
}

fn sibling_temp_path(out_path: &Path, ext: &str) -> PathBuf {
    let parent = out_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = out_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(sanitize_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shangyi-report".to_string());
    let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
    parent.join(format!(".{stem}.shangyi-tmp-{}-{seq}.{ext}", std::process::id()))
}

fn ensure_parent_dir(out_path: &Path) -> Result<(), RunFailure> {
    if let Some(parent) = out_path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| {
            RunFailure::Io(format!("无法创建输出目录 `{}`：{e}", parent.display()))
        })?;
    }
    Ok(())
}

fn write_atomic_bytes(out_path: &Path, ext: &str, bytes: &[u8]) -> Result<(), RunFailure> {
    ensure_parent_dir(out_path)?;
    let tmp_path = sibling_temp_path(out_path, ext);
    let mut guard = TempCleanup::new(tmp_path.clone());
    {
        let mut f = std::fs::File::create(&tmp_path).map_err(|e| {
            RunFailure::Io(format!("创建临时文件 `{}` 失败：{e}", tmp_path.display()))
        })?;
        f.write_all(bytes).map_err(|e| {
            RunFailure::Io(format!("写入临时文件 `{}` 失败：{e}", tmp_path.display()))
        })?;
        f.flush().map_err(|e| {
            RunFailure::Io(format!("刷新临时文件 `{}` 失败：{e}", tmp_path.display()))
        })?;
    }
    std::fs::rename(&tmp_path, out_path).map_err(|e| {
        RunFailure::Io(format!("写入目标文件 `{}` 失败：{e}", out_path.display()))
    })?;
    guard.disarm();
    Ok(())
}

fn export_via_pandoc(
    case_ref: &str,
    case_id: &str,
    format: ReportFormat,
    out_path: &Path,
    md: &str,
) -> Result<(), RunFailure> {
    if out_path.is_dir() {
        return Err(RunFailure::Io(format!(
            "输出路径 `{}` 是目录，请指定具体文件路径",
            out_path.display()
        )));
    }

    let Some(pandoc_path) = probe_program("pandoc") else {
        return Err(RunFailure::Session(format!(
            "未在 PATH 中找到 `pandoc`，无法导出 {} 文件（商奕拒绝写入伪造后缀或空文件）。\n\
             安装方法（macOS 示例）：`brew install pandoc`\n\
             或改用以下方式：\n\
             1. `shangyi report {case_ref} --format md > report.md`（或 `--output report.md`）\n\
             2. 运行 `shangyi start`，在 Web 工作台打开案例「{case_id}」，点击「导出 Markdown / 打印 PDF」。",
            format.as_str()
        )));
    };

    let pdf_engine = if format == ReportFormat::Pdf {
        let Some(engine) = PDF_ENGINES
            .iter()
            .copied()
            .find(|eng| probe_program(eng).is_some())
        else {
            return Err(RunFailure::Session(format!(
                "已找到 `pandoc`（{}），但 PATH 中未检测到可用的 PDF 排版引擎（已依次探测：{}），无法生成 PDF（商奕拒绝写入伪造后缀或空文件）。\n\
                 中文 Markdown 推荐安装 `xelatex`（BasicTeX / TeX Live）、`typst` 或 `weasyprint`。\n\
                 或立即改用：\n\
                 1. `shangyi report {case_ref} --format md > report.md`\n\
                 2. `shangyi report {case_ref} --format docx --output {}.docx`\n\
                 3. 运行 `shangyi start`，在 Web 工作台打开案例「{case_id}」，点击「导出 Markdown / 打印 PDF」。",
                pandoc_path.display(),
                PDF_ENGINES.join(" / "),
                sanitize_filename(case_id)
            )));
        };
        Some(engine)
    } else {
        None
    };

    ensure_parent_dir(out_path)?;
    let tmp_path = sibling_temp_path(out_path, format.as_str());
    let mut tmp_guard = TempCleanup::new(tmp_path.clone());

    let mut cmd = Command::new(&pandoc_path);
    cmd.arg("--from=markdown+pipe_tables+strikeout")
        .arg("--standalone")
        .arg("-t")
        .arg(format.as_str())
        .arg("-o")
        .arg(&tmp_path);
    if let Some(engine) = pdf_engine {
        cmd.arg(format!("--pdf-engine={engine}"));
        if matches!(engine, "xelatex" | "lualatex" | "tectonic") {
            if let Ok(font) = std::env::var("SHANGYI_PDF_FONT") {
                let font = font.trim();
                if !font.is_empty() {
                    cmd.arg("-V").arg(format!("CJKmainfont={font}"));
                    cmd.arg("-V").arg(format!("mainfont={font}"));
                }
            }
        }
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        RunFailure::Io(format!("启动 `pandoc`（{}）失败：{e}", pandoc_path.display()))
    })?;
    let write_res = if let Some(mut stdin) = child.stdin.take() {
        let r = stdin.write_all(md.as_bytes());
        drop(stdin);
        r
    } else {
        Ok(())
    };
    let output = child
        .wait_with_output()
        .map_err(|e| RunFailure::Io(format!("等待 `pandoc` 完成失败：{e}")))?;

    if !output.status.success() {
        let stderr_raw = String::from_utf8_lossy(&output.stderr);
        let stderr_summary: String = stderr_raw.trim().chars().take(600).collect();
        let stderr_block = if stderr_summary.is_empty() {
            "（pandoc 未输出 stderr 详情）".to_string()
        } else {
            stderr_summary
        };
        let font_hint = if format == ReportFormat::Pdf {
            "\n若因中文字体缺失导致排版失败，可设置 `SHANGYI_PDF_FONT=<字体名>`（如 `PingFang SC` / `Noto Serif CJK SC`）后重试。"
        } else {
            ""
        };
        return Err(RunFailure::Session(format!(
            "`pandoc` 转换 {} 失败（退出码 {:?}）：{stderr_block}{font_hint}\n\
             或改用：\n\
             1. `shangyi report {case_ref} --format md > report.md`\n\
             2. 运行 `shangyi start`，在 Web 工作台打开案例「{case_id}」，点击「导出 Markdown / 打印 PDF」。",
            format.as_str(),
            output.status.code()
        )));
    }

    if let Err(e) = write_res {
        return Err(RunFailure::Io(format!("向 `pandoc` 写入 Markdown 失败：{e}")));
    }

    let tmp_len = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
    if tmp_len == 0 {
        return Err(RunFailure::Session(format!(
            "`pandoc` 返回成功但未生成非空 {} 文件（已拒绝落盘）。\n\
             请改用 `shangyi report {case_ref} --format md > report.md`，或在 Web 工作台（`shangyi start`）导出。",
            format.as_str()
        )));
    }

    std::fs::rename(&tmp_path, out_path).map_err(|e| {
        RunFailure::Io(format!(
            "写入目标 {} 文件 `{}` 失败：{e}",
            format.as_str(),
            out_path.display()
        ))
    })?;
    tmp_guard.disarm();
    println!("已生成 {} 报告：{}", format.as_str(), out_path.display());
    Ok(())
}

fn has_authoritative_artifacts(s: &ExtractedReportState) -> bool {
    s.report_model.is_some()
        || s.recommend.is_some()
        || s.solve.is_some()
        || s.select.is_some()
        || s.consequence.is_some()
        || s.discovery.is_some()
        || s.scoped_result.is_some()
        || s.conditional.is_some()
        || !s.costs.is_empty()
}

fn resolve_case_source(case_ref: &str) -> Result<CaseSource, RunFailure> {
    let trimmed = case_ref.trim();
    if trimmed.is_empty() {
        return Err(RunFailure::Session("案例标识或路径不能为空".into()));
    }
    let p = Path::new(trimmed);
    if p.is_file() {
        let header = p
            .parent()
            .map(|dir| dir.join("header.json"))
            .filter(|h| h.is_file())
            .and_then(|h| std::fs::read_to_string(h).ok())
            .and_then(|s| serde_json::from_str::<CaseHeader>(&s).ok());
        let case_id = header
            .as_ref()
            .map(|h| h.case_id.clone())
            .or_else(|| {
                p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "case-file".to_string());
        return Ok(CaseSource {
            case_id,
            log_path: p.to_path_buf(),
            header,
        });
    }

    let dir = if p.is_dir() {
        p.to_path_buf()
    } else {
        let home = shangyi_home()?;
        let cases_root = home.join("cases");
        find_case_dir_in(&cases_root, trimmed).ok_or_else(|| RunFailure::CaseDir {
            path: cases_root.display().to_string(),
            detail: format!("找不到案例 `{trimmed}`（支持案例 ID、`latest`、案例目录或 `case.jsonl` 文件路径）"),
        })?
    };

    let log_path = dir.join("case.jsonl");
    if !log_path.is_file() {
        return Err(RunFailure::CaseDir {
            path: dir.display().to_string(),
            detail: "案例目录内缺少 `case.jsonl`".into(),
        });
    }
    let header_path = dir.join("header.json");
    let header = if header_path.is_file() {
        std::fs::read_to_string(&header_path)
            .ok()
            .and_then(|s| serde_json::from_str::<CaseHeader>(&s).ok())
    } else {
        None
    };
    let case_id = header
        .as_ref()
        .map(|h| h.case_id.clone())
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| trimmed.to_string());

    Ok(CaseSource {
        case_id,
        log_path,
        header,
    })
}

fn phase_rank(phase: &str) -> Option<usize> {
    match phase {
        "intake" => Some(0),
        "intel" => Some(1),
        "model_build" => Some(2),
        "solve" => Some(3),
        "select" => Some(4),
        "consequence" => Some(5),
        "discover" => Some(6),
        "recommend" => Some(7),
        "report" => Some(8),
        "archived" => Some(9),
        _ => None,
    }
}

fn unwrap_json_str(raw: &str) -> Value {
    let mut v: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return Value::Null,
    };
    for _ in 0..3 {
        if let Some(inner) = v.get("arguments").and_then(Value::as_str) {
            if let Ok(parsed) = serde_json::from_str::<Value>(inner) {
                v = parsed;
                continue;
            }
        }
        break;
    }
    v
}

fn json_deep_merge(mut base: Value, patch: Value) -> Value {
    match (&mut base, patch) {
        (Value::Object(b), Value::Object(p)) => {
            // If an adjacently/internally tagged variant (`kind` or `form`) changed,
            // replace cleanly so stale variant fields cannot leak across merges.
            let kind_changed = matches!((b.get("kind"), p.get("kind")), (Some(bk), Some(pk)) if bk != pk);
            let form_changed = matches!((b.get("form"), p.get("form")), (Some(bf), Some(pf)) if bf != pf);
            if kind_changed || form_changed {
                return Value::Object(p);
            }
            for (k, v) in p {
                match b.remove(&k) {
                    Some(old) => {
                        b.insert(k, json_deep_merge(old, v));
                    }
                    None => {
                        b.insert(k, v);
                    }
                }
            }
            Value::Object(b.clone())
        }
        (_, patch) => patch,
    }
}

fn unwrap_report_spec(raw: &Value) -> Option<(bool, Value)> {
    let mut cur = match raw {
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?,
        other => other.clone(),
    };
    let mut is_merge = cur.get("merge").and_then(Value::as_bool).unwrap_or(false);

    for _ in 0..4 {
        let Some(obj) = cur.as_object() else {
            return None;
        };
        if let Some(m) = obj.get("merge").and_then(Value::as_bool) {
            is_merge = is_merge || m;
        }
        if obj.contains_key("game_id")
            || obj.contains_key("players")
            || obj.contains_key("strategies")
            || obj.contains_key("payoffs")
            || obj.contains_key("params")
        {
            break;
        }
        let inner = obj
            .get("spec")
            .or_else(|| obj.get("arguments"))
            .or_else(|| obj.get("input"))
            .cloned();
        match inner {
            Some(Value::String(s)) => {
                cur = serde_json::from_str::<Value>(&s).ok()?;
            }
            Some(other) => cur = other,
            None => break,
        }
    }

    if let Some(obj) = cur.as_object_mut() {
        if let Some(m) = obj.get("merge").and_then(Value::as_bool) {
            is_merge = is_merge || m;
        }
        obj.remove("merge");
        for key in ["players", "strategies", "payoffs", "params"] {
            if let Some(Value::String(s)) = obj.get(key) {
                if let Ok(parsed) = serde_json::from_str::<Value>(s) {
                    obj.insert(key.to_string(), parsed);
                }
            }
        }
    }
    if !cur.is_object() {
        return None;
    }
    Some((is_merge, cur))
}

fn is_rollback_rewind(from: &str, to: &str, due_to: &str) -> bool {
    if from.is_empty() || from == to {
        return false;
    }
    let d = due_to.trim();
    let dl = d.to_ascii_lowercase();
    d.contains("（用户确认：")
        || d.contains("模型提议")
        || d.contains("用户同意")
        || dl.starts_with("rollback")
        || dl.starts_with("user_rewind")
        || dl.starts_with("request_rewind")
}

fn clear_for_rewind(state: &mut ExtractedReportState, from: &str, to: &str, due_to: &str) {
    let target = phase_rank(to);
    let clear_target = !is_rollback_rewind(from, to, due_to);
    let cutoff = target.map(|r| if clear_target { r } else { r + 1 });
    if matches!(cutoff, Some(c) if c <= 2) {
        state.gamespec_args = None;
    }
    if matches!(cutoff, Some(c) if c <= 3) {
        state.solve = None;
    }
    if matches!(cutoff, Some(c) if c <= 4) {
        state.select = None;
    }
    if matches!(cutoff, Some(c) if c <= 5) {
        state.consequence = None;
    }
    if matches!(cutoff, Some(c) if c <= 6) {
        state.discovery = None;
    }
    if matches!(cutoff, Some(c) if c <= 7) {
        state.recommend = None;
        state.scoped_result = None;
        state.conditional = None;
    }
    if matches!(cutoff, Some(c) if c <= 8) {
        state.report_model = None;
    }
    if target == Some(0) && clear_target {
        state.scope = None;
        state.scope_reason = None;
        state.costs.clear();
        state.constraints.clear();
        state.constraint_check = None;
    }
    state.failed_tools.clear();
    for cp in &mut state.checkpoints {
        if matches!(cp.status, "pending" | "answered") {
            let cp_rank = phase_rank(&cp.phase);
            if cutoff.is_some_and(|c| cp_rank.is_some_and(|cr| cr >= c))
                || (to == "intake" && clear_target)
            {
                cp.status = "rewound";
            }
        }
    }
}

fn clear_for_followup(state: &mut ExtractedReportState) {
    let was_conditional = state.scope.as_deref() == Some("conditional")
        || state.conditional.is_some()
        || state
            .scoped_result
            .as_ref()
            .and_then(|v| v.get("scope"))
            .and_then(Value::as_str)
            == Some("conditional");
    state.scope = None;
    state.scope_reason = None;
    state.scoped_result = None;
    state.conditional = None;
    state.failed_tools.clear();
    if was_conditional {
        state.gamespec_args = None;
        state.solve = None;
        state.select = None;
        state.consequence = None;
        state.discovery = None;
        state.recommend = None;
        state.report_model = None;
        state.constraint_check = None;
        for cp in &mut state.checkpoints {
            if matches!(cp.status, "pending" | "answered") {
                cp.status = "rewound";
            }
        }
    }
}

fn extract_case_state(source: &CaseSource) -> Result<ExtractedReportState, RunFailure> {
    let raw = std::fs::read_to_string(&source.log_path).map_err(|e| RunFailure::CaseDir {
        path: source.log_path.display().to_string(),
        detail: format!("读取 case.jsonl 失败：{e}"),
    })?;
    if raw.trim().is_empty() {
        return Err(RunFailure::CaseDir {
            path: source.log_path.display().to_string(),
            detail: "case.jsonl 为空".into(),
        });
    }

    let mut state = ExtractedReportState::default();
    let mut started: BTreeMap<String, ToolInvocation> = BTreeMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<Recorded>(trimmed) {
            Ok(rec) => rec.event,
            Err(_) => match serde_json::from_str::<Event>(trimmed) {
                Ok(ev) => ev,
                Err(_) => {
                    state.bad_lines += 1;
                    continue;
                }
            },
        };

        match event {
            Event::TaskScopeSelected { scope, reason, .. } => {
                state.scope = Some(scope.as_str().to_string());
                state.scope_reason = Some(reason);
            }
            Event::PhaseEntered { to, .. } => {
                state.phases.push(to);
            }
            Event::PhaseRewound {
                from, to, due_to, ..
            } => {
                clear_for_rewind(&mut state, &from, &to, &due_to);
                state.phases.push(format!("回退→{to}"));
            }
            Event::FollowupTaskStarted { .. } => {
                clear_for_followup(&mut state);
            }
            Event::CheckpointOpened {
                id,
                phase,
                items,
                at,
            } => {
                if let Some(prev) = state.checkpoints.iter_mut().find(|c| c.id == id && c.status == "pending") {
                    prev.status = "superseded";
                }
                state.checkpoints.push(CheckpointEntry {
                    id,
                    phase,
                    items,
                    status: "pending",
                    answers: Vec::new(),
                    actor: None,
                    at,
                });
            }
            Event::CheckpointAnswered {
                id,
                answers,
                user_actor,
                ..
            } => {
                let is_followup = answers
                    .first()
                    .is_some_and(|a| a.starts_with("追问："));
                if let Some(cp) = state
                    .checkpoints
                    .iter_mut()
                    .rev()
                    .find(|c| c.id == id && c.status == "pending")
                {
                    cp.answers.extend(answers);
                    cp.actor = Some(user_actor);
                    if !is_followup {
                        cp.status = "answered";
                    }
                }
            }
            Event::ScopedTaskCompleted { result, .. } => {
                state.scope = Some(result.scope.as_str().to_string());
                for u in &result.source_urls {
                    if !u.trim().is_empty() {
                        state.web_sources.insert(u.clone());
                    }
                }
                if let Ok(val) = serde_json::to_value(&result) {
                    if let Some(cond) = val.get("conditional").filter(|c| !c.is_null()) {
                        state.conditional = Some(cond.clone());
                    }
                    state.scoped_result = Some(val);
                }
            }
            Event::ToolCallStarted {
                call_id,
                name,
                arguments,
                at,
                ..
            } => {
                if name == "sandbox_exec" {
                    state.exploratory_calls += 1;
                }
                started.insert(
                    call_id,
                    ToolInvocation {
                        name,
                        arguments: unwrap_json_str(&arguments),
                        at,
                    },
                );
            }
            Event::ToolCallFinished {
                call_id,
                outcome,
                ..
            } => {
                let Some(inv) = started.get(&call_id).cloned() else {
                    continue;
                };
                match outcome {
                    ToolOutcome::Ok { output } => {
                        if inv.name == "sandbox_exec" {
                            // Exploratory 工具数值严禁进入权威报告模型
                            continue;
                        }
                        let parsed = serde_json::from_str::<Value>(&output).unwrap_or(Value::Null);
                        match inv.name.as_str() {
                            "gamespec_build" => {
                                if let Some((is_merge, spec_patch)) =
                                    unwrap_report_spec(&inv.arguments)
                                {
                                    if is_merge {
                                        state.gamespec_args = Some(match state.gamespec_args.take() {
                                            Some(prev) => json_deep_merge(prev, spec_patch),
                                            None => spec_patch,
                                        });
                                    } else if spec_patch.get("players").is_some()
                                        && spec_patch.get("strategies").is_some()
                                    {
                                        state.gamespec_args = Some(spec_patch);
                                    }
                                }
                            }
                            "solve_equilibria" => {
                                if !parsed.is_null() {
                                    state.solve = Some(parsed);
                                }
                            }
                            "select_fuse" => {
                                if !parsed.is_null() {
                                    state.select = Some(parsed);
                                }
                            }
                            "consequence_predict" => {
                                if !parsed.is_null() {
                                    state.consequence = Some(parsed);
                                }
                            }
                            "discover_satisfice" => {
                                if !parsed.is_null() {
                                    state.discovery = Some(parsed);
                                }
                            }
                            "recommend_build" => {
                                if !parsed.is_null() {
                                    state.recommend = Some(parsed);
                                }
                            }
                            "report_render" => {
                                if let Some(rep_val) = parsed.get("report") {
                                    match serde_json::from_value::<Report>(rep_val.clone()) {
                                        Ok(rep) => state.report_model = Some(rep),
                                        Err(e) => {
                                            return Err(RunFailure::Session(format!(
                                                "report_render 产物反序列化失败：{e}"
                                            )));
                                        }
                                    }
                                }
                            }
                            "business_cost" => {
                                if !parsed.is_null() {
                                    let key = parsed
                                        .get("scenario_id")
                                        .or_else(|| parsed.get("scenario"))
                                        .and_then(Value::as_str)
                                        .unwrap_or(&call_id)
                                        .to_string();
                                    state.costs.insert(key, parsed);
                                }
                            }
                            "constraint_register" => {
                                if !parsed.is_null() {
                                    state.constraints.push(parsed);
                                }
                            }
                            "constraint_check" => {
                                if !parsed.is_null() {
                                    state.constraint_check = Some(parsed);
                                }
                            }
                            "select_task_scope" => {
                                if let Some(sc) = parsed.get("scope").and_then(Value::as_str) {
                                    state.scope = Some(sc.to_string());
                                }
                            }
                            "conditional_analysis" => {
                                state.scope = Some(
                                    parsed
                                        .get("scope")
                                        .and_then(Value::as_str)
                                        .unwrap_or("conditional")
                                        .to_string(),
                                );
                                if !parsed.is_null() {
                                    if let Some(cond) = parsed.get("conditional").filter(|c| !c.is_null()) {
                                        state.conditional = Some(cond.clone());
                                    }
                                    state.scoped_result = Some(parsed);
                                }
                            }
                            "research_record" => {
                                if !parsed.is_null() {
                                    state.research_records.push(parsed);
                                }
                            }
                            "poi_survey" => {
                                if !parsed.is_null() {
                                    state.poi_surveys.push(parsed);
                                }
                            }
                            "web_search" => {
                                if let Some(results) = parsed.get("results").and_then(Value::as_array) {
                                    for item in results {
                                        if let Some(u) = item.get("url").and_then(Value::as_str) {
                                            if !u.trim().is_empty() {
                                                state.web_sources.insert(u.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                            "web_fetch" => {
                                if let Some(u) = parsed
                                    .get("url")
                                    .or_else(|| inv.arguments.get("url"))
                                    .and_then(Value::as_str)
                                {
                                    if !u.trim().is_empty() {
                                        state.web_sources.insert(u.to_string());
                                    }
                                }
                            }
                            "kb_retrieve" => {
                                if let Some(q) = inv.arguments.get("query").and_then(Value::as_str) {
                                    state.kb_queries.push(format!("{q} ({})", inv.at));
                                }
                            }
                            _ => {}
                        }
                    }
                    ToolOutcome::Failed { error } => {
                        state.failed_tools.push((inv.name, error));
                    }
                    ToolOutcome::Blocked { detail } => {
                        state.failed_tools.push((inv.name, format!("外部阻塞：{detail}")));
                    }
                    ToolOutcome::InsufficientEvidence { .. } => {
                        state.failed_tools.push((inv.name, "证据不足（已按铁律 2 拦截）".into()));
                    }
                    ToolOutcome::Unknown => {
                        state.failed_tools.push((inv.name, "调用结局未知（中断残留）".into()));
                    }
                }
            }
            _ => {}
        }
    }

    Ok(state)
}

fn param_source_zh(src: &Value) -> (&'static str, String) {
    let kind = src.get("kind").and_then(Value::as_str).unwrap_or("unknown");
    match kind {
        "user_confirmed" => (
            "用户已确认",
            src.get("checkpoint")
                .and_then(Value::as_str)
                .map(|c| format!("关口 {c}"))
                .unwrap_or_default(),
        ),
        "confirmed_assumption" => {
            let cp = src.get("checkpoint").and_then(Value::as_str).unwrap_or("");
            let basis = src.get("basis").and_then(Value::as_str).unwrap_or("");
            ("已确认情景假设", format!("{cp}：{basis}"))
        }
        "public_data" => (
            "公开数据",
            src.get("provenance")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        "industry_prior" => (
            "行业先验（非本案实证）",
            src.get("prior_ref")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        "assumed" => (
            "未确认假设（禁止入解）",
            src.get("note").and_then(Value::as_str).unwrap_or("").to_string(),
        ),
        "derived" => (
            "引擎派生采样",
            src.get("note").and_then(Value::as_str).unwrap_or("").to_string(),
        ),
        _ => ("未标注", String::new()),
    }
}

fn fmt_json_num(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "未取得".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => other.to_string(),
    }
}

fn format_markdown_report(source: &CaseSource, state: &ExtractedReportState) -> String {
    let mut out = String::new();
    let mode = source
        .header
        .as_ref()
        .map(|h| h.mode.as_str())
        .unwrap_or("未知");
    let playbook = source
        .header
        .as_ref()
        .map(|h| h.playbook_id.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("未绑定");
    let created = source
        .header
        .as_ref()
        .map(|h| h.created_at.as_str())
        .unwrap_or("未知");

    out.push_str(&format!("# 商奕决策报告：{}\n\n", source.case_id));
    out.push_str(&format!(
        "- **案例 ID**：`{}`\n\
         - **模式 / 剧本 / 范围**：`{mode}` / `{playbook}` / `{}`\n\
         - **创建时间**：`{created}`\n\
         - **证据日志**：`{}`\n",
        source.case_id,
        state.scope.as_deref().unwrap_or("未记录"),
        source.log_path.display()
    ));
    if !state.phases.is_empty() {
        out.push_str(&format!("- **阶段轨迹**：`{}`\n", state.phases.join(" → ")));
    }
    if state.bad_lines > 0 {
        out.push_str(&format!(
            "\n> **⚠ 日志告警**：检测到 {} 行损坏 JSONL 记录已跳过，请检查原始日志。\n",
            state.bad_lines
        ));
    }
    if state.exploratory_calls > 0 {
        out.push_str(&format!(
            "\n> **⚠ 探索性工具隔离声明**：本案例曾发生 {} 次 `sandbox_exec`（`TrustLevel::Exploratory`）调用；其任何数值均已隔离排除，未纳入本报告数字。\n",
            state.exploratory_calls
        ));
    }
    out.push('\n');

    // 1. 白话结论与行动建议
    out.push_str("## 一、白话结论与下一步行动\n\n");
    if let Some(rec) = &state.recommend {
        if let Some(hl) = rec.get("headline").and_then(Value::as_str) {
            out.push_str(&format!("**核心结论**：{hl}\n\n"));
        }
        if let Some(actions) = rec.get("actions").and_then(Value::as_array) {
            out.push_str("### 建议推进动作\n\n");
            for (i, act) in actions.iter().enumerate() {
                let text = act.get("action").and_then(Value::as_str).unwrap_or("");
                let why = act.get("rationale").and_then(Value::as_str).unwrap_or("");
                out.push_str(&format!("{}. **{text}**\n   - 依据：{why}\n", i + 1));
            }
            out.push('\n');
        }
        if let Some(do_not) = rec.get("do_not").and_then(Value::as_array) {
            if !do_not.is_empty() {
                out.push_str("### 明确不建议（底线避坑）\n\n");
                for item in do_not {
                    if let Some(s) = item.as_str() {
                        out.push_str(&format!("- {s}\n"));
                    }
                }
                out.push('\n');
            }
        }
        if let Some(caveats) = rec.get("caveats").and_then(Value::as_array) {
            if !caveats.is_empty() {
                out.push_str("### 边界与注意事项\n\n");
                for item in caveats {
                    if let Some(s) = item.as_str() {
                        out.push_str(&format!("- {s}\n"));
                    }
                }
                out.push('\n');
            }
        }
    } else if let Some(scoped) = &state.scoped_result {
        if let Some(sum) = scoped.get("summary").and_then(Value::as_str) {
            out.push_str(&format!("{sum}\n\n"));
        }
        if let Some(missing) = scoped.get("missing").and_then(Value::as_array) {
            if !missing.is_empty() {
                out.push_str("**尚未取得/仍缺项**：\n");
                for m in missing {
                    if let Some(s) = m.as_str() {
                        out.push_str(&format!("- {s}\n"));
                    }
                }
                out.push('\n');
            }
        }
    } else if let Some(rep) = &state.report_model {
        for sec in &rep.sections {
            if sec.layer == "boss_page" {
                for p in &sec.paragraphs {
                    out.push_str(&format!("- {p}\n"));
                }
                out.push('\n');
            }
        }
    } else {
        out.push_str("当前案例尚未产生最终建议包（可能为独立费用测算或未走到建议阶段）。以下列示已完成的确定性引擎产出。\n\n");
    }

    if let Some(cond) = &state.conditional {
        out.push_str("### 条件交付限制与后续行动\n\n");
        if let Some(conds) = cond.get("decision_conditions").and_then(Value::as_array) {
            for c in conds {
                if let Some(s) = c.as_str() {
                    out.push_str(&format!("- 决策条件：{s}\n"));
                }
            }
        }
        if let Some(acts) = cond.get("information_actions").and_then(Value::as_array) {
            for a in acts {
                if let Some(s) = a.as_str() {
                    out.push_str(&format!("- 补证动作：{s}\n"));
                }
            }
        }
        if let Some(lims) = cond.get("limitations").and_then(Value::as_array) {
            for l in lims {
                if let Some(s) = l.as_str() {
                    out.push_str(&format!("- 限制说明：{s}\n"));
                }
            }
        }
        out.push('\n');
    }

    // 2. 博弈求解 / 均衡分布 / 后果 / 满意策略
    if state.solve.is_some() || state.select.is_some() || state.consequence.is_some() || state.discovery.is_some() {
        out.push_str("## 二、博弈推演与引擎求解\n\n");
        if let Some(solve) = &state.solve {
            out.push_str(&format!(
                "- **求解路线**：`{}` / **完整度**：`{}`\n",
                solve.get("route").and_then(Value::as_str).unwrap_or("未知"),
                solve.get("completeness").and_then(Value::as_str).unwrap_or("未知")
            ));
            if let Some(eqs) = solve.get("equilibria").and_then(Value::as_array) {
                for eq in eqs {
                    let id = eq.get("id").and_then(Value::as_str).unwrap_or("e?");
                    let payoffs = eq.get("payoffs").map(|p| p.to_string()).unwrap_or_default();
                    out.push_str(&format!("  - 均衡 `{id}`：期望收益 `{payoffs}`\n"));
                }
            }
            out.push('\n');
        }
        if let Some(sel) = &state.select {
            if let Some(entries) = sel.get("entries").and_then(Value::as_array) {
                out.push_str("### 多判据均衡概率分布\n\n| 均衡 | 概率 | 主导判据 |\n|---|---|---|\n");
                for e in entries {
                    let eid = e.get("equilibrium_id").or_else(|| e.get("equilibrium")).and_then(Value::as_str).unwrap_or("-");
                    let prob = fmt_json_num(e.get("prob"));
                    let drivers = e
                        .get("drivers")
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("、"))
                        .unwrap_or_default();
                    out.push_str(&format!("| `{eid}` | `{prob}` | {drivers} |\n"));
                }
                out.push('\n');
            }
            if let Some(conf) = sel.get("confidence") {
                out.push_str(&format!("- **选择置信度**：`{conf}`\n"));
            }
            if let Some(divs) = sel.get("divergences").and_then(Value::as_array) {
                if !divs.is_empty() {
                    out.push_str("- **判据分歧**：\n");
                    for d in divs {
                        if let Some(s) = d.as_str() {
                            out.push_str(&format!("  - {s}\n"));
                        }
                    }
                }
            }
            out.push('\n');
        }
        if let Some(cr) = &state.consequence {
            out.push_str(&format!(
                "### 条件后果推演（我方固定：`{}`）\n\n",
                cr.get("decision").and_then(Value::as_str).unwrap_or("-")
            ));
            if let Some(reacts) = cr.get("reactions").and_then(Value::as_array) {
                for r in reacts {
                    let name = r.get("reaction").and_then(Value::as_str).unwrap_or("-");
                    let prob = fmt_json_num(r.get("probability"));
                    let outcomes = r.get("outcomes").map(Value::to_string).unwrap_or_default();
                    out.push_str(&format!("- 反应 `{name}`（概率 `{prob}`）：`{outcomes}`\n"));
                }
            }
            if let Some(note) = cr.get("note").and_then(Value::as_str) {
                out.push_str(&format!("- 说明：{note}\n"));
            }
            out.push('\n');
        }
        if let Some(dr) = &state.discovery {
            out.push_str(&format!(
                "### 满意策略发现（达标 {} / 放宽建议 {}）\n\n",
                fmt_json_num(dr.get("satisfied_count")),
                fmt_json_num(dr.get("relaxation_proposals"))
            ));
            if let Some(note) = dr.get("note").and_then(Value::as_str) {
                out.push_str(&format!("- {note}\n\n"));
            }
        }
    }

    // 3. 成本精算与硬约束
    if !state.costs.is_empty() || !state.constraints.is_empty() {
        out.push_str("## 三、成本精算对照与底线约束\n\n");
        if let Some(cc) = &state.constraint_check {
            out.push_str(&format!(
                "- **底线核对（v{}）**：可接受 `{}` / 超限对照 `{}` / 条件未知 `{}`\n\n",
                fmt_json_num(cc.get("constraint_revision")),
                cc.get("eligible_scenario_ids").map(Value::to_string).unwrap_or_else(|| "[]".into()),
                cc.get("comparison_only_scenario_ids").map(Value::to_string).unwrap_or_else(|| "[]".into()),
                cc.get("unknown_scenario_ids").map(Value::to_string).unwrap_or_else(|| "[]".into())
            ));
        }
        for (sid, sc) in &state.costs {
            let label = sc.get("scenario").and_then(Value::as_str).unwrap_or(sid);
            let cur = sc.get("currency").and_then(Value::as_str).unwrap_or("");
            out.push_str(&format!("### 方案：{label}（`{sid}`）\n\n"));
            out.push_str(&format!(
                "- 期初现金总需求（含押金）：`{}` {cur}（其中一次性现金支出 `{}`，可退押金 `{}`）\n\
                 - 已列月固定现金支出：`{}` {cur}/月\n\
                 - 金额完整 / 性质分类完整 / 清单声明完整：`{}` / `{}` / `{}`\n",
                fmt_json_num(sc.get("listed_upfront_cash")),
                fmt_json_num(sc.get("listed_upfront_cash_expense")),
                fmt_json_num(sc.get("listed_refundable_deposits")),
                fmt_json_num(sc.get("listed_monthly_fixed_cost")),
                sc.get("amounts_complete").and_then(Value::as_bool).unwrap_or(false),
                sc.get("cash_classification_complete").and_then(Value::as_bool).unwrap_or(false),
                sc.get("costs_complete").and_then(Value::as_bool).unwrap_or(false),
            ));
            if let Some(ht) = sc.get("horizon_totals").filter(|v| !v.is_null()) {
                out.push_str(&format!(
                    "- **{} 个月期间累计**：现金费用 `{}` {cur}，含押金总流出 `{}` {cur}\n",
                    fmt_json_num(ht.get("months")),
                    fmt_json_num(ht.get("listed_cash_expense")),
                    fmt_json_num(ht.get("listed_cash_outflow_before_deposit_return"))
                ));
            }
            if let Some(items) = sc.get("items").and_then(Value::as_array) {
                out.push_str("\n| 费用项目 | 性质 | 换算周期 | 标准化金额 | 来源 | 缺项说明 |\n|---|---|---|---|---|---|\n");
                for it in items {
                    let name = it.get("name").and_then(Value::as_str).unwrap_or("-");
                    let tr = it.get("treatment").and_then(Value::as_str).unwrap_or("-");
                    let per = it.get("normalized_period").and_then(Value::as_str).unwrap_or("-");
                    let amt = fmt_json_num(it.get("normalized_amount"));
                    let src = it.get("source").and_then(Value::as_str).unwrap_or("-");
                    let miss = it.get("missing_amount_reason").and_then(Value::as_str).unwrap_or("");
                    out.push_str(&format!("| {name} | `{tr}` | `{per}` | `{amt}` | {src} | {miss} |\n"));
                }
                out.push('\n');
            }
            if let Some(note) = sc.get("note").and_then(Value::as_str) {
                out.push_str(&format!("> 口径说明：{note}\n\n"));
            }
        }
    }

    // 4. 核心参数与四类证据来源 / 证据缺口
    if let Some(spec) = &state.gamespec_args {
        if let Some(params) = spec.get("params").and_then(Value::as_object) {
            if !params.is_empty() {
                out.push_str("## 四、核心假设与四类证据来源\n\n");
                out.push_str("| 参数名 | 表达式/值 | 单位 | 来源分类 | 来源明细 |\n|---|---|---|---|---|\n");
                for (k, p) in params {
                    let val = p.get("value").map(Value::to_string).unwrap_or_default();
                    let unit = p.get("unit").and_then(Value::as_str).unwrap_or("-");
                    let (kind_zh, detail) = p
                        .get("source")
                        .map(param_source_zh)
                        .unwrap_or(("未知", String::new()));
                    out.push_str(&format!("| `{k}` | `{val}` | {unit} | {kind_zh} | {detail} |\n"));
                }
                out.push('\n');
            }
        }
    }

    // 5. 真人关口审批与外部情报
    if !state.checkpoints.is_empty() || !state.web_sources.is_empty() || !state.poi_surveys.is_empty() {
        out.push_str("## 五、真人审批轨迹与外部证据来源\n\n");
        for cp in &state.checkpoints {
            let ans = if cp.answers.is_empty() {
                "待确认".to_string()
            } else {
                cp.answers.join("；")
            };
            let items = if cp.items.is_empty() {
                String::new()
            } else {
                format!(" | 核对项：{}", cp.items.join("；"))
            };
            out.push_str(&format!(
                "- 关口 `{}`（阶段 `{}` / 状态 `{}` / 操作者 `{}` / 时间 `{}`）：{}{}\n",
                cp.id,
                cp.phase,
                cp.status,
                cp.actor.as_deref().unwrap_or("-"),
                cp.at,
                ans,
                items
            ));
        }
        for poi in &state.poi_surveys {
            out.push_str(&format!("- POI 周边统计：`{poi}`\n"));
        }
        for url in &state.web_sources {
            out.push_str(&format!("- 公开来源：{url}\n"));
        }
        out.push('\n');
    }

    // 6. Report_render 结构化章节与数字溯源台账
    if let Some(rep) = &state.report_model {
        out.push_str("## 六、引擎报告模型与数值溯源台账\n\n");
        for sec in &rep.sections {
            out.push_str(&format!("### {}\n\n", sec.title));
            for p in &sec.paragraphs {
                out.push_str(&format!("{p}\n\n"));
            }
        }
        if !rep.numbers.is_empty() {
            out.push_str("### 登记数值清单\n\n");
            for (k, num) in &rep.numbers {
                out.push_str(&format!(
                    "- `{k}` = **{}**（{}，溯源 `{}`）\n",
                    num.display(),
                    num.label,
                    num.provenance.0
                ));
            }
            out.push('\n');
        }
        out.push_str(&rep.provenance_appendix());
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sanitize_filename_blocks_traversal_and_special_chars() {
        assert_eq!(sanitize_filename("case-20261114-demo"), "case-20261114-demo");
        assert_eq!(sanitize_filename("../etc/passwd"), "___etc_passwd");
        assert_eq!(sanitize_filename("a:b\\c/d"), "a_b_c_d");
        assert_eq!(sanitize_filename(".."), "shangyi-report");
        assert_eq!(sanitize_filename("."), "shangyi-report");
        assert_eq!(sanitize_filename("   "), "shangyi-report");
    }

    #[test]
    fn unwrap_report_spec_and_merge_update_params_cleanly() {
        let base_call = json!({
            "game_id": "g1",
            "players": [{"id": "us", "name": "我方"}, {"id": "comp", "name": "对手"}],
            "strategies": {"us": ["A"], "comp": ["B"]},
            "params": {
                "k_aa_1": {
                    "name": "单杯毛利",
                    "value": {"kind": "literal", "value": 40},
                    "source": {"kind": "user_confirmed", "checkpoint": "cp2"},
                    "unit": "元"
                }
            }
        });
        let (m0, v0) = unwrap_report_spec(&base_call).unwrap();
        assert!(!m0);

        let patch_call = json!({
            "merge": true,
            "spec": {
                "params": {
                    "k_aa_1": {
                        "value": {"kind": "literal", "value": 42}
                    }
                }
            }
        });
        let (m1, v1) = unwrap_report_spec(&patch_call).unwrap();
        assert!(m1);

        let merged = json_deep_merge(v0, v1);
        assert_eq!(merged["params"]["k_aa_1"]["value"]["value"], json!(42));
        assert_eq!(merged["params"]["k_aa_1"]["name"], json!("单杯毛利"));
        assert_eq!(merged["params"]["k_aa_1"]["unit"], json!("元"));
        assert_eq!(merged["params"]["k_aa_1"]["source"]["checkpoint"], json!("cp2"));
    }

    #[test]
    fn clear_for_rewind_revokes_checkpoints_at_or_after_target_consistently() {
        let mut st = ExtractedReportState::default();
        st.gamespec_args = Some(json!({"game_id": "g1"}));
        st.solve = Some(json!({"route": "exact"}));
        st.checkpoints = vec![
            CheckpointEntry {
                id: "cp1".into(),
                phase: "intel".into(),
                items: vec![],
                status: "answered",
                answers: vec!["ok".into()],
                actor: Some("user".into()),
                at: "t1".into(),
            },
            CheckpointEntry {
                id: "cp2".into(),
                phase: "model_build".into(),
                items: vec![],
                status: "answered",
                answers: vec!["ok".into()],
                actor: Some("user".into()),
                at: "t2".into(),
            },
            CheckpointEntry {
                id: "cp3".into(),
                phase: "select".into(),
                items: vec![],
                status: "pending",
                answers: vec![],
                actor: None,
                at: "t3".into(),
            },
        ];

        clear_for_rewind(&mut st, "solve", "model_build", "重设参数");
        assert!(st.gamespec_args.is_none());
        assert!(st.solve.is_none());
        assert_eq!(st.checkpoints[0].status, "answered", "早于 target 的 cp1 保留");
        assert_eq!(st.checkpoints[1].status, "rewound", "invalidation 回退 target 处的 cp2 应作废");
        assert_eq!(st.checkpoints[2].status, "rewound", "下游 cp3 应作废");

        // In-place reconsider (from == to) also clears model_build and cp2
        st.gamespec_args = Some(json!({"game_id": "g1"}));
        st.checkpoints[1].status = "pending";
        clear_for_rewind(&mut st, "model_build", "model_build", "用户修改了本轮材料");
        assert!(st.gamespec_args.is_none());
        assert_eq!(st.checkpoints[1].status, "rewound");

        // Cross-phase rollback_to (request_rewind) preserves target model_build's gamespec and cp2
        st.gamespec_args = Some(json!({"game_id": "g1"}));
        st.solve = Some(json!({"route": "exact"}));
        st.checkpoints[1].status = "answered";
        st.checkpoints[2].status = "answered";
        clear_for_rewind(
            &mut st,
            "select",
            "model_build",
            "补充参数来源（用户确认：同意回退到 model_build）",
        );
        assert!(
            st.gamespec_args.is_some(),
            "request_rewind(to=model_build) 必须保留 model_build 自身的 gamespec 以供 merge=true 增量更新"
        );
        assert!(st.solve.is_none(), "下游 solve 必须清除");
        assert_eq!(st.checkpoints[0].status, "answered");
        assert_eq!(st.checkpoints[1].status, "answered", "request_rewind(to=model_build) 保留 cp2");
        assert_eq!(st.checkpoints[2].status, "rewound", "严格下游已答 cp3 也须作废");

        // Rollback rewind to intake preserves unknown-phase / intake checkpoints while rewinding intel+
        st.checkpoints.push(CheckpointEntry {
            id: "cp0".into(),
            phase: "intake".into(),
            items: vec![],
            status: "answered",
            answers: vec!["ok".into()],
            actor: Some("user".into()),
            at: "t0".into(),
        });
        clear_for_rewind(&mut st, "model_build", "intake", "rollback: 回到 intake 保留起始记录");
        assert_eq!(st.checkpoints[0].status, "rewound", "rollback(to=intake) 仍须作废 intel 关口 cp1");
        assert_eq!(st.checkpoints[3].status, "answered", "rollback(to=intake) 不应误清除 intake 关口");
    }

    #[test]
    fn json_deep_merge_replaces_variant_objects_on_discriminator_change() {
        let base = json!({
            "source": {"kind": "public_data", "provenance": "年报"},
            "payoffs": {"form": "normal", "values": {"cells": {"a,b": {"us": 1}}}}
        });
        let patch = json!({
            "source": {"kind": "user_confirmed", "checkpoint": "cp2"},
            "payoffs": {"form": "extensive", "values": {"tree": {"root": "n0"}}}
        });
        let merged = json_deep_merge(base, patch);
        assert_eq!(merged["source"]["kind"], json!("user_confirmed"));
        assert!(merged["source"].get("provenance").is_none());
        assert_eq!(merged["payoffs"]["form"], json!("extensive"));
        assert!(merged["payoffs"]["values"].get("cells").is_none());
    }

    #[test]
    fn clear_for_followup_preserves_evidence_costs_constraints_and_poi() {
        let mut st = ExtractedReportState::default();
        st.scope = Some("cost".into());
        st.scope_reason = Some("费用测算".into());
        st.scoped_result = Some(json!({"scope": "cost"}));
        st.costs.insert("s1".into(), json!({"scenario_id": "s1"}));
        st.constraints.push(json!({"limit": 100000}));
        st.research_records.push(json!({"topic": "rent"}));
        st.poi_surveys.push(json!({"address": "人民广场"}));
        st.web_sources.insert("https://example.com".into());

        clear_for_followup(&mut st);
        assert!(st.scope.is_none());
        assert!(st.scoped_result.is_none());
        assert_eq!(st.costs.len(), 1);
        assert_eq!(st.constraints.len(), 1);
        assert_eq!(st.research_records.len(), 1);
        assert_eq!(st.poi_surveys.len(), 1);
        assert_eq!(st.web_sources.len(), 1);

        // Conditional followup invalidates prior model/solve artifacts while preserving evidence
        st.scope = Some("conditional".into());
        st.conditional = Some(json!({"limitations": ["missing"]}));
        st.gamespec_args = Some(json!({"game_id": "g1"}));
        st.solve = Some(json!({"route": "exact"}));
        clear_for_followup(&mut st);
        assert!(st.gamespec_args.is_none());
        assert!(st.solve.is_none());
        assert_eq!(st.costs.len(), 1);
        assert_eq!(st.poi_surveys.len(), 1);
    }

    #[test]
    fn clear_for_followup_rewinds_answered_cp1_only_when_conditional() {
        let mut st = ExtractedReportState::default();
        st.scope = Some("cost".into());
        st.checkpoints = vec![
            CheckpointEntry {
                id: "cp1".into(),
                phase: "intel".into(),
                items: vec!["竞品定价".into()],
                status: "answered",
                answers: vec!["确认：18 元".into()],
                actor: Some("user".into()),
                at: "t1".into(),
            },
            CheckpointEntry {
                id: "cp2".into(),
                phase: "model_build".into(),
                items: vec!["收益矩阵".into()],
                status: "pending",
                answers: vec![],
                actor: None,
                at: "t2".into(),
            },
        ];

        // Non-conditional followup preserves prior answered and pending checkpoints
        clear_for_followup(&mut st);
        assert_eq!(st.checkpoints[0].status, "answered", "非条件 followup 必须保留已确认的 cp1");
        assert_eq!(st.checkpoints[1].status, "pending", "非条件 followup 不得撤销 cp2");

        // Conditional followup must mark both pending and answered prior checkpoints as rewound
        st.scope = Some("conditional".into());
        st.conditional = Some(json!({"limitations": ["missing"]}));
        st.gamespec_args = Some(json!({"game_id": "g1"}));
        clear_for_followup(&mut st);
        assert!(st.gamespec_args.is_none());
        assert_eq!(st.checkpoints[0].status, "rewound", "条件交付后 followup 必须将已确认 cp1 标为 rewound");
        assert_eq!(st.checkpoints[1].status, "rewound", "条件交付后 followup 必须将待决 cp2 标为 rewound");

        // End-to-end extract_case_state check: answered cp1 -> conditional_analysis -> FollowupTaskStarted
        let tmp_dir = std::env::temp_dir().join(format!(
            "shangyi-report-unit-cond-fu-{}-{}",
            std::process::id(),
            TEMP_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&tmp_dir).unwrap();
        let log_path = tmp_dir.join("case.jsonl");
        let lines = [
            json!({"seq": 1, "event": {"type": "task_scope_selected", "scope": "consultation", "reason": "完整推演", "at": "t1"}}),
            json!({"seq": 2, "event": {"type": "phase_entered", "from": "intake", "to": "intel", "playbook": "pricing_response", "reason": "ok", "at": "t2"}}),
            json!({"seq": 3, "event": {"type": "checkpoint_opened", "id": "cp1", "phase": "intel", "items": ["情报"], "at": "t3"}}),
            json!({"seq": 4, "event": {"type": "checkpoint_answered", "id": "cp1", "answers": ["确认"], "user_actor": "user", "at": "t4"}}),
            json!({"seq": 5, "event": {"type": "tool_call_started", "step": {"turn": 1, "step": 1}, "call_id": "ca1", "name": "conditional_analysis", "arguments": "{}", "at": "t5"}}),
            json!({"seq": 6, "event": {"type": "tool_call_finished", "call_id": "ca1", "outcome": {"status": "ok", "output": "{\"scope\":\"conditional\",\"summary\":\"条件结论\",\"conditional\":{\"limitations\":[\"缺毛利\"]}}"}, "at": "t6"}}),
            json!({"seq": 7, "event": {"type": "followup_task_started", "at": "t7"}}),
        ];
        let body = lines.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n") + "\n";
        std::fs::write(&log_path, body).unwrap();
        let extracted = extract_case_state(&CaseSource {
            case_id: "case-unit-cond-fu".into(),
            log_path,
            header: None,
        })
        .unwrap();
        assert_eq!(extracted.bad_lines, 0);
        assert_eq!(extracted.checkpoints.len(), 1);
        assert_eq!(extracted.checkpoints[0].id, "cp1");
        assert_eq!(extracted.checkpoints[0].status, "rewound");
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }
}

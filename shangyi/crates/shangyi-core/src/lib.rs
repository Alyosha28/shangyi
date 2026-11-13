//! 商奕核心接线层：咨询会话的阶段机（设计文档 §7.3）。
//!
//! 本 crate 同时承担一个**架构护栏**职责：证明内核不依赖任何外部 harness／基座。
//! 历史上这里挂着一个 `vendor_harness_is_linkable` 测试（引用 vendored 基线 crate
//! 的类型来证明依赖边成立）。R1 摘除该基座后，"能链接"不再是需要证明的事，
//! 新的不变量是"**链接不到**"——负向断言只有真的扫一遍文件才能被验证，故护栏改为：
//!
//! - **权威门**：`tools/loop/check-no-harness.py`。它扫根 `Cargo.toml`、`Cargo.lock`
//!   与 `shangyi/**/Cargo.toml`，出现 `codex` / `rama` / `openai-oss-forks` 任一
//!   （词边界匹配）即非零退出，CI 应据此失败。
//! - **进程内快速版**：`tests::no_external_harness_in_manifests`，与脚本同一规则，
//!   使单独跑 `cargo test` 也能抓住回归，而不必记得手跑脚本。
//!
//! 判据是**依赖声明**而非字面：`Cargo.toml` 先剥掉行内注释再匹配，故"为何自研而非
//! 复用某基座"这类历史注释不会被误判成依赖残留；被注释掉的旧依赖行同样不算边
//! （它不参与解析）。`Cargo.lock` 没有注释，整行匹配。
//!
//! # 模块
//!
//! - [`phase_graph`]：**模式感知**的阶段图 + 产物闸门 + checkpoint 结算（§7.3、§7.6）
//! - [`agent`]：Agent 核心循环（步进、闸门、记账、诚实终止）（§7）
//! - [`session`]：会话事件词汇 + 崩溃修复纯函数（§7.1、§7.2）
//! - [`consultation`]：一次咨询的装配（剧本、引擎产物、模式校验）

pub mod sandbox;

/// 供集成测试回放真实样本用的薄封装。
///
/// 存在的理由：归一化器（`normalize_spec_input`）是私有函数，
/// 而"它对**真实失败样本**有效"这条证据必须在真实样本上取——
/// 手写的单元测试证明不了这一点（它们测的是我**以为**的形状）。
#[doc(hidden)]
pub mod test_support {
    /// 走一遍 GameSpec 输入的归一化（不解包、不校验）。
    ///
    /// 返回 `Ok` 只说明**形状**可被识别；铁律 2 的校验是另一层。
    pub fn normalize_spec_for_replay(
        v: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        crate::domain_exec::normalize_spec_for_replay(v)
    }
}

pub mod agent;
pub mod business;
pub mod research_task;
pub mod conditional;
pub mod constraints;
pub mod case_log;
pub mod clock;
pub mod consultation;
pub mod domain_exec;
pub mod delivery_render;
pub mod phase_graph;
pub mod providers;
pub mod runner;
pub mod session;
pub mod task_scope;

// 阶段机的公开面从 `phase_graph` 重导出：调用方只见一处定义。
// `Consultation` 结构已删除（见 `consultation.rs` 末尾的记录：它是与
// `SessionRunner` 并行的实现，只被测试使用 ⇒ 那些测试验证的不是真实路径）。
pub use consultation::ConsultationMode;
pub use domain_exec::{DomainExecutor, DomainState, SearchHit, SearchProvider, wiring_status};
pub use case_log::{
    CaseHeader, CaseLog, LogError, Recorded, ResumeOutcome, apply_repairs, resume,
};
pub use clock::{Clock, FixedClock, SeqClock, SystemClock};
pub use runner::{
    CheckpointAnswer, Progress, RunError, RunReport, SessionRunner, UserIo, is_durable, progress,
};
pub use agent::{
    AgentLoop, ExecOutcome, LoopConfig, LoopError, PreStepDecision, PreStepGate, ToolCall,
    ToolExecutor, TurnOutcome,
};
pub use session::{
    CancelCause, Event, StepId, ToolOutcome, TurnEndReason, TurnId,
    interrupted_turn_closers,
};
pub use phase_graph::{
    ArtifactKind, CheckpointId, CheckpointItem, CheckpointKind, CheckpointRequest,
    CheckpointStatus, PhaseGateError, PhaseGraph, PhaseMachine, SessionPhase,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// 命中即违规的符号（大小写不敏感）。与 `tools/loop/check-no-harness.py` 的
    /// `FORBIDDEN` 保持一致——两处都要改。
    const FORBIDDEN: [&str; 3] = ["codex", "rama", "openai-oss-forks"];

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// 去掉 TOML 的行内注释：不在引号内的 `#` 起算（`"a#b"` 里的 `#` 不是注释）。
    ///
    /// 判据（与 `tools/loop/check-no-harness.py` 的 `strip_toml_comment` 一致）：
    /// **注释是解释，依赖声明才是边**。历史注释（如"为何自建而非复用某基座"）保留为
    /// 设计理由；真正的依赖 / 钉版 / patch 一定出现在未注释的代码行上，故不会漏判；
    /// 被注释掉的旧依赖行也不构成边（它不参与解析）。
    fn strip_toml_comment(line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut quote: Option<char> = None;
        let mut chars = line.chars();
        while let Some(c) = chars.next() {
            match quote {
                Some(q) => {
                    if c == '\\' && q == '"' {
                        out.push(c);
                        if let Some(n) = chars.next() {
                            out.push(n);
                        }
                        continue;
                    }
                    if c == q {
                        quote = None;
                    }
                    out.push(c);
                }
                None => {
                    if c == '#' {
                        break;
                    }
                    if c == '"' || c == '\'' {
                        quote = Some(c);
                    }
                    out.push(c);
                }
            }
        }
        out
    }

    /// 逐行扫一个文件，返回 `路径:行号: 行内容 ← 命中 X`。
    ///
    /// 边界规则：命中处前后不得是字母／数字／下划线，故 `param`、`drama` 不算，
    /// 而 `rama-core`、`codex-core-api`、`openai-oss-forks/crossterm` 算。
    /// `Cargo.lock` 无注释 → 整行扫；`Cargo.toml` → 先剥注释再扫
    /// （判据见 `strip_toml_comment`）。
    fn scan(path: &std::path::Path) -> Vec<String> {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("读不到 {}: {e}", path.display()));
        let is_lock = path
            .file_name()
            .is_some_and(|n| n.to_string_lossy() == "Cargo.lock");
        let mut hits = Vec::new();
        for (idx, line) in text.lines().enumerate() {
            let subject = if is_lock {
                line.to_string()
            } else {
                strip_toml_comment(line)
            };
            let lower = subject.to_lowercase();
            // 同一行同一符号只报一次（一条依赖边可能在同一行命中多次）。
            let mut symbols: Vec<&str> = Vec::new();
            for needle in FORBIDDEN {
                for (at, _) in lower.match_indices(needle) {
                    let prev = lower[..at].chars().next_back().is_some_and(is_word_char);
                    let next = lower[at + needle.len()..]
                        .chars()
                        .next()
                        .is_some_and(is_word_char);
                    if prev || next {
                        continue;
                    }
                    if !symbols.contains(&needle) {
                        symbols.push(needle);
                    }
                }
            }
            for needle in symbols {
                hits.push(format!(
                    "{}:{}: {}  ← 命中 `{needle}`",
                    path.display(),
                    idx + 1,
                    line.trim()
                ));
            }
        }
        hits
    }

    fn collect_manifests(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("读不到 {}: {e}", dir.display()));
        for entry in entries {
            let p = entry.expect("目录项可读").path();
            if p.is_dir() {
                collect_manifests(&p, out);
            } else if p.file_name().is_some_and(|n| n == "Cargo.toml") {
                out.push(p);
            }
        }
    }

    /// 架构护栏：依赖图里不得出现任何外部 harness／基座。
    ///
    /// 扫描面与 `tools/loop/check-no-harness.py` 一致：根 `Cargo.toml`、`Cargo.lock`、
    /// `shangyi/**/Cargo.toml`。`research/` 下的独立实验 workspace 不在扫面内
    /// （它有自己的 lock，且不属于内核依赖图）。另有一条目录级断言：
    /// **`vendor/` 不得存在**（与脚本同款判据，见测试末尾）。
    ///
    /// 为什么必须是测试：R1 之前的不变量是"基座可链接"（旧测试 `vendor_harness_is_linkable`
    /// 用 `type_name::<Config>()` 证明），现在的不变量是"基座不可见"。这类负向断言
    /// 只有真去读文件才能验证，写进注释只会腐烂。
    #[test]
    fn no_external_harness_in_manifests() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("crate 位于 <root>/shangyi/crates/shangyi-core")
            .to_path_buf();

        let mut files = vec![root.join("Cargo.toml"), root.join("Cargo.lock")];
        collect_manifests(&root.join("shangyi"), &mut files);

        assert!(
            files.len() > 2,
            "只扫到 {} 个文件，扫描面失效: {files:?}",
            files.len()
        );

        let hits: Vec<String> = files.iter().flat_map(|p| scan(p)).collect();
        assert!(
            hits.is_empty(),
            "依赖图里出现外部 harness／基座残留（{} 处）：\n{}\n规则见 tools/loop/check-no-harness.py",
            hits.len(),
            hits.join("\n")
        );

        // 目录级断言（P2-6）：`vendor/` 不得回来。`check-no-harness.py` 已查这一条，
        // 这里在 Rust 侧补同一断言——仓库没有 CI，`cargo test` 就是门禁；缺了它，
        // "把 vendor 重新拉回来"这种回归只能靠人记得手跑脚本。
        let vendor = root.join("vendor");
        assert!(
            !vendor.exists(),
            "vendor/ 目录回来了：{}。R1 已摘除该基座目录；它回来一定会带回被移除的 \
             harness 依赖、钉版与 patch（见 docs 变更记录与 tools/loop/check-no-harness.py）",
            vendor.display()
        );
    }

    /// 三个模式的路径必须**互不相同**——本测试是对旧实现的直接回归：
    /// 曾经 `required_producing_phases` 对 scenario 与 discover 返回同一个列表，
    /// 而 `whatif` 里错误地包含了 `select`。
    #[test]
    fn mode_paths_are_not_collapsed() {
        let paths: Vec<Vec<SessionPhase>> = [
            ConsultationMode::Scenario,
            ConsultationMode::Whatif,
            ConsultationMode::Discover,
        ]
        .iter()
        .map(|m| PhaseGraph::for_mode(*m).path().to_vec())
        .collect();
        assert_ne!(paths[0], paths[1]);
        assert_ne!(paths[0], paths[2]);
        assert_ne!(paths[1], paths[2]);
    }
}

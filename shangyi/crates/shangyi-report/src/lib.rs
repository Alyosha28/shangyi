//! 商奕报告生成与 provenance 链（设计文档 §9 与 Harness §15.4）。
//!
//! # 本 crate 的核心不是排版，而是**审计链**
//!
//! v0.1 §1.5 铁律 1：**报告中每个数字必须有代码/数据出处**。
//! 这不是一句口号——它必须能被机器检查。故本 crate 的设计是：
//!
//! 1. **数值出生即带 provenance**：[`Number`] 无法在不提供 [`ProvenanceRecord`] 的情况下构造
//! 2. **渲染前强制自检**：[`Report::check_provenance`] 遍历全部数字与图表，
//!    任一缺失即返回错误——**缺源即拒绝出报告**（工程不变量②）
//! 3. **provenance 是 DAG**：`derives_from` 指向上游，使"这个数字从哪来"可逐级回溯到
//!    工具调用 / 事件 seq / 数据文件
//!
//! # 为什么用类型强制而非约定
//!
//! 若允许"先建数字、后补 provenance"，那在工程压力下总会有数字漏补。
//! 使 [`Number::new`] **必须**接收 provenance，则"漏补"在类型层面不可能——
//! 这比代码评审可靠。
//!
//! # 与渲染层的分离
//!
//! 本 crate **不**绘图、**不**调 pandoc。它只产出：
//! - 图表**规格**（[`FigureSpec`]）——描述"画什么、用什么数据"
//! - 报告**结构**（四层）
//!
//! 实际渲染由进程隔离的 Python 侧车（matplotlib）与 pandoc 完成。
//! 这样做的理由（Harness §15.1）：**数字必须由 Rust 确定性代码产生，
//! 渲染侧不得参与计算**。若允许渲染层算数，provenance 链就断了。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// provenance 标识。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProvenanceId(pub String);

impl fmt::Display for ProvenanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// 产生数值的来源类别。
///
/// 与 Harness §15.4 的 `ProvenanceKind` 对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceKind {
    /// 工具直接产出（`game_solve` / `monte_carlo` / …）。
    ToolOutput,
    /// 上传或抓取落盘的数据文件。
    DataFile,
    /// checkpoint 答案（铁律 2）。
    UserAnswer,
    /// 行业先验——报告**必须**标注 "borrowed，非本对手实证"。
    PriorBorrowed,
    /// LLM 提案（`focal_llm` 聚点判断、novel move 候选）——**须经引擎评估**。
    LlmProposal,
    /// Rust 代码计算（求解器、选择引擎、Monte Carlo）。
    CodeComputed,
    /// **探索性数值任务的产出**（`sandbox_exec` 这类沙箱）。
    ///
    /// # 为什么必须有这个变体（实测缺口）
    ///
    /// 设计文档 §5.3 与 Harness R8 都规定：`sandbox_exec` 的产出
    /// （LLM 临时写的数值代码的结果）**不得进报告**——它只能用于探索，
    /// 任何要进报告的结论必须由 Rust 侧重写。
    ///
    /// 而本枚举此前**没有表达它的方式**，于是：
    /// - 沙箱结果若被引用，只能标成 `ToolOutput` 或 `CodeComputed`
    ///   ——**两者都会让它合法地进报告**
    /// - `TrustLevel::Exploratory` 那条规则（"报告生成器拒绝读取"）
    ///   **在生产路径上无强制点**（`check_reportable` 只在测试里被调用）
    ///
    /// 加这个变体是让规则**可强制**的第一步：有了它，
    /// `check_provenance` 才能识别并拒绝。
    /// （若缺它，规则就只能靠人记得——而本项目的经验是：靠人记的规则必漏。）
    Exploratory,
}

impl ProvenanceKind {
    /// 该来源**是否被允许进入报告**。
    ///
    /// 探索性产出不允许——见 [`ProvenanceKind::Exploratory`] 的文档。
    pub fn reportable(self) -> bool {
        !matches!(self, ProvenanceKind::Exploratory)
    }

    /// 该来源是否需要特别标注。
    ///
    /// `PriorBorrowed` 必须在报告中显式标注"非本对手实证"——
    /// 否则读者会把行业先验误当成本对手的实证数据。这是 §12 风险表"先验标注 borrowed"的要求。
    pub fn requires_caveat(self) -> Option<&'static str> {
        match self {
            ProvenanceKind::PriorBorrowed => Some("行业先验（borrowed，非本对手实证）"),
            ProvenanceKind::LlmProposal => Some("由 LLM 提案，已经引擎评估"),
            // 本条正常路径下不该出现（`reportable()` 会先拒绝它）。
            // 保留说明是为了：若有人放宽了 `reportable()`，报告里至少会
            // 带一句警告，而不是悄悄把探索性数字当结论。
            ProvenanceKind::Exploratory => Some(
                "**探索性数值任务的产出**——不得作为结论依据，须由确定性代码重算",
            ),
            _ => None,
        }
    }
}

/// 内容哈希（blake3），用于校验数据文件未被改动。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ContentHash(pub String);

impl ContentHash {
    /// 从字节计算。
    pub fn of(bytes: &[u8]) -> Self {
        ContentHash(blake3::hash(bytes).to_hex().to_string())
    }
}

/// 工具调用引用。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ToolCallRef {
    /// 工具 wire 名（如 `game_solve`）。
    pub tool: String,
    /// 调用 id。
    pub call_id: String,
}

/// LLM 调用引用——用于区分"LLM 提案"与"引擎算出"。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LlmCallRef {
    pub model: String,
    pub call_id: String,
}

/// 代码版本标识：crate + 修订 + 参数哈希。
///
/// **为何要参数哈希**：同一份代码在不同参数下可能给出不同数字。
/// 只记 crate 版本无法复现具体结果；参数哈希使"这个数字是这份代码在这组参数下算出的"可验证。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CodeRev {
    pub crate_name: String,
    pub crate_version: String,
    pub param_hash: String,
}

/// provenance 记录（Harness §15.4 的结构）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    pub id: ProvenanceId,
    pub kind: ProvenanceKind,
    /// 产生该数字的工具调用。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ToolCallRef>,
    /// 日志锚点：该数值首次出现在日志中的事件序号。
    ///
    /// **这是最关键的字段**：它使"从报告数字反查到日志行"成为可能。
    /// 故它是必填（非 `Option`）。
    pub event_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<ContentHash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_rev: Option<CodeRev>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm_call: Option<LlmCallRef>,
    /// 上游来源（DAG）：本次数字由哪些其他数字派生。
    #[serde(default)]
    pub derives_from: Vec<ProvenanceId>,
    pub produced_at_ms: i64,
}

impl ProvenanceRecord {
    /// 最小构造：只给 `kind` 与 `event_seq`。
    ///
    /// `event_seq` 是必需的——没有日志锚点的数字无法被审计，
    /// 故本构造器**不允许**省略它。
    pub fn new(id: impl Into<String>, kind: ProvenanceKind, event_seq: u64) -> Self {
        Self {
            id: ProvenanceId(id.into()),
            kind,
            tool_call: None,
            event_seq,
            source_path: None,
            source_hash: None,
            code_rev: None,
            llm_call: None,
            derives_from: Vec::new(),
            produced_at_ms: 0,
        }
    }

    pub fn with_tool(mut self, tool: impl Into<String>, call_id: impl Into<String>) -> Self {
        self.tool_call = Some(ToolCallRef {
            tool: tool.into(),
            call_id: call_id.into(),
        });
        self
    }

    pub fn with_source_file(mut self, path: impl Into<String>, hash: ContentHash) -> Self {
        self.source_path = Some(path.into());
        self.source_hash = Some(hash);
        self
    }

    pub fn with_code(mut self, crate_name: &str, version: &str, param_hash: impl Into<String>) -> Self {
        self.code_rev = Some(CodeRev {
            crate_name: crate_name.to_string(),
            crate_version: version.to_string(),
            param_hash: param_hash.into(),
        });
        self
    }

    pub fn with_llm(mut self, model: impl Into<String>, call_id: impl Into<String>) -> Self {
        self.llm_call = Some(LlmCallRef {
            model: model.into(),
            call_id: call_id.into(),
        });
        self
    }

    pub fn derived_from(mut self, ups: impl IntoIterator<Item = ProvenanceId>) -> Self {
        self.derives_from = ups.into_iter().collect();
        self
    }

    pub fn at(mut self, ms: i64) -> Self {
        self.produced_at_ms = ms;
        self
    }
}

/// provenance 台账：登记全部记录，支持 DAG 校验。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceLedger {
    records: BTreeMap<ProvenanceId, ProvenanceRecord>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProvenanceError {
    #[error("provenance 记录 `{0}` 重复登记")]
    DuplicateId(ProvenanceId),
    #[error("provenance `{id}` 引用了不存在的上游 `{upstream}`")]
    DanglingRef {
        id: ProvenanceId,
        upstream: ProvenanceId,
    },
    #[error("provenance DAG 存在环，涉及 `{0}`")]
    Cycle(ProvenanceId),
    #[error("数字 `{0}` 缺少 provenance——铁律 1 要求每个数字可溯源")]
    MissingProvenance(String),
    #[error("图表 `{0}` 引用了未登记的数字 `{1}`")]
    UnknownNumberRef(String, String),
    #[error("报告中没有任何数字——空报告不应通过自检")]
    EmptyReport,
    /// **探索性产出不得进报告**（设计文档 §5.3、Harness R8）。
    #[error(
        "数字 `{number}` 的来源是**探索性数值任务**（沙箱）——不得作为结论依据。         若该结果确实要进报告，须由**确定性 Rust 代码重算**：         数字的可信性来自「代码是我们自己写的、可复现的」，         不来自「它在沙箱里跑过」。"
    )]
    ExploratoryNotReportable { number: String },
    #[error("数据文件哈希不匹配：`{path}` 登记 {expected}，实际 {actual}")]
    HashMismatch {
        path: String,
        expected: String,
        actual: String,
    },
}

impl ProvenanceLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条记录。重复 id 报错（不静默覆盖——覆盖会让审计链丢失历史）。
    pub fn register(&mut self, rec: ProvenanceRecord) -> Result<(), ProvenanceError> {
        if self.records.contains_key(&rec.id) {
            return Err(ProvenanceError::DuplicateId(rec.id));
        }
        self.records.insert(rec.id.clone(), rec);
        Ok(())
    }

    pub fn get(&self, id: &ProvenanceId) -> Option<&ProvenanceRecord> {
        self.records.get(id)
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// 校验全部 `derives_from` 指向已登记记录（无悬空引用）。
    pub fn check_refs(&self) -> Result<(), ProvenanceError> {
        for (id, rec) in &self.records {
            for up in &rec.derives_from {
                if !self.records.contains_key(up) {
                    return Err(ProvenanceError::DanglingRef {
                        id: id.clone(),
                        upstream: up.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// 检测 DAG 中的环（有环则"溯源"会无限递归）。
    pub fn check_acyclic(&self) -> Result<(), ProvenanceError> {
        // 三色标记 DFS
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Gray,
            Black,
        }
        fn visit(
            id: &ProvenanceId,
            records: &BTreeMap<ProvenanceId, ProvenanceRecord>,
            colors: &mut BTreeMap<ProvenanceId, Color>,
        ) -> Result<(), ProvenanceError> {
            colors.insert(id.clone(), Color::Gray);
            if let Some(rec) = records.get(id) {
                for up in &rec.derives_from {
                    match colors.get(up).copied().unwrap_or(Color::White) {
                        Color::Gray => return Err(ProvenanceError::Cycle(id.clone())),
                        Color::White => visit(up, records, colors)?,
                        Color::Black => {}
                    }
                }
            }
            colors.insert(id.clone(), Color::Black);
            Ok(())
        }

        let mut colors: BTreeMap<ProvenanceId, Color> = self
            .records
            .keys()
            .map(|k| (k.clone(), Color::White))
            .collect();
        for id in self.records.keys() {
            if colors.get(id).copied() == Some(Color::White) {
                visit(id, &self.records, &mut colors)?;
            }
        }
        Ok(())
    }

    /// 追溯一个 provenance 的完整上游闭包（含自身），供"这个数字从哪来"的审计。
    ///
    /// 返回按 id 排序的集合（确定性）。调用前应已 `check_acyclic`，否则可能不终止。
    pub fn trace(&self, id: &ProvenanceId) -> BTreeSet<ProvenanceId> {
        let mut acc = BTreeSet::new();
        let mut stack = vec![id.clone()];
        while let Some(cur) = stack.pop() {
            if !acc.insert(cur.clone()) {
                continue; // 已访问（DAG 中允许菱形依赖）
            }
            if let Some(rec) = self.records.get(&cur) {
                stack.extend(rec.derives_from.iter().cloned());
            }
        }
        acc
    }

    /// 校验数据文件哈希未被改动。
    ///
    /// **为何要这一步**：登记时的哈希若与实际文件不符，说明数据在报告生成后被改过——
    /// 那报告里的数字就不再可由该文件复现，审计链实际断裂。
    pub fn verify_file_hash(
        &self,
        id: &ProvenanceId,
        actual_bytes: &[u8],
    ) -> Result<(), ProvenanceError> {
        let Some(rec) = self.records.get(id) else {
            return Ok(());
        };
        let (Some(path), Some(expected)) = (&rec.source_path, &rec.source_hash) else {
            return Ok(());
        };
        let actual = ContentHash::of(actual_bytes);
        if actual != *expected {
            return Err(ProvenanceError::HashMismatch {
                path: path.clone(),
                expected: expected.0.clone(),
                actual: actual.0,
            });
        }
        Ok(())
    }
}

/// 一个带 provenance 的数值。
///
/// **构造即必须提供 provenance**——这是铁律 1 的类型级强制。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Number {
    /// 数值标识（报告内唯一）。
    pub key: String,
    pub value: f64,
    /// 人类可读标签（进报告）。
    pub label: String,
    /// 单位（可选）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    pub provenance: ProvenanceId,
}

impl Number {
    /// 唯一构造入口：`provenance` 是必填参数。
    pub fn new(
        key: impl Into<String>,
        value: f64,
        label: impl Into<String>,
        provenance: ProvenanceId,
    ) -> Self {
        Self {
            key: key.into(),
            value,
            label: label.into(),
            unit: None,
            provenance,
        }
    }

    pub fn with_unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = Some(unit.into());
        self
    }

    /// 展示文本（含单位）。
    pub fn display(&self) -> String {
        match &self.unit {
            Some(u) => format!("{} {u}", self.value),
            None => format!("{}", self.value),
        }
    }
}

/// 图表规格——**描述**要画什么，不负责绘制。
///
/// 绘制由进程隔离的 Python 侧车完成。**侧车不得计算**：
/// 规格里的数据点全部由 Rust 算出并已带 provenance。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FigureSpec {
    pub id: String,
    /// 图类型（`bar` / `heatmap` / `tree` / `line` / `tornado` …）。
    pub kind: String,
    pub title: String,
    /// 数据点：键为序列名，值为数字 key 列表（**引用** [`Number`]，不内联数值）。
    ///
    /// **为何引用而非内联**：引用使"图上每个点都能反查到 provenance"成立。
    /// 内联数值会让图与 provenance 脱钩。
    pub series: BTreeMap<String, Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y_label: Option<String>,
}

impl FigureSpec {
    pub fn new(id: impl Into<String>, kind: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            title: title.into(),
            series: BTreeMap::new(),
            x_label: None,
            y_label: None,
        }
    }

    pub fn with_series(mut self, name: impl Into<String>, number_keys: Vec<String>) -> Self {
        self.series.insert(name.into(), number_keys);
        self
    }

    /// 该图引用的全部数字 key。
    pub fn referenced_keys(&self) -> Vec<&str> {
        self.series
            .values()
            .flat_map(|v| v.iter().map(|s| s.as_str()))
            .collect()
    }
}

/// 报告章节（四层结构的某一层）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Section {
    /// 层标识：`boss` / `scenario` / `profile` / `appendix`。
    pub layer: String,
    pub title: String,
    /// 正文段落（可为 markdown）。
    pub paragraphs: Vec<String>,
    /// 该章节引用的数字 key。
    pub number_keys: Vec<String>,
    /// 该章节引用的图 id。
    pub figure_ids: Vec<String>,
}

impl Section {
    pub fn new(layer: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            layer: layer.into(),
            title: title.into(),
            paragraphs: Vec::new(),
            number_keys: Vec::new(),
            figure_ids: Vec::new(),
        }
    }

    pub fn with_paragraph(mut self, p: impl Into<String>) -> Self {
        self.paragraphs.push(p.into());
        self
    }

    pub fn with_numbers(mut self, keys: Vec<String>) -> Self {
        self.number_keys = keys;
        self
    }

    pub fn with_figures(mut self, ids: Vec<String>) -> Self {
        self.figure_ids = ids;
        self
    }
}

/// 报告。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub case_id: String,
    pub sections: Vec<Section>,
    pub numbers: BTreeMap<String, Number>,
    pub figures: BTreeMap<String, FigureSpec>,
    pub ledger: ProvenanceLedger,
}

/// 四层结构的层标识常量（与设计文档 §9.1 对应）。
pub const LAYER_BOSS: &str = "boss";
pub const LAYER_SCENARIO: &str = "scenario";
pub const LAYER_PROFILE: &str = "profile";
pub const LAYER_APPENDIX: &str = "appendix";

impl Report {
    pub fn new(case_id: impl Into<String>) -> Self {
        Self {
            case_id: case_id.into(),
            sections: Vec::new(),
            numbers: BTreeMap::new(),
            figures: BTreeMap::new(),
            ledger: ProvenanceLedger::new(),
        }
    }

    /// 登记 provenance（供 [`Number`] 引用）。
    pub fn register_provenance(&mut self, rec: ProvenanceRecord) -> Result<(), ProvenanceError> {
        self.ledger.register(rec)
    }

    /// 加入一个数字。**要求其 provenance 已登记**——否则报错。
    /// 台账中已登记的 provenance 记录数（供报告元信息引用）。
    pub fn ledger_len(&self) -> usize {
        self.ledger.len()
    }

    pub fn add_number(&mut self, n: Number) -> Result<(), ProvenanceError> {
        if self.ledger.get(&n.provenance).is_none() {
            return Err(ProvenanceError::MissingProvenance(n.key));
        }
        self.numbers.insert(n.key.clone(), n);
        Ok(())
    }

    pub fn add_figure(&mut self, f: FigureSpec) {
        self.figures.insert(f.id.clone(), f);
    }

    pub fn add_section(&mut self, s: Section) {
        self.sections.push(s);
    }

    /// **渲染前的强制自检**——本 crate 的核心职责。
    ///
    /// 检查项：
    /// 1. 台账内部一致：无悬空引用、无环
    /// 2. 每个 [`Number`] 的 provenance 已登记
    /// 3. 每个图表引用的数字 key 都存在
    /// 4. 每个章节引用的数字 key 与图 id 都存在
    /// 5. 报告非空（空报告不应通过）
    ///
    /// **任一失败即返回错误**。调用方**不得**在自检失败时出报告——
    /// 缺源的报告违反铁律 1，宁可不出。
    pub fn check_provenance(&self) -> Result<(), ProvenanceError> {
        if self.numbers.is_empty() {
            return Err(ProvenanceError::EmptyReport);
        }
        self.ledger.check_refs()?;
        self.ledger.check_acyclic()?;

        for (key, n) in &self.numbers {
            let Some(rec) = self.ledger.get(&n.provenance) else {
                return Err(ProvenanceError::MissingProvenance(key.clone()));
            };
            // **探索性产出不得进报告**（设计文档 §5.3 / Harness R8）。
            //
            // 这条检查此前**无法实现**——因为 `ProvenanceKind` 没有表达
            // "探索性"的变体。缺了它，沙箱结果只能被标成 ToolOutput 或
            // CodeComputed，而两者都会让它**合法地**进报告。
            if !rec.kind.reportable() {
                return Err(ProvenanceError::ExploratoryNotReportable {
                    number: key.clone(),
                });
            }
        }

        for (fid, f) in &self.figures {
            for key in f.referenced_keys() {
                if !self.numbers.contains_key(key) {
                    return Err(ProvenanceError::UnknownNumberRef(
                        fid.clone(),
                        key.to_string(),
                    ));
                }
            }
        }

        for s in &self.sections {
            for key in &s.number_keys {
                if !self.numbers.contains_key(key) {
                    return Err(ProvenanceError::UnknownNumberRef(
                        format!("section:{}", s.title),
                        key.clone(),
                    ));
                }
            }
            for fid in &s.figure_ids {
                if !self.figures.contains_key(fid) {
                    return Err(ProvenanceError::UnknownNumberRef(
                        format!("section:{}", s.title),
                        format!("figure:{fid}"),
                    ));
                }
            }
        }

        Ok(())
    }

    /// 生成 provenance 附录（第④层技术附录的内容之一）。
    ///
    /// 逐数字列出：标签、值、来源类别、事件 seq、上游依赖。
    /// **这是审计链的人可读出口**。
    pub fn provenance_appendix(&self) -> String {
        let mut out = String::from("# 数字溯源清单\n\n");
        out.push_str(
            "本报告每个数字的来源如下。`事件序号` 指向会话日志中的锚点，可据以回放。\n\n",
        );
        out.push_str("| 数字 | 值 | 来源类别 | 事件序号 | 上游依赖 |\n");
        out.push_str("|---|---|---|---|---|\n");
        for n in self.numbers.values() {
            let Some(rec) = self.ledger.get(&n.provenance) else {
                // 自检应已拦住这种情形；保底显示以免静默
                out.push_str(&format!(
                    "| {} | {} | **缺少 provenance** | — | — |\n",
                    n.label,
                    n.display()
                ));
                continue;
            };
            let deps = if rec.derives_from.is_empty() {
                "—".to_string()
            } else {
                rec.derives_from
                    .iter()
                    .map(|d| d.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let kind = match rec.kind {
                ProvenanceKind::ToolOutput => "工具产出",
                ProvenanceKind::DataFile => "数据文件",
                ProvenanceKind::UserAnswer => "用户确认",
                ProvenanceKind::PriorBorrowed => "行业先验",
                ProvenanceKind::LlmProposal => "LLM 提案",
                ProvenanceKind::CodeComputed => "代码计算",
                // 正常路径下不该出现（`check_provenance` 会先拒绝它）；
                // 但 provenance 表是**诊断输出**，它应当能展示任意记录
                // ——包括不合规的那些。故作可读映射而非 panic。
                ProvenanceKind::Exploratory => "⚠ 探索性（不得作结论）",
            };
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                n.label,
                n.display(),
                kind,
                rec.event_seq,
                deps
            ));
        }

        // 需标注的来源单独列出（`PriorBorrowed` 必须显式提醒）
        let caveats: Vec<(&Number, &'static str)> = self
            .numbers
            .values()
            .filter_map(|n| {
                self.ledger
                    .get(&n.provenance)
                    .and_then(|rec| rec.kind.requires_caveat().map(|c| (n, c)))
            })
            .collect();
        if !caveats.is_empty() {
            out.push_str("\n## 需注意的来源标注\n\n");
            for (n, c) in caveats {
                out.push_str(&format!("- **{}**：{}\n", n.label, c));
            }
        }
        out
    }

    /// 从某个数字出发的完整溯源链（人可读）。
    pub fn trace_number(&self, key: &str) -> Option<Vec<ProvenanceId>> {
        let n = self.numbers.get(key)?;
        Some(self.ledger.trace(&n.provenance).into_iter().collect())
    }
}

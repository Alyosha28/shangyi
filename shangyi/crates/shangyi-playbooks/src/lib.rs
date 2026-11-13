//! 商奕剧本加载与配置分层（设计文档 §13）。
//!
//! # 整行替换语义（本 crate 的核心）
//!
//! 配置分层采用**整行替换**：同 `id` 的层条目**完全替换**该行，**绝不深合并**。
//!
//! ## 为什么不深合并（§2.4 工程不变量③）
//!
//! 深合并是"为什么这个超时还是 30？"的经典来源——某字段从低层存活，
//! 因为上层只想改一个兄弟字段。
//!
//! dsh 的原文说明对商奕尤其贴切：只写 `timeout: 30` 的补丁"可能用一份字段不完整
//! 的新 config 覆盖整行，而不是仅修改超时"。整行替换要求覆盖者**完整表达最终意图**，
//! 代价是不能依赖未写出的继承；好处是避免多层嵌套对象与数组在"追加、替换还是去重"上
//! 形成难以解释的隐式合并。
//!
//! 对咨询 agent（层为 bundle 默认 → 事务所政策 → 委托剧本 → 一次性 CLI 覆盖），
//! 整行替换使生效配置**可检视、可 diff**——这正是设计文档与审计所需。
//!
//! # ⚠️ 作用范围（Harness §13.4.1 的边界）
//!
//! 本 crate 的整行替换**只管辖商奕自有配置**（剧本行栈）。
//! **Codex 基座**的 TOML 层栈是**深合并**（已实测证实：`merge.rs` 对表递归、
//! 对非表整体替换）。基座侧需要"完整重述配置"的例外清单，但那不在本 crate 范围——
//! 把两者混为一谈会让读者以为整行替换是全局规则，而它不是。

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PlaybookError {
    #[error("剧本 id 为空")]
    EmptyId,
    #[error("剧本 `{id}` 的行 `{row}` 其 plugin 名为空")]
    EmptyPlugin { id: String, row: String },
    #[error("剧本 `{id}` 含重复行 id `{row}`")]
    DuplicateRowId { id: String, row: String },
    #[error("层 `{layer}` 的补丁 `{patch}` 未匹配任何行（id 不存在）")]
    UnmatchedPatch { layer: String, patch: String },
    #[error("YAML 解析失败：{0}")]
    Yaml(String),
    #[error("合并后无任何行——空配置不应通过")]
    EmptyResult,
}

/// 一行插件配置（与 dsh 的 loader row 同形）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Row {
    /// 行标识（整行替换按此匹配）。
    pub id: String,
    /// 插件名（如 `shangyi.persona`、`solver.rust`）。
    pub plugin: String,
    /// 该行的完整配置。
    ///
    /// **整行替换的语义要求**：覆盖者必须给出**完整**的 `config`，
    /// 而非增量字段。故类型是完整的 `Value` 而非 `Option<Value>`。
    #[serde(default, skip_serializing_if = "serde_yaml::Value::is_null")]
    pub config: serde_yaml::Value,
    /// 条件禁用（v0.1 用 `platform:` 键替代 dsh 的 `!!js` 表达式）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// 是否禁用。禁用行仍参与替换（可被上层重新启用）。
    #[serde(default)]
    pub disabled: bool,
}

impl Row {
    pub fn new(id: impl Into<String>, plugin: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            plugin: plugin.into(),
            config: serde_yaml::Value::Null,
            platform: None,
            disabled: false,
        }
    }

    /// 该行在当前平台上是否生效。
    ///
    /// `platform` 为 `None` ⇒ 全平台生效；否则须与 `current` 相等。
    pub fn active_on(&self, current: &str) -> bool {
        !self.disabled
            && match &self.platform {
                None => true,
                Some(p) => p == current,
            }
    }
}

/// 一层（层名 + 该层的行列表）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layer {
    /// 层标识（用于错误信息与可检视性，如 `bundled` / `firm` / `playbook` / `cli`）。
    pub name: String,
    pub rows: Vec<Row>,
}

impl Layer {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            rows: Vec::new(),
        }
    }

    pub fn with_rows(name: impl Into<String>, rows: Vec<Row>) -> Self {
        Self {
            name: name.into(),
            rows,
        }
    }
}

/// 合并产物：生效配置 + 每行的来源层（可检视性的关键）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Composed {
    /// 生效行，按 id 排序（确定性）。
    pub rows: Vec<Row>,
    /// 行 id → 提供该行的**最终**层名。
    ///
    /// **这是整行替换的审计出口**：报告与 `--dump-config` 据此回答"这行是谁定的"。
    /// 深合并无法给出这个答案（一个行的字段可能来自多层），这正是整行替换的好处。
    pub origin: BTreeMap<String, String>,
    /// 曾被覆盖的 (行 id, 层名) 记录——完整历史，不只最终值。
    pub history: Vec<(String, String)>,
}

impl Composed {
    pub fn get(&self, id: &str) -> Option<&Row> {
        self.rows.iter().find(|r| r.id == id)
    }

    /// 某行的来源层。
    pub fn origin_of(&self, id: &str) -> Option<&str> {
        self.origin.get(id).map(|s| s.as_str())
    }

    /// 当前平台上生效的行。
    pub fn active_on(&self, platform: &str) -> Vec<&Row> {
        self.rows.iter().filter(|r| r.active_on(platform)).collect()
    }
}

/// **整行替换**地合并多层配置。
///
/// # 语义
///
/// - 层按数组顺序应用，**后者胜**
/// - 同 `id` 的行被**完全替换**（不是字段合并）
/// - 新 `id` 追加
/// - 每行的最终来源记入 [`Composed::origin`]，全部覆盖历史记入 [`Composed::history`]
///
/// # 与 dsh 的一致性
///
/// dsh 的 `applyEntryPatches` 明确：patch **替换该行的整个 config**，
/// 不做深合并；插入的行可被同一 patch 列表中后续的条目命中。
/// 本函数实现同样的语义。
pub fn compose(layers: &[Layer]) -> Result<Composed, PlaybookError> {
    let mut rows: Vec<Row> = Vec::new();
    let mut origin: BTreeMap<String, String> = BTreeMap::new();
    let mut history: Vec<(String, String)> = Vec::new();

    for layer in layers {
        // 层内重复 id：报错而非静默后者胜——静默会让"我明明写了两行"变成难查的错误
        let mut seen = BTreeSet::new();
        for row in &layer.rows {
            if row.id.trim().is_empty() {
                return Err(PlaybookError::EmptyId);
            }
            if row.plugin.trim().is_empty() {
                return Err(PlaybookError::EmptyPlugin {
                    id: layer.name.clone(),
                    row: row.id.clone(),
                });
            }
            if !seen.insert(row.id.clone()) {
                return Err(PlaybookError::DuplicateRowId {
                    id: layer.name.clone(),
                    row: row.id.clone(),
                });
            }

            match rows.iter().position(|r| r.id == row.id) {
                Some(idx) => {
                    history.push((row.id.clone(), layer.name.clone()));
                    // **整行替换**：整个 Row 被换掉，不保留任何旧字段
                    rows[idx] = row.clone();
                }
                None => rows.push(row.clone()),
            }
            origin.insert(row.id.clone(), layer.name.clone());
        }
    }

    if rows.is_empty() {
        return Err(PlaybookError::EmptyResult);
    }

    // 按 id 排序保证确定性（层的应用顺序已体现在 origin 里）
    rows.sort_by(|a, b| a.id.cmp(&b.id));

    Ok(Composed {
        rows,
        origin,
        history,
    })
}

/// 剧本元信息（`playbook.yml` 的 `meta` 段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlaybookMeta {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub order: i32,
    #[serde(default)]
    pub version: u32,
}

/// 价值网五向扫描的深度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanDepth {
    Deep,
    Medium,
    Shallow,
    Skip,
}

/// 剧本（`playbook.yml`）。
///
/// 字段与设计文档 §13.3 的示例一一对应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playbook {
    pub meta: PlaybookMeta,
    /// 适用条件（声明项 1）。
    #[serde(default)]
    pub applicable_when: Vec<String>,
    /// 失效条件——与适用条件同等重要：说明**何时不该用本剧本**。
    #[serde(default)]
    pub fails_when: Vec<String>,
    #[serde(default)]
    pub required_inputs: Vec<String>,
    /// 价值网侧重（声明项 2）。
    #[serde(default)]
    pub value_net: Option<ValueNetSpec>,
    /// intake 问题清单（声明项 3）。
    #[serde(default)]
    pub intake: Vec<IntakeQuestion>,
    /// 推荐博弈模型（声明项 4）。
    #[serde(default)]
    pub models: Vec<String>,
    /// 选择器配置（声明项 5）。
    #[serde(default)]
    pub selectors: BTreeMap<String, f64>,
    /// 工具白名单（剧本即白名单来源）。
    #[serde(default)]
    pub tool_allowlist: Vec<String>,
    /// 报告模板变量。
    #[serde(default)]
    pub report_vars: BTreeMap<String, String>,
}

/// 价值网侧重规格。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValueNetSpec {
    /// 五向扫描优先级。
    #[serde(default)]
    pub priority: Vec<String>,
    /// 各方向的扫描深度。
    #[serde(default)]
    pub scan_depth: BTreeMap<String, ScanDepth>,
}

/// intake 问题。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntakeQuestion {
    pub id: String,
    pub text: String,
    /// `choice` / `text` / `number`。
    pub kind: String,
    #[serde(default)]
    pub options: Vec<String>,
    /// 该问题的答案如何抽取为结构化字段。
    #[serde(default)]
    pub extracts: Option<serde_yaml::Value>,
}

impl Playbook {
    /// 从 YAML 载入。
    ///
    /// **用真的 YAML 解析而非自造格式**：剧本由领域专家编写（§13.7 的杠杆点），
    /// YAML 是他们熟悉的形式；且 YAML 支持注释，而注释对"为什么这样配"的传达不可替代。
    pub fn from_yaml(s: &str) -> Result<Self, PlaybookError> {
        serde_yaml::from_str(s).map_err(|e| PlaybookError::Yaml(e.to_string()))
    }

    pub fn to_yaml(&self) -> Result<String, PlaybookError> {
        serde_yaml::to_string(self).map_err(|e| PlaybookError::Yaml(e.to_string()))
    }

    /// 校验剧本内部一致性。
    ///
    /// 检查项：
    /// - 元信息非空
    /// - intake 问题 id 唯一
    /// - `choice` 型问题必须有选项（否则用户无从选择）
    /// - 选择器权重非负有限
    /// - 若声明了 `applicable_when`，也应有 `fails_when`（失效条件同等重要）
    pub fn validate(&self) -> Result<(), PlaybookError> {
        if self.meta.id.trim().is_empty() {
            return Err(PlaybookError::EmptyId);
        }
        let mut seen = BTreeSet::new();
        for q in &self.intake {
            if !seen.insert(q.id.clone()) {
                return Err(PlaybookError::DuplicateRowId {
                    id: self.meta.id.clone(),
                    row: q.id.clone(),
                });
            }
            if q.kind == "choice" && q.options.is_empty() {
                return Err(PlaybookError::Yaml(format!(
                    "intake 问题 `{}` 是 choice 型但无选项——用户无从选择",
                    q.id
                )));
            }
        }
        for (k, &w) in &self.selectors {
            if !w.is_finite() || w < 0.0 {
                return Err(PlaybookError::Yaml(format!(
                    "选择器 `{k}` 的权重非法：{w}（须有限且非负）"
                )));
            }
        }
        if !self.applicable_when.is_empty() && self.fails_when.is_empty() {
            return Err(PlaybookError::Yaml(
                "声明了 applicable_when 但未声明 fails_when——\
                 说明「何时不该用本剧本」与「何时该用」同等重要"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// 该剧本的工具白名单是否包含某工具。
    pub fn allows_tool(&self, tool: &str) -> bool {
        self.tool_allowlist.iter().any(|t| t == tool)
    }
}

/// 内置剧本的搜索根（rank 越小越优先，同 id 时先到先得）。
///
/// 与 §13.7 的 skill rank 表同构（`仅借鉴设计` 自 dsh）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlaybookRoot {
    /// 内置四剧本（随发布物）。
    Bundled,
    /// 用户级（`$SHANGYI_HOME/playbooks`）。
    User,
    /// 项目级（`<projectRoot>/.shangyi/playbooks`）——优先级最高。
    Project,
}

impl PlaybookRoot {
    pub fn rank(self) -> u32 {
        match self {
            PlaybookRoot::Bundled => 600,
            PlaybookRoot::User => 400,
            PlaybookRoot::Project => 100,
        }
    }
}

/// 剧本选择结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedPlaybook {
    pub playbook: Playbook,
    pub root: PlaybookRoot,
    /// 是否发生了遮蔽（同 id 的低 rank 剧本被高 rank 遮蔽）。
    pub shadowed: Vec<PlaybookRoot>,
}

/// 从多个候选中选择剧本：**重名时较低 rank 胜出**。
///
/// 与 dsh 的 skill 遮蔽规则一致（`仅借鉴设计`）。
pub fn select_playbook(
    candidates: &[(PlaybookRoot, Playbook)],
    id: &str,
) -> Option<SelectedPlaybook> {
    let mut hits: Vec<&(PlaybookRoot, Playbook)> = candidates
        .iter()
        .filter(|(_, p)| p.meta.id == id)
        .collect();
    if hits.is_empty() {
        return None;
    }
    // rank 升序 = 优先级降序
    hits.sort_by_key(|(r, _)| r.rank());
    let (win_root, win) = hits[0];
    let shadowed: Vec<PlaybookRoot> = hits[1..].iter().map(|(r, _)| *r).collect();
    Some(SelectedPlaybook {
        playbook: win.clone(),
        root: *win_root,
        shadowed,
    })
}

//! LLM-Wiki 知识库层：页 + **链接图** + 索引页（设计文档 R22）。
//!
//! # 它解决什么问题
//!
//! 本 crate 原有的检索是**关键词匹配**（`keyword_candidates`）。对一份策展 KB，
//! 那不够：领域知识的关键结构是**"这条理论在什么条件下失效、该转去哪条"**，
//! 而关键词匹配看不见这种关系。
//!
//! 十年前的解法是向量 RAG（\u{00a7}7.1 原案：Chroma + BGE）。本层选择
//! **LLM-Wiki 路线**——把知识组织成**带链接图的页面**，检索是**导航式**的：
//! 模型先读索引定位，再顺着链接下钻。
//!
//! 该路线源自 Karpathy 的 LLM knowledge base 想法（"把文档当作 LLM 维护、
//! 增量生长的知识产物"），TencentDB-Agent-Memory 的 LLM-Wiki 组件实现了它。
//! 本模块**自己实现**，理由见下。
//!
//! # 为什么自研而不接 TencentDB-Agent-Memory（MIT，许可合规）
//!
//! 那个项目的接入方式是**把 Agent 的 base URL 指向它的 Proxy**。这对商奕
//! 不可接受，因为它破坏**不变量①（模型可见即可审计）**：
//! 我们在 `RequestHeader` 里记录发出的工具白名单哈希、模型 slug、温度，
//! 而一个中间的 Proxy 可以**改写请求**——那么日志记录的就不再是模型实际
//! 收到的东西。审计链断在中间，且**从我们这侧看不见**。
//!
//! 保留那份架构里真正有价值的部分（链接图 + 增量生长 + 索引导航），
//! 丢弃会把审计权交出去的接入方式。
//!
//! # 与 R16 的关系（不冲突，是它的自然延伸）
//!
//! R16 已定：**同一份 markdown 为唯一真相源，矢量索引为派生投影**。
//! 本层把"派生投影"从"矢量索引"换成"**链接图**"——同样是派生、同样可重建，
//! 但**确定性**（同一份 markdown 恒得同一张图）且**可解释**（为什么检索到
//! 这一页：因为从 A 页的第 12 行链接过来）。
//!
//! # 边界（铁律 1 的延伸）
//!
//! LLM 在这里做两件事：**结构化**（把文档整理成页）与**导航**（按图下钻）。
//! 它**不发明**理论内容——页的内容来自策展/领域专家。这与"数字必须来自
//! 确定性代码"同源：**领域主张必须来自可追溯的来源**。

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::TheoryEntry;

/// 页面类型（决定索引里的分组，也提示读者该页的性质）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// 博弈论理论（四要素 schema）。
    Theory,
    /// 方法论（怎么做某件事，非理论）。
    Method,
    /// 概念/术语。
    Concept,
    /// 案例簇（从案例库归纳出的模式）。
    Case,
    /// 其它笔记。
    Note,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PageKind::Theory => "theory",
            PageKind::Method => "method",
            PageKind::Concept => "concept",
            PageKind::Case => "case",
            PageKind::Note => "note",
        }
    }
    /// 中文名（进索引与报告）。
    pub fn label_zh(self) -> &'static str {
        match self {
            PageKind::Theory => "理论",
            PageKind::Method => "方法论",
            PageKind::Concept => "概念",
            PageKind::Case => "案例",
            PageKind::Note => "笔记",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "theory" => Some(PageKind::Theory),
            "method" => Some(PageKind::Method),
            "concept" => Some(PageKind::Concept),
            "case" => Some(PageKind::Case),
            "note" => Some(PageKind::Note),
            _ => None,
        }
    }
}

/// 一页。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WikiPage {
    /// 页 id（链接指向它）。在整份 KB 内唯一。
    pub id: String,
    pub title: String,
    pub kind: PageKind,
    /// 一句话摘要——**索引页只用它**，故必须短（进模型上下文）。
    pub summary: String,
    /// 正文（markdown，可含 `[[链接]]`）。
    pub body: String,
    /// 除正文外还参与链接扫描的文本字段。
    ///
    /// # 为什么链接不止在正文里
    ///
    /// 商奕的知识里有一条最重要的关系：**"这条理论在什么条件下失效、该转去哪条"**。
    /// 它写在 `fails_when` 里（如"强容量约束（此时 Bertrand 失真，转
    /// [[cournot-numeric]]）"），而不在正文。若只扫正文，这张最关键的图就丢了。
    #[serde(default)]
    pub extra_link_text: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl WikiPage {
    /// 从理论条目构造（R16 要求四要素 schema 原样保留，故这里是**转换**而非替代）。
    pub fn from_theory(e: &TheoryEntry) -> Self {
        let mut extra = Vec::new();
        extra.extend(e.applicable_when.iter().cloned());
        extra.extend(e.fails_when.iter().cloned());
        extra.extend(e.data_needs.iter().cloned());
        extra.extend(e.outputs.iter().cloned());

        // 摘要取正文首句（索引页要短）；无正文时退回名称
        let summary = first_sentence(&e.body)
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| e.name.clone());

        Self {
            id: e.theory_id.clone(),
            title: e.name.clone(),
            kind: PageKind::Theory,
            summary,
            body: e.body.clone(),
            extra_link_text: extra,
            tags: if e.family.is_empty() {
                Vec::new()
            } else {
                vec![e.family.clone()]
            },
        }
    }

    /// 该页所有参与链接扫描的文本（正文 + extra）。
    fn link_sources(&self) -> Vec<&str> {
        let mut v = vec![self.body.as_str()];
        v.extend(self.extra_link_text.iter().map(|s| s.as_str()));
        v
    }
}

/// 一条链接。`label` 是**链接的理由**，不是"相关"这类空话。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WikiLink {
    /// 目标页 id。
    pub target: String,
    /// `[[id|理由]]` 里的理由。`None` 表示裸链接。
    pub label: Option<String>,
    /// 出现在**原文第几行**（1-based）。
    ///
    /// 记它有两个用途：断链报错能指出"哪一行写错了"；
    /// 检索时能回答"这一页为什么被取到"——**可解释性是选这条路线的核心理由**。
    pub line: usize,
}

/// KB 的完整性问题。
///
/// # 为什么这些问题**必须可见**
///
/// 断链若被静默忽略，读者（模型或人）会以为"这条理论没有后续"，
/// 而实际是**有人拼错了 id**。两种情形在页面上长得一样，但含义相反：
/// 前者是知识主张，后者是笔误。故它们必须被区分出来。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WikiIssue {
    #[error("第 {line} 行的链接 `[[{target}]]` 指向不存在的页（来源：`{from}`）")]
    BrokenLink {
        from: String,
        target: String,
        line: usize,
    },
    #[error("页 `{id}` 既不在索引中、也没有任何入链——它不会被检索到（孤儿页）")]
    Orphan { id: String },
    #[error("页 `{id}` 的摘要为空——索引页需要它来定位")]
    EmptySummary { id: String },
    #[error("页 `{from}` 以 `{target}` 为链接目标，但目标页的 id 与自身相同（自环）")]
    SelfLink { from: String, target: String },
}

/// 重复 id（无法消歧，故直接拒绝构造）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum WikiError {
    #[error("页 id 重复：`{0}`——id 是链接目标，重复会让链接指向不确定的页")]
    DuplicateId(String),
    #[error("页 id 非法（不得为空、不得含 `[[`/`]]`/空白）：`{0}`")]
    InvalidId(String),
}

/// 知识库的链接图。
#[derive(Debug, Clone)]
pub struct Wiki {
    pages: BTreeMap<String, WikiPage>,
    /// 出链（含断链——断链不入 `pages` 但要在 `issues` 里报出来）。
    outbound: BTreeMap<String, Vec<WikiLink>>,
    /// 入链（仅统计**已解析**的链接；断链没有入链可言）。
    inbound: BTreeMap<String, Vec<String>>,
    issues: Vec<WikiIssue>,
}

impl Wiki {
    /// 构造。
    ///
    /// **只有"无法消歧"的情况才返回 `Err`**（重复/非法 id）；
    /// 断链、孤儿等**内容质量问题**进入 [`Wiki::issues`]，由调用方决定如何处置。
    ///
    /// 这个划分是刻意的：
    /// - 重复 id 让"链接指向哪一页"不确定 ⇒ 图本身不成立 ⇒ 必须拒绝
    /// - 断链只是"某条边指向空" ⇒ 图仍成立，但**必须被报出来**
    pub fn build(pages: impl IntoIterator<Item = WikiPage>) -> Result<Self, WikiError> {
        let mut map: BTreeMap<String, WikiPage> = BTreeMap::new();
        for p in pages {
            validate_id(&p.id)?;
            if map.contains_key(&p.id) {
                return Err(WikiError::DuplicateId(p.id));
            }
            map.insert(p.id.clone(), p);
        }

        let mut outbound: BTreeMap<String, Vec<WikiLink>> = BTreeMap::new();
        let mut inbound: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for id in map.keys() {
            inbound.insert(id.clone(), Vec::new());
        }

        let mut issues = Vec::new();
        for (id, page) in &map {
            let mut links = Vec::new();
            for text in page.link_sources() {
                links.extend(parse_links(text));
            }
            for l in &links {
                if l.target == *id {
                    issues.push(WikiIssue::SelfLink {
                        from: id.clone(),
                        target: l.target.clone(),
                    });
                    continue;
                }
                if map.contains_key(&l.target) {
                    inbound
                        .entry(l.target.clone())
                        .or_default()
                        .push(id.clone());
                    inbound.get_mut(&l.target).expect("刚插入").sort();
                    inbound.get_mut(&l.target).expect("刚插入").dedup();
                } else {
                    issues.push(WikiIssue::BrokenLink {
                        from: id.clone(),
                        target: l.target.clone(),
                        line: l.line,
                    });
                }
            }
            outbound.insert(id.clone(), links);
        }

        for (id, page) in &map {
            if page.summary.trim().is_empty() {
                issues.push(WikiIssue::EmptySummary { id: id.clone() });
            }
            // 孤儿：没有任何入链（且不是"根"页——根页由调用方在索引里显式列出）
            let has_inbound = inbound.get(id).map(|v| !v.is_empty()).unwrap_or(false);
            if !has_inbound {
                issues.push(WikiIssue::Orphan { id: id.clone() });
            }
        }

        Ok(Self {
            pages: map,
            outbound,
            inbound,
            issues,
        })
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
    pub fn get(&self, id: &str) -> Option<&WikiPage> {
        self.pages.get(id)
    }
    pub fn ids(&self) -> BTreeSet<&str> {
        self.pages.keys().map(|s| s.as_str()).collect()
    }
    /// 内容质量问题（断链、孤儿、空摘要、自环）。
    pub fn issues(&self) -> &[WikiIssue] {
        &self.issues
    }
    /// 是否没有任何问题（供 CI 与启动自检用）。
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }
    /// 要求干净——**调用方用它把"内容有错"变成启动失败**。
    pub fn require_clean(&self) -> Result<(), Vec<WikiIssue>> {
        if self.issues.is_empty() {
            Ok(())
        } else {
            Err(self.issues.clone())
        }
    }

    /// 某页的出链（含断链——**不静默丢弃**，调用方可据此提示"这条链接坏了"）。
    pub fn outbound(&self, id: &str) -> &[WikiLink] {
        self.outbound.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 某页的入链来源。
    pub fn inbound(&self, id: &str) -> &[String] {
        self.inbound.get(id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// 广度优先取 `depth` 跳内的邻页（不含起点）。用于"下钻"。
    pub fn neighbors(&self, id: &str, depth: usize) -> Vec<&WikiPage> {
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(id.to_string());
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        queue.push_back((id.to_string(), 0));
        let mut out = Vec::new();
        while let Some((cur, d)) = queue.pop_front() {
            if d >= depth {
                continue;
            }
            for l in self.outbound(&cur) {
                if seen.insert(l.target.clone()) {
                    if let Some(p) = self.pages.get(&l.target) {
                        out.push(p);
                        queue.push_back((l.target.clone(), d + 1));
                    }
                }
            }
        }
        out
    }

    /// 渲染索引页（**模型先读它定位**）。
    ///
    /// # 格式取舍
    ///
    /// 索引会进模型上下文，故**紧凑优先**：只给 id、一行摘要、出链目标。
    /// 不给正文、不给入链（入链对定位没用，只对导航有用，下钻时再给）。
    ///
    /// 按 `PageKind` 分组：模型通常知道自己要找的是"理论"还是"方法论"，
    /// 分组让它能**跳过整组**而不是逐条扫。
    pub fn render_index(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("# 知识库索引（{} 页）\n\n", self.pages.len()));
        s.push_str(
            "> 用法：先在本页定位目标，再用 `kb_retrieve(page=\"<id>\")` 取该页正文与它的链接。\n\
             > `→` 后的括号里是该条链接的**理由**（不是「相关」，是「为什么指向它」）。\n\n",
        );

        // 有问题时**必须写在索引顶部**——索引是模型的入口，
        // 把问题藏在末尾等于没写。
        if !self.issues.is_empty() {
            s.push_str(&format!(
                "> ⚠ 本知识库有 {} 处内容问题（断链/孤儿/空摘要）。\
                 检索仍可用，但缺失的链接**不代表没有后续**，可能是拼写错误。\n\n",
                self.issues.len()
            ));
        }

        let mut by_kind: BTreeMap<PageKind, Vec<&WikiPage>> = BTreeMap::new();
        for p in self.pages.values() {
            by_kind.entry(p.kind).or_default().push(p);
        }
        for (kind, mut pages) in by_kind {
            pages.sort_by(|a, b| a.id.cmp(&b.id));
            s.push_str(&format!("## {}（{}）\n", kind.label_zh(), pages.len()));
            for p in pages {
                s.push_str(&format!("- **{}** — {}\n", p.id, p.summary.trim()));
                let links = self.outbound(&p.id);
                if !links.is_empty() {
                    let rendered: Vec<String> = links
                        .iter()
                        .take(6) // 索引里只给前几条，全量在页内
                        .map(|l| match &l.label {
                            Some(r) => format!("[[{}]]（{}）", l.target, r),
                            None => format!("[[{}]]", l.target),
                        })
                        .collect();
                    let more = if links.len() > 6 {
                        format!(" …另有 {} 条", links.len() - 6)
                    } else {
                        String::new()
                    };
                    s.push_str(&format!("    → {}{}\n", rendered.join("；"), more));
                }
            }
            s.push('\n');
        }
        s
    }

    /// 渲染单页（**下钻**时用）：正文 + 出链（带理由）+ 入链（谁引用了我）。
    ///
    /// 入链是 LLM-Wiki 的关键：它让模型能回答"**还有谁提到了这个概念**"，
    /// 而不必遍历全库。向量检索做不到这件事。
    pub fn render_page(&self, id: &str) -> Option<String> {
        let p = self.pages.get(id)?;
        let mut s = String::new();
        s.push_str(&format!("# {}（{}）\n\n", p.title, p.id));
        s.push_str(&format!("_{}_\n\n", p.kind.label_zh()));
        if !p.summary.trim().is_empty() {
            s.push_str(&format!("**摘要**：{}\n\n", p.summary.trim()));
        }
        if !p.tags.is_empty() {
            s.push_str(&format!("**标签**：{}\n\n", p.tags.join("、")));
        }
        s.push_str("---\n\n");
        s.push_str(p.body.trim());
        s.push_str("\n\n");

        let outs = self.outbound(id);
        if !outs.is_empty() {
            s.push_str("## 出链\n\n");
            for l in outs {
                let broken = if self.pages.contains_key(&l.target) {
                    ""
                } else {
                    "  ⚠ **断链**（目标页不存在——可能是拼写错误）"
                };
                match &l.label {
                    Some(r) => s.push_str(&format!("- [[{}]]（{}）{}\n", l.target, r, broken)),
                    None => s.push_str(&format!("- [[{}]]{}\n", l.target, broken)),
                }
            }
            s.push('\n');
        }

        let ins = self.inbound(id);
        if !ins.is_empty() {
            s.push_str("## 入链（谁引用本页）\n\n");
            for src in ins {
                s.push_str(&format!("- [[{}]]\n", src));
            }
            s.push('\n');
        }
        Some(s)
    }

    /// 关键词检索（**兜底**，不是主路径）。
    ///
    /// 主路径是"读索引 → 下钻"。关键词只在模型不知道该从哪页开始、
    /// 或想确认"库里有没有提过 X"时用。
    ///
    /// 排序：命中 id/title 的权重大于命中正文的；同权重按 id 有序（确定性）。
    pub fn search(&self, keywords: &[String]) -> Vec<(&WikiPage, usize)> {
        let mut scored: Vec<(&WikiPage, usize)> = Vec::new();
        for p in self.pages.values() {
            let mut score = 0usize;
            let id_l = p.id.to_lowercase();
            let title_l = p.title.to_lowercase();
            let summary_l = p.summary.to_lowercase();
            let body_l = p.body.to_lowercase();
            for k in keywords {
                let k = k.to_lowercase();
                if k.is_empty() {
                    continue;
                }
                if id_l.contains(&k) || title_l.contains(&k) {
                    score += 10;
                }
                if summary_l.contains(&k) {
                    score += 3;
                }
                if body_l.contains(&k) {
                    score += 1;
                }
            }
            if score > 0 {
                scored.push((p, score));
            }
        }
        scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.id.cmp(&b.0.id)));
        scored
    }
}

fn validate_id(id: &str) -> Result<(), WikiError> {
    if id.trim().is_empty() || id.contains("[[") || id.contains("]]") || id.contains(char::is_whitespace) {
        return Err(WikiError::InvalidId(id.to_string()));
    }
    Ok(())
}

/// 从 markdown 文本里解析 `[[target]]` 与 `[[target|理由]]`。
///
/// # 三处必须正确处理的地方（都有测试）
///
/// 1. **代码块内的 `[[...]]` 不是链接**。文档里举例说明语法时会写在代码块中，
///    若把它当链接，就会凭空造出一条边——而那是**假的图**。
/// 2. **行内代码 `` `[[x]]` `` 同理**。
/// 3. **行号必须准**：断链报告要指到具体行，否则作者要去全文件找。
pub fn parse_links(text: &str) -> Vec<WikiLink> {
    let mut out = Vec::new();
    let mut in_fence: Option<String> = None;

    for (i, raw_line) in text.lines().enumerate() {
        let line_no = i + 1;

        // 代码围栏切换（``` 或 ~~~）
        let trimmed = raw_line.trim_start();
        if let Some(marker) = fence_marker(trimmed) {
            match &in_fence {
                Some(open) if open == marker => in_fence = None,
                Some(_) => {} // 不同标记的围栏在围栏内不生效
                None => in_fence = Some(marker.to_string()),
            }
            continue;
        }
        if in_fence.is_some() {
            continue;
        }

        // 去掉行内代码，避免把 `[[x]]` 当链接
        let line = strip_inline_code(raw_line);

        let bytes = line.as_bytes();
        let mut idx = 0usize;
        while idx + 1 < bytes.len() {
            if bytes[idx] == b'[' && bytes[idx + 1] == b'[' {
                // 找配对的 ]]
                if let Some(close) = line[idx + 2..].find("]]") {
                    let inner = &line[idx + 2..idx + 2 + close];
                    if let Some(link) = parse_inner(inner, line_no) {
                        out.push(link);
                    }
                    idx = idx + 2 + close + 2;
                    continue;
                } else {
                    // 未闭合的 `[[`：不是链接，跳过（不报错——正文里出现 `[[`
                    // 本身是合法的，例如在讲 Markdown 语法）
                    break;
                }
            }
            // 按字符推进（多字节安全）
            match line[idx..].chars().next() {
                Some(c) => idx += c.len_utf8(),
                None => break,
            }
        }
    }
    out
}

/// 若该行是代码围栏，返回其标记（``` 或 ~~~，含长度无关的归一化）。
fn fence_marker(line: &str) -> Option<&'static str> {
    if line.starts_with("```") {
        Some("```")
    } else if line.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// 把行内代码（反引号包裹）替换成空格，使其中的 `[[x]]` 不参与解析。
fn strip_inline_code(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    let mut consecutive = 0usize;
    for ch in line.chars() {
        if ch == '`' {
            consecutive += 1;
            // 单个或成对的反引号都切换状态（`` 与 ` 都算定界符）
            in_code = !in_code;
            out.push(' ');
            continue;
        }
        if in_code {
            if ch.is_whitespace() {
                out.push(ch);
            } else {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
        consecutive = 0;
    }
    let _ = consecutive;
    out
}

fn parse_inner(inner: &str, line: usize) -> Option<WikiLink> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    match inner.split_once('|') {
        Some((t, label)) => {
            let t = t.trim();
            if t.is_empty() {
                return None;
            }
            Some(WikiLink {
                target: t.to_string(),
                label: Some(label.trim().to_string()).filter(|s| !s.is_empty()),
                line,
            })
        }
        None => Some(WikiLink {
            target: inner.to_string(),
            label: None,
            line,
        }),
    }
}

/// 取正文首句作摘要（索引页要短）。
///
/// 按中文句号、问号、感叹号与 ASCII 句点切；取不到则整段（截断到 120 字符）。
fn first_sentence(body: &str) -> Option<String> {
    let trimmed = body.trim_start_matches(|c: char| c == '#' || c.is_whitespace());
    if trimmed.is_empty() {
        return None;
    }
    let mut end = trimmed.len();
    for (i, ch) in trimmed.char_indices() {
        if matches!(ch, '。' | '！' | '？' | '\n') {
            end = i;
            break;
        }
    }
    let s = trimmed[..end].trim();
    if s.is_empty() {
        return None;
    }
    Some(truncate_chars(s, 120))
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let t: String = s.chars().take(max).collect();
    format!("{t}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(id: &str, body: &str) -> WikiPage {
        WikiPage {
            id: id.into(),
            title: id.into(),
            kind: PageKind::Theory,
            summary: format!("{id} 的摘要。"),
            body: body.into(),
            extra_link_text: vec![],
            tags: vec![],
        }
    }

    // ── 链接解析 ──

    #[test]
    fn parses_plain_and_labeled_links() {
        let links = parse_links("见 [[bertrand]] 与 [[cournot|容量约束强时改用]]。");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "bertrand");
        assert_eq!(links[0].label, None);
        assert_eq!(links[1].target, "cournot");
        assert_eq!(links[1].label.as_deref(), Some("容量约束强时改用"));
    }

    #[test]
    fn link_line_numbers_are_accurate() {
        let text = "第一行\n第二行 [[a]]\n第三行\n第四行 [[b]]";
        let links = parse_links(text);
        assert_eq!(links[0].line, 2, "断链报告要指到准确行，否则作者要全文件找");
        assert_eq!(links[1].line, 4);
    }

    /// **代码块里的 `[[...]]` 不是链接**——否则会凭空造出假边。
    #[test]
    fn links_inside_code_fences_are_ignored() {
        let text = "\
正文 [[real]]

```markdown
这是语法示例：[[fake]]
```
";
        let links = parse_links(text);
        assert_eq!(links.len(), 1, "围栏内的不得被解析：{links:?}");
        assert_eq!(links[0].target, "real");
    }

    #[test]
    fn links_inside_inline_code_are_ignored() {
        let links = parse_links("用 `[[x]]` 表示链接，例如 [[real]]。");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "real");
    }

    #[test]
    fn tilde_fences_also_guard_links() {
        let links = parse_links("~~~\n[[fake]]\n~~~\n[[real]]");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "real");
    }

    #[test]
    fn unclosed_bracket_pair_is_not_a_link() {
        // 讲 Markdown 语法时会出现未闭合的 `[[`
        let links = parse_links("写成 [[ 是未闭合的，不是链接。");
        assert!(links.is_empty());
    }

    #[test]
    fn empty_or_whitespace_link_targets_are_skipped() {
        assert!(parse_links("[[  ]]").is_empty());
        assert!(parse_links("[[|理由]]").is_empty());
        assert!(parse_links("[[]]").is_empty());
    }

    /// 多字节安全：中文与 emoji 不得让逐字节扫描切坏字符。
    #[test]
    fn multibyte_content_does_not_break_parsing() {
        let links = parse_links("📊 中文 [[目标页]] 与 emoji 🎯 [[b]]");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "目标页");
    }

    // ── 图构造与完整性 ──

    #[test]
    fn builds_graph_with_inbound_and_outbound() {
        let w = Wiki::build(vec![
            page("a", "指向 [[b]]"),
            page("b", "被 a 指向"),
        ])
        .unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w.outbound("a").len(), 1);
        assert_eq!(w.outbound("a")[0].target, "b");
        assert_eq!(w.inbound("b"), &["a".to_string()]);
        assert!(w.inbound("a").is_empty());
    }

    /// 断链**必须报出来**，不得静默当成"这条理论没有后续"。
    #[test]
    fn broken_link_is_reported_not_silently_dropped() {
        let w = Wiki::build(vec![page("a", "指向 [[不存在的页]]")]).unwrap();
        assert!(!w.is_clean());
        assert!(
            w.issues().iter().any(|i| matches!(
                i,
                WikiIssue::BrokenLink { target, line: 1, .. } if target == "不存在的页"
            )),
            "应报告断链：{:?}",
            w.issues()
        );
        assert!(
            w.require_clean().is_err(),
            "require_clean 必须能被启动自检用来拒绝带错的库"
        );
    }

    #[test]
    fn orphan_page_is_reported() {
        let w = Wiki::build(vec![page("a", "无链接"), page("b", "指向 [[a]]")]).unwrap();
        // a 有入链（来自 b），b 没有
        assert!(
            w.issues().iter().any(|i| matches!(i, WikiIssue::Orphan { id } if id == "b")),
            "应报告孤儿页：{:?}",
            w.issues()
        );
    }

    #[test]
    fn self_link_is_reported_and_not_counted_as_inbound() {
        let w = Wiki::build(vec![page("a", "自指 [[a]]")]).unwrap();
        assert!(w
            .issues()
            .iter()
            .any(|i| matches!(i, WikiIssue::SelfLink { .. })));
        assert!(w.inbound("a").is_empty(), "自环不得被算作入链");
    }

    #[test]
    fn duplicate_id_is_a_hard_error() {
        let e = Wiki::build(vec![page("a", ""), page("a", "")]).unwrap_err();
        assert!(matches!(e, WikiError::DuplicateId(_)));
    }

    #[test]
    fn invalid_id_is_rejected() {
        for bad in ["", "  ", "a b", "a[[b", "a]]b"] {
            assert!(
                Wiki::build(vec![page(bad, "")]).is_err(),
                "id `{bad}` 应被拒"
            );
        }
    }

    /// **链接不止在正文里**：`fails_when` 里的"转 [[x]]"是最关键的一条边。
    #[test]
    fn links_are_also_scanned_from_extra_fields() {
        let mut p = page("bertrand", "差异化价格竞争。");
        p.extra_link_text = vec!["强容量约束（此时结论失真，转 [[cournot-numeric]]）".into()];
        let w = Wiki::build(vec![p, page("cournot-numeric", "数量竞争数值路径")]).unwrap();
        assert!(
            w.outbound("bertrand").iter().any(|l| l.target == "cournot-numeric"),
            "fails_when 里的链接必须进图——它是'失效时转去哪'的表达"
        );
        assert_eq!(w.inbound("cournot-numeric"), &["bertrand".to_string()]);
    }

    #[test]
    fn from_theory_preserves_four_element_schema() {
        let e = TheoryEntry {
            theory_id: "t".into(),
            name: "测试理论".into(),
            version: "1".into(),
            family: "noncooperative".into(),
            applicable_when: vec!["完全信息".into()],
            fails_when: vec!["信息不对称（转 [[signaling]]）".into()],
            data_needs: vec!["支付矩阵".into()],
            outputs: vec!["均衡集".into()],
            body: "这是正文。第二句不该进摘要。".into(),
        };
        let p = WikiPage::from_theory(&e);
        assert_eq!(p.id, "t");
        assert_eq!(p.title, "测试理论");
        assert_eq!(p.summary, "这是正文", "摘要应取首句");
        assert_eq!(p.tags, vec!["noncooperative".to_string()]);
        // 四要素都进了链接扫描面
        assert_eq!(p.extra_link_text.len(), 4);
    }

    // ── 导航 ──

    #[test]
    fn neighbors_walks_breadth_first_up_to_depth() {
        let w = Wiki::build(vec![
            page("a", "[[b]]"),
            page("b", "[[c]]"),
            page("c", "[[d]]"),
            page("d", "终点"),
        ])
        .unwrap();
        let n1 = w.neighbors("a", 1);
        assert_eq!(n1.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), vec!["b"]);
        let n2 = w.neighbors("a", 2);
        assert_eq!(
            n2.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        let n3 = w.neighbors("a", 3);
        assert_eq!(n3.len(), 3);
    }

    #[test]
    fn neighbors_do_not_loop_on_cycles() {
        let w = Wiki::build(vec![page("a", "[[b]]"), page("b", "[[a]]")]).unwrap();
        let n = w.neighbors("a", 10);
        assert_eq!(n.len(), 1, "环不得导致无限展开：{:?}", n.iter().map(|p| &p.id).collect::<Vec<_>>());
    }

    // ── 渲染 ──

    #[test]
    fn index_groups_by_kind_and_shows_link_reasons() {
        let mut a = page("a", "指向 [[b|因为要对比]]");
        a.kind = PageKind::Theory;
        let mut b = page("b", "被指向");
        b.kind = PageKind::Method;
        let w = Wiki::build(vec![a, b]).unwrap();
        let idx = w.render_index();
        assert!(idx.contains("## 理论（1）"), "{idx}");
        assert!(idx.contains("## 方法论（1）"), "{idx}");
        assert!(idx.contains("**a**"), "{idx}");
        assert!(
            idx.contains("[[b]]（因为要对比）"),
            "索引必须带链接的**理由**，否则读者只能猜：{idx}"
        );
        // 用法说明必须在（模型靠它知道下一步怎么做）
        assert!(idx.contains("kb_retrieve"), "{idx}");
    }

    /// 有内容问题时，索引**顶部**必须警示——藏起来等于没写。
    #[test]
    fn index_warns_about_issues_at_the_top() {
        let w = Wiki::build(vec![page("a", "[[缺失]]")]).unwrap();
        let idx = w.render_index();
        let head: String = idx.lines().take(6).collect::<Vec<_>>().join("\n");
        assert!(
            head.contains("内容问题") && head.contains("不代表"),
            "警示必须在顶部，且要点明'缺失不代表没有后续'：{head}"
        );
    }

    #[test]
    fn page_renders_outbound_inbound_and_flags_broken() {
        let w = Wiki::build(vec![
            page("a", "指向 [[b]] 与 [[断了]]"),
            page("b", "b 的正文"),
            page("c", "c 引用 [[b]]"),
        ])
        .unwrap();
        let out = w.render_page("b").unwrap();
        assert!(out.contains("## 出链") || out.contains("b 的正文"));
        assert!(out.contains("## 入链（谁引用本页）"), "{out}");
        assert!(out.contains("[[a]]") && out.contains("[[c]]"), "入链应含 a 与 c：{out}");

        let out_a = w.render_page("a").unwrap();
        assert!(
            out_a.contains("断链") && out_a.contains("拼写错误"),
            "断链必须在该页上标出来：{out_a}"
        );
        assert!(w.render_page("不存在").is_none());
    }

    #[test]
    fn search_prefers_id_and_title_hits_over_body_hits() {
        let mut by_title = page("x", "正文里也有那个词：定价");
        by_title.title = "定价理论".into();
        let by_body = page("y", "正文提到定价一次。");
        let w = Wiki::build(vec![by_title, by_body]).unwrap();
        let r = w.search(&["定价".into()]);
        assert_eq!(r[0].0.id, "x", "命中标题的应排在命中正文的前面");
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn search_is_deterministic_on_ties() {
        let w = Wiki::build(vec![page("b", "关键词"), page("a", "关键词")]).unwrap();
        let r1: Vec<&str> = w.search(&["关键词".into()]).iter().map(|(p, _)| p.id.as_str()).collect();
        let r2: Vec<&str> = w.search(&["关键词".into()]).iter().map(|(p, _)| p.id.as_str()).collect();
        assert_eq!(r1, r2);
        assert_eq!(r1, vec!["a", "b"], "同分按 id 有序");
    }

    #[test]
    fn first_sentence_handles_chinese_and_fallback() {
        assert_eq!(first_sentence("第一句。第二句。").unwrap(), "第一句");
        assert_eq!(first_sentence("# 标题\n\n正文").unwrap(), "标题");
        // 无终止符 ⇒ 整段（超长则截断）
        let long = "啊".repeat(200);
        let s = first_sentence(&long).unwrap();
        assert!(s.ends_with('…') && s.chars().count() <= 121);
        assert!(first_sentence("   ").is_none());
    }
}

// ────────────────────────────────────────────────────────────────────────────
// 加载器：把目录里的 markdown 变成 Wiki
// ────────────────────────────────────────────────────────────────────────────

/// 加载错误。
///
/// # 为什么分成"每个文件一类"而不是一个字符串
///
/// 目录里可能有几十个文件，报错必须**指出是哪个文件的什么问题**——
/// 否则作者要逐个排除。这与"断链报告要指到行"是同一个理由。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoadError {
    #[error("读取 {path} 失败：{detail}")]
    Io { path: String, detail: String },
    #[error("{path} 解析失败：{detail}")]
    Parse { path: String, detail: String },
    #[error("{path} 的 frontmatter 缺 `id`（或 `theory_id`）——文件名不能当 id，因为重命名会让链接失效")]
    MissingId { path: String },
}

/// 从目录加载**所有** `.md` 文件为一册 Wiki。
///
/// # 三条设计选择
///
/// 1. **id 取自 frontmatter，不取文件名**。文件名会变（改标题、整理目录），
///    而 id 是链接目标——让链接依赖文件名等于让重命名破坏全库的图。
/// 2. **`index.md` / `README.md` 被跳过**：它们是**生成的**索引页，
///    不是内容页（`render_index` 负责产出）。把它们当内容页会在图里
///    制造一个谁都不指向的假节点。
/// 3. **加载失败即报错**（不跳过坏文件）。跳过会让"少了一页"表现为
///    "从没写过这页"——而两者对用户的意义完全相反。
///
/// # ⚠ 写 frontmatter 时的一处陷阱（实测踩到）
///
/// 含 `[[链接]]` 的字符串**必须加引号**：
///
/// ```yaml
/// # ✗ 报错：did not find expected ',' or ']'
/// fails_when: [容量约束强（转 [[cournot]]）]
/// # ✓ 正确
/// fails_when: ['容量约束强（转 [[cournot]]）']
/// ```
///
/// 原因：YAML 流式序列里 `[` 是序列定界符。**本模块无法替作者规避**——
/// 那是 YAML 语法层面的事。但它值得写在这里，因为
/// "用 `[[ ]]` 表示链接"与"YAML 用 `[` 开序列"这两件事撞在一起时，
/// 报错信息（`did not find expected ',' or ']'`）完全不会提到链接。
pub fn load_dir(dir: &std::path::Path) -> Result<Wiki, LoadError> {
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    let rd = std::fs::read_dir(dir).map_err(|e| LoadError::Io {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })?;
    for e in rd {
        let e = e.map_err(|e| LoadError::Io {
            path: dir.display().to_string(),
            detail: e.to_string(),
        })?;
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let name = p.file_name().and_then(|x| x.to_str()).unwrap_or("");
        if matches!(name, "index.md" | "README.md") {
            continue;
        }
        // 跳过 macOS 的 AppleDouble 伴生文件（`._foo.md`）。
        //
        // # 为什么必须在代码里跳过（而不是只写进文档）
        //
        // 这些文件是**挂载盘/网络盘自动生成的**，用户看不到它们、也没创建它们。
        // 在挂载盘上编辑 KB 的人**必然**会遇到——实测在本项目的挂载盘上
        // 写第一批知识页就产生了 14 个 `._*.md`。
        //
        // 它们的名字以 `.md` 结尾，会被扩展名检查放过，而内容是二进制，
        // 于是 `read_to_string` 报 "stream did not contain valid UTF-8"
        // ——一个**指向错误方向的诊断**：看起来像"知识页编码坏了"，
        // 实际是操作系统生成的元数据文件。
        //
        // 跳过的正当性：它们不是内容（是 Finder 元数据），跳过不丢任何知识。
        if name.starts_with("._") || name.starts_with('.') {
            continue;
        }
        entries.push(p);
    }
    // 有序处理：错误报告与日志顺序稳定（不依赖文件系统枚举序）
    entries.sort();

    let mut pages = Vec::with_capacity(entries.len());
    for p in entries {
        let text = std::fs::read_to_string(&p).map_err(|e| LoadError::Io {
            path: p.display().to_string(),
            detail: e.to_string(),
        })?;
        // 把真实路径填进**所有**错误变体——早先只处理了 MissingId，
        // 于是"哪个文件解析失败"这个信息丢了，而错误里写着 `<inline>`。
        // 那正是我自己在注释里说要避免的（"报错必须指出是哪个文件"）。
        let path_str = p.display().to_string();
        pages.push(parse_page_md(&text).map_err(|e| {
            let detail = match &e {
                LoadError::Io { detail, .. } | LoadError::Parse { detail, .. } => detail.clone(),
                LoadError::MissingId { .. } => String::new(),
            };
            match e {
                LoadError::MissingId { .. } => LoadError::MissingId { path: path_str },
                _ => LoadError::Parse {
                    path: path_str,
                    detail,
                },
            }
        })?);
    }
    Wiki::build(pages).map_err(|e| LoadError::Parse {
        path: dir.display().to_string(),
        detail: e.to_string(),
    })
}

/// 解析一个 markdown 文件为页。
///
/// # 与 `parse_theory_md` 的关系
///
/// **共用同一份 frontmatter 约定**（R16：同一份文件既作理论条目、也作 wiki 页）。
/// 但本函数**不要求**四要素齐备——那是对 `Theory` 类页的要求，
/// 而 wiki 里还有方法论、概念、案例页（它们的 schema 不同）。
/// 若此处强制四要素，就等于把"所有页都必须是理论"写死进了加载器。
pub fn parse_page_md(text: &str) -> Result<WikiPage, LoadError> {
    let (fm, body) = crate::split_frontmatter_pub(text).map_err(|e| LoadError::Parse {
        path: String::new(), // 由 load_dir 填真实路径
        detail: e.to_string(),
    })?;
    let v: serde_yaml::Value = serde_yaml::from_str(fm).map_err(|e| LoadError::Parse {
        path: String::new(),
        detail: format!(
            "{e}\n（frontmatter 共 {} 行。常见原因：含 `[[链接]]` 或 `:` 的字符串未加引号——\
             YAML 里这些字符有语法含义。用单引号包起来最稳：\
             `fails_when: ['容量约束强（转 [[cournot]]）']`）",
            fm.lines().count()
        ),
    })?;

    // id：兼容 `id`（wiki 原生）与 `theory_id`（既有理论库 schema）
    let id = v
        .get("id")
        .or_else(|| v.get("theory_id"))
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or(LoadError::MissingId {
            path: "<inline>".into(),
        })?
        .to_string();

    let title = v
        .get("name")
        .or_else(|| v.get("title"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| id.clone());

    let kind = v
        .get("kind")
        .and_then(|x| x.as_str())
        .and_then(PageKind::parse)
        // 未声明 kind 时**按有理论四要素判断**，否则记为 Note。
        // 兜底成 Note 而不是 Theory：把方法论页错标成理论，会让索引
        // 把它们混在一起，而读者会以为那是一条可引用的理论。
        .unwrap_or_else(|| {
            if v.get("applicable_when").is_some() || v.get("fails_when").is_some() {
                PageKind::Theory
            } else {
                PageKind::Note
            }
        });

    let summary = v
        .get("summary")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| first_sentence(&body))
        .unwrap_or_default();

    // 四要素（若存在）参与链接扫描——它们是"失效时转去哪"的载体
    let mut extra = Vec::new();
    for key in ["applicable_when", "fails_when", "data_needs", "outputs"] {
        if let Some(seq) = v.get(key).and_then(|x| x.as_sequence()) {
            for it in seq {
                if let Some(s) = it.as_str() {
                    extra.push(s.to_string());
                }
            }
        }
    }

    let tags = v
        .get("tags")
        .and_then(|x| x.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .or_else(|| {
            v.get("family")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|f| vec![f.to_string()])
        })
        .unwrap_or_default();

    Ok(WikiPage {
        id,
        title,
        kind,
        summary,
        body: body.trim().to_string(),
        extra_link_text: extra,
        tags,
    })
}

#[cfg(test)]
mod loader_tests {
    use super::*;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "shangyi-wiki-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(dir: &std::path::Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn loads_pages_from_markdown() {
        let d = tmp("load");
        write(
            &d,
            "bertrand.md",
            "---\nid: bertrand\nname: 差异化 Bertrand\nfamily: noncooperative\napplicable_when: [产品有差异化]\nfails_when: ['容量约束强（转 [[cournot]]）']\n---\n\n差异化价格竞争。均衡价高于边际成本。\n",
        );
        write(
            &d,
            "cournot.md",
            "---\nid: cournot\nname: Cournot 数量竞争\n---\n\n数量竞争路径。\n",
        );
        let w = load_dir(&d).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w.get("bertrand").unwrap().title, "差异化 Bertrand");
        assert_eq!(w.get("bertrand").unwrap().kind, PageKind::Theory);
        assert_eq!(
            w.get("bertrand").unwrap().summary,
            "差异化价格竞争",
            "摘要取首句"
        );
        // fails_when 里的链接进了图
        assert!(
            w.outbound("bertrand").iter().any(|l| l.target == "cournot"),
            "fails_when 的链接必须进图"
        );
        assert_eq!(w.inbound("cournot"), &["bertrand".to_string()]);
    }

    /// `theory_id` 是既有理论库 schema 的键名，必须兼容（R16 要求四要素原样保留）。
    #[test]
    fn accepts_theory_id_as_well_as_id() {
        let d = tmp("theoryid");
        write(&d, "a.md", "---\ntheory_id: nash\nname: 纳什均衡\n---\n\n正文。\n");
        let w = load_dir(&d).unwrap();
        assert!(w.get("nash").is_some());
    }

    /// **缺 id 必须报错**——文件名不能当 id（重命名会让链接失效）。
    #[test]
    fn missing_id_is_an_error_not_a_filename_fallback() {
        let d = tmp("noid");
        write(&d, "some-file.md", "---\nname: 没有 id\n---\n\n正文。\n");
        match load_dir(&d) {
            Err(LoadError::MissingId { path }) => {
                assert!(path.contains("some-file.md"), "应指出是哪个文件：{path}");
            }
            other => panic!("缺 id 应报 MissingId，实得 {other:?}"),
        }
    }

    /// `index.md` / `README.md` 是**生成物**，不得被当内容页（否则图里多一个假节点）。
    #[test]
    fn generated_index_and_readme_are_skipped() {
        let d = tmp("skipidx");
        write(&d, "a.md", "---\nid: a\nname: A\n---\n\n正文\n");
        write(&d, "index.md", "# 索引（自动生成）\n");
        write(&d, "README.md", "# 说明\n");
        let w = load_dir(&d).unwrap();
        assert_eq!(w.len(), 1, "只应加载 a.md：{:?}", w.ids());
    }

    /// 非 `.md` 文件被忽略（图片、yaml 等）。
    #[test]
    fn non_markdown_files_are_ignored() {
        let d = tmp("nonmd");
        write(&d, "a.md", "---\nid: a\nname: A\n---\n\n正文\n");
        write(&d, "notes.txt", "不是 markdown");
        write(&d, "data.json", "{}");
        let w = load_dir(&d).unwrap();
        assert_eq!(w.len(), 1);
    }

    /// **坏文件即失败**（不跳过）——跳过会让"漏了一页"表现为"从没写过"。
    #[test]
    fn malformed_file_fails_the_whole_load() {
        let d = tmp("bad");
        write(&d, "a.md", "---\nid: a\n---\n\n好文件\n");
        write(&d, "b.md", "没有 frontmatter 的文件\n");
        assert!(
            load_dir(&d).is_err(),
            "含坏文件时必须整体失败，不得静默跳过"
        );
    }

    /// 未声明 `kind` 时：有四要素 ⇒ Theory；没有 ⇒ Note（不默认成 Theory）。
    #[test]
    fn kind_is_inferred_conservatively() {
        let d = tmp("kind");
        write(&d, "t.md", "---\nid: t\napplicable_when: [x]\n---\n\n理论内容\n");
        write(&d, "m.md", "---\nid: m\nsummary: 怎么做事\n---\n\n方法内容\n");
        let w = load_dir(&d).unwrap();
        assert_eq!(w.get("t").unwrap().kind, PageKind::Theory);
        assert_eq!(
            w.get("m").unwrap().kind,
            PageKind::Note,
            "没有四要素的不该被当成理论（那会让读者以为可引用）"
        );
    }

    #[test]
    fn explicit_kind_wins_over_inference() {
        let d = tmp("kindexp");
        write(
            &d,
            "x.md",
            "---\nid: x\nkind: method\napplicable_when: [看起来像理论]\n---\n\n方法\n",
        );
        let w = load_dir(&d).unwrap();
        assert_eq!(w.get("x").unwrap().kind, PageKind::Method);
    }

    #[test]
    fn load_order_is_deterministic_across_runs() {
        let d = tmp("order");
        for i in 0..5 {
            write(
                &d,
                &format!("p{i}.md"),
                &format!("---\nid: p{i}\nname: P{i}\n---\n\n正文\n"),
            );
        }
        let a: Vec<String> = load_dir(&d).unwrap().ids().into_iter().map(str::to_string).collect();
        let b: Vec<String> = load_dir(&d).unwrap().ids().into_iter().map(str::to_string).collect();
        assert_eq!(a, b);
        assert_eq!(a, vec!["p0", "p1", "p2", "p3", "p4"]);
    }
}

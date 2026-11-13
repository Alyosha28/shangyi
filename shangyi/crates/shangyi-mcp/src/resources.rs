//! MCP Resources（2025-06-18 规范）：把知识库 / 剧本 / 专题规程暴露成**只读 URI**。
//!
//! # 为什么要资源，而不只给工具
//!
//! `kb_retrieve` 是**检索**：模型给查询、拿回命中片段——它回答"哪几页相关"，
//! 但答不了"这一页的原文长什么样"。模型想核对一条理论的失效边界时，需要的是
//! **原文**，而不是再猜一个查询词去换回另一段截断的片段。资源（resource）在
//! MCP 里的分工正是这一件事：客户端可以按 URI 直接读，读到的就是磁盘上的字节。
//!
//! | 方法 | 本模块 | 返回 |
//! |---|---|---|
//! | `resources/list` | [`list_resources`] | `{ "resources": [...] }` |
//! | `resources/templates/list` | [`list_resource_templates`] | `{ "resourceTemplates": [...] }` |
//! | `resources/read` | [`read_resource`] | `{ "contents": [{ uri, mimeType, text }] }` |
//!
//! # 三类资源，三种 URI
//!
//! ```text
//! shangyi://kb/<slug>                          知识库 Markdown 原文（mimeType: text/markdown）
//! shangyi://playbook/<id>                      行业剧本 YAML 原文（mimeType: application/yaml）
//! shangyi://guide/store_location_research      门店选址专题规程（mimeType: text/markdown）
//! ```
//!
//! # 查找顺序与 `shangyi-cli` / [`crate::host`] 一致（但不共用函数）
//!
//! - 知识库：`$home/kb` → 仓库内 `shangyi/kb`（取**第一个存在**的目录）。
//! - 剧本：`$home/playbooks/<id>/playbook.yml` → 仓库内 `shangyi/playbooks/<id>/playbook.yml`。
//! - 选址规程：`$home/playbooks/store_location_research.md` → 仓库内同名文件。
//!
//! 三处都"先用户目录、后仓库"：`--home` 是用户的覆盖点，仓库里的资产是缺省值。
//!
//! **为什么不复用 `host::load_playbook` / `host::load_wiki`**：那两个函数返回的是
//! 领域类型（`Playbook` / `Arc<Wiki>`），而列举资源还需要"这页的 slug 对应哪个
//! 文件"——`Wiki` 的 id 来自 frontmatter（见 `shangyi_kb::wiki::load_dir` 的设计
//! 说明），拿它反推文件名会多一层猜测。故这里直接按目录列举文件，元信息才走
//! `load_dir`（与 `kb_retrieve` 的 title/summary 同源）。
//!
//! # 安全边界（路径穿越）
//!
//! URI 的 `<slug>` / `<id>` 段**只允许** ASCII 字母、数字、`_`、`-`（`is_safe_segment`）。
//! 这条校验在**任何磁盘访问之前**执行，因此 `..%2f`、`a/b`、`adverse selection`
//! 这类输入不可能变成路径。规范没有规定这段校验，但"URI 由模型给出"意味着它
//! 必须当作不可信输入处理——否则 `shangyi://kb/../../.ssh/id_rsa` 会把私钥
//! 读进模型上下文。
//!
//! # 失败语义
//!
//! [`ResourceReadError::InvalidParams`]（URI 不成话）、[`ResourceReadError::NotFound`]
//! （URI 成话但没有这个资源）、[`ResourceReadError::Io`]（资源在、但读不出来）
//! 三档分开：合成一档会让客户端无法区分"我拼错了"与"服务器坏了"。
//!
//! # 列出即可读
//!
//! [`list_resources`] 只列出**当前确实能读到**的条目（文件存在且可解析）。故
//! `resources/list` 的结果里不会出现"列出来但 read 报 NotFound"的幽灵资源；
//! 反过来，`store_location_research.md` 缺失时它也不出现在清单里——缺失是
//! 部署问题，应被 `resources/list` 的差异看出来，而不是让客户端撞一次 404。

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// 四套行业剧本 id。**顺序即 [`list_resources`] 的输出顺序**（确定性要求）。
///
/// 与 `shangyi/playbooks/` 下的目录名、`shangyi-playbooks` 的 `meta.id` 一一对应。
/// 新增剧本时改这里，并在 `shangyi/playbooks/` 放同名目录。
pub const PLAYBOOK_IDS: &[&str] = &[
    "pricing_response",
    "market_entry",
    "negotiation",
    "competitor_intel",
];

/// 门店选址专题规程的固定 URI（它没有 id 维度：只有一份）。
///
/// 不对外导出：客户端只会从 `resources/list` 拿到这个 URI，不需要在代码里拼它。
const GUIDE_URI: &str = "shangyi://guide/store_location_research";

/// 知识库资源的 URI 前缀。
const KB_PREFIX: &str = "shangyi://kb/";
/// 剧本资源的 URI 前缀。
const PLAYBOOK_PREFIX: &str = "shangyi://playbook/";

/// 选址规程的文件名（用户目录与仓库目录同名）。
const GUIDE_FILE: &str = "store_location_research.md";
/// 选址规程的展示名（与 `shangyi/playbooks/store_location_research.md` 的职责一致）。
const GUIDE_NAME: &str = "门店选址公开信息与周边尽调规程";
/// 选址规程的一句话说明。
const GUIDE_DESCRIPTION: &str = "实体门店选址、新开、续租、搬迁及同品牌加密的强制取证与测算规程";

const MIME_MARKDOWN: &str = "text/markdown";
const MIME_YAML: &str = "application/yaml";

/// `resources/read` 的失败。
///
/// 三档分开的理由见模块文档：客户端要能区分"URI 不成话""没有这个资源""资源坏了"。
/// `Eq` 让调用方（与测试）能直接比较，而不必匹配字符串。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceReadError {
    /// URI 不成话（空、slug/id 含非法字符、路径穿越企图）。
    #[error("资源 URI 非法：{0}")]
    InvalidParams(String),
    /// URI 成话，但没有这个资源。
    #[error("资源不存在：{0}")]
    NotFound(String),
    /// 资源在、但读不出来（权限、编码、IO）。
    #[error("资源读取失败：{0}")]
    Io(String),
}

/// 列出全部资源（`resources/list` 的 result）。
///
/// 顺序是确定的：
/// 1. 知识库页面按 slug 字典序；
/// 2. [`PLAYBOOK_IDS`] 的声明顺序；
/// 3. 门店选址规程（固定 URI）。
///
/// `home` 为 [`None`] 时只用仓库内资产（与 `shangyi mcp-serve` 不给 `--home` 时一致）。
pub fn list_resources(home: Option<&Path>) -> Value {
    let mut resources: Vec<Value> = Vec::new();

    if let Some(dir) = kb_dir(home) {
        // title/summary 与 `kb_retrieve` 同源（都来自 `load_dir` 的 frontmatter 解析）。
        // 加载失败不 panic：清单里退回文件名与首行标题，**至少 URI 是可读的**。
        let wiki = shangyi_kb::wiki::load_dir(&dir).ok();
        for (slug, path) in kb_pages(&dir) {
            let (name, description) = match wiki.as_ref().and_then(|w| w.get(&slug)) {
                Some(page) => (page.title.clone(), page.summary.clone()),
                None => match std::fs::read_to_string(&path) {
                    Ok(text) => page_meta(&text, &slug),
                    Err(e) => {
                        eprintln!(
                            "商奕[mcp]：知识页 {} 读取失败：{e}（清单中以 slug 占位）",
                            path.display()
                        );
                        (slug.clone(), String::new())
                    }
                },
            };
            resources.push(json!({
                "uri": format!("{KB_PREFIX}{slug}"),
                "name": name,
                "description": description,
                "mimeType": MIME_MARKDOWN,
            }));
        }
    }

    for &id in PLAYBOOK_IDS {
        let Some(path) = playbook_file(id, home) else {
            continue;
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                eprintln!(
                    "商奕[mcp]：剧本 {} 读取失败：{e}（本次不列入资源清单）",
                    path.display()
                );
                continue;
            }
        };
        // name/description 取自 `meta` 段——**由真解析器读出**，不在这里再写一份
        // YAML 解析：两份解析必然漂移，而漂移只会在用户改剧本后暴露。
        match shangyi_playbooks::Playbook::from_yaml(&text) {
            Ok(playbook) => resources.push(json!({
                "uri": format!("{PLAYBOOK_PREFIX}{id}"),
                "name": playbook.meta.name,
                "description": playbook.meta.description,
                "mimeType": MIME_YAML,
            })),
            Err(e) => eprintln!(
                "商奕[mcp]：剧本 {} 解析失败：{e}（本次不列入资源清单）",
                path.display()
            ),
        }
    }

    if guide_file(home).is_some() {
        resources.push(json!({
            "uri": GUIDE_URI,
            "name": GUIDE_NAME,
            "description": GUIDE_DESCRIPTION,
            "mimeType": MIME_MARKDOWN,
        }));
    }

    json!({ "resources": resources })
}

/// 列出资源模板（`resources/templates/list` 的 result）。
///
/// 模板是**静态**的：两个维度（kb slug / playbook id）的 URI 构造规则不随磁盘变化，
/// 具体有哪些值由 [`list_resources`] 给出。混合两者会让客户端在目录缺失时
/// 误以为"这个维度不存在"。
pub fn list_resource_templates() -> Value {
    json!({
        "resourceTemplates": [
            {
                "uriTemplate": "shangyi://kb/{slug}",
                "name": "商奕博弈论与商业分析知识库页面",
                "description": "按 slug 读取 35 篇知识库原文（含适用前提、失效边界与跳转链接）",
                "mimeType": MIME_MARKDOWN,
            },
            {
                "uriTemplate": "shangyi://playbook/{id}",
                "name": "商奕行业剧本配置",
                "description": "按剧本 ID（pricing_response / market_entry / negotiation / competitor_intel）读取 YAML 剧本",
                "mimeType": MIME_YAML,
            },
        ],
    })
}

/// 读一个资源（`resources/read` 的 result）。
///
/// 支持的 URI：
///
/// - `shangyi://kb/<slug>`：知识库页面 `.md` 原文；
/// - `shangyi://playbook/<id>`：剧本 `playbook.yml` 原文；
/// - `shangyi://guide/store_location_research`：门店选址专题规程原文。
///
/// 返回 `{ "contents": [{ "uri", "mimeType", "text" }] }`——原文**逐字**返回，
/// 不裁剪、不改写：这条链上的字节就是文件里的字节。
pub fn read_resource(uri: &str, home: Option<&Path>) -> Result<Value, ResourceReadError> {
    let uri = uri.trim();
    if uri.is_empty() {
        return Err(ResourceReadError::InvalidParams(
            "`uri` 不得为空（示例：shangyi://kb/adverse-selection）".to_string(),
        ));
    }

    if let Some(slug) = uri.strip_prefix(KB_PREFIX) {
        // 校验先于任何磁盘访问：URI 由模型给出 ⇒ 按不可信输入处理。
        if !is_safe_segment(slug) {
            return Err(ResourceReadError::InvalidParams(format!(
                "知识页 slug 非法：`{slug}`（只允许 ASCII 字母、数字、`_`、`-`，不得含路径分隔符）"
            )));
        }
        let dir = kb_dir(home).ok_or_else(|| not_found(uri))?;
        let path = kb_pages(&dir)
            .into_iter()
            .find(|(page_slug, _)| page_slug.as_str() == slug)
            .map(|(_, path)| path)
            .ok_or_else(|| not_found(uri))?;
        return Ok(contents(uri, MIME_MARKDOWN, read_text(&path)?));
    }

    if let Some(id) = uri.strip_prefix(PLAYBOOK_PREFIX) {
        if !is_safe_segment(id) {
            return Err(ResourceReadError::InvalidParams(format!(
                "剧本 id 非法：`{id}`（只允许 ASCII 字母、数字、`_`、`-`，不得含路径分隔符）"
            )));
        }
        let path = playbook_file(id, home).ok_or_else(|| not_found(uri))?;
        return Ok(contents(uri, MIME_YAML, read_text(&path)?));
    }

    if uri == GUIDE_URI {
        let path = guide_file(home).ok_or_else(|| not_found(uri))?;
        return Ok(contents(uri, MIME_MARKDOWN, read_text(&path)?));
    }

    // 未知 scheme（`https://`、`shangyi://metric/...`）与拼错的 URI 走同一档：
    // 对客户端而言都是"这个 URI 没有资源"，区分它们没有可操作价值。
    Err(not_found(uri))
}

// ─────────────────────────── 内部：定位 ───────────────────────────

/// `resources/read` 的"没有这个资源"。
fn not_found(uri: &str) -> ResourceReadError {
    ResourceReadError::NotFound(format!("Resource not found: {uri}"))
}

/// 组装 `resources/read` 的成功结果。
fn contents(uri: &str, mime: &str, text: String) -> Value {
    json!({
        "contents": [
            {
                "uri": uri,
                "mimeType": mime,
                "text": text,
            }
        ],
    })
}

/// slug / id 段是否安全：非空、长度有上限、只含 ASCII 字母数字与 `_`/`-`。
///
/// 白名单而非黑名单：黑名单（"不许含 `..`"）挡不住编码、分隔符与平台的路径怪癖。
fn is_safe_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= 128
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// 仓库内相对 `CARGO_MANIFEST_DIR` 的路径（编译期确定，故用 `env!`）。
fn manifest_path(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
}

/// 知识库目录：`$SHANGYI_KB_DIR`（仅未指定显式 `home` 时）→ `$home/kb` → 仓库 `shangyi/kb`。
fn kb_dir(home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if home.is_none() {
        if let Ok(dir) = std::env::var("SHANGYI_KB_DIR") {
            if !dir.trim().is_empty() {
                candidates.push(PathBuf::from(dir.trim()));
            }
        }
    }
    if let Some(home) = home {
        candidates.push(home.join("kb"));
    }
    candidates.push(manifest_path("../../kb"));
    candidates.into_iter().find(|dir| dir.is_dir())
}

/// 剧本文件：`$home/playbooks/<id>/playbook.yml` → 仓库同名文件，取第一个存在的文件。
fn playbook_file(id: &str, home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join("playbooks").join(id).join("playbook.yml"));
    }
    candidates.push(
        manifest_path("../../playbooks")
            .join(id)
            .join("playbook.yml"),
    );
    candidates.into_iter().find(|path| path.is_file())
}

/// 门店选址规程：`$home/playbooks/store_location_research.md` → 仓库同名文件。
fn guide_file(home: Option<&Path>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join("playbooks").join(GUIDE_FILE));
    }
    candidates.push(manifest_path("../../playbooks").join(GUIDE_FILE));
    candidates.into_iter().find(|path| path.is_file())
}

/// 目录里的 Markdown 页：`(slug, 路径)`，按 slug 字典序。
///
/// **跳过以 `.` 开头的文件**（含 macOS 挂载盘自动生成的 `._foo.md`）。这不是
/// 洁癖：`._*` 是 AppleDouble 元数据，内容是二进制，`read_to_string` 会报
/// "stream did not contain valid UTF-8"——一个指向错误方向的诊断（看起来像
/// 知识页编码坏了，实际是操作系统生成的伴随文件）。`shangyi_kb::wiki::load_dir`
/// 对同一问题有同样的跳过规则，两处必须一致，否则"列得出来但加载不了"。
fn kb_pages(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut pages: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|ft| ft.is_file()) {
            continue;
        }
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if !is_safe_segment(slug) {
            continue;
        }
        pages.push((slug.to_string(), path));
    }
    // 有序返回：清单顺序不能依赖文件系统的枚举顺序（不同机器不同）。
    pages.sort_by(|a, b| a.0.cmp(&b.0));
    pages
}

/// 读文本文件；失败归到 [`ResourceReadError::Io`] 并带上路径。
fn read_text(path: &Path) -> Result<String, ResourceReadError> {
    std::fs::read_to_string(path)
        .map_err(|e| ResourceReadError::Io(format!("读取 {} 失败：{e}", path.display())))
}

// ─────────────────────────── 内部：frontmatter 兜底 ───────────────────────────
//
// 只在 `load_dir` 不可用（目录里有页缺 `id` 等）时使用。目的不是替代真 YAML
// 解析，而是让**清单**还能给出一个像人话的 name/description，而不是让整份
// `resources/list` 因为一页写坏而变空。

/// 拆出 frontmatter 与正文（`---` 包裹）。没有 frontmatter 时返回 `None`。
fn split_frontmatter(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let after = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))?;
    let mut offset = 0usize;
    for line in after.split_inclusive('\n') {
        if line.trim_end_matches(['\n', '\r']) == "---" {
            return Some((&after[..offset], &after[offset + line.len()..]));
        }
        offset += line.len();
    }
    None
}

/// 取 frontmatter 里第一个非空的 `key: value`（`keys` 按优先序尝试）。
fn front_field<'a>(front: &'a str, keys: &[&str]) -> Option<&'a str> {
    for line in front.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if !keys.contains(&key.trim()) {
            continue;
        }
        let value = strip_quotes(value.trim());
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// 去掉 YAML 字符串两侧的成对引号（`'…'` / `"…"`）。
fn strip_quotes(s: &str) -> &str {
    for quote in ['\'', '"'] {
        if s.len() >= 2 && s.starts_with(quote) && s.ends_with(quote) {
            return &s[1..s.len() - 1];
        }
    }
    s
}

/// 页面元信息的兜底：frontmatter 的 `name`/`title`/`summary`，再退到首行标题与首段。
fn page_meta(text: &str, slug: &str) -> (String, String) {
    let (front, body) = split_frontmatter(text).unwrap_or(("", text));
    let title = front_field(front, &["name", "title"])
        .map(str::to_string)
        .or_else(|| first_heading(body).map(str::to_string))
        .unwrap_or_else(|| slug.to_string());
    // 描述要短：它进模型上下文，一整段正文会把清单撑成正文。
    let summary = match front_field(front, &["summary", "description"]) {
        Some(summary) => summary.to_string(),
        None => first_paragraph(body)
            .map(|line| truncate_chars(line, 160))
            .unwrap_or_default(),
    };
    (title, summary)
}

/// 正文里第一个 `# 标题`。
fn first_heading(body: &str) -> Option<&str> {
    body.lines()
        .map(str::trim)
        .find(|line| line.starts_with("# "))
        .map(|line| line[2..].trim())
        .filter(|title| !title.is_empty())
}

/// 正文第一段（跳过标题、空行与代码块）。
fn first_paragraph(body: &str) -> Option<&str> {
    let mut in_code_block = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block || line.is_empty() || line.starts_with('#') || line == "---" {
            continue;
        }
        return Some(line);
    }
    None
}

/// 按**字符**截断（不切坏多字节汉字），超出时加省略号。
fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 临时主目录（`$SHANGYI_HOME` 的形状）。名字带进程 id 与纳秒：
    /// 单元测试并行跑，撞目录会让一个测试删掉另一个的资产。
    fn temp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shangyi-mcp-resources-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("建临时目录");
        dir
    }

    fn resources_of(value: &Value) -> Vec<Value> {
        value["resources"]
            .as_array()
            .expect("`resources` 必须是数组")
            .clone()
    }

    fn uris_of(resources: &[Value]) -> Vec<String> {
        resources
            .iter()
            .map(|r| {
                r["uri"]
                    .as_str()
                    .expect("每个资源都必须有字符串 uri")
                    .to_string()
            })
            .collect()
    }

    fn texts_of(value: &Value) -> Vec<&str> {
        value["contents"]
            .as_array()
            .expect("`contents` 必须是数组")
            .iter()
            .map(|c| c["text"].as_str().expect("每块内容都必须有 text"))
            .collect()
    }

    #[test]
    fn playbook_ids_are_the_four_industry_playbooks() {
        assert_eq!(
            PLAYBOOK_IDS,
            &[
                "pricing_response",
                "market_entry",
                "negotiation",
                "competitor_intel"
            ]
        );
    }

    #[test]
    fn list_resources_covers_kb_playbooks_and_guide() {
        let resources = resources_of(&list_resources(None));
        assert!(
            resources.len() >= 40,
            "资源数应 ≥ 40（35 篇知识库 + 4 套剧本 + 选址规程），实际 {}",
            resources.len()
        );

        let uris = uris_of(&resources);
        for &id in PLAYBOOK_IDS {
            let uri = format!("shangyi://playbook/{id}");
            assert!(uris.contains(&uri), "缺剧本资源 {uri}：{uris:?}");
        }
        assert!(
            uris.contains(&GUIDE_URI.to_string()),
            "缺选址规程资源 {GUIDE_URI}"
        );
        assert!(
            uris.contains(&"shangyi://kb/adverse-selection".to_string()),
            "缺知识页 shangyi://kb/adverse-selection"
        );

        // 每项都必须有非空 name、mimeType 取值合法，且不含 AppleDouble 伴生文件。
        for resource in &resources {
            let uri = resource["uri"].as_str().expect("uri");
            assert!(!uri.contains("._"), "不得列出 AppleDouble 文件：{uri}");
            assert!(
                !resource["name"].as_str().unwrap_or("").trim().is_empty(),
                "{uri} 的 name 为空"
            );
            let mime = resource["mimeType"].as_str().unwrap_or("");
            assert!(
                mime == MIME_MARKDOWN || mime == MIME_YAML,
                "{uri} 的 mimeType 意外：{mime}"
            );
            assert!(
                resource["description"].is_string(),
                "{uri} 的 description 必须是字符串"
            );
        }

        // 剧本项的 name/description 必须来自 `meta` 段（真解析，不是占位）。
        let pricing = resources
            .iter()
            .find(|r| r["uri"] == "shangyi://playbook/pricing_response")
            .expect("应有 pricing_response");
        assert_eq!(pricing["name"], "调价/促销响应");
        assert!(
            pricing["description"]
                .as_str()
                .unwrap_or("")
                .contains("Bertrand"),
            "剧本 description 应来自 meta.description：{}",
            pricing["description"]
        );
    }

    #[test]
    fn list_resources_is_deterministic_and_kb_sorted_by_slug() {
        let first = list_resources(None);
        let second = list_resources(None);
        assert_eq!(first, second, "两次列举必须完全一致（确定性）");

        let uris = uris_of(&resources_of(&first));
        let kb_slugs: Vec<&str> = uris
            .iter()
            .filter_map(|uri| uri.strip_prefix("shangyi://kb/"))
            .collect();
        assert!(
            kb_slugs.len() >= 35,
            "知识库页应 ≥ 35，实际 {}",
            kb_slugs.len()
        );
        let mut sorted = kb_slugs.clone();
        sorted.sort_unstable();
        assert_eq!(kb_slugs, sorted, "知识库页必须按 slug 字典序");
        assert!(
            kb_slugs.iter().all(|slug| !slug.starts_with('.')),
            "不得出现以 `.` 开头的 slug：{kb_slugs:?}"
        );

        // 无重复 URI：重复会让客户端缓存与"按 URI 读"的语义都变得不确定。
        let mut unique = uris.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), uris.len(), "资源 URI 不得重复");
    }

    #[test]
    fn every_listed_resource_is_readable() {
        // 模块文档承诺的不变式："列出即可读"。
        for resource in resources_of(&list_resources(None)) {
            let uri = resource["uri"].as_str().expect("uri");
            let read = read_resource(uri, None)
                .unwrap_or_else(|e| panic!("列出的资源必须可读：{uri} ⇒ {e}"));
            assert_eq!(
                read["contents"][0]["mimeType"], resource["mimeType"],
                "{uri} 的 mimeType 在 list 与 read 之间不一致"
            );
            let text = read["contents"][0]["text"].as_str().expect("text");
            assert!(!text.trim().is_empty(), "{uri} 读到的原文为空");
        }
    }

    #[test]
    fn list_resource_templates_declares_kb_and_playbook() {
        let templates = list_resource_templates();
        let arr = templates["resourceTemplates"]
            .as_array()
            .expect("`resourceTemplates` 必须是数组");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["uriTemplate"], "shangyi://kb/{slug}");
        assert_eq!(arr[0]["mimeType"], MIME_MARKDOWN);
        assert_eq!(arr[1]["uriTemplate"], "shangyi://playbook/{id}");
        assert_eq!(arr[1]["mimeType"], MIME_YAML);
        for template in arr {
            assert!(
                !template["name"].as_str().unwrap_or("").trim().is_empty(),
                "模板 name 不得为空"
            );
            assert!(
                !template["description"]
                    .as_str()
                    .unwrap_or("")
                    .trim()
                    .is_empty(),
                "模板 description 不得为空"
            );
        }
    }

    #[test]
    fn read_resource_returns_kb_markdown_verbatim() {
        let value = read_resource("shangyi://kb/adverse-selection", None).expect("应能读到知识页");
        let contents = value["contents"].as_array().expect("contents");
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["uri"], "shangyi://kb/adverse-selection");
        assert_eq!(contents[0]["mimeType"], MIME_MARKDOWN);

        let text = texts_of(&value)[0];
        assert!(text.contains("id: adverse-selection"), "应含 frontmatter");
        assert!(text.contains("Akerlof"), "应含正文（逐字原文，不裁剪）");
        assert!(text.len() > 500, "原文不应被截断，实际 {} 字节", text.len());
    }

    #[test]
    fn read_resource_returns_playbook_yaml_verbatim() {
        let value =
            read_resource("shangyi://playbook/pricing_response", None).expect("应能读到剧本 YAML");
        assert_eq!(value["contents"][0]["mimeType"], MIME_YAML);
        let text = texts_of(&value)[0];
        // 原文必须能被领域解析器原样吃下——否则客户端读到的是"另一种 YAML"。
        let playbook = shangyi_playbooks::Playbook::from_yaml(text).expect("剧本 YAML 应可解析");
        assert_eq!(playbook.meta.id, "pricing_response");
        assert!(
            text.starts_with('#') || text.starts_with("meta:"),
            "应是原文首行"
        );
    }

    #[test]
    fn read_resource_returns_store_location_guide() {
        let value = read_resource(GUIDE_URI, None).expect("应能读到选址规程");
        assert_eq!(value["contents"][0]["uri"], GUIDE_URI);
        assert_eq!(value["contents"][0]["mimeType"], MIME_MARKDOWN);
        let text = texts_of(&value)[0];
        assert!(text.contains("选址"), "规程原文应含选址内容");
        assert!(text.contains("租金"), "规程原文应含租金要求");
    }

    #[test]
    fn read_resource_rejects_path_traversal_and_illegal_segments() {
        let bad_uris = [
            "shangyi://kb/../secrets",
            "shangyi://kb/..",
            "shangyi://kb/.",
            "shangyi://kb/a/b",
            "shangyi://kb/adverse-selection/",
            "shangyi://kb/%2e%2e%2fsecrets",
            "shangyi://kb/adverse selection",
            "shangyi://kb/adverse-selection.md",
            "shangyi://playbook/../../etc/passwd",
            "shangyi://playbook/pricing/response",
            "shangyi://playbook/pricing_response/",
        ];
        for uri in bad_uris {
            match read_resource(uri, None) {
                Err(ResourceReadError::InvalidParams(message)) => {
                    assert!(!message.is_empty(), "{uri} 的报错说明不得为空");
                }
                other => panic!("{uri} 应被拒绝为 InvalidParams，实际 {other:?}"),
            }
        }
        // 空 URI 同样是参数问题（不是"没有这个资源"）。
        for uri in ["", "   "] {
            assert!(matches!(
                read_resource(uri, None),
                Err(ResourceReadError::InvalidParams(_))
            ));
        }
    }

    #[test]
    fn read_resource_reports_not_found_for_unknown_uris() {
        let unknown = [
            "shangyi://kb",
            "shangyi://kb/no-such-page",
            "shangyi://playbook/no_such_playbook",
            "shangyi://guide/no-such-guide",
            "shangyi://metric/profit",
            "https://example.com/kb/adverse-selection",
        ];
        for uri in unknown {
            match read_resource(uri, None) {
                Err(ResourceReadError::NotFound(message)) => {
                    assert_eq!(
                        message,
                        format!("Resource not found: {uri}"),
                        "NotFound 文案必须带原 URI（客户端据此定位是哪个 URI）"
                    );
                }
                other => panic!("{uri} 应报 NotFound，实际 {other:?}"),
            }
        }
    }

    #[test]
    fn home_kb_overrides_repo_and_skips_appledouble_files() {
        let home = temp_home("kb");
        let kb = home.join("kb");
        std::fs::create_dir_all(&kb).expect("建 kb 目录");
        std::fs::write(
            kb.join("zz-test-page.md"),
            "---\nid: zz-test-page\nname: '本地测试页'\nsummary: '只有临时目录里才有的页'\n---\n\n# 本地测试页\n\n正文。\n",
        )
        .expect("写测试页");
        // AppleDouble 伴随文件：二进制内容 + `._` 前缀。若被当成知识页，
        // 读取会因非 UTF-8 失败——故必须在列举阶段就跳过。
        std::fs::write(kb.join("._zz-test-page.md"), [0xff, 0xfe, 0x00, 0x01])
            .expect("写 AppleDouble 文件");

        let resources = resources_of(&list_resources(Some(&home)));
        let kb_uris: Vec<String> = uris_of(&resources)
            .into_iter()
            .filter(|uri| uri.starts_with(KB_PREFIX))
            .collect();
        assert_eq!(
            kb_uris,
            vec!["shangyi://kb/zz-test-page".to_string()],
            "home/kb 存在时只列它；`._` 文件必须跳过"
        );

        let value =
            read_resource("shangyi://kb/zz-test-page", Some(&home)).expect("应读到 home 页");
        assert_eq!(value["contents"][0]["mimeType"], MIME_MARKDOWN);
        assert!(texts_of(&value)[0].contains("正文。"));

        // 取第一个存在的目录 ⇒ home/kb 覆盖后，仓库里的知识页不再可见。
        assert!(matches!(
            read_resource("shangyi://kb/adverse-selection", Some(&home)),
            Err(ResourceReadError::NotFound(_))
        ));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn home_playbook_overrides_repo_playbook() {
        let home = temp_home("playbook");
        let dir = home.join("playbooks").join("pricing_response");
        std::fs::create_dir_all(&dir).expect("建剧本目录");
        std::fs::write(
            dir.join("playbook.yml"),
            "meta:\n  id: pricing_response\n  name: 本地调价剧本\n  description: 临时目录里的同名剧本\n",
        )
        .expect("写剧本");

        let resources = resources_of(&list_resources(Some(&home)));
        let pricing = resources
            .iter()
            .find(|r| r["uri"] == "shangyi://playbook/pricing_response")
            .expect("应有 pricing_response");
        assert_eq!(pricing["name"], "本地调价剧本");
        assert_eq!(pricing["description"], "临时目录里的同名剧本");

        let value = read_resource("shangyi://playbook/pricing_response", Some(&home))
            .expect("应读到 home 剧本");
        assert!(texts_of(&value)[0].contains("本地调价剧本"));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn page_meta_falls_back_to_heading_and_first_paragraph() {
        // 没有 frontmatter，也没有 `load_dir` 能用的 `id`：清单仍应给出人话。
        let (title, summary) = page_meta("# 标题一行\n\n正文第一段足以作为摘要。\n", "slug-x");
        assert_eq!(title, "标题一行");
        assert_eq!(summary, "正文第一段足以作为摘要。");

        let (title, summary) = page_meta("完全没有结构的纯文本\n", "slug-y");
        assert_eq!(title, "slug-y", "无标题时退回 slug");
        assert_eq!(summary, "完全没有结构的纯文本");

        // 带引号的 frontmatter 值要去引号；代码块不参与摘要。
        let (title, summary) = page_meta(
            "---\nname: '带引号的标题'\nsummary: \"摘要\"\n---\n\n```text\n不算摘要\n```\n\n# 标题\n",
            "slug-z",
        );
        assert_eq!(title, "带引号的标题");
        assert_eq!(summary, "摘要");
    }

    #[test]
    fn is_safe_segment_allows_digits_underscore_and_hyphen() {
        for ok in ["adverse-selection", "a_b", "x1", "0123", "-_-"] {
            assert!(is_safe_segment(ok), "{ok} 应被允许");
        }
        for bad in [
            "", "..", "a/b", "a b", "a.md", "中文", "a\\b", "a%2f", "a\n",
        ] {
            assert!(!is_safe_segment(bad), "{bad:?} 应被拒绝");
        }
        assert!(!is_safe_segment(&"a".repeat(129)), "超长段应被拒绝");
    }
}

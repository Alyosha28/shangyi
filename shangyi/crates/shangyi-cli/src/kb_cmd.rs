//! `shangyi kb` 子命令实现：检索、查看、列出与导入 Markdown 知识库条目。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use shangyi_kb::wiki::{self, Wiki, WikiPage};

use crate::run::{RunFailure, shangyi_home};

#[derive(Debug)]
pub struct LoadedKb {
    pub wiki: Wiki,
    pub dirs: Vec<PathBuf>,
    pub page_paths: BTreeMap<String, PathBuf>,
}

/// 定位知识库目录：
/// - 若显式设置 `SHANGYI_KB_DIR`，严格仅用该目录；
/// - 否则合并内置仓库 `shangyi/kb` 与用户目录 `$SHANGYI_HOME/kb`（后者可补充或覆盖同 id 条目）。
pub fn resolve_kb_dirs() -> (Vec<PathBuf>, bool) {
    if let Ok(d) = std::env::var("SHANGYI_KB_DIR") {
        let trimmed = d.trim();
        if !trimmed.is_empty() {
            return (vec![PathBuf::from(trimmed)], true);
        }
    }

    let mut dirs = Vec::new();
    let mut seen = BTreeSet::new();

    for cand in builtin_kb_candidates() {
        if cand.is_dir() {
            let key = std::fs::canonicalize(&cand).unwrap_or_else(|_| cand.clone());
            if seen.insert(key) {
                dirs.push(cand);
                break;
            }
        }
    }

    if let Ok(home) = shangyi_home() {
        let user_kb = home.join("kb");
        if user_kb.is_dir() {
            let key = std::fs::canonicalize(&user_kb).unwrap_or_else(|_| user_kb.clone());
            if seen.insert(key) {
                dirs.push(user_kb);
            }
        }
    }

    (dirs, false)
}

fn builtin_kb_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(repo) = std::env::var("SHANGYI_REPO_ROOT") {
        let r = repo.trim();
        if !r.is_empty() {
            out.push(PathBuf::from(r).join("shangyi/kb"));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        for anc in cwd.ancestors() {
            out.push(anc.join("shangyi/kb"));
            out.push(anc.join("kb"));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        for anc in exe.ancestors() {
            out.push(anc.join("shangyi/kb"));
            out.push(anc.join("kb"));
        }
    }
    out.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kb"));
    out
}

pub fn load_kb() -> Result<LoadedKb, RunFailure> {
    let (dirs, explicit) = resolve_kb_dirs();
    if dirs.is_empty() {
        let hint = if explicit {
            "SHANGYI_KB_DIR 指向的目录不存在".to_string()
        } else {
            "未找到内置 `shangyi/kb` 目录或 `$SHANGYI_HOME/kb`".to_string()
        };
        return Err(RunFailure::Session(format!("知识库不可用：{hint}")));
    }

    let mut by_id: BTreeMap<String, (WikiPage, PathBuf)> = BTreeMap::new();
    for dir in &dirs {
        let entries = collect_md_files(dir)?;
        for path in entries {
            let text = std::fs::read_to_string(&path).map_err(|e| RunFailure::CaseDir {
                path: path.display().to_string(),
                detail: format!("读取知识库页面失败：{e}"),
            })?;
            let page = wiki::parse_page_md(&text).map_err(|e| RunFailure::CaseDir {
                path: path.display().to_string(),
                detail: format!("解析知识库页面失败：{e}"),
            })?;
            by_id.insert(page.id.clone(), (page, path));
        }
    }

    let mut pages = Vec::with_capacity(by_id.len());
    let mut page_paths = BTreeMap::new();
    for (id, (page, path)) in by_id {
        page_paths.insert(id, path);
        pages.push(page);
    }

    let built = Wiki::build(pages).map_err(|e| RunFailure::Session(format!("知识库索引构建失败：{e}")))?;
    Ok(LoadedKb {
        wiki: built,
        dirs,
        page_paths,
    })
}

pub fn load_combined_wiki_for_runtime() -> Option<Arc<Wiki>> {
    match load_kb() {
        Ok(loaded) => Some(Arc::new(loaded.wiki)),
        Err(err) => {
            eprintln!("商奕：知识库加载失败：{err}");
            eprintln!("       （本次咨询将以无知识库模式进行）");
            None
        }
    }
}

fn collect_md_files(dir: &Path) -> Result<Vec<PathBuf>, RunFailure> {
    let rd = std::fs::read_dir(dir).map_err(|e| RunFailure::CaseDir {
        path: dir.display().to_string(),
        detail: format!("读取知识库目录失败：{e}"),
    })?;
    let mut files = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| RunFailure::CaseDir {
            path: dir.display().to_string(),
            detail: format!("枚举知识库目录失败：{e}"),
        })?;
        let p = entry.path();
        if p.extension().and_then(|s| s.to_str()) != Some("md") {
            continue;
        }
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(name, "index.md" | "README.md") || name.starts_with("._") || name.starts_with('.') {
            continue;
        }
        files.push(p);
    }
    files.sort();
    Ok(files)
}

pub fn run_kb_list() -> Result<(), RunFailure> {
    let loaded = load_kb()?;
    println!("{}", loaded.wiki.render_index());
    println!(
        "\n共 {} 篇知识库词条（目录：{}）。\n提示：`shangyi kb show <词条id>` 查看全文，`shangyi kb search <关键词>` 检索。",
        loaded.wiki.len(),
        loaded
            .dirs
            .iter()
            .map(|d| d.display().to_string())
            .collect::<Vec<_>>()
            .join(" , ")
    );
    Ok(())
}

pub fn run_kb_search(query: &str) -> Result<(), RunFailure> {
    let loaded = load_kb()?;
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(RunFailure::Session("知识库检索关键词不能为空".into()));
    }
    let keywords: Vec<String> = trimmed.split_whitespace().map(str::to_string).collect();
    let hits = loaded.wiki.search(&keywords);

    if hits.is_empty() {
        println!(
            "未命中包含「{trimmed}」的知识库词条（当前共加载 {} 篇）。",
            loaded.wiki.len()
        );
        println!("提示：可用 `shangyi kb list` 查看全部词条，或换用更简短的关键词再试。");
        return Ok(());
    }

    println!(
        "商奕知识库检索「{trimmed}」：命中 {} / {} 篇\n",
        hits.len(),
        loaded.wiki.len()
    );
    for (idx, (page, score)) in hits.iter().enumerate() {
        let tags = if page.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", page.tags.join(", "))
        };
        let path_note = loaded
            .page_paths
            .get(&page.id)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        println!(
            "{}. {} — {}（相关度 {}{}）",
            idx + 1,
            page.id,
            page.title,
            score,
            tags
        );
        if !page.summary.is_empty() {
            println!("   摘要：{}", page.summary);
        }
        if !path_note.is_empty() {
            println!("   文件：{path_note}");
        }
    }
    println!("\n查看全文：shangyi kb show <词条id>");
    Ok(())
}

pub fn run_kb_show(id_or_slug: &str) -> Result<(), RunFailure> {
    let loaded = load_kb()?;
    let needle = resolve_page_id(&loaded.wiki, id_or_slug).ok_or_else(|| {
        RunFailure::Session(format!(
            "知识库中未找到词条 `{}`；请先运行 `shangyi kb search <关键词>` 或 `shangyi kb list` 查找。",
            id_or_slug.trim()
        ))
    })?;

    let rendered = loaded.wiki.render_page(&needle).ok_or_else(|| {
        RunFailure::Session(format!("无法渲染词条 `{needle}`"))
    })?;
    println!("{rendered}");
    if let Some(path) = loaded.page_paths.get(&needle) {
        println!("\n---\n源文件：{}", path.display());
    }
    Ok(())
}

fn resolve_page_id(wiki: &Wiki, raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches(".md");
    if trimmed.is_empty() {
        return None;
    }
    if wiki.get(trimmed).is_some() {
        return Some(trimmed.to_string());
    }
    let hyphen = trimmed.replace('_', "-");
    if wiki.get(&hyphen).is_some() {
        return Some(hyphen);
    }
    let under = trimmed.replace('-', "_");
    if wiki.get(&under).is_some() {
        return Some(under);
    }
    let low = trimmed.to_lowercase();
    for id in wiki.ids() {
        if id.to_lowercase() == low {
            return Some(id.to_string());
        }
        if let Some(p) = wiki.get(id) {
            if p.title.to_lowercase() == low {
                return Some(id.to_string());
            }
        }
    }
    None
}

pub fn run_kb_add(path: &str) -> Result<(), RunFailure> {
    let src = Path::new(path.trim());
    if !src.is_file() {
        return Err(RunFailure::CaseDir {
            path: src.display().to_string(),
            detail: "知识库导入源路径不存在或不是普通文件".into(),
        });
    }
    let fname = src.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if fname.starts_with("._") || src.extension().and_then(|s| s.to_str()) != Some("md") {
        return Err(RunFailure::CaseDir {
            path: src.display().to_string(),
            detail: "知识库仅支持导入 UTF-8 Markdown 文件（.md）".into(),
        });
    }

    let content = std::fs::read_to_string(src).map_err(|e| RunFailure::CaseDir {
        path: src.display().to_string(),
        detail: format!("读取源文件失败：{e}"),
    })?;
    let parsed = wiki::parse_page_md(&content).map_err(|e| RunFailure::CaseDir {
        path: src.display().to_string(),
        detail: format!("知识页格式未通过校验：{e}"),
    })?;

    if parsed.id.contains('/') || parsed.id.contains('\\') || parsed.id.contains("..") || parsed.id.contains('\0') {
        return Err(RunFailure::Session(format!(
            "知识页 id `{}` 包含非法路径字符，已拒绝导入",
            parsed.id
        )));
    }

    let dest_dir = match std::env::var("SHANGYI_KB_DIR") {
        Ok(d) if !d.trim().is_empty() => PathBuf::from(d.trim()),
        _ => shangyi_home()?.join("kb"),
    };
    std::fs::create_dir_all(&dest_dir).map_err(|e| RunFailure::CaseDir {
        path: dest_dir.display().to_string(),
        detail: format!("创建知识库目录失败：{e}"),
    })?;

    let dest = dest_dir.join(format!("{}.md", parsed.id));
    let existed = dest.exists();
    let prev_content = if existed {
        Some(std::fs::read_to_string(&dest).ok())
    } else {
        None
    };

    std::fs::write(&dest, &content).map_err(|e| RunFailure::CaseDir {
        path: dest.display().to_string(),
        detail: format!("写入知识库文件失败：{e}"),
    })?;

    if let Err(err) = load_kb() {
        // 校验失败时回滚，不留半截损坏文件破坏全库加载
        match prev_content {
            Some(Some(old)) => {
                let _ = std::fs::write(&dest, old);
            }
            _ => {
                let _ = std::fs::remove_file(&dest);
            }
        }
        return Err(err);
    }

    println!(
        "✓ 已{}知识库词条：`{}`（{}）→ {}",
        if existed { "更新" } else { "导入" },
        parsed.id,
        parsed.title,
        dest.display()
    );
    Ok(())
}

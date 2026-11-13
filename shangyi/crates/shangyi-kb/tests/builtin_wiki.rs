//! 内置知识库（`shangyi/kb/`）的加载与**完整性**测试。
//!
//! # 为什么这些测试是承重的
//!
//! wiki 的价值全在**链接图**。而图的两类错误都是静默的：
//!
//! - **断链**：读者以为「这条理论没有后续」，实际是有人拼错了 id
//! - **孤儿页**：那一页存在于库里，但**任何导航都到不了它**
//!
//! 两者都不会让程序崩溃，只会让知识悄悄缺失。故必须由测试守住。

use std::path::PathBuf;

use shangyi_kb::wiki::{PageKind, WikiIssue, load_dir};

fn kb_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../kb")
}

#[test]
fn builtin_kb_loads() {
    let w = load_dir(&kb_dir()).expect("内置知识库应能加载");
    assert!(w.len() >= 10, "内置库不应只有零星几页，实得 {}", w.len());
    let ids = w.ids();
    let kinds: std::collections::BTreeSet<PageKind> = ids
        .iter()
        .filter_map(|id| w.get(id).map(|p| p.kind))
        .collect();
    assert!(kinds.contains(&PageKind::Theory), "应有理论页");
    assert!(kinds.contains(&PageKind::Method), "应有方法论页");
    assert!(kinds.contains(&PageKind::Concept), "应有概念页");
}

/// **零断链**。断链让「知识缺口」与「笔误」无法区分。
#[test]
fn no_broken_links() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    let broken: Vec<String> = w
        .issues()
        .iter()
        .filter_map(|i| match i {
            WikiIssue::BrokenLink { from, target, line } => {
                Some(format!("{from}:{line} → [[{target}]]"))
            }
            _ => None,
        })
        .collect();
    assert!(
        broken.is_empty(),
        "知识库有 {} 处断链（要么是漏写的页，要么是拼错的 id）：\n  {}",
        broken.len(),
        broken.join("\n  ")
    );
}

/// **零孤儿页**。孤儿页在库里存在但导航到不了——等于不存在。
#[test]
fn no_orphan_pages() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    let orphans: Vec<&str> = w
        .issues()
        .iter()
        .filter_map(|i| match i {
            WikiIssue::Orphan { id } => Some(id.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "有 {} 页没有任何入链（导航到不了）：{:?}\n\
         修法：从相关页加一条带理由的链接（[[id|理由]]）指向它。",
        orphans.len(),
        orphans
    );
}

/// 每页都必须有摘要——索引页靠它定位，空摘要的条目在索引里没有信息。
#[test]
fn every_page_has_a_summary() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    for id in w.ids() {
        let p = w.get(id).unwrap();
        assert!(
            !p.summary.trim().is_empty(),
            "页 `{id}` 缺摘要。在 frontmatter 加 summary，或确保正文首句非空。"
        );
        assert!(
            p.summary.chars().count() <= 160,
            "页 `{id}` 的摘要过长（{} 字）——索引页会臃肿：{}",
            p.summary.chars().count(),
            p.summary
        );
    }
}

/// 理论页必须有四要素中的**失效条件**。
///
/// `fails_when` 是「何时**不该**用这条理论」。缺了它，理论会被套用到
/// 不适用的场景上，而读者无从察觉。
#[test]
fn theory_pages_declare_failure_conditions() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    for id in w.ids() {
        let p = w.get(id).unwrap();
        if p.kind != PageKind::Theory {
            continue;
        }
        assert!(
            !p.extra_link_text.is_empty(),
            "理论页 `{id}` 没有任何四要素（applicable_when/fails_when/data_needs/outputs）——\
             它是可引用的理论，必须声明适用与失效边界"
        );
    }
}

/// 图必须**连通**：从任一页出发都能走到其它页。
///
/// 分成两簇的库在导航上等于两个库——模型从一簇出发会看不到另一簇。
#[test]
fn graph_is_connected() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    let ids: Vec<&str> = w.ids().into_iter().collect();
    let Some(&start) = ids.first() else { return };
    let mut seen: std::collections::BTreeSet<String> = Default::default();
    seen.insert(start.to_string());
    let mut queue = std::collections::VecDeque::from([start.to_string()]);
    while let Some(cur) = queue.pop_front() {
        for l in w.outbound(&cur) {
            if seen.insert(l.target.clone()) {
                queue.push_back(l.target.clone());
            }
        }
        for src in w.inbound(&cur) {
            if seen.insert(src.clone()) {
                queue.push_back(src.clone());
            }
        }
    }
    let unreachable: Vec<&&str> = ids.iter().filter(|id| !seen.contains(**id)).collect();
    assert!(
        unreachable.is_empty(),
        "有 {} 页与主图不连通（导航到不了）：{:?}",
        unreachable.len(),
        unreachable
    );
}

/// 索引页必须可渲染且含用法说明（模型靠它知道下一步）。
#[test]
fn index_renders_with_usage_hint() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    let idx = w.render_index();
    assert!(idx.contains("知识库索引"), "{}", &idx[..idx.len().min(200)]);
    assert!(idx.contains("kb_retrieve"), "索引必须告诉读者怎么取正文");
    for id in w.ids() {
        assert!(idx.contains(id), "索引里缺页 `{id}`");
    }
}

/// 每页正文都必须非空——空页在导航里是个死胡同。
#[test]
fn every_page_has_body() {
    let w = load_dir(&kb_dir()).expect("应能加载");
    for id in w.ids() {
        let p = w.get(id).unwrap();
        assert!(
            p.body.trim().chars().count() >= 50,
            "页 `{id}` 正文过短（{} 字）——导航到它会得到空信息",
            p.body.trim().chars().count()
        );
    }
}

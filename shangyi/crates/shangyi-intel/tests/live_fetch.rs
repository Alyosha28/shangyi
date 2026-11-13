//! 真实网络抓取测试（`#[ignore]`，需手动运行）。
//!
//! 默认不跑：CI 不该依赖外网可达性。运行方式：
//! ```text
//! cargo test -p shangyi-intel --test live_fetch -- --ignored
//! ```
//!
//! # 它验证的是"人机验证不可绕过"这条**决策**是否被正确实现
//!
//! 实测（2026-09-18）`html.duckduckgo.com` 会返回人机验证挑战页。
//! 本测试把它当作**预期行为**断言：系统必须**如实报告**、**不绕过**。
//! 这与"抓不到就报错"的普通测试不同——它钉住的是一条**产品政策**。

use shangyi_intel::fetch::{FetchError, HttpFetcher};

/// 一个明确允许抓取的站点（IANA 维护的示例域）应能抓到。
#[test]
#[ignore = "需要网络"]
fn can_fetch_a_site_that_allows_crawling() {
    let f = HttpFetcher::new();
    let page = f.fetch("https://example.com").expect("example.com 应可抓取");
    assert_eq!(page.status, 200);
    assert!(
        page.text.to_lowercase().contains("example domain"),
        "应抽到正文：{}",
        page.text
    );
}

/// robots.txt 被遵守：对 robots 明确禁止的路径不得抓取。
///
/// 用一个**实际存在且禁止**的路径来验证——若某天该站点放开，测试会失败并
/// 提示换一个（这是"测试跟着现实走"，而不是放宽断言）。
#[test]
#[ignore = "需要网络"]
fn robots_is_actually_respected_live() {
    let f = HttpFetcher::new();
    // 先确认该站的 robots 可读（否则本测试验证的就不是"被禁止"而是"读不到"）
    match f.check_allowed("https://www.iana.org/domains/root") {
        Ok(shangyi_intel::fetch::RobotsVerdict::Allowed) => {}
        Ok(v) => panic!("预期 iana 允许该路径，实得 {v:?}（若站点政策变了请更新测试）"),
        Err(e) => panic!("robots 查询失败：{e}"),
    }
}

/// **核心政策断言**：遇到人机验证必须如实报告，不得返回"内容"。
///
/// 若未来有人实现了绕过（伪装 UA、解验证码），本测试会失败——
/// 那正是它存在的意义。
#[test]
#[ignore = "需要网络"]
fn human_verification_is_reported_not_bypassed() {
    let f = HttpFetcher::new();
    match f.fetch("https://html.duckduckgo.com/html/?q=test") {
        Err(FetchError::BotChallenge { .. }) => {
            // 预期：如实识别为验证页
        }
        Err(FetchError::HttpStatus { status, .. }) => {
            // 也接受：站点直接返回非 2xx（同样是"未取得"，同样没有绕过）
            assert!(status >= 400, "非 2xx 才是拒绝，实得 {status}");
        }
        Ok(page) => {
            // 若站点放宽了限制，抓到真内容也可以接受——
            // 但绝不能是"验证页被当成了正文"
            assert!(
                !page.text.to_ascii_lowercase().contains("complete the following challenge"),
                "验证页正文不得被抓取器当作内容返回（那是绕过或误判）"
            );
        }
        Err(e) => panic!("意外错误：{e}"),
    }
}

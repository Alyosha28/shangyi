//! 检索后端的**真实网络**测试（`#[ignore]`）。
//!
//! 运行：`cargo test -p shangyi-cli --test search_live -- --ignored --nocapture`
//! （凭据取自 `BOCHA_API_KEY` 或 `~/.shangyi-secrets/bocha.env`；**按次计费**）
//!
//! # 为什么需要它
//!
//! `web_search` 的价值全在"真能取回东西"。而它此前一直报「未取得」
//! （未配后端），所以**从未有人验证过接上后端之后能不能用**。
//! 这个测试补上那一步：起真后端、发真查询、看真结果。
//!
//! # 正式基准（会花钱，务必先读方案）
//!
//! `frozen_official_coverage_benchmark` 重跑 2026-10-02 冻结的 4 条查询。
//! 它与旧成绩的**协议差异必须明说**：本次走的是融合管线（多后端按名次融合
//! 并重排），而**当前只装配一个后端**（商业检索 API）。因此它是一轮新基准，
//! 不是对旧成绩的改写；判定口径沿用旧方案：
//! 4 条都健康 + 至少 3 条的前 8 含 `shanghai.gov.cn`。
//!
//! 每条查询会**产生一次按次计费的博查调用**（4 条 = 4 次）。运行前先确认
//! 这是有意为之，并把输出留档到 `research/qa/`。

use shangyi_cli::search_provider_from_env;
use shangyi_core::domain_exec::{SearchProvider, SearchUnavailable};

/// 冻结基准复测（2026-10-02 的 4 条查询，见 `bocha-benchmark-plan.md`）。
///
/// 判定沿用旧口径，但协议换成新融合管线（两种后端都查），故结果记为新一轮。
/// 输出为逐条 JSON 行，便于留档到 `research/qa/`。
#[test]
#[ignore = "需要真实检索后端（博查按次计费，共 4 次调用）与网络"]
fn frozen_official_coverage_benchmark() {
    const QUERIES: [(&str, &str); 4] = [
        ("B1", "上海国际咖啡文化节 2025 举办时间 地点 官方"),
        (
            "B2",
            "2025年上海国际咖啡文化节 北外滩主市集 活动日期 官方",
        ),
        ("B3", "北外滩主市集 2025 咖啡文化节 商家参与 活动安排"),
        ("B4", "上海 2025 咖啡文化节 主办单位 市集组织单位 政府"),
    ];
    let provider = search_provider_from_env();
    let mut bocha_healthy = 0;
    let mut coverage = 0;
    for (id, query) in QUERIES {
        let (hits, health) = match provider.search_with_health(query) {
            Ok(result) => result,
            Err(SearchUnavailable::NoBackend { detail }) => {
                panic!("未配置任何检索后端：{detail}")
            }
            Err(SearchUnavailable::Failed { detail, .. }) => {
                println!("{{\"id\":\"{id}\",\"error\":{detail:?}}}");
                continue;
            }
        };
        // 健康判定沿用旧口径：**博查**正常返回即算健康；其他后端若失败只记为
        // 降级覆盖，不判本轮失败（历史上 SearXNG 的部分引擎限流/CAPTCHA 就是
        // 这样处理的；现只装配博查，该分支留作多后端时的口径）。
        let bocha_ok = health.backends.iter().any(|backend| {
            backend.backend == "bocha" && backend.responded && backend.failure.is_none()
        });
        if bocha_ok {
            bocha_healthy += 1;
        }
        let strict_rank = hits
            .iter()
            .position(|hit| hit.url.contains("shanghai.gov.cn"))
            .map(|index| index + 1);
        if strict_rank.is_some() {
            coverage += 1;
        }
        let record = serde_json::json!({
            "id": id,
            "query": query,
            "bocha_healthy": bocha_ok,
            "availability": health.availability,
            "backends": health.backends,
            "strict_shanghai_gov_rank": strict_rank,
            "ranking": health.ranking,
            "hits": hits.iter().enumerate().map(|(index, hit)| serde_json::json!({
                "rank": index + 1,
                "score": hit.rank_score,
                "sources": hit.backend,
                "signals": hit.rank_signals,
                "title": hit.title,
                "url": hit.url,
            })).collect::<Vec<_>>(),
        });
        println!("{record}");
    }
    assert_eq!(
        bocha_healthy, 4,
        "4 条查询都应得到博查的健康返回；实得 {bocha_healthy}/4"
    );
    assert!(
        coverage >= 3,
        "官方原文覆盖未达标：{coverage}/4（冻结门槛 3/4）"
    );
}

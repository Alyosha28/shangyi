//! 检索结果的确定性重排层：查询意图解析 → 去重 → 意图加权。
//!
//! # 为什么需要这一层（有实测基线，不是预防性设计）
//!
//! 2026-10-02 的冻结基准（`research/qa/2026-10-02-guided-session/bocha-benchmark-results.json`）：
//! 4 条真实查询经博查 **4/4 健康返回**，但上海市政府原文只在前 8 条中出现 **2/4**，
//! 低于预设的 3/4 门槛，正式测试记为失败。失败形态是可复现的：
//!
//! | 现象 | 证据 |
//! |---|---|
//! | 同一篇通稿被同一发布方的多个域名占位 | B2 的 `new.qq.com`/`news.qq.com` 占第 2、3、8 名 |
//! | 同一页面的桌面版/移动版各占一条 | B1 的 `sh.bendibao.com`/`m.sh.bendibao.com` 占第 3、5 名 |
//! | 官方原文被挤到第 6 名或完全落榜 | B1、B2 在第 6 名；B3、B4 前 8 条没有 |
//!
//! 上表**前两行是单一后端内部的问题**，靠去重与同站限位就能修——那正是本层保留的部分。
//! 第三行靠意图加权把官方页提上来。而 2026-10-02 那次基准里"B3 官方页完全落榜"
//! 一条，当轮是靠第二个后端的候选补进来的；自 2026-10 起**只装配一个后端**
//! （商业检索 API），故 B3 类查询仍会退回"前 8 条没有官方页"——这是候选面的
//! 限制，不是重排层能补的（[`fuse`] 只融合已返回的候选）。
//!
//! # 本层做什么（全部是本地确定性计算，无网络、无模型）
//!
//! 1. **查询意图解析**：抽出年份、时效词、官方意图、`site:`/`-site:` 过滤；
//!    这些既用于后端请求参数（`freshness`/`include`/`exclude`），也用于重排。
//! 2. **同文档合并**：先按规范化 URL，再按规范化标题。合并只影响"排在一起"，
//!    不丢弃审计信息。
//! 3. **意图加权**：查询词覆盖度、官方域名（仅当查询表达了官方意图）、
//!    年份相符/不符（2025 的查询不该被 2026 的页面顶掉）。
//! 4. **同站限位**：同一站点最多先放 2 条；没人竞争时被降权的仍会补回来
//!    （`site_cap_demoted` 记录数量），因此不是静默丢弃。
//!
//! # 边界
//!
//! 评分只表示**检索排序**，不表示事实真伪或来源权威性。任何一条结果仍须
//! 经 `web_fetch` 读原文核对——摘要与供应商日期元数据都不能当事实用。
//! 所有加权规则都是公开常量，可被单元测试逐条钉死。

use crate::fetch::SearchResult;

/// 排序方法标识。它进入 [`crate::fetch::RankingReport`]，使报告能回答
/// "这条排序是哪一版规则产生的"。
pub const RANKING_METHOD: &str = "intent-rerank-v1";

/// 名次先验的平滑常数。名次 1 与名次 2 的差距约为 `1/(k+1) - 1/(k+2)`；
/// k 取 10 是因为候选集很小（≤ 20 条），取文献常用的 60 会把头部名次
/// 差异压得几乎不可见。
const RANK_PRIOR_SMOOTHING: f64 = 10.0;

/// 同一站点在竞争情况下最多先占的条数。取 2 而不是 1：同一站点可能确有
/// 多条不同页面（例如政府站的"通知"与"问答"），全压成 1 条会损失召回。
const SITE_CAP: usize = 2;

/// 标题近重复的降权系数（第二条及其后的同一标题簇）。
const NEAR_DUPLICATE_FACTOR: f64 = 0.55;

// ── 评分权重（全部集中在此，便于审计与测试）─────────────────────────────
//
// 权重取值的依据（2026-10-02 冻结基线与免费干跑各暴露了一半）：
//   - 只按供应商名次排 → 官方原文被挤到第 6（基线失败形态）；
//   - 名次权重过大 → 偶然命中的无关政府页会被"官方域名"加分推到第一。
// 故：名次是较强的先验（0.45），但查询词覆盖度必须能推翻它（0.30）；
// 官方与年份只在**主题相关**的候选中起作用（按覆盖度缩放，见 `relevance`）。
const W_RANK_PRIOR: f64 = 0.45;
const W_TITLE_COVERAGE: f64 = 0.30;
const W_SNIPPET_COVERAGE: f64 = 0.10;
const BONUS_OFFICIAL_DOMAIN: f64 = 0.30;
const BONUS_YEAR_MATCH: f64 = 0.10;
const PENALTY_YEAR_MISMATCH: f64 = 0.15;
/// 主题相关性达到该标题覆盖度时，意图加分按满额计。
const RELEVANCE_FULL_COVERAGE: f64 = 0.4;
/// 显示分的上界（各分项权重之和），使 0–100 保留完整排序分辨率。
const MAX_SCORE: f64 =
    W_RANK_PRIOR + W_TITLE_COVERAGE + W_SNIPPET_COVERAGE + BONUS_OFFICIAL_DOMAIN + BONUS_YEAR_MATCH;

/// 查询的时效意图。`None` = 没有时效词，后端不加时间限制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Recency {
    #[default]
    None,
    Day,
    Week,
    Month,
    Year,
}

impl Recency {
    /// 博查 `freshness` 取值（官方文档：oneDay/oneWeek/oneMonth/oneYear/noLimit）。
    pub fn bocha_freshness(self) -> &'static str {
        match self {
            Recency::None => "noLimit",
            Recency::Day => "oneDay",
            Recency::Week => "oneWeek",
            Recency::Month => "oneMonth",
            Recency::Year => "oneYear",
        }
    }
}

/// 一条查询的结构化意图。后端参数与重排共用同一份解析结果——
/// 两处若各自解析，迟早会对同一个查询给出不同的理解。
#[derive(Debug, Clone, PartialEq)]
pub struct QueryPlan {
    /// 规范化后的原查询（保留 `site:` 运算符）。
    pub raw: String,
    /// 去掉 `site:`/`-site:` 过滤词后的正文查询，用于把过滤作为独立参数的
    /// 后端（博查的 `include`/`exclude`）。过滤词提取失败时退回 `raw`。
    pub keywords: String,
    /// 重排用的查询词（已去停用词与纯意图词）。
    pub terms: Vec<String>,
    /// 查询中出现的年份（如 2025），按出现顺序去重。
    pub years: Vec<u32>,
    pub recency: Recency,
    /// 查询是否明确要求官方/权威来源。只有它为真时才会加官方域名分。
    pub official_intent: bool,
    pub site_includes: Vec<String>,
    pub site_excludes: Vec<String>,
}

impl QueryPlan {
    /// 博查 `include` 参数值（用 `|` 连接）。空 = 不加该参数。
    pub fn bocha_include(&self) -> Option<String> {
        (!self.site_includes.is_empty()).then(|| self.site_includes.join("|"))
    }

    /// 博查 `exclude` 参数值。
    pub fn bocha_exclude(&self) -> Option<String> {
        (!self.site_excludes.is_empty()).then(|| self.site_excludes.join("|"))
    }
}

/// 官方意图词。命中任一即认为查询在要权威来源。
/// 这是**召回增强**（只加分），不是过滤（不删除任何结果）。
const OFFICIAL_INTENT_WORDS: &[&str] = &[
    "官方", "政府", "政策", "公告", "通知", "印发", "原文", "官网", "权威", "监管", "备案",
    "许可", "主办", "主管", "采购", "文件", "办法", "条例",
];

/// 时效词（按窗口从窄到宽判定，先命中者胜）。
const RECENCY_DAY: &[&str] = &["今天", "今日", "当天", "刚刚", "最新消息"];
const RECENCY_WEEK: &[&str] = &["本周", "这周", "近一周", "最近一周", "一周内", "上周"];
const RECENCY_MONTH: &[&str] = &["本月", "这个月", "近一个月", "最近一个月", "近期", "最新"];
const RECENCY_YEAR: &[&str] = &["今年", "本年度", "年内", "上半年", "下半年"];

/// 不参与覆盖度打分的词：无区分度，或会奖励"自称官方"的门户页
/// （官方与否由**域名**判断，不由页面文字自称判断）。
const STOPWORDS: &[&str] = &[
    "的", "了", "是", "在", "和", "与", "及", "或", "吗", "呢", "吧", "怎么", "如何", "哪些",
    "什么", "多少", "官方", "政府", "权威", "官网", "原文",
];

/// 跟踪参数：只影响"是不是同一条链接"的判定，不携带内容语义。
const TRACKING_PARAMS: &[&str] = &[
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "gclid",
    "fbclid",
    "spm",
    "_f",
    "f",
    "from",
    "share_token",
    "share_source",
    "ref_src",
];

/// 常见中文门户/聚合站在标题末尾追加的站点名。仅用于**近重复判定**，
/// 不影响任何一条结果的展示标题。
const TITLE_SITE_SUFFIXES: &[&str] = &[
    "腾讯新闻", "腾讯网", "网易订阅", "网易新闻", "新浪新闻", "搜狐新闻", "今日头条",
    "看看新闻网", "百家号", "澎湃新闻", "界面新闻", "上观新闻", "东方网", "人民网", "新华网",
    "央广网", "中国新闻网", "上海本地宝", "本地宝", "百度百科", "微信公众平台", "知乎",
    "小红书", "哔哩哔哩", "观察者网", "第一财经", "财新网", "凤凰网", "中国政府网",
    "上海市人民政府",
];

/// 公共后缀（两段式）。用于把 `www.shanghai.gov.cn` 归到 `shanghai.gov.cn`，
/// 而不是误把 `shanghai.gov.cn` 与 `shhk.gov.cn` 当成同一个站。
const TWO_PART_SUFFIXES: &[&str] = &[
    "gov.cn", "com.cn", "org.cn", "net.cn", "edu.cn", "ac.cn", "mil.cn", "gov.hk", "com.hk",
    "org.hk", "edu.hk", "gov.mo", "com.tw", "org.tw", "co.jp", "co.uk", "org.uk", "com.au",
    "co.nz", "com.sg", "com.my",
];

/// 解析一条查询。纯函数：同一输入永远得到同一结果。
pub fn plan_query(query: &str) -> QueryPlan {
    let raw = normalize_whitespace(query);
    let (keywords, mut site_includes, mut site_excludes) = extract_site_filters(&raw);
    let keywords = if keywords.trim().is_empty() {
        // 形如 `site:example.com` 的纯过滤查询：没有正文关键词时，过滤参数
        // 会变成"用空查询搜整个域名"，两种后端都不接受。退回原文整体处理。
        site_includes.clear();
        site_excludes.clear();
        raw.clone()
    } else {
        keywords
    };
    let years = extract_years(&raw);
    let recency = detect_recency(&raw);
    let official_intent = OFFICIAL_INTENT_WORDS.iter().any(|word| raw.contains(word));
    let terms = extract_terms(&keywords);
    QueryPlan {
        raw,
        keywords,
        terms,
        years,
        recency,
        official_intent,
        site_includes,
        site_excludes,
    }
}

fn normalize_whitespace(query: &str) -> String {
    query
        .replace(['\u{3000}', '\u{200b}', '\u{feff}'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '。' | '，' | '、' | '；' | ';' | ','))
        .to_string()
}

fn normalize_punct(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '。' | '，' | '、' | '；' | '：' | '？' | '！' | '“' | '”' | '‘' | '’' | '（'
                | '）' | '《' | '》' | '【' | '】' | '—' | '–' | '·' | '…' | '"' | '\'' | '|'
                | '/' | '\\' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | '.' | ';' | ':' | '?'
                | '!' | '#' | '~' | '`'
        )
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{9fff}' | '\u{f900}'..='\u{faff}')
}

/// 域名合法性（与博查 `include` 的要求一致：至少两段、标签合法）。
pub fn valid_domain(domain: &str) -> bool {
    if domain.is_empty() || domain.len() > 253 || !domain.contains('.') {
        return false;
    }
    domain.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && label
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            && label
                .chars()
                .last()
                .is_some_and(|c| c.is_ascii_alphanumeric())
    })
}

/// 把 `site:d` / `-site:d` 词提取成过滤列表；非法或孤立的过滤词保留在正文里
/// （宁可让后端按普通关键词处理，也不静默丢掉用户写的条件）。
fn extract_site_filters(query: &str) -> (String, Vec<String>, Vec<String>) {
    let mut terms: Vec<&str> = Vec::new();
    let mut includes = Vec::new();
    let mut excludes = Vec::new();
    for token in query.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| {
            matches!(c, ';' | ',' | '，' | '；' | '。' | '、')
        });
        let (excluded, body) = if let Some(rest) = trimmed
            .strip_prefix("-site:")
            .or_else(|| trimmed.strip_prefix("-SITE:"))
        {
            (true, rest)
        } else if let Some(rest) = trimmed
            .strip_prefix("site:")
            .or_else(|| trimmed.strip_prefix("SITE:"))
        {
            (false, rest)
        } else {
            terms.push(token);
            continue;
        };
        let domain = body.to_ascii_lowercase();
        if valid_domain(&domain) {
            let bucket = if excluded { &mut excludes } else { &mut includes };
            if bucket.len() < 20 && !bucket.contains(&domain) {
                bucket.push(domain);
            }
        } else {
            terms.push(token);
        }
    }
    for bucket in [&mut includes, &mut excludes] {
        bucket.sort();
    }
    (terms.join(" "), includes, excludes)
}

/// 抽出查询中的年份（19xx/20xx 连续四位）。只认独立数字串，
/// `12025` 这类不是年份。
fn extract_years(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut years = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        let len = i - start;
        if len == 4 {
            let value = text[start..i].parse::<u32>().unwrap_or(0);
            if (1900..=2099).contains(&value) && !years.contains(&value) {
                years.push(value);
            }
        }
    }
    years
}

fn detect_recency(text: &str) -> Recency {
    if RECENCY_DAY.iter().any(|w| text.contains(w)) {
        Recency::Day
    } else if RECENCY_WEEK.iter().any(|w| text.contains(w)) {
        Recency::Week
    } else if RECENCY_MONTH.iter().any(|w| text.contains(w)) {
        Recency::Month
    } else if RECENCY_YEAR.iter().any(|w| text.contains(w)) {
        Recency::Year
    } else {
        Recency::None
    }
}

fn extract_terms(keywords: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for token in keywords.split_whitespace() {
        let cleaned: String = token
            .chars()
            .filter(|c| !normalize_punct(*c))
            .collect::<String>()
            .trim()
            .to_string();
        if cleaned.chars().count() < 2 {
            continue;
        }
        if STOPWORDS.contains(&cleaned.as_str()) {
            continue;
        }
        if !terms.iter().any(|existing| existing == &cleaned) {
            terms.push(cleaned);
        }
    }
    terms
}

// ─────────────────────────── URL 规范化 ───────────────────────────

/// 取主机名（小写、去端口、去 userinfo）。非 http(s) 返回 `None`。
pub fn host_of(url: &str) -> Option<String> {
    let rest = strip_http_scheme(url)?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
        .map(|(host, _)| host)
        .unwrap_or(authority)
        .trim_end_matches('.')
        .to_ascii_lowercase();
    (!host.is_empty()).then_some(host)
}

fn strip_http_scheme(url: &str) -> Option<&str> {
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let (scheme, rest) = url.split_once("://")?;
    if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") {
        Some(rest)
    } else {
        None
    }
}

/// 文档身份键：同一文档的不同跟踪参数/片段/http(s) 形式应归为一条。
///
/// 只做**保守**归一：去掉片段、去掉跟踪参数、路径尾斜杠、参数排序；
/// **不**把 `m.` 子域并到主域（移动版可能是不同页面，合并靠标题簇）。
pub fn canonical_url_key(url: &str) -> Option<String> {
    let rest = strip_http_scheme(url)?;
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let host = host_of(url)?;
    let after_authority = &rest[authority_end..];
    let (path, query) = match after_authority.split_once('?') {
        Some((path, query)) => (path, query.split('#').next().unwrap_or("")),
        None => (after_authority.split('#').next().unwrap_or(""), ""),
    };
    let mut path = path.to_string();
    while path.ends_with('/') && path.len() > 1 {
        path.pop();
    }
    let mut params: Vec<(String, String)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            if TRACKING_PARAMS.contains(&key.to_ascii_lowercase().as_str()) {
                None
            } else {
                Some((key.to_string(), value.to_string()))
            }
        })
        .collect();
    params.sort();
    let query = params
        .iter()
        .map(|(k, v)| if v.is_empty() { k.clone() } else { format!("{k}={v}") })
        .collect::<Vec<_>>()
        .join("&");
    Some(if query.is_empty() {
        format!("{host}{path}")
    } else {
        format!("{host}{path}?{query}")
    })
}

/// 标题簇键：去掉站点后缀、标点与空白后的小写文本。
/// 少于 6 个字符的标题不做近重复合并（太短，误合并风险高）。
pub fn title_key(title: &str) -> Option<String> {
    let mut text = title.trim().to_string();
    for separator in ['_', '|', '｜', '-', '–', '—'] {
        if let Some((head, tail)) = text.rsplit_once(separator) {
            let tail_trimmed = tail.trim().trim_matches(normalize_punct).trim();
            let head_trimmed = head.trim();
            if TITLE_SITE_SUFFIXES.contains(&tail_trimmed)
                && head_trimmed.chars().count() >= 6
            {
                text = head_trimmed.to_string();
                break;
            }
        }
    }
    let key: String = text
        .chars()
        .filter(|c| !normalize_punct(*c))
        .flat_map(|c| c.to_lowercase())
        .collect();
    (key.chars().count() >= 6).then_some(key)
}

// ─────────────────────────── 站点与官方判定 ───────────────────────────

/// 政府域名判定。用**域名后缀**，不用页面自称。
pub fn is_government_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "gov.cn"
        || host.ends_with(".gov.cn")
        || host == "gov.hk"
        || host.ends_with(".gov.hk")
        || host == "gov.mo"
        || host.ends_with(".gov.mo")
        || host == "gov"
        || host.ends_with(".gov")
}

/// 站点键（注册域近似）：用于同站限位。`gov.cn` 是两段式公共后缀，
/// 故 `www.shanghai.gov.cn` 与 `shanghai.gov.cn` 同站，而 `shhk.gov.cn` 是另一个站。
pub fn site_key(host: &str) -> String {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').filter(|l| !l.is_empty()).collect();
    if labels.len() <= 2 {
        return labels.join(".");
    }
    let last_two = labels[labels.len() - 2..].join(".");
    if TWO_PART_SUFFIXES.contains(&last_two.as_str()) && labels.len() >= 3 {
        labels[labels.len() - 3..].join(".")
    } else {
        last_two
    }
}

// ─────────────────────────── 去重与重排 ───────────────────────────

/// 重排后的一条结果。
#[derive(Debug, Clone, PartialEq)]
pub struct RankedHit {
    pub result: SearchResult,
    /// 0–100 的**检索排序分**（不是事实可信度）。
    pub score: u32,
    /// 命中的排序信号（中文短标签，进工具输出供审计）。
    pub signals: Vec<String>,
    /// 来源后端。融合后是**全部**来源，以 `+` 连接（如 `bocha+tavily`）：
    /// 一条文档被多个后端同时返回是共识证据，不合并成单值。
    pub origins: Vec<String>,
}

/// `SearchResult::backend` 用 `+` 连接多来源（见该字段文档），这里拆回列表。
fn split_origins(backend: &str) -> Vec<String> {
    backend
        .split('+')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// 一个后端返回的原始候选（保持供应商名次），融合的输入单位。
#[derive(Debug, Clone, PartialEq)]
pub struct BackendResults {
    pub backend: String,
    pub results: Vec<SearchResult>,
}

impl BackendResults {
    pub fn new(backend: impl Into<String>, results: Vec<SearchResult>) -> Self {
        Self {
            backend: backend.into(),
            results,
        }
    }
}

/// 重排统计，进 [`crate::fetch::RankingReport`]。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RankStats {
    pub candidates: usize,
    pub unique_documents: usize,
    pub duplicates_merged: usize,
    pub site_cap_demoted: usize,
    pub truncated: usize,
}

/// 按查询意图重排**单一后端**返回的候选。
///
/// 名次先验取 `1/(k+rank)`，与 [`fuse`] 的 RRF 同形——因此把多后端候选按
/// 名次合成一份后交给本函数，与单后端直接调用的**打分口径完全一致**：
/// 跨后端只融合名次（供应商分数不可比），重排逻辑始终只有这一份。
pub fn rank(
    results: &[SearchResult],
    plan: &QueryPlan,
    limit: usize,
) -> (Vec<RankedHit>, RankStats) {
    let limit = limit.max(1);
    #[derive(Debug)]
    struct Group {
        result: SearchResult,
        prior: f64,
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut index: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let mut candidates = 0usize;
    let mut duplicates_merged = 0usize;

    for (position, result) in results.iter().enumerate() {
        candidates += 1;
        let rank = position + 1;
        let url_key = canonical_url_key(&result.url).unwrap_or_else(|| format!("rank:{rank}"));
        let title_cluster = title_key(&result.title);
        // 先按规范化 URL 合并；URL 未见过时再看标题簇（同一篇通稿的
        // 多个转载页面）。两者都没命中就是新文档。
        let url_identity = format!("u:{url_key}");
        let title_identity = title_cluster.as_ref().map(|title| format!("t:{title}"));
        let key = if index.contains_key(&url_identity) {
            url_identity.clone()
        } else if let Some(identity) = &title_identity
            && index.contains_key(identity)
        {
            identity.clone()
        } else {
            url_identity.clone()
        };
        let prior = 1.0 / (RANK_PRIOR_SMOOTHING + rank as f64);
        if let Some(&existing) = index.get(&key) {
            let group = &mut groups[existing];
            duplicates_merged += 1;
            // 同一文档的后续副本**不累加名次先验**——它们不是独立证据，
            // 累加会等于把"同一篇通稿被多家转载"读成"更可信"。只补齐字段。
            if result.snippet.chars().count() > group.result.snippet.chars().count() {
                group.result.snippet = result.snippet.clone();
            }
            if group.result.published_at.is_none() {
                group.result.published_at = result.published_at.clone();
            }
            // 把刚见到的 URL/标题也挂到同一组，后续副本才能按任一线索命中。
            index.entry(url_identity).or_insert(existing);
            if let Some(identity) = title_identity {
                index.entry(identity).or_insert(existing);
            }
        } else {
            let slot = groups.len();
            index.insert(key.clone(), slot);
            if let Some(identity) = &title_identity {
                index.entry(identity.clone()).or_insert(slot);
            }
            groups.push(Group {
                result: result.clone(),
                prior,
            });
        }
    }

    let max_prior = 1.0 / (RANK_PRIOR_SMOOTHING + 1.0);

    // 第一遍：算基础分（不含近重复降权），并记下每条的标题簇。
    struct Scored {
        hit: RankedHit,
        score: f64,
        cluster: Option<String>,
    }
    let mut scored: Vec<Scored> = groups
        .into_iter()
        .map(|mut group| {
            let title_cluster = title_key(&group.result.title);
            let mut signals: Vec<String> = Vec::new();

            let title_cov = coverage(&plan.terms, &group.result.title);
            let snippet_cov = coverage(&plan.terms, &group.result.snippet);
            let host = host_of(&group.result.url).unwrap_or_default();
            let gov = is_government_host(&host);
            let year_state = year_state(plan, &group.result);

            let mut score = W_RANK_PRIOR * (group.prior / max_prior).clamp(0.0, 1.0)
                + W_TITLE_COVERAGE * title_cov
                + W_SNIPPET_COVERAGE * snippet_cov;

            // 意图加分只在**主题相关**的候选中生效：查询要"官方来源"，
            // 不等于任何政府页都算命中（免费干跑里，广东商务厅的无关通知
            // 曾因此被推到第一）。相关性只看**年份之外**的查询词——否则
            // 一个只匹配"2025"的无关页面会被当成相关。摘要覆盖度由供应商
            // 生成，噪声大，不参与这个缩放。
            let relevance = topical_relevance(plan, &group.result.title);
            if plan.official_intent && gov {
                score += BONUS_OFFICIAL_DOMAIN * relevance;
                if relevance > 0.0 {
                    signals.push("官方域名".to_string());
                }
            }
            match year_state {
                YearState::Match => {
                    score += BONUS_YEAR_MATCH * relevance;
                    if relevance > 0.0 {
                        signals.push("年份相符".to_string());
                    }
                }
                YearState::Mismatch => {
                    score -= PENALTY_YEAR_MISMATCH * relevance;
                    if relevance > 0.0 {
                        signals.push("年份不符".to_string());
                    }
                }
                YearState::Unknown => {}
            }

            Scored {
                hit: RankedHit {
                    origins: split_origins(&group.result.backend),
                    result: group.result,
                    score: 0, // 第二遍写入
                    signals,
                },
                score,
                cluster: title_cluster,
            }
        })
        .collect();

    // 第二遍：同一标题簇只保留**分数最高**的一条不降权，其余按系数降权——
    // 降权对象必须由分数决定，不能由插入顺序决定（那是供应商序，不是质量序）。
    let mut cluster_best: std::collections::BTreeMap<String, usize> = Default::default();
    for (slot, item) in scored.iter().enumerate() {
        if let Some(cluster) = &item.cluster {
            match cluster_best.get(cluster) {
                Some(&best) if scored[best].score >= item.score => {}
                _ => {
                    cluster_best.insert(cluster.clone(), slot);
                }
            }
        }
    }
    for (slot, item) in scored.iter_mut().enumerate() {
        if let Some(cluster) = &item.cluster
            && cluster_best.get(cluster).is_some_and(|best| *best != slot)
        {
            item.score *= NEAR_DUPLICATE_FACTOR;
            item.hit.signals.push("近重复标题".to_string());
        }
    }

    // 排序用**未取整**的原始分，先排序再映射到 0–100：若先取整再排，
    // 第 1 名与第 3 名可能同分，排序会退化成按 URL 字母序——那会把真实
    // 差异藏起来。
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.hit.result.url.cmp(&b.hit.result.url))
    });
    let hits: Vec<RankedHit> = scored
        .into_iter()
        .map(|mut item| {
            item.hit.score = ((item.score / MAX_SCORE) * 100.0).clamp(0.0, 100.0).round() as u32;
            item.hit
        })
        .collect();

    // 同站限位：先满足多样性；名额没用满时，被降权的按分数补回。
    let mut stats = RankStats {
        candidates,
        unique_documents: hits.len(),
        duplicates_merged,
        ..Default::default()
    };
    let mut taken: Vec<RankedHit> = Vec::new();
    let mut deferred: Vec<RankedHit> = Vec::new();
    let mut per_site: std::collections::BTreeMap<String, usize> = Default::default();
    for hit in hits {
        let site = host_of(&hit.result.url).map(|h| site_key(&h)).unwrap_or_default();
        let count = per_site.entry(site).or_insert(0);
        if *count < SITE_CAP {
            *count += 1;
            taken.push(hit);
        } else {
            deferred.push(hit);
        }
    }
    stats.site_cap_demoted = deferred.len();
    for hit in deferred {
        if taken.len() >= limit {
            break;
        }
        taken.push(hit);
    }
    stats.truncated = taken.len().saturating_sub(limit);
    taken.truncate(limit);
    (taken, stats)
}

/// 多后端融合：**每个**后端都被查询，候选按名次合并，再交给 [`rank`] 按意图重排。
///
/// # 为什么只融合名次，不融合分数
///
/// 供应商之间的分数不可比（各自的量纲与校准都不同），名次可比。故按
/// `1/(k+rank)` 累加各后端的名次贡献，k 与 [`rank`] 的先验同取
/// [`RANK_PRIOR_SMOOTHING`]——单后端时融合退化回原打分，不带入新口径。
///
/// # 去重即共识
///
/// 同一文档（同链接键或同标题键）被多个后端返回时合并成一条，来源记在
/// `origins`（`+` 连接），重复条数计进 `duplicates_merged`。重排只有一份
/// 实现：本函数不另写打分，只决定候选顺序。
pub fn fuse(reports: &[BackendResults], plan: &QueryPlan, limit: usize) -> (Vec<RankedHit>, RankStats) {
    use std::collections::BTreeMap;

    let mut fused: BTreeMap<String, f64> = BTreeMap::new();
    let mut first_seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut merged: BTreeMap<String, (SearchResult, Vec<String>)> = BTreeMap::new();
    let mut candidates = 0usize;
    let mut duplicates_merged = 0usize;

    for report in reports {
        for (position, result) in report.results.iter().enumerate() {
            candidates += 1;
            let key = canonical_url_key(&result.url)
                .or_else(|| title_key(&result.title))
                .unwrap_or_else(|| format!("{}#{}", report.backend, position));
            *fused.entry(key.clone()).or_insert(0.0) +=
                1.0 / (RANK_PRIOR_SMOOTHING + (position + 1) as f64);
            match merged.get_mut(&key) {
                Some((_, origins)) => {
                    duplicates_merged += 1;
                    if !origins.iter().any(|name| name == &report.backend) {
                        origins.push(report.backend.clone());
                    }
                }
                None => {
                    let seen = first_seen.len();
                    first_seen.insert(key.clone(), seen);
                    merged.insert(key, (result.clone(), vec![report.backend.clone()]));
                }
            }
        }
    }

    let mut keys: Vec<String> = merged.keys().cloned().collect();
    keys.sort_by(|a, b| {
        fused[b]
            .partial_cmp(&fused[a])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| first_seen[a].cmp(&first_seen[b]))
    });
    let ordered: Vec<SearchResult> = keys
        .iter()
        .map(|key| {
            let (result, origins) = &merged[key];
            let mut item = result.clone();
            item.backend = origins.join("+"); // 共识证据：多来源以 + 连接
            item
        })
        .collect();

    let (hits, mut stats) = rank(&ordered, plan, limit);
    stats.candidates = candidates;
    stats.duplicates_merged += duplicates_merged;
    stats.unique_documents = merged.len();
    (hits, stats)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum YearState {
    Unknown,
    Match,
    Mismatch,
}

/// 主题相关性（0–1）：标题对查询词（**不含年份 token**）的覆盖度，
/// 除以满额阈值。查询只有年份 token 时退回全部词，避免除零式退化。
fn topical_relevance(plan: &QueryPlan, title: &str) -> f64 {
    let topical: Vec<String> = plan
        .terms
        .iter()
        .filter(|term| !is_bare_year(term))
        .cloned()
        .collect();
    let terms = if topical.is_empty() { &plan.terms } else { &topical };
    (coverage(terms, title) / RELEVANCE_FULL_COVERAGE).clamp(0.0, 1.0)
}

fn is_bare_year(term: &str) -> bool {
    term.chars().count() == 4 && term.chars().all(|c| c.is_ascii_digit())
}

fn year_state(plan: &QueryPlan, result: &SearchResult) -> YearState {
    if plan.years.is_empty() {
        return YearState::Unknown;
    }
    let title_and_url = format!("{} {}", result.title, result.url);
    if plan
        .years
        .iter()
        .any(|year| title_and_url.contains(&year.to_string()))
    {
        return YearState::Match;
    }
    if plan
        .years
        .iter()
        .any(|year| result.snippet.contains(&year.to_string()))
    {
        return YearState::Match;
    }
    if let Some(published) = &result.published_at {
        if plan
            .years
            .iter()
            .any(|year| published.starts_with(&year.to_string()))
        {
            return YearState::Match;
        }
    }
    // 明确写着别的年份，且查询年份一个都没出现 → 降权。
    let other_year = extract_years(&title_and_url)
        .into_iter()
        .find(|year| !plan.years.contains(year));
    match (other_year, &result.published_at) {
        (Some(_), _) => YearState::Mismatch,
        (None, Some(date)) => {
            if extract_years(date)
                .into_iter()
                .any(|year| !plan.years.contains(&year))
            {
                YearState::Mismatch
            } else {
                YearState::Unknown
            }
        }
        (None, None) => YearState::Unknown,
    }
}

/// 查询词对文本的覆盖度（0–1）。中文词按字符二元组部分匹配，
/// 使"北外滩主市集"能部分命中"北外滩沿岸"；英文词按子串匹配。
fn coverage(terms: &[String], text: &str) -> f64 {
    if terms.is_empty() {
        return 0.0;
    }
    let haystack = text.to_lowercase();
    if haystack.trim().is_empty() {
        return 0.0;
    }
    let total: f64 = terms.iter().map(|t| t.chars().count().max(1) as f64).sum();
    let matched: f64 = terms
        .iter()
        .map(|term| term.chars().count().max(1) as f64 * term_match_ratio(term, &haystack))
        .sum();
    (matched / total).clamp(0.0, 1.0)
}

fn term_match_ratio(term: &str, haystack_lower: &str) -> f64 {
    if !term.chars().any(is_cjk) {
        return if haystack_lower.contains(&term.to_lowercase()) {
            1.0
        } else {
            0.0
        };
    }
    let chars: Vec<char> = term.chars().collect();
    if chars.len() < 2 {
        return if haystack_lower.contains(term) { 1.0 } else { 0.0 };
    }
    let bigrams: Vec<String> = chars
        .windows(2)
        .map(|pair| pair.iter().collect::<String>().to_lowercase())
        .collect();
    let hit = bigrams
        .iter()
        .filter(|bigram| haystack_lower.contains(bigram.as_str()))
        .count();
    hit as f64 / bigrams.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: &str, url: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: url.into(),
            snippet: String::new(),
            backend: "test".into(),
            published_at: None,
            rank_score: 0,
            rank_signals: Vec::new(),
        }
    }

    #[test]
    fn plan_extracts_site_filters_years_intent_and_recency() {
        let plan = plan_query("  官方 2025年 上海 咖啡文化节  site:Shanghai.Gov.CN， -site:example.com 最新 ");
        assert_eq!(plan.years, vec![2025]);
        assert!(plan.official_intent);
        assert_eq!(plan.recency, Recency::Month);
        assert_eq!(plan.site_includes, vec!["shanghai.gov.cn"]);
        assert_eq!(plan.site_excludes, vec!["example.com"]);
        assert_eq!(plan.keywords, "官方 2025年 上海 咖啡文化节 最新");
        assert!(plan.raw.contains("site:Shanghai.Gov.CN"));
    }

    #[test]
    fn malformed_or_query_only_site_filters_stay_in_the_keywords() {
        let plan = plan_query("site:shanghai.gov.cn/path 文化节");
        assert!(plan.site_includes.is_empty());
        assert!(plan.keywords.contains("site:shanghai.gov.cn/path"));

        let only = plan_query("site:shanghai.gov.cn");
        assert!(only.site_includes.is_empty());
        assert_eq!(only.keywords, "site:shanghai.gov.cn");
    }

    #[test]
    fn recency_maps_to_provider_parameters() {
        assert_eq!(plan_query("今天 天气").recency, Recency::Day);
        assert_eq!(plan_query("本周 新闻").recency, Recency::Week);
        assert_eq!(plan_query("近期 政策").recency, Recency::Month);
        assert_eq!(plan_query("今年 展会").recency, Recency::Year);
        assert_eq!(plan_query("2025 展会").recency, Recency::None);
        assert_eq!(Recency::None.bocha_freshness(), "noLimit");
        assert_eq!(Recency::Week.bocha_freshness(), "oneWeek");
    }

    #[test]
    fn years_are_only_four_digit_runs() {
        assert_eq!(extract_years("12025 和 2026年"), vec![2026]);
        assert_eq!(extract_years("1999 2000x"), vec![1999, 2000]);
        assert!(extract_years("42 300").is_empty());
    }

    #[test]
    fn canonical_url_removes_tracking_fragment_and_parameters_order() {
        let a = canonical_url_key("https://Example.com/a/b/?utm_source=x&f=1#part").unwrap();
        let b = canonical_url_key("http://example.com/a/b").unwrap();
        assert_eq!(a, b);
        let c = canonical_url_key("https://example.com/a/b?b=2&a=1").unwrap();
        let d = canonical_url_key("https://example.com/a/b?a=1&b=2").unwrap();
        assert_eq!(c, d);
        assert!(canonical_url_key("javascript:alert(1)").is_none());
        // 同站不同页面必须是不同文档键（曾把整条路径误当 authority，
        // 导致同站所有结果被合并成一条）。
        assert_ne!(
            canonical_url_key("https://news.qq.com/a/1").unwrap(),
            canonical_url_key("https://news.qq.com/a/2").unwrap()
        );
        assert_ne!(
            canonical_url_key("https://example.com/x").unwrap(),
            canonical_url_key("https://example.com/y").unwrap()
        );
    }

    #[test]
    fn title_key_strips_site_suffix_and_punctuation() {
        assert_eq!(
            title_key("上海国际咖啡文化节4月30日开幕,主市集设在北外滩_腾讯新闻"),
            title_key("上海国际咖啡文化节 4 月 30 日开幕，主市集设在北外滩")
        );
        assert_eq!(
            title_key("足不出“沪”嗨翻天!2025上海国际咖啡文化节虹口主会场亮点抢先看_上海市人民政府")
                .is_some(),
            true
        );
        assert!(title_key("短标题").is_none());
    }

    #[test]
    fn government_and_site_keys_respect_two_part_suffixes() {
        assert!(is_government_host("www.shanghai.gov.cn"));
        assert!(is_government_host("ccgp.gov.cn"));
        assert!(!is_government_host("gov.cn.example.com"));
        assert_eq!(site_key("www.shanghai.gov.cn"), "shanghai.gov.cn");
        assert_eq!(site_key("shhk.gov.cn"), "shhk.gov.cn");
        assert_eq!(site_key("news.qq.com"), "qq.com");
        assert_eq!(site_key("sh.bendibao.com"), "bendibao.com");
    }

    #[test]
    fn official_source_is_lifted_for_an_official_query() {
        // 冻结基线 B1 的真实前 6 名（标题与网址摘自 bocha-benchmark-results.json）。
        let plan = plan_query("上海国际咖啡文化节 2025 举办时间 地点 官方");
        let results = vec![
            hit("文化活动(CULTURAL)丨2025上海国际咖啡文化节沙龙文旅举办地上海市_网易订阅", "https://www.163.com/dy/article/JSNDTALJ0514A42S.html?f=post2020_dy_recommends"),
            hit("文化活动(CULTURAL)丨2025上海国际咖啡文化节", "https://new.qq.com/rain/a/20250409A06K2N00"),
            hit("2025上海国际咖啡文化节活动详情(预约入口+参展品牌)- 上海本地宝", "http://sh.bendibao.com/xiuxian/20251021/301791.shtm"),
            hit("2025年上海咖啡节-上海咖啡展_门票_时间地点_会刊名录-世展网", "https://www.shifair.com/exhibitionDetails/2264.html"),
            hit("2025上海国际咖啡文化节活动详情(预约入口+参展品牌)- 上海本地宝", "https://m.sh.bendibao.com/xiuxian/301791.html"),
            hit("足不出“沪”嗨翻天!2025上海国际咖啡文化节虹口主会场亮点抢先看", "https://www.shanghai.gov.cn/nw17239/20250429/542d7d5e6a53405aba48d8a736ed8a4c.html"),
        ];
        let (hits, stats) = rank(&results, &plan, 8);
        assert_eq!(stats.duplicates_merged, 1, "两条本地宝是同一标题簇");
        assert!(
            hits[0].result.url.contains("shanghai.gov.cn"),
            "官方查询下政府原文必须排第一，实得：{:?}",
            hits.iter().map(|h| &h.result.url).collect::<Vec<_>>()
        );
        assert!(hits[0].signals.contains(&"官方域名".to_string()));
    }

    /// **只有单一后端时的已知代价**——用测试钉住，而不是只留一句口头说明。
    ///
    /// 冻结基线 B3 当轮能把官方页送进前 3，靠的是第二个后端提供了候选；
    /// 单后端没返回的官方原文，重排层无法凭空补出来。故 B3 类查询现在
    /// 会退回「前 8 条没有官方页」。这条断言写的是**当前真实行为**，不是
    /// 理想行为；将来若接入第二个后端，本测试应当随之改掉。
    #[test]
    fn an_official_page_absent_from_the_candidates_cannot_be_recovered() {
        let plan = plan_query("北外滩主市集 2025 咖啡文化节 商家参与 活动安排");
        let results = vec![
            hit("2025上海国际咖啡文化节主市集在北外滩沿岸举行_看看新闻网", "https://www.kankanews.com/detail/M8Q801b6XQL"),
            hit("上海国际咖啡文化节4月30日开幕,主市集设在北外滩", "https://new.qq.com/rain/a/20250424A06IIP00"),
        ];
        let (hits, stats) = rank(&results, &plan, 8);
        assert_eq!(stats.candidates, 2);
        assert!(
            !hits.iter().any(|h| h.result.url.contains("shanghai.gov.cn")),
            "候选里没有的官方页不得凭空出现"
        );
    }

    #[test]
    fn official_intent_makes_the_government_page_rank_first() {
        let plan = plan_query("上海国际咖啡文化节 2025 官方");
        let results = vec![
            hit("2025上海国际咖啡文化节活动详情(预约入口+参展品牌)- 上海本地宝", "https://sh.bendibao.com/xiuxian/301791.shtm"),
            hit("足不出“沪”嗨翻天!2025上海国际咖啡文化节虹口主会场亮点抢先看", "https://www.shanghai.gov.cn/nw17239/20250429/542d7d5e6a53405aba48d8a736ed8a4c.html"),
        ];
        let (hits, _) = rank(&results, &plan, 8);
        assert!(hits[0].result.url.contains("shanghai.gov.cn"));
    }

    #[test]
    fn mismatching_year_is_demoted_for_a_year_specific_query() {
        let plan = plan_query("北外滩主市集 2025 咖啡文化节");
        let results = vec![
            hit("2026上海国际咖啡文化节虹口主会场", "https://www.shanghai.gov.cn/nw4411/20260425/a.html"),
            hit("2025上海国际咖啡文化节北外滩主市集商家参与安排", "https://news.example.com/2025-cafe"),
        ];
        let (hits, _) = rank(&results, &plan, 8);
        assert_eq!(hits[0].result.url, "https://news.example.com/2025-cafe");
        assert!(hits[1].signals.contains(&"年份不符".to_string()));
    }

    /// 只匹配查询年份的无关政府页不得拿意图加分。
    ///
    /// 这条规则的来历是免费干跑：B4 查询"上海 2025 咖啡文化节 主办单位…
    /// 政府"下，弱引擎把《广东省商务厅 2025 年"粤贸全国"活动目录》排到了
    /// 第一，而它只与查询共享 "2025"。规则本身与后端数量无关，故保留。
    #[test]
    fn a_page_matching_only_the_query_year_earns_no_intent_bonus() {
        let plan = plan_query("上海 2025 咖啡文化节 政府");
        let results = vec![
            hit(
                "广东省商务厅关于印发2025年“粤贸全国”活动目录的通知",
                "https://www.shanwei.gov.cn/notice.html",
            ),
            hit(
                "2025上海国际咖啡文化节将在虹口北外滩开幕",
                "https://www.shanghai.gov.cn/news.html",
            ),
        ];
        let (hits, _) = rank(&results, &plan, 8);
        assert_eq!(hits[0].result.url, "https://www.shanghai.gov.cn/news.html");
        let irrelevant = &hits[1];
        assert_eq!(irrelevant.result.url, "https://www.shanwei.gov.cn/notice.html");
        assert!(
            !irrelevant.signals.contains(&"官方域名".to_string()),
            "只匹配年份的无关政府页不得拿官方加分：{:?}",
            irrelevant.signals
        );
        assert!(
            !irrelevant.signals.contains(&"年份相符".to_string()),
            "分数没拿到时不该报告该信号：{:?}",
            irrelevant.signals
        );
    }

    /// 同一文档在**单一后端**的列表里出现两次，仍应合并成一条。
    /// 这是基线的真实形态：同一篇通稿的多个域名、同一页面的桌面版/移动版。
    #[test]
    fn same_document_twice_in_one_list_is_merged() {
        let plan = plan_query("上海 咖啡文化节");
        let doc = "https://www.shanghai.gov.cn/nw4411/2025/cafe.html";
        let results = vec![
            hit("上海咖啡文化节", doc),
            hit("上海咖啡文化节", "https://www.shanghai.gov.cn/nw4411/2025/cafe.html?from=share"),
        ];
        let (hits, stats) = rank(&results, &plan, 8);
        assert_eq!(hits.len(), 1, "同一规范化 URL 只保留一条");
        assert_eq!(stats.duplicates_merged, 1);
        assert_eq!(stats.candidates, 2);
    }

    #[test]
    fn site_cap_demotes_extra_same_site_hits_but_backfills_when_room() {
        let plan = plan_query("社区生鲜 价格");
        // 显式标注：`rank` 收 `&[SearchResult]`，不标注时推断会落到不定长切片上。
        let results: Vec<SearchResult> = (1..=4)
            .map(|i| hit(&format!("社区生鲜价格战报道{i}"), &format!("https://news.qq.com/a/{i}")))
            .collect();
        let (hits, stats) = rank(&results, &plan, 8);
        assert_eq!(hits.len(), 4, "名额没用满时降权的仍补回");
        assert_eq!(stats.site_cap_demoted, 2);
        // 降权后同站两条先出现，另外两条排在后面。
        let qq: Vec<&RankedHit> = hits.iter().filter(|h| h.result.url.contains("qq.com")).collect();
        assert_eq!(qq.len(), 4);
    }

    #[test]
    fn ranking_is_deterministic_for_identical_input() {
        let plan = plan_query("上海国际咖啡文化节 2025 官方");
        let results = vec![
            hit("B标题", "https://b.example.com/1"),
            hit("A标题", "https://a.example.com/1"),
            hit("C标题", "https://c.example.com/1"),
        ];
        let first = rank(&results, &plan, 8);
        let second = rank(&results, &plan, 8);
        assert_eq!(first, second);
    }
}

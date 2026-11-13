//! 公开网页抓取：robots.txt 遵从 + 限速（设计文档 §7.2 的 `web_fetch`）。
//!
//! # 三件必须做的事（以及为什么）
//!
//! 1. **先读 robots.txt 再抓**。这不是礼貌，是合规底线。被 `Disallow` 的路径
//!    一律不抓，且这一判断是**代码强制**的——不依赖调用方记得检查。
//! 2. **限速**（同一主机的最小间隔）。除此之外不设任何伪装：不改 UA 冒充浏览器、
//!    不绕验证码、不做任何反爬绕过。**遇到验证码就如实报告不可用**——
//!    绕过它既违反站点条款，也让"这份情报从哪来"变得不可审计。
//! 3. **失败即拒绝**（不变量②）。抓不到就是抓不到，返回 [`FetchError`]，
//!    绝不返回一段"大概是这个意思"的编造正文。
//!
//! # 与 `SearchProvider` 的关系
//!
//! [`HttpFetcher`] 实现 `crate::…::SearchProvider` 的**抓取**那一半。
//! 检索那一半需要一个真实检索后端（API key 或自托管实例），未配置时如实报
//! "未取得"——见 [`FusionSearch`] 与 [`crate::search_rank`]。

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 抓取错误。
///
/// # 变体刻意分得细（不变量②：失败即拒绝，且原因**可操作**）
///
/// 每一种都对应**不同的应对**：
///
/// | 变体 | 应对 |
/// |---|---|
/// | `DisallowedByRobots` | 换数据源——此路被站点明确禁止，不该再试 |
/// | `RobotsUnavailable` | 可能是网络抖动；可稍后重试（**不是**被禁止） |
/// | `BotChallenge` | 该站点要求人机验证；**不做绕过**，改用其他来源或人工提供 |
/// | `HttpStatus` | 4xx 多为链接失效（换来源）；429/5xx 可退避重试 |
/// | `Transport` | 网络层问题，可重试 |
/// | `TooLarge` | 该页不适合直读，需换方式（如只取摘要） |
/// | `InvalidUrl` | 调用方参数错，改参数 |
///
/// 合并成 `Failed(String)` 会让这些应对全部退化为"再试一次"，
/// 而其中有些"再试"是白费（例如被 robots 禁止的路径）。
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FetchError {
    #[error("URL 非法（仅支持 http/https）：{0}")]
    InvalidUrl(String),
    #[error("robots.txt 禁止抓取 {url}（命中规则 `{rule}`）——这是站点的明确政策，不应绕过")]
    DisallowedByRobots { url: String, rule: String },
    #[error("无法获取 {url} 的 robots.txt（{detail}）——按 fail-closed 不抓取；注意这**不是**被禁止")]
    RobotsUnavailable { url: String, detail: String },
    #[error("{url} 要求人机验证（HTTP {status}）——本系统**不做任何绕过**；请改用其他数据源或人工提供")]
    BotChallenge { url: String, status: u16 },
    #[error("{url} 返回 HTTP {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("传输失败（{url}）：{detail}")]
    Transport { url: String, detail: String },
    #[error("服务返回了无法识别的响应（{url}）：{detail}")]
    InvalidResponse { url: String, detail: String },
    #[error("{url} 响应体过大（{bytes} 字节 > 上限 {limit}）")]
    TooLarge {
        url: String,
        bytes: usize,
        limit: usize,
    },
}

impl FetchError {
    /// **重试有无意义**。
    ///
    /// # 为什么这是一个类型层面的问题，不是文案问题
    ///
    /// 实测（run24）：模型对同一个 404 的 `stats.gov.cn` URL 连续试了 4 次。
    /// 它的行为在信息不足时是**理性的**——"抓取失败"这句话没有回答它真正
    /// 的问题：**换个时间再试有用吗？**
    ///
    /// 而两类失败的处置方向完全相反：
    ///
    /// | 失败 | 重试 | 正确动作 | 本函数返回 |
    /// |---|---|---|---|
    /// | 404 / 410 | ❌ | 换数据源 | `false` |
    /// | 证书过期 | ❌ | 换数据源（或让对方修证书） | `false` |
    /// | 人机验证 | ❌ | 换数据源 / 人工提供 | `false` |
    /// | robots 禁止 | ❌ | **尊重站点政策**，不重试不绕过 | `false` |
    /// | robots.txt 取不到 | ✅ | 本次仍 fail-closed；稍后可重试获取许可 | `true` |
    /// | URL 非法 | ❌ | 改 URL | `false` |
    /// | 响应体过大 | ❌ | 换来源 | `false` |
    /// | 429 / 5xx | ✅ | 稍后重试 | `true` |
    /// | 网络超时 / 连接失败 | ✅ | 稍后重试 | `true` |
    ///
    /// 注意默认分支：**未知状态码一律不可重试**。这个方向的取舍是有意的
    /// ——若未知的可重试，模型会浪费 turn 去试；若未知的不可重试，
    /// 最坏情况是"本来能成功但没试"，而模型会换数据源（仍有进展）。
    pub fn retryable(&self) -> bool {
        match self {
            FetchError::HttpStatus { status, .. } => {
                // 只有限流与服务端故障值得重试
                *status == 429 || (500..600).contains(status)
            }
            FetchError::Transport { .. } | FetchError::RobotsUnavailable { .. } => true,
            // 其余的处置都是"换来源"或"改输入"，重试无用
            FetchError::InvalidUrl(_)
            | FetchError::InvalidResponse { .. }
            | FetchError::DisallowedByRobots { .. }
            | FetchError::BotChallenge { .. }
            | FetchError::TooLarge { .. } => false,
        }
    }

    /// 给**模型**看的完整说明（含可操作的下一步）。
    ///
    /// 与 `Display` 分开：日志里要简洁，而给模型的文本必须回答"我该怎么办"。
    pub fn actionable(&self) -> String {
        if self.retryable() {
            format!(
                "{self}\n→ 这可能是**临时性故障**，稍后重试可能成功；\
                 但若连续失败，请改换数据源（不要在同一 URL 上循环）。"
            )
        } else {
            match self {
                FetchError::DisallowedByRobots { .. } => format!(
                    "{self}\n→ 这是站点的明确政策。**换数据源**；\
                     本系统不绕过 robots.txt。"
                ),
                FetchError::BotChallenge { .. } => format!(
                    "{self}\n→ **换数据源**或请人工提供该页面。\
                     本系统不绕过人机验证。"
                ),
                FetchError::InvalidResponse { .. } => format!(
                    "{self}\n→ 服务有响应，但格式与接口约定不符；请核对 API 密钥、账户额度和请求格式，\
                     并改用其他可用来源。本次不是‘没有搜索结果’，不要原样循环重试。"
                ),
                _ => format!(
                    "{self}\n→ **重试无用**（该资源不存在、永久不可达，或输入有误）。\
                     请**换数据源**或修正输入——同一请求再试只会得到同样的结果。"
                ),
            }
        }
    }
}

/// 抓取结果。
#[derive(Debug, Clone, PartialEq)]
pub struct FetchedPage {
    pub url: String,
    /// 最终 URL（跟随重定向后）。
    pub final_url: String,
    pub status: u16,
    pub content_type: String,
    /// 抽取出的正文（去标签后的纯文本）。
    pub text: String,
    /// 原始字节数（供审计判断"是不是抓到了一个空页"）。
    pub raw_bytes: usize,
}

/// robots.txt 的**抓取许可**判断结果。
///
/// 三态而非布尔：`Unavailable`（读不到 robots.txt）与 `Disallowed`（明确禁止）
/// 是两件事。前者按惯例**保守处理为禁止抓取**（`Disallow: /` 的语义），
/// 但错误信息必须说清是"读不到"而不是"被禁止"——否则审计会误判站点政策。
#[derive(Debug, Clone, PartialEq)]
pub enum RobotsVerdict {
    /// 站点有 robots.txt 且明确允许。
    Allowed,
    /// 站点**没有** robots.txt（HTTP 4xx）。
    ///
    /// # 为什么它与 `Allowed` 分开（RFC 9309 §2.3.1.3）
    ///
    /// 标准规定：robots.txt 返回 **4xx ⇒ 视为无限制**（该文件不存在，
    /// 站点未施加任何爬取限制）；**5xx ⇒ 视为完全禁止**（服务端出错，
    /// 保守起见不许抓）。
    ///
    /// 分开记录的价值在**审计**：报告的"情报来源合规性"一栏应能回答
    /// "该站点是否有爬取政策"，`NoRestrictions` 与 `Allowed` 的答案不同。
    NoRestrictions,
    Disallowed { rule: String },
    /// robots.txt 取不到且**无法**按标准推断（网络失败 / 5xx / 解析失败）。
    Unavailable { detail: String },
}

impl RobotsVerdict {
    /// 是否允许抓取。
    ///
    /// 按 RFC 9309：`Allowed` 与 `NoRestrictions` 允许；
    /// `Disallowed` 与 `Unavailable`（5xx/网络失败）不允许。
    ///
    /// ⚠️ 早先的实现把 4xx 也归入 `Unavailable`，于是**没有 robots.txt 的站点
    /// 一律抓不到**——实测 `example.com` 正是这种情况（robots.txt 返回 404）。
    /// 那不是"保守"，是**读错了标准**：4xx 的语义是"站点未设限制"，不是"未知"。
    pub fn allows_fetch(&self) -> bool {
        matches!(self, RobotsVerdict::Allowed | RobotsVerdict::NoRestrictions)
    }
}

/// 一个主机的 robots 规则（已解析为前缀匹配列表）。
#[derive(Debug, Clone, Default)]
struct RobotsRules {
    /// `Disallow` 前缀（长度升序；最长匹配优先）。
    disallow: Vec<String>,
    /// `Allow` 前缀（用于覆盖更宽的 Disallow）。
    allow: Vec<String>,
    /// 抓取该文件的时刻（用于缓存过期）。
    fetched_at: Option<Instant>,
}

impl RobotsRules {
    fn verdict_for(&self, path: &str) -> RobotsVerdict {
        // 最长匹配优先：Allow 与 Disallow 取更具体的那条
        let best_disallow = self
            .disallow
            .iter()
            .filter(|p| path.starts_with(p.as_str()))
            .max_by_key(|p| p.len());
        let best_allow = self
            .allow
            .iter()
            .filter(|p| path.starts_with(p.as_str()))
            .max_by_key(|p| p.len());

        match (best_disallow, best_allow) {
            (None, _) => RobotsVerdict::Allowed,
            (Some(d), Some(a)) if a.len() >= d.len() => RobotsVerdict::Allowed,
            (Some(d), _) => RobotsVerdict::Disallowed { rule: d.clone() },
        }
    }

    /// 解析 robots.txt 全文，只取 `User-agent: *`（及 `User-agent: <我们>`）段。
    ///
    /// # 为什么只取这两段
    ///
    /// robots.txt 是按 UA 分段的。我们用一个**可识别的、非浏览器的** UA
    /// （形如 `ShangYi/0.1 (+contact)`），故只关心 `*` 段与明确点名我们 UA 的段。
    /// 冒充浏览器 UA 去匹配更宽松的段是伪装，不做。
    fn parse(text: &str, our_agent: &str) -> Self {
        let mut rules = RobotsRules::default();
        let mut active = false; // 当前段是否适用于我们
        // 记录上一行是不是 User-agent（用于处理连续多个 agent 行）
        let mut last_was_agent = false;

        for raw in text.lines() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let key = k.trim().to_ascii_lowercase();
            let val = v.trim();

            match key.as_str() {
                "user-agent" => {
                    let is_us = val == "*"
                        || our_agent
                            .to_ascii_lowercase()
                            .contains(&val.to_ascii_lowercase());
                    if last_was_agent {
                        // 连续 agent 行 ⇒ 同一段适用于多个 UA
                        active = active || is_us;
                    } else {
                        active = is_us;
                    }
                    last_was_agent = true;
                }
                "disallow" if active => {
                    last_was_agent = false;
                    // 空 Disallow 表示"允许全部"（不添加规则）
                    if !val.is_empty() {
                        rules.disallow.push(val.to_string());
                    }
                }
                "allow" if active => {
                    last_was_agent = false;
                    if !val.is_empty() {
                        rules.allow.push(val.to_string());
                    }
                }
                _ => {
                    last_was_agent = false;
                }
            }
        }
        rules
    }
}

/// 抓取器配置。
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// UA。**必须可识别且不自称浏览器**——冒充浏览器是伪装。
    pub user_agent: String,
    /// 同一主机两次请求的最小间隔。
    pub per_host_interval: Duration,
    /// robots.txt 缓存时长。
    pub robots_ttl: Duration,
    /// 单个响应体的字节上限（防止一个超大页面打爆内存）。
    pub max_body_bytes: usize,
    /// 请求超时。
    pub timeout: Duration,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            // 可识别、不带浏览器特征；附项目标识便于站点方追溯
            user_agent: "ShangYi/0.1 (public-business-data; +https://github.com/shangyi-agent)".into(),
            // 1 秒/主机：温和，且足够低到不会给目标站点造成压力
            per_host_interval: Duration::from_secs(1),
            robots_ttl: Duration::from_secs(3600),
            max_body_bytes: 2 * 1024 * 1024,
            timeout: Duration::from_secs(20),
        }
    }
}

/// 传输抽象（使抓取逻辑可在无网络下测试）。
pub trait HttpTransport: Send + Sync {
    /// GET 请求，返回 `(状态码, content-type, 正文)`。
    fn get(&self, url: &str, user_agent: &str, timeout: Duration) -> Result<(u16, String, String), String>;
}

/// 基于 ureq 的真实传输。
pub struct UreqTransport;

impl HttpTransport for UreqTransport {
    fn get(&self, url: &str, user_agent: &str, timeout: Duration) -> Result<(u16, String, String), String> {
        let config = ureq::config::Config::builder()
            // 关掉"非 2xx 即错误"：我们要读状态码本身（403/404/429 都有诊断意义），
            // 而不是把状态码折叠成一个错误。这与 `shangyi-llm` 的处置同源。
            .http_status_as_error(false)
            .timeout_global(Some(timeout))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        match agent.get(url).header("User-Agent", user_agent).call() {
            Ok(mut resp) => {
                let status = resp.status().as_u16();
                let ct = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let body = resp
                    .body_mut()
                    .read_to_string()
                    .map_err(|e| format!("读响应体失败：{e}"))?;
                Ok((status, ct, body))
            }
            Err(e) => Err(format!("{e}")),
        }
    }
}

/// 网页抓取器：遵守 robots.txt + 限速。
pub struct HttpFetcher<T: HttpTransport = UreqTransport> {
    transport: T,
    config: FetchConfig,
    /// 主机 → robots 规则（带获取时刻）。
    robots: std::sync::Mutex<HashMap<String, RobotsRules>>,
    /// 主机 → 上次请求时刻（限速用）。
    last_request: std::sync::Mutex<HashMap<String, Instant>>,
}

impl HttpFetcher<UreqTransport> {
    pub fn new() -> Self {
        Self::with_transport(UreqTransport, FetchConfig::default())
    }
}

impl Default for HttpFetcher<UreqTransport> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: HttpTransport> HttpFetcher<T> {
    pub fn with_transport(transport: T, config: FetchConfig) -> Self {
        Self {
            transport,
            config,
            robots: std::sync::Mutex::new(HashMap::new()),
            last_request: std::sync::Mutex::new(HashMap::new()),
        }
    }

    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    /// 拆出 `(scheme, host, path)`。仅支持 http/https。
    fn split_url(url: &str) -> Result<(String, String, String), FetchError> {
        let rest = url
            .strip_prefix("https://")
            .map(|r| ("https", r))
            .or_else(|| url.strip_prefix("http://").map(|r| ("http", r)))
            .ok_or_else(|| FetchError::InvalidUrl(url.to_string()))?;
        let (scheme, after) = rest;
        let (host_part, path) = match after.find('/') {
            Some(i) => (&after[..i], &after[i..]),
            None => (after, "/"),
        };
        let host = host_part.split('@').next_back().unwrap_or(host_part);
        let host = host.split(':').next().unwrap_or(host).to_string();
        if host.is_empty() {
            return Err(FetchError::InvalidUrl(url.to_string()));
        }
        Ok((scheme.to_string(), host, path.to_string()))
    }

    /// 取（并缓存）某主机的 robots 规则。
    ///
    /// **不缓存 `Unavailable` 的负面结论**：一次网络抖动不该让整个会话期间
    /// 都认为该站点禁止抓取。故失败时只返回本次结论，不写入缓存。
    fn robots_for(&self, scheme: &str, host: &str) -> Result<(RobotsRules, bool), String> {
        {
            let cache = self.robots.lock().map_err(|_| "robots 缓存锁中毒".to_string())?;
            if let Some(r) = cache.get(host) {
                let fresh = r
                    .fetched_at
                    .map(|t| t.elapsed() < self.config.robots_ttl)
                    .unwrap_or(false);
                if fresh {
                    // 缓存里同时存"有没有该文件"（`disallow`/`allow` 皆空 ⇒ 没有）
                    let had = !r.disallow.is_empty() || !r.allow.is_empty();
                    return Ok((r.clone(), had));
                }
            }
        }
        let url = format!("{scheme}://{host}/robots.txt");
        let (status, _ct, body) = self
            .transport
            .get(&url, &self.config.user_agent, self.config.timeout)
            .map_err(|e| format!("取 robots.txt 失败：{e}"))?;
        // RFC 9309 §2.3.1.3 的三种情形必须分开处理（详见 `RobotsVerdict`）：
        //   2xx ⇒ 解析并遵守
        //   4xx ⇒ **无限制**（文件不存在 ⇒ 站点未施加限制）
        //   5xx ⇒ 完全禁止（服务端异常，不得当作"没限制"）
        let rules = if (200..300).contains(&status) {
            RobotsRules::parse(&body, &self.config.user_agent)
        } else if (400..500).contains(&status) {
            let mut r = RobotsRules::default();
            r.fetched_at = Some(Instant::now());
            // 空规则表 ⇒ viewport 判断恒为 Allowed；用 fetched_at 标记已缓存
            if let Ok(mut cache) = self.robots.lock() {
                cache.insert(host.to_string(), r.clone());
            }
            return Ok((r, false));
        } else {
            return Err(format!("robots.txt 返回 HTTP {status}（5xx 按 RFC 9309 视为完全禁止）"));
        };
        let mut out = rules.clone();
        out.fetched_at = Some(Instant::now());
        if let Ok(mut cache) = self.robots.lock() {
            cache.insert(host.to_string(), out);
        }
        Ok((rules, true))
    }

    /// 判断某 URL 是否允许抓取（只读查询，不发正文请求）。
    pub fn check_allowed(&self, url: &str) -> Result<RobotsVerdict, FetchError> {
        let (scheme, host, path) = Self::split_url(url)?;
        match self.robots_for(&scheme, &host) {
            Ok((rules, had_file)) => {
                let v = rules.verdict_for(&path);
                // 区分"有 robots.txt 且允许"与"根本没有 robots.txt"
                match (had_file, v) {
                    (false, RobotsVerdict::Allowed) => Ok(RobotsVerdict::NoRestrictions),
                    (_, other) => Ok(other),
                }
            }
            Err(detail) => Ok(RobotsVerdict::Unavailable { detail }),
        }
    }

    /// 限速等待：距上次请求同一主机不足间隔则 sleep。
    fn throttle(&self, host: &str) {
        let wait = {
            let mut last = match self.last_request.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            let now = Instant::now();
            let wait = match last.get(host) {
                Some(prev) => {
                    let elapsed = now.duration_since(*prev);
                    self.config.per_host_interval.checked_sub(elapsed)
                }
                None => None,
            };
            last.insert(host.to_string(), now + wait.unwrap_or_default());
            wait
        };
        if let Some(d) = wait {
            std::thread::sleep(d);
        }
    }

    /// 抓取一个 URL（含 robots 检查与限速）。
    pub fn fetch(&self, url: &str) -> Result<FetchedPage, FetchError> {
        let (_scheme, host, _path) = Self::split_url(url)?;

        // 1) robots
        match self.check_allowed(url)? {
            RobotsVerdict::Allowed | RobotsVerdict::NoRestrictions => {}
            RobotsVerdict::Disallowed { rule } => {
                return Err(FetchError::DisallowedByRobots {
                    url: url.to_string(),
                    rule,
                });
            }
            RobotsVerdict::Unavailable { detail } => {
                // fail-closed：读不到 robots.txt 就不抓，且说清是"读不到"
                return Err(FetchError::RobotsUnavailable {
                    url: url.to_string(),
                    detail,
                });
            }
        }

        // 2) 限速
        self.throttle(&host);

        // 3) 抓
        let (status, content_type, body) = self
            .transport
            .get(url, &self.config.user_agent, self.config.timeout)
            .map_err(|e| FetchError::Transport {
                url: url.to_string(),
                detail: e,
            })?;

        // 4) 反爬识别：**如实报告，不做绕过**
        //
        // 识别特征刻意保守（只认明确的挑战页标志），因为误报会让一次本该成功的
        // 抓取被拒。识别出来的意义是给用户**可操作的**提示（该站点要求人机验证），
        // 而不是让系统去想办法过它。
        if looks_like_bot_challenge(&body) {
            return Err(FetchError::BotChallenge {
                url: url.to_string(),
                status,
            });
        }

        let raw_bytes = body.len();
        if raw_bytes > self.config.max_body_bytes {
            return Err(FetchError::TooLarge {
                url: url.to_string(),
                bytes: raw_bytes,
                limit: self.config.max_body_bytes,
            });
        }

        if !(200..300).contains(&status) {
            return Err(FetchError::HttpStatus {
                url: url.to_string(),
                status,
            });
        }

        Ok(FetchedPage {
            url: url.to_string(),
            final_url: url.to_string(),
            status,
            content_type: content_type.clone(),
            text: extract_text(&body, &content_type),
            raw_bytes,
        })
    }
}

/// 是否是明确的人机验证/反爬挑战页。
///
/// **只用于如实报告**，不用于绕过。
fn looks_like_bot_challenge(body: &str) -> bool {
    let head: String = body.chars().take(4000).collect::<String>().to_ascii_lowercase();
    const MARKERS: [&str; 6] = [
        "complete the following challenge",
        "confirm this search was made by a human",
        "please verify you are a human",
        "are you a robot",
        "cf-challenge",
        "captcha",
    ];
    MARKERS.iter().any(|m| head.contains(m))
}

/// 从 HTML 抽取纯文本。
///
/// # 这是**粗抽**，不是正文提取
///
/// 它去脚本/样式/标签并压空白。**不做读音识别、不做正文主体检测**——
/// 那些属于 `Sidecar`（§7.2：进程隔离的 Python 侧车做解析提纯），
/// 且在此处做会让"抽出来的东西算不算证据"变得不可审计。
/// 调用方拿到的是"这一页大致说了什么"，进一步结构化提取要另走工具。
pub fn extract_text(body: &str, content_type: &str) -> String {
    // JSON / 纯文本就不去标签（去了会破坏内容）
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("json") || ct.starts_with("text/plain") {
        return body.trim().to_string();
    }
    let mut out = String::with_capacity(body.len() / 3);
    let bytes = body.as_bytes();
    let mut i = 0;
    let mut skip_until: Option<&str> = None;
    while i < bytes.len() {
        if let Some(tag) = skip_until {
            // 找 </script> 或 </style>
            if let Some(pos) = body[i..].to_ascii_lowercase().find(tag) {
                i += pos + tag.len();
                skip_until = None;
            } else {
                break;
            }
            continue;
        }
        if bytes[i] == b'<' {
            let lower = body[i..].to_ascii_lowercase();
            if lower.starts_with("<script") {
                skip_until = Some("</script>");
                i += 1;
                continue;
            }
            if lower.starts_with("<style") {
                skip_until = Some("</style>");
                i += 1;
                continue;
            }
            // 跳过整个标签
            match body[i..].find('>') {
                Some(pos) => {
                    i += pos + 1;
                    // 块级标签处补空格，避免"</p><p>"把两段粘成一个词
                    out.push(' ');
                    continue;
                }
                None => break,
            }
        }
        // 按字符推进（多字节安全）
        let ch = body[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    // 解几个最常见的实体
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── 测试用假传输 ──

    struct FakeHttp {
        /// (状态码, content-type, body) 按 URL 前缀匹配
        routes: Vec<(String, u16, String, String)>,
        calls: AtomicUsize,
    }

    impl FakeHttp {
        fn new(routes: Vec<(&str, u16, &str, &str)>) -> Self {
            Self {
                routes: routes
                    .into_iter()
                    .map(|(u, s, ct, b)| (u.to_string(), s, ct.to_string(), b.to_string()))
                    .collect(),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl HttpTransport for FakeHttp {
        fn get(&self, url: &str, _ua: &str, _t: Duration) -> Result<(u16, String, String), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            for (prefix, s, ct, b) in &self.routes {
                if url.starts_with(prefix.as_str()) {
                    return Ok((*s, ct.clone(), b.clone()));
                }
            }
            Err(format!("未配置的路由：{url}"))
        }
    }

    fn fetcher(routes: Vec<(&str, u16, &str, &str)>) -> HttpFetcher<FakeHttp> {
        HttpFetcher::with_transport(
            FakeHttp::new(routes),
            FetchConfig {
                // 测试里把限速降到 0，避免用例变慢
                per_host_interval: Duration::ZERO,
                ..Default::default()
            },
        )
    }

    // ── robots 解析 ──

    #[test]
    fn robots_parses_disallow_and_allow() {
        let txt = "\
User-agent: *
Disallow: /private/
Allow: /private/public/
Disallow: /tmp
";
        let r = RobotsRules::parse(txt, "ShangYi/0.1");
        assert!(matches!(r.verdict_for("/index.html"), RobotsVerdict::Allowed));
        assert!(matches!(
            r.verdict_for("/private/x"),
            RobotsVerdict::Disallowed { .. }
        ));
        // Allow 更长 ⇒ 覆盖 Disallow（最长匹配优先）
        assert!(matches!(
            r.verdict_for("/private/public/y"),
            RobotsVerdict::Allowed
        ));
        assert!(matches!(r.verdict_for("/tmp"), RobotsVerdict::Disallowed { .. }));
    }

    /// 只取适用于我们的段——不得把别人的宽松规则套到自己头上。
    #[test]
    fn robots_only_applies_matching_agent_sections() {
        let txt = "\
User-agent: Googlebot
Disallow:

User-agent: *
Disallow: /
";
        // 我们不是 Googlebot ⇒ 应命中 `*` 段的 Disallow: /
        let r = RobotsRules::parse(txt, "ShangYi/0.1");
        assert!(matches!(
            r.verdict_for("/anything"),
            RobotsVerdict::Disallowed { .. }
        ));
    }

    #[test]
    fn robots_empty_disallow_means_allow_all() {
        let txt = "User-agent: *\nDisallow:\n";
        let r = RobotsRules::parse(txt, "ShangYi/0.1");
        assert!(matches!(r.verdict_for("/x"), RobotsVerdict::Allowed));
    }

    #[test]
    fn robots_ignores_comments_and_blank_lines() {
        let txt = "\
# 这是注释
User-agent: *    # 行尾注释
Disallow: /secret/   # 另一个注释

";
        let r = RobotsRules::parse(txt, "ShangYi/0.1");
        assert!(matches!(
            r.verdict_for("/secret/a"),
            RobotsVerdict::Disallowed { .. }
        ));
    }

    // ── 抓取行为 ──

    #[test]
    fn fetch_extracts_text_from_html() {
        let f = fetcher(vec![
            ("https://ok.test/robots.txt", 200, "text/plain", "User-agent: *\nAllow: /"),
            (
                "https://ok.test/",
                200,
                "text/html",
                "<html><head><style>p{color:red}</style><script>var x=1</script></head>\
                 <body><h1>竞对公告</h1><p>自 3 月起降价 15%</p></body></html>",
            ),
        ]);
        let page = f.fetch("https://ok.test/a").expect("应可抓取");
        assert!(page.text.contains("竞对公告"));
        assert!(page.text.contains("降价 15%"));
        // 脚本与样式必须被剔除（否则会把代码当正文喂给模型）
        assert!(!page.text.contains("var x=1"), "脚本内容不得进正文：{}", page.text);
        assert!(!page.text.contains("color:red"), "样式不得进正文：{}", page.text);
    }

    /// **被 robots.txt 禁止的路径一律不抓**，且错误要说清是哪条规则。
    #[test]
    fn fetch_refuses_disallowed_paths() {
        let f = fetcher(vec![(
            "https://no.test/robots.txt",
            200,
            "text/plain",
            "User-agent: *\nDisallow: /private/",
        )]);
        match f.fetch("https://no.test/private/x") {
            Err(FetchError::DisallowedByRobots { rule, .. }) => assert_eq!(rule, "/private/"),
            other => panic!("被禁路径必须拒绝抓取，实得 {other:?}"),
        }
    }

    /// **5xx ⇒ 完全禁止**（RFC 9309 §2.3.1.3），且错误须说明是"读不到"而非"被禁止"。
    #[test]
    fn robots_5xx_fails_closed_with_correct_reason() {
        let f = fetcher(vec![("https://x.test/robots.txt", 500, "text/plain", "oops")]);
        match f.fetch("https://x.test/a") {
            Err(FetchError::RobotsUnavailable { detail, .. }) => {
                assert!(detail.contains("500"), "应带上状态码以便诊断：{detail}");
                assert!(detail.contains("完全禁止"), "应说明按 RFC 的哪条处理：{detail}");
            }
            other => panic!("5xx 应 fail-closed，实得 {other:?}"),
        }
    }

    #[test]
    fn unavailable_robots_can_recover_without_fetching_body_before_permission() {
        struct RecoveringRobots {
            robots_calls: AtomicUsize,
            body_calls: AtomicUsize,
        }
        impl HttpTransport for RecoveringRobots {
            fn get(&self, url: &str, _ua: &str, _timeout: Duration) -> Result<(u16, String, String), String> {
                if url.ends_with("/robots.txt") {
                    if self.robots_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        return Err("io: unexpected end of file".into());
                    }
                    return Ok((200, "text/plain".into(), "User-agent: *\nAllow: /".into()));
                }
                self.body_calls.fetch_add(1, Ordering::SeqCst);
                Ok((200, "text/html".into(), "<p>取得许可后读取的正文</p>".into()))
            }
        }
        let fetcher = HttpFetcher::with_transport(RecoveringRobots {
            robots_calls: AtomicUsize::new(0), body_calls: AtomicUsize::new(0),
        }, FetchConfig { per_host_interval: Duration::ZERO, ..Default::default() });
        let error = fetcher.fetch("https://recover.test/page").unwrap_err();
        assert!(matches!(error, FetchError::RobotsUnavailable { .. }));
        assert!(error.retryable());
        assert!(error.actionable().contains("稍后重试"));
        assert!(!error.actionable().contains("重试无用"));
        assert_eq!(fetcher.transport.body_calls.load(Ordering::SeqCst), 0,
            "robots 获取失败时必须 fail-closed，不能请求正文");
        let page = fetcher.fetch("https://recover.test/page").unwrap();
        assert_eq!(page.text, "取得许可后读取的正文");
        assert_eq!(fetcher.transport.robots_calls.load(Ordering::SeqCst), 2);
        assert_eq!(fetcher.transport.body_calls.load(Ordering::SeqCst), 1);
    }

    /// **4xx ⇒ 无限制**（RFC 9309 §2.3.1.3）：站点没有 robots.txt 表示
    /// 它未施加爬取限制，**不是**"不可用"。
    ///
    /// 这是一处真实缺陷的回归测试：早先的实现把 4xx 也当"不可用"，
    /// 于是**没有 robots.txt 的站点一律抓不到**（实测 `example.com` 正是如此）。
    #[test]
    fn robots_4xx_means_no_restrictions_not_unavailable() {
        let f = fetcher(vec![
            ("https://n.test/robots.txt", 404, "text/plain", "Not Found"),
            ("https://n.test/", 200, "text/html", "<p>公开信息</p>"),
        ]);
        // 应能抓取（404 表示无限制）
        let page = f.fetch("https://n.test/a").expect("robots.txt 404 时应可抓取");
        assert!(page.text.contains("公开信息"));
        // 且判定结果是 NoRestrictions（与"有文件且允许"可区分，供审计）
        assert_eq!(
            f.check_allowed("https://n.test/a").unwrap(),
            RobotsVerdict::NoRestrictions
        );
    }

    /// 有 robots.txt 且明确允许时，判定为 `Allowed`（与 404 区分）。
    #[test]
    fn robots_present_and_allowing_is_distinguishable_from_absent() {
        let f = fetcher(vec![(
            "https://p.test/robots.txt",
            200,
            "text/plain",
            "User-agent: *\nAllow: /",
        )]);
        assert_eq!(
            f.check_allowed("https://p.test/a").unwrap(),
            RobotsVerdict::Allowed
        );
        assert!(RobotsVerdict::NoRestrictions.allows_fetch());
        assert!(!RobotsVerdict::Unavailable { detail: "x".into() }.allows_fetch());
        assert!(RobotsVerdict::Allowed.allows_fetch());
    }

    /// 遇到人机验证页 ⇒ 如实报"要求验证"，**不绕过**、也不把它当正文。
    #[test]
    fn bot_challenge_is_reported_not_bypassed() {
        let f = fetcher(vec![
            ("https://c.test/robots.txt", 200, "text/plain", "User-agent: *\nAllow: /"),
            (
                "https://c.test/",
                202,
                "text/html",
                "<html><body>Please complete the following challenge to confirm this search \
                 was made by a human. Select all squares containing a duck.</body></html>",
            ),
        ]);
        match f.fetch("https://c.test/a") {
            Err(FetchError::BotChallenge { status, .. }) => assert_eq!(status, 202),
            other => panic!("验证页必须被识别并如实上报，实得 {other:?}"),
        }
    }

    #[test]
    fn non_2xx_status_is_an_error_not_an_empty_page() {
        let f = fetcher(vec![
            ("https://s.test/robots.txt", 200, "text/plain", "User-agent: *\nAllow: /"),
            ("https://s.test/", 404, "text/html", "<html>not found</html>"),
        ]);
        match f.fetch("https://s.test/missing") {
            Err(FetchError::HttpStatus { status, .. }) => assert_eq!(status, 404),
            other => panic!("非 2xx 不得被当成空页，实得 {other:?}"),
        }
    }

    #[test]
    fn oversize_body_is_refused() {
        let big = "x".repeat(3 * 1024 * 1024);
        let f = HttpFetcher::with_transport(
            FakeHttp::new(vec![
                ("https://b.test/robots.txt", 200, "text/plain", "User-agent: *\nAllow: /"),
                ("https://b.test/", 200, "text/html", Box::leak(big.into_boxed_str())),
            ]),
            FetchConfig {
                per_host_interval: Duration::ZERO,
                max_body_bytes: 1024,
                ..Default::default()
            },
        );
        match f.fetch("https://b.test/a") {
            Err(FetchError::TooLarge { limit, .. }) => assert_eq!(limit, 1024),
            other => panic!("超大响应应被拒，实得 {other:?}"),
        }
    }

    /// robots.txt 只取一次（缓存），不每个 URL 都重取。
    #[test]
    fn robots_is_cached_per_host() {
        let f = fetcher(vec![
            ("https://k.test/robots.txt", 200, "text/plain", "User-agent: *\nAllow: /"),
            ("https://k.test/", 200, "text/html", "<p>hi</p>"),
        ]);
        f.fetch("https://k.test/1").unwrap();
        f.fetch("https://k.test/2").unwrap();
        f.fetch("https://k.test/3").unwrap();
        // 3 次页面 + 1 次 robots = 4 次调用（而不是 3 次 robots）
        assert_eq!(
            f.transport.calls.load(Ordering::SeqCst),
            4,
            "robots.txt 应被缓存，不得每页重取"
        );
    }

    #[test]
    fn invalid_url_scheme_is_rejected() {
        let f = fetcher(vec![]);
        assert!(matches!(
            f.fetch("ftp://x/y"),
            Err(FetchError::InvalidUrl(_))
        ));
        assert!(matches!(
            f.fetch("不是网址"),
            Err(FetchError::InvalidUrl(_))
        ));
    }

    #[test]
    fn url_splitting_handles_ports_and_userinfo_and_no_path() {
        assert_eq!(
            HttpFetcher::<UreqTransport>::split_url("https://a.b:8443/x/y").unwrap(),
            ("https".into(), "a.b".into(), "/x/y".into())
        );
        assert_eq!(
            HttpFetcher::<UreqTransport>::split_url("http://a.b").unwrap(),
            ("http".into(), "a.b".into(), "/".into())
        );
        // userinfo 不得被当成主机
        assert_eq!(
            HttpFetcher::<UreqTransport>::split_url("https://user:pw@a.b/p").unwrap().1,
            "a.b"
        );
    }

    #[test]
    fn plain_text_and_json_are_not_tag_stripped() {
        assert_eq!(extract_text("{\"a\":1}", "application/json"), "{\"a\":1}");
        assert_eq!(extract_text("  裸文本  ", "text/plain; charset=utf-8"), "裸文本");
    }

    /// 中文与多字节字符在抽取中不得被破坏（按字节切会切坏 UTF-8）。
    #[test]
    fn multibyte_text_survives_extraction() {
        let t = extract_text("<p>降价 15% 与「默契分层」</p>", "text/html");
        assert!(t.contains("降价 15% 与「默契分层」"), "实得：{t}");
    }
}

// ─────────────────────────── 检索（web_search）───────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchAvailability { Healthy, Degraded, Unavailable, Unknown }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EngineFailure { pub engine: String, pub reason: String }

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchBackendHealth {
    pub backend: String,
    pub responded: bool,
    pub returned_results: usize,
    pub contributing_engines: Vec<String>,
    pub unresponsive_engines: Vec<EngineFailure>,
    pub failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SearchHealth {
    pub availability: SearchAvailability,
    pub backends: Vec<SearchBackendHealth>,
    /// 融合与重排过程报告。单后端直出（解析层）时为 `None`。
    /// 它使"8 条结果从多少候选来、合并了几条重复"可审计，而不是只看到最终 8 条。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ranking: Option<RankingReport>,
}

/// 多后端融合的统计报告（不含任何结果正文，只有计数与方法标识）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RankingReport {
    pub method: String,
    pub backends_queried: Vec<String>,
    /// 各后端返回的原始候选条数之和。
    pub candidates: usize,
    /// 去重后进入排序的文档数。
    pub unique_documents: usize,
    /// 被合并的重复条目数（同链接或同标题）。
    pub duplicates_merged: usize,
    /// 因同站限位被后置的条数（名额够时会补回）。
    pub site_cap_demoted: usize,
    /// 因最终条数上限而未返回的条数。
    pub truncated: usize,
}

impl SearchHealth {
    pub fn unreported(results: usize) -> Self {
        Self { availability: SearchAvailability::Unknown, backends: vec![SearchBackendHealth {
            backend: "unspecified".into(), responded: true, returned_results: results,
            contributing_engines: vec![], unresponsive_engines: vec![], failure: None,
        }], ranking: None }
    }

    pub fn empty_result_is_conclusive(&self) -> bool {
        self.availability == SearchAvailability::Healthy
            && !self.backends.is_empty()
            && self.backends.iter().all(|backend| backend.responded && backend.failure.is_none()
                && backend.unresponsive_engines.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResponse { pub results: Vec<SearchResult>, pub health: SearchHealth }

/// 一个检索后端。
///
/// # 为什么必须有真实后端才能检索（而不是抓搜索引擎的 HTML）
///
/// 开发期曾计划用免费端点（D6 记录的 `ddgs`）。**2026-09-18 实测：该路已封**——
/// `html.duckduckgo.com` 返回 HTTP 202 与一个人机验证挑战页
/// （"Please complete the following challenge…Select all squares containing a duck"）。
///
/// 本系统**不做任何绕过**：不模拟浏览器指纹、不解验证码、不轮换代理。
/// 理由不是技术难度，而是：
/// 1. 绕过验证码违反站点条款；
/// 2. 更重要的——若检索结果来自一条"站点明确不欢迎"的通道，那么
///    "这份情报从哪来"就不可审计，而报告要能回答这个问题。
///
/// 因此：**没有配置后端就是"未取得"**（`Blocked`），不是"没找到"。
pub trait SearchBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// 检索。返回 `(标题, URL, 摘要)` 列表。
    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, FetchError>;
    fn search_with_health(&self, query: &str, limit: usize) -> Result<SearchResponse, FetchError> {
        self.search(query, limit).map(|results| {
            let mut health = SearchHealth::unreported(results.len());
            health.backends[0].backend = self.name().to_string();
            SearchResponse { results, health }
        })
    }
}

/// 一条检索结果。
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    /// 来源后端。经融合去重后为 `+` 连接的全部来源（如 `bocha+searxng`）——
    /// 一条文档被多个后端同时返回是重要的**共识证据**，不合并成单值。
    pub backend: String,
    /// 供应商返回的页面日期元数据。**未与原文核对**，只能作排序线索，
    /// 不得当事实使用（与报告里的"网页发布日期"是两回事）。
    pub published_at: Option<String>,
    /// 融合排序分（0–100）。后端解析层直出时为 `0` = 未评分。
    /// 它只表示**检索排序**，不表示事实真伪。
    pub rank_score: u32,
    /// 命中的排序信号（中文短标签，如"官方域名""年份相符"）。后端解析层为空。
    pub rank_signals: Vec<String>,
}

/// 多后端融合检索：查**所有**可用后端，按名次融合、去重、按查询意图重排。
///
/// # 为什么不是"第一个成功者胜出"
///
/// 2026-10-02 的冻结基线给出了反例：博查 4/4 健康返回，但两条查询的官方原文
/// 只在第 6 名；同一批查询在自托管 SearXNG 上第 1 名就是官方页。
/// 旧语义下 SearXNG **从未被查询**——回退策略把"另一个后端更好"这件事
/// 结构性地排除了。融合让每个后端都贡献候选，名次由 RRF 合并
/// （见 [`crate::search_rank`]），供应商之间的分数不可比，名次可比。
///
/// # 健康语义（与旧的链式回退一致，一处都不放松）
///
/// - 未配置后端 ⇒ `Err`（"未取得"），不是空结果；
/// - 全部后端失败 ⇒ 结果为空、`Unavailable`，`search()` 返回列出**每个**原因的 `Err`；
/// - 部分失败或部分引擎未响应 ⇒ 结果照常返回，健康标 `Degraded` 并保留失败后端；
/// - 全部响应且无失败但零结果 ⇒ `Healthy` 的空结果，是**结论**而非"未取得"；
/// - 有后端失败时即使零结果，`search()` 也报错——覆盖不完整时"没找到"不是结论。
pub struct FusionSearch {
    backends: Vec<Box<dyn SearchBackend>>,
    /// 每个后端请求的候选深度。大于最终条数，给重排留出空间
    /// （例如官方页在供应商的第 6 名，融合后仍能进入前 3）。
    ///
    /// 取 20 的理由：SearXNG 默认一页就返回约 20 条，取满不增加请求次数；
    /// 博查的 `count` 上限是 10，会被适配层自行夹取。
    candidate_depth: usize,
}

impl FusionSearch {
    pub fn new(backends: Vec<Box<dyn SearchBackend>>) -> Self {
        Self { backends, candidate_depth: 20 }
    }

    /// 未配置任何后端（默认状态）。
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }

    /// 设置每个后端的候选深度（至少 1）。
    pub fn with_candidate_depth(mut self, depth: usize) -> Self {
        self.candidate_depth = depth.max(1);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.backends.is_empty()
    }

    pub fn backend_names(&self) -> Vec<&'static str> {
        self.backends.iter().map(|b| b.name()).collect()
    }

    fn no_backend_error(query: &str) -> FetchError {
        FetchError::Transport {
            url: format!("search:{query}"),
            detail: "未配置任何检索后端。需要配置一个（API key 或自托管实例）后才能检索；\
                     在此之前该项情报为「未取得」，不得用推测填补。"
                .to_string(),
        }
    }

    /// 融合检索。空 `Ok` 只在**覆盖完整且确实无结果**时返回；
    /// 覆盖不完整时空结果转成 `Err`，避免把"未取得"读成"不存在"。
    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, FetchError> {
        let response = self.search_with_health(query, limit)?;
        if response.results.is_empty() && !response.health.empty_result_is_conclusive() {
            let mut reasons = Vec::new();
            for backend in &response.health.backends {
                if let Some(failure) = &backend.failure {
                    reasons.push(format!("{}: {failure}", backend.backend));
                } else if !backend.responded {
                    reasons.push(format!("{}: 未响应", backend.backend));
                } else if !backend.unresponsive_engines.is_empty() {
                    let engines = backend
                        .unresponsive_engines
                        .iter()
                        .map(|e| format!("{}（{}）", e.engine, e.reason))
                        .collect::<Vec<_>>()
                        .join("、");
                    reasons.push(format!("{}: 部分引擎未响应：{engines}", backend.backend));
                }
            }
            return Err(FetchError::Transport {
                url: format!("search:{query}"),
                detail: format!("没有可用结果，且检索覆盖不完整：{}", reasons.join("；")),
            });
        }
        Ok(response.results)
    }

    /// 查询所有后端并保留各自的健康状态与贡献计数。
    pub fn search_with_health(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<SearchResponse, FetchError> {
        if self.backends.is_empty() {
            return Err(Self::no_backend_error(query));
        }
        let plan = crate::search_rank::plan_query(query);
        let depth = self.candidate_depth.max(limit);
        let mut health = SearchHealth {
            availability: SearchAvailability::Unavailable,
            backends: Vec::new(),
            ranking: None,
        };
        let mut reports: Vec<crate::search_rank::BackendResults> = Vec::new();
        for backend in &self.backends {
            match backend.search_with_health(query, depth) {
                Ok(mut response) => {
                    health.backends.append(&mut response.health.backends);
                    reports.push(crate::search_rank::BackendResults::new(
                        backend.name(),
                        response.results,
                    ));
                }
                Err(error) => health.backends.push(SearchBackendHealth {
                    backend: backend.name().into(),
                    responded: false,
                    returned_results: 0,
                    contributing_engines: Vec::new(),
                    unresponsive_engines: Vec::new(),
                    failure: Some(error.to_string()),
                }),
            }
        }
        let any_responded = health.backends.iter().any(|b| b.responded);
        let any_incomplete = health.backends.iter().any(|b| {
            !b.responded || b.failure.is_some() || !b.unresponsive_engines.is_empty()
        });
        health.availability = if !any_responded {
            SearchAvailability::Unavailable
        } else if any_incomplete {
            SearchAvailability::Degraded
        } else {
            SearchAvailability::Healthy
        };

        let (fused, stats) = crate::search_rank::fuse(&reports, &plan, limit);
        health.ranking = Some(RankingReport {
            method: crate::search_rank::RANKING_METHOD.to_string(),
            // 用健康列表而不是融合报告：即使某后端失败，它也被查询过，
            // "查了哪些"必须如实列出（候选计数另算，只统计实际返回的）。
            backends_queried: health.backends.iter().map(|b| b.backend.clone()).collect(),
            candidates: stats.candidates,
            unique_documents: stats.unique_documents,
            duplicates_merged: stats.duplicates_merged,
            site_cap_demoted: stats.site_cap_demoted,
            truncated: stats.truncated,
        });
        let results = fused
            .into_iter()
            .map(|mut hit| {
                hit.result.backend = hit.origins.join("+");
                hit.result.rank_score = hit.score;
                hit.result.rank_signals = hit.signals;
                hit.result
            })
            .collect();
        Ok(SearchResponse { results, health })
    }
}

#[cfg(test)]
mod search_tests {
    use super::*;

    struct FakeBackend {
        name: &'static str,
        result: Result<Vec<SearchResult>, &'static str>,
    }
    impl SearchBackend for FakeBackend {
        fn name(&self) -> &'static str {
            self.name
        }
        fn search(&self, _q: &str, _l: usize) -> Result<Vec<SearchResult>, FetchError> {
            match &self.result {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(FetchError::Transport {
                    url: "x".into(),
                    detail: e.to_string(),
                }),
            }
        }
    }

    fn hit(t: &str) -> SearchResult {
        SearchResult {
            title: t.into(),
            url: format!("https://x/{t}"),
            snippet: "s".into(),
            backend: "fake".into(),
            published_at: None,
            rank_score: 0,
            rank_signals: Vec::new(),
        }
    }

    /// **未配置后端 ⇒ 报错（"未取得"），不得静默返回空列表。**
    ///
    /// 这条是承重的：若返回 `Ok(vec![])`，上游会把"检索系统没配"读成
    /// "这个查询没有结果"——那正是把"无法评估"说成"评估为负面"。
    #[test]
    fn no_backend_configured_is_an_error_not_an_empty_result() {
        let c = FusionSearch::empty();
        assert!(c.is_empty());
        let e = c.search("任何查询", 5).expect_err("未配置后端必须报错");
        let msg = e.to_string();
        assert!(msg.contains("未配置任何检索后端"), "实得：{msg}");
        assert!(msg.contains("未取得"), "应说明这是「未取得」：{msg}");
    }

    /// 融合语义：**每个**后端都被查询，结果合并——而不是第一个成功者胜出。
    #[test]
    fn every_configured_backend_contributes_candidates() {
        let c = FusionSearch::new(vec![
            Box::new(FakeBackend {
                name: "down",
                result: Err("连接超时"),
            }),
            Box::new(FakeBackend {
                name: "ok",
                result: Ok(vec![hit("A")]),
            }),
            Box::new(FakeBackend {
                name: "second",
                result: Ok(vec![hit("B")]),
            }),
        ]);
        let hits = c.search("q", 5).unwrap();
        let mut titles: Vec<&str> = hits.iter().map(|h| h.title.as_str()).collect();
        titles.sort();
        assert_eq!(titles, vec!["A", "B"]);
        assert!(
            hits.iter().all(|h| h.rank_score > 0),
            "融合后的每条结果都应带排序分：{hits:?}"
        );
    }

    #[test]
    fn health_report_retains_failed_backends_with_usable_fused_results() {
        let search = FusionSearch::new(vec![
            Box::new(FakeBackend { name: "failed", result: Err("timeout") }),
            Box::new(FakeBackend { name: "healthy", result: Ok(vec![hit("official plan")]) }),
        ]);
        let response = search.search_with_health("specific document", 8).unwrap();
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.health.availability, SearchAvailability::Degraded);
        assert_eq!(response.health.backends.len(), 2);
        assert!(!response.health.backends[0].responded);
        assert!(response.health.backends[1].responded);
        assert!(!response.health.empty_result_is_conclusive());
        let ranking = response.health.ranking.expect("融合必须给出排序报告");
        assert_eq!(ranking.method, crate::search_rank::RANKING_METHOD);
        assert_eq!(ranking.candidates, 1);
        assert_eq!(ranking.unique_documents, 1);
    }

    #[test]
    fn all_failed_backends_return_unavailable_coverage_not_a_negative_fact() {
        let search = FusionSearch::new(vec![
            Box::new(FakeBackend { name: "a", result: Err("CAPTCHA") }),
            Box::new(FakeBackend { name: "b", result: Err("429") }),
        ]);
        let response = search.search_with_health("official organizer", 8).unwrap();
        assert!(response.results.is_empty());
        assert_eq!(response.health.availability, SearchAvailability::Unavailable);
        assert_eq!(response.health.backends.len(), 2);
        assert!(!response.health.empty_result_is_conclusive());
        assert!(
            search.search("official organizer", 8).is_err(),
            "全部失败时便捷入口必须报错，不能返回空列表"
        );
    }

    /// 空结果是**结论**：所有后端都健康响应且都没有命中时，`Ok(空)`。
    #[test]
    fn conclusive_empty_result_is_ok_not_failure() {
        let search = FusionSearch::new(vec![
            Box::new(FakeBackend { name: "empty1", result: Ok(vec![]) }),
            Box::new(FakeBackend { name: "empty2", result: Ok(vec![]) }),
        ]);
        let hits = search.search("q", 5).unwrap();
        assert!(hits.is_empty(), "覆盖完整时空结果是结论");
        let response = search.search_with_health("q", 5).unwrap();
        assert_eq!(response.health.availability, SearchAvailability::Healthy);
        assert!(response.health.empty_result_is_conclusive());
    }

    /// 有后端失败时，即使另一个后端空结果也不能声称"没有"——覆盖不完整。
    #[test]
    fn incomplete_coverage_turns_an_empty_result_into_an_error() {
        let search = FusionSearch::new(vec![
            Box::new(FakeBackend { name: "empty", result: Ok(vec![]) }),
            Box::new(FakeBackend { name: "down", result: Err("429 限流") }),
        ]);
        assert!(
            search.search("q", 5).is_err(),
            "覆盖不完整时的空结果不是结论，必须报错"
        );
    }

    /// 全部失败时，错误里要列出**每一个**后端的原因（否则不知道该修哪个）。
    #[test]
    fn all_backends_failing_reports_every_reason() {
        let search = FusionSearch::new(vec![
            Box::new(FakeBackend { name: "b1", result: Err("超时") }),
            Box::new(FakeBackend { name: "b2", result: Err("401 未授权") }),
        ]);
        let msg = search.search("q", 5).unwrap_err().to_string();
        assert!(msg.contains("b1") && msg.contains("超时"), "实得：{msg}");
        assert!(msg.contains("b2") && msg.contains("401"), "实得：{msg}");
    }

    #[test]
    fn backend_names_are_reported() {
        let search = FusionSearch::new(vec![
            Box::new(FakeBackend { name: "alpha", result: Ok(vec![]) }),
            Box::new(FakeBackend { name: "beta", result: Ok(vec![]) }),
        ]);
        assert_eq!(search.backend_names(), vec!["alpha", "beta"]);
    }
}

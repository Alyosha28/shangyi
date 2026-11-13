//! 外部数据与模型提供者接线层（检索/抓取、周边 POI、群仿真 Actor）。
//!
//! 放在 `shangyi-core` 使 `shangyi-cli` 与 `shangyi-mcp` 共享完全相同的生产适配器，
//! 彻底消除「CLI 有检索/POI/群仿真、MCP 悬空」的双轨分裂。

use shangyi_intel::amap::AmapClient;
use shangyi_intel::bocha::{backends_from, backends_from_env};
use shangyi_intel::fetch::{FusionSearch, HttpFetcher, SearchHealth};
use shangyi_intel::secrets::{load_api_key, load_magpie_gateway_key};
use shangyi_llm::{Endpoint, LlmClient, Transport, UreqTransport};

use crate::domain_exec::{
    FetchedText, PoiCount, PoiProvider, PoiQuery, PoiSurvey, SearchHit, SearchProvider,
    SearchUnavailable, SwarmActorFactory,
};

/// 真实检索与网页抓取适配器（`web_search` + `web_fetch`）。
pub struct LiveSearch {
    fetcher: HttpFetcher,
    fusion: FusionSearch,
}

impl LiveSearch {
    pub fn new() -> Self {
        Self {
            fetcher: HttpFetcher::new(),
            fusion: FusionSearch::empty(),
        }
    }

    /// 按显式博查密钥装配（供单测与显式注入）。
    pub fn with_bocha_key(mut self, bocha_key: Option<String>) -> Self {
        self.fusion = FusionSearch::new(backends_from(bocha_key));
        self
    }

    /// 从环境变量或 `~/.shangyi-secrets/bocha.env` 自动装配检索后端。
    pub fn with_backends_from_env(mut self) -> Self {
        self.fusion = FusionSearch::new(backends_from_env());
        self
    }

    pub fn has_backends(&self) -> bool {
        !self.fusion.is_empty()
    }

    pub fn backend_names(&self) -> Vec<&'static str> {
        self.fusion.backend_names()
    }
}

impl Default for LiveSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchProvider for LiveSearch {
    fn search(&self, query: &str) -> Result<Vec<SearchHit>, SearchUnavailable> {
        self.search_with_health(query).map(|(hits, _)| hits)
    }

    fn search_with_health(
        &self,
        query: &str,
    ) -> Result<(Vec<SearchHit>, SearchHealth), SearchUnavailable> {
        if self.fusion.is_empty() {
            return Err(SearchUnavailable::NoBackend {
                detail: "未配置检索后端。抓取（web_fetch）可用，但检索需要商业检索 API——\
                         设 BOCHA_API_KEY（或写入 ~/.shangyi-secrets/bocha.env）后重启。\
                         在此之前该项情报为「未取得」，不得用推测填补。"
                    .into(),
            });
        }
        let response = self
            .fusion
            .search_with_health(query, 8)
            .map_err(|e| SearchUnavailable::Failed {
                detail: e.to_string(),
                retryable: e.retryable(),
            })?;
        Ok((
            response
                .results
                .into_iter()
                .map(|h| SearchHit {
                    title: h.title,
                    url: h.url,
                    snippet: h.snippet,
                    backend: h.backend,
                    published_at: h.published_at,
                    rank_score: h.rank_score,
                    rank_signals: h.rank_signals,
                })
                .collect(),
            response.health,
        ))
    }

    fn fetch(&self, url: &str) -> Result<FetchedText, SearchUnavailable> {
        match self.fetcher.fetch(url) {
            Ok(page) => Ok(FetchedText {
                url: page.final_url,
                text: page.text,
                raw_bytes: page.raw_bytes,
            }),
            Err(e) => Err(SearchUnavailable::Failed {
                detail: e.to_string(),
                retryable: e.retryable(),
            }),
        }
    }
}

/// 高德周边 POI 适配器（`poi_survey`）。
pub struct AmapPoi {
    client: AmapClient,
}

impl AmapPoi {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            client: AmapClient::new(key),
        }
    }

    /// 从环境变量 `AMAP_KEY` 或 `~/.shangyi-secrets/amap.env` 自动装配。
    pub fn from_env() -> Option<Self> {
        AmapClient::from_env().map(|client| Self { client })
    }
}

impl PoiProvider for AmapPoi {
    fn survey(&self, query: &PoiQuery) -> Result<PoiSurvey, SearchUnavailable> {
        let raw = self
            .client
            .survey(&query.address, query.radius, &query.brand, &query.keywords)
            .map_err(|failure| SearchUnavailable::Failed {
                detail: failure.detail,
                retryable: failure.retryable,
            })?;
        Ok(PoiSurvey {
            address: raw.address,
            district: raw.district,
            radius: raw.radius,
            counts: raw
                .counts
                .into_iter()
                .map(|c| PoiCount {
                    label: c.label,
                    count: c.count,
                })
                .collect(),
            brand: raw.brand.map(|b| PoiCount {
                label: b.label,
                count: b.count,
            }),
            keywords: raw.keywords.map(|k| PoiCount {
                label: k.label,
                count: k.count,
            }),
            unavailable: raw.unavailable,
            source: raw.source,
        })
    }
}

/// `swarm_run` 的 LLM Persona Actor 工厂（CLI 与 MCP 共用）。
pub struct LlmSwarmActorFactory<'a, T: Transport> {
    client: &'a LlmClient<T>,
    default_model: String,
}

impl<'a, T: Transport> LlmSwarmActorFactory<'a, T> {
    pub fn new(client: &'a LlmClient<T>, default_model: impl Into<String>) -> Self {
        Self {
            client,
            default_model: default_model.into(),
        }
    }
}

impl<'a, T: Transport + 'a> SwarmActorFactory<'a> for LlmSwarmActorFactory<'a, T> {
    fn build(
        &self,
        model: &str,
        personas: shangyi_swarm::PersonaSet,
    ) -> Box<dyn shangyi_swarm::llm_actor::TracedPersonaActor + 'a> {
        let m = if model.trim().is_empty() || model.starts_with("(未指定") {
            self.default_model.clone()
        } else {
            model.to_string()
        };
        Box::new(shangyi_swarm::llm_actor::LlmPersonaActor::new(
            self.client,
            personas,
            m,
        ))
    }
}

/// 为 `swarm_run` 自动装配默认 LLM 客户端（优先本机 Magpie Gateway，其次 DeepSeek / GLM）。
///
/// 无任何可用凭据时返回 `None`，让 `swarm_run` 如实报告 `Blocked`（绝不伪造行动）。
pub fn build_default_swarm_client() -> Option<(LlmClient<UreqTransport>, String)> {
    let explicit_provider = std::env::var("SHANGYI_PROVIDER")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty());
    let model_override = std::env::var("SHANGYI_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    match explicit_provider.as_deref() {
        Some("magpie") => {
            let key = load_magpie_gateway_key()?;
            let model =
                model_override.unwrap_or_else(|| "opencode-go/deepseek-v4.1-flash".to_string());
            Some((LlmClient::new(Endpoint::magpie_local(), UreqTransport).with_key(key), model))
        }
        Some("deepseek") => {
            let key = load_api_key("deepseek")?;
            let model = model_override.unwrap_or_else(|| "deepseek-flash".to_string());
            Some((LlmClient::new(Endpoint::deepseek(), UreqTransport).with_key(key), model))
        }
        Some("glm" | "zhipu") => {
            let key = load_api_key("glm")?;
            let model = model_override.unwrap_or_else(|| "glm-5.3-flashx".to_string());
            Some((LlmClient::new(Endpoint::glm(), UreqTransport).with_key(key), model))
        }
        Some(_) => None,
        None => {
            if let Some(key) = load_magpie_gateway_key() {
                let model = model_override
                    .unwrap_or_else(|| "opencode-go/deepseek-v4.1-flash".to_string());
                return Some((
                    LlmClient::new(Endpoint::magpie_local(), UreqTransport).with_key(key),
                    model,
                ));
            }
            if let Some(key) = load_api_key("deepseek") {
                let model = model_override.unwrap_or_else(|| "deepseek-flash".to_string());
                return Some((
                    LlmClient::new(Endpoint::deepseek(), UreqTransport).with_key(key),
                    model,
                ));
            }
            if let Some(key) = load_api_key("glm") {
                let model = model_override.unwrap_or_else(|| "glm-5.3-flashx".to_string());
                return Some((
                    LlmClient::new(Endpoint::glm(), UreqTransport).with_key(key),
                    model,
                ));
            }
            None
        }
    }
}

fn read_secret_file(path: &std::path::Path, expected_key: &str) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == expected_key {
            let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
            if !val.is_empty() {
                return Some(val);
            }
        }
    }
    None
}

fn allow_host_secrets(custom_home: Option<&std::path::Path>) -> bool {
    custom_home.is_none()
        || std::env::var("SHANGYI_ENABLE_HOST_SECRETS")
            .map(|v| v.trim() == "1")
            .unwrap_or(false)
}

/// 为 MCP 宿主装配检索后端：显式 `--home` 隔离测试默认不读取宿主机 `~/.shangyi-secrets/`，
/// 生产环境（`custom_home == None`）或显式设置环境变量/私有 secrets 时自动装配。
pub fn search_for_mcp(custom_home: Option<&std::path::Path>) -> LiveSearch {
    if allow_host_secrets(custom_home) {
        return LiveSearch::new().with_backends_from_env();
    }
    let env_key = std::env::var("BOCHA_API_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            custom_home.and_then(|h| read_secret_file(&h.join("secrets").join("bocha.env"), "BOCHA_API_KEY"))
        });
    LiveSearch::new().with_bocha_key(env_key)
}

/// 为 MCP 宿主装配高德周边 POI 数据源。
pub fn poi_for_mcp(custom_home: Option<&std::path::Path>) -> Option<AmapPoi> {
    if allow_host_secrets(custom_home) {
        return AmapPoi::from_env();
    }
    let env_key = std::env::var("AMAP_KEY")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            custom_home.and_then(|h| read_secret_file(&h.join("secrets").join("amap.env"), "AMAP_KEY"))
        });
    env_key.map(AmapPoi::new)
}

/// 为进程级宿主（如 `shangyi-mcp`）装配 `'static` 生命周期的群仿真 Actor 工厂。
pub fn swarm_for_mcp(
    custom_home: Option<&std::path::Path>,
) -> Option<Box<dyn SwarmActorFactory<'static>>> {
    if !allow_host_secrets(custom_home) {
        let has_explicit_env = ["MAGPIE_GATEWAY_KEY", "DEEPSEEK_API_KEY", "GLM_API_KEY"]
            .iter()
            .any(|k| {
                std::env::var(k)
                    .map(|v| !v.trim().is_empty())
                    .unwrap_or(false)
            });
        if !has_explicit_env {
            return None;
        }
    }
    let (client, model) = build_default_swarm_client()?;
    let client: &'static LlmClient<UreqTransport> = Box::leak(Box::new(client));
    Some(Box::new(LlmSwarmActorFactory::new(client, model)))
}

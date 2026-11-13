//! 博查（Bocha）Web Search API 请求构造、响应解析与 [`SearchBackend`] 实现。
//!
//! 下沉至 `shangyi-intel` 使 `shangyi-cli` 与 `shangyi-mcp` 共用同一份检索后端，
//! 且把异常 JSON 与空搜索结果严格区分开。

use serde_json::Value;

use crate::fetch::{
    EngineFailure, FetchError, SearchAvailability, SearchBackend, SearchBackendHealth,
    SearchHealth, SearchResponse, SearchResult,
};
use crate::search_rank::plan_query;
use crate::secrets::load_api_key;

/// Canonical host in Bocha's detailed Web Search API reference.
pub const ENDPOINT: &str = "https://api.bocha.cn/v1/web-search";

/// 按给定凭据装配检索后端。**纯函数**：不读环境、不发网络，故"装配了哪些后端"可单测。
pub fn backends_from(bocha_key: Option<String>) -> Vec<Box<dyn SearchBackend>> {
    let mut backends: Vec<Box<dyn SearchBackend>> = Vec::new();
    if let Some(api_key) = bocha_key.filter(|k| !k.trim().is_empty()) {
        backends.push(Box::new(BochaSearchBackend::new(api_key)));
    }
    backends
}

/// 从环境变量或 `~/.shangyi-secrets/bocha.env` 装配检索后端。
pub fn backends_from_env() -> Vec<Box<dyn SearchBackend>> {
    backends_from(load_api_key("bocha"))
}

/// Bocha Web Search API. A search call returns candidate evidence only;
/// actual-page verification still uses the original URL and robots-aware fetcher.
pub struct BochaSearchBackend {
    api_key: String,
    client: ureq::Agent,
}

impl BochaSearchBackend {
    pub fn new(api_key: impl Into<String>) -> Self {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(8)))
            .build();
        Self {
            api_key: api_key.into(),
            client: ureq::Agent::new_with_config(config),
        }
    }

    fn failure(error: ureq::Error) -> FetchError {
        let detail = match error {
            ureq::Error::StatusCode(status) => {
                format!("博查接口返回 HTTP {status}；检查密钥、账户额度或请求参数")
            }
            ureq::Error::Timeout(_) => "博查搜索请求超时；是否计费无法从超时信息确认".into(),
            _ => "博查搜索请求失败；是否计费无法从网络错误确认".into(),
        };
        FetchError::InvalidResponse {
            url: "博查 Search API".into(),
            detail: format!("{detail}；本次付费请求不自动重试，改用其他可用来源"),
        }
    }
}

impl SearchBackend for BochaSearchBackend {
    fn name(&self) -> &'static str {
        "bocha"
    }

    fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>, FetchError> {
        self.search_with_health(query, limit)
            .map(|response| response.results)
    }

    fn search_with_health(&self, query: &str, limit: usize) -> Result<SearchResponse, FetchError> {
        let body = request_body(query, limit);
        let mut response = self
            .client
            .post(ENDPOINT)
            .header("Authorization", &format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .send_json(body)
            .map_err(Self::failure)?;
        let response_text = response
            .body_mut()
            .with_config()
            .limit(1024 * 1024)
            .read_to_string()
            .map_err(|_| FetchError::InvalidResponse {
                url: "博查 Search API".into(),
                detail: "响应读取失败或超过1MiB上限；不作为空搜索结果".into(),
            })?;
        let value: Value =
            serde_json::from_str(&response_text).map_err(|_| FetchError::InvalidResponse {
                url: "博查 Search API".into(),
                detail: "没有返回可识别的 JSON 搜索结果；不自动重试".into(),
            })?;
        parse_response(&value, limit.clamp(1, 10))
    }
}

/// 按查询意图构造博查请求体。
pub fn request_body(query: &str, limit: usize) -> Value {
    let plan = plan_query(query);
    let mut body = serde_json::json!({
        "query": plan.keywords,
        "freshness": plan.recency.bocha_freshness(),
        "summary": true,
        "count": limit.clamp(1, 10),
    });
    if let Some(include) = plan.bocha_include() {
        body["include"] = Value::String(include);
    }
    if let Some(exclude) = plan.bocha_exclude() {
        body["exclude"] = Value::String(exclude);
    }
    body
}

fn invalid_response(detail: &str) -> FetchError {
    FetchError::InvalidResponse {
        url: "博查 Search API".into(),
        detail: detail.into(),
    }
}

fn safe_code(value: &Value) -> Option<String> {
    let code = match value {
        Value::String(code) => code.clone(),
        Value::Number(code) => code.to_string(),
        _ => return None,
    };
    if !code.is_empty()
        && code.len() <= 40
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        Some(code)
    } else {
        None
    }
}

fn provider_error_code(value: &Value) -> Option<String> {
    [
        value.get("code"),
        value.get("error_code"),
        value.get("error").and_then(|error| error.get("code")),
    ]
    .into_iter()
    .flatten()
    .find_map(safe_code)
}

fn search_payload(value: &Value) -> &Value {
    if value.get("webPages").is_some() {
        value
    } else if value
        .get("data")
        .is_some_and(|data| data.get("webPages").is_some())
    {
        value.get("data").expect("checked above")
    } else {
        value
    }
}

pub fn parse_response(value: &Value, limit: usize) -> Result<SearchResponse, FetchError> {
    let payload = search_payload(value);
    let pages = payload
        .get("webPages")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            let has_error_envelope = ["error", "message", "msg", "code", "success"]
                .iter()
                .any(|key| value.get(*key).is_some() || payload.get(*key).is_some());
            if has_error_envelope {
                let code = provider_error_code(value)
                    .map(|code| format!("供应商错误码 {code}；"))
                    .unwrap_or_default();
                invalid_response(&format!(
                    "博查接口返回错误结构；{code}检查密钥、额度或参数；不自动重试"
                ))
            } else {
                invalid_response("缺少文档约定的 webPages 对象；不是有效的空搜索结果")
            }
        })?;
    let rows = pages
        .get("value")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            invalid_response("缺少文档约定的 webPages.value 数组；不是有效的空搜索结果")
        })?;
    let mut invalid = 0;
    let results = rows
        .iter()
        .filter_map(|row| {
            let url = row
                .get("url")
                .and_then(Value::as_str)
                .filter(|s| safe_http_url(s));
            let Some(url) = url else {
                invalid += 1;
                return None;
            };
            let title = row
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(url);
            let snippet = row
                .get("summary")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .or_else(|| row.get("snippet").and_then(Value::as_str))
                .unwrap_or("")
                .to_string();
            let published_at = row
                .get("datePublished")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string());
            Some(SearchResult {
                title: title.to_string(),
                url: url.to_string(),
                snippet,
                backend: "bocha-web-search".into(),
                published_at,
                rank_score: 0,
                rank_signals: Vec::new(),
            })
        })
        .take(limit)
        .collect::<Vec<_>>();
    let mut failures = Vec::new();
    if invalid > 0 {
        failures.push(EngineFailure {
            engine: "bocha-web-search".into(),
            reason: format!("{invalid} 条结果的网址缺失或不安全，已跳过"),
        });
    }
    Ok(SearchResponse {
        results,
        health: SearchHealth {
            availability: if failures.is_empty() {
                SearchAvailability::Healthy
            } else {
                SearchAvailability::Degraded
            },
            backends: vec![SearchBackendHealth {
                backend: "bocha".into(),
                responded: true,
                returned_results: rows.len(),
                contributing_engines: vec!["bocha-web-search".into()],
                unresponsive_engines: failures,
                failure: None,
            }],
            ranking: None,
        },
    })
}

fn safe_http_url(value: &str) -> bool {
    if value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty() && !authority.contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn backend_assembly_is_bocha_only_and_never_falls_back_silently() {
        let none = backends_from(None);
        assert!(none.is_empty(), "没有凭据时不得装配任何后端");
        let some = backends_from(Some("sk-test-not-used".into()));
        assert_eq!(some.len(), 1, "有凭据时只装配一个后端");
        assert_eq!(some[0].name(), "bocha");
    }

    #[test]
    fn reads_real_bocha_field_names_and_keeps_published_date_as_unverified_metadata() {
        let response = parse_response(
            &json!({
                "_type":"SearchResponse",
                "webPages":{"totalEstimatedMatches":1,"value":[{
                    "name":"官方页面标题","url":"https://example.gov.cn/notice",
                    "snippet":"原文相关短摘","summary":"供应商生成的搜索摘要",
                    "siteName":"政府网站","datePublished":"2025-04-24"
                }]}
            }),
            8,
        )
        .unwrap();
        assert_eq!(response.health.availability, SearchAvailability::Healthy);
        assert_eq!(response.health.backends[0].backend, "bocha");
        assert_eq!(response.health.backends[0].returned_results, 1);
        assert_eq!(response.results[0].title, "官方页面标题");
        assert_eq!(response.results[0].url, "https://example.gov.cn/notice");
        assert_eq!(response.results[0].snippet, "供应商生成的搜索摘要");
        assert_eq!(
            response.results[0].published_at.as_deref(),
            Some("2025-04-24"),
            "供应商日期进独立字段，不混进摘要正文"
        );
    }

    #[test]
    fn unwraps_the_search_response_from_the_top_level_data_envelope() {
        let response = parse_response(
            &json!({
                "code": 200,
                "msg": "success",
                "data": {
                    "_type": "SearchResponse",
                    "queryContext": {"originalQuery": "官方搜索文档"},
                    "webPages": {"totalEstimatedMatches": 1, "value": [{
                        "name": "官方接口文档",
                        "url": "https://open.bochaai.com/",
                        "snippet": "官方字段说明",
                        "summary": "搜索摘要"
                    }]}
                }
            }),
            8,
        )
        .unwrap();
        assert_eq!(response.health.availability, SearchAvailability::Healthy);
        assert_eq!(response.health.backends[0].backend, "bocha");
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].title, "官方接口文档");
        assert_eq!(response.results[0].backend, "bocha-web-search");
    }

    #[test]
    fn an_empty_webpages_list_is_a_healthy_query_but_a_missing_list_is_not() {
        let empty = parse_response(&json!({"webPages":{"value":[]}}), 8).unwrap();
        assert_eq!(empty.health.availability, SearchAvailability::Healthy);
        assert!(empty.results.is_empty());
        assert!(parse_response(&json!({"error":"Unauthorized"}), 8).is_err());
        assert!(parse_response(&json!({"webPages":{}}), 8).is_err());
    }

    #[test]
    fn provider_error_envelopes_are_permanent_and_never_misreported_as_empty() {
        let error = parse_response(
            &json!({"code":"quota_error","message":"details omitted from logs","webPages":null}),
            8,
        )
        .unwrap_err();
        assert!(matches!(error, FetchError::InvalidResponse { .. }));
        assert!(!error.retryable());
        assert!(error.to_string().contains("quota_error"));
        assert!(error.actionable().contains("不要原样循环重试"));
        assert!(!error.to_string().contains("details omitted"));
    }

    #[test]
    fn unsafe_or_unavailable_result_urls_are_counted_degraded_not_silently_accepted() {
        let result = parse_response(
            &json!({"webPages":{"value":[
                {"name":"安全","url":"https://example.gov.cn/path","snippet":"ok"},
                {"name":"脚本","url":"javascript:alert(1)"},
                {"name":"凭据","url":"https://user:password@example.com"},
                {"name":"缺网址","snippet":"no URL"}
            ]}}),
            8,
        )
        .unwrap();
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.health.availability, SearchAvailability::Degraded);
        assert_eq!(result.health.backends[0].returned_results, 4);
        assert_eq!(
            result.health.backends[0].unresponsive_engines[0].engine,
            "bocha-web-search"
        );
    }

    #[test]
    fn site_filters_become_validated_bocha_include_domains() {
        let body = request_body("咖啡文化节 site:shanghai.gov.cn", 8);
        assert_eq!(body["query"], "咖啡文化节");
        assert_eq!(body["include"], "shanghai.gov.cn");
        assert_eq!(body["count"], 8);

        let multiple = request_body(
            "site:shanghai.gov.cn 北外滩 site:www.sh.gov.cn 咖啡文化节",
            50,
        );
        assert_eq!(multiple["query"], "北外滩 咖啡文化节");
        assert_eq!(multiple["include"], "shanghai.gov.cn|www.sh.gov.cn");
        assert_eq!(multiple["count"], 10);
    }

    #[test]
    fn exclude_filters_and_recency_words_reach_the_provider_parameters() {
        let body = request_body("-site:example.com 社区生鲜 本周 价格", 8);
        assert_eq!(body["query"], "社区生鲜 本周 价格");
        assert_eq!(body["exclude"], "example.com");
        assert_eq!(body["freshness"], "oneWeek");
        assert!(body.get("include").is_none());

        let historical = request_body("2025 上海国际咖啡文化节 举办时间", 8);
        assert_eq!(historical["freshness"], "noLimit");
    }

    #[test]
    fn malformed_or_query_only_site_filters_are_not_sent_as_include() {
        let malformed = request_body("site:shanghai.gov.cn/path 文化节", 8);
        assert!(malformed.get("include").is_none());
        assert_eq!(malformed["query"], "site:shanghai.gov.cn/path 文化节");

        let query_only = request_body("site:shanghai.gov.cn", 8);
        assert!(query_only.get("include").is_none());
        assert_eq!(query_only["query"], "site:shanghai.gov.cn");
    }
}

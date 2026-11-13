//! 高德周边 POI 计数客户端（`poi_survey` 的底层实现）。
//!
//! 只回**计数**，不回门店明细：工具输出会进事件日志（长期保存），而地图服务
//! 条款限制 POI 明细的长期存储与再分发。故这里解析完计数就把响应丢掉。

use crate::secrets::load_amap_key;

const AMAP_GEOCODE: &str = "https://restapi.amap.com/v3/geocode/geo";
const AMAP_AROUND: &str = "https://restapi.amap.com/v3/place/around";

/// 类目 → 高德类型码。与 sidecar 的 `amap_poi.py` 同一套口径（同一批事实）。
pub const AMAP_CATEGORIES: &[(&str, &str)] = &[
    ("茶饮咖啡", "050700|050500|050900"), // 冷饮店 | 咖啡厅 | 甜品店
    ("餐饮", "050000"),
    ("商场", "060100"),
    ("超市便利", "060400|060200"),
    ("写字楼", "120201"),
    ("住宅区", "120300"),
    ("地铁站", "150500"),
    ("公交站", "150700"),
];

/// 免费 key 的 QPS 很低（实测并发立刻 `CUQPS_HAS_EXCEEDED_THE_LIMIT`），
/// 故类目之间必须留间隔；这比"并发打满再逐个失败"诚实也更快。
const AMAP_CALL_INTERVAL_MS: u64 = 350;

/// 一次高德调用的失败。`retryable` 决定要不要退避重试（限流可重试，密钥错不重试）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmapFailure {
    pub detail: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmapCategoryCount {
    pub label: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AmapSurveyResult {
    pub address: String,
    pub district: String,
    pub radius: u32,
    pub counts: Vec<AmapCategoryCount>,
    pub brand: Option<AmapCategoryCount>,
    pub keywords: Option<AmapCategoryCount>,
    pub unavailable: Vec<String>,
    pub source: String,
}

/// 高德周边查询。地址解析 + 逐类目计数，全部串行。
pub struct AmapClient {
    key: String,
    client: ureq::Agent,
}

impl AmapClient {
    pub fn new(key: impl Into<String>) -> Self {
        let config = ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build();
        Self {
            key: key.into(),
            client: ureq::Agent::new_with_config(config),
        }
    }

    /// 从环境或 `~/.shangyi-secrets/amap.env` 装配。没有 key ⇒ `None`。
    pub fn from_env() -> Option<Self> {
        load_amap_key().map(Self::new)
    }

    fn call(&self, url: &str, params: &[(&str, &str)]) -> Result<serde_json::Value, AmapFailure> {
        let mut last = AmapFailure {
            detail: "高德未返回结果".into(),
            retryable: false,
        };
        for attempt in 0..2 {
            let mut request = self.client.get(url).query("key", self.key.as_str());
            for (name, value) in params {
                request = request.query(*name, *value);
            }
            match request.call() {
                Ok(mut response) => {
                    let text = match response
                        .body_mut()
                        .with_config()
                        .limit(1024 * 1024)
                        .read_to_string()
                    {
                        Ok(text) => text,
                        Err(_) => {
                            return Err(AmapFailure {
                                detail: "响应读取失败或超过 1MiB".into(),
                                retryable: true,
                            })
                        }
                    };
                    let value: serde_json::Value = match serde_json::from_str(&text) {
                        Ok(value) => value,
                        Err(_) => {
                            return Err(AmapFailure {
                                detail: "返回不是可识别的 JSON".into(),
                                retryable: false,
                            })
                        }
                    };
                    if value.get("status").and_then(|s| s.as_str()) == Some("1") {
                        return Ok(value);
                    }
                    let info = value
                        .get("info")
                        .and_then(|i| i.as_str())
                        .unwrap_or("未知错误")
                        .to_string();
                    let retryable = info.starts_with("CUQPS")
                        || info.to_uppercase().contains("LIMIT")
                        || info.to_uppercase().contains("BUSY");
                    let done = !retryable;
                    last = AmapFailure {
                        detail: info,
                        retryable,
                    };
                    if done {
                        break;
                    }
                }
                Err(error) => {
                    last = AmapFailure {
                        detail: format!("请求失败：{error}"),
                        retryable: true,
                    };
                }
            }
            if attempt == 0 {
                std::thread::sleep(std::time::Duration::from_millis(1200));
            }
        }
        Err(last)
    }

    fn count_of(
        &self,
        location: &str,
        radius: u32,
        extra: &[(&str, &str)],
    ) -> Result<u64, AmapFailure> {
        let radius = radius.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("location", location),
            ("radius", radius.as_str()),
            ("offset", "25"),
            ("page", "1"),
            ("extensions", "base"),
        ];
        params.extend_from_slice(extra);
        let value = self.call(AMAP_AROUND, &params)?;
        value
            .get("count")
            .and_then(|c| c.as_str())
            .and_then(|c| c.parse::<u64>().ok())
            .ok_or(AmapFailure {
                detail: "返回里没有可用计数".into(),
                retryable: false,
            })
    }

    fn pause(&self, index: usize) {
        if index > 0 {
            std::thread::sleep(std::time::Duration::from_millis(AMAP_CALL_INTERVAL_MS));
        }
    }

    /// 执行地址解析 + 周边分类计数。
    pub fn survey(
        &self,
        raw_address: &str,
        raw_radius: u32,
        raw_brand: &str,
        raw_keywords: &str,
    ) -> Result<AmapSurveyResult, AmapFailure> {
        let radius = raw_radius.clamp(100, 3000);
        let geo = self
            .call(AMAP_GEOCODE, &[("address", raw_address)])
            .map_err(|failure| AmapFailure {
                detail: format!("地址解析失败：{}", failure.detail),
                retryable: failure.retryable,
            })?;
        let entry = geo
            .get("geocodes")
            .and_then(|g| g.as_array())
            .and_then(|entries| entries.first())
            .cloned();
        let Some(entry) = entry else {
            return Err(AmapFailure {
                detail: format!("这个地址解析不出坐标：{raw_address}"),
                retryable: false,
            });
        };
        let location = entry
            .get("location")
            .and_then(|l| l.as_str())
            .unwrap_or_default()
            .to_string();
        if !location.contains(',') {
            return Err(AmapFailure {
                detail: format!("坐标格式异常：{location}"),
                retryable: false,
            });
        }
        let district = entry
            .get("district")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        let address = entry
            .get("formatted_address")
            .and_then(|a| a.as_str())
            .unwrap_or(raw_address)
            .to_string();

        let mut counts = Vec::new();
        let mut unavailable = Vec::new();
        for (index, (label, types)) in AMAP_CATEGORIES.iter().enumerate() {
            self.pause(index);
            match self.count_of(&location, radius, &[("types", types)]) {
                Ok(count) => counts.push(AmapCategoryCount {
                    label: (*label).to_string(),
                    count,
                }),
                Err(failure) => unavailable.push(format!("{label}：{}", failure.detail)),
            }
        }
        let mut extra_count = |index: usize, label: String, extra: &[(&str, &str)]| {
            self.pause(index);
            match self.count_of(&location, radius, extra) {
                Ok(count) => Some(AmapCategoryCount { label, count }),
                Err(failure) => {
                    unavailable.push(format!("{label}：{}", failure.detail));
                    None
                }
            }
        };
        let mut index = AMAP_CATEGORIES.len();
        let brand = if raw_brand.trim().is_empty() {
            None
        } else {
            index += 1;
            extra_count(
                index - 1,
                format!("{}（同品牌）", raw_brand.trim()),
                &[("keywords", raw_brand.trim())],
            )
        };
        let keywords = if raw_keywords.trim().is_empty() {
            None
        } else {
            extra_count(
                index,
                format!("{}（关键词）", raw_keywords.trim()),
                &[("keywords", raw_keywords.trim())],
            )
        };
        Ok(AmapSurveyResult {
            address,
            district,
            radius,
            counts,
            brand,
            keywords,
            unavailable,
            source: "高德开放平台 Web 服务（POI 周边搜索）".into(),
        })
    }
}

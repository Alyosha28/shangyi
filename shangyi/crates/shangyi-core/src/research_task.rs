//! Requested-field ledger and bounded document recovery. Text matching proves
//! provenance and scope anchors, not the truth of a model's interpretation.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use shangyi_intel::fetch::SearchHealth;
use std::collections::{BTreeMap, BTreeSet};

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} 必须是非空文字"))
}
fn compact(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace()).collect()
}
fn title_key(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).collect()
}
fn same_title(a: &str, b: &str) -> bool {
    let (a, b) = (title_key(a), title_key(b));
    a.chars().count() >= 8 && b.chars().count() >= 8 && (a.contains(&b) || b.contains(&a))
}
fn domain(url: &str) -> Option<String> {
    let tail = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let authority = tail.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(
        authority
            .split(':')
            .next()?
            .trim_start_matches("www.")
            .to_lowercase(),
    )
}
fn document_title(title: &str) -> bool {
    [
        "方案",
        "通知",
        "办法",
        "公告",
        "报告",
        "指南",
        "Plan",
        "Report",
        "Guideline",
    ]
    .iter()
    .any(|word| title.contains(word))
}

fn labeled_value(quote: &str, value: &str, labels: &[&str]) -> bool {
    let quote = compact(quote);
    let value = compact(value);
    labels.iter().any(|label| {
        quote.match_indices(*label).any(|(at, _)| {
            quote[at + label.len()..]
                .trim_start_matches([':', '：', '－', '-'])
                .starts_with(&value)
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRequired {
    OriginalText,
    PublicationTime,
    EventDate,
    DocumentIssueDate,
    DocumentSignatureDate,
    SourceLink,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldStatus {
    Pending,
    Supported,
    Unavailable,
    NotApplicable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchEvidence {
    pub value: String,
    pub source_url: String,
    pub quote: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_label: Option<String>,
}
/// 结构化查询回执：**由工具产生**，模型只能引用它的编号，不能改写其中的值。
///
/// # 为什么它与原文引文分开（而不是混进 `ResearchEvidence`）
///
/// 两者的证据等级不同：原文引文是"某篇文章里逐字这么说"，回执是"某次按坐标
/// 与半径的结构化查询返回了这个计数"。混成一类，等于让结构化计数借原文的
/// 可信度——而它没有原文可核；反过来，把回执当成"未核实"又抹掉了它确实
/// 可复现、有来源、有口径的事实性。
///
/// 防伪造靠**结构**而不是靠提示词：`value`/`source`/`scope` 全部取自回执本体，
/// 模型在上报时只能给回执编号，写不出一个不存在的数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StructuredEvidence {
    /// 回执编号（工具返回，模型逐字引用）。
    pub receipt: String,
    /// 取数方式（如「高德 POI 周边计数」）。
    pub kind: String,
    /// 口径（查询对象 + 半径，如「上海市黄浦区南京东路 · 500 米」）。
    pub scope: String,
    /// 取到的值（如「144 家」）。
    pub value: String,
    /// 数据来源（如「高德开放平台 Web 服务（POI 周边搜索）」）。
    pub source: String,
}

/// 本次会话内由工具产生的结构化回执：编号 → 回执。
pub type StructuredReceipts = std::collections::BTreeMap<String, StructuredEvidence>;

#[derive(Debug, Clone, Serialize)]
pub struct ResearchField {
    pub id: String,
    pub label: String,
    pub subject: Option<String>,
    pub request_quote: String,
    pub evidence_required: EvidenceRequired,
    pub minimum_findings: usize,
    pub status: FieldStatus,
    pub evidence: Vec<ResearchEvidence>,
    /// 结构化查询依据（与原文引文并列，来源类型不同）。
    #[serde(default)]
    pub structured_evidence: Vec<StructuredEvidence>,
    pub reason: Option<String>,
    pub gap_source_url: Option<String>,
    pub applicability_evidence: Option<ApplicabilityEvidence>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ApplicabilityEvidence {
    pub source_kind: String,
    pub source_url: String,
    pub quote: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResearchBudget {
    pub used: usize,
    pub limit: usize,
    pub remaining: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResearchSearchReceipt {
    pub query: String,
    pub health: SearchHealth,
    pub returned_hits: usize,
    pub budget: ResearchBudget,
}
#[derive(Debug, Clone, Serialize)]
pub struct DocumentLookup {
    pub query: String,
    pub document_title: String,
    pub site_domain: String,
    pub field_ids: Vec<String>,
    pub candidate_urls: Vec<String>,
    pub attempted: bool,
    pub candidate_fetch_receipts: Vec<CandidateFetchReceipt>,
    pub resolution: Option<LookupResolution>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CandidateFetchReceipt {
    pub requested_url: String,
    pub returned_url: Option<String>,
    pub body_obtained: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupResolution {
    CandidateBodyObtained,
    CandidateUnavailable,
    NoMatchingCandidate,
    SearchCoverageUnavailable,
}
#[derive(Debug, Clone, Serialize)]
pub struct ResearchDelivery {
    pub missing: Vec<String>,
    pub fields: Vec<ResearchField>,
    pub source_urls: Vec<String>,
    pub health: Vec<ResearchSearchReceipt>,
    pub budget: ResearchBudget,
}
#[derive(Debug, Clone)]
pub struct ResearchTask {
    fields: BTreeMap<String, ResearchField>,
    order: Vec<String>,
    hits: BTreeMap<String, String>,
    successful_urls: BTreeSet<String>,
    failed_urls: BTreeSet<String>,
    fetch_attempts: BTreeSet<String>,
    searches: Vec<ResearchSearchReceipt>,
    lookups: BTreeMap<String, DocumentLookup>,
    budget: ResearchBudget,
}

impl ResearchTask {
    pub fn plan(args: &Value, request_text: &str) -> Result<Self, String> {
        if request_text.trim().is_empty() {
            return Err("引擎尚未保存本任务用户原文，不能用模型自行填写的理由替代授权".into());
        }
        let fields = args
            .get("fields")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty() && v.len() <= 16)
            .ok_or("fields须含1到16个用户实际请求字段")?;
        let mut result = Self {
            fields: BTreeMap::new(),
            order: vec![],
            hits: BTreeMap::new(),
            successful_urls: BTreeSet::new(),
            failed_urls: BTreeSet::new(),
            fetch_attempts: BTreeSet::new(),
            searches: vec![],
            lookups: BTreeMap::new(),
            budget: ResearchBudget {
                used: 0,
                limit: 16,
                remaining: 16,
            },
        };
        for field in fields {
            let id = text(field, "id")?;
            let label = text(field, "label")?;
            let quote = text(field, "request_quote")?;
            if id.len() > 64
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                || result.fields.contains_key(id)
            {
                return Err("字段id须唯一，使用简短字母数字下划线".into());
            }
            if !compact(request_text).contains(&compact(quote))
                || !compact(quote).contains(&compact(label))
            {
                return Err(format!(
                    "字段{label}缺少用户原文锚点；不要加入用户没问的承办、指导、门牌等字段"
                ));
            }
            let subject = field
                .get("subject")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty());
            if subject.is_some_and(|s| !compact(request_text).contains(&compact(s))) {
                return Err("subject必须来自本任务用户原文，不得扩大对象".into());
            }
            let required: EvidenceRequired = serde_json::from_value(
                field
                    .get("evidence_required")
                    .cloned()
                    .ok_or("缺少evidence_required")?,
            )
            .map_err(|_| "evidence_required无效")?;
            let minimum = match field.get("minimum_findings") {
                None => 1,
                Some(value) => value
                    .as_u64()
                    .filter(|n| (1..=5).contains(n))
                    .ok_or("minimum_findings须为1到5的整数")?
                    as usize,
            };
            result.order.push(id.to_string());
            result.fields.insert(
                id.to_string(),
                ResearchField {
                    id: id.into(),
                    label: label.into(),
                    subject: subject.map(String::from),
                    request_quote: quote.into(),
                    evidence_required: required,
                    minimum_findings: minimum,
                    status: FieldStatus::Pending,
                    evidence: vec![],
                    structured_evidence: vec![],
                    reason: None,
                    gap_source_url: None,
                    applicability_evidence: None,
                },
            );
        }
        Ok(result)
    }

    pub fn observe_search(
        &mut self,
        query: &str,
        hits: &[(String, String)],
        health: &SearchHealth,
        used: usize,
        limit: usize,
    ) {
        self.budget = ResearchBudget {
            used,
            limit,
            remaining: limit.saturating_sub(used),
        };
        for (title, url) in hits {
            if domain(url).is_some() {
                self.hits.insert(url.clone(), title.clone());
            }
        }
        self.searches.push(ResearchSearchReceipt {
            query: query.into(),
            health: health.clone(),
            returned_hits: hits.len(),
            budget: self.budget.clone(),
        });
        if let Some(lookup) = self.lookups.get_mut(query) {
            lookup.attempted = true;
            lookup.candidate_urls = hits
                .iter()
                .filter(|(title, url)| {
                    same_title(title, &lookup.document_title)
                        && domain(url).as_deref() == Some(lookup.site_domain.as_str())
                })
                .map(|(_, url)| url.clone())
                .collect();
            lookup.candidate_fetch_receipts.clear();
            lookup.resolution = None;
        }
    }

    pub fn observe_fetch(&mut self, requested: &str, actual: Option<&str>, success: bool) {
        if domain(requested).is_none() {
            return;
        }
        self.fetch_attempts.insert(requested.into());
        for lookup in self.lookups.values_mut() {
            if lookup.attempted && lookup.candidate_urls.iter().any(|url| url == requested) {
                lookup.candidate_fetch_receipts.push(CandidateFetchReceipt {
                    requested_url: requested.into(),
                    returned_url: actual.map(String::from),
                    body_obtained: success,
                });
            }
        }
        if success {
            self.failed_urls.remove(requested);
            let url = actual
                .filter(|url| domain(url).is_some())
                .unwrap_or(requested);
            self.successful_urls.insert(url.into());
        } else {
            self.failed_urls.insert(requested.into());
        }
    }

    pub fn observe_search_failure(&mut self, query: &str, detail: &str, used: usize, limit: usize) {
        let health = SearchHealth {
            availability: shangyi_intel::fetch::SearchAvailability::Unavailable,
            backends: vec![shangyi_intel::fetch::SearchBackendHealth {
                backend: "search_provider".into(),
                responded: false,
                returned_results: 0,
                contributing_engines: vec![],
                unresponsive_engines: vec![],
                failure: Some(detail.chars().take(500).collect()),
            }],
            ranking: None,
        };
        self.observe_search(query, &[], &health, used, limit);
    }

    pub fn set_budget(&mut self, used: usize, limit: usize) {
        self.budget = ResearchBudget {
            used,
            limit,
            remaining: limit.saturating_sub(used),
        };
    }

    pub fn update(
        &mut self,
        args: &Value,
        bodies: &BTreeMap<String, String>,
    ) -> Result<Value, String> {
        self.update_with_receipts(args, bodies, &StructuredReceipts::new())
    }

    /// 与 [`Self::update`] 相同，但额外接受**结构化查询回执**作为依据。
    ///
    /// 回执由工具（如 `poi_survey`）产生并登记在执行器状态里；模型只能给编号，
    /// 值/来源/口径一律从回执本体取——这是"结构化证据不可伪造"的落点。
    pub fn update_with_receipts(
        &mut self,
        args: &Value,
        bodies: &BTreeMap<String, String>,
        receipts: &StructuredReceipts,
    ) -> Result<Value, String> {
        let updates = args
            .get("updates")
            .and_then(Value::as_array)
            .filter(|v| !v.is_empty())
            .ok_or("updates不能为空")?;
        let mut next = self.fields.clone();
        for update in updates {
            let id = text(update, "field_id")?;
            let field = next
                .get_mut(id)
                .ok_or_else(|| format!("字段{id}不在用户请求中；不能新增无关缺口"))?;
            let status = text(update, "status")?;
            match status {
                "supported" => {
                    if update.get("evidence").is_none() && update.get("structured").is_none() {
                        return Err(
                            "supported必须提供evidence（原文引文）或structured（结构化查询回执编号）"
                                .into(),
                        );
                    }
                    let evidence: Vec<ResearchEvidence> = match update.get("evidence").cloned() {
                        Some(value) => serde_json::from_value(value)
                            .map_err(|e| format!("evidence格式无效：{e}"))?,
                        None => Vec::new(),
                    };
                    // 结构化回执：只认本次工具真实产生的编号，值从回执本体取。
                    let mut structured: Vec<StructuredEvidence> = Vec::new();
                    if let Some(value) = update.get("structured").cloned() {
                        let refs: Vec<Value> = serde_json::from_value(value)
                            .map_err(|e| format!("structured格式无效：{e}"))?;
                        for item in &refs {
                            let id = item
                                .get("receipt")
                                .and_then(Value::as_str)
                                .ok_or("structured每项必须给receipt（工具返回的回执编号）")?;
                            let receipt = receipts.get(id).ok_or_else(|| {
                                format!(
                                    "回执{id}不是本次工具产生的：结构化依据只能引用真实取数回执，\
                                     不能自述数值"
                                )
                            })?;
                            if structured.iter().any(|e| e.receipt == receipt.receipt) {
                                continue;
                            }
                            structured.push(receipt.clone());
                        }
                    }
                    if evidence.len() > 10 || structured.len() > 10 {
                        return Err("单字段依据最多10项".into());
                    }
                    let mut distinct = BTreeSet::new();
                    for item in &evidence {
                        let body = bodies
                            .get(&item.source_url)
                            .ok_or("evidence来源正文未取得，搜索摘要不能验证字段")?;
                        if item.quote.trim().is_empty()
                            || item.quote.chars().count() > 600
                            || !compact(body).contains(&compact(&item.quote))
                        {
                            return Err("quote须是已抓正文中同一处不超过600字的原文摘录".into());
                        }
                        if item.value.trim().is_empty()
                            || (!matches!(field.evidence_required, EvidenceRequired::SourceLink)
                                && !compact(&item.quote).contains(&compact(&item.value)))
                        {
                            return Err(
                                "value须逐字来自该条原文摘录，不拼接其他位置或推测数字".into()
                            );
                        }
                        if matches!(field.evidence_required, EvidenceRequired::SourceLink)
                            && item.value != item.source_url
                        {
                            return Err("来源链接字段只能使用实际取得正文的网址".into());
                        }
                        if let Some(label) = &item.date_label {
                            if label.trim().is_empty()
                                || !compact(&item.quote).contains(&compact(label))
                            {
                                return Err("date_label必须沿用引文里的日期标签".into());
                            }
                        }
                        if matches!(field.evidence_required, EvidenceRequired::DocumentIssueDate)
                            && !labeled_value(&item.quote, &item.value, &["印发日期", "印发时间"])
                        {
                            return Err("印发日期需要原文印发标签，不能用正文落款替代".into());
                        }
                        if matches!(field.evidence_required, EvidenceRequired::PublicationTime)
                            && !labeled_value(
                                &item.quote,
                                &item.value,
                                &[
                                    "发布日期",
                                    "发布时间",
                                    "信息时间",
                                    "更新时间",
                                    "更新日期",
                                    "Published",
                                    "Updated",
                                ],
                            )
                        {
                            return Err(
                                "发布/更新时间须保留原文标签，不得用活动日期或获取时间替代".into(),
                            );
                        }
                        if matches!(
                            field.evidence_required,
                            EvidenceRequired::DocumentSignatureDate
                        ) && ["印发日期", "发布日期", "更新时间"]
                            .iter()
                            .any(|label| item.quote.contains(label))
                        {
                            return Err("正文落款引用不能混用顶部印发/发布/更新标签".into());
                        }
                        distinct.insert((
                            item.value.clone(),
                            item.source_url.clone(),
                            item.quote.clone(),
                        ));
                    }
                    if distinct.len() + structured.len() < field.minimum_findings {
                        return Err(
                            "重复引文或重复回执不能凑够用户要求的条目数（原文引文与结构化回执各算一项）"
                                .into(),
                        );
                    }
                    if evidence.is_empty() && structured.is_empty() {
                        return Err(format!(
                            "字段{}至少需要{}项已取得的依据",
                            field.label, field.minimum_findings
                        ));
                    }
                    field.status = FieldStatus::Supported;
                    field.evidence = evidence;
                    field.structured_evidence = structured;
                    field.reason = None;
                    field.gap_source_url = None;
                    field.applicability_evidence = None;
                }
                "unavailable" => {
                    let reason = text(update, "reason_code")?;
                    if ![
                        "source_unavailable",
                        "not_found",
                        "insufficient_evidence",
                        "provider_unavailable",
                        "budget_exhausted",
                    ]
                    .contains(&reason)
                    {
                        return Err("reason_code无效；缺口只能归属于已有请求字段".into());
                    }
                    if self.searches.is_empty() && self.fetch_attempts.is_empty() {
                        return Err("尚未实际查询，不能直接把公开字段标成未取得".into());
                    }
                    if reason == "budget_exhausted" && self.budget.remaining > 0 {
                        return Err("仍有检索预算，不能声称预算耗尽".into());
                    }
                    if reason == "not_found"
                        && !self
                            .searches
                            .iter()
                            .any(|s| s.returned_hits == 0 && s.health.empty_result_is_conclusive())
                    {
                        return Err(
                            "没有健康的空检索凭据；引擎未响应只能标检索覆盖不足，不能标查无结果"
                                .into(),
                        );
                    }
                    let source = update.get("source_url").and_then(Value::as_str);
                    if reason == "source_unavailable"
                        && !source.is_some_and(|url| self.failed_urls.contains(url))
                    {
                        return Err("source_unavailable必须引用本任务真实抓取失败的网址".into());
                    }
                    field.status = FieldStatus::Unavailable;
                    field.evidence.clear();
                    field.reason = Some(reason.into());
                    field.gap_source_url = source.map(String::from);
                    field.applicability_evidence = None;
                }
                "not_applicable" => {
                    if !matches!(
                        field.evidence_required,
                        EvidenceRequired::DocumentIssueDate
                            | EvidenceRequired::DocumentSignatureDate
                    ) {
                        return Err("主办、活动日期、发布时间等所问事实不能标成不适用；没有取得时必须如实登记unavailable".into());
                    }
                    let source = text(update, "source_url")?;
                    let body = bodies.get(source).ok_or("不适用判断也须基于实际已读来源")?;
                    let kind = text(update, "source_kind")?;
                    let quote = text(update, "source_kind_quote")?;
                    if kind != "news_article"
                        || quote.chars().count() > 240
                        || !compact(body).contains(&compact(quote))
                        || !["新闻稿", "新闻报道", "新闻动态", "新闻", "记者"]
                            .iter()
                            .any(|marker| quote.contains(marker))
                    {
                        return Err("文种不适用仅支持有原文新闻/记者标记的新闻页面；source_kind_quote须逐字来自已读正文，不能自称非公文".into());
                    }
                    if ["印发日期", "发文字号", "关于印发", "签发日期"]
                        .iter()
                        .any(|marker| body.contains(marker))
                    {
                        return Err("来源仍含公文或印发标记，不能把所问公文日期标为不适用；须核对具体日期或保留未取得".into());
                    }
                    let explanation = text(update, "explanation")?;
                    let applicable_terms =
                        if field.evidence_required == EvidenceRequired::DocumentIssueDate {
                            vec!["印发"]
                        } else {
                            vec!["落款", "签署"]
                        };
                    if explanation.chars().count() > 200
                        || !applicable_terms
                            .iter()
                            .any(|term| explanation.contains(term))
                        || !["新闻", "报道"]
                            .iter()
                            .any(|term| explanation.contains(term))
                    {
                        return Err("explanation须简短说明新闻文种为何不适用该项印发/落款日期，不能泛称已完成".into());
                    }
                    field.status = FieldStatus::NotApplicable;
                    field.reason = Some(explanation.to_string());
                    field.gap_source_url = Some(source.into());
                    field.evidence.clear();
                    field.applicability_evidence = Some(ApplicabilityEvidence {
                        source_kind: kind.into(),
                        source_url: source.into(),
                        quote: quote.into(),
                    });
                }
                _ => return Err("status须为supported/unavailable/not_applicable".into()),
            }
        }
        self.fields = next;
        Ok(self.report())
    }

    pub fn prepare_document_lookup(
        &mut self,
        args: &Value,
        used: usize,
        limit: usize,
    ) -> Result<DocumentLookup, String> {
        self.set_budget(used, limit);
        if self.budget.remaining < 2 {
            return Err(
                "定向找正文至少须保留一次检索和一次抓取预算；当前预算不足，按实际未取得交付".into(),
            );
        }
        let title = text(args, "document_title")?;
        if !self.hits.values().any(|seen| seen == title) {
            return Err(
                "document_title必须逐字引用本任务已见搜索结果的确切文件标题，不能编造".into(),
            );
        }
        let site = text(args, "site_url")?;
        if !self.successful_urls.contains(site) {
            return Err("site_url必须是本任务已经成功读取的站点页面，不猜官方域名".into());
        }
        let host = domain(site).ok_or("site_url无效")?;
        let field_ids: Vec<String> =
            serde_json::from_value(args.get("field_ids").cloned().ok_or("缺少field_ids")?)
                .map_err(|_| "field_ids须为数组")?;
        if field_ids.is_empty() || field_ids.iter().any(|id| !self.fields.contains_key(id)) {
            return Err("field_ids只能引用用户请求中的字段".into());
        }
        let query = format!("site:{host} {}", title.replace(['"', '\n', '\r'], " "));
        if let Some(previous) = self.lookups.get(&query) {
            if previous.attempted {
                return Err(
                    "此确切文件与站点已做过定向检索；不重复同一查询，核对已有命中或保留具体缺口"
                        .into(),
                );
            }
            return Ok(previous.clone());
        }
        let lookup = DocumentLookup {
            query: query.clone(),
            document_title: title.into(),
            site_domain: host,
            field_ids,
            candidate_urls: vec![],
            attempted: false,
            candidate_fetch_receipts: vec![],
            resolution: None,
        };
        self.lookups.insert(query, lookup.clone());
        Ok(lookup)
    }

    pub fn lookup_candidates(&self, query: &str) -> Vec<String> {
        self.lookups
            .get(query)
            .map(|lookup| lookup.candidate_urls.clone())
            .unwrap_or_default()
    }

    /// Finalization is separate from observing search: a same-title result
    /// needs a fetch receipt produced after that lookup's search response.
    pub fn finish_document_lookup(&mut self, query: &str) -> Result<DocumentLookup, String> {
        let receipt = self
            .searches
            .iter()
            .rev()
            .find(|receipt| receipt.query == query)
            .ok_or("定向查询尚无真实搜索凭据")?;
        let lookup = self.lookups.get_mut(query).ok_or("未知定向查询")?;
        if !lookup.attempted {
            return Err("仅准备查询不能作为恢复尝试".into());
        }
        lookup.resolution = Some(if lookup.candidate_urls.is_empty() {
            if receipt.health.empty_result_is_conclusive() {
                LookupResolution::NoMatchingCandidate
            } else {
                LookupResolution::SearchCoverageUnavailable
            }
        } else {
            if lookup.candidate_fetch_receipts.is_empty() {
                return Err(
                    "已找到真实同名候选，但尚未执行候选正文抓取；仅搜索不能完成恢复路径".into(),
                );
            }
            if lookup
                .candidate_fetch_receipts
                .iter()
                .any(|fetch| fetch.body_obtained)
            {
                LookupResolution::CandidateBodyObtained
            } else {
                LookupResolution::CandidateUnavailable
            }
        });
        Ok(lookup.clone())
    }

    pub fn delivery(&self) -> Result<ResearchDelivery, String> {
        let mut missing = vec![];
        let mut sources = BTreeSet::new();
        for id in &self.order {
            let field = &self.fields[id];
            if field.status == FieldStatus::Pending {
                return Err(format!(
                    "已登记请求字段「{}」尚未登记结果或具体未取得状态",
                    field.label
                ));
            }
            if field.status == FieldStatus::Unavailable {
                if self.lookups.values().any(|lookup| {
                    lookup.field_ids.contains(id) && lookup.attempted && lookup.resolution.is_none()
                }) {
                    return Err(format!(
                        "已登记请求字段「{}」的定向恢复尚未完成；有同名候选须保留本次真实抓取凭据，无候选或服务受阻须明确结束状态",
                        field.label
                    ));
                }
                if let Some(url) = &field.gap_source_url {
                    if let Some(title) = self.hits.get(url).filter(|title| document_title(title)) {
                        let alternate = self
                            .successful_urls
                            .iter()
                            .any(|seen| domain(seen) != domain(url));
                        let recovered = self.lookups.values().any(|lookup| {
                            same_title(&lookup.document_title, title)
                                && lookup.field_ids.contains(id)
                                && lookup.attempted
                                && lookup.resolution.is_some()
                                && (lookup.candidate_urls.is_empty()
                                    || !lookup.candidate_fetch_receipts.is_empty())
                        });
                        if alternate && !recovered && self.budget.remaining >= 2 {
                            return Err(format!(
                                "字段「{}」已有确切文件「{}」线索且仍有{}次预算；先用research_document_lookup在已读官方站点定向查同名正文，不能因镜像失败直接交付缺口",
                                field.label, title, self.budget.remaining
                            ));
                        }
                    }
                }
                let reason = match field.reason.as_deref() {
                    Some("source_unavailable") => "对应原文未取得",
                    Some("not_found") => "本次查询未找到匹配，不能推断事实不存在",
                    Some("provider_unavailable") => "检索覆盖不足，未能核实",
                    Some("budget_exhausted") => "预算内未能核实",
                    _ => "依据不足，未核实",
                };
                missing.push(format!(
                    "{}{}：{}",
                    field
                        .subject
                        .as_ref()
                        .map(|s| format!("{s}·"))
                        .unwrap_or_default(),
                    field.label,
                    reason
                ));
            }
            for evidence in &field.evidence {
                sources.insert(evidence.source_url.clone());
            }
            if field.status == FieldStatus::NotApplicable {
                if let Some(url) = &field.gap_source_url {
                    sources.insert(url.clone());
                }
            }
        }
        Ok(ResearchDelivery {
            missing,
            fields: self
                .order
                .iter()
                .map(|id| self.fields[id].clone())
                .collect(),
            source_urls: sources.into_iter().collect(),
            health: self.searches.clone(),
            budget: self.budget.clone(),
        })
    }

    pub fn report(&self) -> Value {
        json!({"fields":self.order.iter().map(|id|&self.fields[id]).collect::<Vec<_>>(),"searches":self.searches,"budget":self.budget,"document_lookups":self.lookups.values().collect::<Vec<_>>(),"note":"仅追踪已登记请求字段，不保证用户原话中的全部需求已被识别；引文可追踪不等于语义已自动证实，获取时间不是文章发布时间。structured_evidence 为结构化查询回执（值取自工具返回，非模型自述），与原文引文来源类型不同。"})
    }
}

fn cell(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
}
fn source_link(url: &str) -> String {
    if domain(url).is_none() {
        return "来源网址无效".into();
    }
    let safe = url
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E");
    format!("[原文]({safe})")
}
fn observed_date_label(field: &ResearchField, evidence: &ResearchEvidence) -> String {
    if let Some(label) = &evidence.date_label {
        return cell(label);
    }
    for label in [
        "印发日期",
        "印发时间",
        "发布日期",
        "发布时间",
        "信息时间",
        "更新时间",
        "更新日期",
        "Published",
        "Updated",
    ] {
        if labeled_value(&evidence.quote, &evidence.value, &[label]) {
            return cell(label);
        }
    }
    match field.evidence_required {
        EvidenceRequired::EventDate => "活动日期".into(),
        EvidenceRequired::DocumentSignatureDate => "正文落款".into(),
        _ => "—".into(),
    }
}

/// Render only the registered request fields. Model-authored explanatory text
/// cannot append unrelated missing fields or relabel a source's date.
pub fn render_delivery(delivery: &ResearchDelivery) -> String {
    let mut output = String::from(
        "按本次已登记的请求字段整理如下。\n\n\
         说明：结果为**原文引文**时，引文逐字来自已抓正文；标注**结构化查询**的，\
         是按明确口径（查询对象与半径）向数据源取到的计数，可复现但不来自文章正文。\n\n\
         | 核查内容 | 对象 | 结果 | 日期含义 | 来源 |\n|---|---|---|---|---|\n",
    );
    for field in &delivery.fields {
        let subject = field
            .subject
            .as_deref()
            .map(cell)
            .unwrap_or_else(|| "—".into());
        let label = cell(&field.label);
        match field.status {
            FieldStatus::Supported => {
                for evidence in &field.evidence {
                    let value = if field.evidence_required == EvidenceRequired::SourceLink {
                        source_link(&evidence.source_url)
                    } else {
                        cell(&evidence.value)
                    };
                    output.push_str(&format!(
                        "| {label} | {subject} | {value} | {} | {} |\n",
                        observed_date_label(field, evidence),
                        source_link(&evidence.source_url)
                    ));
                }
                // 结构化查询依据：来源类型与原文引文不同，故在结果里点明，
                // 不伪装成"某篇文章这么说"，也不降级成"未取得"。
                for receipt in &field.structured_evidence {
                    output.push_str(&format!(
                        "| {label} | {subject} | {}（结构化查询：{} · {}） | 取数时间见分析过程 | {} |\n",
                        cell(&receipt.value),
                        cell(&receipt.kind),
                        cell(&receipt.scope),
                        cell(&receipt.source)
                    ));
                }
            }
            FieldStatus::NotApplicable => output.push_str(&format!(
                "| {label} | {subject} | 不适用：{} | — | {} |\n",
                cell(field.reason.as_deref().unwrap_or("已读来源不适用此项")),
                field
                    .gap_source_url
                    .as_deref()
                    .map(source_link)
                    .unwrap_or_else(|| "—".into())
            )),
            FieldStatus::Unavailable | FieldStatus::Pending => {
                let detail = match field.reason.as_deref() {
                    Some("source_unavailable") => "对应原文未取得",
                    Some("provider_unavailable") => "检索覆盖不足，未能核实",
                    Some("budget_exhausted") => "本次查询额度内未能核实",
                    Some("not_found") => "本次查询未找到匹配，不能据此推断不存在",
                    _ => "依据不足，尚未核实",
                };
                output.push_str(&format!(
                    "| {label} | {subject} | 未取得：{detail} | — | {} |\n",
                    field
                        .gap_source_url
                        .as_deref()
                        .map(source_link)
                        .unwrap_or_else(|| "—".into())
                ));
            }
        }
    }
    let unavailable = delivery.health.iter().any(|receipt| {
        matches!(
            receipt.health.availability,
            shangyi_intel::fetch::SearchAvailability::Unavailable
        )
    });
    let degraded = delivery.health.iter().any(|receipt| {
        matches!(
            receipt.health.availability,
            shangyi_intel::fetch::SearchAvailability::Degraded
                | shangyi_intel::fetch::SearchAvailability::Unknown
        )
    });
    if unavailable {
        output.push_str(
            "\n部分查询因检索服务未响应而受阻；已取得的原文仍保留，未取得不代表相关事实不存在。\n",
        );
    } else if degraded {
        output.push_str("\n部分搜索来源未响应或未提供完整运行状态，检索覆盖仍有限；已读原文与未取得项已分别列出。\n");
    }
    if delivery.budget.remaining == 0 {
        output.push_str("\n本次查询额度已用完，尚未核实的请求字段已保留为未取得。\n");
    }
    output.push_str("\n表中日期沿用对应原文含义，页面获取时间不作为文章发布日期。\n");
    output
}

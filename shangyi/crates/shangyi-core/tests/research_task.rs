use serde_json::json;
use shangyi_core::research_task::{LookupResolution, ResearchTask, render_delivery};
use shangyi_intel::fetch::{EngineFailure, SearchAvailability, SearchBackendHealth, SearchHealth};
use std::collections::BTreeMap;

const REQUEST: &str = "请核实整个文化节的主办单位和日期，给出发布时间和来源链接。";
const PORTAL: &str = "https://www.shanghai.gov.cn/news/article";
const MIRROR: &str = "https://mirror.gov.cn/plan";
const ORIGINAL: &str = "https://www.shanghai.gov.cn/gwk/search/content/actual-result";
const TITLE: &str = "上海市商务委关于印发《第六届上海五五购物节总体方案》的通知";
fn health(degraded: bool) -> SearchHealth {
    SearchHealth {
        availability: if degraded {
            SearchAvailability::Degraded
        } else {
            SearchAvailability::Healthy
        },
        backends: vec![SearchBackendHealth {
            backend: "bocha".into(),
            responded: true,
            returned_results: 0,
            contributing_engines: vec![],
            unresponsive_engines: if degraded {
                vec![EngineFailure {
                    engine: "duckduckgo".into(),
                    reason: "CAPTCHA".into(),
                }]
            } else {
                vec![]
            },
            failure: None,
        }],
        ranking: None,
    }
}
fn plan() -> ResearchTask {
    ResearchTask::plan(&json!({"fields":[{"id":"organizer","label":"主办单位","subject":"整个文化节","request_quote":"请核实整个文化节的主办单位和日期","evidence_required":"original_text"}]}),REQUEST).unwrap()
}
fn failed_mirror_task() -> ResearchTask {
    let mut task = plan();
    task.observe_search(
        "2025 咖啡文化节 主办单位",
        &[(TITLE.into(), MIRROR.into())],
        &health(false),
        1,
        16,
    );
    task.observe_fetch(PORTAL, Some(PORTAL), true);
    task.observe_fetch(MIRROR, None, false);
    task.set_budget(3, 16);
    task.update(&json!({"updates":[{"field_id":"organizer","status":"unavailable","reason_code":"source_unavailable","source_url":MIRROR}]}),&BTreeMap::new()).unwrap();
    task
}

#[test]
fn unrequested_fields_cannot_be_added_to_plan_or_missing_list() {
    let added = ResearchTask::plan(
        &json!({"fields":[{"id":"address","label":"门牌地址","request_quote":REQUEST,"evidence_required":"original_text"}]}),
        REQUEST,
    );
    assert!(added.unwrap_err().contains("用户原文锚点"));
    let mut task = plan();
    task.observe_search("主办单位", &[], &health(true), 1, 16);
    assert!(task.update(&json!({"updates":[{"field_id":"coorganizer","status":"unavailable","reason_code":"provider_unavailable"}]}),&BTreeMap::new()).is_err());
    task.update(&json!({"updates":[{"field_id":"organizer","status":"unavailable","reason_code":"provider_unavailable"}]}),&BTreeMap::new()).unwrap();
    let delivery = task.delivery().unwrap();
    assert_eq!(
        delivery.missing,
        vec!["整个文化节·主办单位：检索覆盖不足，未能核实"]
    );
    assert!(!serde_json::to_string(&delivery).unwrap().contains("门牌"));
}

#[test]
fn successful_search_snippet_is_not_body_evidence_and_updates_are_atomic() {
    let mut task = plan();
    task.observe_search(
        "主办单位",
        &[(TITLE.into(), ORIGINAL.into())],
        &health(false),
        1,
        16,
    );
    let supported = json!({"field_id":"organizer","status":"supported","evidence":[{"value":"协会","source_url":ORIGINAL,"quote":"主办单位：协会"}]});
    assert!(
        task.update(&json!({"updates":[supported.clone()]}), &BTreeMap::new())
            .unwrap_err()
            .contains("正文未取得")
    );
    let bodies = BTreeMap::from([(ORIGINAL.into(), "主办单位：协会".into())]);
    assert!(task.update(&json!({"updates":[supported.clone(),{"field_id":"invented","status":"not_applicable"}]}),&bodies).is_err());
    assert!(task.delivery().unwrap_err().contains("尚未登记"));
    task.update(&json!({"updates":[supported]}), &bodies)
        .unwrap();
    assert!(task.delivery().unwrap().missing.is_empty());
}

#[test]
fn issue_publication_and_signature_dates_do_not_share_an_unlabeled_value() {
    let request = "请核实印发日期和发布时间。";
    let mut task=ResearchTask::plan(&json!({"fields":[{"id":"issued","label":"印发日期","request_quote":request,"evidence_required":"document_issue_date"}]}),request).unwrap();
    let bodies = BTreeMap::from([(
        ORIGINAL.into(),
        "印发日期：2025-04-23 发布日期：2025-04-24 上海市商务委员会 2025年4月21日".into(),
    )]);
    let incorrect = json!({"updates":[{"field_id":"issued","status":"supported","evidence":[{"value":"2025年4月21日","source_url":ORIGINAL,"quote":"上海市商务委员会 2025年4月21日"}]}]});
    assert!(
        task.update(&incorrect, &bodies)
            .unwrap_err()
            .contains("不能用正文落款替代")
    );
    let mut mixed = incorrect.clone();
    mixed["updates"][0]["evidence"][0]["quote"] = json!(bodies[ORIGINAL]);
    assert!(
        task.update(&mixed, &bodies).is_err(),
        "包括正确标签的长引文不能把另一处日期绑定到该标签"
    );
    task.update(&json!({"updates":[{"field_id":"issued","status":"supported","evidence":[{"value":"2025-04-23","source_url":ORIGINAL,"quote":"印发日期：2025-04-23","date_label":"印发日期"}]}]}),&bodies).unwrap();
    assert_eq!(
        task.delivery().unwrap().fields[0].evidence[0].value,
        "2025-04-23"
    );
}

#[test]
fn mirror_failure_requires_real_title_and_known_domain_recovery_before_partial() {
    let mut task = failed_mirror_task();
    assert!(
        task.delivery()
            .unwrap_err()
            .contains("research_document_lookup")
    );
    assert!(
        task.prepare_document_lookup(
            &json!({"field_ids":["organizer"],"document_title":"编造的官方文件","site_url":PORTAL}),
            3,
            16
        )
        .is_err()
    );
    assert!(task.prepare_document_lookup(&json!({"field_ids":["organizer"],"document_title":TITLE,"site_url":"https://invented.gov.cn/page"}),3,16).is_err());
    let lookup = task
        .prepare_document_lookup(
            &json!({"field_ids":["organizer"],"document_title":TITLE,"site_url":PORTAL}),
            3,
            16,
        )
        .unwrap();
    assert!(lookup.query.contains("site:shanghai.gov.cn"));
    assert!(lookup.query.contains(TITLE));
    assert!(
        task.delivery().is_err(),
        "preparing a query is not executing it"
    );
    task.observe_search(
        &lookup.query,
        &[
            (TITLE.into(), ORIGINAL.into()),
            (
                "2026年另一份方案".into(),
                "https://www.shanghai.gov.cn/unrelated".into(),
            ),
        ],
        &health(false),
        4,
        16,
    );
    assert_eq!(task.lookup_candidates(&lookup.query), vec![ORIGINAL]);
    assert!(
        task.delivery().is_err(),
        "a matching search lead still needs its body"
    );
    task.observe_fetch(ORIGINAL, Some(ORIGINAL), true);
    task.set_budget(5, 16);
    assert_eq!(
        task.finish_document_lookup(&lookup.query)
            .unwrap()
            .resolution,
        Some(LookupResolution::CandidateBodyObtained)
    );
    let bodies = BTreeMap::from([(ORIGINAL.into(), "主办单位：上海市文化创意产业促进会".into())]);
    task.update(&json!({"updates":[{"field_id":"organizer","status":"supported","evidence":[{"value":"上海市文化创意产业促进会","source_url":ORIGINAL,"quote":"主办单位：上海市文化创意产业促进会"}]}]}),&bodies).unwrap();
    let result = task.delivery().unwrap();
    assert!(result.missing.is_empty());
    assert_eq!(result.source_urls, vec![ORIGINAL]);
}

#[test]
fn failed_engines_allow_honest_partial_without_negative_fact_or_repeated_lookup() {
    let mut task = failed_mirror_task();
    let args = json!({"field_ids":["organizer"],"document_title":TITLE,"site_url":PORTAL});
    let lookup = task.prepare_document_lookup(&args, 3, 16).unwrap();
    task.observe_search(&lookup.query, &[], &health(true), 4, 16);
    assert!(
        task.delivery().is_err(),
        "observing search alone is not lookup finalization"
    );
    assert_eq!(
        task.finish_document_lookup(&lookup.query)
            .unwrap()
            .resolution,
        Some(LookupResolution::SearchCoverageUnavailable)
    );
    assert!(task.update(&json!({"updates":[{"field_id":"organizer","status":"unavailable","reason_code":"not_found"}]}),&BTreeMap::new()).unwrap_err().contains("没有健康的空检索凭据"));
    let delivery = task.delivery().unwrap();
    assert_eq!(delivery.missing.len(), 1);
    assert_eq!(
        delivery.health.last().unwrap().health.backends[0].unresponsive_engines[0].reason,
        "CAPTCHA"
    );
    assert!(
        task.prepare_document_lookup(&args, 4, 16)
            .unwrap_err()
            .contains("不重复")
    );
    let mut exhausted = failed_mirror_task();
    exhausted.set_budget(15, 16);
    assert_eq!(exhausted.delivery().unwrap().budget.remaining, 1);
    assert!(exhausted.prepare_document_lookup(&args, 15, 16).is_err());
}

#[test]
fn not_applicable_is_explained_under_requested_field_not_added_as_a_gap() {
    let request = "请查这篇新闻的印发日期。";
    let mut task=ResearchTask::plan(&json!({"fields":[{"id":"issued","label":"印发日期","request_quote":request,"evidence_required":"document_issue_date"}]}),request).unwrap();
    let args = json!({"updates":[{"field_id":"issued","status":"not_applicable","source_url":PORTAL,"source_kind":"news_article","source_kind_quote":"新闻","explanation":"该页面为新闻稿，不适用公文印发日期；保留实际发布日期"}]});
    assert!(task.update(&args, &BTreeMap::new()).is_err());
    task.update(
        &args,
        &BTreeMap::from([(PORTAL.into(), "新闻 发布日期：2025-04-24".into())]),
    )
    .unwrap();
    let delivery = task.delivery().unwrap();
    assert!(delivery.missing.is_empty());
    assert!(
        delivery.fields[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("不适用公文印发日期")
    );
    let rendered = render_delivery(&delivery);
    assert!(rendered.contains("不适用"));
    assert!(!rendered.contains("未取得"));
}

#[test]
fn provider_transport_failure_is_observed_and_bounded_partial_is_readable() {
    let mut task = failed_mirror_task();
    let lookup = task
        .prepare_document_lookup(
            &json!({"field_ids":["organizer"],"document_title":TITLE,"site_url":PORTAL}),
            3,
            16,
        )
        .unwrap();
    task.observe_search_failure(&lookup.query, "HTTP 503 search upstream", 4, 16);
    assert!(task.delivery().is_err());
    task.finish_document_lookup(&lookup.query).unwrap();
    let delivery = task.delivery().unwrap();
    assert_eq!(
        delivery.health.last().unwrap().health.availability,
        SearchAvailability::Unavailable
    );
    let output = render_delivery(&delivery);
    assert!(output.contains("| 核查内容 | 对象 | 结果 |"));
    assert!(output.contains("整个文化节"));
    assert!(output.contains("检索服务未响应"));
    assert!(!output.contains("HTTP 503"));
    assert!(!output.contains("field_id"));
    assert!(!output.contains("budget"));
    assert!(!output.contains("\\n"));
    assert!(!output.contains("承办"));
    assert!(!output.contains("门牌"));
}

#[test]
fn organizer_or_event_date_cannot_escape_missing_via_not_applicable() {
    for (label, kind) in [
        ("主办单位", "original_text"),
        ("日期", "event_date"),
        ("发布时间", "publication_time"),
    ] {
        let mut task=ResearchTask::plan(&json!({"fields":[{"id":"required","label":label,"request_quote":REQUEST,"evidence_required":kind}]}),REQUEST).unwrap();
        let result=task.update(&json!({"updates":[{"field_id":"required","status":"not_applicable","source_url":PORTAL,"source_kind":"news_article","source_kind_quote":"新闻稿","explanation":"新闻稿没有这项内容"}]}),&BTreeMap::from([(PORTAL.into(),"新闻稿 发布日期：2025-04-24".into())]));
        assert!(result.unwrap_err().contains("不能标成不适用"));
        assert!(task.delivery().is_err());
    }
}

#[test]
fn not_applicable_requires_real_non_document_type_evidence() {
    let request = "请查印发日期。";
    let mut task=ResearchTask::plan(&json!({"fields":[{"id":"issued","label":"印发日期","request_quote":request,"evidence_required":"document_issue_date"}]}),request).unwrap();
    let args = json!({"updates":[{"field_id":"issued","status":"not_applicable","source_url":PORTAL,"source_kind":"news_article","source_kind_quote":"新闻稿","explanation":"新闻稿不适用公文印发日期"}]});
    assert!(
        task.update(
            &args,
            &BTreeMap::from([(PORTAL.into(), "普通网页，没有文种标记".into())])
        )
        .is_err(),
        "model source_kind is not evidence"
    );
    let mut navigation = args.clone();
    navigation["updates"][0]["source_kind_quote"] = json!("新闻导航");
    assert!(
        task.update(
            &navigation,
            &BTreeMap::from([(
                PORTAL.into(),
                "新闻导航 市商务委关于印发总体方案的通知 印发日期：2025-04-23".into()
            )])
        )
        .unwrap_err()
        .contains("公文或印发标记")
    );
    assert!(task.delivery().is_err());
}

#[test]
fn old_or_unrelated_fetch_does_not_finish_new_candidate_lookup_even_at_low_budget() {
    let mut task = failed_mirror_task();
    task.observe_fetch(ORIGINAL, None, false);
    let lookup = task
        .prepare_document_lookup(
            &json!({"field_ids":["organizer"],"document_title":TITLE,"site_url":PORTAL}),
            14,
            16,
        )
        .unwrap();
    task.observe_search(
        &lookup.query,
        &[(TITLE.into(), ORIGINAL.into())],
        &health(false),
        15,
        16,
    );
    assert!(
        task.finish_document_lookup(&lookup.query)
            .unwrap_err()
            .contains("尚未执行候选正文抓取")
    );
    assert!(
        task.delivery().is_err(),
        "one remaining call must not bypass a found candidate"
    );
    task.observe_fetch("https://www.shanghai.gov.cn/unrelated", None, false);
    assert!(task.finish_document_lookup(&lookup.query).is_err());
    task.observe_fetch(ORIGINAL, None, false);
    task.set_budget(16, 16);
    let finished = task.finish_document_lookup(&lookup.query).unwrap();
    assert_eq!(finished.candidate_fetch_receipts.len(), 1);
    assert_eq!(
        finished.resolution,
        Some(LookupResolution::CandidateUnavailable)
    );
    assert_eq!(task.delivery().unwrap().missing.len(), 1);
}

#[test]
fn deterministic_delivery_preserves_requested_order_sources_and_date_labels() {
    let mut task=ResearchTask::plan(&json!({"fields":[
        {"id":"date","label":"发布时间","request_quote":REQUEST,"evidence_required":"publication_time"},
        {"id":"organizer","label":"主办单位","subject":"整个文化节","request_quote":REQUEST,"evidence_required":"original_text"}
    ]}),REQUEST).unwrap();
    let body = "更新时间：2025-05-12 主办单位：甲协会";
    task.update(&json!({"updates":[
        {"field_id":"organizer","status":"supported","evidence":[{"value":"甲协会","source_url":ORIGINAL,"quote":"主办单位：甲协会"}]},
        {"field_id":"date","status":"supported","evidence":[{"value":"2025-05-12","source_url":ORIGINAL,"quote":"更新时间：2025-05-12","date_label":"更新时间"}]}
    ]}),&BTreeMap::from([(ORIGINAL.into(),body.into())])).unwrap();
    let output = render_delivery(&task.delivery().unwrap());
    assert!(output.find("| 发布时间 |").unwrap() < output.find("| 主办单位 |").unwrap());
    assert!(output.contains("| 2025-05-12 | 更新时间 |"));
    assert!(output.contains(&format!("[原文]({ORIGINAL})")));
    assert!(!output.contains("承办"));
    assert!(!output.contains("指导"));
}

// ── 结构化查询依据（POI 计数） ─────────────────────────────────────────────

/// 与 `plan()` 同源，但可指定条目数下限（测"重复回执不能凑数"）。
fn task_with_minimum(minimum: usize) -> ResearchTask {
    ResearchTask::plan(
        &json!({"fields":[{"id":"organizer","label":"主办单位","subject":"整个文化节",
            "request_quote":"请核实整个文化节的主办单位和日期","evidence_required":"original_text",
            "minimum_findings":minimum}]}),
        REQUEST,
    )
    .unwrap()
}

/// 回执只能引用工具真实产生的编号；值/来源/口径由回执给出。
#[test]
fn structured_receipt_supports_a_field_without_a_body_quote() {
    let mut task = task_with_minimum(1);
    let mut receipts = shangyi_core::research_task::StructuredReceipts::new();
    receipts.insert("call-1#1".into(), shangyi_core::research_task::StructuredEvidence {
        receipt: "call-1#1".into(),
        kind: "高德 POI 周边计数".into(),
        scope: "上海市黄浦区南京东路 · 500 米".into(),
        value: "144 家".into(),
        source: "高德开放平台 Web 服务（POI 周边搜索）".into(),
    });
    task.update_with_receipts(
        &json!({"updates":[{"field_id":"organizer","status":"supported","structured":[{"receipt":"call-1#1"}]}]}),
        &BTreeMap::new(),
        &receipts,
    )
    .expect("引用真实回执应当被接受");
    // 走公开上报路径核对（fields 是私有的，report() 才是对外契约）。
    let reported = task.report();
    let field = &reported["fields"][0];
    assert_eq!(field["status"], json!("supported"));
    assert_eq!(field["structured_evidence"][0]["value"], json!("144 家"));
    assert_eq!(field["structured_evidence"][0]["kind"], json!("高德 POI 周边计数"));
    assert_eq!(
        field["evidence"].as_array().map(|a| a.len()).unwrap_or(0),
        0,
        "结构化依据不该混进原文引文"
    );
}

/// 编造回执编号必须被拒——这是"结构化证据不可伪造"的落点。
#[test]
fn fabricated_receipt_is_refused() {
    let mut task = task_with_minimum(1);
    let receipts = shangyi_core::research_task::StructuredReceipts::new();
    let error = task
        .update_with_receipts(
            &json!({"updates":[{"field_id":"organizer","status":"supported","structured":[{"receipt":"我自己编的#9"}]}]}),
            &BTreeMap::new(),
            &receipts,
        )
        .expect_err("没有真实回执时必须拒绝");
    assert!(error.contains("不是本次工具产生的"), "{error}");
}

/// 没有任何依据（既无引文也无回执）不能标 supported。
#[test]
fn supported_without_any_evidence_is_refused() {
    let mut task = task_with_minimum(1);
    let error = task
        .update_with_receipts(
            &json!({"updates":[{"field_id":"organizer","status":"supported"}]}),
            &BTreeMap::new(),
            &shangyi_core::research_task::StructuredReceipts::new(),
        )
        .expect_err("空依据不能标 supported");
    assert!(error.contains("evidence") && error.contains("structured"), "{error}");
}

/// 重复引用同一回执不能凑够条目数。
#[test]
fn duplicate_receipts_cannot_pad_the_finding_count() {
    let mut task = task_with_minimum(2);
    let mut receipts = shangyi_core::research_task::StructuredReceipts::new();
    receipts.insert("call-1#1".into(), shangyi_core::research_task::StructuredEvidence {
        receipt: "call-1#1".into(), kind: "高德 POI 周边计数".into(),
        scope: "某地 · 500 米".into(), value: "144 家".into(), source: "高德".into(),
    });
    let error = task
        .update_with_receipts(
            &json!({"updates":[{"field_id":"organizer","status":"supported","structured":[{"receipt":"call-1#1"},{"receipt":"call-1#1"}]}]}),
            &BTreeMap::new(),
            &receipts,
        )
        .expect_err("同一回执重复引用不能凑数");
    assert!(error.contains("不能凑够"), "{error}");
}

/// 交付渲染必须把结构化依据与原文引文分开写清。
#[test]
fn delivery_marks_structured_evidence_as_such() {
    let mut task = task_with_minimum(1);
    let mut receipts = shangyi_core::research_task::StructuredReceipts::new();
    receipts.insert("call-1#1".into(), shangyi_core::research_task::StructuredEvidence {
        receipt: "call-1#1".into(), kind: "高德 POI 周边计数".into(),
        scope: "上海市黄浦区南京东路 · 500 米".into(), value: "144 家".into(),
        source: "高德开放平台 Web 服务（POI 周边搜索）".into(),
    });
    task.update_with_receipts(
        &json!({"updates":[{"field_id":"organizer","status":"supported","structured":[{"receipt":"call-1#1"}]}]}),
        &BTreeMap::new(),
        &receipts,
    )
    .unwrap();
    let delivery = task.delivery().unwrap();
    let rendered = shangyi_core::research_task::render_delivery(&delivery);
    assert!(rendered.contains("144 家"), "{rendered}");
    assert!(rendered.contains("结构化查询"), "必须标明来源类型：{rendered}");
    assert!(rendered.contains("高德"), "{rendered}");
    assert!(
        !rendered.contains("未取得"),
        "有结构化依据就不该再写未取得：{rendered}"
    );
}

//! `web_search` 接上真后端后的**端到端**行为（离线，用假 provider）。
//!
//! 与 `search_live.rs` 的分工：那个测"真后端能取回东西"，
//! 这个测"接上后端之后，`web_search` 工具与 agent 循环的行为对不对"。
//!
//! 为什么两者都需要：真后端测试证明**链路通**，离线测试证明**语义对**
//! （例如"未配置"与"零结果"必须区分开）。混在一起会因为网络波动而变得不可靠。

use std::collections::BTreeMap;

use shangyi_core::agent::{ExecOutcome, ToolCall, ToolExecutor};
use shangyi_core::domain_exec::{
    DomainExecutor, FetchedText, SearchHit, SearchProvider, SearchUnavailable,
};
use shangyi_core::phase_graph::SessionPhase;
use shangyi_kb::TheoryLibrary;
use shangyi_playbooks::{Playbook, PlaybookMeta};

fn playbook() -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: "p".into(),
            name: "n".into(),
            description: String::new(),
            order: 0,
            version: 1,
        },
        applicable_when: vec![],
        fails_when: vec![],
        required_inputs: vec![],
        value_net: None,
        intake: vec![],
        models: vec![],
        selectors: BTreeMap::new(),
        tool_allowlist: vec![],
        report_vars: BTreeMap::new(),
    }
}

struct Fake {
    hits: Vec<SearchHit>,
    unconfigured: bool,
    health_known: bool,
}

impl SearchProvider for Fake {
    fn search(&self, _q: &str) -> Result<Vec<SearchHit>, SearchUnavailable> {
        if self.unconfigured {
            return Err(SearchUnavailable::NoBackend {
                detail: "未配置".into(),
            });
        }
        Ok(self.hits.clone())
    }
    fn search_with_health(
        &self,
        query: &str,
    ) -> Result<(Vec<SearchHit>, shangyi_intel::fetch::SearchHealth), SearchUnavailable> {
        let hits = self.search(query)?;
        let mut health = shangyi_intel::fetch::SearchHealth::unreported(hits.len());
        if self.health_known {
            health.availability = shangyi_intel::fetch::SearchAvailability::Healthy;
            health.backends[0].backend = "fake".into();
        }
        Ok((hits, health))
    }
    fn fetch(&self, url: &str) -> Result<FetchedText, SearchUnavailable> {
        Ok(FetchedText {
            url: url.into(),
            text: "正文".into(),
            raw_bytes: 6,
        })
    }
}

fn call(name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        call_id: "c1".into(),
        name: name.into(),
        arguments: args,
    }
}

/// 未知来源健康状态下，空结果只能说本次未取得，不能当作已确认的零命中。
#[test]
fn empty_hits_without_health_are_not_conclusive() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let fake = Fake { hits: vec![], unconfigured: false, health_known: false };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&fake);
    let out = ex.execute(&call("web_search", serde_json::json!({"query":"x"})), SessionPhase::Intel);
    match out {
        ExecOutcome::Ok(s) => {
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            assert_eq!(v["hits"].as_array().unwrap().len(), 0);
            assert_eq!(
                v["empty_is_a_conclusion"],
                serde_json::json!(false),
                "来源健康未知时必须保留证据缺口：{s}"
            );
            assert!(
                v["note"].as_str().unwrap().contains("覆盖不足"),
                "必须说明来源覆盖不足：{s}"
            );
        }
        other => panic!("零结果应是 Ok，实得 {other:?}"),
    }
}

/// 只有搜索后端明确报告健康响应且没有故障来源，空结果才是本次查询的结论。
#[test]
fn healthy_empty_search_is_conclusive_but_not_proof_of_absence() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let fake = Fake { hits: vec![], unconfigured: false, health_known: true };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&fake);
    let out = ex.execute(&call("web_search", serde_json::json!({"query":"x"})), SessionPhase::Intel);
    match out {
        ExecOutcome::Ok(s) => {
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            assert_eq!(v["empty_is_a_conclusion"], serde_json::json!(true), "{s}");
            assert!(v["note"].as_str().unwrap().contains("不等于相关信息不存在"), "{s}");
        }
        other => panic!("健康来源的零结果应是 Ok，实得 {other:?}"),
    }
}

/// **未配置后端必须是 Blocked（"未取得"），不是 Ok（"没有"）**——
/// 二者在报告里是完全不同的两句话。
#[test]
fn unconfigured_backend_is_blocked_not_empty_result() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let fake = Fake { hits: vec![], unconfigured: true, health_known: false };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&fake);
    match ex.execute(&call("web_search", serde_json::json!({"query":"x"})), SessionPhase::Intel) {
        ExecOutcome::Blocked(m) => assert!(m.contains("未配置"), "实得：{m}"),
        other => panic!("未配置后端应是 Blocked，实得 {other:?}"),
    }
}

/// 有结果时必须带上**来源后端**（多后端链式回退时，审计要知道情报出自哪家）。
#[test]
fn hits_carry_backend_provenance() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let fake = Fake {
        hits: vec![SearchHit {
            title: "竞对降价".into(),
            url: "https://e.com/a".into(),
            snippet: "自 3 月起降价 15%".into(),
            backend: "searxng".into(),
            published_at: None,
            rank_score: 73,
            rank_signals: vec!["官方域名".into()],
        }],
        unconfigured: false,
        health_known: true,
    };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&fake);
    let out = ex.execute(&call("web_search", serde_json::json!({"query":"降价"})), SessionPhase::Intel);
    match out {
        ExecOutcome::Ok(s) => {
            let v: serde_json::Value = serde_json::from_str(&s).unwrap();
            assert_eq!(v["hits"][0]["backend"], serde_json::json!("searxng"));
            assert_eq!(v["provenance"]["kind"], serde_json::json!("web"));
            assert_eq!(
                v["empty_is_a_conclusion"],
                serde_json::json!(false),
                "有结果时不该标成空结果结论"
            );
        }
        other => panic!("实得 {other:?}"),
    }
}

/// 空 `query` 必须被拒——空查询要么浪费一次调用，要么返回无关内容。
#[test]
fn empty_query_is_rejected() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let fake = Fake { hits: vec![], unconfigured: false, health_known: false };
    let mut ex = DomainExecutor::new(&theory, &pb).with_search(&fake);
    match ex.execute(&call("web_search", serde_json::json!({"query":"  "})), SessionPhase::Intel) {
        ExecOutcome::Failed(m) => assert!(m.contains("query"), "实得：{m}"),
        other => panic!("空查询应被拒，实得 {other:?}"),
    }
}

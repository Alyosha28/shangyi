//! Provider-call budgets, independent of model wording or real network timing.
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};

use serde_json::{Value, json};
use shangyi_core::agent::{ExecOutcome, ToolCall, ToolExecutor};
use shangyi_core::domain_exec::{
    DomainExecutor, FetchedText, SearchHit, SearchProvider, SearchUnavailable,
};
use shangyi_core::phase_graph::SessionPhase;
use shangyi_kb::TheoryLibrary;
use shangyi_playbooks::{Playbook, PlaybookMeta};

const URL: &str = "https://example.invalid/page";

fn playbook() -> Playbook {
    Playbook {
        meta: PlaybookMeta {
            id: "test".into(),
            name: "test".into(),
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

struct ScriptedProvider {
    replies: RefCell<VecDeque<Result<FetchedText, SearchUnavailable>>>,
    urls: RefCell<Vec<String>>,
}
impl ScriptedProvider {
    fn new(replies: Vec<Result<FetchedText, SearchUnavailable>>) -> Self {
        Self {
            replies: RefCell::new(replies.into()),
            urls: RefCell::new(vec![]),
        }
    }
    fn calls(&self) -> usize {
        self.urls.borrow().len()
    }
}
impl SearchProvider for ScriptedProvider {
    fn search(&self, _query: &str) -> Result<Vec<SearchHit>, SearchUnavailable> {
        unreachable!("fetch-only fixture")
    }
    fn fetch(&self, url: &str) -> Result<FetchedText, SearchUnavailable> {
        self.urls.borrow_mut().push(url.to_string());
        self.replies
            .borrow_mut()
            .pop_front()
            .expect("unexpected network-provider call")
    }
}
fn failure(retryable: bool) -> Result<FetchedText, SearchUnavailable> {
    Err(SearchUnavailable::Failed {
        detail: "fixture fetch failure".into(),
        retryable,
    })
}
fn page(text: &str) -> Result<FetchedText, SearchUnavailable> {
    Ok(FetchedText {
        url: URL.into(),
        text: text.into(),
        raw_bytes: text.len(),
    })
}
fn fetch(executor: &mut DomainExecutor, url: &str, refresh: bool) -> ExecOutcome {
    executor.execute(
        &ToolCall {
            call_id: "fetch".into(),
            name: "web_fetch".into(),
            arguments: json!({"url":url,"refresh":refresh}),
        },
        SessionPhase::Intel,
    )
}
fn body(outcome: ExecOutcome) -> Value {
    match outcome {
        ExecOutcome::Ok(raw) => serde_json::from_str(&raw).unwrap(),
        other => panic!("expected body, got {other:?}"),
    }
}
fn blocked(outcome: ExecOutcome) -> String {
    match outcome {
        ExecOutcome::Blocked(reason) => reason,
        other => panic!("expected block, got {other:?}"),
    }
}

#[test]
fn permanent_failure_gets_one_provider_attempt_even_with_refresh() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let provider = ScriptedProvider::new(vec![failure(false), page("different source")]);
    let mut executor = DomainExecutor::new(&theory, &pb).with_search(&provider);
    blocked(fetch(&mut executor, URL, false));
    for _ in 0..20 {
        assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
    }
    assert_eq!(provider.calls(), 1);
    assert_eq!(
        executor.state().retrieval_attempts.get(),
        1,
        "locally blocked repeats must not consume network budget"
    );
    body(fetch(
        &mut executor,
        "https://example.invalid/alternative",
        false,
    ));
    assert_eq!(provider.calls(), 2, "another URL remains available");
}

#[test]
fn retryable_failure_gets_two_provider_attempts_and_new_task_resets_limit() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let provider = ScriptedProvider::new(vec![
        failure(true),
        failure(true),
        failure(true),
        failure(true),
    ]);
    let mut executor = DomainExecutor::new(&theory, &pb).with_search(&provider);
    for _ in 0..2 {
        blocked(fetch(&mut executor, URL, true));
    }
    for _ in 0..20 {
        assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
    }
    assert_eq!(provider.calls(), 2);
    assert_eq!(executor.state().retrieval_attempts.get(), 2);
    executor.begin_followup_task();
    for _ in 0..2 {
        blocked(fetch(&mut executor, URL, true));
    }
    assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
    assert_eq!(provider.calls(), 4);
    assert_eq!(executor.state().retrieval_attempts.get(), 2);
}

#[test]
fn successful_retrieval_resets_failure_streak_and_allows_normal_refresh() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let provider = ScriptedProvider::new(vec![
        failure(true),
        page("version one"),
        page("version two"),
        failure(true),
        failure(true),
    ]);
    let mut executor = DomainExecutor::new(&theory, &pb).with_search(&provider);
    blocked(fetch(&mut executor, URL, false));
    assert_eq!(body(fetch(&mut executor, URL, true))["text"], "version one");
    assert_eq!(body(fetch(&mut executor, URL, true))["text"], "version two");
    for _ in 0..2 {
        blocked(fetch(&mut executor, URL, true));
    }
    assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
    assert_eq!(provider.calls(), 5);
    let cached = body(fetch(&mut executor, URL, false));
    assert_eq!(cached["text"], "version two");
    assert_eq!(cached["cached"], true);
    assert_eq!(
        provider.calls(),
        5,
        "cache remains readable without resetting exhausted failures"
    );
    assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
}

#[test]
fn permanent_refresh_failure_preserves_snapshot_until_new_task_can_retry() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let provider = ScriptedProvider::new(vec![
        page("old evidence"),
        failure(false),
        page("new evidence"),
    ]);
    let mut executor = DomainExecutor::new(&theory, &pb).with_search(&provider);
    let original = body(fetch(&mut executor, URL, false));
    blocked(fetch(&mut executor, URL, true));
    assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
    assert_eq!(
        body(fetch(&mut executor, URL, false))["retrieved_at_unix"],
        original["retrieved_at_unix"]
    );
    executor.begin_followup_task();
    assert_eq!(
        body(fetch(&mut executor, URL, false))["text"],
        "old evidence"
    );
    assert_eq!(provider.calls(), 2);
    assert_eq!(
        body(fetch(&mut executor, URL, true))["text"],
        "new evidence"
    );
    assert_eq!(provider.calls(), 3);
}

#[test]
fn empty_body_is_a_failed_attempt_and_does_not_replace_existing_evidence() {
    let theory = TheoryLibrary::new();
    let pb = playbook();
    let provider = ScriptedProvider::new(vec![page("read evidence"), page("  ")]);
    let mut executor = DomainExecutor::new(&theory, &pb).with_search(&provider);
    body(fetch(&mut executor, URL, false));
    assert!(blocked(fetch(&mut executor, URL, true)).contains("没有取得页面正文"));
    assert!(blocked(fetch(&mut executor, URL, true)).contains("本次未再次联网"));
    assert_eq!(
        body(fetch(&mut executor, URL, false))["text"],
        "read evidence"
    );
    assert_eq!(provider.calls(), 2);
}

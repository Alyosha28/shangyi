//! Structured browser interaction. Events use a dedicated file, never parsed stdout.
use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{Value, json};
use shangyi_core::agent::UserQuestion;
use shangyi_core::phase_graph::{CheckpointRequest, SessionPhase};
use shangyi_core::runner::{CheckpointAnswer, RewindDecision, UserIo};
use shangyi_core::task_scope::ScopedTaskResult;

pub const PROTOCOL_VERSION: u32 = 2;

pub struct WebIo<R, W> {
    input: R,
    events: W,
    sequence: u64,
    mailbox: Option<PathBuf>,
    consumed_lines: usize,
    responses: Option<PathBuf>,
    response_offset: usize,
}

impl<R: BufRead, W: Write> WebIo<R, W> {
    pub fn new(input: R, events: W, mailbox: Option<PathBuf>) -> Self {
        Self { input, events, sequence: 0, mailbox, consumed_lines: 0, responses: None, response_offset: 0 }
    }

    pub fn with_responses(mut self, path: PathBuf) -> Self { self.responses=Some(path); self }

    pub fn emit(&mut self, event: Value) {
        // If the event channel fails, terminate instead of asking for an invisible approval.
        serde_json::to_writer(&mut self.events, &event).expect("web event write failed");
        self.events.write_all(b"\n").expect("web event newline failed");
        self.events.flush().expect("web event flush failed");
    }

    fn request(&mut self, kind: &str, payload: Value) -> Option<Value> {
        self.sequence += 1;
        let request_id = format!("request-{}", self.sequence);
        self.emit(json!({"type":"request", "request_id":request_id,
                         "kind":kind, "payload":payload}));
        loop {
            let mut line = String::new();
            if let Some(path) = &self.responses {
                let body = match std::fs::read_to_string(path) {
                    Ok(body) => body,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
                    Err(e) => panic!("response channel read failed: {e}"),
                };
                if let Some(end) = body[self.response_offset..].find('\n') {
                    line=body[self.response_offset..self.response_offset+end].to_string();
                    self.response_offset+=end+1;
                } else {
                    std::thread::sleep(std::time::Duration::from_millis(100)); continue;
                }
            } else if self.input.read_line(&mut line).ok()? == 0 { return None; }
            let Ok(value) = serde_json::from_str::<Value>(&line) else { continue; };
            if value.get("request_id").and_then(Value::as_str) != Some(&request_id) {
                continue;
            }
            let action = value.get("action").and_then(Value::as_str).unwrap_or("");
            let text = value.get("text").and_then(Value::as_str).unwrap_or("");
            let valid = match kind {
                "question" => action == "message" && !text.trim().is_empty(),
                "checkpoint" => action == "approve"
                    || (matches!(action, "revise" | "followup") && !text.trim().is_empty()),
                "rewind" => matches!(action, "approve" | "reject"),
                _ => false,
            };
            if !valid { continue; }
            self.emit(json!({"type":"response_received", "request_id":request_id,
                             "message_id":value.get("message_id")}));
            return Some(value);
        }
    }

    fn context_answers(text: String) -> Vec<(String, String)> {
        vec![("自由补充（不是逐项确认；未回答的内容仍须标记未取得）".into(), text)]
    }
}

impl<R: BufRead, W: Write> UserIo for WebIo<R, W> {
    fn scoped_task_completed(&mut self, result: &ScopedTaskResult) {
        self.scoped_task_completed_at(result,shangyi_core::SessionPhase::Intake);
    }
    fn scoped_task_completed_at(&mut self, result: &ScopedTaskResult, phase: shangyi_core::SessionPhase) {
        let mut event=serde_json::to_value(result).expect("task result serialization failed");
        event["type"]=json!("task_completed");
        event["phase"]=json!(phase.as_str());
        event["awaiting_followup"]=json!(self.mailbox.is_some());
        event["completed"]=json!(result.is_complete());
        self.emit(event);
    }

    fn wait_for_followup_task(&mut self) -> Option<String> {
        if self.mailbox.is_none() { return None; }
        loop {
            let messages=self.take_pending_messages();
            if !messages.is_empty() {
                self.emit(json!({"type":"task_started","phase":"intake","scope":null}));
                return Some(messages.join("\n\n"));
            }
            // The engine stays reachable, but no model request, question or
            // checkpoint is generated while the user reads the completed work.
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn can_revise_in_place(&self) -> bool { true }
    fn take_pending_messages(&mut self) -> Vec<String> {
        let Some(path) = &self.mailbox else { return vec![]; };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind()==std::io::ErrorKind::NotFound => return vec![],
            Err(error) => panic!("mailbox read failed: {error}"),
        };
        let lines: Vec<_> = text.split_inclusive('\n').filter(|l| l.ends_with('\n')).collect();
        let mut messages = vec![];
        for line in lines.iter().skip(self.consumed_lines) {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(text) = v.get("text").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
                    messages.push(text.to_string());
                    self.emit(json!({"type":"context_received", "message_id":v.get("message_id")}));
                }
            }
        }
        self.consumed_lines = lines.len();
        messages
    }

    fn ask(&mut self, questions: &[UserQuestion]) -> Vec<(String, String)> {
        let queued = self.take_pending_messages();
        if !queued.is_empty() { return Self::context_answers(queued.join("\n\n")); }
        let payload = json!({"questions":questions.iter().map(|q|json!({
            "id":q.id,"text":q.text,"kind":q.kind,"options":q.options
        })).collect::<Vec<_>>()});
        match self.request("question", payload) {
            Some(v) => Self::context_answers(v["text"].as_str().unwrap_or("").to_string()),
            None => vec![],
        }
    }

    fn present_checkpoint(&mut self, req: &CheckpointRequest) -> CheckpointAnswer {
        let queued = self.take_pending_messages();
        if !queued.is_empty() {
            return CheckpointAnswer::Revise { reason: queued.join("\n\n") };
        }
        let Some(v) = self.request("checkpoint", json!({
            "id":req.id.as_str(),"phase":req.phase.as_str(),"items":req.items
        })) else {
            return CheckpointAnswer::Revise { reason: "网页输入通道已关闭，未获得确认".into() };
        };
        let text = v["text"].as_str().unwrap_or("").to_string();
        match v["action"].as_str() {
            Some("approve") => CheckpointAnswer::Approve,
            Some("followup") => CheckpointAnswer::AskFollowUp { question: text },
            _ => CheckpointAnswer::Revise { reason: text },
        }
    }

    fn decide_rewind(&mut self, target: SessionPhase, reason: &str) -> RewindDecision {
        match self.request("rewind", json!({"target":target.as_str(),"reason":reason})) {
            Some(v) if v["action"].as_str() == Some("approve") => RewindDecision::Approve,
            _ => RewindDecision::Reject,
        }
    }

    fn inform(&mut self, message: &str) {
        self.emit(json!({"type":"message","text":message}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use shangyi_core::phase_graph::{CheckpointId, CheckpointItem};

    fn req() -> CheckpointRequest {
        CheckpointRequest { id: CheckpointId::Cp1, phase: SessionPhase::Intel,
            items: vec![CheckpointItem { id:"fact".into(),content:"核对事实".into(),
                source:Some("用户描述".into()),proposed_value:None }],revision_requested:false }
    }

    #[test]
    fn freeform_answer_preserves_multiline_without_fabricating_question_answers() {
        let input = json!({"request_id":"request-1","action":"message","text":"前面的估计不对\n我实际更关心现金流"}).to_string()+"\n";
        let mut io=WebIo::new(Cursor::new(input),Vec::new(),None);
        let questions=vec![UserQuestion{id:"q1".into(),text:"问题".into(),kind:"text".into(),options:vec![]}];
        let answer=io.ask(&questions);
        assert_eq!(answer.len(),1);
        assert!(answer[0].0.contains("不是逐项确认"));
        assert_eq!(answer[0].1,"前面的估计不对\n我实际更关心现金流");
        assert!(!io.is_automated());
    }

    #[test]
    fn stale_or_malformed_approvals_cannot_confirm_current_checkpoint() {
        let input="not json\n{\"request_id\":\"request-0\",\"action\":\"approve\"}\n{\"request_id\":\"request-1\",\"action\":\"unknown\"}\n";
        let mut io=WebIo::new(Cursor::new(input),Vec::new(),None);
        assert!(matches!(io.present_checkpoint(&req()),CheckpointAnswer::Revise{..}));
    }

    #[test]
    fn explicit_approval_and_followup_have_different_meanings() {
        for (action,text,approved) in [("approve","",true),("followup","来源是什么？",false)] {
            let line=json!({"request_id":"request-1","action":action,"text":text}).to_string()+"\n";
            let mut io=WebIo::new(Cursor::new(line),Vec::new(),None);
            let result=io.present_checkpoint(&req());
            assert_eq!(matches!(result,CheckpointAnswer::Approve),approved);
            if !approved { assert!(matches!(result,CheckpointAnswer::AskFollowUp{..})); }
        }
    }

    #[test]
    fn durable_response_survives_closed_stdin_and_does_not_replay_approval() {
        let dir=std::env::temp_dir().join(format!("shangyi-webio-{}",std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path=dir.join("responses.jsonl");
        let lines=[json!({"request_id":"request-0","action":"approve"}),
            json!({"request_id":"request-1","action":"followup","text":"仍需核对"}),
            json!({"request_id":"request-1","action":"approve"}),
            json!({"request_id":"request-2","action":"revise","text":"条件已改变"})]
            .iter().map(|v|v.to_string()+"\n").collect::<String>();
        std::fs::write(&path,lines).unwrap();
        let mut io=WebIo::new(Cursor::new(""),Vec::new(),None).with_responses(path);
        assert!(matches!(io.present_checkpoint(&req()),CheckpointAnswer::AskFollowUp{..}));
        assert!(matches!(io.present_checkpoint(&req()),CheckpointAnswer::Revise{..}));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rewind_without_response_is_rejected() {
        let mut io=WebIo::new(Cursor::new(""),Vec::new(),None);
        assert_eq!(io.decide_rewind(SessionPhase::Intake,"补充资料"),RewindDecision::Reject);
    }

    #[test]
    fn task_delivery_waits_for_mailbox_followup_without_checkpoint_or_finished_event() {
        let dir=std::env::temp_dir().join(format!("shangyi-task-webio-{}-{}",std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();let mailbox=dir.join("mailbox.jsonl");
        let mut io=WebIo::new(Cursor::new(""),Vec::new(),Some(mailbox.clone()));
        let result=ScopedTaskResult {
            scope:shangyi_core::task_scope::TaskScope::Research,status:shangyi_core::task_scope::TaskResultStatus::Complete,
            summary:"已整理资料。".into(),missing:vec![],source_urls:vec!["https://example.invalid/source".into()],retrieval_attempts:1,cost_scenarios:vec![],conditional:None,research:None,
        };
        io.inform(&result.summary);io.scoped_task_completed(&result);
        let writer=std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            std::fs::write(mailbox,"{\"message_id\":\"followup-1\",\"text\":\"再算一下费用\"}\n").unwrap();
        });
        assert_eq!(io.wait_for_followup_task(),Some("再算一下费用".into()));writer.join().unwrap();
        assert!(io.take_pending_messages().is_empty(),"followup must be consumed exactly once");
        let events:Vec<Value>=String::from_utf8(io.events).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(events.iter().map(|e|e["type"].as_str().unwrap()).collect::<Vec<_>>(),vec!["message","task_completed","context_received","task_started"]);
        assert_eq!(events[1]["awaiting_followup"],true);assert_eq!(events[1]["status"],"complete");
        assert_eq!(events[2]["message_id"],"followup-1");assert!(events[3]["scope"].is_null());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conditional_delivery_reports_its_actual_phase_and_waits_for_followup_without_approval() {
        use shangyi_core::conditional::{ConditionalSnapshot,ConditionalTrigger,EvidenceGap,EvidenceGapReason,build_conditional_result};
        let dir=std::env::temp_dir().join(format!("shangyi-web-conditional-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();let mailbox=dir.join("mailbox.jsonl");
        let result=build_conditional_result(&ConditionalSnapshot{question:"怎样谈续租？".into(),gaps:vec![EvidenceGap{reason:EvidenceGapReason::UnknownOpponentPayoff,participant_id:None,profile:None,detail:"对方收益未取得".into()}],..Default::default()},ConditionalTrigger::RepeatedNoProgress).unwrap();
        let mut io=WebIo::new(Cursor::new(""),Vec::new(),Some(mailbox.clone()));
        io.scoped_task_completed_at(&result,shangyi_core::SessionPhase::ModelBuild);
        let writer=std::thread::spawn(move||{std::thread::sleep(std::time::Duration::from_millis(100));std::fs::write(mailbox,"{\"message_id\":\"user-followup\",\"text\":\"我补充新的租金报价\"}\n").unwrap();});
        assert_eq!(io.wait_for_followup_task(),Some("我补充新的租金报价".into()));writer.join().unwrap();
        let events:Vec<Value>=String::from_utf8(io.events).unwrap().lines().map(|l|serde_json::from_str(l).unwrap()).collect();
        assert_eq!(events[0]["type"],"task_completed");assert_eq!(events[0]["scope"],"conditional");assert_eq!(events[0]["status"],"partial");
        assert_eq!(events[0]["phase"],"model_build");assert_eq!(events[0]["completed"],false);assert_eq!(events[0]["awaiting_followup"],true);
        assert!(events.iter().all(|e|e["type"]!="request"&&e["type"]!="finished"));
        assert_eq!(events.last().unwrap()["type"],"task_started");assert_eq!(events.last().unwrap()["phase"],"intake");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

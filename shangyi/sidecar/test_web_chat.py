"""Offline process/API tests; the child is a protocol fixture, not an LLM."""
import json
import os
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
from web_chat import ChatService, ChatError

ENGINE = r'''
import json,os,sys,time
if "--web-protocol-version" in sys.argv:
    print("1");sys.exit(0)
path=os.environ["SHANGYI_WEB_EVENTS_PATH"]
def emit(obj):
    with open(path,"a") as f:
        f.write(json.dumps(obj,ensure_ascii=False)+"\n");f.flush()
def ask(n,kind,payload):
    request_id="request-"+str(n)
    emit({"type":"request","request_id":request_id,"kind":kind,"payload":payload})
    line=sys.stdin.readline()
    if not line:sys.exit(0)
    v=json.loads(line)
    emit({"type":"response_received","request_id":request_id,"message_id":v.get("message_id")})
    return v
assert not os.environ.get("SHANGYI_ASSUME_YES")
emit({"type":"ready","protocol":1,"case_id":"case-20260921-scenario-001","date":"2026-09-21"})
time.sleep(.15)
mail=os.environ["SHANGYI_WEB_MAILBOX_PATH"]
if os.path.exists(mail):
    for line in open(mail):
        v=json.loads(line);emit({"type":"context_received","message_id":v["message_id"]})
question={"questions":[{"id":"missing-goal","text":"你最希望保住的是什么？","kind":"text","options":[]}]}
v=ask(1,"question",question)
emit({"type":"message","text":"已收到这段自由描述："+v["text"]})
items={"id":"cp1","phase":"intel","items":[{"id":"f1","content":"这只是协议测试，不是实际分析","source":"测试输入","proposed_value":None}]}
v=ask(2,"checkpoint",items)
if v["action"]!="approve":
    emit({"type":"message","text":"已根据你的补充重新核对。"})
    v=ask(3,"checkpoint",items)
emit({"type":"finished","completed":v["action"]=="approve","phase":"archived","turns":2})
'''

class ChatProtocolTests(unittest.TestCase):
    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory()
        self.folder=Path(self.tmp.name)
        self.bin=self.folder/"engine"
        self.bin.write_text("#!"+sys.executable+"\n"+ENGINE)
        self.bin.chmod(0o755)
        self.service=ChatService(lambda:str(self.bin),lambda:self.folder,
            lambda:[{"id":x} for x in ["pricing_response","negotiation","market_entry","competitor_intel"]])

    def tearDown(self):
        for e in self.service.entries.values():
            p=e.get("proc")
            if p is not None:
                self.service.stop(e["id"])
                # Some tests deliberately report completion before the fixture exits.
                if p.poll() is None:
                    p.terminate()
                p.wait(timeout=3)
                if p.stdin:
                    p.stdin.close()
        self.tmp.cleanup()

    def start(self,key="start-12345678"):
        return self.service.start({"text":"续租谈判，我更重视灵活退出","message_id":key})

    def waiting(self,id,request="request-1"):
        until=time.monotonic()+3
        while time.monotonic()<until:
            d=self.service.get(id)
            if (d.get("pending") or {}).get("request_id")==request:return d
            time.sleep(.02)
        self.fail("No pending request: "+str(d))

    def test_live_cost_scenarios_merge_with_case_without_dropping_prior_results(self):
        chat_id = "chat-" + "a" * 24
        entry = {"id": chat_id, "dir": self.folder, "case": "2026-10-02/case-20261002-costs",
                 "lock": threading.RLock()}
        self.service.entries[chat_id] = entry
        old = {"listed_upfront_cash": 10}
        first = {"scenario": "A店", "listed_upfront_cash": 100}
        second = {"scenario": "B店", "listed_upfront_cash": 200}
        self.service.project_case = Mock(return_value={"artifacts": {
            "business_costs": [old, first, second], "business_cost": second}})
        outputs = [{"scenario": "B店", "listed_upfront_cash": 250},
                   {"scenario": "C店", "listed_upfront_cash": 300},
                   {"scenario": "B店", "listed_upfront_cash": 275}]
        events = []
        for i, output in enumerate(outputs):
            events.extend([
                {"type": "tool_call_started", "call_id": str(i), "name": "business_cost"},
                {"type": "tool_call_finished", "call_id": str(i), "outcome": {
                    "status": "ok", "output": json.dumps(output)}},
            ])
        (self.folder / "engine-progress.jsonl").write_text(''.join(json.dumps(e) + '\n' for e in events))
        with patch.object(self.service, "_summary", return_value={"id": chat_id, "alive": False, "status": "stopped"}):
            result = self.service.get(chat_id)
        self.assertEqual([c.get("scenario") for c in result["business_costs"]], [None, "A店", "B店", "C店"])
        self.assertEqual([c["listed_upfront_cash"] for c in result["business_costs"]], [10, 100, 275, 300])
        self.assertEqual(result["business_cost"], outputs[-1])
        self.assertEqual(second["listed_upfront_cash"], 200)

    def test_legacy_single_cost_artifact_still_projects_as_one_scenario(self):
        chat_id = "chat-" + "b" * 24
        self.service.entries[chat_id] = {"id": chat_id, "dir": self.folder,
            "case": "2026-10-02/case-20261002-costs", "lock": threading.RLock()}
        legacy = {"listed_upfront_cash": 98000}
        self.service.project_case = Mock(return_value={"artifacts": {"business_cost": legacy}})
        with patch.object(self.service, "_summary", return_value={"id": chat_id, "alive": False, "status": "stopped"}):
            result = self.service.get(chat_id)
        self.assertEqual(result["business_costs"], [legacy])
        self.assertNotIn("cash_classification_complete", result["business_cost"])

    def test_freeform_response_stale_protection_and_idempotency(self):
        c=self.start();id=c["id"]
        self.assertEqual(c["topic"],"negotiation")
        self.assertEqual(self.start()["id"],id)
        self.waiting(id)
        with self.assertRaises(ChatError):
            self.service.reply(id,{"request_id":"old","action":"approve","message_id":"bad-12345678"})
        message={"request_id":"request-1","action":"message","text":"前面说错了\n我在意的是退出条件","message_id":"reply-12345678"}
        self.service.reply(id,message)
        self.service.reply(id,message)
        d=self.waiting(id,"request-2")
        users=[e for e in d["messages"] if e["type"]=="user"]
        self.assertEqual(len(users),2)
        self.assertEqual(users[-1]["text"],message["text"])
        self.assertEqual(users[-1]["delivery"],"received")
        self.service.reply(id,{"request_id":"request-2","action":"followup","text":"请说明来源","message_id":"follow-12345678"})
        d=self.waiting(id,"request-3")
        self.assertEqual(d["status"],"waiting")
        self.service.reply(id,{"request_id":"request-3","action":"approve","text":"","message_id":"approve-12345678"})
        until=time.monotonic()+3
        while time.monotonic()<until:
            d=self.service.get(id)
            if d["status"]=="completed":break
            time.sleep(.02)
        self.assertEqual(d["status"],"completed")

    def test_queued_context_is_preserved_and_acknowledged(self):
        c=self.start()
        self.service.reply(c["id"],{"action":"message","text":"补充：还有另一种选择","message_id":"queue-12345678"})
        d=self.waiting(c["id"])
        user=[e for e in d["messages"] if e["type"]=="user"][-1]
        self.assertEqual(user["delivery"],"received")

    def test_restart_preserves_transcript_without_claiming_live_connection(self):
        c=self.start();d=self.waiting(c["id"])
        restored=ChatService(lambda:str(self.bin),lambda:self.folder,self.service.playbooks)
        listed=restored.list()["chats"][0]
        snap=restored.get(c["id"])
        self.assertEqual(listed, {k:snap[k] for k in listed})
        if self.service._protocol_version == "2":
            self.assertEqual(snap["status"], "waiting")
            self.assertTrue(snap["alive"])
            self.assertEqual(snap["pending"]["kind"], "question")
            return
        self.assertEqual(snap["status"],"interrupted")
        self.assertIsNone(snap["pending"])
        self.assertTrue(snap["messages"])
        with self.assertRaises(ChatError):
            restored.reply(c["id"],{"action":"message","text":"不能丢失","message_id":"restart-12345678"})

    def test_unknown_binary_cannot_silently_fall_back_to_automatic_approvals(self):
        self.bin.write_text("#!"+sys.executable+"\nimport sys\nsys.exit(2)\n")
        with self.assertRaises(ChatError):
            self.start()

    def test_finished_event_closes_input_before_process_reaping(self):
        c=self.start();self.waiting(c["id"])
        self.service.entries[c["id"]]["finished"]={"completed":True}
        d=self.service.get(c["id"])
        self.assertEqual(d["status"],"completed")
        self.assertFalse(d["alive"])
        self.assertIsNone(d["pending"])

    def test_stop_is_idempotent_and_list_reflects_the_stop(self):
        c=self.start();self.waiting(c["id"])
        self.assertEqual(self.service.stop(c["id"]), {"ok":True})
        self.service.entries[c["id"]]["proc"].wait(timeout=3)
        state=self.service.root/c["id"]/"state.json"
        stopped=state.read_bytes()
        self.assertEqual(self.service.stop(c["id"]), {"ok":True})
        self.assertEqual(state.read_bytes(), stopped)
        summary=self.service.list()["chats"][0]
        self.assertEqual(summary["status"], "stopped")
        self.assertFalse(summary["alive"])

    def test_stale_message_key_cannot_be_reused_for_different_text(self):
        c=self.start();self.waiting(c["id"])
        v={"request_id":"request-1","action":"message","text":"第一个回答","message_id":"same-12345678"}
        self.service.reply(c["id"],v)
        with self.assertRaises(ChatError):
            self.service.reply(c["id"],{**v,"text":"另一个回答"})

class DurableChatTests(ChatProtocolTests):
    def setUp(self):
        super().setUp()
        durable = ENGINE.replace('print("1")', 'print("2")')
        durable = durable.replace('path=os.environ["SHANGYI_WEB_EVENTS_PATH"]', '''path=os.environ["SHANGYI_WEB_EVENTS_PATH"]
import threading
from pathlib import Path
folder=Path(path).parent
def heartbeat():
    while True:
        if (folder/"stop.request").exists(): os._exit(130)
        (folder/"heartbeat.tmp").write_text(str(time.time()))
        os.replace(folder/"heartbeat.tmp",folder/"heartbeat")
        time.sleep(.05)
threading.Thread(target=heartbeat,daemon=True).start()
def durable_read(request_id):
    while True:
        p=Path(os.environ["SHANGYI_WEB_RESPONSES_PATH"])
        if p.exists():
            for line in p.read_text().splitlines():
                if json.loads(line).get("request_id")==request_id:return line
        time.sleep(.02)
''').replace('line=sys.stdin.readline()', 'line=durable_read(request_id)')
        self.bin.write_text("#!"+sys.executable+"\n"+durable)

    def test_service_restart_reconnects_without_replaying_approval(self):
        c=self.start();id=c['id'];self.waiting(id)
        resumed=ChatService(lambda:str(self.bin),lambda:self.folder,self.service.playbooks)
        listed=resumed.list()['chats'][0]
        current=resumed.get(id)
        self.assertEqual(listed, {k:current[k] for k in listed})
        self.assertTrue(current['alive'])
        self.assertEqual(current['pending']['kind'],'question')
        resumed.reply(id,{'request_id':'request-1','action':'message','text':'未知部分保持未知', 'message_id':'answer-recovered-1'})
        until=time.monotonic()+3
        while time.monotonic()<until:
            current=resumed.get(id)
            if (current.get('pending') or {}).get('request_id')=='request-2':break
            time.sleep(.02)
        self.assertEqual(current['pending']['kind'],'checkpoint')
        self.assertFalse(current['status']=='completed')
        resumed.stop(id)
        self.service.entries[id]['proc'].wait(timeout=3)
        self.assertFalse(resumed.get(id)['alive'])

    def test_stale_heartbeat_does_not_claim_recovery(self):
        c=self.start();id=c['id'];self.waiting(id)
        self.service.stop(id);self.service.entries[id]['proc'].wait(timeout=3)
        state=self.folder/'web-conversations'/id/'state.json'
        data=json.loads(state.read_text());data['stopped']=False;state.write_text(json.dumps(data))
        (state.parent/'heartbeat').write_text(str(time.time()-30))
        resumed=ChatService(lambda:str(self.bin),lambda:self.folder,self.service.playbooks)
        self.assertEqual(resumed.list()['chats'][0]['status'],'interrupted')
        self.assertEqual(resumed.get(id)['status'],'interrupted')


class ChatSummaryTests(unittest.TestCase):
    """Snapshot contracts; actual child-process/restart behavior is exercised above."""

    def setUp(self):
        self.tmp=tempfile.TemporaryDirectory()
        self.folder=Path(self.tmp.name)
        self.service=ChatService(lambda:None,lambda:self.folder,lambda:[])

    def tearDown(self):
        self.tmp.cleanup()

    def entry(self, number=1, **changes):
        chat_id="chat-"+format(number,"024x")
        folder=self.service.root/chat_id
        folder.mkdir(parents=True)
        entry=dict(id=chat_id, dir=folder, created_at=number, topic="negotiation",
                   mode="scenario", initial="讨论租约", offset=0, case=None,
                   finished=None, stopped=False, pending=None, replies={},
                   durable_input=False, restored=False, proc=None, lock=threading.RLock())
        entry.update(changes)
        self.service.entries[chat_id]=entry
        self.service._write_state(entry)
        return entry

    def events(self, entry, *events):
        (entry["dir"]/"engine-events.jsonl").write_text(
            "".join(json.dumps(e,ensure_ascii=False)+"\n" for e in events),encoding="utf-8")

    def test_list_keeps_full_initial_question_but_omits_transcripts_and_artifacts(self):
        self.entry(1,finished={"completed":True})
        initial="原始问题"*30+"需要搜索到的末尾"
        entry=self.entry(2,initial=initial,mode="whatif",proc=Mock(poll=Mock(return_value=None)))
        self.events(entry,
            {"type":"ready","date":"2026-10-02","case_id":"case-list-test"},
            {"type":"request","request_id":"request-1","kind":"question","payload":{}})
        self.service.project_case=Mock(side_effect=AssertionError("list projected case artifacts"))
        with patch("web_chat.lines",side_effect=AssertionError("list loaded transcript")), \
             patch.object(self.service,"_progress",side_effect=AssertionError("list loaded research progress")):
            result=self.service.list()
        self.assertTrue(result["ok"])
        self.assertEqual([s["created_at"] for s in result["chats"]],[2,1])
        self.assertEqual(result["chats"][0],{
            "id":entry["id"],"title":initial[:100],"initial":initial,
            "topic":"negotiation","created_at":2,"mode":"whatif",
            "status":"waiting","alive":True,"case":"2026-10-02/case-list-test",
            "task_scope":None,"task_status":None,"awaiting_followup":False})
        self.service.project_case.assert_not_called()

    def test_list_and_detail_agree_for_all_process_states(self):
        states=[
            ("thinking",{"proc":Mock(poll=Mock(return_value=None))}),
            ("waiting",{"proc":Mock(poll=Mock(return_value=None)),"pending":{"request_id":"r1"}}),
            ("completed",{"finished":{"completed":True}}),
            ("blocked",{"finished":{"completed":False}}),
            ("stopped",{"stopped":True}),
            ("failed",{"proc":Mock(poll=Mock(return_value=1))}),
            ("interrupted",{"restored":True}),
        ]
        expected={}
        for number,(status,changes) in enumerate(states,1):
            entry=self.entry(number,**changes)
            expected[entry["id"]]=status
        for summary in self.service.list()["chats"]:
            with self.subTest(status=expected[summary["id"]]):
                self.assertEqual(summary["status"],expected[summary["id"]])
                self.assertEqual(summary["alive"],summary["status"] in ("thinking","waiting"))
                detail=self.service.get(summary["id"])
                self.assertEqual(summary,{k:detail[k] for k in summary})

    def test_list_observes_finish_before_detail_is_opened(self):
        entry=self.entry(proc=Mock(poll=Mock(return_value=None)),pending={"request_id":"r1"})
        self.events(entry,{"type":"finished","completed":True})
        summary=self.service.list()["chats"][0]
        self.assertEqual(summary["status"],"completed")
        self.assertFalse(summary["alive"])
        self.assertIsNone(self.service.get(entry["id"])["pending"])

    def test_scoped_result_is_delivered_without_closing_input_and_followup_clears_it(self):
        entry=self.entry(durable_input=True,pending={"request_id":"old"})
        (entry["dir"]/"heartbeat").write_text(str(time.time()))
        completion={"type":"task_completed","scope":"cost","status":"partial",
                    "summary":"只计算已知费用。","missing":["押金金额未取得"],
                    "awaiting_followup":True,"phase":"intake"}
        self.events(entry,{"type":"message","text":completion["summary"]},completion)
        first=self.service.get(entry["id"])
        self.assertEqual(first["status"],"partial")
        self.assertTrue(first["alive"])
        self.assertTrue(first["awaiting_followup"])
        self.assertEqual(first["task_result"],completion)
        self.assertIsNone(first["pending"])
        self.assertIsNone(entry["finished"])
        summary=self.service.list()["chats"][0]
        self.assertNotIn("task_result",summary,"history list must not load the full result")
        self.assertEqual(summary["task_status"],"partial")
        self.service.reply(entry["id"],{"message_id":"followup-12345678","action":"message",
                                        "text":"押金已确认是18000元","request_id":None})
        envelope=json.loads((entry["dir"]/"mailbox.jsonl").read_text())
        self.assertEqual(envelope["text"],"押金已确认是18000元")
        with (entry["dir"]/"engine-events.jsonl").open("a") as stream:
            for event in ({"type":"context_received","message_id":"followup-12345678"},
                          {"type":"task_started","phase":"intake","scope":None}):
                stream.write(json.dumps(event)+"\n")
        next_task=self.service.get(entry["id"])
        self.assertEqual(next_task["status"],"thinking")
        self.assertFalse(next_task["awaiting_followup"])
        self.assertIsNone(next_task["task_result"])
        self.assertIsNone(next_task["task_scope"])
        self.assertEqual(next_task["messages"][-3]["delivery"],"received")

    def test_scoped_completion_survives_sidecar_reload_without_becoming_full_archived(self):
        entry=self.entry(durable_input=True)
        (entry["dir"]/"heartbeat").write_text(str(time.time()))
        self.events(entry,{"type":"task_completed","scope":"research","status":"complete",
                           "summary":"资料已整理","phase":"intake","missing":[]})
        self.assertEqual(self.service.get(entry["id"])["status"],"completed")
        restored=ChatService(lambda:None,lambda:self.folder,lambda:[])
        result=restored.get(entry["id"])
        self.assertTrue(result["alive"])
        self.assertEqual(result["task_scope"],"research")
        self.assertEqual(result["task_result"]["phase"],"intake")
        self.assertIsNone(restored.entries[entry["id"]]["finished"])
        restored.stop(entry["id"])
        self.assertEqual(restored.get(entry["id"])["status"],"completed")
        self.assertFalse(restored.get(entry["id"])["alive"])
        self.assertEqual(restored.get(entry["id"])["task_result"]["summary"],"资料已整理")
        self.assertTrue((entry["dir"]/"stop.request").exists())

    def test_stale_stop_preserves_completed_or_blocked_outcomes(self):
        for number,completed in enumerate((True,False),1):
            with self.subTest(completed=completed):
                entry=self.entry(number,durable_input=True,proc=Mock(poll=Mock(return_value=None)))
                self.events(entry,{"type":"finished","completed":completed})
                with patch("web_chat.os.killpg") as terminate:
                    self.assertEqual(self.service.stop(entry["id"]),{"ok":True})
                    persisted=(entry["dir"]/"state.json").read_bytes()
                    self.service.stop(entry["id"])
                terminate.assert_not_called()
                self.assertFalse((entry["dir"]/"stop.request").exists())
                self.assertEqual((entry["dir"]/"state.json").read_bytes(),persisted)
                self.assertFalse(json.loads(persisted)["stopped"])
                self.assertEqual(self.service.get(entry["id"])["status"],"completed" if completed else "blocked")

    def test_stop_does_not_relabel_an_interrupted_conversation(self):
        entry=self.entry(restored=True,durable_input=True)
        persisted=(entry["dir"]/"state.json").read_bytes()
        self.service.stop(entry["id"])
        self.assertEqual((entry["dir"]/"state.json").read_bytes(),persisted)
        self.assertFalse((entry["dir"]/"stop.request").exists())
        self.assertEqual(self.service.get(entry["id"])["status"],"interrupted")


if __name__=="__main__":
    unittest.main()

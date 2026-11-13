"""Live browser conversations over the CLI's structured input/event channel."""
from __future__ import annotations

import json
import os
import re
import signal
import subprocess
import threading
import time
import uuid
from pathlib import Path
from web_source_projection import WebSources
from business_cost_projection import cost_scenarios, merge_cost_scenarios, record_business_cost
from frame_projection import decision_frame

CHAT_RE = re.compile(r"^chat-[a-f0-9]{24}$")
MAX_TEXT = 24000


class ChatError(ValueError):
    pass


def read_json(path, default=None):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return default


def lines(path):
    try:
        return [json.loads(s) for s in path.read_text(encoding="utf-8").splitlines() if s.strip()]
    except (OSError, ValueError):
        return []


def clean_text(value):
    if not isinstance(value, str) or not value.strip():
        raise ChatError("请写下想讨论的事情。")
    if len(value) > MAX_TEXT:
        raise ChatError("这段内容过长，请分几次补充。")
    return value.strip()


def choose_topic(text):
    """Explicitly heuristic routing; the UI lets the user override the topic."""
    for key, words in (
        ("negotiation", ("谈判", "议价", "租金", "租约", "合同", "供应商", "续租")),
        ("market_entry", ("开店", "新市场", "进入市场", "入驻", "扩张", "第二家")),
        ("pricing_response", ("降价", "涨价", "调价", "定价", "促销", "折扣")),
    ):
        if any(word in text for word in words):
            return key
    return "competitor_intel"


class ChatService:
    def __init__(self, binary, home, playbooks, project_case=None):
        self.project_case = project_case
        self.binary = binary
        self.home = home
        self.playbooks = playbooks
        self.entries = {}
        self.lock = threading.RLock()
        self._capability_cache = None
        self._protocol_version = None

    @property
    def root(self):
        return Path(self.home()) / "web-conversations"

    def capable(self):
        binary = self.binary()
        if not binary:
            return False
        try:
            stat = Path(binary).stat()
            key = (binary, stat.st_mtime_ns, stat.st_size)
            if self._capability_cache and self._capability_cache[0] == key:
                return self._capability_cache[1]
            p = subprocess.run([binary, "--web-protocol-version"],
                               capture_output=True, text=True, timeout=5)
            ready = p.returncode == 0 and p.stdout.strip() in ("1", "2")
            self._protocol_version = p.stdout.strip() if ready else None
            self._capability_cache = (key, ready)
            return ready
        except (OSError, subprocess.TimeoutExpired):
            return False

    def _write_state(self, entry):
        keys = ("id", "created_at", "topic", "mode", "action", "initial",
                "start_key", "offset", "case", "finished", "stopped", "replies", "pending", "durable_input",
                "task_result", "task_scope")
        state = {k: entry.get(k) for k in keys}
        target = entry["dir"] / "state.json"
        temp = target.with_suffix(".tmp")
        temp.write_text(json.dumps(state, ensure_ascii=False), encoding="utf-8")
        temp.replace(target)

    def _append(self, entry, event):
        event = {"at": time.time(), **event}
        with (entry["dir"] / "conversation.jsonl").open("a", encoding="utf-8") as f:
            f.write(json.dumps(event, ensure_ascii=False) + "\n")
            f.flush()
        return event

    def _load(self, chat_id):
        if not CHAT_RE.fullmatch(chat_id):
            raise ChatError("咨询标识无效。")
        if chat_id not in self.entries:
            folder = self.root / chat_id
            state = read_json(folder / "state.json")
            if not state:
                raise ChatError("这次咨询不存在。")
            state.update(dir=folder, proc=None, lock=threading.RLock(), restored=True)
            self.entries[chat_id] = state
        return self.entries[chat_id]

    def start(self, body):
        text = clean_text(body.get("text"))
        key = body.get("message_id")
        if not isinstance(key, str) or not re.fullmatch(r"[a-zA-Z0-9-]{8,80}", key):
            raise ChatError("缺少有效的消息标识，请刷新页面后重试。")
        topic = body.get("playbook") or choose_topic(text)
        if topic not in {p["id"] for p in self.playbooks()}:
            raise ChatError("所选咨询主题不可用。")
        mode = body.get("mode", "scenario")
        if mode not in ("scenario", "whatif", "discover"):
            raise ChatError("咨询目标无效。")
        action = str(body.get("action") or "").strip() if mode == "whatif" else ""
        if mode == "whatif" and not action:
            raise ChatError("请补充希望推演的具体动作。")
        with self.lock:
            self.root.mkdir(parents=True, exist_ok=True)
            for file in self.root.glob("chat-*/state.json"):
                state = read_json(file, {})
                if state.get("start_key") == key:
                    if (state.get("initial"), state.get("topic"), state.get("mode"), state.get("action")) != (text, topic, mode, action):
                        raise ChatError("消息标识已用于不同内容，请重新发送。")
                    return self.get(state["id"])
            if not self.capable():
                raise ChatError("当前分析引擎尚未更新到实时对话版本，请先重启最新服务。")
            chat_id = "chat-" + uuid.uuid4().hex[:24]
            folder = self.root / chat_id
            folder.mkdir()
            entry = dict(id=chat_id, dir=folder, created_at=time.time(), topic=topic,
                         mode=mode, action=action, initial=text, start_key=key,
                         offset=0, case=None, pending=None, finished=None,
                         stopped=False, replies={}, proc=None, restored=False, durable_input=self._protocol_version == "2",
                         task_result=None, task_scope=None,
                         lock=threading.RLock())
            self.entries[chat_id] = entry
            self._append(entry, {"type": "user", "text": text, "message_id": key, "delivery": "initial"})
            self._write_state(entry)
            env = dict(os.environ)
            env.pop("SHANGYI_ASSUME_YES", None)
            env.pop("SHANGYI_SKIP_PREFLIGHT", None)
            env["SHANGYI_HOME"] = str(self.home())
            env["SHANGYI_WEB_BRIEF"] = text
            env["SHANGYI_WEB_EVENTS_PATH"] = str(folder / "engine-events.jsonl")
            env["SHANGYI_WEB_PROGRESS_PATH"] = str(folder / "engine-progress.jsonl")
            env["SHANGYI_WEB_MAILBOX_PATH"] = str(folder / "mailbox.jsonl")
            if entry["durable_input"]:
                env["SHANGYI_WEB_RESPONSES_PATH"] = str(folder / "responses.jsonl")
            cmd = [self.binary(), "new", "--mode", mode, "--playbook", topic]
            if action:
                cmd += ["--action", action]
            try:
                with (folder / "engine.log").open("a", encoding="utf-8") as log:
                    entry["proc"] = subprocess.Popen(cmd, stdin=subprocess.PIPE, stdout=log,
                        stderr=subprocess.STDOUT, text=True, encoding="utf-8", bufsize=1,
                        start_new_session=True, env=env,
                        cwd=Path(__file__).resolve().parents[2])
            except OSError as exc:
                entry["finished"] = {"completed": False}
                self._append(entry, {"type": "error", "text": "分析进程未能启动：" + str(exc)})
                self._write_state(entry)
                raise ChatError("分析未能启动，原始描述已经保存。") from exc
            return self.get(chat_id)

    def _sync(self, entry):
        path = entry["dir"] / "engine-events.jsonl"
        if not path.exists():
            return
        changed = False
        with path.open("r", encoding="utf-8") as f:
            f.seek(entry["offset"])
            while True:
                line = f.readline()
                if not line or not line.endswith("\n"):
                    break
                entry["offset"] = f.tell()
                try:
                    event = json.loads(line)
                except ValueError:
                    continue
                kind = event.get("type")
                if kind == "request":
                    entry["pending"] = event
                    entry["task_result"] = None
                elif kind == "response_received":
                    if (entry.get("pending") or {}).get("request_id") == event.get("request_id"):
                        entry["pending"] = None
                elif kind == "ready":
                    date, case_id = event.get("date", ""), event.get("case_id", "")
                    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", date) and re.fullmatch(r"case-[0-9a-z-]+", case_id):
                        entry["case"] = date + "/" + case_id
                elif kind == "finished":
                    entry["finished"] = event
                    entry["pending"] = None
                elif kind == "task_scope_selected":
                    entry["task_scope"] = event.get("scope")
                elif kind == "task_completed":
                    entry["task_result"] = event
                    entry["task_scope"] = event.get("scope")
                    entry["pending"] = None
                elif kind == "task_started":
                    entry["task_result"] = None
                    entry["task_scope"] = None
                    entry["pending"] = None
                self._append(entry, event)
                changed = True
        if changed:
            self._write_state(entry)

    def _progress(self, entry):
        path=entry["dir"] / "engine-progress.jsonl"
        result={"status":"", "artifacts":{}}
        if not path.exists(): return result
        calls={}
        sources=WebSources()
        labels={"web_search":"正在检索公开资料", "web_fetch":"正在核对原文", "research_record":"正在整理资料依据", "business_cost":"正在计算经营成本", "poi_survey":"正在查周边门店"}
        with path.open(encoding="utf-8") as f:
            for line in f:
                if not line.endswith("\n"): break
                e=json.loads(line)
                sources.feed(e)
                if e.get("type")=="step_started": result["status"]="正在分析已有信息"
                if e.get("type")=="tool_call_started":
                    calls[e["call_id"]]=e["name"]
                    result["status"]=labels.get(e["name"], "正在执行分析步骤")
                if e.get("type")=="tool_call_finished":
                    name=calls.get(e["call_id"])
                    outcome=e.get("outcome",{})
                    if outcome.get("status")=="ok" and name in ("research_record","business_cost","poi_survey"):
                        artifact=json.loads(outcome["output"])
                        if name=="business_cost":
                            record_business_cost(result["artifacts"], artifact)
                        elif name=="poi_survey":
                            result["artifacts"].setdefault("poi_surveys", []).append(artifact)
                        else:
                            result["artifacts"]["research"]=artifact
                    result["status"]="已取得结果，正在整理" if outcome.get("status")=="ok" else "部分步骤未完成，正在检查原因"
        if sources.records():result["artifacts"]["web_sources"]=sources.records()
        return result

    def _alive(self, entry):
        if entry.get("finished") or entry.get("stopped"):
            return False
        proc = entry.get("proc")
        if proc is not None:
            return proc.poll() is None
        if entry.get("durable_input"):
            try:
                stamp = float((entry["dir"] / "heartbeat").read_text())
                return 0 <= time.time() - stamp < 5
            except FileNotFoundError:
                return False
            except (ValueError, OSError):
                return False
        return False

    def _summary(self, entry):
        """Synchronize status under the entry lock without projecting case artifacts."""
        self._sync(entry)
        alive = self._alive(entry)
        if entry.get("stopped"):
            result = entry.get("task_result")
            status = ("partial" if result.get("status") == "partial" else "completed") if result else "stopped"
        elif entry.get("finished"):
            status = "completed" if entry["finished"].get("completed") else "blocked"
        elif alive:
            result = entry.get("task_result")
            status = ("partial" if result.get("status") == "partial" else "completed") if result else ("waiting" if entry.get("pending") else "thinking")
        else:
            status = "interrupted" if entry.get("restored") else "failed"
        initial = entry.get("initial", "")
        return {"id": entry["id"], "title": initial[:100], "initial": initial,
                "created_at": entry.get("created_at", 0), "topic": entry.get("topic"),
                "mode": entry.get("mode"), "status": status, "alive": alive,
                "task_scope": entry.get("task_scope"),
                "task_status": (entry.get("task_result") or {}).get("status"),
                "awaiting_followup": bool(alive and entry.get("task_result")),
                "case": entry.get("case")}

    def get(self, chat_id):
        entry = self.entries.get(chat_id)
        if entry is None:
            with self.lock:
                entry = self._load(chat_id)
        with entry["lock"]:
            summary = self._summary(entry)
            alive, status = summary["alive"], summary["status"]
            pending = entry.get("pending") if alive else None
            messages = lines(entry["dir"] / "conversation.jsonl")
            received = {e.get("message_id") for e in messages if e.get("type") in ("context_received", "response_received")}
            for e in messages:
                if e.get("type") == "user" and e.get("delivery") != "initial":
                    e["delivery"] = "received" if e.get("message_id") in received else ("queued" if alive else "unprocessed")
            log_tail = ""
            if status in ("failed", "blocked", "interrupted"):
                try:
                    log_tail = (entry["dir"] / "engine.log").read_text(encoding="utf-8")[-6000:]
                except OSError:
                    pass
            artifacts = {}
            if entry.get("case") and self.project_case:
                projected = self.project_case(*entry["case"].split("/", 1))
                artifacts = dict((projected or {}).get("artifacts", {}))
            live = self._progress(entry)
            costs = merge_cost_scenarios(cost_scenarios(artifacts), cost_scenarios(live["artifacts"]))
            artifacts.update(live["artifacts"])
            # 显式决策帧：把「机器现在怎么理解这个问题」摆到「看依据」里可核对。
            frame = decision_frame(
                question=summary.get("initial") or "", topic=summary.get("topic"), mode=summary.get("mode"),
                scope=summary.get("task_scope"), action=entry.get("action") or None,
                artifacts=artifacts, missing=(entry.get("task_result") or {}).get("missing"))
            return {"ok": True, **summary,
                    "task_result": entry.get("task_result"),
                    "progress": live["status"],
                    "research": artifacts.get("research", {}).get("records", []),
                    "poi_surveys": artifacts.get("poi_surveys", []),
                    "web_sources": artifacts.get("web_sources", []),
                    "business_cost": artifacts.get("business_cost"),
                    "business_costs": costs,
                    "frame": frame,
                    "pending": pending, "messages": messages,
                    "error_detail": log_tail, "revision": len(messages)}

    def reply(self, chat_id, body):
        with self.lock:
            entry = self._load(chat_id)
        with entry["lock"]:
            self._sync(entry)
            key = body.get("message_id")
            if not isinstance(key, str) or not re.fullmatch(r"[a-zA-Z0-9-]{8,80}", key):
                raise ChatError("消息标识无效。")
            existing = entry["replies"].get(key)
            payload = {k: body.get(k) for k in ("request_id", "action", "text")}
            if existing is not None:
                if existing != payload:
                    raise ChatError("这个消息标识已用于其他内容。")
                return self.get(chat_id)
            proc = entry["proc"]
            if not self._alive(entry):
                raise ChatError("本轮已经结束，不能继续发送；已有内容已保留。")
            pending = entry.get("pending")
            action = body.get("action", "message")
            text = body.get("text", "")
            request_id = body.get("request_id")
            if request_id:
                if not pending or pending["request_id"] != request_id:
                    raise ChatError("当前问题已经更新，请查看最新消息后再发送。")
                kind = pending["kind"]
                allowed = {"question": {"message"}, "checkpoint": {"approve", "revise", "followup"}, "rewind": {"approve", "reject"}}
                if action not in allowed.get(kind, set()):
                    raise ChatError("此操作不适用于当前问题。")
                if action in ("message", "revise", "followup"):
                    text = clean_text(text)
                envelope = dict(request_id=request_id, action=action, text=text, message_id=key)
                try:
                    if entry.get("durable_input"):
                        with (entry["dir"] / "responses.jsonl").open("a", encoding="utf-8") as f:
                            f.write(json.dumps(envelope, ensure_ascii=False) + "\n")
                            f.flush()
                            os.fsync(f.fileno())
                    else:
                        proc.stdin.write(json.dumps(envelope, ensure_ascii=False) + "\n")
                        proc.stdin.flush()
                except (BrokenPipeError, OSError, ValueError) as exc:
                    raise ChatError("连接已中断，内容仍在输入框中。") from exc
                entry["pending"] = None
                delivery = "submitted"
            else:
                # A correction typed during a model turn is delivered at the next safe boundary.
                if action != "message":
                    raise ChatError("确认操作必须对应具体的待确认内容。")
                text = clean_text(text)
                if pending:
                    raise ChatError("当前有待回复内容，请刷新状态后回应。")
                with (entry["dir"] / "mailbox.jsonl").open("a", encoding="utf-8") as f:
                    f.write(json.dumps({"message_id":key,"text":text}, ensure_ascii=False) + "\n")
                    f.flush()
                delivery = "queued"
            display = text or {"approve":"确认以上内容", "reject":"保留当前结果，不回退"}.get(action, "")
            self._append(entry, {"type":"user","text":display,"action":action,
                                  "request_id":request_id,"message_id":key,"delivery":delivery})
            entry["replies"][key] = payload
            self._write_state(entry)
            return self.get(chat_id)

    def stop(self, chat_id):
        with self.lock:
            entry = self._load(chat_id)
        with entry["lock"]:
            # A finish event can arrive after the browser rendered its stop button.
            # Repeated or stale stop requests must preserve the terminal outcome.
            if not self._summary(entry)["alive"]:
                return {"ok": True}
            proc = entry["proc"]
            if entry.get("durable_input"):
                (entry["dir"] / "stop.request").write_text("stop", encoding="utf-8")
            if proc and proc.poll() is None:
                try:
                    os.killpg(proc.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    proc.stdin.close()
                except OSError:
                    pass
            entry["stopped"] = True
            self._write_state(entry)
            return {"ok": True}

    def list(self):
        if not self.root.exists():
            return {"ok":True,"chats":[]}
        chats=[]
        for path in self.root.glob("chat-*/state.json"):
            state=read_json(path, {})
            if state.get("id") and CHAT_RE.fullmatch(state["id"]):
                with self.lock:
                    entry = self._load(state["id"])
                with entry["lock"]:
                    chats.append(self._summary(entry))
        return {"ok":True,"chats":sorted(chats,key=lambda c:c["created_at"],reverse=True)}

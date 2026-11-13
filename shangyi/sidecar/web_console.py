#!/usr/bin/env python3
"""商奕控制台 sidecar：把 `shangyi` CLI 与 `~/.shangyi` 案例数据接成 localhost Web API。

# 定位（与 data_ingest.py 同一 sidecar 模式）

**纯标准库，零第三方依赖**——与 `data_ingest.py` 的约束一致：
sidecar 是薄桥，不是服务框架。渲染 PDF/docx 那样的重活才交给外部工具；
本文件只做三件事：

1. **静态服务**：`web/` 目录下的前端（单页应用，无构建步骤）；
2. **读 API**：扫描 `$SHANGYI_HOME/cases/`（header.json + case.jsonl + library.json），
   还原案例的时间线与各阶段产物。**不解析、不加工、不缓存**——数据
   只有一个真相源（事件日志），本层只做投影；
3. **写 API**：
   - `POST /api/solve`：spawn `shangyi solve`（离线确定性求解，无 LLM）；
   - `POST /api/run`：spawn `shangyi new`（完整咨询，`SHANGYI_ASSUME_YES=1`
     自动确认检查点；intake 预答由调用方给出行文本，经 stdin 喂入）。

# 为什么轮询而不是 WebSocket

事件日志是 append-only JSONL，2 秒轮询对本机演示足够；WebSocket 会把
sidecar 从"标准库脚本"变成"长连接服务"，复杂度不成比例。前端
`app.js` 的 poll 层就是为此设计的。

用法：
    python3 shangyi/sidecar/web_console.py [--port 8787] [--bind 127.0.0.1]

环境变量：
    SHANGYI_BIN    shangyi 二进制路径（缺省按序探测，见 find_shangyi_bin）
    SHANGYI_HOME   与 CLI 同源（缺省 ~/.shangyi）
"""

from __future__ import annotations

import argparse
from copy import deepcopy
import glob
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from web_chat import ChatService, ChatError
from service_health import health as service_health
from business_cost_projection import record_business_cost
from web_source_projection import project_web_sources
from frame_projection import decision_frame
from payoff_model import compile_payoffs
from robustness import scan
import model_store
import amap_poi

# ─────────────────────────────────────────────────────────────────────────────
# 定位：shangyi 二进制与主目录
# ─────────────────────────────────────────────────────────────────────────────


def find_shangyi_bin() -> str | None:
    """按序探测 shangyi 二进制。

    顺序即优先级：显式指定 > 构建产物（本项目 CARGO_TARGET_DIR 约定）> PATH。
    """
    if os.environ.get("SHANGYI_BIN"):
        return os.environ["SHANGYI_BIN"]
    candidates = [
        Path.home() / ".shangyi-build" / "shangyi" / "debug" / "shangyi",
        Path.home() / ".shangyi-build" / "shangyi" / "release" / "shangyi",
    ]
    for c in candidates:
        if c.is_file() and os.access(c, os.X_OK):
            return str(c)
    import shutil

    return shutil.which("shangyi")


def shangyi_home() -> Path:
    # 与 CLI 的 run.rs::shangyi_home 同源：$SHANGYI_HOME → ~/.shangyi
    env = os.environ.get("SHANGYI_HOME", "").strip()
    return Path(env) if env else Path.home() / ".shangyi"


HERE = Path(__file__).resolve().parent
WEB_DIR = HERE / "web"
BIN = find_shangyi_bin()

# 运行注册表：run_id → 进程句柄与日志路径（进程对象只在启动线程里写）
_RUNS: dict[str, dict] = {}
_RUNS_LOCK = threading.Lock()
_RUN_SEQ = 0


# ─────────────────────────────────────────────────────────────────────────────
# 案例数据投影（只读，单一真相源是磁盘上的事件日志）
# ─────────────────────────────────────────────────────────────────────────────

# 路径成分白名单：date 形如 2026-09-19，case_id 形如 case-20260919-discover-072136
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_CASE_RE = re.compile(r"^case-[0-9][0-9a-z-]*$")

# 产物提取映射：工具名 → (来源, 产物键)。
# gamespec 的完整结构在**参数**里（输出只是摘要——players/strategy_counts/validated），
# 其余产物在输出里。这是对 domain_exec 工具契约的如实镜像，不是猜测。
TOOL_ARTIFACTS = {
    "research_record": ("output", "research"),
    "business_cost": ("output", "business_cost"),
    "gamespec_build": ("arguments", "gamespec"),
    "solve_equilibria": ("output", "equilibria"),
    "select_fuse": ("output", "distribution"),
    "consequence_predict": ("output", "consequence"),
    "discover_satisfice": ("output", "discovery"),
    "report_render": ("output", "report"),
    "recommend_build": ("output", "recommendation"),
}


def load_json(path: Path):
    try:
        with open(path, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, json.JSONDecodeError):
        return None


def scan_cases() -> list[dict]:
    """案例清单（轻量投影：header + 行数 + mtime，不解析事件体）。"""
    root = shangyi_home() / "cases"
    out = []
    for date_dir in sorted(glob.glob(str(root / "*" / "*")), reverse=True):
        p = Path(date_dir)
        if not (p / "case.jsonl").exists():
            continue
        header = load_json(p / "header.json") or {}
        try:
            stats = (p / "case.jsonl").stat()
            n_events = sum(1 for _ in open(p / "case.jsonl", encoding="utf-8"))
        except OSError:
            continue
        out.append(
            {
                "case_id": header.get("case_id", p.name),
                "date": p.parent.name,
                "mode": header.get("mode"),
                "playbook_id": header.get("playbook_id"),
                "created_at": header.get("created_at"),
                "question": find_stored_question(p / "case.jsonl"),
                "n_events": n_events,
                "log_bytes": stats.st_size,
                "mtime": stats.st_mtime,
            }
        )
    return out


def load_library() -> dict:
    """library.json 的 cases 是 {case_id: 记录} 字典——统一投影成列表，前端不用关心键形态。"""
    lib = load_json(shangyi_home() / "cases" / "library.json") or {}
    cases = lib.get("cases") or {}
    if isinstance(cases, dict):
        cases = list(cases.values())
    lib["cases"] = cases
    return lib


def find_stored_question(jsonl_path: Path) -> str | None:
    """案例落档时 case_store_write **参数**里的 question（用户问题原文）。

    这是磁盘案例与「用户当时问的是哪件事」的唯一链路：library.json 的
    留档 case_id 与运行期 case_id 不同名（实测交集为 0），按 id 联不上。
    未走到归档的案例没有该调用 → 返回 None，前端如实降级。
    逐行做子串预滤、只解析命中行——scan_cases 每个轮询周期都会跑，必须便宜。
    """
    started: dict[str, str] = {}
    question: str | None = None
    try:
        f = open(jsonl_path, encoding="utf-8")
    except OSError:
        return None
    with f:
        for line in f:
            if "case_store_write" not in line:
                continue
            try:
                ev = json.loads(line).get("event", {})
            except json.JSONDecodeError:
                continue
            t = ev.get("type")
            if t == "tool_call_started":
                started[ev.get("call_id", "")] = ev.get("arguments", "")
            elif t == "tool_call_finished" and ev.get("outcome", {}).get("status") == "ok":
                args = started.get(ev.get("call_id", ""))
                if args:
                    try:
                        question = json.loads(args).get("question") or question
                    except (json.JSONDecodeError, TypeError):
                        pass
    return question


def _case_conversation_context(date: str, case_id: str) -> dict | None:
    """Recover the original question only from one exact, identifiable conversation."""
    matches = []
    for path in (shangyi_home() / "web-conversations").glob("chat-*/state.json"):
        state = load_json(path)
        if not isinstance(state, dict) or state.get("case") != date + "/" + case_id:
            continue
        chat_id = state.get("id")
        if not isinstance(chat_id, str) or not re.fullmatch(r"chat-[a-f0-9]{24}", chat_id) or path.parent.name != chat_id:
            continue
        matches.append(state)
    if len(matches) != 1 or not isinstance(matches[0].get("initial"), str) or not matches[0]["initial"].strip():
        return None
    state = matches[0]
    return {"source": "web_conversation_state", "chat_id": state["id"],
            "initial": state["initial"], "case": state["case"]}


def _merge_spec(base, patch):
    """Mirror domain_exec.rs::deep_merge: objects recurse; every other value replaces."""
    if isinstance(base, dict) and isinstance(patch, dict):
        merged = deepcopy(base)
        for key, value in patch.items():
            merged[key] = _merge_spec(merged[key], value) if key in merged else deepcopy(value)
        return merged
    return deepcopy(patch)


def _spec_expr(value):
    if isinstance(value, dict) and "kind" in value:
        return deepcopy(value)
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return {"kind": "literal", "value": value}
    if isinstance(value, str):
        return {"kind": "ref", "value": value}
    return deepcopy(value)


def _spec_expr_leaves(value):
    if not isinstance(value, dict):
        return _spec_expr(value)
    return {key: deepcopy(item) if isinstance(item, dict) and "kind" in item else
            _spec_expr_leaves(item) if isinstance(item, dict) else _spec_expr(item)
            for key, item in value.items()}


def _first_spec_key(value, keys):
    # Rust's get(...).or_else(...) checks key presence, not truthiness.
    return next((value[key] for key in keys if key in value), None)


def _spec_dynamics(value):
    if isinstance(value, str):
        return {"form": value}
    if not isinstance(value, dict):
        return value
    for key in sorted(value):
        inner = value[key]
        if key in ("one_shot", "repeated") and inner is not None:
            values = {}
            if isinstance(inner, dict):
                for name in sorted(inner):
                    if name == "values":
                        if isinstance(inner[name], dict):
                            values.update(inner[name])
                    elif name != "form":
                        values[name] = inner[name]
            return {"form": key, **({"values": values} if values else {})}
    tag = _first_spec_key(value, ("form", "type", "structure"))
    if not isinstance(tag, str):
        return value
    existing = _first_spec_key(value, ("values", "params"))
    values = dict(existing) if isinstance(existing, dict) else {}
    values.update({k: v for k, v in value.items()
                   if k not in ("form", "type", "structure", "values", "params", "note")})
    if "horizon" in values and (not isinstance(values["horizon"], (int, float)) or
                                isinstance(values["horizon"], bool)):
        values["horizon"] = None
    return {"form": tag, **({"values": values} if values else {})}


def _spec_information(value):
    if isinstance(value, str):
        return {"type": value}
    if not isinstance(value, dict):
        return value
    for key in sorted(value):
        inner = value[key]
        if key in ("complete", "incomplete") and inner is not None:
            return {"type": key, **(inner if isinstance(inner, dict) else {})}
    tag = _first_spec_key(value, ("form", "type"))
    if not isinstance(tag, str):
        return value
    result = {"type": tag}
    for key in sorted(value):
        if key in ("form", "type"):
            continue
        if key in ("values", "params"):
            if isinstance(value[key], dict):
                result.update(value[key])
        else:
            result[key] = value[key]
    return result


def _normalize_spec_input(arguments):
    """Read-only counterpart of domain_exec.rs::normalize_spec_input, before merging.

    This does not validate or approve a model: callers replay only successful calls.
    Keep normalization before merge, since normalizing the merged tree changes patches.
    """
    value = json.loads(arguments) if isinstance(arguments, str) else deepcopy(arguments)
    for _ in range(4):
        if not isinstance(value, dict) or any(k in value for k in ("game_id", "players", "strategies")):
            break
        if not any(k in value for k in ("spec", "arguments", "input")):
            break
        inner = _first_spec_key(value, ("spec", "arguments", "input"))
        if isinstance(inner, str):
            try:
                value = json.loads(inner)
            except ValueError:
                break
        else:
            value = inner
    if not isinstance(value, dict):
        raise ValueError("GameSpec projection requires an object")
    for key in ("dynamics", "information"):
        if isinstance(value.get(key), str):
            try:
                decoded = json.loads(value[key])
                if isinstance(decoded, dict):
                    value[key] = decoded
            except ValueError:
                pass  # Natural tags such as one_shot are accepted by the engine.
    for key in ("payoffs", "strategies", "params", "meta"):
        if isinstance(value.get(key), str):
            value[key] = json.loads(value[key])
    def meta_text(item):
        return item if isinstance(item, str) else json.dumps(item, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    if isinstance(value.get("meta"), dict):
        value["meta"] = {k: "、".join(meta_text(x) for x in v) if isinstance(v, list) else meta_text(v)
                         for k, v in value["meta"].items()}
    payoffs = value.get("payoffs")
    if isinstance(payoffs, dict):
        content = payoffs.get("values")
        if "form" not in payoffs and ("cells" in payoffs or isinstance(content, dict) and "cells" in content):
            payoffs["form"] = "normal"
        if isinstance(content, dict) and "cells" in content:
            content["cells"] = _spec_expr_leaves(content["cells"])
    if isinstance(value.get("params"), dict):
        for param in value["params"].values():
            if isinstance(param, dict) and "value" in param:
                param["value"] = _spec_expr(param["value"])
    if "dynamics" in value:
        value["dynamics"] = _spec_dynamics(value["dynamics"])
    if "information" in value:
        value["information"] = _spec_information(value["information"])
    return value


def _stored_spec(value):
    """Project the serialized GameSpec shape used as the next merge's base.

    Defaults and ignored fields follow shangyi-gamespec/src/lib.rs. In particular,
    absent optional fields become null; unknown wrapper/control fields do not survive.
    """
    def fields(item, names):
        return {key: item[key] for key in names if key in item}
    def participant(item):
        result = fields(item, ("id", "name", "vn_role", "relation_to_game"))
        if result.get("relation_to_game") in ("我方", "本企业", "焦点企业"):
            result["relation_to_game"] = "player"
        if item.get("profile_ref") is not None:
            result["profile_ref"] = item["profile_ref"]
        return result
    required = ("game_id", "players", "strategies", "payoffs", "params")
    if any(key not in value for key in required):
        raise ValueError("Incomplete successful GameSpec in event log")
    result = {key: deepcopy(value[key]) for key in required}
    result["players"] = [participant(p) for p in result["players"]]
    result["external_actors"] = [participant(p) for p in value.get("external_actors", [])]
    result["meta"] = deepcopy(value.get("meta", {}))
    information = value.get("information")
    result["information"] = None if information is None else {"type": information["type"]}
    if information is not None and information["type"] == "incomplete":
        result["information"]["types"] = deepcopy(information.get("types", {}))
    dynamics = value.get("dynamics")
    result["dynamics"] = None if dynamics is None else {"form": dynamics["form"]}
    if dynamics is not None and dynamics["form"] == "repeated":
        values = dynamics["values"]
        result["dynamics"]["values"] = {"discount": values["discount"]}
        if values.get("horizon") is not None:
            result["dynamics"]["values"]["horizon"] = values["horizon"]
    preference = value.get("preference")
    result["preference"] = None if preference is None else {
        "risk_aversion": preference.get("risk_aversion", 0.0),
        "objective_weights": deepcopy(preference.get("objective_weights", {}))}
    payoffs = result["payoffs"]
    result["payoffs"] = {"form": payoffs["form"]}
    if payoffs["form"] == "normal":
        result["payoffs"]["values"] = {"cells": {
            cell: {player: fields(expr, ("kind", "value")) for player, expr in values.items()}
            for cell, values in payoffs["values"].get("cells", {}).items()}}
    source_fields = {"user_confirmed": ("checkpoint",), "confirmed_assumption": ("checkpoint", "basis"),
                     "public_data": ("provenance",), "industry_prior": ("prior_ref",),
                     "assumed": ("note",), "derived": ("note",)}
    dist_fields = {"normal": ("mu", "sigma"), "uniform": ("low", "high"), "triangular": ("low", "mode", "high")}
    for key, param in result["params"].items():
        source = param["source"]
        saved = {"name": param["name"], "value": fields(param["value"], ("kind", "value")),
                 "source": fields(source, ("kind", *source_fields[source["kind"]]))}
        if param.get("unit") is not None:
            saved["unit"] = param["unit"]
        if param.get("dist") is not None:
            dist = param["dist"]
            saved["dist"] = {"dist": dist["dist"], "values": fields(dist["values"], dist_fields[dist["dist"]])}
        result["params"][key] = saved
    return result


def _project_successful_spec(arguments, previous=None):
    # The merge flag is read before normalization and only from the original object.
    merge = isinstance(arguments, dict) and arguments.get("merge") is True
    normalized = _normalize_spec_input(arguments)
    return _stored_spec(_merge_spec(previous, normalized) if merge and previous is not None else normalized)


def project_case(date: str, case_id: str) -> dict | None:
    """完整案例投影：事件流 + 统计 + 产物。

    - 事件按 seq 原样返回（前端是唯一渲染方）；
    - `tool_call_finished` 不带工具名（名字只在 started 里）——在此完成
      call_id 联接，前端拿到的每条事件已可独立渲染；
    - 产物按"最后一次成功调用"取；经营成本按场景保留，GameSpec 成功补丁按顺序重放。
    """
    if not (_DATE_RE.match(date) and _CASE_RE.match(case_id)):
        return None
    base = shangyi_home() / "cases" / date / case_id
    header = load_json(base / "header.json")
    jsonl = base / "case.jsonl"
    if header is None or not jsonl.exists():
        return None

    events: list[dict] = []
    call_names: dict[str, str] = {}  # call_id → 工具名
    spec_arguments: dict[str, object] = {}
    artifacts: dict[str, object] = {"task_results": [], "latest_task": None}
    tokens = {"input": 0, "output": 0, "cached": 0, "reasoning": 0, "calls": 0}
    tool_stats: dict[str, dict] = {}
    phases: list[str] = []

    with open(jsonl, encoding="utf-8") as f:
        for line in f:
            try:
                rec = json.loads(line)
            except json.JSONDecodeError:
                continue  # 尾部半行：正在 append 的实时文件可能截在换行前
            ev = rec.get("event", {})
            et = ev.get("type")
            if et == "tool_call_started":
                call_names[ev.get("call_id", "")] = ev.get("name", "?")
                if ev.get("name") == "gamespec_build":
                    try:
                        raw = ev.get("arguments", "")
                        spec_arguments[ev.get("call_id", "")] = json.loads(raw) if isinstance(raw, str) else deepcopy(raw)
                    except (ValueError, TypeError):
                        pass
            elif et == "tool_call_finished":
                cid = ev.get("call_id", "")
                name = call_names.get(cid, "?")
                ev["name"] = name  # 联接结果直接写回事件，前端零额外查询
                outcome = ev.get("outcome", {})
                st = tool_stats.setdefault(name, {"ok": 0, "failed": 0, "blocked": 0})
                status = outcome.get("status", "unknown")
                if status in st:
                    st[status] += 1
                # 产物提取：只认成功调用（失败调用的输出是错误信息，不是产物）。
                # GameSpec 只在成功完成时更新；参数解包与增量合并遵循引擎顺序。
                if status == "ok" and name == "gamespec_build":
                    try:
                        artifacts["gamespec"] = _project_successful_spec(spec_arguments[cid], artifacts.get("gamespec"))
                        artifacts.pop("gamespec_projection_error", None)
                    except (ValueError, TypeError, KeyError, AttributeError):
                        # Do not present a superseded model as the current successful state.
                        artifacts.pop("gamespec", None)
                        artifacts["gamespec_projection_error"] = "这次成功建模的记录无法完整还原，暂不展示模型，原始调用仍保留在分析过程。"
                if status == "ok" and name == "poi_survey":
                    # 周边计数是**累积**的产物：同一个案子里可能换了地址再查。
                    raw = outcome.get("output")
                    if raw:
                        try:
                            artifacts.setdefault("poi_surveys", []).append(json.loads(raw))
                        except (ValueError, TypeError):
                            pass
                if status == "ok" and name in TOOL_ARTIFACTS:
                    _, key = TOOL_ARTIFACTS[name]
                    if key != "gamespec":
                        raw = outcome.get("output")
                        if raw:
                            try:
                                artifact = json.loads(raw)
                                if key == "business_cost":
                                    record_business_cost(artifacts, artifact)
                                else:
                                    artifacts[key] = artifact
                            except (json.JSONDecodeError, TypeError):
                                pass
            elif et == "token_usage":
                tokens["input"] += ev.get("input_tokens", 0)
                tokens["output"] += ev.get("output_tokens", 0)
                tokens["cached"] += ev.get("cached_input_tokens", 0)
                tokens["reasoning"] += ev.get("reasoning_tokens", 0)
                tokens["calls"] += 1
            elif et == "phase_entered":
                phases.append(ev.get("to", ""))
            elif et == "scoped_task_completed" and isinstance(ev.get("result"), dict):
                result = {**deepcopy(ev["result"]), "at": ev.get("at")}
                artifacts["task_results"].append(result)
                artifacts["latest_task"] = deepcopy(result)
            elif et == "followup_task_started":
                # A new request invalidates only the current completion, not prior results.
                artifacts["latest_task"] = {"status": "processing", "at": ev.get("at")}
            events.append(ev)

    # 时间范围
    times = [e.get("at") for e in events if e.get("at")]
    artifacts["web_sources"] = project_web_sources(events)
    stored_question = find_stored_question(jsonl)
    context = _case_conversation_context(date, case_id)
    question = stored_question or (context["initial"] if context else None)
    frame = decision_frame(
        question=question or "", topic=header.get("playbook_id"), mode=header.get("mode"),
        artifacts=artifacts,
        missing=[m for r in (artifacts.get("task_results") or []) for m in (r.get("missing") or [])])
    return {
        "header": header,
        "frame": frame,
        "question": question,
        "question_source": "case_store_write" if stored_question else "web_conversation_state" if context else None,
        "context": context,
        "events": events,
        "stats": {
            "n_events": len(events),
            "tokens": tokens,
            "tools": tool_stats,
            "phases": phases,
            "first_at": times[0] if times else None,
            "last_at": times[-1] if times else None,
        },
        "artifacts": artifacts,
    }


# ─────────────────────────────────────────────────────────────────────────────
# 剧本投影（显示级解析：只取 meta/intake/fails_when；解析不了就如实降级）
# ─────────────────────────────────────────────────────────────────────────────


def _yaml_scalar(line: str) -> str | None:
    m = re.match(r"^\s*-?\s*([A-Za-z_][\w]*)\s*:\s*(.+)$", line)
    return m.groups() if m else None


def parse_playbook(path: Path) -> dict:
    """显示级 YAML 子集解析（meta.id/name/description + intake 问题清单）。

    **不做通用 YAML 解析**：那需要 PyYAML（sidecar 零依赖）。这里只取
    前端运行表单需要的字段；拿不到就标 `parsed: false` 并附原文，
    绝不编造。
    """
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return {"id": path.parent.name, "parsed": False}
    meta: dict[str, str] = {}
    intake: list[dict] = []
    section = None
    cur_q: dict | None = None
    for raw in text.splitlines():
        line = raw.rstrip()
        if not line.strip() or line.strip().startswith("#"):
            continue
        if not line.startswith((" ", "\t")):  # 顶层键
            # Flush the last intake item before leaving its section.
            if section == "intake" and cur_q is not None:
                intake.append(cur_q)
            section = line.split(":", 1)[0].strip()
            cur_q = None
            continue
        indented = line.strip()
        if section == "meta":
            g = _yaml_scalar(line)
            if g:
                k, v = g
                if k in ("id", "name", "description") and v:
                    meta[k] = v.strip().strip('"')
        elif section == "intake":
            m = re.match(r"^\s*-\s*id\s*:\s*(\S+)", line)
            if m:
                if cur_q:
                    intake.append(cur_q)
                cur_q = {"id": m.group(1)}
                continue
            if cur_q is not None:
                m = re.match(r"^\s*(text|kind)\s*:\s*\"?(.*?\"?)\s*$", line)
                if m:
                    cur_q[m.group(1)] = m.group(2).strip().strip('"')
                    continue
                m = re.match(r"^\s*options\s*:\s*\[(.*)\]\s*$", line)
                if m:
                    cur_q["options"] = [
                        o.strip().strip('"') for o in m.group(1).split(",") if o.strip()
                    ]
    if cur_q:
        intake.append(cur_q)
    return {
        "id": meta.get("id", path.parent.name),
        "name": meta.get("name", path.parent.name),
        "description": meta.get("description", ""),
        "intake": intake,
        "parsed": bool(meta),
        "raw": text,
    }


def list_playbooks() -> list[dict]:
    # Match CLI load_playbook: user-home definitions override repository ones.
    paths: dict[str, Path] = {}
    for root in (HERE.parent / "playbooks", shangyi_home() / "playbooks"):
        for pbf in sorted(root.glob("*/playbook.yml")):
            if not pbf.parent.name.startswith("."):
                paths[pbf.parent.name] = pbf
    return [parse_playbook(paths[key]) for key in sorted(paths)]


# ─────────────────────────────────────────────────────────────────────────────
# 写路径：solve（离线）与 run（完整咨询）
# ─────────────────────────────────────────────────────────────────────────────


def run_solver(spec: dict, method=None, refine=None, refine_denominator=None) -> dict:
    """跑一次 `shangyi solve`，返回解集 JSON（失败抛 RuntimeError）。

    solve 与 robustness 共用：解集口径必须只有一个来源。
    """
    if BIN is None:
        raise RuntimeError("找不到 shangyi 二进制（设 SHANGYI_BIN 指定路径）")
    with tempfile.NamedTemporaryFile(
        "w", suffix=".json", prefix="shangyi-solve-", delete=False, encoding="utf-8"
    ) as f:
        json.dump(spec, f, ensure_ascii=False)
        tmp = f.name
    cmd = [BIN, "solve", "--in", tmp]
    if method:
        cmd += ["--method", str(method)]
    if refine:
        cmd += ["--refine", str(refine)]
    if refine_denominator:
        cmd += ["--refine-denominator", str(int(refine_denominator))]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, timeout=60, cwd=tempfile.gettempdir())
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or f"退出码 {proc.returncode}")
        return json.loads(proc.stdout)
    except subprocess.TimeoutExpired:
        raise RuntimeError("求解超时（60s）")
    except json.JSONDecodeError:
        raise RuntimeError("引擎输出不是 JSON")
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def do_robustness(body: dict) -> dict:
    """POST /api/robustness {bundle}

    逐个推偏假设（客流、份额、抢客力度、固定成本），重编译重求解，
    找出第一个让均衡翻面的幅度——结论的生死线，而不是一句「稳健」。
    """
    bundle = body.get("bundle", body)
    if not isinstance(bundle, dict):
        return {"ok": False, "error": "请求体缺少 bundle（数字对象）"}
    compiled = compile_payoffs(bundle)
    if not compiled.get("ok"):
        return {"ok": False, "error": "数字没填齐，先补上再扫", "incomplete": compiled.get("incomplete", [])}
    try:
        run_solver(compiled["spec"])          # 基线先跑通，避免把「求解失败」当成「没翻面」
    except RuntimeError as error:
        return {"ok": False, "error": f"基线求解失败：{error}"}
    try:
        return scan(bundle, lambda spec: run_solver(spec))
    except RuntimeError as error:
        return {"ok": False, "error": f"扫描中断：{error}"}


def do_poi(body: dict) -> dict:
    """POST /api/poi {address, radius?, brand?}

    查一个地址的周边计数（竞品密度、写字楼、地铁口、同品牌门店）。
    **POI 明细只在响应里过一遍**：存档时由服务端剥成计数（见 counts_only），
    不依赖调用方自觉。
    """
    key = amap_poi.load_key()
    if not key:
        return {"ok": False, "error": "没有配置高德密钥：设 AMAP_KEY，或写入 ~/.shangyi-secrets/amap.env"}
    try:
        return amap_poi.survey(str(body.get("address") or ""), key=key,
                               radius=body.get("radius") or amap_poi.DEFAULT_RADIUS,
                               brand=str(body.get("brand") or "").strip())
    except amap_poi.AmapError as error:
        return {"ok": False, "error": str(error)}


def do_save_model(body: dict) -> dict:
    """POST /api/models {bundle, robustness?}

    数字 → 编译 → 求解 →（可选）扫描 → 存档。**存档前必须求解成功**：
    存一条算不出来的记录，等于把「没结论」伪装成「有记录」。
    """
    bundle = body.get("bundle")
    if not isinstance(bundle, dict):
        return {"ok": False, "error": "请求体缺少 bundle（数字对象）"}
    compiled = compile_payoffs(bundle)
    if not compiled.get("ok"):
        return {"ok": False, "error": "数字没填齐，先补上再存", "incomplete": compiled.get("incomplete", [])}
    try:
        solution = run_solver(compiled["spec"])
    except RuntimeError as error:
        return {"ok": False, "error": f"求解失败，未存档：{error}"}
    robustness = None
    if body.get("robustness"):
        try:
            robustness = scan(bundle, lambda spec: run_solver(spec))
        except RuntimeError as error:
            robustness = {"ok": False, "error": str(error)}
    try:
        saved = model_store.save(shangyi_home(), {
            # 周边事实只留计数与口径；POI 明细在服务端剥掉，不落库。
            "facts": amap_poi.counts_only(body.get("poi") or {}),
            "bundle": bundle, "spec": compiled["spec"], "cells": compiled["cells"],
            "payback_months": compiled["payback_months"], "unmodeled": compiled["unmodeled"],
            "warning": compiled["warning"], "solution": solution, "robustness": robustness,
        })
    except model_store.ModelStoreError as error:
        return {"ok": False, "error": str(error)}
    return {"ok": True, "id": saved["id"], "title": saved["title"], "created_at": saved["created_at"]}


def do_model(body: dict) -> dict:
    """POST /api/model {market, me, rival}

    数字 → 博弈矩阵：收益由算式生成、每格带算式原文，缺项整体拒绝。
    本层不求解（求解仍只走 `shangyi solve`），只做「数字 → 可求解的 GameSpec」。
    """
    bundle = body.get("bundle", body)
    if not isinstance(bundle, dict):
        return {"ok": False, "error": "请求体缺少 bundle（数字对象）"}
    return compile_payoffs(bundle)


def do_solve(body: dict) -> dict:
    """POST /api/solve {spec, method?, refine?, refine_denominator?}

    spawn `shangyi solve` 子进程：解集口径必须与引擎完全同源，
    sidecar 绝不自己实现求解（那会成为第二个、会漂移的真相源）。
    """
    if BIN is None:
        return {"ok": False, "error": "找不到 shangyi 二进制（设 SHANGYI_BIN 指定路径）"}
    spec = body.get("spec")
    if not isinstance(spec, dict):
        return {"ok": False, "error": "请求体缺少 spec（GameSpec JSON 对象）"}
    with tempfile.NamedTemporaryFile(
        "w", suffix=".json", prefix="shangyi-solve-", delete=False, encoding="utf-8"
    ) as f:
        json.dump(spec, f, ensure_ascii=False)
        tmp = f.name
    cmd = [BIN, "solve", "--in", tmp]
    if body.get("method"):
        cmd += ["--method", str(body["method"])]
    if body.get("refine"):
        cmd += ["--refine", str(body["refine"])]
    if body.get("refine_denominator"):
        cmd += ["--refine-denominator", str(int(body["refine_denominator"]))]
    try:
        proc = subprocess.run(
            cmd, capture_output=True, text=True, timeout=60, cwd=tempfile.gettempdir()
        )
        if proc.returncode == 0:
            try:
                return {"ok": True, "result": json.loads(proc.stdout)}
            except json.JSONDecodeError:
                return {"ok": False, "error": f"引擎输出不是 JSON：{proc.stdout[:300]}"}
        return {"ok": False, "error": proc.stderr.strip() or f"退出码 {proc.returncode}"}
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "求解超时（60s）"}
    finally:
        try:
            os.unlink(tmp)
        except OSError:
            pass


def detect_new_case(before: set[str], started_at: float) -> str | None:
    """找出本次运行新建的案例目录（对比启动前已存在的 case_id 集合）。"""
    for c in scan_cases():
        key = f"{c['date']}/{c['case_id']}"
        if key not in before:
            # mtime 佐证：目录在启动之后才有活动（容忍 2s 时钟偏差）
            if c["mtime"] >= started_at - 2:
                return key
    return None


def do_run(body: dict) -> dict:
    """POST /api/run {mode, action?, playbook?, preanswers_text?}

    启动 `shangyi new` 完整咨询（后台进程）：
    - `SHANGYI_ASSUME_YES=1`：检查点自动确认（CLI 的既有通道，非本层发明）；
    - intake 预答：`preanswers_text` 按行写入 stdin——CLI 逐题读行，
      行序与问题序一致；不足的题按「未取得」处理（CLI 对 EOF 的既有
      语义，不编造答案）。
    """
    if BIN is None:
        return {"ok": False, "error": "找不到 shangyi 二进制（设 SHANGYI_BIN 指定路径）"}
    mode = body.get("mode", "scenario")
    if mode not in ("scenario", "whatif", "discover"):
        return {"ok": False, "error": "mode 必须是 scenario / whatif / discover"}
    action = str(body.get("action") or "").strip()
    if mode == "whatif" and not action:
        return {"ok": False, "error": "whatif 模式必须给 action"}
    preanswers = str(body.get("preanswers_text") or "")

    cmd = [BIN, "new", "--mode", mode]
    if action:
        cmd += ["--action", action]
    if body.get("playbook"):
        cmd += ["--playbook", str(body["playbook"])]

    run_root = shangyi_home() / "web-runs"
    run_root.mkdir(parents=True, exist_ok=True)
    with _RUNS_LOCK:
        global _RUN_SEQ
        _RUN_SEQ += 1
        run_id = f"run-{time.strftime('%H%M%S')}-{_RUN_SEQ}"
    log_path = run_root / f"{run_id}.log"

    before = {f"{c['date']}/{c['case_id']}" for c in scan_cases()}
    started_at = time.time()

    env = dict(os.environ)
    env["SHANGYI_ASSUME_YES"] = "1"  # 检查点自动确认（CLI 既有语义）
    env.pop("SHANGYI_SKIP_PREFLIGHT", None)

    log_f = open(log_path, "w", encoding="utf-8")
    stdin_f = tempfile.TemporaryFile("w+")
    if preanswers:
        stdin_f.write(preanswers)
        if not preanswers.endswith("\n"):
            stdin_f.write("\n")
    stdin_f.seek(0)
    proc = subprocess.Popen(
        cmd,
        stdout=log_f,
        stderr=subprocess.STDOUT,
        stdin=stdin_f,
        env=env,
        start_new_session=True,  # 独立进程组：stop 时整组信号，避免遗留子进程
    )

    def _reap():
        proc.wait()
        log_f.close()
        stdin_f.close()

    threading.Thread(target=_reap, daemon=True).start()
    with _RUNS_LOCK:
        _RUNS[run_id] = {
            "run_id": run_id,
            "pid": proc.pid,
            "proc": proc,
            "mode": mode,
            "action": action or None,
            "cmd": cmd,
            "log_path": str(log_path),
            "started_at": started_at,
            "before_keys": before,
            "case": None,
            "reaped": False,
        }
    return {"ok": True, "run_id": run_id, "cmd": cmd}


def run_status(entry: dict) -> dict:
    proc: subprocess.Popen = entry["proc"]
    alive = proc.poll() is None
    # 案例目录发现：懒检测（进程活着且尚未定位到目录时每查一次试一次）
    case_key = entry.get("case")
    if case_key is None and not entry.get("reaped"):
        case_key = detect_new_case(entry["before_keys"], entry["started_at"])
        if case_key:
            entry["case"] = case_key
    tail = ""
    try:
        with open(entry["log_path"], encoding="utf-8") as f:
            lines = f.read().splitlines()
        tail = "\n".join(lines[-40:])
    except OSError:
        pass
    return {
        "run_id": entry["run_id"],
        "mode": entry["mode"],
        "action": entry["action"],
        "cmd": entry["cmd"],
        "alive": alive,
        "exit_code": proc.returncode,
        "case": case_key,
        "started_at": entry["started_at"],
        "log_tail": tail,
    }


def stop_run(run_id: str) -> dict:
    with _RUNS_LOCK:
        entry = _RUNS.get(run_id)
    if entry is None:
        return {"ok": False, "error": f"未知运行 {run_id}"}
    if entry["proc"].poll() is not None:
        return {"ok": True, "already_exited": True}
    # SIGTERM 到整个进程组（start_new_session 使 pid 即 pgid）
    try:
        os.killpg(os.getpgid(entry["proc"].pid), signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        return {"ok": False, "error": "进程组已消失"}
    entry["reaped"] = True
    return {"ok": True}


# ─────────────────────────────────────────────────────────────────────────────
# HTTP 层
# ─────────────────────────────────────────────────────────────────────────────

MIME = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".svg": "image/svg+xml",
    ".png": "image/png",
    ".ico": "image/x-icon",
}


CHAT = ChatService(lambda: BIN, shangyi_home, list_playbooks, project_case)


class Handler(BaseHTTPRequestHandler):
    server_version = "shangyi-console/0.1"

    # ---- 基础设施 ----
    def log_message(self, fmt, *args):  # 安静模式：访问日志打到 stderr 但压成一行
        sys.stderr.write("[%s] %s\n" % (time.strftime("%H:%M:%S"), fmt % args))

    def _json(self, obj, status: int = 200):
        body = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self) -> dict:
        n = int(self.headers.get("Content-Length") or 0)
        if n <= 0 or n > 5 * 1024 * 1024:
            return {}
        try:
            return json.loads(self.rfile.read(n).decode("utf-8"))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return {}

    # ---- 路由 ----
    def do_GET(self):
        path = urllib.parse.urlparse(self.path).path
        if path.startswith("/api/"):
            return self._api_get(path)
        return self._static(path)

    def do_POST(self):
        path = urllib.parse.urlparse(self.path).path
        body = self._read_body()
        if path.startswith("/api/chats"):
            try:
                if not isinstance(body, dict):
                    raise ChatError("请求内容必须是对象。")
                if path == "/api/chats":
                    return self._json(CHAT.start(body))
                match = re.fullmatch(r"/api/chats/(chat-[a-f0-9]{24})/(message|stop)", path)
                if match:
                    result = CHAT.reply(match[1], body) if match[2] == "message" else CHAT.stop(match[1])
                    return self._json(result)
                return self._json({"ok": False, "error": "咨询路径无效。"}, 404)
            except ChatError as exc:
                return self._json({"ok": False, "error": str(exc)}, 409)
        if path == "/api/model":
            return self._json(do_model(body))
        if path == "/api/robustness":
            return self._json(do_robustness(body))
        if path == "/api/models":
            return self._json(do_save_model(body))
        if path == "/api/poi":
            return self._json(do_poi(body))
        if path == "/api/solve":
            return self._json(do_solve(body))
        if path == "/api/run":
            return self._json(do_run(body))
        m = re.match(r"^/api/runs/([^/]+)/stop$", path)
        if m:
            return self._json(stop_run(m.group(1)))
        return self._json({"ok": False, "error": f"未知端点 {path}"}, 404)

    def _api_get(self, path: str):
        if path == "/api/health":
            return self._json(
                {
                    "ok": True,
                    "frontend_contract": 3,
                    "services": service_health(),
                    "interactive_chat": CHAT.capable(),
                    "shangyi_bin": BIN,
                    "shangyi_home": str(shangyi_home()),
                    "web_dir": str(WEB_DIR),
                }
            )
        if path == "/api/models":
            return self._json({"ok": True, "models": model_store.listing(shangyi_home())})
        single = re.fullmatch(r"/api/models/(model-[a-f0-9]{12})", path)
        if single:
            record = model_store.load(shangyi_home(), single.group(1))
            if record is None:
                return self._json({"ok": False, "error": "没有这条记录。"}, 404)
            return self._json({"ok": True, "model": record})
        if path == "/api/cases":
            lib = load_library()
            by_id = {c.get("case_id"): c for c in lib.get("cases", [])}
            cases = scan_cases()
            for c in cases:
                extra = by_id.get(c["case_id"])
                if extra:
                    c["library"] = {
                        k: extra.get(k)
                        for k in ("scenario", "decision_question", "assumptions", "prediction")
                        if extra.get(k) is not None
                    }
            return self._json({"ok": True, "cases": cases})
        if path == "/api/chats":
            return self._json(CHAT.list())
        match = re.fullmatch(r"/api/chats/(chat-[a-f0-9]{24})", path)
        if match:
            try:
                return self._json(CHAT.get(match[1]))
            except ChatError as exc:
                return self._json({"ok": False, "error": str(exc)}, 404)
        if path == "/api/library":
            return self._json({"ok": True, **load_library()})
        if path == "/api/playbooks":
            return self._json({"ok": True, "playbooks": list_playbooks()})
        if path == "/api/runs":
            with _RUNS_LOCK:
                entries = list(_RUNS.values())
            return self._json({"ok": True, "runs": [run_status(e) for e in entries]})
        m = re.match(r"^/api/case/([\d-]+)/([^/]+)$", path)
        if m:
            proj = project_case(m.group(1), m.group(2))
            if proj is None:
                return self._json({"ok": False, "error": "案例不存在或路径非法"}, 404)
            return self._json({"ok": True, **proj})
        return self._json({"ok": False, "error": f"未知端点 {path}"}, 404)

    def _static(self, path: str):
        if path == "/":
            path = "/index.html"
        # 防目录穿越：解析后必须仍落在 web/ 内
        target = (WEB_DIR / path.lstrip("/")).resolve()
        if not str(target).startswith(str(WEB_DIR.resolve()) + os.sep) and target != WEB_DIR.resolve():
            return self._json({"ok": False, "error": "禁止的路径"}, 403)
        if not target.is_file():
            # SPA：非文件路径回落到 index（前端用 hash 路由，此处兜底）
            target = WEB_DIR / "index.html"
        mime = MIME.get(target.suffix, "application/octet-stream")
        body = target.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(body)))
        # 本地控制台：静态资源也不缓存——改版后刷新即生效，
        # 避免"改了没反应"的困惑（带宽在此不是目标）。
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)


def main() -> int:
    ap = argparse.ArgumentParser(description="商奕控制台 sidecar")
    ap.add_argument("--port", type=int, default=8787)
    ap.add_argument("--bind", default="127.0.0.1")
    ns = ap.parse_args()

    if BIN is None:
        print(
            "⚠ 找不到 shangyi 二进制——solve/run 将不可用（读 API 不受影响）。\n"
            "  设 SHANGYI_BIN=/path/to/shangyi 或先构建：cargo build -p shangyi-cli",
            file=sys.stderr,
        )
    if not WEB_DIR.is_dir():
        print(f"⚠ 前端目录不存在：{WEB_DIR}", file=sys.stderr)

    srv = ThreadingHTTPServer((ns.bind, ns.port), Handler)
    url = f"http://{ns.bind}:{ns.port}"
    print(f"商奕控制台：{url}  （bin={'✓' if BIN else '✗'} home={shangyi_home()}）")
    print("Ctrl+C 退出。")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        print("\n再见。")
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""独立对拍：卡片投影（Node 半）vs 原始 case.jsonl（不经 Rust、不经 Node 投影代码）。

用法：
  node --input-type=module -e "import {projectCase} from './shangyi/ui/dsh-plugin-shangyi/lib/index.js'; process.stdout.write(JSON.stringify(projectCase(process.argv[1])))" <case_id> > /tmp/cards.json
  python3 shangyi/ui/dsh-plugin-shangyi/scripts/check-projection.py <case.jsonl> /tmp/cards.json

判据：每一条都是「从原始日志独立重算」的结果与卡片字段逐项相等；
不相等即 exit 1（防止前端展示与引擎留档漂移）。
"""
import json
import sys


PHASE_ORDER_INDEX = {
    "intake": 0,
    "intel": 1,
    "model_build": 2,
    "solve": 3,
    "select": 4,
    "consequence": 5,
    "discover": 6,
    "recommend": 7,
    "report": 8,
    "archived": 9,
}


def unwrap_spec(raw):
    if not raw:
        return None
    try:
        v = json.loads(raw) if isinstance(raw, str) else raw
    except Exception:
        return None
    for _ in range(3):
        if isinstance(v, dict) and isinstance(v.get("arguments"), str):
            try:
                v = json.loads(v["arguments"])
            except Exception:
                break
        else:
            break
    if isinstance(v, dict) and isinstance(v.get("spec"), dict) and not v.get("players") and not v.get("params"):
        outer_merge = v.get("merge")
        inner = v["spec"]
        if isinstance(outer_merge, bool) and "merge" not in inner:
            v = dict(inner)
            v["merge"] = outer_merge
        else:
            v = inner
    return v if isinstance(v, dict) else None


def parse_json_safe(raw):
    if raw is None:
        return None
    if isinstance(raw, (dict, list)):
        return raw
    if not isinstance(raw, str) or not raw.strip():
        return None
    try:
        return json.loads(raw)
    except Exception:
        return None


def json_values_equal(a, b):
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a == b
    if isinstance(a, (int, float)) and isinstance(b, (int, float)):
        return a == b
    return a == b


def deep_merge(base, patch):
    if isinstance(base, dict) and isinstance(patch, dict):
        kind_changed = "kind" in patch and "kind" in base and not json_values_equal(base.get("kind"), patch.get("kind"))
        form_changed = "form" in patch and "form" in base and not json_values_equal(base.get("form"), patch.get("form"))
        if kind_changed or form_changed:
            return deep_merge(None, patch)
        out = dict(base)
        for k, v in patch.items():
            out[k] = deep_merge(out[k], v) if k in out else deep_merge(None, v)
        return out
    if isinstance(patch, dict):
        return {k: deep_merge(None, v) for k, v in patch.items()}
    if isinstance(patch, list):
        return [deep_merge(None, x) for x in patch]
    return patch


def is_rollback_rewind(from_phase, to_phase, due_to):
    f = from_phase if isinstance(from_phase, str) else ""
    t = to_phase if isinstance(to_phase, str) else ""
    if not f or f == t:
        return False
    d = due_to.strip() if isinstance(due_to, str) else ""
    dl = d.lower()
    return (
        "（用户确认：" in d
        or "模型提议" in d
        or "用户同意" in d
        or dl.startswith("rollback")
        or dl.startswith("user_rewind")
        or dl.startswith("request_rewind")
    )


def merge_specs(specs):
    out = {"players": None, "strategies": None, "payoffs": None, "params": None, "game_id": None}
    for s in specs:
        if not isinstance(s, dict):
            continue
        inc = s.get("merge") is True
        if isinstance(s.get("players"), list) and len(s["players"]) > 0:
            out["players"] = s["players"]
        if isinstance(s.get("strategies"), dict) and len(s["strategies"]) > 0:
            out["strategies"] = (
                deep_merge(out["strategies"], s["strategies"])
                if inc and isinstance(out["strategies"], dict)
                else deep_merge(None, s["strategies"])
            )
        if isinstance(s.get("payoffs"), dict):
            out["payoffs"] = (
                deep_merge(out["payoffs"], s["payoffs"])
                if inc and isinstance(out["payoffs"], dict)
                else deep_merge(None, s["payoffs"])
            )
        if isinstance(s.get("params"), dict):
            out["params"] = (
                deep_merge(out["params"], s["params"])
                if inc and isinstance(out["params"], dict)
                else deep_merge(None, s["params"])
            )
        if isinstance(s.get("game_id"), str) and s["game_id"]:
            out["game_id"] = s["game_id"]
    return out


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    log_path, cards_path = sys.argv[1], sys.argv[2]
    with open(cards_path, encoding="utf-8") as f:
        cards = json.load(f)
    events = []
    with open(log_path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rec = json.loads(line)
                if rec.get("event"):
                    events.append(rec["event"])

    started = {}
    scope = None
    scope_reason = None
    has_conditional = False
    spec_builds = []
    solve_out = None
    fuse_out = None
    consequence_out = None
    discovery_out = None
    recommend_out = None
    report_model = None
    scoped_result = None
    conditional_out = None
    costs = {}
    constraints = []
    constraint_check = None
    poi_surveys = []
    research_records = []
    web_sources = {}
    failures = []
    phases = []
    phase = None
    open_map = {}
    open_order = []
    checkpoints_answered = []
    checkpoint_timeline = []
    turns = 0
    tools_count = 0
    tokens = 0

    for e in events:
        if not isinstance(e, dict) or not e.get("type"):
            continue
        t = e["type"]
        if t == "phase_entered" and e.get("to"):
            phase = e["to"]
            phases.append(e["to"])
        elif t == "task_scope_selected" and e.get("scope"):
            scope = e["scope"]
            scope_reason = e.get("reason")
            if e["scope"] == "conditional":
                has_conditional = True
        elif t == "scoped_task_completed" and isinstance(e.get("result"), dict):
            res = e["result"]
            if res.get("scope"):
                scope = res["scope"]
                if res["scope"] == "conditional":
                    has_conditional = True
            scoped_result = res
            if res.get("conditional"):
                conditional_out = res["conditional"]
                has_conditional = True
            for u in res.get("source_urls") or []:
                if u and str(u).strip() and u not in web_sources:
                    web_sources[u] = {"url": str(u)}
        elif t == "phase_rewound" and e.get("to"):
            to_phase = e["to"]
            phase = to_phase
            rank = PHASE_ORDER_INDEX.get(to_phase)
            clear_target = not is_rollback_rewind(e.get("from"), to_phase, e.get("due_to"))
            if isinstance(rank, int):
                cutoff = rank if clear_target else rank + 1
                if cutoff <= 2:
                    spec_builds = []
                if cutoff <= 3:
                    solve_out = None
                if cutoff <= 4:
                    fuse_out = None
                if cutoff <= 5:
                    consequence_out = None
                if cutoff <= 6:
                    discovery_out = None
                if cutoff <= 7:
                    recommend_out = None
                    conditional_out = None
                    scoped_result = None
                    has_conditional = False
                if cutoff <= 8:
                    report_model = None
                if rank == 0 and clear_target:
                    scope = None
                    scope_reason = None
                    has_conditional = False
                    costs = {}
                    constraints = []
                    constraint_check = None
                for cp in checkpoint_timeline:
                    if cp["status"] in ("pending", "answered"):
                        cp_rank = PHASE_ORDER_INDEX.get(cp["phase"])
                        if (isinstance(cp_rank, int) and cp_rank >= cutoff) or (to_phase == "intake" and clear_target):
                            cp["status"] = "rewound"
                for idx in range(len(open_order) - 1, -1, -1):
                    cp_id = open_order[idx]
                    cp_phase = open_map.get(cp_id)
                    cp_rank = PHASE_ORDER_INDEX.get(cp_phase)
                    if (isinstance(cp_rank, int) and cp_rank >= cutoff) or (to_phase == "intake" and clear_target):
                        open_map.pop(cp_id, None)
                        open_order.pop(idx)
            failures = []
            phases.append(f"回退→{to_phase}")
        elif t == "followup_task_started":
            was_conditional = (
                scope == "conditional"
                or has_conditional
                or bool(conditional_out)
                or (isinstance(scoped_result, dict) and scoped_result.get("scope") == "conditional")
                or e.get("prior_scope") == "conditional"
                or e.get("scope") == "conditional"
                or e.get("task_scope") == "conditional"
            )
            scope = None
            scope_reason = None
            scoped_result = None
            conditional_out = None
            has_conditional = False
            failures = []
            if was_conditional:
                spec_builds = []
                solve_out = None
                fuse_out = None
                consequence_out = None
                discovery_out = None
                recommend_out = None
                report_model = None
                constraint_check = None
                for cp in checkpoint_timeline:
                    if cp["status"] in ("pending", "answered"):
                        cp["status"] = "rewound"
                phase = "intake"
                open_map.clear()
                open_order.clear()
        elif t == "checkpoint_opened" and e.get("id"):
            cp_id = str(e["id"])
            for prev in checkpoint_timeline:
                if prev["id"] == cp_id and prev["status"] == "pending":
                    prev["status"] = "superseded"
            checkpoint_timeline.append({"id": cp_id, "phase": e.get("phase") or None, "status": "pending"})
            open_map[cp_id] = e.get("phase") or phase or None
            if cp_id in open_order:
                open_order.remove(cp_id)
            open_order.append(cp_id)
        elif t == "checkpoint_answered" and e.get("id"):
            cp_id = str(e["id"])
            checkpoints_answered.append(cp_id)
            answers = e.get("answers")
            first = str(answers[0] or "") if isinstance(answers, list) and len(answers) > 0 else ""
            is_followup = first.startswith("追问：")
            for i in range(len(checkpoint_timeline) - 1, -1, -1):
                cp = checkpoint_timeline[i]
                if cp["id"] == cp_id and cp["status"] == "pending":
                    if not is_followup:
                        cp["status"] = "answered"
                    break
            if not is_followup:
                open_map.pop(cp_id, None)
                if cp_id in open_order:
                    open_order.remove(cp_id)
        elif t == "token_usage":
            tokens += (e.get("input_tokens") or 0) + (e.get("output_tokens") or 0)
        elif t == "turn_started":
            turns += 1
        elif t == "tool_call_started" and e.get("call_id"):
            started[e["call_id"]] = e
        elif t == "tool_call_finished":
            tools_count += 1
            s = started.get(e.get("call_id"), {})
            name = s.get("name", "?")
            outcome = e.get("outcome") or {}
            status = outcome.get("status") or "unknown"
            if status != "ok":
                failures.append(e)
                continue
            if name == "sandbox_exec":
                continue
            parsed_out = parse_json_safe(outcome.get("output"))
            args_obj = parse_json_safe(s.get("arguments"))
            if name == "gamespec_build":
                cand = unwrap_spec(s.get("arguments", ""))
                if cand is not None:
                    spec_builds.append(cand)
            elif name == "solve_equilibria" and isinstance(parsed_out, dict):
                solve_out = parsed_out
            elif name == "select_fuse" and isinstance(parsed_out, dict):
                fuse_out = parsed_out
            elif name == "consequence_predict" and isinstance(parsed_out, dict):
                consequence_out = parsed_out
            elif name == "discover_satisfice" and isinstance(parsed_out, dict):
                discovery_out = parsed_out
            elif name == "recommend_build" and isinstance(parsed_out, dict):
                recommend_out = parsed_out
            elif name == "report_render" and isinstance(parsed_out, dict) and isinstance(parsed_out.get("report"), dict):
                report_model = parsed_out["report"]
            elif name == "business_cost" and isinstance(parsed_out, dict):
                key = str(parsed_out.get("scenario_id") or parsed_out.get("scenario") or e.get("call_id") or len(costs))
                costs[key] = parsed_out
            elif name == "constraint_register" and isinstance(parsed_out, dict):
                constraints.append(parsed_out)
            elif name == "constraint_check" and isinstance(parsed_out, dict):
                constraint_check = parsed_out
            elif name == "select_task_scope" and isinstance(parsed_out, dict):
                if parsed_out.get("scope"):
                    scope = parsed_out["scope"]
                    if scope == "conditional":
                        has_conditional = True
                if parsed_out.get("reason"):
                    scope_reason = parsed_out["reason"]
            elif name == "conditional_analysis":
                scope = (parsed_out.get("scope") if isinstance(parsed_out, dict) else None) or "conditional"
                if isinstance(parsed_out, dict):
                    scoped_result = parsed_out
                    if parsed_out.get("conditional"):
                        conditional_out = parsed_out["conditional"]
                if (isinstance(parsed_out, dict) and parsed_out.get("conditional")) or scope == "conditional":
                    has_conditional = True
            elif name == "poi_survey" and isinstance(parsed_out, dict):
                poi_surveys.append(parsed_out)
            elif name == "research_record" and isinstance(parsed_out, dict):
                research_records.append(parsed_out)
            elif name == "web_search" and isinstance(parsed_out, dict) and isinstance(parsed_out.get("results"), list):
                for item in parsed_out["results"]:
                    if isinstance(item, dict) and item.get("url"):
                        web_sources[item["url"]] = {"url": str(item["url"])}
            elif name == "web_fetch":
                u = (parsed_out.get("url") if isinstance(parsed_out, dict) else None) or (
                    args_obj.get("url") if isinstance(args_obj, dict) else None
                )
                if u:
                    web_sources[u] = {"url": str(u)}

    spec = merge_specs(spec_builds)

    checks = []
    checks.append(("events", cards["meta"]["events"] == len(events)))
    checks.append(("turns", cards["meta"]["turns"] == turns))
    checks.append(("tokens", cards["meta"]["tokens"] == tokens))
    checks.append(("tools", cards["meta"]["tools"] == tools_count))
    checks.append(("scope", cards["meta"].get("scope") == scope))
    checks.append(("checkpoints", cards["meta"]["checkpoints"] == checkpoints_answered))
    checks.append(("phases", cards["meta"]["phases"] == phases))
    checks.append(("pendingCheckpoint", cards.get("pendingCheckpoint") == (open_order[-1] if open_order else None)))
    checks.append((
        "checkpointTimeline",
        [(c.get("id"), c.get("phase"), c.get("status")) for c in cards["evidence"]["checkpoints"]]
        == [(c["id"], c["phase"], c["status"]) for c in checkpoint_timeline],
    ))

    if spec["players"] and spec["strategies"] and len(spec["players"]) >= 2:
        p0_id = str(spec["players"][0].get("id") or spec["players"][0].get("name") or "?")
        p0_name = str(spec["players"][0].get("name") or p0_id)
        p1_id = str(spec["players"][1].get("id") or spec["players"][1].get("name") or "?")
        p1_name = str(spec["players"][1].get("name") or p1_id)
        rows = spec["strategies"].get(p0_id) or spec["strategies"].get(p0_name) or []
        cols = spec["strategies"].get(p1_id) or spec["strategies"].get(p1_name) or []
        if rows and cols:
            checks.append(("matrix present", bool(cards.get("matrix"))))
            if cards.get("matrix"):
                pay = spec.get("payoffs") if isinstance(spec.get("payoffs"), dict) else {}
                pay_vals = pay.get("values") if isinstance(pay.get("values"), dict) else pay
                expected_form = pay.get("form") if pay.get("form") is not None else pay_vals.get("form")
                expected_cells = pay_vals.get("cells") if isinstance(pay_vals.get("cells"), dict) else None
                checks.append(("matrix rows", cards["matrix"]["rows"] == rows))
                checks.append(("matrix cols", cards["matrix"]["cols"] == cols))
                checks.append(("matrix form", cards["matrix"].get("form") == expected_form))
                checks.append(("matrix hasPayoffCells", cards["matrix"].get("hasPayoffCells") == bool(expected_cells and len(expected_cells) > 0)))
                checks.append(("matrix game_id", cards["matrix"]["game_id"] == spec.get("game_id")))
                checks.append(("matrix first key", cards["matrix"]["grid"][0]["cells"][0]["key"] == rows[0] + "," + cols[0]))
                checks.append(("matrix last key", cards["matrix"]["grid"][-1]["cells"][-1]["key"] == rows[-1] + "," + cols[-1]))
                checks.append(("matrix params", cards["matrix"].get("params") == (len(spec["params"]) if isinstance(spec.get("params"), dict) else 0)))
    else:
        checks.append(("matrix cleared", cards.get("matrix") is None))

    if solve_out:
        checks.append(("solve present", bool(cards.get("solve"))))
        if cards.get("solve"):
            checks.append(("solve route", cards["solve"]["route"] == solve_out.get("route")))
            checks.append(("solve payoffs", [e["payoffs"] for e in cards["solve"]["equilibria"]] == [e["payoffs"] for e in solve_out["equilibria"]]))
    else:
        checks.append(("solve cleared", cards.get("solve") is None))

    if fuse_out:
        checks.append(("selection present", bool(cards.get("selection"))))
        if cards.get("selection"):
            checks.append(("selection probs", [e["prob"] for e in cards["selection"]["entries"]] == [e["prob"] for e in fuse_out["entries"]]))
            checks.append(("selection confidence", cards["selection"]["confidence"] == fuse_out.get("confidence")))
    else:
        checks.append(("selection cleared", cards.get("selection") is None))

    checks.append(("costs count", len(cards["costs"]["scenarios"]) == len(costs)))
    checks.append(("constraints count", len(cards["costs"]["constraints"]) == len(constraints)))
    checks.append(("constraintCheck", bool(cards["costs"]["constraintCheck"]) == bool(constraint_check)))
    checks.append(("conditional", bool(cards["scopeDetail"]["conditional"]) == bool(conditional_out)))
    checks.append(("consequence", bool(cards["conclusion"]["consequence"]) == bool(consequence_out)))
    checks.append(("discovery", bool(cards["conclusion"]["discovery"]) == bool(discovery_out)))
    checks.append(("recommend", bool(cards["conclusion"]["recommend"]) == bool(recommend_out)))
    checks.append(("reportModel", bool(cards["conclusion"]["reportModel"]) == bool(report_model)))
    checks.append(("sources count", len(cards["evidence"]["sources"]) == len(web_sources)))
    checks.append(("poiSurveys count", len(cards["evidence"]["poiSurveys"]) == len(poi_surveys)))
    checks.append(("researchRecords count", len(cards["evidence"]["researchRecords"]) == len(research_records)))
    checks.append(("failures tail", bool(cards["failures"]) == bool(failures)))

    ok = 0
    for name, passed in checks:
        print(("PASS" if passed else "FAIL"), name)
        ok += 1 if passed else 0
    print(f"\nTOTAL {ok} / {len(checks)}")
    return 0 if ok == len(checks) else 1


if __name__ == "__main__":
    sys.exit(main())

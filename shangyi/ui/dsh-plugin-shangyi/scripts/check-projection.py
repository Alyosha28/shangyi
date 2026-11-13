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


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__)
        return 2
    log_path, cards_path = sys.argv[1], sys.argv[2]
    cards = json.load(open(cards_path, encoding="utf-8"))
    events = []
    for line in open(log_path, encoding="utf-8"):
        line = line.strip()
        if line:
            rec = json.loads(line)
            if rec.get("event"):
                events.append(rec["event"])

    started = {e["call_id"]: e for e in events if e["type"] == "tool_call_started"}
    finished = [e for e in events if e["type"] == "tool_call_finished"]

    def unwrap(raw):
        v = json.loads(raw)
        for _ in range(3):
            if isinstance(v, dict) and isinstance(v.get("arguments"), str):
                v = json.loads(v["arguments"])
            else:
                break
        return v

    spec = None
    for e in reversed(finished):
        s = started.get(e["call_id"], {})
        if s.get("name") == "gamespec_build" and e["outcome"].get("status") == "ok":
            try:
                cand = unwrap(s["arguments"])
            except Exception:
                continue
            if cand.get("players") and cand.get("strategies") and cand.get("payoffs"):
                spec = cand
                break

    def parse_ok(name):
        out = []
        for e in finished:
            s = started.get(e["call_id"], {})
            if s.get("name") == name and e["outcome"].get("status") == "ok":
                try:
                    out.append(json.loads(e["outcome"]["output"]))
                except Exception:
                    pass
        return out

    solves = parse_ok("solve_equilibria")
    fuses = parse_ok("select_fuse")
    tokens = sum((e.get("input_tokens") or 0) + (e.get("output_tokens") or 0) for e in events if e["type"] == "token_usage")
    failures = [e for e in finished if e["outcome"].get("status") != "ok"]

    checks = []
    checks.append(("events", cards["meta"]["events"] == len(events)))
    checks.append(("turns", cards["meta"]["turns"] == sum(1 for e in events if e["type"] == "turn_started")))
    checks.append(("tokens", cards["meta"]["tokens"] == tokens))
    checks.append(("tools", cards["meta"]["tools"] == len(finished)))
    checks.append(("checkpoints", cards["meta"]["checkpoints"] == [e["id"] for e in events if e["type"] == "checkpoint_answered"]))
    checks.append(("phases", cards["meta"]["phases"] == [e["to"] for e in events if e["type"] == "phase_entered"]))
    if spec and cards.get("matrix"):
        rows = spec["strategies"][spec["players"][0]["id"]]
        cols = spec["strategies"][spec["players"][1]["id"]]
        checks.append(("matrix rows", cards["matrix"]["rows"] == rows))
        checks.append(("matrix cols", cards["matrix"]["cols"] == cols))
        checks.append(("matrix game_id", cards["matrix"]["game_id"] == spec.get("game_id")))
        checks.append(("matrix first key", cards["matrix"]["grid"][0]["cells"][0]["key"] == rows[0] + "," + cols[0]))
    if solves and cards.get("solve"):
        last = solves[-1]
        checks.append(("solve route", cards["solve"]["route"] == last.get("route")))
        checks.append(("solve payoffs", [e["payoffs"] for e in cards["solve"]["equilibria"]] == [e["payoffs"] for e in last["equilibria"]]))
    if fuses and cards.get("selection"):
        last = fuses[-1]
        checks.append(("selection probs", [e["prob"] for e in cards["selection"]["entries"]] == [e["prob"] for e in last["entries"]]))
        checks.append(("selection confidence", cards["selection"]["confidence"] == last.get("confidence")))
    checks.append(("failures tail", bool(cards["failures"]) == bool(failures)))

    ok = 0
    for name, passed in checks:
        print(("PASS" if passed else "FAIL"), name)
        ok += 1 if passed else 0
    print(f"\nTOTAL {ok} / {len(checks)}")
    return 0 if ok == len(checks) else 1


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""商奕推进循环 · 控制平面（确定性，不调用任何模型）。

职责（只做规则，不做判断）：
  ready   —— 计算可派发集合：依赖满足 + touch 互斥 + 优先级 + 预算
  status  —— 打印工作图状态
  record  —— 追加账本并原子更新工作图
  gate    —— 检查 Gate A→B 与预算，未获批准时以非零码退出
  grow    —— 依据验证判定生成新工作项（图生长，单轮上限）

用法：
  python3 tools/loop/queue.py ready  --max 6
  python3 tools/loop/queue.py status
  python3 tools/loop/queue.py record --id A2 --verdict accepted --note "..." --producer X --verifier Y
  python3 tools/loop/queue.py gate   --phase B
  python3 tools/loop/queue.py grow   --from A2 --reason "缺证据" --kind verify --title "..."
"""
from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LOOP = ROOT / ".shangyi-loop"
QUEUE = LOOP / "queue.json"
LEDGER = LOOP / "ledger.jsonl"
GATES = LOOP / "gates.json"

# 依赖满足的判定集。
# 为什么 `accepted-with-followups` 也算满足：当交付物的验收标准全部达成、
# 但验证者指出可分离的缺口、缺口已生长为**独立后续工作项**时，该交付物本身
# 已可用于下游依赖，缺口由子项承接。若坚持只有干净 accept 才解锁依赖，
# 则"父项 partial + 子项依赖父项"会让子项永远不可派发（死锁）。
# 该状态仍与干净 accept 明确区分，以防"把 partial 记成 accept 让循环前进"的假收敛。
DONE = {"accepted", "accepted-with-followups"}
LIVE = {"dispatched", "in_progress"}
CLOSED = {"accepted", "accepted-with-followups", "rejected", "blocked-on-human", "superseded"}
WRITE_PHASES = {"B"}         # 需要 Gate A→B 的阶段
BUDGET = {"max_agents_per_iter": 6, "max_grow_per_iter": 3, "max_attempts": 2}


# ── io ────────────────────────────────────────────────────────────────
def now() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def load_queue() -> dict:
    if not QUEUE.exists():
        sys.exit(f"[loop] 找不到 {QUEUE}；请先初始化工作图")
    with QUEUE.open(encoding="utf-8") as fh:
        return json.load(fh)


def save_queue(q: dict) -> None:
    """原子写：临时文件 + rename，避免半截文件。"""
    q["updated_at"] = now()
    LOOP.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(LOOP), prefix=".queue.", suffix=".tmp")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(q, fh, ensure_ascii=False, indent=2)
            fh.write("\n")
        os.replace(tmp, QUEUE)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def append_ledger(rec: dict) -> None:
    """账本只追加，绝不改写。"""
    rec.setdefault("ts", now())
    LOOP.mkdir(parents=True, exist_ok=True)
    with LEDGER.open("a", encoding="utf-8") as fh:
        fh.write(json.dumps(rec, ensure_ascii=False) + "\n")


def load_gates() -> dict:
    if not GATES.exists():
        return {"gate_ab": {"approved": False, "approved_by": None, "approved_at": None}}
    with GATES.open(encoding="utf-8") as fh:
        return json.load(fh)


def by_id(q: dict) -> dict:
    return {it["id"]: it for it in q["items"]}


# ── ready ─────────────────────────────────────────────────────────────
def deps_satisfied(item: dict, idx: dict) -> bool:
    return all(idx.get(d, {}).get("status") in DONE for d in item.get("deps", []))


def touches_overlap(a: list[str], b: list[str]) -> bool:
    """路径前缀重叠即视为冲突（保守：'vendor/' 与 'vendor/x' 冲突）。"""
    for x in a:
        for y in b:
            if x == y or x.startswith(y.rstrip("/") + "/") or y.startswith(x.rstrip("/") + "/"):
                return True
    return False


def compute_ready(q: dict, max_n: int, allow_phase_b: bool) -> list[dict]:
    idx = by_id(q)
    cands = [
        it for it in q["items"]
        if it["status"] == "pending"
        and it.get("phase", "A") in ({"A", "B"} if allow_phase_b else {"A"})
        and deps_satisfied(it, idx)
    ]
    cands.sort(key=lambda it: (it.get("priority", 9), it["id"]))
    chosen: list[dict] = []
    for it in cands:
        if len(chosen) >= max_n:
            break
        if any(touches_overlap(it.get("touch", []), c.get("touch", [])) for c in chosen):
            continue
        chosen.append(it)
    return chosen


MAX_VERIFIERS = 3
VERIFIER_FANOUT = 2   # 一个验证者最多复核几个产物


def plan_budget(requested: int, total_budget: int) -> tuple[int, int]:
    """在总预算内求解 (producers, verifiers)。

    验证者数 = ceil(producers / VERIFIER_FANOUT)，上限 MAX_VERIFIERS。
    取满足 producers + verifiers <= total_budget 的最大 producers，
    使"生产者 ≠ 验证者"这条硬规则不会因为预算被悄悄放弃。
    """
    for n in range(min(requested, total_budget), 0, -1):
        v = min(MAX_VERIFIERS, -(-n // VERIFIER_FANOUT))
        if n + v <= total_budget:
            return n, v
    return 0, 0


def cmd_ready(args: argparse.Namespace) -> int:
    q = load_queue()
    gates = load_gates()
    allow_b = bool(gates.get("gate_ab", {}).get("approved"))
    budget = min(BUDGET["max_agents_per_iter"], args.max)
    n_prod, n_ver = plan_budget(budget, budget)
    chosen = compute_ready(q, n_prod, allow_b)
    blocked_b = [it["id"] for it in q["items"]
                 if it["status"] == "pending" and it.get("phase") == "B" and not allow_b]
    plan = {
        "generated_at": now(),
        "gate_ab_approved": allow_b,
        "agent_budget": budget,
        "planned_agents": len(chosen) + n_ver,
        "dispatch": [
            {"id": it["id"], "kind": it["kind"], "priority": it.get("priority", 9),
             "title": it["title"], "acceptance": it.get("acceptance", []),
             "touch": it.get("touch", []), "deps": it.get("deps", []),
             "attempts": it.get("attempts", 0)}
            for it in chosen
        ],
        "verification_plan": {
            "verifiers": n_ver,
            "fanout": VERIFIER_FANOUT,
            "rule": "生产者 ≠ 验证者；验证者只拿产物 + 验收标准，不拿生产者推理过程",
        },
        "held_phase_b": blocked_b,
        "held_by_budget": [
            it["id"] for it in q["items"]
            if it["status"] == "pending" and it.get("phase", "A") == "A"
            and deps_satisfied(it, by_id(q)) and it not in chosen
        ],
    }
    print(json.dumps(plan, ensure_ascii=False, indent=2))
    return 0


# ── status ────────────────────────────────────────────────────────────
def cmd_status(args: argparse.Namespace) -> int:
    q = load_queue()
    gates = load_gates()
    buckets: dict[str, list[str]] = {}
    for it in q["items"]:
        buckets.setdefault(it["status"], []).append(it["id"])
    print(f"工作图 {q.get('name', '')}  更新于 {q.get('updated_at', '?')}")
    print(f"Gate A→B：{'已批准' if gates.get('gate_ab', {}).get('approved') else '未批准（Phase B 冻结）'}")
    for st in ["pending", "dispatched", "in_progress", "accepted", "rejected", "blocked-on-human", "superseded"]:
        ids = buckets.get(st)
        if ids:
            print(f"  {st:<18} {len(ids):>2}  {' '.join(ids)}")
    total = len(q["items"])
    acc = len(buckets.get("accepted", []))
    print(f"  完成度：{acc}/{total}")
    return 0


# ── record ────────────────────────────────────────────────────────────
def cmd_record(args: argparse.Namespace) -> int:
    q = load_queue()
    idx = by_id(q)
    it = idx.get(args.id)
    if it is None:
        sys.exit(f"[loop] 未知工作项 {args.id}")

    if args.status:
        it["status"] = args.status
    if args.verdict == "rejected":
        it["attempts"] = it.get("attempts", 0) + 1
        if it["attempts"] >= BUDGET["max_attempts"]:
            it["status"] = "blocked-on-human"
    elif args.verdict == "accepted":
        it["status"] = "accepted"
    elif args.verdict == "partial":
        # partial 不是 accepted：保留在 pending，等 grow 出的补缺项
        it["status"] = "pending"

    if args.artifact:
        it.setdefault("artifacts", [])
        if args.artifact not in it["artifacts"]:
            it["artifacts"].append(args.artifact)

    append_ledger({
        "iter": args.iter, "item": args.id, "event": "verdict" if args.verdict else "note",
        "verdict": args.verdict, "status": it["status"], "note": args.note,
        "producer": args.producer, "verifier": args.verifier,
        "artifact": args.artifact, "attempts": it.get("attempts", 0),
    })
    save_queue(q)
    print(f"[loop] {args.id} → {it['status']}"
          + (f"（attempts={it['attempts']}）" if args.verdict == "rejected" else ""))
    return 0


# ── grow ──────────────────────────────────────────────────────────────
def cmd_grow(args: argparse.Namespace) -> int:
    q = load_queue()
    idx = by_id(q)
    if args.from_ not in idx:
        sys.exit(f"[loop] 未知来源工作项 {args.from_}")

    this_iter = [it for it in q["items"] if it.get("grew_in_iter") == args.iter]
    if len(this_iter) >= BUDGET["max_grow_per_iter"]:
        print(f"[loop] 本轮生长已达上限 {BUDGET['max_grow_per_iter']}，拒绝再生成 {args.id}")
        return 1
    if args.id in idx:
        print(f"[loop] {args.id} 已存在，跳过")
        return 0

    src = idx[args.from_]
    q["items"].append({
        "id": args.id,
        "kind": args.kind,
        "phase": args.phase,
        "priority": src.get("priority", 9),
        "title": args.title,
        "question": args.note or "",
        "acceptance": args.acceptance or [],
        "deps": [args.from_] if args.depends_on_parent else [],
        "touch": args.touch or [],
        "status": "pending",
        "attempts": 0,
        "grew_in_iter": args.iter,
        "grew_from": args.from_,
        "reason": args.reason,
    })
    append_ledger({
        "iter": args.iter, "item": args.id, "event": "grown",
        "from": args.from_, "kind": args.kind, "reason": args.reason,
    })
    save_queue(q)
    print(f"[loop] 新生工作项 {args.id}（来源 {args.from_}，原因：{args.reason}）")
    return 0


# ── gate ──────────────────────────────────────────────────────────────
def cmd_gate(args: argparse.Namespace) -> int:
    gates = load_gates()
    ab = gates.get("gate_ab", {})
    if args.phase == "A":
        print("[gate] Phase A 允许（只读；写限 research/、docs/、.shangyi-loop/）")
        return 0
    if ab.get("approved"):
        print(f"[gate] Gate A→B 已批准，Phase B 放行（批准人 {ab.get('approved_by')}）")
        # 预算检查
        q = load_queue()
        n_b = len([it for it in q["items"] if it.get("phase") == "B" and it["status"] == "pending"])
        if n_b > BUDGET["max_agents_per_iter"]:
            print(f"[gate] 警告：Phase B 待办 {n_b} 项超过单轮预算 {BUDGET['max_agents_per_iter']}，需分批")
        return 0
    print("[gate] Gate A→B **未批准** —— Phase B 冻结。\n"
          "       人类放行方式：编辑 .shangyi-loop/gates.json，设\n"
          '       {"gate_ab": {"approved": true, "approved_by": "<你的名字>", "approved_at": "<ISO 时间>"}}\n'
          "       循环自身无权修改该字段。")
    return 1


# ── cli ───────────────────────────────────────────────────────────────
def main() -> int:
    p = argparse.ArgumentParser(prog="queue", description="商奕推进循环控制平面")
    sub = p.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("ready", help="计算可派发集合")
    r.add_argument("--max", type=int, default=BUDGET["max_agents_per_iter"])
    r.set_defaults(fn=cmd_ready)

    s = sub.add_parser("status", help="打印工作图状态")
    s.set_defaults(fn=cmd_status)

    c = sub.add_parser("record", help="追加账本并更新工作图")
    c.add_argument("--id", required=True)
    c.add_argument("--verdict", choices=["accepted", "partial", "rejected"])
    c.add_argument("--status", choices=sorted(DONE | LIVE | CLOSED))
    c.add_argument("--note", default="")
    c.add_argument("--producer", default="")
    c.add_argument("--verifier", default="")
    c.add_argument("--artifact", default="")
    c.add_argument("--iter", type=int, default=0)
    c.set_defaults(fn=cmd_record)

    g = sub.add_parser("grow", help="依据判定生成新工作项（图生长）")
    g.add_argument("--from", dest="from_", required=True)
    g.add_argument("--id", required=True)
    g.add_argument("--kind", required=True, choices=["verify", "spec", "implement", "integrate", "contradiction"])
    g.add_argument("--phase", default="A", choices=["A", "B"])
    g.add_argument("--title", required=True)
    g.add_argument("--reason", required=True)
    g.add_argument("--note", default="")
    g.add_argument("--acceptance", action="append", default=[])
    g.add_argument("--touch", action="append", default=[])
    g.add_argument("--depends-on-parent", action="store_true")
    g.add_argument("--iter", type=int, default=0)
    g.set_defaults(fn=cmd_grow)

    t = sub.add_parser("gate", help="检查门与预算")
    t.add_argument("--phase", required=True, choices=["A", "B"])
    t.set_defaults(fn=cmd_gate)

    args = p.parse_args()
    return args.fn(args)


if __name__ == "__main__":
    sys.exit(main())

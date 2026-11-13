#!/usr/bin/env python3
"""收益编译器：把事实数字按算式编译成博弈矩阵（GameSpec）。

# 它解决什么

此前的矩阵要么由模型手摆（等于让它编数），要么由用户凭感觉填好坏。
两条路都拿不到「这个数字怎么来的」。本模块只做一件事：**给定数字，按
算术算出每一格**，并把算式一并交出来——矩阵从此可核对，而不是可相信。

# 模型（两方同时行动的市场份额模型）

对每个格子（我方动作 m × 对方动作 r）：

    我方单量   = 区域客流 × 我方份额 × m.份额乘数 × r.对我份额乘数
    我方营收   = 单量 × 客单价 × m.价格乘数
    我方毛利   = 营收 × 毛利率 × m.毛利乘数
    我方月净利 = 毛利 − 月固定成本          （m 为「暂不开」时记 0）

对方同式，只是各自的参数。跨方影响必须**显式声明**（`opponent_share_mult`）：
不声明就按 1.0 算，**并计入 `unmodeled`** 如实说明「此格未考虑跨方抢客」。
不声明不等于没有影响——这一点不藏。

# 模型假设（写在明处）

- **单位成本不随价格与销量变化**：降价后的毛利率由 `(新价 − 单位成本) / 新价`
  精算，不需要用户填；填了则以填的为准。
- 跨方影响必须显式声明，否则按 1.0 并计入 `unmodeled`。

# 拒绝条件（宁可不算，不猜）

- 任何一方缺 share / price / margin / fixed_monthly / moves ⇒ 整体拒绝，
  逐条列出缺什么（`incomplete`）。
- 动作名含逗号（GameSpec 的 cells 键用逗号分隔策略）或重名 ⇒ 拒绝。
- 数值非有限数或为负（份额、价格、成本）⇒ 拒绝。
"""

from __future__ import annotations

import math

# 一格的算式要能被人读懂，数字统一按中文习惯压缩。
def _wan(value: float) -> str:
    return f"{value / 10000:.2f}万".rstrip("0").rstrip(".")


def _num(value) -> float | None:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    return number if math.isfinite(number) else None


def _positive(value) -> bool:
    number = _num(value)
    return number is not None and number >= 0


def _mult(move: dict, key: str) -> float | None:
    if key not in move or move.get(key) is None:
        return None
    number = _num(move.get(key))
    return number if number is not None and number >= 0 else None


def _check_side(side: dict, label: str, problems: list) -> bool:
    for key in ("share", "price", "margin", "fixed_monthly"):
        if not _positive(side.get(key)):
            problems.append(f"{label}的「{key}」缺失或不合法")
    moves = side.get("moves")
    if not isinstance(moves, list) or len(moves) < 2:
        problems.append(f"{label}至少要有两种动作")
        return False
    names = []
    for move in moves:
        if not isinstance(move, dict):
            problems.append(f"{label}有动作不是对象")
            return False
        name = str(move.get("name") or "").strip()
        if not name:
            problems.append(f"{label}有动作没写名字")
            continue
        if "," in name or "，" in name:
            problems.append(f"{label}的动作名不能含逗号：{name}")
        if name in names:
            problems.append(f"{label}的动作重名：{name}")
        names.append(name)
        if move.get("closed"):
            continue
        for key in ("share_mult", "price_mult"):
            if _mult(move, key) is None:
                problems.append(f"{label}的动作「{name}」缺 {key}")
        for key in ("price_mult", "margin_mult"):
            value = _mult(move, key)
            if value is not None and value > 1.5:
                problems.append(f"{label}的动作「{name}」{key} 超过 1.5，先确认再算")
    return True


def _effective_margin_mult(side: dict, move: dict) -> tuple[float, bool]:
    """毛利率乘数：给了就用给的；没给就按「单位成本不变」精算。

    降价不等于毛利率等比例下降——单位成本不动时，新毛利率是
    (新价 − 单位成本) / 新价，这里把它算出来，而不是让用户凭感觉填。
    """
    given = _mult(move, "margin_mult")
    if given is not None:
        return given, False
    unit_cost = side["price"] * (1 - side["margin"])
    new_price = side["price"] * move.get("price_mult", 1.0)
    if new_price <= 0:
        return 1.0, False
    return ((new_price - unit_cost) / new_price) / side["margin"], True


def _side_profit(side: dict, move: dict, market: dict, cross_mult: float, own_share_mult: float) -> tuple[float, str]:
    """算一方的月净利，返回（金额, 算式）。算式里保留每个因素，便于核对。"""
    margin_mult, derived = _effective_margin_mult(side, move)
    orders = market["traffic"] * side["share"] * own_share_mult * cross_mult
    revenue = orders * side["price"] * move.get("price_mult", 1.0)
    gross = revenue * side["margin"] * margin_mult
    profit = gross - side["fixed_monthly"]
    formula = (
        f"客流 {_wan(market['traffic'])} × 份额 {side['share']:.0%}"
        + (f" × {own_share_mult:g}" if own_share_mult != 1 else "")
        + (f" × 跨方 {cross_mult:g}" if cross_mult != 1 else "")
        + f" = {_wan(orders)}单；× 客单 {side['price'] * move.get('price_mult', 1.0):g}元 × 毛利 {side['margin'] * margin_mult:.0%}"
        + ("（毛利率按单位成本不变推导）" if derived else "")
        + f" = 毛利 {_wan(gross)}；− 固定 {_wan(side['fixed_monthly'])} = {_wan(profit)}"
    )
    return profit, formula


def compile_payoffs(bundle: dict) -> dict:
    """数字 → {spec, cells, incomplete, unmodeled}。算不了就只回 incomplete。"""
    bundle = bundle if isinstance(bundle, dict) else {}
    problems: list[str] = []
    market = bundle.get("market") if isinstance(bundle.get("market"), dict) else {}
    traffic = _num(market.get("traffic"))
    if traffic is None or traffic <= 0:
        problems.append("区域月客流缺失或不合法")
    me = bundle.get("me") if isinstance(bundle.get("me"), dict) else {}
    rival = bundle.get("rival") if isinstance(bundle.get("rival"), dict) else {}
    ok_me = _check_side(me, "我方", problems)
    ok_rival = _check_side(rival, "对方", problems)
    if problems or not ok_me or not ok_rival:
        return {"ok": False, "incomplete": problems}

    market = {"traffic": traffic}
    unmodeled: list[str] = []

    def cross(move: dict, label: str) -> float:
        value = _mult(move, "opponent_share_mult")
        if value is None:
            unmodeled.append(f"「{label}」没声明对对方份额的影响，按 1.0 算")
            return 1.0
        return value

    me_moves, rival_moves = me["moves"], rival["moves"]
    cells = {}
    spec_cells = {}
    for m in me_moves:
        for r in rival_moves:
            key = f"{m['name']},{r['name']}"
            m_cross = cross(r, r["name"])   # 对方的动作如何影响我
            r_cross = cross(m, m["name"])   # 我的动作如何影响对方
            if m.get("closed"):
                me_profit, me_formula = 0.0, "暂不开：没有营收也不花固定成本，记 0"
            else:
                me_profit, me_formula = _side_profit(me, m, market, m_cross, m.get("share_mult", 1.0))
            if r.get("closed"):
                rival_profit, rival_formula = 0.0, "对方不做：记 0"
            else:
                rival_profit, rival_formula = _side_profit(rival, r, market, r_cross, r.get("share_mult", 1.0))
            me_profit, rival_profit = round(me_profit), round(rival_profit)
            cells[(m["name"], r["name"])] = {
                "me_profit": me_profit, "rival_profit": rival_profit,
                "formula": {"me": me_formula, "rival": rival_formula},
            }
            spec_cells[key] = {"me": {"kind": "literal", "value": me_profit},
                               "rival": {"kind": "literal", "value": rival_profit}}

    me_name = str(me.get("name") or "我方")
    rival_name = str(rival.get("name") or "对方")
    spec = {
        "game_id": "web-model",
        "players": [
            {"id": "me", "name": me_name, "vn_role": "focal", "relation_to_game": "player"},
            {"id": "rival", "name": rival_name, "vn_role": "substitutor", "relation_to_game": "player"},
        ],
        "strategies": {"me": [m["name"] for m in me_moves], "rival": [r["name"] for r in rival_moves]},
        "payoffs": {"form": "normal", "values": {"cells": spec_cells}},
        "params": {},
        "information": {"type": "complete"},
        "dynamics": {"form": "one_shot"},
        "meta": {"origin": "web-model", "unit": "元/月"},
    }

    warnings: list[str] = []
    if not any(_mult(m, "opponent_share_mult") is not None for m in (*me_moves, *rival_moves)
               if isinstance(m, dict)):
        warnings.append("两边动作互不影响：矩阵会退化，求解只能看出各自哪个动作更划算，说明不了博弈")

    upfront = _num(me.get("upfront")) or 0
    payback = {}
    for key, cell in cells.items():
        if upfront and cell["me_profit"] > 0:
            payback[key[0] + "," + key[1]] = round(upfront / cell["me_profit"], 1)
    return {"ok": True, "spec": spec,
            "cells": [{"me_move": k[0], "rival_move": k[1], **v} for k, v in cells.items()],
            "payback_months": payback,
            "unmodeled": sorted(set(unmodeled)),
            "warning": warnings,
            "assumptions": ["单位成本不随价格与销量变化",
                            "降价对毛利率的影响按上式推导，不是拍出来的"]}


# ── 命令行：与 solve 同款的机器接口（供脚本/测试直接管道） ──────────────────
if __name__ == "__main__":
    import json
    import sys

    try:
        payload = json.load(sys.stdin)
    except ValueError as error:
        print(json.dumps({"ok": False, "error": f"输入不是 JSON：{error}"}, ensure_ascii=False))
        raise SystemExit(2)
    print(json.dumps(compile_payoffs(payload), ensure_ascii=False))

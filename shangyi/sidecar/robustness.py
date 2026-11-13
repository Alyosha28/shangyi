#!/usr/bin/env python3
"""稳健性扫描：哪个假设一变，结论就翻面。

# 为什么需要它

算出「均衡是降价跟进、月净利 12.6 万」并不等于这个结论可用——它建立在
客流、份额、抢客力度这些**假设**上。本模块逐个把假设按比例推偏，重编译、
重求解，找出**第一个让均衡改变的幅度**：那就是这套结论的生死线。

# 口径

- 一个「结论」= 纯策略均衡的集合（按策略下标排序后的指纹）。指纹变了才算翻面，
  收益变化不算翻面——收益随参数连续变化，均衡改變才是质变。
- 每项假设只报告**最靠近 1.0 的翻面点**（最脆弱之处），方向分涨/跌。
- 扫不过去就如实说「在扫描范围内没翻面」，不含糊成「稳健」。

求解器由调用方注入（`solve`），因此本模块可脱离引擎单测；生产里注入的是
`shangyi solve` 子进程。
"""

from __future__ import annotations

from payoff_model import compile_payoffs

# 默认扫描比例：先温和后极端，细密到能指出「跌到四成才翻面」这种话。
DEFAULT_SCALES = (0.9, 0.8, 0.7, 0.6, 0.5, 0.4, 0.3, 1.1, 1.25, 1.5, 2.0)

# 因子：path 指向 bundle 里的数值；mode=complement 表示该值是「被抢走/抢来」的比例，
# 扰动的是它的强度 (1−v)，因为 0.9 再乘 1.2 会变成「比没影响还强」，没有意义。
FACTOR_SPECS = (
    {"id": "traffic", "label": "区域月客流", "path": ("market", "traffic"), "mode": "scale"},
    {"id": "my_share", "label": "我的份额", "path": ("me", "share"), "mode": "scale"},
    {"id": "my_fixed", "label": "我的每月固定成本", "path": ("me", "fixed_monthly"), "mode": "scale"},
    {"id": "my_grab", "label": "我降价抢客力度", "path": ("me", "moves", 1, "share_mult"), "mode": "strength"},
    {"id": "my_take", "label": "我从对方抢走的客流", "path": ("me", "moves", 1, "opponent_share_mult"), "mode": "complement"},
    {"id": "rival_grab", "label": "对方降价抢客力度", "path": ("rival", "moves", 1, "share_mult"), "mode": "strength"},
    {"id": "rival_take", "label": "对方降价抢我的客流", "path": ("rival", "moves", 1, "opponent_share_mult"), "mode": "complement"},
)


def _dig(bundle: dict, path):
    node = bundle
    for key in path:
        if isinstance(node, dict):
            if key not in node:
                return None
            node = node[key]
        elif isinstance(node, list) and isinstance(key, int) and 0 <= key < len(node):
            node = node[key]
        else:
            return None
    return node


def _put(bundle: dict, path, value):
    node = bundle
    for key in path[:-1]:
        node = node[key] if isinstance(node, dict) else node[key]
    node[path[-1]] = value


def perturb(bundle: dict, factor: dict, scale: float):
    """按比例推偏一个因子，返回新的 bundle（不改原对象）。

    - scale：原值 × scale（客流、份额、成本、自身抢客力度）
    - complement：扰动「强度」1−v，避免出现比「毫无影响」更强的荒谬值
    - strength：把「多出来的那部分」按比例缩放，例如 1.33 → 1 + 0.33×scale
    """
    current = _dig(bundle, factor["path"])
    if not isinstance(current, (int, float)):
        return None
    if factor["mode"] == "complement":
        value = 1 - (1 - current) * scale
    elif factor["mode"] == "strength":
        value = 1 + (current - 1) * scale
    else:
        value = current * scale
    if value < 0:
        return None
    copy = _deep_copy(bundle)
    _put(copy, factor["path"], value)
    return copy


def _deep_copy(value):
    if isinstance(value, dict):
        return {k: _deep_copy(v) for k, v in value.items()}
    if isinstance(value, list):
        return [_deep_copy(v) for v in value]
    return value


def equilibrium_fingerprint(result: dict):
    """纯策略均衡指纹：排序后的 (我行下标, 对方行下标) 列表。"""
    if not isinstance(result, dict):
        return []
    pairs = []
    for eq in result.get("equilibria") or []:
        if not isinstance(eq, dict):
            continue
        if eq.get("kind") != "Pure":
            continue
        support = eq.get("support") or []
        if len(support) != 2 or len(support[0]) != 1 or len(support[1]) != 1:
            continue
        pairs.append((support[0][0], support[1][0]))
    return sorted(pairs)


def describe_equilibrium(strategies: dict, fingerprint) -> str:
    """指纹 → 人话：「降价10% × 照常」。"""
    if not fingerprint:
        return "没有纯策略均衡"
    me = strategies.get("me") or []
    rival = strategies.get("rival") or []
    parts = []
    for i, j in fingerprint:
        left = me[i] if 0 <= i < len(me) else f"#{i}"
        right = rival[j] if 0 <= j < len(rival) else f"#{j}"
        parts.append(f"{left} × {right}")
    return "、".join(parts)


def my_payoff(result: dict, fingerprint) -> float | None:
    """我在结论里的月净利：取第一个均衡的收益（引擎按 id 排序，稳定）。"""
    if not isinstance(result, dict) or not fingerprint:
        return None
    for eq in result.get("equilibria") or []:
        if not isinstance(eq, dict) or eq.get("kind") != "Pure":
            continue
        payoffs = eq.get("payoffs") or []
        if payoffs:
            try:
                return round(float(payoffs[0]))
            except (TypeError, ValueError):
                return None
    return None


def scan(bundle: dict, solve, factors=None, scales=DEFAULT_SCALES) -> dict:
    """逐个推偏假设，回报首个翻面点。solve(spec) → 引擎解集 JSON。"""
    baseline = compile_payoffs(bundle)
    if not baseline.get("ok"):
        return {"ok": False, "incomplete": baseline.get("incomplete", []), "reason": baseline.get("reason")}
    baseline_result = solve(baseline["spec"])
    baseline_print = equilibrium_fingerprint(baseline_result)
    strategies = baseline["spec"]["strategies"]

    all_factors = factors if factors is not None else FACTOR_SPECS
    checks = []
    for factor in all_factors:
        if _dig(bundle, factor["path"]) is None:
            continue
        points = []
        flip = None
        for scale in sorted(scales, key=lambda s: abs(s - 1.0)):
            copy = perturb(bundle, factor, scale)
            if copy is None:
                continue
            compiled = compile_payoffs(copy)
            if not compiled.get("ok"):
                continue
            result = solve(compiled["spec"])
            print_now = equilibrium_fingerprint(result)
            point = {
                "scale": scale,
                "equilibrium": describe_equilibrium(compiled["spec"]["strategies"], print_now),
                "me_profit": my_payoff(result, print_now),
                "changed": print_now != baseline_print,
            }
            points.append(point)
            if point["changed"] and flip is None:
                flip = point
        if not points:
            continue
        profits = [p["me_profit"] for p in points if isinstance(p["me_profit"], (int, float))]
        checks.append({
            "id": factor["id"], "label": factor["label"], "mode": factor["mode"],
            "flip": flip, "points": points,
            "worst_me_profit": min(profits) if profits else None,
            "best_me_profit": max(profits) if profits else None,
        })

    fragile = [c for c in checks if c["flip"]]
    fragile.sort(key=lambda c: abs(c["flip"]["scale"] - 1.0))
    return {
        "ok": True,
        "baseline": {
            "equilibrium": describe_equilibrium(strategies, baseline_print),
            "fingerprint": [list(pair) for pair in baseline_print],
            "me_profit": my_payoff(baseline_result, baseline_print),
        },
        "checks": checks,
        "fragile": [{"label": c["label"], "scale": c["flip"]["scale"],
                     "equilibrium": c["flip"]["equilibrium"]} for c in fragile],
        "scales": list(scales),
    }


if __name__ == "__main__":
    import json
    import sys

    print(json.dumps({"ok": False, "error": "本模块需要注入求解器；请用 /api/robustness"}, ensure_ascii=False))
    sys.exit(2)

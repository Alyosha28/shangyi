#!/usr/bin/env python3
"""决策帧投影：把「机器现在怎么理解这个问题」显式化成可读的槽位。

# 为什么需要它

求解引擎（均衡/后果/满意化）本身是通用的，但用户看不到机器把这句人话
理解成了什么：类型、能动的人、已知事实、假设、缺口。这些此前散在对话里，
用户只能从模型的追问里猜。本模块把它们投影成一张**显式帧**，供「看依据」
面板展示——让「机器怎么理解你」可核对、可纠正。

# 三态规则（唯一的诚实性约束）

每个槽只有三种状态，且**只报已有的事实**：

- `facts`（已核实）：检索到的字段，且原文里逐字存在（带 quote 与来源网址）。
- `assumptions`（靠输入与假设）：你提供的、公开报价、分析假设——每项标注 `via`。
- `gaps`（还缺）：引擎或费用清单明确标为未取得的项目。

**不推测**：没有的项目不出现在帧里，缺的项目必须出现在 gaps。这样帧本身
不会冒充「已经知道」。

# 纯函数

不读磁盘、不联网：输入是引擎已产出的 artifacts 与会话摘要，输出是 dict。
可与 `business_cost_projection` / `web_source_projection` 一样被单元测试直接调用。
"""

from __future__ import annotations

PLAYBOOK_CN = {
    "pricing_response": "定价",
    "negotiation": "谈条件",
    "market_entry": "新生意",
    "competitor_intel": "看对手",
}
MODE_CN = {"scenario": "看可能", "whatif": "试做法", "discover": "找做法"}
SCOPE_CN = {
    "research": "资料整理",
    "cost": "费用整理",
    "research_and_cost": "资料和费用",
    "consultation": "完整咨询",
}
BASIS_CN = {"user_provided": "你给的", "public_quote": "公开报价", "assumption": "分析假设"}
TREATMENT_CN = {
    "cash_expense": "现金花费",
    "refundable_deposit": "能退的押金",
    "opportunity_cost": "让出的机会",
    "noncash_expense": "不掏钱的费用",
    "unknown": "性质待确认",
}
# 「未取得」的写法在数据里有多种（金额未取得 / 未取得 / None），统一识别。
_EMPTY = ("", "未取得", "未知", "未注明", "none")


def _text(value) -> str:
    if value is None:
        return ""
    if isinstance(value, (int, float)):
        return f"{value:,}".rstrip("0").rstrip(".") if isinstance(value, float) else f"{value:,}"
    return str(value).strip()


def _is_empty(value) -> bool:
    text = _text(value)
    return text == "" or text.lower() in _EMPTY


def _first_line(value, limit=120) -> str:
    line = (str(value or "").split("\n")[0] or "").strip()
    return line[:limit]


def _players(artifacts: dict) -> list[dict]:
    """局里有谁、各自能做什么——取自已摆好的 GameSpec；没摆局就是空表。"""
    spec = artifacts.get("gamespec") or {}
    spec = spec.get("spec", spec) if isinstance(spec, dict) else {}
    strategies = spec.get("strategies") or {}
    players = []
    for index, player in enumerate(spec.get("players") or []):
        if not isinstance(player, dict):
            continue
        if (player.get("relation_to_game") or "player") != "player":
            continue
        pid = player.get("id")
        players.append({
            "name": player.get("name") or pid or "未命名",
            "side": "我方" if (player.get("vn_role") == "focal" or index == 0) else "对方",
            "moves": [m for m in (strategies.get(pid) or []) if isinstance(m, str)][:4],
        })
    return players


def _facts_and_gaps(artifacts: dict) -> tuple[list, list, list]:
    """检索字段 → 已核实；费用条目 → 按 basis 归入你说的/假设；金额缺失 → 缺口。"""
    facts, assumptions, gaps = [], [], []
    for record in (artifacts.get("research") or {}).get("records", []) or []:
        if not isinstance(record, dict):
            continue
        url = record.get("url")
        for key, item in (record.get("fields") or {}).items():
            item = item if isinstance(item, dict) else {}
            if _is_empty(item.get("value")):
                gaps.append({"label": str(key), "why": "公开资料未取得"})
                continue
            if item.get("quote"):
                facts.append({"label": str(key), "value": _text(item.get("value")), "source": url})
            else:
                assumptions.append({"label": str(key), "value": _text(item.get("value")), "via": "资料整理"})

    # 周边计数是坐标级事实（有来源、有地址与半径），归入「已核实」。
    for survey in artifacts.get("poi_surveys") or []:
        if not isinstance(survey, dict):
            continue
        where = survey.get("address") or ""
        radius = survey.get("radius")
        source = survey.get("source") or "周边数据源"
        label_prefix = "周边"
        for item in survey.get("counts") or []:
            if not isinstance(item, dict):
                continue
            facts.append({"label": f"{label_prefix}·{item.get('label')}",
                          "value": f"{item.get('count')} 家",
                          "source": f"{source}（{where} {radius}米）".strip()})
        brand = survey.get("brand")
        if isinstance(brand, dict):
            facts.append({"label": f"{label_prefix}·{brand.get('label')}",
                          "value": f"{brand.get('count')} 家",
                          "source": f"{source}（{where} {radius}米）".strip()})
        keywords = survey.get("keywords")
        if isinstance(keywords, dict):
            facts.append({"label": f"{label_prefix}·{keywords.get('label')}",
                          "value": f"{keywords.get('count')} 家",
                          "source": f"{source}（{where} {radius}米）".strip()})
        for missing in survey.get("unavailable") or []:
            text = str(missing)
            label = text.split("：", 1)[0] if "：" in text else text
            gaps.append({"label": f"周边·{label}", "why": "这次没取得"})

    costs = artifacts.get("business_costs")
    if not isinstance(costs, list):
        costs = [artifacts["business_cost"]] if artifacts.get("business_cost") else []
    for cost in costs:
        if not isinstance(cost, dict):
            continue
        for item in cost.get("items") or []:
            if not isinstance(item, dict):
                continue
            name = _text(item.get("name") or (item.get("original") or {}).get("name")) or "费用"
            treatment = TREATMENT_CN.get(item.get("treatment") or (item.get("original") or {}).get("treatment"), "")
            label = f"{name}（{treatment}）" if treatment else name
            amount = item.get("normalized_amount")
            if _is_empty(amount):
                gaps.append({"label": label, "why": "金额未取得"})
                continue
            via = BASIS_CN.get((item.get("original") or {}).get("basis"), "费用整理")
            assumptions.append({"label": label, "value": _text(amount), "via": via})
    return facts, assumptions, gaps


def decision_frame(*, question="", topic=None, mode=None, scope=None,
                   action=None, artifacts=None, missing=None) -> dict:
    """把当前理解投影成显式决策帧。缺什么就空什么，不补默认值。"""
    artifacts = artifacts if isinstance(artifacts, dict) else {}
    facts, assumptions, gaps = _facts_and_gaps(artifacts)
    for item in missing or []:
        text = _text(item)
        if text and not any(g["label"] == text for g in gaps):
            gaps.append({"label": text, "why": "这次没取得"})

    equilibria = artifacts.get("equilibria") or {}
    count = len(equilibria.get("equilibria") or []) if isinstance(equilibria, dict) else 0
    result = None
    if count or artifacts.get("recommendation"):
        completeness = equilibria.get("completeness") if isinstance(equilibria, dict) else None
        result = {
            "equilibria": count,
            "completeness": "找全了" if completeness == "Complete" else ("可能没找全" if completeness else None),
            "has_recommendation": bool(artifacts.get("recommendation")),
        }

    return {
        "question": _first_line(question),
        "type": PLAYBOOK_CN.get(topic) or (_text(topic) or None),
        "mode": MODE_CN.get(mode) or (_text(mode) or None),
        "scope": SCOPE_CN.get(scope) or (_text(scope) or None),
        "move": ({"side": "我方", "value": _text(action), "via": "你指定的"}
                 if action else {"side": "我方", "value": None, "via": None}),
        "players": _players(artifacts),
        "facts": facts,
        "assumptions": assumptions,
        "gaps": gaps,
        "result": result,
    }

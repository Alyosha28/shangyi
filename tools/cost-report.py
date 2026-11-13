#!/usr/bin/env python3
"""从案例日志算 token 用量与成本。

用法：
    python3 tools/cost-report.py <case.jsonl> [--provider deepseek|glm]

为什么需要它：在 TokenUsage 事件之前，商奕完全不知道自己花了多少 token，
于是"哪个模型贵、该换哪个"只能靠外部文档猜。这个脚本把成本变成**实测数据**。

价格表只收**已核实的来源**（网页实测抓取）。GLM 的定价页是 JS 空壳抓不到，
故其价格留空并**明确标注未知**——不填一个凭记忆猜的数字（那比留空更坏：
它会让人以为算出来的成本有依据）。
"""
import json
import sys
from collections import defaultdict

# 已核实的单价（USD / 百万 tokens）
# 来源：DeepSeek 官方定价页 https://api-docs.deepseek.com/quick_start/pricing，
#       2026-09-18 网页实测抓取。低峰价为高峰价的一半，此处取**高峰价**
#       （保守：算出来的是上界）。
# GLM：定价页为动态渲染（3.9KB 空壳，无内嵌数据），**未能核实**，
#       故不在表中。要算 GLM 成本请填入你账户的实际单价。
PRICING = {
    "deepseek": {
        # model: (缓存命中输入, 未命中输入, 输出)
        "deepseek-flash": (0.006, 0.30, 1.20),
        "deepseek-v4-pro": (0.044, 1.32, 3.96),
    },
    # GLM 价格未核实 ⇒ 留空。**不猜**。
    "glm": {},
}


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    path = sys.argv[1]
    provider = "deepseek"
    if "--provider" in sys.argv:
        provider = sys.argv[sys.argv.index("--provider") + 1]

    per_model = defaultdict(
        lambda: {"calls": 0, "in": 0, "cached": 0, "out": 0, "reasoning": 0}
    )
    for line in open(path, encoding="utf-8"):
        line = line.strip()
        if not line:
            continue
        try:
            ev = json.loads(line)["event"]
        except (json.JSONDecodeError, KeyError):
            continue
        if ev.get("type") != "token_usage":
            continue
        m = per_model[ev.get("model", "?")]
        m["calls"] += 1
        m["in"] += ev.get("input_tokens", 0)
        m["cached"] += ev.get("cached_input_tokens", 0)
        m["out"] += ev.get("output_tokens", 0)
        m["reasoning"] += ev.get("reasoning_tokens", 0)

    if not per_model:
        print("日志里没有 token_usage 事件。")
        print("（该事件是 2026-09-18 新增的；早于此的日志不会有。）")
        return 1

    table = PRICING.get(provider, {})
    print(f"─── 用量（provider={provider}）───")
    total_cost = 0.0
    unpriced = []
    for model, m in sorted(per_model.items()):
        uncached = max(0, m["in"] - m["cached"])
        hit = m["cached"] / m["in"] if m["in"] else 0.0
        rr = m["reasoning"] / m["out"] if m["out"] else 0.0
        print(f"\n模型 {model}")
        print(f"  调用次数      {m['calls']}")
        print(f"  输入 tokens   {m['in']:,}（缓存命中 {m['cached']:,}，命中率 {hit:.0%}）")
        print(f"  输出 tokens   {m['out']:,}（其中思考 {m['reasoning']:,}，占 {rr:.0%}）")
        if model in table:
            ci, cm, co = table[model]
            cost = m["cached"] / 1e6 * ci + uncached / 1e6 * cm + m["out"] / 1e6 * co
            total_cost += cost
            print(f"  估算成本      ${cost:.4f}（按高峰价，低峰减半）")
            print("    构成：", end="")
            print(
                f"缓存输入 ${m['cached']/1e6*ci:.4f} + "
                f"未命中输入 ${uncached/1e6*cm:.4f} + "
                f"输出 ${m['out']/1e6*co:.4f}"
            )
        else:
            unpriced.append(model)
            print("  估算成本      **未知**（该模型单价未核实——不猜）")

    if total_cost:
        print(f"\n─── 合计（已定价部分）${total_cost:.4f} ───")
    if unpriced:
        print(
            f"\n⚠ 以下模型单价未核实，未计入：{', '.join(unpriced)}\n"
            f"  要算它们请把账户实际单价填进本脚本的 PRICING 表——\n"
            f"  填一个凭记忆猜的数字比留空更坏：它会让结果看起来有依据。"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())

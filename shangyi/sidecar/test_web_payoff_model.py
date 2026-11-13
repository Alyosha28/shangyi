#!/usr/bin/env python3
"""收益编译器的单元测试：数字必须算得准，算不了必须拒。"""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

from payoff_model import compile_payoffs


def bundle(**overrides):
    base = {
        "market": {"traffic": 400000},
        "me": {"share": 0.06, "price": 24, "margin": 0.6, "fixed_monthly": 330000, "upfront": 1200000,
               "moves": [{"name": "原价开店", "share_mult": 1.0, "price_mult": 1.0, "margin_mult": 1.0},
                         {"name": "降价10%", "share_mult": 1.33, "price_mult": 0.9, "margin_mult": 0.93},
                         {"name": "暂不开", "closed": True}]},
        "rival": {"share": 0.35, "price": 22, "margin": 0.6, "fixed_monthly": 900000,
                  "moves": [{"name": "照常", "share_mult": 1.0, "price_mult": 1.0, "margin_mult": 1.0},
                            {"name": "降价跟进", "share_mult": 1.15, "price_mult": 0.9, "margin_mult": 0.93}]},
    }
    base.update(overrides)
    return base


def cell(result, me_move, rival_move):
    for c in result["cells"]:
        if c["me_move"] == me_move and c["rival_move"] == rival_move:
            return c
    raise AssertionError(f"没有这一格：{me_move} × {rival_move}")


class CompileArithmetic(unittest.TestCase):
    """手算基准：400000×6%×24×60% − 330000 = 15600。"""

    def test_plain_cell_matches_hand_calculation(self):
        result = compile_payoffs(bundle())
        self.assertTrue(result["ok"])
        plain = cell(result, "原价开店", "照常")
        self.assertEqual(plain["me_profit"], 15600)
        self.assertEqual(plain["rival_profit"], 948000)  # 400000×35%×22×60% − 900000

    def test_move_multipliers_apply_to_share_price_and_margin(self):
        result = compile_payoffs(bundle())
        discounted = cell(result, "降价10%", "降价跟进")
        # 400000×6%×1.33 = 31920 单；×24×0.9 = 689472；×60%×0.93 = 384725.376；−330000
        self.assertEqual(discounted["me_profit"], 54725)
        self.assertEqual(discounted["rival_profit"], 878792)

    def test_closed_move_costs_nothing_and_earns_nothing(self):
        result = compile_payoffs(bundle())
        closed = cell(result, "暂不开", "照常")
        self.assertEqual(closed["me_profit"], 0)
        self.assertIn("暂不开", closed["formula"]["me"])
        self.assertEqual(closed["rival_profit"], 948000, "我不开店不影响对方当格默认算式")

    def test_every_cell_carries_a_readable_formula(self):
        result = compile_payoffs(bundle())
        formula = cell(result, "原价开店", "照常")["formula"]["me"]
        for token in ("客流", "份额", "客单", "毛利", "固定"):
            self.assertIn(token, formula)
        self.assertIn("1.56万", formula)

    def test_payback_months_uses_upfront_over_monthly_profit(self):
        result = compile_payoffs(bundle())
        self.assertEqual(result["payback_months"]["原价开店,照常"], 76.9)  # 120万 ÷ 1.56万
        self.assertNotIn("暂不开,照常", result["payback_months"], "不赚钱的格不给回本月数")


class CompileCrossEffects(unittest.TestCase):
    def test_undeclared_cross_effects_are_reported_not_silently_assumed(self):
        result = compile_payoffs(bundle())
        self.assertEqual(len(result["unmodeled"]), 5, "五个动作各一条：三个我方 + 两个对方")
        self.assertTrue(all("没声明对对方份额的影响" in item for item in result["unmodeled"]))

    def test_declared_cross_effect_shifts_the_other_side(self):
        data = bundle()
        data["me"]["moves"][1]["opponent_share_mult"] = 0.9   # 我降价，对方份额 ×0.9
        data["rival"]["moves"][1]["opponent_share_mult"] = 0.8  # 对方降价，我份额 ×0.8
        result = compile_payoffs(data)
        discounted = cell(result, "降价10%", "降价跟进")
        # 我：份额 1.33 × 0.8 = 1.064 ⇒ 400000×6%×1.064×24×0.9×60%×0.93 − 330000
        self.assertEqual(discounted["me_profit"], round(400000 * 0.06 * 1.33 * 0.8 * 24 * 0.9 * 0.6 * 0.93 - 330000))
        # 对方：份额 1.15 × 0.9
        self.assertEqual(discounted["rival_profit"], round(400000 * 0.35 * 1.15 * 0.9 * 22 * 0.9 * 0.6 * 0.93 - 900000))
        self.assertFalse([u for u in result["unmodeled"] if "降价" in u], "声明过的动作不再进未建模清单")


class CompileRefusals(unittest.TestCase):
    def test_missing_traffic_refuses_the_whole_matrix(self):
        result = compile_payoffs(bundle(market={}))
        self.assertFalse(result["ok"])
        self.assertIn("区域月客流缺失或不合法", result["incomplete"])
        self.assertNotIn("spec", result)

    def test_missing_cost_or_price_is_named_per_side(self):
        data = bundle()
        del data["me"]["margin"]
        result = compile_payoffs(data)
        self.assertFalse(result["ok"])
        self.assertIn("我方的「margin」缺失或不合法", result["incomplete"])

    def test_single_move_cannot_make_a_game(self):
        data = bundle()
        data["rival"]["moves"] = data["rival"]["moves"][:1]
        result = compile_payoffs(data)
        self.assertFalse(result["ok"])
        self.assertIn("对方至少要有两种动作", result["incomplete"])

    def test_comma_in_move_name_is_refused_because_cells_are_comma_joined(self):
        data = bundle()
        data["me"]["moves"][0]["name"] = "原价, 开店"
        result = compile_payoffs(data)
        self.assertFalse(result["ok"])
        self.assertIn("我方的动作名不能含逗号：原价, 开店", result["incomplete"])

    def test_duplicate_move_names_are_refused(self):
        data = bundle()
        data["rival"]["moves"][1]["name"] = "照常"
        result = compile_payoffs(data)
        self.assertFalse(result["ok"])
        self.assertIn("对方的动作重名：照常", result["incomplete"])

    def test_missing_multiplier_is_named_instead_of_defaulting_to_one(self):
        data = bundle()
        del data["me"]["moves"][1]["share_mult"]
        result = compile_payoffs(data)
        self.assertFalse(result["ok"])
        self.assertIn("我方的动作「降价10%」缺 share_mult", result["incomplete"])

    def test_margin_multiplier_is_derived_when_not_given(self):
        """单位成本 24×40%=9.6；新价 21.6 ⇒ 新毛利率 55.6%，不需要用户填。"""
        data = bundle()
        del data["me"]["moves"][1]["margin_mult"]
        result = compile_payoffs(data)
        self.assertTrue(result["ok"])
        self.assertEqual(cell(result, "降价10%", "照常")["me_profit"],
                         round(400000 * 0.06 * 1.33 * (21.6 - 9.6) - 330000))
        self.assertIn("按单位成本不变推导", cell(result, "降价10%", "照常")["formula"]["me"])
        self.assertIn("单位成本不随价格与销量变化", result["assumptions"])

    def test_given_margin_multiplier_wins_over_derivation(self):
        result = compile_payoffs(bundle())
        formula = cell(result, "降价10%", "照常")["formula"]["me"]
        self.assertNotIn("按单位成本不变推导", formula)
        self.assertEqual(cell(result, "降价10%", "照常")["me_profit"], 54725)

    def test_implausible_multiplier_needs_confirmation(self):
        data = bundle()
        data["rival"]["moves"][1]["margin_mult"] = 3
        result = compile_payoffs(data)
        self.assertFalse(result["ok"])
        self.assertIn("对方的动作「降价跟进」margin_mult 超过 1.5，先确认再算", result["incomplete"])


class CompiledSpecContract(unittest.TestCase):
    def test_spec_matches_the_engine_contract(self):
        spec = compile_payoffs(bundle())["spec"]
        self.assertEqual([p["id"] for p in spec["players"]], ["me", "rival"])
        self.assertEqual(spec["strategies"]["me"], ["原价开店", "降价10%", "暂不开"])
        cells = spec["payoffs"]["values"]["cells"]
        self.assertEqual(sorted(cells), sorted(["原价开店,照常", "原价开店,降价跟进", "降价10%,照常",
                                                "降价10%,降价跟进", "暂不开,照常", "暂不开,降价跟进"]))
        self.assertEqual(cells["原价开店,照常"]["me"]["value"], 15600)
        self.assertEqual(cells["原价开店,照常"]["me"]["kind"], "literal")

    def test_undeclared_cross_effects_are_flagged_as_a_degenerate_game(self):
        result = compile_payoffs(bundle())
        self.assertTrue(result["warning"], "没有任何跨方影响时必须告警")
        self.assertIn("退化", result["warning"][0])

    def test_declared_cross_effects_remove_the_degeneracy_warning(self):
        data = bundle()
        data["me"]["moves"][1]["opponent_share_mult"] = 0.9
        self.assertEqual(compile_payoffs(data)["warning"], [])

    def test_the_real_engine_accepts_the_compiled_spec(self):
        """编译产物必须被真引擎吃下——否则矩阵只是好看的 JSON。"""
        try:
            from web_console import find_shangyi_bin
            binary = find_shangyi_bin()
        except Exception:
            binary = None
        if not binary:
            self.skipTest("本机没有 shangyi 二进制，跳过引擎联调")
        data = bundle()  # 声明跨方影响：两边互相抢客，局才是真的
        data["me"]["moves"][1]["opponent_share_mult"] = 0.9
        data["rival"]["moves"][1]["opponent_share_mult"] = 0.8
        spec = compile_payoffs(data)["spec"]
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False, encoding="utf-8") as f:
            json.dump(spec, f, ensure_ascii=False)
            path = f.name
        try:
            proc = subprocess.run([binary, "solve", "--in", path], capture_output=True, text=True, timeout=60)
        finally:
            Path(path).unlink(missing_ok=True)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        result = json.loads(proc.stdout)
        equilibria = result.get("equilibria") or []
        # 手算结论：我对「照常」的最优应对是降价（54725 > 15600 > 0），
        # 对方对「降价10%」的最优应对是照常（763200 > 700913）。
        self.assertEqual([e["kind"] for e in equilibria], ["Pure"])
        self.assertEqual(equilibria[0]["support"], [[1], [0]], "均衡应是：我降价10% × 对方照常")
        self.assertEqual(equilibria[0]["payoffs"], ["54725", "763200"])
        # 引擎另报 ghost：有做法从不值得选（本例 5 格），这是它的完备性口径，不在本测试断言范围。


if __name__ == "__main__":
    unittest.main()

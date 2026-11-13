#!/usr/bin/env python3
"""稳健性扫描的单元测试。

桩不是「按剧本返回」——它真的按矩阵求纯策略纳什均衡，
所以「哪个假设翻面」是算出来的，不是写死的。
"""

import unittest

from payoff_model import compile_payoffs
from robustness import equilibrium_fingerprint, perturb, scan, FACTOR_SPECS


def stub_solver(spec):
    """最小纯策略纳什求解器：两侧都对对方的动作取最优应对。"""
    strategies = spec["strategies"]
    cells = spec["payoffs"]["values"]["cells"]
    me_moves, rival_moves = strategies["me"], strategies["rival"]
    equilibria = []
    for i, me_move in enumerate(me_moves):
        for j, rival_move in enumerate(rival_moves):
            payoffs = cells[f"{me_move},{rival_move}"]
            mine = payoffs["me"]["value"]
            theirs = payoffs["rival"]["value"]
            best_me = max(cells[f"{m},{rival_move}"]["me"]["value"] for m in me_moves)
            best_rival = max(cells[f"{me_move},{r}"]["rival"]["value"] for r in rival_moves)
            if mine == best_me and theirs == best_rival:
                equilibria.append({"id": f"e{len(equilibria) + 1}", "kind": "Pure",
                                   "support": [[i], [j]], "payoffs": [str(mine), str(theirs)]})
    return {"equilibria": equilibria, "completeness": "Complete"}


def bundle(**overrides):
    base = {
        "market": {"traffic": 400000},
        "me": {"share": 0.06, "price": 24, "margin": 0.6, "fixed_monthly": 330000, "upfront": 1200000,
               "moves": [{"name": "原价开店", "share_mult": 1.0, "price_mult": 1.0},
                         {"name": "降价10%", "share_mult": 1 + 0.035 / 0.06, "price_mult": 0.9, "opponent_share_mult": 0.9},
                         {"name": "暂不开", "closed": True}]},
        "rival": {"share": 0.35, "price": 22, "margin": 0.6, "fixed_monthly": 900000,
                  "moves": [{"name": "照常", "share_mult": 1.0, "price_mult": 1.0},
                            {"name": "降价跟进", "share_mult": 1 + 0.012 / 0.35, "price_mult": 0.9, "opponent_share_mult": 0.8}]},
    }
    base.update(overrides)
    return base


class Perturbation(unittest.TestCase):
    def test_scale_mode_multiplies_the_raw_value(self):
        factor = {"path": ("market", "traffic"), "mode": "scale"}
        self.assertEqual(perturb(bundle(), factor, 0.5)["market"]["traffic"], 200000)

    def test_strength_mode_scales_only_the_excess(self):
        factor = next(f for f in FACTOR_SPECS if f["id"] == "my_grab")
        base = bundle()["me"]["moves"][1]["share_mult"]
        self.assertAlmostEqual(perturb(bundle(), factor, 0.5)["me"]["moves"][1]["share_mult"], 1 + (base - 1) * 0.5)
        self.assertAlmostEqual(perturb(bundle(), factor, 2.0)["me"]["moves"][1]["share_mult"], 1 + (base - 1) * 2)

    def test_complement_mode_can_never_exceed_full_impact(self):
        """抢客比例是「抢走多少」，扰动强度而不是乘原值——乘原值会出现 0.9×2=1.8 的荒谬值。"""
        factor = next(f for f in FACTOR_SPECS if f["id"] == "my_take")
        self.assertAlmostEqual(perturb(bundle(), factor, 0.5)["me"]["moves"][1]["opponent_share_mult"], 0.95)
        self.assertAlmostEqual(perturb(bundle(), factor, 2.0)["me"]["moves"][1]["opponent_share_mult"], 0.8)
        self.assertLessEqual(perturb(bundle(), factor, 3.0)["me"]["moves"][1]["opponent_share_mult"], 1.0)

    def test_perturbation_does_not_touch_the_original_bundle(self):
        original = bundle()
        perturb(original, FACTOR_SPECS[0], 0.5)
        self.assertEqual(original["market"]["traffic"], 400000)

    def test_missing_path_is_skipped_instead_of_invented(self):
        factor = {"path": ("me", "upfront"), "mode": "scale"}
        trimmed = bundle()
        del trimmed["me"]["upfront"]
        self.assertIsNone(perturb(trimmed, factor, 0.5))


class ScanBehaviour(unittest.TestCase):
    def test_baseline_equilibrium_is_described_in_plain_words(self):
        result = scan(bundle(), stub_solver)
        self.assertTrue(result["ok"])
        self.assertEqual(result["baseline"]["equilibrium"], "降价10% × 照常")
        expected = next(c["me_profit"] for c in compile_payoffs(bundle())["cells"]
                        if c["me_move"] == "降价10%" and c["rival_move"] == "照常")
        self.assertEqual(result["baseline"]["me_profit"], expected)

    def test_the_fragile_assumption_is_found_with_its_flip_scale(self):
        result = scan(bundle(), stub_solver)
        labels = [f["label"] for f in result["fragile"]]
        self.assertIn("区域月客流", labels, "客流跌到一定程度，降价就不再最优——必须被找出来")
        traffic = next(c for c in result["checks"] if c["label"] == "区域月客流")
        self.assertIsNotNone(traffic["flip"])
        self.assertLess(traffic["flip"]["scale"], 1.0, "翻面应发生在客流下跌方向")
        self.assertNotEqual(traffic["flip"]["equilibrium"], result["baseline"]["equilibrium"])

    def test_fragile_list_is_ordered_by_distance_to_flipping(self):
        result = scan(bundle(), stub_solver)
        distances = [abs(f["scale"] - 1.0) for f in result["fragile"]]
        self.assertEqual(distances, sorted(distances), "最容易翻面的假设排最前")

    def test_a_dominant_strategy_game_never_flips_and_says_so(self):
        data = bundle()
        # 把「降价10%」改成全方位占优：份额乘数抬到任何客流下都更优，且对手不受影响
        data["me"]["moves"][1]["share_mult"] = 12
        data["me"]["moves"][1]["opponent_share_mult"] = None
        result = scan(data, stub_solver, factors=[f for f in FACTOR_SPECS if f["id"] == "my_fixed"])
        self.assertTrue(result["ok"])
        self.assertEqual(result["fragile"], [], "扫不出翻面就如实说没有，不含糊成稳健")
        check = result["checks"][0]
        self.assertIsNone(check["flip"])
        self.assertIsNotNone(check["worst_me_profit"])

    def test_every_factor_carries_its_scanned_points_and_profit_range(self):
        result = scan(bundle(), stub_solver)
        for check in result["checks"]:
            self.assertTrue(check["points"], check["label"])
            self.assertEqual(len(check["points"]), len(result["scales"]))
            self.assertIsNotNone(check["worst_me_profit"])
            self.assertGreaterEqual(check["best_me_profit"], check["worst_me_profit"])

    def test_points_are_scanned_from_mild_to_extreme(self):
        result = scan(bundle(), stub_solver)
        scales = [p["scale"] for p in result["checks"][0]["points"]]
        self.assertEqual(scales, sorted(scales, key=lambda s: abs(s - 1.0)))

    def test_incomplete_bundle_is_refused_not_scanned(self):
        result = scan(bundle(market={}), stub_solver)
        self.assertFalse(result["ok"])
        self.assertIn("区域月客流缺失或不合法", result["incomplete"])


class Fingerprint(unittest.TestCase):
    def test_mixed_equilibria_do_not_count_as_a_conclusion_change(self):
        pure = {"equilibria": [{"kind": "Pure", "support": [[1], [0]], "payoffs": ["1", "2"]}]}
        with_mixed = {"equilibria": pure["equilibria"] + [
            {"kind": "Mixed", "support": [[0, 1], [0, 1]], "payoffs": ["1", "2"]}]}
        self.assertEqual(equilibrium_fingerprint(pure), equilibrium_fingerprint(with_mixed))

    def test_order_of_equilibria_does_not_matter(self):
        a = {"equilibria": [{"kind": "Pure", "support": [[1], [0]], "payoffs": ["1", "2"]},
                            {"kind": "Pure", "support": [[0], [1]], "payoffs": ["3", "4"]}]}
        b = {"equilibria": list(reversed(a["equilibria"]))}
        self.assertEqual(equilibrium_fingerprint(a), equilibrium_fingerprint(b))

    def test_malformed_entries_are_ignored(self):
        self.assertEqual(equilibrium_fingerprint({"equilibria": [
            {"kind": "Pure", "support": []}, {"kind": "Pure"}, None]}), [])


if __name__ == "__main__":
    unittest.main()

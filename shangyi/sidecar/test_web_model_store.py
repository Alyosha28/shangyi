#!/usr/bin/env python3
"""数字摆局存档的单元测试：写入原子、读取健壮、坏数据不拖垮列表。"""

import json
import tempfile
import unittest
from pathlib import Path

import model_store as store


def bundle(me="我（喜茶）", rival="周边竞品", traffic=400000):
    return {
        "market": {"traffic": traffic},
        "me": {"name": me, "share": 0.06, "price": 24, "margin": 0.6, "fixed_monthly": 330000,
               "moves": [{"name": "原价开店", "share_mult": 1, "price_mult": 1},
                         {"name": "降价10%", "share_mult": 1.5, "price_mult": 0.9}]},
        "rival": {"name": rival, "share": 0.35, "price": 22, "margin": 0.6, "fixed_monthly": 900000,
                  "moves": [{"name": "照常", "share_mult": 1, "price_mult": 1},
                            {"name": "降价跟进", "share_mult": 1.03, "price_mult": 0.9}]},
    }


class SavesAndLoads(unittest.TestCase):
    def setUp(self):
        self.home = Path(tempfile.mkdtemp())

    def test_save_assigns_an_id_and_load_returns_the_record(self):
        saved = store.save(self.home, {"bundle": bundle(), "cells": [{"me_move": "降价10%"}]})
        self.assertRegex(saved["id"], r"^model-[a-f0-9]{12}$")
        loaded = store.load(self.home, saved["id"])
        self.assertEqual(loaded["bundle"]["market"]["traffic"], 400000)
        self.assertEqual(loaded["cells"], [{"me_move": "降价10%"}])

    def test_title_is_recognizable_from_the_bundle(self):
        saved = store.save(self.home, {"bundle": bundle()})
        self.assertIn("我（喜茶） × 周边竞品", saved["title"])
        self.assertIn("降价10%", saved["title"])
        self.assertIn("40万", saved["title"])

    def test_saving_leaves_no_temp_file_behind(self):
        store.save(self.home, {"bundle": bundle()})
        leftovers = list((self.home / "web-models").glob(".*tmp"))
        self.assertEqual(leftovers, [], "原子写入不能留下半截文件")

    def test_duplicate_id_is_refused_instead_of_overwritten(self):
        first = store.save(self.home, {"bundle": bundle()})
        with self.assertRaises(store.ModelStoreError):
            store.save(self.home, {"bundle": bundle(), "id": first["id"]})

    def test_path_traversal_and_bad_ids_are_refused(self):
        # 注意：不给 id（或给空）是合法用法——表示"生成一个新 id"。
        for bad in ["../evil", "model-../../etc/passwd", "model-ZZZZZZZZZZZZ"]:
            with self.assertRaises(store.ModelStoreError):
                store.save(self.home, {"bundle": bundle(), "id": bad})
        for bad in ["../evil", "model-x", None, "model-0123456789ab/../.."]:
            self.assertIsNone(store.load(self.home, bad))

    def test_missing_bundle_is_refused(self):
        with self.assertRaises(store.ModelStoreError):
            store.save(self.home, {"cells": []})

    def test_loading_an_unknown_id_is_none_not_an_error(self):
        self.assertIsNone(store.load(self.home, "model-abcdefabcdef"))


class Listing(unittest.TestCase):
    def setUp(self):
        self.home = Path(tempfile.mkdtemp())

    def test_newest_first(self):
        older = store.save(self.home, {"bundle": bundle(), "created_at": 1000})
        newer = store.save(self.home, {"bundle": bundle(me="另一个我"), "created_at": 2000})
        self.assertEqual([row["id"] for row in store.listing(self.home)], [newer["id"], older["id"]])

    def test_summary_reports_baseline_profit_and_scan_flag(self):
        store.save(self.home, {
            "bundle": bundle(),
            "solution": {"equilibria": [{"kind": "Mixed", "payoffs": ["1"]},
                                        {"kind": "Pure", "payoffs": ["125990", "763200"]}]},
            "robustness": {"ok": True},
        })
        row = store.listing(self.home)[0]
        self.assertEqual(row["me_profit"], "125990")
        self.assertEqual(row["equilibria"], 2)
        self.assertTrue(row["scanned"])

    def test_corrupt_files_are_skipped_not_fatal(self):
        store.save(self.home, {"bundle": bundle()})
        (self.home / "web-models" / "model-ffffffffffff.json").write_text("{ 坏掉的 json", encoding="utf-8")
        rows = store.listing(self.home)
        self.assertEqual(len(rows), 1, "坏文件不能让整张列表失败")

    def test_missing_directory_lists_empty(self):
        self.assertEqual(store.listing(self.home / "不存在的家"), [])

    def test_listing_does_not_expose_the_whole_record(self):
        store.save(self.home, {"bundle": bundle(), "spec": {"players": [1, 2, 3]}})
        row = store.listing(self.home)[0]
        self.assertNotIn("spec", row)
        self.assertNotIn("bundle", row)


class Description(unittest.TestCase):
    def test_without_a_second_move_it_still_names_both_sides(self):
        data = bundle()
        data["me"]["moves"] = data["me"]["moves"][:1]
        self.assertIn("我（喜茶） × 周边竞品", store.describe(data))

    def test_missing_names_fall_back_to_plain_words(self):
        text = store.describe({"market": {"traffic": 10000}, "me": {}, "rival": {}})
        self.assertIn("我方 × 对方", text)
        self.assertIn("1万", text)


class RecordShape(unittest.TestCase):
    def test_all_expected_fields_survive_a_round_trip(self):
        saved = store.save(tempfile.mkdtemp(), {
            "bundle": bundle(), "spec": {"game_id": "web-model"}, "cells": [{"me_move": "降价10%"}],
            "facts": {"counts": [{"label": "茶饮咖啡", "count": 59}]},
            "payback_months": {"降价10%,照常": 9.5}, "unmodeled": ["x"], "warning": [], "solution": {"equilibria": []},
            "robustness": {"ok": True, "fragile": []},
        })
        for key in ("bundle", "spec", "facts", "cells", "payback_months", "unmodeled",
                    "warning", "solution", "robustness"):
            self.assertIn(key, saved, key)


if __name__ == "__main__":
    unittest.main()

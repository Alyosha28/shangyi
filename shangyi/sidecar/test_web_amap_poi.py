#!/usr/bin/env python3
"""高德周边 POI 的单元测试：不联网，用注入的假 HTTP 钉住行为。"""

import unittest
from urllib.parse import parse_qs, urlparse

import amap_poi as amap


def payload(**overrides):
    base = {"status": "1", "info": "OK", "count": "7",
            "pois": [{"name": "甲店"}, {"name": "乙店"}]}
    base.update(overrides)
    return base


class FakeHttp:
    """按 URL 参数分派响应，并记录每次调用（含顺序与间隔）。"""

    def __init__(self, handler):
        self.handler = handler
        self.calls = []

    def __call__(self, url, timeout):
        query = {k: v[0] for k, v in parse_qs(urlparse(url).query).items()}
        self.calls.append(query)
        return self.handler(query)

    def types_called(self):
        return [c.get("types") for c in self.calls if c.get("types")]


class KeyLoading(unittest.TestCase):
    def test_environment_key_wins(self):
        self.assertEqual(amap.load_key({"AMAP_KEY": " abc "}), "abc")

    def test_missing_key_is_none_not_an_empty_string(self):
        import tempfile
        # 隔离到空目录：否则会回退到本机真实的 ~/.shangyi-secrets/amap.env
        self.assertIsNone(amap.load_key({"SHANGYI_SECRETS": tempfile.mkdtemp()}))

    def test_secrets_file_is_read_when_environment_is_empty(self):
        import tempfile
        from pathlib import Path
        folder = Path(tempfile.mkdtemp())
        (folder / "amap.env").write_text("export AMAP_KEY=from-file\n", encoding="utf-8")
        self.assertEqual(amap.load_key({"SHANGYI_SECRETS": str(folder)}), "from-file")


class Geocode(unittest.TestCase):
    def test_address_is_resolved_to_coordinates(self):
        http = FakeHttp(lambda q: {"status": "1", "info": "OK",
                                   "geocodes": [{"location": "121.477,31.234", "district": "黄浦区",
                                                 "formatted_address": "上海市黄浦区南京东路"}]})
        place = amap.geocode("上海市南京东路32号", key="k", http=http, sleeper=lambda _: None)
        self.assertEqual(place["location"], "121.477,31.234")
        self.assertEqual(place["district"], "黄浦区")

    def test_unresolvable_address_raises_instead_of_guessing(self):
        http = FakeHttp(lambda q: {"status": "1", "info": "OK", "geocodes": []})
        with self.assertRaises(amap.AmapError) as caught:
            amap.geocode("不存在的地方", key="k", http=http, sleeper=lambda _: None)
        self.assertIn("解析不出坐标", str(caught.exception))

    def test_empty_address_is_refused(self):
        with self.assertRaises(amap.AmapError):
            amap.geocode("   ", key="k", http=FakeHttp(lambda q: {}), sleeper=lambda _: None)

    def test_broken_coordinate_shape_is_refused(self):
        http = FakeHttp(lambda q: {"status": "1", "info": "OK", "geocodes": [{"location": "??"}]})
        with self.assertRaises(amap.AmapError):
            amap.geocode("某地", key="k", http=http, sleeper=lambda _: None)


class Survey(unittest.TestCase):
    def _handler(self, query):
        if "address" in query:
            return {"status": "1", "info": "OK",
                    "geocodes": [{"location": "121.4,31.2", "district": "黄浦区",
                                  "formatted_address": "上海市黄浦区南京东路"}]}
        if query.get("keywords"):
            return payload(count="3", pois=[{"name": "喜茶(甲店)"}])
        counts = {"050700|050500|050900": "59", "050000": "600"}
        return payload(count=counts.get(query.get("types"), "11"))

    def test_each_category_gets_its_own_count_and_samples(self):
        http = FakeHttp(self._handler)
        result = amap.survey("上海市南京东路32号", key="k", http=http, sleeper=lambda _: None,
                             categories=(("茶饮咖啡", "050700|050500|050900"), ("餐饮", "050000")))
        self.assertTrue(result["ok"])
        self.assertEqual([c["label"] for c in result["counts"]], ["茶饮咖啡", "餐饮"])
        self.assertEqual(result["counts"][0]["count"], 59)
        self.assertEqual(result["counts"][0]["samples"], ["甲店", "乙店"])
        self.assertEqual(result["address"], "上海市黄浦区南京东路")
        self.assertIn("高德", result["source"])

    def test_brand_check_reports_how_many_of_the_same_brand_already_exist(self):
        http = FakeHttp(self._handler)
        result = amap.survey("某地", key="k", http=http, sleeper=lambda _: None, brand="喜茶",
                             categories=(("茶饮咖啡", "050700"),))
        self.assertEqual(result["brand"]["count"], 3)
        self.assertIn("同品牌", result["brand"]["label"])

    def test_calls_are_sequential_with_spacing_not_a_burst(self):
        slept = []
        http = FakeHttp(self._handler)
        amap.survey("某地", key="k", http=http, sleeper=slept.append,
                    categories=(("茶饮咖啡", "050700"), ("餐饮", "050000"), ("商场", "060100")))
        self.assertEqual(len(slept), 2, "三个类目之间要各留一次间隔（免费 key QPS 很低）")
        self.assertTrue(all(s >= amap.CALL_INTERVAL for s in slept))

    def test_rate_limit_is_retried_once_then_reported_not_counted_as_zero(self):
        state = {"n": 0}

        def handler(query):
            if "address" in query:
                return {"status": "1", "info": "OK",
                        "geocodes": [{"location": "121.4,31.2"}]}
            state["n"] += 1
            if state["n"] == 1:
                return {"status": "0", "info": "CUQPS_HAS_EXCEEDED_THE_LIMIT"}
            return payload(count="12")

        slept = []
        result = amap.survey("某地", key="k", http=FakeHttp(handler), sleeper=slept.append,
                             categories=(("茶饮咖啡", "050700"),))
        self.assertEqual(result["counts"][0]["count"], 12, "限流后重试成功应拿到真实计数")
        self.assertIn(amap.RETRY_DELAY, slept)

    def test_persistent_rate_limit_is_reported_as_unavailable_with_its_reason(self):
        def handler(query):
            if "address" in query:
                return {"status": "1", "info": "OK", "geocodes": [{"location": "121.4,31.2"}]}
            return {"status": "0", "info": "CUQPS_HAS_EXCEEDED_THE_LIMIT"}

        result = amap.survey("某地", key="k", http=FakeHttp(handler), sleeper=lambda _: None,
                             categories=(("茶饮咖啡", "050700"),))
        self.assertEqual(result["counts"], [], "拿不到计数就不能写 0")
        self.assertIn("CUQPS", result["unavailable"][0]["why"])

    def test_non_retryable_error_fails_fast_and_keeps_the_reason(self):
        def handler(query):
            if "address" in query:
                return {"status": "1", "info": "OK", "geocodes": [{"location": "121.4,31.2"}]}
            return {"status": "0", "info": "INVALID_USER_KEY"}

        slept = []
        result = amap.survey("某地", key="k", http=FakeHttp(handler), sleeper=slept.append,
                             categories=(("餐饮", "050000"),))
        self.assertEqual(result["unavailable"][0]["why"], "INVALID_USER_KEY")
        self.assertNotIn(amap.RETRY_DELAY, slept, "密钥错不该重试，那是白等")

    def test_radius_is_clamped_to_a_sane_range(self):
        http = FakeHttp(self._handler)
        big = amap.survey("某地", key="k", http=http, sleeper=lambda _: None,
                          radius=99999, categories=(("餐饮", "050000"),))
        self.assertEqual(big["radius"], amap.MAX_RADIUS)
        tiny = amap.survey("某地", key="k", http=FakeHttp(self._handler), sleeper=lambda _: None,
                           radius=1, categories=(("餐饮", "050000"),))
        self.assertEqual(tiny["radius"], 100)

    def test_key_never_leaks_into_errors(self):
        def handler(query):
            raise TimeoutError("timed out")

        with self.assertRaises(amap.AmapError) as caught:
            amap.geocode("某地", key="SECRET-KEY-123", http=FakeHttp(handler), sleeper=lambda _: None)
        self.assertNotIn("SECRET-KEY-123", str(caught.exception))


class ArchiveShape(unittest.TestCase):
    def test_counts_only_strips_poi_details_for_compliance(self):
        http = FakeHttp(lambda q: ({"status": "1", "info": "OK",
                                    "geocodes": [{"location": "121.4,31.2"}]} if "address" in q
                                   else payload(count="59", pois=[{"name": "某奶茶店"}])))
        result = amap.survey("某地", key="k", http=http, sleeper=lambda _: None,
                             categories=(("茶饮咖啡", "050700"),))
        slim = amap.counts_only(result)
        self.assertEqual(slim["counts"][0]["count"], 59)
        self.assertNotIn("samples", slim["counts"][0])
        self.assertNotIn("某奶茶店", str(slim), "存档不留 POI 明细")

    def test_counts_only_of_a_failed_survey_is_empty(self):
        self.assertEqual(amap.counts_only({"ok": False}), {})
        self.assertEqual(amap.counts_only(None), {})


if __name__ == "__main__":
    unittest.main()

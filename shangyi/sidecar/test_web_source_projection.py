import json
import unittest
from web_source_projection import WebSources


class WebSourceProjectionTests(unittest.TestCase):
    def call(self, projection, name, args, outcome):
        identifier = str(len(projection.calls))
        projection.feed({"type": "tool_call_started", "call_id": identifier, "name": name, "arguments": json.dumps(args)})
        projection.feed({"type": "tool_call_finished", "call_id": identifier, "outcome": outcome})

    def test_later_failed_fetch_does_not_erase_previously_read_body(self):
        p = WebSources()
        a, b = "https://example.com/official", "https://example.com/lead"
        self.call(p, "web_search", {"query": "test"}, {"status": "ok", "output": json.dumps({"hits": [{"title": "原始资料", "url": a}, {"title": "只找到摘要", "url": b}]})})
        self.call(p, "web_fetch", {"url": a}, {"status": "ok", "output": json.dumps({"url": a, "text": "真实取得的页面内容", "retrieved_at_unix": 100})})
        self.call(p, "web_fetch", {"url": a}, {"status": "blocked", "detail": "网络预算耗尽"})
        records = p.records()
        self.assertEqual(records[0]["status"], "body_retrieved")
        self.assertEqual(records[0]["retrieved_at_unix"], 100)
        self.assertTrue(records[0]["last_attempt_failed"])
        self.assertEqual(records[1]["status"], "search_only")
        self.assertNotIn("text", records[0], "polling should not resend entire source documents")

    def test_redirect_and_cached_replay_keep_actual_url_and_original_read_time(self):
        p = WebSources()
        old, actual = "http://example.com/a", "https://example.com/a"
        self.call(p, "web_search", {}, {"status": "ok", "output": json.dumps({"hits": [{"title": "标题", "url": old}]})})
        output = {"url": actual, "text": "正文", "retrieved_at_unix": 123, "cached": True}
        self.call(p, "web_fetch", {"url": old}, {"status": "ok", "output": json.dumps(output)})
        self.call(p, "web_search", {}, {"status": "ok", "output": json.dumps({"hits": [{"title": "标题", "url": old}]})})
        self.assertEqual(len(p.records()), 1)
        self.assertEqual(p.records()[0]["url"], actual)
        self.assertEqual(p.records()[0]["retrieved_at_unix"], 123)
        self.assertTrue(p.records()[0]["cached"])

    def test_failed_empty_or_unsafe_sources_never_become_read_pages(self):
        p = WebSources()
        self.call(p, "web_search", {}, {"status": "ok", "output": json.dumps({"hits": [{"url": "javascript:bad()"}, {"url": "https://user:secret@example.com"}]})})
        self.assertEqual(p.records(), [])
        self.call(p, "web_fetch", {"url": "https://example.com/empty"}, {"status": "ok", "output": json.dumps({"url": "https://example.com/empty", "text": " "})})
        self.assertEqual(p.records()[0]["status"], "body_unavailable")

    def test_failed_or_empty_redirect_does_not_move_or_erase_previous_snapshot(self):
        for status, body in [("failed", "error page"), ("ok", " ")]:
            with self.subTest(status=status):
                p = WebSources()
                old, different = "https://example.com/old", "https://example.com/different"
                self.call(p, "web_fetch", {"url": old}, {"status": "ok", "output": json.dumps({"url": old, "text": "已读正文", "retrieved_at_unix": 100})})
                self.call(p, "web_search", {}, {"status": "ok", "output": json.dumps({"hits": [{"url": different}]})})
                self.call(p, "web_fetch", {"url": old}, {"status": status, "output": json.dumps({"url": different, "text": body})})
                by_url = {source["url"]: source for source in p.records()}
                self.assertEqual(by_url[old]["status"], "body_retrieved")
                self.assertEqual(by_url[old]["retrieved_at_unix"], 100)
                self.assertTrue(by_url[old]["last_attempt_failed"])
                self.assertEqual(by_url[different]["status"], "search_only")
                self.assertNotIn(old, p.aliases)

    def test_cached_replay_cannot_repair_failed_refresh_or_backdate_newer_snapshot(self):
        p = WebSources()
        url = "https://example.com/a"
        self.call(p, "web_fetch", {"url": url}, {"status": "ok", "output": json.dumps({"url": url, "text": "新版正文", "retrieved_at_unix": 200})})
        self.call(p, "web_fetch", {"url": url, "refresh": True}, {"status": "blocked", "detail": "网络失败"})
        self.call(p, "web_fetch", {"url": url}, {"status": "ok", "output": json.dumps({"url": url, "text": "旧版缓存", "retrieved_at_unix": 100, "cached": True})})
        source = p.records()[0]
        self.assertEqual(source["retrieved_at_unix"], 200)
        self.assertEqual(source["last_replayed_snapshot_at_unix"], 100)
        self.assertTrue(source["last_attempt_failed"])
        self.call(p, "web_fetch", {"url": url, "refresh": True}, {"status": "ok", "output": json.dumps({"url": url, "text": "刷新成功", "retrieved_at_unix": 300})})
        self.assertEqual(source["retrieved_at_unix"], 300)
        self.assertFalse(source["last_attempt_failed"])
        self.assertNotIn("last_replayed_snapshot_at_unix", source)

    def test_malformed_hit_list_unsafe_return_url_and_invalid_timestamp_are_not_promoted(self):
        p = WebSources()
        self.call(p, "web_search", {}, {"status": "ok", "output": json.dumps({"hits": None})})
        self.call(p, "web_fetch", {"url": "https://example.com/a"}, {"status": "ok", "output": json.dumps({"url": "https://user:secret@example.com/private", "text": "正文"})})
        self.assertEqual(p.records()[0]["status"], "body_unavailable")
        self.call(p, "web_fetch", {"url": "https://example.com/a"}, {"status": "ok", "output": json.dumps({"url": "https://example.com/a", "text": "正文", "retrieved_at_unix": True})})
        self.assertIsNone(p.records()[0]["retrieved_at_unix"])

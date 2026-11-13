#!/usr/bin/env python3
"""deepseek_search.py 的离线测试。

全部用本机 127.0.0.1 上的 stdlib HTTP 服务器，不碰外网、不需要真实密钥。
运行::

    python3 tools/deepseek-search/test_deepseek_search.py -v
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import threading
import unittest
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import deepseek_search as ds  # noqa: E402

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "deepseek_search.py")


def fixture_response(base: str = "https://example.test") -> dict:
    return {
        "id": "msg_test",
        "content": [
            {
                "type": "text",
                "text": "model prose that must not be treated as an answer",
                "citations": [
                    {"url": f"{base}/a", "cited_text": "snippet A"},
                    {"url": f"{base}/b", "cited_text": "snippet B"},
                    {"url": f"{base}/a", "cited_text": "later duplicate snippet"},
                ],
            },
            {
                "type": "web_search_tool_result",
                "content": [
                    {
                        "type": "web_search_result",
                        "url": f"{base}/a",
                        "title": "Alpha",
                        "page_age": "2026-01-02",
                    },
                    {"type": "web_search_result", "url": f"{base}/b", "title": "Beta"},
                ],
            },
            {
                "type": "web_search_tool_result",
                "content": [
                    {"type": "web_search_result", "url": f"{base}/a", "title": "Alpha duplicate"},
                    {"type": "web_search_result", "url": f"{base}/c", "title": "Gamma"},
                ],
            },
        ],
        "usage": {"input_tokens": 42, "output_tokens": 7},
        "stop_reason": "end_turn",
    }


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):  # noqa: N802
        length = int(self.headers.get("content-length") or 0)
        raw = self.rfile.read(length)
        record = {
            "path": self.path,
            "headers": {key.lower(): value for key, value in self.headers.items()},
            "raw": raw,
        }
        try:
            record["body"] = json.loads(raw.decode("utf-8"))
        except ValueError:
            record["body"] = None
        self.server.requests.append(record)  # type: ignore[attr-defined]

        if getattr(self.server, "redirect_to", None):
            self.send_response(getattr(self.server, "response_code", 302))
            self.send_header("location", self.server.redirect_to)  # type: ignore[attr-defined]
            self.send_header("content-length", "0")
            self.end_headers()
            return

        payload = getattr(self.server, "response_body", b"{}")
        body = payload if isinstance(payload, bytes) else json.dumps(payload).encode("utf-8")
        self.send_response(getattr(self.server, "response_code", 200))
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):  # 保持测试输出干净
        pass


@contextmanager
def local_server(*, response_code: int = 200, response_body=None, redirect_to: str | None = None):
    server = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
    server.daemon_threads = True
    server.requests = []  # type: ignore[attr-defined]
    server.response_code = response_code  # type: ignore[attr-defined]
    server.response_body = response_body if response_body is not None else b"{}"  # type: ignore[attr-defined]
    server.redirect_to = redirect_to  # type: ignore[attr-defined]
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def client_for(server, **kwargs) -> ds.DeepSeekSearch:
    kwargs.setdefault("cache", False)
    return ds.DeepSeekSearch(
        api_key="sk-test",
        base_url=f"http://127.0.0.1:{server.server_port}/v1",
        proxies={},  # 绕开环境代理，测试只走回环
        **kwargs,
    )


class ProtocolTests(unittest.TestCase):
    def test_request_shape_and_parsing(self):
        with local_server(response_body=fixture_response()) as server:
            result = client_for(server).search("hello world", max_results=2)

        self.assertEqual(len(server.requests), 1)
        request = server.requests[0]
        self.assertEqual(request["path"], "/v1/messages")
        headers = request["headers"]
        self.assertEqual(headers.get("x-api-key"), "sk-test")
        self.assertEqual(headers.get("authorization"), "Bearer sk-test")
        self.assertEqual(headers.get("anthropic-version"), "2023-06-01")
        self.assertEqual(headers.get("content-type"), "application/json")

        body = request["body"]
        self.assertEqual(body["model"], "deepseek-v4-flash")
        self.assertEqual(body["max_tokens"], 4096)
        self.assertEqual(
            body["messages"][0]["content"][0]["text"],
            "Perform a web search for the query: hello world",
        )
        self.assertEqual(
            body["tools"],
            [{"type": "web_search_20250305", "name": "web_search", "max_uses": 5}],
        )

        self.assertEqual([source.url for source in result.sources], [
            "https://example.test/a",
            "https://example.test/b",
        ])
        self.assertTrue(result.truncated)
        self.assertIsNone(result.answer)
        self.assertEqual(result.sources[0].title, "Alpha")
        self.assertEqual(result.sources[0].snippet, "snippet A")  # 首次引用胜出
        self.assertEqual(result.sources[0].published_at, "2026-01-02")
        self.assertEqual(result.sources[1].snippet, "snippet B")
        self.assertEqual(result.usage, {"input_tokens": 42, "output_tokens": 7})

    def test_include_answer_is_opt_in(self):
        with local_server(response_body=fixture_response()) as server:
            result = client_for(server).search("q", include_answer=True)
        self.assertEqual(result.answer, "model prose that must not be treated as an answer")

    def test_no_result_block_is_an_error(self):
        payload = {"content": [{"type": "text", "text": "no search happened"}]}
        with local_server(response_body=payload) as server:
            with self.assertRaises(ds.SearchError) as caught:
                client_for(server).search("q")
        self.assertEqual(caught.exception.code, "WEB_PROVIDER_ERROR")
        self.assertIn("web_search_tool_result", str(caught.exception))

    def test_http_401_surfaces_api_message(self):
        payload = {"error": {"message": "Authentication Fails"}}
        with local_server(response_code=401, response_body=payload) as server:
            with self.assertRaises(ds.SearchError) as caught:
                client_for(server).search("q")
        message = str(caught.exception)
        self.assertEqual(caught.exception.code, "WEB_PROVIDER_ERROR")
        self.assertIn("HTTP 401", message)
        self.assertIn("Authentication Fails", message)

    def test_redirect_is_refused(self):
        with local_server(response_code=302, redirect_to="https://evil.example/collect") as server:
            with self.assertRaises(ds.SearchError) as caught:
                client_for(server).search("q")
        self.assertIn("redirect", str(caught.exception).lower())

    def test_missing_key_fails_before_network(self):
        client = ds.DeepSeekSearch(api_key="", cache=False)
        with self.assertRaises(ds.SearchError) as caught:
            client.search("q")
        self.assertEqual(caught.exception.code, "WEB_PROVIDER_CREDENTIAL_MISSING")


class CacheTests(unittest.TestCase):
    def test_second_identical_search_hits_cache(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_path = os.path.join(tmp, "cache.json")
            with local_server(response_body=fixture_response()) as server:
                client = client_for(server, cache=True, cache_path=cache_path)
                first = client.search("same query")
                second = client.search("same query")
            self.assertEqual(len(server.requests), 1)
            self.assertFalse(first.cached)
            self.assertTrue(second.cached)
            self.assertEqual([s.url for s in first.sources], [s.url for s in second.sources])


class MergeTests(unittest.TestCase):
    def _fake_transport(self, responses: dict[str, dict], calls: list):
        def transport(endpoint, headers, body, timeout, proxies):
            query = body["messages"][0]["content"][0]["text"]
            calls.append(query)
            return 200, responses[query]

        return transport

    def test_many_queries_merge_round_robin_and_dedupe(self):
        responses = {
            "Perform a web search for the query: q1": {
                "content": [
                    {"type": "web_search_tool_result", "content": [
                        {"type": "web_search_result", "url": "https://x/1", "title": "1"},
                        {"type": "web_search_result", "url": "https://x/2", "title": "2"},
                    ]}
                ]
            },
            "Perform a web search for the query: q2": {
                "content": [
                    {"type": "web_search_tool_result", "content": [
                        {"type": "web_search_result", "url": "https://x/1", "title": "1 dup"},
                        {"type": "web_search_result", "url": "https://x/3", "title": "3"},
                    ]}
                ]
            },
        }
        calls: list[str] = []
        client = ds.DeepSeekSearch(
            api_key="sk-test", transport=self._fake_transport(responses, calls), cache=False
        )
        merged = client.search_merged(["q1", "q2", "q1"], max_results=8)
        self.assertEqual(sorted(calls), sorted(responses))
        # 轮询顺序：q1#1, q2#1(去重跳过), q1#2, q2#2
        self.assertEqual(
            [source.url for source in merged.sources],
            ["https://x/1", "https://x/2", "https://x/3"],
        )

    def test_one_failing_query_aborts_batch(self):
        def transport(endpoint, headers, body, timeout, proxies):
            query = body["messages"][0]["content"][0]["text"]
            if query.endswith("bad"):
                raise ds.SearchError("WEB_PROVIDER_ERROR", "boom")
            return 200, fixture_response()

        client = ds.DeepSeekSearch(api_key="sk-test", transport=transport, cache=False)
        with self.assertRaises(ds.SearchError):
            client.search_many(["ok", "bad"])


class CliTests(unittest.TestCase):
    def test_dry_run_needs_no_key_and_prints_body(self):
        env = {key: value for key, value in os.environ.items() if key != "DEEPSEEK_API_KEY"}
        completed = subprocess.run(
            [sys.executable, SCRIPT, "--dry-run", "hello", "--no-env-file"],
            capture_output=True,
            text=True,
            env=env,
            check=False,
        )
        self.assertEqual(completed.returncode, 0, completed.stderr)
        payload = json.loads(completed.stdout)
        self.assertEqual(payload[0]["endpoint"], f"{ds.DEFAULT_BASE_URL}/messages")
        self.assertEqual(payload[0]["body"]["tools"][0]["name"], "web_search")
        self.assertEqual(payload[0]["headers"]["x-api-key"], "<DEEPSEEK_API_KEY>")

    def test_cli_end_to_end_against_local_server(self):
        with local_server(response_body=fixture_response()) as server:
            env = dict(os.environ)
            env["DEEPSEEK_API_KEY"] = "sk-test"
            env["NO_PROXY"] = "127.0.0.1,localhost"
            env["no_proxy"] = "127.0.0.1,localhost"
            completed = subprocess.run(
                [
                    sys.executable,
                    SCRIPT,
                    "--base-url",
                    f"http://127.0.0.1:{server.server_port}/v1",
                    "--max-results",
                    "2",
                    "--json",
                    "--no-cache",
                    "hello",
                ],
                capture_output=True,
                text=True,
                env=env,
                check=False,
            )
            self.assertEqual(completed.returncode, 0, completed.stderr)
            payload = json.loads(completed.stdout)[0]
            self.assertEqual(len(payload["sources"]), 2)
            self.assertTrue(payload["truncated"])
            self.assertEqual(payload["sources"][0]["publishedAt"], "2026-01-02")


if __name__ == "__main__":
    unittest.main(verbosity=2)

#!/usr/bin/env python3
"""DeepSeek 原生 web 搜索客户端 —— DSH `@deepseek-ai/dsh-web-search-deepseek` 的协议复刻。

DSH 没有自建搜索服务，也没有专用搜索端点：它把一次查询包装成一次 Anthropic 兼容
Messages 调用，挂上 DeepSeek 服务端原生 `web_search` 工具。本模块复刻同一协议与同一
套解析规则，去掉 cordis/DSH 依赖，任何 Python 代码或 CLI 都能直接用。

协议（与 DSH 逐字段一致）::

    POST {base_url}/messages          # 默认 https://api.deepseek.com/anthropic/v1/messages
    x-api-key: <DEEPSEEK_API_KEY>
    authorization: Bearer <DEEPSEEK_API_KEY>
    anthropic-version: 2023-06-01
    content-type: application/json
    user-agent: <自定义>

    {
      "model": "deepseek-v4-flash",
      "max_tokens": 4096,
      "messages": [{"role": "user", "content": [{"type": "text",
          "text": "Perform a web search for the query: <query>"}]}],
      "tools": [{"type": "web_search_20250305", "name": "web_search", "max_uses": 5}]
    }

解析规则（照抄 DSH，不做任何降级）:

* 只信任结构化的 ``web_search_tool_result`` 块；模型正文默认不作为答案
  （确需正文时显式传 ``include_answer=True``）。
* 来源来自结果块内的 ``web_search_result``：``url`` / ``title`` / ``page_age``。
* snippet 来自 text 块 ``citations[].cited_text``，按 URL 关联（首次出现胜出）。
* 结果按 URL 去重；``max_results`` 由客户端截断并置 ``truncated`` —— DeepSeek 端
  没有结果数量参数。
* 一个结果块都没有 => ``WEB_PROVIDER_ERROR``，绝不从正文里抓 URL。
* HTTP 重定向一律拒绝跟随。

成本提醒：一次 search = 一次完整模型轮次（默认 deepseek-v4-flash / max_tokens 4096），
输入与输出都计费，延迟通常数秒到数十秒。因此默认开启磁盘缓存（24h），且把
``max_uses`` 保持较小；多条 query 会并发发起多次调用。

命令行示例::

    export DEEPSEEK_API_KEY=sk-...                 # 或 --env-file ~/.shangyi-secrets/deepseek.env
    python3 deepseek_search.py "全国大学生数学建模竞赛 2026 论文格式"
    python3 deepseek_search.py --json "query a" "query b"     # 多查询轮询合并，默认只输出一次合并结果
    python3 deepseek_search.py --dry-run "query"              # 只打印请求体，不联网、不读密钥

注意：DSH 的另一种鉴权方式（登录账号的 ``x-dsh-auth-token``）是 harness 内部能力，
本复刻只使用公开的 API key 路径。
"""

from __future__ import annotations

import argparse
import concurrent.futures
import dataclasses
import hashlib
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
from pathlib import Path

# ---------------------------------------------------------------------------
# 与 DSH 对齐的默认值
# ---------------------------------------------------------------------------

DEFAULT_BASE_URL = "https://api.deepseek.com/anthropic/v1"
DEFAULT_MODEL = "deepseek-v4-flash"
DEFAULT_API_VERSION = "2023-06-01"
DEFAULT_MAX_TOKENS = 4096
DEFAULT_MAX_USES = 5
DEFAULT_TIMEOUT = 60.0
DEFAULT_MAX_RESULTS = 8
DEFAULT_CACHE_MAX_AGE = 24 * 60 * 60.0
DEFAULT_CACHE_PATH = "~/.cache/deepseek-search/cache.json"
DEFAULT_ENV_FILE = "~/.shangyi-secrets/deepseek.env"
USER_AGENT = "deepseek-search/0.1"
CACHE_MAX_ENTRIES = 500


class SearchError(RuntimeError):
    """结构化失败；``code`` 沿用 DSH 的 WebError 码，便于两边同样路由。"""

    def __init__(self, code: str, message: str) -> None:
        super().__init__(message)
        self.code = code

    def __str__(self) -> str:  # 让 CLI 输出自带错误码
        return f"[{self.code}] {super().__str__()}"


@dataclasses.dataclass(frozen=True)
class Source:
    """一条可引用来源；``published_at`` 来自 DeepSeek 的 ``page_age``。"""

    url: str
    title: str | None = None
    snippet: str | None = None
    published_at: str | None = None

    def to_dict(self) -> dict:
        data: dict = {"url": self.url}
        if self.title:
            data["title"] = self.title
        if self.snippet:
            data["snippet"] = self.snippet
        if self.published_at:
            data["publishedAt"] = self.published_at
        return data


@dataclasses.dataclass
class SearchResult:
    """一次搜索的规范化结果（字段形状对齐 DSH seam 的 search 结果）。"""

    query: str
    sources: list[Source] = dataclasses.field(default_factory=list)
    truncated: bool = False
    answer: str | None = None
    usage: dict | None = None
    cached: bool = False

    def to_dict(self) -> dict:
        data: dict = {
            "query": self.query,
            "sources": [source.to_dict() for source in self.sources],
            "truncated": self.truncated,
        }
        if self.answer is not None:
            data["answer"] = self.answer
        if self.usage:
            data["usage"] = self.usage
        return data


# ---------------------------------------------------------------------------
# 传输层（stdlib，无第三方依赖；可注入假 transport 做离线测试）
# ---------------------------------------------------------------------------


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """DSH 明确拒绝重定向；复刻这一行为，避免查询被重定向到未知端点。"""

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D102
        raise SearchError(
            "WEB_PROVIDER_ERROR",
            f"DeepSeek search response attempted a redirect (HTTP {code} -> {newurl}); "
            "redirects are not followed",
        )


def http_error_message(status: int, raw: bytes, endpoint: str) -> str:
    """把 HTTP 错误体里的 error/message 提取成可读消息（对齐 DSH 的提取顺序）。"""

    message = f"DeepSeek API error (HTTP {status})"
    detail = None
    try:
        parsed = json.loads(raw.decode("utf-8", "replace"))
        if isinstance(parsed, dict):
            error = parsed.get("error")
            if isinstance(error, str):
                detail = error
            elif isinstance(error, dict):
                detail = error.get("message")
            if not detail:
                detail = parsed.get("message")
    except ValueError:
        pass
    if detail:
        message += f": {detail}"
    message += (
        f"\n\nThe web search request used endpoint {endpoint!r}. "
        "Search endpoint configuration is separate from chat."
    )
    return message


def default_transport(
    endpoint: str, headers: dict, body: dict, timeout: float, proxies: dict | None = None
) -> tuple[int, dict]:
    """真实 HTTP 传输；返回 ``(status, payload)``，非 2xx 抛 ``WEB_PROVIDER_ERROR``。"""

    data = json.dumps(body, ensure_ascii=False).encode("utf-8")
    request = urllib.request.Request(endpoint, data=data, headers=headers, method="POST")
    opener = urllib.request.build_opener(
        _NoRedirectHandler(), urllib.request.ProxyHandler(proxies)
    )
    try:
        with opener.open(request, timeout=timeout) as response:
            raw = response.read()
            status = getattr(response, "status", response.getcode())
    except SearchError:
        raise
    except urllib.error.HTTPError as exc:
        raise SearchError(
            "WEB_PROVIDER_ERROR", http_error_message(exc.code, exc.read(), endpoint)
        ) from None
    except (urllib.error.URLError, OSError, TimeoutError) as exc:
        reason = getattr(exc, "reason", exc)
        raise SearchError(
            "WEB_PROVIDER_ERROR",
            f"DeepSeek search request failed: {reason} (endpoint {endpoint})",
        ) from None

    try:
        payload = json.loads(raw.decode("utf-8"))
    except (ValueError, UnicodeDecodeError) as exc:
        raise SearchError(
            "WEB_PROVIDER_ERROR",
            f"DeepSeek returned an unprocessable response body: {exc} (endpoint {endpoint})",
        ) from None
    if not isinstance(payload, dict):
        raise SearchError(
            "WEB_PROVIDER_ERROR",
            f"DeepSeek returned a non-object response body (endpoint {endpoint})",
        )
    if not 200 <= status < 300:
        raise SearchError("WEB_PROVIDER_ERROR", http_error_message(status, raw, endpoint))
    return status, payload


# ---------------------------------------------------------------------------
# 响应解析（与 DSH 的 mapAnthropicResponse 一一对应）
# ---------------------------------------------------------------------------


def _citation_snippets(blocks: list) -> dict[str, str]:
    """从所有 text 块的 citations 建 url -> cited_text（首次出现胜出）。"""

    snippets: dict[str, str] = {}
    for block in blocks:
        if not isinstance(block, dict) or block.get("type") != "text":
            continue
        for citation in block.get("citations") or []:
            if not isinstance(citation, dict):
                continue
            url = citation.get("url")
            text = citation.get("cited_text")
            if url and text and url not in snippets:
                snippets[url] = text
    return snippets


def parse_search_response(payload: dict, *, max_results: int = DEFAULT_MAX_RESULTS) -> tuple[list[Source], bool]:
    """把 Messages 响应映射为 ``(sources, truncated)``；无结果块即报错。"""

    blocks = payload.get("content") or []
    result_blocks = [
        block
        for block in blocks
        if isinstance(block, dict) and block.get("type") == "web_search_tool_result"
    ]
    if not result_blocks:
        raise SearchError(
            "WEB_PROVIDER_ERROR",
            "DeepSeek returned no web_search_tool_result blocks; "
            "the request may not have triggered native web search",
        )

    snippets = _citation_snippets(blocks)
    seen: set[str] = set()
    sources: list[Source] = []
    for block in result_blocks:
        for item in block.get("content") or []:
            if not isinstance(item, dict) or item.get("type") != "web_search_result":
                continue
            url = item.get("url") or ""
            if not url or url in seen:
                continue
            seen.add(url)
            sources.append(
                Source(
                    url=url,
                    title=item.get("title") or None,
                    snippet=snippets.get(url),
                    published_at=item.get("page_age") or None,
                )
            )

    truncated = len(sources) > max_results
    return sources[:max_results], truncated


def extract_answer(payload: dict) -> str | None:
    """把 text 块拼成模型正文；默认调用方不会使用（DSH 不信任它为答案）。"""

    parts = []
    for block in payload.get("content") or []:
        if isinstance(block, dict) and block.get("type") == "text":
            text = (block.get("text") or "").strip()
            if text:
                parts.append(text)
    return "\n\n".join(parts) or None


def merge_round_robin(results: list[SearchResult], *, max_results: int) -> tuple[list[Source], bool]:
    """多查询结果按排名轮询合并、按 URL 去重（对齐 DSH dsh-tool-web 的行为）。"""

    merged: list[Source] = []
    seen: set[str] = set()
    depth = max((len(result.sources) for result in results), default=0)
    for rank in range(depth):
        for result in results:
            if rank >= len(result.sources):
                continue
            source = result.sources[rank]
            if source.url in seen:
                continue
            seen.add(source.url)
            merged.append(source)
    truncated = len(merged) > max_results
    return merged[:max_results], truncated


# ---------------------------------------------------------------------------
# 缓存（搜索很贵，默认开启；按 endpoint/model/max_uses/query 建键）
# ---------------------------------------------------------------------------


class FileCache:
    """极简 JSON 文件缓存；单进程内加锁，原子替换写入，超龄条目即失效。"""

    def __init__(self, path: str | os.PathLike, max_age: float = DEFAULT_CACHE_MAX_AGE) -> None:
        self.path = Path(path).expanduser()
        self.max_age = float(max_age)
        self._lock = threading.Lock()

    @staticmethod
    def make_key(*parts: str) -> str:
        return hashlib.sha256("\x00".join(parts).encode("utf-8")).hexdigest()

    def get(self, key: str) -> dict | None:
        with self._lock:
            entry = self._load().get(key)
        if not entry:
            return None
        if time.time() - float(entry.get("created", 0)) > self.max_age:
            return None
        return entry.get("value")

    def put(self, key: str, value: dict) -> None:
        with self._lock:
            data = self._load()
            data[key] = {"created": time.time(), "value": value}
            if len(data) > CACHE_MAX_ENTRIES:
                oldest = sorted(data, key=lambda k: float(data[k].get("created", 0)))
                for stale in oldest[: len(data) - CACHE_MAX_ENTRIES]:
                    data.pop(stale, None)
            self._store(data)

    def _load(self) -> dict:
        try:
            parsed = json.loads(self.path.read_text("utf-8"))
        except (OSError, ValueError):
            return {}
        return parsed if isinstance(parsed, dict) else {}

    def _store(self, data: dict) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_name(f"{self.path.name}.tmp{os.getpid()}")
        tmp.write_text(json.dumps(data, ensure_ascii=False), "utf-8")
        os.replace(tmp, self.path)


def result_from_dict(payload: dict) -> SearchResult:
    sources = [
        Source(
            url=item.get("url", ""),
            title=item.get("title"),
            snippet=item.get("snippet"),
            published_at=item.get("publishedAt"),
        )
        for item in payload.get("sources") or []
        if item.get("url")
    ]
    return SearchResult(
        query=payload.get("query", ""),
        sources=sources,
        truncated=bool(payload.get("truncated")),
        answer=payload.get("answer"),
        usage=payload.get("usage"),
    )


# ---------------------------------------------------------------------------
# 客户端
# ---------------------------------------------------------------------------


class DeepSeekSearch:
    """一次调用一条查询；多条查询用 :meth:`search_many` 并发、:meth:`search_merged` 合并。"""

    def __init__(
        self,
        api_key: str | None = None,
        *,
        base_url: str | None = None,
        model: str = DEFAULT_MODEL,
        max_tokens: int = DEFAULT_MAX_TOKENS,
        max_uses: int = DEFAULT_MAX_USES,
        api_version: str = DEFAULT_API_VERSION,
        timeout: float = DEFAULT_TIMEOUT,
        transport=None,
        cache: bool = True,
        cache_path: str | None = None,
        cache_max_age: float = DEFAULT_CACHE_MAX_AGE,
        proxies: dict | None = None,
        max_concurrency: int = 4,
        user_agent: str = USER_AGENT,
    ) -> None:
        key = api_key if api_key is not None else os.environ.get("DEEPSEEK_API_KEY", "")
        self.api_key = (key or "").strip()
        resolved_base = (
            base_url or os.environ.get("DEEPSEEK_SEARCH_BASE_URL") or DEFAULT_BASE_URL
        ).rstrip("/")
        if not resolved_base.startswith(("http://", "https://")):
            raise ValueError(f"base_url must be http(s): {resolved_base!r}")
        self.base_url = resolved_base
        self.model = model
        self.max_tokens = int(max_tokens)
        self.max_uses = int(max_uses)
        self.api_version = api_version
        self.timeout = float(timeout)
        self.user_agent = user_agent
        self.max_concurrency = max(1, int(max_concurrency))
        self._transport = transport or default_transport
        self._proxies = proxies
        if cache:
            self._cache: FileCache | None = FileCache(
                cache_path or os.environ.get("DEEPSEEK_SEARCH_CACHE") or DEFAULT_CACHE_PATH,
                cache_max_age,
            )
        else:
            self._cache = None

    # -- 构造 ---------------------------------------------------------------

    def build_body(self, query: str, max_uses: int | None = None) -> dict:
        """构造与 DSH 完全一致的 Messages 请求体。"""

        return {
            "model": self.model,
            "max_tokens": self.max_tokens,
            "messages": [
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "text",
                            "text": f"Perform a web search for the query: {query}",
                        }
                    ],
                }
            ],
            "tools": [
                {
                    "type": "web_search_20250305",
                    "name": "web_search",
                    "max_uses": int(max_uses if max_uses is not None else self.max_uses),
                }
            ],
        }

    def _headers(self, api_key: str) -> dict:
        return {
            "x-api-key": api_key,
            "authorization": f"Bearer {api_key}",
            "anthropic-version": self.api_version,
            "content-type": "application/json",
            "accept": "application/json",
            "user-agent": self.user_agent,
        }

    # -- 搜索 ---------------------------------------------------------------

    def search(
        self,
        query: str,
        *,
        max_results: int = DEFAULT_MAX_RESULTS,
        max_uses: int | None = None,
        timeout: float | None = None,
        include_answer: bool = False,
        use_cache: bool | None = None,
    ) -> SearchResult:
        """执行一条查询。返回规范化结果；失败抛 :class:`SearchError`。"""

        text = (query or "").strip()
        if not text:
            raise ValueError("query must be a non-empty string")
        if int(max_results) < 1:
            raise ValueError("max_results must be a positive integer")
        if not self.api_key:
            raise SearchError(
                "WEB_PROVIDER_CREDENTIAL_MISSING",
                "DeepSeek search has no API key; set DEEPSEEK_API_KEY, pass --env-file, "
                "or construct DeepSeekSearch(api_key=...)",
            )

        uses = int(max_uses if max_uses is not None else self.max_uses)
        body = self.build_body(text, uses)
        cache = self._cache if (use_cache is None or use_cache) else None
        cache_key = None
        if cache is not None:
            cache_key = FileCache.make_key(
                self.base_url,
                self.model,
                str(self.max_tokens),
                str(uses),
                "answer" if include_answer else "sources",
                text,
            )
            hit = cache.get(cache_key)
            if hit is not None:
                result = result_from_dict(hit)
                result.cached = True
                return result

        endpoint = f"{self.base_url}/messages"
        _status, payload = self._transport(
            endpoint,
            self._headers(self.api_key),
            body,
            float(timeout if timeout is not None else self.timeout),
            self._proxies,
        )
        sources, truncated = parse_search_response(payload, max_results=int(max_results))
        usage = payload.get("usage") if isinstance(payload.get("usage"), dict) else None
        result = SearchResult(
            query=text,
            sources=sources,
            truncated=truncated,
            answer=extract_answer(payload) if include_answer else None,
            usage=usage,
        )
        if cache is not None and cache_key is not None:
            cache.put(cache_key, result.to_dict())
        return result

    def search_many(
        self,
        queries: list[str],
        *,
        max_results: int = DEFAULT_MAX_RESULTS,
        max_uses: int | None = None,
        timeout: float | None = None,
        include_answer: bool = False,
        use_cache: bool | None = None,
    ) -> list[SearchResult]:
        """并发执行多条查询，返回顺序与输入一致；任一条失败则中止其余并抛出首个错误。"""

        unique = _dedupe_queries(queries)
        if not unique:
            raise ValueError("queries must contain at least one non-empty string")
        results: list[SearchResult | None] = [None] * len(unique)
        with concurrent.futures.ThreadPoolExecutor(
            max_workers=min(self.max_concurrency, len(unique))
        ) as pool:
            futures = {
                pool.submit(
                    self.search,
                    query,
                    max_results=max_results,
                    max_uses=max_uses,
                    timeout=timeout,
                    include_answer=include_answer,
                    use_cache=use_cache,
                ): index
                for index, query in enumerate(unique)
            }
            try:
                for future in concurrent.futures.as_completed(futures):
                    results[futures[future]] = future.result()
            except BaseException:
                for future in futures:
                    future.cancel()
                raise
        return [result for result in results if result is not None]

    def search_merged(
        self,
        queries: list[str],
        *,
        max_results: int = DEFAULT_MAX_RESULTS,
        **kwargs,
    ) -> SearchResult:
        """多查询并发后轮询合并（对齐 DSH ``web_search({queries})`` 的输出）。"""

        unique = _dedupe_queries(queries)
        results = self.search_many(unique, max_results=max_results, **kwargs)
        merged, truncated = merge_round_robin(results, max_results=int(max_results))
        answer = "\n\n".join(r.answer for r in results if r.answer) or None
        return SearchResult(
            query=" | ".join(unique),
            sources=merged,
            truncated=truncated,
            answer=answer,
        )


def _dedupe_queries(queries: list[str]) -> list[str]:
    seen: set[str] = set()
    unique: list[str] = []
    for query in queries or []:
        text = (query or "").strip()
        if not text or text in seen:
            continue
        seen.add(text)
        unique.append(text)
    return unique


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def load_env_file(path: str) -> dict[str, str]:
    """读 KEY=VALUE 形式的 env 文件；只填不覆盖已有的进程环境变量。"""

    loaded: dict[str, str] = {}
    try:
        lines = Path(path).expanduser().read_text("utf-8").splitlines()
    except OSError as exc:
        raise SearchError("WEB_PROVIDER_CREDENTIAL_MISSING", f"cannot read env file {path}: {exc}") from None
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        name, _, value = line.partition("=")
        name = name.strip()
        if name.startswith("export "):
            name = name[len("export "):].strip()
        value = value.strip().strip("'\"")
        if name and name not in os.environ:
            os.environ[name] = value
            loaded[name] = value
    return loaded


def _redacted_headers(client: DeepSeekSearch) -> dict:
    return {
        "x-api-key": "<DEEPSEEK_API_KEY>",
        "authorization": "Bearer <DEEPSEEK_API_KEY>",
        "anthropic-version": client.api_version,
        "content-type": "application/json",
        "accept": "application/json",
        "user-agent": client.user_agent,
    }


def render_markdown(result: SearchResult) -> str:
    """渲染成接近 DSH ``web_search`` 工具输出的文本。"""

    lines: list[str] = []
    if result.answer:
        lines.append(result.answer)
        lines.append("")
    lines.append("Sources:")
    for source in result.sources:
        label = source.title or source.url
        line = f"- [{label}]({source.url})"
        if source.published_at:
            line += f"  ({source.published_at})"
        lines.append(line)
        if source.snippet:
            lines.append(f"  > {source.snippet}")
    if not result.sources:
        lines.append("- (no sources)")
    if result.truncated:
        lines.append("")
        lines.append(
            f"(Showing the first {len(result.sources)} sources. Refine the query for more.)"
        )
    if result.cached:
        lines.append("")
        lines.append("(cached result)")
    return "\n".join(lines)


def build_arg_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="deepseek_search.py",
        description="DeepSeek 原生 web 搜索（DSH dsh-web-search-deepseek 协议复刻）",
    )
    parser.add_argument("queries", nargs="+", help="一条或多条查询；多条默认并发后轮询合并")
    parser.add_argument("--max-results", type=int, default=DEFAULT_MAX_RESULTS, help="来源上限（默认 8）")
    parser.add_argument("--max-uses", type=int, default=None, help=f"单次调用内的服务端搜索次数（默认 {DEFAULT_MAX_USES}）")
    parser.add_argument("--model", default=DEFAULT_MODEL, help=f"模型名（默认 {DEFAULT_MODEL}）")
    parser.add_argument("--base-url", default=None, help=f"端点基址（默认 {DEFAULT_BASE_URL}）")
    parser.add_argument("--max-tokens", type=int, default=DEFAULT_MAX_TOKENS, help=f"生成上限（默认 {DEFAULT_MAX_TOKENS}）")
    parser.add_argument("--timeout", type=float, default=DEFAULT_TIMEOUT, help=f"超时秒数（默认 {DEFAULT_TIMEOUT:g}）")
    parser.add_argument("--include-answer", action="store_true", help="同时返回模型正文（默认只信结构化来源）")
    parser.add_argument("--per-query", action="store_true", help="多查询时逐条输出，而非合并")
    parser.add_argument("--json", action="store_true", help="输出 JSON 而非 markdown")
    parser.add_argument("--env-file", default=None, help=f"密钥 env 文件（默认在需要时尝试 {DEFAULT_ENV_FILE}）")
    parser.add_argument("--no-env-file", action="store_true", help="禁止自动读取默认 env 文件")
    parser.add_argument("--api-key", default=None, help="直接传 key（不推荐，易进 shell 历史）")
    parser.add_argument("--no-cache", action="store_true", help="禁用磁盘缓存")
    parser.add_argument("--cache-file", default=None, help=f"缓存路径（默认 {DEFAULT_CACHE_PATH}）")
    parser.add_argument("--cache-max-age", type=float, default=DEFAULT_CACHE_MAX_AGE, help="缓存有效期秒数（默认 86400）")
    parser.add_argument("--dry-run", action="store_true", help="只打印请求体，不联网、不读密钥")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_arg_parser().parse_args(argv)
    queries = _dedupe_queries(args.queries)
    if not queries:
        print("error: queries must contain at least one non-empty string", file=sys.stderr)
        return 2

    client = DeepSeekSearch(
        args.api_key,
        base_url=args.base_url,
        model=args.model,
        max_tokens=args.max_tokens,
        max_uses=args.max_uses if args.max_uses is not None else DEFAULT_MAX_USES,
        timeout=args.timeout,
        cache=not args.no_cache,
        cache_path=args.cache_file,
        cache_max_age=args.cache_max_age,
    )

    if args.dry_run:
        payload = [
            {
                "endpoint": f"{client.base_url}/messages",
                "headers": _redacted_headers(client),
                "body": client.build_body(query, args.max_uses),
            }
            for query in queries
        ]
        print(json.dumps(payload, ensure_ascii=False, indent=2))
        return 0

    if args.api_key is None and not os.environ.get("DEEPSEEK_API_KEY"):
        env_file = args.env_file
        if env_file is None and not args.no_env_file:
            candidate = Path(DEFAULT_ENV_FILE).expanduser()
            env_file = str(candidate) if candidate.exists() else None
        if env_file:
            try:
                load_env_file(env_file)
            except SearchError as exc:
                print(f"error {exc}", file=sys.stderr)
                return 1
        # 重建客户端，让刚加载的 key 生效
        client = DeepSeekSearch(
            None,
            base_url=args.base_url,
            model=args.model,
            max_tokens=args.max_tokens,
            max_uses=args.max_uses if args.max_uses is not None else DEFAULT_MAX_USES,
            timeout=args.timeout,
            cache=not args.no_cache,
            cache_path=args.cache_file,
            cache_max_age=args.cache_max_age,
        )

    try:
        if len(queries) > 1 and not args.per_query:
            results = [client.search_merged(
                queries,
                max_results=args.max_results,
                include_answer=args.include_answer,
            )]
        else:
            results = client.search_many(
                queries,
                max_results=args.max_results,
                include_answer=args.include_answer,
            )
    except SearchError as exc:
        print(f"error {exc}", file=sys.stderr)
        return 1

    if args.json:
        print(json.dumps([result.to_dict() for result in results], ensure_ascii=False, indent=2))
    else:
        for index, result in enumerate(results):
            if index:
                print()
            print(render_markdown(result))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

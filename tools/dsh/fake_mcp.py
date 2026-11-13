#!/usr/bin/env python3
"""假 MCP 服务端 —— 只用来在没有 Rust `shangyi mcp-serve` 时验证 profile 装配。

协议：JSON-RPC 2.0 over stdio，每行一条 JSON（newline-delimited）。
这个 framing 不是猜的：dsh 内置的 `@modelcontextprotocol/client@2.2.0` 里
`serializeMessage()` 就是 `JSON.stringify(message) + "\n"`，读侧用 ReadBuffer
按换行切分（本机已核，见 dsh/README.md「已核」表）。

实现的方法（够 dsh-mcp-client 完成一次装配）：
  - initialize                 → 回 protocolVersion / capabilities / serverInfo
  - notifications/initialized  → 无响应（通知）
  - tools/list                 → 2 个假工具
  - tools/call                 → 确定性 JSON 结果（不随机、不联网、不写文件）
  - ping / resources/list / prompts/list → 空结果，避免装配期被探活请求打断
  - 其它                       → JSON-RPC -32601

两个假工具刻意对齐商奕的领域语义（求解 + 检索），不是 hello world：
  - solve_equilibria  返回一份**确定性**解集 JSON（对应铁律 1：数值来自工具）
  - kb_retrieve       返回两条固定理论库条目（对应 kb 检索面）

用法：
  python3 tools/dsh/fake_mcp.py                  # 作为 stdio 服务端跑
  python3 tools/dsh/fake_mcp.py --selftest       # 自测：spawn 自己，跑握手断言
  python3 tools/dsh/fake_mcp.py --trace /tmp/x   # 把收到的每条消息追加到 /tmp/x

`--selftest` 存在的理由：smoke.sh 需要一个"断言 initialize → tools/list 拿到 2 个工具"
的客户端，把它留在同一个文件里，协议的两端就只有一份实现，不会互相漂移。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time

PROTOCOL_VERSION = "2026-07-28"
MODERN_PROTOCOL_MIN = "2026-07-28"   # 从这一修订版起：server/discover + resultType
LEGACY_PROTOCOL_VERSION = "2025-11-25"  # dsh 客户端回落时用的修订版（本机实测）
SERVER_NAME = "shangyi-fake"
SERVER_VERSION = "0.1.0"

# FAKE_MCP_LEGACY=1 时故意对 server/discover 回 -32601，逼客户端走 legacy
# initialize 路径。用来同时覆盖两条协商路径（smoke.sh 会各跑一次）。
_DISCOVER_ENABLED = os.environ.get("FAKE_MCP_LEGACY") != "1"

# 服务端声明的 instructions 会被 dsh-mcp-client 当字面文本并入 system prompt
# （见 @deepseek-ai/dsh-mcp-client/README.md「Server instructions」）。
SERVER_INSTRUCTIONS = (
    "这是 shangyi 的假 MCP 服务端（R1 冒烟用）。它只返回固定数据，"
    "不求解、不联网。任何真实数值都必须来自真实服务端。"
)

TOOLS = [
    {
        "name": "solve_equilibria",
        "title": "求解博弈均衡",
        "description": (
            "对给定的博弈结构做确定性求解，返回均衡集与每个均衡的支付向量。"
            "这是报告里数值的合法来源：模型不得自行计算或补全这些数字。"
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "game_spec": {
                    "type": "string",
                    "description": "GameSpec JSON 文本（玩家、策略、支付矩阵）",
                },
                "method": {
                    "type": "string",
                    "enum": ["support_enumeration", "lemke_howson", "analytic"],
                    "description": "求解方法；缺省 support_enumeration",
                },
            },
            "required": ["game_spec"],
            "additionalProperties": False,
        },
    },
    {
        "name": "kb_retrieve",
        "title": "检索理论库",
        "description": (
            "从商奕理论库检索与查询相关的条目，返回条目 id、标题与摘录。"
            "返回的是原文摘录，不是模型生成的解释。"
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "检索词"},
                "k": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10,
                    "description": "返回条数上限，缺省 3",
                },
            },
            "required": ["query"],
            "additionalProperties": False,
        },
    },
]

# 固定结果：同样入参永远同样出参，便于断言与 diff。
SOLVE_FIXTURE = {
    "game_spec_id": "fixture-prisoner-dilemma",
    "method": "support_enumeration",
    "equilibria": [
        {
            "kind": "nash_mixed",
            "strategies": {"row": {"cooperate": 0.5, "defect": 0.5},
                           "col": {"cooperate": 0.5, "defect": 0.5}},
            "payoffs": {"row": 0.5, "col": 0.5},
        }
    ],
    "provenance": {"solver": "fake_mcp.py", "deterministic": True},
}

KB_FIXTURE = [
    {
        "id": "kb/prisoners-dilemma",
        "title": "囚徒困境：占优策略与均衡的唯一性",
        "excerpt": "单次囚徒困境中，背叛是双方的严格占优策略，均衡唯一且帕累托劣于合作。",
    },
    {
        "id": "kb/mixed-strategy-existence",
        "title": "Nash 存在性定理",
        "excerpt": "有限博弈至少存在一个混合策略 Nash 均衡（Nash, 1950）。",
    },
]


def _trace(path: str | None, direction: str, payload: object) -> None:
    if not path:
        return
    try:
        with open(path, "a", encoding="utf-8") as handle:
            # pid 用来分辨"探针进程"与"正式服务进程"：dsh 的 stdio 协商会先起一个
            # 探针 sibling 再起正式服务（@modelcontextprotocol/client 的
            # version-negotiation sibling flow），两个进程各写各的行。
            handle.write(json.dumps({"ts": time.time(), "pid": os.getpid(),
                                     "dir": direction, "payload": payload},
                                    ensure_ascii=False) + "\n")
    except OSError:
        pass  # 追踪失败绝不能影响协议本身


def _result(request_id, result: dict, modern: bool = False, cacheable: bool = False) -> dict:
    # 2026-07-28 起的协议要求每个结果带 resultType 判别符（本机实测：缺了会被
    # dsh 的客户端判为 Invalid result，报 "missing required resultType"）。
    # 早期修订版不要求，多带反而无意义，所以只在 modern 路径上加。
    if modern:
        result = {**result, "resultType": "complete"}
        if cacheable:
            # 可缓存结果（tools/list 等）在 2026-07-28 下还必须带 SEP-2549 的
            # 缓存提示，否则同样是 Invalid result（本机实测：缺 ttlMs / cacheScope
            # 会分别报 invalid_type / invalid_value）。
            # ttlMs=0 表示"立刻过期"：本服务端数据是本地固定桩，不需要缓存。
            result = {**result, "ttlMs": 0, "cacheScope": "private"}
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def _error(request_id, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def _is_modern(message: dict) -> bool:
    """按请求自带的 _meta 判断是否走 2026-07-28 及以后的协议。

    本机实测（见 dsh/README.md「已核」表）：modern 路径不发明 initialize，
    协议版本随每个请求的 params._meta["io.modelcontextprotocol/protocolVersion"]
    传下来，服务端据此决定结果要不要带 resultType。
    """
    meta = (message.get("params") or {}).get("_meta") or {}
    version = meta.get("io.modelcontextprotocol/protocolVersion")
    return isinstance(version, str) and version >= MODERN_PROTOCOL_MIN


def _call_tool(params: dict) -> dict:
    name = params.get("name")
    arguments = params.get("arguments") or {}
    if name == "solve_equilibria":
        payload = dict(SOLVE_FIXTURE)
        payload["method"] = arguments.get("method", SOLVE_FIXTURE["method"])
        payload["game_spec_id"] = "fixture-" + str(len(str(arguments.get("game_spec", ""))))
        return {"content": [{"type": "text", "text": json.dumps(payload, ensure_ascii=False)}],
                "isError": False}
    if name == "kb_retrieve":
        limit = int(arguments.get("k", 3) or 3)
        rows = KB_FIXTURE[: max(1, min(limit, len(KB_FIXTURE)))]
        payload = {"query": arguments.get("query", ""), "hits": rows}
        return {"content": [{"type": "text", "text": json.dumps(payload, ensure_ascii=False)}],
                "isError": False}
    return {"content": [{"type": "text", "text": f"unknown tool: {name!r}"}], "isError": True}


def handle(message: dict) -> dict | None:
    """处理一条入站消息；返回要写出的响应（通知返回 None）。"""
    method = message.get("method")
    request_id = message.get("id")
    params = message.get("params") or {}
    modern = _is_modern(message)

    if method == "server/discover":
        if not _DISCOVER_ENABLED:
            # 故意不实现：客户端收到 -32601 会回落到 legacy initialize。
            return _error(request_id, -32601, "method not found: 'server/discover' (forced legacy)")
        # 2026-07-28 起服务端必须实现这个探测方法（DiscoverResultSchema：
        # supportedVersions + capabilities + 可选 instructions）。
        # 不实现也能工作：dsh 的客户端收到 -32601 会回落到 legacy initialize，
        # 本机实测过两条路径都能完成装配（见 dsh/README.md「已核」表）。
        return _result(request_id, {
            "supportedVersions": [PROTOCOL_VERSION, LEGACY_PROTOCOL_VERSION],
            "capabilities": {"tools": {"listChanged": False}},
            "instructions": SERVER_INSTRUCTIONS,
        })
    if method == "initialize":
        requested = params.get("protocolVersion") or LEGACY_PROTOCOL_VERSION
        return _result(request_id, {
            "protocolVersion": requested,
            "capabilities": {"tools": {"listChanged": False}},
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "instructions": SERVER_INSTRUCTIONS,
        })
    if method in ("notifications/initialized", "initialized", "notifications/cancelled"):
        return None
    if method == "ping":
        return _result(request_id, {}, modern)
    if method == "tools/list":
        return _result(request_id, {"tools": TOOLS}, modern, cacheable=True)
    if method == "tools/call":
        return _result(request_id, _call_tool(params), modern)
    if method == "resources/list":
        return _result(request_id, {"resources": []}, modern, cacheable=True)
    if method == "resources/templates/list":
        return _result(request_id, {"resourceTemplates": []}, modern, cacheable=True)
    if method == "prompts/list":
        return _result(request_id, {"prompts": []}, modern, cacheable=True)
    return _error(request_id, -32601, f"method not found: {method!r}")


def serve(trace_path: str | None = None) -> int:
    """stdin 逐行读 JSON-RPC，stdout 逐行回 JSON-RPC。"""
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError as exc:
            sys.stdout.write(json.dumps(_error(None, -32700, f"parse error: {exc}")) + "\n")
            sys.stdout.flush()
            continue
        _trace(trace_path, "in", message)
        try:
            response = handle(message)
        except Exception as exc:  # 单条请求出错不拖垮整个服务端
            response = _error(message.get("id"), -32603, f"internal error: {exc}")
        if response is None:
            continue
        sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
        sys.stdout.flush()
        _trace(trace_path, "out", response)
    return 0


class _Reader(threading.Thread):
    """带超时的行读取：把 stdout 行塞进队列，主线程按 deadline 取。"""

    def __init__(self, stream):
        super().__init__(daemon=True)
        self._stream = stream
        self.queue: list[str] = []
        self.done = False

    def run(self) -> None:
        try:
            for line in self._stream:
                self.queue.append(line)
        finally:
            self.done = True


def _wait_for(reader: _Reader, predicate, deadline: float, what: str):
    while time.monotonic() < deadline:
        for index, line in enumerate(reader.queue):
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if predicate(message):
                return reader.queue.pop(index)
        if reader.done and not reader.queue:
            raise AssertionError(f"服务端提前关闭，没等到 {what}")
        time.sleep(0.01)
    raise AssertionError(f"超时（{what} 未在期限内返回）")


def _spawn(legacy: bool) -> subprocess.Popen:
    env = dict(os.environ)
    if legacy:
        env["FAKE_MCP_LEGACY"] = "1"
    else:
        env.pop("FAKE_MCP_LEGACY", None)
    return subprocess.Popen(
        [sys.executable, os.path.abspath(__file__)],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, encoding="utf-8", bufsize=1, env=env,
    )


def _run_path(legacy: bool, timeout: float) -> list[str]:
    """跑一条协商路径并断言，返回人读的检查清单。

    legacy=True：先 initialize（2025-11-25）再 tools/list。
    legacy=False：modern 路径，没有 initialize；server/discover 探测 + 每个请求
    自带 _meta.protocolVersion=2026-07-28，结果必须带 resultType。
    """
    server = _spawn(legacy)
    reader = _Reader(server.stdout)
    reader.start()
    deadline = time.monotonic() + timeout
    checks: list[str] = []
    meta = {"io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
            "io.modelcontextprotocol/clientInfo": {"name": "fake-mcp-selftest",
                                                   "version": SERVER_VERSION},
            "io.modelcontextprotocol/clientCapabilities": {}}
    try:
        def send(payload: dict) -> None:
            assert server.stdin is not None
            server.stdin.write(json.dumps(payload, ensure_ascii=False) + "\n")
            server.stdin.flush()

        if legacy:
            send({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                  "params": {"protocolVersion": LEGACY_PROTOCOL_VERSION,
                             "capabilities": {},
                             "clientInfo": {"name": "fake-mcp-selftest",
                                            "version": SERVER_VERSION}}})
            result = json.loads(_wait_for(reader, lambda m: m.get("id") == 1,
                                          deadline, "initialize 响应"))["result"]
            assert result["protocolVersion"] == LEGACY_PROTOCOL_VERSION, f"协议版本不符：{result}"
            assert result["serverInfo"]["name"] == SERVER_NAME, f"serverInfo 不符：{result}"
            assert "tools" in result["capabilities"], f"未声明 tools 能力：{result}"
            checks.append(f"initialize → protocolVersion={result['protocolVersion']} "
                          f"serverInfo={result['serverInfo']['name']} "
                          f"capabilities={sorted(result['capabilities'])}")
            send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        else:
            send({"jsonrpc": "2.0", "id": 1, "method": "server/discover",
                  "params": {"_meta": meta}})
            result = json.loads(_wait_for(reader, lambda m: m.get("id") == 1,
                                          deadline, "server/discover 响应"))["result"]
            assert PROTOCOL_VERSION in result["supportedVersions"], f"版本列表不符：{result}"
            assert "tools" in result["capabilities"], f"未声明 tools 能力：{result}"
            checks.append(f"server/discover → supportedVersions={result['supportedVersions']} "
                          f"capabilities={sorted(result['capabilities'])}")

        send({"jsonrpc": "2.0", "id": 2, "method": "tools/list",
              "params": {} if legacy else {"_meta": meta}})
        listed = json.loads(_wait_for(reader, lambda m: m.get("id") == 2,
                                      deadline, "tools/list 响应"))["result"]
        if not legacy:
            assert listed.get("resultType") == "complete", \
                f"modern 结果缺 resultType：{sorted(listed)}"
            # dsh 的客户端对 modern 的 tools/list 还强制要求 SEP-2549 缓存提示，
            # 缺了会直接判 Invalid result（本机实测）。
            assert isinstance(listed.get("ttlMs"), int), f"modern 结果缺 ttlMs：{sorted(listed)}"
            assert listed.get("cacheScope") in ("public", "private"), \
                f"modern 结果缺 cacheScope：{sorted(listed)}"
        tools = listed["tools"]
        names = [tool["name"] for tool in tools]
        assert len(tools) == 2, f"工具数不是 2：{names}"
        assert names == ["solve_equilibria", "kb_retrieve"], f"工具名不符：{names}"
        for tool in tools:
            assert tool["inputSchema"]["type"] == "object", f"schema 不是 object：{tool['name']}"
        checks.append(f"tools/list → {len(tools)} 个工具：{names}"
                      + ("" if legacy else "（含 resultType=complete / ttlMs / cacheScope）"))

        send({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
              "params": {"name": "solve_equilibria",
                         "arguments": {"game_spec": "{\"players\":[\"row\",\"col\"]}"},
                         **({} if legacy else {"_meta": meta})}})
        call = json.loads(_wait_for(reader, lambda m: m.get("id") == 3,
                                    deadline, "tools/call 响应"))["result"]
        assert call["isError"] is False, f"tools/call 报错：{call}"
        payload = json.loads(call["content"][0]["text"])
        assert payload["provenance"]["deterministic"] is True, f"结果未标记确定性：{payload}"
        checks.append("tools/call → solve_equilibria 返回确定性解集 "
                      f"(equilibria={len(payload['equilibria'])})")

        send({"jsonrpc": "2.0", "id": 4, "method": "no/such/method", "params": {}})
        err = json.loads(_wait_for(reader, lambda m: m.get("id") == 4,
                                   deadline, "未知方法错误响应"))
        assert err["error"]["code"] == -32601, f"未知方法未回 -32601：{err}"
        checks.append("未知方法 → JSON-RPC -32601")
    finally:
        try:
            if server.stdin:
                server.stdin.close()
        except OSError:
            pass
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
    return checks


def selftest(timeout: float = 20.0) -> int:
    """两条协商路径各跑一遍，任何断言失败都返回非零。

    这样 smoke.sh 只调一条命令，就同时覆盖：
      - legacy 路径（initialize → notifications/initialized → tools/list）
      - modern 路径（server/discover → tools/list，结果带 resultType）
      - 2 个工具、确定性 tools/call 结果、未知方法错误码
    """
    print("fake_mcp.py 自测：")
    for legacy in (True, False):
        label = "legacy 路径（initialize）" if legacy else "modern 路径（server/discover）"
        checks = _run_path(legacy, timeout)
        print(f"  [{label}] 通过")
        for check in checks:
            print(f"    ✓ {check}")
    return 0


def main(argv: list[str]) -> int:
    if "--selftest" in argv:
        return selftest()
    # 追踪路径优先取 --trace；否则取 FAKE_MCP_TRACE 环境变量。
    # 环境变量这条是给"被 dsh 直接 spawn"的场景用的：那时我们没法加命令行参数，
    # 但 dsh 的 stdio 子进程会继承父环境（只剔掉名字里含 KEY/PASSWORD/SECRET/TOKEN
    # 的变量和 DSH_*，见 dsh-mcp-client README「Environment scrubbing」）。
    trace_path = os.environ.get("FAKE_MCP_TRACE") or None
    if "--trace" in argv:
        index = argv.index("--trace")
        if index + 1 >= len(argv):
            sys.stderr.write("--trace 需要一个文件路径\n")
            return 2
        trace_path = argv[index + 1]
    return serve(trace_path)


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except BrokenPipeError:
        sys.exit(0)

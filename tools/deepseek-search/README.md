# deepseek-search

DSH `@deepseek-ai/dsh-web-search-deepseek` 的**协议复刻**：零依赖 Python 客户端 + CLI。

不依赖 DSH、Electron、cordis 或任何第三方包（只用 Python 标准库）。任何 Python 项目、
脚本或 agent 流程都能直接调用 DeepSeek 的原生 web 搜索。

## 为什么是"协议复刻"

DeepSeek Harness 没有自建搜索服务，也没有对外搜索 API（DeepSeek 不提供专用检索端点）。
它的 `web_search` 本质上是：

> 一次完整的 Anthropic 兼容 Messages 模型调用 + 服务端原生 `web_search_20250305` 工具

本目录复刻的就是这条线路：请求体、鉴权头、结果解析规则与 DSH 逐字段对齐，但不带
DSH 的插件运行时。协议细节见 `deepseek_search.py` 顶部 docstring。

## 快速使用

```bash
cd <repo>

# 密钥：优先进程环境；未设置时 CLI 自动尝试 ~/.shangyi-secrets/deepseek.env
export DEEPSEEK_API_KEY=sk-...

# 单查询，markdown 输出
python3 tools/deepseek-search/deepseek_search.py "全国大学生数学建模竞赛 2026 论文格式"

# 多查询：并发扇出 + 轮询合并 + 按 URL 去重（对齐 DSH web_search 工具行为）
python3 tools/deepseek-search/deepseek_search.py --json "query a" "query b"

# 只看请求体，不联网、不读密钥（自检/排错用）
python3 tools/deepseek-search/deepseek_search.py --dry-run "query"

# 离线测试：本机 127.0.0.1 HTTP 服务器，11 个用例，不需要外网和真 key
python3 tools/deepseek-search/test_deepseek_search.py -v
```

常用开关：`--max-results`（来源上限，默认 8）、`--max-uses`（单次调用内服务端搜索次数，
默认 5）、`--include-answer`（同时返回模型正文，默认丢弃）、`--per-query`（多查询逐条输出）、
`--no-cache`、`--timeout`（默认 60s）。

## 输出形状

```json
{
  "query": "…",
  "sources": [
    {"url": "https://…", "title": "…", "snippet": "…", "publishedAt": "2026-01-02"}
  ],
  "truncated": false,
  "usage": {"input_tokens": 4986, "output_tokens": 582,
            "server_tool_use": {"web_search_requests": 1}}
}
```

`title` / `snippet` / `publishedAt` 可能缺省：DeepSeek 只在模型引用时给出 `snippet`
（来自 text 块的 `citations[].cited_text`，按 URL 首次出现关联）；`publishedAt` 来自 `page_age`。

## Python 调用

```python
import sys
sys.path.insert(0, "tools/deepseek-search")
from deepseek_search import DeepSeekSearch

search = DeepSeekSearch()                        # 读 DEEPSEEK_API_KEY
result = search.search("铜价 库存 2026", max_results=8)
for source in result.sources:
    print(source.url, source.title, source.snippet)

merged = search.search_merged(["q1", "q2"])      # 多查询并发 + 轮询合并
```

错误统一抛 `SearchError`，`code` 沿用 DSH 的错误码：
`WEB_PROVIDER_CREDENTIAL_MISSING`（无密钥）、`WEB_PROVIDER_ERROR`（HTTP/传输/无结果块）。

## 与 DSH 实现的差异（有意为之）

| 项 | DSH | 这里 |
|---|---|---|
| 账号登录鉴权 `x-dsh-auth-token` | 支持 | 不支持，只用公开 API key |
| 模型正文 | 一律丢弃，绝不当答案 | 默认丢弃；`--include-answer` 显式打开 |
| 结果缓存 | 无 | 默认 24h 文件缓存（`~/.cache/deepseek-search/cache.json`） |
| HTTP 重定向 | 拒绝跟随 | 拒绝跟随 |
| `max_results` | seam 事后截断 | 客户端事后截断 |
| 多查询 | 工具层并发、轮询合并、URL 去重 | 同 |

## 成本与限制

- **一次搜索 = 一次完整模型轮次**（默认 `deepseek-v4-flash`、`max_tokens` 4096），
  输入与输出都计费；多查询并发 = 多次调用。`max_uses` 只限制单次调用内部的服务端搜索次数。
- DeepSeek 端没有结果数量、时间范围、域名过滤参数；`--max-results` 只是本地截断。
- 常常只有 URL + 标题，没有摘要；需要正文时再抓取页面。
- 本机实测（2026-10-13）：一次 `--max-uses 1 --max-tokens 512` 调用约 **4.3s**，
  计费 4986 输入 / 582 输出 token，1 次服务端搜索，返回 3 条来源。

## 下一步（可选）

需要让 Codex / DSH / 其他 agent 共用时，可以在本模块外包一个 MCP server
（`mcp` 包本机已装，约 50 行），或做成 FastAPI 小服务；客户端与 CLI 不用改。

# `shangyi-mcp` —— 商奕确定性博弈引擎的 MCP 服务端（v0.3）

一句话：`shangyi mcp-serve` 启动一个 **JSON-RPC 2.0 over stdio** 服务，向 DeepSeek Harness（DSH）暴露 `shangyi-tools` 注册表中的**全部 39 个确定性领域工具**以及**全部只读知识库/剧本 MCP Resources**。工具执行、精确求解、阶段闸门、双通道真人 Checkpoint 确认与 `case.jsonl` 事件留档全部在 Rust 侧完成。

```text
  DSH（宿主流式循环 + Web 工作台）         shangyi mcp-serve（纯 Rust）
  ┌────────────────────────────┐  stdio   ┌──────────────────────────────────┐
  │ agent loop / Magpie 路由   │◀────────▶│ ToolRegistry（39 个领域工具）    │
  │ dsh-plugin-shangyi(-wb)    │ JSON-RPC │ MCP Resources（35 KB + 4 剧本）  │
  │  ├ POST /api/shangyi/cp    │───┐      │ DomainExecutor（精确求解/检索）  │
  │  └ session/event 用户账本  │─┐ │      │ PhaseMachine（8 阶段产物闸门）   │
  └────────────────────────────┘ │ └─────▶│ CaseLog（case.jsonl 增量同步）   │
                                 └───────▶│ trusted_user_inputs.jsonl 核验   │
                                          └──────────────────────────────────┘
```

## 用法

```text
shangyi mcp-serve [--playbook <id>] [--mode scenario|whatif|discover] [--home <目录>]
                  [--resume <案例|latest>] [--auto-approve-checkpoints]
```

| 选项 | 缺省 | 说明 |
|---|---|---|
| `--playbook <id>` | `pricing_response` | 初始剧本；会话内可通过 `select_task_scope` 动态切换为 `market_entry` / `negotiation` / `competitor_intel` |
| `--mode <mode>` | `scenario` | 初始咨询问法；会话内可通过 `select_task_scope(mode, action)` 切换为 `scenario` / `whatif` / `discover` |
| `--home <目录>` | `$SHANGYI_HOME` → `$HOME/.shangyi` | 案例留档与运行态账本根目录 |
| `--resume <案例\|latest>` | 关闭 | 从既有 `case.jsonl` 重放恢复 `DomainExecutor` 与 `PhaseMachine` 阶段/产物状态 |
| `--auto-approve-checkpoints` | **关闭** | 仅用于自动化回归测试（日志记 `user_actor="auto"`，绝不伪装成 `"user"`） |

## 核心协议与工程特性（v0.3 已全量闭环）

1. **惰性 `CaseLog` 创建（消除 DSH 探针空目录污染）**：
   - DSH 的 `@modelcontextprotocol/client` 在连接前会先启动一次性探针子进程发送 `server/discover`。
   - `SessionHost::open` 启动期不创建案例目录，直到收到第一次实际 `tools/call` 时才落盘 `header.json`、`case.jsonl` 并更新 `cases/active.json`；`case_id` 采用毫秒 + PID 后缀，并发零碰撞。
2. **共享实时检索、POI 与多智能体推演后端**：
   - `BochaSearchBackend`（博查搜索）、`AmapPoi`（高德周边 POI）与 `SwarmActor` LLM 工厂（自动兼容本机 Magpie Gateway `~/.config/magpie/caller-keys.json` 与 `~/.shangyi-secrets/*.env`）已下沉至 `shangyi-intel` / `shangyi-swarm`，与 CLI 同源共用；未配置外部 API 密钥时按契约如实返回 `Blocked`（`isError = false`），绝不编造数据。
3. **双通道真人 Checkpoint 确认（Option 1A，严守铁律 2）**：
   - 调用 `checkpoint({items})` 只打开待确认关口（`CheckpointOpened`）并挂起阶段推进，严禁模型同轮自批。
   - **通道一（工作台 / CLI 外部确认）**：真人在 Web 工作台点击「核准通过 / 驳回修订」（`POST /api/shangyi/checkpoint`）或运行 `shangyi checkpoint <case_id|latest> --approve|--reject` 将 `CheckpointAnswered(user_actor="user")` 追加至 `case.jsonl`；`shangyi-mcp` 在每次 `tools/call` 前调用 `sync_external_case_events()` 增量同步并自动推进阶段机。
   - **通道二（对话式核验确认）**：DSH `session/event` 钩子将真实用户消息写入 `$SHANGYI_HOME/runtime/trusted_user_inputs.jsonl`；用户在关口打开后于对话框明确回复确认后，模型调用 `checkpoint({decision, user_quote})` 经引擎逐字比对关口后真实用户消息通过后，以 `user_actor="user"` 结算（伪造确认或重放关口前旧消息均被硬拦截）。
4. **MCP Resources（只读知识库与剧本暴露，Option 2A）**：
   - 实现 `resources/list`、`resources/templates/list` 与 `resources/read`：
     - `shangyi://kb/<slug>`：35 篇博弈论理论、方法与概念 Markdown 文档；
     - `shangyi://playbook/<id>`：4 套 YAML 行业剧本（启动时强制通过 `Playbook::validate()`）；
     - `shangyi://guide/store_location_research`：门店选址与商圈调研专题规程。
   - 严格拦截 `..`、绝对路径与未知 scheme 越界读取。
5. **Intake 阶段容量与 Schema 子集净化**：
   - `vn_map` 与 `pars_check` 在同一 `intake` 阶段可组合执行，直至信息齐备后再平滑推进至 `intel`；
   - JSON Schema 自动剥离 DSH 子集不支持的 `minimum`/`maximum`/`minItems`/`pattern` 等关键字并在描述中保留约束说明。

## 三类失败通道严格分离

| 失败类型 | 返回通道 | 语义 |
|---|---|---|
| 非法 JSON / 未知 RPC 方法 / 非法 URI | JSON-RPC `error`（`-32700` / `-32601` / `-32602` / `-32600`） | 协议级异常 |
| 错阶段调用 / 越任务范围 / 伪造关口确认 / 参数校验失败 | `result.isError = true` | 工具级可纠正错误 |
| 外部未取得（`Blocked`）/ 等待真人裁决（`NeedsUserInput` / `AsksUser`） | `result.isError = false` + 结构化 JSON | 真实世界缺口或挂起等待真人确认 |

## 测试门禁

```bash
tools/loop/build.sh test -p shangyi-mcp                       # 24 项单元 + 21 项协议/资源/双通道确认集成测试
tools/loop/build.sh test -p shangyi-cli --test mcp_serve_e2e  # 6 项真实子进程 stdio 端到端测试（含跨进程 checkpoint 与 --resume 重开）
```

# 商奕 × DeepSeek Harness（dsh）配置面（v0.3）

本目录是「商奕（ShangYi）内核跑在 DeepSeek Harness（DSH）上」的**宿主配置与受控技能面**仓库产物：
- **`dsh/profile/`**：命令行无头版 Profile（`shangyi`，基于 `dsh-base` + `dsh-headless`），由 `tools/dsh/install-profile.sh` 安装、`tools/dsh/smoke.sh` 26 项离线冒烟门禁验证；
- **`dsh/profile-web/`**：宣纸·墨玉三栏 Web 工作台版 Profile（`shangyi-web`，基于 `dsh-base` + `dsh-web-app` + 商奕三件套插件 `dsh-plugin-shangyi` / `dsh-plugin-shangyi-shell` / `dsh-plugin-shangyi-workbench`），由 `bash shangyi/ui/scripts/install-workbench.sh` 一键构建并安装；
- **`dsh/skills/`**：受控项目级技能包（`shangyi-consultation-discipline`、`shangyi-store-location`、`shangyi-dev-verify`），在 `shangyi-web` 的唯一专属预设 `preset-shangyi` 中通过 `includeDefaultRoots: false` 严格隔离加载。

形态一句话：**DSH 负责会话循环、模型路由与 Web 三栏工作台；`shangyi mcp-serve`（纯 Rust）负责全部 38 个确定性领域工具、MCP Resources、阶段/产物闸门、双通道真人 Checkpoint 确认与 `case.jsonl` 审计落盘。**

## 目录

```
dsh/
  README.md                  ← 本文件
  profile/                   ← 命令行无头版（装到 $DSH_HOME/profiles/shangyi）
    package.json             bundles = dsh-base + dsh-headless
    cordis.yml               空入口列表（dsh 每次启动重写，内容恒为 []）
    cordis.patch.yml         MCP 行 / persona / Magpie+DeepSeek 路由 / 21 行编码工具禁用
    pnpm-workspace.yaml      profile 是 pnpm workspace 根
    lenient/cordis.patch.yml 叠加层变体：failOnStartupError 降回 false（仅供无二进制 CI）
  profile-web/               ← Web 工作台版（装到 $DSH_HOME/profiles/shangyi-web）
    package.json             bundles = dsh-base + dsh-web-app + 商奕三件套 UI 插件
    cordis.yml               空入口列表
    cordis.patch.yml         mcp-shangyi + Magpie 网关路由 + 唯一专属预设 preset-shangyi
    pnpm-workspace.yaml      pnpm workspace 根
  skills/                    ← 受控项目级 Skills（仅暴露给 preset-shangyi）
    shangyi-consultation-discipline/SKILL.md
    shangyi-store-location/SKILL.md
    shangyi-dev-verify/SKILL.md  (disable-model-invocation: true)
tools/dsh/
  install-profile.sh         幂等安装 dsh/profile 到 ${DSH_HOME:-$HOME/.dsh}/profiles/shangyi
  fake_mcp.py                纯标准库假 MCP 服务端（2 个假工具，双协议路径）
  smoke.sh                   离线冒烟（26 项断言，不联网、不调模型）
```

## 快速开始

```bash
# 0) 编译商奕 Rust 二进制（CARGO_TARGET_DIR 默认落在 /tmp/shangyi-target）
tools/loop/build.sh build --release -p shangyi-cli
export SHANGYI_BIN=/tmp/shangyi-target/release/shangyi

# 1) 安装命令行 profile 并跑 26 项离线冒烟
tools/dsh/install-profile.sh
tools/dsh/smoke.sh

# 2) 通过本机 Magpie Gateway（http://127.0.0.1:3425/v1）跑真实会话
export MAGPIE_GATEWAY_KEY=...   # 或设置 DEEPSEEK_API_KEY 走官方直连
dsh --profile shangyi --json "帮我测算：月租 20000 元、押三付一、装修 15 万、租期 24 个月的总成本"

# 3) 一键构建并启动宣纸·墨玉 Web 工作台（默认端口 8791）
bash shangyi/ui/scripts/install-workbench.sh
```

冒烟脚本默认把 profile 装到 `/tmp/dsh-smoke`，**不会**写 `~/.dsh`（沙箱下 `~/.dsh`
也不可写）。要装到真实 HOME，直接跑 `tools/dsh/install-profile.sh`（不带 `DSH_HOME`）。

## 这个 profile 改了什么

四类改动，全部在 `dsh/profile/cordis.patch.yml`。patch 层是「按行 id 覆盖、最后写入者赢」，
且**整份 config 替换而非字段级合并**（`@deepseek-ai/dsh-base/cordis.patch.yml:6-7` 的注释原话：
"A patch replaces the targeted row's whole `config` rather than merging into it"）。
层序：`dsh-base` → `dsh-headless` → 本 profile → `$DSH_HOME/cordis.patch.yml` → `--patch` 叠加层。

### 1) MCP 客户端行（领域工具的入口）

```yaml
- insert:
    - id: mcp-shangyi
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: shangyi
        transport: stdio
        command: !!js 'process.env.SHANGYI_BIN ?? "shangyi"'
        args: ['mcp-serve']
        toolCallTimeoutMs: 300000
        failOnStartupError: true
```

- 配置键出处：`@deepseek-ai/dsh-mcp-client/README.md:55-70` 的 Minimal configuration 表
  （`serverName` / `transport` / `command` / `args` / `env` / `cwd` / `toolCallTimeoutMs` /
  `maxInstructionBytes` / `failOnStartupError` / `reconnect.*`）。工具名规则见同文件 `:71`：
  `mcp__<serverName>__<tool>`。
- **`${VAR}` 不被 mcp-client 展开**：它的 README 只提供「`env` 的值用 `!!js` 读环境变量」的写法。
  所以覆盖二进制路径的方式是 dsh patch 层的 `!!js` 表达式 + 环境变量：
  `SHANGYI_BIN=/abs/path/shangyi dsh --profile shangyi "..."`；不设则落到 PATH 上的 `shangyi`。
  这条 `!!js` 被真实求值过（冒烟 d1 用 `SHANGYI_BIN` 指向包装脚本，dsh 确实 spawn 到了它）。
- **`failOnStartupError: true` 是发布默认**（P1-3 的修复）：服务端缺失时不许静默。
  本机实测（无二进制、未设 `SHANGYI_BIN`）：
  - stderr 打印 `dsh: warning: 1 entry did not activate` +
    `mcp-client(shangyi): initial connection or tool synchronization failed` +
    **`Error: spawn shangyi ENOENT`**；
  - 模型可见工具面变成 **0 个**（连 3 个 MCP 资源工具都没有——`mcp-resources` 只在 client 行
    激活时才登记它们，见 `@deepseek-ai/dsh-mcp-resources/README.md`："Connection failures do not
    remove the shared tools while the client entry remains active"）；
  - **但运行不会中止**：dsh 的 required 列表是全局写死的（`agent-loop`/`webserver`/
    `web-runtime`/`modules`/`connection`/`headless-runner`/`acp`/`sdk-jsonrpc-server`，
    `@deepseek-ai/dsh-app-boot/README.md:43`、`:96`），patch 条目**没有** `required` 字段
    （`dsh --profile shangyi --dump-config-schema` 的 `entryMetadata` 只有
    `id/name/config/group/disabled/inject/intercept/isolate`）。所以「运行期硬失败」在 profile 层
    做不到——**硬门放在 `tools/dsh/install-profile.sh`**（装的时候就非零退出，见 §5 第 2 条）。
  - `--help` 与 `--dump-config` **不受影响**：两者都不激活插件，实测缺二进制时仍 exit 0、无 stderr。
- **lenient 变体**：`dsh/profile/lenient/cordis.patch.yml` 把 `failOnStartupError` 降回 `false`，
  安装后位于 `$DSH_HOME/profiles/shangyi/lenient.patch.yml`，用法
  `dsh --profile shangyi --patch "$DSH_HOME/profiles/shangyi/lenient.patch.yml" "..."`。
  它回到**静默降级**（无 MCP 报错、零领域工具、3 个资源工具还在），只适合"本来就不跑真实咨询"
  的机器（CI 只看配置树、评审机）。
- `toolCallTimeoutMs: 300000` 是**我的选择（未实测）**：默认 60s（README `:61`），
  八阶段里的求解/群仿真可能更长；真实耗时没有测过，见 §6。

### 2) 部署 persona（`system-prompt` 覆写）

`personaPrefix` 是 order 0 的全局 persona 模板（`@deepseek-ai/dsh-system-prompt/README.md:48`），
headless bundle 把它设成空串（`@deepseek-ai/dsh-headless/cordis.patch.yml:9-11`），本 profile 覆盖它。
内容按《商奕-Agent-Harness 设计文档》§2.1 的两条铁律写：

- **铁律 1 · LLM 不算数**：每个数字必须逐字来自确定性工具调用；不得计算/换算/补全/编造；
  工具没返回就写「未取得」并说明缺哪个输入。
- **铁律 2 · 关键假设必须过 checkpoint**：payoff、对手成本/收益、概率与先验必须经确认或标注
  `source ∈ {user_confirmed, public_data, prior_borrowed, llm_inferred}`；未确认不得进入求解。
- 工具纪律：决策依据只来自 `mcp__shangyi__*`；**另有 3 个 MCP 资源浏览工具**
  （`list_mcp_resources` / `list_mcp_resource_templates` / `read_mcp_resource`）——
  只读、只回原文、不产生数值（P2-7：这三行由 `mcp-resources` 提供，保留它们是因为
  R2 可能把理论库条目登记成 MCP 资源；要更紧的工具面就把 `mcp-resources` 那一行禁掉，
  实测不影响 session 持久化）。没有的能力说「本部署未提供」，不得模拟工具输出。
- 数据纪律：不编造数据/来源/引文，缺口如实报告。
- 新增一条兜底：**工具列表里一个 `mcp__shangyi__*` 都没有时，直接报「领域工具不可用」**，
  不许靠常识作答——这正是 P1-3 那个失败形态（部署方会误以为引擎没数据）。

> 文本里不能出现 `{{...}}`：system-prompt 会当变量插值，未解析即报错
> （`dsh-system-prompt/README.md:78` 关于 `{{cwd}}` 的警告同理）。

### 3) 模型路由（本机 Magpie Gateway + DeepSeek 官方双通道）

```yaml
- id: llm-pi-ai
  name: '@deepseek-ai/dsh-llm-pi-ai'
  config:
    providers:
      magpie:
        apiKeyEnv: MAGPIE_GATEWAY_KEY
        api: openai-completions
        baseURL: http://127.0.0.1:3425/v1
        ...
- id: llm-deepseek
  config: { apiKeyEnv: DEEPSEEK_API_KEY, reasoningEffort: high }
- id: agent-default-model
  config:
    provider: !!js 'process.env.SHANGYI_PROVIDER ?? (process.env.MAGPIE_GATEWAY_KEY ? "magpie" : "deepseek-official")'
    model: !!js 'process.env.SHANGYI_MODEL ?? (process.env.MAGPIE_GATEWAY_KEY ? "opencode-go/deepseek-v4.1-flash" : "deepseek-flash")'
    reasoningEffort: high
```

- 同时挂载 `@deepseek-ai/dsh-llm-pi-ai`（本机 Magpie Gateway `http://127.0.0.1:3425/v1`，读取 `MAGPIE_GATEWAY_KEY`）与 `@deepseek-ai/dsh-llm-deepseek-api-key`（`DEEPSEEK_API_KEY`）。
- 当环境中存在 `MAGPIE_GATEWAY_KEY` 时，自动路由至 `magpie`（默认 `opencode-go/deepseek-v4.1-flash`，亦可通过 `SHANGYI_PROVIDER` / `SHANGYI_MODEL` 覆盖）；当两个凭据均未设置时（如 `tools/dsh/smoke.sh` 离线冒烟），回退到 `deepseek-official/deepseek-flash` 并按预期返回 `MISSING_CREDENTIAL`。
- 在 `dsh/profile-web`（Web 工作台）中，由于 `@deepseek-ai/dsh-web-app` 使用 `agent-preset` 管理会话内插件，配置显式禁用 `preset-standard` / `preset-ptc` / `preset-minimal` / `preset-cordis` 等通用编码预设，仅保留唯一默认预设 `preset-shangyi`（内含商奕 persona、`dsh/skills` 隔离目录与 `tool-ask-user`），并由宿主插件 `dsh-plugin-shangyi` 提供 `/api/shangyi/*`（含 `POST /api/shangyi/checkpoint` 受控真人关口确认与 `session/event` 可信用户输入账本）。

### 4) 关掉的 21 行（编码类 / 非领域工具）

原则：**只禁「模型可见的工具行」及其执行后端；内部插件依赖的服务行保留**（`fs-sandbox`、
`skill`、`web`、`sandbox` 等），否则 session 持久化、附件、工作目录这些内部依赖会一起失效。
`@deepseek-ai/dsh-base/cordis.patch.yml` 里的行号是出处。

| 行 id | 是什么 | 为什么关 |
|---|---|---|
| `tool-bash` (`:281`) | shell 工具 | 咨询内核不需要跑命令；能力面越大，铁律 1 越难守 |
| `bash-sandbox` (`:249`) | bash 执行后端 | 与上一行配套；关掉后 `shell` 服务不存在 |
| `tool-pwsh` (`:285`) / `pwsh-sandbox` (`:255`) | Windows 侧同上 | 同一理由（本机是 darwin，这两行本就 `disabled`，显式关掉避免换平台时复活） |
| `tool-fs` (`:295`) / `tool-fs-search` (`:298`) | 文件读写 / glob / grep | 交付物是报告不是代码；领域数据应来自工具而非直接读盘 |
| `tool-jobs` (`:289`) | 后台作业（依赖 shell） | 依赖已禁的 shell，且属执行面 |
| `ptc-runtime` (`:397`) / `workflow-ptc` (`:400`) / `tool-workflow` (`:405`) | 程序化工具调用 / workflow | PTC 让模型写代码编排工具，与铁律 1「数值不得由模型生成的代码产出」直接冲突 |
| `tool-subagent` (`:380`) / `tool-subagent-fork` (`:391`) / `tool-subagent-control` (`:374`) / `tool-subagent-list-agents` (`:377`) | 子 agent 与消息 | 子 agent 会继承完整工具面，等于绕过本节全部限制 |
| `tool-working-directory` (`:169`) | `working_directory` 工具 | 非领域工具；服务行 `working-directory` 保留（内部仍需要 cwd 事实） |
| `tool-skill` (`:314`) | 加载 skill 的工具 | skill 会注入任意指令与脚本路径，超出部署 persona 的可控范围 |
| `plan-mode` (`:332`) | `exit_plan_mode` | headless 没有人工审批回环，计划模式无落点 |
| `tool-todo` (`:437`) / `tool-goal` (`:444`) | 任务清单 / 目标 | 记账类工具，不是领域工具 |
| `tool-web` (`:486`) | `web_search` / `web_fetch` | 外部文本未经核验，R1 不作为决策输入进入求解（服务行 `web*` 保留但无工具） |
| `permission` (`:264`) | 权限预设 | 它注入 `shell` 服务来组装预设；shell 关掉后该行永久 pending，每次启动打印告警。headless 无 UI、也无可审批工具，整行关掉（本机实测：关掉后启动告警归零，`approval` 行不受影响） |

保留的服务行（有意）：`fs-sandbox`、`fs-observation-policy`、`skill`、`skill-filesystem`、
`web`、`web-search-deepseek`、`web-fetch-http`、`mcp-resources`、`agent-instructions`、
`sandbox`/`sandbox-policy`、`approval`、`session-persistence-jsonl`、`attachment-local`、
`working-directory`、`commands*`、`goal`/`goal-round-driver`。

**结果已核**（不是推理）：装配后的模型可见工具面恰好 5 个——
`mcp__shangyi__solve_equilibria`、`mcp__shangyi__kb_retrieve`（假服务端给的领域工具）
+ `list_mcp_resources`、`list_mcp_resource_templates`、`read_mcp_resource`（`mcp-resources`
提供的资源发现/读取）。见 §5 的 d2 证据。

## 5. 本机已核（都是真跑过的，输出见 `tools/dsh/smoke.sh`）

环境：dsh `0.2.1-rc.2`，macOS/darwin，Python 3.13.15，`zstd` 可用。

| # | 项 | 怎么核的 | 结果 |
|---|---|---|---|
| 1 | profile 能从零装到干净 `DSH_HOME` | `DSH_HOME=/tmp/dsh-smoke tools/dsh/install-profile.sh` | 4 个文件齐；不需要 `pnpm install`（bundle 从 dsh 安装根解析） |
| 2 | **安装脚本的二进制硬门** | 无二进制跑 `install-profile.sh` / 加 `--allow-missing-server` 再跑 | 默认 **退出码 3** + 说明后果；加 flag 后退出码 0（显式降级） |
| 2b | 安装幂等 | 连跑两次，比 `cordis.patch.yml` 的 sha（`shasum` 缺失时退 `sha256sum`，再退 `cksum`） | 内容一致（`6c24b3ad…`） |
| 3 | `--dump-config` 能合成树 | `DSH_HOME=… dsh --profile shangyi --dump-config` | 退出码 0，467 行合成树 |
| 4 | 合成树含 mcp-client 行 | grep `mcp-shangyi` / `serverName: shangyi` / `transport: stdio` / `- mcp-serve` | 全部命中，见下方片段 |
| 5 | 合成树含 system-prompt 覆写 | grep `personaPrefix: \|` + 铁律 1/2 文本 | 命中 |
| 6 | 21 行编码/非领域工具确实禁用 | 按行 id 解析合成树，逐行断言 `disabled: true` | 21/21 通过（合成树 99 行里 disabled 共 26 行：我关的 21 + bundle 层自带的 5：`tool-plugin-manager`、headless 关的 `hmr`/`credentials`/`authorization`/`deepseek-account`） |
| 7 | 假服务端两条协商路径 | `python3 tools/dsh/fake_mcp.py --selftest` | legacy（`initialize`）与 modern（`server/discover`）各拿 2 个工具 |
| 8 | `dsh --profile shangyi --help` | 直接跑（**含缺二进制的情况**） | 退出码 0；缺二进制时 stderr 也为空（`--help` 不激活插件）。注：文案里写的是 `--profile headless`，那是 headless bundle 自己的帮助文本 |
| 8b | **缺二进制 + 严格默认必须吵** | 无 `SHANGYI_BIN` 跑 `dsh --profile shangyi --json …`，读 stderr 与 session log | stderr 含 `spawn shangyi ENOENT` + `did not activate`；`request/header` 里工具面为**空**（零领域工具，连 3 个资源工具都没有） |
| 8c | lenient 叠加层回到静默 | 同一台机器加 `--patch …/lenient.patch.yml` | 无 MCP 报错、退出码 1 仅因缺模型凭据（这就是要避免的形态，只在 CI 用） |
| 9 | **dsh 真实 spawn 假服务端** | `SHANGYI_BIN=… dsh --profile shangyi --patch <failOnStartupError:true> --json …`，用 `FAKE_MCP_TRACE` 抓 wire | 两条路径都完成握手；`failOnStartupError: true` 未触发；wire 上 `tools/list` 返回 2 个工具 |
| 10 | **persona 真的进了 system prompt** | 解 `$DSH_HOME/sessions/.../session.v4.jsonl.zstd` 里的 `system/message` | 1102 字符，含 persona 全文 + `MCP server: shangyi` 段 + 假服务端 instructions |
| 11 | **模型可见工具面被真正裁剪** | 同一日志的 `request/header` | 恰好 5 个（2 领域 + 3 资源），无 bash/fs/web/subagent/workflow |
| 12 | 模型路由解析正确 | 同一日志的 `request/context` | `deepseek-official` / `deepseek-flash` / `contextWindow=1000000` |
| 13 | 没有模型凭据时的失败形态 | 上面第 9 步的退出码与 stderr | 退出码 1，`MISSING_CREDENTIAL`（**不是** MCP 错误）；`did not activate` 归零 |
| 13b | **双 spawn 是探针 + 正式服务，不是重连** | 给 `fake_mcp.py` 的 trace 记 pid，跑一次真实装配（两条路径各一次） | 恰好 **2 个 PID**：探针只收到 `server/discover`（legacy 路径下它拿到 `-32601`），正式服务进程收到 `initialize`/`notifications/initialized`/`tools/list`；无重连日志 |
| 13c | patch 条目没有 `required` 字段 | `dsh --profile shangyi --dump-config-schema` | `entryMetadata` 字段只有 `id/name/config/group/disabled/inject/intercept/isolate`——这是"运行期无法硬失败"的依据 |
| 14 | `cordis.yml` 无漂移 | 装完 vs 启动后 vs 仓库，三方 sha | 逐字节一致（dsh 每次启动重写它，内容恒为 `[]` + 固定注释） |

`--dump-config` 里 mcp-client 与 system-prompt 的实际片段：

```yaml
# == /tmp/dsh-smoke/profiles/shangyi/cordis.patch.yml
- id: mcp-shangyi
  name: '@deepseek-ai/dsh-mcp-client'
  config:
    serverName: shangyi
    transport: stdio
    command: !!js process.env.SHANGYI_BIN ?? "shangyi"
    args:
      - mcp-serve
    toolCallTimeoutMs: 300000
```

```yaml
# == @deepseek-ai/dsh-base, patched by /tmp/dsh-smoke/profiles/shangyi/cordis.patch.yml
- id: system-prompt
  name: '@deepseek-ai/dsh-system-prompt'
  config:
    personaPrefix: |
      你是「商奕」（ShangYi）——博弈论金融决策咨询内核。你的交付物是一份能回答
      "你凭什么"的决策咨询报告：结论、依据，以及每个数字的来源。你不是通用编码
      助手，不要写代码、不要改文件、不要执行命令。
      …
```

`request/header` 里模型真正看到的工具（第 11 项证据，节选）：

```json
{"config": {"provider": "deepseek-official", "model": "deepseek-flash",
            "reasoningEffort": "high", "maxTokens": 256000},
 "tools": [{"name": "list_mcp_resource_templates"}, {"name": "list_mcp_resources"},
           {"name": "mcp__shangyi__kb_retrieve"},
           {"name": "mcp__shangyi__solve_equilibria"},
           {"name": "read_mcp_resource"}]}
```

## 6. 未核（`[未核]`，不许当成已验证）

| # | 未核事项 | 为什么没核 | 怎么核 |
|---|---|---|---|
| U1 | **真实模型 E2E**：dsh 起会话 → 模型调 `mcp__shangyi__*` → 走完八阶段 | 本 shell 无 `DEEPSEEK_API_KEY` / `MAGPIE_GATEWAY_KEY`；DeepSeek 余额为负（`.shangyi-loop/STATE.md` 记 -0.35 元）。冒烟只到 `MISSING_CREDENTIAL` 为止 | 充值后：`export DEEPSEEK_API_KEY=…` + `SHANGYI_BIN=…` → `dsh --profile shangyi --json "…"`，核对事件流里的工具调用与 provenance |
| U2 | persona 对模型行为的**实际**约束力（是否真的不再编数字） | 同上，没有一次真实生成 | U1 之后跑对抗用例：给一个缺 payoff 的问题，看它是否报「未取得」而不是编数 |
| U3 | `toolCallTimeoutMs: 300000` 是否够 | 没有真实求解调用，测不出耗时 | R2 接通后跑 `solve_equilibria` 计时；不够就改这一行 |
| U4 | `shangyi mcp-serve` 子命令本身 | R2 任务；当前 `shangyi` 只有 `new/resume/solve/report/backtest/kb` 子命令 | 见 §7 的协议要点 |
| U5 | 真实 Rust 服务端的 stdio 行为（缓冲/退出/信号） | 同上；本轮用的是 Python 假服务端 | R2 用 `FAKE_MCP_TRACE` 同款手法抓 wire 对比 |
| U6 | Windows 侧（`tool-pwsh` / `pwsh-sandbox`） | 本机 darwin | 在 Windows 上跑 `tools/dsh/smoke.sh` |
| U7 | 把 profile 装进真实 `~/.dsh` | 沙箱只允许写 workspace 与 `/tmp`，`~/.dsh` 不可写 | 在有权限的 shell 里跑 `tools/dsh/install-profile.sh`（不带 `DSH_HOME`） |
| U8 | dsh 版本演进风险 | 官方标注 developer preview | 锁 `0.2.1-rc.2`；profile 与 MCP 协议各自可替换 |
| U9 | **严格模式下"缺二进制 + 凭据齐备"时模型是否真会带 0 工具作答** | 无凭据，只观测到它走到模型请求这一步（`request/header` 无 `tools` 字段）并因 `MISSING_CREDENTIAL` 失败；无法证明有凭据时的行为 | 充值后：不设 `SHANGYI_BIN` 跑一轮，看模型是否回「领域工具不可用」而不是编数（persona 已加这条兜底） |
| U10 | 真实 `shangyi` 二进制被 dsh spawn 两次的**开销**（进程启动时间、是否有副作用如写文件） | 本轮用 Python 假服务端，秒级启动；Rust 二进制的启动成本没测 | R2 接通后计时两次 spawn 的耗时；若探针有副作用（写文件/建连接），要设计成幂等 |

## 7. 给 R2（`shangyi mcp-serve`）的协议要点

这些是**本轮实测出来的**，不是从规范抄的（证据：`FAKE_MCP_TRACE` 抓的 wire + dsh 的报错原文）：

1. **framing**：stdio 是**每行一条 JSON**（newline-delimited），不是 LSP 的 `Content-Length` 头。
   依据：`@modelcontextprotocol/client@2.2.0` 的 `serializeMessage()` 就是
   `JSON.stringify(message) + "\n"`。
2. **dsh 会 spawn 两次，这是 SDK 的"探针 + 正式服务"，不是重连/重复注册**。
   实证（给 `fake_mcp.py` 的 trace 记 pid，两条路径各跑一次真实装配）：恰好 2 个 PID；
   探针进程只收到 `server/discover`（legacy 路径下它拿到 `-32601` 后就被回收），
   正式服务进程收到 `initialize` / `notifications/initialized` / `tools/list`（以及后续
   `tools/call`）。依据：`dsh-mcp-client/README.md:28` 原话 "stdio negotiation starts a
   temporary probe process before the serving process"，以及客户端
   `@modelcontextprotocol/client/dist/stdio.mjs:146-148` 的注释 "Reap a disposable probe
   sibling (see the version-negotiation sibling flow)"。**结论：预期行为，不需要调
   `reconnect` 配置。** 但服务端必须能被启动/终止多次，且**探针里不能有不可重入的副作用**。
3. **两条协商路径都支持**，dsh 会按第一条自动选：
   - modern（协议 `2026-07-28`）：探针发 `server/discover`（结果含 `supportedVersions` +
     `capabilities`），之后**没有** `initialize`；协议版本随每个请求的
     `params._meta["io.modelcontextprotocol/protocolVersion"]` 传下来；**结果必须带
     `resultType: "complete"`**，可缓存结果（`tools/list` 等）还必须带 **`ttlMs` + `cacheScope`**
     （缺了 dsh 直接判 `Invalid result`，报错原文见下）。
   - legacy（`2025-11-25`）：`server/discover` 回 `-32601` → 客户端回落
     `initialize` → `notifications/initialized` → `tools/list`。
   - 最省事的做法：**只实现 legacy 路径**（对 `server/discover` 回 `-32601`），已验证 dsh 能装配；
     想走 modern 就必须补齐上面三个字段。
4. **stdio 子进程环境被 scrub**：`@deepseek-ai/dsh-mcp-client/README.md:135` 的原话——
   父环境里名字匹配 `/KEY|PASSWORD|SECRET|TOKEN/i` 的变量和所有 `DSH_*` 都会被丢掉，
   只保留显式写在配置 `env:` 里的。**所以如果服务端自己要调模型，凭据必须在 profile 的
   `env:` 里显式传**（例如 `DEEPSEEK_API_KEY: !!js process.env.DEEPSEEK_API_KEY`），
   不能指望继承。本轮故意没加这行（避免未设变量时传空串），R2 需要时再加。
5. 工具名会加前缀：服务端暴露 `solve_equilibria`，模型看到的是 `mcp__shangyi__solve_equilibria`。
   persona 里写的名字要与之一致。
6. 服务端返回的 `instructions` 会作为**字面文本**并入 system prompt
   （`dsh-mcp-client/README.md:187` 的 "Server instructions" 段）。
   本轮实测：假服务端的 instructions 出现在 `system/message` 里。

R2 实现后，把冒烟里的 `fake_mcp.py` 换成真二进制即可——`smoke.sh` 的 d1/d2 两段断言可以原样复用
（`SHANGYI_BIN` 指到真 `shangyi`）。

## 8. 回滚

```bash
rm -rf "${DSH_HOME:-$HOME/.dsh}/profiles/shangyi"
```

- 只删 profile，不动 dsh 本体、不动 `~/.dsh` 里别的 profile、不动会话（会话在
  `$DSH_HOME/sessions`，要一起删就自己决定）。
- 回滚后 `dsh --profile shangyi` 会报 profile 不存在；`dsh --profile headless` 照旧可用。
- 仓库侧回滚就是删 `dsh/` 与 `tools/dsh/` 两个目录（本轮新增，没有改任何既有文件）。

## 9. 已知坑

1. **`--dump-config` 不求值 `!!js`**：它只打印合成树，表达式按原文打印
   （`@deepseek-ai/dsh/lib/types/dump-config.d.ts:1-6`）。所以 dump 里看到的是
   `command: !!js process.env.SHANGYI_BIN ?? "shangyi"`，不是求值后的 `shangyi`——
   想知道求值结果得看真实启动（冒烟 d1 就是这么做的）。
2. **`--help` 文案写死 `--profile headless`**：那是 `@deepseek-ai/dsh-headless` 自己的
   帮助文本，跟 profile 名无关，别以为装错了。
3. **`cordis.yml` 每次启动被重写**：内容恒为 `[]` + 固定注释（dsh 的 Loader 需要真实的
   include 根）。所以仓库里那份是逐字节复制 dsh 生成的文本，改它没有意义——要改行为改
   `cordis.patch.yml`。
4. **patch 是整行替换 config**：想在 profile 里改 `mcp-shangyi` 的一个字段（比如
   `failOnStartupError`），必须把整份 config 重述一遍，否则其余字段会被 schema 默认值接管。
   冒烟里的 `overlay-failfast.yml` 就是这么写的。
5. **`--dump-config` 也会写文件**：它会重写 profile 的 `cordis.yml`（`prepareProfile` 的行为），
   所以冒烟一律用 `/tmp` 下的 `DSH_HOME`。
6. **`failOnStartupError: true` 不会中止运行**：它只让那一行"不激活"并打印告警/堆栈，
   dsh 的 required 列表是全局写死的，patch 条目没有 `required` 字段。别指望它当"启动闸门"用；
   启动闸门只有安装期（`install-profile.sh` 退出码 3）和你自己的部署脚本。
7. **严格默认下缺二进制的工具面是 0 个**，不是 3 个：`mcp-resources` 只在 client 行激活时
   才登记 `list_mcp_resources` 等三个工具，所以连"资源浏览"都没有。这比 lenient 的
   "3 个工具但一调就报连接错"更难被误读成"引擎没数据"。

# `dsh-plugin-shangyi` —— 商奕领域卡片的 DSH 客户端插件

在 **DSH Web GUI** 的会话头部加一个「商奕」按钮，点开是右侧抽屉，展示**真实案例数据**：

| 卡片 | 数据来源 |
|---|---|
| **案例概况** | `case.jsonl` 的事件投影：案例 id、模式/剧本、阶段链、确认点、轮次、工具调用数、token、事件数 |
| **支付矩阵** | 最近一次成功的 `gamespec_build` 入参（players/strategies/payoffs）；支付值保持引擎的 ⟨参数⟩ 引用形状 |
| **均衡与选择** | 最近一次成功的 `solve_equilibria` 输出（均衡 id / kind / 支付）+ `select_fuse` 输出（概率分布、校准状态、完整度） |
| **工具失败 / 闸门拒绝** | 最近 5 条非 ok 的工具结局原文（如参数校验失败、案例已存在等） |

前端**只展示**，不重算、不编造数字；接口不可用时报错并**不**退化成样例（宁可不显示，也不把样例讲成真结果）。

## 状态（2026-11-10）

- ✅ 插件在真实 DSH Web GUI（0.2.1-rc.2）加载并渲染；Playwright 实拍见
  [`research/ui/2026-11-10-dsh-plugin-mvp/`](../../../research/ui/2026-11-10-dsh-plugin-mvp/)。
- ✅ 数据接线完成：Host 半注册只读路由，前端拉取真实案例日志投影；**独立对拍 15/15 通过**
  （[`scripts/check-projection.py`](scripts/check-projection.py)：不经 Node 投影代码，直接从 `case.jsonl` 重算）。
- ⚠️ 不是 per-session 绑定：抽屉里用「案例」下拉选择（默认 = 最新**含博弈结构**的案例）；
  DSH 会话与商奕案例的映射留到接 `sessionQuery` 时做。

## 路由（Host 半，经 DSH 浏览器鉴权）

| 路由 | 说明 |
|---|---|
| `GET /api/shangyi/cards?case=latest-cards\|latest\|<case_id>` | 一个案例的卡片数据；`latest-cards` = 最新且含博弈结构的案例（**向后兼容，字段未删**） |
| `GET /api/shangyi/cases` | 最近 30 个非空案例的摘要（id / 时间 / 模式 / 剧本 / 是否有矩阵·均衡·选择 / **phase·status**），供案例列表与工作台用 |
| `GET /api/shangyi/kb` | 知识库索引：`{ok, docs:[{slug,title,group,path,id,kind,summary,tags,heading}]}`（35 篇） |
| `GET /api/shangyi/kb/doc?slug=<slug>` | 单篇正文：`{ok, slug, title, group, markdown}`；越界 slug → 400，不存在 → 404 |

读取位置：案例读 `$SHANGYI_HOME`（缺省 `~/.shangyi`）下的 `cases/<日期>/<case_id>/case.jsonl`；
知识库读**仓库内** `shangyi/kb/**/*.md`（仓库根按 `SHANGYI_ROOT` → 本文件位置上溯 4 层 → cwd 的顺序解析）。**只读，无写操作，无 POST。**

### `phase` / `status` 的判据（写在 `lib/index.js` 的 `inferProgress()` 注释里）

| 字段 | 判据 | 来源 |
|---|---|---|
| `phase` | 最后一个 `phase_entered.to`；无该事件 → `null` | case.jsonl |
| `status=done` | `phase === 'archived'` | `phase_graph.rs` 的 `assert_well_formed()`：每条剧本路径都必须终止于 archived |
| `status=stuck` | ① 有 `checkpoint_opened` 在最后一次同 id 的 `checkpoint_answered` 之后（闸门等用户答复）；或 ② 最后一个 `tool_call_finished.outcome.status === 'failed'`（`blocked` 是引擎的正当拒绝，不算） | case.jsonl |
| `status=running` | 最后一次 `turn_started` 没有对应的 `turn_ended`。**口径**：日志分不清「正在跑」与「进程被杀」，该值应读作「未收尾」；需要区分时读 `openTurn`/`lastEvent` | case.jsonl |
| `status=null` | 以上都不成立 | — |

辅助字段（新增，供 UI 显示细状态）：`pendingCheckpoint` / `openTurn` / `lastEvent` / `lastToolOk`。

### 知识库字段口径

实测 35 篇是**一层平铺**、无 theory/method/concept 子目录；每篇开头有 front-matter
（`id` / `name` / `kind` / `summary` / `tags`），front-matter 之后的第一个 `#` 是**章节标题**。
故 `group` 取 front-matter 的 `kind`（有子目录时回退目录名），`title` 取 front-matter 的 `name`
（回退首个 `#` 标题、再回退文件名），首个 `#` 标题另存 `heading`。
> 与契约 §2.3「title=首个 # 标题」的差异：按实测那样取会得到「为什么这一步不能交给模型自由发挥」这类章节标题，故改用文件自声明的 `name`；契约方（Lead）若要改回，只需调 `lib/index.js` 里一行优先级。

## 结构

```
dsh-plugin-shangyi/
├── package.json                 # dsh.bundle.patch + dsh.client { platform: web }
├── cordis.patch.yml             # 插一行 Node 半
├── lib/
│   ├── index.js                 # Node 半：案例日志投影 + 两条只读路由（connection.fetch.register）
│   └── client.js                # 浏览器半：会话头部按钮 + 抽屉（手写 ModuleLoader bundle）
└── scripts/
    ├── check-projection.py       # 独立对拍脚本（Node 投影 vs 原始 JSONL）
    └── check-host-data.mjs       # 宿主数据面自检（kb 索引/正文边界、phase·status 双推导对拍、路由注册与返回码）
```

## 安装 / 运行（本机实测）

```bash
dsh shangyi-web --from-default-profile web
dsh plugin --profile shangyi-web add /绝对路径/shangyi/ui/dsh-plugin-shangyi
dsh --profile shangyi-web --no-open      # 读出 http://127.0.0.1:<port>/?token=…
```

打开 URL → 任一会话 → 头部「商奕」。改了插件代码后重启该实例即可（Host 半与浏览器 bundle 都重新加载）。

自检（不依赖 DSH 运行时、不联网）：

```bash
node scripts/check-host-data.mjs    # 7 项断言：kb / 越界 / 案例摘要对拍 / cards 兼容 / 路由
```

## 已知限制

- 只读 case log；**不用 DSH 会话状态**，因此不知道"你正在看的这个会话属于哪个商奕案例"。
- 支付矩阵最多渲染两名玩家的二维网格；第三名及以后只在格子内并列展示（引擎支持 N 人，前端首版只画 2 人轴）。
- 插件 API 属 developer preview（官方原话：core plugins and APIs will continue to evolve）；本版依赖
  `conversation.session.header.utilities` 槽位与 `connection.fetch.register`，升级 DSH 前需复核。
- 启动日志里 `schedule` 的 StorageError 与插件无关（0.2.1-rc.2 的既有告警）。

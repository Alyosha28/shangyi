# 商奕工作台 · 实施契约 v1

> **历史草案说明**：本文件为 M2 之前的工作台 v1 实施草案；当前正式架构与实现规范已被 [`docs/商奕-系统设计文档-v0.3.md`](../../docs/商奕-系统设计文档-v0.3.md) 以及 `shangyi/ui/dsh-plugin-shangyi` / `dsh-plugin-shangyi-workbench` 取代（现已实现 6 个工作台模块、`POST /api/shangyi/checkpoint` 受控关口写入与 `web_search` 接入）。

**形态一句话**：打开 DSH（商奕发行版）→ 侧栏多一个「商奕」面板入口 → 主区域是**博弈分析工作台**（案例、阶段、支付矩阵、均衡、选择），AI 对话仍是其中一个面板，不再是全部。

**验收（本轮）**：在验证实例里，点侧栏「商奕」能进入工作台；能选中一个真实案例并看到它的阶段链 / 支付矩阵 / 均衡分布；无案例时给出可执行空态；整个过程不动 DSH 官方组件、可一键卸载。

---

## 0. 已确认的技术事实（本机已核，来源：DSH 0.2.1-rc.2 运行时 + client bundle）

| 事实 | 证据 |
|---|---|
| `main` 是 keyed slot，当前只有 `conversation` 一个 key；注册新 key 即新增一个主面板 | client Slots.listSubTree |
| `sidebar.panellist` 的每条注册 = 一个面板入口，`id` 同时是 `main` 的 key，点击 `selectPanel(id)` | `dsh-client-ui-sidebar/lib/client.js` |
| 面板图标通过 `sidebar.panellist` 的组件渲染，props 含 `{ size, active }` | 同上 |
| 客户端插件 = 一个调用 `window.__ModuleLoader__.load({id, factory})` 的普通脚本；React 由 `require('react')` 提供 | 官方 decoration 模板 |
| token 覆盖用 `ctx.theme.overrideTokens(source, {token:{light,dark}})`；卸载自动还原 | `dsh-client-ui-theme/lib/client.js` |
| 品牌位是单槽：`sidebar.brand.mark` / `sidebar.brand.name` / `conversation.hero.brand.mark`；官方品牌插件行 id 是 `ui-brand-official` | 同上 |
| 宿主只读路由经 `ctx.inject(['connection']).fetch.register({path, methods, fetch})` 注册，走 DSH 浏览器鉴权 | 现有 `dsh-plugin-shangyi/lib/index.js` |

---

## 1. 目录与所有权（各 agent 只写自己那格）

```
shangyi/ui/
├── WORKBENCH-CONTRACT.md          # 本文件（Lead 所有）
├── dsh-plugin-shangyi/            # 领域卡片插件（已有）：宿主投影 + 案例选择器
│   └── Agent: host-data          #   只写这个目录
├── dsh-plugin-shangyi-shell/      # 视觉外壳（已有）：品牌位 + 107 token + 材料层
│   └── Agent: Lead                #   只写这个目录
├── dsh-plugin-shangyi-workbench/  # 工作台主面板（新建）
│   └── Agent: workbench-ui       #   只写这个目录
└── shangyi-icons/                 # 商奕图标集（新建，kubai 规范）
    └── Agent: icons              #   只写这个目录
```

`~/.dsh/profiles/shangyi-web/`（验证实例 profile）**只有 Lead 能改**：其他 agent 不改 profile、不 boot 实例、不碰端口。

---

## 2. 数据契约（host-data 负责实现，workbench-ui 负责消费）

所有响应 `cache-control: no-store`，形状带 `ok: true|false`。

### 2.1 `GET /api/shangyi/cases`（已有，扩展）
```json
{ "ok": true, "cases": [ {
  "caseId": "20261111-...",
  "mtime": "2026-11-11T02:00:00.000Z",
  "mode": "consult|research|cost|null",
  "events": 42,
  "hasMatrix": true, "hasSolve": true, "hasSelection": false,
  "phase": "solve",            // 最后一个 phase_entered.to
  "status": "running|waiting|done|stuck|null",  // 判据见下
  "pendingCheckpoint": "cp2|null", "openTurn": true, "lastEvent": "…", "lastToolOk": true
} ] }
```

**判据口径（来源：host-data 实现，已与 `shangyi-core/src/phase_graph.rs` 对拍）**
- `done` = `phase === 'archived'`（`assert_well_formed()` 要求每条剧本路径终止于 archived）
- `waiting` = 有 `checkpoint_opened` 晚于同 id 的 `checkpoint_answered`（正常等待用户核准/确认关口，工作台显示琥珀色「待确认」）
- `stuck` = 最后一个 `tool_call_finished.outcome.status === 'failed'`。
  `blocked` 是引擎正当拒绝（MCP 侧 web_search 未接线），**不算失败**
- `running` = 最后一个 `turn_started` 无对应 `turn_ended`。**口径注意**：日志分不清「正在跑」与「进程被杀」，这个值只读作「未收尾」；UI 要精确展示用 `openTurn`
- 都不成立 → `null`（不猜）

### 2.2 `GET /api/shangyi/cards?case=latest-cards|<caseId>`（已有，保持兼容）
返回 `{ok, meta:{case_id,mode,playbook,phases[],checkpoints[],turns,tools,tokens,events}, matrix|null, solve|null, selection|null}`。
**保持向后兼容**：现有抽屉客户端依赖它。

### 2.3 `GET /api/shangyi/kb`（新增）
```json
{ "ok": true, "docs": [ { "slug": "gamespec-build", "title": "如何把生意写成博弈（GameSpec 构建）", "group": "method", "heading": "…", "path": "shangyi/kb/gamespec-build.md" } ] }
```
来源：`shangyi/kb/**/*.md`（35 篇）。

**口径（Lead 已裁决，2026-11-11）**：
- `title` 取 **front-matter 的 `name`** → 首个 `#` → 文件名（实测 35 篇都是 front-matter + 章节标题，按「首个 #」取会得到章节标题，故改）
- `heading` 另存首个 `#`，供正文预览用
- `group` 取 **front-matter 的 `kind`**（`shangyi/kb` 是平铺目录，无 theory/method/concept 子目录；有子目录时回退目录名）
- 实测分布：theory 20 / method 12 / concept 3，与 `shangyi/README.md` 的「理论 20 / 方法论 12 / 概念 3」互证

### 2.4 `GET /api/shangyi/kb/doc?slug=<slug>`（新增）
```json
{ "ok": true, "slug": "...", "title": "…", "markdown": "…" }
```

**边界**：只读。不允许 POST；不允许读 `~/.shangyi` 之外的东西（kb 读的是仓库内 `shangyi/kb`）。

---

## 3. 工作台 UI 契约（workbench-ui）

### 3.1 注册
- 包名：`dsh-plugin-shangyi-workbench`
- `sidebar.panellist` 注册：`{ id: 'shangyi-workbench', order: 10, label: '商奕' }`，组件 props `{ size, active }`
- `main` 注册：`{ name: 'main', key: 'shangyi-workbench' }`（keyed slot，单面板）
- 图标：优先用 `shangyi-icons/dist/embedded.js` 提供的 `ShangYiIcons.mount(el, name, {accent})`；在图标未就绪前允许用内置 SVG 占位，但**不得**在最终版留占位。

### 3.2 布局（三栏，Operate 模式：密度优先，不做营销页）
```
┌──────────────┬────────────────────────────────────────────┬─────────────┐
│ 案例列表      │ 案例主视图                                  │ 检查器       │
│ · 搜索        │  标题行：case_id / mode / playbook / 时间    │ 确认点        │
│ · 案例卡片    │  阶段链：intel → … → done（当前阶段高亮）    │ 参数          │
│ · 空态提示    │  支付矩阵（只读，⟨参数⟩ 原样显示）            │ 工具调用摘要  │
│              │  均衡列表 + 选择分布条                       │ 知识引用      │
└──────────────┴────────────────────────────────────────────┴─────────────┘
```
- 中栏顶部 tab：`概览 / 矩阵 / 均衡 / 轨迹`（同一案例的切片，不要跳页）
- 右侧检查器在窄屏（< 1100px）折叠为抽屉按钮
- 数字一律来自接口，前端不重算；矩阵单元格保持 `⟨参数⟩` 引用形状
- 空态（无案例）：印章 + 一句「还没有案例」+ 两条可执行动作（打开对话面板 / 刷新）

### 3.3 视觉
- 全部用 `--dsw-alias-*` token 取色，不写死颜色（明暗自动跟随）
- 字体：界面用系统 sans；标题/数字可用 `font-variant-numeric: tabular-nums`
- 密度：行高 1.5、卡片圆角 8、间距 8 的倍数；不做渐变、阴影不超过 1 层
- 中文文案，英文只出现在 case_id / 工具名

### 3.4 脚手架
- `package.json`（`dsh.client.platform: "web"` + `immediately: true` + `inject` 里至少含 `@deepseek-ai/dsh-client-ui-conversation`、`@deepseek-ai/dsh-client-ui-sidebar`）
- `lib/index.js`：空 `apply()`
- `lib/client.js`：**产物**，由 `scripts/build.sh` 拼接生成（tokens/icons 的嵌入片段 + `src/*.js`），`node --check` 必须通过
- `cordis.patch.yml`：只做 `insert` 一行 `{ id: shangyi-workbench, name: dsh-plugin-shangyi-workbench }`（**不要**在补丁里 disable 官方行，避免和 shell 插件的补丁打架）

---

## 4. 图标契约（icons）

- 复用 kubai-icons 的 `src/engine.js`，不重写引擎；新图标按 `PROMPT.md` 追加
- 6 个图标（名字即中文，语义固定）：

| 名字 | 物体 | 悬停动作 | 用在哪 |
|---|---|---|---|
| 弈 | 棋盘（四分之一枰线 + 一枚子） | 落子 | 面板入口 / 工作台标题 |
| 算 | 算盘 | 珠子拨动 | 「矩阵 / 参数」tab |
| 判 | 天平 | 指针摆动 | 「均衡 / 选择」tab |
| 证 | 卷宗 + 印章 | 盖章 | 「轨迹 / 确认点」 |
| 知 | 书册 | 翻页 | 知识库 |
| 档 | 档案盒 | 抽出一半 | 案例列表 |

- 产物：
  - `src/engine.js`（复制，保留版权头）、`src/icons.js`（6 个新图标）、`src/app.js` + `index.html`（预览页）
  - `scripts/check.mjs`（复制）→ 跑 `node scripts/check.mjs --only 弈,算,判,证,知,档`，截图在 `check-out/`
  - `dist/embedded.js`：可被直接拼接进客户端 bundle 的经典脚本片段，暴露
    ```js
    var ShangYiIcons = {
      names: ['弈','算','判','证','知','档'],
      mount: function (el, name, opts) { /* 创建并返回实例；el 里插入 svg */ },
      setAccent: function (hex) { /* 可选 */ }
    };
    ```
    不得污染其它全局变量；`mount` 幂等（重复调用替换内容）。
- 色板：主色 `#1F6B5C`（宣纸·墨玉）；`dist/embedded.js` 默认用它，但接受 `opts.accent` 覆盖

---

## 5. 交付与验收

1. 各 agent 完成后给 Lead 一条消息：改了哪些文件、怎么验证、还剩什么坑
2. Lead 负责：装进 `shangyi-web` 验证 profile → boot → 截图 → `impeccable detect` → 出结论
3. 验收三条：
   - 点侧栏「商奕」进入工作台，中栏出现真实案例数据（不是 mock）
   - 明暗切换后配色跟随，无硬编码颜色残留
   - 卸载工作台插件后，DSH 恢复原样（对话面板仍可用）

## 6. 纪律

- 数字只来自确定性代码（商奕铁律 1）：前端不编造、不重算、不"估算"
- 不改 DSH 官方组件、不读别的插件的 DOM
- 每个 agent 只写自己目录；需要跨目录改动先在消息里说明
- 每次 UI 改动收尾跑一次 `~/.dsh/skills/impeccable/scripts/impeccable detect --json <改动的目录>`

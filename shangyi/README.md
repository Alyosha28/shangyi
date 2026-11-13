# 商奕（ShangYi）引擎层

本目录是商奕**自有 Rust 引擎**（设计文档 §6 的"平面 B"）。内核**无外部 harness 基座**：`vendor/codex/` 已于 2026-11-10 整树移除，改为 DeepSeek Harness（dsh）以**进程边界**提供 agent 运行时、本目录的引擎经 MCP 暴露领域工具——见 [`docs/商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md`](../docs/商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md)。

## 目录结构

```
shangyi/
├── README.md                          # 本文件
├── crates/
│   ├── shangyi-gamespec/              # L0 领域基座：GameSpec 数据结构与校验
│   ├── shangyi-core/                  # L3 接线层：会话阶段机 + 工具调度 + 留档
│   └── shangyi-mcp/                   # MCP 服务端：38 个领域工具暴露给 dsh 运行时
│   └── …                              # 其余 crate（solver/selection/…）
├── kb/                                # 知识库（35 篇 markdown：理论 20 / 方法论 12 / 概念 3，唯一真相源）
├── playbooks/                         # 剧本 YAML
└── sidecar/
    ├── data_ingest.py                 # CSV 数据导入侧车（stdin/stdout JSON 契约）
    ├── web_console.py                 # Web 控制台 sidecar（见 sidecar/web/README.md）
    └── web/                           # 商奕咨询前端（对话、使用引导与分析依据）
```

## Web 控制台（本地交互前端）

```bash
python3 shangyi/sidecar/web_console.py     # 打开 http://127.0.0.1:8787
```

新咨询从自由描述开始；“使用引导”提供不启动模型的虚构练习。只查资料或计算费用的任务直接交付结果与缺口，交付后可以继续补充；需要比较做法的咨询保留原有确认与求解流程。记录页保存各次结果、原始问题、来源与计算依据。“比比不同做法”继续使用本地确定性求解器。
架构与边界见 [`sidecar/web/README.md`](sidecar/web/README.md)。

### 搜索后端

配置 `BOCHA_API_KEY`（或写入 `~/.shangyi-secrets/bocha.env` 的单行 `BOCHA_API_KEY=<key>`）
后启用博查 Web Search——**当前唯一的检索后端**。

自托管 SearXNG 已于 2026-10 删除（`SHANGYI_SEARXNG_URL` 不再被读取）：它要用户
自己维护实例，而实测的检索质量与可用性都不值得那条运维成本；`tools/searxng-up.sh`
一并删除。缺后端时如实报「未取得」，不退回编造。

融合层仍在：多后端一律**都查询后按名次融合**（RRF 合并、URL 与标题去重、再按
查询意图重排：官方域名、年份相符、查询词覆盖度），返回前 8 条；接入第二个后端
（Tavily/Brave 等）时无需改重排逻辑。逐条对比见
[`research/qa/2026-10-02-guided-session/fusion-benchmark.md`](../research/qa/2026-10-02-guided-session/fusion-benchmark.md)。

后端按查询意图补齐参数：`site:域名` 转为博查 `include`，`-site:域名` 转为
`exclude`，明确的时效词（本周/近期/今年…）映射为博查 `freshness`。**年份不会
触发时效收紧**：查 2025 年的活动时加 `oneYear` 会把真正相关的旧原文排除掉。

工具输出里每条结果带 `rank_score`（0–100，仅表示检索排序）与 `rank_signals`
（如"官方域名""年份相符""多后端共识"），`provider_date_unverified` 是供应商
日期元数据、**未经原文核对**。搜索摘要只是候选线索，报告事实仍须取得并核对
原始网页。该本地 secrets 目录应设为 `0700`，密钥文件应设为 `0600`，不要把密钥
写进仓库、日志或咨询内容。首页健康检查只报**凭据是否已配置**，不做实时探测：
博查按次计费，把探测挂到页面加载上等于把成本挂到浏览行为；因此“已配置”不等于
当前查询已验证——可用与否由实际查询回答。融合不会增加付费调用次数：每条查询
仍只调用博查一次。

## 构建

项目根即完整 Rust workspace（`members = ["shangyi/crates/*"]`）。**不要**在 `shangyi/` 内直接 `cargo build`——有一条已实测的构建约束，包装器已统一处理：

```bash
# 从项目根调用（推荐）
tools/loop/build.sh build        # 包装器：设好 CARGO_TARGET_DIR 后转发给 cargo
cargo build                      # 等价，但需自行设置 CARGO_TARGET_DIR（见下）
```

### 约束：`CARGO_TARGET_DIR` 必须在本机盘，**不能**落在挂载盘

项目位于外接挂载盘，其驱动会为每个文件生成 AppleDouble `._*` 伴随文件。实测（2026-11-10）：把 target 放在挂载盘（`.cargo-target/`）后立刻出现 `._debug`、`._CACHEDIR.TAG` 等伴生文件；历史上一份 **4,532** 个伴生文件造成过**随机且具误导性的**编译错误（如假报 `zeroize_derive` 缺失）。

`tools/loop/build.sh` 默认 `CARGO_TARGET_DIR=/tmp/shangyi-target`；手工调用需自行设置。

> **两条旧约束已随 codex 移除而失效**（2026-11-10）：15 个 `rama-*` 的整组钉版、以及 `[patch.crates-io]` 指向 `openai-oss-forks` 的 crossterm/tungstenite fork。它们的存在理由只有一个——让 `codex-core` 的依赖图可解析。移除 codex 后两者都不再出现在根 `Cargo.toml`（依据：`docs/商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md` §2）。

## 边界规则

内核**不依赖任何外部 harness 源码**。这条过去是口径，现在是可执行的：

- **依赖方向**：`shangyi-*` 之间只走 crate 公开 API；`shangyi-gamespec` 是 L0，不依赖上层，以保证领域模型可独立测试与演进
- **外部能力**：与 DeepSeek Harness 的耦合只发生在**进程边界**（MCP 工具桥，见 [`dsh/README.md`](../dsh/README.md)），不是源码依赖
- **护栏**：`tools/loop/check-no-harness.py` 在根 `Cargo.toml`／`Cargo.lock`／各 crate `Cargo.toml` 中命中 `codex` / `rama` / `openai-oss-forks` 即非零退出（替代旧的 `vendor_harness_is_linkable` 架构测试）

### 旧 `exclude = ["vendor"]` 的说明（已失效，保留为历史）

`vendor/` 曾在商奕 workspace 目录树**之内**，若不 `exclude`，cargo 会把 vendor 内 crate 视作本 workspace 成员，导致 `dep = { workspace = true }` 式的依赖来商奕根 `Cargo.toml` 查找并报 `workspace.dependencies was not defined`。vendor 移除后该字段与原因一并消失；**不要再把任何上游源码镜像放进 workspace 目录树**。


## ⚠️ 序列化规则：含数值字段的枚举必须用 adjacent 或 external tagging

**这是一条硬约束，且现在有牙**：违反它会在 `cargo test` 下真的失败——但只在**测试构建**里失败，因为把该 feature 显式武装到测试图的正是我们自己的 dev-dependencies。

### 机制（为什么 internally-tagged 会炸）

`serde_json` 的 `arbitrary_precision` 开启时，JSON 数字在 serde 内部被表示为 **map 形态**；而
`#[serde(tag = "...")]`（internally tagged）与 `#[serde(untagged)]` 的枚举要经 serde 的 `Content`
缓冲路径 ⇒ **缓冲后的数字无法反序列化为 `f64`**：

```
error: invalid type: map, expected f64
```

历史上这个 feature 是被上游基座里的某个 crate 带进全图的（**R1 已移除该基座**）；R1 之后正式依赖图里**没有**它
（`cargo tree --workspace -e features,no-dev | grep -c arbitrary_precision` = 0），而测试图里**有**——
因为 `shangyi-gamespec` 与 `shangyi-intel` 各自在 `[dev-dependencies]` 里显式启用了它，用来给这条规则保留回归覆盖。

两道判据都由 `tools/loop/check-features.py` 双向守：

| 方向 | 期望 | 含义 |
|---|---|---|
| 正式图（`-e features,no-dev`） | **0 命中** | 没人偷偷改变 release 二进制的数字表示语义 |
| 测试图（`-e features`） | **必须命中（当前 2 处）** | 护栏的 dev-deps 掉了 → 规则退回"靠人遵守"，脚本立即失败 |

### 曾踩到的失败模式（保留为教训）

| 测试方式 | R1 之前 | 现在 |
|---|---|---|
| `cargo test -p shangyi-intel --test profiles` | ✅ 通过（不在 workspace 图中，无该 feature） | ✅ 通过，**且在 feature 下**（该 crate 的 dev-deps 已武装） |
| `cargo test --test profiles`（workspace-wide） | ❌ 失败 | ✅ 通过 |

**故验证仍应用 `cargo test` 全量跑，不只 `-p`。** 「全量跑」这条纪律与依赖图内容无关，继续保留。

### 规则

```rust
// ✅ 正确：adjacent tagging（保留显式 tag，且免疫）
#[serde(tag = "dist", content = "params", rename_all = "snake_case")]
pub enum Dist { Normal { mu: f64, sigma: f64 }, ... }

// ✅ 也可以：externally tagged（默认，无 tag 属性）
#[serde(rename_all = "snake_case")]
pub enum Expr { Literal(f64), Ref(String) }

// ❌ 禁止（含数值字段时）
#[serde(tag = "dist", rename_all = "snake_case")]
pub enum Dist { ... }          // internally tagged

#[serde(untagged)]
pub enum Expr { Literal(f64), ... }   // untagged
```

### 已验证的受影响类型（均已修正）

| crate | 类型 | 修正方式 |
|---|---|---|
| `shangyi-gamespec` | `Dist`、`Dynamics` | 改 `tag` + `content` |
| `shangyi-gamespec` | `Expr`、`PayoffForm` | 由 untagged / internally tagged 改 adjacent |
| `shangyi-intel` | `RoleSpecific` | 改 `tag` + `content` |
| `shangyi-swarm` | `Action` | 改 `tag` + `content` |

### 回归护栏

`shangyi/crates/shangyi-intel/tests/profiles.rs::profile_serializes_round_trip`
在 workspace-wide 构建下运行，若 `RoleSpecific` 退回 internally-tagged 即失败。
新增含数值字段的枚举时，应仿此补一条 round-trip 测试。

## 已建立的能力

| crate | 内容 | 测试 |
|---|---|---|
| `shangyi-gamespec` | GameSpec 数据结构（参与者/价值网角色/参数带来源与分布/信息结构/动态形式/偏好）；`validate()` 把**铁律 2**（未确认假设不得进入求解）落为类型级约束 | **10 passed**（`cargo test -p shangyi-gamespec --lib`，2026-11-10 实测） |
| `shangyi-core` | 八阶段会话状态机（设计文档 §3.1）；阶段推进**必须交付产物**（工程不变量②）；checkpoint 可回退至任意更早阶段 | **171 passed + 3 环境性失败**（`cargo test -p shangyi-core --lib`，2026-11-10 实测；3 条为 `sandbox::live_tests`，需本机允许 `sandbox-exec`，本会话外层沙箱禁止嵌套 seatbelt） |
| `shangyi-mcp` | MCP（JSON-RPC 2.0 over stdio）服务端：`tools/list` 暴露 38 个领域工具、`tools/call` 走 `check_call` 阶段白名单 → `DomainExecutor` → 产物登记 → 阶段推进；checkpoint/ask_user 以结构化 JSON 交回调用方 | **21 passed**（`cargo test -p shangyi-mcp`）+ `shangyi mcp-serve` 真二进制 e2e 4 passed |

其中 `tools/loop/check-no-harness.py` 是替代旧 `vendor_harness_is_linkable` 的**架构验证**：
它存在即证明"内核不再依赖外部 harness 源码"这条边界能被 CI 拒绝违规，而不只是写在文档里。

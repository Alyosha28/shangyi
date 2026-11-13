# 商奕 Agent Harness 设计文档

> ## ⚠️ 已被取代（2026-11-10）
>
> 本文 §3.1 的 **D-A2**（harness 基座 = Codex fork-and-trim）与 **D-A3**（不采用 dsh 运行时）已被
> [`商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md`](商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md) 取代：
> **内核改为「DeepSeek Harness 作 agent 运行时段 + 商奕 Rust 引擎经 MCP 暴露」，`vendor/codex` 已整树移除。**
>
> 本文其余章节**保留为历史设计记录**，不逐条重写（全文 254 处提到 codex）。阅读时按以下对照：
>
> | 本文的表述 | 现状 |
> |---|---|
> | 平面 A = `vendor/codex`，商奕 crate 只能依赖其 public 面 | 平面 A 已无源码基座；外部耦合只发生在 MCP 进程边界 |
> | §4 的 Apache-2.0/OpenAI 归属义务、NOTICE 模板、`vendor/patches` 登记 | 随 codex 移除而**终止**；现行义务是对 dsh 的 MIT 署名（见变更记录 §2/§6） |
> | §6 的 workspace 树、§10 的模型接入（复用 `codex-api` 等） | 均以变更记录为准；实际实现早于本文（自研 `shangyi-llm`，1551 行） |
>
> 仍**完全有效**的部分：§2 设计原则（两条铁律）、§5.2 的领域步骤与工具语义、§7 的 agent 循环不变量
> （诚实终止/阶段闸门/事件留档）、§8 的事件日志语义、§9 的工具可信度分级、§11 的沙箱与检查点语义、§13 的剧本分层。

**版本**：v0.1（Rust Harness 设计）
**日期**：2026-09-17
**状态**：待评审 —— 与《商奕-系统设计文档-v0.2》配套；第 17 章为待拍板决策清单；**§3.1 的 D-A2/D-A3 已于 2026-11-10 被取代（见文首横幅）**
**架构主体语言**：Rust（edition 2024 / MSRV 1.95）
**上游依据（历史）**：OpenAI Codex（Apache-2.0，© 2025 OpenAI）——已移除 · DeepSeek Harness（MIT，© 2026 DeepSeek）——现为运行时段

---

## 导读

本文档回答一个问题：**商奕的 agent 运行时怎么写。**

它既不是产品设计文档（那是《商奕-系统设计文档》，本文简称"v0.1/v0.2 文档"），也不是商业计划书，而是夹在两者之间的**工程实现设计**：把 v0.1 文档里的博弈引擎、三模式、三检查点、子智能体群，落到一套具体的 Rust 运行时上。

三句话概括本文的立场：

1. **商奕的 agent 架构主体用 Rust 写。** 领域逻辑（博弈引擎、均衡选择、情报、案例库、Swarm）全部是自研 Rust crate。
2. **运行时的"脏活"不重写，复用上游开源实现并注明来源。** 事件日志、工具注册表、沙箱、审批、多智能体编排这些与领域无关的重型基础设施，直接复用或修改 **OpenAI Codex**（Apache-2.0）的源码；**DeepSeek Harness**（MIT）是 TypeScript，无法共用源码，因此只**借鉴其设计模式**并同样注明来源。本文对每个构件都标注了 `直接复用` / `修改后复用` / `仅借鉴设计` / `自研` 四类处置，并给出上游与许可。
3. **求解器弃用 Gambit，改为 Rust 自研。** `python-gambit` 是 GPL-2.0-or-later 且 Python 绑定无独立许可，对闭源商业产品是实质合规风险。Rust 生态没有现成的通用纳什均衡库，因此这部分必须自研 —— 这是本项目**最大的工期与正确性风险**，第 16、17 章按此排期。

### 五条最承重的结论

| # | 结论 | 位置 |
|---|---|---|
| 1 | Codex 基座**只支持 Responses API**，`wire_api = "chat"` 被硬拒；GLM/DeepSeek 主推 Chat Completions，**必须新增 Chat 适配层**，否则模型接不进来 | §3.5、§10.2 |
| 2 | `codex-core` 的工具注册表与任务驱动是 `pub(crate)`，**加原生领域工具必须 fork**；且 `core` 依赖约 60 个内部 crate，因此要以 workspace 形式 vendor（而非当库引用） | §3.3、§6.2 |
| 3 | **Gambit 必须退出依赖树**；求解改自研 Rust，`nashpy`（MIT）只在 CI 做差分验证 | §3.6、§5.3（v0.2） |
| 4 | 核心数值改由**确定性 Rust** 产生，Python 降为绘图/CI 侧车 —— 这**强化**而非削弱"LLM 不算数"铁律 | §2.1、§3.7 |
| 5 | 沙箱必须"**要么强制，要么拒绝**"：跑 LLM 生成的 Python 一律独立短生命周期进程 + 无网络命名空间，且无应答者时审批 fail-closed | §11.1、§11.3、§11.8 |

### 阅读路径

- 只关心**选型与合规** → §3 技术选型决策、§4 来源与许可
- 只关心**哪些代码是别人的** → §6.2 vendor 处置表、§9.1、§10.1、§11、§12.1
- 只关心**产品语义如何落到运行时** → §5.3 术语映射、§7.3 状态机、§11.7 检查点、§13 剧本
- 评审关注**与旧稿的差异** → §1.6 差异登记（R1–R19）、第 17 章

### 依据的工作底稿

本文的技术事实来自三份研究简报，存于 `research/harness/`，索引见**附录 B**。简报中每条事实均标注了是"读取所得"还是 `[推断]`；本文沿用该约定。

---

## 术语与标注约定

**处置标签**（每个构件级标注，非文档级）

| 标签 | 含义 |
|---|---|
| `直接复用` | 取上游源码原样使用，不改动；只需保留许可与归属声明 |
| `修改后复用` | 取上游源码并改动；须按 Apache-2.0 §4(b) 在被改文件附显著变更声明 |
| `仅借鉴设计` | 只采纳设计思路，代码自写；须在文档中注明思想来源 |
| `自研` | 商奕自有设计，不依赖上游对应构件 |

**事实标注**

- 未标注者为**读取上游源码/元数据所得**。
- `[推断]` 为推理结论，未直接读到。
- `[未取得]` 为调研中未能确证、需在实现阶段核实的事项；汇总于附录 A。

**简报编号**：`GB 01` = `research/harness/01-codex-rust-harness.md`；`GB 02` = `02-dsh-patterns-and-attribution.md`；`GB 03` = `03-rust-solvers-and-licensing.md`。

**声明**：本文档的全部许可与合规分析均为工程调研结论，**不构成法律意见**。涉及商业发布、融资尽调或开源义务的具体判断，请咨询律师。

---

## 1. 文档定位与范围

### 1.1 本文是什么

本文是《商奕 Agent Harness 设计文档 v0.1》的前半部分（第 1–8 章），回答一个此前悬空的问题：**商奕的 agent 运行时（harness）用什么、从哪里来、怎么改、怎么署名。**

《商奕-系统设计文档-v0.1》（下称 **v0.1 文档**）定义的是**领域设计**——博弈建模、求解器层、均衡选择引擎、Swarm、情报与画像、报告四层结构。它假定 agent 循环、工具注册、会话持久化、沙箱与审批是"已有的基础设施"，只用第 3 章和第 4 章各一页带过。本文补的就是这一页的下面：

| 维度 | v0.1 文档 | 本文（harness 文档） |
|---|---|---|
| 回答什么 | 商奕**算**什么、交付什么 | 商奕**跑**在什么上、如何被驱动、如何留档 |
| 粒度 | 领域算法与数据结构 | crate 边界、wire 协议、事件日志、许可义务 |
| 语言栈假设 | Python 3.11+ 单体 | **Rust 为主体的 workspace**（见 §3） |
| 交付物 | 咨询报告 | 报告的**可审计性**所依赖的运行时证据链 |

本文**不**重复领域设计。凡是 v0.1 文档已经定义的概念（GameSpec、价值网、五个选择器、三 checkpoint、四层报告），本文只做**接线**与**术语映射**（§5.3），不改其语义。

### 1.2 与 v0.1 文档、项目计划书的关系

三份文档的关系是单向的、有优先级的：

```
商奕项目计划书.md            （立意、比赛叙事、里程碑）
        │  约束
        ▼
商奕-系统设计文档-v0.1.md    （领域设计：算不算得对、交付得对不对）   ← 语义权威
        │  基础设施缺口
        ▼
本文（harness 设计文档）     （运行时设计：跑不跑得起来、留不留得下证据）← 工程权威
```

规则：

1. **语义冲突以 v0.1 文档为准**，本文必须改写自己；
2. **工程冲突以本文为准**，但必须**显式登记为 v0.2 修订项**（§1.5 表），不允许静默改写；
3. 本文修改的技术选型（Rust、求解器自研、Python 降级）**会反向要求 v0.1 文档修订**——这些修订项在 §3.8 汇总，供 v0.2 文档一次性吸收。

### 1.3 范围

**In scope（本文负责）**

| 范围 | 章节 |
|---|---|
| 设计原则与工程不变量 | §2 |
| 语言/基座/wire/求解器/Python 的技术选型决策（ADR） | §3 |
| 来源署名、许可合规、商标、依赖白名单、`NOTICE` | §4 |
| 三层平面架构、术语映射、边界规则 | §5 |
| Rust workspace 目录树、crate 清单、依赖分层、patch 策略 | §6 |
| Agent 核心循环：turn/step、事件词汇、会话状态机、钩子、steering、checkpoint | §7 |
| 会话事件日志、持久化、投影、归档映射 | §8 |

**Out of scope（本文不负责）**

| 不做 | 归属 |
|---|---|
| 均衡选择器的理论与算法规格 | v0.1 文档 §5.4（语义权威） |
| GameSpec 的领域字段含义 | v0.1 文档 §5.2 |
| 报告四层内容、图表清单 | v0.1 文档 §9 |
| Swarm 博弈论职责与 feedback 环语义 | v0.1 文档 §5.7 |
| 第 9–15 章（界面与交付、测试与 CI、部署、风险、路线图等） | 本系列后半部分 |
| 代码实现、具体 crate 版本锁定 | W1 起的实现工作 |

### 1.4 依据的工程简报

本文的全部事实性陈述来自三份本地研究简报（下称 GB 01/02/03），并以此编号引用：

| 编号 | 文件 | 提供的事实 |
|---|---|---|
| **GB 01** | `research/harness/01-codex-rust-harness.md` | Codex（Apache-2.0, Rust）架构、workspace 成员、可见性、工具系统、wire 限制、rollout、沙箱审批、hooks/plugins/多智能体、复用地图 |
| **GB 02** | `research/harness/02-dsh-patterns-and-attribution.md` | DeepSeek Harness（MIT, TypeScript）的 14 个可移植设计模式、两条贯穿不变量、署名事实、成熟度 |
| **GB 03** | `research/harness/03-rust-solvers-and-licensing.md` | Rust 博弈求解生态现状、Python 求解器许可风险、Gambit 的 GPL 分析、数值栈 crate 清单 |

引用纪律：**未标注来源的判断一律标 `[推断]`**；GB 中标注为 `[未取得]`/`[不确定]` 的事实，本文同样保留不确定性标记，不得升级为断言。

### 1.5 处置标签读法

本文对每一个**被复用的外部构件或设计**都打一个且只有一个标签：

| 标签 | 含义 | 附加义务 |
|---|---|---|
| `直接复用` | 原样使用，不改源码 | 保留 `LICENSE`/`NOTICE`，无 §4(b) 变更声明义务 |
| `修改后复用` | 使用源码并做最小修改 | 额外：**每个被改文件附显著变更声明**（Apache-2.0 §4(b)），登记 `vendor/patches/` 编号 |
| `仅借鉴设计` | 只取设计模式，**不复制一行源码** | 署名（学术/工程礼貌 + MIT 要求的"substantial portions"边界外仍主动声明），无源码分发义务 |
| `自研` | 商奕自己实现 | 无第三方义务 |

> 标签的粒度是**构件级**，不是文档级。同一个上游（Codex）可以同时贡献 `直接复用`、`修改后复用`、`仅借鉴设计` 三种构件（§6.2 逐 crate 标注）。

### 1.6 相对 v0.1 文档的差异登记（v0.2 修订项）

以下差异是**本文有意引入**的，逐条登记，v0.2 文档需吸收：

| # | v0.1 原文位置 | v0.1 说法 | 本文结论 | 性质 |
|---|---|---|---|---|
| R1 | §1.5 铁律 1 | "所有数值计算（均衡求解、仿真、统计、绘图）在 **Python sandbox** 中执行" | 核心数值改为**确定性 Rust 原生**；Python 降为 sidecar（绘图 + CI 差分）。铁律 1 的**结论不变**（"数值必须来自代码"），但**执行载体改变** | 修订（强化） |
| R2 | §5.3 求解器层 | `nashpy` + `python-gambit` | **移除 Gambit**（GPL-2.0-or-later，无绑定例外）；`nashpy` **仅**用于 CI 差分测试 | 修订（合规强制） |
| R3 | §10 技术栈 | "Python 3.11+" | "Rust 核心（edition 2024 / MSRV 1.95）+ 可选 Python sidecar" | 修订 |
| R4 | §10 项目结构 | `shangyi-agent/` 为 Python 包树 | Rust workspace（§6.1），Python 树仅保留 `sidecar/` | 修订 |
| R5 | §3.1 会话状态机 | `class SessionState(str, Enum)`（Python） | 同**语义**、同**八个命名**，但为 Rust `enum SessionPhase`（§7.3） | 工程落地 |
| R6 | §3.1 "状态与全部中间产物持久化到本地 case 目录" | 未定义格式 | 具体化为事件溯源 JSONL + SQLite 索引 + 产物目录（§8.3） | 具体化 |
| R7 | §3.3 LLM 抽象 | "OpenAI 兼容客户端抽象（base_url + api_key 可配）……任意兼容端点切换" | **在 Codex 基座上不成立**：基座仅支持 Responses API，`wire_api = "chat"` 被硬拒。必须新增 Chat Completions 适配层（§3.5） | **纠正**（重要） |
| R8 | §4 工具层 | 工具表含 `sandbox_exec`（内置 gambit） | 工具层重写为领域工具集；`sandbox_exec` 保留但**仅**用于临时数值任务，求解不走沙箱（§3.7、GB 03 §3） | 修订 |
| R9 | §8.3 checkpoint 交互件 | 未定义与运行时审批的关系 | 三 checkpoint 落在**结构化用户提问 seam**（`request_user_input`）+ 领域审批语义（fail-closed）；沙箱审批是**另一条** seam（§7.6） | 具体化 |
| R10 | §12 风险表 | 无求解器许可风险 | 新增"求解器许可合规风险"与"Rust 博弈算法自研工期/正确性风险"（GB 03 §4 建议） | 补充 |
| R11 | — | — | 由 R2/R3 引入的新义务：`vendor/codex-*` 的 Apache-2.0 归属与商标改名（§4） | 新增 |
| R12 | §10（vendor 侧） | 未规定 vendor 层配置的**合并语义** | **已证实**（原 W1 第一个审计项，闭环）：Codex 层间合并 = **表递归深合并 + 非表（标量/数组）整体替换**。证据：`codex-rs/config/src/merge.rs` L75 入口、L94–95 与 L143 表分支递归、L149–152 非表分支整体替换；跨层测试 `config_loader_tests.rs` L854 `merges_managed_config_layer_on_top` 断言 `foo == 2`（标量被替换）且仅 base 写出的 `nested.extra == true` **存活** —— 存活即深合并的证据。**影响**：`[model_providers.*]` 与 `[mcp_servers.*]` 在基座层栈内走深合并，不能假设整行替换；须按 §13.4.1 例外清单**完整重述**配置。详见 `research/harness/04-codex-config-semantics.md` | 已证实差异 |
| R13 | §5.3 精炼过滤 | "颤抖手完美、序贯理性（**gambit 内建**）" | 自研 Rust（trembling-hand / sequential），与 R2 同源 | 修订（合规连带） |
| R14 | §5.4 均衡选择引擎 | 选择器为 Python 实现（`risk_dom`/`stochastic`/`learning`/`behavioral`/`focal_llm`/`persona_sim`） | 数值内核**全部 Rust 原生**；接口语义不变（§9.3） | 修订 |
| R15 | §9.2 图表清单 | "matplotlib，**sandbox 统一产出**" | **规格/渲染分离**：Rust 算数并产出图规格，Python 侧车只做渲染；图可重建，**数字不得由渲染侧产生**（§15.1） | 收紧 |
| R16 | §7.1 理论库 | Chroma + BGE 构成的**独立 RAG 资产** | 同一份 markdown（含 frontmatter）为**唯一真相源**，矢量索引为**派生投影**（可重建、可丢弃）；同一文件同时作 `SKILL.md` 方法论卡被跨工具消费；v0.1 四要素 schema 原样保留（§13.7、§13.8） | 新增互操作面 |
| R17 | §11.1 MVP | "learning 选择器与多轮 sub-agent 交互放 **v0.12**" | 视为 **v0.2**（"v0.12" 疑为笔误）；MVP 关闭该选择器（权重置 0），仅保留接口 | 需确认 |
| R18 | §5.7.1 三重过滤 | "激励相容（**预期收益 > 成本**）" | 改为"`P(收益 > 成本) ≥ 阈值`"，与 §5.6.2 达标概率口径一致，由 `monte_carlo` 实现（§12.5） | 收紧 |
| R19 | §6.1 合规红线 | 报告模板固定免责与措辞边界 | 结论保留（报告/提示层承载语义），并加 `ToolGuard`（单调 deny）；**沙箱只管能力，不管语义**（§11.9） | 澄清边界 |
| R20 | §3.1 / §7.3 | §3.1 八状态；§7.3 的模式表引入了 `consequence`/`discover` 两个**节点**（§7.2 的工具阶段白名单已把它们当阶段用） | `SessionPhase` 补全为**十变体**（八状态命名与语义逐字未动）；阶段**路径**按模式由 `PhaseGraph` 给出，不再用全局线性顺序表达三种边集 | 补全 |
| R21 | §7.4 `PhaseGateHook` | "checkpoint 未结算 → 硬拒绝" | **不可照实实现（自锁）**：`intel` 的 cp1 在 `intel` 内结算，若未打开即拒绝，则采集期无法调用任何工具，而死锁于"空提问件被 validate 拒"（详见 `.shangyi-loop/DECISIONS.md` 的 R21 条目）。判据改为：**挂着或被拒**则拒绝；"未打开"由指引注入提示，强制力落在"**离开**阶段必须经批准"（`gate_to_advance`） | 修正（实现驱动） |

补充说明：v0.1 §11.1 中"nashpy/gambit 枚举 + 精炼"的 MVP 表述与 §5.3/§10 的修订同源，归入 R2、R5，不另列条目。

> **两条铁律本身不被修订。** 本文 §2.1 只做重述。上表 R1–R19 与第 9–15 章口径一致，是 v0.2 文档必须吸收的完整清单。

---

## 2. 设计原则

本章定义贯穿全部后续章节的**五条原则**：两条继承自 v0.1 文档的铁律，三条为工程不变量（`[仅借鉴设计]` 自 GB 02 §C，语言与载体改为 Rust）。

### 2.1 继承的两条铁律（在 harness 层的重述）

**铁律 1 · LLM 不算数。**

> v0.1 原文：所有数值计算（均衡求解、仿真、统计、绘图）在 Python sandbox 中执行；LLM 只负责建模、调度、解读。报告中每个数字必须有代码/数据出处（provenance 溯源字段）。

harness 层的重述（**载体改变，结论不变**，见 R1）：

- 进入报告的**每一个数字**必须能沿事件日志回溯到一次**确定性工具调用**（`solve_equilibria`、`run_swarm`、`monte_carlo`、`sensitivity_scan`）及其输入产物摘要；
- LLM 生成的代码**不得**作为任何报告数字的产生者。LLM 生成的 Python（`sandbox_exec`）只允许用于**探索性**数值任务，且其结果**必须**被 Rust 求解器独立复算后才可进报告；
- 这条铁律在 harness 层的**落点**是 `ArtifactProduced` 事件携带的 `provenance: Vec<ProvenanceRef>`（§8.2）——provenance 不是报告生成步骤的装饰，而是**事件日志的字段**；
- 由此推出**不变量 ① 模型可见即可审计**（§2.2）：如果某个数字影响了模型的推理却被推断（inference）而非记录（logging），铁律 1 就已经被违反。

**铁律 2 · 关键假设必须过 checkpoint。**

> v0.1 原文：payoff 参数、对手估计等硬假设，须经用户确认或标注来源后方可进入模型。

harness 层的重述：

- checkpoint 不是 prompt 里的礼貌请求，而是**状态机的前置条件**：`model_build` 阶段在 `checkpoint②` 未结算前**拒绝**推进（§7.3 的 `PreStepHook` 硬闸门）；
- 每个假设条目必须携带 `source ∈ {user_confirmed, public_data, prior_borrowed, llm_inferred}`；`llm_inferred` 是**合法但高风险**的来源，必须在报告 ④ 层显式标注（v0.1 §6.3 的"先验 borrowed，非本对手实证"同理）；
- 由此推出**不变量 ② 能力 seam 失败即拒绝**（§2.3）：如果 checkpoint seam 不可用（无应答者、无审批服务），系统必须**拒绝推进**，而不是把假设当默认值放过去。

### 2.2 工程不变量 ①：模型可见即可审计（Model-visible ⟺ logged）

**定义（GB 02 §C 原话）**：凡是影响请求的内容都是会话事件——system prompt（`system/message` 是 surface 节点 0）、注入上下文（带 source 的 `user/message`）、请求信封（`request/header`）、工具 schema（在 header 内）。

**为什么对本产品是承重的。** 商奕的交付物是一份**咨询报告**，报告里每个数字、每条假设、每个"为什么这么判断"都必须能回答"你凭什么"。这与通用编码 agent 有本质区别：

- 编码 agent 的交付物是代码，代码可以独立被测试；日志缺失只影响可调试性。
- 咨询报告的交付物**就是审计链本身**。若模型在 turn 7 看到的是"对手成本估计 55（来源：公开财报）"而日志里只有"对手成本估计 55"，那么报告 ③ 层的"参数估计、来源与可信度"就是**编造的**——直接违反铁律 1 的 provenance 要求。

**落点。** 三类内容强制 surface 化（§8.2）：

| 内容 | 事件形态 | 若缺失的后果 |
|---|---|---|
| 部署/剧本 persona system prompt | `SystemMessage`（surface 节点 0） | 无法回答"你以什么身份给的这份建议" |
| 注入的上下文（情报摘要、画像、先验库条目） | `UserMessage` + `source: ContextSource` | 报告的"证据来源"无法自动生成 |
| 每次模型调用的信封（模型、温度、工具 schema 哈希、阶段） | `RequestHeader`（log-only，但**必须**存在） | 无法证明"某个结论是在禁用 `web_search` 的阶段产出的" |
| 工具调用与结果 | `ToolResult`（surface） | 数值 provenance 断链 |

> 判据（可直接写成测试）：**若删掉日志里任一 surface 事件后 `derive_messages()` 的输出不变，则该事件在撒谎**——它声称影响请求却没有影响。

### 2.3 工程不变量 ②：能力 seam 失败即拒绝，绝不静默降级

**定义（GB 02 §C 原话）**：无审批 seam 的 `ask` 一律 deny；无法强制的沙箱拒绝执行；provider 缺能力在委派**前**报类型化错误；现存但损坏的 patch 文件导致启动失败。

**为什么对本产品是承重的。** 静默降级在通用 agent 里是"体验差一点"，在商奕里是**结论失效**：

| 静默降级场景 | 后果 |
|---|---|
| provider 不支持 persona 覆盖，系统忽略之 | "对抗性监管者"persona 悄悄继承通用助手框架 → rollout 分布不再是博弈证据 → `persona_sim` 选择器的权重**整份作废**，而报告仍会给出一个看起来正常的概率分布 |
| 沙箱不可用则静默不隔离，跑 LLM 生成的 Python | 数值可能被网络/环境污染 → 违反铁律 1，且**不可检测** |
| LLM 聚点判断（`focal_llm`）供应商返回非 JSON 则静默填空串 | 融合公式少一项判据却照常归一化 → 概率分布被无声重标定 |
| checkpoint 无应答者则沿用上次答案 | 铁律 2 被绕过，且日志里看不出发生过 |

**落点。** 三类 seam 必须返回**可区分的类型化错误**，而不是 `Option::None` 或默认值：

```rust
#[derive(Debug, thiserror::Error)]
pub enum SeamError {
    #[error("sandbox unavailable: {0}")]
    SandboxUnavailable(String),        // 码 SANDBOX_UNAVAILABLE
    #[error("approval unavailable: no approver could decide for {request}")]
    ApprovalUnavailable { request: String },
    #[error("provider {provider} lacks capability {capability}")]
    CapabilityMissing { provider: String, capability: &'static str },
    #[error("config patch failed to apply: {0}")]
    PatchApplyFailed(String),
}
```

并且**让遗忘在类型系统上不可能**：需要沙箱的求解工具构造函数**只**接受 `Confined`，永不接受裸 `argv: Vec<String>`（GB 02 §11）。

```rust
pub enum Confined { Enforced(ConfinedArgv, Enforcement), Denied(SandboxUnavailable) }
```

> 交付物即测试：**"无应答者 ⇒ `ApprovalUnavailable` ⇒ 视为拒绝"** 这一个单测，比整张权限 preset 表更有价值（GB 02 §10 结论）。

### 2.4 工程不变量 ③：配置整行替换，不做隐式深合并

**定义（GB 02 §14）**：补丁替换**整行的 config**，绝不深合并。理由原话："整行替换要求覆盖者完整表达最终意图，代价是不能依赖未写出的继承；好处是避免多层嵌套对象与数组在'追加、替换还是去重'上形成难以解释的隐式合并。"

**为什么对本产品是承重的。** 三个具体理由：

1. **"为什么这个 timeout 还是 30" 是审计失败的前兆。** 跨层深合并的经典故障：上层只想改一个兄弟字段，下层字段存活。在商奕里，层栈是 `部署默认 → 事务所政策 → 委托剧本 → 一次性 CLI 覆盖`，任何一层"只改了一个字段"的补丁都可能让另一层的数值悄悄生效。
2. **影响结论的数值配置不允许隐式继承。** `stochastic` 选择器的 `dt` / 突变率 / `horizon`、`learning` 的初始信念网格、Monte Carlo 样本数、rollout 次数——这些直接改变概率分布。若它们来自"某层的默认值没被覆盖"，案例**不可复现**，案例库回测（v0.1 §7.2 的闭环校准）就失去意义。
3. **可 diff 的生效配置是设计文档的一部分。** 设计文档要写"本次委托用了哪些选择器、权重多少"，这个答案必须能从配置系统中**打印**出来，而不是靠人脑推演层合并。

**落点。**

```rust
pub struct Row {
    pub id: String,
    pub plugin: String,
    pub config: serde_yaml::Value,
    pub disabled: Option<Condition>,
    pub isolate: Option<String>,
}

/// 整行替换：同 `id` 的层条目**完全替换**该行（不合并、不追加、不去重）。
pub fn compose(layers: &[Vec<Row>]) -> Result<Vec<Row>, ConfigError> {
    let mut acc: Vec<Row> = Vec::new();
    for layer in layers {
        for row in layer {
            match acc.iter().position(|r| r.id == row.id) {
                Some(i) => acc[i] = row.clone(),   // 替换，绝不合并
                None => acc.push(row.clone()),
            }
        }
    }
    Ok(acc)
}
```

配套：`shangyi config --dump-default` 与 `shangyi config --dump` 两个命令（不启动会话即可检视组合后的树），以及案例日志里的 `ConfigResolved` 快照事件（§8.2）。

**范围限定与基座边界：**

- **适用**：商奕自己的领域配置（剧本行、工具行、选择器权重行、prompt 段行、checkpoint 行）—— 其加载器由本项目控制；
- **已证实（原"待验证"项，闭环）**：`vendor/codex/codex-rs/config` 的层**优先级**链不变（`PackagedDefaults(-10) < Mdm(0) < System(10) < EnterpriseManaged(15) < User(20) < Project(25) < SessionFlags(30) < LegacyManaged(40/50)`，GB 01 §7），但层间**合并语义已证实不是整行替换**，而是**表递归深合并 + 非表（标量/数组）整体替换**（`merge.rs` L75 / L94–95 / L143 / L149–152；`config_loader_tests.rs` L854 的 `nested.extra` 存活断言；详见 `research/harness/04-codex-config-semantics.md`）。因此 `[model_providers.*]` 与 `[mcp_servers.*]` 不改为整行替换，而是按 §13.4.1 的**例外清单 + 完整重述**处理。**R12 状态：已证实。**
- **不适用**：用户拥有的**标量设置**（如 `permission.default_preset`、预算上限）走带优先级链的类型化结构，不走补丁层（GB 02 §14 尾注）。

**边界（本原则管得到哪里）。** ③号不变量约束的是**加载器由本项目控制**的那部分配置：playbook 行栈由 `shangyi-config` 的 `compose()` 折叠，整行替换在此完整生效。**Codex 基座**的 TOML 层栈由上游 `merge_toml_values` 折叠，本项目不接管其合并算法，深合并在此**不受本原则约束** —— 这是对象与加载器的分界，不是原则的例外条款。基座侧的补偿写在 §13.4.1：例外清单的**完整重述**规则 + `--dump-config` / `--dump-default-config` 的**逐键 origin** 可检视性。

### 2.5 五条原则的违规后果映射

| # | 原则 | 一句话违反形态 | 可观察后果 | 主要落点章节 |
|---|---|---|---|---|
| 铁律 1 | LLM 不算数 | 报告数字由 LLM 或 LLM 生成代码产生 | 数字不可溯源；铁律失效 | §7.2、§8.2（`provenance`） |
| 铁律 2 | 关键假设过 checkpoint | 假设被静默默认化 | 建模前提未确认；结论虚构 | §7.3（阶段闸门）、§7.6 |
| ① | 模型可见即可审计 | 影响请求的内容未入日志 | 报告证据层不可自动生成 | §8.2（`Presence`） |
| ② | seam 失败即拒绝 | 缺能力/缺沙箱时给默认值 | 概率分布或隔离性无声失效 | §2.3 类型化错误、§7.4 |
| ③ | 配置整行替换 | 深合并让下层字段存活 | 案例不可复现；回测校准失真 | §2.4、§6.5 |

---

## 3. 技术选型决策

本章以 ADR 风格记录冻结决策。`§3.2–§3.8` 展开有争议或高风险的条目。

### 3.1 决策总表

| ID | 决策 | 备选 | 结论 | 理由 | 影响 |
|---|---|---|---|---|---|
| **D-A1** | 核心语言 | Rust / Python / TypeScript / Go | **Rust**，edition 2024，MSRV **1.95** | 与 Codex 基座同语言同工具链（GB 01 §0：`rust-toolchain.toml` → `1.95.0`、`edition = "2024"`），使 fork 具备可行性；数值求解性能与确定性；沙箱/isolation 原语是生态原生的（GB 02 §11：Rust 在此比 Node 有实质优势）；`#![deny(clippy::print_stdout)]` 式的库纪律 | 全栈重写；单人 6 周内必须靠复用，不能从零搭 harness |
| **D-A2** | harness 基座 | ① Codex fork-and-trim ② dsh-as-runtime ③ build-from-scratch | ~~① Codex fork-and-trim~~ → **已弃用（2026-11-10）：改为 ② dsh-as-runtime**——dsh 以**进程边界**提供 agent 运行时（profile + MCP 工具桥），商奕 Rust 引擎经 MCP 暴露领域工具；`vendor/codex` 整树移除。依据：[`变更记录 v0.1`](商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md) | 原选 ① 的理由（"加原生领域工具必须 fork"）只在**要复用源码**时成立；实测该复用从未发生——全仓仅 **1 行**代码引用 codex（`shangyi-core/src/lib.rs:81` 的架构测试），代价却是 91 个 codex crate / 76 MB vendor / 92 个锁定项 / 851 crate 依赖图 | 不再有 patch 集与 rebase 成本；改为维护 MCP 协议边界与 dsh profile |
| **D-A3** | 是否采用 dsh 运行时 | 采用 / 仅借鉴 / 忽略 | **已改判（2026-11-10）：采用（进程边界）**——dsh 作 agent 运行时段；"借鉴设计模式并署名"继续保留 | "语言不可共享"仍然成立，但它否证的是"复用 dsh 源码"，**不是**"用 dsh 运行时"。进程边界让 TS 运行时与 Rust 引擎共存；代价是 dsh 为 developer preview，"核心插件与 API 将持续演进"（GB 02 §A），须锁定审阅版本 | 需要 `shangyi mcp-serve`、dsh profile、事件流桥接；见变更记录 §1/§3/§5 |
| **D-A4a** | Code Mode / V8 | 保留 / 移除 | **移除** | 引入 V8 构建重量（GB 01 §0 的 `patches/rusty_v8_*`）；且"LLM 生成代码执行"与"数值可信性"目标不符（GB 01 §4 对商奕的判断） | 去掉 `code-mode`、`code-mode-host`、`code-mode-protocol`、`code-mode-runtime`、`v8-poc`；工具面收窄 |
| **D-A4b** | OpenAI 账号耦合 crate | 保留 / 裁剪 | **移除** `login`、`keyring-store`、`backend-client`、`analytics`、`cloud-tasks`（及 `chatgpt` 认证路径） | 均为 OpenAI 账号体系专用（GB 01 §5 对商奕的判断），商奕用自有 API key/环境变量凭据层 | 需自研凭据层（便宜）；`responses-api-proxy` 需相应调整 |
| **D-A5** | 模型接入 wire | Responses-only / 双 wire | **新增 Chat Completions 适配层** | Codex `WireApi` 仅 `Responses`，`wire_api = "chat"` 触发 `CHAT_WIRE_API_REMOVED_ERROR` 硬报错（GB 01 §5）；国内主力 provider（GLM/智谱、DeepSeek）主推 Chat（v0.1 §13 D2 已定 GLM 系） | **头号改造项**。落点：`codex-api` 新增 Chat wire adapter + 放宽 `model-provider-info` 的 `WireApi` enum |
| **D-A6** | 求解器实现 | 原生 Rust / PyO3 嵌 Python / C++ 绑定 / Gambit | **原生 Rust 自研** | Rust 生态**无**维护中的通用纳什求解库：支持枚举/Lemke–Howson/虚构对局/复制者动态/QRE/level-k/ESS **全部需手写**（GB 03 §1.7）；Gambit 不可用（见 D-A8） | **工期最大单一风险项**，建议预留 1.5–2 周（GB 03 结论摘要 1） |
| **D-A7** | 数值栈 | — | `faer`（首选）/`nalgebra` + `microlp`（首选）/`highs` + `clarabel` + `argmin` + `statrs`/`rand`/`rand_distr` | GB 03 §5；`microlp` 为纯 Rust LP（Apache-2.0），`faer` 纯 Rust 稠密 LA（MIT），均无 C 依赖 | 无 BLAS/LAPACK 构建摩擦 |
| **D-A8** | Gambit | 保留 / 进程隔离 / 弃用 | **弃用** | `pygambit`/Gambit 为 **GPL-2.0-or-later**，Python 绑定**无独立许可或商业例外**（`COPYING` 即 GPL-2.0）→ 当前方案中**最大的合规风险**（GB 03 §2）；进程隔离是缓解而非盾牌（GB 03 §2.2） | v0.1 §5.3 必须修订（R2）；多玩家/扩展式/精炼全部转自研 |
| **D-A9** | 差分测试 oracle | 无 / `nashpy` / Gambit | `nashpy` **仅用于 CI 差分测试**，**进程隔离** | `nashpy` 为 MIT（© 2016 Vince Knight），许可干净（GB 03 §2）；作为 oracle 保住"经战斗检验的边界情况处理"（GB 03 §3） | 不进发货路径；发货二进制**零 Python 依赖** |
| **D-A10** | Python 定位 | 主栈 / 同进程嵌入 / sidecar | **sidecar**：仅绘图（`matplotlib`，PSF 系）+ CI 验证；LLM 生成的临时 Python 走**失败即拒绝**的沙箱 | 消除 GPL 链接风险；**强化铁律 1**（数值来自确定性 Rust 而非生成代码）；消除沙箱风险面（GB 02 §11 尾注、GB 03 §3） | v0.1 §10 技术栈修订（R3） |
| **D-A11** | 工具层 | 保留编码工具 / 重写为领域工具集 | **重写**，替换 `exec_command`/`apply_patch`/`view_image` 等编码形态工具 | 商奕不写代码，写咨询（v0.1 §4 工具表已是领域形态） | 触及 `core/src/tools/spec_plan.rs::add_core_tool_sources` 与 `handlers/`（GB 01 §10 复用地图） |
| **D-A12** | 会话状态机落点 | 自建循环 / 挂在 harness 钩子 | 挂在 **`agent/pre-step` + `agent/request`** 上，阶段为 playbook 驱动的数据 | `pre-step` 是唯一能在"拟议 step"执行前**拒绝或替换**的 seam；`request` 可在调用前改写信封（阶段化工具白名单、温度）（GB 02 §6） | 需要 fork 以注册原生 hook（同 D-A2 理由） |
| **D-A13** | Swarm 实现 | 自建编排 / 复用 harness 多智能体 | 复用 Codex 的 `spawn_agent`/`agent-graph-store`/`AgentRoleConfig`/`ThreadManager::spawn_subagent` **+** dsh 的能力预检与持久描述符 | Codex 多智能体是**一等公民**（GB 01 §8）；dsh 提供"委派前能力协商"与"持久描述符使崩溃后恢复同一批 persona"（GB 02 §8） | 避免自研调度；`AgentRoleConfig` 与 persona 语义需对齐 |
| **D-A14** | 配置合并语义 | 深合并 / 整行替换 | **整行替换**（商奕自有配置层） | §2.4；GB 02 §14 | **边界已澄清（R12 已证实）**：商奕自有 loader 用整行替换；Codex 基座层栈为深合并，按 §13.4.1 例外清单处理 |
| **D-A15** | 域名/目录 | `$CODEX_HOME` / `$SHANGYI_HOME` | **`$SHANGYI_HOME`**（默认 `~/.shangyi`） | 商标与产品边界要求（§4.3）；同时避免与用户已装 `codex` 互相污染 | **真实落点是 `utils/home-dir/src/lib.rs::find_codex_home()`**（上游读 `CODEX_HOME`、默认 `~/.codex`），**不是** `codex-home` —— 后者只有 2 个文件、1 个导出（`CodexHomeUserInstructionsProvider`），属兼容层；patch 0005，见 §6.2 表 B |

### 3.2 Rust 作为核心语言（D-A1）

**决定**：Rust，edition 2024，MSRV 1.95（对齐 Codex `rust-toolchain.toml` 的 `channel = "1.95.0"`，GB 01 §0）。

**为什么不继续 Python 单体。** v0.1 §10 的栈是一个可行的 6 周 MVP 方案，但有一个它自己无法解决的矛盾：v0.1 §1.1 把产品形态定为"对标 Codex / Claude Code 的产品形态"。复用一个编码 agent harness 的**全部价值**在于它的会话循环、审批、沙箱、持久化、多智能体、hooks/plugins/skills；而这些在 Codex 里是 Rust，在 dsh 里是 TypeScript。Python 单体意味着**两条路都走不通**——要么从零写 harness（6 周不可能），要么放弃 harness 复用（那就没有审批/沙箱/留档的成熟实现）。

**代价与缓解。**

| 代价 | 缓解 |
|---|---|
| 领域开发者需要写 Rust | 领域逻辑集中在 `shangyi-*`（§6.3），接口是数据（GameSpec/EquilibriumSet），非 Rust 专家可读 |
| 求解器自研工期 | D-A6；CI 差分测试（D-A9）压正确性风险 |
| 生态缺 matplotlib/pandas | sidecar（D-A10） |

### 3.3 为什么不直接把 dsh 当基座

这是本方案里最容易被提出的质问，因为它看起来更省事：dsh 是 MIT（比 Apache-2.0 更宽松）、"Everything is a plugin"（比 Codex 更可扩展）、核心就是 agent harness 而非编码 agent。**结论仍是不采用**，理由按强度排序：

| # | 理由 | 依据 |
|---|---|---|
| 1 | **语言不可共享**。dsh 是 TypeScript。若以它为基座，商奕核心要么是 TypeScript（与 D-A1 冲突，且放弃 Rust 的求解性能与沙箱原语），要么是"HALF TS half Rust"的双运行时。GB 02 开篇直接写明："dsh 为 TypeScript，故其**源码不能直接用于 Rust**"。而 Codex 与 D-A1 同语言——**这是不可替代的属性** | GB 02 文首 |
| 2 | **成熟度契约不稳定**。dsh 官方逐字："DeepSeek Harness is now in **developer preview**……Its core plugins and APIs will continue to evolve."；`dshbook` 指南原话："开发预览不是'不能试'，而是'不能假装它已经不需要试'。" 一个 6 周比赛项目承担不起基座 API 漂移 | GB 02 §A 成熟度 |
| 3 | **它的价值形态是"模式"而非"代码"**。dsh 的 14 项可移植设计中，真正承重的（事件溯源日志 + surface、turn/step 词汇、整值事件 + 纯投影、waterfall 钩子、能力预检、log-only 遥测、整行替换配置）**全部是数据与协议设计**，用 Rust 重实现的成本远低于把 TS 运行时桥进来的成本。反过来，它的**运行时机制**（Proxy 式 `Context`、GC 生命周期的 `Fiber`、`isolate`/`intercept`、字符串键无类型事件总线）在 GB 02 §E 中被明确列为**不应移植**——"Rust 从 trait、enum 与显式注入获得更多安全性" | GB 02 §D、§E |
| 4 | **沙箱与工具面不可复用**。dsh 的关键隔离能力部分依赖两个 BSD-3-Clause 预编译原生插件（`node-addon-system` Linux Landlock 启动器 + macOS arm64 flock），且其 `package.json` 声明的许可与捆绑 `LICENSE` 文本**不一致**（GB 02 §A 上游不一致项）。在 Rust 里我们有 `landlock`/`seccompiler`/`bwrap`/`sandbox-exec` 原生路径（GB 02 §11） | GB 02 §A、§11 |
| 5 | **Codex 提供 dsh 没有的东西**：已落地的审批策略模型（`AskForApproval::Granular`）、成熟的 `execpolicy`（Starlark）、跨平台沙箱管理器（含 macOS Seatbelt 内嵌 profile）、约 150 crate 的 workspace、`cargo-deny` 强制的许可白名单、以及**已经存在的多智能体基础设施**（`spawn_agent`、`agent-graph-store`、`AgentRoleConfig`） | GB 01 §7、§8 |

**结论**：dsh 是**设计供体（design donor）**，Codex 是**代码基座（code substrate）**。两者角色不重叠，且 dsh 的署名必须显式（§4.1）——我们借了它的设计，就要在 `NOTICE` 与文档里点名，这不因"没抄一行代码"而免除。

### 3.4 移除 Code Mode / V8（D-A4a）

| 项 | 内容 |
|---|---|
| 移除对象 | `code-mode`、`code-mode-host`、`code-mode-protocol`、`code-mode-runtime`、`v8-poc`，以及构建侧的 `patches/rusty_v8_*` |
| Codex 中原有 wire 名 | `exec`、`wait`（`code-mode-protocol::PUBLIC_TOOL_NAME`/`WAIT_TOOL_NAME`） |
| 移除理由 1 | 构建重量：V8 需要 `rusty_v8` 补丁与独立 host 进程（`justfile` recipe `codex-code-mode-host`） |
| 移除理由 2 | 语义冲突：Code Mode 的存在意义是"让模型写代码来编排工具"；商奕的铁律 1 要求**数值不由模型生成的代码产生**。保留它会给出一个诱人但错误的路径 |
| 附属影响 | `ToolExposure::CodeModeOnly`/`ToolMode::CodeMode` 在裁剪后成为死变体，保留 enum 形状但仅使用 `Direct`/`Deferred`（减少 patch 面积）⚠️ 该"删变体/保留形状"的取舍与 vendor 计划 §2.6 末注冲突，见附录 D 清单第 4 条 |
| 替代能力 | 模型的"多步编排"由领域工具的组合与剧本阶段表达，不由代码表达 |

### 3.5 Responses-only 的限制与 Chat 适配层（D-A5）

**事实（GB 01 §5，逐字）**：`pub enum WireApi { #[default] Responses }`，反序列化时 `"responses"` 通过，`"chat"` **硬报错**：

```
CHAT_WIRE_API_REMOVED_ERROR = "wire_api = \"chat\" is no longer supported.
How to fix: set wire_api = \"responses\" in your provider config."
```

旧 `ollama-chat` provider id 同样被拒（`OLLAMA_CHAT_PROVIDER_REMOVED_ERROR`）。

**为什么不能回避。** Codex 允许任意 OpenAI 兼容 `base_url`，但端点**必须说 Responses API**。而 GLM/智谱、DeepSeek 等国内主力 provider 主推 **Chat Completions**；v0.1 §13 D2 已定"GLM 系（国内合规 + 中文）"。GB 01 §5 明确称这是复用 Codex 的"**头号改造项**"。

**三条路线与选择：**

| 路线 | 描述 | 取舍 |
|---|---|---|
| ① 新增 Chat wire adapter（**采纳**） | 在 `codex-api` 增加 Chat Completions 客户端，放宽 `model-provider-info::WireApi` | 一次性工程量，之后任意 Chat 端点可用；无额外部署组件；**在进程内**完成 Responses↔Chat 的语义翻译 |
| ② 外挂转换代理 | 自建 Responses↔Chat 代理进程 | 多一个部署组件与故障点；语义翻译仍要自己写；[推断] Roo/其他社区代理存在但引入未审计依赖 |
| ③ 只用原生 Responses 端点 | 放弃 GLM/DeepSeek | 与 v0.1 §13 D2 冲突；国内合规与中文能力退让 |

**改动落点（两处，必须成对）：**

```rust
// 1. vendor/codex/codex-rs/model-provider-info/src/…（放宽枚举）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WireApi {
    #[default]
    Responses,
    Chat,     // ShangYi 新增（D-A5）。上游仅允许 Responses。
}

// 2. vendor/codex/codex-rs/codex-api/src/…（新增适配器）
pub struct ChatCompletionsClient { /* 复用 http-client/ReqwestTransport 与 codex-client 的重试 */ }
impl ChatCompletionsClient {
    /// 把 ResponsesApiRequest 翻译为 Chat Completions 请求体；
    /// 把 Chat 的 delta 流翻译回 ResponseStream<ResponseEvent>。
    pub async fn stream(&self, req: ResponsesApiRequest) -> Result<ResponseStream, ApiError>;
}
```

**必须处理的语义差异**（这是工期与正确性的真实成本，不能只写"加个适配器"）：

| 差异 | Responses API | Chat Completions | 处理 |
|---|---|---|---|
| 输入形态 | `ResponseItem` 列表（含 reasoning/工具项的有序序列） | `messages[]`（role-based） | 需要一次 `ResponseItem → messages` 的**有损投影**；工具调用/结果合并进 role 序列 |
| 工具 schema | `tools[]` + `ToolSpec::{Function,Freeform,Namespace,ToolSearch,WebSearch}` | `tools[]`（function only） | `Freeform`/`Namespace`/`WebSearch` **无对应物** → 必须 fail-loud 拒绝（不变量 ②），不得静默降级为普通 function |
| hosted 工具 | `web_search` 为 hosted 工具（provider `capabilities().web_search`） | 无 | 商奕的 `web_search` 走**自有领域工具**（v0.1 §4 的 `SearchProvider`），不依赖 hosted 能力 |
| reasoning | 独立 reasoning item | 供应商私有字段（如 `reasoning_content`） | 按 provider 白名单读取，仅入 log-only 事件；**永不**进报告 |
| 流式终止 | `response.completed` 等事件 | `finish_reason` + `[DONE]` | 映射到同一 `ResponseEvent` |
| token 计量 | 原生 usage | usage 字段（部分供应商缺） | 缺失时 fail-soft 记为 `Unknown` 并标注，不伪造 |

**登记为 v0.2 修订项 R7**（v0.1 §3.3 的"任意兼容端点切换"在基座上不成立，须写明需 Chat 适配层）。

### 3.6 求解器实现策略（D-A6 / D-A7 / D-A8 / D-A9）

**事实基线（GB 03 §1.7）**：Rust 生态**无维护中的通用纳什均衡库**。逐算法：

| 算法 | Rust 现成支持 | 结论 |
|---|---|---|
| 支持枚举（support enumeration，n ≤ 4） | **无**（`nash` 0.1.1 为玩具级；`lrslib-rs` 是 AGPL） | 自研 |
| Lemke–Howson（2 人） | **无** | 自研 |
| 扩展式逆向归纳 + 子博弈完美 | `extensive-form` 0.1.0 存在但 21 下载、单次发布 | 自研 |
| 颤抖手完美 / 序贯理性精炼 | **无** | 自研 |
| 虚构对局 / EWA | **未找到** | 自研 |
| 复制者动态 | **无库**（数学本身平凡） | 自研 |
| QRE（logit 不动点） | **无**（内层可用 `argmin`/`clarabel`） | 自研 |
| level-k | **无** | 自研 |
| ESS | **无**（谱检查 + 支撑条件） | 自研 |
| 蒙特卡洛传播 | 数值栈可组合 | 自研（薄） |
| CFR（2 人零和扩展式，**可选**） | ✅ `cfr` 0.6.2（MIT，活跃） | `直接复用`（若需要） |

**四个备选方案的对比：**

| 方案 | 代表 | 许可 | 进程内嵌入 GPL？ | 发货 Python 依赖 | 判定 |
|---|---|---|---|---|---|
| 原生 Rust 自研 | `faer`+`microlp`+`clarabel`+`argmin` | 全部宽松 | 否 | 零 | **采纳** |
| PyO3 嵌 Python | `pygambit` 经 PyO3 | PyO3 为 MIT/Apache-2.0，但被调库是 GPL | **是**（最危险场景） | 需 CPython + wheels | **拒绝**（GB 03 §3：保守/主流解读下，单二进制含 CPython + 进程内加载 GPL C++ 扩展 = 单一衍生作品，将要求你的二进制以 GPL-2.0-or-later 授权） |
| C++ 绑定 | 无可用 Rust 绑定可写（`lrslib-rs` AGPL-3.0-only；`coin_cbc` 传递 EPL-2.0） | AGPL/EPL | 是（AGPL 更糟，含网络条款） | 否 | **拒绝**（GB 03 §1.3、§1.4） |
| Gambit | `pygambit` 子进程 / shell 调 CLI | GPL-2.0-or-later，无例外 | 否（进程隔离） | 是 | **弃用**（GB 03 §2.2：进程边界是缓解手段，不是有保证的盾牌，也不解除附着于 Gambit 组件自身的义务；对商业产品"低风险决策就是不使用 Gambit"） |

**许可理由小结（详细义务见 §4）：**

- **Gambit 必须走**：`pygambit`/Gambit 的 Python 绑定**没有独立许可、没有商业例外**，`COPYING` 即 GPL-2.0；若它在同一地址空间被链接（PyO3/进程内 import），商奕二进制将成为 GPL 衍生作品。这不是"可以接受的风险"，而是**产品许可模式的存亡问题**。
- **`lrslib-rs` 不可用**：虽然 lrslib 本身是 GPL-2.0，其 Rust 绑定是 **AGPL-3.0-only**，且 AGPL 的网络条款比 GPL 更糟——"这恰恰是顶点枚举/支持枚举最想要的机械，但不能发货"（GB 03 §1.3）。
- **`coin_cbc` 不可用**：包装 CoinOR CBC（EPL-2.0），静态链接进闭源二进制是合规问题；改用纯 Rust `microlp` 或 `highs`（GB 03 §1.4）。
- **`nashpy` 可留但只做 oracle**：MIT，© 2016 Vince Knight，许可干净；以子进程 + JSON 边界调用，零边际架构成本（GB 03 §3）。
- **工期上这是最大风险**：GB 03 结论摘要 1 明写"工期上这是最大单一风险项，建议预留 **1.5–2 周**"。W2–W3 的里程碑必须为它单独留缓冲。

**自研模块的验收策略**（对应 v0.1 §11.3 质量评估）：

1. **golden cases**：囚徒困境 / 性别战 / 斗鸡 / Cournot（数值）/ Young 惯例博弈 / 星宇股份原型——沿用 v0.1 §11.3 清单；
2. **CI 差分**：每次 `shangyi-solver` 提交，对随机生成的 n ≤ 4 标准式博弈与 `nashpy` 的输出做**解集集合比较**（以容差归一化后比较，而非逐个均衡比较——枚举顺序不是契约）；差分 harness 以 `nashpy` 的**纯策略支撑一致性**而非浮点数值为判据；
3. **崩溃而非静默**：求解器在数值不收敛、退化博弈、支撑集为空时必须返回类型化错误（`SolveError::Degenerate`/`NoEquilibriumFound`），**禁止**返回空解集掩盖失败（不变量 ②）。

### 3.7 Python 降级为 sidecar（D-A10）

**决定**：Python 只做两件事，且都在**进程边界之外**：

| 用途 | 包 | 许可 | 与主流程关系 |
|---|---|---|---|
| 绘图 | `matplotlib` 3.11.2 | PSF 系（宽松） | sidecar 进程；主进程产出**结构化图数据 JSON**，sidecar 只负责渲染 |
| CI 差分验证 | `nashpy` 0.0.43 | MIT | 仅 CI，不进发货路径 |
| KB 向量检索（沿用 v0.1 §7.1） | `chromadb` + BGE | Apache-2.0 / MIT | 可留在 sidecar 或独立服务；不在 harness 关键路径 |
| 报告渲染 | `pandoc` | **GPL-2.0-or-later** | **外部 CLI 调用**，不捆绑、不作为依赖（§4.4） |
| LLM 生成的临时数值代码 | 用户/模型提供 | — | 沙箱内、**失败即拒绝**，且结果不得直接进报告（铁律 1） |

**为什么这"强化"了铁律 1 而不是削弱它。** v0.1 §1.5 铁律 1 的原话是"所有数值计算在 **Python sandbox** 中执行"——这句话的本意是"数值不能由 LLM 编"，但它的**字面实现**（数值在沙箱里跑模型生成的代码）实际上给了 LLM 生成代码一条通向报告的合法路径。D-A10 把这条路径焊死：

```
v0.1 字面实现：LLM 提议数值算法 → 生成 Python → 沙箱执行 → 数值进报告
                                    ↑ 铁律 1 的形式满足，实质可被绕过

D-A10 实现：  LLM 提议建模 → 领域工具调用 → Rust 确定性求解器 → 数值进报告
                                    数值来源：非 LLM 生成、可复现、可审计
```

三条附带收益：

1. **消除 GPL 链接风险**（§3.6）；
2. **消除最高风险区**：GB 02 §11 把"跑 LLM 生成的 Python"称为"**最高风险区**"——进程边界内的任意代码执行。主路径不再需要它；
3. **可复现性**：Rust 求解器在不同机器上给同一答案（无 BLAS/LAPACK 版本差异、无 Python 版本差异），案例库回测（v0.1 §7.2）的 `logloss` 才有意义。

**保留的沙箱与其契约（不变量 ②在此最硬）**：

```rust
/// 无默认实现。原样返回调用者 argv 即是 bug。
/// （GB 02 §11；隔离层级：独立短生命周期进程、无网络命名空间、
///  只读根、一个可写临时目录、workspace 只读绑定、stdin/stdout JSON）
pub trait SandboxProvider: Send + Sync {
    fn confine(&self, argv: &[String], policy: &SandboxPolicy)
        -> Result<ConfinedArgv, SandboxUnavailable>;
}
```

`ConfinedArgv` 携带 `{ argv, enforcement: 'full'|'partial', denial_signatures, runner_failure_rules }`；denial 签名是**所选后端自己的方言**（bwrap 下 `EROFS`、Landlock 下 `EACCES`、Seatbelt 下 `EPERM`），**不做跨后端并集**（GB 02 §11 明确警告）。`RunnerFailureRule` 区分"运行器从未运行"与"隔离生效并拦下了命令"，但以**正面证据**为主信号——"Exit status alone never proves runner failure"。

**登记为 v0.2 修订项 R1/R3/R8。**

### 3.8 需要 v0.2 文档吸收的技术选型修订

§1.6 已登记 R1–R19。此处只强调三条"会被评审追问"的：

1. **R7（wire）** 是**纠正**而非偏好：v0.1 §3.3 的抽象在当前基座上无法工作。
2. **R1（Python sandbox → Rust 确定性）** 是**铁律的强化**：结论不变，载体改变。评审时需要明确说明这一点，否则会被读成"放弃铁律 1"。
3. **R10（风险表）**：v0.1 §12 缺两条本方案引入的新风险，必须补：求解器许可合规风险（已由 D-A8/D-A9 降到可接受）、Rust 博弈算法自研的工期与正确性风险（**尚未降到可接受，是当前最大风险**）。

---

## 4. 来源、许可与合规

> **本章不构成法律意见。** 全部为工程调研结论（GB 01/03 均声明"不构成法律意见"）。涉及 Gambit、商标使用边界、以及"内部保留 `codex-*` crate 名是否构成商标使用"的具体判断，如进入商业发布或融资尽调，需咨询律师。

### 4.1 逐来源署名与分发形态

| 来源 | 在商奕中的角色 | 许可 | 版权行（逐字） | 上游 | 处置 | 分发形态 |
|---|---|---|---|---|---|---|
| **OpenAI Codex**（`codex-rs`） | harness 基座 | **Apache-2.0** | `Copyright 2025 OpenAI` | `github.com/openai/codex` | `修改后复用`（部分 `直接复用`） | **源码子集随 `vendor/` 分发**；必须保留 `LICENSE` + `NOTICE` |
| **Ratatui**（经 Codex `NOTICE`） | 终端渲染（`codex-tui` 裁剪后） | **MIT** | `Copyright (c) 2016-2022 Florian Dehau` / `Copyright (c) 2023-2025 The Ratatui Developers` | — | `修改后复用`（经 Codex 间接引入） | 同上；Codex 的 `NOTICE` 已含此归属，商奕分发须**逐字复现**；**该复现义务独立于 `tui` 是否保留**（§4.6（1）） |
| **DeepSeek Harness（dsh）** | 设计模式来源（14 项中约 8 项） | **MIT** | `Copyright (c) 2026 DeepSeek` | `github.com/deepseek-ai/deepseek-harness` | `仅借鉴设计` | **不分发其源码**（TS 与 Rust 不可共享）；仍须署名 |
| **Cordis** 4.0.2 | dsh 的插件内核（作为 dsh 的构成被引用） | **MIT** | © 2026 DeepSeek | dsh 仓库 `vendor/cordis` | `仅借鉴设计` | 不分发 |
| **`@deepseek-ai/node-addon-system`** 0.1.2 | dsh 的 Linux Landlock 启动器 + 异步 POSIX flock 原生绑定 | **BSD-3-Clause**（SPDX 字段）；⚠️ 其捆绑 `LICENSE` 文件内容实为 MIT 文本 —— **上游不一致** | `Copyright (c) 2026, node-addon-landlock-run contributors`（**与 MIT 包不同版权人**） | `git+https://github.com/deepseek-harness/deepseek-harness.git`（不同 org，小写） | `仅借鉴设计` | 不分发 |
| **`@deepseek-ai/node-addon-system-darwin-arm64`** 0.1.2 | dsh 的 macOS arm64 flock 绑定 | **BSD-3-Clause**（同上不一致） | 同上 | 同上 | `仅借鉴设计` | 不分发 |
| **nashpy** 0.0.43 | CI 差分 oracle | **MIT** | `Copyright (c) 2016 Vince Knight` | — | `直接复用`（仅 CI） | 不进发货路径 |
| **matplotlib** 3.11.2 | 绘图 sidecar | **PSF 系**（matplotlib 1.3.0+ 许可协议） | — | — | `直接复用`（sidecar） | 不进二进制 |
| **pandoc**（CLI） | 报告渲染 | **GPL-2.0-or-later** | — | — | `直接复用`（**外部子进程**） | **不捆绑、不作为依赖**（§4.4） |
| **LaTeX**（TeX Live） | PDF 中文字体管线（备选） | **LPPL 1.3c** | — | — | `直接复用`（外部子进程） | 不捆绑；逐包留意 |
| **`SKILL.md` 约定** | 方法论卡片格式 | 跨工具约定（无单一许可主体） | — | Claude Code / Codex / dsh 共享 | `仅借鉴设计` | 商奕实现为**消费者** |

**关于 dsh 的版本引用准确性**（GB 02 §A 要求"按审阅时的安装版本"注明）：dsh CLI **0.1.5-rc.1**，插件库 **0.1.5-rc.2**（230 个包），构建于 vendored MIT 许可的 **Cordis 4.0.2** 之上。两串版本号都**不是稳定契约**。241 个包中 239 个为 MIT（© 2026 DeepSeek），2 个预编译原生插件为 BSD-3-Clause（© 2026 node-addon-landlock-run contributors）；该 scope 内**无 GPL/AGPL/商业限制**。

### 4.2 Apache-2.0 §4 对闭源商业产品的义务

| § | 义务 | 商奕的落地动作 | 负责产物 |
|---|---|---|---|
| §4(a) | 向接收方提供许可副本 | 分发物根目录放完整 `LICENSE`（Apache-2.0 全文）+ 商奕版权行；CLI 提供 `shangyi license` 打印 | `LICENSE` |
| §4(b) | **被修改的文件须附显著变更声明**（"此文件已被修改"） | 每个 `vendor/` 下被改文件在头部加注释，统一标记串含字面串 **`MODIFIED BY SHANGYI`**（前 10 行内，CI 断言）+ 在 `vendor/patches/` 有编号补丁 + 在 `ATTRIBUTION.md` 索引 | `ATTRIBUTION.md`、`vendor/patches/*.patch`、`vendor/.shangyi-modified` |
| §4(c) | 保留所有版权/专利/商标/归属声明 | 不删除上游文件头；不删除 `codex-rs/Cargo.toml` 的 `license = "Apache-2.0"`；保留原始 `NOTICE` 文本 | 逐文件 |
| §4(d) | 若含 `NOTICE`，衍生分发须**复现其内容**（不适用者除外） | 商奕 `NOTICE` 中原样复现 Codex `NOTICE` 全文（6 行，含 Ratatui 归属）。**Ratatui 段不随 `tui` crate 的裁剪而删除** —— 该义务的对象是"被分发作品的 `NOTICE` 内容"，与是否保留 `tui` 无关（§4.6（1）） | `NOTICE`（§4.7） |
| §3 | 专利授权 + 防御性终止 | 无需动作；但**须知晓**：若商奕对 OpenAI 提起专利诉讼，Apache-2.0 的专利授权自动终止 | 合规备忘 |
| §5 | 提交贡献默认同许可 | 商奕对 `vendor/` 的上游贡献（若回馈）自动 Apache-2.0 | 流程约定 |

**关键结论**：Apache-2.0 **允许闭源、商业使用、再许可**，**无传染性**，**无源码公开义务**。商奕**不需要**公开自己的 `shangyi-*` 代码。唯一的强制动作是"保留声明 + 标记修改"。

**残留注意项（三个）：**

| 项 | 事实 | 动作 |
|---|---|---|
| **MPL-2.0 文件级 copyleft** | Codex 许可白名单含 `MPL-2.0`（用于 `nucleo-matcher`，GB 01 §0）。MPL-2.0 是**文件级** copyleft | **不修改**任何 MPL-2.0 文件；若必须修改，则该**文件**须以 MPL-2.0 公开（不影响其余代码） |
| **V8 分发** | "若保留 Code Mode 则额外分发 V8（BSD-3-Clause + 其他声明）"（GB 01 §0） | 已由 D-A4a 消除 |

### 4.3 商标约束（Apache-2.0 §6）

**事实（GB 01 §0，逐字）**：**§6 不授予商标权**。"Codex"、"OpenAI"、"ChatGPT"、"GPT-5/6" 均为 OpenAI 商标。**商业产品必须改名，不得暗示背书。**

| 约束 | 商奕的状态 | 动作 |
|---|---|---|
| 产品名不得使用 "Codex"/"OpenAI"/"ChatGPT" | ✅ v0.1 §1.4 已满足：中文品牌 "商奕"、英文 "ShangYi"、CLI `shangyi`、仓库 `shangyi-agent` | 无需动作 |
| 不得暗示 OpenAI 背书或关联 | 需约束**措辞** | 全部对外材料（README、比赛申报、演示脚本、报告页脚）**禁止**"基于 Codex"、"OpenAI 技术"、"Codex 内核"等表述；只能说"包含 OpenAI Codex 项目（Apache-2.0）的衍生代码"这类**事实性归属** |
| 归属声明**必须**保留 | 由其派生 | `NOTICE` 中的 "OpenAI Codex / Copyright 2025 OpenAI" 属 §4(c)(d) 要求的归属声明，**这是被许可允许的指称性使用**，与商标使用无关 |
| **内部 crate 名 `codex-*`** | `vendor/codex/codex-rs/*` 中的 crate 名（`codex-core`、`codex-protocol`、`codex-cli` 等）保留 | **保留**（改名会把 patch 面积放大数倍，违反"最小补丁集"目标）。理由与边界：① 这些是**包名/路径**而非面向用户的产品标识；② 不进入任何用户可见字符串（二进制名、帮助文本、报告、日志对外输出一律 `shangyi`）；③ 登记为**待法务确认项** `[推断]` ——若评审认为包名亦构成商标使用，替代方案是统一的 mechanical rename（`codex-` → `sys-`，[推断] 预计 ≤ 4 小时脚本化改动 + patch rebase 成本） |
| 对外二进制名 | `shangyi`（`shangyi-cli` 的 `default-run`），**不保留** `codex`、`codex-exec`、`codex-app-server` 等对外名称 | 移除或重命名 vendor 的 bin 目标 |

### 4.4 GPL / copyleft 排除清单

**Rust 依赖禁用清单（GB 03 §5、§1）**——由 `deny.toml` 的 `bans` 与 CI 强制：

| crate | 版本（调研时） | 许可 | 禁用理由 |
|---|---|---|---|
| `lrslib-rs` | 0.4.0 | **AGPL-3.0-only** | 闭源商业产品硬性否决；AGPL 网络条款更糟。**且它恰好是支持枚举最想要的机械**——必须自研（D-A6） |
| `lrslib-sys` | 0.3.3 | AGPL 上游 | 同上 |
| `coin_cbc` | 0.1.9 | MIT（绑定）→ 传递 CoinOR **CBC EPL-2.0** | EPL 弱 copyleft，静态链接进闭源二进制是合规问题；改用 `microlp`/`highs` |
| `amfiteatr` / `amfiteatr_core` | 0.14.0 | crates.io 字段字面为 **`non-standard`** | "在读清仓库许可前不可接受"（GB 03 §1.1）。虽然更成熟（0.14.x、多 crate），但许可未明 |
| `game-theory-core` / `game-theory-mcp` | 0.1.0 | MIT | 2026-08-06 单次发布、34/22 下载，**未经验证** |
| `lau-game-theory-agents` | 0.1.0 | MIT | 2026-06-01 单次发布，未验证 |
| `extensive-form` | 0.1.0 | MIT OR Apache-2.0 | 2026-06-07 单次发布、21 下载，"不能用于生产决策" |
| `nash` | 0.1.1 | MPL-2.0 | 约 3.5 年无版本、README 404、玩具级。**仅可作参考代码，不作依赖** |
| `neumann` | 0.1.0 | MIT OR Apache-2.0 | 2020-10-21 单次发布，已废弃 |
| `gambit`（crates.io） | — | — | **同名陷阱**：实为"A chess engine implemented in Rust"，与博弈论无关 |
| `gambit-rs` | 0.2.0 | — | 同名陷阱：某 Web 协议的异步客户端 |
| `pygambit` / Gambit（**非 Rust，但同属禁用**） | 16.7.0 | **GPL-2.0-or-later** | 见 §4.2 前的分析；进程隔离是缓解而非盾牌；**弃用**（D-A8） |
| `minilp` | 0.2.2 | Apache-2.0 | 2020 年后停止维护（仅成熟度原因） |

**进程边界清单（允许但须严格遵守边界）：**

| 工具 | 许可 | 边界要求 |
|---|---|---|
| `pandoc`（PDF/docx 渲染） | GPL-2.0-or-later | 仅以**独立进程**+ 普通 CLI/文件通信调用；**不分发**其二进制（让用户/Brew/pkg 自装）；不得把 pandoc 当作库或链接其代码；不得搬运其代码进商奕 |
| LaTeX (TeX Live) | LPPL 1.3c | 同上；LPPL 义务主要落在"修改并再分发 LaTeX 文件本身"时 |
| `nashpy` | MIT | 子进程 + JSON；仅 CI；不进发货路径 |
| `scipy`（若 sidecar 用到） | BSD-3-Clause（捆绑物含 GCC runtime `GPL-3.0-or-later` **带 GCC 例外**、`LGPL-2.1-or-later` libquadmath、OpenBLAS BSD） | 主代码 BSD，正常使用安全；捆绑的 GPL/LGPL 部分带标准例外（GCC 运行时例外即为此设计，**不"感染"用户代码**） |

> **Gambit 若日后确需**：只能作为**独立可执行/服务**（stdio 或 localhost JSON-RPC）运行，**由客户自行安装**（从而完全避免再分发），并把该边界写进文档；仍然**不解除**附着于 Gambit 组件自身的 GPL 义务，且需律师确认衍生作品判定。**当前决定：不使用。**

### 4.5 依赖许可白名单

以 Codex `deny.toml` 的既有白名单为基线（GB 01 §0，CI `cargo-deny.yml` 强制），加 sidecar 与领域栈的增补：

```toml
# deny.toml（商奕）—— 未列许可即 CI 失败
[licenses]
allow = [
  "Apache-2.0",
  "Apache-2.0 WITH LLVM-exception",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "BSL-1.0",
  "CC0-1.0",
  "CDLA-Permissive-2.0",
  "ISC",
  "MIT",
  "MIT-0",
  "MPL-2.0",       # 文件级 copyleft：不得修改该文件（nucleo-matcher）
  "OpenSSL",       # aws-lc-sys
  "Unicode-3.0",
  "Unlicense",
  "Zlib",
  "PSF-2.0",       # sidecar：matplotlib
]
exceptions = [
  { name = "nucleo-matcher", allow = ["MPL-2.0"] },   # 显式登记，禁止改动
]
confidence-threshold = 0.9
[licenses.private]
ignore = true      # shangyi-* 为本仓库私有 crate
```

**商奕领域栈的 crate 许可（全部在白名单内，GB 03 §5）：**

| 用途 | crate | 许可 | 处置 |
|---|---|---|---|
| 线性代数 | `faer`（首选，纯 Rust 无 C 依赖）/ `nalgebra` | MIT / Apache-2.0（⚠️ `nalgebra` 自 0.32.x 起的许可由 BSD-3-Clause 变为 Apache-2.0，GB 03 §1.5） | `直接复用` |
| LP/MILP 子问题（零和、相关均衡、QRE 内层） | `microlp`（首选纯 Rust）/ `highs` | Apache-2.0 / MIT | `直接复用` |
| 锥优化（QRE 约束形式） | `clarabel` | Apache-2.0 | `直接复用` |
| 非线性优化 | `argmin` | MIT OR Apache-2.0 | `直接复用` |
| 分布与统计 | `statrs` / `rand_distr` | MIT / MIT OR Apache-2.0 | `直接复用` |
| 随机数 | `rand` | MIT OR Apache-2.0 | `直接复用`（⚠️ 版本线待核实，GB 03 §1.5 标 `[不确定]`） |
| 2 人零和不完全信息扩展式（可选） | `cfr` | MIT | `直接复用`（唯一维护中的 Rust 博弈 crate） |
| 重复博弈/锦标赛脚手架（参考） | `t4t` | MIT | `仅借鉴设计`（迭代博弈脚手架） |

**Harness 侧新增依赖**（GB 02 §F crate 汇总，全部白名单内）：`tokio`、`serde`/`serde_json`/`serde_yaml`、`async-trait`、`tower`、`inventory`/`linkme`、`arc-swap`、`rusqlite`、`uuid`、`notify`、`landlock`/`seccompiler`/`bwrap`/`sandbox-exec`（argv 包装）、`tiktoken-rs`、`minijinja`（可选）、`opentelemetry`+`tracing`、`thiserror`、`tokio-util::sync::CancellationToken`。

### 4.6 必须保留的声明

以下文本**必须**出现在分发物中，逐字（不翻译，不改写）：

**（1）Codex `NOTICE` 全文（复现自上游，**逐字节**；共 6 行，含第 3 行空行）**

```
OpenAI Codex
Copyright 2025 OpenAI

This project includes code derived from [Ratatui](https://github.com/ratatui/ratatui), licensed under the MIT license.
Copyright (c) 2016-2022 Florian Dehau
Copyright (c) 2023-2025 The Ratatui Developers
```

> **该段不随 `tui` 删除（合规红线）**：Apache-2.0 §4(d) 要求衍生分发**复现被分发作品的 `NOTICE` 内容**。商奕 vendor 的是 Codex 整个 workspace，Codex 的 `NOTICE` 含 Ratatui 段，因此无论 `tui` / `ansi-escape` / `ratatui` 依赖链是否被裁剪，这 6 行都必须原样出现在分发物中。**裁剪 `tui` 时删除本段是合规错误**（`research/harness/11-vendor-plan.md` §1.3 对"NOTICE 义务是否随 `tui` 消失"的回答：**不消失，且不能删**）。若法务希望将该段改为"历史归属声明"之类的简化措辞，属复核事项，**不得由工程师在裁剪时自行删除**。
>
> **行数口径（订正）**：上游 `NOTICE` 文件共 **6 行**（242 B，每行以 `\n` 结尾，文件到第 6 行结束；第 3 行为空行，第 4 行为 `derived from [Ratatui](URL)` 一句，第 5–6 行为两行版权）。**Ratatui 归属段本身是 3 行正文**（第 4–6 行）；旧文 §C.1 曾写"这 4 行"，与 §4.6 的"6 行"打架，现统一为"全文 6 行 / 其中 Ratatui 段 3 行正文"。
>
> **§4(d) 复现要求是"逐字节"而非"逐字"**：本项目曾有版本把第 4 行写成 `This project includes code from Ratatui ... licensed under the MIT license.`（丢 `derived`、丢 URL、以 `...` 占位），并把第 2 行写成普通空格。该写法**不是**合规的复现文本。正确来源是字节级真值文件 `vendor/codex/NOTICE`（242 B；第 2 行 `Copyright` 与 `2025` 之间、`2025` 与 `OpenAI` 之间是 **U+00A0 不换行空格**，不是普通空格 —— 肉眼不可分辨，必须用字节比对验证）。CI 应加一条断言：分发物 `NOTICE` 中的该段与 `vendor/codex/NOTICE` **逐字节相等**。

**（2）dsh 设计署名（GB 02 §G 建议行，本文采纳）**

> 本文档中的设计模式改编自 **DeepSeek Harness (dsh)**，© 2026 DeepSeek，依 MIT 许可（`LICENSE`）发布；CLI `@deepseek-ai/dsh` 0.1.5-rc.1，插件库 0.1.5-rc.2，构建于 vendored MIT 许可的 **Cordis** 4.0.2 内核之上 —— `https://github.com/deepseek-ai/deepseek-harness`。其中两个预编译原生沙箱/flock 插件（`@deepseek-ai/node-addon-system`、`@deepseek-ai/node-addon-system-darwin-arm64`，v0.1.2）版权归 © 2026 node-addon-landlock-run contributors，依 BSD-3-Clause 而非 MIT 许可。技能格式（`SKILL.md` + YAML frontmatter）遵循与 Claude Code 共享的跨工具 Agent Skills 约定。DeepSeek Harness 属**开发者预览**软件，其核心插件与 API 将持续演进；本文引用的所有包名、事件名与配置键均为带版本与时点的观察，不构成稳定契约。

**（3）逐文件变更声明模板（Apache-2.0 §4(b)）**

```rust
// SPDX-License-Identifier: Apache-2.0
// MODIFIED BY SHANGYI. This file was modified by the ShangYi project (2026).
// Original: OpenAI Codex, Copyright 2025 OpenAI, Apache-2.0.
// Change: wire_api = "chat" accepted; see vendor/patches/0001-wire-api-chat.patch
```

> **标记串是 CI 契约，不得意译**：CI 门（`cargo xtask verify-change-notices`）断言每个被改文件的**前 10 行**内含字面串 `MODIFIED BY SHANGYI`；被改文件清单以 `vendor/.shangyi-modified` 为权威（路径在 vendor 根，不在 codex-rs 内），且对 `vendor/codex/codex-rs/` 做一次 `git diff --name-only <上游快照 tag>` 反向核对（改了文件却漏登记 → CI 失败）。**新增文件不是"修改"**，不加本标记（否则无法区分哪些是 OpenAI 的代码、哪些是商奕的），改用商奕自己的版权头（`Copyright 2026 商奕`，Apache-2.0）。`.md`/`.json`/`Cargo.toml`/`justfile` 等非源码文件用其原生注释语法（`#` / `//`）插入同一标记串。

**（4）无 GPL 依赖声明**

```
This distribution does not include, link against, or depend on:
  - Gambit / pygambit (GPL-2.0-or-later)
  - lrslib / lrslib-rs (AGPL-3.0-only)
  - CoinOR CBC / coin_cbc (EPL-2.0)
External tools invoked as separate processes (pandoc, LaTeX) are not distributed
with this product. See COMPLIANCE.md.
```

### 4.7 `NOTICE` 骨架（商奕分发物）

```
ShangYi（商奕）
Copyright 2026 <商奕项目权利人>

--------------------------------------------------------------------------
This product includes software developed as part of the OpenAI Codex project
(https://github.com/openai/codex), licensed under the Apache License, Version 2.0.
A copy of the Apache License, Version 2.0 is provided in LICENSE.

Derived portions are located under vendor/codex/codex-rs/. Modified files carry a
"MODIFIED BY SHANGYI" header and are indexed in ATTRIBUTION.md with
the corresponding patch under vendor/patches/. Upstream crate names are
retained unmodified for patch-minimization; all user-visible names are ShangYi.

--- Start of upstream NOTICE (reproduced verbatim, per Apache-2.0 §4(d)) ---
OpenAI Codex
Copyright 2025 OpenAI

This project includes code derived from [Ratatui](https://github.com/ratatui/ratatui), licensed under the MIT license.
Copyright (c) 2016-2022 Florian Dehau
Copyright (c) 2023-2025 The Ratatui Developers
--- End of upstream NOTICE ---

The terminal rendering used by this product derives from ratatui, licensed
under the MIT License, Copyright (c) 2016-2022 Florian Dehau and
Copyright (c) 2023-2025 The Ratatui Developers.

--------------------------------------------------------------------------
Design attribution (no source code included):

Portions of this product's design are adapted from DeepSeek Harness (dsh),
Copyright (c) 2026 DeepSeek, licensed under the MIT License,
https://github.com/deepseek-ai/deepseek-harness
 - dsh CLI @deepseek-ai/dsh 0.1.5-rc.1, plugin library 0.1.5-rc.2
 - built by dsh on a vendored copy of Cordis 4.0.2 (MIT), Copyright (c) 2026 DeepSeek
 - two precompiled native sandbox/posix-flock addons in the dsh distribution,
   @deepseek-ai/node-addon-system and @deepseek-ai/node-addon-system-darwin-arm64
   v0.1.2, are BSD-3-Clause, Copyright (c) 2026 node-addon-landlock-run contributors
 - DeepSeek Harness is developer-preview software; its core plugins and APIs
   continue to evolve. Names, events and config keys cited above are
   point-in-time observations, not a stable contract.
No TypeScript source from dsh or Cordis is included in this product.

The methodology-card format consumed by this product (SKILL.md + YAML frontmatter)
follows the cross-tool Agent Skills convention shared by Claude Code, Codex and dsh.

--------------------------------------------------------------------------
CI / sidecar components (not part of the shipped Rust binary):

nashpy 0.0.43 - MIT, Copyright (c) 2016 Vince Knight  [CI differential testing only]
matplotlib 3.11.2 - PSF-based license  [plotting sidecar]

--------------------------------------------------------------------------
Excluded, not used, not distributed:

Gambit / pygambit - GPL-2.0-or-later
lrslib / lrslib-rs - AGPL-3.0-only
CoinOR CBC / coin_cbc - EPL-2.0
pandoc - GPL-2.0-or-later (invoked as an external process; not distributed)
```
> ⚠️ **两处措辞待跟进（附录 D 清单第 8 条）**：骨架里 `The terminal rendering used by this product derives from ratatui …` 与"本产品包含源自 Ratatui 的代码（经 Codex 传递）"（附录 C.2）都**预设了 `tui` 仍被保留**。`tui` 移除后，这两句的"使用/包含"措辞会成为不准确的陈述，应改为**历史归属声明**（"上游 Codex 曾包含 ratatui 派生代码；商奕分发物保留其归属"）。**§4(d) 的逐字复现义务由 `--- Start/End of upstream NOTICE ---` 之间的英文块满足**（已核验为与 `vendor/codex/NOTICE` 逐字节相等，242 B），**与这两句措辞无关**；但只要它们维持现状，读者就会得到"本产品仍含 ratatui 代码"的错误印象。
>
> **`NOTICE` 骨架的真值纪律**：嵌入块的正确内容以 `vendor/codex/NOTICE` 为**唯一字节级真值**。附录 C.2 的中文骨架是**面向中文读者的转述**，不得作为 §4(d) 的复现文本；分发物中必须出现的是本节的英文块。

以及 `ATTRIBUTION.md`（机器可生成的 patch 索引）：

```toml
# ATTRIBUTION.md 的数据源（由 `cargo xtask attribution` 生成）
[[patch]]
id = "0001"
file = "vendor/codex/codex-rs/model-provider-info/src/lib.rs"
upstream = "github.com/openai/codex"
reason = "WireApi::Chat for Chat Completions endpoints (D-A5)"
statement = "MODIFIED BY SHANGYI. This file was modified by the ShangYi project (2026)."
```

### 4.8 合规结论与遗留项

| 项 | 状态 |
|---|---|
| Codex（Apache-2.0）闭源商业复用 | ✅ 允许；义务为"保留声明 + 标记修改"，无源码公开义务 |
| dsh（MIT）设计借鉴 | ✅ 允许；已主动署名，不分发源码 |
| Gambit（GPL-2.0-or-later） | ✅ 已弃用（D-A8），风险消除 |
| `lrslib-rs`（AGPL-3.0-only） | ✅ 已禁用，改自研 |
| `coin_cbc`（EPL-2.0 传递） | ✅ 已禁用，改 `microlp`/`highs` |
| Codex 自身依赖树 | ✅ 白名单内无 GPL/LGPL/AGPL；残留 MPL-2.0 文件级义务（不改该文件即可） |
| **商标（"内部保留 `codex-*` crate 名"）** | ⚠️ `[推断]` 认为可接受（包名非用户可见标识），**待法务确认** |
| **pandoc 的 GPL 边界** | ⚠️ 需在分发流程中固化为"外部进程 + 不捆绑"；产品化的安装器不得顺手打包 pandoc |
| **`amfiteatr` 的 `non-standard` 许可** | ⚠️ 若未来想用（它是唯一成熟的多智能体博弈仿真框架），须先读清仓库许可；当前不使用 |
| **MPL-2.0（`nucleo-matcher`）** | ⚠️ 需在 CI 中加检查：该 crate 的任何 vendored 文件不得出现在 patch 集中 |
| **Ratatui 归属段（Codex `NOTICE` 第 3 段）** | ✅ 必须随分发物复现（§4.7）；**不因裁剪 `tui` / `ratatui` / `ansi-escape` 依赖链而免除** —— §4(d) 义务独立于相关 crate 是否保留（§4.6（1）） |
| 本文的法律性质 | ❌ **不是法律意见**；进入商业发布/融资尽调须咨询律师 |

---

## 5. 总体架构

### 5.1 三层平面架构

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ 平面 C · 交互与交付层                                                          │
│  `shangyi` CLI（vendor `codex-tui` 裁剪）· checkpoint 提问件 · 进度视图        │
│  `shangyi new|resume|fork|config|report|backtest|kb` · 报告产物 PDF/docx/md   │
│  处置：`修改后复用`（codex-tui / cli，Apache-2.0）· `自研`（shangyi-cli）      │
├──────────────────────────────────────────────────────────────────────────────┤
│ 平面 B · 商奕引擎层（`shangyi/crates/*`）                                      │
│  只依赖平面 A 的 **public** 面（§5.4 边界规则）                                │
│                                                                              │
│   L3 接线与交付  shangyi-core（循环接线 + 阶段机 + 日志）  shangyi-tools        │
│                  shangyi-cli                                                  │
│   L2 决策引擎    shangyi-selection · shangyi-swarm · shangyi-consequence       │
│                  shangyi-discover                                             │
│   L1 领域基座    shangyi-solver · shangyi-kb · shangyi-intel · shangyi-report  │
│   L0 数据与配置  shangyi-gamespec · shangyi-playbooks                          │
│                                                                              │
│  处置：全部 `自研`；算法来源标 `仅借鉴设计`（dsh / 文献）                       │
├──────────────────────────────────────────────────────────────────────────────┤
│ 平面 A · harness 基座层（`vendor/codex/codex-rs/*`，fork-and-trim，Apache-2.0）│
│                                                                              │
│  Agent 核心   core: ThreadManager / CodexThread / Session / sessions/turn/step│
│  协议         protocol: Op / Event / EventMsg / approval / sandbox 策略类型    │
│  嵌入面       app-server(+protocol/-transport/-client/-daemon) JSON-RPC        │
│  Agent 循环   tasks / steering / compact / context_manager / world_state       │
│  工具         tools(ToolSpec/ToolExecutor) · core::tools(registry/router)      │
│  模型接入     codex-api · codex-client · http-client · model-provider(-info)   │
│  持久化       rollout / rollout-trace / history / state(SQLite)                │
│  安全         sandboxing(Seatbelt/Landlock) · execpolicy(Starlark) · approval  │
│  扩展         hooks · skills · plugin/core-plugins · ext/* · memories          │
│  多智能体     agent-roles · agent-graph-store · collaboration-mode-templates   │
│  配置         config / config-schema / features · models-manager/models.json   │
│                                                                              │
│  处置：`直接复用` / `修改后复用` / 移除（§6.2 逐 crate 标注）                   │
└──────────────────────────────────────────────────────────────────────────────┘

依赖方向（唯一合法方向）：C → B → A → 下游 crate
反向依赖（A → B、B → C）为架构违规，由 CI 的 `cargo xtask depcheck` 拒绝。
```

**为什么是三平面而非分层。** 平面 A 的规模（约 150 crate → 裁剪后预计 90–110）决定了它必须被视为"外部系统"而非"我们的一层代码"。把它画成平面并施加"只能走 public API"的硬约束，是为了让 patch 面可度量、可 rebase（§6.5）。

### 5.2 一次咨询的数据流（映射到 harness 原语）

以 `scenario` 模式 `pricing_response` 剧本为例。左列是 v0.1 §2.2 的领域步骤（语义权威），右列是 harness 原语。

| # | 领域步骤（v0.1 §2.2） | harness 原语 | 关键事件/工具 |
|---|---|---|---|
| 0 | `shangyi new --mode scenario --playbook pricing_response` | `ThreadManager::start_thread(StartThreadOptions)` → `CodexThread` | `SessionConfigured`；log-only `PlaybookSelected` |
| 1 | **intake** 剧本识别 + 价值网五向扫描 + PARTS 检查 + 引导式补全 | 1 个 **turn**，内含若干 **step**；每 step = 一次模型调用 + 其工具执行 | 工具 `vn_map`、`ask_user`、`pars_check`；产物 `CaseBrief`/`ValueNetMap`/`ParticipantProfiles` |
| 2 | **intel** 公开数据搜集 + 参与者画像更新（全角色） | 多 step，并行工具调用 | 工具 `web_search`、`web_fetch`、`data_ingest`、`participant_profile_update`；每 step 一个 `RequestHeader` |
| 3 | **checkpoint①（事实确认）** | **结构化用户提问 seam**：`RequestUserInput` 事件 + `Op::UserInputAnswer`；暂停当前 turn | log-only `CheckpointOpened` → surface `UserMessage{source: checkpoint}` → log-only `CheckpointAnswered` |
| 4 | **model_build** 参与人筛选 + GameSpec 构建 | turn/step | 工具 `gamespec_build`、`gamespec_validate`；产物 `GameSpecValidated` |
| 5 | **checkpoint②（假设确认）** | 同 3 | 假设清单逐条 `source ∈ {user_confirmed, public_data, prior_borrowed, llm_inferred}` |
| 6 | **solve** 均衡枚举 + 精炼 | 单次确定性工具调用（**不经沙箱**，§3.7） | 工具 `solve_equilibria` → 产物 `EquilibriumSet` |
| 7 | **select** 均衡选择引擎 → 概率分布 | 多个选择器工具调用 + 融合 | 工具 `select:{risk_dom,stochastic,learning,behavioral,focal_llm,persona_sim}`、`select_fuse`；log-only `SelectionWeights` |
| 8 | **checkpoint③（结果预览）** | 同 3 | 用户可回退到 4/6/7 |
| 9 | **recommend** 偏好注入 + 条件策略 + 敏感性/翻转阈值 | turn/step（可能调用 Swarm） | 工具 `preference_inject`、`recommend_build`、`sensitivity_scan`、`monte_carlo` |
| 10 | **report** 图文报告生成 | turn/step + **sidecar 进程**渲染 | 工具 `report_render`（产出结构化 `ReportModel` JSON）→ sidecar（matplotlib）→ 外部 CLI（pandoc） |
| 11 | **archive** 案例入库 | 工具调用 + 日志封存 | `case_store_write` → log-only `CaseArchived`；`rollout` 从 `cases/` 移入 `archived_cases/` |

**Swarm 的映射（v0.1 §5.7）**：`persona_sim` 选择器与非常规反应生成走 Codex 多智能体基础设施——`ThreadManager::spawn_subagent` 派生 N 个 persona 子会话（`AgentRoleConfig` 定义角色），orchestrator 是**纯代码**（不占 turn），rollout 轨迹以 `CollabAgentSpawnBegin/End`、`CollabAgentInteraction*`、`SubAgentActivity` 事件入日志，商奕再投影出 `SwarmRollout` 领域事件（§8.2）。

### 5.3 术语映射表

| harness 术语（Codex/dsh） | 商奕概念（v0.1 文档） | 说明与差异 |
|---|---|---|
| `preset`（dsh）/ `playbook` | **剧本**（v0.1 §3.2） | 商奕采用 **`playbook`** 命名（与 v0.1 一致），语义取 dsh preset 的"行列表 + 整行替换层栈"，但**不采用** dsh 的 scope/单次挂载机制（GB 02 §2 Rust 落地：`Arc<T>` 每 agent 实例化一份即可） |
| `thread` / `session` | **咨询案例**（case） | Codex 的 `Thread` = dsh 的 `Session` = 商奕的 case。商奕用 `case` 作为用户可见词，内部沿用 `ThreadId`（复用 Codex 类型，避免重造） |
| `turn` / `step` | **交互轮 / 步** | step = 一次模型调用 + 它请求的工具执行；turn = 0..n 个 step（GB 02 §4）。商奕的**阶段**（phase）跨多个 turn |
| `tool`（`ToolSpec`/`ToolExecutor`） | **工具**（v0.1 §4） | 领域工具集替换编码工具（D-A11）；wire 名见 §7.2 |
| `subagent`（`spawn_agent`） | **参与者子智能体**（persona sub-agent，v0.1 §5.7） | 一个 persona = 一个**可续子会话**；`maxDepth` 界定递归；持久描述符使崩溃后恢复**同一批** persona |
| `approval` / `approval_policy` | **checkpoint 确认**（v0.1 §8.3） | ⚠️ **非同一机制**：Codex 的 approval 是**权限升级**（沙箱外动作需批准），商奕的 checkpoint 是**语义闸门**（假设必须人确认）。两者共用"暂停-询问-结算-留档"的形状与 fail-closed 语义，但归属不同的 seam（§7.6） |
| `rollout`（JSONL 会话日志） | **案例留档** | Codex `RolloutRecorder` + `SESSIONS_SUBDIR`；商奕落在 `$SHANGYI_HOME/cases/`（§8.3） |
| `skill`（`SKILL.md` + frontmatter） | **方法论卡片** | 跨工具 Agent Skills 约定（GB 02 §12）；商奕的理论库条目与方法论卡片可共用该载体，使领域专家可编写 |
| `hooks`（12 个事件名） | **剧本钩子 / 阶段闸门** | 商奕的**承重**钩子是 `agent/pre-step` 与 `agent/request`（dsh 的 waterfall seam，GB 02 §6）——Codex 的 12 个 hook 名中**没有**这两个，落点见 §7.4 |
| `compact` / `ContextCompacted` | **上下文压缩** | 长咨询（多轮 Swarm）必然触发；先移植 dsh 的**确定性剪枝器**（无模型），后接摘要器（GB 02 §7） |
| `EventMsg`/`Op` | **（协议层）** | 商奕领域事件另建同层类型，不污染 `codex-protocol`（GB 01 §10 复用地图："领域事件类型另建同层 crate"） |
| `AgentRoleConfig` | **persona 角色卡**（v0.1 §6.2 画像 → 角色卡） | `[agents] roles.<name> { description, config_file, nickname_candidates }` 与画像卡字段需映射；persona 需"自足覆盖"（`complete: true` 语义），否则会继承全局助手框架（GB 02 §9） |
| `deny.toml`（cargo-deny） | **依赖许可合规** | `直接复用` Codex 的白名单策略（GB 01 §0） |

### 5.4 边界规则

**规则 1（依赖方向）**：`shangyi-*` crate **只能**依赖 `vendor/codex-*` 的 **public** API。

```
允许：shangyi-core → codex_core::{ThreadManager, CodexThread, NewThread, StartThreadOptions,
                                  TurnContext, RolloutRecorder, SessionMeta, Event, EventMsg, Op}
允许：shangyi-core → codex_protocol::*, codex_config::*, codex_sandboxing::*, codex_tools::*
禁止：shangyi-* → codex_core::session::*          （pub(crate)，不可达）
禁止：shangyi-* → codex_core::tools::registry::*  （pub(crate)，不可达）
禁止：shangyi-* → codex_core::tasks::*            （mod tasks; 私有）
```

违反检查：CI 中 `cargo xtask depcheck` 读 `Cargo.toml` 依赖图 + 一个 `grep` 规则集（禁止 `shangyi-*` 依赖任何 `vendor/codex/codex-rs` 下非白名单 crate）。

**规则 2（patch 最小化与可度量）**：

| 要求 | 判据 |
|---|---|
| 最小 | patch 集目标 ≤ 40 个文件、≤ 1000 行改动 `[目标值，非事实]`；超出需 ADR 记录 |
| 逐个文档化 | 每个 patch 有编号文件 `vendor/patches/NNNN-<slug>.patch` + 一行 reason + 关联 D-A 编号 |
| 逐文件变更声明 | 每个被改文件头部注释（§4.6 模板），由 `cargo xtask attribution` 校验覆盖率 **100%** |
| 可 rebase | CI 定期对上游 `main` 执行 `git apply --check` 全部 patch；失败即红灯（这不是测试，是**维护性保险**） |
| 不放领域逻辑 | `vendor/` 内**禁止**出现 `shangyi` 语义（如博弈论术语）。领域逻辑只能出现在 `shangyi-*`。**唯一例外**：D-A12 的 hook 注册点与 D-A5 的 wire 适配器，二者允许存在"接线"但不允许存在"策略" |

**规则 3（新增能力的优先顺序）**：遇到新需求时按此顺序选择实现位置，**不允许跳级**：

```
① shangyi-* 新 crate / 新工具（首选，零 patch 成本）
② 配置 / 剧本 YAML（改为数据）
③ 现有 public API + hook / MCP（Codex 支持的扩展路径）
④ 修改 vendor/ 的最小补丁（必须走 ADR + patch 登记）
⑤ fork 出新的 vendor crate（最后手段，会永久增加 rebase 成本）
```

**规则 4（协议类型的所有权）**：`codex-protocol` 的 `Op`/`EventMsg` 是 `#[non_exhaustive]`，商奕**不**在其中新增领域变体（那会污染 patch 面并使 rebase 困难）。领域事件与领域 Op 全部落在 `shangyi-core::session::event`（§8.2），通过**已公开**的 `request_user_input` 与 `dynamic_tools` 机制与基座通信。

---

## 6. Rust Workspace 结构

### 6.1 目录树

```
shangyi-agent/
├── Cargo.toml                     # [workspace] resolver = "2"
│                                  # members = ["vendor/codex/codex-rs/*", "shangyi/crates/*"]
├── Cargo.lock                     # 单一锁文件（vendor 与 shangyi 共享依赖解析）
├── rust-toolchain.toml            # ⚠️ 见下方"工具链"注：D-L2 决定不钉；上游随附的 codex-rs/rust-toolchain.toml（1.95.0）已按 W1d 删除
├── deny.toml                      # 许可白名单 + bans（§4.5）
├── justfile                       # set working-directory := "vendor/codex/codex-rs"（沿用上游 recipe）
├── LICENSE                        # Apache-2.0 全文 + 商奕版权行
├── NOTICE                         # §4.7 骨架
├── ATTRIBUTION.md                 # patch 索引（由 cargo xtask attribution 生成）
├── COMPLIANCE.md                  # 许可合规说明（§4 的可分发版本）
│
├── vendor/
│   ├── VendorPatches.md           # §4(b) 变更登记（P-000x：改了什么、为什么、回滚方式）——合规凭据
│   ├── .shangyi-modified          # 被改文件清单（CI 反向核对的权威表；路径在 vendor/ 根，不在 codex-rs/ 内）
│   ├── patches/                   # **商奕补丁集**（计划中的最小补丁集；0001–0010，0003 拆 a/b/c）
│   │   ├── 0001-wire-api-chat.patch              # D-A5  model-provider-info + config
│   │   ├── 0002-chat-completions-client.patch    # D-A5  codex-api + model-provider
│   │   ├── 0003a-remove-code-mode-ws.patch       # D-A4a workspace 清册（只改 manifest）
│   │   ├── 0003b-decouple-code-mode-core-tools.patch   # D-A4a core + tools 源码解耦
│   │   ├── 0003c-decouple-code-mode-rollout-app.patch  # D-A4a rollout-trace + app-server 解耦
│   │   ├── 0004-remove-openai-account.patch      # D-A4b workspace + core-api + core-plugins + models-manager + rmcp-client + app-server + cli
│   │   ├── 0005-shangyi-home.patch               # D-A15 utils/home-dir + codex-home + config
│   │   ├── 0006-domain-tool-sources.patch        # D-A11 core/src/tools/spec_plan.rs + handlers
│   │   ├── 0007-agent-pre-step-hook.patch        # D-A12 core/src/hook_runtime.rs + hooks/src
│   │   ├── 0008-models-json-instructions.patch   # §6.5  models-manager + protocol
│   │   ├── 0009-bin-name-shangyi.patch           # §4.3  cli + arg0 + install-context
│   │   └── 0010-domain-doc-filename.patch        # §6.2B core/src/agents_md.rs + config
│   └── codex/                     # fork 自 github.com/openai/codex（Apache-2.0）——**完整镜像上游仓库层级**
│       ├── LICENSE                # 上游 Apache-2.0 全文
│       ├── NOTICE                 # 上游归属声明（§4.6（1）的**字节级真值**，242 B；CI 逐字节比对）
│       ├── codex-rs/              # Rust workspace 根（上游 `members` 149 项 → 已裁剪至 138）
│       │   ├── Cargo.toml  Cargo.lock
│       │   ├── core/              # `修改后复用`（账号依赖解耦 P-0004、领域工具源 P-0006、hook P-0007；⚠️ 原计划的 Code Mode 源码解耦 P-0003b **Phase B 实测判定为非必需**，见附录 D 清单第 3b 条）
│       │   ├── core-api/          # `直接复用`
│       │   ├── protocol/          # `直接复用`
│       │   ├── tools/             # `直接复用`（wire 层原语；注意与上层的 `vendor/codex/tools/` 区分）
│       │   ├── app-server*/       # `直接复用`
│       │   ├── config/ config-schema/ features/          # `直接复用`
│       │   ├── codex-api/ codex-client/ http-client/     # `修改后复用`（+Chat 客户端）
│       │   ├── model-provider/ model-provider-info/      # `修改后复用`（WireApi::Chat）
│       │   ├── model-provider/    # `直接复用`
│       │   ├── models-manager/    # `修改后复用`（models.json 内容替换，§6.5）
│       │   ├── prompts/           # `修改后复用`（领域 instructions 模板接点）
│       │   ├── rollout/ rollout-trace/ history/ state/   # `直接复用`
│       │   ├── sandboxing/ linux-sandbox/ bwrap/ execpolicy/ process-hardening/  # `直接复用`
│       │   ├── hooks/ skills/ plugin/ core-plugins/ ext/*  memories/  # `直接复用`
│       │   ├── agent-roles/ agent-graph-store/ collaboration-mode-templates/  # `直接复用`
│       │   ├── utils/home-dir/    # `修改后复用`（$SHANGYI_HOME 的真实落点：find_codex_home()）
│       │   ├── codex-home/        # `修改后复用`（兼容层，仅 1 个导出；见 §6.2 表 B）
│       │   ├── ~~cli/  tui/~~      # **已随 Tier 1 裁剪删除**（2026-09-17 实测：`cli`、`tui` 不再在 `members` 中、目录已删）。⚠️ 本行与 §6.2 表 B 的 `cli` 行（patch 0009）及 §5.1/§6.3 的 `codex-tui` 引用**尚未对齐**，登记为待跟进项（附录 D 清单第 2 条）
│       │   ├── utils/* async-utils/ build-info/ git-utils/  # `直接复用`
│       │   ├── ansi-escape/       # **`待定`**（§6.2 D 段 24 项之一；不得当作已决）
│       │   └── …（其余见 §6.2 处置表）
│       ├── patches/               # 上游构建侧补丁（**30 个 `.patch`**，其中 **6 个** V8 专属 → 非 V8 **24 个**；§6.2 C 表）
│       ├── third_party/           # 上游第三方源码（v8 随 Code Mode 裁剪；其余按计划 §5 处理）
│       ├── tools/  docs/  bazel/  MODULE.bazel  justfile  …   # 上游 repo 根资产（**不可与 codex-rs/ 下的同名目录合并**，见下方警示）
│       └── …（其余为上游 repo 根的其余条目）
│
├── shangyi/
│   ├── crates/
│   │   ├── shangyi-gamespec/      # L0
│   │   ├── shangyi-playbooks/     # L0
│   │   ├── shangyi-solver/        # L1
│   │   ├── shangyi-kb/            # L1
│   │   ├── shangyi-intel/         # L1
│   │   ├── shangyi-report/        # L1
│   │   ├── shangyi-selection/     # L2
│   │   ├── shangyi-swarm/         # L2
│   │   ├── shangyi-consequence/   # L2
│   │   ├── shangyi-discover/      # L2
│   │   ├── shangyi-tools/         # L3
│   │   ├── shangyi-core/          # L3
│   │   └── shangyi-cli/           # L3（二进制 `shangyi`，default-run）
│   ├── playbooks/                 # 剧本 YAML（v0.1 §3.2 的落点）
│   │   ├── pricing_response/{playbook.yml, agent.plugins.yml, report.tmpl.md}
│   │   ├── competitor_intel/…
│   │   ├── negotiation/…
│   │   └── market_entry/…
│   ├── prompts/                   # 领域 prompt 段（persona / 阶段指引 / 受控 prompt）
│   │   ├── base.md                # → model_instructions_file
│   │   ├── persona/*.md           # 每个 persona 一套 prefix/suffix（"complete" 语义）
│   │   ├── focal_point.md         # focal_llm 选择器的受控 prompt（温度 0）
│   │   └── stages/{intake,intel,model_build,solve,select,recommend,report}.md
│   ├── models.json                # 替换 vendor models-manager/models.json（§6.5）
│   ├── skills/                    # 方法论卡片（SKILL.md + frontmatter）
│   ├── kb/                        # 理论库策展 markdown（v0.1 §7.1，29–35 篇）
│   └── catalog/                   # 参数先验库 / 案例库 schema
│
├── sidecar/                       # Python 侧车（不进发货二进制）
│   ├── pyproject.toml
│   ├── plot/                      # 结构化图数据 JSON → matplotlib → PNG/SVG
│   ├── ci/oracle/                 # nashpy 差分 oracle（仅 CI）
│   └── sandbox_runner/            # 沙箱入口（失败即拒绝，§3.7）
│
├── tests/
│   ├── golden/                    # 囚徒/性别战/斗鸡/Cournot/Young 惯例/星宇原型
│   ├── differential/              # 与 nashpy 的解集集合比较
│   └── e2e/                       # 端到端 mock 咨询
└── docs/                          # 本文与 v0.1 文档
```

**vendor 布局（Phase B 实测，2026-09-17）**：vendor 树是 **`vendor/codex/`** —— 上游仓库的**完整镜像**，Rust workspace 在 **`vendor/codex/codex-rs/`**。

> **为什么不能扁平化成 `vendor/codex-rs/`（已踩过的坑，务必不要重犯）**：上游 **repo 根**与 **`codex-rs/`** 下**都存在 `tools/` 与 `docs/` 目录**。把这些平铺进同一个 `vendor/codex-rs/` 时，两次复制互相覆盖 —— **repo 根的 `tools/` 会覆盖 `codex-rs/tools/`，把 `codex-tools` crate 的内容替换成上游的构建脚本目录**（同时 `docs/` 也被覆盖、repo 根的 `LICENSE`/`NOTICE` 在重组中丢失）。这类破坏**不会在 `cargo metadata` 阶段暴露**（manifest 仍在），但会让 `codex-tools` 编译失败或产出错误语义。因此：**保持上游原始层级，任何"整理目录"的冲动都以本段为依据否决**。所有路径引用一律写全 `vendor/codex/codex-rs/<crate>/…`。
>
> 规模实测：17,437 文件 / 8,746 个 `.rs` / 129 MB；`vendor/codex/` 与上游 tarball 逐文件比对为 **8,245 文件、0 差异、0 大小不符**，唯一删除是 `codex-rs/rust-toolchain.toml`（登记为 `P-0001`，见 `vendor/VendorPatches.md`）。
>
> **工具链注（D-L2 + W1d）**：项目根**不再**放置钉版 `rust-toolchain.toml`；上游随附的 `codex-rs/rust-toolchain.toml`（`channel = "1.95.0"`）**已删除** —— 否则 rustup 的"向上查找最近文件"规则会让 `vendor/` 内构建被强制切到 1.95.0，而项目根用 1.98.0。删除后需实测复核：`find vendor -name 'rust-toolchain*'` 无输出。**这不是"随 Codex"**：D-A1 的"随 Codex"指语言与 edition 基线，不指钉死版本号。

### 6.2 `vendor/codex/codex-rs/*` 处置表

**处置标签**：A `直接复用` / B `修改后复用` / C `移除` / **D `待定`**（本表新增桶）。A–C 描述的是"进入商奕 workspace 后的形态"，与 §1.5 的四种复用处置口径不同；**D 桶的存在是刻意的** —— 计划实测 149 个成员中有 24 项在 W1 收敛前**无法判定**，它们**不属于** A/B/C 任何一桶，详见 D 段。

> **本节所称"计划"** = `research/harness/11-vendor-plan.md`（工作项 `W1-vendor-plan` 的产物，**处置执行表**；本节是**处置意图表**）。该计划逐项断言的上游事实来自 `codex-rs/Cargo.toml`（149 个 `members`）与 `main` 分支全量 tarball 的反向依赖查询，**凡与本节不一致处，以计划为准**（§1.5 的标签读法同样适用）。**表中所有 crate 路径都位于 `vendor/codex/codex-rs/` 下**（布局与理由见 §6.1 末尾的"vendor 布局"注；本节与全篇的旧写法 `vendor/codex-rs/` 已统一订正）。

**A. 直接复用（零改动）**

| crate | 作用 | 许可 |
|---|---|---|
| `app-server-protocol`、`app-server-protocol-noop-macros`、`app-server-transport`、`app-server-client`、`app-server-daemon`、`app-server-test-client` | JSON-RPC 嵌入面、threads/turns、通知、TS schema 导出 | Apache-2.0 |
| `features`、`config-schema` | 特性开关；schema 生成工具（0 Cargo 依赖者，保留成本极低） | Apache-2.0 |
| `exec-server`、`exec-server-protocol`、`exec-server/tests/support` | **`core` 的环境抽象基座**：`EnvironmentManager` 是 `ThreadManager` 的构造参数；计划实测 **18 个 manifest** 依赖 `codex-exec-server` —— **保留，不替换**（见下方更正 ②） | Apache-2.0 |
| `sandboxing`、`linux-sandbox`、`bwrap`、`mxc-sandbox`、`execpolicy`、`process-hardening`、`shell-command`、`shell-escalation`、`network-proxy` | Seatbelt/Landlock/bwrap、Starlark 策略引擎 | Apache-2.0 |
| `rollout`、`history`、`state`、`thread-store`、`attachment-store`、`codex-experimental-api-macros`、`context-fragments` | JSONL 会话日志 + SQLite 索引 + 元数据 | Apache-2.0 |
| `hooks`、`skills`、`plugin`、`memories/read`、`memories/write`、`ext/extension-api`、`ext/items`、`ext/skills`、`ext/memories`、`ext/mcp`、`ext/queue`、`ext/history-notes`、`ext/goal`、`ext/agent`、`file-search` | 扩展机制 | Apache-2.0 |
| `agent-roles`、`agent-graph-store`、`collaboration-mode-templates` | 多智能体：`AgentRoleConfig`、父子拓扑、协作模式模板 | Apache-2.0 |
| `codex-mcp` | MCP 客户端（⚠️ 依赖 `connectors` —— 待定 D2；若最终移除 `connectors`，`codex-mcp/src/runtime.rs:22-23`、`client_tool_catalog.rs:13`、`lib.rs:36` 需在 P-0006 内附带解耦） | Apache-2.0 |
| `codex-client`、`http-client`、`websocket-client` | 重试、SSE、`ReqwestTransport`；`http-client` 是**唯一**允许直接依赖 `reqwest` 的 crate（`deny.toml` bans） | Apache-2.0 |
| `otel`、`build-info`、`diagnostics`、`install-context`、`response-debug-context`、`terminal-detection`、`uds`、`stdio-to-uds` | 可观测性与运维 | Apache-2.0 |
| `file-system`、`file-watcher`、`utils/pty`、`utils/rustls-provider`、`utils/plugins`、`utils/image`、`async-utils`、`git-utils`、`utils/absolute-path`、`utils/path-uri`、`utils/path-utils`、`utils/cache`、`utils/git-discovery`、`utils/json-to-toml`、`utils/redacted-string`、`utils/string`、`utils/cli`、`utils/output-truncation`、`utils/stream-parser`、`utils/template`、`utils/cargo-bin`、`test-binary-support` | L0 叶子与测试夹具 | Apache-2.0 |

> **口径（相对本表旧版的三处收紧）**：本表按计划 §1.5.1 实测的 **79 项"直接复用"** 归并成组；**"零改动"是实测结论，不是意图**。① 原表中 `protocol`、`config`、`tools`、`model-provider`、`core-plugins`、`rollout-trace`、`app-server` 被记为"零改动"，但计划 §1.5.2 实测它们**必须打补丁**，故已移入 B 表；② `ext/*` **不是**整组直接复用 —— `ext/web-search`（D8）、`ext/guardian-reviewer`、`ext/guardian-v2`、`ext/git-attribution`、`ext/connectors` 属**待定**（D 段），`ext/image-generation` 属**移除**（C 表）；③ `utils/home-dir` 在 B 表（`$SHANGYI_HOME` 的真实落点），`ansi-escape`、`mermaid`、`utils/elapsed`、`utils/sleep-inhibitor`、`utils/approval-presets`、`utils/fuzzy-match`、`utils/sandbox-summary`、`utils/audio`、`utils/oss`、`utils/readiness` 属**待定**（D 段）。逐项权威清单以计划 §1.5.1/§1.5.2 为准。

**B. 修改后复用（必须登记 patch + §4(b) 变更声明）**

> 本表成员集与计划 §1.5.2 实测的 **18 项**逐项一致（`workspace Cargo.toml` 为非成员项，单列）。补丁号沿用计划 §2.0 的约定：`0003` 已拆为 **0003a/0003b/0003c**，**不要**再引用未拆分的旧 `0003-remove-code-mode.patch`。

| crate | 修改内容 | patch | 依据 |
|---|---|---|---|
| `model-provider-info` | `WireApi` 增 `Chat` 变体；删 `CHAT_WIRE_API_REMOVED_ERROR` 拒绝路径；`validate()` 按 wire 校验；内置 provider 表裁剪 | 0001 | D-A5 |
| `codex-api` | 新增 `ChatCompletionsClient`（Responses↔Chat 翻译，新文件 `src/chat_completions.rs`） | 0002 | D-A5 |
| `model-provider` | `create_model_provider` 按 `WireApi` 分派；`ProviderCapabilities` 增 `chat_wire`/`reasoning_content`/`parallel_tool_calls` | 0002 | D-A5 |
| `core` | 五处：① **Code Mode 解耦**（删 `core/src/tools/code_mode/` 整目录、`thread_manager` 去 provider 参数、`registry`/`context`/`executed_tool_calls` 去 `CellId` 与缓存字段、`spec_plan` 去注册函数与分支 —— 实测 **488 处 `code_mode` 文本命中、52 个非测试源文件**）；② 账号依赖解耦（P-0004）；③ `spec_plan::add_core_tool_sources` 挂载领域工具源（P-0006）；④ `hook_runtime.rs` 注册 `agent/pre-step`/`agent/request`（P-0007）；⑤ 项目文档文件名（P-0010） | 0003b、0004、0006、0007、0010 | D-A4a、D-A4b、D-A11、D-A12、§6.2B |
| `tools` | **Code Mode 解耦**：删 `codex-code-mode` 依赖与 `src/code_mode.rs`（200 行）+ `lib.rs` 的 5 个 `pub use`（消费者 `core/src/tools/code_mode/execute_spec.rs` 随上一行一起消失） | 0003b、0006 | D-A4a、D-A11 |
| `rollout-trace` | **Code Mode 解耦**：`codex_code_mode::PUBLIC_TOOL_NAME` 换本地常量 `const PUBLIC_TOOL_NAME: &str = "exec"`（**保留字符串**：历史 rollout 里的 `exec` 调用仍需被 reducer 识别并按"未知工具"降级，不得改历史） | 0003c | D-A4a |
| `app-server` | **Code Mode 解耦**：去掉 `CodeModeSessionProvider` 注入（`lib.rs:5-6`、`message_processor.rs:76`）与其配置键；删账号相关 JSON-RPC 方法 | 0003c、0004 | D-A4a、D-A4b |
| `cli` | 对外二进制名 `shangyi`、`default-run`、裁剪 `login`/`logout`/`cloud` 等子命令 | 0009、0004 | §4.3、D-A4b |
| `arg0` | 沙箱 helper 别名硬编码 `codex-linux-sandbox` → `shangyi-linux-sandbox`（`arg0/src/lib.rs:63-64,285-287,657`）。**必须与 `cli` 改名同一补丁**，否则 Linux 沙箱静默不可用（macOS 开发机不暴露） | 0009 | §4.3 |
| `utils/home-dir` | **`$SHANGYI_HOME` 的真实落点**：`find_codex_home()` 的环境变量名与 `~/.codex` 默认值（建议保留函数名、只改函数体，实现 `SHANGYI_HOME` → `CODEX_HOME` → `~/.shangyi` 双读兼容，避免历史会话目录"消失"） | 0005 | D-A15 |
| `codex-home` | **兼容层**：全 crate 仅 2 个文件、1 个导出（`CodexHomeUserInstructionsProvider`）；其指令装载路径需指向商奕 home | 0005 | D-A15 |
| `config` | `thread_config/remote.rs` 的 proto `WireApi` 映射增 `Chat`（改 `.proto` 源并重跑 `tonic-prost-build`，**不手改生成文件**）；`project_doc_*` 键保持 | 0001、0005 | D-A5、D-A15 |
| `prompts` | 领域 instructions 接点（不写死商奕文案，文案在 `shangyi/prompts/`） | 0008 | §6.5 |
| `models-manager` | 解耦 `codex-login`（`AuthManager` 参数改为 `Arc<dyn CredentialProvider>`）；`models.json` 与 `prompt.md` 内容替换 | 0004、0008 | D-A4b、§6.5 |
| `protocol` | `src/prompts/base_instructions/default.md`（与 `models-manager/prompt.md` **逐字节相同**，20903 B）替换；`models.rs::BASE_INSTRUCTIONS_DEFAULT` 语义保持 | 0008 | §6.5 |
| `rmcp-client` | 解耦 `codex-keyring-store`（`src/ema_identity.rs:11-12`、`oauth/store_lock.rs:17`、`enterprise_oauth_login.rs:12`） | 0004 | D-A4b |
| `core-api` | 纯 `pub use` facade：删/改 `AnalyticsEventsClient`、`AuthManager`/`CodexAuth`/`AuthHeaders`/`ExternalAuth*`、`install_image_generation_extension`、`set_default_originator` 等 re-export | 0004 | D-A4b |
| `core-plugins` | 解耦 `analytics` + `login`（+ 视 #D2 结果解耦 `connectors`） | 0004 | D-A4b |
| workspace `Cargo.toml` | `members` 与 `[workspace.dependencies]` 清册（**含 3 条隐式成员**：`codex-chatgpt`/`codex-message-history`/`codex-windows-sandbox`）；`[workspace.metadata.cargo-shear] ignored` 删 `codex-v8-poc`（**保留** `icu_provider`、`openssl-sys`） | 0003a、0004、0009 | D-A4a、D-A4b |

> **`tui` 的归属（本表唯一未按计划收敛的行）**：旧版把 `tui` 列为"修改后复用（裁剪；保留）"。计划 §1.3/§1.7 实测结论是 **`tui` 应移除**并已计入"移除 28 项"（它是移除组 B 的最大单一消费者：依赖 30+ 内部 crate，含 `login`、`cloud-config`、`backend-client`、`feedback`、`realtime-webrtc`；保留它会让半数账号体系 crate 无法删除），商奕前端自研。本行**本轮未做全文改写**：把 `tui` 改为"移除"会连带改写 §5.1 平面 C、§6.1 目录树、§6.3 `shangyi-cli` 的（可选）`codex_tui` 依赖与 §4.1 的 Ratatui 行，超出本次修订的边界，**已登记为待跟进项**。**在跟进项结算前，按"计划优先"规则，裁剪执行以计划 §1.7 的移除处置为准（`tui` 不进入 workspace）**。注意 §4.6（1）的 `NOTICE` 复现义务**与 `tui` 去留无关**。

**C. 移除（不进入 workspace）**

| crate / 模块 | 移除理由 | 依据 |
|---|---|---|
| `code-mode`、`code-mode-host`、`code-mode-protocol`、`code-mode-runtime`、`v8-poc`（5 个成员） | V8 构建重量 + 与铁律 1 的数值可信性目标冲突。⚠️ **代价不是"删 5 个目录"**：`codex-code-mode` 被 **4 个保留 crate** 直接依赖（`core`、`tools`、`rollout-trace`、`app-server`），每个都要源码手术（见 B 表）→ 补丁拆为 **0003a（manifest 清册）/ 0003b（`core`+`tools` 解耦）/ 0003c（`rollout-trace`+`app-server` 解耦）**。另：移除 Code Mode 是替换 `models.json` 的**前置条件**（上游 9 条内置模型里 7 条 `tool_mode = "code_mode_only"`） | D-A4a |
| V8 构建侧资产：`patches/` 下 **6 个** V8 专属补丁 + `third_party/v8/` **整目录** + `MODULE.bazel` 的 V8 段 + `justfile` 的 `code-mode-host` / `bazel-code-mode-host` recipe | 上游实测：6 个文件是 `llvm_rusty_v8_custom_libcxx`、`rules_cc_rusty_v8_custom_libcxx`、`rusty_v8_prebuilt_out_dir`、`v8_bazel_rules`、`v8_module_deps`、`v8_source_portability`；`MODULE.bazel` 需处理 `v8_targets` 扩展（542-544）、`rusty_v8_*` 环境注入（549-554）、`crate = "v8"` 条目（556-569）、`llvm`/`rules_cc` 的两行补丁挂载（15-20、154）。**`patches/` 下其余 24 个与 V8 无关**（服务 MSVC/gnullvm、压缩库、jemalloc）—— **只删上面 6 个**。**计数口径**：`vendor/codex/patches/` 下 `.patch` 文件共 **30 个**，V8 专属 **6 个** → 非 V8 = 30 − 6 = **24 个**（另有 `BUILD.bazel` 一个非 `.patch` 文件，不计入；旧文写作"25 个"是把它误算进来了） | D-A4a |
| `login`、`keyring-store`、`backend-client`、`analytics`、`cloud-tasks`、`chatgpt`（隐式成员） | OpenAI 账号体系耦合 | D-A4b |
| `cloud-config`、`realtime-webrtc`、`external-agent-migration`、`feedback` | OpenAI 云/realtime/迁移专用，商奕不使用 | `[推断]`（GB 01 §1 成员表 + D-A4b 精神） |
| `image-generation`（`ext/image-generation`、`imagegen` 工具） | 咨询报告不用生成图；`view_image` 工具一并移除 | D-A8/D-A11 |
| `exec`（二进制，唯一依赖者是 `cli`）、`otel-trace-websocket`（唯一依赖者是 `code-mode-host`） | 面向编码 agent 的对外 exec 面 / Code Mode 孤儿项；领域工具不经过它 | `[推断]`、计划 §1.1.2(e) |
| `ollama`、`lmstudio`（与 `utils/oss` 三元组，见 D14） | 本地 provider 引导；商奕用 API provider（保留代码亦可，列此处为可裁） | GB 01 §10（"可选/可删"） |
| `aws-auth`、`workload-identity`、`agent-identity` | Bedrock/工作负载身份；商奕用 API key + 环境变量凭据层 | D-A4b 精神 `[推断]` |

> **更正 ①（Code Mode 的成本）与更正 ②（`exec-server`）**：① Code Mode 的移除**不能**再表述为"删除 5 个成员 + `patches/rusty_v8_*`"（旧版 C 表的写法已证实过于乐观，见计划 §1.1.2 与 `⚠️ 与 §6.2 的偏差 #1`）。② **`exec-server`、`exec-server-protocol` 从本表移出**：旧版依据 `[推断]` 判它们可移除，计划实测反驳 —— `codex-exec-server` 是 `core` 的环境抽象基座，**18 个 manifest** 依赖它（`app-server-client`、`app-server`、`arg0`、`cli`、`core`、`core-api`、`codex-mcp`、`ext/mcp`、`ext/skills`、`rmcp-client`、`utils/plugins` 等），删除等于重写 core 的环境层。**若要收窄面，用"只走 `LOCAL_ENVIRONMENT_ID`"的配置约束，不要删代码**（§6.4 的"零改动优于改逻辑"）。
>
> **本表不是 28 项移除清单的完整枚举** —— 它只列出设计文档原有条目 + 本轮更正的条目。**逐字完整清单以计划 §1.7 为准**（28 项：另含 `cloud-tasks-client`、`cloud-tasks-mock-client`、`responses-api-proxy`、`voice-host`、`codex-backend-openapi-models` 等），裁剪 runbook 按该清单做 `members` 的**字符串精确匹配**删除。同理，A 表的 79 项与 B 表的 18 项也以计划 §1.5.1/§1.5.2 的逐项枚举为准。
>
> **不得随 Code Mode 一起删的外部依赖（常见误判）**：`tonic`、`tonic-prost`、`tonic-prost-build`、`prost` **必须保留** —— `config`（`Cargo.toml:33,45,46,71,72`，含 protobuf 生成的 `thread_config/proto/`）与 `exec-server`（`Cargo.toml:45` 用 `prost`）也在用。可随 Code Mode 删除的只有 `v8`、`deno_core_icudata`、`protoc-bin-vendored`（唯一使用者是 `code-mode-protocol` 的 `[build-dependencies]`；删除前先 `cargo build -p codex-config` 验证 `config` 的 protoc 来源，该项为 `[未取得]` U5）。

**D. 待定与未取得 —— 不得静默升级为已决**

计划以脚本对 149 个成员逐项断言，处置为：**直接复用 79 / 保留但需补丁 18 / 移除 28 / 待定 24**。**这 24 项待定与 12 项 `[未取得]`（U1–U12）在收敛前不得被当作已决**；本表 A/B/C 未列出它们，不等于它们已被处置。

| 类别 | 内容 | 收敛动作 |
|---|---|---|
| **24 项待定**（成员目录） | `ansi-escape`、`apply-patch`、`connectors`、`ext/connectors`、`ext/git-attribution`、`ext/guardian-reviewer`、`ext/guardian-v2`、`ext/web-search`、`guardian-context`、`mermaid`、`secrets`、`tcp-tunnel`、`thread-manager-sample`、`user-verification`、`utils/approval-presets`、`utils/audio`、`utils/elapsed`、`utils/fuzzy-match`、`utils/oss`、`utils/readiness`、`utils/sandbox-summary`、`utils/sleep-inhibitor`、`windows-sandbox-service`、`worktree` | 计划 §1.6 的 **D1–D14** 为每一项给出"能在 W1 第一天用一个可执行动作收敛"的判定问题（读一个文件 / 跑一条 `cargo tree` / 问一个产品问题）。**不允许"再想想"**；收敛后必须回填计划 §1.7 与 §7.1 V2 的实数（最终成员数会低于 149−28=121） |
| **12 项 `[未取得]`（U1–U12）** | 计划 §8.2：`user-verification` 用途（U1）、`connectors` 公开面（U2）、`guardian-*` 完整耦合面（U3）、`ext/web-search` 性质（U4）、`config` 的 protoc 来源（U5）、`exec-server` 环境抽象完整性（U6）、**上游 commit SHA（U7）**、构建实测数字（U8）、裁剪后 `cargo deny` 输出（U9）、MPL 残留（U10）、上游 `AGENTS.md` 影响（U11）、`vendor/bubblewrap/` 接线（U12） | 引用这些事项时必须保留 `[未取得]` 标注，不得改写为结论 |
| **最高风险的待定（计划点名）** | `guardian-context` / `ext/guardian-reviewer` / `ext/guardian-v2`（D3）：`core/src/tools/spec_plan.rs:973-990` 用 `crate::guardian::is_basic_session_source()` 做**早退** —— 在 basic session source 下**直接不注册任何工具**（`add_core_tool_sources` 整段跳过），**且不报错**。若商奕替换工具源时理解错这条分支，会话会"一个工具都没有"却静默通过（违反不变量 ②） | W1 第一天动作：`grep -rn "guardian" core/src --include=*.rs \| wc -l` + `cargo tree -i codex-guardian-context`；替换 `add_core_tool_sources` 时**必须**先判定这条早退的去留 |
| **执行前置（`D8` 固定策略）** | **U7：上游 `main` 的 commit SHA 未取得**（tarball 不含 `.git`）。`D8` 要求 vendor 固定 commit —— 因此**在取到并记录具体上游 commit SHA 之前，不得开始 vendoring/裁剪**（计划 §5 步骤 0 与本表是同一约束的两处表述） | `git clone` 或 GitHub API 取 SHA，记录到 `vendor/VENDOR.md` 与基线 commit message |

> **待定收敛的纪律**：待定项**只允许**向"移除 / 保留（零改动）/ 保留但需补丁（分配 patch 号）"三者之一收敛，且每次收敛必须同时更新计划 §1.7 的计数与本节。**不得**把待定项直接写进 A/B/C 表以制造"已决"的观感。

> **裁剪原则**：先"移出 workspace members"（补丁 **0003a**/0004），保留源码在树内一个 release 周期以便对照；确认无编译依赖后再删除目录（编译器会**穷尽地**列出耦合点，比人工 grep 可靠）。这样 rebase 上游时冲突面最小。

### 6.3 `shangyi/crates/*` 清单

全部 `自研`；"对应 v0.1"列指出它实现/承载的 v0.1 章节。

| crate | 层 | 职责 | 对应 v0.1 | 关键依赖（外部 / 内部） |
|---|---|---|---|---|
| `shangyi-gamespec` | L0 | GameSpec 数据结构、序列化、**校验**（策略空间离散性、2–4 玩家、payoff 表达式引用完整性、`external_actors` 引用一致性）、`Equilibrium`/`EquilibriumSet`/`SelectorScore` 类型 | §5.2、§5.3、§5.4 | serde/serde_yaml、`shangyi-*` 无 |
| `shangyi-playbooks` | L0 | 剧本 YAML 解析、`Row` 模型、**整行替换**组合（§2.4）、`--dump-config`、阶段图与 checkpoint 声明、mode 路径 | §3.2、§8 | serde_yaml；`shangyi-gamespec` |
| `shangyi-solver` | L1 | 支持枚举（n≤4）、Lemke–Howson（2 人）、扩展式逆向归纳、颤抖手/序贯理性精炼、复制者动态、虚构对局/EWA、QRE、level-k、ESS、蒙特卡洛传播 | §5.3、§5.4（`stochastic`/`learning`/`behavioral`）、§5.6 | `faer`、`microlp`、`clarabel`、`argmin`、`statrs`、`rand`；`shangyi-gamespec` |
| `shangyi-kb` | L1 | 理论库 RAG 检索、条目 schema（四要素）、参数先验库、案例库读写接口（后端在 sidecar/独立服务） | §5.1、§7.1、§7.2 | `shangyi-gamespec`（schema） |
| `shangyi-intel` | L1 | 公开数据采集编排、抽取、**参与者画像**（全角色 schema + 贝叶斯后验）、行为参数校准（λ/k 拟合） | §6.1、§6.2、§6.3 | `shangyi-gamespec` |
| `shangyi-report` | L1 | `ReportModel`（四层内容块的**结构化中间表示**）→ 图数据 JSON → sidecar 渲染 → 外部 pandoc 管线；模板变量装配 | §9 | serde；无 harness 依赖 |
| `shangyi-selection` | L2 | 六个选择器（`risk_dom`/`stochastic`/`learning`/`behavioral`/`focal_llm`/`persona_sim`）的统一接口、融合公式、校准更新、差异告警 | §5.4 | `shangyi-gamespec`、`shangyi-solver`、`shangyi-kb`（先验） |
| `shangyi-swarm` | L2 | persona 子智能体定义（角色卡）、orchestrator（纯代码）、rollout 调度、多样性采样、非常规 Move 生成与**三重过滤**、feedback 环 | §5.7、§5.7.1、§5.6.1 步骤 2 | 经 `shangyi-tools` 调用 harness 多智能体（见下） |
| `shangyi-consequence` | L2 | whatif 管线：条件子博弈、`BR(d)` 枚举、非常规反应注册回 GameSpec、`P(r|d)`、路径级后果、Monte Carlo → P10/P50/P90 | §5.6.1 | `shangyi-gamespec`、`shangyi-solver`、`shangyi-selection`（只读） |
| `shangyi-discover` | L2 | 满意策略发现：候选生成（模板×参数网格 + LLM 提议）、满意过滤、抱负水平放宽、帕累托前沿与稳健满意度排序 | §5.6.2 | `shangyi-consequence` |
| `shangyi-tools` | L3 | **领域 `ToolSpec` 集合**与执行器：每个工具一个 `ToolExecutor` 实现，注册进 harness 的工具路由器；工具边界处做 schema 校验与 provenance 记录 | §4 | `codex_tools::{ToolSpec, ToolExecutor}`、`shangyi-*` 全部 |
| `shangyi-core` | L3 | agent 循环接线、**会话状态机**（阶段机）、阶段闸门 hook、领域事件类型与日志、投影单元、checkpoint seam、预算计量、resume/fork/rewind | §3.1、§3.3、§8.3、§11 | `codex_core::{ThreadManager, CodexThread, ...}`、`codex_protocol`、`codex_config`、`codex_rollout`、`codex_state`、`shangyi-tools` |
| `shangyi-cli` | L3 | `shangyi` 二进制：`new`/`resume`/`fork`/`config`/`report`/`backtest`/`kb`/`license`，CLI 交互与进度视图 | §8、§10（CLI 命令） | `shangyi-core`、`shangyi-playbooks`、（可选）`codex_tui` |

**注意 `shangyi-swarm` 的依赖倒置。** `shangyi-selection` 的 `persona_sim` 选择器**需要** Swarm 的行为证据，而 Swarm 的非常规 Move 过滤**需要**后果预测，后果预测**需要**选择器——形成环。解法：`shangyi-selection` 只声明一个 trait，由 L3 注入实现：

```rust
/// shangyi-selection 声明；shangyi-swarm 实现；shangyi-core 在 L3 装配。
pub trait PersonaSimProvider: Send + Sync {
    fn action_distribution(&self, game: &GameSpec, personas: &PersonaSet, n: u32)
        -> Result<ActionDistribution, SwarmError>;
}
```

于是 L2 内部依赖是**偏序**：`{selection, swarm}` → `consequence` → `discover`。这是本 workspace 唯一需要解释的依赖关系。

### 6.4 依赖分层规则

| 层 | crate | 硬规则 |
|---|---|---|
| **L0** 数据与配置 | `shangyi-gamespec`、`shangyi-playbooks` | 只依赖 serde 系 + 标准库；**禁止**依赖 tokio/网络/IO（保持纯数据，可被 CI 快速 fuzz） |
| **L1** 领域基座 | `shangyi-solver`、`shangyi-kb`、`shangyi-intel`、`shangyi-report` | 可依赖数值栈与异步运行时；**禁止**依赖 `vendor/codex-*`（保持"领域算法可脱离 harness 单测"） |
| **L2** 决策引擎 | `shangyi-selection`、`shangyi-swarm`、`shangyi-consequence`、`shangyi-discover` | 只依赖 L0/L1 与 L2 内的偏序（见 §6.3）；**禁止**依赖 `vendor/codex-*`（同 L1 理由：算法层不感知 harness） |
| **L3** 接线与交付 | `shangyi-tools`、`shangyi-core`、`shangyi-cli` | 唯一允许依赖 `vendor/codex-*` 的层；且只能走 public 面（§5.4 规则 1） |

**这条分层规则的直接收益**：`shangyi-solver` / `shangyi-selection` / `shangyi-consequence` / `shangyi-discover` 可以在**不编译 harness** 的情况下单测与差分（把 `cargo test -p shangyi-solver` 的编译时间从"整个 workspace"降到"数值栈"）。这是把 1.5–2 周的求解器工期风险（D-A6）变得可管理的关键工程手段。CI 中由 `cargo xtask depcheck` 强制层规则。

### 6.5 `models.json` / prompt / patch 策略

**（1）`models.json`——为什么必须替换而非新增。**

GB 01 §9 的关键事实：**base system prompt 来自模型目录，而不是静态 `prompt.md`**：

```rust
pub fn render_model_instructions(model_info: &ModelInfo) -> String {
    let template = ResolvedModelMessages::from_model(model_info).instructions_template();
    if template.is_none() { tracing::warn!(model = %model_info.slug, "Model has no instruction template; returning empty instructions."); }
    template.unwrap_or_default().to_owned()
}
```

→ 若商奕只改 `prompts/` 而不改 `models.json`，**模型会拿到空指令**（只 warn 不报错——这正是 §2.3 要禁止的静默降级形态，此处只能"绕过上游客忍"）。因此 `models.json` 是**必改项**，且 `instructions_template` 的缺失必须在商奕侧被显式检测（启动自检）。

**三层策略：**

| 机制 | 用途 | 归属 |
|---|---|---|
| `models.json` 的 `instructions_template`（按模型 slug） | 该模型的**基础**指令骨架（含商奕身份与硬约束） | `shangyi/models.json` → patch 0008 同步到 `vendor/codex/codex-rs/models-manager/models.json` |
| `model_instructions_file`（配置键，绝对路径） | 覆盖 base instructions 为 `shangyi/prompts/base.md`；**这是日常迭代的入口** | `shangyi/config.toml` |
| persona 段（playbook 的 `persona` 行） | 会话/子会话级 prompt **遮蔽**；`complete: true` 时 prefix 成为**整个** system prompt | `shangyi/playbooks/<id>/playbook.yml` |

> **注意两个上游陷阱**（GB 01 §9）：**没有** `base_instructions` 配置键（它只作为 app-server `thread/start` 参数存在）；**没有** `experimental_instructions_file`（已重命名为 `model_instructions_file`）。写配置时不要凭记忆。

**`models.json` 的商奕条目骨架**（v0.1 §13 D2 定 GLM 系）：`slug`、`context_window`、`max_context_window`、`auto_compact_token_limit`、`truncation_policy`、`tool_mode`（商奕只用 `Direct`/`Deferred`，D-A4a）、`input_modalities`、`multi_agent_version`（Swarm 需要 V1/V2 之一）、`supported_reasoning_levels`、`instructions_template`。

**（2）persona 的"自足覆盖"要求（GB 02 §9，承重）。**

persona 是博弈论仿真的**单位**。每个 persona 必须是行文身份的**硬覆盖**并具备自足能力：

- 若不 `complete`，"对抗性监管者"persona 会继承全局"乐于助人的助手"框架 → 仿真失效（§2.3 的静默降级场景）；
- 渲染规则：**最高优先级的非空 `complete` persona 整体胜出**；否则最具体的 prefix/suffix 遮蔽部署层；
- 模板插值**严格**：未知 `{{var}}` 是**硬错误**，不是空串（同样是不变量 ②）。

```rust
pub enum PersonaScope { Deployment, Playbook, Agent(AgentId) }
pub struct PersonaSection { pub scope: PersonaScope, pub prefix: String,
                            pub suffix: Option<String>, pub complete: bool }
impl PromptAssembler {
    /// 最高优先级的非空 `complete` persona 整体胜出；
    /// 否则最具体的 prefix/suffix 遮蔽部署层。
    /// 未知 `{{var}}` → Err(UnknownVariable)。
    pub fn render(&self, vars: &VarTable) -> Result<String, PromptError>;
}
```

**（3）patch 策略**：见 §5.4 规则 2。补充一条工程实践——**patch 集必须能被上游 `main` 的定期 rebase 消化**，因此：

| 实践 | 说明 |
|---|---|
| 优先"挂载点"而非"改逻辑" | 例如领域工具：不改 `ToolRegistry` 的实现，只在 `add_core_tool_sources` 里加一次调用（`register_trusted` 是公开方法） |
| 优先"新文件"而非"改旧文件" | Chat 客户端放在 `codex-api/src/chat_completions.rs`（新文件），只改 1 行 `mod` 声明 |
| 每个 patch 独立文件、独立目的 | 多目的 patch 在 rebase 时只能整块丢弃。上游实测的反例：Code Mode 移除实际要改 4 个**保留** crate 的源码，故 `0003` 已拆为 **0003a/0003b/0003c**（manifest / `core`+`tools` / `rollout-trace`+`app-server`），使 rebase 时能逐块丢弃 |
| 上游改名跟踪 | Codex 近期有重大重命名（`ConversationManager`→`ThreadManager`、`CodexConversation`→`CodexThread`、`core/src/codex.rs` 已不存在，GB 01 §2）。patch 中引用的符号必须有 `cargo build` 兜底，且 rebase 时优先跑编译 |

---

## 7. Agent 核心循环

### 7.1 turn / step 词汇（Rust 类型）

v0.1 §3.1 的状态机是一个**粗粒度**抽象（八阶段）。harness 的 turn/step 是**细粒度**执行单位。两者关系：**阶段包含多个 turn，turn 包含多个 step。**

```rust
/// step = 一次模型调用 + 它请求的工具执行（GB 02 §4）
/// turn = 0..n 个 step
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TurnId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StepId { pub turn: TurnId, pub step: u64 }

/// turn 的终止原因。`Interrupted` **从不实时发出**——
/// 崩溃遗留的 turn 由 resume repair 事后合成（GB 02 §4）。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TurnEndReason {
    Completed,
    Aborted { reason: CancelCause },
    /// 数据源不可用等外部阻塞；与 `completed` 必须**永久可区分**。
    Blocked,
    Error { error: LlmFailure },
    MaxTokens,
    /// 仅由 resume repair 为崩溃遗留 turn 写入。
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum CancelCause { User, Parent, Hook { reason: String }, Disposed, Legacy }
```

**为什么 `Blocked` / `Interrupted` 对商奕是承重的**（GB 02 §4）：一次**因数据源不可用而终止**的尽调，必须与一次**正常得出结论**的尽调**永久可区分**。崩溃恢复伪造一个干净的 `completed` 是会导致审计失败的**正确性 bug**——报告读者会以为这个案例是完整分析。因此我们逐字移植 dsh 的 repair 逻辑（它是 `&[Event]` 上的**纯函数**，"它的确定性就是全部价值"）：

```rust
/// 逐字移植自 dsh repair（含测试）。确定性即全部价值。
/// 1) 给未匹配的工具调用补错误结果（TOOL_NOT_STARTED / TOOL_OUTCOME_UNKNOWN）
/// 2) 补开放 step/end
/// 3) 补 `interrupted` 的 turn/end，时间戳复用最后一个真实事件
pub fn interrupted_turn_closers(events: &[Event]) -> Vec<Event>;
```

### 7.2 事件词汇

**三套事件，各归其位：**

| 层 | 类型 | 归属 | 处置 |
|---|---|---|---|
| 基座协议 | `codex_protocol::Op`、`EventMsg` | `vendor/codex/codex-rs/protocol` | `直接复用`（**不新增领域变体**，§5.4 规则 4） |
| 领域事件 | `shangyi_core::session::Event` | `shangyi-core` | `自研`（§8.2 的 schema） |
| 领域 Op | 复用 `Op::UserInputAnswer`、`Op::DynamicToolResponse`、`Op::TurnInput`、`Op::Interrupt` | 通过 public 面 | `直接复用` |

**基座事件词汇中商奕实际依赖的（选自 GB 01 §2，`EventMsg`）**：`TurnStarted`/`TurnComplete`、`AgentMessage`、`AgentReasoning`、`UserMessage`、`TokenCount`、`SessionConfigured`、`ContextCompacted`、`RequestUserInput`、`RequestPermissions`、`ExecApprovalRequest`、`TurnAborted`、`ItemStarted`/`ItemCompleted`、`HookStarted`/`HookCompleted`、`CollabAgentSpawnBegin`/`End`、`CollabAgentInteraction*`、`CollabWaiting*`、`CollabClose*`、`SubAgentActivity`、`Error`、`Warning`。

**领域工具 wire 名（`shangyi-tools`，对应 v0.1 §4 的工具表 + §5 引擎能力）：**

| wire 名 | 归属 v0.1 | 阶段白名单 | 幂等 |
|---|---|---|---|
| `vn_map` | §5 参与者确定（价值网五向扫描） | intake | 否 |
| `pars_check` | §5 PARTS 检查表 | intake | 是 |
| `ask_user` | §8.1 引导式 intake | intake | 否（表面为提问 seam） |
| `web_search` | §4（`SearchProvider` 抽象，多后端链式回退） | intake, intel | 是 |
| `poi_survey` | §4（`PoiProvider` 抽象；高德 POI 周边计数，只回计数不回明细） | intake, intel | 是 |
| `web_fetch` | §4（尊重 robots.txt、限速） | intake, intel | 是 |
| `data_ingest` | §4（CSV/Excel 解析与列映射建议） | intel, model_build | 是 |
| `participant_profile_update` | §4、§6.2（含贝叶斯后验） | intel | 否 |
| `gamespec_build` | §5.2 | model_build | 否 |
| `gamespec_validate` | §5.2（校验规则在 `shangyi-gamespec`） | model_build, solve | 是 |
| `solve_equilibria` | §5.3（**确定性 Rust，不经沙箱**） | solve | 是 |
| `select_equilibria` | §5.4（单选择器） | select | 是 |
| `select_fuse` | §5.4 融合机制 | select | 是 |
| `swarm_run` | §5.7（orchestrator + persona rollout） | select, recommend, consequence | 否 |
| `novel_move_filter` | §5.7.1 三重过滤 | consequence, discover | 是 |
| `consequence_predict` | §5.6.1 | consequence, discover | 是 |
| `monte_carlo` | §5.5、§5.6.1 步骤 5 | recommend, consequence | 是（种子固定） |
| `sensitivity_scan` | §5.5 翻转阈值 | recommend | 是 |
| `discover_satisfice` | §5.6.2 | discover | 是 |
| `preference_inject` | §5.5、§8.2 | recommend | 是 |
| `report_render` | §9（产出 `ReportModel` → sidecar） | report | 否 |
| `case_store_write` / `case_store_read` | §4、§7.2 | 全阶段（读）/ archived（写） | read 是 / write 否 |
| `kb_retrieve` | §4、§7.1 | 全阶段 | 是 |
| `checkpoint` | §8.3 | 阶段末 | 否 |
| `request_rewind` | —（§7.5 的模型侧补充，登记为 R23） | 全阶段 | 否 |
| `sandbox_exec` | §4（**仅**临时数值任务；结果不得直接进报告） | 任意（受审批约束） | 否 |

**`sandbox_exec` 的收窄（对应 R8）**：v0.1 §4 的原说明含 "numpy/scipy/nashpy/gambit/matplotlib"。在 D-A8/D-A10 之后，其可用面变为：`numpy`/`scipy`（可选，仅探索）+ **禁止** `gambit`（许可）+ **禁止** `nashpy`（已在 Rust 侧有 oracle，沙箱内使用会造成"两套求解路径"的审计歧义）。所有产出带 `provenance`，且**标注 `exploratory: true`**，报告生成器拒绝读取带该标记的结果。

### 7.3 会话状态机

**与 v0.1 §3.1 完全同构**（八个阶段、同命名），载体从 Python 迁到 Rust（R5）：

```rust
/// 对应 v0.1 §3.1 的 SessionState。命名逐字保留。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Intake,      // 剧本识别与信息收集
    Intel,       // 公开数据搜集与参与者画像
    ModelBuild,  // GameSpec 构建
    Solve,       // 均衡枚举 + 精炼
    Select,      // 均衡选择
    Recommend,   // 对策与敏感性
    Report,      // 报告生成
    Archived,    // 已归档
}

/// 阶段契约：进入条件、产物、checkpoint、工具白名单。
/// 全部来自剧本 YAML（数据，不是代码）——这是"阶段转移由剧本驱动"的实现。
pub struct PhaseContract {
    pub phase: SessionPhase,
    /// 进入该 phase 前**必须已存在**的产物（闸门判据，§7.4）。
    pub requires: &'static [ArtifactKind],
    pub produces: &'static [ArtifactKind],
    /// 该 phase 结束时需要结算的 checkpoint（None = 无需）。
    pub checkpoint: Option<CheckpointId>,
    /// 该 phase 的领域工具白名单。
    pub tool_allow: &'static [&'static str],
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    CaseBrief, ValueNetMap, ParticipantProfiles, IntelDigest,
    GameSpecValidated, EquilibriumSet, SelectionOutput,
    RecommendationPack, ReportBundle,
}
```

**阶段图（`scenario` 模式，即 v0.1 §2.2 主线）：**

```
intake ──► intel ──►[cp1 事实确认]──► model_build ──►[cp2 假设确认]──► solve
                                                                        │
   archived ◄── report ◄── recommend ◄──[cp3 结果预览]◄── select ◄───────┘
```

**`whatif` / `discover` 模式的路径是同一状态机的**不同边集**，由剧本 YAML 声明（数据，不是新代码）：**

| 模式 | 路径 | 说明 |
|---|---|---|
| `scenario` | `intake → intel → model_build → solve → select → recommend → report → archived` | §2.2 主线，三个 checkpoint |
| `whatif` | `intake → intel → model_build → [solve] → consequence → recommend → report → archived` | 用户给定动作 d；`solve` 可选（仅在需要基线均衡时）。cp1/cp2 保留；cp3 前移到 `consequence` 之后 |
| `discover` | `intake → intel → model_build → solve → discover → recommend → report → archived` | 满意化搜索；`select` 被 `discover` 替代（候选评估内部**复用** `consequence` 管线，v0.1 §5.6.2 步骤 2） |

**阶段机的实现骨架：**

```rust
pub struct PhaseMachine {
    graph: PhaseGraph,          // 来自 shangyi-playbooks（经整行替换组合后的生效图）
}

impl PhaseMachine {
    /// 从投影读出的当前状态出发，判断能否推进；不能则给出**可操作的**原因。
    pub fn next_phase(&self, st: &ConsultState) -> Result<SessionPhase, PhaseGateError> {
        let c = self.graph.contract(st.phase);
        if let Some(cp) = c.checkpoint {
            match st.checkpoints.get(&cp) {
                None | Some(CheckpointStatus::Open { .. }) =>
                    return Err(PhaseGateError::CheckpointPending { phase: st.phase, checkpoint: cp }),
                Some(CheckpointStatus::Rejected { .. }) =>
                    return Err(PhaseGateError::CheckpointRejected { phase: st.phase, checkpoint: cp }),
                Some(CheckpointStatus::Approved { .. }) => {}
            }
        }
        let missing: Vec<ArtifactKind> = self.graph.next(st.phase)
            .map(|n| n.requires).unwrap_or(&[])
            .iter().filter(|k| !st.has_artifact(k)).copied().collect();
        if !missing.is_empty() {
            return Err(PhaseGateError::MissingArtifacts { phase: st.phase, missing });
        }
        self.graph.next(st.phase).ok_or(PhaseGateError::Terminal(st.phase))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PhaseGateError {
    #[error("phase {phase:?} still has an open checkpoint {checkpoint:?}")]
    CheckpointPending { phase: SessionPhase, checkpoint: CheckpointId },
    #[error("checkpoint {checkpoint:?} was rejected by the user; phase {phase:?} must be revised")]
    CheckpointRejected { phase: SessionPhase, checkpoint: CheckpointId },
    #[error("phase {phase:?} is missing required artifacts: {missing:?}")]
    MissingArtifacts { phase: SessionPhase, missing: Vec<ArtifactKind> },
    #[error("phase {phase:?} is terminal for this playbook")]
    Terminal(SessionPhase),
}
```

> **注意 `PhaseGateError` 的三个变体与 v0.1 §2.2 "三个 checkpoint 均可回退修改"的关系**：`CheckpointRejected` 不是错误终止，而是"回到该阶段重做"的**信号**——上层 loop 收到它之后发 `PhaseRewound` 事件（§8.3），把当前阶段退回，并用相同的 turn/step 机制重新进入。错误变体的存在意义是**让"卡住"显式可见**，而不是让 loop 悄悄前进（不变量 ②）。

### 7.4 挂载到 `agent/pre-step` 与 `agent/request`

**为什么是这两个钩子（D-A12）**：dsh 的循环扩展点是一张 waterfall 表（GB 02 §6），其中只有两个能**在 step 的模型调用发生前**介入：

| 钩子 | 模式 | 契约 | 商奕用它做什么 |
|---|---|---|---|
| `agent/pre-step` | waterfall | `(payload{agent,messages,turn,step,signal}, next) -> PreStepDecision`；可**拒绝**拟议 step 或**替换**进入 step 的消息 | ① **阶段闸门**：当前阶段的 `requires` 产物不全则拒绝；② **checkpoint 闸门**：未结算则拒绝；③ 阶段指引 prompt 注入 |
| `agent/request` | waterfall | `(payload, next) -> LlmCallConfig`；`await next()` 得机器本会使用的配置，返回替换即切换 | ① **阶段化工具白名单**（`tool_filter = phase.tool_allow`）；② 温度（`focal_llm` 的受控 prompt 必须温度 0）；③ 模型档位切换 |

```rust
#[async_trait::async_trait]
pub trait PreStepHook: Send + Sync {
    async fn on_pre_step(&self, p: &mut PreStepPayload) -> PreStepDecision;
}

#[derive(Clone, Debug)]
pub enum PreStepDecision {
    Accept,
    Replace(Vec<UserMessage>),      // 替换进入 step 的消息
    Reject { reason: String },      // 拒绝拟议 step
}
```

**阶段闸门 hook（本章的核心交付物）——"阶段的必需产物必须存在才允许推进"：**

```rust
pub struct PhaseGateHook {
    machine: PhaseMachine,
    graph: PhaseGraph,
}

#[async_trait::async_trait]
impl PreStepHook for PhaseGateHook {
    async fn on_pre_step(&self, p: &mut PreStepPayload) -> PreStepDecision {
        // 1. 读投影（纯函数、O(新增事件)）：当前阶段、
        //    已产出产物、checkpoint 状态、开放问题。
        let st: Arc<ConsultState> = p.projections.consult_state();

        // 2. 栈：checkpoint 未结算 → 硬拒绝。模型不能"先跑再说"。
        if let Some(cp) = self.graph.contract(st.phase).checkpoint {
            match st.checkpoints.get(&cp) {
                Some(CheckpointStatus::Approved { .. }) => {}
                _ => return PreStepDecision::Reject {
                    reason: format!(
                        "阶段 {} 的 checkpoint {:?} 尚未结算。请调用 `checkpoint` 工具向用户确认后继续。",
                        st.phase, cp
                    ),
                },
            }
        }

        // 3. 闸门：本阶段的必需产物缺失 → 拒绝，并把缺什么**说清楚**
        //    （类型化原因，不是"出错了"）。
        let missing: Vec<_> = self.graph.contract(st.phase).requires.iter()
            .filter(|k| !st.has_artifact(k))
            .map(|k| format!("{k:?}"))
            .collect();
        if !missing.is_empty() {
            return PreStepDecision::Reject {
                reason: format!(
                    "阶段 {} 缺少必需产物：{}。请先通过对应工具产出。",
                    st.phase, missing.join("、")
                ),
            };
        }

        // 4. 注入阶段指引（surface 化：它影响请求，所以它必须被记录）
        let guide = self.graph.contract(st.phase).guide_message();
        PreStepDecision::Replace(vec![guide])
    }
}
```

**请求 hook（阶段化信封）：**

```rust
pub struct PlaybookRequestHook;

#[async_trait::async_trait]
impl RequestHook for PlaybookRequestHook {
    async fn on_request(&self, p: &RequestPayload, mut cfg: LlmCallConfig) -> LlmCallConfig {
        let st = p.projections.consult_state();
        cfg.tool_filter = st.phase.tool_allow().to_vec();   // 阶段白名单
        cfg.temperature  = st.phase.temperature();          // focal_llm ⇒ 0.0
        cfg.reasoning    = st.phase.reasoning_level();
        cfg.phase        = Some(st.phase);                  // 写入 RequestHeader（可审计）
        cfg
    }
}
```

> **`RequestHeader` 是必答项**（不变量 ①）：`cfg.phase`、`tool_filter` 的**哈希**、模型 slug、温度都必须落进 log-only 的 `RequestHeader` 事件。否则无法回答"这条结论是在**禁用** `web_search` 的 `model_build` 阶段产出的吗"这类审计问题。

### 7.5 steering 与中断

**harness 提供的一等公民能力（GB 01 §2）**：steering 在 Codex 中是**一等公民**——`TurnInputMode`、`TurnInputRequest`、`SteerSubmission`、`RecoverTurnRequest`、`SuspendTurnOutcome`、`NotSubmittedReason`；`CodexThread::{start_or_steer_turn, steer_turn}`；app-server 暴露 `turn/steer`；甚至有 `EventMsg::NonSteerableTurnKind`（部分 turn 类型**明确不可 steer**）。取消路径：`Op::Interrupt` → `EventMsg::TurnAborted`；`Session::{mark_interrupted, is_interrupted}`；优雅中断超时 `GRACEFULL_INTERRUPTION_TIMEOUT_MS = 100`。

**商奕的两类用户介入，映射到不同原语：**

| 用户意图 | 商奕 CLI 形态 | harness 原语 | 语义 |
|---|---|---|---|
| **补充/修正当前进行中的分析**（"等一下，其实对手还有第三家"） | 用户直接输入 | `Op::TurnInput` + `TurnInputMode::Steer`；`CodexThread::steer_turn`；app-server `turn/steer` | **不中断 turn**，新输入在**下一个 step** 边界注入；这是正确行为——因为当前 step 可能正在等工具结果 |
| **停止当前动作**（"别抓了，直接建模"） | `Esc` | `Op::Interrupt` → `EventMsg::TurnAborted`；`TurnEndReason::Aborted { reason: CancelCause::User }` | 中断 turn；已完成的部分保留在日志中；**必须**记 `Aborted` 而非 `Completed`（§7.1） |
| **回退到某个 checkpoint 重做**（v0.1 §2.2 "三个 checkpoint 均可回退修改"） | `shangyi resume <case> --reopen cp2` | 见 §8.4：追加 `PhaseRewound` + surface `replace` | **不**改日志：以遮蔽（shadowing）表达作废 |
| **修改阶段产物**（改 payoff 参数） | 在 checkpoint 提问件里选"修改" | 领域工具调用 + `HypothesisRevised` 事件 | 旧假设保留在日志，新假设追加（§8.7） |

**Steering 的领域约束（一处必须注意）**：`steer_turn` 在**确定性工具执行期间**不被接受（工具调用不是模型调用）。商奕的 `solve_equilibria` 可能有秒级耗时；用户在此时按 `Esc` 必须走 `Op::Interrupt`（而不是 steer），且中断后求解器必须**取消干净**——用 `tokio_util::sync::CancellationToken`（GB 02 §F：取消正确性正是 agent 运行时会烂掉的地方），而不是让任务在后台跑完再丢结果。

**`TurnEndReason` 的正确性清单（写成测试）：**

| 场景 | 期望 `TurnEndReason` | 反例（错误实现） |
|---|---|---|
| 正常完成 | `Completed` | — |
| 用户 Esc | `Aborted { User }` | `Completed`（**审计失败**） |
| 数据源全部不可用，无法继续 intel | `Blocked` | `Completed` 或 `Error`（丢失"外部阻塞"语义） |
| 进程崩溃后 resume | `Interrupted`（由 `interrupted_turn_closers` 合成） | `Completed`（**正确性 bug**，GB 02 §4） |
| 上下文超限 | `MaxTokens` | `Completed` |

### 7.6 checkpoint 如何暂停循环（两条 seam 的区分）

**这是最容易混淆的一点，必须讲清（对应 R9）。**

| 维度 | **checkpoint seam**（商奕的语义闸门） | **approval seam**（基座的权限闸门） |
|---|---|---|
| 触发者 | 剧本/阶段机（领域语义："假设必须人确认"） | 工具执行（安全语义："这个动作在沙箱外，需要批准"） |
| 问题内容 | **领域问题**：待确认事实/假设清单 + 来源（v0.1 §8.3） | **权限问题**："是否允许写 `/workspace` 外？""是否允许联网？" |
| 载体 | 复用 `codex_protocol::request_user_input` 模块 + `EventMsg::RequestUserInput` + `Op::UserInputAnswer`；领域侧包装为 `checkpoint` 工具 | `Op::ExecApproval`/`Op::PatchApproval`/`Op::RequestPermissionsResponse`/`Op::ResolveElicitation`；`AskForApproval` 策略 |
| 失败语义 | 无应答者 ⇒ **拒绝推进**（`PhaseGateError::CheckpointPending`） | 无应答者 ⇒ `Unavailable` ⇒ **视为拒绝**（GB 02 §10 的 fail-closed） |
| 谁需要它 | 铁律 2 | LLM 生成的 Python 沙箱（§3.7） |
| 是否影响报告 | **是**：checkpoint 答案是报告 ④ 层"假设清单"的数据源 | 否（只影响可执行性） |

**checkpoint 的完整生命周期（暂停 → 询问 → 结算 → 留档）：**

```
阶段末（如 intel 结束）
  │
  ├─1 领域工具 `checkpoint` 被调用（参数：items[]，每项含 content/source/proposed_value）
  │     → 追加 log-only `CheckpointOpened { id, items, phase }`
  │     → 追加 surface 事件：一条工具调用消息，其载荷复用
  │       `codex_protocol::request_user_input` 的 `RequestUserInput` 形态
  │       （即"工具调用 + 结构化提问"同一个 step 内完成）
  │
  ├─2 turn 在此**挂起**：等待 `Op::UserInputAnswer`
  │     循环不推进（PreStepHook 在恢复后仍会因 checkpoint 未结算而拒绝任何 step）
  │     用户可见：CLI 渲染提问件（v0.1 §8.3 的三操作：[确认/修改/追问]）
  │
  ├─3 用户作答
  │     → log-only `CheckpointAnswered { id, answers[], user_actor, at }`
  │       ★ durable 强制写点：fsync（§8.3）
  │     → surface `UserMessage { source: CheckpointSource { id }, text }`
  │       ★ 这是"回答到达模型"的唯一通道（不变量 ① 的 dsh compactCheckpointSource 同款技巧）
  │
  └─4 以**确定性代码**重算产物（不由模型做）：
        - 事实确认 → 更新 `ParticipantProfiles`（`evidence_quality` 重算）
        - 假设确认 → 写 `HypothesisRevised` / 固定 `source` 字段
        - 结果预览 → 批准则允许进入下一阶段；拒绝则 `PhaseRewound`
      然后 loop 从 1 开始判定能否推进
```

**checkpoint 的 `kind` 与 v0.1 三个 checkpoint 的对应：**

| v0.1 | 结算阶段 | `kind` | 待确认内容 | 拒绝后的动作 |
|---|---|---|---|---|
| checkpoint①  | intel 末 | `fact_confirm` | 事实清单（公开数据抓取结果、画像证据） | 退回 intel 重新采集/修正抽取 |
| checkpoint② | model_build 末 | `assumption_confirm` | payoff 参数、对手估计等硬假设 + 每条来源 | 退回 model_build（可能进一步退回 intel） |
| checkpoint③ | select 末 | `result_preview` | 均衡概率分布预览 + 判据驱动 | 退回 select 调整权重/退回 model_build 改模型 |

**为什么 checkpoint 不能实现为"prompt 里的请求"。** 如果它只是 prompt 文本，模型可以（并且迟早会）跳过它——那时铁律 2 就退化为一句愿望。把它实现为 `PreStepHook` 的**硬拒绝**意味着：只要 checkpoint 未结算，**任何** step 都无法通过，无论模型怎么想。这是本章把 §2.1 铁律 2 与 §7.4 钩子接起来的地方。

### 7.7 阶段转移的持久化

**阶段转移是事件，不是可变状态**（§8.1 的事件溯源要求）：

| 事件 | 类型 | 时机 |
|---|---|---|
| `PhaseEntered { from, to, playbook, reason }` | log-only | 阶段机推进成功时 |
| `PhaseApproved { phase, fingerprint }` | log-only | checkpoint 结算为 approved 时（`fingerprint` = 该阶段产物摘要，用于检测"批准后产物被改"） |
| `PhaseRewound { from, to, due_to: CheckpointRevision(id) }` | log-only | checkpoint 被拒 / 用户主动回退时 |
| `PlaybookSelected { id, source }` | log-only | 会话创建时；**必须持久**，因为 playbook 决定会话的**工具与 prompt**——恢复出不同组合会让模型面对无法执行的历史（GB 02 §2） |

**投影 `phase: SessionPhase`** 定义为**最后一条阶段事件胜出**（整值事件规则）：

```rust
fn apply_phase(prev: &SessionPhase, ev: &Event) -> Arc<SessionPhase> {
    match ev {
        Event::PhaseEntered { to, .. } => Arc::new(*to),
        Event::PhaseRewound { to, .. } => Arc::new(*to),
        _ => Arc::new(*prev),   // 不关心时返回新 Arc 以便简单比较；
                                // 热路径可改为 Arc 复用 + ptr_eq 捷径
    }
}
```

**`PhaseApproved.fingerprint` 的作用**（一个具体审计场景）：checkpoint② 批准时报的是"payoff P1 = 42 ± 8"。若之后 `ModelBuildHack`（模型）修改了 P1 而未重新过 checkpoint，则 `ArtifactProduced` 的摘要与 `PhaseApproved.fingerprint` 不匹配，`PhaseGateHook` 必须**重新要求** checkpoint②。这把铁律 2 从"过一次"升级为"过一次且此后未被改动"。

---

## 8. 会话事件日志与持久化

### 8.1 事件溯源的设计取向

**选择事件溯源而非"结构化状态 + 快照"的理由**，按重要性：

1. **审计链是交付物**（§2.2）：只有 append-only 日志能回答"当时模型看到的是什么"。
2. **三 checkpoint 的回退需要表达"作废"而不是"删除"**（v0.1 §2.2）：用户退回 checkpoint② 并改参数，报告 ④ 层仍应能展示"原假设 vs 修正后假设"。删除语义做不到；遮蔽（shadowing）语义做得到。
3. **崩溃恢复必须可区分"正常完成"与"被中断"**（§7.1）：只有日志能承载 `TurnEndReason::Interrupted` 这种事后方能合成的收尾。
4. **投影使派生状态可重算**：如果把"当前阶段/假设/预算"当权威状态存，日志就退化为日志；把它当投影，日志才是唯一的真相源。

**三条来自 dsh 的纪律（`仅借鉴设计`，GB 02 §3/§5/§13）：**

| 纪律 | 内容 | 若违反的后果 |
|---|---|---|
| **seq 连续且等于日志长度** | `Case::seq() == events.len()`；`SessionEvent { type, seq, time, data }` | 无法检测丢行；快进重放不可信 |
| **整值事件规则** | 携带状态的事件必须携带**变更后的完整状态**，绝不是裸增量 | 投影的每个单元转换代价升高，且每个被服务的值不能自描述 |
| **未知事件默认必需** | 读者遇到不认识的 `type` 且无 `ignorable: true` 时**必须拒绝重建**，而非静默跳过 | 静默丢事件 → 静默降级（不变量 ②） |

### 8.2 surface 与 log-only（`Presence`）

**精确定义（GB 02 §13）**：log-only 事件**持久且可重放**但**模型不可见**；不携带 `surfaceOp`，`isSurfaceEligibleType` 返回 false，故 `derive_messages()` 跳过它，而**持久化、投影、重放与导出全部保留**。二阶规则："事实要保存，机会要广播"；仓库表述为 **"Model-visible ⟺ logged"**（注意这是双向断言的单向使用：本设计保证 **visible ⇒ logged**）。

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Seq(pub u64);

/// surface 资格。只有少数类型是 Surface；其余一切持久、可重放、可投影，
/// 但对模型不可见（GB 02 §13）。
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Presence { Surface, LogOnly }

/// surface op。`Replace` 节点必须在 `sources` 中引用它遮蔽的**每一个**节点；
/// assistant 消息不可作为 replace 目标（它们内嵌 provider stream）。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum SurfaceOp {
    Append,
    Replace { start: Seq, end: Seq, sources: Vec<Seq> },
}

/// 上下文来源：注入的每一段内容都必须自述其出处（不变量 ①）。
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ContextSource {
    Playbook { id: String },
    Checkpoint { id: CheckpointId },
    Intel { artifact: ArtifactKind },
    Profile { participant: String },
    Kb { theory_id: String },
    Prior { name: String },          // 先验 borrowed（v0.1 §6.3）
}
```

**商奕的领域事件枚举：**

```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum Event {
    // ── A. turn / step（移植自 dsh，GB 02 §3/§4）────────────────────────
    TurnStart { turn: u64 },
    TurnEnd { turn: u64, reason: TurnEndReason },
    StepStart { turn: u64, step: u64 },
    StepEnd { turn: u64, step: u64 },

    // ── B. surface 事件（仅四类；Presence::Surface）─────────────────────
    SystemMessage  { text: String, sections: Vec<PersonaScope>, #[serde(flatten)] surf: Surf },
    UserMessage    { text: String, source: ContextSource,          #[serde(flatten)] surf: Surf },
    AssistantMessage { text: String, turn: u64, step: u64,        #[serde(flatten)] surf: Surf },
    ToolResult     { call_id: ToolCallId, name: String, outcome: ToolOutcome,
                     provenance: Vec<ProvenanceRef>, exploratory: bool,
                     #[serde(flatten)] surf: Surf },

    // ── C. 请求信封（log-only，但**必须**存在：不变量 ①）──────────────
    RequestHeader {
        reason: RequestHeaderReason,      // initial | steer | resume | compaction
        model: String, provider: String, phase: Option<SessionPhase>,
        temperature_milli: u32,
        tool_filter: Vec<String>, tool_schema_digest: String,
        starts_series: bool,
    },

    // ── D. 商奕领域事件（全部 log-only）────────────────────────────────
    PlaybookSelected { id: String, source: PlaybookSource, mode: ConsultMode },
    PhaseEntered  { from: Option<SessionPhase>, to: SessionPhase, reason: PhaseReason },
    PhaseApproved { phase: SessionPhase, fingerprint: String },
    PhaseRewound  { from: SessionPhase, to: SessionPhase, due_to: RewoundDueTo },
    ArtifactProduced { kind: ArtifactKind, digest: String,
                       provenance: Vec<ProvenanceRef>, produced_by: ToolCallId },
    CheckpointOpened { id: CheckpointId, phase: SessionPhase, kind: CheckpointKind,
                       items: Vec<CheckpointItem> },
    CheckpointAnswered { id: CheckpointId, answers: Vec<CheckpointAnswer>,
                         user_actor: ActorId, at_ms: i64 },
    HypothesisRevised { id: HypothesisId, before: String, after: String,
                        source: HypothesisSource, revised_by: RevisionActor },
    EvidenceLedgerAppend { entry: EvidenceEntry },      // v0.1 §6.2 的 timeline/证据
    SelectionWeights { selector: SelectorId, weight_milli: u32, version: u32,
                       calibrated: bool },
    SwarmRollout { rollout: u32, persona: PlayerId, seat: u8,
                   mv: MoveRecord, novel: NovelVerdict },
    BudgetConsumed { llm_tokens: u64, tool_calls: u64, wall_ms: u64,
                     swarm_rollouts: u32 },
    ConfigResolved { digest: String, layers: Vec<ConfigLayerSource> },  // §2.4
    CaseArchived { record_digest: String, outcome_known: bool },
    OutcomeRecorded { realized_equilibrium: EquilibriumId, at_ms: i64 }, // 回测
    // ── E. 前向兼容兜底 ───────────────────────────────────────────────
    Unknown { raw: serde_json::Value },
}

impl Event {
    /// **唯一**的 surface 判定规则；`derive_messages` 只匹配 `Presence::Surface`。
    pub fn presence(&self) -> Presence {
        match self {
            Event::SystemMessage { .. }
            | Event::UserMessage { .. }
            | Event::AssistantMessage { .. }
            | Event::ToolResult { .. } => Presence::Surface,
            _ => Presence::LogOnly,
        }
    }
}
```

**落盘信封：**

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct Recorded {
    pub seq: Seq,
    pub time_ms: i64,
    /// 落盘时计算并**冗余保存**：供冷读者重建 surface 索引与导出时使用，
    /// **不**用于"跳过未知事件"的判定（那是 `ignorable` 的职责）。
    pub presence: Presence,
    pub event: Event,
}
```

> **实现注意（诚实标注）**：`#[serde(tag="type", content="data")]` 的邻接标签枚举**不能**直接无损捕获未知 `type`。GB 02 §3 给出的落地方式是"用 untagged 兜底臂或 `#[serde(default)] unknown_events`"。商奕采用**两段式解码**：先 `serde_json::from_str::<Value>` 读 `type` 字段 → 路由到具体变体 → 失败则构造 `Event::Unknown { raw }` 并**标记为必需**（除非该 `type` 在 `KNOWN_EVENTS` 之外且显式带 `ignorable: true`）。读者遇到必需但未知的事件**拒绝重建**，报 `LogError::UnknownRequiredEvent { seq, type }`。
>
> `Unknown` 变体与 `surf: Surf`（内含 `SurfaceOp`）的字段组织见 `shangyi-core/src/session/event.rs`；此处省略 `Surf` 定义以避免正文过长。

**surface 事件的完整清单（四类，闭集）**：`SystemMessage`、`UserMessage`、`AssistantMessage`、`ToolResult`。这直接对应 dsh 的四个 `SurfaceEventType`（GB 02 §3）。**新增领域事件时若它需要到达模型，必须改写为"某个 surface 事件 + 领域 source 标记"**，不得把领域事件升级为 surface 类型——这是防止 surface 集合漂移的纪律。

**`derive_messages` 的唯一规则**（GB 02 §3）：`deriveEventMessage(event) -> Message | null` 是**唯一**的逐节点投影规则；`Case::derive_messages()` 在活 surface 上折叠并**按节点缓存**（`derived`、`derived_nodes`、`derived_generation`）；`replace` 提升 generation 并重建。Rust 落地用 `Arc<Event>` 与按节点缓存，`Arc::ptr_eq` 即 `Object.is` 等值捷径的类比（GB 02 §5）。

### 8.3 磁盘格式

**目录布局（`$SHANGYI_HOME`，默认 `~/.shangyi`）**：

```
$SHANGYI_HOME/
├── config.toml                     # 用户配置（对应 $CODEX_HOME/config.toml）
├── <profile>.config.toml           # profile 配置（V2 独立文件）
├── cases/                          # 进行中的案例（对应 sessions/）
│   └── 2026-09-17/                 # 按日期分片（对应 rollout_date_parts）
│       └── case-20260917-pricing/
│           ├── header.json         # 不可变会话头（CaseHeader）
│           ├── case.jsonl          # 事件日志：每行一个 Recorded（append-only）
│           ├── case.jsonl.zst      # 归档压缩（对应 rollout/compress）
│           ├── projection.db       # SQLite 投影缓存（(case_id,key) → {ver,seq,val}）
│           ├── state.db            # SQLite 案例元数据（对应 codex-state）
│           └── artifacts/          # 产物（内容寻址：<digest>.<ext>）
│               ├── gamespec.yaml
│               ├── equilibria.json
│               ├── selection.json
│               ├── report_model.json
│               └── figures/*.svg
├── archived_cases/                 # 已归档案例（对应 archived_sessions/）
├── index.db                        # 全局案例索引（跨案例检索、回测查询）
├── kb/                             # 理论库 markdown + 检索索引（v0.1 §7.1）
├── skills/                         # 方法论卡片（SKILL.md）
├── sqlite_home/                    # 对应上游 sqlite_home
└── logs/                           # 对应上游 log_dir
```

**与 Codex 的对应关系（标明复用与改动）：**

| 商奕 | Codex | 处置 |
|---|---|---|
| `$SHANGYI_HOME` | `$CODEX_HOME`（默认 `~/.codex`） | `修改后复用` `codex-home`（patch 0005，D-A15） |
| `cases/` | `sessions/`（`SESSIONS_SUBDIR = "sessions"`） | `修改后复用`（常量重命名） |
| `archived_cases/` | `archived_sessions/`（`ARCHIVED_SESSIONS_SUBDIR`） | `修改后复用` |
| 日期分片 | `rollout_date_parts` | `直接复用` |
| `case.jsonl` 每行一个 `RolloutLine` | `rollout` JSONL + `decode_rollout_line`（规避 serde `arbitrary_precision` 展平 bug） | `修改后复用`：外层沿用 `RolloutLine`（`timestamp`），`event` 载荷为商奕 `Event` |
| `state.db` | `codex-state`（`ThreadMetadata`/`apply_rollout_item`） | `直接复用` |
| `projection.db` | —（dsh 的 `dsh-session-projection-cache` 是 TS，不可复用） | `自研`（`自研` + `仅借鉴设计` dsh §5 的 `{ver,seq,val}` 行模型） |
| `artifacts/` 内容寻址 | Codex 无对应物 | `自研` |
| `index.db` | `rollout/session_index.rs`、`sqlite_metrics` | `修改后复用` + `自研` 领域表 |

**`index.db` 的领域表（对应 v0.1 §7.2 的案例库 schema）：**

```sql
CREATE TABLE cases (
  case_id      TEXT PRIMARY KEY,
  created_at   TEXT NOT NULL,
  archived_at  TEXT,
  scenario     TEXT NOT NULL,        -- 剧本 id，如 pricing_response
  mode         TEXT NOT NULL,        -- scenario | whatif | discover
  playbook     TEXT NOT NULL,
  phase        TEXT NOT NULL,        -- 投影快照（权威在 JSONL）
  config_digest TEXT NOT NULL,       -- ConfigResolved.digest（可复现性锚点）
  log_bytes    INTEGER NOT NULL,
  last_seq     INTEGER NOT NULL
);

CREATE TABLE case_assumptions (      -- v0.1 §7.2 assumptions[]
  case_id TEXT, id TEXT, content TEXT,
  source TEXT NOT NULL,              -- user_confirmed | public_data | prior_borrowed | llm_inferred
  confirmed_at TEXT,
  revised_from TEXT,                 -- HypothesisRevised.before
  PRIMARY KEY (case_id, id)
);

CREATE TABLE case_equilibria (        -- v0.1 §7.2 equilibria_found[]
  case_id TEXT, eq_id TEXT, eq_type TEXT, refined INTEGER,
  prob_milli INTEGER, drivers TEXT,  -- JSON array of SelectorId
  PRIMARY KEY (case_id, eq_id)
);

CREATE TABLE case_outcomes (          -- 回测闭环（v0.1 §7.2 outcome/calibration）
  case_id TEXT PRIMARY KEY,
  occurred_at TEXT, realized_eq TEXT,
  logloss_milli INTEGER, weight_update TEXT
);

CREATE TABLE projection_cache (        -- §8.5 的 fail-soft 缓存
  case_id TEXT, key TEXT, ver INTEGER, seq INTEGER, val TEXT,
  PRIMARY KEY (case_id, key)
);

CREATE VIRTUAL TABLE case_fts USING fts5(case_id, narrative);  -- 中文需另配分词
```

> **向量检索的位置**：v0.1 §7.1/§7.2 的 `Chroma` + BGE embedding 路径**保留原状**（Apache-2.0 / MIT，许可安全），但它**不在** harness 的关键路径上——`shangyi-kb` 通过进程边界调用它（§3.7 的 sidecar 原则）。harness 只负责 `index.db` 的结构化字段与 FTS。

**写盘协议（durability）：**

| 时机 | 动作 | 理由 |
|---|---|---|
| 每个事件 append | `write` + `BufWriter` 缓冲 | 吞吐 |
| **强制 fsync 点**：会话创建、`turn/end`、`CheckpointOpened`、`CheckpointAnswered`、`PhaseApproved`、`CaseArchived`、销毁 | `fsync` | 对应 dsh 的强制写点（GB 02 §5）；checkpoint 是**承重的**（铁律 2 的证据），绝不能因掉电丢失 |
| 归档 | 压缩 + 移入 `archived_cases/` | 对应 `rollout/compress` |
| 崩溃后 | 读 `case.jsonl` → 若最后 turn 未闭合，调 `interrupted_turn_closers` 合成收尾 → **追加**（不改写历史） | §7.1 |

**写锁**：`rollout` crate 已有 `writer_lock.rs`，`直接复用`（防止同一 case 被两个 `shangyi` 进程同时写）。

### 8.4 resume / fork / rewind

**三个操作，三种语义（对应 GB 01 §6 的能力 + 商奕的审计要求）：**

| 操作 | 语义 | harness 原语 | 商奕的附加规则 |
|---|---|---|---|
| **resume**（续聊） | 从磁盘恢复同一案例继续 | `ThreadManager::resume_thread_from_rollout`；CLI `codex resume`；app-server `thread/resume` | `PlaybookSelected` 必须从日志读回，**不得**用当前配置重新推断（GB 02 §2：preset 选择是持久的，"恢复出不同组合会让模型面对无法执行的历史"） |
| **fork**（派生分支） | 以某案例为父，派生独立案例 | `ThreadManager::fork_internal_session`、`ForkSnapshot`、`forked_from_id`；app-server `thread/fork` | 用途：**假设敏感性对照**（同一问题两套 payoff 假设，比较结论差异）。`CaseHeader.forked_from` 记录父案例 + 父 `seq` |
| **rewind**（回退） | 退回某 checkpoint 重做 | **不**用 `truncate_rollout_after_turn_id`（它改写历史） | 见下 |

**为什么不用 truncate 做 rewind。** Codex 确实提供 `truncate_rollout_after_turn_id` / `truncate_rollout_before_turn_id`（GB 01 §6），但对商奕它是错误的默认：它会**删除**已发生的交互，而 v0.1 §2.2 要求"每次 checkpoint 的用户回答全部留档"。因此：

```
rewind(case, to_cp = cp2, reason = "payoff 需修正")
  ① 追加 log-only `PhaseRewound { from: Select, to: ModelBuild, due_to: CheckpointRevision(cp2) }`
  ② 追加 surface `Replace { start: <cp2 之前的最后一个节点>, end: <当前最后节点>,
                            sources: [遮蔽范围里的每一个节点] }`
     → 模型在下一 turn 看到的历史是"被遮蔽后"的视图（不含已作废的 select 结论）
     → 但日志里那些节点仍在，`SessionEventSurface::Shadowed` 可查（GB 02 §13 的三值查询）
  ③ 追加 log-only `HypothesisRevised` / `CheckpointOpened(cp2 重开)`
  ④ 目录不移动、seq 不减、`Case::seq() == events.len()` 保持
```

**硬删除是管理员命令**：`shangyi case prune --force --before <date>`，走 Codex 的 `codex delete --force` 路径（GB 01 §6），需二次确认，并在 `$SHANGYI_HOME/logs/` 留一条删除审计记录。

**崩溃恢复的具体行为**（必须与 §7.1 一致）：

```rust
/// 冷启动 resume 的确定性收尾。幂等：对已闭合的日志是 no-op。
pub fn repair_on_resume(events: &[Event]) -> Vec<Event> {
    // 复用 Codex 的 resume 能力 + dsh 的 repair（GB 02 §4）
    // 1. 未匹配的工具调用 → 补 ToolResult::Error { code: TOOL_NOT_STARTED | TOOL_OUTCOME_UNKNOWN }
    // 2. 开放的 step → 补 StepEnd
    // 3. 开放的 turn → 补 TurnEnd { reason: Interrupted }（时间戳 = 最后一个真实事件）
    interrupted_turn_closers(events)
}
```

### 8.5 投影（projections）

**模型（`仅借鉴设计` 自 dsh §5，Rust 落地）：**

```rust
/// 投影定义。`apply` 必须**纯、同步**；不关心某事件时必须返回**同一个 Arc**
/// （这是 `Object.is` 零下游工作捷径的类比：`Arc::ptr_eq`）。
pub struct ProjectionDef<S> {
    pub key: &'static str,
    pub ver: u32,                                // 提升即作废缓存行
    pub init: fn(&CaseHeader) -> S,
    pub apply: fn(&S, &Event) -> Arc<S>,
    pub view: Option<fn(&S) -> serde_json::Value>, // 对外暴露的只读视图
}

/// 缓存行。**"The cache is a fold shortcut, never an authority"**：
/// 行可能过期（`seq` 说明过期程度）但**永不错误**，故每条写路径都是 fail-soft；
/// `ver` 不匹配则**丢弃该行而不是迁移它**（GB 02 §5）。
#[derive(Serialize, Deserialize)]
pub struct CacheRow { pub ver: u32, pub seq: i64, pub val: serde_json::Value }

impl<S: serde::de::DeserializeOwned> ProjectionDef<S> {
    pub fn restore(&self, row: &CacheRow) -> Option<S> {
        if row.ver != self.ver { return None; }               // 绝不迁移；重建
        serde_json::from_value(row.val.clone()).ok()
    }
}
```

**商奕注册的投影单元：**

| key | 状态 | 派生自 | 用途 |
|---|---|---|---|
| `phase` | `SessionPhase` | `PhaseEntered`/`PhaseRewound`（最后胜出） | 阶段闸门、进度条 |
| `consult_state` | `ConsultState`（合并下面几项 + `artifacts`） | 全部领域事件 | `PreStepHook` 的单一读取点（§7.4） |
| `hypotheses` | `Vec<Hypothesis>` | `HypothesisRevised`、`CheckpointAnswered` | 报告 ④ 层假设清单 |
| `open_questions` | `Vec<OpenQuestion>` | `CheckpointOpened`/`Answered`、`ask_user` 结果 | 未决问题提示、断点续聊的恢复点 |
| `evidence_ledger` | `EvidenceLedger` | `EvidenceLedgerAppend`、`ToolResult`(web/intel) | 报告 ③ 层证据与来源 |
| `budget` | `BudgetCounter` | `BudgetConsumed`、`TokenCount` | v0.1 §12 的预算上限风险控制 |
| `swarm_actions` | `BTreeMap<PlayerId, ActionHistogram>` | `SwarmRollout` | `persona_sim` 选择器的输入 |
| `selection` | `SelectionOutput` | `SelectionWeights`、`ArtifactProduced(SelectionOutput)` | 报告主图数据 |

```rust
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ConsultState {
    pub phase: SessionPhase,
    pub artifacts: BTreeMap<ArtifactKind, ArtifactRef>,   // 内容寻址 + digest
    pub hypotheses: Vec<Hypothesis>,
    pub open_questions: Vec<OpenQuestion>,
    pub evidence: EvidenceLedger,
    pub budget: BudgetCounter,
    pub checkpoints: BTreeMap<CheckpointId, CheckpointStatus>,
    pub approved_fingerprints: BTreeMap<SessionPhase, String>,  // §7.7
}

impl ConsultState {
    /// 阶段闸门的判据之一：产物存在。
    pub fn has_artifact(&self, k: &ArtifactKind) -> bool { self.artifacts.contains_key(k) }

    /// 阶段闸门的判据之二：批准时的指纹与当前产物摘要一致（§7.7）。
    /// 若批准后被改动，则要求重新过一次 checkpoint。
    pub fn approval_stale(&self, p: SessionPhase, main: ArtifactKind) -> bool {
        match (self.approved_fingerprints.get(&p), self.artifacts.get(&main)) {
            (Some(fp), Some(a)) => fp != &a.digest,
            _ => false,   // 未批准过（由 checkpoint 状态另行判定）或无产物
        }
    }
}
```

**驱动与写回策略（GB 02 §5）**：投影**急切驱动**（每个已注册单元在事件 append 时同步 `apply`），一致读切点由 `ProjectionSnapshot { as_of_seq, values }` 给出；缓存行写回用**有界 channel**（`tokio::sync::mpsc`），写失败 **fail-soft**（缓存过期无害，重建即可）。强制写点为会话创建、`turn/end`、销毁；其间按 `write_every_events`/`write_interval_ms` 节流。

**OTel 是投影而非第二个真相源**（GB 02 §13，保持这一点）：`opentelemetry` + `tracing-opentelemetry` 的 span 属性集取自 log-only 事件，**不得**引入独立埋点成为权威数据。

### 8.6 案例归档映射（回测校准的数据来源）

**归档触发**：`report` 阶段产物签收后，模型调用 `case_store_write` → 追加 `CaseArchived` → 日志压缩并移入 `archived_cases/`；`index.db` 的 `cases` 行写 `archived_at`。

**哪些事件成为案例库记录（v0.1 §7.2 schema → 事件来源）：**

| v0.1 §7.2 字段 | 事件来源 | 备注 |
|---|---|---|
| `case_id` | `CaseHeader.case_id` | 不可变 |
| `date` | `CaseHeader.created_at` | |
| `client_anonymous` | `CaseHeader.client_anon` | 用户输入，仅存匿名化标识 |
| `scenario` | `PlaybookSelected` | |
| `decision_question` | `CaseBrief` 产物的 `ArtifactProduced.provenance` 指向的 `UserMessage`（intake 首条） | |
| `assumptions[]` | `CheckpointAnswered`（②）+ `HypothesisRevised` | `source` 字段即 `HypothesisSource`；**每条假设可溯源到某次用户确认**（铁律 2 的证据） |
| `game_spec_ref` | `ArtifactProduced { kind: GameSpecValidated, digest }` | 内容寻址 → `artifacts/gamespec.yaml` |
| `equilibria_found[]` | `ArtifactProduced { kind: EquilibriumSet }` | |
| `selection_output.distribution` | `ArtifactProduced { kind: SelectionOutput }` | |
| `selection_output.selector_weights_snapshot` | `SelectionWeights*`（全量） + `ConfigResolved.digest` | **权重快照必须随案例存**，否则回测无法复现当时的融合 |
| `recommendation` | `ArtifactProduced { kind: RecommendationPack }` | |
| `outcome` | `OutcomeRecorded`（由 `shangyi backtest` 在日后追加，用户提供实际发生情况） | 允许在归档后追加 → 归档案例**不是只读的终态** |
| `calibration` | 由 `shangyi backtest` 计算的 `logloss` / `weight_update`，写回 `case_outcomes` | 闭环校准（v0.1 §5.4 融合机制） |

**两条归档纪律：**

1. **归档不改写日志**：`OutcomeRecorded` 与后续的 `SelectionWeights` 校准是**追加**到同一 `case.jsonl`（即使已在 `archived_cases/`）；因此 `Case::seq()` 单调不减，`index.db` 的 `last_seq` 同步更新。
2. **回测只读投影**：`shangyi backtest` 不重放模型交互，只折叠 `hypotheses` / `selection` / `outcome` 投影，用**纯函数**计算 logloss 与权重更新（可复现、可单测）。这是铁律 1 在回测路径上的落点——回测数字也来自确定性代码。

### 8.7 checkpoint 回答的持久记录

**这是铁律 2 的物理证据，单独成节。**

**双向记录（两个事件 + 一个 surface 通道）：**

```rust
#[derive(Clone, Serialize, Deserialize)]
pub struct CheckpointItem {
    pub key: String,                    // 如 "P1"
    pub content: String,                // 如 "对手降价后本企业利润率下降 8pp"
    pub proposed: serde_json::Value,    // 模型提议的值（如 42.0 ± 8）
    pub source: HypothesisSource,       // user_confirmed | public_data | prior_borrowed | llm_inferred
    pub evidence: Vec<ProvenanceRef>,   // 指向数据/工具产出
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum CheckpointAnswer {
    Confirm { key: String },
    Modify  { key: String, new_value: serde_json::Value, note: Option<String> },
    Query   { key: String, question: String },   // "追问"（v0.1 §8.3 的三操作之一）
}

#[derive(Clone, Serialize, Deserialize)]
pub struct CheckpointAnswered {
    pub id: CheckpointId,
    pub answers: Vec<CheckpointAnswer>,
    pub user_actor: ActorId,            // 来自 live sender，不从模型文本推断
    pub at_ms: i64,
    pub resolved_sources: BTreeMap<String, HypothesisSource>,  // 结算后的来源表
}
```

| 记录项 | 事件 | Presence | 强制 fsync |
|---|---|---|---|
| 提问件本身（问了什么、模型提议什么） | `CheckpointOpened { items }` | **LogOnly** | ✅ |
| 用户回答（逐条 Confirm/Modify/Query + 来源结算） | `CheckpointAnswered { answers, user_actor, at_ms }` | **LogOnly** | ✅ |
| 回答**到达模型** | `UserMessage { text, source: ContextSource::Checkpoint { id } }` | **Surface** | ✅ |
| 因回答而修改的假设 | `HypothesisRevised { before, after, source, revised_by }` | **LogOnly** | 常规 |
| 阶段批准 | `PhaseApproved { phase, fingerprint }` | **LogOnly** | ✅ |

**三个设计要点：**

1. **`user_actor` 必须是 durable 的发送者归属**（GB 02 §8："the service derives durable sender attribution from the exact live sender"）。**禁止**从模型输出的文本推断"用户说了什么"。在 Swarm 场景里这一点尤其重要：persona 子会话的回答**不是**用户回答，二者必须在类型上不可混淆（`ActorId` 区分 `User` / `Persona(PlayerId)` / `Orchestrator`）。
2. **提问与回答必须成对提交**：`CheckpointOpened` 与 `CheckpointAnswered` 要么都在日志里，要么都没有（"returning an unlogged decision would violate the pair"，GB 02 §10）。实现上：`CheckpointOpened` 先落盘成功才允许展示提问件；用户作答后 `CheckpointAnswered` 落盘成功才允许恢复循环。任一落盘失败 → 询问失败（fail-closed，不变量 ②）。
3. **`Query`（追问）不改变假设状态**：它只追加一条 surface `UserMessage` 与一条 `CheckpointAnswered { action: query }`，checkpoint 仍为 `Open`。这使"用户只是想问清楚"与"用户确认了假设"在日志上**形态不同**——报告 ④ 层因此可以准确区分"已确认 N 条、其中 M 条经过追问"。

**checkpoint 的状态投影：**

```rust
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum CheckpointStatus {
    Open { id: CheckpointId, items: Vec<CheckpointItem> },
    /// 全部 item 已结算（Confirm 或 Modify）；`queries` 计数用于审计。
    Approved { id: CheckpointId, at_ms: i64, queries: u32, fingerprint: String },
    /// 用户明确拒绝 → 触发 `PhaseRewound`（§7.3 的 `CheckpointRejected`）。
    Rejected { id: CheckpointId, at_ms: i64, reason: String },
    /// 用户未作答而会话中断 → 恢复后仍为 `Open`，阶段闸门继续拒绝推进。
    /// ★ 绝不隐式转为 Approved。
    Abandoned { id: CheckpointId, last_seen_seq: Seq },
}
```

`Abandoned` 变体的存在是为了让"用户跑了"这一事实**显式**：它不会推进阶段，也不会让模型"猜"用户的意图。这是铁律 2 在异常路径上的守门——**唯一**能让 checkpoint 变成 `Approved` 的路径是 `CheckpointAnswered` 里出现覆盖全部 `items` 的 `Confirm`/`Modify`。

---

*（本文档为《商奕 Agent Harness 设计文档 v0.1》第 1–8 章；第 9–15 章由另一部分提供，合并后形成完整文档。本文中的每一处外部构件复用均带处置标签与来源许可，见 §1.5 标签定义、§4.1 逐来源表、§6.2 逐 crate 表。）*

## 9. 工具系统

本章规定商奕的工具抽象、分发管线与领域工具清单。总体策略是 **substrate 复用、工具面重写**（D-A8）：工具**抽象与分发机制**来自 Codex（Apache-2.0），**工具集合**整体替换为商奕领域工具，编码类工具（`exec_command`、`apply_patch`、`write_stdin`、`view_image`、Code Mode 的 `exec`/`wait`）全部移除。

### 9.1 工具抽象与注册表（复用面）

Codex 把工具原语刻意放在 `core` 之外，位于 `codex-rs/tools`（package `codex-tools`），使 wire 形状与 agent 循环解耦 —— 这正是商奕能安全复用的原因。可复用类型：

| 类型 | 位置 | 作用 | 商奕处置 |
|---|---|---|---|
| `ToolSpec` | `codex-tools::tool_spec` | 模型可见的工具声明；变体 `Function(ResponsesApiTool)` / `Freeform(FreeformTool)` / `Namespace(ResponsesApiNamespace)` / `ToolSearch` / `WebSearch` | `直接复用`（来源 Codex，Apache-2.0） |
| `ToolName { name, namespace }` | `codex-tools` | 命名空间化工具名；`DEFAULT_FUNCTION_NAMESPACE = "functions"`；`Display` 对非默认命名空间渲染 `namespace.name` | `直接复用` |
| `ToolExecutor` / `ToolExecutorFuture` | `codex-tools` | 工具执行 trait 与其 future 别名 | `直接复用` |
| `ToolCall` / `ToolPayload` / `ToolOutput` / `JsonToolOutput` | `codex-tools` | 调用与结果信封 | `直接复用` |
| `FunctionCallError` | `codex-tools` | 类型化工具错误 | `直接复用` |
| `OutputSchema` / `parse_tool_input_schema` | `codex-tools` | JSON Schema 输入/输出校验 | `直接复用` |
| `ToolExposure` / `ToolExposures` | `codex-tools` | 暴露面控制 | `修改后复用`：删去 `CodeModeOnly`/`DeferredModelOnly`/`Deferred` 三支（D-A4 移除 Code Mode；延迟工具发现对 14 个领域工具无收益）。⚠️ **本行与 vendor 计划 §2.6 末注冲突**（计划建议 W1 第一阶段**保留全部变体**、只在商奕域工具里恒用 `Direct`，因为删变体是"改逻辑"、rebase 冲突面大）；**本轮未改**，已登记为待跟进项（附录 D 清单第 4 条） |
| `ToolRouter` | `core/src/tools/router.rs`（原为 `pub use`） | `from_registry`/`from_parts`、`build_tool_call(ResponseItem) -> Result<Option<ToolCall>, FunctionCallError>`、`dispatch_tool_call_*`、`model_visible_specs() -> Arc<[ToolSpec]>`、`tool_supports_parallel`、`can_manage_children` | `直接复用`（`dispatch_tool_call_with_code_mode_result` 一支随 Code Mode 删除） |
| `ToolRegistry` | `core/src/tools/registry.rs` | `IndexMap<ToolName, RegisteredTool>` + `with_allowed_tools`、`add`、`add_with_exposure`、`register_trusted`、`register_external`、`prepend_trusted`、`remove`、`entries`、`dispatch_any_with_terminal_outcome` | `修改后复用`：作为 `shangyi-tools` 内的注册表实现，**保留**其 `register_external` 拒绝保留名（`exec_command`/`shell_command`）的纪律，保留名单改为商奕保留名 |
| `build_tool_router` | `core/src/tools/spec_plan.rs`（`pub(crate)`） | 装配点：`add_core_tool_sources` → MCP 追加 → `apply_mcp_tool_exposure_policy` → `append_extension_tool_executors` → `append_dynamic_tool_runtimes` → `hosted_model_tool_specs` → `finalize_tool_router` | `修改后复用`：装配顺序照搬，`add_core_tool_sources` 整体替换为 `add_domain_tool_sources`（D-A8） |
| `format_exec_output_for_model` / `format_exec_output_str` | `core/src/tools/mod.rs` | 唯一对 core 外部公开的输出格式化 | `自研`：改为 `format_tool_result_for_model`（领域结果整形 + provenance 注入） |
| `[tools]` 配置段 | `config` | 工具相关配置 | `修改后复用` |

**结构性约束（来自简报 01）**：`core/src/tools/mod.rs` 除 `ToolRouter` 与格式化函数外全部 `pub(crate)`，`spec_plan::build_tool_router` 亦为 `pub(crate)`。按 D-A2 的 fork-and-trim 路线这是可接受的 —— 商奕直接在新 workspace 内改这些模块，但由于 `shangyi-*` 领域 crate **只允许依赖 harness 的公开面**，`ToolRegistry` 的注册能力必须提升为公开 API（列入 v0.2 revision item）。

### 9.2 分发管线与领域策略挂载点

分发沿 dsh 的模式（`tools/pre-execute` / `tools/execute` / `tools/post-execute`，见简报 02 第 D.6 项）落为 Rust 三层。参考 **仅借鉴设计**（上游 dsh，MIT © 2026 DeepSeek），机制用 Rust 原生实现。

| 挂载点 | 语义 | Rust 机制 | 商奕领域策略 |
|---|---|---|---|
| `tools/pre-execute` | 在调用前决定 `allow` / `deny(reason)` / `ask(reason)` | 有序 handler `Vec` + 显式决策 enum（**不**用 tower，因为它能终止管线） | 剧本工具白名单校验；报告期禁止任何写入类工具；`python_sidecar_exec` 恒为 `ask`（或在非交互环境为 `deny`） |
| `tools/execute` | around-wrapper：超时、重试、度量 | `tower::Layer` + `ServiceBuilder`（最外层优先的 `next` 语义由 tower 免费给出） | 求解工具超时与结果 schema 校验；`web_fetch` 限速；每个工具结果追加进证据台账 |
| `tools/post-execute` | accept（可替换 `content` **异或** `value`）或 `block(feedback)` 转为错误结果 | 有序 handler `Vec` | provenance 强制：无 `provenance` 字段的数值结果一律 `block`（铁律 1 的机器落点） |

另一条必须保留的规则：**单调 `ToolGuard` 在可扩展 waterfall 之后运行且只能 deny** —— "listener ordering cannot turn a denial back into permission"（简报 02）。商奕用它承载反垄断措辞红线与合规黑名单，使其不可能被剧本层的 handler 覆盖。

```rust
// shangyi-tools/src/pipeline.rs
#[async_trait::async_trait]
pub trait PreExecuteHook: Send + Sync {
    async fn on_pre_execute(&self, call: &ToolCall, cx: &TurnCx) -> PreDecision;
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreDecision {
    Allow,
    Deny { reason: String },
    Ask { reason: String },
}

#[async_trait::async_trait]
pub trait PostExecuteHook: Send + Sync {
    /// 只能替换 content（异或）或 block；不得改写调用身份。
    async fn on_post_execute(&self, out: ToolOutput, cx: &TurnCx) -> PostDecision;
}
#[derive(Debug, Clone)]
pub enum PostDecision {
    Accept(Box<ToolOutput>),
    Block { feedback: String },
}

/// 单调 guard：位于 waterfall 之后，只能 deny，不能反向放行。
pub trait ToolGuard: Send + Sync {
    fn check(&self, call: &ToolCall, cx: &TurnCx) -> Result<(), Deny>;
}

pub struct ToolPipeline {
    pre: Vec<Arc<dyn PreExecuteHook>>,
    guards: Vec<Arc<dyn ToolGuard>>,   // 恒在 pre 之后
    post: Vec<Arc<dyn PostExecuteHook>>,
}

impl ToolPipeline {
    pub async fn dispatch(&self, call: ToolCall, cx: &TurnCx) -> Result<ToolOutput, FunctionCallError> {
        for h in &self.pre {
            match h.on_pre_execute(&call, cx).await {
                PreDecision::Allow => {}
                PreDecision::Deny { reason } => return Err(FunctionCallError::Denied(reason)),
                PreDecision::Ask { reason } => {
                    // 无应答者 ⇒ Unavailable ⇒ 视为拒绝（见 11.8）
                    match cx.approval.request(ApprovalRequest::tool(&call, reason)).await? {
                        ApprovalOutcome::AllowedOnce => {}
                        o => return Err(FunctionCallError::Denied(format!("approval: {o:?}"))),
                    }
                }
            }
        }
        for g in &self.guards {
            g.check(&call, cx).map_err(FunctionCallError::Denied)?;
        }
        let out = cx.execute_with_timeout(&call).await?;   // tower ServiceBuilder 包裹
        for h in &self.post {
            match h.on_post_execute(out, cx).await {
                PostDecision::Accept(o) => return Ok(*o),
                PostDecision::Block { feedback } => {
                    return Err(FunctionCallError::Blocked { feedback })
                }
            }
        }
        unreachable!("post hook 链必须以 Accept/Block 结束")
    }
}
```

### 9.3 商奕领域工具清单

14 个领域工具（`web_search`/`web_fetch`/`data_ingest`/`kb_retrieve`/`case_store_read`/`case_store_write`/`participant_profile_update`/`report_render` 沿用 v0.1 §4 的语义，其余为新增）。实现位置均在新建的 `shangyi-tools` crate 内，按其 `ToolSpec.namespace` 分组。

| 工具（wire 名） | 命名空间 | 输入 | 输出 | 实现位置 | disposition（+来源/许可） | 与 v0.1 §4 工具层的关系 |
|---|---|---|---|---|---|---|
| `game_solve` | `shangyi` | `{game_spec_ref, solvers: [support_enum \| lemke_howson \| backward_induction], refine: [trembling_hand \| sequential]} ` | `EquilibriumSet {equilibria: [{id, type, strategies, expected_payoffs, refined}]}` | `shangyi-tools/src/game_solve.rs` → `shangyi-solver` | `自研`（工具壳复用 Codex `ToolSpec`/`ToolExecutor`，`直接复用`） | **取代** §4 `sandbox_exec` 中的 `nashpy`/`python-gambit` 求解路径（D-A6） |
| `equilibrium_select` | `shangyi` | `{equilibrium_set_ref, selectors: [risk_dom \| stochastic \| learning \| behavioral \| focal_llm \| persona_sim], weights}` | `{distribution: [{eq, prob, drivers}], confidence, calibration_note}` | `shangyi-tools/src/equilibrium_select.rs` → `shangyi-select` | `自研` | §5.4 全部选择器由 Python 数值内核改为 Rust 原生（D-A6/D-A7） |
| `consequence_predict` | `shangyi` | `{game_spec_ref, action_d}` | `ConsequenceReport {d → [(reaction r, P(r\|d), 各方结果分布, drivers)]}` | `shangyi-tools/src/consequence_predict.rs` → `shangyi-engine` | `自研` | §5.6.1 whatif 管线的工具化封装；原先隐含在 sandbox 调用内 |
| `satisfice_search` | `shangyi` | `{game_spec_ref, action_templates, param_grid, aspiration}` | `DiscoveryReport {满意策略 ≤5, 达标概率, 最差情形, 帕累托前沿}` | `shangyi-tools/src/satisfice_search.rs` → `shangyi-engine` | `自研` | §5.6.2 discover 模式 |
| `monte_carlo` | `shangyi` | `{params, n<=1000, seed, targets}` | `{P10, P50, P90 per target, provenance}` | `shangyi-tools/src/monte_carlo.rs` → `shangyi-numeric` | `自研`（数值栈用 `rand`/`rand_distr`/`statrs`，MIT / MIT OR Apache-2.0） | §5.5 敏感性/翻转阈值的传播器；原先在 sandbox |
| `web_search` | `shangyi` | `{query, max_results}` | `[{title, url, snippet}]` | `shangyi-tools/src/web_search.rs` | `自研`（背后依赖可选后端 `ddgs`，MIT）| §4 同名工具，schema 与多后端链式回退设计沿用 v0.1 §4 |
| `web_fetch` | `shangyi` | `{url}` | `{text, title, fetch_time}` | `shangyi-tools/src/web_fetch.rs` | `自研`（正文抽取依赖 `trafilatura`，Apache-2.0，经侧车，见 9.5） | §4 同名工具 |
| `data_ingest` | `shangyi` | `{file_path}` | `{schema, profile, sample, provenance}` | `shangyi-tools/src/data_ingest.rs` | `自研`（解析用 Rust `csv`/`calamine`，`[推断]` 具体 crate） | §4 同名工具 |
| `kb_retrieve` | `shangyi` | `{query, top_k}` | `[{doc, section, score, theory_id}]` | `shangyi-tools/src/kb_retrieve.rs` → `shangyi-kb` | `自研`（向量检索经侧车；依赖 `chromadb` Apache-2.0 + BGE，MIT） | §4 同名工具；理论库条目 schema（§7.1）不变 |
| `case_store_read` | `shangyi` | `{case_id \| query}` | `case 对象 / 相似案例列表` | `shangyi-tools/src/case_store.rs` | `自研`（SQLite 经 `rusqlite`） | §4 `case_store_write/read` 拆为两个工具 |
| `case_store_write` | `shangyi` | `case 对象` | `{case_id, written_at}` | `shangyi-tools/src/case_store.rs` | `自研` | 同上；写入受 `ToolGuard` 强制 provenance |
| `participant_profile_update` | `shangyi` | `{participant_id, events[]}` | `profile`（含贝叶斯后验） | `shangyi-tools/src/participant_profile.rs` | `自研` | §4 同名工具；schema 为 §6.2 |
| `figure_render` | `shangyi` | `{figure_spec, format}` | `{artifact_path, content_hash}` | `shangyi-tools/src/figure_render.rs` → Python 侧车 | `自研`（仅渲染侧调用 `matplotlib`，PSF 系）；规格/渲染分离见第 15 章 | v0.1 §4 把绘图藏在 `sandbox_exec` 内；此处提升为独立工具 |
| `report_render` | `shangyi` | `{case_id, format: pdf\|docx\|md}` | `{file_path, layers[]}` | `shangyi-tools/src/report_render.rs` → `shangyi-report` | `自研`（最终格式调用外部进程 `pandoc`，GPL-2.0-or-later，见 15.1） | §4 同名工具；管线见第 15 章 |
| `python_sidecar_exec` | `shangyi` | `{code, files[], timeout_ms}` | `{stdout, artifacts[], provenance[]}` | `shangyi-tools/src/python_sidecar.rs` | `自研`；其 fail-closed 沙箱语义 `仅借鉴设计`（dsh，MIT © 2026 DeepSeek） | **取代** §4 `sandbox_exec` 的"LLM 写 Python"分支，且收窄为临时数值任务 |

**移除清单（D-A4/D-A8）**：`exec_command`、`write_stdin`、`request_permissions`（shell 变体）、`apply_patch`、`update_plan`、`view_image`、`tool_search`、`exec`/`wait`（Code Mode）、`web_search` 的 hosted `ToolSpec::WebSearch` 变体（改为自持 `web_search` 工具）、`send_message`/`followup_task`/`interrupt_agent`/`list_agents`（保留 v1 的 `spawn_agent`/`send_input`/`wait_agent`/`resume_agent`/`close_agent`，见第 12 章）。Codex **没有** `local_shell` 工具，也**没有** `read_file` 工具（读文件走 `exec_command`/MCP resources）—— 商奕不需要 shell，故不补位。

#### 9.3.1 `game_solve` 的 `ToolSpec` / `ToolExecutor` 实现

`ToolSpec` 与 `ToolExecutor` 的形状来自 `codex-tools`（`直接复用`，Codex / Apache-2.0）；下面的领域实现为 `自研`。结构体字段名、`OutputSchema` 构造器与 `FunctionCallError` 变体名为商奕侧拟制（`[推断]`），代码落笔时须以 vendored 快照中的实际签名为准。

```rust
// shangyi-tools/src/game_solve.rs
use codex_tools::{ToolSpec, ResponsesApiTool, ToolExecutor, ToolExecutorFuture,
                  ToolInvocation, ToolOutput, JsonToolOutput, FunctionCallError, OutputSchema};
use shangyi_solver::{SolverRequest, EquilibriumSet, SolverBackend, Refinement};

pub const GAME_SOLVE_TOOL_NAME: &str = "game_solve";
pub const SHANGYI_NAMESPACE: &str = "shangyi";

/// 模型可见声明。namespace 非默认值 ⇒ wire 侧名 `shangyi.game_solve`
/// （Chat wire 下拍平为 `shangyi__game_solve`，见 10.2）。
pub fn game_solve_spec() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: GAME_SOLVE_TOOL_NAME.to_string(),
        namespace: Some(SHANGYI_NAMESPACE.to_string()),
        description: "枚举并精炼商奕博弈的均衡解集。输入必须引用已确认的 GameSpec 版本；\
                      不执行任何 LLM 生成的代码，结果为确定性输出。".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "required": ["game_spec_ref", "solvers"],
            "additionalProperties": false,
            "properties": {
                "game_spec_ref": { "type": "string", "description": "GameSpec 版本引用，如 case-2026xxxx-pricing#v3" },
                "solvers": {
                    "type": "array", "minItems": 1,
                    "items": { "enum": ["support_enumeration", "lemke_howson", "backward_induction"] }
                },
                "refine": {
                    "type": "array",
                    "items": { "enum": ["trembling_hand", "sequential"] }
                },
                "tolerance": { "type": "number", "default": 1e-9 }
            }
        }),
        // 输出 schema 供 tools/post-execute 做结构校验（Codex 既有 OutputSchema）
        output_schema: Some(OutputSchema::json_schema(serde_json::json!({
            "type": "object",
            "required": ["equilibria"],
            "properties": {
                "equilibria": { "type": "array", "items": {
                    "type": "object",
                    "required": ["id", "type", "strategies", "expected_payoffs", "refined"],
                    "properties": {
                        "id": {"type": "string"},
                        "type": {"enum": ["pure", "mixed", "behavioral", "subgame_perfect"]},
                        "strategies": {"type": "object"},
                        "expected_payoffs": {"type": "object"},
                        "refined": {"type": "boolean"}
                    }
                }},
                "provenance": {"type": "array", "items": {"type": "object"}}
            }
        }))),
    })
}

pub struct GameSolveTool {
    solver: Arc<dyn SolverBackend>,        // shangyi-solver，纯 Rust，无沙箱参数
    store: Arc<GameSpecStore>,
    provenance: Arc<ProvenanceWriter>,
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for GameSolveTool {
    fn tool_name(&self) -> &str { GAME_SOLVE_TOOL_NAME }

    fn execute(&self, inv: ToolInvocation) -> ToolExecutorFuture<'_> {
        Box::pin(async move {
            // 1) 反序列化入参：schema 已由 ToolRouter 校验，此处只做类型映射
            let args: GameSolveArgs = serde_json::from_value(inv.arguments.clone())
                .map_err(|e| FunctionCallError::MalformedArgs(e.to_string()))?;

            // 2) 取 GameSpec —— 必须是 checkpoint ② 已确认的版本（铁律 2）
            let spec = self.store.load_confirmed(&args.game_spec_ref).await
                .map_err(|e| FunctionCallError::Denied(format!("GameSpec 未确认或不存在: {e}")))?;

            // 3) 规模门禁：D-A6 支持枚举限 n≤4；超限返回类型化错误，绝不静默降级
            if spec.players.len() > 4 {
                return Err(FunctionCallError::Denied(
                    "支持枚举上限为 4 名玩家；当前规模请改用数值路径（monte_carlo + 网格最佳反应）".into()));
            }

            // 4) 求解（纯 Rust；无进程边界、无沙箱依赖、无 LLM 参与）
            let req = SolverRequest {
                game: spec.clone(),
                solvers: args.solvers,
                refinements: args.refine.unwrap_or_default(),
                tolerance: args.tolerance.unwrap_or(1e-9),
            };
            let set: EquilibriumSet = self.solver.solve(req).await
                .map_err(|e| FunctionCallError::Execution(format!("求解失败: {e}")))?;

            // 5) 写 provenance：报告里每一个均衡数值都必须指向本次调用（铁律 1）
            let prov = self.provenance.record(ProvenanceKind::ToolOutput, &inv,
                                             &spec, "shangyi-solver").await?;

            Ok(ToolOutput::Json(JsonToolOutput(
                serde_json::to_value(GameSolveOutput { equilibria: set.equilibria, provenance: prov })?)))
        })
    }
}

// 注册进 ToolRegistry（装配点见 9.1 的 spec_plan）
pub fn register(reg: &mut ToolRegistry, deps: GameSolveDeps) {
    reg.add_with_exposure(game_solve_spec(), Arc::new(GameSolveTool::new(deps)),
                          ToolExposure::Direct)?;
}
```

### 9.4 暴露面与并行

`ToolExposure` 裁剪后按 §9.1 的处置**打算**保留 `Hidden` 与 `Direct` 两支 —— ⚠️ 该处置与 vendor 计划 §2.6 末注冲突（计划建议第一阶段不删任何变体），**本轮未改**，见附录 D 清单第 4 条。下面描述的是**商奕领域工具实际采用的 exposure 取值**，与"是否删除 enum 变体"是两回事。领域工具的默认暴露策略：

| 工具 | 默认 exposure | 理由 |
|---|---|---|
| `game_solve`、`equilibrium_select`、`consequence_predict`、`monte_carlo` | `Direct` | 主 agent 直接驱动 eval→prediction |
| `satisfice_search` | `Direct`（仅 `discover` 模式剧本开启） | 剧本白名单控制 |
| `web_search`、`web_fetch`、`data_ingest`、`kb_retrieve` | `Direct`（`intel`/`model` 阶段） | 情报层 |
| `case_store_read` | `Direct` | 参考历史 |
| `case_store_write`、`report_render` | `Hidden` → 仅在 `report`/`archive` 阶段由剧本解禁 | 防止中途覆写交付物 |
| `participant_profile_update` | `Direct`（仅主 agent；persona 子智能体不可见，见第 12 章） | 画像一致性由主 agent 独占 |
| `figure_render` | `Hidden` | 由 `report_render` 内部调用；模型不直接触发 |
| `python_sidecar_exec` | `Direct` 但恒过 `ask` | 见 11.3 |

并行：`tool_supports_parallel` 仅对只读、无共享可变状态的工具返回 `true` —— `web_search`、`web_fetch`、`kb_retrieve`、`case_store_read`、`monte_carlo`。`game_solve` 与 `equilibrium_select` 串行（共享求解缓存与 RNG 种子流），`python_sidecar_exec` 串行（单进程池）。

### 9.5 Python 侧车桥（subprocess + JSON-RPC over stdio）

**为什么是子进程而不是进程内嵌入。** 三个理由逐条对应简报 03 §3：

1. **许可**：进程内 import（PyO3/CPython 嵌入）在主流保守解读下把被嵌入的库与 Rust 二进制组合成单一衍生作品。商奕的侧车要跑 `chromadb`（Apache-2.0）、`trafilatura`（Apache-2.0）、`matplotlib`（PSF 系）—— 这些本身宽松，但 `pygambit` 是 GPL-2.0-or-later，**Gambit 已按 D-A6 整体弃用**；把"永不进程内嵌入第三方 Python"写成架构规则，是为了让这条红线不依赖某一次依赖审查。子进程 + stdin/stdout JSON 是被接受的 "separate programs" 立场。
2. **运行时耦合**：进程内嵌入需要 CPython 运行时、`maturin`/wheels、ABI 与版本耦合；商奕的发货物是一个 Rust 二进制，不应要求目标机安装匹配的 CPython。
3. **沙箱**：侧车本来就必须跑在 fail-closed 沙箱内（第 11 章）。既然进程边界与沙箱边界重合，就没有理由再开一条进程内通道。

**协议**：JSON-RPC over stdio，最长 4 字节大端长度前缀分帧（与 Codex `code-mode-host` 的 framed host 协议同构，但实现自研）。一请求一响应，`id` 单调递增；服务端 `stderr` 只做日志，不参与协议。会话级单例进程，超时后 kill 并重启。

```rust
// shangyi-tools/src/python_sidecar.rs
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command, ChildStdin, ChildStdout};

pub struct SidecarClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    confined: ConfinedArgv,        // 只接受 Confined，永不接受裸 Vec<String>（见 11.1）
    python_exe: std::path::PathBuf,
    script: std::path::PathBuf,
}

impl SidecarClient {
    /// 唯一构造函数：必须提供已被 enforce 的 argv。
    pub fn spawn(confined: ConfinedArgv, policy: &SidecarPolicy) -> Result<Self, SandboxUnavailable> {
        let mut child = Command::new(&confined.argv[0])
            .args(&confined.argv[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| SandboxUnavailable::RunnerFailed)?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        Ok(Self { child, stdin, stdout, next_id: 1, confined,
                  python_exe: policy.python_exe.clone(), script: policy.entry_script.clone() })
    }

    pub async fn call(&mut self, method: &str, params: serde_json::Value,
                      timeout: std::time::Duration) -> Result<serde_json::Value, SidecarError> {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let body = serde_json::to_vec(&req)?;
        let mut frame = (body.len() as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&body);

        tokio::time::timeout(timeout, async {
            self.stdin.write_all(&frame).await?;
            self.stdin.flush().await?;
            let mut len_buf = [0u8; 4];
            self.stdout.read_exact(&mut len_buf).await?;
            let n = u32::from_be_bytes(len_buf) as usize;
            let mut buf = vec![0u8; n];
            self.stdout.read_exact(&mut buf).await?;
            Ok::<_, std::io::Error>(serde_json::from_slice::<serde_json::Value>(&buf)?)
        })
        .await
        .map_err(|_| SidecarError::Timeout)?
        .map_err(|e| SidecarError::Transport(e.to_string()))
    }
}

/// 侧车允许的方法面 —— 白名单，不是自由代码执行面。
pub const SIDECAR_METHODS: &[&str] = &[
    "figure.render",      // matplotlib → PNG/SVG
    "kb.embed",           // BGE embedding
    "kb.search",          // chromadb 检索
    "extract.readability",// trafilatura 正文抽取
    "verify.nashpy",      // CI 差分验证专用（生产配置禁用）
];
```

**侧车不做的事**：`game_solve`/`equilibrium_select`/`monte_carlo` 等核心求解工具**不经过 Python**。理由（简报 03 §3 与 §4）：

- **可复现与可审计**：Rust 求解器在同一输入下确定性输出，且与 LLM 无关；"LLM 不算数"铁律的更强形式是"**求解过程也不依赖生成代码**"。
- **许可**：Python 求解路径一旦需要 `pygambit` 即触发 GPL 风险；保留侧车为求解通道等于保留这条风险。
- **沙箱面**：Python 侧车跑的东西越多，fail-closed 沙箱的失败模式就越多。绘图与 embedding 是无状态、无博弈语义的，失败可降级（图缺失）或阻塞（embedding 失败则明确报错）；求解失败则整条咨询失效，不应放在隔离度最高的进程里。
- **性能**：支持枚举与 Lemke–Howson 在小规模矩阵上是毫秒级；进程往返开销相对不可忽略。

`nashpy`（MIT）**只保留在 CI 差分测试**中，通过 `verify.nashpy` 方法进程隔离调用，**从不出现在生产工具白名单**（D-A6）。

### 9.6 MCP：第三方扩展

Codex 的 MCP 支持面完整保留：

| 组件 | 位置 | disposition |
|---|---|---|
| MCP 客户端与生命周期 | `codex-mcp`（`McpBinding`、`McpRuntime`、`McpStartupPolicy`、`McpCatalogBuilder`、`ResolvedMcpCatalog`、`McpResourceClient`、`ElicitationLifecycle`、`McpToolCatalogCache`） | `直接复用`（Codex，Apache-2.0） |
| 传输与认证 | `rmcp-client`（stdio + streamable HTTP、OAuth 发现、`McpProtocolMode`、`McpAuthState`、`InProcessTransportFactory`） | `修改后复用`：OAuth 与企业托管认证对商奕场景非必需，保留 stdio 与 streamable HTTP |
| 配置键 | `[mcp_servers.<name>]`、`mcp_optional_startup_grace_ms` | `直接复用`；`mcp_enterprise_managed_auth`、`mcp_oauth_*` 裁剪 |
| 运行时事件 | `Op::RefreshMcpServers`、`EventMsg::McpStartupUpdate`/`McpStartupComplete`/`McpToolCallBegin`/`McpToolCallEnd`/`ElicitationRequest` | `直接复用` |
| 暴露策略 | `apply_mcp_tool_exposure_policy`、`mcp_tool_exposure.rs` | `修改后复用`：MCP 工具默认 `Hidden`，须由委托剧本显式列白名单才 `Direct` |

商奕把 MCP 定位为**受控的第三方扩展面**，不是主路径：客户若已有内部数据服务，可挂 MCP server 而不必改商奕源码。注意 `ToolRegistry::register_external` 拒绝保留名（Codex 中为 `exec_command`/`shell_command`）；商奕的保留名集合为全部 14 个领域工具的 wire 名加 `spawn_agent`/`send_input`/`wait_agent`，MCP server 不得占用。

---

## 10. 模型接入

### 10.1 provider 抽象（复用面）

Codex 的 provider 分层可整体复用，唯一必须新增的是 **Chat Completions wire 适配层**（D-A5）。

| 组件 | 位置 | 内容 | disposition |
|---|---|---|---|
| provider 描述与注册表 | `model-provider-info`（`codex-model-provider-info`） | `ModelProviderInfo`、`WireApi`、内置 provider 表 | `修改后复用`（放宽 `WireApi`，见 10.2） |
| provider trait | `model-provider`（`codex-model-provider`） | `ModelProvider`、`create_model_provider`、`ProviderCapabilities`、`SharedModelProvider`、`BearerAuthProvider`、`auth_provider_from_auth_manager` | `修改后复用`：`ProviderCapabilities` 增 `chat_wire`/`reasoning_content`/`parallel_tool_calls` 三个布尔（`[推断]` 具体字段名） |
| API 客户端 | `codex-api` | `ApiProvider`、`RetryConfig`、`ResponsesClient`、`ResponsesWebsocketClient`、`ModelsClient`、`ApiError`、`ResponsesApiRequest`、`ResponseStream`、`ResponseEvent` | `修改后复用`：新增 `ChatCompletionsClient`，与 `ResponsesClient` 并列 |
| HTTP 与重试 | `codex-client` | `ReqwestTransport`、`sse_stream`、`run_with_retry`、`RetryPolicy`/`RetryOn`/`backoff` | `直接复用` |
| HTTP 底层 | `http-client` | **唯一**允许直接依赖 `reqwest` 的 crate（`deny.toml` 强制） | `直接复用`：这条治理规则原样保留 —— 新 wire 适配器**不得**绕开 `http-client` 直接引 `reqwest` |
| 本地 provider | `ollama`、`lmstudio` | 端口 11434 / 1234，`DEFAULT_OSS_MODEL = gpt-oss:20b` | `修改后复用`（可选）：`ollama` 的 `ollama-chat` provider id 已被上游拒绝（`OLLAMA_CHAT_PROVIDER_REMOVED_ERROR`）；商奕若要接 Ollama，走新的 Chat 适配器 |
| Responses 代理 | `responses-api-proxy` | 独立特权阻塞代理，CLI `codex responses-api-proxy` | `自研`替代：商奕不需要"沙箱进程访问 Responses API"这一用例（沙箱里只有 Python 侧车），删除 |

### 10.2 Chat Completions 适配器（D-A5）

**为什么必需。** Codex 只支持 Responses API：

```rust
pub enum WireApi { #[default] Responses }   // Display "responses"
```

反序列化时 `"chat"` 硬报错：

```
CHAT_WIRE_API_REMOVED_ERROR = "wire_api = \"chat\" is no longer supported.
How to fix: set wire_api = \"responses\" in your provider config."
```

国内 provider（GLM/智谱、DeepSeek）主推 Chat Completions，任意 OpenAI 兼容 `base_url` 虽可用但**端点必须说 Responses**。简报 01 判定这是"复用 Codex 的**头号改造项**"。

**落地两点**：

1. `codex-api` 新增 `ChatCompletionsClient`，实现与 `ResponsesClient` 相同的内部接口（`stream(request) -> ResponseStream`），使 agent 循环不需要知道 wire 差异。
2. `model-provider-info` 放宽 `WireApi` 为 `{ Responses, Chat }`，删除 `CHAT_WIRE_API_REMOVED_ERROR` 这条拒绝路径，并在 `ModelProviderInfo::validate()` 中按 wire 校验字段互斥性（如 `use_responses_lite` 仅对 Responses 有意义）。

**归一化 `StreamChunk` 映射。** 新增内部类型，避免把 Chat 的 delta 语义泄漏到循环层：

```rust
// codex-api/src/chat/mod.rs  [推断：以下类型名为商奕新增，非 Codex 既有]
#[derive(Debug, Clone)]
pub enum StreamChunk {
    TextDelta { text: String },
    ReasoningDelta { text: String },        // GLM thinking / DeepSeek reasoning_content
    ToolCallDelta { index: u32, id: Option<String>, name: Option<String>, args_fragment: String },
    Usage { prompt_tokens: u64, completion_tokens: u64, cached_tokens: Option<u64> },
    Finish { reason: FinishReason },        // stop | length | tool_calls | content_filter
    Error { code: Option<String>, message: String },
}
```

| Responses 侧事件 | Chat 侧来源（`chat.completion.chunk`） | 归一化 |
|---|---|---|
| `response.output_text.delta` | `choices[0].delta.content` | `TextDelta` |
| `response.reasoning_summary_text.delta` | `choices[0].delta.reasoning_content`（DeepSeek-R1 系）/ `delta.reasoning`（GLM 思考系） | `ReasoningDelta` |
| `response.output_item.added` + `response.function_call_arguments.delta` | `choices[0].delta.tool_calls[]` 的 `index`/`id`/`function.name`/`function.arguments` 分片 | `ToolCallDelta`（按 `index` 累积） |
| `response.completed.usage` | 仅当请求带 `stream_options: {"include_usage": true}` 才有尾部 usage chunk | `Usage` |
| `response.completed` / `response.incomplete` | `choices[0].finish_reason` | `Finish` |
| `error` 事件 | HTTP 非 2xx 或 SSE `error` 对象 | `Error` |

**工具调用翻译（双向）**：

- 出站：`ToolSpec::Function(ResponsesApiTool)` → `{"type":"function","function":{"name","description","parameters"}}`。命名空间工具（`Namespace(ResponsesApiNamespace)`）在 Chat 侧**不可表达**，故把 `ToolName{name, namespace}` 拍平为 `namespace__name`（双下划线，如 `shangyi__game_solve`），入站时反解。`ProviderCapabilities::namespace_tools` 为 `false` 时必须走这条拍平路径。
- 入站：`tool_calls` 数组 → 逐项构造 `ToolCall`；`arguments` 是 **JSON 字符串**，需二次 `serde_json::from_str` 并失败时回 `FunctionCallError::MalformedArgs`（不得把畸形 JSON 直接喂给工具）。
- 历史回放：Responses 的 `function_call`/`function_call_output` item 需映射为 Chat 的 `assistant.tool_calls` + `role:"tool"` 消息对。这是适配器最容易出错处：**工具调用与其结果必须成对回放**，否则上游多数实现会 400。
- 并行工具调用：`parallel_tool_calls` 是否被端点支持需实测（见 10.7）；不支持时适配器把多调用降级为顺序调用序列。

**reasoning-content 处理**：`ReasoningDelta` 只用于 UI 展示与日志（对应 Codex 的 `EventMsg::AgentReasoning`），**不**回灌到下一轮请求体（多数 Chat 端点不接受回传 reasoning 字段；GLM/DeepSeek 的契约需实测）。若某端点要求回传（例如要求保留 `reasoning_content` 以维持上下文），在 `ProviderCapabilities` 加开关，不做猜测性默认。

> **✅ 本表述已由 `A10b` 实测证实（DeepSeek Responses 端点，2026-09-17；见 `research/harness/12-provider-live-probe.md` §5）**：**"不回灌 reasoning"这一取舍成立，无需新增"必须回灌"的约束。** 三向对照实测（同一 `call_id`，带 `tools`）：
>
> | 场景 | 回传内容 | HTTP | 结果 |
> |---|---|---|---|
> | 1 | `reasoning` + `function_call` + `function_call_output`（完整回灌） | **200** | 正常完成 |
> | 2 | 仅 `function_call` + `function_call_output`（**省略 reasoning**） | **200** | 正常完成，且给出正确回答 |
> | 3 | `reasoning` 但**剥掉 `encrypted_content`** | **200** | 正常完成 |
>
> **三种情形全部 200，无一报 400。**
>
> **⚠️ 不要"修回去"**：早先一份工作底稿（`A10`）曾断言"带 `tools` 时 `reasoning_content` 必须完整回传，否则 API 返回 400"，并把它列为会导致硬失败的约束。**该断言已被实测推翻**，本节的"不回灌"表述是**正确**的一方。若未来读者看到本节与某处旧文冲突，**以实测为准，不要按旧断言修改本节**（附录 A 的 A11 行同步标注为已结算）。
>
> **两点保留（不要过度外推）**：① 该结论**只在 DeepSeek 上实测**；GLM/智谱侧**未测**（无 API key，工作项 `A10d` 处于 `blocked-on-human`）—— 首发若同时支持 GLM，需同口径复测一次（见 §10.7 U6）。② 该结论可能对配置敏感（`store: true` + `previous_response_id` 续接等场景未测），W1 落地时应在**实际使用的模型与参数组合**下复测；若届时出现 400，再按需回灌。

### 10.3 `[model_providers.*]` 配置

```toml
# $SHANGYI_HOME/config.toml
model = "glm-4.6"
model_provider = "zhipu"

[model_providers.zhipu]
name = "Zhipu GLM"
base_url = "https://open.bigmodel.cn/api/paas/v4"
env_key = "ZHIPU_API_KEY"
env_key_instructions = "在 https://open.bigmodel.cn 控制台创建 API Key，并 export ZHIPU_API_KEY=..."
wire_api = "chat"                       # D-A5：新增的合法值。⚠️ GLM 侧**未实测**（A10d 无 key，blocked-on-human）：智谱官方文档要求 `wire_api = "responses"`（附录 A.1），本值是否仍需要由 D-A5' 与 GLM 实测共同决定
request_max_retries = 4
stream_max_retries = 5
stream_idle_timeout_ms = 120000
http_headers = { "X-Client" = "shangyi" }
env_http_headers = { "X-Request-Id-Source" = "SHANGYI_TRACE_ID_ENV" }
[model_providers.zhipu.query_params]

[model_providers.deepseek]
name = "DeepSeek"
base_url = "https://api.deepseek.com"   # 端点路径见下方"路径与模型名"注；`/responses` 最短且可用
env_key = "DEEPSEEK_API_KEY"
wire_api = "responses"                  # D-A5'：DeepSeek 原生提供 Responses（A10b 实测），走 Responses 而非 Chat
request_max_retries = 4
stream_max_retries = 5
stream_idle_timeout_ms = 180000        # DeepSeek 长思考时首 token 可能较慢
model = "deepseek-flash"               # 必须用 /models 实际返回的 ID，不要用 `deepseek-chat` 别名
[model_providers.deepseek.query_params]

# 需要 Responses 语义且端点原生支持时的配置（保留路径）
[model_providers.selfhosted-responses]
name = "Self-hosted Responses gateway"
base_url = "http://127.0.0.1:8080/v1"
env_key = "SHANGYI_GATEWAY_KEY"
wire_api = "responses"
```

**路径与模型名（`A10b` 实测，`research/harness/12-provider-live-probe.md` §1、§0 结论 4）**：

| `POST` 路径 | HTTP | 说明 |
|---|---|---|
| `/responses` | **200** | ✅ **落地时固定用这条**（最短、可用） |
| `/v1/responses` | **200** | 可用；别名或网关重写 |
| `/beta/responses` | **200** | 可用；同上 |
| `/api/v1/responses` | **404** | 不存在 |
| `/openai/v1/responses` | **404** | 不存在 |

> 三条可用路径**可能是同一处理器注册了多个别名，也可能是网关重写**（机制未证实，`[推断]`）。设计取向：**在配置里显式写死 `base_url` 指向 `/responses`**，不要依赖别名行为。
>
> **模型名必须用 `/models` 的实际返回**：`GET /models` 只列出 **`deepseek-flash`** 与 **`deepseek-v4-pro`** 两个可用模型。**`deepseek-chat` 是别名** —— 请求它时响应的 `model` 字段返回的是 `deepseek-flash`（即别名被解析到实际模型）；`deepseek-reasoner` 不在 `/models` 列表中。**`models.json` 与 provider 配置应以 `deepseek-flash` / `deepseek-v4-pro` 为准**，不要写别名（否则目录前置的窗口/阈值元数据会对不上实际服务的模型）。

字段约束沿用 Codex 事实（简报 01 §5）：

| 键 | 约束 |
|---|---|
| `wire_api` | `"responses"` \| `"chat"`（商奕扩展） |
| `env_key` / `env_key_instructions` / `experimental_bearer_token` | 凭据来源，三选一 |
| `auth = { command, args, timeout_ms }` | 命令式 bearer（可用于企业内网网关） |
| `http_headers` / `env_http_headers` | 静态头 / 从环境变量取值 |
| `request_max_retries` / `stream_max_retries` | 上限 `MAX_REQUEST_MAX_RETRIES = 100`、`MAX_STREAM_MAX_RETRIES = 100` |
| `stream_idle_timeout_ms` | 默认 `DEFAULT_STREAM_IDLE_TIMEOUT_MS = 300_000` |
| `requires_openai_auth` | **裁剪**：`login`/`chatgpt` 体系移除后无意义 |
| `supports_websockets` | **裁剪**：商奕只用 SSE |
| `supports_standalone_web_search` | 保留但恒 `false`（商奕自带 `web_search`） |
| `aws = { ... }` | **裁剪**（Bedrock 不在 MVP 路径） |

`ModelProviderInfo` 保持 `#[schemars(deny_unknown_fields)]` 与 `validate()` 互斥校验纪律 —— 这条纪律对咨询 agent 尤其重要：拼错的 provider 键必须启动失败，而不是静默回退到默认端点。

### 10.4 重试与流空闲策略

沿用 `codex-client` 的 `run_with_retry` + `RetryPolicy`/`RetryOn`/`backoff`（`直接复用`）。商奕的默认值：

| 参数 | 默认 | 说明 |
|---|---|---|
| `DEFAULT_REQUEST_MAX_RETRIES` | 4（Codex 默认） | 非流式请求 |
| `DEFAULT_STREAM_MAX_RETRIES` | 5（Codex 默认） | 流中断后重连 |
| `DEFAULT_STREAM_IDLE_TIMEOUT_MS` | 300_000（Codex 默认） | 国内端点建议按 provider 下调（10.3 示例给 120–180 s） |
| 退避 | `codex-client` 既有 `backoff` | 指数 + 抖动 |

**必须新增的语义**：重试只允许发生在**首个 tool call 提交之前**。若一次 turn 中已有工具产生了副作用（`case_store_write`、`participant_profile_update`、`report_render`、`python_sidecar_exec`），流中断**不得**透明重试整个 turn —— 否则证据台账会重复记录，审计链断裂。实现方式：在 `TurnCx` 上记录 `side_effect_committed: bool`，`RetryOn` 对该 turn 返回 `false`，改为向模型注入"上一轮已提交副作用 X、Y"的重试提示并开启新 step。这是对 Codex 行为的一处**语义收紧**，与铁律 2（假设可追溯）一致。

### 10.5 token 计量与模型目录

**计量**（`仅借鉴设计`，上游 dsh `dsh-token-meter`，MIT © 2026 DeepSeek）：在 `shangyi-core` 内建 `TokenMeter`，从两个来源取数：

1. **服务端回报**：Chat 端点带 `stream_options.include_usage` 时读 `usage.prompt_tokens`/`completion_tokens`（权威值）；Responses 侧读 `response.completed.usage`。
2. **本地估算**：无 usage 时用估算器（`tiktoken-rs` [推断] 或字符级启发式），仅用于**是否触发压缩**的判定，不写入报告。Codex 侧的对应公开面是 `TokenCountEvent { info: Option<TokenUsageInfo>, rate_limits }` 与 `CodexThread::token_usage_info()`，`直接复用`。

关键纪律：**计量值可以估，报告里的数字不行**。报告中出现的概率、收益、阈值一律来自求解/传播工具的输出，token 计量不进报告（只在技术附录的成本小节，且标注为估算）。

**模型目录**。Codex 的 base system prompt 与大量行为**由模型目录驱动**：`render_model_instructions(model_info)` 从 `ResolvedModelMessages` 取 instructions 模板，无静态 `prompt.md`；无模板时 `tracing::warn!` 并返回空指令。`models-manager/models.json` 每项含 `context_window`、`max_context_window`、`auto_compact_token_limit`、`truncation_policy{mode, limit}`、`tool_mode`、`input_modalities`、`apply_patch_tool_type`、`web_search_tool_type`、`multi_agent_version`、`use_responses_lite`、`supported_reasoning_levels`、`include_skills/apps/plugin_usage_instructions`。

策略：**不打补丁改 `models.json`，而是用 `model_catalog_json` 指向商奕自有目录文件**（Codex 既有配置键），自持目录随后者覆盖；同时在自持目录里写入商奕领域模型的条目：

```jsonc
{
  "models": [
    {
      "slug": "glm-4.6",
      "context_window": 200000,          // 待核实
      "max_context_window": 200000,
      "auto_compact_token_limit": 150000,
      "truncation_policy": { "mode": "tokens", "limit": 200000 },
      "tool_mode": "direct",
      "web_search_tool_type": "none",
      "apply_patch_tool_type": "none",
      "input_modalities": ["text"],
      "multi_agent_version": "v1",
      "use_responses_lite": false,
      "supported_reasoning_levels": ["low", "medium", "high"],
      "include_skills": true,
      "include_apps": false,
      "include_plugin_usage_instructions": false,
      "instructions_template": "…商奕 base instructions（含两条铁律与领域口径）…"
    },
    { "slug": "deepseek-flash",  "context_window": 128000, "max_context_window": 128000,
      "auto_compact_token_limit": 100000, "…": "…" },
    { "slug": "deepseek-v4-pro", "context_window": 128000, "max_context_window": 128000,
      "auto_compact_token_limit": 100000, "supported_reasoning_levels": ["medium"], "…": "…" }
  ]
}
```

三个字段的语义按 Codex 事实：`context_window` 是当前生效窗口，`max_context_window` 是模型上限，`auto_compact_token_limit` 是触发自动压缩的阈值（第 14 章）。`instructions_template` 是**替换**而非新增（简报 01 §9：base 指令来自模型元数据，没有静态 `prompt.md`）；商奕的 base instructions 需承载"两条铁律"与领域报告口径，并把 methodology 段落留给 `SKILL.md`（第 13 章）。

> **DeepSeek 条目：`slug` 必须用 `/models` 的实际返回**（`A10b` 实测）：`deepseek-flash`、`deepseek-v4-pro`。**不要写 `deepseek-chat`** —— 它是别名，请求它时响应 `model` 字段返回的是 `deepseek-flash`；`deepseek-reasoner` 不在 `/models` 列表中。上例中两个条目自 2026-09-17 起已按实测值填写（旧文写的 `deepseek-chat`/`deepseek-reasoner` 已废弃）。`context_window` 等数值仍待 V10 实测回填。

### 10.6 凭据

**替换**整条 OpenAI 账号耦合链：`login`、`chatgpt`、`keyring-store`、`backend-client`、`analytics`、`cloud-tasks`、`aws-auth`、`agent-identity`、`workload-identity`（D-A4）。理由：这些 crate 服务 OpenAI 账号体系与云端任务，与商奕的"国内合规 + 自带 key"前提冲突。

替换后的凭据层（`shangyi-credentials`，`自研`）：

| 来源 | 优先级 | 用途 |
|---|---|---|
| 环境变量（`env_key`） | 1 | CI、容器、临时会话 |
| `$SHANGYI_HOME/credentials.toml`（权限 0600） | 2 | 本地持久 |
| `auth = { command, args, timeout_ms }` 命令式 bearer | 3 | 企业内网网关动态令牌 |
| 交互式一次性输入（`shangyi login`） | 4 | 首次配置，写入 2 |

约束：**密钥永不进入会话日志**。所有 `EventMsg` 与 `RolloutItem` 在序列化前经过 `redact()`，对已知凭据字面量做替换；这是 UI 之外的一层保险，且必须有单测（"任何 provider key 的字节序列不出现在 rollout 文件中"）。

### 10.7 待验证清单（中文端点）

以下均为**未取得事实**，W1–W2 必须实测并回填，不得按推断写进代码默认值。**逐项实测后须把状态从"待验证"改为"已结算/已关闭"并注明证据来源**（V2 已按此办理，见下表与下方 §10.7.1）：

| # | 待验证项 | 影响面 | 验证方法 | 状态（2026-09-17） |
|---|---|---|---|---|
| V1 | GLM/智谱是否提供 Responses 兼容端点（若有，可否省去适配器） | 决定 Chat 适配器是否为唯一路径 | 打端点探测请求 | ⏳ 待验证（GLM 侧**未实测**，`A10d` 无 key） |
| V2 | Chat 端点 `tools` 的 `function.parameters` JSON Schema 子集支持度（是否支持 `anyOf`/`oneOf`/嵌套 `$ref`） | 14 个领域工具的输入 schema 复杂度上限 | 用含 `anyOf` 的 schema 实测 | ✅ **已关闭**（`A10b` 实测；证据见 §10.7.1） |
| V3 | `tool_calls` 增量分片的 `index` 语义与 `id` 出现时机 | 适配器累积逻辑 | 抓一次多工具调用流 | ⏳ 待验证 |
| V4 | `parallel_tool_calls` 支持 | 工具并行策略（9.4） | 请求中显式置 true | ⏳ 待验证（`A10b` 未测并行行为：U2） |
| V5 | reasoning 字段名（`reasoning_content` vs `reasoning` vs `thinking`） | `ReasoningDelta` 映射 | 逐 provider 抓流 | ⏳ 待验证（DeepSeek **Responses** 侧的流式事件为 `response.reasoning_text.delta`，已验证；Chat 侧字段名仍待测） |
| V6 | reasoning 内容是否需回传下一轮请求 | 适配器历史构造 | 两轮对话实测 | ✅ **已结算（DeepSeek）**：**不需要**回传，三向对照全 200（§10.2 注；GLM 侧未测） |
| V7 | `stream_options.include_usage` 支持与 usage chunk 位置 | token 计量权威来源 | 抓流末帧 | ⏳ 待验证（Chat 侧）；**Responses 侧的 usage 已在 `response.completed` 中实测到**（含 `reasoning_tokens`） |
| V8 | `finish_reason` 取值集合（是否出现 provider 私有值） | `FinishReason` 映射完备性 | 累积观测 | ⏳ 待验证 |
| V9 | 速率限制响应头与 429 语义（是否给 `Retry-After`） | `RetryOn` 策略 | 压测 | ⏳ 待验证（`A10b` 只测到 400 与 404：U7） |
| V10 | 上下文窗口与最大输出真实值（厂商页与实测不一致的常见） | `models.json` 三个窗口字段 | 长输入探测 | ⏳ 待验证 |
| V11 | 流空闲超时实际表现（长思考首 token 延迟 P99） | `stream_idle_timeout_ms` 默认值 | 观测 | ⏳ 待验证 |
| V12 | 中文 tokenizer 的 token/汉字比 | 本地估算器精度（仅影响压缩时机） | 对照服务端 usage | ⏳ 待验证 |
| V13 | 工具结果消息是否计入系统/用户侧的长度限制（部分端点对 `role:"tool"` 消息有额外截断） | 长工具结果（如 `consequence_predict`）能否原样回放 | 构造超长 tool result | ⏳ 待验证 |

> 若 V1 成立且 V2 支持度不足，则**第三条路径**（自建 Responses↔Chat 转换代理）劣于适配器：代理会掩盖 wire 差异，使失败模式更难定位，且引入第二个进程边界。
>
> **V2 关闭后的净效果**：商奕的 14 个领域工具（`game_solve`、`equilibrium_select` 等带联合类型与嵌套结构的入参）**无需拍平** —— 这曾是本清单里**最大的未决风险**。

#### 10.7.1 V2 关闭证据（`A10b` 实测）

来源：`research/harness/12-provider-live-probe.md` §2.1（2026-09-17，直接 HTTP 探测，非文档判读）。

| 特性 | 实测结果 |
|---|---|
| `anyOf` 联合类型（string enum 或 integer） | ✅ **支持**（返回 `{"mode":"fast"}`） |
| 嵌套 `object`（含 `properties`） | ✅ **支持** |
| `required` 字段 | ✅ 正常解析 |
| `enum` 严格性 | ✅ 正确取 `enum` 内的值（`thorough`） |
| `additionalProperties: false` | ✅ 与 `anyOf` 组合可用 |

> **一处必须记住的"假故障"**：首次探测曾报 `Invalid schema … null is not of type "string"`。**那是测试脚本自己写了无效的 `"$ref": null`** —— **不是端点限制**。去掉后 anyOf + 嵌套 object 正常工作。**别把它记成"DeepSeek 不支持 `$ref`"**。
>
> **适用范围**：以上是 **DeepSeek** 的实测。GLM 侧（U6）**未测**。

#### 10.7.2 新增硬约束：`tool_choice`（`A10b` 实测）

> **约束**：DeepSeek 的 Responses 端点**只支持 `tool_choice` 的两个取值 —— `"auto"` 与 `"none"`**。`"required"` 与 `{"type":"function","name":...}`（锁定到指定工具）**恒返回 HTTP 400**，错误信息逐字为：
>
> ```
> Thinking mode does not support this tool_choice
> ```
>
> **两个可用模型（`deepseek-flash` 与 `deepseek-v4-pro`）都拒绝**，不是单模型问题。

| `tool_choice` 取值 | `deepseek-flash` | `deepseek-v4-pro` | 说明 |
|---|---|---|---|
| `"auto"` | **200** ✅ | — | 模型自行决定 |
| `"none"` | **200** ✅ | — | 不调用工具 |
| `"required"` | **400** ❌ | **400** ❌ | 报上述错误信息 |
| `{"type":"function","name":"ping"}` | **400** ❌ | — | 同上 |

**影响面**：
- 若商奕的领域工具**全部通过模型自主决定调用**（`auto`），**不受影响**。
- 若某条路径需要**强制**模型调用某个工具（`required`）或**锁定**到指定工具，则该路径在 DeepSeek 上**不可用** —— 必须改为 `auto` + 提示词约束，或在能力层降级。

**待确认项（基座是否依赖 `required`）**：`[推断]` —— 在 Codex 基座的主请求构造路径（`codex-rs/tools/src/tool_spec.rs`、`codex-rs/protocol/src/protocol.rs`、`codex-rs/codex-api/src/lib.rs`）中**未检索到 `tool_choice` 字样**，初步判断基座默认不发送该字段（即走端点默认 `auto`）。**但全局代码搜索 API 需认证、无法全仓确认，故此判断标为 `[推断]`，不是已验证事实。**
>
> **W1 落地时的必做动作**：确认基座是否在任何路径设置 `tool_choice = required`。**若有，该路径需要 patch 为 `auto`，并加一个 provider 能力开关**（`ProviderCapabilities` 增一个布尔，如 `tool_choice_required: bool`），而不是假定该字段普遍可用。

**未覆盖面（不要过度外推）**：
- **GLM/智谱未测**（无 GLM key，`A10d` = `blocked-on-human`）→ **D-A5' 的"选项 A 技术前提成立"结论目前只对 DeepSeek 成立**；GLM 侧仍需一次同口径实测（含它是否支持 `tool_choice = required`）。
- `tool_choice = required` 的**不可用性是 DeepSeek 特有的约束**，不是 Responses 协议的普遍性质。将来接入 OpenAI 等支持该字段的端点时，应在**能力层做 provider 差异化**，而非假定普遍可用或普遍不可用。

---

## 11. 沙箱、权限与检查点

本章是**全文档风险最高**的一章。理由是商奕的原始设计（v0.1 §1.5 铁律 1"所有数值计算在 Python sandbox 中执行"）把任意代码执行放在了关键路径上。harness 设计对该铁律做了**方向性修订**：数值由原生 Rust 产生，Python 侧车退到绘图与 CI 验证（D-A6/D-A7）。这**强化**了铁律而不是削弱它 —— "LLM 不算数"的最强形式是连执行环境都不需要引入 LLM 生成代码。但 `python_sidecar_exec` 仍然存在（LLM 写临时数值代码），故沙箱 seam 仍必须存在且必须 fail-closed。

### 11.1 沙箱 seam

dsh 的 seam 定义是本章的规格来源（`仅借鉴设计`，上游 dsh，MIT © 2026 DeepSeek）：

> `confine` must return enforcing argv or fail closed at wrap or runner-execution time; **silent unconfined passthrough is forbidden**.

`ConfinedArgv` 携带 `{ argv, enforcement: 'full'|'partial', denialSignatures, runnerFailureRules }`。三条必须原样移植的纪律：

1. **无默认实现**。`confine` 是抽象方法；某实现"原样返回调用者 argv"即是 bug，用类型系统使其不可能。
2. **denial 签名是所选后端自己的方言**，不做跨后端并集 —— "the union claims denials a given backend never produces"。具体：bwrap 只读绑定下是 `EROFS`，Landlock 下是 `EACCES`，Seatbelt 下是 `EPERM`。
3. **`RunnerFailureRule { allowedExitCodes?, fatalSignatures, informationalLines? }`** 区分*运行器从未运行*与*隔离生效并拦下了命令*：`Exit status alone never proves runner failure.` 无法隔离时抛 `SandboxUnavailableError`（码 `SANDBOX_UNAVAILABLE`），经结构化 `tool/result` 错误通道上报 —— 使模型不会把"sandbox 不可用"误读为"求解器失败"。

```rust
// shangyi-sandbox/src/lib.rs
#[derive(Debug, Clone)]
pub struct ConfinedArgv {
    pub argv: Vec<String>,
    pub enforcement: Enforcement,           // Full | Partial
    pub denial_signatures: &'static [DenySignature],   // 后端自己的方言
    pub runner_failure_rules: &'static [RunnerFailureRule],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement { Full, Partial }
#[derive(Debug, thiserror::Error)]
#[error("sandbox unavailable: {0}")]
pub struct SandboxUnavailable(pub SandboxUnavailableCode);
#[derive(Debug, Clone, Copy)]
pub enum SandboxUnavailableCode { NoBackend, BackendMissing, ConfineFailed, SanboxUnavailable }

pub trait SandboxProvider: Send + Sync {
    /// 无默认实现。原样返回调用者 argv 即是 bug。
    fn confine(&self, argv: &[String], policy: &SandboxPolicy)
        -> Result<ConfinedArgv, SandboxUnavailable>;
}

/// 只有被 enforce 的 argv 能进入执行器 —— 使"忘记沙箱"类型不可表达。
pub enum Confined {
    Enforced(ConfinedArgv),
    Denied(SandboxUnavailable),
}
impl Confined {
    pub fn argv(self) -> Result<ConfinedArgv, SandboxUnavailable> {
        match self { Confined::Enforced(a) => Ok(a), Confined::Denied(e) => Err(e) }
    }
}
```

**关键设计**：`python_sidecar_exec` 与 Python 侧车的构造函数**只**接受 `Confined`,永不接受裸 `Vec<String>`（对应 9.5 的 `SidecarClient::spawn(confined: ConfinedArgv, ..)`）。求解核心工具的构造函数不需要沙箱参数 —— 它们不执行外部代码，这正是 D-A6 的收益。

### 11.2 平台实现

| 平台 | 实现 | 位置 | disposition |
|---|---|---|---|
| macOS | **Seatbelt**：内嵌 4 个 `.sbpl` profile，经 `/usr/bin/sandbox-exec` 包装 argv | `sandboxing/src/seatbelt.rs`（`pub mod seatbelt`） | `直接复用`（Codex，Apache-2.0） |
| macOS profile 常量 | `MACOS_SEATBELT_BASE_POLICY` ← `seatbelt_base_policy.sbpl`；`MACOS_SEATBELT_NETWORK_POLICY` ← `seatbelt_network_policy.sbpl`；`MACOS_SEATBELT_PREFERENCES_POLICY` ← `seatbelt_preferences_policy.sbpl`；`MACOS_RESTRICTED_READ_ONLY_PLATFORM_DEFAULTS` ← `seatbelt_read_only_platform_defaults.sbpl`（均 `include_str!` 内嵌） | `sandboxing/src/` | `修改后复用`：profile 以 `(version 1)` / `(deny default)` 开头，允许 `process-exec`、`process-fork`、`signal (target same-sandbox)`、`process-info*`、最小 `sysctl-read`、`/dev/null` 写入（仿 Chrome 沙箱策略）。商奕需新增一条：**Python 侧车的可写临时目录**（原 profile 的 read-only 变体不容许） |
| Linux | **Landlock + seccomp**（进程内限制：`no_new_privs` + seccomp）叠加 **bubblewrap** 文件系统隔离 | `sandboxing/src/landlock`（`pub mod landlock`）、`linux-sandbox`（`bwrap`、`bundled_bwrap`、`landlock`、`launcher`、`linux_run_main`、`daemon_mounts`、`fd_mount`、`proxy_lifecycle`、`wslg`） | `直接复用`；`BUNDLED_BWRAP_DIGEST_VERIFICATION_FAILURE_EXIT_CODE = 8`；经 arg0 分发（`arg0 == "codex-linux-sandbox"`） |
| 平台选择 | `get_platform_sandbox`、`SandboxManager`、`SandboxCommand`、`SandboxExecRequest`、`SandboxTransformRequest`、`SandboxTransformError`、`SandboxType`、`SandboxablePreference`、`is_likely_sandbox_denied`、`find_system_bwrap_in_path`、`system_bwrap_warning` | `sandboxing` | `直接复用` |
| Windows | `mxc-sandbox`（`codex_mxc_sandbox::{CODEX_WINDOWS_MXC_ARG1, is_available, run_main}`）、`windows-sandbox-service` | 同名 crate | ⚠️ **`windows-sandbox-service` 是待定项（§6.2 D 段 24 项之一），不得当作已决**：它未被列入 A/B/C 任何一桶。`mxc-sandbox` 属 A 表（`直接复用`，计划 §1.5.1）；`windows-sandbox-service` 的处置需先在 W1 用计划 §1.6 的判定动作收敛。**MVP 口径不变**：只承诺 macOS + Linux，Windows 走 `mxc-sandbox` 但**不在 MVP 测试矩阵**内，若不可用则 `python_sidecar_exec` 直接 `Denied`（fail-closed，不是降级） |
| 网络代理 | `network-proxy`（rama/hickory；`NetworkProxy`、`PROXY_URL_ENV_KEYS`、`ManagedNetworkSandboxContext`） | `network-proxy` | `修改后复用`：商奕只用其"禁用网络"路径（见 11.3），不做域名白名单代理 |
| 进程硬化 | `process-hardening` | 同名 crate | `直接复用` |

**本机为 macOS（darwin 25.6.0 arm64，见环境信息），Seatbelt 路径是 MVP 的主验证路径。**

### 11.3 LLM 生成的 Python 必须跑在独立短生命周期进程、无网络命名空间

简报 02 给出的建议被采纳为硬规格：

> **对 LLM 写的数值代码建议的隔离层级：独立短生命周期进程**（若有 `youki`/`crun` 容器更佳），**无网络命名空间**、只读根、一个可写临时目录、workspace 只读绑定、经 stdin/stdout JSON 通信。这强于任何同世界沙箱，也是对"模型写的 Python"的诚实答案。

落地为 `python_sidecar_exec` 的执行画像：

| 维度 | 设定 | 理由 |
|---|---|---|
| 生命周期 | 单次调用一个进程，结束即销毁；不跨调用复用状态 | 防止状态泄漏与"上次留下的文件"成为隐式输入 |
| 网络 | **无**。Seatbelt 用 `seatbelt_network_policy.sbpl` 的 deny 形态；Linux 用无网络命名空间 + `NetworkAccess::Restricted` | LLM 临时数值代码不需要网络；`web_fetch` 是独立工具 |
| 文件系统 | 只读根 + 一个可写临时目录（`/tmp/shangyi-<uuid>/`）+ workspace 只读绑定 | 结果经 stdout JSON 返回，不经文件系统回传（artifact 走临时目录的显式拷贝） |
| 超时 | 默认 30 s，上限 120 s（`tool_call_timeout_policy`，对应 dsh `dsh-tool-call-timeout-policy`） | 防止死循环占满咨询预算 |
| 审批 | 恒 `PreDecision::Ask`；非交互环境（CI/无人值守）解析为 `Unavailable` ⇒ 拒绝 | 见 11.8 |
| 结果 | 必须带 `provenance`；无 `provenance` 的结果被 `tools/post-execute` 的 `Block` 拦下 | 铁律 1 |

**求解核心不需要沙箱**：`game_solve`/`equilibrium_select`/`consequence_predict`/`satisfice_search`/`monte_carlo` 是纯 Rust 计算，不执行外部代码、不读 workspace、不联网。这条"能力差距"是 D-A6/D-A7 的核心收益：**沙箱的失败模式不再可能污染求解正确性**，求解正确性只依赖单元测试与 CI 差分验证。

### 11.4 `execpolicy`（Starlark）

Codex 的 `execpolicy` 用 Starlark 表达命令允许/拒绝（`starlark` crate，`Dialect::Extended`，`dialect.enable_f_strings = true`，`execpolicy/src/parser.rs`）。公开面：`PolicyParser`、`Policy`、`Evaluation`、`MatchOptions`、`Decision`、`PrefixRule`、`PrefixPattern`、`PatternToken`、`NetworkRule`、`Rule`、`RuleMatch`、`RuleRef`、`ExecPolicyCheckCommand`、`RequirementsExecPolicy`、`prefix_rule_migration`、`blocking_append_allow_prefix_rule`、`blocking_append_network_rule`；CLI `codex execpolicy check`。

| 决策 | disposition |
|---|---|
| 保留 `execpolicy` crate 与 Starlark 解析器 | `直接复用`（Codex，Apache-2.0；`starlark` crate 许可需在 vendoring 时核对 `deny.toml`） |
| 是否暴露 shell | **否**（D-A8：编码工具全删）。故 `execpolicy` 在 MVP 中是**休眠的** |

保留但休眠的理由：① 删掉它意味着将来若确需受控 shell（例如客户希望商奕直接跑其内部脚本），要重新引入一条完整策略语言；② 它与 `sandboxing` 共享测试面。**但它不构成商奕的执行路径** —— 若 v0.2 决定开放 shell，必须先补齐 `execpolicy` 的领域规则与 fail-closed 测试，才允许 `SandboxMode::WorkspaceWrite` 下的 shell 工具注册（本项列入 v0.2 revision item）。

### 11.5 沙箱模式与审批策略（代码事实）

以下为代码中的既有定义，**不是设计意图**（简报 01 §7）：

```rust
#[serde(tag="type", rename_all="kebab-case")]
pub enum SandboxPolicy {
    #[serde(rename="danger-full-access")] DangerFullAccess,
    #[serde(rename="read-only")] ReadOnly { network_access: bool },
    #[serde(rename="external-sandbox")] ExternalSandbox { network_access: NetworkAccess },
    #[serde(rename="workspace-write")] WorkspaceWrite {
        writable_roots: Vec<AbsolutePathBuf>, network_access: bool,
        exclude_tmpdir_env_var: bool, exclude_slash_tmp: bool,
    },
}
pub enum NetworkAccess { Restricted /*默认*/, Enabled }

#[serde(rename_all="kebab-case")]
pub enum SandboxMode { ReadOnly /*默认*/, WorkspaceWrite, DangerFullAccess }

#[serde(rename_all="kebab-case")]
pub enum AskForApproval {
    #[serde(rename="untrusted")] UnlessTrusted,
    #[serde(alias="on-failure")] #[default] OnRequest,
    Granular(GranularApprovalConfig),
    Never,
}
pub struct GranularApprovalConfig {
    sandbox_approval: bool, rules: bool, skill_approval: bool,
    request_permissions: bool, mcp_elicitations: bool,
}
```

**必须修正的常见误解**（简报 01 明示）：`AskForApproval::OnFailure` **已移除**；`on-failure` 现为 `OnRequest` 的**别名**；细粒度控制改用 `Granular(GranularApprovalConfig)`。`SandboxPolicy` 新增了 `ExternalSandbox` 与 per-policy `network_access`；另有更新的"分离文件系统策略"模型（`FileSystemSandboxPolicy`、`PermissionProfile`、`[permissions]`），README 称其在语义可往返处取代 legacy `SandboxPolicy`，并有 `compatibility_sandbox_policy_for_permission_profile` 做桥接。

商奕的默认取值（在 `$SHANGYI_HOME/config.toml` 与剧本层）：

```toml
sandbox_mode = "read-only"          # 默认；商奕不写 workspace（成果写 case 目录，另开口）
approval_policy = "on-request"      # 对应 OnRequest（默认）；"on-failure" 是同一值的别名
[sandbox_workspace_write]
writable_roots = []                 # 由 case 目录在运行期注入
network_access = false
[permissions]                       # 新模型优先：分离文件系统/网络策略
shell_environment_policy = { inherit = "core", ignore_default_excludes = false }
allow_login_shell = false
experimental_use_unified_exec_tool = false   # shell 已移除
```

`SandboxMode` 三值**仅覆盖文件效果** —— 上游明文注明"Network and process visibility are outside this vocabulary"。这条限制对商奕是承重的：**不能**用"`sandbox_mode = read-only`"论证 Python 侧车无网络，必须显式走 `network_access = false` 与平台后端的网络 profile。

### 11.6 权限 preset：两个正交旋钮 + 持久 log-only 事件

`仅借鉴设计`（上游 dsh，MIT © 2026 DeepSeek；`dsh-permission-presets`、`dsh-sandbox-policy`、`dsh-user-approval`）。dsh 把授权拆成**三个分离的、持久、log-only、整值事件**，全部明确排除在模型转录之外：`permission/preset { preset }`、`sandbox/mode { mode, source? }`、`approval/policy { policy: 'ask' | 'never', source? }`。

商奕保留**两个正交旋钮**（`sandbox mode` × `approval policy`）与一个用户意图层（preset）：

| preset 名 | sandbox mode | approval policy | 用途 |
|---|---|---|---|
| `consult-default` | `ReadOnly` + `network_access = false` | `OnRequest` | 商奕默认；只读工作区 + 每次外部影响都问 |
| `consult-no-ask` | `ReadOnly` | `Never` | 批量回测/CI；`Never` 下所有 ask 一律 `Rejected`（fail-closed） |
| `power-user` | `WorkspaceWrite`（仅 case 目录） | `OnRequest` | 客户希望商奕直写其 case 目录时 |
| `custom` | — | — | **保留值**，`CUSTOM_PRESET = "custom"`，表示"不匹配任何条目"的派生状态，**永不是**切换目标 |

三条移植细节必须保留：

1. **切换先写 preset 事件，再通过各旋钮自己的 setter 写入"变化的"旋钮**。这样执行侧继续读各自旋钮的折叠结果，而两个 preset 共享同一旋钮值时用户意图仍被保留。
2. **`sandbox/mode` 是 THE write path**："the switch IS its event; nothing mutates mode state out of band." 生效值 = 最后一个事件 ?? 部署默认。
3. **审计配对**：`approval/request` 与其结果必须**同时**提交，否则请求拒绝 —— "returning an unlogged decision would violate the pair"。

```rust
#[derive(Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "kebab-case")]
pub enum SandboxMode { ReadOnly, WorkspaceWrite, DangerFullAccess }
#[derive(Clone, Copy, Serialize, Deserialize)] #[serde(rename_all = "kebab-case")]
pub enum ApprovalPolicy { Ask, Never }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalOutcome { AllowedOnce, Rejected, Cancelled, Unavailable }

// log-only 事件族（不携带 surfaceOp，不进 derive_messages）—— 见第 13/14 章同一纪律
pub enum LogOnlyEvent {
    PermissionPreset { preset: String },
    SandboxMode { mode: SandboxMode, source: Option<SwitchSource> },
    ApprovalPolicy { policy: ApprovalPolicy, source: Option<SwitchSource> },
}
#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum SwitchSource { Delegation, Cli, Playbook, User }
```

### 11.7 三个 checkpoint → 审批 seam + `RequestUserInput`

v0.1 §2.2 / §8.3 定义了三个 checkpoint（① 事实确认，`intel` 阶段；② 假设确认，`model` 阶段；③ 结果预览，`select` 阶段），均可回退修改，用户回答全部留档。D-A9 指定其映射方式。**规格**：

> **checkpoint = 一个持久的、受审批门控的暂停；其答案成为已记录事件；其拒绝回滚该阶段。**

落地拆成两半：

| 半 | 机制（Codex 既有） | 说明 |
|---|---|---|
| **门控与暂停** | 审批 seam：`Op::ExecApproval`、`Op::PatchApproval`、`Op::RequestPermissionsResponse`、`Op::ResolveElicitation`；事件 `EventMsg::ExecApprovalRequest`、`RequestPermissions`、`ApplyPatchApprovalRequest` | 商奕**不**复用 exec/patch 的具体审批类型，而是复用同一 seam 形态：新增 `shangyi-checkpoint` 的 `ApprovalKind::Checkpoint{id}`，走 `ApprovalService::request` 的同一路径与同一 fail-closed 规则 |
| **结构化问答** | `EventMsg::RequestUserInput` + `protocol` 的 `request_user_input` / `user_input` 模块；回答案经 `Op::UserInputAnswer` | 承载"待确认事实/假设清单 + 每条来源 + [确认/修改/追问] 三操作"（v0.1 §8.3）。答案事件进日志，成为审计链的一部分 |
| **回滚** | `rollout` 的 `truncate_rollout_after_turn_id` / `truncate_rollout_before_turn_id`（回滚支持）；app-server `thread/revert` | 拒绝 checkpoint 时回滚到该阶段起始的 turn，使"未确认的假设"不可能进入 GameSpec |

```rust
// shangyi-core/src/checkpoint.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointKind { FactConfirm /*① intel*/, AssumptionConfirm /*② model*/, ResultPreview /*③ select*/ }

pub struct CheckpointRequest {
    pub kind: CheckpointKind,
    pub phase: PhaseId,                 // 对应 v0.1 SessionState 的阶段
    pub items: Vec<CheckpointItem>,     // 待确认条目，每条带 source
    pub rollback_turn: Option<String>,  // 拒绝时回滚到该 turn
}
pub struct CheckpointItem {
    pub id: String,                     // "A1"/"A2"…，与 v0.1 §7.2 assumptions[].id 对齐
    pub content: String,
    pub source: AssumptionSource,       // UserConfirmed | PublicData | PriorBorrowed
    pub evidence_refs: Vec<Seq>,        // 支撑它的事件 seq（provenance，见第 15 章）
}

pub enum CheckpointOutcome { Confirmed, Modified { edits: Vec<ItemEdit> }, Rejected }

impl ApprovalService {
    /// 三个 checkpoint 共用同一入口；拒绝 ⇒ 追加回滚意图并结束该阶段 turn。
    pub async fn checkpoint(&self, cx: &TurnCx, req: CheckpointRequest)
        -> Result<CheckpointOutcome, ApprovalError> {
        // 1) 先落 log-only 记录（即使随后失败，尝试本身留在日志中）
        cx.log.append(LogOnlyEvent::CheckpointRequested { kind: req.kind, items: req.items.clone() })?;
        // 2) 走同一 approval seam（无应答者 ⇒ Unavailable ⇒ 视为拒绝）
        let decision = cx.approval.request(ApprovalRequest::checkpoint(&req)).await?;
        let outcome = match decision {
            ApprovalOutcome::AllowedOnce => self.await_user_answer(cx, &req).await?,
            ApprovalOutcome::Rejected | ApprovalOutcome::Cancelled | ApprovalOutcome::Unavailable
                => CheckpointOutcome::Rejected,
        };
        // 3) 答案与结果必须配对提交
        cx.log.append(LogOnlyEvent::CheckpointResolved { kind: req.kind, outcome: outcome.clone() })?;
        if matches!(outcome, CheckpointOutcome::Rejected) {
            if let Some(t) = &req.rollback_turn { cx.rollout.truncate_after_turn(t)?; }
        }
        Ok(outcome)
    }
}
```

**为什么"答案必须是已记录事件"而不是 prompt 里的一句话**：报告是交付物，审计链要求"哪些数字来自用户确认的假设、哪些来自公开数据、哪些是先验借用"可被机器重放。若 checkpoint 答案只存在于对话文本中，则 v0.1 §7.2 案例库的 `assumptions[].source` 字段无法被验证。这条要求与简报 02 的贯穿性不变量一致：**Model-visible ⟺ logged**。

### 11.8 `ApprovalService` 与 fail-closed 单元测试（交付物）

```rust
// shangyi-sandbox/src/approval.rs
#[async_trait::async_trait]
pub trait Approver: Send + Sync {
    async fn decide(&self, req: &ApprovalRequest, cancel: &CancellationToken) -> ApprovalOutcome;
}

pub struct ApprovalService { policy: ApprovalPolicy, approvers: Vec<Arc<dyn Approver>> }

impl ApprovalService {
    pub async fn request(&self, req: ApprovalRequest) -> Result<ApprovalOutcome, ApprovalError> {
        match self.policy {
            ApprovalPolicy::Never => Ok(ApprovalOutcome::Rejected),
            // 无组合的应答者解析为 unavailable —— FAIL CLOSED
            ApprovalPolicy::Ask if self.approvers.is_empty() => Ok(ApprovalOutcome::Unavailable),
            ApprovalPolicy::Ask => {
                let cancel = CancellationToken::new();
                let mut set = futures::stream::FuturesUnordered::new();
                for a in &self.approvers { set.push(a.decide(&req, &cancel)); }
                match set.next().await {
                    // 'allowed-once' 是唯一授权
                    Some(ApprovalOutcome::AllowedOnce) => Ok(ApprovalOutcome::AllowedOnce),
                    Some(other) => Ok(normalize(other)),   // 越界返回值 → Unavailable
                    None => Ok(ApprovalOutcome::Unavailable),
                }
            }
        }
    }
}

/// 越界/未知返回值归一化为 Unavailable（绝不归一化为 AllowedOnce）。
fn normalize(o: ApprovalOutcome) -> ApprovalOutcome {
    match o { ApprovalOutcome::AllowedOnce => ApprovalOutcome::Unavailable, x => x }
}
```

必须交付的 fail-closed 单测（这些**就是**本阶段的验收物，而不是文档）：

| 测试名 | 断言 |
|---|---|
| `no_approver_is_deny` | `ApprovalPolicy::Ask` + `approvers = []` ⇒ `Unavailable`，且**下游视为拒绝**（工具返回 `Denied`） |
| `never_policy_denies_everything` | `ApprovalPolicy::Never` ⇒ 任何请求 `Rejected`，包括 checkpoint |
| `throwing_approver_is_unavailable` | 应答者返回错误/panic 恢复 ⇒ `Unavailable`，绝不 `AllowedOnce` |
| `out_of_range_outcome_normalizes_to_unavailable` | 非 `AllowedOnce` 的成功值不升级为授权 |
| `approval_pair_must_be_logged` | 只提交 `approval/request` 而无结果时，请求被拒绝（"returning an unlogged decision would violate the pair"） |
| `idle_ask_rejected_before_write` | 无开放 turn 时的 ask 在追加任何事件**之前**拒绝 |
| `cancel_settles_cancelled` | `signal` 撤回 ⇒ 立即结算 `Cancelled`（不是 `Unavailable`，两者在下游都拒绝但审计含义不同） |
| `silent_unconfined_passthrough_is_impossible` | `SandboxProvider::confine` 的实现若不改变 argv，`Confined::argv()` 不得返回 `Enforced`（类型层 + 运行期断言） |
| `sandbox_unavailable_is_not_solver_failure` | `SandboxUnavailable` 经 `tool/result` 以 `SANDBOX_UNAVAILABLE` 码上报，且**不**产生 `EquilibriumSet` 空结果 |
| `sidecar_network_denied` | macOS/Linux 上侧车尝试出网 ⇒ 被拒，且 denial 签名与后端方言匹配（Seatbelt `EPERM` / Landlock `EACCES` / bwrap `EROFS`） |
| `python_sidecar_result_without_provenance_is_blocked` | 无 `provenance` 的侧车结果被 `post-execute` `Block` |
| `checkpoint_reject_rolls_back_phase` | 拒绝 ② ⇒ 回滚到该阶段起始 turn，且 GameSpec 中不出现未确认假设 |

### 11.9 语义边界：合规在报告/提示层，沙箱只管能力

**必须显式声明的边界**：沙箱与权限系统解决的是**能力**问题（这个进程能不能写这个目录、能不能出网、能不能执行），**不解决**"这份建议是否违反反垄断法"这类**语义**问题。v0.1 §6.1 的合规红线（"不输出与竞争对手协同定价的建议；博弈分析仅用于预判对手反应、优化自身单边策略"）属于**报告层与提示层**：

| 语义约束 | 落点 | 机制 |
|---|---|---|
| 反垄断措辞红线（不协同定价） | 报告模板 + `ToolGuard`（单调 deny，不可被剧本覆盖，见 9.2） | 模板固定免责声明（v0.1 §6.1）；`report_render` 前做措辞扫描，命中协同定价表述即 `Block` |
| 仅公开信息、robots.txt、限速 | `web_fetch`/`web_search` 工具实现 + 工具级 `ask` | 属工具行为，不属沙箱 |
| 不采集个人信息 | `data_ingest` 与 intel 抽取的 schema 校验 | 属工具行为 |
| 报告能力边界（"本报告不构成法律意见"） | 模板变量 | 属报告层 |

把这两者混为一谈的典型失败是：用 `SandboxMode::ReadOnly` 来"证明"合规 —— 沙箱不读报告文本，不判断措辞。文档口径必须是：**沙箱管能力，报告层管语义；两者都需要 fail-closed 测试，但测试的断言对象不同。**

---

## 12. 参与者子智能体与 Swarm

本章实现 v0.1 §5.7 的多 persona 引擎。策略：**编排骨架全部复用 Codex 的多智能体基础设施**（Codex 把子智能体做成一等公民，`spawn_agent` 工具存在于 `handlers/multi_agents_spec.rs`），**补上 dsh 的能力预检、持久描述符与相邻消息三条纪律**（D-A10）。

### 12.1 复用的 Codex 多智能体原语

| 原语 | 位置 | disposition | 商奕用法 |
|---|---|---|---|
| `ThreadManager::spawn_subagent` | `core/src/thread_manager.rs` | `直接复用`（Codex，Apache-2.0） | 创建一个 persona 子线程 |
| `ThreadManager::spawn_internal_session` | 同上 | `直接复用` | 非会话型内部子会话（orchestrator 内部调度用） |
| `ThreadManager::fork_internal_session` | 同上 | `直接复用` | 从父快照复制上下文再改 persona（同批次多样性的低成本路径） |
| `ThreadManager::list_agent_subtree_thread_ids` | 同上 | `直接复用` | 回收与终止 |
| `ForkSnapshot`、`InternalSessionParent` | `core` 公开导出 | `直接复用` | fork 语义与父子链接 |
| `AgentRoleConfig`、`ResolvedAgentRoleFile`、`parse_agent_role_file_contents`、`load_agent_roles` | `agent-roles` | `修改后复用`：`AgentRoleConfig` 作为 persona 的**角色定义载体**，新增 persona 专属字段（见 12.3） |
| `AgentGraphStore`、`LocalAgentGraphStore`、`ThreadSpawnEdgeStatus` | `agent-graph-store` | `直接复用`："存储中立的父子拓扑" —— 正是 Swarm 拓扑持久化所需 |
| `spawn_agent`、`send_input`、`wait_agent`、`resume_agent`、`close_agent` | `handlers/multi_agents_spec.rs`，命名空间 `multi_agent_v1` | `直接复用`。**v2 工具集（`send_message`/`followup_task`/`interrupt_agent`/`list_agents`）不采用**：v2 的 `MultiAgentVersion::V2` 在商奕无必要，且 `send_message` 的语义面比相邻消息更宽，与 12.2 的相邻纪律冲突 [推断] |
| 深度控制 | `core/src/agent/exceeds_thread_spawn_depth_limit`、`next_thread_spawn_depth` | `直接复用` | 递归预算 |
| 编排支持 | `core/src/agent/*`（`agent_resolver`、`child_config`、`control`、`registry`、`role`、`status`）、`session/multi_agents.rs` | `直接复用` | 子 agent 生命周期 |
| 通信 op | `Op::InterAgentCommunication` | `直接复用` | persona 间消息（受 12.2 相邻约束） |
| 事件 | `CollabAgentSpawnBegin/End`、`CollabAgentInteraction*`、`CollabWaiting*`、`CollabClose*`、`CollabResume*`、`SubAgentActivity` | `直接复用` | 进度展示与日志 |
| 源类型 | `SessionSource`、`SubAgentSource`、`InternalSessionSource`、`ThreadSource` | `直接复用` | 子会话来源标注 |
| 配置 | `[agents] { max_concurrent_threads_per_session, max_depth, default_subagent_model, default_subagent_reasoning_effort, job_max_runtime_seconds, interrupt_message }`；`[agents] roles.<name> { description, config_file, nickname_candidates }`；`multi_agent_v2` | `修改后复用`：保留前六键；`roles.<name>` 扩展为 persona 定义入口；`multi_agent_v2` 置 `false` |

```toml
# $SHANGYI_HOME/config.toml（Swarm 相关）
[agents]
max_concurrent_threads_per_session = 6     # MVP：players ≤4，留 2 个给内部会话
max_depth = 1                              # MVP：persona 不得再派生子 agent
default_subagent_model = "glm-4.6"
default_subagent_reasoning_effort = "low"   # persona 只需行为选择，不需长推理
job_max_runtime_seconds = 120
interrupt_message = "参与方仿真已中止，请回到主分析流程。"
multi_agent_v2 = false

[agents.roles.opponent_dumping]
description = "清库存型对手：短期现金流压力大，价格敏感，倾向激进跟随"
config_file = "$SHANGYI_HOME/agents/opponent_dumping.toml"

[agents.roles.opponent_strategic]
description = "长期战略型对手：份额导向，容忍短期亏损，倾向选择性反击"
config_file = "$SHANGYI_HOME/agents/opponent_strategic.toml"
```

### 12.2 dsh 补充：能力预检、持久描述符、相邻消息

三条纪律全部 `仅借鉴设计`（上游 dsh，MIT © 2026 DeepSeek；对应 `dsh-subagent`、`dsh-subagent-in-process-driver`、`dsh-persona`）：

**(a) 能力预检在委派之前，类型化错误，绝不静默降级。**

> "a request that needs a capability the chosen provider lacks is rejected with a typed error rather than accepted-then-ignored（**fail loud, no silent degradation**）"

商奕的直接收益：persona 仿真里"provider 不支持 persona 导致 persona 被静默忽略"会**直接使结论失效** —— 一个 3 玩家的博弈若只跑了 2 个 persona，选择引擎拿到的行动分布是错的，而报告不会显示任何异常。必须在 spawn 任何 task **之前**调用 `validate(caps, &req)`。

```rust
// shangyi-swarm/src/provider.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentCapabilities {
    pub agent_options: bool,
    pub output_schema: bool,
    pub depth_limit: bool,
    pub tool_filter: bool,
    pub persona: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("provider {provider} lacks capability: {missing:?}")]
    UnsupportedCapability { provider: String, missing: Vec<&'static str> },
    #[error("depth limit exceeded: {depth} > {limit}")]
    DepthLimitExceeded { depth: u32, limit: u32 },
    #[error("provider failed: {0}")]
    ProviderFailed(String),
    #[error("child aborted: {0:?}")]
    Stopped(SubagentStopReason),
}

#[async_trait::async_trait]
pub trait SubagentProvider: Send + Sync {
    fn name(&self) -> &str;
    fn capabilities(&self) -> SubagentCapabilities;
    fn inherits_parent_context(&self) -> bool { false }

    /// 预检：必须在 spawn 之前调用。不支持的标志 ⇒ 类型化错误，绝不静默回退。
    fn validate(&self, req: &SubagentRequest) -> Result<(), SubagentError> {
        let c = self.capabilities();
        let mut missing = Vec::new();
        if req.persona.is_some() && !c.persona { missing.push("persona"); }
        if req.output_schema.is_some() && !c.output_schema { missing.push("output_schema"); }
        if req.tool_filter.is_some() && !c.tool_filter { missing.push("tool_filter"); }
        if !missing.is_empty() {
            return Err(SubagentError::UnsupportedCapability { provider: self.name().into(), missing });
        }
        Ok(())
    }

    async fn start(&self, req: ResolvedStartRequest) -> Result<SubagentRun, SubagentError>;
    /// 方法存在本身即是"可续创建"能力（与 dsh 同一技巧）。
    async fn prepare_continuable(&self, _req: ContinuableCreateRequest)
        -> Result<Option<ContinuableCreateSpec>, SubagentError> { Ok(None) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubagentStopReason { Completed, Aborted, Error, MaxTokens, Refusal }
```

**(b) 持久描述符写在子会话内，且在压缩中存活。**

dsh 的 `subagent/descriptor` 是 log-only、无 `surfaceOp`、**可存活压缩**的事件，`SUBAGENT_DESCRIPTOR_VERSION = 3`，`mode: 'one-shot' | 'continuable'`；"The first `subagent/descriptor` event is authoritative."另有父方拥有的 `subagent/catalog` 事件（`SUBAGENT_CATALOG_VERSION = 0`）供不加载子会话的枚举。

商奕需要它，因为 Swarm 崩溃恢复必须恢复**同一批 persona**，而不是重组过的 persona —— 后者会让同一 case 的两次运行产生不同结论（v0.1 §5.4 的校准闭环会因此拿到脏样本）。

```rust
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum SubagentDescriptor {
    OneShot { version: u32, provider: String, label: Option<String> },
    Continuable {
        version: u32,
        provider: String,
        label: String,                       // "opponent_dumping@rollout-3"
        persona: Option<String>,             // 见 12.3
        tool_filter: Option<ToolFilter>,
        player_id: String,                   // 商奕扩展：绑定 GameSpec player
        rollout_index: u32,                  // 商奕扩展：第几次 rollout
        seed: u64,                           // 商奕扩展：RNG 种子（可复现）
    },
}
pub const SHANGYI_SUBAGENT_DESCRIPTOR_VERSION: u32 = 1;
```

**(c) 相邻消息 + 深度持久在 header。**

- `sendMessage(sender, targetId, ...)` **仅限相邻** Agent —— 直接父或直接可续子；服务"derives durable sender attribution from the exact live sender"并拒绝非相邻。Rust 中最简单的落地是**使非相邻消息不可表达**：API 只暴露 `parent: &AgentHandle` 与 `children: &ChildRegistry`。
- `SessionHeader.delegationDepth` 持久化 —— "Persisted so a recursion budget survives restart and resume — a runtime-only depth would reset a resumed child to top-level." 由 `assertSubagentMaxDepth`/`resolveChildDepth` 强制。商奕的 `max_depth = 1` 在一次崩溃恢复后必须仍是 1，否则 persona 可能意外派生出孙 agent，破坏"persona 只提议、引擎评估"的分工。

### 12.3 persona：显式优先级的 prompt 遮蔽 + `complete` 短路

`仅借鉴设计`（上游 dsh `dsh-persona`，MIT © 2026 DeepSeek）。dsh 的关键洞察：`dsh-persona` 刻意是**一个 row，而非注册表**；`dsh-system-prompt` 拥有全局 persona 并无条件注册其段落，因此 persona **必须**挂在 agent 作用域内才能为该会话遮蔽部署 persona —— "mounted globally it collides with the registry's own registration and fails loud."

设计理由原话（必须保留）：**"An agent preset cannot mount the prompt registry itself, so without a row of its own a preset could change an agent's tools but never its identity."**

商奕的映射：**persona = 一个显式优先级的 prompt 段落，遮蔽部署身份，并具备 `complete` 短路。**

| persona 配置键 | 语义 | 商奕取值示例 |
|---|---|---|
| `prefix` | 段落前缀，严格 `{{var}}` 插值（未知变量是**硬错误**，不是空串） | `"你是「{{player_name}}」，一名{{archetype}}的{{role}}。"` |
| `suffix` | 可选后缀 | `"你的目标函数是{{objective}}；风险偏好{{risk}}。"` |
| `complete` | `true` 时 prefix 成为**整个** system prompt（短路） | 对手 persona 用 `true` |
| `includeRuntimeContext` | `false` 抑制动态上下文快照 | 对手 persona 用 `false`（避免泄露本企业 cwd/工具面） |

**为什么 `complete: true` 是承重的**：dsh 的理由原话是"否则'对抗性监管者' persona 会悄悄继承'乐于助人的助手'框架，仿真即失效"。商奕的对应失败模式更具体：若对手 persona 继承了商奕的部署口径（"为客户提供单边建议"），它会从"我的处境下的最佳行动"滑向"如何帮助提问者"，行动分布系统性偏软，`persona_sim` 选择器给出的权重随之失真。

```rust
// shangyi-swarm/src/persona.rs
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersonaScope { Deployment, Playbook, Agent(AgentId), Player(String) }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersonaSection {
    pub scope: PersonaScope,
    pub prefix: String,
    pub suffix: Option<String>,
    pub complete: bool,
    pub include_runtime_context: bool,
}

pub struct PromptAssembler { builtin: Vec<BuiltinSection>, personas: Vec<PersonaSection> }

impl PromptAssembler {
    /// 最高优先级的非空 `complete` persona 整体胜出；
    /// 否则最具体的 prefix/suffix 遮蔽部署层。
    /// 严格插值：未知 {{var}} ⇒ Err（硬错误）。
    pub fn render(&self, vars: &VarTable) -> Result<String, PersonaError> {
        let mut candidates: Vec<&PersonaSection> =
            self.personas.iter().filter(|p| !p.prefix.trim().is_empty()).collect();
        candidates.sort_by_key(|p| p.scope.specificity());   // 越具体越高
        if let Some(p) = candidates.iter().find(|p| p.complete) {
            return interpolate_strict(&p.prefix, vars).map(|s|
                match &p.suffix { Some(sfx) => format!("{s}\n{}", interpolate_strict(sfx, vars).unwrap_or_default()), None => s });
        }
        // 非 complete：组装 builtin + 最高优先级的 prefix/suffix
        let mut out = String::new();
        for b in &self.builtin { out.push_str(&b.render(vars)?); out.push('\n'); }
        if let Some(p) = candidates.first() {
            out.push_str(&interpolate_strict(&p.prefix, vars)?);
            out.push('\n');
            if let Some(sfx) = &p.suffix { out.push_str(&interpolate_strict(sfx, vars)?); out.push('\n'); }
        }
        Ok(out)
    }
}
```

作用域遮蔽以**类型化、显式优先级排序**实现（dsh 的 scope 机制不移植，简报 02 第 E.1 项：不移植 Cordis 机制的 Proxy/isolate）；模板用 `minijinja` 或手写 `{{var}}` 替换（简报 02 第 D.9 项指出手写替换足够，且避免引入模板引擎的转义语义）。**保留的三个行为**：严格插值、空文本丢弃该段、`complete` 短路。

### 12.4 orchestrator（纯代码）

编排器不含 LLM：回合调度、终止条件、多样性采样（v0.1 §5.7）。`自研`。

```rust
// shangyi-swarm/src/orchestrator.rs
pub struct SwarmOrchestrator {
    provider: Arc<dyn SubagentProvider>,
    roles: Arc<RoleRegistry>,            // 来自 [agents.roles.*] + 剧本 persona 列表
    budget: RolloutBudget,               // 轮数上限 + LLM 调用上限（v0.1 §12 风险对策）
    rng: StdRng,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub rollout_index: u32,
    pub seed: u64,
    pub rounds: Vec<RoundRecord>,        // 每轮：谁的 move、理由、是否有 novel 提议
    pub stop: SubagentStopReason,
    pub descriptor_refs: Vec<String>,    // 各 persona 的 subagent/descriptor id
}

impl SwarmOrchestrator {
    /// 一次 rollout：N 个 persona 逐轮行动，直到终止条件；
    /// 每轮 move 追加进日志（可重放，verdict 由数据决定）。
    pub async fn rollout(&mut self, game: &GameSpec, personas: &[PersonaSpec],
                         ctx: &SwarmCtx<'_>) -> Result<Trajectory, SubagentError> {
        // 1) 能力预检：所有 persona 一次全部预检，任一失败 ⇒ 整体拒绝（fail loud）
        for p in personas {
            self.provider.validate(&p.to_request())?;
        }
        // 2) 按 GameSpec.players 顺序创建子会话（可续），写 subagent/descriptor
        let mut children = ChildRegistry::new();
        for (i, p) in personas.iter().enumerate() {
            let seed = self.rng.gen();
            let spec = persona_section(p, seed);
            let run = self.provider.start(ResolvedStartRequest {
                persona: Some(spec), tool_filter: Some(PLAYER_TOOL_FILTER.clone()),
                player_id: p.player_id.clone(), rollout_index: self.rollout_index, seed,
                max_depth: 1, ..Default::default()
            }).await?;
            ctx.log.append(SubagentDescriptorEvent(SubagentDescriptor::Continuable {
                version: SHANGYI_SUBAGENT_DESCRIPTOR_VERSION,
                provider: self.provider.name().into(),
                label: format!("{}@rollout-{}", p.player_id, self.rollout_index),
                persona: Some(p.persona_id.clone()),
                tool_filter: Some(PLAYER_TOOL_FILTER.clone()),
                player_id: p.player_id.clone(), rollout_index: self.rollout_index, seed,
            }))?;
            children.insert(p.player_id.clone(), run);
        }
        // 3) 回合循环：只与直接子通信（相邻）；终止由数据决定（inbox 空 + 轮数上限）
        let mut rounds = Vec::new();
        while rounds.len() < self.budget.max_rounds {
            let mut progressed = false;
            for player in game.player_order() {
                let child = children.get(&player)?;         // 相邻：只暴露直接子
                let view = GameView::for_player(game, &player, &rounds);
                let mv = child.act(&view, &rounds).await?;  // persona 的 Move（可含 novel 提议）
                if mv.is_actionable() { progressed = true; }
                ctx.log.append(MoveEvent { player: player.clone(), mv: mv.clone() })?;
                rounds.push(RoundRecord::from(&player, mv));
            }
            if !progressed { break; }      // 数据决定终止，不由最后运行的 handler 决定
        }
        Ok(Trajectory { rollout_index: self.rollout_index, seed: self.rng.gen(),
                        rounds, stop: SubagentStopReason::Completed,
                        descriptor_refs: children.descriptor_refs() })
    }

    /// 多样性采样：persona 参数扰动 × N 次 rollout → 行动分布。
    pub async fn distribution(&mut self, n_rollouts: u32, perturb: bool,
                              game: &GameSpec, base: &[PersonaSpec], ctx: &SwarmCtx<'_>)
        -> Result<ActionDistribution, SubagentError> {
        let mut acc = ActionDistribution::default();
        for i in 0..n_rollouts {
            self.rollout_index = i;
            let personas = if perturb { perturb_personas(base, &mut self.rng) } else { base.to_vec() };
            let traj = self.rollout(game, &personas, ctx).await?;
            acc.observe(&traj, &personas);
        }
        Ok(acc)
    }
}
```

**多样性采样的三个扰动维度**（`自研`，源自 v0.1 §5.7 "persona 参数扰动 × N 次 rollout"）：① 画像中的行为参数（`lambda_qre`、`level_k`、`aggressiveness`、`cost_estimate{mu,sigma}`，v0.1 §6.2）；② 初始信念（对对手类型的先验，`posterior_types`）；③ 温度/采样种子。每次 rollout 的 `seed` 与扰动后参数写入日志，使行动分布可被独立复现（v0.1 §7.2 案例库的 `selection_output.selector_weights_snapshot` 需要这个粒度）。MVP 的 `n_rollouts` 默认 5（v0.1 §12 的"rollout 轮数与 LLM 调用预算上限"）。

### 12.5 novel-move 生成器与三重过滤

v0.1 §5.7.1 的设计教训（星宇股份事件，2026-08 常州星宇批量解约 107 名应届生：弱势方学生向星宇的**客户**与**海外供应商**发函投诉、诉至港交所，引入新参与者借其 ESG/IPO 声誉激励改变支付结构，最终市值大幅蒸发）要求**开放行动空间**。机制化落法（v0.1 §5.6.1 步骤 2）：

1. persona 的 `Move` 允许 `type="novel"`：矩阵外行动（投诉对手的客户/供应商、舆论、集体行动、引入监管/交易所）。
2. **三重过滤**（合法性 × 弱势方资源可得性 × 激励相容）
3. 通过者**注册回 GameSpec**（含新增 `external_actors`）→ 重新求解与选择。
4. 分工原则：**LLM 想得到，解析引擎算得清**。

```rust
// shangyi-swarm/src/novel_moves.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NovelMove {
    pub proposer: String,                 // player_id（可为弱势方）
    pub action_text: String,              // 自然语言描述
    pub targets: Vec<String>,             // 被触及的既有玩家
    pub introduces: Vec<ExternalActorDraft>, // 拟引入的新参与者
    pub stated_rationale: String,
}

#[derive(Clone, Debug)]
pub enum FilterVerdict {
    Register(RegisteredMove),             // 扩入策略空间（含 external_actors）
    Reject(RejectReason),
}
#[derive(Clone, Debug)]
pub enum RejectReason {
    Illegality { finding: LegalFinding },          // 违反法律/平台规则
    ResourceUnavailable { gap: ResourceGap },      // 弱势方实际无法执行
    NotIncentiveCompatible { expected: f64, cost: f64 }, // 预期收益 ≤ 成本
}

/// 三重过滤：合法性 × 资源可得性 × 激励相容。
/// 实现为 async 因为合法性检查需要检索法规/平台规则（kb_retrieve），
/// 激励相容检查需要调用 monte_carlo（成本与收益均为分布，非点估计）。
pub async fn triple_filter(
    cand: &NovelMove,
    ctx: &FilterCtx<'_>,      // 含 GameSpec、画像、kb 句柄、monte_carlo 句柄
) -> Result<FilterVerdict, FilterError>;

pub struct FilterCtx<'a> {
    pub game: &'a GameSpec,
    pub profiles: &'a ProfileStore,
    pub kb: &'a dyn KbRetrieve,
    pub mc: &'a dyn MonteCarlo,
    pub guard: &'a dyn ToolGuard,     // 单调 deny：合规黑名单不可被剧本覆盖
}
```

**过滤顺序与短路**：合法性 → 资源可得性 → 激励相容（成本递增）。第三关用 `monte_carlo` 而非点估计：v0.1 §5.7.1 的原话是"激励相容（预期收益 > 成本）"，但支付参数带不确定性分布（§5.2 `params.dist`），故判据应为"`P(收益 > 成本) ≥ 阈值`"而非确定性比较 —— 这是对 v0.1 的一处**收紧**，理由与 §5.6.2 满意过滤的"达标概率 ≥ 阈值（默认 0.6）"一致。

**注册回 GameSpec** 的形状：

```rust
pub struct RegisteredMove {
    pub move_id: String,                        // "novel-<hash>"
    pub strategy_label: String,                 // 进 strategies[player_id]
    pub external_actors: Vec<ExternalActor>,    // 进 GameSpec.external_actors
    pub payoff_overrides: Vec<PayoffExpr>,      // 新 actor 引入后改变的支付表达式
    pub filter_evidence: Vec<Seq>,              // 三关的判定证据（事件 seq）
}
```

`external_actors` 的 schema 沿用 v0.1 §5.2：非直接玩家（客户、供应商、监管、交易所、媒体、公众），平时不行动，但可被玩家行动**引入**并改变支付结构。相应地理论库扩展（v0.1 §5.7.2）：非市场战略（Baron）、集体行动的逻辑（Olson）、不对称冲突与弱者的武器、声誉与舆论博弈。

### 12.6 feedback 环

主 agent 审查 rollout 轨迹与参与者画像、博弈论合理性的一致性，偏离则修正 persona 参数并重跑（v0.1 §5.7）：

```rust
// shangyi-swarm/src/feedback.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditFinding {
    pub player_id: String,
    pub dimension: AuditDimension,      // ProfileConsistency | GameTheoreticPlausibility | NovelMoveQuality
    pub observed: String,
    pub expected: String,
    pub profile_refs: Vec<Seq>,
    pub severity: Severity,             // Info | Warn | Invalidating
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersonaPatch {
    pub player_id: String,
    pub field: String,                  // "lambda_qre" | "level_k" | "aggressiveness" | "cost_estimate.mu" | ...
    pub old_value: serde_json::Value,
    pub new_value: serde_json::Value,
    pub justification: String,          // 引用 AuditFinding
}

pub struct FeedbackLoop { max_iterations: u32, tolerance: f64 }

impl FeedbackLoop {
    pub async fn converge(&self, orch: &mut SwarmOrchestrator, game: &GameSpec,
                          personas: &[PersonaSpec], ctx: &SwarmCtx<'_>)
        -> Result<ConvergedDistribution, SwarmError> {
        let mut cur = personas.to_vec();
        for it in 0..self.max_iterations {
            let dist = orch.distribution(5, true, game, &cur, ctx).await?;
            let findings = audit(&dist, game, ctx).await?;      // 主 agent：一次 LLM 调用 + 结构校验
            let patches = derive_patches(&findings);
            if patches.iter().all(|p| p.is_negligible(self.tolerance)) { return Ok(dist.into()); }
            // 每次修正必须记录，且修正后的 persona 参数快照进日志
            ctx.log.append(PersonaPatchedEvent { iteration: it, patches: patches.clone() })?;
            cur = apply_patches(&cur, &patches);
        }
        // 未收敛：按 v0.1 §12 风险对策回退解析选择器，并显式标注
        Err(SwarmError::NotConverged { fallback: Fallback::AnalyticSelectorsOnly })
    }
}
```

**审计的三个维度**（`自研`）：① **画像一致性** —— persona 的行动是否与其画像卡（`aggressiveness`、`switching_cost`、`holat_risk` 等）自洽；② **博弈论合理性** —— 行动是否在理性范围内（例如对手在无成本优势时无条件自杀式降价是可疑的）；③ **novel move 质量** —— 提议的矩阵外行动是否被三重过滤大量拒绝（拒绝率过高说明 persona 在幻觉行动；过低说明过滤太松）。

**铁律不变**（v0.1 §5.7 原文，必须逐字保留其效力）：**"sub-agent 可以提议行动（含矩阵外），但评估一律回主引擎"** —— novel move 注册入 GameSpec 后由解析求解与选择引擎重算后果。Swarm 的输出是**行为证据与候选来源，不是结论本身**。机器落法：persona 子会话的 `tool_filter` 只含 `web_search`/`web_fetch`/`kb_retrieve`/`case_store_read`（读信息）与 move 提交（`send_input`），**不含** `game_solve`/`equilibrium_select`/`consequence_predict`/`monte_carlo`。persona 无法计算后果，因此不可能"自评"。

### 12.7 关键 Rust 接口汇总

```rust
// shangyi-swarm/src/lib.rs
pub struct PersonaSubAgent {
    pub player_id: String,
    pub persona: PersonaSection,
    pub descriptor: SubagentDescriptor,
    handle: AgentHandle,
}

impl PersonaSubAgent {
    /// Move = { action: 策略集内动作 | {type:"novel", spec}, rationale }
    pub async fn act(&self, view: &GameView, history: &[Move]) -> Result<Move, SubagentError> {
        let prompt = render_player_prompt(view, history, &self.persona)?;
        let out = self.handle.turn(prompt, PLAYER_TOOL_FILTER.clone()).await?;
        Move::parse_strict(&out)             // 结构校验失败 ⇒ 类型化错误（不静默视为"不动"）
    }
}

pub struct SubagentRun {
    pub id: String,
    pub local_agent: Arc<PersonaSubAgent>,
    pub result: tokio::task::JoinHandle<Result<Trajectory, SubagentError>>,
    pub dispose: Box<dyn FnOnce() + Send>,   // 可逆注册：每次"加东西"都返回"怎么拿掉"
}
```

`可逆注册` 纪律来自简报 02 的代码评审清单原话：**每一次"向系统加东西"的调用都必须返回"以后怎么拿掉"。**

### 12.8 MVP 范围与 v0.2 项

| 项 | MVP（v0.1 / 6 周） | v0.2+ |
|---|---|---|
| rollout 轮次 | **单轮 rollout + feedback 环**（v0.1 §11.1 明文） | 多轮 sub-agent 间交互/谈判仿真（v0.1 §11.2 out-of-scope） |
| 适用剧本 | 仅 `pricing_response`（对手 persona = 替代者） | `negotiation`（供应商与大客户同为 persona）、`competitor_intel` |
| persona 数 | ≤4（`max_concurrent_threads_per_session = 6`） | 非市场参与者简化反应体（客户/供应商/监管/舆论） |
| novel move | 三重过滤 + 注册回 GameSpec 打通 | 跨案例的 novel move 库（复用已通过过滤的候选） |
| 递归 | `max_depth = 1`，persona 不派生子 agent | 保留 `max_depth = 1`（不建议放开） |
| learning 选择器 | **不实现**（v0.1 §11.1 记为"放 v0.12"，应为 v0.2 的笔误，见文末修订项） | 虚拟对局/EWA 选择器 + 与其配套的多轮仿真 |
| v2 工具集 | 不采用 `MultiAgentVersion::V2` | 若上游 v1 工具被弃用再评估 |

**能力预检的单测是本章的验收物之一**（对应简报 02 第 D.8 项的四个必须保留决策之 (a)）："每个不支持的标志都产生类型化错误、绝不静默回退"。

---

## 13. 剧本（Playbook）与配置分层

本章实现 v0.1 §3.2 的剧本机制，并把 dsh 的 **agent preset 即数据** 模式移植为商奕的 playbook（`仅借鉴设计`，上游 dsh `dsh-agent-presets`，MIT © 2026 DeepSeek）。核心目标有两个：① 每次客户委托的"行文风格 + 工具白名单 + 数值工具 + persona 集"是**数据变更**而非重编译；② 生效配置**可检视、可 diff**，因为这是审计要求。

### 13.1 dsh preset → 商奕 playbook 的映射

| dsh 概念 | dsh 事实 | 商奕映射 | 理由 |
|---|---|---|---|
| `preset.yml`（`name`、`description`、`order`） | 目录级元数据 | `playbook.yml` 的 `meta` 段（+ 商奕扩展段，见 13.2） | 元数据与行列表分层，便于只读元数据做剧本目录展示 |
| `agent.cordis.yml` | 一个 **loader row 的 YAML 列表**，每行 `{ id, name, config, disabled?, group?, isolate? }` | `agent.plugins.yml`，行 schema 保持逐字同形 | 文件布局完全镜像 ⇒ 已编写资产可移植、可 diff（简报 02 第 D.2 项） |
| preset 发现（shipped root + 配置 root + user root，同 id 先到先得） | `dsh-agent-presets` | playbook 发现同构；rank 顺序见 13.4 | |
| `SessionHeader.agentPreset` | 持久记录 | `SessionHeader.playbook_id` | 见 13.4 |
| log-only `agent-preset/selected` 事件 + `agentPreset` projection | 由事件推进、由投影折叠 | log-only `playbook_selected` 事件 + `playbook` projection | 见 13.4 |
| **不移植**：每进程只挂载一次于 standing scope + `bindScopeParent` 加入 | "一份组合，不是每会话一份" | Rust 中 `Arc<T>` 每 agent 实例化一份插件集合 | 简报 02 第 D.2 项明示"**不要**移植 scope / 单次挂载机制" |
| **不移植**：`isolate` realm（发布服务的行必须放 isolate group 内） | TS 动态 DI 的产物 | 无对应物 | 简报 02 第 E.1 项：移植语义（可逆注册、声明依赖），不移植机制 |
| `disabled: !!js process.platform === 'win32'` | JS 表达式 | `platform:` 键（compose 时检查），**不引入 `rhai`** | 简报 02 第 D.14 项 |

### 13.2 磁盘布局

```
$SHANGYI_HOME/                       # 部署级（deployment）
├── config.toml
├── credentials.toml                 # 0600
├── models.json                      # 自持模型目录（10.5）
├── playbooks/                       # 剧本根
│   ├── pricing_response/
│   │   ├── playbook.yml             # meta + 领域段（商奕扩展）
│   │   └── agent.plugins.yml        # 行列表（与 dsh loader row 同形）
│   ├── competitor_intel/{playbook.yml, agent.plugins.yml}
│   ├── negotiation/{...}
│   └── market_entry/{...}
├── skills/                          # SKILL.md 方法论卡（rank 400）
│   ├── bertrand-differentiated/SKILL.md
│   ├── repeated-game-folk-theorem/SKILL.md
│   └── value-net-parts/SKILL.md
└── agents/                          # persona 角色定义（第 12 章）
    ├── opponent_dumping.toml
    └── opponent_strategic.toml

<projectRoot>/.shangyi/               # 事务所策略（firm policy，随 case 仓库走）
├── config.toml
├── playbooks/                        # 事务所私有剧本（rank 高于部署级）
└── skills/                           # 事务所私有方法论卡（rank 100）

<case_dir>/consult/                   # 委托级（engagement）
├── playbook.override.yml             # 一次性覆盖（CLI --playbook-override）
└── patch.yml                         # 委托期临时补丁
```

发现 rank（**借鉴 dsh `dsh-skill` 的显式 rank 表**，`仅借鉴设计`）：

| rank | 根 | 语义 |
|---|---|---|
| 100 | `<projectRoot>/.shangyi/playbooks` | 事务所/项目 |
| 200 | `<projectRoot>/.agents/playbooks` | 跨工具共享资产（与 Agent Skills 目录约定对齐） |
| 300 | `customPlaybookDirs`（配置） | 客户自定义 |
| 400 | `$SHANGYI_HOME/playbooks` | 部署 |
| 600 | bundled（`BUNDLED_PLAYBOOK_RANK`） | 随二进制发布的四个 MVP 剧本 |

"同 id 先到先得" ⇒ 低 rank 胜出。项目根 = 最近的含 `.git` 祖先（与 dsh 的项目根定义一致）。

### 13.3 `pricing_response` 完整剧本示例

```yaml
# $SHANGYI_HOME/playbooks/pricing_response/playbook.yml
meta:
  id: pricing_response
  name: 调价/促销响应
  description: 差异化 Bertrand + 重复博弈；回答"我调价后对手怎么反应、各方结果如何"
  order: 10
  version: 1

# ── 适用条件（v0.1 §3.2 剧本声明项 1）──────────────────────────
applicable_when:
  - 决策涉及本企业对最终顾客的价格或促销力度
  - 相同或邻近商圈存在 1–3 个可观察价格的直接竞对
  - 产品有可辨识的差异化（非同质大宗品）
fails_when:
  - 强容量约束（此时 Bertrand 结论失真，转 Cournot 数值路径）
  - 招投标/拍卖式定价（转 `negotiation` 或专用机制设计路径）
required_inputs: [本企业当前价格带, 主要竞对名单, 近 6 个月调价/促销事件, 粗略毛利率]

# ── 价值网侧重（v0.1 §3.2 声明项 2 / §5 参与者确定）────────────
value_net:
  priority: [substitutor, customer, supplier]      # 五向扫描优先级
  parts_checklist:                                 # PARTS 完备性检查
    players: required
    added_value: required
    rules: optional                                # 定价规则/平台规则
    tactics: required
    scope: optional
  scan_depth:
    substitutor: deep          # 本轮博弈的直接对手
    customer: medium           # 需求弹性的来源
    supplier: shallow          # 除非成本冲击是议题
    complementor: skip
    external: shallow          # 监管/舆论仅在被引入时展开（§5.7.2）

# ── intake 问题清单（v0.1 §3.2 声明项 3 / §8.1）────────────────
intake:
  - id: q1
    text: "如果对手下个月在你同商圈降价 15%，你会？"
    kind: choice
    options: ["跟着降", "加大促销", "提升服务差异化", "按兵不动"]
    extracts: { field: reaction_style, map: { "跟着降": aggressive, "按兵不动": passive } }
  - id: q2
    text: "去年对手搞过一次大促，当时你跟进了吗？效果如何？"
    kind: open
    extracts: { field: historical_follow, via: llm_structured_extract }
  - id: q3
    text: "你的毛利率大致在什么区间？（用于收益核算，不需要精确）"
    kind: range
    extracts: { field: gross_margin, unit: percent }
  - id: q4
    text: "你能接受的最差结果是？"
    kind: open
    extracts: { field: aspiration_floor, feeds: preference }

# ── 情报需求（v0.1 §3.2 声明项 4 / §6.1 数据源）────────────────
intel_requirements:
  - { source: 电商平台/外卖/点评, need: 竞对价格轨迹与促销节奏, cadence: 近 6 个月, tools: [web_search, web_fetch, data_ingest] }
  - { source: 工商信息, need: 竞对扩店/分支变动, cadence: 近 12 个月, tools: [web_search, web_fetch] }
  - { source: 招聘 JD, need: 岗位扩张暴露的意图信号, cadence: 近 6 个月, tools: [web_search] }
  - { source: 用户自有数据, need: 销量/客流/毛利率（可选）, tools: [data_ingest] }
  profile_targets: [comp_a, comp_b, customers_core, supplier_x]
  evidence_quality_floor: "B"        # v0.1 §6.2 evidence_quality；低于此值必须在报告标注

# ── 推荐博弈模型（v0.1 §3.2 声明项 5 / §5.1 场景→模型映射）──────
model_recommendation:
  primary: differentiated_bertrand
  dynamics: { form: repeated, discount: 0.9, horizon: infinite }   # 价格战可维持性
  theory_refs: [bertrand-differentiated, repeated-game-folk-theorem, value-net-parts]
  discontinuous_alternatives:
    - { when: "容量约束强", model: cournot_numeric, note: 走 monte_carlo + 数值最佳反应 }
  payoff_form: normal
  players_max: 4                     # D-A6：支持枚举 n≤4

# ── 求解器与精炼（v0.1 §5.3，改 Rust）─────────────────────────
solver:
  enumerate: [support_enumeration, lemke_howson]      # n≤4 支持枚举；2 人 Lemke–Howson
  refine: [trembling_hand, sequential]
  seeds: { support_enum_tolerance: 1e-9, lh_max_pivots: 10000 }

# ── 选择器配置（v0.1 §3.2 声明项 6 / §5.4）────────────────────
selectors:
  - { id: risk_dom,     weight: 0.15, enabled: true }
  - { id: stochastic,   weight: 0.25, enabled: true, config: { dt: 0.01, mutation: 0.001, horizons: 3 } }
  - { id: behavioral,   weight: 0.20, enabled: true, config: { lambda_grid: [0.5, 1.2, 2.0], k_max: 2 } }
  - { id: focal_llm,    weight: 0.15, enabled: true, config: { temperature: 0.0, require_rationale: true } }
  - { id: persona_sim,  weight: 0.25, enabled: true, config: { n_rollouts: 5, perturb: true, max_rounds: 1 } }
  - { id: learning,     weight: 0.00, enabled: false }        # MVP 不实现（12.8）
  divergence_alert: 0.35             # persona_sim 与解析法差异过大时告警（§5.4）
  weights_source: prior_v0          # 待案例库回测校准；报告须标注

# ── persona 列表（v0.1 §5.7 / 第 12 章）───────────────────────
personas:
  - { player_id: comp_a, role: opponent_strategic, persona_id: comp_a_strategic, count: 3,
      perturb: { lambda_qre: [0.6, 1.2], level_k: [1, 2], aggressiveness: [0.5, 1.0] } }
  - { player_id: comp_b, role: opponent_dumping,   persona_id: comp_b_dumping,   count: 2,
      perturb: { cost_estimate_mu: [50, 62], aggressiveness: [0.7, 1.2] } }
novel_moves:
  enabled: true
  three_filter: { legality: required, resource_availability: required, incentive_compatibility: required }
  incentive_threshold: 0.6           # P(收益>成本) ≥ 0.6，与 §5.6.2 达标阈值一致
external_actors_declared: [customers_core, platform_x, regulator_market]   # 可被引入

# ── whatif / discover 参数（v0.1 §5.6）─────────────────────────
consequence:
  conventional_br: true
  unconventional: true
  repeated_path_rollout: true
  mc: { n: 1000, percentiles: [10, 50, 90], seed_policy: case_derived }
discover:
  action_templates: [price_hold, price_cut_5, price_cut_10, price_cut_15, promo_bundle, promo_duration, differentiated_service]
  param_grid: { price_delta_pct: [-20, -15, -10, -5, 0], promo_depth_pct: [0, 5, 10], timing_weeks: [1, 2, 4] }
  aspiration_default: { profit_ge: null, worst_loss_le: null }   # 由 intake 覆盖
  satisfice_threshold: 0.6
  max_candidates: 30
  output_limit: 5

# ── 报告模板变量（v0.1 §3.2 声明项 7 / §9.1 四层）──────────────
report:
  template: pricing_response           # report/templates/pricing_response.md.j2
  layers: [boss_page, scenario, profile_evidence, technical_appendix]
  variables:
    - { name: equilibrium_distribution, source: equilibrium_select.distribution }
    - { name: price_war_sustainability, source: "solver.dynamics.discount_threshold" }
    - { name: flip_thresholds, source: monte_carlo.sensitivity }
    - { name: consequence_matrix, source: consequence_predict }
    - { name: value_net_map, source: value_net.scan }
    - { name: assumption_ledger, source: checkpoint.items }
  figures: [value_net_map, payoff_heatmap, eq_distribution_bar, reaction_curves,
            replicator_phase, tornado_sensitivity, flip_threshold_curve,
            participant_timeline, profile_cards]
  disclaimers: [antitrust_redline, prior_borrowed_notice, evidence_quality_grade]

# ── 预算与门控（D-A9 / §11.1）────────────────────────────────
budget:
  max_turns: 40                      # turn 预算
  max_steps_per_turn: 8              # step 预算（step = 一次模型调用 + 其工具执行）
  max_llm_calls: 120
  max_rollouts: 5
  wall_clock_minutes: 45
checkpoints:
  - { kind: fact_confirm,       after_phase: intel,       items_from: [profiles, intel_evidence] }
  - { kind: assumption_confirm, after_phase: model_build,  items_from: [game_spec.params], gates_phase: solve }
  - { kind: result_preview,     after_phase: select,       items_from: [equilibrium_distribution], gates_phase: report }

# ── 工具白名单（第 9 章；未列入者一律 deny）────────────────────
tool_allowlist:
  intake:      [kb_retrieve, case_store_read]
  intel:       [web_search, web_fetch, data_ingest, kb_retrieve, case_store_read,
                participant_profile_update, case_store_write]
  model_build: [kb_retrieve, participant_profile_update, case_store_read]
  solve:       [game_solve]
  select:      [equilibrium_select, game_solve]
  recommend:   [consequence_predict, satisfice_search, monte_carlo, equilibrium_select]
  report:      [report_render, figure_render]
  archive:     [case_store_write]
blocked_tools: [python_sidecar_exec]   # 本题材不在关键路径上需要 LLM 写代码 ⇒ 直接禁（强于 ask）
```

```yaml
# $SHANGYI_HOME/playbooks/pricing_response/agent.plugins.yml
# 行列表：与 dsh loader row 同形（{ id, name, config, disabled?, ... }）
- id: persona
  name: shangyi.persona
  config:
    prefix: "你是商奕的参与者仿真子智能体，仅代表 {{player_name}} 的立场行事。"
    complete: true
    includeRuntimeContext: false

- id: system-prompt
  name: shangyi.system-prompt
  config:
    base_from_model_catalog: true              # 10.5：base 指令来自模型目录
    include_iron_laws: true                    # 两条铁律
    include_field_glossary: true

- id: tools
  name: shangyi.tools
  config:
    allowlist_from: playbook.tool_allowlist     # 剧本即白名单来源

- id: solver
  name: solver.rust
  config:
    backend: native_rust
    algorithms: [support_enumeration, lemke_howson, trembling_hand, sequential]
    python_fallback: false                      # D-A6：生产路径不得回退 Python

- id: sidecar
  name: tool.python-sidecar
  config:
    enabled: true
    methods: [figure.render, kb.embed, kb.search, extract.readability]   # 无 verify.nashpy
    sandbox: { network: false, read_only_root: true, tmp_writable: true, timeout_ms_default: 30000 }

- id: compaction
  name: shangyi.compaction
  config:
    pruner_first: true
    protect: [game_spec, checkpoint_items, evidence_ledger]

- id: report
  name: shangyi.report
  config: { renderer: rust_template, pandoc_process: true, figures_via_sidecar: true }
```

### 13.4 配置分层与整行替换

分层顺序（后者胜，`仅借鉴设计`，上游 dsh 的"bundle patch → profile patch → home patch → `--patch`"，简报 02 第 D.14 项）：

| 序 | 层 | 商奕对应 | Codex 的对应 `ConfigLayerSource::priority()` |
|---|---|---|---|
| 1 | bundle 默认 | `BUNDLED_PLAYBOOK_RANK` 内的内置四剧本 | `PackagedDefaults(-10)` |
| 2 | 事务所策略 | `<projectRoot>/.shangyi/`（firm policy）；企业托管走 `EnterpriseManaged{id,name}` | `EnterpriseManaged(15)` |
| 3 | 委托 playbook | 剧本 + `playbook.override.yml` | `Project(25)` |
| 4 | CLI 覆盖 | `-c key=value`（点号路径） | `SessionFlags(30)` |

**整行替换语义**（dsh 第 14 项，理由逐字保留）：补丁替换**整行的 config，绝不深合并**。

> 整行替换要求覆盖者完整表达最终意图，代价是不能依赖未写出的继承；好处是避免多层嵌套对象与数组在"追加、替换还是去重"上形成难以解释的隐式合并。

dsh 给出的动机说明对商奕尤其贴切：只写 `timeout: 30` 的补丁"可能用一份字段不完整的新 config 覆盖整行，而不是仅修改超时"。对咨询 agent（层为 bundle 默认 → 事务所政策 → 委托剧本 → 一次性 CLI 覆盖），整行替换使生效配置**可检视、可 diff**，这正是设计文档与审计所需。

```rust
// shangyi-config/src/compose.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Row {
    pub id: String,
    pub plugin: String,
    pub config: serde_yaml::Value,
    #[serde(default)] pub disabled: Option<DisabledCond>,
    #[serde(default)] pub platform: Option<Platform>,
}

/// 整行替换：同 `id` 的层条目**完全替换**该行，绝不合并。
pub fn compose(layers: &[Vec<Row>]) -> Result<Vec<Row>, ConfigError> {
    let mut acc: Vec<Row> = Vec::new();
    for (layer_idx, layer) in layers.iter().enumerate() {
        for row in layer {
            if let Some(cond) = &row.disabled {
                if cond.eval_current()? { continue; }        // 条件行：platform 检查，无 !!js
            }
            match acc.iter().position(|r| r.id == row.id) {
                Some(i) => {
                    acc[i] = row.clone();                     // 替换，绝不合并
                    tracing::debug!(layer = layer_idx, id = %row.id, "row replaced");
                }
                None => acc.push(row.clone()),
            }
        }
    }
    Ok(acc)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform { Darwin, Linux, Windows }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisabledCond { pub platform: Option<Platform> }
impl DisabledCond {
    fn eval_current(&self) -> Result<bool, ConfigError> {
        Ok(match self.platform {
            Some(p) => p != Platform::current(),
            None => false,
        })
    }
}
```

**唯一适合深合并的对象是"设置"**（用户拥有的标量与用户意图，如 `permission.default_preset`、`agent-loop.max_parallel_tool_calls`）—— 商奕把设置建模为**带优先级链的类型化结构**，与行层分离（简报 02 第 D.14 项末段）。这与 Codex 的 `Config` 分层（`_c` 覆盖、profiles、`deny_unknown_fields`）可以共存：Codex 侧管 provider/sandbox/model 等设置，商奕侧管 playbook 行（注意：基座侧的**表**合并是深合并，见 §13.4.1）。

#### 13.4.1 例外清单：Codex 基座内的深合并表

**分界必须先写清楚**：整行替换是**商奕自有配置层**（playbook 行栈，即上一节的 `compose()`）的语义 —— 该加载器由本项目控制，整行替换在此**完整生效**。**Codex 基座**的 TOML 层栈不由本项目折叠，其已证实规则为**表递归深合并 + 非表（标量/数组）整体替换**（`codex-rs/config/src/merge.rs` L75 入口、L94–95 与 L143 表分支递归、L149–152 非表整体替换；跨层测试 `codex-rs/core/src/config/config_loader_tests.rs` L854 的 `nested.extra` 存活断言；详见 `research/harness/04-codex-config-semantics.md`）。二者作用于**不同对象、不同加载器**，不能混为一谈：§13.4 的整行替换设计不变，但在基座层栈内不成立。

**例外清单**（在基座层栈内会被深合并、因而**不能**假设整行替换的表）：

| 表路径 | 合并行为 | 上层写配置的规则 |
|---|---|---|
| `[model_providers.<id>]` | 跨层**逐键深合并**：上层只写 `base_url`，下层写的 `name` / `env_key` / `http_headers` 等**保留**（`merge.rs` 对该路径无特判） | **重述完整预期配置**，不得只写差异键 |
| `[mcp_servers.<name>]` | 同一条通用合并路径 ⇒ 同名 server 跨层键合并（`[推断]`：未找到专门跨层单测；字段为普通表，规则同 `merge.rs` L94/L143） | 同上 |
| `[hooks]` / `[plugins]` / `[agents]` / `[features.*]` 等其它基座表 | 同上；其中 `features.multi_agent_v2` / `features.network_proxy` / `features.sleep_tool` 另有 Boolean↔Table 互操作特例 | 同上；完整特例表见 `research/harness/04-codex-config-semantics.md` §4.3 |

**操作规则（进 code review 检查项）**：任何在基座层栈内设置例外表的一层，必须**重述该条目的完整预期配置** —— 原因是上游合并保证的是"未写出的下层字段存活"，而不是"整条替换"；依赖"只改一个字段"的部分补丁，正是 §2.4 理由 1 描述的"为什么这个 timeout 还是 30"故障。本项目**不为例外表改** `merge.rs`（保持 D-A2 的最小补丁集）；若未来某表必须整体替换，走 `vendor/patches/` 登记的显式补丁，而不是默认换语义。

**可检视性要求（`--dump-config` / `--dump-default-config`）**：深合并使"某键最终来自哪一层"无法从单层文件读出，因此**生效配置必须可 dump** —— `--dump-config` 在 §13.5 的**行级**来源标注之外，还须打印基座层栈折叠后的**逐键 origin**（上游 `ConfigLayerStack::origins()` 即现成模式，`codex-rs/config/src/state.rs:484-533`）；`--dump-default-config` 打印无用户覆盖的基线，二者 diff 即"本次咨询相对默认改了什么"。缺这条，例外清单就退化为口口相传的隐式知识，§13.5 的审计闭环也就不完整。

**层职责指引（让例外保持小）**：

- **商奕自有对象**（playbook 行：persona / tools / solver / selectors / checkpoint…）→ 一律走整行替换行栈，**不进**例外清单；
- **基座对象**（provider、MCP server、sandbox、model 等）→ 尽量只在**一层**完整声明（推荐用户层或剧本层），避免跨层拼装；
- **必须强制且不可被下层拼装的** provider 定义 → 走 Codex 的**托管要求（requirements）**通道：该通道对同名 provider 是**整条替换**（`codex-rs/config/src/model_provider_requirements.rs:23-41`），语义与普通层不同；事务所强制策略若需要"不可拼装"语义，用它而不是普通 `EnterpriseManaged` 层。

### 13.5 持久记录与可检视性

**必须写两条记录，否则恢复会静默重组**（dsh 对 preset 的选择是持久的，原因原文："preset 决定会话的工具与 prompt，恢复出不同组合会让模型面对无法执行的历史"）：

1. `SessionHeader.playbook_id` —— 会话级字段。
2. log-only `playbook_selected` 事件（无 `surfaceOp`，不进 `derive_messages`），由 `playbook` projection 折叠出生效剧本与组合 generation。

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlaybookSelected {
    pub playbook_id: String,
    pub source_rank: u32,                 // 100/200/300/400/600
    pub source_path: std::path::PathBuf,
    pub compose_generation: u64,          // 行组合的 generation（热重载用）
    pub rows_hash: ContentHash,           // 组合结果的内容哈希 ⇒ 可与 --dump-config 对账
}
```

**`--dump-config` / `--dump-default-config`**（`仅借鉴设计`，上游 dsh）：不启动会话即检视组合后的树，并标出每行来自哪一层。商奕的等价输出必须包含：

```
$ shangyi debug dump-config --playbook pricing_response
row:id=persona        layer=deploy(rank 400)  file=$SHANGYI_HOME/playbooks/pricing_response/agent.plugins.yml
row:id=tools          layer=firm(rank 100)    file=<case>/.shangyi/playbooks/pricing_response/agent.plugins.yml  [OVERRIDDEN]
row:id=solver         layer=bundled(rank 600) file=<binary>/playbooks/pricing_response/agent.plugins.yml
...
compose_generation=7  rows_hash=blake3:9f2c...
```

这条能力是**审计要求**而非便利功能：报告④技术附录必须能回答"这次咨询用的是哪个版本的选择器权重表"，而权重表在 `playbook.yml` 的 `selectors` 段 —— 其哈希进 `rows_hash` 即可闭环。

**热重载**：若启用，用 `notify` 监视 + 重组 + **generation 计数器** —— 变更后创建的会话用新组合，运行中的会话保持启动时的 generation（dsh 对 preset 正是如此）；`compose_generation` 持久在 header 中使这一点可审计。

### 13.6 四个 MVP 剧本与三种模式

四个 MVP 剧本（v0.1 §3.2）全部表达为 playbook，同一引擎栈、不同数据：

| 剧本 | 博弈模型（v0.1 §3.2/§5.1） | 价值网侧重 | 关键 persona | 新增/特有的剧本段 |
|---|---|---|---|---|
| `pricing_response` | 差异化 Bertrand + 重复博弈（价格战可维持性） | substitutor → customer | 替代者对手（strategic / dumping） | `discover` 模板与参数网格 |
| `competitor_intel` | 不完全信息博弈 + 贝叶斯类型推断 | substitutor → external | 对手类型先验（`posterior_types`） | `intel_requirements` 加招聘 JD 与公告；`model_recommendation.information = incomplete`；画像独立成章作为价值出口 |
| `negotiation` | Rubinstein 议价 + BATNA | supplier ↔ customer | 供应商与大客户**同为 persona**（v0.1 §5.7 明文） | `payoff_form` 含议价解；`intake` 改为 BATNA 与转换成本问题；禁止 `discover` 的价格网格 |
| `market_entry` | 进入威慑 + 空间竞争（原"选址"并入） | substitutor → complementor | 在位者（威慑可信度） | `model_recommendation` 含 Hotelling/引力模型；`external_actors` 含监管与商圈 |

**三种模式（v0.1 §3.2）不是三个引擎，而是三个入口**：

| 模式 | 表达方式 | 关键差异 |
|---|---|---|
| `scenario` 情景分析 | 剧本的标准流程（§2.2 主流程） | 无额外段 |
| `whatif` 决策推演 | 通过 `-c mode=whatif -c action=<动作>` 触发 `consequence` 段 | `consequence_predict` 成为主工具；`satisfice_search` 与 `discover` 段被禁 |
| `discover` 策略发现 | 通过 `-c mode=discover -c aspiration=<json>` 触发 `discover` 段 | `satisfice_search` 成为主工具；`report.variables` 增 `pareto_frontier` 与 `satisfaction_probabilities` |

CLI：`shangyi new [--mode scenario|whatif|discover] [--playbook <id>]`，与 v0.1 §10 的命令面一致；模式作为 CLI 覆盖层（`SessionFlags` 优先级），不改剧本文件 —— 使"同一剧本在三模式下跑"可复现且可 diff。

### 13.7 `SKILL.md` 方法论卡机制

`SKILL.md` + YAML frontmatter 是**与 Claude Code（及实质上 Codex）共享的 Agent Skills 约定**（简报 02 第 D.12 项）。商奕实现的是跨工具约定的**消费者**，而非自造特性 —— 这是清单中杠杆率最高的一项移植：咨询方法论卡因此可跨工具复用，且**可由领域专家编写**。

| 机制 | disposition | 来源与事实 |
|---|---|---|
| `SKILL.md` + frontmatter 解析 | `修改后复用` | Codex `skills` crate：`parse_skill_frontmatter_metadata`、`ParsedSkillFrontmatter`、`SkillParseError`、`SkillMetadata`、`SkillInterface`、`SkillPolicy`、`SkillDependencies` | 
| 根发现与快照 | `修改后复用` | Codex：`SkillRootLoader`、`LoadedSkillRoot`、`LoadedSkills`、`SkillRootSnapshots`、`path_to_skills_md`；根来自插件（`PluginSkillRoot`、`effective_skill_roots`）+ 用户/项目 skill 根；host 扩展 `ext/skills`（`HostSkillsLoadInput`、`InjectedHostSkillPrompts`）；配置 `[skills]`、`features.skills` |
| 显式/隐式调用 | `修改后复用` | Codex：`ExplicitSkillLookup`、`ImplicitSkillLookup`、`detect_implicit_skill_invocation_for_command`、`extract_tool_mentions`、`ToolMentionKind` |
| **rank 遮蔽** | `仅借鉴设计` | dsh `dsh-skill-filesystem`：候选贡献者 + "重名时较低 `rank` 胜出"；根 rank 表见 13.2 |
| **catalog → 按需加载** | `仅借鉴设计` | dsh：`SkillSummary {name, description, invocation, source, provider, resourceBase?}`、`SkillCandidate {rank, locator, path?, metadata?}`、`SkillDefinition {content}`、`SkillCatalogSource { kind:'skill-catalog', form:'catalog', entries }`；模型可见 catalog 作为持久 catalog 形态上下文，正文由 `skill` 工具按需加载且**每次重读**。⚠️ **键名拼写差异（已核实，勿混用）**：dsh 的类型定义把"何时使用"写作**驼峰式**（camelCase，`02` 号简报按 dsh 源码转记；本行**刻意不逐字写出该拼写**，以免被复制进卡片），而 Claude Code / Agent Skills 约定使用**下划线式** `when_to_use`。**商奕卡片一律用 `when_to_use`**，不得使用 dsh 的驼峰拼写（跨 dsh 消费该驼峰键会静默失效；详见 `research/harness/07-skill-frontmatter-compat.md` §7 危险点 7） |
| **畸形 frontmatter 即丢弃该卡并告警** | `仅借鉴设计` | dsh 原文："a rejected spelling or a non-boolean value drops the whole skill with a warning rather than silently permitting a surface" —— **绝不静默放行** |
| **不发现嵌套 `**/SKILL.md`** | `仅借鉴设计` | dsh 刻意如此（只认 `<name>/SKILL.md` 目录包或扁平 `<name>.md`） |
| 单一规范渲染包装 + 标签转义 | `仅借鉴设计` | dsh `renderSkillContent` 产出单一 `<skill_content>` 块，并做转义使技能**无法开合框架标签**；工具结果与用户显式调用路径共享同一渲染 |
| 双面调用控制 | `仅借鉴设计` | dsh `SkillInvocationPolicy { modelInvocable, userInvocable }`；frontmatter 可选 `disable-model-invocation`、`user-invocable` |
| 监视机制（chokidar 稳定性阈值等） | **不移植** | 简报 02 第 E.5 项：按需重扫即可；catalog 很小，按 turn 重扫更简单更省 |

商奕方法论卡示例：

```markdown
---
name: bertrand-differentiated
description: 差异化产品价格竞争的标准分析路径：何时适用、需要什么数据、输出什么
when_to_use: 决策涉及价格/促销，且产品有可辨识差异化、价格可观察
user-invocable: true
metadata:
  theory_id: bertrand-differentiated
  family: noncooperative
  applicable_when: [同业竞对, 可观察价格, 差异化产品]
  fails_when: [容量约束强, 拍卖/招投标]
  data_needs: [价格轨迹, 需求交叉弹性]
  outputs: [均衡价格, 反应函数]
  version: "1.0"
---

# 差异化 Bertrand 价格竞争

## 适用条件
…
## 失效条件
…
## 数据需求
…
## 输出物
…
## 与商奕管线的接口
- 本卡只提供建模路径与参数口径；**具体均衡数值由 `game_solve` 产生**（铁律 1）。
- payoff 参数进入 `GameSpec.params`，必须带 `dist` 与 `source`。
```

`catalog` 只给 `name` + `description`（+ `when_to_use`），正文按需加载 —— 使 30 余张卡不占用上下文预算（第 14 章）。

#### 13.7.1 frontmatter 键集：本地可用 ≠ 可分发（**卡作者的硬约束**）

**键名归属已核实（2026-09-17，工作项 `A8`，`research/harness/07-skill-frontmatter-compat.md`）**：本卡只需 `name` + `description`（顶层）+ `metadata`（map）三键即可双端消费 —— 这是 Codex 解析器与 Claude Code / Agent Skills 约定的**逐字交集**。三个关键事实：

| 事实 | 来源与证据 |
|---|---|
| **Codex 不拒绝未知字段** | `codex-rs/skills/src/parser.rs` 的 `SkillFrontmatter` 只有 `#[derive(Debug, Deserialize)]`，**未启用 `deny_unknown_fields`**；全 crate 16 个文件中该属性出现 **0 次**。故 Claude 侧扩展键（`allowed-tools`、`when_to_use`、`model`、`hooks`、`paths` …）写入同一份 `SKILL.md` 时，Codex **静默忽略、不丢卡**（`parser_tests.rs` 有未知键容忍的测试） |
| **`description` 在 Codex 侧是必填** | 空/缺省 → `SkillParseError::MissingField("description")` → **该卡被丢弃**（Claude Code 则会回退到正文首个非空行，不报错） |
| **Claude 的"分发路径"是唯一的硬错误来源** | Claude Code **本地加载**接受文档列出的扩展键；但 **claude.ai 上传 / Skills API / `package_skill.py` 打包**只接受**恰好六个**顶层键，任何表外键都会**硬错误整卡拒绝**（不是忽略） |

> **分发路径六键白名单（逐字）**：`name`、`description`、`license`、`compatibility`、`metadata`、`allowed-tools`。
>
> 报错形态逐字：`Unexpected key(s) in SKILL.md frontmatter: argument-hint. Allowed properties are: allowed-tools, compatibility, description, license, metadata, name`。
>
> **⚠️ 这是卡作者最容易踩的坑**：一张卡在**本地**（Rust harness + 本地 Claude Code）完全可用，但因为它带了 `when_to_use`（或 `argument-hint`/`hooks`/`model` 等）而**在 claude.ai 上传时被硬错误拒绝**。**本地可用性不能推断分发性。**
>
> **决策规则**：
> 1. 卡片只在**本地**消费 → 可用扩展键（含 `when_to_use`），Codex 侧不破。
> 2. 卡片要走 **claude.ai 分发 / Skills API / `package_skill.py`** → 顶层**只保留六字段**，把"何时使用"的语义并入 `description`，把扩展语义移到正文或 `metadata` 内。
> 3. 无论走哪条路，卡片模板必须是：`name`（小写连字符、≤64、**与卡片目录名完全一致**）+ **非空** `description`（≤1024，含触发语）+ `metadata`（**必须是 map**）。
> 4. **Codex 专属字段**（`interface` / `policy` / `dependencies`）**不要写进 `SKILL.md`**，放同目录的 `agents/openai.yaml`（Codex 自己的 sidecar 扩展通道，Claude 侧对多出的 sidecar 文件不作要求）。

**两处已证实的勘误（吸收自 `A8` 的独立验证者，避免下游按错症状修数据）**：

1. **"`metadata` 值为标量 = 硬失败"是过度概括**。Codex 侧对 `metadata.short-description` 的 `Option<String>` 反序列化，**数字 / 浮点 / 布尔会被 serde 强制转成字符串**（`25` → `"25"`、`1.5` → `"1.5"`、`true` → `"true"`，均成功）；**只有序列 / map**（`[a,b]`、`{a: b}`）才报 `invalid type: sequence/map, expected a string` → 整卡失败。**工程建议不变且更稳妥**：值一律加引号（示例卡里的 `version: "1.0"` 即为此故）。**但不要据此写"把数字改成字符串"的修复脚本** —— 那会误判症状：数字本身不是问题，**列表与映射才是**。
2. **`deny_unknown_fields` 的计数**：产物初稿写"全仓 30+ 处"，独立核实为 **269 处 / 67 个文件**（30+ 只是搜索工具返回上限）。**承重事实不是这个总数，而是 `skills/` crate 内为 0** —— 这 269 处全部位于 `config`/`protocol`/core handlers 等其他 crate，说明该属性在 Codex 中是**刻意使用**的，而 skills crate **刻意不用**。

> **Codex 侧"`description` 缺失即丢卡"的行为，与 dsh "畸形 frontmatter 即丢弃该卡并告警"的纪律一致**（本表第 6 行）—— 两者都是"宁可丢卡也不静默放行"，与本项目的不变量 ② 同向。

### 13.8 理论库条目与 skills 的关系

v0.1 §7.1 的理论库（v0.1 约 29–35 篇：博弈论核心 12 + 扩展理论 12 + 方法论 5，Chroma + BGE 中文 embedding）与 skills 是**同一资产的两个消费面**，必须显式区分以免出现两个真相源：

| 面 | 用途 | 机制 | 触发方式 |
|---|---|---|---|
| **方法论卡（skill）** | 模型**主动加载**一段建模路径 | `SKILL.md` + catalog → 正文按需加载（13.7） | 模型按 `description`/`when_to_use` 选择；或用户显式 `/skill <name>` |
| **理论库检索（RAG）** | 语义检索"哪条理论适用于我的情形" | 矢量索引（`kb_retrieve`，经 Python 侧车 `kb.embed` + `kb.search`） | 工具调用，top_k 返回 doc/section/score |

**规范化方向**：**markdown 文件（含 frontmatter）是唯一真相源**；矢量索引是其**派生的投影**（可重建、可丢弃）。理由：① skills 面要求正文可 diff、可人工编写；② 索引可因 embedding 模型换代而重建，不能承载真相；③ v0.1 §7.1 的四要素 schema（`applicable_when` / `fails_when` / `data_needs` / `outputs`）落在 frontmatter `metadata` 中，使同一文件同时满足两种消费。v0.1 §7.1 的 `theory_id`/`family`/`version` 字段原样保留。

因此 `kb_retrieve` 的返回必须带 `theory_id`，并**可解析回该 `SKILL.md`**；模型检索到后可用 `skill` 工具加载全文，避免索引命中但拿不到方法细节。

---

## 14. 上下文与压缩

### 14.1 每模型上下文窗口与自动压缩阈值

三个字段来自模型目录（10.5，Codex 事实）：`context_window`（当前生效窗口）、`max_context_window`（模型上限）、`auto_compact_token_limit`（触发自动压缩的阈值）；`truncation_policy{mode, limit}` 决定溢出时的截断方式。Codex 侧的对应机制：`Session::get_estimated_token_count`、`estimated_tokens_after_last_model_generated_item`、`auto_compact_window_snapshot`、`get_total_token_usage`、`token_usage_info`；`core/src/compact.rs` 的 `run_compact_task`、`run_inline_auto_compact_task`、`CompactionPhase::{PreTurn, MidTurn}`、`TurnItem::ContextCompaction`、`COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000`；溢出时"removing oldest history item"。配置键 `model_context_window`、`model_auto_compact_token_limit`、`model_auto_compact_token_limit_scope`、`compact_prompt`、`experimental_compact_prompt_file`、`tool_output_token_limit`、`project_doc_max_bytes`。

| 组件 | disposition |
|---|---|
| Codex 的压缩触发与 token 预算逻辑（`core/src/compact.rs`、`compact_token_budget.rs`、`session/token_budget.rs`、`rollout_budget.rs`、`session/context_window.rs`、`context_manager/{history,normalize,updates}.rs`） | `直接复用`（Codex，Apache-2.0） |
| Hook `PreCompact`/`PostCompact`、事件 `EventMsg::ContextCompacted`、`Op::Compact` | `直接复用` |
| `prompts::SUMMARIZATION_PROMPT`、`prompts::SUMMARY_PREFIX` | `修改后复用`：摘要提示需承载商奕的"保留 GameSpec/假设/台账"要求（见 14.5） |
| dsh 的 `compaction/*` 事件协议与影子价格模型 | `仅借鉴设计`（上游 dsh，MIT © 2026 DeepSeek） |

商奕的窗口设定（示例，真实值待 V10 实测回填）：`glm-4.6` → `auto_compact_token_limit = 150000`；`deepseek-flash`/`deepseek-v4-pro` → 100000（**模型 ID 用 `/models` 的实际返回，不用 `deepseek-chat` 别名**，见 §10.3/§10.5）。**阈值应显著低于 `context_window`**，因为压缩本身要消耗一次完整请求的预算（`COMPACT_USER_MESSAGE_MAX_TOKENS = 20_000` 是 Codex 对压缩用户消息的硬上限）。

### 14.2 压缩事件协议

采纳 dsh 的协议（`仅借鉴设计`），五个事件：

| 事件 | 载荷 | 语义 |
|---|---|---|
| `compaction/start` | `{ compactionId, sourceCommandId?, turn: number\|null }` | log-only，**且它就是锁**，持有到一个 `compaction/end`。`turn: null` 标识 turn 之间的独立手动事务 |
| `compaction/summary` | `{ compactionId, summary, shadowedRange{start,end}, shadowedSeqs, shadowedTokenCount, provider, model, maxTokens?, usage?, rawOutput, llmStreamCall? }` | log-only；surface 替换由**紧接着的**携带 `compactCheckpointSource = { kind:'plugin', plugin:'compact', compactionId, sourceCommandId? }` 的 `user/message` 执行 |
| `compaction/prune` | `{ shadowedRange, shadowedSeqs, shadowedTokenCount }` | 无模型剪枝的**同一影子价格协议** |
| `compaction/end` | `{ compactionId, turn, error? }` | 释放锁；**失败尝试仍留在日志中** |

三条契约细节必须保留（简报 02 第 D.7 项）：

1. **相邻性是契约性的**，不是巧合：surface 替换事件必须**紧随**其计价事件，因为被遮蔽的计价字段就是该替换的"影子价格"。
2. **`shadowedRange` 是 surface 位置跨度，不是数值区间** —— 先前 replace 把高 seq 摘要落到较旧位置后，`start` **可能大于** `end`；**`shadowedSeqs` 才是权威**。
3. **边界范围必须配平**（`toolPairingBalancedBefore/After`），使工具调用与结果保持配对。

```rust
// shangyi-compact/src/types.rs
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Seq(pub u64);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShadowPrice {
    /// surface 位置跨度；start 可能 > end，不能当区间用。
    pub shadowed_range: (Seq, Seq),
    /// 权威：被遮蔽事件的精确 seq 集合。
    pub shadowed_seqs: Vec<Seq>,
    pub shadowed_token_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CompactionEvent {
    Start  { compaction_id: CompactionId, source_command_id: Option<String>, turn: Option<u64> },
    Summary { compaction_id: CompactionId, summary: String, price: ShadowPrice,
              provider: String, model: String, max_tokens: Option<u64>,
              usage: Option<TokenUsage>, raw_output: String },
    Prune  { price: ShadowPrice },
    End    { compaction_id: CompactionId, turn: Option<u64>, error: Option<String> },
}
pub type CompactionId = uuid::Uuid;

pub struct CompactionEngine {
    budget: TokenBudget,
    pruner: Arc<dyn ToolResultPruner>,
    summarizer: Arc<dyn Summarizer>,
    log: Arc<EventLog>,
}

impl CompactionEngine {
    /// 压力触发：token 用量越过 auto_compact_token_limit。
    pub async fn compact_if_needed(&self, agent: &AgentHandle, trigger: CompactTrigger,
                                   cancel: &CancellationToken) -> Result<Option<CompactionId>, CompactError>;
    /// 空闲会话手动压缩（Op::Compact / CLI `shangyi compact`）。
    pub async fn compact_now(&self, agent: &AgentHandle, cancel: &CancellationToken)
        -> Result<CompactionId, CompactError>;
    /// 指定 surface 区域压缩（用于长 whatif 管线的中途压缩）。
    pub async fn compact_region(&self, start: Seq, end: Seq, agent: &AgentHandle,
                                cancel: &CancellationToken) -> Result<CompactionId, CompactError>;
}

trait Summarizer { async fn summarize(&self, region: &RegionView<'_>) -> Result<SummaryOutput, CompactError>; }
```

执行顺序固定为 **剪枝器先跑，摘要器后跑**（dsh：`dsh-compaction-tool-result-pruner` **先跑，无模型、确定性**）。

### 14.3 确定性剪枝器（无模型，先跑）

dsh `dsh-compaction-tool-result-pruner` 的默认参数与纪律（`仅借鉴设计`）：

| 参数/行为 | 值 | 商奕沿用 |
|---|---|---|
| `thresholdChars` | 8192 | 沿用；商奕的 `consequence_predict` 与 `satisfice_search` 结果常超此值 |
| `headChars` | 4096 | 沿用 |
| `tailChars` | 1024 | 沿用 |
| 剪枝标记 | `"\n\n[... tool result middle pruned ...]\n\n"` | 改为中文标记 `"\n\n[… 工具结果中段已剪枝 …]\n\n"` 并保持单行、可正则识别 |
| 切分单位 | **Unicode 码点**（不是 UTF-16 单元），绝不切断代理对 | Rust 中即 `s.chars().count()` 与 `char_indices()`；`[推断]` dsh 的这条约束在 Rust 中天然满足（`String` 是 UTF-8，无代理对），但**必须**用 `char_indices` 而非字节切片，否则会切断多字节码点 |
| 边界配平 | `toolPairingBalancedBefore/After` | 原样移植为纯函数 |

```rust
// shangyi-compact/src/pruner.rs
#[derive(Clone, Debug)]
pub struct PruneConfig {
    pub threshold_chars: usize,   // 8192
    pub head_chars: usize,        // 4096
    pub tail_chars: usize,        // 1024
    pub marker: &'static str,     // "\n\n[… 工具结果中段已剪枝 …]\n\n"
}

/// 确定性、无模型：同一输入必然同一输出。返回 None 表示无需剪枝。
/// 按 Unicode 码点切分（char_indices），绝不切断多字节码点。
pub fn prune_tool_result(text: &str, cfg: &PruneConfig) -> Option<PrunedResult> {
    let n = text.chars().count();
    if n <= cfg.threshold_chars { return None; }
    let head_end = text.char_indices().nth(cfg.head_chars).map(|(i, _)| i)?;
    let tail_start = text.char_indices()
        .nth(n - cfg.tail_chars)
        .map(|(i, _)| i)
        .unwrap_or(head_end);
    if tail_start <= head_end { return None; }        // 绝不重叠
    let pruned = format!("{}{}{}", &text[..head_end], cfg.marker, &text[tail_start..]);
    Some(PrunedResult {
        text: pruned,
        original_chars: n,
        pruned_chars: cfg.head_chars + cfg.tail_chars + cfg.marker.chars().count(),
        head_end_char: cfg.head_chars,
        tail_start_char: n - cfg.tail_chars,
    })
}

/// 边界配平：使 replace 区域的工具调用与其结果保持配对。
pub fn tool_pairing_balanced_before(events: &[Recorded<Event>], start: Seq) -> Seq;
pub fn tool_pairing_balanced_after(events: &[Recorded<Event>], end: Seq) -> Seq;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_splits_multibyte_codepoint() {
        let s = "价".repeat(9000);                    // 3 字节/码点
        let out = prune_tool_result(&s, &PruneConfig::default()).unwrap();
        assert!(out.text.is_char_boundary(out.head_end_char));
        assert!(std::str::from_utf8(out.text.as_bytes()).is_ok());
        assert_eq!(out.pruned_chars, 4096 + 1024 + out.marker_chars());
    }
    #[test]
    fn idempotent_and_deterministic() {
        let s = "x".repeat(10000);
        assert_eq!(prune_tool_result(&s, &Default::default()).unwrap().text,
                   prune_tool_result(&s, &Default::default()).unwrap().text);
    }
}
```

剪枝器是**纯函数**：简报 02 明示"把 `tool_pairing_balanced_before/after` 与剪枝器按纯函数移植，用手工构造的日志做单测"，且"用 `debug_assert!` + 测试守住顺序不变量（replace 事件必须紧随其计价事件同步追加），因为它是契约而非巧合"。

### 14.4 摘要器：复用会话自身的 system prompt 与工具

dsh 的关键约束（`仅借鉴设计`）：`dsh-compaction-basic` 随后一次性 `ctx.llm.stream()` 摘要，"其前缀**复用会话自身的 system prompt、工具与消息，以免作废 provider 的 KV 缓存" —— `summarize()` 是唯一的子类钩子。

商奕必须保留这一点，因为一次咨询的 session system prompt（base instructions + 铁律 + 领域口径 + skill catalog）是长前缀；换一个摘要专用 prompt 会使 prefix cache 全部失效，压缩成本反而上升。落地：

- 摘要请求的 prefix **逐字节等于**当前 session 的 prompt 前缀（不重排段、不插新段）。
- 只在末位追加压缩指令段（该段不计入 prefix cache，但只影响尾部）。
- `prompts::SUMMARIZATION_PROMPT` 与 `SUMMARY_PREFIX` 保留结构，内容按 14.5 的"不可压缩清单"改写。
- token 计量用 `tiktoken-rs` 或与估算器一致的 tokenizer（简报 02 建议；真实值以服务端 usage 为准，见 10.5）。

### 14.5 商奕不可压缩清单与 pin 机制

**这是本章最重要的一节。** 报告的审计链是交付物：v0.1 §1.5 铁律 1 要求"报告中每个数字必须有代码/数据出处"，v0.1 §7.2 的案例库要求 `assumptions[].source`、`game_spec_ref`、`selection_output.selector_weights_snapshot` 可验证。压缩会遮蔽事件（`ShadowPrice`），而**被遮蔽的内容不再出现在上下文里** —— 若关键结构被遮蔽，后续报告渲染拿不到它，provenance 链断裂。

| 必须永不压缩的对象 | 为什么 | pin 机制 |
|---|---|---|
| **GameSpec**（v0.1 §5.2，含全部 `params` 的 `expr`/`dist`/`source`） | 报告中每个 payoff 与参数的最终来源；一旦被摘要文字取代，`params[].source` 不可机读 | ① 作为独立 projection（`game_spec`）常量持有，不经 surface；② 每个 `model_build` 阶段结束把当前版本写入 case 目录（`game-spec.v<N>.yml`）；③ 压缩后以 `game_spec_pin` 段**重注入** |
| **已确认假设**（checkpoint ①② 的答案） | 铁律 2；`assumptions[].source` 需可验证 | ① checkpoint 答案已是 log-only 事件（11.7），日志本身不被压缩（**压缩只改 surface，不改日志**）；② `assumption_ledger` projection；③ 压缩后重注入摘要行 |
| **checkpoint 答案**（三个 checkpoint 全部） | 同上；③ 结果预览的答案决定报告是否发布 | 同 assumption ledger；③ 的答案作为 `report_release_decision` 进 manifest |
| **证据台账（evidence ledger）** | provenance 的最终凭据（`event_seq` → 工具调用 → 数据文件） | ① 独立 append-only 文件 `artifacts/evidence-ledger.jsonl`，**不受压缩影响**；② 上下文中只保留其 projection 的计数与最近 N 条；③ 需要时按 `event_seq` 重新物化 |

```rust
// shangyi-compact/src/pin.rs
/// pin 集合：这些 projection 的当前值在每次压缩后**必须**被重新注入 surface。
pub const PROTECTED_PROJECTIONS: &[ProjectionKey] = &[
    ProjectionKey::GameSpec,             // v0.1 §5.2
    ProjectionKey::AssumptionLedger,     // checkpoint ①② 答案
    ProjectionKey::CheckpointOutcome,    // checkpoint ③
    ProjectionKey::EvidenceLedgerRef,    // 台账引用（计数 + 最近 N 条 seq）
    ProjectionKey::PreferenceVector,     // v0.1 §8.2 偏好，用户确认过，不得被改写
];

pub struct PinInjector { policy: PinPolicy }
#[derive(Clone, Debug)]
pub struct PinPolicy {
    pub max_pinned_tokens: u64,          // 硬上限，超出 ⇒ 压缩失败而非静默丢弃
    pub revalidate_after_compaction: bool, // true：pin 后校验哈希与压缩前一致
}

impl PinInjector {
    /// 压缩后重注入。若注入后仍超预算 ⇒ 返回错误（fail closed），
    /// **绝不**静默丢弃受保护内容 —— 宁可压缩失败并告知用户。
    pub fn reinject(&self, engine: &CompactionEngine, cx: &TurnCx)
        -> Result<PinInjectionReport, CompactError> {
        let mut report = PinInjectionReport::default();
        for key in PROTECTED_PROJECTIONS {
            let value = cx.projections.snapshot(key)?;
            let rendered = render_pin(*key, &value, self.policy.max_pinned_tokens)?;
            cx.surface.inject_user_message(UserMessage::pinned(*key, rendered))?;
            report.injected.push((*key, value.content_hash()));
        }
        if cx.tokens.estimated() > cx.tokens.limit() {
            return Err(CompactError::PinnedExceedsBudget { report });
        }
        Ok(report)
    }
}
```

**为什么 pin 而不只是"排除在压缩区外"**：`compaction/prune` 与 `compaction/summary` 都以 surface 位置跨度表达遮蔽；把受保护内容**排除**出压缩区在长会话里会累积到窗口不可承受。正确做法是**允许遮蔽、但强制重注入**（内容短：GameSpec 的摘要形态 + 假设清单 + 台账计数），并设硬上限使失败可见。这也是 `PinPolicy` 必须有 `max_pinned_tokens` 的原因。

`compact_prompt` / `experimental_compact_prompt_file` 的商奕改写要点：摘要提示必须显式要求"列出全部已确认假设 id 与来源、列出当前 GameSpec 版本号与参数来源分布、列出证据台账中支撑主结论的事件 seq"，使摘要本身也携带可核对的指针（即使 pin 机制失效，摘要里仍有线索）。

### 14.6 注入类上下文（`repeat-tool-reminder` / `time-context`）

`仅借鉴设计`（上游 dsh：`dsh-repeat-tool-reminder`、`dsh-time-context`、`dsh-tmux-context`、`dsh-output-retention`、`dsh-agent-tool-presentation`、`dsh-system-prompt`）。

| 注入 | dsh 对应 | 商奕用法 |
|---|---|---|
| 重复工具提醒 | `dsh-repeat-tool-reminder` | 当模型在同一 turn 内反复调用同一工具且参数近似时，注入一条简短提醒（"你在重复检索同一 query；已命中的条目见上"）。商奕场景里常见于反复 `web_search` 同一竞对 |
| 时间上下文化 | `dsh-time-context` | 每次请求注入"当前日期 + 距上次竞对调价事件的天数"。对 `pricing_response` 是承重的：价格战时长、利润恢复期的解释依赖时间锚点 |
| 终端/环境上下文 | `dsh-tmux-context` | **不适用**（商奕无终端）；对应物为"当前 case 目录 + 当前阶段 + 已确认假设摘要"，由 `shangyi-system-prompt` 的 `include_field_glossary` 段承载 |
| 输出保留 | `dsh-output-retention` | 用于决定工具结果在 surface 上的保留期；与 14.3 剪枝器配合（先剪枝、后遮蔽） |

**纪律**：所有注入都是 `SurfaceEventType::user/message` 且必须带 `source`（`Model-visible ⟺ logged`）。注入不得包含未记录的数值 —— 若时间上下文提到"距上次调价 45 天"，该 45 天必须来自某条已记录画像事件（v0.1 §6.2 `timeline[].date`）。

---

## 15. 报告生成管线

### 15.1 分层职责与许可边界

| 层 | 负责 | 技术 | disposition |
|---|---|---|---|
| 数值与规格 | 计算全部数字系列、图表**规格**（数据 + 坐标轴 + 标注 + 样式 token） | Rust（`shangyi-report`、`shangyi-numeric`） | `自研` |
| 图形渲染 | 把规格渲染成 PNG/SVG | 进程隔离的 Python 侧车 `figure.render`（`matplotlib`，PSF 系） | `自研`（渲染侧仅消费规格；v0.1 §9.2 "matplotlib sandbox 统一产出"收窄为规格驱动） |
| 文档装配 | 模板 + 变量 → markdown | Rust（`minijinja` 或手写渲染） | `自研`，见 15.5 |
| 最终格式 | markdown → PDF / docx | **独立进程** `pandoc` | `自研`（调用外部 `pandoc` 进程；管线沿用 v0.1 §9.3） |
| 溯源 | 每层产物的 provenance 记录 | Rust | `自研`，见 15.4 |

**许可说明（必须写入文档，非法律意见）**：`pandoc` 为 **GPL-2.0-or-later**。以独立进程调用并通过普通 CLI/管道/文件通信，按 FSF 的经典立场不使商奕成为衍生作品 —— 与专有构建系统调用 GCC 同理；**若再分发** pandoc（如在安装器中捆绑二进制），则须**就 pandoc 本身**遵守 GPL：提供其完整对应源码或书面要约、附许可、不限制接收方的 GPL 权利；它仍可作为聚集体与专有部分一同分发。`matplotlib` 为 **PSF 系**（matplotlib 1.3.0+ 许可协议），宽松，安全。`numpy`（BSD-3-Clause AND 0BSD AND MIT AND Zlib AND CC0-1.0）与 `scipy`（BSD-3-Clause；捆绑的 GCC runtime 部分为 GPL-3.0-or-later **带 GCC 例外**，不感染用户代码）同样安全。**Gambit 不在管线中**（D-A6）。

**未取得项**：`graphviz`（v0.1 §9.2 的博弈树渲染、§10 技术栈）许可**未在本轮简报中核实**；若沿用"外部进程调用 graphviz 渲染 DOT"（与 pandoc 同类，进程边界保持臂长），分发前须单独核实其许可并补入本节的许可清单。

### 15.2 图表清单 → 谁计算

v0.1 §9.2 的十项图表，逐一映射到**计算方**与**渲染方**（计算方是审计要点：图上的每个数字必须来自 Rust 确定性代码，不得由 matplotlib 或 LLM 决定）：

| # | 图 | 数值/结构由谁计算 | 渲染方式 | 数据来源（事件/投影） |
|---|---|---|---|---|
| F1 | 价值网全景图 | Rust：`value_net.scan` 产出五向节点与边 | graphviz DOT（Rust 造 DOT，外部进程渲染） | `value_net` projection |
| F2 | 参与者画像卡 | Rust：`ProfileStore` → 卡片字段（v0.1 §6.2） | Rust 排版 + 侧车渲染卡图 | 画像事件 + `participant_profile_update` 工具结果 |
| F3 | 支付矩阵热力图 | Rust：`game_solve` 的 payoff 矩阵（按 P50 参数） | 侧车 `figure.render` | GameSpec projection + `EquilibriumSet` |
| F4 | 博弈树 | Rust：扩展式游戏树（逆向归纳路径标注） | graphviz DOT | GameSpec（`payoff_form = extensive`）+ `game_solve` 逆向归纳结果 |
| F5 | 均衡概率分布（条形） | Rust：`equilibrium_select` 的 `distribution` | 侧车 `figure.render` | `equilibrium_select` 工具结果 |
| F6 | 反应曲线 | Rust：`consequence_predict` 的常规 BR 集与条件反应 | 侧车 `figure.render` | GameSpec + BR 枚举结果 |
| F7 | replicator 相图 | Rust：`equilibrium_select` 的 `stochastic` 选择器轨迹（顶点场 + 轨迹） | 侧车 `figure.render` | 选择器证据 `evidence`（收敛轨迹摘要，v0.1 §5.4 `SelectorScore.evidence`） |
| F8 | 敏感性龙卷风图 | Rust：`monte_carlo` 的 one-at-a-time 扫描 | 侧车 `figure.render` | `monte_carlo` 工具结果 |
| F9 | 翻转阈值曲线 | Rust：`monte_carlo.sensitivity` 的阈值求解 | 侧车 `figure.render` | `monte_carlo` 工具结果 |
| F10 | 参与者事件时间线 | Rust：画像 `timeline[]` 合并排序 | 侧车 `figure.render` | 画像事件 + `intel_evidence` |

**规格/渲染分离的收益**：① 侧车只拿到"画什么"的声明式规格，不拿到业务逻辑 ⇒ 侧车不可能改变数字；② 规格可序列化存盘（`artifacts/figure-specs/*.json`），使图可在不重跑咨询的情况下重渲染（样式升级、字体修复）；③ 侧车失败只导致图缺失，**数字仍在**。

```rust
// shangyi-report/src/figure.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FigureSpec {
    pub figure_id: String,                  // 与 §9.2 清单的稳定 id 对应
    pub kind: FigureKind,                   // ValueNet | ProfileCard | PayoffHeatmap | GameTree |
                                            // EqDistributionBar | ReactionCurves | ReplicatorPhase |
                                            // TornadoSensitivity | FlipThresholdCurve | Timeline
    pub data: FigureData,                   // 已算好的系列；见下
    pub style: StyleToken,                  // 主题、中文字体、字号、配色（PSF 渲染侧只消费）
    pub annotations: Vec<Annotation>,       // 关键标注（含 provenance id）
    pub provenance: Vec<ProvenanceId>,      // 本图全部数值的来源
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum FigureData {
    Series { labels: Vec<String>, values: Vec<f64>, units: Option<String> },
    Matrix { row_labels: Vec<String>, col_labels: Vec<String>, cells: Vec<Vec<f64>> },
    GraphDot { dot: String },                       // Rust 造好 DOT，渲染侧不加工
    Timeline { events: Vec<TimelineEvent> },
    Params { items: Vec<ParamSweep> },              // 龙卷风/阈值
}
```

### 15.3 四层报告结构与数据来源

四层结构沿用 v0.1 §9.1。**每一层的数据必须来自 logged 事件 + projection，不得来自 LLM 的叙述性生成** —— 这是让"每个数字可溯源"成立的结构性前提：LLM 只负责把结构化内容组织成可读文字，不负责产生数字。

| 层 | 读者 | 内容（v0.1 §9.1） | 数据来源（事件 / 投影） | LLM 的角色 |
|---|---|---|---|---|
| ① 老板页 | 决策者 | 主图（均衡概率分布）+ 三句话结论 + 建议动作 | `equilibrium_select` 结果 + `recommend` 阶段的偏好注入结果 | 把结论改写成小白可读的三句话；**不得引入新数字**，数字槽位由模板填充 |
| ② 情景推演 | 经营层 | 博弈树、支付矩阵热力图、演化动态图、条件策略预案表 | `game_solve`（树/矩阵）、`equilibrium_select`（演化轨迹）、`consequence_predict`（预案表） | 解释每张图"怎么看"；预案表的"若对手 X 则转 Y"由条件策略生成结果直接填入 |
| ③ 参与者画像与证据 | 经营层 | 价值网全景、各参与者画像卡、时间线、参数估计、来源与可信度 | 画像事件（v0.1 §6.2）+ `evidence_quality` + 情报事件 | 归纳"对手的行为风格"叙述；`evidence_quality` 等级由规则给出 |
| ④ 技术附录 | 懂行者/大企业战略部 | 模型设定、均衡解、选择判据明细、敏感性、假设清单 | GameSpec projection、`EquilibriumSet`、`SelectorScore[]`（含 `rationale` 与 `evidence`）、`monte_carlo`、checkpoint items | 只做格式整理；附录**以结构化 dump 为主**，减少叙述 |

第 ④ 层是审计主战场，故应包含机器可读段落：GameSpec 的完整 YAML、`EquilibriumSet` 的 JSON、各选择器的 `SelectorScore`（`equilibrium_id`/`weight`/`rationale`/`evidence`）、`monte_carlo` 的百分位表、以及**全部 pin 内容**（14.5）与 `compose_generation`/`rows_hash`（13.5）。

### 15.4 provenance 链

**要求（逐字）**：*每个数字可溯源到某次工具调用 / 某个事件 seq / 某个数据文件。*

三层落地：

1. **数值出生即带 provenance**。工具的 `ToolOutput` 必须携带 `provenance`（9.3 的 `tools/post-execute` 强制；无 provenance 即 `Block`）。
2. **事件 seq 是锚点**。工具调用与其结果都是 surface 事件，有稳定 `seq`；`ProvenanceRecord` 记录 `event_seq`，故可从报告数字反查到日志行。
3. **渲染时先解析 provenance，再落盘**。`report_render` 在写文件前把所有图表与数值槽位的 `ProvenanceId` 解析成 `provenance.json`；**任何未解析成功的引用 ⇒ 渲染失败**（fail closed，不是占位符）。

```rust
// shangyi-report/src/provenance.rs
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProvenanceId(pub uuid::Uuid);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    pub id: ProvenanceId,
    pub kind: ProvenanceKind,
    /// 产生该数字的工具调用（工具 wire 名 + 调用 id）。
    pub tool_call: Option<ToolCallRef>,
    /// 日志锚点：该数值首次出现在日志中的事件序号。
    pub event_seq: Seq,
    /// 数据文件来源（`data_ingest`、用户上传、公开数据抓取落盘）。
    pub source_path: Option<std::path::PathBuf>,
    pub source_hash: Option<ContentHash>,          // blake3
    /// 若数字由 Rust 代码计算：代码版本标识（crate + git rev + 参数哈希）。
    pub code_rev: Option<CodeRev>,
    /// 若数字涉及 LLM 输出（如聚点判断）：模型与调用 id —— 便于区分"LLM 提案"与"引擎算出"。
    pub llm_call: Option<LlmCallRef>,
    /// 上游来源：本次数字由哪些其他 ProvenanceId 派生（DAG）。
    pub derives_from: Vec<ProvenanceId>,
    pub produced_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceKind {
    ToolOutput,      // 工具直接产出（game_solve / monte_carlo / …）
    DataFile,        // 上传或抓取落盘的数据
    UserAnswer,      // checkpoint 答案（铁律 2）
    PriorBorrowed,   // 行业先验（报告须标注 "borrowed，非本对手实证"）
    LlmProposal,     // LLM 提案（如 focal_llm 聚点判断、novel move 候选）—— 须经引擎评估
    Derived,         // 由其他 ProvenanceId 派生
}

impl ProvenanceResolver {
    /// 渲染前全量解析；任一条目缺失或哈希不符 ⇒ 渲染失败（fail closed）。
    pub fn resolve_all(&self, ids: &[ProvenanceId]) -> Result<Vec<ProvenanceRecord>, ReportError> {
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            let rec = self.records.get(id)
                .ok_or(ReportError::UnresolvedProvenance(*id))?;
            if let (Some(p), Some(h)) = (&rec.source_path, &rec.source_hash) {
                if &self.hash_file(p)? != h { return Err(ReportError::SourceTampered(*id)); }
            }
            out.push(rec.clone());
        }
        Ok(out)
    }
}
```

**`LlmProposal` 这一 kind 是商奕特有的**：v0.1 §5.4 的 `focal_llm` 选择器与 §5.7 的 novel move 都涉及 LLM 提案。它们必须标为 `LlmProposal` 且**必须** `derives_from` 到某个引擎评估结果，使报告能区分"这是 LLM 觉得显然的"与"这是算出来的"。缺 `derives_from` 的裸 `LlmProposal` 不得进入报告的数值槽位。

### 15.5 模板策略：Jinja2 vs Rust 模板

v0.1 §9.3 规定模板为 `report/templates/*.md.j2`（Jinja2）。**建议：改为 Rust 侧渲染，采用 Jinja2 语法兼容的实现（`minijinja`）。** 理由：

| 维度 | Jinja2（Python 进程） | Rust 模板（`minijinja`，Jinja2 语法兼容） |
|---|---|---|
| 进程边界 | 每次渲染多一个进程；模板错误发生在另一进程，栈信息不可用 | 同进程；错误可被类型化并 fail closed |
| provenance 强制的可行性 | 需把所有 provenance 数据序列化过进程边界，且渲染侧的"未解析引用"必须回传错误 —— 往返复杂 | 渲染前调用 `ProvenanceResolver::resolve_all` 天然在原位；未解析即 `Err`（15.4）" |
| 数字不落地为字符串的保证 | 数字经 JSON 变字符串，模板内可做运算（`{{ x * 1.1 }}`）⇒ 报告出现"模板算的数" | 同风险存在，但可用严格模式禁用算术过滤器并在 CI 中 lint 模板，使"模板只做插值"可强制 |
| 资产兼容 | 现有 `.md.j2` 直接可用 | `minijinja` 目标是 Jinja2 语法兼容，`.j2` 资产基本可直接复用；差异处（如自定义 filter）需适配 `[推断]` |
| 依赖 | 需 CPython + Jinja2（**许可未在本轮简报中核实**） | 需 `minijinja`（简报 02 第 D.9 项列为可选模板引擎；**其许可亦未在本轮简报核实**） |

推荐结论：**Rust 侧 `minijinja`（或手写 `{{var}}` 替换 + 段组装）**，并把"模板只做插值与条件、不做算术"写成评审规则 + CI lint。**两项依赖的许可均需在 W1 核对**（列入 v0.2 revision item 的许可清单）。

模板分层与剧本的对应（13.3 `report.template`）：

```
report/templates/
├── base.md.j2                  # 通用骨架（四层顺序、免责声明、图表占位）
├── pricing_response.md.j2
├── competitor_intel.md.j2
├── negotiation.md.j2
├── market_entry.md.j2
└── partials/
    ├── boss_page.md.j2
    ├── scenario.md.j2
    ├── profile_evidence.md.j2
    ├── technical_appendix.md.j2
    └── disclaimers.md.j2        # 反垄断红线、先验 borrowed 标注、证据等级
```

### 15.6 `report_render` 工具与 CLI

```rust
// shangyi-report/src/render.rs
#[derive(Clone, Debug, Deserialize)]
pub struct ReportRenderArgs {
    pub case_id: String,
    pub format: ReportFormat,           // Pdf | Docx | Md
    pub layers: Option<Vec<Layer>>,     // 缺省 = 剧本 report.layers
    pub template_override: Option<String>,
}
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReportFormat { Pdf, Docx, Md }

pub struct ReportRenderer {
    projections: Arc<ProjectionStore>,
    provenance: Arc<ProvenanceResolver>,
    figures: Arc<FigurePipeline>,       // 造 spec + 调侧车渲染
    templates: Arc<TemplateSet>,
    pandoc: Arc<PandocProcess>,         // 独立进程；超时与版本记录
}

impl ReportRenderer {
    pub async fn render(&self, args: ReportRenderArgs, cx: &TurnCx)
        -> Result<ReportArtifact, ReportError> {
        // 0) 门控：checkpoint ③ 必须已确认（11.7），否则本工具恒 deny
        cx.require_checkpoint_confirmed(CheckpointKind::ResultPreview)?;
        // 1) 从 projection 取结构化内容（不读 LLM 文本）
        let c = self.projections.snapshot_all(&args.case_id)?;
        // 2) 造图规格 → 侧车渲染（失败 ⇒ 记警告并继续，数字仍在）
        let specs = self.figures.build_specs(&c)?;
        let rendered = self.figures.render_all(&specs, cx).await?;
        // 3) 解析 provenance（失败 ⇒ fail closed）
        let prov = self.provenance.resolve_all(&c.all_provenance_ids())?;
        // 4) 模板渲染 markdown
        let md = self.templates.render(&args, &c, &rendered, &prov)?;
        // 5) 落盘 + pandoc（仅 pdf/docx）
        let artifact = self.write_artifacts(&args, md, &rendered, &prov).await?;
        ctx.log.append(ReportRenderedEvent { artifact: artifact.summary() })?;
        Ok(artifact)
    }
}

impl ToolExecutor<ToolInvocation> for ReportRenderTool { /* 见 9.3 表 */ }
```

**CLI**（沿用 v0.1 §10 的命令面，改名与扩展）：

| 命令 | 作用 |
|---|---|
| `shangyi report <case> --format pdf\|docx\|md` | 生成报告（可指定单层） |
| `shangyi report <case> --figures-only` | 只重渲染图（用已存规格，不重跑咨询） |
| `shangyi report <case> --verify-provenance` | 只做 provenance 解析校验，不生成文件（CI 用） |
| `shangyi debug dump-config --playbook <id>` | 见 13.5 |
| `shangyi debug dump-default-config` | 只看 bundle 默认组合（定位"这行是谁加的"） |

`--verify-provenance` 是 CI 的守门命令：它使"报告里有一个数字无法溯源"成为**构建失败**，而不是评审时被偶然发现。

### 15.7 报告产物目录布局

```
cases/<case_id>/
├── manifest.json                     # case 级索引：playbook_id、compose_generation、rows_hash、
│                                     #   三个 checkpoint 的 outcome、报告版本、发布决定
├── session/
│   └── rollout-2026-09-17.jsonl      # 事件日志（append-only，压缩不改它）
├── projections/
│   └── cache.sqlite                  # dsh 式投影缓存（fold shortcut，永不权威；见简报 02 第 D.5 项）
├── artifacts/
│   ├── evidence-ledger.jsonl         # 证据台账（append-only，受保护）
│   ├── game-spec.v3.yml              # GameSpec 每个已确认版本
│   ├── equilibria.json               # EquilibriumSet
│   ├── selection.json                # 分布 + SelectorScore[]（含 evidence）
│   ├── consequence.json              # ConsequenceReport（whatif）
│   ├── discovery.json                # DiscoveryReport（discover）
│   ├── figure-specs/
│   │   ├── F5-eq-distribution-bar.json
│   │   └── …（十项）
│   └── figures/
│       ├── F5-eq-distribution-bar.png
│       └── …（渲染产物；可重建，故非真相源）
├── report/
│   ├── report.md                     # 模板渲染的中间产物
│   ├── report.pdf
│   ├── report.docx
│   └── provenance.json               # 全部 ProvenanceRecord（15.4）
└── intel/
    ├── raw/                          # 抓取正文原样落盘（带 source_hash）
    └── ingested/                     # data_ingest 解析产物
```

两个纪律：① `session/`、`artifacts/evidence-ledger.jsonl`、`report/provenance.json` 是**真相源或凭据**，其余（`figures/`、`report.md` 之外的渲染物）是**可重建产物**；② `projections/cache.sqlite` 按 dsh 的明文纪律 —— "**The cache is a fold shortcut, never an authority**: a row is possibly stale (its `seq` says how stale) but never wrong" —— 版本不匹配即丢弃该行并重建，绝不迁移（简报 02 第 D.5 项）。

---

## 16. 实施里程碑

相对 v0.1 §11.3 的六周计划，本章的重排依据是三条：① 事件日志是一切的地基，必须最先完成；② Chat 适配层是模型能否接通的**阻塞项**，与地基并行；③ 求解器自研是最大风险项，需要独立的差分测试通道。

### 16.1 里程碑表

| 周 | 交付 | 关键验收 |
|---|---|---|
| **W1–2** | ① `vendor/codex/codex-rs` 固定 commit 引入并裁剪（移除 Code Mode/V8、`login`/`chatgpt`/`keyring-store`/`backend-client`/`analytics`/`cloud-tasks`）；② `cargo-deny` 许可门接入 CI（白名单见 §4.5，无 GPL）；③ **会话事件日志 + surface + turn/step**（§7.1、§8）；④ `shangyi-core` 骨架与 `agent/pre-step`、`agent/request` 钩子挂载；⑤ **Chat Completions 适配器**（§10.2，并行）；⑥ `NOTICE` 与变更声明落地 | 事件日志重放一致性测试通过（同一 log → 同一 surface → 同一 messages）；崩溃恢复补 `interrupted` 收尾通过；GLM 端点至少打通一次工具调用往返 |
| **W3** | ① `shangyi-solver`：支持枚举（n≤4）、Lemke–Howson（2 人）、扩展式逆向归纳、颤抖手/序贯精炼；② `shangyi-selection`：`risk_dom`/`stochastic`/`behavioral`/`focal_llm` + 融合；③ `nashpy` CI 差分测试通道；④ `shangyi-consequence`（whatif 管线） | 教科书博弈求解单测（囚徒/性别战/斗鸡/Cournot 数值）；选择器在文献有定论的博弈上方向正确；差分测试在随机博弈族上无差异 |
| **W4** | ① playbook 加载器与配置分层（整行替换，§13）；② `pricing_response` 剧本端到端；③ 领域工具集（§9.3）；④ 三检查点落在审批 seam + 结构化提问（§11.7）；⑤ `shangyi-swarm` 最小实现（每对手一个 persona subagent + orchestrator 单轮 rollout + feedback 环 + novel-move 三重过滤）；⑥ **fail-closed 沙箱测试** | 沙箱无应答者时审批结果为 `Unavailable` 且按拒绝执行；persona 能力预检对不支持的标志报类型化错误且不静默降级；剧本可 `--dump-config` 检视 |
| **W5** | ① `shangyi-report`：Rust 出图规格 + Python 侧车渲染 + pandoc 管线（§15）；② 案例库与 `backtest` 命令；③ projections 持久缓存 | 每条报告数字可溯源到事件 seq 或数据文件（provenance 链完整）；图表可由规格重建 |
| **W6** | ① 2–3 个端到端案例（真实/半真实）；② 评审打磨与演示脚本；③ 合规收尾（NOTICE 随分发、变更声明齐全、商标改名核对） | 端到端案例通过专家评分表；分发物含完整归属声明 |

### 16.2 质量评估（golden cases）

沿用 v0.1 §11.3 的四项，并新增四项与 Rust 化直接相关的门：

| # | 评估项 | 通过标准 |
|---|---|---|
| ① | 求解器正确性单测 | 对已知均衡的教科书博弈逐案通过 |
| ② | 选择器方向性验证 | 在文献有明确结论的博弈上（2×2 协调博弈风险占优、Young 惯例博弈随机稳定）方向正确 |
| ③ | 条件预测 / 满意过滤正确性 | 在构造博弈上验证 whatif 与 discover 的输出符合解析预期 |
| ④ | 端到端 mock 咨询专家评分 | 建模合理性 / 证据质量 / 建议可用性 / 小白可读性 四维评分达标 |
| ⑤ | **非常规反应回归** | 以星宇股份事件（2026-08）为原型构造"弱势方 + 压力传导链"案例，验证开放行动空间能产出"向对手客户/监管投诉"类候选并通过三重过滤 |
| ⑥ | **`nashpy` 差分测试**（新增） | Rust 求解器与 `nashpy`（MIT，进程隔离，仅 CI）在随机博弈族上结果一致；不一致即视为缺陷 |
| ⑦ | **许可门**（新增） | `cargo-deny` 在 CI 通过：依赖树内无 GPL/LGPL/AGPL；白名单外许可即失败（与 Codex 自身的做法一致） |
| ⑧ | **日志不变式测试**（新增） | 事件重放确定性；`turn/end` 的 `interrupted` 收尾是纯函数且可复现；投影缓存版本不匹配时丢弃并重建、绝不迁移 |

### 16.3 工期风险提示

§1.6 的 R2/R13/R14 把全部博弈数值从现成库转移到自研代码，这是本项目**唯一无法靠"多调用一个工具"消解的风险**。GB 03 §1.8 的估算是在 6 周里为原生均衡算法与其测试预留 **1.5–2 周**，其余时间留给 agent 产品本体。若工期紧张，正确的裁剪顺序是：先砍 `learning` 选择器（R17 已把它推到 v0.2，权重置 0）、再砍扩展式（只保留标准式）、最后砍 2 人以上的支持枚举规模 —— **不要**砍差分测试与许可门，那两项是正确性与合规的底线。

---

## 17. 待拍板决策清单（Review 重点）

| # | 决策点 | 建议 |
|---|---|---|
| **D1** | MVP 首发剧本 | **已定（用户）**：`pricing_response` 领域 + 三模式（scenario / whatif / discover） |
| ~~**D2**~~ | LLM 供应商 | ~~GLM 系为主（国内合规 + 中文），DeepSeek 备选；**前置条件**：须先完成 Chat Completions 适配层（§10.2）——这是本方案的阻塞项，不是可选项~~<br>**✅ 已定（用户，2026-09-18）：`deepseek-flash`**。Chat 适配层已完成并实测通过（GLM 可随时切回 `SHANGYI_PROVIDER=glm`）。选型依据见 `.shangyi-loop/runs/iter21.md` 的用量实测 |
| **D3** | embedding | 本地 BGE（MIT，可商用；经 Python 侧车调用）优先；如省事可用 API。索引是派生投影，可随模型换代重建（§13.8） |
| **D4** | 比赛项目名 | 申报用纯中文"商奕——基于博弈论的企业决策咨询智能体"（防英文字符校验失败且提交后不可改） |
| **D5** | PDF 引擎 | pandoc + 中文字体（docx 同管线）。**许可说明**：以独立进程调用 pandoc 不使商奕成为衍生作品；若随分发物再分发 pandoc，则仅对 pandoc 自身承担 GPL 义务（§15.1） |
| **D6** | 搜索后端 | 开发默认 `ddgs`（MIT，免费无 key）；生产经 SearchProvider 抽象切博查（中文质量）/Tavily/Brave/自托管 SearXNG，多后端链式回退。沿用 v0.1 D6。<br>**2026-10 复盘与反转**：自托管 SearXNG **已删除**（`tools/searxng-up.sh`、`SHANGYI_SEARXNG_URL`、`parse_searxng` 一并移除）——它把"维护一个实例"的成本转嫁给每个用户，而实测检索质量与可用性都不值得。现行实现是**博查单后端 + 融合层保留**（`search_rank::fuse` 仍在，接第二个后端无需改重排）；缺后端时如实报「未取得」 |
| **D7** | **Harness 基座策略** | **建议 fork-and-trim Codex workspace**。理由：`core` 工具注册表为 `pub(crate)`，纯经 app-server + MCP 的路径无法注册原生领域工具，且同样受 Responses-only 限制；fork 是唯一能同时拿到原生工具面与 Chat 适配的路径。代价是必须跟随上游变更（见 D8） |
| **D8** | **Rust 依赖固定策略** | vendor 固定 commit（上游 `version = "0.0.0"`、重命名频繁，如 `ConversationManager` → `ThreadManager`）；提交 `Cargo.lock`；**每个补丁单独文档化**（改了什么、为什么、上游对应位置）；每季度评估一次 rebase |
| **D9** | **Python 侧车的发行方式** | 随包内置固定版本解释器与依赖 + 进程隔离调用，避免依赖用户环境；同时评估中期用纯 Rust 绘图（如 `plotters`）替掉 `matplotlib`，进一步缩小侧车面积 |
| **D10** | **许可合规复核** | 建议在提交前请学校技术转移办公室或法务复核三件事：Apache-2.0 四项义务的落地、`Codex`/`OpenAI` 商标改名是否彻底、Gambit 相关表述是否已完全移除 |
| **D11** | **是否公开 harness 改造补丁** | 建议公开 `vendor/` 补丁集（不含领域逻辑）。Apache-2.0 不要求公开修改，但公开补丁有利于评审叙事与生态互信，且成本极低 |

> D7–D11 为本文新引入的决策项，需与 v0.2 文档 §13 的 D7–D10 对齐后一并拍板。

---

## 附录 A：未取得与需核实项清单

以下事项在**首轮调研**中未能确证，不得作为已确认事实引用。**自动化推进循环（`docs/商奕-自动化推进循环-v0.1.md`）迭代 1–2 已结算多项**，结算状态逐条标注如下。

| # | 事项 | 现状 | 影响 |
|---|---|---|---|
| A1 | `codex-core` / `codex-protocol` / `codex-tools` / `codex-api` 在 crates.io 的发布状态 | ✅ **已结算**：四者中三者 **404**（不存在）；唯一存在的 `codex-protocol` 是**第三方镜像 fork** 的陈旧快照（0.63.0，非 OpenAI 发布）。详见 `research/harness/05-codex-crate-publish-status.md` | 决定"当库引用"还是"vendor workspace" → **确定为整 workspace vendor**（§6.1 判断不变） |
| A2 | Codex 配置层的**合并语义**（深合并 or 整行替换） | ✅ **已结算**：**表递归深合并 + 非表（标量/数组）整体替换**；证据见 §1.6 R12 与 `research/harness/04-codex-config-semantics.md` | ~~W1 第一个审计项~~ → 已闭环；处理方式为 §13.4.1 例外清单 + 完整重述，**不改**为整行替换 |
| A3 | `graphviz` 许可 | ✅ **已结算**：crates.io 上**不存在**名为 `graphviz` 的 crate；**Graphviz C 库为 `EPL-2.0`**（非首轮假设的 EPL-1.0），**出局**。`graphviz-rust` 元数据为 `non-standard`（会挂 `cargo-deny`）而 LICENSE 文件为 MIT —— 属元数据/License 不一致陷阱 | §15 博弈树渲染须改用许可干净方案（详见 `research/harness/06-dependency-licenses.md`） |
| A4 | `minijinja` / Jinja2 许可 | ✅ **已结算**：`minijinja` = **Apache-2.0**（上游 LICENSE 已读）；Jinja2 = **BSD-3-Clause**（PyPI classifier；`license_expression` 为 null，证据等级为 classifier）。两者均**在白名单内** | §15.5 模板策略两者皆可，选型回到工程偏好 |
| A5 | `starlark` crate 许可 | ✅ **已结算**：**Apache-2.0**（上游 LICENSE 已读） | §11.4 `execpolicy` 可保留 |
| A6 | `rand` 最新版本线 | ✅ **已结算**：**最新稳定版 `0.10.2`**。首轮困惑的根因已定位：crates.io `newest_version` 字段按**发布日期**而非 semver 排序（正确字段为 `max_stable_version`），`0.8.8` 是发布更晚的旧线回填补丁 | 固定版本前用 `max_stable_version` 而非 `newest_version`（对**所有**依赖版本核查均适用，见 D8） |
| A7 | HiGHS 的 C++ 上游许可 | ✅ **已结算**：Rust 绑定 `highs` = **MIT**；C++ 上游按产物结论为 **MIT**（绑定侧已由上游 LICENSE 确认，C++ 侧证据等级较弱，采用前再扫一次 SBOM） | `highs` 可用；`microlp`（Apache-2.0，纯 Rust）仍为无 C 依赖备选 |
| A8 | Codex 与 Claude Code 的 `SKILL.md` frontmatter 键名是否逐字兼容 | ✅ **已结算**（工作项 `A8`，独立验证者三重核实）：**兼容**。双方逐字共同消费 `name` + `description`（`metadata` 双方都接受，语义不同不冲突）；Codex **不**拒绝未知字段（`skills/` crate 内 `deny_unknown_fields` 出现 **0 次**）；Codex 侧 `description` **必填**（缺则丢卡）。**唯一硬约束来自 Claude 的分发路径**（claude.ai / Skills API / `package_skill.py`）：只接受 `name`/`description`/`license`/`compatibility`/`metadata`/`allowed-tools` 六键，表外键**硬错误整卡拒绝**（本地加载则容忍）。详见 `research/harness/07-skill-frontmatter-compat.md` | 已落地到 §13.7.1（含键集约束与两处勘误）；卡作者须区分"本地可用"与"可分发" |
| A9 | Rust 相对 Python 的具体加速比 | ⏳ **待实测**（W3 回填） | §5.6.2 计算量口径 |
| A10 | 国内各端点的 wire 契约细节 | ⚠️ **partial**：Chat 适配规格已产出（`research/harness/08-chat-wire-spec.md`，1110 行）；但**"Chat 适配层是唯一路径"的前提已被削弱** —— 智谱官方文档明确要求 `wire_api` 必须为 `responses`，即 GLM 按 Codex 的 **Responses** 协议对接。**DeepSeek 侧已由 `A10b` 实测（Responses 端点存在且工具调用/流式/联合 schema 均可用）**；**GLM 侧仍未实测**（`A10d` 无 key，blocked-on-human） | 见下方 §A.1；D-A5' 已由人类拍板"走 Responses、不写 Chat 层"，但**该结论目前只对 DeepSeek 有实测支撑** |
| A11 | 带 `tools` 时 `reasoning_content` 是否必须回传（否则 400） | ❌ **已推翻（原断言不成立）**：`A10b` 三向对照实测（完整回灌 / 省略 reasoning / 剥掉 `encrypted_content`）**全部返回 HTTP 200**，无一报 400。**结论：在 DeepSeek Responses 端点上省略 reasoning 回传不会失败**，§10.2 的"不回灌"表述**可以保留**（见 §10.2 的实测注） | ~~§14 剪枝器需新增不可压缩约束~~ → **不需要**；GLM 侧未测，W1 落地时在实用模型组合下复测一次 |

### A.1 ⚠️ 迭代 1 引入的选型变化：D-A5 的前提被削弱

智谱官方 Coding Plan 文档（`https://docs.bigmodel.cn/cn/coding-plan/tool/codex`，已独立抓取核实）原文要求 **`wire_api` 必须为 `responses`**，并给出 `model_catalog_json` 指向 `~/.codex/models.json`、`experimental_bearer_token` 填 API Key 的完整配置法。

**含义**：GLM 官方就是按 Codex 的 **Responses** 协议对接的，**不是**只能走 Chat Completions。D-A5 从"唯一路径"降为"其中一条路径"。三个选项：

| 选项 | 内容 | 代价 | 现状 |
|---|---|---|---|
| **A** | 走 Responses 路线（GLM 官方姿势），不写 Chat 适配层 | 依赖 GLM Responses 端点工具调用/流式/思维链的完备度（**端点存在 ≠ 全功能可用**） | ✅ **已由人类拍板选定**（决策 `D-A5'`）。**DeepSeek 侧已实测支撑**（`A10b`：端点存在、工具调用、联合 schema、流式均可用；`tool_choice` 只有 `auto`/`none` 可用 —— 见 §10.7.2）；**GLM 侧仍未实测** |
| **B** | 仍写 Chat 适配层 | W1–2 多一项阻塞工作，但兼容面更广 | 未选 |
| **C** | 两者都做，Responses 优先、Chat 兜底 | 工作量最大，容错最好 | 未选 |

**建议**：先做 `A10b` 实测（Responses 端点的工具调用/流式/思维链），**在实测出结果前不宜拍板**。若 Responses 全功能可用，选 A 可显著缩短 W1–2。

> **结账（2026-09-17）**：`A10b` 已完成（`research/harness/12-provider-live-probe.md`），人类已按选项 A 拍板（`D-A5'`：走 Responses、不写 Chat 层）。**上面的"建议"已执行完毕，不再是待办。** 两条残留义务：① **GLM 侧必须做同口径实测**（`A10d`，无 key，blocked-on-human）—— 在它完成前，"选项 A 成立"只对 DeepSeek 有证据；② `tool_choice = required` 的不可用性需在能力层做 provider 差异化（§10.7.2）。

---

## 附录 B：工程简报索引

本文的技术事实来自以下工作底稿，存于 `research/harness/`。简报为**调研产物**，其中标注了每条事实是"读取所得"还是 `[推断]`；本文引用时保留了该标注。

| 编号 | 文件 | 内容 |
|---|---|---|
| GB 01 | `research/harness/01-codex-rust-harness.md` | Codex（Apache-2.0，Rust）workspace 分层、agent 核心与 turn/step、protocol 与 app-server 嵌入面、工具系统、模型接入（含 Responses-only 限制）、rollout 持久化、沙箱与审批（含 macOS Seatbelt）、插件/skills/hooks/memories、多智能体、prompt 与上下文、复用难度与嵌入路径、许可与义务 |
| GB 02 | `research/harness/02-dsh-patterns-and-attribution.md` | DeepSeek Harness（MIT，TypeScript）署名与许可事实、241 包按角色分组、两条贯穿性不变量、14 个可移植模式（含 Rust 落地建议）、不应移植的模式、建议移植顺序、crate 汇总、建议署名行 |
| GB 03 | `research/harness/03-rust-solvers-and-licensing.md` | Rust 博弈求解 crate 调研（含禁用清单）、算法支持现状、Python 求解器许可评估（Gambit GPL 风险与进程边界分析）、Rust↔Python 互操作对比、对现有设计的直接影响、建议 crate 清单 |
| GB 07 | `research/harness/07-skill-frontmatter-compat.md` | `SKILL.md` frontmatter 跨工具兼容性（A8）：Codex 解析器实际接受的键、`deny_unknown_fields` 的仓内分布、Claude Code / Agent Skills 键集、Claude **分发路径**六键白名单（硬错误）、dsh 驼峰拼写差异；§13.7.1 的全部依据 |
| GB 11 | `research/harness/11-vendor-plan.md` | vendor 处置执行表（W1）：149 成员逐项分类、Code Mode/V8 的传递收缩分析、补丁拆分与行号、12 项 `[未取得]`（U1–U12）、`ToolExposure` 保留建议（§2.6 末注） |
| GB 12 | `research/harness/12-provider-live-probe.md` | **DeepSeek Responses 端点实测（A10b）**：端点路径表、工具调用、**工具 schema 联合类型支持（V2 关闭证据）**、流式 SSE、**`tool_choice` 仅 `auto`/`none`**、reasoning 回灌三向对照（推翻 A10 的 400 断言）、7 项未测（U1–U7） |
| GB 14 | `research/harness/14-w1-vendor-landing.md` | Phase B 首日落盘：上游 SHA 钉版（U7 闭环）、AppleDouble 污染事故、工具链统一（W1d）、首次编译验证；**其 §2 的 `vendor/codex-rs/` 扁平化结构已被回退**，实际布局见 `vendor/VendorPatches.md` 与 §6.1 的"vendor 布局"注 |

---

## 附录 C：署名与许可声明

### C.1 上游署名（必须随文档与分发物保留）

**OpenAI Codex**

> 商奕 harness 基座的部分层直接复用或修改自 **OpenAI Codex**（`https://github.com/openai/codex`），依 **Apache-2.0** 许可，Copyright 2025 OpenAI。按 Apache-2.0 §4：保留 `LICENSE` 与 `NOTICE`（或在分发物中提供 `NOTICE`）；被修改的文件须附**显著变更声明**；向接收方提供许可副本；若含 `NOTICE`，衍生分发须复现其内容。按 §6，`Codex`、`OpenAI`、`ChatGPT`、`GPT-5/6` 均为 OpenAI 商标，**不得用于商奕的产品名、二进制名或暗示背书**。
>
> Codex 自身的 `NOTICE` 还包含对 **Ratatui**（MIT，Copyright (c) 2016-2022 Florian Dehau；2023-2025 The Ratatui Developers）的归属声明。**该声明必须随分发物保留，与商奕是否保留 `tui` crate 无关**：Apache-2.0 §4(d) 的义务指向"被分发作品的 `NOTICE` 内容"，而非商奕是否仍在使用相关代码；裁剪 `tui`（或 `ansi-escape` / `ratatui` 依赖链）时**不得**删除该段（§4.6（1））。**行数口径**：`vendor/codex/NOTICE` 全文 **6 行**（242 B），其中 Ratatui 归属段是 **3 行正文**（第 4–6 行）；旧文此处写"这 4 行"与 §4.6 的"6 行"打架，现已统一（详见 §4.6（1）的"行数口径"与"逐字节"注）。

**DeepSeek Harness (dsh)**

> 本文档中的设计模式改编自 **DeepSeek Harness (dsh)**，© 2026 DeepSeek，依 **MIT** 许可（`LICENSE`）发布；CLI `@deepseek-ai/dsh` 0.1.5-rc.1，插件库 0.1.5-rc.2，构建于 vendored MIT 许可的 **Cordis** 4.0.2 内核之上 —— `https://github.com/deepseek-ai/deepseek-harness`。其中两个预编译原生沙箱/flock 插件（`@deepseek-ai/node-addon-system`、`@deepseek-ai/node-addon-system-darwin-arm64`，v0.1.2）版权归 © 2026 node-addon-landlock-run contributors，依 **BSD-3-Clause** 而非 MIT 许可。DeepSeek Harness 属**开发者预览**软件，其核心插件与 API 将持续演进；本文引用的所有包名、事件名与配置键均为带版本与时点的观察，**不构成稳定契约**。
>
> 附注一处上游不一致：`node-addon-system` 的 `package.json` 声明 BSD-3-Clause，但其捆绑 `LICENSE` 文件内容为 MIT 文本 —— 以 SPDX `license` 字段为准作署名。

### C.2 商奕分发物的 `NOTICE` 骨架

```
商奕 ShangYi
Copyright 2026 <商奕团队>

本产品包含源自 OpenAI Codex 的代码。
OpenAI Codex
Copyright 2025 OpenAI
Licensed under the Apache License, Version 2.0.
原始项目：https://github.com/openai/codex
本产品对上述源码作出修改；被修改文件附有变更声明。

本产品包含源自 Ratatui 的代码（经 Codex 传递）。
Copyright (c) 2016-2022 Florian Dehau
Copyright (c) 2023-2025 The Ratatui Developers
Licensed under the MIT License.

本产品的运行时设计借鉴（未包含其源码）DeepSeek Harness 的设计模式。
DeepSeek Harness
Copyright (c) 2026 DeepSeek
Licensed under the MIT License.
原始项目：https://github.com/deepseek-ai/deepseek-harness

完整依赖许可清单见 THIRD_PARTY_LICENSES.md（由 cargo-deny 生成）。
```

> **⚠️ 本骨架的定位（不要误用）**：这是**面向中文读者的转述骨架**，**不是** Apache-2.0 §4(d) 要求的"复现文本"。§4(d) 要求逐字（严格讲是**逐字节**）复现上游 `NOTICE`，其唯一真值来自 `vendor/codex/NOTICE`（242 B）；分发物中必须包含的是 §4.7 骨架里的**英文** `--- Start/End of upstream NOTICE ---` 块（已核验与上游逐字节相等）。**本节的"本产品包含源自 Ratatui 的代码（经 Codex 传递）"措辞在 `tui` 移除后并不准确**（应改为"历史归属声明"），登记为附录 D 清单第 8 条。
>
> **`Copyright 2025 OpenAI` 在中文骨架里不必带 U+00A0**：上游第 2 行用的是不换行空格（U+00A0），中文转述无法体现该细节 —— 这正是为什么**必须**用英文块做合规文本，而不能用本节的中文骨架。

### C.3 依赖许可白名单（CI 强制）

允许：`Apache-2.0`、`Apache-2.0 WITH LLVM-exception`、`BSD-2-Clause`、`BSD-3-Clause`、`BSL-1.0`、`CC0-1.0`、`CDLA-Permissive-2.0`、`ISC`、`MIT`、`MIT-0`、`MPL-2.0`、`OpenSSL`、`Unicode-3.0`、`Unlicense`、`Zlib`。

**禁用（许可原因）**：`GPL-2.0-or-later`（`pygambit`/Gambit —— 本方案已移除）、`AGPL-3.0-only`（`lrslib-rs` 及其上游绑定）、`EPL-2.0`（`coin_cbc` 传递依赖 CoinOR CBC）、`non-standard`（`amfiteatr*`）。

**禁用（成熟度原因）**：`nash`、`neumann`（均已废弃）；`game-theory-*`、`extensive-form`、`lau-*`（近月内单次发布、未经验证）。

完整清单与逐项理由见 §4.4、§4.5 与 GB 03。

---

## 附录 D：已知文档内矛盾清单（待跟进）

**为什么有这个附录**：2026-09-17 的 `W1b-absorb-vendor-deviations` 修订在收敛 §6.2/§4 时，**发现了 8 处超出其编辑边界的矛盾，但刻意没有改**（"报告而不静默编辑"的纪律 —— 强行半改会制造新的自相矛盾）。问题在于该次报告**没有落盘**，这些矛盾只存在于一次性的验证报告里。本附录把它们持久化为可勾选清单，使它们**可被跟进而不是被遗忘**。

**使用方式**：每结算一条，把状态改为 ✅ 并注明结算 commit/工作项。**在结算前，读到本清单点名的章节时，不得把它当作已决口径引用。** 状态列中标记"本轮已消除"的行属于**已完成**（列在此处仅为留痕）。

| # | 位置 | 矛盾内容 | 影响 | 相关文件 |
|---|---|---|---|---|
| 1 | §3.4（`移除 Code Mode / V8（D-A4a）` 表，"移除对象"行） | 仍写旧口径"以及构建侧的 `patches/rusty_v8_*`"，未同步为 **6 个具体 V8 补丁 + `third_party/v8/` 整目录 + `MODULE.bazel` 的 V8 段**（§6.2 C 表已更新） | 低（执行面以 §6.2 为准，但读者可能按 §3.4 只删 `rusty_v8_*` 两个文件，漏删 4 个） | `docs/商奕-Agent-Harness设计文档-v0.1.md` |
| 2 | §5.1（平面 C 架构图两行）与 §6.3（`shangyi-cli` 行） | 仍引用 `codex-tui`（"vendor `codex-tui` 裁剪"、"（可选）`codex_tui`"），而 vendor 计划 §1.3/§1.7 已把 **`tui` 判为移除**，且 **Phase B 已实际删除 `tui` 目录**（`members` 中不再有 `tui`）。§6.2 表 B 脚注已说明该冲突但**未做全文改写** | 中（`shangyi-cli` 的依赖清单指向一个不存在的 crate，照此写会编译失败） | 同上；`research/harness/11-vendor-plan.md` §1.3/§1.7 |
| 3 | §6.2 表 B（`cli` 行，patch 0009） | 表 B 仍保留 `cli` 行（改二进制名等），但 **Phase B 的 Tier 1 裁剪已把 `cli` 从 `members` 删除、目录已删**。`shangyi` 二进制的主机应是自研 `shangyi-cli`，而非改名后的 `codex-cli`；patch 0009（与 `arg0` 同补丁）的对象已不存在 | 中（直接影响 W1 的二进制落地路径与 patch 编号分配） | 同上；`.shangyi-loop/ledger.jsonl` iter6 |
| 3b | §6.2 表 B（`core`/`tools`/`rollout-trace`/`app-server` 四行的 `0003b`/`0003c` 补丁号）与 §6.2 C 表（"补丁拆为 0003a/b/c"） | **Phase B 实测推翻"4 个 crate 源码手术"的预估**：`vendor/VendorPatches.md` 的 `P-0003a` 记录显示 **Code Mode 的客户端层（`code-mode`/`code-mode-protocol`）不依赖 V8**，V8 仅在叶子 crate（`code-mode-runtime`/`code-mode-host`/`v8-poc`）中；删除 V8 运行时**无需改动任何保留 crate 的源码**，故 **`0003b`/`0003c` 不需要执行**。文档 §6.2 的 B 表与 C 表仍按旧预估写"需要" | 中（若照文档执行，会做无谓的 488 处解耦手术，反而扩大 rebase 冲突面） | `vendor/VendorPatches.md` §P-0003a；`research/harness/11-vendor-plan.md` §1.1.2 |
| 4 | §9.1（`ToolExposure` / `ToolExposures` 行）、§9.4（暴露面引言）、§3.4（附属影响行） | §9.1 写"删去 `CodeModeOnly`/`DeferredModelOnly`/`Deferred` 三支"，与 **vendor 计划 §2.6 末注的逐字建议相反**（计划：W1 第一阶段**保留全部变体**，只在商奕域工具里恒用 `Direct`；删变体是"改逻辑"，rebase 冲突面大）。本轮已在三处加了冲突标注，但**未按计划改判** | 中（若照 §9.1 执行，会在 `router.rs`/`registry.rs`/`spec_plan.rs` 制造大面积 patch） | `research/harness/11-vendor-plan.md` §2.6 末注 |
| 5 | §8.3（"与 Codex 的对应关系"表，`$SHANGYI_HOME` 行） | 仍写 `修改后复用` `codex-home`（patch 0005，D-A15），与 §6.2 表 B（`utils/home-dir` 才是 `$SHANGYI_HOME` 的真实落点、`codex-home` 仅兼容层）及决策表 D-A15 行冲突 | 低（口径不一致；patch 0005 的对象与范围会被读错） | 同上 |
| 6 | §6.1（目录树，`ansi-escape/` 行） | 旧文把 `ansi-escape` 标为 `直接复用`，与 §6.2 D 段"**24 项待定**"（`ansi-escape` 在列）冲突 → **本附录所在轮次已改为 `待定` 口径**（见 §6.1） | 已消除（此条保留为记录，说明待定集与树的一致性现已成立） | 同上 |
| 7 | §11.2（平台实现表，Windows 行） | 旧文把 `windows-sandbox-service` 标为 `修改后复用`，与 §6.2 D 段"**24 项待定**"（该 crate 在列）冲突 → **本附录所在轮次已改为待定口径**（见 §11.2） | 已消除（同上） | 同上 |
| 8 | §4.7（`NOTICE` 骨架）与附录 C.2（`NOTICE` 骨架的中文版） | §4.7 骨架里那句 `The terminal rendering used by this product derives from ratatui …` 与附录 C.2 的"本产品包含源自 Ratatui 的代码（经 Codex 传递）"都**预设了 `tui` 仍被保留**；`tui` 移除后，这两句的"本产品使用/包含"措辞会变成不准确的陈述（正确的形态是"历史归属声明：上游 Codex 曾包含 ratatui 派生代码"）。**§4.7 的英文嵌入块已在本轮改为逐字节复现**（合规要求的部分已满足）；**这两句措辞仍未调整** | 低（不违反 §4(d)，但会给出错误的"本产品仍含 ratatui 代码"印象；法务复核项） | `vendor/codex/NOTICE`、`research/harness/11-vendor-plan.md` §1.3 |

> **对账说明（表内 9 行 vs 原报告 8 处）**：原报告 `W1b` 的构成经独立验证者逐字核实为 **6 处已定位 + 2 处报告未提及的域外项 = 8**。逐条映射如下：
>
> | 原报告的 8 处 | 本附录对应行 |
> |---|---|
> | §3.4 V8 构建资产旧写法 | 第 1 行 |
> | §5.1 平面 C 的 `codex-tui` | 第 2 行（与 §6.3 **合并为一行**） |
> | §6.3 `shangyi-cli` 的（可选）`codex_tui` | 第 2 行（同条） |
> | §9.1 `ToolExposure` 与计划 §2.6 末注冲突 | 第 4 行 |
> | §8.3 `$SHANGYI_HOME` 行仍写 `codex-home` | 第 5 行 |
> | 附录 C.2 `NOTICE` 骨架措辞 | 第 8 行 |
> | （报告未提及）§6.1 `ansi-escape` 标 `直接复用` | 第 6 行 |
> | （报告未提及）§11.2 `windows-sandbox-service` 标 `修改后复用` | 第 7 行 |
>
> **行数结论（如实报告差异，不强行凑数）**：
> - 表内 **9 行**。原报告的 8 处映射到其中 **7 行**（§5.1 与 §6.3 因属同一问题而合并），另 **2 行（第 3 行 `cli`、第 3b 行 `0003b`/`0003c`）是本轮新发现**——转写任务书时这两项尚不存在（写于 vendoring 之前/基于彼时预估），Phase B 的 Tier 1 裁剪与 `P-0003a` 实测之后才成立。故**覆盖的不同问题共 10 个**（8 + 2），分布在 9 行里。
> - **第 3 行（`cli`）的归类说明**：任务书点名"§6.3 引用 `codex-tui` 而 vendor 计划移除 `tui`"，`cli` 的删除是同一裁剪波次（Tier 1）的伴生事实，但**是不同的行**（`cli` 在 §6.2 表 B 里有独立的 patch 0009 行），故单列。
>
> **结算状态**：第 6、7 两条（两处"待定被误标为已决"）已在本轮改回待定口径 —— 因为把它们改回待定**不需要**任何连带改写，属本轮编辑范围内的顺手项。**仍需跟进：第 1、2、3、3b、4、5、8 共 7 条。**

---

*本文档与《商奕-系统设计文档-v0.2》互为配套：v0.2 描述"做什么"，本文描述"怎么搭"。两文的口径差异已逐条登记在 §1.6（R1–R19）。第 17 章拍板后进入 W1：先落地事件日志与 Chat 适配层。*

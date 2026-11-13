# 商奕 · 内核由 Codex 改为 DeepSeek Harness —— 变更记录 v0.1

**日期**：2026-11-10
**触发**：项目所有者指令「把我们的内核博弈 agent 由 codex 改为 deepseek harness」
**性质**：架构口径变更（不是模型切换——模型早已是 DeepSeek API）
**取代**：《商奕 Agent Harness 设计文档 v0.1》§3.1 的 **D-A2**（harness 基座 = Codex fork-and-trim）与 **D-A3**（不采用 dsh 运行时，仅借鉴设计）

---

## 0. 一句话

**商奕的 Agent 内核不再以 Codex 源码为基座；改为「DeepSeek Harness（dsh）作 agent 运行时段 + 商奕 Rust 领域引擎经 MCP 暴露」。** `vendor/codex` 整树移除，依赖图归零 codex。

---

## 1. 为什么「改为 dsh」不等于「把 dsh vendor 进来」

这条前提必须先说清，否则会做出无法落地的方案：

| 事实 | 依据 |
|---|---|
| dsh 是 **TypeScript**，源码不能作为 Rust 依赖被 `path`/`cargo` 引用 | `research/harness/02-dsh-patterns-and-attribution.md` 文首："dsh 为 **TypeScript**，故其**源码不能直接用于 Rust**" |
| dsh 是可运行的 CLI + profile/bundle 体系，官方提供 headless 单发模式与机器可读事件流 | 本机实测 `dsh --version` = `0.2.1-rc.2`；`dsh headless --help` 列 `--json`（NDJSON 运行事件）与 `--session-id`（续接持久化会话） |
| dsh 官方提供 MCP 客户端，可将外部进程的工具注册给模型 | `@deepseek-ai/dsh-mcp-client` 的 `README.md`：stdio / streamable-HTTP，工具名形如 `mcp__<serverName>__<tool>` |
| 商奕的领域能力已经是进程内的确定性 Rust 代码，且工具契约齐备 | `shangyi/crates/shangyi-tools/src/lib.rs:193`（**38** 条工具 spec；旧文档写 41，2026-11-10 以源码 + 运行时 `ToolRegistry::standard().len()` 双证更正）、`src/schema.rs`（参数 JSON Schema）、`shangyi/crates/shangyi-core/src/domain_exec.rs:5791`（分派表） |

因此「内核改为 dsh」唯一可落地的形态是：

```
DSH（TypeScript 进程）              商奕（Rust 进程）
agent loop / session / 压缩         ┌─ 阶段机 phase_graph
工具编排 / 权限 / 子代理    ⇄ MCP ⇄  ├─ 38 个领域工具 domain_exec
模型调用 / 事件流                    └─ 求解器 / 选择 / 报告（确定性）
```

**分工原则不变**：数值只由 Rust 确定性代码产生（铁律 1），未确认假设不得进求解（铁律 2）。dsh 负责「编排与对话」，商奕负责「算与判」。

---

## 2. 口径变更（四要素）

| 项 | 旧值 | 新值 |
|---|---|---|
| **内核基座** | `vendor/codex`（OpenAI Codex fork-and-trim，Apache-2.0，© 2025 OpenAI），`shangyi-core` 经 `codex-core-api` 依赖其公开面 | **无 vendor 基座**；内核 = 自研 Rust 引擎 + dsh 运行时段（MIT，© 2026 DeepSeek），两者经 MCP 连接 |
| **Codex 的实际承重** | 文档称复用其 agent loop / session / tools / sandbox / 多智能体 | **实测仅 1 行代码引用**（`shangyi/crates/shangyi-core/src/lib.rs:81` 的架构测试），agent loop 早已是自研 `agent.rs` |
| **许可义务** | Apache-2.0：保留 `LICENSE`/`NOTICE`、被改文件附变更声明、`VendorPatches.md` 逐补丁登记 | **Codex 相关义务随移除而终止**；新增 dsh 的 MIT 归属（署名，非传染），无源码分发 |
| **对外措辞** | 禁止"基于 Codex"，只能说"包含 OpenAI Codex 项目的衍生代码" | 可正面表述"内核运行在 DeepSeek Harness 上"；历史来源仍需一句事实性说明 |

**影响条数（本机实测，2026-11-10）**：

| 指标 | 变更前 |
|---|---|
| `cargo tree -p shangyi-core` 图内 crate | **851**（其中 `codex-*` 91、`rama-*` 16） |
| `Cargo.lock` 中 `^name = "codex` | **92** 处 |
| `vendor/` 体积 | **76 MB**（git 跟踪 **5,664** 个文件） |
| 源码中 codex 引用 | **1 行**（`shangyi-core/src/lib.rs:81`） |
| 构建包装器依赖 vendor workspace | `tools/loop/build.sh`（`cd vendor/codex/codex-rs`） |
| 文档中 "codex" 提及 | Harness 设计文档 **254**、系统设计 v0.2 **41**、shangyi/README **7**、根 Cargo.toml **4**、`check-modified.py` **4**、`build.sh` **3** |

> 变更后的实测数字见 §4.2（由本轮执行者复测后填入；未填者为未核）。

**回滚方式**：
1. vendor 树：备份在 `/tmp/shangyi-vendor-backup`（73 MB，本轮开工前 `rsync` 拷贝）；恢复 = `rsync -a /tmp/shangyi-vendor-backup/ vendor/`。
2. 文本文件：本仓库 git 基线为 `main` 分支最近一次提交（本轮改动均未提交，`GIT_OPTIONAL_LOCKS=0 git diff` 可完整审阅）。
   ⚠️ 本机 git 目录在 workspace 之外（`~/.shangyi-git`），沙箱禁止写该目录，故本轮**不能**用 `git checkout` 回滚；回滚需在放行写权限的环境执行，或按 §2 的手工清单反向操作。
3. dsh profile：删除 `${DSH_HOME:-$HOME/.dsh}/profiles/shangyi` 即回到原状（不影响其它 profile）。

---

## 3. 本轮实施内容（两轮多 agent）

| 轮次 | 工作项 | 归属 | 产物 |
|---|---|---|---|
| R1 | 摘除 codex 基座 | `codex-remover` | 根 `Cargo.toml` / `Cargo.lock` / `shangyi-core` 依赖与架构测试 / `vendor/` 删除 / `tools/loop/` 改造 / 新增 `tools/loop/check-no-harness.py` |
| R1 | dsh profile 与冒烟 | `dsh-bridge` | `dsh/profile/*`、`tools/dsh/{install-profile.sh,smoke.sh,fake_mcp.py}`、`dsh/README.md` |
| R1 | 文档 / 署名 / 口径 | lead | 本文件、`shangyi/README.md`、Harness 设计文档横幅与本表、PPT 架构页 |
| R2 | MCP 服务端 | `mcp-builder` | 新 crate `shangyi/crates/shangyi-mcp/` + `shangyi mcp-serve` 子命令 + 协议测试 |
| R2 | 独立验证 | `verifier` | `research/harness/16-refactor-verdict.md` |
| R2 | 按 verdict 修复 | 按归属 | 逐条处置见本文 **§8「独立审计与修复」** 与 [`research/harness/17-audit-r1-diff.md`](../../research/harness/17-audit-r1-diff.md)（注：`16-refactor-verdict.md` **不含**「修复记录」段，早期表述指向该段属误记） |

---

## 4. 证据

### 4.1 变更前（本机实测）

```
$ cargo tree -p shangyi-core --prefix none | ... 去重
distinct crates in shangyi-core graph: 851
codex crates: 91
rama crates: 16

$ grep -rn "codex_[a-z_]*::\|use codex" --include=*.rs shangyi/
shangyi/crates/shangyi-core/src/lib.rs:81:  let _ = std::any::type_name::<codex_core_api::Config>();

$ grep -c '^name = "codex' Cargo.lock
92

$ du -sh vendor/codex
76M     # git 跟踪 5,664 个文件

$ grep -rn "Command::new" --include=*.rs shangyi/
shangyi/crates/shangyi-core/src/sandbox.rs:399        # seatbelt
shangyi/crates/shangyi-intel/src/sidecar.rs:614       # python sidecar
# → 不存在对 codex 进程的调用；codex 从未在运行时承重
```

### 4.2 变更后（本轮实测，2026-11-10）

```
$ cargo tree -p shangyi-core --prefix none --offline | awk '{print $1}' | sort -u | wc -l
66                                   # 前：851

$ cargo tree -p shangyi-core --prefix none --offline | grep -c '^codex'
0     # 前：91
$ cargo tree -p shangyi-core --prefix none --offline | grep -c '^rama'
0     # 前：16
$ grep -c '^name = "codex' Cargo.lock ; grep -c '^name = "rama' Cargo.lock
0 ; 0
$ grep -c 'openai-oss-forks' Cargo.lock
0
$ grep -c '^\[\[package\]\]' Cargo.lock ; wc -l < Cargo.lock
118 ; 1091                          # 前：1149 ; 13508（R2 新增 shangyi-mcp 后 package 数 +1）
$ grep -c '^source = "git' Cargo.lock
0     # 前：3 处 fork 源
$ du -sh vendor
No such file or directory           # 前：76M / git 跟踪 5,664 文件
$ python3 tools/loop/check-no-harness.py ; echo $?
扫描 17 个文件，符号命中 0 处，vendor/ 不存在 ✓
0
```

**测试（`cargo test --workspace`，删 vendor 前/后各一次）**

```
删 vendor 前（仅改依赖）： passed=1019 failed=3 ignored=7
删 vendor 后：          passed=1019 failed=3 ignored=7     # 失败集合逐项一致 ⇒ 删目录无影响
跳过 3 条 seatbelt live 测试后： 1016 passed, 0 failed, 7 ignored, EXIT=0
R2 补齐 MCP 服务端后（终态）： passed=1044 failed=3 ignored=7（48 个 test binary + 15 组 doc-test = 63 个 test target，总用例 1054）
跳过 3 条 seatbelt 后（终态）： 1041 passed, 0 failed, 7 ignored
```

**3 条失败的定性：环境性，非重构引入（不可算作通过）**

| 证据 | 内容 |
|---|---|
| 触发点 | `sandbox::live_tests::{sandbox_runs_numeric_code, sandbox_enforces_timeout, sandbox_confines_writes_to_workdir}`，stderr 均为 `sandbox-exec: sandbox_apply: Operation not permitted` |
| 文件未改 | `shangyi/crates/shangyi-core/src/sandbox.rs` 与 HEAD 字节相同（`git diff` 为空） |
| 绕开 cargo 的探针 | `/usr/bin/sandbox-exec -p '(version 1)(allow default)' /bin/echo ok` → exit 71，同样 `Operation not permitted` ⇒ **本会话外层沙箱禁止嵌套 seatbelt** |
| 历史旁证 | 同一测试在 2026-10-02（跑在无外层沙箱的 `~/.shangyi-build`）为 `ok`，见 `research/qa/2026-10-02-conditional-analysis/core-cli-frozen.log:1254` |

**已识别的真实缺陷（本轮不修，登记为后续）**：`sandbox.rs` 的 `isolation_available()` 只判
`/usr/bin/sandbox-exec` 是否存在、不探测可用性 ⇒ 在"二进制存在但被外层拒绝"的环境里测试失败而非跳过。
这不是重构引入的，但它会让 `sandbox_exec` 工具在同类环境下"声称有隔离、实际失败"。

**未核**：同环境下的 R0（含 vendor）基线测试数未重建（成本 >15 min / 数 GB）。上表"删前/删后一致"是在同一环境、同一 target 下连跑两次得到的，已足以排除删目录的因果。

### 4.4 真实模型 E2E：**费用/资料类已核，完整八阶段不成立**（R2 计划外达成）

R1/R2 期间一直把"真模型 E2E"列为未核（理由：DeepSeek 余额为负、本 shell 无 key）。R2 的独立验证者发现该前提**已过期**——CLI 从 `~/.shangyi-secrets/deepseek.env` 取 key（不在环境变量里，所以"本 shell 无 `DEEPSEEK_API_KEY`"字面为真）。它据此跑了 **2 次有界真实 E2E**（各 ≤150 s，均 exit 0，凭据未打印/未落盘），证据在 [`research/harness/16-refactor-verdict.md`](../../research/harness/16-refactor-verdict.md) §3：

| 场景 | 结果 |
|---|---|
| **费用测算**（`scope=cost`） | **全链路成功**：`dsh --profile shangyi` → 模型选 `mcp__shangyi__select_task_scope` → `mcp__shangyi__business_cost` → `mcp__shangyi__complete_task`；模型答「需准备现金 380,000 元」，三个数字逐项标 `business_cost` 出处 —— **铁律 1 在真实装配下成立**（模型没有自己算） |
| **完整咨询**（`scope=consultation`） | 走到 `ask_user` **正确停下**，模型如实说"缺两个决定性数字，不能凭常识替你估计"。**没有**假装完成 |

**因此本次验收的准确表述是**：

> 内核的 agent 运行时**已由 Codex 换成 DeepSeek Harness**，并在真模型下走通了「dsh → MCP → Rust 引擎 → 模型据工具数字作答」；
> **费用/资料类任务可用**；**完整八阶段咨询在 dsh 路径上尚不可达**（卡在 checkpoint 答复通道，见 U4），完整咨询仍走 Rust 内核 `shangyi new`。

### 4.3 dsh 侧（本机实测，审阅时版本 0.2.1-rc.2）

```
$ dsh --version
0.2.1-rc.2

$ dsh headless --help
  --json             write newline-delimited run events to stdout instead of the final message
  --session-id <id>  adopt the persisted Session with this id
```

- profile 目录：`~/.dsh/profiles/<name>/{package.json, cordis.yml, cordis.patch.yml}`；`cordis.yml` 为 `[]`，层由 bundles + patch 合成。
- headless bundle = `@deepseek-ai/dsh-base` + `@deepseek-ai/dsh-headless`。
- MCP 客户端：`@deepseek-ai/dsh-mcp-client`（stdio / streamable-HTTP；工具名 `mcp__<serverName>__<tool>`）。

---

## 5. 未核项（必须显式列出）

| # | 未核事项 | 为什么未核 | 如何核 |
|---|---|---|---|
| U1 | **真实模型下的 dsh 驱动 E2E** | **已部分核（R2，见 §4.4）**：费用/资料类走通；完整八阶段仍不可达（U4）。此前"余额为负、无 key"的前提已过期——key 在 `~/.shangyi-secrets/deepseek.env` | 完整咨询需先解决 U4/U10，再按 §4.4 的命令复跑 |
| U2 | dsh 的 developer preview 演进风险 | 官方原话"core plugins and APIs will continue to evolve"（`research/harness/02-…md` §A） | 锁定审阅版本；profile 与 MCP 协议各自可替换，避免耦合到内部包 |
| U3 | `shangyi new` 默认是否已改走 dsh | 本轮先交付 MCP 服务端与 profile；CLI 默认内核切换需要真实 E2E 才能验收 | 见 R2 缺口清单；切换前保持 Rust 内核为默认 |
| **U4** | **checkpoint 的用户答复没有回传通道** | 实测 dsh 0.2.1-rc.2 的 `@modelcontextprotocol/client@2.2.0` 虽支持 MRTR/elicitation，但 `@deepseek-ai/dsh-mcp-client` 里**没有** `inputRequired`/`elicitation` 处理（只有 `isError`/`structuredContent`）。故服务端把 cp1/cp2 提问件交出去后收不到"用户点了确认"，默认路径会停在 `intel` | 需要 dsh 侧支持 elicitation；在此之前无人值守只能用 `--auto-approve-checkpoints`（日志记 `actor=auto`，**不是**真人确认，不满足铁律 2 的原意），或由 Rust 内核 `shangyi resume` 承接确认 |
| **U5** | MCP 侧 `web_search` 未接线 | 唯一检索后端是 CLI 内的博查适配器（`shangyi-cli/src/bocha_search.rs`），`shangyi-mcp` 不能反向依赖 CLI；故该工具返回 `blocked`（"未取得"，非错误） | 把检索后端下沉为库 crate，或在 MCP 进程内注入 HTTP 后端 |
| **U6** | MCP 边界的 JSON Schema 子集降级 | dsh 的 schema 子集只有 `type/oneOf/properties/required/additionalProperties/items/enum/const` + 描述类字段；38 个工具里 **18 个**用了 `minimum/maximum/minItems/maxLength/pattern`。这些关键字在 MCP 边界被省略，并逐条在 description 里注明"已省略…取值合法性仍由执行器在调用时校验"（**不是静默丢字段**） | 若要求零降级，应在 `schema.rs` 里就不使用越界关键字（需领域 owner 决策） |
| **U7** | `whatif` 缺"决策动作 d"的通道 | CLI 有 `--action`，MCP 子命令按约定未加参数；目前只能靠模型把 d 写进 `consequence_predict` 参数 | 下一轮加 `--action` 或把它作为工具参数 |
| **U8** | intake 阶段推进靠"首个非控制类域工具成功"推断 | intake 无产物闸门，故第一次成功调用即推进到 `intel`（intake-only 工具只有一次机会）。已把会话控制类工具排除在推进之外 | 给"推进"一个显式信号（MCP notification 或会话控制工具），不要靠推断 |
| U9 | 工具数口径更正 | 旧文档写 **41** 个工具；2026-11-10 实测 **38**（`specs` 元组 38 条 + `registry.rs` expected 38 项 + 运行时 `len()`=38，三处互证）。本轮已把变更记录/PPT/项目 README 的 41 与源码注释的 24 全部改为 38 | 已完成；若后续新增工具，`registry.rs` 的 expected 列表会先失败 |
| **U10** | **MCP 服务端不支持跨进程续接** | `host.rs` 无条件 `PhaseMachine::new()`，`--home` 只决定落盘位置。实测：同 `--home` 起第二个进程，阶段回到 `intake`、任务范围丢失 ⇒ dsh 的"一次一问 + `--session-id` 续接"无法延续同一咨询 | 需要把 PhaseMachine/DomainState 序列化进 case log 并在启动时恢复；在此之前只支持"一个进程 = 一次会话" |

---

## 6. 与冻结决策 D-A1 / D-A2 / D-A3 的关系

冻结决策只能由人类改判（`.shangyi-loop/DECISIONS.md` 开头的规则）。本记录**不改写历史结论**，只登记事实：

| 决策 | 原文结论 | 现状 |
|---|---|---|
| **D-A1**（核心语言 Rust） | Rust，edition 2024 | **继续成立**。领域引擎、求解器、工具仍是 Rust。变化的是：不再为了"与基座同语言"而选 Rust——理由收窄为「数值确定性 + 沙箱原语」，仍足以成立 |
| **D-A2**（harness 基座 = Codex fork-and-trim） | ① Codex fork-and-trim | **已被取代**。移除理由有两层：(a) 所有者的指令；(b) 实测它从未承重——1 行测试引用 vs 851 crate / 76 MB / 92 锁定项的代价 |
| **D-A3**（不采用 dsh 运行时，仅借鉴设计模式） | 不采用 | **已被取代（方向反转）**。D-A3 的论据"语言不可共享"仍然成立，但它推出的结论是"不能用 dsh 源码"，**不是**"不能用 dsh 运行时"。新的选择是：dsh 以**进程边界**提供运行时，钱（数值）与判（阶段/闸门/留档）留在 Rust |
| **D-A4b**（移除 OpenAI 账号耦合 crate） | 移除 login / keyring / analytics 等 | 因整树移除而**自动达成**，不再需要逐 crate 裁剪 |

**需要人类确认的一句**：D-A2/D-A3 的逆转是否写入 `DECISIONS.md`（该文件规定循环无权修改既有决策，只能追加执行记录）。

---

## 7. 下一步（按优先级）

1. **跑通 U1 的完整八阶段真实 E2E**——这是"内核真的改为 dsh"的验收线。
   折价说明（2026-11-12 复核）：**"缺凭据 / 余额为负"已不再是卡点**。R2 已利用 `~/.shangyi-secrets/deepseek.env` 中的真实凭据跑完**短任务真实 E2E**（费用测算全链路成功、完整咨询正确停在 `ask_user`，见 §4.4）；剩余的卡点是 **U4（checkpoint 答复通道）** 与 **U10（MCP 跨进程续接）**。
   后续轮次接入**本机 Magpie Gateway**（统一模型路由）与**双通道 Checkpoint**（Web `/api/shangyi/checkpoint` + 可信用户账本校验，`actor = "user"`）后，即可推进完整八阶段验证。
   在此之前，对外表述应为「内核已支持 dsh 运行时（MCP 工具桥已就绪，短任务可用），Rust 内核仍为默认」。
2. 把 `shangyi new` 的默认内核切到 dsh（需要 U1 先通过），保留 `--kernel rust` 回退。
3. 复核 dsh profile 对编码类工具的关闭范围（能力面最小化）是否与咨询内核匹配。

---

## 8. 独立审计与修复（R1.5 / R2）

由**独立审计者**（非执行者）对进行中的 diff 做对抗性审计，报告见
[`research/harness/17-audit-r1-diff.md`](../../research/harness/17-audit-r1-diff.md)。结论：**P0 = 0 / P1 = 3 / P2 = 10**；审计窗口内另有 6 项被即时修复并复核。

| 编号 | 问题 | 处置 |
|---|---|---|
| **P1-1** | 删 codex 后 `serde_json/arbitrary_precision` 从全图消失，序列化"硬约束"退化为无人执行——`profiles.rs:509` 仍在计数却拦不住 intern-tagged 回退；4 处代码注释仍写"feature 已启用/已复现" | **选 A：让护栏复牙**——把该 feature 加进**拥有护栏测试的 crate 自己**的 `[dev-dependencies]`（只影响 test profile，不改 release 数字语义），并修 4 处注释为"当前状态 + 为什么保留"。依据：R1 之前（feature 存在时）全量测试本就全过，故这是恢复已验证环境而非引入新语义 |
| **P1-2** | `.shangyi-loop/STATE.md` 把已删除的 `tools/loop/check-modified.py` 登记为生效门禁 | 已改：§4(b) 门**退役**（Apache-2.0 §4(b) 义务随 vendor 移除而终止），现行架构门改为 `tools/loop/check-no-harness.py` + `no_external_harness_in_manifests` 测试 |
| **P1-3** | 未装 `shangyi` 二进制时 dsh **完全静默**启动、零领域工具、无任何 MCP 报错；而 profile 注释声称"日志报错" | 修：发布 profile 的 `failOnStartupError` 默认 `true`（缺二进制硬失败），另留 lenient 变体；`install-profile.sh` 加二进制自检 |
| P2 若干 | README 陈旧测试计数、PPT 页脚文档名、实验 workspace 的 rama 死钉版、`.gitignore`/`deny.toml` 残留、persona 未提 3 个 MCP 资源工具、`smoke.sh` 依赖 `shasum` 等 | 按归属逐一修（低风险项记录不修者见审计报告 §4） |

**审计给这次重构的定性（值得引用）**：所有"硬"链路（构建可解析、依赖归零、锁文件、护栏四用例、dsh 装配 20 项断言）实跑通过；没有发现阻断级缺陷。最大风险不是"改错了"，而是**一个自认为在拦、实际拦不住的测试**——这正是把"删依赖"当成纯删除容易忽略的地方。

---

## 9. 门禁退役登记

| 门 | 处置 | 理由 | 回滚 |
|---|---|---|---|
| `tools/loop/check-modified.py` | **删除**（2026-11-10） | 唯一职责是校验 vendor 与上游快照的差异登记（Apache-2.0 §4(b)）；`vendor/` 已移除，该义务终止 | 从 git 恢复（历史件仍在提交中） |
| `tools/loop/prebuild.sh` | 保留，清污面由 `vendor/` 改为 `shangyi/` + target | 挂载盘的 AppleDouble 污染仍然存在 | — |
| `tools/loop/build.sh` | 改为从项目根构建本 workspace，默认 `CARGO_TARGET_DIR=/tmp/shangyi-target` | 不再有 vendor workspace | — |
| `tools/loop/check-no-harness.py` | **新增**（架构门） | 替代"边界规则靠约定"：依赖声明命中 `codex`/`rama`/`openai-oss-forks` 或 `vendor/` 回归即失败 | — |


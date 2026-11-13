# 商奕（ShangYi）

面向商业与金融决策场景的博弈论智能体系统：基于 **DeepSeek Harness（DSH）** 宿主运行时，以纯 **Rust 确定性博弈引擎**（`shangyi-mcp`）为核心，提供情景推演（`scenario`）、决策后果预测（`whatif`）与满意策略发现（`discover`）能力。

---

## 核心原则

1. **LLM 绝不算数**：大语言模型负责自然语言理解、信息收集编排、形式化建模提案（`GameSpec` / 成本明细）与结果解释；所有向用户报告的数值（纳什均衡、量化反应均衡、防悔下界、敏感度翻转点、期间累计现金流、盈亏平衡点等）均必须来自 `TrustLevel::Engine` 的 Rust 确定性工具输出。
2. **未经确认的假设不得静默进入求解器**：收益矩阵与先验分布必须附带来源标签（`user_confirmed`、`public_data`、`prior_borrowed`、`llm_inferred`），存在缺口或硬假设时须经检查点（`cp1` / `cp2` / `cp3`）由真实用户确认（`user_actor = "user"`）后方可推进。
3. **进程级 MCP 确定性边界**：Rust Workspace（15 个 crate）通过 `shangyi mcp-serve`（JSON-RPC 2.0 over stdio）向 DSH 暴露 38 个领域工具与 MCP Resources（35 篇知识库 + 4 套行业剧本），数值状态机与 `case.jsonl` 追加日志全部保留在纯 Rust 进程内。

---

## 仓库结构

```text
.
├── Cargo.toml                  # Rust Workspace（15 个 crate，Apache-2.0，unsafe_code = "forbid"）
├── deny.toml                   # 依赖许可白名单与禁用清单（cargo-deny）
├── AGENTS.md                   # 开发规范与必过门禁
├── ATTRIBUTION.md              # 第三方组件与运行时归属声明
├── shangyi/
│   ├── crates/                 # 15 个纯 Rust 核心 crate（gamespec/solver/selection/consequence/discover/core/mcp/cli...）
│   ├── kb/                     # 35 篇博弈论与商业分析 Markdown 知识库
│   ├── playbooks/              # 4 套行业 YAML 剧本与门店选址调研规程
│   ├── sidecar/                # Python 数据导入与轻量本地控制台 sidecar
│   └── ui/                     # DSH Web 三栏工作台插件族与 shangyi-icons 印记图标集
├── dsh/
│   ├── profile/                # DSH 命令行无头版 Profile（shangyi）
│   ├── profile-web/            # DSH 三栏 Web 工作台版 Profile（shangyi-web）
│   └── skills/                 # 商奕受控项目级 Skills
├── docs/                       # 系统设计与演进文档（当前权威规范：docs/商奕-系统设计文档-v0.3.md）
└── tools/                      # 构建包装、静态门禁、知识库体检与 DSH 冒烟脚本
```

---

## 快速开始

### 1. 环境要求

- Rust 工具链（`cargo` / `rustc`，edition 2024）
- Node.js `>= 20`（用于 DSH 运行时与工作台插件校验）
- Python `>= 3.11`（用于门禁脚本与 sidecar）
- 可选：`dsh`（`@deepseek-ai/dsh`，用于运行 DeepSeek Harness 宿主与 Web 工作台）

### 2. 编译与运行 Rust 引擎 / CLI

```bash
# 编译商奕 CLI（通过构建包装器默认输出至 /tmp/shangyi-target）
tools/loop/build.sh build --release -p shangyi-cli

# 查看 CLI 用法
/tmp/shangyi-target/release/shangyi --help

# 启动 stdio MCP 服务端（供 DSH 挂载）
/tmp/shangyi-target/release/shangyi mcp-serve
```

### 3. 启动 DSH Web 工作台（`shangyi-web`）

```bash
# 1) 编译发布版商奕二进制
tools/loop/build.sh build --release -p shangyi-cli

# 2) 构建并挂载三件套插件与受控配置到 ~/.dsh/profiles/shangyi-web
bash shangyi/ui/scripts/install-workbench.sh
```

更多 DSH Profile 配置与无头模式用法见 [`dsh/README.md`](dsh/README.md)；引擎与序列化约束见 [`shangyi/README.md`](shangyi/README.md)；系统总体架构见 [`docs/商奕-系统设计文档-v0.3.md`](docs/商奕-系统设计文档-v0.3.md)。

---

## 质量门禁与测试

```bash
# 静态护栏
python3 tools/loop/check-no-harness.py
python3 tools/loop/check-features.py
python3 tools/kb-lint.py shangyi/kb

# Rust 全量测试
tools/loop/build.sh test --workspace

# DSH Web 工作台与宿主数据面自检
node shangyi/ui/dsh-plugin-shangyi-workbench/scripts/verify.mjs
node shangyi/ui/dsh-plugin-shangyi/scripts/verify-host.mjs
node shangyi/ui/dsh-plugin-shangyi/scripts/check-host-data.mjs

# Python sidecar 与搜索工具单元测试
python3 -m unittest discover -s shangyi/sidecar -p 'test_*.py'
python3 -m unittest discover -s tools/deepseek-search -p 'test_*.py'

# sidecar Web 测试
node --test shangyi/sidecar/web/tests/*.test.mjs
```

如本机已安装 `cargo-deny` 与 `dsh`：

```bash
cargo deny check licenses sources
bash tools/dsh/smoke.sh
```

---

## 许可证与第三方声明

- 本项目主体源码与文档采用 [Apache License 2.0](LICENSE)（见 `Cargo.toml`）。
- 第三方运行时与依赖组件归属声明见 [`ATTRIBUTION.md`](ATTRIBUTION.md)。
- `shangyi/ui/shangyi-icons/` 与 `shangyi/sidecar/web/vendor/` 下附带的第三方/独立许可组件分别适用其目录内的 `LICENSE` / `*-LICENSE` 文件。

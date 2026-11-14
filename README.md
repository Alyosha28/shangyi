# 商奕（ShangYi）

面向商业与金融决策场景的博弈论智能体系统：基于 **DeepSeek Harness（DSH）** 宿主运行时，以纯 **Rust 确定性博弈引擎**（`shangyi-mcp`）为核心，提供情景推演（`scenario`）、决策后果预测（`whatif`）与满意策略发现（`discover`）能力。

---

## 核心原则

1. **LLM 绝不算数**：大语言模型负责自然语言理解、信息收集编排、形式化建模提案（`GameSpec` / 成本明细）与结果解释；所有向用户报告的数值（纳什均衡、量化反应均衡、防悔下界、敏感度翻转点、期间累计现金流、盈亏平衡点等）均必须来自 `TrustLevel::Engine` 的 Rust 确定性工具输出。
2. **未经确认的假设不得静默进入求解器**：收益矩阵与先验分布必须附带来源标签（`user_confirmed`、`public_data`、`prior_borrowed`、`llm_inferred`），存在缺口或硬假设时须经检查点（`cp1` / `cp2` / `cp3`）由真实用户确认（`user_actor = "user"`）后方可推进。
3. **进程级 MCP 确定性边界**：Rust Workspace（15 个 crate）通过 `shangyi mcp-serve`（JSON-RPC 2.0 over stdio）向 DSH 暴露 39 个领域工具与 MCP Resources（35 篇知识库 + 4 套行业剧本），数值状态机与 `case.jsonl` 追加日志全部保留在纯 Rust 进程内。

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

### 2. 编译、运行模式与单入口启动

商奕提供两种互补运行模式：
- **DSH 完整 Web 工作台 / DSH 无头对话模式**（推荐完整交互）：`shangyi start`（别名 `shangyi ui`，默认端口 `8791`）需本机 `PATH` 已安装 `@deepseek-ai/dsh`（`dsh`）；
- **纯 Rust 独立 CLI 与轻量 Sidecar**：`shangyi new / resume / solve / report / backtest / kb / checkpoint / mcp-serve` 不依赖 `dsh`；另随附零构建 Python Sidecar Web 控制台 [`shangyi/sidecar/web_console.py`](shangyi/sidecar/web/README.md)。

```bash
# 1) 编译并安装发布版商奕 CLI 到 ~/.shangyi/bin（不污染工作目录），再加入 PATH
tools/loop/build.sh build --release -p shangyi-cli
mkdir -p ~/.shangyi/bin && cp /tmp/shangyi-target/release/shangyi ~/.shangyi/bin/shangyi
export PATH="$HOME/.shangyi/bin:$PATH"

# 2) 按需配置模型与检索/周边 API 凭据（或写入 ~/.shangyi-secrets/*.env）
export MAGPIE_GATEWAY_KEY=...   # 或 DEEPSEEK_API_KEY=...
export BOCHA_API_KEY=...        # 可选：公开网页搜索
export AMAP_KEY=...             # 可选：poi_survey 周边门店计数

# 3) 一条命令初始化 ~/.shangyi/{cases,runtime,kb}、安装/启动 DSH Web 工作台（默认端口 8791）并打开浏览器
shangyi start

# 如仅需无浏览器模式或指定端口：
shangyi start --no-open --port 8791
```

若尚未把工作台插件挂入 DSH，请先执行一次 `bash shangyi/ui/scripts/install-workbench.sh --install-only`（或在 `install-workbench.sh` 后直接用 `shangyi start` 统一拉起）。工作台顶部与对话侧同时接有 `/api/shangyi/health` 健康探测：若缺少 `shangyi` 二进制或 MCP 工具未连接，界面会**强阻断告警并给出修复命令**，绝不会静默退化成纯大模型对话。

### 3. 常用 CLI 子命令（`start` / `new` / `resume` / `solve` / `report` / `backtest` / `kb` / `checkpoint` / `mcp-serve`）

```bash
# 导出确定性决策报告（校验 provenance、隔离 sandbox_exec，拒绝空案例或纯沙箱案例）
# 默认即为 md（直接输出到 stdout）；支持 --output/-o 指定路径，pdf/docx 按需调用本机 pandoc
shangyi report latest
shangyi report <case_id> -o report.md
shangyi report latest --format docx -o report.docx
shangyi report latest --format pdf -o report.pdf   # PDF 需本机 pandoc + xelatex 等排版引擎

# 检索、阅读、列出与追加自定义经验知识条目（合并内置 35 篇 + ~/.shangyi/kb）
shangyi kb list
shangyi kb search "均衡"
shangyi kb show equilibrium-selection
shangyi kb add ./my-experience.md

# 独立无 dsh 咨询与关口确认 / 离线求解 / stdio MCP 服务
shangyi new --mode scenario
shangyi checkpoint latest --approve
shangyi mcp-serve
```

更多 DSH Profile 配置与无头模式用法见 [`dsh/README.md`](dsh/README.md)；独立 Python Sidecar 控制台说明见 [`shangyi/sidecar/web/README.md`](shangyi/sidecar/web/README.md)；引擎与序列化约束见 [`shangyi/README.md`](shangyi/README.md)；系统总体架构见 [`docs/商奕-系统设计文档-v0.3.md`](docs/商奕-系统设计文档-v0.3.md)；M2 遗留审计与架构收敛评估见 [`docs/M2-遗留审计与架构收敛评估.md`](docs/M2-遗留审计与架构收敛评估.md)。

---

## 质量门禁与测试

```bash
# 静态护栏
python3 tools/loop/check-no-harness.py
python3 tools/loop/check-features.py
python3 tools/loop/check-docs-sync.py
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

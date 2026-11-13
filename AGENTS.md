# 商奕（ShangYi）工作区开发与铁律规范（AGENTS.md）

本仓库是 **商奕（ShangYi）** —— 基于 **DeepSeek Harness（DSH）** 宿主、以纯 Rust 确定性博弈引擎（`shangyi-mcp`）为核心的博弈论商业/金融决策 Agent。

## 1. 架构分层与非妥协铁律

1. **铁律一（LLM 绝不算数）**：
   - 大语言模型只负责自然语言理解、信息收集编排、形式化建模提案（`GameSpec` / 费用明细）与结果解释。
   - **任何向用户报告的数值**（纳什均衡、防悔下界、敏感度翻转点、期间累计成本、盈亏平衡点、单价换算）必须逐字引自 `TrustLevel::Engine` 的 Rust 确定性工具输出；前端 UI 与 LLM 均严禁自行重算、口算或用 `0`/`1` 填充未知项。
2. **铁律二（未经确认的假设不得静默进入求解器）**：
   - 所有收益矩阵与先验分布必须附带四类证据来源之一：`user_confirmed`、`public_data`、`prior_borrowed`、`llm_inferred`。
   - 存在证据缺口或未确认假设时，阶段闸门（`cp1`/`cp2`/`cp3`）必须挂起等待真实用户确认（`actor = "user"`），严禁模型伪造自动审批。
3. **无外部旧基座污染**：
   - 严禁引入 `codex`、`rama`、`openai-oss-forks` 或任何自建通用 Agent 循环，由 `python3 tools/loop/check-no-harness.py` 强制门禁。
   - Rust workspace 全局 `unsafe_code = "forbid"`（见 `Cargo.toml`）。
4. **Serde 含浮点枚举标注铁律**：
   - 正式依赖图中严禁 `serde_json/arbitrary_precision`（由 `python3 tools/loop/check-features.py` 门禁）。
   - 任何直接或间接包含 `f64`/`f32` 的 Rust `enum`，**必须**使用相邻标注 `#[serde(tag = "...", content = "...")]` 或默认外部标注，**严禁**使用内部标注 `#[serde(tag = "...")]` 或无标注 `#[serde(untagged)]`（详见 `shangyi/README.md`）。

## 2. 本机工具链与挂载盘构建规范（必读）

本仓库若置于 macOS 外接挂载盘（如挂载卷工作目录），文件系统会生成 `._*` AppleDouble 伴随文件：

- **非登录 Shell 工具链 `PATH`**：
  ```bash
  export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.local/share/fnm/node-versions/v24.20.0/installation/bin:/opt/homebrew/opt/python@3.13/libexec/bin:/opt/homebrew/bin:$HOME/.local/bin:$PATH"
  ```
- **Rust 编译与测试必须走 `tools/loop/build.sh`**：
  - `CARGO_TARGET_DIR` 必须落在本地磁盘（默认 `/tmp/shangyi-target`），**严禁**在仓库内创建 `target/`。
  - 常用命令：`tools/loop/build.sh check --workspace`、`tools/loop/build.sh test --workspace`。
- **禁止无剪枝全盘 `find`**：避免扫描 `.pnpm-store` 与 `.git.mountbak`。

## 3. 核心目录地图

- `shangyi/crates/`：15 个纯 Rust crate
  - `shangyi-gamespec` / `shangyi-solver` / `shangyi-selection` / `shangyi-consequence` / `shangyi-discover` / `shangyi-playbooks`：博弈形式化、精确有理数/LP/EFG/贝叶斯求解、多判据均衡筛选、条件后果预测、满意策略发现与剧本规格
  - `shangyi-core`：8 阶段咨询状态机（`PhaseMachine`）、`DomainExecutor`、`CaseLog`（`case.jsonl` 唯一事件真相源）
  - `shangyi-intel` / `shangyi-kb` / `shangyi-swarm` / `shangyi-llm` / `shangyi-report`：情报抓取/搜索/POI、BM25 知识库、对手画像推演、LLM 客户端与结构化报告模型
  - `shangyi-tools`：工具契约与 `TrustLevel` 注册表
  - `shangyi-mcp`：面向 DSH 的 stdio JSON-RPC 2.0 MCP 服务器（Tools + Resources + 双通道关口同步）
  - `shangyi-cli`：`shangyi` 命令行与 `shangyi mcp-serve` 入口
- `shangyi/kb/`：35 篇博弈论与商业分析 Markdown 知识库（需通过 `python3 tools/kb-lint.py shangyi/kb`）
- `shangyi/playbooks/`：4 套 YAML 行业剧本（`pricing_response`、`market_entry`、`negotiation`、`competitor_intel`）与门店选址专题规程
- `shangyi/ui/`：DSH Web 三栏工作台插件族（宣纸·墨玉东方极简风格）
  - `dsh-plugin-shangyi`：宿主只读投影 + 受控关口确认 API（`/api/shangyi/*`）
  - `dsh-plugin-shangyi-shell`：左对话 · 中工作台 · 右检视器三栏壳层与设计令牌
  - `dsh-plugin-shangyi-workbench`：六模块工作台视图（总览、推演、精算、假设、证据、知识库）
  - `shangyi-icons`：六枚动态印记图标集（`弈`、`算`、`判`、`证`、`知`、`档`）
- `dsh/`：DSH Profile 配置（`dsh/profile` 命令行版、`dsh/profile-web` Web 工作台版）与受控项目级 Skills（`dsh/skills/`）
- `docs/`：系统架构设计文档（以 `docs/商奕-系统设计文档-v0.3.md` 为当前权威规范；v0.1/v0.2 已归档）

## 4. 变更必过门禁

每次完成代码、知识库或 UI 修改后，必须通过以下门禁：

1. `python3 tools/loop/check-no-harness.py`
2. `python3 tools/loop/check-features.py`
3. `python3 tools/kb-lint.py shangyi/kb`
4. `tools/loop/build.sh test --workspace`
5. 若修改 `shangyi/ui/`：`node shangyi/ui/dsh-plugin-shangyi-workbench/scripts/verify.mjs` 与 `python3 shangyi/ui/dsh-plugin-shangyi/scripts/check-projection.py`
6. 严禁将任何真实 API Key / Token 写入受 Git 追踪的文件、日志或文档（密钥仅从环境变量或 `~/.shangyi-secrets/*.env` / `~/.config/magpie/` 运行时读取）。

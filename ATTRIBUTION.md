# 第三方归属声明（ATTRIBUTION）

本文件登记商奕（ShangYi）**实际使用**的第三方软件归属。分发衍生内容时须保留本文件与相应许可副本。

> 变更记录：2026-11-10 起，Codex（Apache-2.0）不再作为内核基座被使用，`vendor/codex` 已整树移除；
> 内核改为 DeepSeek Harness 作 agent 运行时段。依据见
> [`docs/商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md`](docs/商奕-内核-由Codex改为DeepSeek-Harness-变更记录-v0.1.md)。

---

## 1. 运行时段：DeepSeek Harness（dsh）

| 项 | 内容 |
|---|---|
| 名称 | DeepSeek Harness（CLI/运行时包 `@deepseek-ai/dsh`） |
| 上游 | https://github.com/deepseek-ai/deepseek-harness |
| 许可 | **MIT** —— `Copyright (c) 2026 DeepSeek` |
| 使用方式 | **运行时依赖**：以独立进程（`dsh --profile shangyi …`）运行，商奕的 Rust 引擎经 **MCP**（stdio）向其提供领域工具；**不分发其源码**，不静态链接 |
| 审阅时版本 | CLI/runtime **0.2.1-rc.2**（2026-11-10 实测 `dsh --version`） |
| 成熟度声明 | 官方标注 **developer preview**："DeepSeek Harness remains in developer preview and is still being tested by developers building agent harnesses."、"Its core plugins and APIs will continue to evolve." 上述包名、事件名、配置键均为**带时点的观察，不构成稳定契约** |
| 组件 | 其自身 vendored 依赖含 **Cordis 4.0.5**（MIT，© 2026 DeepSeek）；另有 2 个预编译原生插件 `@deepseek-ai/node-addon-system`、`@deepseek-ai/node-addon-system-darwin-arm64` 为 **BSD-3-Clause**（© 2026 node-addon-landlock-run contributors）。后者仅由 dsh 自身使用，商奕不直接链接 |

MIT 义务（运行时/源码分发两种情形都适用到最小必要程度）：保留版权声明与许可文本；不得使用作者名义为衍生品背书。

## 2. 其他第三方（沿用既有选型，未在本轮变更）

| 组件 | 许可 | 用途 |
|---|---|---|
| `nashpy` | MIT（© 2016 Vince Knight） | **仅 CI 差分 oracle**，不进发货路径 |
| `matplotlib` | PSF 系 | 进程隔离的绘图侧车 |
| `pandoc` | GPL-2.0-or-later | **外部 CLI 调用**，不捆绑、不作为依赖 |
| Rust crate 依赖 | 见 `deny.toml` 白名单 + `cargo deny check licenses` | 编译期内联 |

## 3. 历史（已不再适用）

| 组件 | 状态 |
|---|---|
| OpenAI Codex（`codex-rs`，Apache-2.0，© 2025 OpenAI） | **2026-11-10 移除**。曾作为 harness 基座以源码形式镜像于 `vendor/codex/`；移除时其 `LICENSE`/`NOTICE` 与 `VendorPatches.md` 随目录一并删除。此前分发物中的 Apache-2.0 义务（保留 LICENSE/NOTICE、被改文件附变更声明、逐补丁登记）**随之终止** |
| dsh 设计模式借鉴（事件溯源日志、turn/step 词表、整值事件 + 纯投影、waterfall 钩子、能力预检等） | 继续保留。此类借鉴为**设计模式**，商奕以 Rust 重实现，未复制其源码；仍按 MIT 署名上游 |

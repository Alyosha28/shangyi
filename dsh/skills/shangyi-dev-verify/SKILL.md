---
name: shangyi-dev-verify
description: 商奕（ShangYi）项目工程构建、三大静态门禁、MCP 协议测试与 DSH 三栏工作台（shangyi-web）端到端验收工作流。维护或验证本仓库 Rust 引擎、知识库与 DSH 插件时使用。
compatibility: macOS zsh with rustup, node v24, python3, dsh
metadata:
  domain: developer-verification
  owner: shangyi
---

# 商奕工程构建与全栈验收规程

## 1. 环境准备（挂载盘防污染）

在非交互式 Shell 中执行任何构建或验证前，先注入工具链路径并确保 `CARGO_TARGET_DIR` 指向本机磁盘：

```bash
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.local/share/fnm/node-versions/v24.20.0/installation/bin:/opt/homebrew/opt/python@3.13/libexec/bin:/opt/homebrew/bin:$HOME/.local/bin:$PATH"
export CARGO_TARGET_DIR="/tmp/shangyi-target"
```

## 2. 三大静态门禁（每次提交前必跑）

```bash
python3 tools/loop/check-no-harness.py
python3 tools/loop/check-features.py
python3 tools/kb-lint.py shangyi/kb
```

- `check-no-harness.py`：确保无 `codex`/`rama` 旧基座残留。
- `check-features.py`：确保正式依赖图 0 处 `arbitrary_precision`，且测试图已开启特性门禁。
- `kb-lint.py`：确保 `shangyi/kb/*.md` YAML frontmatter 标量引号安全。

## 3. Rust 引擎与 MCP 协议测试

```bash
tools/loop/build.sh test --workspace
```

重点关注：
- `shangyi-tools` 工具契约注册表测试
- `shangyi-mcp` JSON-RPC 2.0 `tools/list`、`tools/call`、`resources/list`、`resources/read`、延迟建档与关口结算测试
- `shangyi-cli` `mcp_serve_e2e` 进程级管道测试

## 4. DSH 工作台（`shangyi-web`）构建与验证

```bash
# 1) 生成六枚印记图标内嵌包并构建工作台客户端
node shangyi/ui/shangyi-icons/scripts/build-embedded.mjs
bash shangyi/ui/dsh-plugin-shangyi-workbench/scripts/build.sh

# 2) 运行契约校验与投影一致性检查
node shangyi/ui/dsh-plugin-shangyi-workbench/scripts/verify.mjs
python3 shangyi/ui/dsh-plugin-shangyi/scripts/check-projection.py

# 3) 安装/同步 shangyi-web Profile 并执行端到端冒烟
bash shangyi/ui/scripts/install-workbench.sh
bash tools/dsh/smoke.sh
```

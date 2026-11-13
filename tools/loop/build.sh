#!/usr/bin/env bash
# 商奕构建包装器（R1 起：构建本仓库 workspace，不再有 vendor 基座）
#
# 为什么还需要包装（两条实测教训，仍然成立）：
#   1) 本项目位于外接挂载盘，驱动为每个文件生成 AppleDouble `._*` 伴随文件，
#      会被构建工具的通配符扫到 → 随机且误导性的编译失败。
#   2) `target/` 若落在挂载盘会被污染（实测 4532 个伴随文件）。
#   故：构建前清污，并把 `CARGO_TARGET_DIR` 默认指到本机盘 `/tmp`。
#
# 用法: tools/loop/build.sh <cargo 参数...>
#   tools/loop/build.sh test --workspace
#   tools/loop/build.sh test --workspace --no-fail-fast
#   tools/loop/build.sh check -p shangyi-core
#
# 环境变量:
#   CARGO_TARGET_DIR   覆盖默认的 /tmp/shangyi-target
#   （~/.shangyi-build 曾用于此，但本机沙箱下不可写，实测 EPERM，勿再用）
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/tmp/shangyi-target}"

if [ "$#" -eq 0 ]; then
  echo "用法: tools/loop/build.sh <cargo 参数...>" >&2
  exit 2
fi
[ -f "$ROOT/Cargo.toml" ] || { echo "找不到 workspace 根: $ROOT/Cargo.toml" >&2; exit 3; }

# 清污染（脚本缺失时静默跳过）：源码树与 target 目录。
# 只清理 shangyi/ 与 target——vendor/ 已在 R1 摘除，不再有需要清的巨大源码树。
if [ -x "$ROOT/tools/loop/prebuild.sh" ]; then
  "$ROOT/tools/loop/prebuild.sh" "$ROOT/shangyi" "$CARGO_TARGET_DIR" >/dev/null 2>&1 || true
fi

echo "[build] $(cargo --version) | root=$ROOT | target=$CARGO_TARGET_DIR" >&2
cd "$ROOT"
mkdir -p "$CARGO_TARGET_DIR"
ROOT_MARKER="$CARGO_TARGET_DIR/.shangyi-root"
PREV_ROOT=""
if [ -f "$ROOT_MARKER" ]; then
  PREV_ROOT="$(cat "$ROOT_MARKER" 2>/dev/null || true)"
fi
if [ "${1:-}" != "clean" ]; then
  if [ "$PREV_ROOT" != "$ROOT" ] && { [ -d "$CARGO_TARGET_DIR/debug" ] || [ -d "$CARGO_TARGET_DIR/release" ] || [ -d "$CARGO_TARGET_DIR/doc" ]; }; then
    echo "[build] target root changed (${PREV_ROOT:-<unknown>} -> $ROOT), cleaning workspace artifacts" >&2
    cargo clean --workspace -q || true
    cargo clean --workspace --release -q || true
  fi
  printf '%s\n' "$ROOT" > "$ROOT_MARKER"
fi
exec cargo "$@"

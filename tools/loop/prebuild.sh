#!/usr/bin/env bash
# 商奕预构建清污脚本（AppleDouble `._*` 清理）
#
# 背景：本项目位于外接挂载盘（/dev/disk4s2），其驱动为每个文件生成 AppleDouble
# 伴随文件（`._` 前缀）。这些文件会被构建工具的**通配符**扫到并当作源文件解析，
# 造成随机且极具误导性的编译失败。实测案例：
#   - `protoc` 把 src 目录下的 `._*.code_mode.v1.proto` 当作 proto 源码 → 构建失败
#   - `target/` 内 4532 个伴随文件导致 cargo 报出**每次不同**的假缺失错误
#     （如 "can't find crate for zeroize_derive"，实为污染假象）
#
# R1 起 `vendor/` 已摘除，默认清理面改为 `shangyi/` 源码树 + `CARGO_TARGET_DIR`
# （后者默认在 /tmp，正常不会有 AppleDouble，留着是为了防御 CARGO_TARGET_DIR 被
#  人为指回挂载盘的情况）。
#
# 用法：
#   tools/loop/prebuild.sh                # 清理 shangyi/ 与 $CARGO_TARGET_DIR
#   tools/loop/prebuild.sh <目录> [...]   # 清理指定目录
#
# 安全：只删除 `file -b` 判定为 AppleDouble 的文件；非 AppleDouble 的 `._*` 会告警保留。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DEFAULT_TARGETS=("$ROOT/shangyi" "${CARGO_TARGET_DIR:-/tmp/shangyi-target}")

if [ "$#" -gt 0 ]; then
  TARGETS=("$@")
else
  TARGETS=("${DEFAULT_TARGETS[@]}")
fi

total=0
kept=0

for t in "${TARGETS[@]}"; do
  [ -d "$t" ] || continue
  while IFS= read -r f; do
    [ -e "$f" ] || continue
    if file -b "$f" | grep -q "AppleDouble"; then
      rm -f "$f"
      total=$((total + 1))
    else
      # 非 AppleDouble 的 `._*` 可能是真实文件，保留并告警
      echo "[prebuild] ⚠️  保留非 AppleDouble 文件: $f" >&2
      kept=$((kept + 1))
    fi
  done < <(find "$t" -name '._*' -type f 2>/dev/null || true)
done

echo "[prebuild] 清除 AppleDouble 文件 ${total} 个；保留 ${kept} 个"
if [ "$kept" -gt 0 ]; then
  echo "[prebuild] ⚠️  有非 AppleDouble 文件被保留，请人工确认其来源" >&2
fi

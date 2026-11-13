#!/usr/bin/env bash
# 把 dsh/profile/ 安装成 dsh 的 shangyi profile。
#
# 幂等：重复执行只覆盖 profile 文件，不碰会话、不装依赖、不改 dsh 本体。
# 目标目录：${DSH_HOME:-$HOME/.dsh}/profiles/shangyi
#
# 为什么不需要 pnpm install：profile 的 package.json 里 dependencies 是空对象，
# bundle 从 dsh 安装根自己的 node_modules 解析（dsh-app-boot 的 resolveBundleDir
# 先找 profile 目录、再找安装锚点）。本机已核：装完直接 --dump-config 就能合成树。
#
# 退出码：
#   0  装好，且找到了商奕二进制（SHANGYI_BIN 指向可执行文件，或 PATH 上有 shangyi）
#   3  装好了，但**没有**商奕二进制。发布用 profile 是 failOnStartupError: true，
#      缺二进制时每次真实运行都会打印 "spawn shangyi ENOENT" 且零领域工具；
#      dsh 的 required 列表写死、profile 无法让它中止运行，所以硬门放在这里。
#      确实要在没二进制的机器上装（只跑 --dump-config 的 CI 等）：
#        install-profile.sh --allow-missing-server   （降级为告警，退出码 0）
#   2  参数错 / 源文件缺失 / 写盘失败
#
# 用法：
#   tools/dsh/install-profile.sh                            # 装到 ~/.dsh/profiles/shangyi
#   DSH_HOME=/tmp/dsh-smoke tools/dsh/install-profile.sh    # 装到临时 HOME（冒烟用）
#   tools/dsh/install-profile.sh --allow-missing-server     # 缺二进制也当成功
#   tools/dsh/install-profile.sh --dry-run                  # 只打印要做什么
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
SRC_DIR="$REPO_ROOT/dsh/profile"
PROFILE_NAME="shangyi"
DSH_HOME_DIR="${DSH_HOME:-$HOME/.dsh}"
DEST_DIR="$DSH_HOME_DIR/profiles/$PROFILE_NAME"
DRY_RUN=0
ALLOW_MISSING_SERVER=0

for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    --allow-missing-server) ALLOW_MISSING_SERVER=1 ;;
    -h|--help)
      sed -n '2,28p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0
      ;;
    *)
      echo "install-profile.sh: 未知参数 $arg（支持 --dry-run / --allow-missing-server / --help）" >&2
      exit 2
      ;;
  esac
done

fail() { echo "install-profile.sh: $*" >&2; exit 2; }

[ -d "$SRC_DIR" ] || fail "找不到 profile 源目录：$SRC_DIR"
for required in package.json cordis.patch.yml pnpm-workspace.yaml cordis.yml lenient/cordis.patch.yml; do
  [ -f "$SRC_DIR/$required" ] || fail "profile 源缺文件：$SRC_DIR/$required"
done

# 商奕二进制自检：SHANGYI_BIN 优先（profile 里的 command 就是这么解析的），
# 否则看 PATH。两处都没有 → 这个部署拿不到任何领域工具。
SERVER_KIND=""
SERVER_PATH=""
if [ -n "${SHANGYI_BIN:-}" ]; then
  if [ -x "${SHANGYI_BIN}" ]; then
    SERVER_KIND="SHANGYI_BIN"
    SERVER_PATH="$SHANGYI_BIN"
  else
    SERVER_KIND="SHANGYI_BIN_BAD"
    SERVER_PATH="$SHANGYI_BIN"
  fi
elif command -v shangyi >/dev/null 2>&1; then
  SERVER_KIND="PATH"
  SERVER_PATH="$(command -v shangyi)"
else
  SERVER_KIND="MISSING"
fi

DSH_BIN="$(command -v dsh || true)"
if [ -z "$DSH_BIN" ]; then
  echo "提示：PATH 里没有 dsh，装完还得装/找 dsh 本体才能启动。" >&2
fi

echo "安装 shangyi profile"
echo "  源：$SRC_DIR"
echo "  目标：$DEST_DIR"
echo "  DSH_HOME：$DSH_HOME_DIR"
case "$SERVER_KIND" in
  SHANGYI_BIN)     echo "  商奕二进制：$SERVER_PATH（来自 SHANGYI_BIN）" ;;
  PATH)            echo "  商奕二进制：$SERVER_PATH（来自 PATH）" ;;
  SHANGYI_BIN_BAD) echo "  商奕二进制：SHANGYI_BIN=$SERVER_PATH 不是可执行文件" ;;
  MISSING)         echo "  商奕二进制：未找到（SHANGYI_BIN 未设，PATH 上也没有 shangyi）" ;;
esac

if [ "$DRY_RUN" -eq 1 ]; then
  echo "  --dry-run：不写任何文件。将执行：mkdir -p \"$DEST_DIR\" 并覆盖 4 个 profile 文件 + lenient.patch.yml。"
  exit 0
fi

mkdir -p "$DEST_DIR" || fail "创建目录失败：$DEST_DIR（沙箱下 ~/.dsh 可能不可写，改用 DSH_HOME=/tmp/...）"

# 4 个文件全部覆盖：profile 是仓库产物，本机副本以仓库为准。
# cordis.yml 由 dsh 每次启动重写为同一个空入口列表，这里写它只是让目录立刻可读。
for file in package.json cordis.patch.yml pnpm-workspace.yaml cordis.yml; do
  cp -f "$SRC_DIR/$file" "$DEST_DIR/$file" || fail "复制失败：$file"
done
# lenient 变体装成叠加层：dsh --profile shangyi --patch <profile>/lenient.patch.yml
cp -f "$SRC_DIR/lenient/cordis.patch.yml" "$DEST_DIR/lenient.patch.yml" || fail "复制失败：lenient.patch.yml"

echo
echo "已写入："
for file in package.json cordis.yml cordis.patch.yml pnpm-workspace.yaml lenient.patch.yml; do
  printf '  %-22s %s\n' "$file" "$(wc -c < "$DEST_DIR/$file" | tr -d ' ') bytes"
done

echo
echo "下一步："
echo "  1) 离线核对合成树（不激活插件，不需要二进制、不需要模型凭据）："
echo "       DSH_HOME=\"$DSH_HOME_DIR\" dsh --profile $PROFILE_NAME --dump-config | grep -A9 'mcp-shangyi'"
echo "  2) 放好商奕二进制（R2 才实现 mcp-serve 子命令）："
echo "       export SHANGYI_BIN=/abs/path/to/shangyi   # 不设则用 PATH 上的 shangyi"
echo "  3) 配模型凭据并跑一轮（真实模型 E2E，余额未核）："
echo "       export DEEPSEEK_API_KEY=..."
echo "       DSH_HOME=\"$DSH_HOME_DIR\" dsh --profile $PROFILE_NAME --json \"用囚徒困境给个例子\""
echo "  4) 没有二进制的机器只想看配置树（放弃领域工具，回到静默降级）："
echo "       dsh --profile $PROFILE_NAME --patch \"$DEST_DIR/lenient.patch.yml\" \"...\""
echo
echo "回滚：rm -rf \"$DEST_DIR\""

STATUS=0

# ── 硬门：缺二进制就不许"装成功" ────────────────────────────────────────────────
# 发布用 profile 是 failOnStartupError: true。缺二进制时 dsh 仍会启动（required 列表
# 是全局写死的，profile 无法中止运行），只是每次真实运行都会打印 spawn ENOENT 且
# 零领域工具。这种部署等于"引擎没数据"，必须在这里拦住。
if [ "$SERVER_KIND" = "MISSING" ] || [ "$SERVER_KIND" = "SHANGYI_BIN_BAD" ]; then
  echo >&2
  if [ "$SERVER_KIND" = "SHANGYI_BIN_BAD" ]; then
    echo "install-profile.sh: SHANGYI_BIN=$SERVER_PATH 不是可执行文件。" >&2
  else
    echo "install-profile.sh: 找不到商奕二进制（SHANGYI_BIN 未设，PATH 上也没有 shangyi）。" >&2
  fi
  echo "  后果：模型看不到任何领域工具（mcp__shangyi__* 一个都没有）；" >&2
  echo "        每次真实运行 stderr 会打印 \"spawn shangyi ENOENT\" 且 dsh 仍继续跑，" >&2
  echo "        于是模型对每个问题只能回\"未取得\"——看起来像引擎没数据，实际是工具没挂上。" >&2
  echo "  解决：export SHANGYI_BIN=/abs/path/to/shangyi 后重跑本脚本；" >&2
  echo "        或确实只要配置树（CI/评审机）：加 --allow-missing-server。" >&2
  if [ "$ALLOW_MISSING_SERVER" -eq 1 ]; then
    echo "  （已按 --allow-missing-server 降级为告警，退出码 0。）" >&2
  else
    STATUS=3
  fi
fi

if [ -z "${DEEPSEEK_API_KEY:-}" ]; then
  echo >&2
  echo "提示：当前 shell 没有 DEEPSEEK_API_KEY，真实运行会以 MISSING_CREDENTIAL 失败。" >&2
fi

exit "$STATUS"

#!/usr/bin/env bash
# 把商奕三件套插件（host + shell + workbench）与受控 patch 层装进 Web 工作台 profile（shangyi-web）。
#
# 用法：
#   bash shangyi/ui/scripts/install-workbench.sh                 # 构建 + 安装 + 启动实例并打开浏览器
#   bash shangyi/ui/scripts/install-workbench.sh --no-open       # 启动实例但不自动拉起浏览器
#   bash shangyi/ui/scripts/install-workbench.sh --install-only  # 仅构建 + 同步插件与 profile（不启动实例）
#   bash shangyi/ui/scripts/install-workbench.sh --uninstall
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"   # 仓库根
PROFILE="${SHANGYI_VERIFY_PROFILE:-shangyi-web}"
PORT="${SHANGYI_VERIFY_PORT:-8791}"
DSH_HOME_DIR="${DSH_HOME:-$HOME/.dsh}"
SHANGYI_HOME_DIR="${SHANGYI_HOME:-$HOME/.shangyi}"
DEST_PROFILE_DIR="$DSH_HOME_DIR/profiles/$PROFILE"

HOST_DIR="$ROOT/shangyi/ui/dsh-plugin-shangyi"
SHELL_DIR="$ROOT/shangyi/ui/dsh-plugin-shangyi-shell"
WB_DIR="$ROOT/shangyi/ui/dsh-plugin-shangyi-workbench"
ICONS_DIR="$ROOT/shangyi/ui/shangyi-icons"
PATCH_SRC="$ROOT/dsh/profile-web/cordis.patch.yml"

DO_UNINSTALL=0
DO_BOOT=1
DO_OPEN=1
for a in "$@"; do
  case "$a" in
    --uninstall) DO_UNINSTALL=1 ;;
    --no-boot|--install-only) DO_BOOT=0 ;;
    --no-open) DO_OPEN=0 ;;
    *) echo "未知参数: $a" >&2; exit 2 ;;
  esac
done

mkdir -p "$SHANGYI_HOME_DIR/cases" "$SHANGYI_HOME_DIR/runtime" "$SHANGYI_HOME_DIR/kb"

clean_appledouble() {
  for d in "$ROOT/shangyi/ui" "$ROOT/dsh/skills" "$DEST_PROFILE_DIR"; do
    if [ -d "$d" ]; then
      find "$d" -maxdepth 4 -name '._*' -delete 2>/dev/null || true
    fi
  done
}

stop_instance() {
  local pids
  pids="$(lsof -ti "tcp:$PORT" -sTCP:LISTEN 2>/dev/null || true)"
  if [ -n "$pids" ]; then
    echo "停止旧实例 (port $PORT): $pids"
    # shellcheck disable=SC2086
    kill $pids 2>/dev/null || true
    for _ in $(seq 1 20); do
      [ -z "$(lsof -ti "tcp:$PORT" 2>/dev/null || true)" ] && return 0
      sleep 0.5
    done
  fi
}

if [ "$DO_UNINSTALL" = "1" ]; then
  for pkg in dsh-plugin-shangyi-workbench dsh-plugin-shangyi-shell dsh-plugin-shangyi; do
    DSH_HOME="$DSH_HOME_DIR" dsh plugin --profile "$PROFILE" remove "$pkg" || true
  done
  stop_instance
  echo "已卸载商奕插件族（profile: $PROFILE）"
  exit 0
fi

clean_appledouble

echo "== 构建六枚印记内嵌包、宣纸·墨玉壳层与工作台客户端 bundle =="
node "$ICONS_DIR/scripts/build-embedded.mjs"
bash "$SHELL_DIR/scripts/build.sh"
bash "$WB_DIR/scripts/build.sh"

echo "== 语法检查 =="
node --check "$HOST_DIR/lib/index.js"
node --check "$HOST_DIR/lib/client.js"
node --check "$SHELL_DIR/lib/client.js"
node --check "$WB_DIR/lib/client.js"

mkdir -p "$DEST_PROFILE_DIR"
if [ ! -f "$DEST_PROFILE_DIR/package.json" ]; then
  cat > "$DEST_PROFILE_DIR/package.json" <<'JSON'
{
  "name": "dsh-profile-shangyi-web",
  "private": true,
  "dependencies": {},
  "dsh": {
    "profile": {
      "bundles": [
        "@deepseek-ai/dsh-base",
        "@deepseek-ai/dsh-web-app"
      ]
    }
  }
}
JSON
fi
[ -f "$DEST_PROFILE_DIR/cordis.yml" ] || cp -f "$ROOT/dsh/profile-web/cordis.yml" "$DEST_PROFILE_DIR/cordis.yml"
[ -f "$DEST_PROFILE_DIR/pnpm-workspace.yaml" ] || cp -f "$ROOT/dsh/profile-web/pnpm-workspace.yaml" "$DEST_PROFILE_DIR/pnpm-workspace.yaml"

echo "== 挂载三件套插件到 profile: $PROFILE =="
for dir in "$HOST_DIR" "$SHELL_DIR" "$WB_DIR"; do
  DSH_HOME="$DSH_HOME_DIR" dsh plugin --profile "$PROFILE" add "$dir" | tail -3
done

echo "== 同步受控 patch 层（mcp-shangyi + Magpie 路由 + preset-shangyi 隔离） =="
cp -f "$PATCH_SRC" "$DEST_PROFILE_DIR/cordis.patch.yml"
clean_appledouble

echo "== 校验合成配置树 =="
DSH_HOME="$DSH_HOME_DIR" SHANGYI_ROOT="$ROOT" dsh --profile "$PROFILE" --dump-config > /tmp/shangyi-web-dump.yml
grep -q "id: mcp-shangyi" /tmp/shangyi-web-dump.yml
grep -q "id: preset-shangyi" /tmp/shangyi-web-dump.yml
grep -q "dsh-plugin-shangyi-workbench" /tmp/shangyi-web-dump.yml
echo "✓ $PROFILE 合成树核验通过（$(wc -l < /tmp/shangyi-web-dump.yml | tr -d ' ') 行）"

resolve_shangyi_bin() {
  if [ -n "${SHANGYI_BIN:-}" ]; then
    [ -x "$SHANGYI_BIN" ] && { printf '%s\n' "$SHANGYI_BIN"; return 0; }
    return 1
  fi
  for cand in "$SHANGYI_HOME_DIR/bin/shangyi" "$HOME/.shangyi/bin/shangyi" "/tmp/shangyi-target/release/shangyi"; do
    if [ -x "$cand" ]; then
      printf '%s\n' "$cand"
      return 0
    fi
  done
  if command -v shangyi >/dev/null 2>&1; then
    command -v shangyi
    return 0
  fi
  return 1
}

SHANGYI_BIN_RESOLVED="$(resolve_shangyi_bin || true)"

if [ "$DO_BOOT" != "1" ]; then
  if [ -z "$SHANGYI_BIN_RESOLVED" ]; then
    echo "⚠ 提示：尚未检测到可执行 'shangyi' 二进制；插件已装入 profile，启动前请执行：" >&2
    echo "    tools/loop/build.sh build --release -p shangyi-cli" >&2
    echo "  或安装到用户目录：cargo install --path shangyi/crates/shangyi-cli --root ~/.shangyi" >&2
  fi
  echo "✓ 已完成工作台构建与安装（profile: $PROFILE，持久化工作区: $SHANGYI_HOME_DIR/cases）"
  exit 0
fi

stop_instance

if [ -z "$SHANGYI_BIN_RESOLVED" ] || [ ! -x "$SHANGYI_BIN_RESOLVED" ]; then
  echo "✗ 错误：找不到可执行商奕确定性二进制（未发现 SHANGYI_BIN / ~/.shangyi/bin/shangyi / PATH shangyi）。" >&2
  echo "  为避免工作台退化成无工具纯对话，拒绝继续拉起裸聊实例。" >&2
  echo "  请先构建或安装后重试：" >&2
  echo "    1) tools/loop/build.sh build --release -p shangyi-cli && /tmp/shangyi-target/release/shangyi start" >&2
  echo "    2) 或 cargo install --path shangyi/crates/shangyi-cli --root ~/.shangyi && ~/.shangyi/bin/shangyi start" >&2
  exit 3
fi

# 若调用环境未显式注入 MAGPIE_GATEWAY_KEY，则从本机 ~/.config/magpie/caller-keys.json 内存读取（绝不回显或写日志）
if [ -z "${MAGPIE_GATEWAY_KEY:-}" ] && [ -f "$HOME/.config/magpie/caller-keys.json" ]; then
  MAGPIE_GATEWAY_KEY="$(python3 -c '
import json, os
p = os.path.expanduser("~/.config/magpie/caller-keys.json")
data = json.load(open(p, encoding="utf-8"))
items = data if isinstance(data, list) else (data.get("keys") if isinstance(data, dict) and isinstance(data.get("keys"), list) else [])
best = None
for item in items:
    if isinstance(item, dict):
        sec = (item.get("secret") or item.get("key") or "").strip()
        if not sec:
            continue
        nm = str(item.get("name") or "").lower()
        if any(tag in nm for tag in ("dsh", "shangyi", "agy")):
            best = sec
            break
        if best is None:
            best = sec
if best:
    print(best)
' 2>/dev/null || true)"
  export MAGPIE_GATEWAY_KEY
fi

echo "== 起商奕 Web 工作台实例 (port $PORT) =="
LOG="$SHANGYI_HOME_DIR/runtime/dsh-web.log"
URL_FILE="$SHANGYI_HOME_DIR/runtime/workbench-url.txt"
install -m 600 /dev/null "$LOG"
(
  cd "$ROOT"
  export DSH_HOME="$DSH_HOME_DIR"
  export SHANGYI_HOME="$SHANGYI_HOME_DIR"
  export SHANGYI_ROOT="$ROOT"
  export SHANGYI_SKILL_DIR="$ROOT/dsh/skills"
  export SHANGYI_BIN="$SHANGYI_BIN_RESOLVED"
  nohup dsh --profile "$PROFILE" --no-open --port "$PORT" > "$LOG" 2>&1 &
)
for _ in $(seq 1 30); do
  sleep 1
  URL="$(grep -o "http://127.0.0.1:$PORT/?token=[A-Za-z0-9_-]*" "$LOG" | head -1 || true)"
  [ -n "$URL" ] && break
done
if [ -z "${URL:-}" ]; then
  echo "实例没起来，日志尾部：" >&2
  tail -25 "$LOG" >&2
  exit 1
fi
chmod 600 "$LOG" 2>/dev/null || true
install -m 600 /dev/null "$URL_FILE"
printf '%s\n' "$URL" > "$URL_FILE"
echo "✓ 商奕 Web 工作台已就绪（port $PORT）"
echo "  访问地址: $URL"
echo "  地址记录: $URL_FILE"
if [ "$DO_OPEN" = "1" ] && [ "${CI:-}" != "true" ] && [ -z "${SHANGYI_NO_OPEN:-}" ]; then
  if command -v open >/dev/null 2>&1; then
    open "$URL" >/dev/null 2>&1 || true
  elif command -v xdg-open >/dev/null 2>&1; then
    xdg-open "$URL" >/dev/null 2>&1 || true
  fi
fi

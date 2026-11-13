#!/usr/bin/env bash
# 商奕 dsh profile 的离线冒烟。
#
# 全程不联网、不调用真实模型（跑之前显式清掉 DEEPSEEK_API_KEY，避免万一有凭据
# 就真发请求）。覆盖四件事：
#   (a) profile 能装进一个干净的 DSH_HOME，--dump-config 能合成出树，
#       且树里看得见 mcp-client 行与 system-prompt 覆写；安装脚本的二进制自检
#       （缺二进制 → 退出码 3，--allow-missing-server → 0）也在这里断言；
#   (b) 假 MCP 服务端的两条协商路径（legacy initialize / modern server-discover）
#       各自完成 initialize→tools/list 并断言 2 个工具；
#   (c) `dsh --profile shangyi --help` 退出码 0；
#       c2 缺二进制 + 严格默认（failOnStartupError: true）→ stderr 必须出现
#       "spawn shangyi ENOENT"（不许静默）；c3 lenient 叠加层 → 回到静默且零领域工具；
#   (d) 真装配：dsh 自己的 MCP 客户端 spawn 假服务端，抓到 wire 上的 tools/list
#       并断言 2 个工具；再解 session log 核装配层（system prompt + 工具面）。
#
# 失败一律非零退出并打印原因。没有 `timeout` 命令，所有子进程都是自己会退出的。
#
# 用法：
#   tools/dsh/smoke.sh                     # 默认用 /tmp/dsh-smoke
#   SMOKE_DSH_HOME=/tmp/other tools/dsh/smoke.sh
set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/../.." && pwd)"
SMOKE_HOME="${SMOKE_DSH_HOME:-/tmp/dsh-smoke}"
PROFILE="shangyi"
PYTHON="${PYTHON:-python3}"

FAILURES=0
PASSES=0

pass() { PASSES=$((PASSES + 1)); printf '  \033[32m✓\033[0m %s\n' "$1"; }
fail() {
  FAILURES=$((FAILURES + 1))
  printf '  \033[31m✗\033[0m %s\n' "$1"
  [ $# -gt 1 ] && printf '      原因：%s\n' "$2"
  return 0
}
step() { printf '\n[%s] %s\n' "$1" "$2"; }

# 只允许清 /tmp 下的临时 HOME：这条 rm -rf 必须挡在 workspace 之外且路径可控。
case "$SMOKE_HOME" in
  /tmp/*) : ;;
  *)
    echo "smoke.sh: SMOKE_DSH_HOME 必须位于 /tmp 下（当前：$SMOKE_HOME）" >&2
    exit 2
    ;;
esac

command -v dsh >/dev/null 2>&1 || { echo "smoke.sh: PATH 里没有 dsh" >&2; exit 2; }
command -v "$PYTHON" >/dev/null 2>&1 || { echo "smoke.sh: 找不到 $PYTHON" >&2; exit 2; }

echo "商奕 dsh profile 冒烟"
echo "  仓库：$REPO_ROOT"
echo "  临时 DSH_HOME：$SMOKE_HOME"
echo "  dsh：$(command -v dsh) ($(dsh --version 2>/dev/null))"

# ── (a1) 干净安装 ───────────────────────────────────────────────────────────────
step a1 "从零安装 profile 到干净 DSH_HOME"
rm -rf "$SMOKE_HOME"
mkdir -p "$SMOKE_HOME"
if DSH_HOME="$SMOKE_HOME" bash "$SCRIPT_DIR/install-profile.sh" --allow-missing-server \
     >"$SMOKE_HOME/install.log" 2>&1; then
  missing=""
  for file in package.json cordis.yml cordis.patch.yml pnpm-workspace.yaml lenient.patch.yml; do
    [ -f "$SMOKE_HOME/profiles/$PROFILE/$file" ] || missing="$missing $file"
  done
  if [ -z "$missing" ]; then
    pass "安装到 $SMOKE_HOME/profiles/$PROFILE（4 个 profile 文件 + lenient.patch.yml 齐）"
  else
    fail "安装后缺文件：$missing" "$(tail -3 "$SMOKE_HOME/install.log")"
  fi
else
  fail "install-profile.sh 非零退出" "$(tail -5 "$SMOKE_HOME/install.log")"
fi

# ── (a1b) 安装脚本的二进制自检：缺二进制必须非零退出 ────────────────────────────
# 这是 P1-3 的硬门：dsh 的 required 列表写死，profile 无法让运行中止，
# 所以"缺二进制就装不上"只能落在安装脚本上。
step a1b "安装脚本的二进制自检（缺二进制 → 退出码 3）"
if command -v shangyi >/dev/null 2>&1; then
  printf '  \033[33m-\033[0m 跳过：本机 PATH 上真有 shangyi，构造不出"缺二进制"场景\n'
else
  env -u SHANGYI_BIN DSH_HOME="$SMOKE_HOME/strict-probe" \
      bash "$SCRIPT_DIR/install-profile.sh" >"$SMOKE_HOME/install-strict.log" 2>&1
  strict_code=$?
  if [ "$strict_code" -eq 3 ] && grep -q "找不到商奕二进制" "$SMOKE_HOME/install-strict.log"; then
    pass "缺二进制时 install-profile.sh 退出码 3 并说明后果（不是静默成功）"
  else
    fail "缺二进制时安装脚本没有硬失败" "退出码 $strict_code（期望 3）"
  fi
  env -u SHANGYI_BIN DSH_HOME="$SMOKE_HOME/lenient-probe" \
      bash "$SCRIPT_DIR/install-profile.sh" --allow-missing-server >/dev/null 2>&1
  if [ $? -eq 0 ]; then
    pass "--allow-missing-server 时退出码 0（CI/评审机的显式降级）"
  else
    fail "--allow-missing-server 没有放行"
  fi
fi

# ── (a2) 幂等：再装一次必须也成功且内容不变 ────────────────────────────────────
step a2 "重复安装（幂等性）"
if command -v shasum >/dev/null 2>&1; then HASHER=(shasum)
elif command -v sha256sum >/dev/null 2>&1; then HASHER=(sha256sum)
else HASHER=(); fi
hash_file() { if [ ${#HASHER[@]} -eq 0 ]; then cksum; else "${HASHER[@]}"; fi; }
before="$(cat "$SMOKE_HOME/profiles/$PROFILE/cordis.patch.yml" 2>/dev/null | hash_file | cut -d' ' -f1)"
if DSH_HOME="$SMOKE_HOME" bash "$SCRIPT_DIR/install-profile.sh" --allow-missing-server >/dev/null 2>&1; then
  after="$(cat "$SMOKE_HOME/profiles/$PROFILE/cordis.patch.yml" 2>/dev/null | hash_file | cut -d' ' -f1)"
  if [ -n "$before" ] && [ "$before" = "$after" ]; then
    pass "第二次安装成功，patch 内容一致（$before）"
  else
    fail "第二次安装改动了 patch 内容" "$before → $after"
  fi
else
  fail "第二次安装非零退出"
fi

# ── (a3) --dump-config ────────────────────────────────────────────────────────
step a3 "dsh --profile $PROFILE --dump-config"
DUMP="$SMOKE_HOME/dump.txt"
if DSH_HOME="$SMOKE_HOME" dsh --profile "$PROFILE" --dump-config >"$DUMP" 2>"$SMOKE_HOME/dump.err"; then
  pass "--dump-config 退出码 0（$(wc -l < "$DUMP" | tr -d ' ') 行合成树）"
else
  fail "--dump-config 非零退出" "$(tail -3 "$SMOKE_HOME/dump.err")"
fi

check_dump() { # $1=说明 $2=固定字符串
  if grep -qF -- "$2" "$DUMP" 2>/dev/null; then
    pass "合成树含 $1"
  else
    fail "合成树缺 $1" "grep 没找到：$2"
  fi
}
check_dump "mcp-client 行（mcp-shangyi）" "- id: mcp-shangyi"
check_dump "mcp-client 包名" "name: '@deepseek-ai/dsh-mcp-client'"
check_dump "serverName: shangyi" "serverName: shangyi"
check_dump "transport: stdio" "transport: stdio"
check_dump "args: mcp-serve" "- mcp-serve"
check_dump "SHANGYI_BIN 覆盖表达式" "process.env.SHANGYI_BIN"
check_dump "system-prompt personaPrefix 覆写" "personaPrefix: |"
check_dump "persona 里的铁律 1" "铁律 1 · LLM 不算数"
check_dump "persona 里的铁律 2" "铁律 2 · 关键假设必须过 checkpoint"
check_dump "persona 里的未取得口径" "未取得"

# 被禁用的行：逐个断言，避免"写了 disabled 但 id 写错"这种静默失效
disabled_expected="tool-bash bash-sandbox tool-pwsh pwsh-sandbox tool-fs tool-fs-search tool-jobs \
ptc-runtime workflow-ptc tool-workflow tool-subagent tool-subagent-fork tool-subagent-control \
tool-subagent-list-agents tool-working-directory tool-skill plan-mode tool-todo tool-goal tool-web permission"
disabled_ok=1
for row in $disabled_expected; do
  if ! "$PYTHON" - "$DUMP" "$row" <<'PY'
import re, sys
path, row = sys.argv[1], sys.argv[2]
text = open(path, encoding="utf-8").read()
# 找到该 id 的行块（到下一条 "- id: " 或注释为止），检查块内是否有 disabled: true
pattern = re.compile(r"^- id: " + re.escape(row) + r"\n(.*?)(?=^- id: |^# ==|\Z)", re.M | re.S)
match = pattern.search(text)
sys.exit(0 if match and re.search(r"^\s*disabled: true", match.group(1), re.M) else 1)
PY
  then
    disabled_ok=0
    fail "行 $row 未禁用" "dump 里没找到 disabled: true"
  fi
done
[ "$disabled_ok" -eq 1 ] && pass "21 行编码/非领域工具全部 disabled: true"

# ── (b) 假 MCP 服务端两条协商路径 ─────────────────────────────────────────────
step b1 "fake_mcp.py 自测（legacy initialize + modern server/discover）"
SELFTEST_LOG="$SMOKE_HOME/fake-mcp-selftest.log"
if "$PYTHON" "$SCRIPT_DIR/fake_mcp.py" --selftest >"$SELFTEST_LOG" 2>&1; then
  if grep -q "2 个工具" "$SELFTEST_LOG"; then
    pass "两条路径各拿到 2 个工具（$(grep -c '✓ tools/list' "$SELFTEST_LOG") 次 tools/list 断言）"
  else
    fail "自测输出里没有工具数断言" "$(tail -3 "$SELFTEST_LOG")"
  fi
  pass "fake_mcp.py --selftest 退出码 0"
else
  fail "fake_mcp.py --selftest 非零退出" "$(tail -5 "$SELFTEST_LOG")"
fi

# ── (c) dsh --help ────────────────────────────────────────────────────────────
step c1 "dsh --profile $PROFILE --help"
HELP_LOG="$SMOKE_HOME/help.log"
if DSH_HOME="$SMOKE_HOME" dsh --profile "$PROFILE" --help >"$HELP_LOG" 2>&1; then
  pass "--help 退出码 0（$(head -1 "$HELP_LOG")）"
else
  fail "--help 非零退出" "$(tail -3 "$HELP_LOG")"
fi

# ── (c2) 缺二进制 + 严格默认：必须吵，不许静默 ─────────────────────────────────
# 这是 P1-3 的核心断言。profile 默认 failOnStartupError: true，服务端缺失时
# dsh 会打印 "spawn shangyi ENOENT"，且工具面变成 0 个（连 3 个资源工具都没有，
# 因为 mcp-resources 只在 client 行激活时才登记它们）。
# 注意：运行**不会**中止（dsh 的 required 列表写死），所以断言看 stderr 内容，
# 不看退出码——退出码仍是模型凭据导致的 1。
step c2 "缺二进制 + 严格默认（failOnStartupError: true）必须显式报错"
if command -v shangyi >/dev/null 2>&1; then
  printf '  \033[33m-\033[0m 跳过：本机 PATH 上真有 shangyi，构造不出"缺二进制"场景\n'
else
  STRICT_LOG="$SMOKE_HOME/strict-missing.log"
  env -u SHANGYI_BIN -u DEEPSEEK_API_KEY -u MAGPIE_GATEWAY_KEY \
      DSH_HOME="$SMOKE_HOME" dsh --profile "$PROFILE" --json "冒烟：缺二进制" \
      >"$STRICT_LOG" 2>&1
  if grep -q "spawn shangyi ENOENT" "$STRICT_LOG"; then
    pass "stderr 出现 spawn shangyi ENOENT（静默降级已消除）"
  else
    fail "缺二进制时没有 spawn ENOENT 报错（又变回静默了）" "$(tail -4 "$STRICT_LOG")"
  fi
  if grep -q "did not activate" "$STRICT_LOG"; then
    pass "stderr 标出未激活的条目（dsh: warning: … did not activate）"
  else
    fail "没有 did not activate 告警" "$(tail -4 "$STRICT_LOG")"
  fi
  if command -v zstd >/dev/null 2>&1; then
    NEWEST="$(ls -t "$SMOKE_HOME"/sessions/*/session-*/session.v4.jsonl.zstd 2>/dev/null | head -1)"
    zstd -dc "$NEWEST" >"$SMOKE_HOME/session-strict.jsonl" 2>/dev/null
    if "$PYTHON" - "$SMOKE_HOME/session-strict.jsonl" <<'PY'
import json, sys
tools = None
for line in open(sys.argv[1], encoding="utf-8"):
    row = json.loads(line)
    if row.get("type") == "request/header":
        tools = [tool["name"] for tool in (row["data"].get("header") or {}).get("tools") or []]
domain = [name for name in (tools or []) if name.startswith("mcp__shangyi__")]
if domain:
    print(f"      原因：缺二进制却仍有领域工具 {domain}")
    sys.exit(1)
print(f"      缺二进制时模型可见工具面：{tools or '（空）'} —— 零领域工具")
PY
    then
      pass "缺二进制时零领域工具（模型不会假装有工具）"
    else
      fail "缺二进制时工具面断言失败" "见 $SMOKE_HOME/session-strict.jsonl"
    fi
  else
    printf '  \033[33m-\033[0m 跳过工具面断言：没有 zstd\n'
  fi
fi

# ── (c3) lenient 变体：回到静默降级（只给不跑真实咨询的机器用） ────────────────
step c3 "lenient 叠加层：无二进制也能起（静默，零领域工具）"
if command -v shangyi >/dev/null 2>&1; then
  printf '  \033[33m-\033[0m 跳过：本机 PATH 上真有 shangyi\n'
else
  LENIENT_LOG="$SMOKE_HOME/lenient.log"
  env -u SHANGYI_BIN -u DEEPSEEK_API_KEY -u MAGPIE_GATEWAY_KEY \
      DSH_HOME="$SMOKE_HOME" dsh --profile "$PROFILE" \
      --patch "$SMOKE_HOME/profiles/$PROFILE/lenient.patch.yml" --json "冒烟：lenient" \
      >"$LENIENT_LOG" 2>&1
  lenient_code=$?
  if [ "$lenient_code" -eq 1 ] && grep -q "MISSING_CREDENTIAL" "$LENIENT_LOG" \
     && ! grep -q "did not activate" "$LENIENT_LOG" && ! grep -q "spawn shangyi" "$LENIENT_LOG"; then
    pass "lenient：启动无 MCP 报错（退出码 1 仅因缺模型凭据）——这就是要避免的静默形态"
  else
    fail "lenient 变体行为不符" "退出码 $lenient_code；$(tail -3 "$LENIENT_LOG")"
  fi
fi

# ── (d) 真装配：dsh 客户端 → 假服务端 ─────────────────────────────────────────
# 用 SHANGYI_BIN 指向包装脚本，验证 profile 里那条 !!js 表达式真的被求值；
# profile 默认就是 failOnStartupError: true，所以握手失败会在 stderr 显式报出来。
step d1 "dsh 真实 spawn 假 MCP 服务端并抓 wire 上的 tools/list"
BIN_DIR="$SMOKE_HOME/bin"
mkdir -p "$BIN_DIR"
cat >"$BIN_DIR/shangyi" <<EOF
#!/usr/bin/env bash
# 冒烟用占位二进制：把 dsh 的 stdio MCP 调用转给假服务端。
exec "$PYTHON" "$SCRIPT_DIR/fake_mcp.py" "\$@"
EOF
chmod +x "$BIN_DIR/shangyi"

run_wire_case() { # $1=名字 $2=额外 env（可为空）
  local name="$1" extra_env="${2:-}"
  local trace="$SMOKE_HOME/mcp-trace-$name.jsonl"
  local out="$SMOKE_HOME/wire-$name.log"
  rm -f "$trace"
  # 显式清掉模型凭据：冒烟绝不发真实请求
  # shellcheck disable=SC2086
  env -u DEEPSEEK_API_KEY -u MAGPIE_GATEWAY_KEY \
      DSH_HOME="$SMOKE_HOME" SHANGYI_BIN="$BIN_DIR/shangyi" \
      FAKE_MCP_TRACE="$trace" $extra_env \
      dsh --profile "$PROFILE" --json "冒烟：不需要回答" \
      >"$out" 2>&1
  local code=$?
  # 预期失败点：没有模型凭据 → MISSING_CREDENTIAL。除此之外不应有 MCP 报错。
  if ! grep -q "MISSING_CREDENTIAL" "$out"; then
    fail "$name：没走到预期的 MISSING_CREDENTIAL" "$(tail -4 "$out")"
    return
  fi
  if grep -q "mcp-client($PROFILE)" "$out"; then
    fail "$name：MCP 装配失败（failOnStartupError 已开）" "$(grep -A2 "mcp-client(" "$out" | head -6)"
    return
  fi
  if grep -q "did not activate" "$out"; then
    fail "$name：有插件未激活" "$(grep "did not activate" -A2 "$out" | head -4)"
    return
  fi
  if [ ! -s "$trace" ]; then
    fail "$name：假服务端没收到任何消息" "trace 文件为空或不存在：$trace"
    return
  fi
  if "$PYTHON" - "$trace" <<'PY'
import json, sys
rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
lists = [row["payload"].get("result", {}) for row in rows
         if row["dir"] == "out" and (row["payload"].get("result") or {}).get("tools") is not None]
if not lists:
    print("trace 里没有 tools/list 结果"); sys.exit(1)
tools = lists[-1]["tools"]
names = [tool["name"] for tool in tools]
if names != ["solve_equilibria", "kb_retrieve"]:
    print(f"工具列表不符：{names}"); sys.exit(1)
print(f"wire 上的 tools/list 返回 {len(tools)} 个工具：{names}")
PY
  then
    pass "$name：dsh 客户端完成握手，2 个工具已注册（退出码 $code，预期为 1=MISSING_CREDENTIAL）"
  else
    fail "$name：wire 断言失败" "见 $trace"
  fi
}

run_wire_case modern ""
run_wire_case legacy "FAKE_MCP_LEGACY=1"

# ── (d2) 装配层核验：session 日志里的 system prompt 与模型可见工具面 ────────────
# dsh 把每个 turn 的 system/message 与 request/header（含工具 schema）写进
# $DSH_HOME/sessions/<cwd>/<session>/session.v4.jsonl.zstd。离线就能读到"模型
# 到底看到了什么"，不需要真实模型请求。zstd 缺失时跳过并明说。
step d2 "装配层核验：session 日志里的 system prompt 与工具面"
if ! command -v zstd >/dev/null 2>&1; then
  printf '  \033[33m-\033[0m 跳过：本机没有 zstd，解不开 session 日志（装配层未核）\n'
else
  SESSION_ZST="$(ls -t "$SMOKE_HOME"/sessions/*/session-*/session.v4.jsonl.zstd 2>/dev/null | head -1)"
  if [ -z "$SESSION_ZST" ]; then
    fail "找不到 session 日志" "$SMOKE_HOME/sessions 下没有 session.v4.jsonl.zstd"
  else
    DECODED="$SMOKE_HOME/session-decoded.jsonl"
    zstd -dc "$SESSION_ZST" >"$DECODED" 2>/dev/null
    if "$PYTHON" - "$DECODED" <<'PY'
import json, sys

rows = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8") if line.strip()]
system_text = ""
tools, context = [], {}
for row in rows:
    kind, data = row.get("type"), row.get("data") or {}
    if kind == "system/message":
        system_text += "".join(part.get("text", "") for part in data["message"]["content"])
    elif kind == "request/header":
        tools = [tool["name"] for tool in (data.get("header") or {}).get("tools") or []]
    elif kind == "request/context":
        context = data

problems = []
for needle, label in [
    ("商奕", "persona 标题"),
    ("铁律 1 · LLM 不算数", "铁律 1"),
    ("铁律 2 · 关键假设必须过 checkpoint", "铁律 2"),
    ("假 MCP 服务端", "MCP 服务端 instructions 并入 prompt"),
    ("MCP server: shangyi", "MCP 服务器段落"),
]:
    if needle not in system_text:
        problems.append(f"system prompt 缺 {label}")

expected_tools = {
    "mcp__shangyi__solve_equilibria",
    "mcp__shangyi__kb_retrieve",
    "list_mcp_resources",
    "list_mcp_resource_templates",
    "read_mcp_resource",
}
if set(tools) != expected_tools:
    extra = sorted(set(tools) - expected_tools)
    missing = sorted(expected_tools - set(tools))
    problems.append(f"工具面不符：多出 {extra} 缺 {missing}")

coding = {"bash", "pwsh", "read", "write", "edit", "glob", "grep", "jobs",
          "web_search", "web_fetch", "subagent", "subagent_fork", "workflow",
          "todo_write", "skill", "working_directory", "exit_plan_mode"}
leaked = sorted(coding & set(tools))
if leaked:
    problems.append(f"编码类工具仍在工具面里：{leaked}")

if context.get("provider") != "deepseek-official" or context.get("model") != "deepseek-flash":
    problems.append(f"模型路由不符：{context}")

if problems:
    for problem in problems:
        print(f"      原因：{problem}")
    sys.exit(1)
print(f"system prompt {len(system_text)} 字符；模型可见工具面 {len(tools)} 个：{sorted(tools)}")
print(f"模型路由：{context.get('provider')}/{context.get('model')} "
      f"contextWindow={context.get('contextWindow')}")
PY
    then
      pass "装配层已核：persona + MCP instructions 进了 system prompt，工具面恰好 5 个且无编码类工具"
    else
      fail "装配层核验失败" "见上面逐条原因（session 日志：$SESSION_ZST）"
    fi
  fi
fi

# ── 汇总 ──────────────────────────────────────────────────────────────────────
printf '\n'
if [ "$FAILURES" -eq 0 ]; then
  printf '\033[32m冒烟通过\033[0m：%d 项断言全过（临时 DSH_HOME：%s）\n' "$PASSES" "$SMOKE_HOME"
  exit 0
fi
printf '\033[31m冒烟失败\033[0m：%d 项失败 / %d 项通过（日志在 %s）\n' "$FAILURES" "$PASSES" "$SMOKE_HOME"
exit 1

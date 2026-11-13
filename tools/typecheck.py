#!/usr/bin/env python3
"""前端类型体检：跑 tsc --noEmit 检查已自愿加入的前端文件。

用法：python3 tools/typecheck.py

# 为什么需要这个工具（而不是只写进文档）

类型检查用 `// @ts-check` + `tsc --noEmit` 接入，**不加构建步骤**：
`noEmit` 不产出任何文件，`js/**/*.js` 仍是浏览器与 `node --test` 直接执行的
源文件，分发方式不变（`python3 shangyi/sidecar/web_console.py` 即可运行）。

但这条规则有个反直觉的后果，容易被后来者踩：

| 写法 | 谁被检查 |
|---|---|
| `checkJs: true` | **全部** 17 个文件，包括还没准备好的 |
| `checkJs: false` + `// @ts-check` | **只有**声明了这个注释的文件 |

`tsconfig.json` 用的是后者。所以本工具**只对已加入的文件判失败**，
未加入文件的报错原样打印但不计入退出码——否则新加一个视图文件，
还没来得及收口 querySelector 噪声，整个检查就红了。

实测踩到：初次按 `checkJs: true` 跑，46 个错误全部来自未加入的 8 个文件，
其中 37 个是 `.value`——`querySelector()` 返回 `Element`，`Element` 上
本就没有 `value`，纯属噪声。逐个 cast 消灭它是浪费，见设计文档 §3.3。

# 依赖

需要 `tsc`。本项目**不提交** node_modules（见设计文档第 4 节），
故按以下顺序找：环境变量 TS_BIN → web/node_modules → 全局 tsc → npx。

⚠️ `npm install` 在本机会报 EPERM：全局缓存目录里有 root 属主的文件。
   绕过：`npm install --cache /tmp/npm-cache typescript`
   或直接用全局安装的 tsc。
"""
import io
import os
import re
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WEB = os.path.join(ROOT, "shangyi", "sidecar", "web")
TSC = os.path.join(WEB, "node_modules", ".bin", "tsc")

ERR_RE = re.compile(r"^(.+?)\((\d+),(\d+)\): error (TS\d+): (.*)$")


def find_tsc():
    """按优先级找一个可用的 tsc。返回 (路径或None, 说明)。"""
    env = os.environ.get("TS_BIN")
    if env:
        return (env if os.path.exists(env) else shutil.which(env)), "环境变量 TS_BIN"
    if os.path.exists(TSC):
        return TSC, "web/node_modules/.bin/tsc"
    which = shutil.which("tsc")
    if which:
        return which, "全局 tsc"
    return None, "未找到"


def opted_in():
    """已声明 // @ts-check 的文件（相对 web/）。"""
    out = []
    for base, _, files in os.walk(os.path.join(WEB, "js")):
        for name in files:
            # `._*` 是 AppleDouble 元数据伴随文件，不是源码
            if name.startswith("._") or not name.endswith(".js"):
                continue
            p = os.path.join(base, name)
            with io.open(p, encoding="utf-8") as fh:
                if fh.readline().strip() == "// @ts-check":
                    out.append(os.path.relpath(p, WEB))
    return sorted(out)


def main():
    tsc, origin = find_tsc()
    if not tsc:
        print("  ✗ 找不到 tsc，无法类型体检。")
        print("    装一个即可（不必提交 node_modules）：")
        print("      npm install --cache /tmp/npm-cache typescript")
        print("    或全局安装后重试，或用 TS_BIN=/path/to/tsc 指定。")
        return 2

    checked = opted_in()
    print(f"  tsc: {tsc}（{origin}）")
    print(f"  已加入检查的文件：{len(checked)} 个")

    proc = subprocess.run(
        [tsc, "--noEmit", "-p", os.path.join(WEB, "tsconfig.json")],
        cwd=WEB, capture_output=True, text=True,
    )
    mine, others = [], []
    for line in proc.stdout.splitlines():
        m = ERR_RE.match(line.strip())
        if not m:
            continue
        (mine if os.path.relpath(os.path.normpath(os.path.join(WEB, m.group(1))), WEB) in checked
         else others).append(line.strip())

    for line in mine:
        print("  ✗ " + line)
    if others:
        print(f"\n  以下 {len(others)} 条来自尚未加入 @ts-check 的文件，不计入失败：")
        for line in others[:5]:
            print("    · " + line)
        if len(others) > 5:
            print(f"    · …另有 {len(others) - 5} 条")

    if proc.returncode != 0 and not mine:
        print("\n  ✗ tsc 非零退出但未匹配到错误行，请手动运行上面那条命令查看。")
        return 1
    if mine:
        print(f"\n  共 {len(mine)} 处需修")
        return 1
    print("  ✓ 已加入的文件全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
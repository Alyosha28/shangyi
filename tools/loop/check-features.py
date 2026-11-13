#!/usr/bin/env python3
"""护栏：`serde_json/arbitrary_precision` 只允许出现在**测试构建**，不得进入正式依赖图。

## 背景（为什么需要这个门）

R1（2026-11-10）之前，该 feature 由上游 vendor crate 带进整个 workspace 的依赖图，
于是"含数值字段的 internally-tagged / untagged 枚举会报 `invalid type: map,
expected f64`"是一条真实踩过的坑（见 `shangyi/README.md` 的序列化规则章节）。
R1 摘除基座后该 feature 一度从全图消失 ⇒ 那条规则**失去护栏**：把枚举改回
internally-tagged，没有任何测试会失败（审计 P1-1）。

现在它被 `shangyi-gamespec` / `shangyi-intel` 的 `[dev-dependencies]` 显式启用：
**测试构建命中是预期**（护栏有牙），**正式依赖图命中则是回归**（release 的数字
解析语义被改动、`v.clone()` 保留字面量精度之类的理由会重新变得重要）。

## 判据（双向，缺一不可）

1. **正式图**：`cargo tree --workspace -e features,no-dev` **不得**出现
   `arbitrary_precision`。出现 ⇒ 某个正常依赖（`[dependencies]`）把它打开了，
   release 构建的数字表示语义随之改变。
2. **测试图**：`cargo tree --workspace -e features` **必须**出现
   `arbitrary_precision`。不出现 ⇒ 保住护栏的那条 `[dev-dependencies]` 掉了/被删了，
   规则又退化成"靠人遵守"——这正是 P1-1 的形态，必须失败而不是静默。

为什么用 `-e features,no-dev` 而不是 `cargo tree --no-dev`：cargo **没有**
`--no-dev` 开关（实测 `cargo tree --no-dev` → Usage 错误）；`no-dev` 只是
`--edges` 的一个取值。`--offline` 是为了让本门不依赖网络（registry 已缓存即可）。

## 退出码

- `0` = 两个方向都符合
- `1` = 违规（正式图命中 / 测试图未命中）
- `2` = `cargo tree` 跑不起来（例如 workspace 暂时不可解析、缺少 registry 缓存）

用法: `python3 tools/loop/check-features.py [-v]`
"""
import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
TOKEN = "arbitrary_precision"


def run_tree(edges):
    """跑一次 cargo tree，返回 (退出码, stdout, stderr)。"""
    cmd = ["cargo", "tree", "--workspace", "-e", edges, "--offline"]
    env = dict(os.environ)
    env.setdefault("CARGO_TARGET_DIR", "/tmp/shangyi-target")
    proc = subprocess.run(cmd, cwd=ROOT, capture_output=True, text=True, env=env)
    return proc.returncode, proc.stdout, proc.stderr


def lines_with(text, token):
    return [ln.rstrip() for ln in text.splitlines() if token in ln]


def main(argv):
    verbose = "-v" in argv[1:] or "--verbose" in argv[1:]

    rc_formal, out_formal, err_formal = run_tree("features,no-dev")
    rc_test, out_test, err_test = run_tree("features")
    if rc_formal != 0 or rc_test != 0:
        print("[check-features] ✗ cargo tree 跑不起来，无法判定（退出 2）")
        for label, rc, err in (
            ("features,no-dev", rc_formal, err_formal),
            ("features", rc_test, err_test),
        ):
            if rc != 0:
                print(f"  - `-e {label}` exit={rc}: {err.strip().splitlines()[-1] if err.strip() else '(无 stderr)'}")
        return 2

    formal_hits = lines_with(out_formal, TOKEN)
    test_hits = lines_with(out_test, TOKEN)

    if verbose:
        print(
            f"[check-features] 正式图（-e features,no-dev）命中 {len(formal_hits)} 处；"
            f"测试图（-e features）命中 {len(test_hits)} 处"
        )

    problems = []
    if formal_hits:
        problems.append(
            f"正式依赖图命中 `{TOKEN}` {len(formal_hits)} 处（只允许在测试构建）：\n"
            + "\n".join(f"      {ln}" for ln in formal_hits[:10])
        )
    if not test_hits:
        problems.append(
            f"测试图**未**命中 `{TOKEN}`：护栏已失牙——`shangyi-gamespec` / `shangyi-intel` 的 "
            "`[dev-dependencies]` 是否还在？规则会退化成靠人遵守（P1-1 形态）。"
        )

    if problems:
        print(f"[check-features] ✗ 违规 {len(problems)} 项")
        for p in problems:
            print(f"  - {p}")
        return 1

    print(
        f"[check-features] ✓ 正式图干净（0 处 `{TOKEN}`）；测试图已武装"
        f"（{len(test_hits)} 处，预期）"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

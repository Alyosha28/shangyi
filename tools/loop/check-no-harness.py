#!/usr/bin/env python3
"""护栏：内核依赖图里不得出现任何外部 harness／基座。

扫描面（与 `shangyi-core` 的 `no_external_harness_in_manifests` 测试一致）：
  - 根 `Cargo.toml`
  - 根 `Cargo.lock`
  - `shangyi/**/Cargo.toml`
另加一条结构性检查：`vendor/` 目录不得存在（见下）。

命中以下任一符号即违规（词边界匹配，大小写不敏感）：
  codex / rama / openai-oss-forks

为什么是这三类：R1 摘除的 vendor 基座由三块碎片组成——上游基线镜像的
`codex-*` crate、为解析它而抄进 workspace 的整组 `rama-*` 钉版、以及把
crossterm / tungstenite 指到 `openai-oss-forks` 的 git patch。任何一个回来
都意味着基座在回归，故三者一并封死。

## 判据：只查「依赖声明」，不查「注释」

`Cargo.toml` 逐行**先剥行内注释再匹配**（`strip_toml_comment`）。理由：
  - 注释是解释，依赖声明才是边。历史上"为何自研而非复用某基座"这类注释是
    设计理由，删掉它等于丢失决策依据；
  - 真正的回归一定体现为未注释的依赖 / 钉版 / patch 行，剥注释不会漏判；
  - 被注释掉的旧依赖行同样不构成边（它不参与解析），不应误报。
`Cargo.lock` 没有注释，整行匹配。
`vendor/` 目录存在本身就失败：R1 已摘除该目录，它回来一定会带回上面三者。

`research/` 下的独立实验 workspace 不在扫面内：它有自己的 Cargo.lock，
不属于内核依赖图；若它重新依赖 shangyi 的 crate，回归仍会从
`shangyi/**/Cargo.toml` 侧暴露。

退出码：
  0 = 干净
  1 = 有命中（CI 应失败）
  2 = 扫描面异常（必需的 manifest / lock 读不到）

用法:
  python3 tools/loop/check-no-harness.py [-v]
"""
import re
import sys
import pathlib

ROOT = pathlib.Path(__file__).resolve().parents[2]

# 与 shangyi-core/src/lib.rs 的 tests::FORBIDDEN 保持一致——两处都要改。
FORBIDDEN = ("codex", "rama", "openai-oss-forks")

# 词边界：前后不得是字母/数字/下划线，故 `param`、`drama` 不算，
# `rama-core`、`codex-core-api`、`openai-oss-forks/crossterm` 算。
PATTERN = re.compile(
    r"(?<![a-z0-9_])(?:" + "|".join(re.escape(s) for s in FORBIDDEN) + r")(?![a-z0-9_])",
    re.IGNORECASE,
)


def strip_toml_comment(line):
    """去掉 TOML 的行内注释：不在引号内的 `#` 起算。"""
    out = []
    quote = None
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        if quote is not None:
            if c == "\\" and quote == '"':
                out.append(c)
                i += 1
                if i < n:
                    out.append(line[i])
                    i += 1
                continue
            if c == quote:
                quote = None
            out.append(c)
        elif c == "#":
            break
        else:
            if c in ('"', "'"):
                quote = c
            out.append(c)
        i += 1
    return "".join(out)


def scan_targets():
    """返回 (待扫文件列表, 缺失的必需文件列表)。"""
    required = [ROOT / "Cargo.toml", ROOT / "Cargo.lock"]
    missing = [p for p in required if not p.is_file()]
    files = [p for p in required if p.is_file()]
    files += sorted((ROOT / "shangyi").rglob("Cargo.toml"))
    return files, missing


def scan_file(path):
    """返回 [(行号, 命中符号, 原始行)]；判据见模块文档。"""
    text = path.read_text(encoding="utf-8", errors="replace")
    is_lock = path.name == "Cargo.lock"
    hits = []
    for lineno, line in enumerate(text.splitlines(), start=1):
        subject = line if is_lock else strip_toml_comment(line)
        # 同一行同一符号只报一次（`codex-core-api = { path = "../vendor/codex/..." }`
        # 会命中多次，但那是同一条依赖边）。
        symbols = []
        for m in PATTERN.finditer(subject):
            sym = m.group(0).lower()
            if sym not in symbols:
                symbols.append(sym)
        for sym in symbols:
            hits.append((lineno, sym, line.strip()))
    return hits


def main(argv):
    verbose = "-v" in argv[1:] or "--verbose" in argv[1:]

    files, missing = scan_targets()
    if missing:
        for p in missing:
            print(f"[check-no-harness] ✗ 必需文件不存在: {p}")
        print("  → 扫描面失效，不能判定为干净（退出 2）")
        return 2
    if len(files) < 3:  # 根 manifest + lock + 至少一个 member manifest
        print(f"[check-no-harness] ✗ 只扫到 {len(files)} 个文件，扫描面失效（退出 2）")
        return 2

    hits = []
    for path in files:
        try:
            file_hits = scan_file(path)
        except OSError as exc:
            print(f"[check-no-harness] ✗ 读不到 {path}: {exc}")
            return 2
        for lineno, symbol, line in file_hits:
            hits.append((path, lineno, symbol, line))

    vendor = ROOT / "vendor"
    vendor_note = ""
    if vendor.exists():
        vendor_note = "vendor/ 目录存在（R1 已摘除该基座目录）"

    if verbose:
        print(
            f"[check-no-harness] 扫描 {len(files)} 个文件"
            "（根 Cargo.toml / Cargo.lock / shangyi/**/Cargo.toml，只查依赖声明）"
        )
    print(f"[check-no-harness] 扫描 {len(files)} 个文件，符号命中 {len(hits)} 处"
          + ("，vendor/ 存在" if vendor_note else "，vendor/ 不存在"))

    if not hits and not vendor_note:
        print("  ✓ 无外部 harness／基座残留")
        return 0

    if hits:
        print("\n  ✗ 违规命中（内核不得依赖外部 harness／基座）:")
        for path, lineno, symbol, line in hits:
            try:
                rel = path.relative_to(ROOT)
            except ValueError:
                rel = path
            print(f"      {rel}:{lineno}: {line}")
            print(f"          ← 命中 `{symbol}`（依赖声明区）")
    if vendor_note:
        print(f"\n  ✗ {vendor_note}")
    print("\n  → 移除该依赖/钉版/patch、删掉 vendor/ 后重跑；R1 规格见 task-1。")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))

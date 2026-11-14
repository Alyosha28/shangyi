#!/usr/bin/env python3
"""防漂移门禁：核对工具数量、CLI 子命令数量、活跃文档与 DSH 配置面的一致性。

真相源（仅解析，不导入或编译 Rust）：
  1. `shangyi/crates/shangyi-tools/src/lib.rs`：`ToolRegistry::standard()` 注册的工具总数
     （若存在 `STANDARD_TOOL_COUNT` 常量则一并交叉校验）。
  2. `shangyi/crates/shangyi-cli/src/lib.rs`：`enum Command` 的子命令变体总数。

退出码：
  0 = 一致且无陈旧死词命中
  1 = 发现口径漂移或陈旧死词
  2 = 真相源或必需文档缺失/不可解析

用法：
  python3 tools/loop/check-docs-sync.py [-v]
"""
from __future__ import annotations

import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
SKIP_DIRS = {".git", ".git.mountbak", ".pnpm-store", "node_modules", "target", "__pycache__"}

CN_NUM_WORDS = {
    5: "五",
    6: "六",
    7: "七",
    8: "八",
    9: "九",
    10: "十",
}


def read_text(rel: str) -> str:
    p = ROOT / rel
    if not p.is_file():
        raise FileNotFoundError(rel)
    return p.read_text(encoding="utf-8")


def extract_braced_block(text: str, anchor_re: str) -> str:
    m = re.search(anchor_re, text)
    if not m:
        raise ValueError(f"missing anchor: {anchor_re}")
    start = text.find("{", m.end() - 1)
    if start < 0:
        raise ValueError(f"missing opening brace for: {anchor_re}")
    depth = 0
    in_str = False
    escape = False
    i = start
    while i < len(text):
        ch = text[i]
        if in_str:
            if escape:
                escape = False
            elif ch == "\\":
                escape = True
            elif ch == '"':
                in_str = False
        else:
            if ch == '"':
                in_str = True
            elif ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return text[start : i + 1]
        i += 1
    raise ValueError(f"unclosed block for: {anchor_re}")


def parse_standard_tool_count() -> int:
    src = read_text("shangyi/crates/shangyi-tools/src/lib.rs")
    fn_body = extract_braced_block(src, r"pub\s+fn\s+standard\s*\(\s*\)\s*->\s*Self\s*\{")
    vec_m = re.search(r"let\s+specs\b[\s\S]*?vec!\s*\[([\s\S]*?)\]\s*;", fn_body)
    if not vec_m:
        raise ValueError("cannot locate specs vec![] in ToolRegistry::standard()")
    names = []
    for raw_line in vec_m.group(1).splitlines():
        line = raw_line.strip()
        if not line or line.startswith("//"):
            continue
        m = re.match(r'^\(\s*"([a-z0-9_]+)"\s*,', line)
        if m:
            names.append(m.group(1))
    if not names:
        raise ValueError("parsed 0 tools from ToolRegistry::standard()")
    count = len(names)
    const_m = re.search(r"pub\s+const\s+STANDARD_TOOL_COUNT\s*:\s*usize\s*=\s*(\d+)\s*;", src)
    if const_m and int(const_m.group(1)) != count:
        raise ValueError(
            f"STANDARD_TOOL_COUNT={const_m.group(1)} disagrees with ToolRegistry::standard() count={count}"
        )
    return count


def parse_command_count() -> int:
    src = read_text("shangyi/crates/shangyi-cli/src/lib.rs")
    enum_body = extract_braced_block(src, r"pub\s+enum\s+Command\s*\{")
    inner = enum_body[1:-1]
    depth = 0
    variants = 0
    for raw_line in inner.splitlines():
        line = re.sub(r"//.*", "", raw_line).strip()
        if not line:
            continue
        if depth == 0 and re.match(r"^[A-Z][A-Za-z0-9_]*\s*(?:\{|\(|,|//|$)", line):
            variants += 1
        depth += line.count("{") - line.count("}")
    if variants == 0:
        raise ValueError("parsed 0 Command variants")
    return variants


def check_required_patterns(tool_count: int, cmd_count: int) -> list[str]:
    cn_cmd = CN_NUM_WORDS.get(cmd_count, str(cmd_count))
    checks: list[tuple[str, str, str]] = [
        ("README.md", rf"暴露\s+{tool_count}\s+个领域工具", f"README.md 应标注暴露 {tool_count} 个领域工具"),
        ("dsh/README.md", rf"全部\s+{tool_count}\s+个确定性领域工具", f"dsh/README.md 应标注全部 {tool_count} 个确定性领域工具"),
        ("dsh/README.md", rf"共\s+{cmd_count}\s+个子命令", f"dsh/README.md 应标注共 {cmd_count} 个子命令"),
        ("docs/商奕-系统设计文档-v0.3.md", rf"Tools:\s+{tool_count}\s+领域工具", f"设计文档 v0.3 L70 应标注 Tools: {tool_count} 领域工具"),
        ("docs/商奕-系统设计文档-v0.3.md", rf"{tool_count}\s+个标准工具规格", f"设计文档 v0.3 应标注 {tool_count} 个标准工具规格"),
        ("docs/商奕-系统设计文档-v0.3.md", r"shangyi\s+start\s*\(\s*ui\s*\)", "设计文档 v0.3 `shangyi-cli` 行应包含 `start (ui)`"),
        ("shangyi/crates/shangyi-cli/Cargo.toml", rf"把\s+{tool_count}\s+个领域工具", f"shangyi-cli/Cargo.toml 应标注 {tool_count} 个领域工具"),
        ("shangyi/crates/shangyi-cli/Cargo.toml", rf"(?:{cn_cmd}|{cmd_count})\s*个子命令", f"shangyi-cli/Cargo.toml 应标注 {cn_cmd}/{cmd_count} 个子命令"),
        ("shangyi/crates/shangyi-cli/src/start_cmd.rs", rf"{tool_count}\s+项确定性工具", f"start_cmd.rs 应标注 {tool_count} 项确定性工具"),
        ("shangyi/crates/shangyi-mcp/src/lib.rs", rf"ToolRegistry（{tool_count}\s+个工具）", f"shangyi-mcp/src/lib.rs 应标注 ToolRegistry（{tool_count} 个工具）"),
        ("shangyi/crates/shangyi-mcp/scripts/dsh-client-probe.mjs", rf"tools\.length\s*===\s*{tool_count}\b", f"dsh-client-probe.mjs 应断言 tools.length === {tool_count}"),
        ("shangyi/ui/dsh-plugin-shangyi-workbench/scripts/verify.mjs", rf"mcpTools:\s*\{{[^}}]*count:\s*{tool_count}\b", f"workbench verify.mjs 健康模拟夹具应使用 count: {tool_count}"),
    ]

    failures: list[str] = []
    for rel, pattern, desc in checks:
        text = read_text(rel)
        if not re.search(pattern, text):
            failures.append(f"[COUNT-DRIFT] {desc}（未匹配 `{pattern}` in `{rel}`）")
    return failures


def iter_scan_files() -> list[pathlib.Path]:
    targets = [
        ROOT / "README.md",
        ROOT / "AGENTS.md",
        ROOT / "docs" / "商奕-系统设计文档-v0.3.md",
        ROOT / "shangyi" / "sidecar" / "web" / "README.md",
        ROOT / "shangyi" / "ui" / "WORKBENCH-CONTRACT.md",
    ]
    roots = [
        ROOT / "dsh",
        ROOT / "tools",
        ROOT / "shangyi" / "crates",
        ROOT / "shangyi" / "ui" / "dsh-plugin-shangyi",
        ROOT / "shangyi" / "ui" / "dsh-plugin-shangyi-shell",
        ROOT / "shangyi" / "ui" / "dsh-plugin-shangyi-workbench",
    ]
    exts = {".md", ".yml", ".yaml", ".sh", ".py", ".rs", ".toml", ".js", ".mjs"}
    self_path = pathlib.Path(__file__).resolve()
    files: list[pathlib.Path] = []
    for p in targets:
        if p.is_file():
            files.append(p)
    for base in roots:
        if not base.exists():
            continue
        for p in sorted(base.rglob("*")):
            if any(part in SKIP_DIRS for part in p.parts):
                continue
            if p.name.startswith("._") or not p.is_file():
                continue
            if p.suffix.lower() not in exts:
                continue
            if p.resolve() == self_path:
                continue
            files.append(p)
    return files


def check_banned_identifiers() -> list[str]:
    banned = [
        ("report_compile", re.compile(r"\breport_compile\b"), None),
        ("session_configure", re.compile(r"\bsession_configure\b"), None),
        ("poi_lookup", re.compile(r"\bpoi_lookup\b"), None),
        ("reopen_task", re.compile(r"\breopen_task\b"), re.compile(r"不存在的\s*reopen_task")),
        ("5 个子命令", re.compile(r"5\s*个子命令"), None),
        ("五个子命令", re.compile(r"五个子命令"), None),
        ("searxng.env", re.compile(r"searxng\.env"), None),
        ("R2 才实现 mcp-serve", re.compile(r"R2\s*才实现\s*mcp-serve"), None),
    ]
    failures: list[str] = []
    for path in iter_scan_files():
        rel = path.relative_to(ROOT).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except UnicodeDecodeError:
            continue
        for lineno, line in enumerate(text.splitlines(), 1):
            for label, pat, allow in banned:
                if pat.search(line):
                    if allow and allow.search(line):
                        continue
                    failures.append(f"[STALE-ID] {rel}:{lineno} 命中禁用旧标识 `{label}`：{line.strip()}")
    return failures


def main() -> int:
    verbose = "-v" in sys.argv[1:]
    try:
        tool_count = parse_standard_tool_count()
        cmd_count = parse_command_count()
    except Exception as exc:
        print(f"check-docs-sync: ERROR parsing truth sources: {exc}", file=sys.stderr)
        return 2

    if verbose:
        print(f"Truth sources: ToolRegistry::standard()={tool_count}, Command variants={cmd_count}")

    try:
        failures = check_required_patterns(tool_count, cmd_count) + check_banned_identifiers()
    except FileNotFoundError as exc:
        print(f"check-docs-sync: MISSING file {exc}", file=sys.stderr)
        return 2

    if failures:
        print(f"check-docs-sync: FAIL ({len(failures)} issue(s))", file=sys.stderr)
        for item in failures:
            print(f"  - {item}", file=sys.stderr)
        return 1

    print(f"check-docs-sync: OK (tools={tool_count}, subcommands={cmd_count}, 0 stale hits)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

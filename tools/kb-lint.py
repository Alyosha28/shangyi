#!/usr/bin/env python3
"""知识库 frontmatter 体检：给含 YAML 特殊字符的标量补引号。

用法：python3 tools/kb-lint.py [--fix] <kb目录>

# 为什么需要这个工具（而不是只写进文档）

Markdown 与 YAML 在这几个字符上是冲突的：

| 字符 | Markdown 里 | YAML 里 |
|---|---|---|
| `*` | 加粗 `**粗**` | **别名引用**（`*anchor`） |
| `[` | 链接 `[[页]]` | **流式序列起始** |
| `:` + 空格 | 普通 | **键值分隔** |

于是最自然的写法（在 YAML 里写 Markdown 加粗、写 wiki 链接）会解析失败，
而报错（`did not find expected alphabetic character while scanning an alias`）
**完全不会提到 Markdown 或链接**——它只提"别名"，而作者根本没写别名。

实测：写第一批 14 页知识时踩了 6 次。故做成工具，
并在 `shangyi-kb::wiki` 的文档里写明这个陷阱。
"""
import io, os, re, sys


def fm_range(lines):
    if not lines or lines[0].strip() != '---':
        return None
    for i in range(1, len(lines)):
        if lines[i].strip() == '---':
            return (0, i)
    return None


def needs_quote(val: str) -> bool:
    """判断一个 YAML 标量是否需要加引号。

    ⚠️ **必须检查"整行被引号包裹"，而不是"以引号开头"**。

    实测踩到：`- "先动"只是宣告而无承诺能力——**…**，见下` 以 `"` 开头，
    早先的判据据此认为"已有引号"而跳过它。但那是不对称引号
    （`"先动"` 之后还有裸文本），YAML 会把它当引号串直到下一个 `"`，
    后面的内容就成了语法错误——而错误信息只报"did not find expected '-'"。
    """
    if not val or val in ('|', '>'):
        return False
    # 整行被同一对引号包裹 ⇒ 安全
    quoted = len(val) >= 2 and val[0] == val[-1] and val[0] in "'\""
    if quoted:
        return False
    return ('**' in val) or ('[[' in val) or ('\"' in val) or (': ' in val) or val.endswith(':')


def fix_file(path, apply):
    lines = io.open(path, encoding='utf-8').read().split('\n')
    r = fm_range(lines)
    if not r:
        return []
    _, end = r
    changed = []
    for i in range(1, end):
        l = lines[i]
        m = re.match(r'^(\s*-\s+)(.*)$', l)
        if m and needs_quote(m.group(2)):
            lines[i] = m.group(1) + "'" + m.group(2).replace("'", "''") + "'"
            changed.append(i + 1)
            continue
        m2 = re.match(r'^(\s*[A-Za-z_][A-Za-z0-9_]*:\s+)(.*)$', l)
        if m2 and needs_quote(m2.group(2)):
            lines[i] = m2.group(1) + "'" + m2.group(2).replace("'", "''") + "'"
            changed.append(i + 1)
    if changed and apply:
        io.open(path, 'w', encoding='utf-8').write('\n'.join(lines))
    return changed


def main():
    args = [a for a in sys.argv[1:] if not a.startswith('--')]
    apply = '--fix' in sys.argv
    root = args[0] if args else 'shangyi/kb'
    total = 0
    for name in sorted(os.listdir(root)):
        if not name.endswith('.md') or name.startswith('._') or name.startswith('.'):
            continue
        p = os.path.join(root, name)
        ch = fix_file(p, apply)
        if ch:
            total += len(ch)
            verb = "已修" if apply else "需修"
            print(f"  {verb} {name}: 行 {ch}")
    if total == 0:
        print("  ✓ 全部标量都是引号安全的")
    else:
        print(f"\n  共 {total} 处" + ("" if apply else "（用 --fix 修复）"))
    return 0 if (total == 0 or apply) else 1


if __name__ == '__main__':
    sys.exit(main())

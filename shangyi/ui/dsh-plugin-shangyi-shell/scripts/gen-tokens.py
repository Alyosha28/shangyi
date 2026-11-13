#!/usr/bin/env python3
"""由 DSH 默认主题的两份 token 快照，推导商奕三个视觉方向的整套 alias token。

输入：
  research/ui/2026-11-11-shangyi-shell/tokens-light.json / tokens-dark.json
  —— 来自验证实例里 getComputedStyle(document.body) 的实测值（107 个 --dsw-alias-*）。

输出：
  shangyi/ui/dsh-plugin-shangyi-shell/lib/tokens.gen.js
  —— 三个方向 × light/dark 的 token 表，供 lib/client.js 通过 ctx.theme.overrideTokens 叠加。

推导规则（不是随手挑色）：
  1) 中性面（bg/fill/card）按原 token 的相对明度映射到该方向的「纸面阶梯」，
     阶梯端点由方向定义（见 RAMPS），所以原设计里越亮的面上，商奕版也越亮；
  2) 墨色（label/icon/menu-icon）同样按明度映射到该方向的「墨阶梯」，保持原对比关系；
  3) 边框 = 墨色 + 原 alpha；
  4) 主色位（brand/link/button-primary/onboarding）换到该方向的主色；
  5) 语义色（error/success/warn/diff）保留色相，只做暖化与明度微调；
  6) 半透明遮罩、color-mix 表达式保留结构，只替换其中的字面色。
"""
import json
import re
import os

BASE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(BASE, "..", "..", "..", ".."))
DUMP = os.path.join(ROOT, "research", "ui", "2026-11-11-shangyi-shell")
OUT = os.path.join(ROOT, "shangyi", "ui", "dsh-plugin-shangyi-shell", "lib", "tokens.gen.js")

L = json.load(open(os.path.join(DUMP, "tokens-light.json")))
D = json.load(open(os.path.join(DUMP, "tokens-dark.json")))

# ── 三个方向的纸面阶梯（由深到浅：base → layer1 → layer2 → layer3/overlay） ──
RAMPS = {
    # A 宣纸·墨玉
    "paper": {
        "light": {
            "paper": ["#EFE9DB", "#F5F1E7", "#FAF6EC", "#FFFCF5"],
            "ink": ["#1B1812", "#3A342A", "#5C5446", "#8A8170", "#B4AC9A"],
            "accent": "#1F6B5C",
        },
        "dark": {
            "paper": ["#0E0F0B", "#15160F", "#1D1E16", "#26271E"],
            "ink": ["#EDE7D6", "#CDC6B4", "#A8A190", "#827C6C", "#5C574A"],
            "accent": "#57B49B",
        },
    },
    # B 黑漆·鎏金（暗色是主场景，浅色是同一套金线的纸面版）
    "lacquer": {
        "light": {
            "paper": ["#EAE3D3", "#F2EDE2", "#F9F5EC", "#FFFCF4"],
            "ink": ["#191510", "#382F22", "#5B5040", "#8B806C", "#B5AB97"],
            "accent": "#8A6A2F",
        },
        "dark": {
            "paper": ["#0B0A08", "#111010", "#181612", "#201C16"],
            "ink": ["#EDE6D6", "#CFC4AE", "#A99C84", "#83775F", "#5B5343"],
            "accent": "#C6A15B",
        },
    },
    # C 账册·铜版
    "ledger": {
        "light": {
            "paper": ["#EDE6D3", "#F4EFE1", "#FAF6EB", "#FFFDF3"],
            "ink": ["#141A12", "#333A2E", "#57604D", "#878C74", "#B2B7A2"],
            "accent": "#245447",
        },
        "dark": {
            "paper": ["#0D0F0A", "#13160F", "#1A1E14", "#22271B"],
            "ink": ["#E9E7D5", "#C9CBB4", "#A3A88E", "#7D836C", "#575C4C"],
            "accent": "#7FB79F",
        },
    },
}

HEX3 = re.compile(r"^#([0-9a-fA-F]{3})$")
HEX6 = re.compile(r"^#([0-9a-fA-F]{6})$")
HEX8 = re.compile(r"^#([0-9a-fA-F]{8})$")


def parse(c):
    c = c.strip()
    m = HEX3.match(c)
    if m:
        s = m.group(1)
        return [int(s[0] * 2, 16), int(s[1] * 2, 16), int(s[2] * 2, 16), 1.0]
    m = HEX6.match(c)
    if m:
        s = m.group(1)
        return [int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16), 1.0]
    m = HEX8.match(c)
    if m:
        s = m.group(1)
        return [int(s[0:2], 16), int(s[2:4], 16), int(s[4:6], 16), int(s[6:8], 16) / 255]
    return None


def hexof(rgb, alpha=None):
    r, g, b = [max(0, min(255, round(v))) for v in rgb[:3]]
    a = rgb[3] if alpha is None else alpha
    if a >= 0.999:
        return "#%02X%02X%02X" % (r, g, b)
    return "#%02X%02X%02X%02X" % (r, g, b, max(0, min(255, round(a * 255))))


def srgb_to_lin(v):
    v = v / 255
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def lin_to_srgb(v):
    v = max(0.0, min(1.0, v))
    return 12.92 * v if v <= 0.0031308 else 1.055 * (v ** (1 / 2.4)) - 0.055


def lum(rgb):
    r, g, b = [srgb_to_lin(v) for v in rgb[:3]]
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def from_lum(target, chroma_hex):
    """在给定亮度的灰阶上，按另一颜色的色度微调，得到有方向感的同亮度色。"""
    t = [lin_to_srgb(lum(chroma_hex) * 0 + 0)]  # placeholder, unused
    return None


def mix(a, b, t):
    return [a[i] + (b[i] - a[i]) * t for i in range(3)] + [1.0]


def ramp_lookup(stops, x):
    """把 0..1 的位置映射到阶梯（按字符串数组线性插值）。"""
    x = max(0.0, min(1.0, x))
    pos = [i / (len(stops) - 1) for i in range(len(stops))]
    for i in range(len(stops) - 1):
        if x <= pos[i + 1]:
            t = (x - pos[i]) / (pos[i + 1] - pos[i])
            return mix(parse(stops[i]), parse(stops[i + 1]), t)
    return parse(stops[-1])


def neutral_x(rgb, dark):
    """把原 token 的明度折算成 0..1 的阶梯位置。"""
    lv = lum(rgb) ** (1 / 2.2)  # 感知明度
    if dark:
        # 暗色：0.03 最底 → 0.35 最高面
        return (lv - 0.02) / 0.36
    # 亮色：0.86 最底 → 1.0 最高面
    return (lv - 0.86) / 0.15


def ink_x(rgb, dark):
    """墨阶梯的位置：0 = 该明暗下最深的墨。两套阶梯都从深到浅排。"""
    lv = lum(rgb) ** (1 / 2.2)
    if dark:
        # 暗色：最亮的是 0 号位（#EDE7D6），所以按「离最亮的距离」取位置
        return 1 - (lv - 0.5) / 0.42
    return (lv - 0.06) / 0.42


def warmth(rgb, k):
    """把颜色往暖里推一点点（只动色相感觉，不动明度）：R 略升、B 略降。"""
    return [rgb[0] + 6 * k, rgb[1] + 2 * k, rgb[2] - 7 * k]


SURFACE = {
    "--dsw-alias-bg-base", "--dsw-alias-bg-layer-1", "--dsw-alias-bg-layer-2",
    "--dsw-alias-bg-layer-3", "--dsw-alias-bg-overlay", "--dsw-alias-bg-module-platform",
    "--dsw-alias-bg-multi-select", "--dsw-alias-bg-document-preview",
    "--dsw-alias-settings-card-fill", "--dsw-alias-markdown-code-block",
    "--dsw-alias-markdown-code-block-banner", "--dsw-alias-markdown-inline-code",
    "--dsw-alias-markdown-placeholder", "--dsw-alias-markdown-tag",
    "--dsw-alias-markdown-citation", "--dsw-alias-markdown-code-segment-selected",
    "--dsw-alias-markdown-code-segment-unselected", "--dsw-alias-button-elevated-fill",
    "--dsw-alias-button-floating-fill", "--dsw-alias-button-floating-hover",
    "--dsw-alias-button-ghost-active-fill", "--dsw-alias-button-ghost-active-hover",
    "--dsw-alias-button-primary-dimmed", "--dsw-alias-interactive-bg-hover-solid",
    "--dsw-alias-turn-trigger-bg", "--dsw-alias-toast-bg", "--dsw-alias-tooltip-bg",
    "--dsw-alias-scrollbar-bg-l1", "--dsw-alias-scrollbar-bg-l2",
    "--dsw-alias-scrollbar-hover-l1", "--dsw-alias-scrollbar-hover-l2",
    "--dsw-alias-onboarding-secondary-fill", "--dsw-alias-bg-skeleton",
    "--dsw-alias-label-dimmed",
}

INK = {
    "--dsw-alias-label-primary", "--dsw-alias-label-secondary", "--dsw-alias-label-tertiary",
    "--dsw-alias-label-caption", "--dsw-alias-label-primary-dimmed", "--dsw-alias-label-primary-bluish",
    "--dsw-alias-label-document-preview", "--dsw-alias-label-primary-inverted",
    "--dsw-alias-label-primary-foreground", "--dsw-alias-menu-icon",
    "--dsw-alias-button-contrast-fill", "--dsw-alias-brand-text",
    "--dsw-alias-brand-primary-invert", "--dsw-alias-switch-thumb",
    "--dsw-alias-label-inverted", "--dsw-alias-toast-label",
}

BORDER = {
    "--dsw-alias-border-l1", "--dsw-alias-border-l2", "--dsw-alias-border-l3",
    "--dsw-alias-border-l4", "--dsw-alias-border-l2-darkmode-thin",
    "--dsw-alias-settings-card-stroke", "--dsw-alias-button-ghost-active-border",
    "--dsw-alias-onboarding-checkbox-border",
}

ACCENT = {
    "--dsw-alias-brand-primary", "--dsw-alias-button-primary-fill", "--dsw-alias-link",
    "--dsw-alias-label-deep-diving", "--dsw-alias-label-deep-diving-shimmer",
    "--dsw-alias-state-business-primary",
    "--dsw-alias-button-info-fill", "--dsw-alias-button-info-hover",
    "--dsw-alias-onboarding-accent", "--dsw-alias-brand-primary-new-colorprimary-new-color",
}

# 语义色：保留色相，只做一点暖化与对比度对齐
FUNCTIONAL = {
    "--dsw-alias-state-error-primary", "--dsw-alias-state-error-secondary",
    "--dsw-alias-state-success-primary", "--dsw-alias-state-success-secondary",
    "--dsw-alias-state-success-tertiary", "--dsw-alias-state-warn-primary",
    "--dsw-alias-state-warn-secondary", "--dsw-alias-state-warn-tertiary",
    "--dsw-alias-state-warn-label", "--dsw-alias-file-diff-added-bg",
    "--dsw-alias-file-diff-added-gutter", "--dsw-alias-file-diff-added-marker",
    "--dsw-alias-file-diff-deleted-bg", "--dsw-alias-file-diff-deleted-gutter",
    "--dsw-alias-file-diff-deleted-marker",
}

KEEP = {  # 纯遮罩：与方向无关
    "--dsw-alias-bg-mask-1", "--dsw-alias-bg-mask-2", "--dsw-alias-bg-mask-3",
    "--dsw-alias-bg-mask-drop", "--dsw-alias-bg-mask-photo",
    "--dsw-alias-border-inverted", "--dsw-alias-border-inverted2",
    "--dsw-alias-label-shimmer", "--dsw-alias-button-tool-bar-fill",
    "--dsw-alias-button-tool-bar-fill-invisible", "--dsw-alias-button-tool-bar-hover",
}

ACCENT_TINT = {  # 主色的浅底（badge/卡片底），不是实色
    "--dsw-alias-state-business-tertiary",
}

SPECIAL = {
    "--dsw-alias-bg-document-selection", "--dsw-alias-interactive-bg-active",
    "--dsw-alias-interactive-bg-hover", "--dsw-alias-interactive-bg-hover-accent",
    "--dsw-alias-interactive-bg-hover-danger", "--dsw-alias-turn-trigger-bg-hover",
    "--dsw-alias-menu-group-header-fill", "--dsw-alias-onboarding-card-fill",
    "--dsw-alias-tooltip-key-bg", "--dsw-alias-code-diff-added", "--dsw-alias-code-diff-deleted",
}


def transform(name, value, ramp, dark):
    """把一个默认 token 值映射到某方向、某明暗下的值。"""
    if name in KEEP:
        return value
    if "color-mix(" in value:
        # 结构保留，只换里面的字面色（遮罩类的不换）
        if name in KEEP:
            return value

        def sub(m):
            c = parse(m.group(0))
            if not c:
                return m.group(0)
            if name in FUNCTIONAL:
                return hexof(warmth(c, 0.35), c[3])
            if name in ACCENT:
                a = parse(ramp["accent"])
                return hexof(mix(a, c, 0.35), c[3])
            return hexof(ramp_lookup(ramp["paper"], neutral_x(c, dark)), c[3])

        return re.sub(r"#[0-9a-fA-F]{3,8}\b", sub, value)
    c = parse(value)
    if not c:
        return value  # 非颜色（极少），原样保留
    a = c[3]
    if name in ACCENT_TINT:
        acc = parse(ramp["accent"])
        paper = ramp_lookup(ramp["paper"], 0.25 if dark else 0.9)
        return hexof(mix(paper, acc, 0.13 if not dark else 0.20), a if a < 1 else None)
    if name in ACCENT:
        acc = parse(ramp["accent"])
        # 半透明的品牌色：保留原透明度
        return hexof(acc, a)
    if name in BORDER:
        # 边框：亮色下是纸面上压一笔很淡的墨；暗色下是纸面上提一笔很淡的墨。
        ink = ramp_lookup(ramp["ink"], 0.0)
        paper = ramp_lookup(ramp["paper"], 0.1)
        strength = {"--dsw-alias-border-l1": 0.10, "--dsw-alias-border-l2": 0.16,
                    "--dsw-alias-border-l3": 0.10, "--dsw-alias-border-l4": 0.14}.get(name, 0.12)
        mixed = mix(paper, ink, strength)
        # 原本就带透明度的（l3/l4/细线）保留透明度
        return hexof(mixed, a) if a < 1 else hexof(mixed)
    if name in INK:
        if a < 1:  # 半透明墨（少见）保留
            return hexof(ramp_lookup(ramp["ink"], ink_x(c, dark)), a)
        return hexof(ramp_lookup(ramp["ink"], ink_x(c, dark)))
    if name in SURFACE:
        return hexof(ramp_lookup(ramp["paper"], neutral_x(c, dark)), a if a < 1 else None)
    if name in FUNCTIONAL:
        return hexof(warmth(c, 0.35), a)
    if name in SPECIAL:
        if "danger" in name or "error" in name:
            return hexof(warmth(c, 0.3), a)
        return hexof(ramp_lookup(ramp["paper"], neutral_x(c, dark)), a)
    # 兜底：按明度当表面处理（107 个里剩下的都是没归类的）
    return hexof(ramp_lookup(ramp["paper"], neutral_x(c, dark)), a if a < 1 else None)


def build():
    names = sorted(set(L) & set(D))
    out = {}
    for variant, spec in RAMPS.items():
        out[variant] = {}
        for mode, ramp in spec.items():
            src = L if mode == "light" else D
            layer = {}
            for n in names:
                if not n.startswith("--dsw-alias-"):
                    continue
                layer[n] = transform(n, src[n], ramp, mode == "dark")
            out[variant][mode] = layer
    return out


def emit(data):
    lines = [
        "// 由 scripts/gen-tokens.py 生成，不要手改。",
        "// 输入：research/ui/2026-11-11-shangyi-shell/tokens-{light,dark}.json（验证实例实测）。",
        "// 输出：三个方向 × light/dark 的 --dsw-alias-* 覆盖表。",
        "var TOKENS = {",
    ]
    for variant, modes in data.items():
        lines.append('  %s: {' % variant)
        for mode, layer in modes.items():
            lines.append("    %s: {" % mode)
            for k in sorted(layer):
                lines.append('      "%s": "%s",' % (k, layer[k]))
            lines.append("    },")
        lines.append("  },")
    lines.append("};")
    lines.append("")
    lines.append("// 把每个 token 展开成 overrideTokens 需要的 { light, dark } 对。")
    lines.append("function tokenLayers() {")
    lines.append("  var out = {};")
    lines.append("  Object.keys(TOKENS).forEach(function (variant) {")
    lines.append("    var layer = {};")
    lines.append("    Object.keys(TOKENS[variant].light).forEach(function (name) {")
    lines.append("      layer[name] = { light: TOKENS[variant].light[name], dark: TOKENS[variant].dark[name] };")
    lines.append("    });")
    lines.append("    out[variant] = layer;")
    lines.append("  });")
    lines.append("  return out;")
    lines.append("}")
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    data = build()
    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    open(OUT, "w").write(emit(data))
    print("wrote", OUT)
    print("tokens per direction:", {k: len(v["light"]) for k, v in data.items()})
    # 抽查
    for v in ("paper", "lacquer", "ledger"):
        print(v, "bg-base", data[v]["light"]["--dsw-alias-bg-base"], data[v]["dark"]["--dsw-alias-bg-base"])
        print("   label-primary", data[v]["light"]["--dsw-alias-label-primary"], data[v]["dark"]["--dsw-alias-label-primary"])
        print("   border-l1", data[v]["light"]["--dsw-alias-border-l1"], data[v]["dark"]["--dsw-alias-border-l1"])

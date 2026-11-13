#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""商奕「data_ingest」侧车：CSV 解析与结构描述（设计文档 §7.2 的 `data_ingest`）。

# 本脚本的边界由可信度级别决定，不由代码风格决定

`data_ingest` 在工具注册表里的可信度是 `TrustLevel::Sidecar`，其含义是
「进程隔离的 Python 侧车，仅渲染/检索，**不产生数字**」（见
`shangyi/crates/shangyi-tools/src/lib.rs` 的分级表）。这条对本脚本是硬约束：

- **不做任何业务数值计算**：不求和、不算均值、不算增长率、不做同比环比、
  不做单位换算、不把 `"1,234"` 转成 `1234`。
- **样本值一律原样字符串**（`sample_rows` 的值是文件里那一串字符）。
  一旦在这里做类型转换，下游拿到的就是「侧车算出来的数」，
  而侧车没有资格产生进入报告的数字。
- 本脚本产出的数字只有一类：**解析过程的自述**——行数、空值个数、
  「这一列有多少比例的值能按某种格式解析」的计数与比例。它们是解析器的
  结构描述（parser provenance），不是业务指标。响应里
  `computation_boundary` 字段把这条边界显式写给调用方看。

# 只做解析、不猜语义

列语义判断（这列是「金额」还是「数量」还是「单价」）需要业务上下文，本脚本
不做。它只给**格式启发式的建议**：`column_hints` 的每一条都带 `evidence`
（例如「抽样 500 个非空值中 95% 可解析为日期」），由模型/用户决定采不采纳。
数字与日期的区分在格式层面本就无解（`20240105` 两种都成立），此时如实报
`ambiguous`，不挑一个蒙。

# 幂等与确定性

同一文件、同一请求 ⇒ 同一响应（除 `file.mtime_ms` 这类文件系统元数据）。
不依赖网络、不依赖时钟、不依赖随机数。

# 调用契约

- 从 **stdin** 读一个 JSON 请求、向 **stdout** 写一个 JSON 响应。
- 请求：`{"path": "/abs/x.csv", "max_rows": 20, "max_scan_rows": 200000,
  "encoding": null, "delimiter": null}`
  （`encoding` / `delimiter` 是**显式指定**用的——用户或模型若知道实际编码，
  在这里说明比让侧车猜更可靠；给了就照办，失败即报错，不做自动改写。）
- 成功：`{"ok": true, ...}`；失败：`{"ok": false, "error": {"kind": ..., "detail": ...}}`
- **样本行是位置数组**：`sample_rows[i][j]` 对应 `columns[j]`，值是原样字符串或
  `null`（该行缺这一列）。刻意不用「列名→值」对象：对象键序不被 JSON 保证
  （实测 Rust 侧 `serde_json::Value` 中转会重排），且重复列名在对象里会互相覆盖。
- **失败即拒绝，绝不静默降级**：读不到、格式不支持、编码无法确定，
  一律返回类型化错误，不返回「空数据」或「猜出来的列名」。
- 退出码：请求**被受理**（含解析失败、编码不支持等类型化 `ok:false`）⇒ 0；
  请求**未被受理**（stdin 不是合法 JSON 信封、CLI 用法错误）⇒ 2。
  两种情形都会在 stdout 写出合法 JSON 响应，调用方可以只看 JSON——
  分两个退出码是为了区分「传输坏了」与「参数不对」。
- 手工排查时可绕开 stdin：`python3 data_ingest.py <路径> [max_rows]`。

# 依赖

只用标准库（`csv` / `json` / `sys` / `io` / `pathlib` / `codecs` / `hashlib` /
`datetime` / `unicodedata` / `re` / `os`）。**刻意不用 pandas / openpyxl**：
本项目避免重依赖，且 xlsx 是 ZIP 容器，标准库解不了——遇到 xlsx 一律
**明确报不支持**，不假装能读（见 `unsupported_format`）。

# 实测记录（被否决的方案与踩到的坑）

1. **BOM 必须按 `utf-8-sig` 处理**。Excel 导出的 CSV 常带 UTF-8 BOM，
   若按 `utf-8` 读，表头首列会变成 `"\ufeff日期"`——一个不可见字符混进列名，
   下游按列名取值时静默取不到。故探测到 BOM 时直接用 `utf-8-sig`。
2. **「可疑字符」判据不能用码点区间粗判**。曾想用「非 BMP（U+10000 以上）
   视为可疑」，但 GB18030 的四字节序列**本来**就编码生僻汉字
   （实测 `𠀀𠀁` 解码后正是 U+20000/U+20001），那会把合法的中文姓名/地名
   误判为乱码。改用 Unicode 类别判据：私用区（`Co`）、未分配（`Cn`）、
   代理区（`Cs`）、`U+FFFD`、C0/C1 控制符才算可疑——实测真实 GBK 文本为
   0.0%，而 Big5 文本被 GB18030 强行解码时 PUA 占比 50%。
3. **Shift-JIS 是这套探测的已知盲区**。实测「こんにちは世界、売上高は…」
   （Shift-JIS 字节）能被 GB18030 完整解码，得出一串生僻汉字
   （`偙傫偵偪偼悽奅…`），既无 PUA 也无未分配字符——探测无法识别。
   故 `encoding_detail.method` 一律标注为 `sequential_probe`（顺序试探，
   不是统计检测），响应里也写明这条边界，不宣称「已确定编码」。
4. **分隔符不做无约束的 `csv.Sniffer`**。Sniffer 会给出任意字符作为分隔符，
   在单列文件上尤其容易给出荒谬结果。故把候选限制为 `, ; \\t |`，
   且要求「前若干非空行的分列数一致且 ≥ 2」才采纳，并把来源
   （`sniffer` / `fallback_scan` / `default_comma` / `pinned`）写进响应。
5. **字段长度上限要放宽**。`csv` 模块默认字段上限 128 KiB，中小企业导出的
   备注/地址字段可能超；超限时抛的是 `csv.Error`，若不放宽会让一个**合法**
   文件被误报为损坏。故放宽到 4 MiB（仍有界），并在注释里留痕。
"""

import codecs
import csv
import datetime
import hashlib
import io
import json
import os
import pathlib
import re
import sys
import unicodedata

# ────────────────────────────────────────────────────────────────────────────
# 常量（全部有解释，不设「魔法数字」）
# ────────────────────────────────────────────────────────────────────────────

# 编码探测的取样长度。取 64 KiB：足够判定编码，又不至于在大文件上浪费 I/O。
PROBE_BYTES = 64 * 1024

# 自动探测的候选顺序。**顺序即策略**：中文场景里 UTF-8 与 GB18030 覆盖绝大多数
# 导出文件；二者都不成立时如实报错，不继续撒网（撒网会提高误判为某编码的概率，
# 而错误编码的代价是乱码列名，比「读不出来」更坏——后者至少是可诊断的失败）。
AUTO_CANDIDATES = ("utf-8", "gb18030")

# 允许被显式指定的编码。这是「用户给了领域知识」的路径，与自动探测不同：
# 采信用户的指定，但只在他指定的这套编码里拣，不做别名猜测。
PINNABLE_ENCODINGS = (
    "utf-8",
    "utf-8-sig",
    "gb18030",
    "gbk",
    "big5",
    "utf-16",
    "utf-16-le",
    "utf-16-be",
    "latin-1",
    "cp1252",
    "shift_jis",
)

# 分隔符候选。制表符与分号在「从 Excel/ERP 另存」的场景里同样常见。
DELIMITER_CANDIDATES = (",", "\t", ";", "|")
SNIFF_DELIMITERS = ",;\t|"

# 可疑字符占比阈值。超过即判定「这个编码虽然能解码，但解出来的不是正常文本」，
# 报 `encoding_undetectable` 而不是把乱码当数据交付。
# 实测标定：真实 GBK 文本 0.0%，Big5 被 GB18030 强行解码 50%。
# 阈值取 2%：远离两个实测样本，且给「个别怪字符」留出容忍，避免误杀。
IMPLAUSIBLE_RATIO_LIMIT = 0.02

# 列类型建议的抽样与阈值。
HINT_SAMPLE_VALUES = 500  # 每列最多参与判定的非空值个数（有界 ⇒ 耗时可控）
HINT_SAMPLE_ROWS = 2000  # 参与判定的最多数据行数（列数多时候避免 O(行×列) 失控）
HINT_MAX_COLUMNS = 200  # 超过此列数只对前若干列给建议，并在 notes 里说明
HINT_THRESHOLD = 0.90  # 「这列是日期/数值」的认定比例
HINT_EXAMPLES = 3  # 每条建议附几个样例值

# 默认扫描上限。超过即停止扫描并**如实报告未完整扫描**（不猜、不谎报 total）。
DEFAULT_MAX_SCAN_ROWS = 200_000
DEFAULT_MAX_ROWS = 20

# 摘要上限：超过则不算 sha256（避免为一个指纹再全量读一遍 GB 级文件）。
DIGEST_MAX_BYTES = 64 * 1024 * 1024

# 容忍的行内字段数不一致：只要报到 ragged_rows / ragged_examples 里，
# 就不算「静默修正」。补齐动作见 `ingest` 里构造 `rendered_samples` 的那段。
RAGGED_EXAMPLE_LIMIT = 5

# 日期格式白名单。**只收「年在前」的无歧义格式**：`01/02/2024` 这类
# 日月次序依地区而异的写法一律不收——两种解读都能通过，猜错会让整列日期
# 静默错位（例如把 3 月 5 日读成 5 月 3 日）。宁可标 `text`。
DATE_FORMATS = (
    ("%Y-%m-%d", "YYYY-MM-DD"),
    ("%Y/%m/%d", "YYYY/MM/DD"),
    ("%Y.%m.%d", "YYYY.MM.DD"),
    ("%Y%m%d", "YYYYMMDD"),
    ("%Y年%m月%d日", "YYYY年MM月DD日"),
    ("%Y-%m-%d %H:%M:%S", "YYYY-MM-DD HH:MM:SS"),
    ("%Y/%m/%d %H:%M:%S", "YYYY/MM/DD HH:MM:SS"),
    ("%Y-%m-%d %H:%M", "YYYY-MM-DD HH:MM"),
    ("%Y-%m", "YYYY-MM"),
    ("%Y/%m", "YYYY/MM"),
    ("%Y年%m月", "YYYY年MM月"),
)

# 数值格式：**限 ASCII**，不接受全角数字/阿拉伯-印度数字。
# Python 的 `float()` 会接受 `"１２３"`（全角）与 `"١٢٣"`，但把全角字符
# 认成数字是**语义判断**（在中文 ERP 导出里 U+FF11… 更可能是文本）。
# 宁可漏判（标为 `text`，调用方一眼能看出没识别）也不错判。
NUMBER_RE = re.compile(r"^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$")

# ZIP / OLE2 容器魔数：xlsx 与旧版 xls 的真身。**按内容判**而不是只看扩展名——
# 把 xlsx 改名成 .csv 的情况在实务里真实存在，只看扩展名会漏。
ZIP_MAGIC = b"PK\x03\x04"
OLE2_MAGIC = b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"


# ────────────────────────────────────────────────────────────────────────────
# 类型化错误
# ────────────────────────────────────────────────────────────────────────────


class SidecarError(Exception):
    """带 `kind` 的解析失败。**kind 是契约的一部分**，调用方按 kind 决定应对。

    为什么不是一句字符串：不同的失败对应不同的动作——`file_not_found` 要改
    路径、`encoding_undetectable` 要问用户要编码、`unsupported_format` 要换
    格式。合并成字符串后这些应对全部退化为「再试一次」。
    """

    def __init__(self, kind, detail):
        super().__init__(detail)
        self.kind = kind
        self.detail = detail

    def as_json(self):
        return {"ok": False, "error": {"kind": self.kind, "detail": self.detail}}


class EnvelopeError(SidecarError):
    """**请求信封**本身无法受理（不是合法 JSON / 非对象 / `path` 缺失）。

    与普通 `SidecarError` 分开是有意的：信封错误说明调用方的协议用错了，
    对应的退出码是 2（「请求未被受理」）；而 `encoding_not_supported`、
    `max_rows` 非法这类错误说明请求已被受理、只是其中的**值**不被支持，
    退出码仍是 0。把两者混成一个退出码，调用方就无法区分「我的传输坏了」
    与「我的参数不对」——前者要修管道，后者要改参数。
    """


# ────────────────────────────────────────────────────────────────────────────
# 请求解析
# ────────────────────────────────────────────────────────────────────────────


def _as_positive_int(value, field, default, allow_zero):
    """把请求里的整数参数解析成 int；非法即报 `bad_request`（不落回默认值）。

    **为什么不静默用默认值**：`max_rows: "abc"` 被悄悄当成 20，会让调用方以为
    自己拿到的是 100 行样本。参数错就是参数错，报出来。
    """
    if value is None:
        return default
    if isinstance(value, bool) or not isinstance(value, int):
        raise SidecarError(
            "bad_request", "`{}` 须是整数，实得 {!r}".format(field, value)
        )
    if value < 0 or (value == 0 and not allow_zero):
        raise SidecarError(
            "bad_request",
            "`{}` 须{}，实得 {}".format(field, "非负整数" if allow_zero else "正整数", value),
        )
    return value


def parse_request(raw):
    """解析 stdin 的 JSON 请求。信封不成形即 `bad_request`（退出码 2）。"""
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise EnvelopeError(
            "bad_request",
            "请求本身不是 UTF-8：{}。契约要求请求为 UTF-8 编码的 JSON。".format(exc),
        )
    try:
        req = json.loads(text)
    except json.JSONDecodeError as exc:
        raise EnvelopeError("bad_request", "请求不是合法 JSON：{}".format(exc))
    if not isinstance(req, dict):
        raise EnvelopeError(
            "bad_request", "请求须是 JSON 对象，实得 {}".format(type(req).__name__)
        )

    path_raw = req.get("path")
    if not isinstance(path_raw, str) or not path_raw.strip():
        raise EnvelopeError("bad_request", "缺少 `path`（字符串）。")

    # **要求绝对路径**：侧车的 cwd 与调用方的 cwd 可能不同（Rust 侧用
    # `Command` 启动，cwd 继承自父进程但可被改），相对路径会在两个进程里
    # 指向不同的文件——这种错位极难排查，故在入口处拒绝。
    if not os.path.isabs(path_raw):
        raise SidecarError(
            "bad_request",
            "`path` 须是绝对路径，实得 {!r}。进程边界两侧的 cwd 语义可能不同，"
            "相对路径会指向不同文件。".format(path_raw),
        )

    encoding = req.get("encoding")
    if encoding is not None:
        if not isinstance(encoding, str):
            raise SidecarError("bad_request", "`encoding` 须是字符串或 null。")
        encoding = encoding.strip().lower()
        if encoding not in PINNABLE_ENCODINGS:
            raise SidecarError(
                "encoding_not_supported",
                "不支持的 `encoding`：{!r}。可用：{}".format(
                    encoding, "、".join(PINNABLE_ENCODINGS)
                ),
            )

    delimiter = req.get("delimiter")
    if delimiter is not None:
        if not isinstance(delimiter, str) or delimiter not in DELIMITER_CANDIDATES:
            raise SidecarError(
                "bad_request",
                "`delimiter` 须是 {} 之一或 null，实得 {!r}".format(
                    "/".join(repr(d) for d in DELIMITER_CANDIDATES), delimiter
                ),
            )

    return {
        "path": pathlib.Path(path_raw),
        "max_rows": _as_positive_int(req.get("max_rows"), "max_rows", DEFAULT_MAX_ROWS, True),
        "max_scan_rows": _as_positive_int(
            req.get("max_scan_rows"), "max_scan_rows", DEFAULT_MAX_SCAN_ROWS, False
        ),
        "encoding": encoding,
        "delimiter": delimiter,
    }


# ────────────────────────────────────────────────────────────────────────────
# 文件检查
# ────────────────────────────────────────────────────────────────────────────


def _reject_unsupported_container(path, head):
    """按**内容魔数**拒绝容器格式（xlsx/xls 等）。"""
    if head.startswith(ZIP_MAGIC):
        raise SidecarError(
            "unsupported_format",
            "{} 是 ZIP 容器（很可能是 .xlsx/.xlsm，或由 xlsx 改名而来）。"
            "本侧车只解析分隔符文本（CSV/TSV）；解析 xlsx 需要解压与共享字符串表，"
            "标准库做不到，而本项目刻意不引入 openpyxl 这类重依赖。"
            "请在 Excel 里「另存为 CSV」后重试。".format(path.name),
        )
    if head.startswith(OLE2_MAGIC):
        raise SidecarError(
            "unsupported_format",
            "{} 是 OLE2 复合文档（旧版 .xls/.doc）。同上：请另存为 CSV。".format(path.name),
        )


def open_and_probe(path):
    """打开文件、读探测块、返回 (二进制句柄, 探测字节, 文件元信息)。"""
    try:
        stat = path.stat()
    except FileNotFoundError:
        raise SidecarError("file_not_found", "文件不存在：{}".format(path))
    except PermissionError as exc:
        raise SidecarError("permission_denied", "无权限读取：{}（{}）".format(path, exc))
    except OSError as exc:
        raise SidecarError("io_error", "读取文件元信息失败：{}（{}）".format(path, exc))

    if not path.is_file():
        raise SidecarError(
            "not_a_file",
            "{} 存在但不是常规文件（目录/设备/管道？）。请给到具体文件。".format(path),
        )

    try:
        handle = path.open("rb")
    except PermissionError as exc:
        raise SidecarError("permission_denied", "无权限读取：{}（{}）".format(path, exc))
    except OSError as exc:
        raise SidecarError("io_error", "打开文件失败：{}（{}）".format(path, exc))

    head = handle.read(PROBE_BYTES)
    if not head:
        handle.close()
        raise SidecarError(
            "empty_file",
            "{} 是空文件（0 字节）。没有可解析内容——空文件不是「0 行数据」，"
            "后者至少能给出列名。".format(path),
        )
    _reject_unsupported_container(path, head)
    handle.seek(0)

    meta = {
        "path": str(path),
        "size_bytes": stat.st_size,
        "mtime_ms": int(stat.st_mtime * 1000),
    }
    if stat.st_size <= DIGEST_MAX_BYTES:
        # 摘要是**审计凭据**（这份报告用的是哪个版本的文件），不是业务数值。
        # 大文件跳过并在响应里说明，避免为一个指纹再全量读一遍。
        meta["sha256"] = _file_sha256(path)
    else:
        meta["sha256"] = None
        meta["digest_note"] = "文件大于 {} 字节，未计算 sha256".format(DIGEST_MAX_BYTES)
    return handle, head, meta


def _file_sha256(path):
    h = hashlib.sha256()
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


# ────────────────────────────────────────────────────────────────────────────
# 编码探测
# ────────────────────────────────────────────────────────────────────────────


def _implausible_ratio(text):
    """解码结果里「不该出现在正常文本里」的字符占比。

    判据用 Unicode 类别而不是码点区间（见模块头实测记录 2）：
    `Co` 私用区、`Cn` 未分配、`Cs` 代理区、`U+FFFD`、C0/C1 控制符。
    制表/换行/回车是文本的正常组成部分，放行。
    """
    if not text:
        return 0.0
    bad = 0
    for ch in text:
        if ch in "\t\r\n":
            continue
        code = ord(ch)
        if code == 0xFFFD or code < 0x20 or 0x7F <= code <= 0x9F:
            bad += 1
        elif unicodedata.category(ch) in ("Co", "Cn", "Cs"):
            bad += 1
    return bad / len(text)


def _try_decode(data, encoding):
    """严格解码。失败抛 UnicodeDecodeError（由调用方决定如何报）。"""
    return data.decode(encoding, errors="strict")


def probe_encoding(head, pinned):
    """确定编码。返回 `(encoding, detail)`。

    顺序：BOM > 显式指定 > UTF-8 > GB18030。**每一步都诚实标注来源**，
    因为「BOM 说明的」与「试出来的」在可信度上不是一回事。
    """
    # ① BOM 是文件自己声明的编码，比任何试探都强。
    if head.startswith(codecs.BOM_UTF8):
        return "utf-8-sig", {
            "method": "bom",
            "candidates_tried": ["utf-8-sig"],
            "pinned": False,
            "probe_bytes": PROBE_BYTES,
            "note": "文件带 UTF-8 BOM。用 utf-8-sig 读取会自动剥掉 BOM，"
            "否则首列列名会带不可见的 U+FEFF（Excel 导出常见）。",
        }
    if head.startswith(codecs.BOM_UTF16_LE) or head.startswith(codecs.BOM_UTF16_BE):
        return "utf-16", {
            "method": "bom",
            "candidates_tried": ["utf-16"],
            "pinned": False,
            "probe_bytes": PROBE_BYTES,
            "note": "文件带 UTF-16 BOM（Excel「Unicode 文本」导出即此格式）。",
        }

    # ② 调用方显式指定：照办，失败即报错，**不改用别的编码**。
    #    自动改写会违背「用户明确说了」这一层信息——他可能正是因为知道
    #    文件是谁导出的才指定编码。
    if pinned:
        try:
            _try_decode(head, pinned)
        except UnicodeDecodeError as exc:
            raise SidecarError(
                "encoding_undetectable",
                "你显式指定的编码 {!r} 在前 {} 字节处解码失败：{}（偏移 {}）。"
                "不自动改用其他编码——显式指定是明确指令，失败即拒绝。".format(
                    pinned, PROBE_BYTES, exc.reason, exc.start
                ),
            )
        except LookupError as exc:  # 理论上被 PINNABLE_ENCODINGS 拦住，兜底
            raise SidecarError("encoding_not_supported", str(exc))
        return pinned, {
            "method": "pinned",
            "candidates_tried": [pinned],
            "pinned": True,
            "probe_bytes": PROBE_BYTES,
            "note": "编码由调用方显式指定，未做自动探测。",
        }

    # ③ 顺序试探。UTF-8 的第一条就走：UTF-8 的结构约束很强，GBK 中文的字节
    #    组合几乎不可能碰巧构成合法 UTF-8 序列，故「解出来是 UTF-8」可信。
    #    注意纯 ASCII 文件会落在这一档——ASCII 同时是 UTF-8/GB18030/latin-1，
    #    但对 ASCII 而言三者解码结果完全一致，选谁都不影响数据。
    errors = []
    for candidate in AUTO_CANDIDATES:
        try:
            decoded = _try_decode(head, candidate)
        except UnicodeDecodeError as exc:
            errors.append("{}：偏移 {} 处 {}".format(candidate, exc.start, exc.reason))
            continue
        ratio = _implausible_ratio(decoded)
        if ratio > IMPLAUSIBLE_RATIO_LIMIT:
            # 能解码 ≠ 解对了。Big5 文本被 GB18030 强行解码时 PUA 占比可达
            # 50%——若放行，下游会把乱码当成真实列名（比读不出来更坏）。
            raise SidecarError(
                "encoding_undetectable",
                "{} 能按 {} 解码，但解出的文本可疑字符占比 {:.1%}（阈值 {:.0%}）。"
                "这通常意味着实际编码不在候选内（例如 Big5）。"
                "请用 `encoding` 参数显式指定实际编码。".format(
                    "前 %d 字节" % PROBE_BYTES, candidate, ratio, IMPLAUSIBLE_RATIO_LIMIT
                ),
            )
        return candidate, {
            "method": "sequential_probe",
            "candidates_tried": [c for c in AUTO_CANDIDATES[: AUTO_CANDIDATES.index(candidate) + 1]],
            "pinned": False,
            "probe_bytes": PROBE_BYTES,
            "implausible_ratio": round(ratio, 6),
            "note": "按 {} 顺序试探得到，**不是**统计检测。它无法区分同样能装进 "
            "{} 的其他编码（实测 Shift-JIS 中文会被 {} 解成生僻汉字且无异常字符）；"
            "若列名/取值看起来不是正常中文，请用 `encoding` 参数显式指定。".format(
                "、".join(AUTO_CANDIDATES), candidate, candidate
            ),
        }

    raise SidecarError(
        "encoding_undetectable",
        "前 {} 字节按{}逐次解码均失败（{}）。**不默认当 UTF-8 硬读**——那会得到"
        "乱码列名，而下游会把乱码当成真实列名。请用 `encoding` 参数显式指定实际编码"
        "（可用：{}）。".format(
            PROBE_BYTES,
            "、".join(AUTO_CANDIDATES),
            "；".join(errors),
            "、".join(PINNABLE_ENCODINGS),
        ),
    )


# ────────────────────────────────────────────────────────────────────────────
# 分隔符探测
# ────────────────────────────────────────────────────────────────────────────


def _split_agreement(text, delimiter, max_lines=20):
    """该分隔符的「分列一致性」：返回 `(众数列数, 一致比例, 样例行数)`。

    判据取**多数一致**而不是「全部一致」（早期实现要求全部相等，被实测否决）：
    真实 ERP 导出的 CSV 常有行内字段数参差（ragged），若要求全部相等，
    这种文件的合法分隔符会被判为不一致，从而退回到 `default_comma` 的兜底——
    分隔符明明猜对了，来源标签却是「兜底」，调用方无法分辨。
    """
    counts = []
    try:
        for row in csv.reader(io.StringIO(text), delimiter=delimiter):
            if not row:
                continue
            counts.append(len(row))
            if len(counts) >= max_lines:
                break
    except csv.Error:
        return 0, 0.0, 0
    if not counts:
        return 0, 0.0, 0
    modal = max(set(counts), key=lambda c: (counts.count(c), -c))
    agree = counts.count(modal) / len(counts)
    return modal, agree, len(counts)


def sniff_delimiter(probe_text, pinned):
    """确定分隔符。返回 `(delimiter, source)`。

    # 判定规则（以及为什么不用「行行相等」）

    对每个候选算 `(众数列数, 一致比例)`，取「众数列数 ≥ 2 且样例行数 ≥ 2」者，
    按 `(众数列数, 一致比例)` 排序，并列时按 `DELIMITER_CANDIDATES` 的固定顺序。
    顺序固定 ⇒ 同一文件两次调用给出同一分隔符。

    早期实现要求「前 20 行分列数完全相等」，被实测否决：ERP 导出的 CSV 常有
    参差行（ragged），于是**猜对了分隔符也会被判为不一致**，标签退化成
    `default_comma` 兜底——调用方因此无法分辨「猜对的」与「兜底的」。
    改用众数后，`a,b,c / 1,2,3 / 1,2 / 1,2,3,4` 这类文件仍能定出逗号。

    # 已知的不可判定情形

    若两个候选的众数列数相同（如每行恰有一个逗号、也恰有一个分号），
    本函数按固定顺序取靠前者。这**可能**定错——故真值判断交给调用方：
    `delimiter` 参数可显式指定，响应里也回传 `delimiter_source` 说明来源。

    # 来源标签

    `sniffer` = Sniffer 的首选即为胜者；`fallback_scan` = Sniffer 无可用答案或
    其首选落败，由本函数扫描定出；`default_comma` = 没有任何候选成立
    （单列文件），按逗号报；`pinned` = 调用方指定。
    """
    if pinned:
        return pinned, "pinned"
    sample = probe_text[:8192]

    def score(delim):
        modal, agree, lines = _split_agreement(sample, delim)
        if modal < 2 or lines < 2:
            return None
        return (modal, agree)

    try:
        guess = csv.Sniffer().sniff(sample, delimiters=SNIFF_DELIMITERS).delimiter
    except csv.Error:
        guess = None
    if guess not in DELIMITER_CANDIDATES:
        guess = None

    best = None  # (delimiter, source, score)
    for index, candidate in enumerate(DELIMITER_CANDIDATES):
        s = score(candidate)
        if s is None:
            continue
        # 平局时 Sniffer 的首选优先，其次是候选顺序靠前者（顺序固定 ⇒ 确定）
        key = (1 if candidate == guess else 0, s[0], s[1], -index)
        if best is None or key > best[0]:
            best = (key, candidate, s[0])
    if best is not None:
        _, winner, _ = best
        return winner, ("sniffer" if winner == guess else "fallback_scan")
    # 单列文件会走到这里：没有任何分隔符能让列数 ≥ 2。
    return ",", "default_comma"


# ────────────────────────────────────────────────────────────────────────────
# 列名与表头
# ────────────────────────────────────────────────────────────────────────────


def decide_header(first_row, data_rows):
    """判断首行是否表头。**这是启发式建议，不是结论**，故连同证据一起回传。

    判据（两条都记进 evidence）：
    ① 首行无空单元格；
    ② 首行里能按数值/日期解析的单元格比例低（表头通常是文字），
       而随后的数据行里该比例高。
    两条同时成立才判为表头；只成立一条时按「无表头」处理并说明——
    因为把**数据行**误当表头会丢掉一整行数据（不可逆），
    而把表头当数据只是多一行可辨认的垃圾。
    """
    raw = [c.strip() for c in first_row]
    non_empty = [c for c in raw if c]
    header_like = len(non_empty) == len(raw) and len(raw) >= 1

    def numeric_ratio(cells):
        cells = [c.strip() for c in cells if c and c.strip()]
        if not cells:
            return 0.0
        return sum(1 for c in cells if _looks_numeric(c) or _match_date(c)) / len(cells)

    first_ratio = numeric_ratio(raw)
    later = [r for r in data_rows if r]
    later_ratio = (
        sum(numeric_ratio(r) for r in later) / len(later) if later else 0.0
    )

    if header_like and first_ratio < 0.5 and later and later_ratio >= 0.5:
        has_header = True
        confidence = "high"
        evidence = (
            "首行无空单元格，且首行可解析为数值/日期的比例 {:.0%} 明显低于"
            "后续数据行的 {:.0%}——判为表头。".format(first_ratio, later_ratio)
        )
    elif not later:
        # 没有数据行可比对（文件只有一行，或扫描上限恰好卡在首行）。
        # 此时**退回首行自身的形态**判断：
        # - 全是文字（可解析为数值/日期的比例低）⇒ 多半是表头；
        # - 含数值/日期 ⇒ 多半是数据行。
        # 两种误判的代价不同，故宁可在不确定时报「无表头」：把数据行当表头
        # 会丢掉一整行数据（不可逆），把表头当数据只多一行可辨认的垃圾。
        has_header = header_like and first_ratio < 0.5
        confidence = "low"
        evidence = (
            "无数据行可与首行对比：按首行自身形态判断（可解析为数值/日期的比例 "
            "{:.0%}，{}）⇒ {}。原始首行见 `header.first_row_raw`，可据此人工纠正。".format(
                first_ratio,
                "无空单元格" if header_like else "含空单元格",
                "判为表头（低置信）" if has_header else "按无表头处理",
            )
        )
    else:
        has_header = False
        confidence = "medium"
        evidence = (
            "未识别到表头（首行可解析为数值/日期的比例 {:.0%}，"
            "后续数据行 {:.0%}{}）。`columns` 是位置占位名 `column_1…`——"
            "**不是**从内容推断的语义名；原始首行见 `header.first_row_raw`。".format(
                first_ratio,
                later_ratio,
                "" if header_like else "；首行含空单元格",
            )
        )
    return {
        "has_header": has_header,
        "confidence": confidence,
        "evidence": evidence,
        "first_row_raw": list(first_row),
    }


def build_column_names(header_row, n_cols):
    """把表头单元格转成**唯一可用**的列名，并记录所有改名动作。

    为什么必须有这一步：重复列名（`收入,收入`）与空列名在真实导出里很常见，
    而 JSON 对象的键必须唯一——不处理会让 `sample_rows` 静默丢列。
    所有改名都写进返回值里，调用方拿得到原始表头（`columns_raw`）。
    """
    used = []
    seen = {}
    duplicates = []
    unnamed = []
    for i in range(n_cols):
        raw = header_row[i] if i < len(header_row) else ""
        name = raw.strip()
        if not name:
            name = "column_{}".format(i + 1)
            unnamed.append({"index_1based": i + 1, "used": name})
        if name in seen:
            seen[name] += 1
            new_name = "{}__{}".format(name, seen[name])
            duplicates.append({"name": name, "count": seen[name], "used": new_name})
            name = new_name
        else:
            seen[name] = 1
        used.append(name)
    return used, duplicates, unnamed


# ────────────────────────────────────────────────────────────────────────────
# 列提示（格式启发式，全部带证据）
# ────────────────────────────────────────────────────────────────────────────


def _sample_row_into(hint_values, empty_counts, row):
    """把一行里参与列建议抽样的值收进桶里。

    抽样是**有界**的：每列最多 `HINT_SAMPLE_VALUES` 个非空值，且只在
    前 `HINT_SAMPLE_ROWS` 行里抽。故每列证据里的分母是真实抽样数，
    evidence 文本会写出「在抽样的 N 个非空值中」——不假装扫了全表。
    """
    for i in range(min(len(row), HINT_MAX_COLUMNS)):
        bucket = hint_values.setdefault(i, [])
        if len(bucket) >= HINT_SAMPLE_VALUES:
            continue
        value = row[i].strip()
        if value:
            bucket.append(value)
        else:
            empty_counts[i] = empty_counts.get(i, 0) + 1


def _looks_numeric(value):
    return bool(NUMBER_RE.match(value))


def _match_date(value):
    for fmt, label in DATE_FORMATS:
        try:
            datetime.datetime.strptime(value, fmt)
        except ValueError:
            continue
        return label
    return None


def column_hints(columns, samples, empty_counts):
    """给每列一个**建议**标签 + 证据。`samples[col]` 是已抽样的非空值列表。

    标签取值：`date` / `number` / `text` / `empty` / `ambiguous`。
    `ambiguous` 是「两种格式都完全成立」——最典型的是 8 位日期
    （`20240105` 既是合法日期也是合法整数）。此时不挑一个蒙，如实报出来。
    """
    hints = []
    for idx, col in enumerate(columns):
        values = samples.get(idx, [])
        empty_seen = empty_counts.get(idx, 0)
        if not values:
            hints.append(
                {
                    "column": col,
                    "hint": "empty",
                    "evidence": "抽样范围内该列没有任何非空值（抽样行里空 {} 个）。".format(
                        empty_seen
                    ),
                    "non_empty": 0,
                    "empty": empty_seen,
                    "examples": [],
                    "alternatives": [],
                }
            )
            continue
        total = len(values)
        date_hits = {}
        number_hits = 0
        for v in values:
            label = _match_date(v)
            if label:
                date_hits[label] = date_hits.get(label, 0) + 1
            if _looks_numeric(v):
                number_hits += 1
        date_count = sum(date_hits.values())
        date_ratio = date_count / total
        number_ratio = number_hits / total
        examples = values[:HINT_EXAMPLES]

        def fmt_ratio(r, n):
            return "{:.1%}（{}/{}）".format(r, n, total)

        candidates = []
        if date_ratio >= HINT_THRESHOLD:
            best_fmt = max(date_hits.items(), key=lambda kv: (kv[1], kv[0]))[0]
            candidates.append(
                ("date", "可解析为日期，命中格式 {}".format(best_fmt), date_ratio, date_count)
            )
        if number_ratio >= HINT_THRESHOLD:
            candidates.append(("number", "可解析为数值", number_ratio, number_hits))

        if len(candidates) == 2:
            ratio_gap = abs(date_ratio - number_ratio)
            if ratio_gap <= 1e-9:
                hints.append(
                    {
                        "column": col,
                        "hint": "ambiguous",
                        "evidence": "在抽样的 {} 个非空值中，{} 可解析为日期、{} 可解析为数值——"
                        "两种解读**同时完全成立**（如 `20240105`）且无法由格式区分。"
                        "本侧车不做语义判定，请据上下文选择；样本值见 `examples`。".format(
                            total, fmt_ratio(date_ratio, date_count), fmt_ratio(number_ratio, number_hits)
                        ),
                        "non_empty": total,
                        "empty": empty_seen,
                        "examples": examples,
                        "alternatives": ["date", "number"],
                    }
                )
                continue
            candidates.sort(key=lambda c: c[2], reverse=True)
        if candidates:
            label, why, ratio, count = candidates[0]
            others = [c[0] for c in candidates[1:]]
            hints.append(
                {
                    "column": col,
                    "hint": label,
                    "evidence": "在抽样的 {} 个非空值中 {} {}。这是**格式启发式的建议**，"
                    "不是语义判定（金额/数量/单价无法由格式区分）。".format(
                        total, fmt_ratio(ratio, count), why
                    ),
                    "non_empty": total,
                    "empty": empty_seen,
                    "examples": examples,
                    "alternatives": others,
                }
            )
            continue
        hints.append(
            {
                "column": col,
                "hint": "text",
                "evidence": "抽样 {} 个非空值中无达到 {:.0%} 的一致格式"
                "（日期 {:.1%}，数值 {:.1%}）——按文本报告。".format(
                    total, HINT_THRESHOLD, date_ratio, number_ratio
                ),
                "non_empty": total,
                "empty": empty_seen,
                "examples": examples,
                "alternatives": [],
            }
        )
    return hints


# ────────────────────────────────────────────────────────────────────────────
# 主流程
# ────────────────────────────────────────────────────────────────────────────


def ingest(request):
    """解析一个文件，返回响应字典（`ok: true` 的形状）。"""
    path = request["path"]
    max_rows = request["max_rows"]
    max_scan_rows = request["max_scan_rows"]

    handle, head, file_meta = open_and_probe(path)
    try:
        encoding, encoding_detail = probe_encoding(head, request["encoding"])

        # 探测块可能在一个多字节字符中间被切断。用增量解码器（final=False）
        # 处理：未完成的尾部序列被缓冲而非报错——否则大文件的探测会在
        # 随机位置失败，而那是取样问题，不是文件问题。
        decoder = codecs.getincrementaldecoder(encoding)()
        try:
            probe_text = decoder.decode(head, False)
        except UnicodeDecodeError as exc:
            raise SidecarError(
                "encoding_undetectable",
                "探测块（前 {} 字节）按 {} 解码失败：偏移 {} 处 {}。"
                "请用 `encoding` 参数显式指定实际编码。".format(
                    PROBE_BYTES, encoding, exc.start, exc.reason
                ),
            )

        delimiter, delimiter_source = sniff_delimiter(probe_text, request["delimiter"])

        # ── 全量流式扫描（解码与分词都在这一遍里）──
        text_stream = io.TextIOWrapper(handle, encoding=encoding, errors="strict", newline="")
        reader = csv.reader(text_stream, delimiter=delimiter)

        first_row = None
        data_row_count = 0
        # `scanned` 计的是**读进来的非空物理行数（含表头行）**，
        # 与 `row_count`（数据行数）不是一个口径，两者都在响应里写明。
        scanned = 0
        blank_lines = 0
        ragged_rows = 0
        ragged_examples = []
        sample_rows = []
        hint_values = {}  # 列序号 -> 抽样到的非空值
        empty_counts = {}  # 列序号 -> 抽样行里的空值个数
        header_probe_rows = []  # 用于表头判定的前几行
        columns_used = None
        n_cols = None
        truncated = False

        try:
            for row in reader:
                if not row:
                    blank_lines += 1
                    continue
                scanned += 1
                if scanned > max_scan_rows:
                    scanned -= 1
                    truncated = True
                    break
                if first_row is None:
                    first_row = row
                    n_cols = len(first_row)
                    continue
                if len(header_probe_rows) < 20:
                    header_probe_rows.append(row)
                # 表头判定要等拿到若干数据行后才知道，故先按「首行是表头」
                # 记账，稍后若判定为无表头再把首行补回数据与样本里。
                data_row_count += 1
                if len(row) != n_cols:
                    ragged_rows += 1
                    if len(ragged_examples) < RAGGED_EXAMPLE_LIMIT:
                        ragged_examples.append(
                            {"row_1based": scanned, "cells": len(row), "expected": n_cols}
                        )
                if data_row_count <= HINT_SAMPLE_ROWS:
                    _sample_row_into(hint_values, empty_counts, row)
                if len(sample_rows) < max_rows:
                    sample_rows.append(row)
        except UnicodeDecodeError as exc:
            raise SidecarError(
                "encoding_undetectable",
                "扫描到偏移 {} 处按 {} 解码失败（{}）。前 {} 字节可解码，说明该文件"
                "可能混用了多种编码，或实际编码不在候选中。**已如实拒绝**，"
                "不返回半份数据——半份数据的行数是错的，而错的行数比没有行数更危险。".format(
                    exc.start, encoding, exc.reason, PROBE_BYTES
                ),
            )
        except csv.Error as exc:
            raise SidecarError(
                "malformed_csv",
                "第 {} 行附近解析失败（分隔符 {!r}）：{}。".format(
                    reader.line_num, delimiter, exc
                ),
            )
        finally:
            text_stream.close()
    finally:
        handle.close()

    if first_row is None:
        # 走到这里说明文件非空但没有任何非空行（全是空行/纯分隔符行）
        raise SidecarError(
            "empty_file",
            "{} 有 {} 字节但没有任何非空行——没有可解析的表头或数据。".format(
                path, file_meta["size_bytes"]
            ),
        )

    header = decide_header(first_row, header_probe_rows)

    if header["has_header"]:
        columns_raw = list(first_row)
        row_count = data_row_count
    else:
        # 无表头：首行也是数据。把它补回行数与样本，否则会**凭空少一行**。
        columns_raw = ["column_{}".format(i + 1) for i in range(n_cols)]
        row_count = data_row_count + 1
        sample_rows.insert(0, first_row)
        if len(sample_rows) > max_rows:
            # 样本容量被首行挤满 ⇒ 末行被挤出，这是截断（如实标记）
            sample_rows = sample_rows[:max_rows]
        _sample_row_into(hint_values, empty_counts, first_row)

    columns_used, duplicates, unnamed = build_column_names(columns_raw, n_cols)

    # 样本行转成**位置数组**：`sample_rows[i][j]` 对应 `columns[j]`。
    # **值不做任何类型转换**（见模块头）。
    #
    # # 为什么是数组而不是「列名→值」的对象（实测否决了对象形态）
    #
    # ① 对象形态的键序**不被 JSON 保证**，而 Rust 侧经 `serde_json::Value`
    #    中转时会按 BTreeMap 重排——实测列序被打乱，人眼核对样本行时会与表头
    #    完全对不上。数组的顺序由 JSON 语法本身保证，不依赖任何实现的善意。
    # ② 重复列名在对象里只能容纳一个键（先前用 `收入__2` 后缀规避），
    #    数组形态下这个风险在**协议层**就不存在了。
    #
    # 字段数不足的行补 null、超出的截断——两者都已计入 ragged_rows，
    # 不是静默修正。
    rendered_samples = [
        [row[i] if i < len(row) else None for i in range(len(columns_used))]
        for row in sample_rows
    ]

    hint_list = column_hints(columns_used, hint_values, empty_counts)
    if n_cols > HINT_MAX_COLUMNS:
        hint_list = hint_list[:HINT_MAX_COLUMNS]

    notes = []
    if not header["has_header"]:
        notes.append(
            "未识别到表头：`columns` 是位置占位名（column_1…），不是从内容推断的语义名。"
        )
    if truncated:
        notes.append(
            "已到扫描上限 max_scan_rows={}：row_count={} 是**下界**，不是精确值"
            "（row_count_exact=false）。要精确值请提高 max_scan_rows。".format(
                max_scan_rows, row_count
            )
        )
    if ragged_rows:
        notes.append(
            "有 {} 行字段数与表头（{} 列）不一致，样例：{}。样本行已按表头列数补齐"
            "（缺失处为 null），超出部分被丢弃——原始行号见 ragged_examples。".format(
                ragged_rows, n_cols, ragged_examples
            )
        )
    if blank_lines:
        notes.append("跳过 {} 个空行（不计入 row_count）。".format(blank_lines))
    if duplicates:
        notes.append(
            "表头存在重复列名：{}。`columns` 已加 `__N` 后缀去重，原始表头见 `columns_raw`；"
            "样本行是**位置数组**（`sample_rows[i][j]` 对应 `columns[j]`），"
            "故不存在「重复键覆盖」的可能。".format(duplicates)
        )
    if unnamed:
        notes.append("有 {} 个空列名，已命名为 column_N（见 unnamed_columns）。".format(len(unnamed)))
    if delimiter_source != "sniffer":
        notes.append(
            "分隔符 {!r} 的来源是 `{}`（非 Sniffer 判定）。`fallback_scan` = 由候选顺序扫描定出；"
            "`default_comma` = 没有任何候选能让列数 ≥ 2（单列文件），按逗号兜底；"
            "`pinned` = 调用方指定。".format(delimiter, delimiter_source)
        )
    if encoding not in ("utf-8", "utf-8-sig") and encoding_detail["method"] == "sequential_probe":
        notes.append(
            "编码 {} 是顺序试探的结果，不是确定结论（见 encoding_detail.note）。".format(encoding)
        )

    return {
        "ok": True,
        "file": file_meta,
        "encoding": encoding,
        "encoding_detail": encoding_detail,
        "delimiter": delimiter,
        "delimiter_source": delimiter_source,
        "columns": columns_used,
        "columns_raw": columns_raw,
        "header": header,
        "row_count": row_count,
        "row_count_exact": not truncated,
        "scanned_rows": scanned,
        "scan_limit": max_scan_rows,
        "sample_rows": rendered_samples,
        "sample_rows_truncated": (row_count > len(sample_rows)),
        "sample_rows_limit": max_rows,
        "column_hints": hint_list,
        "column_hints_sampled_rows": min(row_count, HINT_SAMPLE_ROWS),
        "column_hints_truncated_columns": max(0, n_cols - HINT_MAX_COLUMNS),
        "column_count": n_cols,
        "ragged_rows": ragged_rows,
        "ragged_examples": ragged_examples,
        "blank_lines": blank_lines,
        "duplicate_columns": duplicates,
        "unnamed_columns": unnamed,
        "notes": notes,
        "computation_boundary": {
            "trust_level": "sidecar",
            "computed": [
                "行数、字段数、空行数等解析结构计数",
                "每列「可解析为某格式」的计数与比例（格式启发式的证据）",
                "文件大小、mtime、sha256（审计凭据）",
            ],
            "not_computed": [
                "任何业务数值：求和、均值、增长率、同比环比、单位换算",
                "值到数值类型的转换（样本值一律是文件里的原样字符串）",
                "列语义判定（金额 / 数量 / 单价 无法由格式区分）",
            ],
            "reason": "「data_ingest」的可信度级别是 Sidecar——仅解析与结构描述，不产生数字。"
            "需要参与计算的数值必须由确定性 Rust 引擎从这些原样字符串出发另行计算并附 provenance。",
        },
    }


# ────────────────────────────────────────────────────────────────────────────
# 入口
# ────────────────────────────────────────────────────────────────────────────


def _emit(payload):
    """把响应写到 stdout。中文以原字写出（stdout 强制 UTF-8）；失败则退回 \\u 转义。"""
    try:
        sys.stdout.reconfigure(encoding="utf-8")
        text = json.dumps(payload, ensure_ascii=False)
    except Exception:
        # 退化成纯 ASCII 的合法 JSON：内容不变，只是中文变成 \uXXXX
        text = json.dumps(payload, ensure_ascii=True)
    sys.stdout.write(text)
    sys.stdout.write("\n")
    sys.stdout.flush()


def main(argv):
    raw = sys.stdin.buffer.read() if len(argv) <= 1 else None
    if len(argv) > 1:
        # 手工排查用的便捷入口：`python3 data_ingest.py <路径> [max_rows]`
        #
        # 这里**替用户把相对路径解析成绝对路径**，与 stdin 协议的严格检查不冲突：
        # 协议那边拒绝相对路径是因为「侧车的 cwd 与调用方的 cwd 可能不同」；
        # 而 CLI 模式下人与脚本在同一台机器上直接敲命令，cwd 就是他的 cwd，
        # 解析是明确的、无歧义的。
        try:
            req = {"path": os.path.abspath(argv[1])}
            if len(argv) > 2:
                req["max_rows"] = int(argv[2])
            request = parse_request(json.dumps(req).encode("utf-8"))
        except (SidecarError, ValueError) as exc:
            err = exc if isinstance(exc, SidecarError) else SidecarError(
                "bad_request", "`max_rows` 须是整数：{}".format(exc)
            )
            _emit(err.as_json())
            return 2
    else:
        try:
            request = parse_request(raw)
        except EnvelopeError as exc:
            _emit(exc.as_json())
            return 2
        except SidecarError as exc:
            # 信封合法、只是某个值不被支持（如 encoding 不在候选内）：
            # 请求**已被受理**，退出码 0。
            _emit(exc.as_json())
            return 0

    try:
        _emit(ingest(request))
    except SidecarError as exc:
        _emit(exc.as_json())
    except Exception as exc:  # 兜底：**任何**异常都要变成类型化响应，不许静默退出
        _emit(
            SidecarError(
                "internal_error",
                "未预期的错误：{}: {}".format(type(exc).__name__, exc),
            ).as_json()
        )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))

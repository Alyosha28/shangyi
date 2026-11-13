#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""`data_ingest.py` 的测试（标准库 `unittest`，不引入 pytest）。

# 为什么通过 subprocess 调脚本本身

侧车的契约**就是**「stdin 进 JSON、stdout 出 JSON、退出码有含义」。
若测试直接 `import` 模块调函数，被绕过的正是这份契约——编码、退出码、
以及「异常是否真的变成了类型化响应」都不会被验证。故每个用例都起一个真实进程。

# 覆盖重点（按重要性排序）

1. **编码**：UTF-8 / GBK（中文中小企业导出最常见的两种）都要能解出**正确的**
   中文列名；解不出来时必须**报错**而不是交付乱码。
2. **失败即拒绝**：不存在的文件、xlsx 容器、二进制、空文件——各自必须是
   **不同的** `error.kind`，不是一个笼统的字符串。
3. **可信度边界**：样本值必须是**原样字符串**，不得被转成数字——侧车不产生
   可进报告的数字（`TrustLevel::Sidecar`）。
4. **行数诚实**：`max_rows` 只截样本，不截 `row_count`；扫描上限生效时
   `row_count_exact` 必须为 false。
"""

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = pathlib.Path(__file__).resolve().parent
SCRIPT = HERE / "data_ingest.py"

UTF8_CSV = (
    "日期,销售额,客户名称,备注\n"
    "2024-01-05,12345.67,北京某某商贸有限公司,首次采购\n"
    "2024-02-05,23456.78,天津某某科技有限公司,\n"
    "2024-03-05,34567.89,上海某某贸易有限公司,回头客\n"
)


def run_sidecar(request, argv=None):
    """调一次侧车，返回 `(响应字典, 退出码, stderr 文本)`。"""
    cmd = [sys.executable, str(SCRIPT)] + (argv or [])
    payload = b"" if argv else json.dumps(request).encode("utf-8")
    proc = subprocess.run(cmd, input=payload, capture_output=True)
    stdout = proc.stdout.decode("utf-8")
    try:
        parsed = json.loads(stdout)
    except json.JSONDecodeError as exc:  # 契约要求 stdout 恒为一份 JSON
        raise AssertionError(
            "stdout 不是合法 JSON：{}；原始输出={!r}；stderr={!r}".format(
                exc, stdout[:400], proc.stderr.decode("utf-8", "replace")[:400]
            )
        )
    return parsed, proc.returncode, proc.stderr.decode("utf-8", "replace")


class SidecarTestCase(unittest.TestCase):
    """公共夹具：一个临时目录，`write(name, bytes_or_str)` 落盘一个文件。"""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="shangyi-digest-")
        self.dir = pathlib.Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def write(self, name, data):
        path = self.dir / name
        if isinstance(data, bytes):
            path.write_bytes(data)
        else:
            path.write_text(data, encoding="utf-8")
        return str(path)

    def ingest(self, path, **extra):
        req = {"path": path}
        req.update(extra)
        resp, rc, err = run_sidecar(req)
        # 契约：写出合法响应即退出码 0（包括 ok:false）——调用方只看 JSON
        self.assertEqual(rc, 0, "退出码应为 0（已写出类型化响应）；stderr={}".format(err))
        return resp

    def expect_error(self, path, kind, **extra):
        resp = self.ingest(path, **extra)
        self.assertFalse(resp.get("ok"), "预期失败，实得 {}".format(resp))
        self.assertEqual(
            resp["error"]["kind"], kind, "错误类型不符：{}".format(resp["error"])
        )
        self.assertTrue(resp["error"]["detail"].strip(), "错误详情不得为空")
        return resp["error"]


# ────────────────────────────────────────────────────────────────────────────
# 1. 正常路径
# ────────────────────────────────────────────────────────────────────────────


class HappyPath(SidecarTestCase):
    def test_utf8_csv_parses_headers_rows_and_hints(self):
        path = self.write("sales.csv", UTF8_CSV)
        resp = self.ingest(path, max_rows=10)

        self.assertEqual(resp["encoding"], "utf-8")
        self.assertEqual(resp["delimiter"], ",")
        # 列名必须是**中文原文**，不是 \uXXXX 转义、不是乱码
        self.assertEqual(resp["columns"], ["日期", "销售额", "客户名称", "备注"])
        self.assertTrue(resp["header"]["has_header"])
        self.assertEqual(resp["row_count"], 3)
        self.assertTrue(resp["row_count_exact"])
        self.assertEqual(len(resp["sample_rows"]), 3)

        hints = {h["column"]: h for h in resp["column_hints"]}
        self.assertEqual(hints["日期"]["hint"], "date")
        self.assertEqual(hints["销售额"]["hint"], "number")
        self.assertEqual(hints["客户名称"]["hint"], "text")
        # **每条建议都要有证据**（不猜语义，只给格式启发式 + 依据）
        for h in resp["column_hints"]:
            self.assertTrue(h["evidence"].strip(), "列建议缺依据：{}".format(h))
        # 备注列有一个空值，要如实计数
        self.assertEqual(hints["备注"]["empty"], 1)

    def test_sample_values_are_verbatim_strings_not_numbers(self):
        """**可信度边界的回归测试**：侧车不得把值转成数字。

        `12345.67` 若被转成 float，下游就会拿到一个「侧车算出来的数」，
        而 Sidecar 级工具没有资格产生进入报告的数字（铁律 1 的闸门在
        `ToolRegistry::check_reportable`）。故这里钉住「原样字符串」。
        """
        path = self.write("sales.csv", UTF8_CSV)
        resp = self.ingest(path)
        row = resp["sample_rows"][0]
        for value in row:
            self.assertIsInstance(value, str, "样本值必须是字符串：{!r}".format(value))
        # 前导零/千分位也不得被规范化；日期不得被变成别的时间格式
        self.assertEqual(row[0], "2024-01-05")
        self.assertEqual(row[1], "12345.67")

    def test_sample_rows_are_positional_arrays_aligned_with_columns(self):
        """样本行是**位置数组**：`sample_rows[i][j]` 对应 `columns[j]`。

        钉住数组形态而不是「列名→值」对象，理由有二，都在实测里出现过：
        ① 对象键序不被 JSON 保证——Rust 侧经 `serde_json::Value` 中转时按
           `BTreeMap` 重排，实测列序被打乱，样本行与表头对不上；
        ② 重复列名在对象里会互相覆盖，静默丢列。
        """
        path = self.write("sales.csv", UTF8_CSV)
        resp = self.ingest(path)
        for row in resp["sample_rows"]:
            self.assertIsInstance(row, list, "样本行须是数组：{!r}".format(row))
            self.assertEqual(len(row), len(resp["columns"]), "样本行长度须与列数一致")
        # 位置对齐：第 1 格是日期、第 3 格是客户名
        self.assertEqual(resp["sample_rows"][0][0], "2024-01-05")
        self.assertEqual(resp["sample_rows"][0][2], "北京某某商贸有限公司")

    def test_max_rows_truncates_samples_but_not_row_count(self):
        rows = "\n".join("2024-01-{:02d},{}".format(i % 28 + 1, i) for i in range(50))
        path = self.write("long.csv", "日期,值\n" + rows + "\n")
        resp = self.ingest(path, max_rows=3)
        self.assertEqual(len(resp["sample_rows"]), 3)
        self.assertEqual(resp["row_count"], 50)
        self.assertTrue(resp["row_count_exact"])
        self.assertTrue(resp["sample_rows_truncated"])

    def test_scan_limit_reports_row_count_as_lower_bound(self):
        """扫描上限生效时必须**如实说未完整扫描**，不得把下界当精确值。"""
        rows = "\n".join("2024-01-{:02d},{}".format(i % 28 + 1, i) for i in range(50))
        path = self.write("long.csv", "日期,值\n" + rows + "\n")
        resp = self.ingest(path, max_scan_rows=10)
        self.assertFalse(resp["row_count_exact"])
        self.assertLess(resp["row_count"], 50)
        self.assertEqual(resp["scanned_rows"], 10)
        self.assertTrue(
            any("下界" in n for n in resp["notes"]), "notes 应说明 row_count 是下界"
        )

    def test_tsv_and_pinned_delimiter(self):
        path = self.write("a.tsv", "日期\t金额\n2024-01-05\t100\n")
        resp = self.ingest(path)
        self.assertEqual(resp["delimiter"], "\t")
        self.assertEqual(resp["columns"], ["日期", "金额"])

        path2 = self.write("b.csv", "姓名;金额\n甲;100\n乙;200\n")
        resp2 = self.ingest(path2, delimiter=";")
        self.assertEqual(resp2["delimiter_source"], "pinned")
        self.assertEqual(resp2["columns"], ["姓名", "金额"])

    def test_utf8_bom_is_stripped_from_first_column_name(self):
        """Excel 导出的 UTF-8 BOM 若不当场剥掉，首列名会带不可见 U+FEFF。"""
        path = self.write("bom.csv", b"\xef\xbb\xbf" + UTF8_CSV.encode("utf-8"))
        resp = self.ingest(path)
        self.assertEqual(resp["encoding"], "utf-8-sig")
        self.assertEqual(resp["encoding_detail"]["method"], "bom")
        self.assertEqual(resp["columns"][0], "日期")
        self.assertNotIn("\ufeff", resp["columns"][0])

    def test_duplicate_and_blank_column_names_are_made_unique_and_reported(self):
        path = self.write("dup.csv", "收入,收入,备注\n1,2,3\n4,5,6\n")
        resp = self.ingest(path)
        self.assertEqual(resp["columns"], ["收入", "收入__2", "备注"])
        self.assertEqual(resp["columns_raw"], ["收入", "收入", "备注"])
        self.assertEqual(resp["duplicate_columns"][0]["name"], "收入")
        # 位置数组形态下**不可能**因重复键而丢列——这正是改用数组的理由之一
        self.assertEqual(len(resp["sample_rows"][0]), 3)
        self.assertEqual(resp["sample_rows"][0], ["1", "2", "3"])

    def test_headerless_file_keeps_row_count_and_marks_positional_names(self):
        path = self.write("nohdr.csv", "2024-01-05,100\n2024-02-05,200\n")
        resp = self.ingest(path)
        self.assertFalse(resp["header"]["has_header"])
        self.assertEqual(resp["columns"], ["column_1", "column_2"])
        self.assertEqual(resp["row_count"], 2, "无表头时首行也是数据，不得少算一行")
        self.assertEqual(resp["sample_rows"][0][0], "2024-01-05")

    def test_numeric_looking_dates_are_reported_as_ambiguous_not_guessed(self):
        """`20240105` 既是日期也是整数——不挑一个蒙，如实报 ambiguous。"""
        path = self.write("amb.csv", "月份\n20240105\n20240106\n")
        resp = self.ingest(path)
        self.assertEqual(resp["column_hints"][0]["hint"], "ambiguous")
        self.assertIn("同时", resp["column_hints"][0]["evidence"])

    def test_ragged_rows_are_counted_and_explained(self):
        path = self.write("ragged.csv", "a,b,c\n1,2,3\n1,2\n1,2,3,4\n")
        resp = self.ingest(path)
        self.assertEqual(resp["row_count"], 3)
        self.assertEqual(resp["ragged_rows"], 2)
        self.assertEqual([e["cells"] for e in resp["ragged_examples"]], [2, 4])
        # 补齐是**显式动作**：缺失处是 null，且逐条记了原始行号
        self.assertIsNone(resp["sample_rows"][1][2])

    def test_computation_boundary_is_declared_in_the_response(self):
        path = self.write("sales.csv", UTF8_CSV)
        resp = self.ingest(path)
        boundary = resp["computation_boundary"]
        self.assertEqual(boundary["trust_level"], "sidecar")
        self.assertTrue(any("增长" in x for x in boundary["not_computed"]))

    def test_response_is_deterministic_for_the_same_file(self):
        path = self.write("sales.csv", UTF8_CSV)
        first = self.ingest(path)
        second = self.ingest(path)
        self.assertEqual(first, second, "同一文件两次调用必须给出完全相同的响应")

    def test_cli_form_accepts_relative_path(self):
        """手工排查入口：人与脚本同 cwd，故相对路径在此**可以**解析。"""
        self.write("rel.csv", UTF8_CSV)
        old = os.getcwd()
        os.chdir(self.dir)
        try:
            resp, rc, _ = run_sidecar(None, argv=["rel.csv", "2"])
        finally:
            os.chdir(old)
        self.assertEqual(rc, 0)
        self.assertTrue(resp["ok"])
        self.assertEqual(resp["columns"][0], "日期")
        self.assertEqual(len(resp["sample_rows"]), 2)


# ────────────────────────────────────────────────────────────────────────────
# 2. 编码
# ────────────────────────────────────────────────────────────────────────────


class Encoding(SidecarTestCase):
    def test_gbk_csv_is_decoded_to_correct_chinese_not_mojibake(self):
        """中文中小企业导出的 CSV 常是 GBK/GB18030，不是 UTF-8。"""
        path = self.write("gbk.csv", UTF8_CSV.encode("gbk"))
        resp = self.ingest(path)
        self.assertEqual(resp["encoding"], "gb18030")
        self.assertEqual(resp["encoding_detail"]["method"], "sequential_probe")
        self.assertEqual(resp["columns"], ["日期", "销售额", "客户名称", "备注"])
        # 探测不是结论，响应里必须留话
        self.assertIn("不是", resp["encoding_detail"]["note"])
        self.assertEqual(resp["sample_rows"][0][2], "北京某某商贸有限公司")

    def test_big5_is_refused_rather_than_delivered_as_mojibake(self):
        """GB18030 能把 Big5 字节**强行解出来**，但解出的是乱码。

        若放行，乱码列名会被下游当成真实列名——比「读不出来」更坏。
        故此处断言**拒绝**，并断言错误里给出了应对（显式指定编码）。
        """
        path = self.write("big5.csv", "日期,金額\n2024-01-05,100\n".encode("big5"))
        err = self.expect_error(path, "encoding_undetectable")
        self.assertIn("encoding", err["detail"])

    def test_undecodable_binary_is_refused_not_read_as_utf8(self):
        path = self.write("blob.bin", bytes(range(256)) * 16)
        self.expect_error(path, "encoding_undetectable")

    def test_pinned_encoding_is_honoured_and_failure_is_reported(self):
        path = self.write("gbk.csv", UTF8_CSV.encode("gbk"))
        resp = self.ingest(path, encoding="gbk")
        self.assertEqual(resp["encoding"], "gbk")
        self.assertEqual(resp["encoding_detail"]["method"], "pinned")
        self.assertEqual(resp["columns"][0], "日期")

        # 指定错了：**不自动改用别的编码**（显式指定是明确指令）
        self.expect_error(path, "encoding_undetectable", encoding="utf-8")

    def test_unsupported_encoding_name_is_reported_as_such(self):
        path = self.write("sales.csv", UTF8_CSV)
        self.expect_error(path, "encoding_not_supported", encoding="utf-7")

    def test_shift_jis_is_a_documented_blind_spot(self):
        """探测的**已知盲区**：Shift-JIS 字节能被 GB18030 完整解成生僻汉字。

        这里钉住的是「响应里写明了这条边界」，而不是「能识别 Shift-JIS」——
        后者本侧车做不到（不该假装做到）。
        """
        path = self.write("sjis.csv", "売上高\n100\n".encode("shift_jis"))
        resp = self.ingest(path)
        if resp.get("ok"):
            self.assertEqual(resp["encoding"], "gb18030")
            self.assertIn("Shift-JIS", resp["encoding_detail"]["note"])
            self.assertTrue(any("试探" in n for n in resp["notes"]))
        else:
            # 也接受拒绝（不同字节序列的运气不同）
            self.assertEqual(resp["error"]["kind"], "encoding_undetectable")


# ────────────────────────────────────────────────────────────────────────────
# 3. 失败即拒绝（每种失败是**不同**的类型化错误）
# ────────────────────────────────────────────────────────────────────────────


class Refusals(SidecarTestCase):
    def test_missing_file(self):
        err = self.expect_error(str(self.dir / "不存在.csv"), "file_not_found")
        self.assertIn("不存在", err["detail"])

    def test_directory_is_not_a_file(self):
        self.expect_error(str(self.dir), "not_a_file")

    def test_empty_file(self):
        err = self.expect_error(self.write("empty.csv", b""), "empty_file")
        self.assertIn("0 字节", err["detail"])

    def test_file_with_only_blank_lines(self):
        self.expect_error(self.write("blank.csv", "\n\n\n"), "empty_file")

    def test_renamed_xlsx_is_refused_by_content_magic_not_extension(self):
        """把 xlsx 改名成 .csv 在实务里真实存在——故按**内容**判，不只看扩展名。"""
        path = self.write("looks_like.csv", b"PK\x03\x04" + b"\x00" * 64)
        err = self.expect_error(path, "unsupported_format")
        self.assertIn("xlsx", err["detail"])
        self.assertIn("CSV", err["detail"])

    def test_xlsx_extension_is_refused_with_actionable_advice(self):
        path = self.write("真表.xlsx", b"PK\x03\x04" + b"\x00" * 64)
        self.expect_error(path, "unsupported_format")

    def test_legacy_xls_ole2_container_is_refused(self):
        path = self.write("old.xls", b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1" + b"\x00" * 64)
        self.expect_error(path, "unsupported_format")

    def test_a_text_file_that_is_not_csv_is_not_a_failure(self):
        """JSON/日志等文本**是**合法的单列 CSV——如实按单列解析。

        「非 CSV」不是**格式**问题而是语义问题：分隔符文本这一层无法区分。
        本用例钉住这种「不假装能判断」的行为。
        """
        path = self.write("data.json", '{"a": 1}\n')
        resp = self.ingest(path)
        self.assertTrue(resp["ok"])
        self.assertEqual(resp["column_count"], 1)
        self.assertEqual(resp["delimiter_source"], "default_comma")  # 单列 → 兜底

    def test_bad_requests_are_typed_not_defaulted(self):
        self.assertEqual(run_sidecar({})[0]["error"]["kind"], "bad_request")
        self.assertEqual(
            run_sidecar({"path": "relative.csv"})[0]["error"]["kind"], "bad_request"
        )
        path = self.write("sales.csv", UTF8_CSV)
        self.assertEqual(
            run_sidecar({"path": path, "max_rows": "abc"})[0]["error"]["kind"],
            "bad_request",
        )
        self.assertEqual(
            run_sidecar({"path": path, "max_rows": -1})[0]["error"]["kind"], "bad_request"
        )
        self.assertEqual(
            run_sidecar({"path": path, "delimiter": "x"})[0]["error"]["kind"], "bad_request"
        )

    def test_broken_stdin_still_yields_a_typed_response(self):
        proc = subprocess.run(
            [sys.executable, str(SCRIPT)], input=b"not json at all", capture_output=True
        )
        resp = json.loads(proc.stdout.decode("utf-8"))
        self.assertFalse(resp["ok"])
        self.assertEqual(resp["error"]["kind"], "bad_request")
        self.assertEqual(proc.returncode, 2, "读不出请求时退出码为 2")


# ────────────────────────────────────────────────────────────────────────────
# 4. 只依赖标准库
# ────────────────────────────────────────────────────────────────────────────


class NoHeavyDependencies(unittest.TestCase):
    def test_script_only_imports_stdlib(self):
        """侧车刻意不引入 pandas/openpyxl——本项目避免重依赖。

        用静态检查（而非运行检查）**是刻意的**：本机装了 pandas 时，
        运行检查会通过，而重依赖照样被引入。故直接查 import 清单。
        """
        source = SCRIPT.read_text(encoding="utf-8")
        import ast

        tree = ast.parse(source)
        imported = set()
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                imported.update(a.name.split(".")[0] for a in node.names)
            elif isinstance(node, ast.ImportFrom) and node.module:
                imported.add(node.module.split(".")[0])
        allowed = {
            "codecs", "csv", "datetime", "hashlib", "io", "json",
            "os", "pathlib", "re", "sys", "unicodedata",
        }
        self.assertTrue(imported <= allowed, "出现非标准库/未列出的依赖：{}".format(imported - allowed))


if __name__ == "__main__":
    unittest.main(verbosity=2)

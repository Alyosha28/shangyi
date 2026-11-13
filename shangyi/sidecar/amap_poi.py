#!/usr/bin/env python3
"""高德周边 POI：把「周边有多少竞品、多少写字楼、几个地铁口」变成硬事实。

# 它补的是哪一层

决策帧里的「已核实」此前只能来自网页检索——搜"南京路奶茶店"抓到的正文多半
残缺。高德 POI 接口能直接回答**计数**：500 米内多少家茶饮咖啡、几家超市、
几个地铁口、该品牌自己已经开了几家。这些是坐标级的硬事实，不是模型猜的。

# 三条纪律

1. **查不到就说查不到**：限流（`CUQPS_*`）、配额用尽、地址解析失败，一律
   如实上报，绝不返回 0 当"没有"。计数为 0 与"没查到"是两件事。
2. **不落库 POI 明细**：高德条款限制 POI 数据的长期存储与再分发。本模块只在
   查询时返回结果；存档时只留**计数与口径**（调用方负责剥掉 `samples`）。
3. **串行 + 退避**：免费 key 的 QPS 很低（实测并发立刻 `CUQPS_HAS_EXCEEDED_THE_LIMIT`），
   故逐个类目顺序请求，限流时退避重试一次，仍失败则如实标注该项未取得。

密钥来源：`AMAP_KEY` 环境变量，或 `~/.shangyi-secrets/amap.env`（`export AMAP_KEY=...`）。
"""

from __future__ import annotations

import json
import os
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

GEOCODE_URL = "https://restapi.amap.com/v3/geocode/geo"
AROUND_URL = "https://restapi.amap.com/v3/place/around"

# 类目 → 高德 POI 类型码（多码用 | 表示「或」）。码值均已实测非空。
CATEGORIES = (
    ("茶饮咖啡", "050700|050500|050900"),   # 冷饮店 | 咖啡厅 | 甜品店
    ("餐饮", "050000"),
    ("商场", "060100"),
    ("超市便利", "060400|060200"),
    ("写字楼", "120201"),
    ("住宅区", "120300"),
    ("地铁站", "150500"),
    ("公交站", "150700"),
)

DEFAULT_RADIUS = 500
MAX_RADIUS = 3000
# 限流与网络抖动都值得重试一次；仍失败就如实报未取得。
RETRY_DELAY = 1.2
CALL_INTERVAL = 0.4


class AmapError(RuntimeError):
    """调用高德失败。message 里带 info 码，便于判断是限流还是配置问题。"""


def load_key(environ=None) -> str | None:
    environ = environ if environ is not None else os.environ
    key = (environ.get("AMAP_KEY") or "").strip()
    if key:
        return key
    path = Path(environ.get("SHANGYI_SECRETS") or (Path.home() / ".shangyi-secrets")) / "amap.env"
    try:
        for line in path.read_text(encoding="utf-8").splitlines():
            line = line.strip()
            if line.startswith("export "):
                line = line[len("export "):]
            name, _, value = line.partition("=")
            if name.strip() == "AMAP_KEY" and value.strip():
                return value.strip().strip("'\"")
    except OSError:
        return None
    return None


def _default_http(url: str, timeout: float) -> dict:
    request = urllib.request.Request(url, headers={"User-Agent": "shangyi/0.1 (+local)"})
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return json.loads(response.read().decode("utf-8"))


def _call(url: str, params: dict, *, key: str, http, sleeper, timeout: float) -> dict:
    """一次 GET，失败按「限流可重试、其余不重试」处理。"""
    query = urllib.parse.urlencode({**params, "key": key})
    full = f"{url}?{query}"
    last = ""
    for attempt in range(2):
        try:
            payload = http(full, timeout)
        except (urllib.error.URLError, TimeoutError, ValueError) as error:
            last = f"网络失败：{error}"
            payload = None
        if payload is not None:
            if str(payload.get("status")) == "1":
                return payload
            info = str(payload.get("info") or "未知错误")
            last = info
            retryable = info.startswith("CUQPS") or "LIMIT" in info.upper() or "BUSY" in info.upper()
            if not retryable:
                break
        if attempt == 0:
            sleeper(RETRY_DELAY)
    raise AmapError(last or "高德未返回结果")


def _count(payload: dict) -> int:
    raw = payload.get("count")
    try:
        return int(raw)
    except (TypeError, ValueError):
        raise AmapError(f"返回里没有可用计数：{raw!r}")


def _names(payload: dict, limit: int = 3) -> list[str]:
    names = []
    for poi in payload.get("pois") or []:
        name = str(poi.get("name") or "").strip()
        if name and name not in names:
            names.append(name)
        if len(names) >= limit:
            break
    return names


def geocode(address: str, *, key: str, http=None, sleeper=time.sleep, timeout: float = 12.0) -> dict:
    """地址 → 坐标。解析不到就报错，不猜一个坐标。"""
    http = http or _default_http
    address = str(address or "").strip()
    if not address:
        raise AmapError("请填写要查的地址")
    payload = _call(GEOCODE_URL, {"address": address}, key=key, http=http, sleeper=sleeper, timeout=timeout)
    entries = payload.get("geocodes") or []
    if not entries:
        raise AmapError(f"这个地址解析不出坐标：{address}")
    first = entries[0]
    location = str(first.get("location") or "")
    if "," not in location:
        raise AmapError(f"坐标格式异常：{location!r}")
    return {
        "location": location,
        "formatted_address": first.get("formatted_address") or address,
        "district": first.get("district") or "",
        "city": first.get("city") or "",
    }


def survey(address: str, *, key: str, radius: int = DEFAULT_RADIUS,
           categories=CATEGORIES, brand: str = "", http=None, sleeper=time.sleep,
           timeout: float = 12.0) -> dict:
    """一个地址的周边计数：逐类目串行查询，限流/失败逐项如实标注。"""
    http = http or _default_http
    radius = max(100, min(int(radius or DEFAULT_RADIUS), MAX_RADIUS))
    place = geocode(address, key=key, http=http, sleeper=sleeper, timeout=timeout)
    counts, unavailable = [], []
    for index, (label, types) in enumerate(categories):
        if index:
            sleeper(CALL_INTERVAL)          # 免费 key 的 QPS 很低，串行且留间隔
        try:
            payload = _call(AROUND_URL, {"location": place["location"], "radius": radius,
                                         "types": types, "offset": 25, "page": 1,
                                         "extensions": "base"},
                            key=key, http=http, sleeper=sleeper, timeout=timeout)
            counts.append({"label": label, "count": _count(payload), "samples": _names(payload)})
        except AmapError as error:
            unavailable.append({"label": label, "why": str(error)})

    brand_count = None
    if brand:
        sleeper(CALL_INTERVAL)
        try:
            payload = _call(AROUND_URL, {"location": place["location"], "radius": radius,
                                         "keywords": brand, "offset": 25, "page": 1, "extensions": "base"},
                            key=key, http=http, sleeper=sleeper, timeout=timeout)
            brand_count = {"label": f"{brand}（同品牌）", "count": _count(payload), "samples": _names(payload)}
        except AmapError as error:
            unavailable.append({"label": f"{brand}（同品牌）", "why": str(error)})

    result = {
        "ok": True,
        "address": place["formatted_address"],
        "location": place["location"],
        "district": place["district"],
        "radius": radius,
        "source": "高德开放平台 Web 服务（POI 周边搜索）",
        "retrieved_at": datetime.now(timezone.utc).isoformat(timespec="seconds"),
        "counts": counts,
        "unavailable": unavailable,
        "notes": [
            f"计数为「{radius} 米内该类目的 POI 数量」，不是营业额或客流。",
            "POI 为查询时结果，按高德条款不长期保存明细；存档只留计数与口径。",
        ],
    }
    if brand_count:
        result["brand"] = brand_count
    return result


def counts_only(survey_result: dict) -> dict:
    """存档用的精简版：只留计数与口径，剥掉 POI 明细（合规）。"""
    if not isinstance(survey_result, dict) or not survey_result.get("ok"):
        return {}
    def strip(item):
        return {"label": item.get("label"), "count": item.get("count")}
    return {
        "address": survey_result.get("address"),
        "radius": survey_result.get("radius"),
        "source": survey_result.get("source"),
        "retrieved_at": survey_result.get("retrieved_at"),
        "counts": [strip(x) for x in survey_result.get("counts") or []],
        "brand": strip(survey_result["brand"]) if survey_result.get("brand") else None,
        "unavailable": survey_result.get("unavailable") or [],
    }

#!/usr/bin/env python3
"""数字摆局存档：把「按数字摆出来的局」存成可回看的记录。

# 与案例库的关系（为什么另存一处）

`$SHANGYI_HOME/cases/` 是**引擎**写的事件日志，sidecar 对它只做投影——这是
项目的一条硬约束（数据只有一个真相源）。网页里按数字摆的局不是引擎跑出来的
咨询，硬塞进案例库会造出一份没有事件来源的记录。

所以单独存 `$SHANGYI_HOME/web-models/<id>.json`，并在列表里明确标成
「数字摆局」，不冒充咨询记录。

# 存什么

输入（bundle）是真相源；矩阵/解集/扫描结果是**当时的快照**，一并存下便于
回看与审计——引擎若升级，快照仍能说明当时算的是什么。
"""

from __future__ import annotations

import json
import os
import re
import secrets
import time
from pathlib import Path

MODEL_ID_RE = re.compile(r"^model-[a-f0-9]{12}$")
MAX_TITLE = 120


class ModelStoreError(ValueError):
    pass


def _root(home) -> Path:
    return Path(home) / "web-models"


def new_id() -> str:
    return "model-" + secrets.token_hex(6)


def _wan(value) -> str:
    try:
        number = float(value)
    except (TypeError, ValueError):
        return ""
    text = f"{number / 10000:.4g}万"
    return text


def describe(bundle: dict) -> str:
    """给记录起个能认出来的名字：谁对谁、什么动作、多大客流。"""
    me = bundle.get("me") if isinstance(bundle.get("me"), dict) else {}
    rival = bundle.get("rival") if isinstance(bundle.get("rival"), dict) else {}
    market = bundle.get("market") if isinstance(bundle.get("market"), dict) else {}
    me_name = str(me.get("name") or "我方")
    rival_name = str(rival.get("name") or "对方")
    moves = me.get("moves") if isinstance(me.get("moves"), list) else []
    action = ""
    if len(moves) > 1 and isinstance(moves[1], dict):
        action = str(moves[1].get("name") or "")
    traffic = _wan(market.get("traffic"))
    parts = [f"{me_name} × {rival_name}"]
    if action:
        parts.append(action)
    if traffic:
        parts.append(f"客流 {traffic}")
    return " · ".join(parts)[:MAX_TITLE]


def save(home, record: dict) -> dict:
    """原子写入一条记录；id 由本模块生成。"""
    if not isinstance(record, dict):
        raise ModelStoreError("记录必须是对象")
    bundle = record.get("bundle")
    if not isinstance(bundle, dict):
        raise ModelStoreError("记录缺少 bundle")
    model_id = str(record.get("id") or "")
    if model_id and not MODEL_ID_RE.fullmatch(model_id):
        raise ModelStoreError("记录标识不合法")
    stored = {
        "id": model_id or new_id(),
        "created_at": float(record.get("created_at") or time.time()),
        "title": str(record.get("title") or describe(bundle))[:MAX_TITLE],
        "bundle": bundle,
        "spec": record.get("spec"),
        "facts": record.get("facts") or {},
        "cells": record.get("cells") or [],
        "payback_months": record.get("payback_months") or {},
        "unmodeled": record.get("unmodeled") or [],
        "warning": record.get("warning") or [],
        "solution": record.get("solution"),
        "robustness": record.get("robustness"),
    }
    root = _root(home)
    root.mkdir(parents=True, exist_ok=True)
    target = root / f"{stored['id']}.json"
    if target.exists():
        raise ModelStoreError("记录已存在")
    tmp = root / f".{stored['id']}.tmp"
    tmp.write_text(json.dumps(stored, ensure_ascii=False, indent=1), encoding="utf-8")
    os.replace(tmp, target)          # 原子落盘：不留半截文件
    return stored


def load(home, model_id: str) -> dict | None:
    if not MODEL_ID_RE.fullmatch(str(model_id or "")):
        return None
    path = _root(home) / f"{model_id}.json"
    try:
        record = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return record if isinstance(record, dict) else None


def listing(home) -> list[dict]:
    """清单：只回列表需要的字段，破损记录跳过而不是让整页失败。"""
    rows = []
    for path in _root(home).glob("model-*.json"):
        if not MODEL_ID_RE.fullmatch(path.stem):
            continue
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, ValueError):
            continue
        if not isinstance(record, dict):
            continue
        solution = record.get("solution") if isinstance(record.get("solution"), dict) else {}
        baseline = None
        for eq in solution.get("equilibria") or []:
            if isinstance(eq, dict) and eq.get("kind") == "Pure":
                baseline = eq.get("payoffs", [None])[0]
                break
        rows.append({
            "id": record.get("id") or path.stem,
            "created_at": record.get("created_at") or 0,
            "title": record.get("title") or "数字摆局",
            "me_profit": baseline,
            "equilibria": len(solution.get("equilibria") or []),
            "scanned": bool(record.get("robustness")),
        })
    rows.sort(key=lambda r: r["created_at"], reverse=True)
    return rows

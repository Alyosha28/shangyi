"""检索健康：只报**凭据是否配置**，绝不代用户发起实时探测。

# 为什么不做实时探测（两件事一起说清）

1. **按次计费**：博查每次调用都计费。健康检查在每个页面加载时都会跑，
   用它去"验证搜索可用"等于把成本挂到浏览行为上——那是成本 bug，不是功能。
2. **未探测 ≠ 可用**：没验证过的能力不能写成"已取得结果"。故状态只有
   `configured` / `unconfigured` / `unknown` 三态，措辞与之一致；
   真正可用与否由**实际查询**回答（查询结果会带来源与健康度）。

自托管 SearXNG 已于 2026-10 删除：它要用户自己维护实例，而检索质量与可用性
都不值得那条运维成本；删除后本项目只有一个检索后端（商业 API）。
"""
import os
from pathlib import Path


def bocha_configuration_status():
    """Report credential presence only; never expose its value or call the API."""
    if os.environ.get('BOCHA_API_KEY', '').strip():
        return 'configured'
    path = Path.home() / '.shangyi-secrets' / 'bocha.env'
    try:
        for line in path.read_text(encoding='utf-8').splitlines():
            line = line.strip().removeprefix('export ')
            key, separator, value = line.partition('=')
            if separator and key.strip() == 'BOCHA_API_KEY' and value.strip():
                return 'configured'
    except FileNotFoundError:
        return 'unconfigured'
    except (OSError, UnicodeError):
        return 'unknown'
    return 'unconfigured'


def health():
    bocha_status = bocha_configuration_status()
    detail = {
        'configured': '博查凭据已配置；为避免按次计费，健康检查不调用接口，实际搜索时再验证',
        'unconfigured': '未配置博查凭据：检索会报「未取得」；抓取（web_fetch）仍可用',
        'unknown': '无法读取博查凭据配置（文件不可读）；检索前请确认配置',
    }[bocha_status]
    search = {'status': bocha_status, 'detail': detail,
              'providers': {'bocha': {'status': bocha_status, 'detail': detail}}}
    return {'search': search,
            'model': {'status': 'not_checked', 'detail': '每次新咨询启动时进行真实工具调用自检'},
            'fetch': {'status': 'source_dependent', 'detail': '逐来源检查；搜索可用不代表所有原文可抓取'}}

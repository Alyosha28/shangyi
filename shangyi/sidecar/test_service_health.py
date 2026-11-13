import unittest
import os
import tempfile
from pathlib import Path
from unittest.mock import patch
from io import BytesIO
from service_health import bocha_configuration_status, health

class HealthTests(unittest.TestCase):
    """健康检查只报凭据状态：**不探测**（博查按次计费），也不把未探测说成可用。"""

    def test_configured_credentials_are_reported_but_never_probed(self):
        with patch.dict(os.environ, {'BOCHA_API_KEY': 'sk-health-sentinel'}), \
             patch('urllib.request.urlopen', side_effect=AssertionError('健康检查不得发起网络调用')):
            result = health()
        self.assertEqual(result['search']['status'], 'configured')
        self.assertNotIn('sk-health-sentinel', repr(result))
        self.assertIn('不调用接口', result['search']['detail'])

    def test_unconfigured_is_reported_as_unconfigured_not_as_unavailable(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch('service_health.Path.home', return_value=Path(directory)), \
                 patch.dict(os.environ, {'BOCHA_API_KEY': ''}):
                result = health()
        self.assertEqual(result['search']['status'], 'unconfigured')
        self.assertEqual(result['search']['providers']['bocha']['status'], 'unconfigured')
        self.assertIn('未取得', result['search']['detail'])

    def test_unreadable_credential_file_is_unknown_not_missing(self):
        with patch('service_health.Path.home', return_value=Path('/nonexistent-home-for-test')), \
             patch.dict(os.environ, {'BOCHA_API_KEY': ''}):
            result = health()
        self.assertEqual(result['search']['status'], 'unconfigured', '文件不存在就是未配置')
        with patch('service_health.bocha_configuration_status', return_value='unknown'):
            self.assertEqual(health()['search']['status'], 'unknown')

    def test_bocha_secret_file_is_detected_without_returning_its_value(self):
        with tempfile.TemporaryDirectory() as directory:
            temp_home = Path(directory)
            secrets = temp_home / '.shangyi-secrets'
            secrets.mkdir(parents=True, exist_ok=True)
            secret_file = secrets / 'bocha.env'
            secret_file.write_text('export BOCHA_API_KEY=sk-health-test-only\n', encoding='utf-8')
            with patch('service_health.Path.home', return_value=temp_home), patch.dict(os.environ, {'BOCHA_API_KEY': ''}):
                self.assertEqual(bocha_configuration_status(), 'configured')

    def test_health_keeps_the_payload_shape_the_frontend_reads(self):
        with patch.dict(os.environ, {'BOCHA_API_KEY': 'sk-shape-check'}):
            result = health()
        for key in ('search', 'model', 'fetch'):
            self.assertIn(key, result)
        self.assertIn('status', result['search'])
        self.assertEqual(result['search']['providers']['bocha']['status'], 'configured')

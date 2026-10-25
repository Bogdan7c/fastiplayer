"""Функциональные проверки collector-а с настоящими дочерними процессами."""

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class CollectorRuntimeTests(unittest.TestCase):
    def run_attempt(self, command):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        completed = subprocess.run([
            sys.executable, str(Path(__file__).with_name('collect.py')),
            '--attempt', '1', '--phase', 'validation',
            '--scenario', 'av1-4k60-sw', '--player', 'fastiplayer',
            '--settle', '0', '--window-start', '0.2', '--duration', '1.2',
            '--output', str(root / 'result.json'), '--log', str(root / 'runtime.log'),
            '--', sys.executable, '-c', command,
        ], capture_output=True, text=True, timeout=10)
        return completed, json.loads((root / 'result.json').read_text())

    def test_live_cpu_and_memory_are_measured_over_requested_window(self):
        completed, result = self.run_attempt('allocation = bytearray(16 * 1024 * 1024)\nwhile True: sum(range(10000))')
        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertGreater(result['cpu_percent_one_logical_core'], 10)
        self.assertGreater(result['rss_kib_sample_min'], 16000)
        self.assertGreaterEqual(result['window_seconds'], 1.2)
        self.assertLess(result['window_seconds'], 1.5)
        self.assertGreaterEqual(result['samples'][0]['elapsed_seconds'], 0.2)
        self.assertFalse(result['forced_kill'])

    def test_early_exit_is_retained_and_excluded(self):
        completed, result = self.run_attempt('raise SystemExit(7)')
        self.assertEqual(completed.returncode, 1)
        self.assertEqual(result['exit_code_after_cleanup'], 7)
        self.assertIn('process_exited_before_window_end', result['exclusions'])
        self.assertNotIn('cpu_percent_one_logical_core', result)



class PlaybackLogProofTests(unittest.TestCase):
    """Признаки этапов по логу плеера: оба семейства рендера различимы."""

    def proofs(self, log_text):
        # Модуль грузится по пути: в соседнем каталоге `local/` есть свой `collect.py`.
        spec = importlib.util.spec_from_file_location('t480s_collect', Path(__file__).with_name('collect.py'))
        collect = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(collect)
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        log_path = Path(directory.name) / 'runtime.log'
        log_path.write_text(log_text)
        return collect.playback_log_proofs(log_path)

    def test_gles2_software_and_hardware_plans_are_reported_separately_from_wgpu(self):
        software = self.proofs('INFO Selected video pipeline plan="ffmpeg-host-upload-gles2"\n')
        self.assertTrue(software['rust_software_pipeline_gles2'])
        self.assertFalse(software['rust_software_pipeline'])
        self.assertFalse(software['rust_hardware_pipeline_gles2'])
        hardware = self.proofs('INFO Zero-copy resource lifecycle contract configured '
                               'backend_contract="VA-API EGL DMA-BUF"\n'
                               'INFO Selected video pipeline plan="vaapi-dmabuf-gles2"\n')
        self.assertTrue(hardware['rust_hardware_pipeline_gles2'])
        self.assertTrue(hardware['rust_dmabuf_pipeline_gles2'])
        self.assertFalse(hardware['rust_dmabuf_pipeline'])

    def test_wgpu_markers_are_unchanged(self):
        proofs = self.proofs('INFO Selected video pipeline plan="ffmpeg-host-upload-wgpu"\n'
                             'INFO backend_contract="VA-API Vulkan DMA-BUF"\n')
        self.assertTrue(proofs['rust_software_pipeline'])
        self.assertTrue(proofs['rust_dmabuf_pipeline'])
        self.assertFalse(proofs['rust_software_pipeline_gles2'])
        self.assertFalse(proofs['rust_dmabuf_pipeline_gles2'])


if __name__ == '__main__':
    unittest.main()

"""Проверяем конечный JSON отчёт из сохранённых attempts, включая запрет смешения условий."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class SnapshotReportTests(unittest.TestCase):
    def test_report_exposes_cpu_win_and_memory_growth_without_approving_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, cpu, memory in [('original', 10, 100), ('current', 5, 200)]:
                (root / name).mkdir()
                (root / name / 'tool-provenance.json').write_text(json.dumps({'tool_sha256': {'collector': 'same'}}))
                for repetition in range(1, 4):
                    attempt = root / name / 'attempts' / str(repetition)
                    attempt.mkdir(parents=True)
                    spec = {'snapshot': name, 'scenario': 'motion', 'mode': 'quiet', 'player': 'fastiplayer',
                            'repetition': repetition, 'media_sha256': 'same-media', 'config_sha256': 'same-config',
                            'warmup_seconds': 12, 'duration_seconds': 30, 'interval_seconds': 1,
                            'display_size': [1920, 1080], 'window_helper_sha256': 'same-helper', 'codec': 'h264'}
                    payload = {'spec': spec, 'status': 'VALID_RESOURCE_OBSERVATION', 'clk_tck': 100,
                               'metrics': {'tree_cpu_percent': cpu, 'rss_sum_kib': {'mean': memory, 'sampled_max': memory},
                                           'pss_sum_kib': {'mean': memory, 'sampled_max': memory}}}
                    (attempt / 'result.json').write_text(json.dumps(payload))
            command = [sys.executable, str(Path(__file__).with_name('compare_snapshots.py')),
                       str(root / 'original'), str(root / 'current'), str(root / 'report.json')]
            completed = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            report = json.loads((root / 'report.json').read_text())['comparisons'][0]
            self.assertEqual(report['cpu']['status'], 'WIN')
            self.assertEqual(report['memory_kib_differences']['pss_sum_kib_mean']['status'], 'WORSE')
            self.assertEqual(report['memory_kib_differences']['pss_sum_kib_mean']['unit'], 'KiB')
            self.assertEqual(report['quality_and_host_equivalence'], 'MANUAL REVIEW REQUIRED')
            changed = root / 'current/attempts/1/result.json'
            payload = json.loads(changed.read_text())
            payload['spec']['media_sha256'] = 'different-media'
            changed.write_text(json.dumps(payload))
            completed = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn('incompatible protocol', completed.stderr)


if __name__ == '__main__':
    unittest.main()

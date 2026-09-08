"""Функциональные проверки Linux collector: реальные processes, children и сохранённый незачёт."""

import json
from pathlib import Path
import sys
import tempfile
import time
import unittest

from analyze import comparison
from collect import collect, summarize
from host import cpu_percent, sample
from service import MeasuredService, isolated_environment


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='fastiplayer-collector-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def launch(self, script):
        service = MeasuredService([sys.executable, '-c', script],
                                  isolated_environment(self.root, 'info'), self.root)
        self.addCleanup(service.stop)
        service.start()
        return service

    def test_short_lived_children_contribute_after_they_are_reaped(self):
        service = self.launch('''import subprocess, sys, time
from pathlib import Path
root = Path("''' + str(self.root) + '''")
while not (root / 'go').exists(): time.sleep(0.01)
for _ in range(3):
 subprocess.run([sys.executable, '-c', 'import time; end=time.process_time()+0.12\\nwhile time.process_time()<end: pass'], check=True)
(root / 'done').touch()
time.sleep(30)
''')
        first = sample(service.pid, service.cgroup)
        (self.root / 'go').touch()
        deadline = time.monotonic() + 5
        while not (self.root / 'done').exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue((self.root / 'done').exists())
        last = sample(service.pid, service.cgroup)
        self.assertEqual(len(last['processes']), 1)
        self.assertGreater(last['cgroup_cpu']['usage_usec'] - first['cgroup_cpu']['usage_usec'], 300000)
        metrics = summarize([first, last])
        self.assertGreater(metrics['tree_cpu_percent'], metrics['process_cpu_percent'] + 20)

    def test_live_workload_cpu_memory_and_cleanup(self):
        service = self.launch('allocation=bytearray(16*1024*1024)\nwhile True: sum(range(10000))')
        time.sleep(0.2)
        first = sample(service.pid, service.cgroup)
        time.sleep(0.4)
        last = sample(service.pid, service.cgroup)
        metrics = summarize([first, last])
        self.assertGreater(metrics['process_cpu_percent'], 10)
        self.assertGreater(metrics['pss_sum_kib']['mean'], 16000)
        ended = service.stop()
        self.assertFalse(ended['forced_kill'])
        self.assertEqual(ended['after_signal']['MainPID'], '0')

    def test_early_exit_cannot_be_sampled_as_zero(self):
        service = self.launch('import time; time.sleep(0.3); raise SystemExit(7)')
        time.sleep(0.6)
        with self.assertRaises((FileNotFoundError, ProcessLookupError, RuntimeError)):
            sample(service.pid, service.cgroup)
        ended = service.stop()
        self.assertEqual(ended['after_signal']['ExecMainStatus'], '7')

    def test_invalid_timing_retains_reason_and_no_cpu(self):
        result = collect({'warmup_seconds': 12, 'duration_seconds': 0, 'interval_seconds': 1}, self.root / 'attempt')
        stored = json.loads((self.root / 'attempt/result.json').read_text())
        self.assertEqual(result, stored)
        self.assertEqual(stored['status'], 'INVALID')
        self.assertNotIn('metrics', stored)
        self.assertIn('invalid measurement timing', stored['failures'][0])

    def test_unreproducible_media_is_not_a_zero_cpu_attempt(self):
        media = self.root / 'media.mp4'
        media.write_bytes(b'changed contents')
        result = collect({'warmup_seconds': 12, 'duration_seconds': 3, 'interval_seconds': 1,
                          'media': str(media), 'media_sha256': '0' * 64, 'binary': sys.executable}, self.root / 'attempt')
        self.assertEqual(result['status'], 'INVALID')
        self.assertIn('media hash mismatch', result['failures'][0])
        self.assertNotIn('unit', result)
        self.assertNotIn('metrics', result)

    def test_absent_service_cleanup_is_a_noop(self):
        service = MeasuredService(['/does/not/exist'], {}, self.root)
        self.assertIsNone(service.stop())
        self.assertFalse(service.started)

    def test_cpu_formula_and_empty_samples(self):
        self.assertEqual(cpu_percent(75, 100, 1.5), 50)
        self.assertEqual(cpu_percent(300, 100, 1.5), 200)
        with self.assertRaises(ValueError):
            summarize([])
        with self.assertRaises(ValueError):
            cpu_percent(-1, 100, 1)

    def test_uncertainty_does_not_turn_noise_or_zero_into_parity(self):
        self.assertEqual(comparison([0, 0, 0], .1)['status'], 'INCONCLUSIVE')
        self.assertEqual(comparison([-5, 0, 5], .1)['status'], 'INCONCLUSIVE')
        self.assertEqual(comparison([-5, -5.1, -4.9], .1)['status'], 'WIN')
        self.assertEqual(comparison([5, 5.1, 4.9], .1)['status'], 'WORSE')


if __name__ == '__main__':
    unittest.main()

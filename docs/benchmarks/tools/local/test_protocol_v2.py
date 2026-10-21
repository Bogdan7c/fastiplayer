"""Функциональные проверки протокола v2: правило остановки серии, quality-gate, GPU и VLC Qt."""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import tempfile
import unittest
from unittest import mock

import analyze
from collect import mpv_command, vlc_command
import evidence
from host import drm_clients, summarize_gpu
import run_series


def fake_attempt(scenario, player, mode, repetition, cpu, *, gfx=1.0, unique=None, frames_lost=0, fps=60.0):
    """Сохранённая попытка в формате collect.py с заданными CPU/GPU/качеством."""
    quality = ({'deltas': {'frame-drop-count': frames_lost, 'decoder-frame-drop-count': 0,
                           'vo-delayed-frame-count': 0}} if player == 'mpv'
               else {'deltas': {'frames_lost': frames_lost, 'frames_displayed': 1200}} if player == 'vlc'
               else {'unique_identities': unique if unique is not None else 1200, 'handoffs': 1200})
    return {'status': analyze.VALID, 'clk_tck': 100, 'source_fps': fps,
            'log_window_started': 100.0, 'log_window_ended': 120.0,
            'spec': {'scenario': scenario, 'player': player, 'mode': mode, 'repetition': repetition},
            'metrics': {'tree_cpu_percent': cpu, 'tree_cpu_percent_window_seconds': 20.0,
                        'rss_sum_kib': {'mean': 100_000, 'sampled_max': 110_000},
                        'pss_sum_kib': {'mean': 60_000, 'sampled_max': 65_000}},
            'gpu': {'engine_busy_percent': {'gfx': gfx}}, 'evidence': {'quality': quality}, 'failures': []}


class SeriesStoppingTests(unittest.TestCase):
    """run_series с подменённым collector-ом: проверяем, сколько попыток реально запущено."""

    def run_plan(self, behaviour, scenarios=('h264-1080p60',)):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        plan = {'protocol_version': 2, 'diagnostic_repetitions': 1, 'common': {},
                'players': {'fastiplayer': {}, 'mpv': {}},
                'scenarios': [s if isinstance(s, dict) else {'scenario': s} for s in scenarios]}
        (root / 'plan-input.json').write_text(json.dumps(plan))
        launched = []

        def fake_run_attempt(series_root, tooling, plan_value, scenario, player, mode, repetition):
            launched.append((scenario['scenario'], player, mode, repetition))
            attempt = series_root / 'attempts' / f'{scenario["scenario"]}-{mode}-{repetition:02}-{player}'
            attempt.mkdir(parents=True)
            payload = behaviour(scenario['scenario'], player, mode, repetition)
            (attempt / 'result.json').write_text(json.dumps(payload))
            return 0

        with mock.patch.object(run_series, 'run_attempt', fake_run_attempt):
            code = run_series.run_series(root / 'plan-input.json', root / 'series')
        return code, launched, json.loads((root / 'series/summary.json').read_text())

    def test_clear_win_stops_after_three_quiet_pairs_with_one_diagnostic_pair(self):
        def behaviour(scenario, player, mode, repetition):
            cpu = 3.0 + 0.01 * repetition if player == 'fastiplayer' else 5.0
            return fake_attempt(scenario, player, mode, repetition, cpu)
        code, launched, summary = self.run_plan(behaviour)
        self.assertEqual(code, 0)
        quiet = [item for item in launched if item[2] == 'quiet']
        diagnostic = [item for item in launched if item[2] == 'diagnostic']
        self.assertEqual(len(quiet), 6)
        self.assertEqual(len(diagnostic), 2)
        # Порядок плееров разворачивается на втором повторе (AB/BA).
        self.assertEqual([item[1] for item in quiet[2:4]], ['mpv', 'fastiplayer'])
        verdicts = {c['mode']: c['resource_verdict'] for c in summary['cohorts']}
        self.assertEqual(verdicts['quiet']['status'], 'WIN')
        self.assertTrue(verdicts['quiet']['within_exit_band'])
        self.assertEqual(verdicts['diagnostic']['status'], 'DESCRIPTIVE')
        self.assertEqual(summary['quality_gates']['h264-1080p60']['status'], 'PASS')

    def test_noisy_scenario_extends_to_nine_pairs_and_stops(self):
        def behaviour(scenario, player, mode, repetition):
            noise = 2.0 if repetition % 2 else -2.0
            return fake_attempt(scenario, player, mode, repetition, 5.0 + noise if player == 'fastiplayer' else 5.0)
        code, launched, summary = self.run_plan(behaviour)
        self.assertEqual(code, 0)
        self.assertEqual(len([item for item in launched if item[2] == 'quiet']), 18)
        quiet = next(c for c in summary['cohorts'] if c['mode'] == 'quiet')['resource_verdict']
        self.assertEqual((quiet['status'], quiet['n'], quiet['within_exit_band']), ('INCONCLUSIVE', 9, False))

    def test_lost_frames_stop_the_whole_series_before_cpu_cohorts(self):
        def behaviour(scenario, player, mode, repetition):
            unique = 1100 if scenario == 'hevc-4k60' else 1200
            return fake_attempt(scenario, player, mode, repetition, 3.0 if player == 'fastiplayer' else 5.0,
                                unique=unique)
        code, launched, summary = self.run_plan(behaviour, scenarios=('h264-1080p60', 'hevc-4k60'))
        self.assertEqual(code, 2)
        self.assertEqual({item[3] for item in launched}, {1})
        self.assertEqual(summary['quality_gates']['hevc-4k60']['status'], 'FAIL')
        self.assertEqual(summary['quality_gates']['h264-1080p60']['status'], 'PASS')

    def test_reference_less_scenario_is_not_tested_and_runs_only_fastiplayer(self):
        def behaviour(scenario, player, mode, repetition):
            return fake_attempt(scenario, player, mode, repetition, 4.0)
        code, launched, summary = self.run_plan(
            behaviour, scenarios=({'scenario': 'hevc-main10-4k60', 'players': ['fastiplayer']},))
        self.assertEqual(code, 0)
        self.assertEqual({item[1] for item in launched}, {'fastiplayer'})
        # Описательные 3 повтора, без продления до 6/9: сравнивать не с чем.
        self.assertEqual(len([item for item in launched if item[2] == 'quiet']), 3)
        quiet = next(c for c in summary['cohorts'] if c['mode'] == 'quiet')['resource_verdict']
        self.assertEqual(quiet['status'], 'NOT TESTED')
        self.assertIn('reference player not measured', quiet['reason'])

    def test_plan_without_protocol_version_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'plan.json').write_text(json.dumps({'common': {}, 'players': {}, 'scenarios': []}))
            with self.assertRaises(ValueError):
                run_series.run_series(root / 'plan.json', root / 'series')
            self.assertFalse((root / 'series/attempts').exists())


class QualityGateTests(unittest.TestCase):
    def test_threshold_is_ninety_nine_percent_of_fps_times_window(self):
        # 60 fps × 20 с = 1200 ожидаемых кадров: 1188 — ровно 99 %, 1187 — уже провал.
        self.assertEqual(analyze.attempt_quality(fake_attempt('s', 'fastiplayer', 'diagnostic', 1, 1, unique=1188))['status'], 'PASS')
        self.assertEqual(analyze.attempt_quality(fake_attempt('s', 'fastiplayer', 'diagnostic', 1, 1, unique=1187))['status'], 'FAIL')
        # 30 fps источник ожидает 600, а не 1200, даже если handoff-ов больше.
        thirty = analyze.attempt_quality(fake_attempt('s', 'fastiplayer', 'diagnostic', 1, 1, unique=600, fps=30.0))
        self.assertEqual((thirty['status'], thirty['expected']), ('PASS', 600.0))

    def test_reference_lost_frames_fail_and_quiet_fastiplayer_is_not_a_quality_proof(self):
        self.assertEqual(analyze.attempt_quality(fake_attempt('s', 'vlc', 'quiet', 1, 1, frames_lost=1))['status'], 'FAIL')
        self.assertEqual(analyze.attempt_quality(fake_attempt('s', 'mpv', 'quiet', 1, 1, frames_lost=2))['frames_lost'], 2)
        self.assertEqual(analyze.attempt_quality(fake_attempt('s', 'mpv', 'quiet', 1, 1))['status'], 'PASS')
        missing = fake_attempt('s', 'mpv', 'quiet', 1, 1)
        del missing['evidence']['quality']['deltas']['decoder-frame-drop-count']
        self.assertEqual(analyze.attempt_quality(missing)['status'], 'NOT TESTED')
        self.assertIsNone(analyze.attempt_quality(fake_attempt('s', 'fastiplayer', 'quiet', 1, 1)))
        gates = analyze.quality_gates([fake_attempt('s', 'fastiplayer', 'quiet', 1, 1),
                                       fake_attempt('s', 'mpv', 'quiet', 1, 1)])
        self.assertEqual(gates['s']['status'], 'NOT TESTED')

    def test_missing_counters_are_not_tested_not_pass(self):
        attempt = fake_attempt('s', 'fastiplayer', 'diagnostic', 1, 1)
        attempt['evidence']['quality'] = {}
        self.assertEqual(analyze.attempt_quality(attempt)['status'], 'NOT TESTED')

    def test_exit_band_accepts_only_narrow_inconclusive(self):
        self.assertTrue(analyze.exit_band({'status': 'INCONCLUSIVE', 'interval': [-0.3, 0.4]}))
        self.assertFalse(analyze.exit_band({'status': 'INCONCLUSIVE', 'interval': [-0.3, 0.7]}))
        self.assertFalse(analyze.exit_band({'status': 'WORSE', 'interval': [0.1, 0.2]}))
        self.assertTrue(analyze.exit_band({'status': 'WIN', 'interval': [-3, -1]}))


class GpuMetricTests(unittest.TestCase):
    @staticmethod
    def sample(time_value, clients):
        return {'drm_time': time_value, 'drm': {'1': {'engine_busy_ns': clients}}}

    def test_busy_percent_is_engine_time_over_read_interval(self):
        first = self.sample(10.0, {'226:1': {'gfx': 1_000_000_000, 'dec': 0}})
        last = self.sample(30.0, {'226:1': {'gfx': 1_400_000_000, 'dec': 2_000_000_000},
                                  '226:9': {'gfx': 5_000_000_000}})
        result = summarize_gpu(first, last)
        self.assertAlmostEqual(result['engine_busy_percent']['gfx'], 2.0)
        self.assertAlmostEqual(result['engine_busy_percent']['dec'], 10.0)
        # Client без начальной точки не выдумывает время с нуля, а записывается отдельно.
        self.assertEqual(result['clients_without_window_start'], ['226:9'])

    def test_decreasing_counter_is_an_error(self):
        with self.assertRaises(ValueError):
            summarize_gpu(self.sample(0, {'c': {'gfx': 5}}), self.sample(1, {'c': {'gfx': 4}}))

    def test_real_render_node_client_is_observed(self):
        nodes = sorted(Path('/dev/dri').glob('renderD*')) if Path('/dev/dri').exists() else []
        if not nodes:
            self.skipTest('no DRM render node on this host')
        # Наблюдаем отдельный процесс, как в замере: при чтении собственного
        # /proc/self/fdinfo исчезает дескриптор, которым читается сам каталог.
        holder = subprocess.Popen([sys.executable, '-c',
                                   f'import os, sys, time; os.open({str(nodes[0])!r}, os.O_RDWR); '
                                   'print("ready", flush=True); time.sleep(30)'],
                                  stdout=subprocess.PIPE, text=True)
        self.addCleanup(holder.stdout.close)
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        self.assertEqual(holder.stdout.readline().strip(), 'ready')
        observed = drm_clients(holder.pid)
        self.assertEqual(observed['observation_errors'], [])
        self.assertTrue(observed['clients_not_additive'])
        self.assertEqual(observed['clients_not_additive'].keys(), observed['engine_busy_ns'].keys())


class MpvEvidenceTests(unittest.TestCase):
    def test_ipc_replies_are_matched_by_request_id_across_events(self):
        directory = tempfile.TemporaryDirectory(dir=os.environ.get('XDG_RUNTIME_DIR'))
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / 'ipc'
        server = socket.socket(socket.AF_UNIX)
        server.bind(str(path))
        server.listen(1)
        self.addCleanup(server.close)

        def serve():
            connection, _ = server.accept()
            with connection:
                requests = b''
                while requests.count(b'\n') < 2:
                    requests += connection.recv(4096)
                # Ответы в обратном порядке, событие и ошибка между ними — как бывает у mpv.
                connection.sendall(b'{"event":"playback-restart"}\n'
                                   b'{"request_id":1,"error":"property unavailable"}\n'
                                   b'{"data":"vulkan","request_id":0,"error":"success"}\n')
        thread = threading.Thread(target=serve)
        thread.start()
        replies = evidence.mpv_properties(path, ('hwdec-current', 'estimated-display-fps'))
        thread.join()
        self.assertEqual(replies, {'hwdec-current': 'vulkan', 'estimated-display-fps': {'error': 'property unavailable'}})

    @staticmethod
    def boundary(hwdec, time_pos, drops, *, sink='running'):
        return {'details': {'hwdec-current': hwdec, 'time-pos': time_pos, 'pause': False, 'current-ao': 'pipewire',
                            'current-vo': 'gpu-next', 'frame-drop-count': drops, 'decoder-frame-drop-count': 0,
                            'vo-delayed-frame-count': 0},
                'audio_sinks': [{'state': sink}]}

    def test_zero_copy_hardware_playback_is_accepted_with_drop_deltas(self):
        backend, progress, audio, quality, failures = evidence.validate_mpv(
            self.boundary('vulkan', 10.0, 3), self.boundary('vulkan', 30.0, 5))
        self.assertEqual((backend, progress, audio, failures), (True, True, True, []))
        self.assertEqual(quality['deltas']['frame-drop-count'], 2)

    def test_copy_back_or_software_decode_is_not_hardware_proof(self):
        for hwdec in ('vaapi-copy', 'no', None):
            backend, *_ = evidence.validate_mpv(self.boundary(hwdec, 1.0, 0), self.boundary(hwdec, 2.0, 0))
            self.assertFalse(backend, hwdec)
        backend, *_ = evidence.validate_mpv(self.boundary('vulkan', 1.0, 0), self.boundary('no', 2.0, 0))
        self.assertFalse(backend)

    def test_missing_counter_and_silent_audio_fail(self):
        after = self.boundary('vaapi', 2.0, 0, sink='suspended')
        del after['details']['decoder-frame-drop-count']
        _, progress, audio, _, failures = evidence.validate_mpv(self.boundary('vaapi', 1.0, 0), after)
        self.assertTrue(progress)
        self.assertFalse(audio)
        self.assertIn('missing mpv counter decoder-frame-drop-count', failures)

    def test_mpv_command_enables_only_hwdec_over_defaults(self):
        command = mpv_command('/usr/bin/mpv', Path('/run/ipc'), 'quiet', Path('/media.mp4'))
        self.assertEqual(command[1:5], ['--no-config', '--hwdec=auto', '--fs', '--volume=0'])
        self.assertIn('--input-ipc-server=/run/ipc', command)
        self.assertNotIn('-v', command)
        self.assertIn('-v', mpv_command('/usr/bin/mpv', Path('/run/ipc'), 'diagnostic', Path('/media.mp4')))


class VlcCommandTests(unittest.TestCase):
    def test_version_one_reference_command_is_preserved_byte_for_byte(self):
        command = vlc_command('/usr/bin/vlc', Path('/run/rc'), 'quiet', Path('/media.mp4'))
        self.assertEqual(command, [
            '/usr/bin/vlc', '--ignore-config', '--no-one-instance', '--intf', 'dummy',
            '--extraintf', 'oldrc', '--rc-fake-tty', '--rc-unix=/run/rc', '--avcodec-hw=vaapi',
            '--vout=gl', '--fullscreen', '--no-video-title-show', '--gain=0', '/media.mp4'])
        self.assertIn('-vv', vlc_command('/usr/bin/vlc', Path('/run/rc'), 'diagnostic', Path('/media.mp4')))


if __name__ == '__main__':
    unittest.main()

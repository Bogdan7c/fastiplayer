"""Расширение протокола v3 для сессии 10 gles2-renderer: игроки по типу, субъект и три
плеера, проверка рендера, mpv args/expect, режим окна, CPU композитора, A/V-синхрон."""

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

import analyze
import collect
import compositor
import evidence
import kwin
import players
import run_series
import service
from test_protocol_v2 import fake_attempt


def three_player_attempt(scenario, player, mode, repetition, cpu, *, compositor_cpu=None, unique=None,
                         avsync=(0.001, -0.002)):
    """Попытка серии «GLES2 против wgpu и mpv»: имя игрока ≠ его тип."""
    kind = {'gles2': 'fastiplayer', 'wgpu': 'fastiplayer', 'mpv-gl': 'mpv'}[player]
    attempt = fake_attempt(scenario, kind, mode, repetition, cpu, unique=unique)
    attempt['spec'].update({'player': player, 'kind': kind, 'subject': 'gles2', 'window_mode': 'wayland'})
    if kind == 'mpv':
        attempt['evidence']['quality']['avsync_seconds'] = list(avsync)
    if compositor_cpu is not None:
        attempt['metrics'].update({'compositor_cpu_percent': compositor_cpu,
                                   'compositor_cpu_percent_by_process': {'kwin_wayland': compositor_cpu,
                                                                          'Xwayland': 0.0},
                                   'compositor_cpu_percent_window_seconds': 20.0,
                                   'tree_plus_compositor_cpu_percent': cpu + compositor_cpu})
    return attempt


THREE_PLAYERS = {'gles2': {'kind': 'fastiplayer', 'renderer': 'gles2'},
                 'wgpu': {'kind': 'fastiplayer', 'renderer': 'wgpu'},
                 'mpv-gl': {'kind': 'mpv', 'args': ['--vo=gpu', '--gpu-api=opengl']}}


class PlayerModelTests(unittest.TestCase):
    def test_kind_defaults_to_name_for_old_plans(self):
        self.assertEqual(players.player_kind({'player': 'mpv'}), 'mpv')
        self.assertEqual(players.player_kind({'player': 'wgpu', 'kind': 'fastiplayer'}), 'fastiplayer')
        with self.assertRaises(ValueError):
            players.player_kind({'player': 'wgpu'})

    def test_plan_errors_surface_before_the_first_attempt(self):
        good = {'subject': 'gles2', 'players': THREE_PLAYERS}
        self.assertEqual(players.validate_players(good), ('gles2', 'wgpu', 'mpv-gl'))
        broken = [
            {'players': {'fastiplayer': {}}},  # один игрок
            {'players': {**THREE_PLAYERS, 'vlc': {}}, 'subject': 'gles2'},  # четыре
            {'players': {'Fast': {}, 'mpv': {}}, 'subject': 'Fast'},  # имя не [a-z0-9-]+
            {'players': THREE_PLAYERS, 'subject': 'mpv-gl'},  # субъект не Fastiplayer
            {'players': THREE_PLAYERS},  # субъект по умолчанию «fastiplayer» отсутствует
            {'players': {'fastiplayer': {}, 'mpv': {'renderer': 'gles2'}}},  # renderer у mpv
            {'players': {'fastiplayer': {'renderer': 'vulkan'}, 'mpv': {}}},  # неизвестный рендер
        ]
        for plan in broken:
            with self.assertRaises(ValueError, msg=plan):
                players.validate_players(plan)

    def test_two_players_keep_the_old_ab_ba_order(self):
        for repetition in range(1, 10):
            self.assertEqual(players.repetition_order(('fastiplayer', 'mpv'), repetition),
                             run_series.ordered(('fastiplayer', 'mpv'), repetition))

    def test_three_players_form_a_latin_square(self):
        orders = [players.repetition_order(('a', 'b', 'c'), repetition) for repetition in (1, 2, 3)]
        self.assertEqual(orders[0], ('a', 'b', 'c'))
        # За три повтора каждый игрок побывал на каждой позиции ровно один раз.
        for position in range(3):
            self.assertEqual(sorted(order[position] for order in orders), ['a', 'b', 'c'])
        self.assertEqual(players.repetition_order(('a', 'b', 'c'), 4), orders[0])

    def test_window_mode_rejects_unknown_values(self):
        self.assertEqual(players.window_mode({}), 'xwayland')
        self.assertEqual(players.window_mode({'window_mode': 'wayland'}), 'wayland')
        with self.assertRaises(ValueError):
            players.window_mode({'window_mode': 'x11'})


class ThreePlayerSeriesTests(unittest.TestCase):
    def run_plan(self, behaviour):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        plan = {'protocol_version': 3, 'diagnostic_repetitions': 1, 'subject': 'gles2',
                'common': {'window_mode': 'wayland'}, 'players': THREE_PLAYERS,
                'scenarios': [{'scenario': 'h264-1080p60-wayland'}]}
        (root / 'plan-input.json').write_text(json.dumps(plan))
        launched, specs = [], []

        def fake_run_attempt(series_root, tooling, plan_value, scenario, player, mode, repetition):
            launched.append((player, mode, repetition))
            attempt = series_root / 'attempts' / f'{scenario["scenario"]}-{mode}-{repetition:02}-{player}'
            attempt.mkdir(parents=True)
            (attempt / 'result.json').write_text(json.dumps(behaviour(scenario['scenario'], player, mode, repetition)))
            return 0

        with mock.patch.object(run_series, 'run_attempt', fake_run_attempt):
            code = run_series.run_series(root / 'plan-input.json', root / 'series')
        return code, launched, json.loads((root / 'series/summary.json').read_text())

    def test_stop_rule_waits_for_every_pair(self):
        # GLES2 явно дешевле mpv (пара решена на 3), а против wgpu разница тонет в шуме.
        def behaviour(scenario, player, mode, repetition):
            noise = 2.0 if repetition % 2 else -2.0
            cpu = {'gles2': 5.0 + 0.01 * repetition, 'wgpu': 5.0 + noise, 'mpv-gl': 9.0}[player]
            return three_player_attempt(scenario, player, mode, repetition, cpu, compositor_cpu=3.0)
        code, launched, summary = self.run_plan(behaviour)
        self.assertEqual(code, 0)
        quiet = [item for item in launched if item[1] == 'quiet']
        self.assertEqual(len(quiet), 27)
        # Порядок повтора 2 — циклический сдвиг плана.
        self.assertEqual([item[0] for item in quiet[3:6]], ['wgpu', 'mpv-gl', 'gles2'])
        cohort = next(c for c in summary['cohorts'] if c['mode'] == 'quiet')
        self.assertEqual(cohort['subject'], 'gles2')
        self.assertEqual(cohort['window_mode'], 'wayland')
        self.assertEqual(cohort['pairs']['mpv-gl']['resource_verdict']['status'], 'WIN')
        self.assertEqual(cohort['pairs']['wgpu']['resource_verdict']['status'], 'INCONCLUSIVE')
        self.assertEqual(cohort['pairs']['wgpu']['resource_verdict']['n'], 9)
        # Верхнеуровневый вердикт — первый эталон по имени, как в старом формате.
        self.assertEqual(cohort['reference_player'], 'mpv-gl')
        self.assertEqual(cohort['resource_verdict'], cohort['pairs']['mpv-gl']['resource_verdict'])
        self.assertEqual(set(cohort['players']), {'gles2', 'wgpu', 'mpv-gl'})
        self.assertEqual(cohort['players']['gles2']['compositor_cpu_mean'], 3.0)
        self.assertEqual(summary['quality_gates']['h264-1080p60-wayland']['status'], 'PASS')

    def test_fastiplayer_reference_is_gated_on_unique_frames(self):
        # wgpu-эталон потерял кадры: при политике stop это FAIL сцены, как потери mpv.
        def behaviour(scenario, player, mode, repetition):
            return three_player_attempt(scenario, player, mode, repetition, 5.0,
                                        unique=1100 if player == 'wgpu' else None)
        code, launched, summary = self.run_plan(behaviour)
        self.assertEqual(code, 2)
        gate = summary['quality_gates']['h264-1080p60-wayland']
        self.assertEqual(gate['status'], 'FAIL')
        wgpu = [r for r in gate['attempts'] if r['player'] == 'wgpu']
        self.assertEqual((wgpu[0]['status'], wgpu[0]['metric']), ('FAIL', 'unique_frame_identities'))


class CompositorVerdictTests(unittest.TestCase):
    def cohort(self, gles2_compositor, wgpu_compositor, *, tree=(5.0, 5.2)):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        for repetition in (1, 2, 3):
            for player, cpu, compositor_cpu in (('gles2', tree[0] + 0.01 * repetition, gles2_compositor),
                                                ('wgpu', tree[1], wgpu_compositor)):
                attempt = three_player_attempt('scene', player, 'quiet', repetition, cpu,
                                               compositor_cpu=compositor_cpu)
                path = root / 'attempts' / f'{player}-{repetition}' / 'result.json'
                path.parent.mkdir(parents=True)
                path.write_text(json.dumps(attempt))
        return analyze.analyze(root)['cohorts'][0]['pairs']['wgpu']

    def test_second_verdict_uses_tree_plus_compositor(self):
        # Дерево GLES2 чуть дешевле (WIN), но композитор с ним работает на 2 п.п. больше:
        # второе число решения 12 обязано это показать.
        pair = self.cohort(gles2_compositor=4.0, wgpu_compositor=2.0)
        self.assertEqual(pair['resource_verdict']['status'], 'WIN')
        second = pair['compositor_resource_verdict']
        self.assertEqual(second['status'], 'WORSE')
        self.assertAlmostEqual(second['mean_difference'], 1.82, places=6)
        # Разрешение: floor основного счётчика + тик на каждый из двух процессов обеих попыток.
        self.assertAlmostEqual(second['quantization_bound'], 0.1 + 2 * (2 * 100 / 100 / 20.0))

    def test_series_without_compositor_is_not_tested(self):
        pair = self.cohort(gles2_compositor=None, wgpu_compositor=None)
        self.assertEqual(pair['resource_verdict']['status'], 'WIN')
        self.assertEqual(pair['compositor_resource_verdict'],
                         {'status': 'NOT TESTED', 'reason': 'compositor CPU not recorded'})

    def test_collect_summary_adds_compositor_to_tree(self):
        identities = [{'name': 'kwin_wayland', 'pid': 10, 'start_ticks': 5}]
        clock = os.sysconf('SC_CLK_TCK')

        def snapshot(time_value, tree_usec, kwin_ticks):
            return {'process': {'start_ticks': 1, 'cpu_ticks': 0, 'cpu_time': time_value},
                    'cgroup_cpu': {'usage_usec': tree_usec, 'time': time_value},
                    'rss_sum_kib': 1, 'pss_sum_kib': 1, 'drm': {},
                    'compositor': {'time': time_value, 'cpu_ticks': {'10': kwin_ticks}}}
        # За 10 с дерево потратило 1 с CPU (10 %), kwin — clock тиков = 1 с (10 %).
        metrics = collect.summarize([snapshot(0.0, 0, 0), snapshot(10.0, 1_000_000, clock)], identities)
        self.assertAlmostEqual(metrics['tree_cpu_percent'], 10.0)
        self.assertAlmostEqual(metrics['compositor_cpu_percent'], 10.0)
        self.assertAlmostEqual(metrics['tree_plus_compositor_cpu_percent'], 20.0)
        self.assertNotIn('compositor_cpu_percent', collect.summarize([snapshot(0.0, 0, 0), snapshot(10.0, 1, 0)]))


class CompositorProcessTests(unittest.TestCase):
    def fake_proc(self, processes):
        """Поддельное /proc: {pid: (comm, utime, stime, starttime)}."""
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        self.write_proc(root, processes)
        return root

    @staticmethod
    def write_proc(root, processes):
        for pid, (comm, utime, stime, start) in processes.items():
            fields = ['S'] + ['0'] * 10 + [str(utime), str(stime)] + ['0'] * 6 + [str(start)]
            (root / str(pid)).mkdir(exist_ok=True)
            (root / str(pid) / 'stat').write_text(f'{pid} ({comm}) ' + ' '.join(fields) + '\n')

    def test_discovers_only_compositor_processes_with_identity(self):
        root = self.fake_proc({1667: ('kwin_wayland', 5, 1, 900), 1795: ('Xwayland', 2, 2, 950),
                               2000: ('fish', 1, 1, 1000)})
        found = compositor.discover(root)
        self.assertEqual(found, [{'name': 'kwin_wayland', 'pid': 1667, 'start_ticks': 900},
                                 {'name': 'Xwayland', 'pid': 1795, 'start_ticks': 950}])
        sample = compositor.read_sample(found, root)
        self.assertEqual(sample['cpu_ticks'], {'1667': 6, '1795': 4})

    def test_restarted_or_missing_compositor_invalidates_the_window(self):
        root = self.fake_proc({1667: ('kwin_wayland', 5, 1, 900)})
        found = compositor.discover(root)
        self.write_proc(root, {1667: ('kwin_wayland', 7, 1, 901)})  # тот же PID, другой процесс
        with self.assertRaises(RuntimeError):
            compositor.read_sample(found, root)
        with self.assertRaises(RuntimeError):
            compositor.verify_unchanged(found, found + [{'name': 'Xwayland', 'pid': 3000, 'start_ticks': 1}])
        with self.assertRaises(RuntimeError):
            compositor.require_processes(found, 'xwayland')  # XWayland-режим без Xwayland
        compositor.require_processes(found, 'wayland')


class FastiplayerRendererTests(unittest.TestCase):
    def validate(self, runtime, renderer):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        log = Path(directory.name) / 'runtime.log'
        log.write_text(runtime)
        boundary = {'details': {'media_matches': {'inode': 1}, 'position_us': 1, 'playback_status': 'Playing'},
                    'audio_sinks': [{'state': 'running'}]}
        after = {**boundary, 'details': {**boundary['details'], 'position_us': 20_000_000}}
        return evidence.validate('fastiplayer', 'h264', 'quiet', 'software', log, boundary, after, 0,
                                 log.stat().st_size, renderer=renderer)

    def test_plan_renderer_must_match_the_logged_family(self):
        gles2_log = 'Selected video pipeline plan="ffmpeg-host-upload-gles2"\nStartup audio playback resumed\n'
        matched = self.validate(gles2_log, 'gles2')
        self.assertEqual(matched['failures'], [])
        self.assertEqual(matched['renderer_family'], 'gles2')
        mismatched = self.validate(gles2_log, 'wgpu')
        self.assertIn("fastiplayer renderer 'gles2' does not match plan renderer 'wgpu'", mismatched['failures'])
        # Без поля renderer — прежнее поведение: семейство только записывается.
        self.assertEqual(self.validate(gles2_log, None)['failures'], [])


class MpvSession10Tests(unittest.TestCase):
    @staticmethod
    def boundary(context, avsync=0.001):
        return {'details': {'hwdec-current': 'vaapi', 'time-pos': 1.0, 'pause': False, 'current-ao': 'pipewire',
                            'current-vo': 'gpu', 'current-gpu-context': context, 'avsync': avsync,
                            'frame-drop-count': 0, 'decoder-frame-drop-count': 0, 'vo-delayed-frame-count': 0},
                'audio_sinks': [{'state': 'running'}]}

    def test_expect_must_hold_on_both_boundaries(self):
        expect = {'current-vo': 'gpu', 'current-gpu-context': 'wayland'}
        after = self.boundary('wayland')
        after['details']['time-pos'] = 2.0
        *_, quality, failures = evidence.validate_mpv(self.boundary('wayland'), after, 'hardware', expect)
        self.assertEqual(failures, [])
        self.assertEqual(quality['gpu_context'], 'wayland')
        *_, failures = evidence.validate_mpv(self.boundary('wayland'), self.boundary('x11egl'), 'hardware', expect)
        self.assertEqual(failures, ["mpv current-gpu-context is 'wayland'/'x11egl', expected 'wayland'"])

    def test_args_go_after_base_options_and_before_ipc(self):
        command = collect.player_command({'player': 'mpv-gl', 'kind': 'mpv', 'args': ['--vo=gpu', '--gpu-api=opengl']},
                                         '/usr/bin/mpv', Path('/run/ipc'), 'quiet', Path('/m.mp4'), 'software')
        self.assertEqual(command[:7], ['/usr/bin/mpv', '--no-config', '--hwdec=no', '--fs', '--volume=0',
                                       '--vo=gpu', '--gpu-api=opengl'])
        self.assertEqual(command[7:], ['--input-ipc-server=/run/ipc', '/m.mp4'])
        with self.assertRaises(ValueError):
            collect.mpv_extra_args({'args': ['--input-ipc-server=/tmp/other']})
        with self.assertRaises(ValueError):
            collect.mpv_extra_args({'args': 'opengl'})

    def test_expect_is_checked_before_launch(self):
        self.assertIsNone(collect.mpv_expectations({'player': 'mpv'}))
        self.assertEqual(collect.mpv_expectations({'player': 'mpv', 'expect': {'current-vo': 'gpu'}}),
                         {'current-vo': 'gpu'})
        for spec in ({'player': 'mpv', 'expect': {'current-vo-typo': 'gpu'}},
                     {'player': 'fastiplayer', 'expect': {'current-vo': 'gpu'}},
                     {'player': 'mpv', 'expect': {}}):
            with self.assertRaises(ValueError, msg=spec):
                collect.mpv_expectations(spec)

    def test_av_sync_gate_reports_mpv_and_admits_fastiplayer_gap(self):
        attempts = [three_player_attempt('scene', 'gles2', 'diagnostic', 1, 5.0),
                    three_player_attempt('scene', 'mpv-gl', 'diagnostic', 1, 5.0, avsync=(0.002, -0.08)),
                    three_player_attempt('scene', 'mpv-gl', 'quiet', 1, 5.0)]
        gate = analyze.quality_gates(attempts)['scene']['av_sync']
        self.assertEqual(gate['status'], 'NOT TESTED')
        self.assertIn('Fastiplayer exposes no A/V offset metric', gate['reason'])
        mpv = gate['references']['mpv-gl']
        self.assertEqual([(r['mode'], r['status']) for r in mpv], [('diagnostic', 'FAIL'), ('quiet', 'PASS')])
        self.assertAlmostEqual(mpv[0]['max_abs_seconds'], 0.08)


class WindowModeTests(unittest.TestCase):
    def test_environment_passes_wayland_display_only_in_wayland_mode(self):
        with tempfile.TemporaryDirectory() as directory, \
                mock.patch.dict(os.environ, {'WAYLAND_DISPLAY': 'wayland-1', 'DISPLAY': ':0'}):
            native = service.isolated_environment(Path(directory) / 'a', 'info', 'wayland')
            legacy = service.isolated_environment(Path(directory) / 'b', 'info')
        self.assertEqual(native['WAYLAND_DISPLAY'], 'wayland-1')
        self.assertNotIn('WAYLAND_DISPLAY', legacy)
        self.assertEqual(legacy['DISPLAY'], ':0')

    def test_wayland_mode_without_wayland_display_is_an_error(self):
        environment = {key: value for key, value in os.environ.items() if key != 'WAYLAND_DISPLAY'}
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(os.environ, environment, clear=True):
            with self.assertRaises(ValueError):
                service.isolated_environment(Path(directory), 'info', 'wayland')

    def test_kwin_marker_line_is_found_and_foreign_markers_are_ignored(self):
        journal = ('js: fastiplayer-window-proof-old [{"fullscreen": false}]\n'
                   'some kwin noise\n'
                   'js: fastiplayer-window-proof-new [{"fullscreen": true, "width": 1920}]\n')
        self.assertEqual(kwin.parse_marker_line(journal, 'fastiplayer-window-proof-new'),
                         [{'fullscreen': True, 'width': 1920}])
        self.assertIsNone(kwin.parse_marker_line(journal, 'fastiplayer-window-proof-missing'))
        self.assertIn('window.pid !== 42', kwin.window_list_script(42, 'marker'))

    def test_wayland_window_proof_requires_one_fullscreen_native_window(self):
        window = {'fullscreen': True, 'width': 1920, 'height': 1080, 'output': 'DP-1', 'active': True}
        cases = [([window], [], None),
                 ([], [], 'expected one KWin window'),
                 ([{**window, 'fullscreen': False}], [], 'not fullscreen'),
                 ([{**window, 'width': 1280}], [], 'expected 1920x1080'),
                 ([window], ['0x400001'], 'owns X11 windows')]
        for windows, x11, error in cases:
            with mock.patch.object(kwin, 'windows_of', return_value=windows), \
                    mock.patch.object(evidence, 'x11_windows_of', return_value=x11):
                if error is None:
                    proof = evidence.window_evidence(42, Path('/helper'), 'wayland', [1920, 1080])
                    self.assertEqual((proof['source'], proof['output']), ('kwin', 'DP-1'))
                else:
                    with self.assertRaisesRegex(ValueError, error):
                        evidence.window_evidence(42, Path('/helper'), 'wayland', [1920, 1080])

    def test_capture_is_refused_for_native_wayland(self):
        with self.assertRaisesRegex(ValueError, 'capture unsupported'):
            evidence.capture(Path('/tmp'), 'before', 42, Path('/helper'), [1920, 1080], 'wayland')


if __name__ == '__main__':
    unittest.main()

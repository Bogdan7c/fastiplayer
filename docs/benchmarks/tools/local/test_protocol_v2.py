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
from collect import decode_path, fastiplayer_command, mpv_command, vlc_command
import evidence
from host import drm_clients, gpu_memory_kib, parse_drm_memory_kib, render_engine_busy_percent, summarize_gpu
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
        plan = {'protocol_version': 3, 'diagnostic_repetitions': 1, 'common': {},
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

    def test_annotated_reference_losses_do_not_stop_the_series(self):
        def behaviour(scenario, player, mode, repetition):
            attempt = fake_attempt(scenario, player, mode, repetition, 3.0 if player == 'fastiplayer' else 5.0,
                                   frames_lost=3 if player == 'mpv' else 0)
            attempt['spec']['reference_losses'] = 'annotate'
            return attempt
        code, launched, summary = self.run_plan(behaviour)
        self.assertEqual(code, 0)
        self.assertGreater(max(item[3] for item in launched), 1)
        self.assertEqual(summary['quality_gates']['h264-1080p60']['status'], 'REFERENCE_LOSSES')

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

    def test_reference_losses_are_annotated_only_when_the_plan_says_so(self):
        def scene(policy=None, reference_lost=2, our_unique=1200):
            attempts = [fake_attempt('s', 'fastiplayer', 'diagnostic', 1, 1, unique=our_unique),
                        fake_attempt('s', 'mpv', 'diagnostic', 1, 1, frames_lost=reference_lost)]
            if policy:
                for attempt in attempts:
                    attempt['spec']['reference_losses'] = policy
            return analyze.quality_gates(attempts)['s']['status']
        self.assertEqual(scene(), 'FAIL')
        self.assertEqual(scene('stop'), 'FAIL')
        self.assertEqual(scene('annotate'), 'REFERENCE_LOSSES')
        self.assertEqual(scene('annotate', reference_lost=0), 'PASS')
        # Свои потери не прощаются никакой политикой.
        self.assertEqual(scene('annotate', our_unique=1000), 'FAIL')
        with self.assertRaisesRegex(ValueError, 'reference_losses'):
            scene('ignore')

    def test_missing_counters_are_not_tested_not_pass(self):
        attempt = fake_attempt('s', 'fastiplayer', 'diagnostic', 1, 1)
        attempt['evidence']['quality'] = {}
        self.assertEqual(analyze.attempt_quality(attempt)['status'], 'NOT TESTED')

    def test_exit_band_accepts_only_narrow_inconclusive(self):
        self.assertTrue(analyze.exit_band({'status': 'INCONCLUSIVE', 'interval': [-0.3, 0.4]}))
        self.assertFalse(analyze.exit_band({'status': 'INCONCLUSIVE', 'interval': [-0.3, 0.7]}))
        self.assertFalse(analyze.exit_band({'status': 'WORSE', 'interval': [0.1, 0.2]}))
        self.assertTrue(analyze.exit_band({'status': 'WIN', 'interval': [-3, -1]}))

    @staticmethod
    def quiet_cohort(cpu_pairs, **spec_fields):
        attempts = []
        for repetition, (ours, reference) in enumerate(cpu_pairs, start=1):
            for player, cpu in (('fastiplayer', ours), ('mpv', reference)):
                attempt = fake_attempt('s', player, 'quiet', repetition, cpu)
                attempt['spec'].update(spec_fields)
                attempts.append(attempt)
        return attempts

    def verdict(self, attempts):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        for attempt in attempts:
            spec = attempt['spec']
            path = root / 'attempts' / f"{spec['scenario']}-{spec['mode']}-{spec['repetition']:02}-{spec['player']}"
            path.mkdir(parents=True)
            (path / 'result.json').write_text(json.dumps(attempt))
        return analyze.analyze(root)['cohorts'][0]['resource_verdict']

    def test_plan_exit_band_widens_the_corridor_only_when_declared(self):
        # Разности +0,1/−0,1/0: sd 0,1, радиус 10·0,1/√3 + 0,1 (разрешение) ≈ 0,68 —
        # INCONCLUSIVE шире ±0,5, но внутри ±1.
        middle = [(10.1, 10.0), (9.9, 10.0), (10.0, 10.0)]
        default_band = self.verdict(self.quiet_cohort(middle))
        self.assertEqual(default_band['status'], 'INCONCLUSIVE')
        self.assertEqual(default_band['exit_band_percentage_points'], 0.5)
        self.assertFalse(default_band['within_exit_band'])
        software_band = self.verdict(self.quiet_cohort(middle, exit_band_percentage_points=1.0))
        self.assertEqual(software_band['exit_band_percentage_points'], 1.0)
        self.assertTrue(software_band['within_exit_band'])
        # Разброс ±0,3: радиус ≈ 1,83 — не проходит и ±1.
        wide = self.verdict(self.quiet_cohort([(10.3, 10.0), (9.7, 10.0), (10.0, 10.0)],
                                              exit_band_percentage_points=1.0))
        self.assertEqual(wide['status'], 'INCONCLUSIVE')
        self.assertFalse(wide['within_exit_band'])

    def test_band_of_one_point_accepts_what_half_point_rejects(self):
        verdict = {'status': 'INCONCLUSIVE', 'interval': [-0.8, 0.9]}
        self.assertFalse(analyze.exit_band(verdict))
        self.assertTrue(analyze.exit_band(verdict, 1.0))

    def test_mixed_or_invalid_plan_exit_band_is_rejected(self):
        mixed = self.quiet_cohort([(10.2, 10.0)] * 3, exit_band_percentage_points=1.0)
        mixed[0]['spec']['exit_band_percentage_points'] = 0.5
        with self.assertRaisesRegex(ValueError, 'mixes exit bands'):
            self.verdict(mixed)
        with self.assertRaisesRegex(ValueError, 'invalid exit band'):
            self.verdict(self.quiet_cohort([(10.2, 10.0)] * 3, exit_band_percentage_points=0))


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
        # Реальный fdinfo парсится в неотрицательные GTT/VRAM, а не в None.
        memory = gpu_memory_kib({'drm': {str(holder.pid): observed}})
        self.assertGreaterEqual(memory['gtt'], 0)
        self.assertGreaterEqual(memory['vram'], 0)


class GpuMemoryTests(unittest.TestCase):
    """Память GPU (GTT/VRAM) дерева процессов: решение владельца 08b."""

    @staticmethod
    def drm_sample(**processes):
        return {'drm': {pid: {'clients_not_additive': clients} for pid, clients in processes.items()}}

    def test_units_are_normalised_to_kib(self):
        self.assertEqual(parse_drm_memory_kib('171 KiB'), 171)
        self.assertEqual(parse_drm_memory_kib('2 MiB'), 2048)
        self.assertIsNone(parse_drm_memory_kib('garbage'))
        self.assertIsNone(parse_drm_memory_kib(''))

    def test_shared_client_is_counted_once_across_processes(self):
        shared = {'drm-memory-gtt': '1000 KiB', 'drm-memory-vram': '50 KiB'}
        sample_value = self.drm_sample(**{'1': {'226:7': shared},
                                          '2': {'226:7': shared, '226:9': {'drm-memory-gtt': '24 KiB'}}})
        self.assertEqual(gpu_memory_kib(sample_value), {'gtt': 1024, 'vram': 50})

    def test_no_drm_clients_gives_none_not_zero(self):
        self.assertIsNone(gpu_memory_kib(self.drm_sample(**{'1': {}})))

    def test_summarize_exposes_gtt_and_vram_with_mean_and_max(self):
        from collect import summarize
        samples = [self.fake_sample(t, gtt) for t, gtt in enumerate((100, 300))]
        result = summarize(samples)
        self.assertEqual(result['gpu_gtt_kib'], {'mean': 200, 'sampled_max': 300, 'minimum': 100})
        self.assertEqual(result['gpu_vram_kib']['mean'], 7)

    @staticmethod
    def fake_sample(second, gtt):
        return {'process': {'start_ticks': 1, 'cpu_ticks': 100 * second, 'cpu_time': float(second)},
                'cgroup_cpu': {'usage_usec': 1_000_000 * second, 'time': float(second)},
                'rss_sum_kib': 10, 'pss_sum_kib': 5,
                'drm': {'1': {'clients_not_additive': {'226:1': {'drm-memory-gtt': f'{gtt} KiB',
                                                                  'drm-memory-vram': '7 KiB'}}}}}

    def test_aggregation_adds_rss_plus_gtt_and_old_attempts_stay_readable(self):
        def with_memory(attempt, gtt, vram):
            attempt['metrics']['gpu_gtt_kib'] = {'mean': gtt, 'sampled_max': gtt + 1024}
            attempt['metrics']['gpu_vram_kib'] = {'mean': vram, 'sampled_max': vram}
            return attempt

        new = [with_memory(fake_attempt('s', 'fastiplayer', 'quiet', 1, 10.0), 51_200, 2048),
               with_memory(fake_attempt('s', 'mpv', 'quiet', 1, 12.0), 102_400, 0)]
        old = [fake_attempt('s', 'fastiplayer', 'quiet', 1, 10.0), fake_attempt('s', 'mpv', 'quiet', 1, 12.0)]
        summaries = {}
        for name, attempts in (('new', new), ('old', old)):
            directory = tempfile.TemporaryDirectory()
            self.addCleanup(directory.cleanup)
            for attempt in attempts:
                path = Path(directory.name) / 'attempts' / attempt['spec']['player']
                path.mkdir(parents=True)
                (path / 'result.json').write_text(json.dumps(attempt))
            summaries[name] = analyze.analyze(Path(directory.name))['cohorts'][0]['players']
        mpv_new = summaries['new']['mpv']
        self.assertAlmostEqual(mpv_new['rss_mean_mib'], 100_000 / 1024)
        self.assertAlmostEqual(mpv_new['gtt_mean_mib'], 100.0)
        self.assertAlmostEqual(mpv_new['gtt_sampled_max_mib'], 101.0)
        self.assertAlmostEqual(mpv_new['rss_plus_gtt_mean_mib'], 100_000 / 1024 + 100.0)
        self.assertEqual(summaries['new']['fastiplayer']['vram_mean_mib'], 2.0)
        old_mpv = summaries['old']['mpv']
        self.assertAlmostEqual(old_mpv['rss_mean_mib'], 100_000 / 1024)
        for key in ('gtt_mean_mib', 'vram_mean_mib', 'rss_plus_gtt_mean_mib', 'gtt_sampled_max_mib'):
            self.assertIsNone(old_mpv[key])



# Реальная форма fdinfo i915 (ThinkPad T480s, сессия 10): память — регионы system0/local0,
# движок рендера — ``render``. Значения из настоящей попытки Fastiplayer.
I915_VULKAN_CLIENT = {'drm-total-system0': '126308 KiB', 'drm-shared-system0': '98292 KiB',
                      'drm-resident-system0': '124260 KiB', 'drm-total-stolen-system0': '0',
                      'drm-resident-stolen-system0': '0'}
I915_VAAPI_CLIENT = {'drm-total-system0': '80308 KiB', 'drm-shared-system0': '72 MiB',
                     'drm-resident-system0': '79756 KiB'}


class DriverNamingTests(unittest.TestCase):
    """GPU-метрики не должны зависеть от того, как драйвер называет движок и регион памяти."""

    @staticmethod
    def sample(clients, second=0):
        return {'process': {'start_ticks': 1, 'cpu_ticks': 100 * second, 'cpu_time': float(second)},
                'cgroup_cpu': {'usage_usec': 1_000_000 * second, 'time': float(second)},
                'rss_sum_kib': 10, 'pss_sum_kib': 5,
                'drm': {'1': {'clients_not_additive': clients}}}

    def test_i915_resident_system_memory_counts_as_gtt(self):
        memory = gpu_memory_kib(self.sample({'226:416': I915_VULKAN_CLIENT, '226:421': I915_VAAPI_CLIENT}))
        self.assertEqual(memory, {'gtt': 124260 + 79756, 'vram': 0})

    def test_i915_discrete_local_memory_counts_as_vram(self):
        memory = gpu_memory_kib(self.sample({'226:1': {'drm-resident-local0': '2 MiB',
                                                       'drm-resident-system0': '3 KiB'}}))
        self.assertEqual(memory, {'gtt': 3, 'vram': 2048})

    def test_amdgpu_legacy_field_wins_over_newer_duplicate(self):
        # Новые amdgpu пишут и drm-memory-gtt, и drm-resident-gtt — одну и ту же память
        # нельзя посчитать дважды, а серии 780M должны считаться как раньше.
        memory = gpu_memory_kib(self.sample({'226:1': {'drm-memory-gtt': '100 KiB',
                                                       'drm-resident-gtt': '100 KiB',
                                                       'drm-memory-vram': '5 KiB'}}))
        self.assertEqual(memory, {'gtt': 100, 'vram': 5})

    def test_render_engine_is_found_under_the_driver_name(self):
        self.assertEqual(render_engine_busy_percent({'gfx': 1.5, 'dec': 9.0}), 1.5)
        self.assertEqual(render_engine_busy_percent({'render': 6.2, 'video': 7.4}), 6.2)
        self.assertIsNone(render_engine_busy_percent({'video': 7.4}))

    def test_i915_series_gets_gpu_verdict_and_memory_from_raw_samples(self):
        # Попытки, снятые до исправления: в metrics нули GTT, в gpu — движок ``render``.
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        for repetition in (1, 2, 3):
            for player, render, clients in (('fastiplayer', 6.0 + repetition / 10, {'226:1': I915_VULKAN_CLIENT}),
                                            ('mpv', 19.0, {'226:2': I915_VAAPI_CLIENT})):
                attempt = fake_attempt('h264-1080p60', player, 'quiet', repetition, 9.0)
                attempt['gpu'] = {'engine_busy_percent': {'render': render, 'video': 7.0}}
                attempt['metrics']['gpu_gtt_kib'] = {'mean': 0, 'sampled_max': 0, 'minimum': 0}
                attempt['metrics']['gpu_vram_kib'] = {'mean': 0, 'sampled_max': 0, 'minimum': 0}
                attempt['samples'] = [self.sample(clients, second) for second in (0, 1)]
                path = Path(directory.name) / 'attempts' / f'{player}-{repetition}'
                path.mkdir(parents=True)
                (path / 'result.json').write_text(json.dumps(attempt))
        cohort = analyze.analyze(Path(directory.name))['cohorts'][0]
        self.assertEqual(cohort['gpu_gfx_verdict']['status'], 'WIN')
        players = cohort['players']
        self.assertAlmostEqual(players['fastiplayer']['gfx_busy_percent_mean'], 6.2)
        self.assertAlmostEqual(players['fastiplayer']['gtt_mean_mib'], 124260 / 1024)
        self.assertAlmostEqual(players['mpv']['gtt_mean_mib'], 79756 / 1024)
        self.assertAlmostEqual(players['mpv']['rss_plus_gtt_mean_mib'], 100_000 / 1024 + 79756 / 1024)

    def test_amdgpu_recalculation_equals_stored_summary(self):
        from collect import summarize
        samples = [self.sample({'226:1': {'drm-memory-gtt': f'{gtt} KiB', 'drm-memory-vram': '7 KiB'}}, second)
                   for second, gtt in enumerate((100, 300, 200))]
        attempt = fake_attempt('s', 'mpv', 'quiet', 1, 10.0)
        attempt['metrics'].update({key: value for key, value in summarize(samples).items()
                                   if key.startswith('gpu_')})
        attempt['samples'] = samples
        self.assertEqual(analyze.gpu_memory_metrics(attempt),
                         {key: value for key, value in attempt['metrics'].items() if key.startswith('gpu_')})

    def test_attempt_from_before_memory_columns_is_not_recalculated(self):
        attempt = fake_attempt('s', 'mpv', 'quiet', 1, 10.0)
        attempt['samples'] = [self.sample({'226:1': {'drm-memory-gtt': '100 KiB'}})]
        self.assertEqual(analyze.gpu_memory_metrics(attempt), {})


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

    def test_software_reference_requires_hwdec_no_on_both_boundaries(self):
        backend, progress, audio, quality, failures = evidence.validate_mpv(
            self.boundary('no', 10.0, 0), self.boundary('no', 30.0, 1), 'software')
        self.assertEqual((backend, progress, audio, failures), (True, True, True, []))
        self.assertEqual(quality['deltas']['frame-drop-count'], 1)
        # mpv, сам включивший hwdec хоть на одной границе, не эталон SW-сцены.
        for first, second in (('vulkan', 'vulkan'), ('no', 'vaapi-copy'), (None, 'no')):
            backend, *_ = evidence.validate_mpv(self.boundary(first, 1.0, 0), self.boundary(second, 2.0, 0),
                                                'software')
            self.assertFalse(backend, (first, second))

    def test_mpv_software_command_differs_only_by_hwdec(self):
        hardware = mpv_command('/usr/bin/mpv', Path('/run/ipc'), 'quiet', Path('/media.mp4'))
        software = mpv_command('/usr/bin/mpv', Path('/run/ipc'), 'quiet', Path('/media.mp4'), 'software')
        self.assertEqual(software[2], '--hwdec=no')
        self.assertEqual(hardware[:2] + hardware[3:], software[:2] + software[3:])

    def test_mpv_command_enables_only_hwdec_over_defaults(self):
        command = mpv_command('/usr/bin/mpv', Path('/run/ipc'), 'quiet', Path('/media.mp4'))
        self.assertEqual(command[1:5], ['--no-config', '--hwdec=auto', '--fs', '--volume=0'])
        self.assertIn('--input-ipc-server=/run/ipc', command)
        self.assertNotIn('-v', command)
        self.assertIn('-v', mpv_command('/usr/bin/mpv', Path('/run/ipc'), 'diagnostic', Path('/media.mp4')))


class DecodePathTests(unittest.TestCase):
    SOFTWARE_LOG = 'INFO resumed: Selected video pipeline plan="ffmpeg-host-upload-wgpu"\n'
    HARDWARE_LOG = ('INFO VA-API codec adapter configured for stream backend_name="VA-API H.264"\n'
                    'INFO Selected video pipeline plan="VA-API Vulkan DMA-BUF"\n')
    # Строки, как их пишет плеер (план видеоконвейера и контракт пула поверхностей VA-API).
    WGPU_HARDWARE_LOG = ('INFO Zero-copy resource lifecycle contract configured '
                         'backend_contract="VA-API Vulkan DMA-BUF" sample_only=true\n'
                         'INFO VA-API codec adapter configured for stream backend_name="VA-API H.264"\n'
                         'INFO Selected video pipeline plan="vaapi-dmabuf-wgpu"\n')
    GLES2_SOFTWARE_LOG = 'INFO resumed: Selected video pipeline plan="ffmpeg-host-upload-gles2"\n'
    GLES2_HARDWARE_LOG = ('INFO Zero-copy resource lifecycle contract configured '
                          'backend_contract="VA-API EGL DMA-BUF" sample_only=true\n'
                          'INFO VA-API codec adapter configured for stream backend_name="VA-API H.264"\n'
                          'INFO Selected video pipeline plan="vaapi-dmabuf-gles2"\n')

    def test_plans_without_decode_stay_hardware(self):
        self.assertEqual(decode_path({'player': 'mpv'}), 'hardware')
        self.assertEqual(decode_path({'player': 'fastiplayer', 'decode': 'software'}), 'software')

    def test_unknown_decode_or_vlc_software_is_a_plan_error(self):
        with self.assertRaisesRegex(ValueError, 'unsupported decode path'):
            decode_path({'player': 'mpv', 'decode': 'hw'})
        with self.assertRaisesRegex(ValueError, 'only hardware'):
            decode_path({'player': 'vlc', 'decode': 'software'})

    def test_fastiplayer_software_proof_needs_host_upload_plan_without_vaapi(self):
        self.assertTrue(evidence.fastiplayer_backend_proven(self.SOFTWARE_LOG, 'h264', 'software'))
        self.assertFalse(evidence.fastiplayer_backend_proven(self.HARDWARE_LOG, 'h264', 'software'))
        # Оба следа сразу (например, откат с VA-API на SW посреди попытки) — не доказательство.
        self.assertFalse(evidence.fastiplayer_backend_proven(self.SOFTWARE_LOG + self.HARDWARE_LOG,
                                                             'h264', 'software'))
        # Аппаратное доказательство не изменилось и не принимает SW-лог.
        self.assertTrue(evidence.fastiplayer_backend_proven(self.HARDWARE_LOG, 'h264', 'hardware'))
        self.assertFalse(evidence.fastiplayer_backend_proven(self.SOFTWARE_LOG, 'h264', 'hardware'))
        self.assertTrue(evidence.fastiplayer_backend_proven(self.WGPU_HARDWARE_LOG, 'h264', 'hardware'))

    def test_gles2_runs_are_classified_and_proven_by_their_own_strings(self):
        self.assertEqual(evidence.fastiplayer_renderer_family(self.GLES2_SOFTWARE_LOG), 'gles2')
        self.assertEqual(evidence.fastiplayer_renderer_family(self.GLES2_HARDWARE_LOG), 'gles2')
        self.assertEqual(evidence.fastiplayer_renderer_family(self.WGPU_HARDWARE_LOG), 'wgpu')
        self.assertEqual(evidence.fastiplayer_renderer_family(self.SOFTWARE_LOG), 'wgpu')
        self.assertIsNone(evidence.fastiplayer_renderer_family('INFO nothing about the renderer\n'))
        self.assertTrue(evidence.fastiplayer_backend_proven(self.GLES2_SOFTWARE_LOG, 'h264', 'software'))
        self.assertTrue(evidence.fastiplayer_backend_proven(self.GLES2_HARDWARE_LOG, 'h264', 'hardware'))
        self.assertFalse(evidence.fastiplayer_backend_proven(self.GLES2_HARDWARE_LOG, 'h264', 'software'))
        self.assertFalse(evidence.fastiplayer_backend_proven(self.GLES2_SOFTWARE_LOG, 'h264', 'hardware'))
        self.assertFalse(evidence.fastiplayer_backend_proven(self.GLES2_HARDWARE_LOG, 'hevc', 'hardware'))

    def test_gles2_hardware_needs_its_plan_and_egl_import_contract(self):
        # План GLES2, но пул поверхностей заявил импорт в Vulkan: следы двух рендеров сразу.
        vulkan_contract = self.GLES2_HARDWARE_LOG.replace('VA-API EGL DMA-BUF', 'VA-API Vulkan DMA-BUF')
        self.assertIsNone(evidence.fastiplayer_renderer_family(vulkan_contract))
        self.assertFalse(evidence.fastiplayer_backend_proven(vulkan_contract, 'h264', 'hardware'))
        # Контракт EGL без аппаратного плана GLES2 — не доказано, что кадры импортировал GLES2.
        without_plan = self.GLES2_HARDWARE_LOG.replace('plan="vaapi-dmabuf-gles2"', '')
        self.assertFalse(evidence.fastiplayer_backend_proven(without_plan, 'h264', 'hardware'))

    def test_mixed_renderer_families_prove_nothing(self):
        mixed = self.SOFTWARE_LOG + self.GLES2_SOFTWARE_LOG
        self.assertIsNone(evidence.fastiplayer_renderer_family(mixed))
        self.assertFalse(evidence.fastiplayer_backend_proven(mixed, 'h264', 'software'))
        # След VA-API в SW-попытке GLES2 — тоже не программный путь.
        self.assertFalse(evidence.fastiplayer_backend_proven(
            self.GLES2_SOFTWARE_LOG + 'INFO VA-API codec adapter configured for stream '
            'backend_name="VA-API H.264"\n', 'h264', 'software'))


class FastiplayerCommandTests(unittest.TestCase):
    def test_version_three_fastiplayer_enters_fullscreen_itself(self):
        command = fastiplayer_command('/opt/fastiplayer', Path('/media.mp4'))
        # Флаг стоит до файла: после файла он тоже сработал бы, но так порядок как у mpv.
        self.assertEqual(command, ['/opt/fastiplayer', '--fullscreen', '/media.mp4'])


class ProtocolVersionTests(unittest.TestCase):
    def test_plan_of_previous_protocol_is_rejected_before_any_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            plan = {'protocol_version': 2, 'diagnostic_repetitions': 1, 'common': {},
                    'players': {'fastiplayer': {}, 'mpv': {}}, 'scenarios': []}
            (root / 'plan.json').write_text(json.dumps(plan))
            with self.assertRaisesRegex(ValueError, 'protocol_version 3'):
                run_series.run_series(root / 'plan.json', root / 'series')
            self.assertFalse((root / 'series/attempts').exists())


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

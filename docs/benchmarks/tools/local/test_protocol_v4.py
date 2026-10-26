"""Протокол v4 (сессия 11 gles2-renderer, T410): dynamic, режим на сцену, VLC Qt, Gen5, стоп-краны,
лесенка, удалённый запуск. Каждый тест проверяет результат, а не вызов."""

import contextlib
import io
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from unittest import mock

import analyze
import collect
import conditions
import dynamic_decode
import evidence
import host
import intel_gpu
import ladder
import mpv_direct_rendering
import plan_v4
import players
import remote
import run_series
import service
from test_protocol_v2 import fake_attempt


# --- образцы -----------------------------------------------------------------------------

def log_line(second, text):
    """Строка tracing-лога с UTC-меткой: секунда от условного старта сервиса (00:00:00)."""
    return f'2026-10-26T12:00:{second:02}.000000Z  INFO {text}\n'


HW_START = (log_line(1, 'Zero-copy resource lifecycle contract configured backend_contract="VA-API EGL DMA-BUF"')
            + log_line(1, 'VA-API codec adapter configured for stream backend_name="VA-API H.264"')
            + log_line(1, 'Selected video pipeline plan="vaapi-dmabuf-gles2"')
            + log_line(2, 'Startup audio playback resumed'))
SWITCH = (log_line(10, 'Selected video pipeline plan="ffmpeg-host-upload-gles2"')
          + log_line(10, 'Hardware frame cannot be shown; controlled software fallback applied')
          + log_line(10, 'Dynamic decode switched to software expected_frames=100'))
# Старт сразу на программном декоде: кодек без аппаратного адаптера на машине (T410: HEVC).
SW_START = (log_line(1, 'Selected video pipeline plan="ffmpeg-host-upload-gles2"')
            + log_line(2, 'Startup audio playback resumed'))
SERVICE_EPOCH = 1_793_016_000.0  # 2026-10-26T12:00:00Z


def boundary_pair():
    """before/after Fastiplayer: тот же файл, время идёт, звук играет."""
    def one(position):
        return {'details': {'media_matches': {'inode': 1}, 'position_us': position, 'playback_status': 'Playing'},
                'audio_sinks': [{'state': 'running'}]}
    return one(1_000_000), one(21_000_000)


def validate_dynamic(log_text, window_start_second, window_end_second, codec='h264'):
    """evidence.validate для dynamic-попытки; окно задаётся секундами лога (по смещению строк)."""
    with tempfile.TemporaryDirectory() as directory:
        log_path = Path(directory) / 'runtime.log'
        log_path.write_text(log_text)
        raw = log_path.read_bytes()
        offsets = [raw.find(f':{second:02}.'.encode()) for second in (window_start_second, window_end_second)]
        offsets = [offset if offset >= 0 else len(raw) for offset in offsets]
        # Смещение — начало строки с нужной секундой.
        offsets = [raw.rfind(b'\n', 0, offset) + 1 if offset < len(raw) else offset for offset in offsets]
        before, after = boundary_pair()
        return evidence.validate('fastiplayer', codec, 'quiet', 'dynamic', log_path, before, after,
                                 offsets[0], offsets[1], renderer='gles2', service_started_epoch=SERVICE_EPOCH)


# --- 1. версия ----------------------------------------------------------------------------

class VersionTests(unittest.TestCase):
    def test_protocol_version_is_four_and_old_summaries_keep_their_version(self):
        self.assertEqual(host.PROTOCOL_VERSION, 4)
        old = fake_attempt('s', 'fastiplayer', 'quiet', 1, 3.0)
        old['protocol_version'] = 3
        self.assertEqual(analyze.series_protocol_version([old]), 3)
        self.assertEqual(analyze.series_protocol_version([fake_attempt('s', 'mpv', 'quiet', 1, 3.0)]), 4)

    def test_v3_plan_is_rejected_before_any_attempt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'plan.json').write_text(json.dumps(
                {'protocol_version': 3, 'common': {}, 'players': {'fastiplayer': {}, 'mpv': {}}, 'scenarios': []}))
            with self.assertRaisesRegex(ValueError, 'protocol_version 4'):
                run_series.run_series(root / 'plan.json', root / 'series')


# --- 2. dynamic ---------------------------------------------------------------------------

class DynamicDecodeTests(unittest.TestCase):
    def test_no_switch_proves_hardware_window(self):
        result = validate_dynamic(HW_START + log_line(12, 'frame'), 5, 15)
        self.assertEqual(result['effective_decode'], 'hardware')
        self.assertTrue(result['backend_confirmed'])
        self.assertEqual(result['failures'], [])
        self.assertEqual(result['dynamic_switch']['position'], 'none')

    def test_switch_before_window_proves_software_window_and_reports_the_second(self):
        # Окно с 12-й секунды: переход (10-я) позади; программный план лежит ВНУТРИ перехода.
        result = validate_dynamic(HW_START + SWITCH + log_line(12, 'frame') + log_line(20, 'frame'), 12, 20)
        self.assertEqual(result['effective_decode'], 'software')
        self.assertTrue(result['backend_confirmed'])
        self.assertEqual(result['failures'], [])
        self.assertEqual(result['dynamic_switch'], {'position': 'before_window', 'seconds': 10.0})

    def test_software_window_without_the_software_plan_is_not_proven(self):
        # Маркер перехода есть, а программного плана перед ним нет: нельзя назвать окно программным.
        no_plan = SWITCH.replace('Selected video pipeline plan="ffmpeg-host-upload-gles2"', 'something else')
        result = validate_dynamic(HW_START + no_plan + log_line(12, 'frame') + log_line(20, 'frame'), 12, 20)
        self.assertEqual(result['effective_decode'], 'software')
        self.assertFalse(result['backend_confirmed'])
        self.assertIn('expected dynamic backend not proven', result['failures'])

    def test_switch_inside_window_is_invalid_with_explicit_reason(self):
        result = validate_dynamic(HW_START + log_line(5, 'frame') + SWITCH + log_line(12, 'frame'), 5, 15)
        self.assertIsNone(result['effective_decode'])
        self.assertIn('dynamic switch inside measurement window', result['failures'])
        self.assertFalse(result['backend_confirmed'])

    def test_switch_after_window_keeps_hardware_proof_from_the_part_before_it(self):
        # Окно 5..10 (до начала перехода), переход на 10-й: путь окна аппаратный, а программный план не портит доказательство.
        result = validate_dynamic(HW_START + log_line(5, 'frame') + SWITCH, 5, 10)
        self.assertEqual(result['effective_decode'], 'hardware')
        self.assertTrue(result['backend_confirmed'])
        self.assertEqual(result['dynamic_switch']['position'], 'after_window')

    def test_no_switch_on_a_codec_without_hardware_proves_software_window(self):
        for codec in ('hevc', 'mpeg2'):
            result = validate_dynamic(SW_START + log_line(12, 'frame') + log_line(20, 'frame'), 12, 20, codec)
            self.assertEqual(result['effective_decode'], 'software', codec)
            self.assertTrue(result['backend_confirmed'], codec)
            self.assertEqual(result['failures'], [], codec)
            self.assertEqual(result['dynamic_switch']['position'], 'none', codec)

    def test_hardware_proof_of_a_codec_without_vaapi_adapter_is_false_not_a_crash(self):
        self.assertFalse(evidence.fastiplayer_backend_proven(HW_START, 'mpeg2', 'hardware'))

    def test_two_switches_are_a_failure(self):
        outcome = dynamic_decode.assess((HW_START + SWITCH + SWITCH).encode(), 0, 10**9, SERVICE_EPOCH)
        self.assertIsNone(outcome['effective_decode'])
        self.assertEqual(outcome['failures'], ['dynamic decode switched 2 times'])

    def test_dynamic_is_valid_only_for_fastiplayer(self):
        self.assertEqual(collect.decode_path({'player': 'fastiplayer', 'decode': 'dynamic'}), 'dynamic')
        for kind in ('mpv', 'vlc'):
            with self.assertRaisesRegex(ValueError, 'fastiplayer'):
                collect.decode_path({'player': kind, 'decode': 'dynamic'})


# --- 3. режим на игрока и сцену, VLC ----------------------------------------------------

def vlc_plan(**scenario_extra):
    return {'protocol_version': 4, 'common': {'decode': 'hardware'}, 'subject': 'gles2',
            'players': {'gles2': {'kind': 'fastiplayer', 'decode': 'dynamic'},
                        'mpv': {'kind': 'mpv', 'args': ['--vo=gpu']},
                        'vlc': {'kind': 'vlc', 'args': ['--avcodec-hw=vaapi', '--vout=gl']}},
            'scenarios': [{'scenario': 'h264-720p30', **scenario_extra}]}


class PlayerModeTests(unittest.TestCase):
    def test_player_entry_decode_overrides_common_and_scene_override_beats_entry(self):
        plan = vlc_plan(player_overrides={
            'vlc': {'decode': 'software', 'args': ['--avcodec-hw=none', '--vout=gl']},
            'mpv': {'args': ['--vo=gpu', '--gpu-api=opengl'], 'expect': {'current-vo': 'gpu'}}})
        scenario = plan['scenarios'][0]
        self.assertEqual(plan_v4.attempt_spec(plan, scenario, 'gles2', 'quiet', 1)['decode'], 'dynamic')
        self.assertEqual(plan_v4.attempt_spec(plan, scenario, 'mpv', 'quiet', 1)['decode'], 'hardware')
        vlc = plan_v4.attempt_spec(plan, scenario, 'vlc', 'quiet', 1)
        self.assertEqual((vlc['decode'], vlc['args']), ('software', ['--avcodec-hw=none', '--vout=gl']))
        self.assertEqual(plan_v4.attempt_spec(plan, scenario, 'mpv', 'quiet', 1)['expect'], {'current-vo': 'gpu'})
        # Другая сцена без переопределения видит записи игроков как есть.
        other = plan_v4.attempt_spec(plan, {'scenario': 'other'}, 'vlc', 'quiet', 1)
        self.assertEqual((other['decode'], other['args'][0]), ('hardware', '--avcodec-hw=vaapi'))
        self.assertNotIn('player_overrides', vlc)
        self.assertEqual(plan_v4.validate_plan(plan), ('gles2', 'mpv', 'vlc'))

    def test_plan_errors_surface_before_launch(self):
        cases = {
            'unknown player': vlc_plan(player_overrides={'nobody': {'decode': 'software'}}),
            'unknown field': vlc_plan(player_overrides={'mpv': {'binary': '/x'}}),
            'dynamic for a reference': vlc_plan(player_overrides={'mpv': {'decode': 'dynamic'}}),
            'vlc software without --avcodec-hw=none': vlc_plan(player_overrides={'vlc': {'decode': 'software'}}),
            'xcb_xv in a player entry': None,
            'xcb_xv in a scene override': vlc_plan(player_overrides={'vlc': {'args': ['--vout=xcb_xv']}}),
            'mpv expect with a typo': vlc_plan(player_overrides={'mpv': {'expect': {'hwdec': 'no'}}}),
        }
        entry = vlc_plan()
        entry['players']['vlc']['args'] = ['--avcodec-hw=vaapi', '--vout=xcb_xv']
        cases['xcb_xv in a player entry'] = entry
        for name, plan in cases.items():
            with self.subTest(name), self.assertRaises(ValueError):
                plan_v4.validate_plan(plan)

    def test_vlc_hardware_conflicting_with_avcodec_hw_none_is_rejected(self):
        plan = vlc_plan()
        plan['players']['vlc']['args'] = ['--avcodec-hw=none']
        with self.assertRaisesRegex(ValueError, 'conflicts'):
            plan_v4.validate_plan(plan)

    def test_vlc_v4_command_is_qt_and_v1_command_stays_frozen(self):
        command = collect.vlc_command_v4('/usr/bin/vlc', Path('/run/rc'), 'diagnostic', Path('/m.mp4'),
                                         ['--avcodec-hw=none', '--vout=gl'])
        self.assertEqual(command, [
            '/usr/bin/vlc', '--ignore-config', '--no-one-instance', '--extraintf', 'oldrc', '--rc-fake-tty',
            '--rc-unix=/run/rc', '--fullscreen', '--no-video-title-show', '--gain=0', '--no-qt-privacy-ask',
            '--no-qt-video-autoresize', '--avcodec-hw=none', '--vout=gl', '-vv', '/m.mp4'])
        self.assertNotIn('dummy', command)
        self.assertNotIn('-vv', collect.vlc_command_v4('/usr/bin/vlc', Path('/r'), 'quiet', Path('/m.mp4')))
        # Команда, которую собирает collector для kind vlc, — именно v4.
        spec = {'player': 'vlc', 'args': ['--avcodec-hw=vaapi', '--vout=gl']}
        built = collect.player_command(spec, '/usr/bin/vlc', Path('/r'), 'quiet', Path('/m.mp4'), 'hardware')
        self.assertIn('--no-qt-privacy-ask', built)
        self.assertIn('--avcodec-hw=vaapi', built)

    def test_vlc_software_evidence_requires_absence_of_hardware_trace(self):
        def verdict(log_text, decode):
            with tempfile.TemporaryDirectory() as directory:
                log_path = Path(directory) / 'runtime.log'
                log_path.write_text(log_text)
                counters = lambda value: {'details': {'counters': {  # noqa: E731
                    'media_time_seconds': value, 'frames_displayed': value * 30, 'frames_lost': 0,
                    'buffers_played': value, 'buffers_lost': 0, 'audio_decoded': value}},
                    'audio_sinks': [{'state': 'running'}]}
                return evidence.validate('vlc', 'h264', 'quiet', decode, log_path, counters(1), counters(21), 0, 0)
        hardware_log = 'avcodec debug: Using VA-API for hardware decoding\n'
        self.assertTrue(verdict('avcodec debug: software decoder\n', 'software')['backend_confirmed'])
        self.assertFalse(verdict(hardware_log, 'software')['backend_confirmed'])
        self.assertTrue(verdict(hardware_log, 'hardware')['backend_confirmed'])
        self.assertFalse(verdict('nothing\n', 'hardware')['backend_confirmed'])

    def test_vlc_in_wayland_cohort_is_proved_as_xwayland_and_says_so(self):
        vlc = {'player': 'vlc', 'window_mode': 'wayland'}
        mpv = {'player': 'mpv', 'window_mode': 'wayland'}
        self.assertEqual(players.window_path(vlc), 'xwayland_forced')
        self.assertEqual(players.window_proof_mode(vlc), 'xwayland')
        self.assertEqual((players.window_path(mpv), players.window_proof_mode(mpv)), ('native_wayland', 'wayland'))
        legacy = {'player': 'vlc'}
        self.assertEqual((players.window_path(legacy), players.window_proof_mode(legacy)), ('xwayland', 'xwayland'))

    def test_environment_forces_c_utf8_locale_whatever_the_session_has(self):
        with tempfile.TemporaryDirectory() as directory, mock.patch.dict(os.environ, {'LC_ALL': 'ru_RU.UTF-8'}):
            environment = service.isolated_environment(Path(directory), 'info')
        self.assertEqual(environment['LC_ALL'], 'C.UTF-8')


# --- 4. i915 Gen5 ------------------------------------------------------------------------

GEN5_FDINFO = ('drm-driver:\ti915\ndrm-client-id:\t52\ndrm-pdev:\t0000:00:02.0\n'
               'drm-total-system0:\t105472 KiB\ndrm-shared-system0:\t0\ndrm-active-system0:\t0\n'
               'drm-resident-system0:\t105472 KiB\ndrm-purgeable-system0:\t0\n')


class Gen5Tests(unittest.TestCase):
    def sample_from_fdinfo(self, text):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        proc = Path(directory.name)
        (proc / '4242/fdinfo').mkdir(parents=True)
        (proc / '4242/fd').mkdir()
        (proc / '4242/fdinfo/7').write_text(text)
        (proc / '4242/fd/7').symlink_to('/dev/null')
        return {'drm': {'4242': host.drm_clients(4242, proc_root=proc)}, 'drm_time': 0.0}

    def test_gen5_gives_memory_but_gpu_busy_is_not_tested_instead_of_zero(self):
        first = self.sample_from_fdinfo(GEN5_FDINFO)
        last = {**self.sample_from_fdinfo(GEN5_FDINFO), 'drm_time': 20.0}
        # Память GPU — из drm-resident-system0.
        self.assertEqual(host.gpu_memory_kib(first), {'gtt': 105472, 'vram': 0})
        gpu = host.summarize_gpu(first, last)
        self.assertEqual(gpu['engine_busy_percent'], {})
        self.assertEqual(gpu['busy_status'], 'NOT TESTED')
        self.assertEqual(gpu['busy_reason'], 'driver exposes no drm-engine-* counters')

    def test_engine_counters_present_means_a_measured_busy(self):
        text = GEN5_FDINFO + 'drm-engine-render:\t1000000000 ns\n'
        first = self.sample_from_fdinfo(text)
        later = self.sample_from_fdinfo(text.replace('1000000000', '3000000000'))
        later['drm_time'] = 20.0
        gpu = host.summarize_gpu(first, later)
        self.assertAlmostEqual(gpu['engine_busy_percent']['render'], 10.0)
        self.assertNotIn('busy_status', gpu)

    def test_pair_verdict_carries_the_driver_reason(self):
        def attempt(player, repetition):
            item = fake_attempt('s', player, 'quiet', repetition, 3.0 if player == 'fastiplayer' else 5.0)
            item['gpu'] = {'engine_busy_percent': {}, 'busy_status': 'NOT TESTED',
                           'busy_reason': 'driver exposes no drm-engine-* counters'}
            return item
        pairs = [(attempt('fastiplayer', r), attempt('mpv', r)) for r in (1, 2, 3)]
        _, gpu_verdict = analyze.cohort_verdicts('quiet', pairs, [a for p in pairs for a in p])
        self.assertEqual(gpu_verdict, {'status': 'NOT TESTED', 'reason': 'driver exposes no drm-engine-* counters'})


# --- 5. intel_gpu_top --------------------------------------------------------------------

def engines(render, video):
    return {'engines': {'Render/3D/0': {'busy': render, 'unit': '%'}, 'Video/0': {'busy': video, 'unit': '%'}}}


class IntelGpuTopTests(unittest.TestCase):
    STREAM = '[\n' + ',\n'.join(json.dumps(engines(r, v)) for r, v in ((90.0, 80.0), (10.0, 4.0), (20.0, 6.0))) + ',\n{"engines": {"Rend'

    def test_truncated_tail_is_ignored_and_whole_objects_are_kept(self):
        self.assertEqual(len(intel_gpu.parse_stream(self.STREAM)), 3)

    def test_first_sample_is_dropped_from_the_mean(self):
        result = intel_gpu.summarize(intel_gpu.parse_stream(self.STREAM))
        self.assertEqual(result['status'], 'MEASURED')
        self.assertEqual(result['samples'], 2)
        self.assertAlmostEqual(result['render_3d_busy_percent'], 15.0)
        self.assertAlmostEqual(result['video_busy_percent'], 5.0)
        self.assertIn('GLOBAL', result['scope'])

    def test_too_few_samples_or_no_engines_are_not_tested(self):
        self.assertEqual(intel_gpu.summarize([engines(1.0, 1.0)])['status'], 'NOT TESTED')
        self.assertEqual(intel_gpu.summarize([{'engines': {}}, {'engines': {}}])['status'], 'NOT TESTED')

    def test_runner_collects_a_real_subprocess_stream_and_survives_failures(self):
        with tempfile.TemporaryDirectory() as directory:
            script = ('import json,sys,time\nprint("[")\n'
                      + ''.join(f'print(json.dumps({engines(r, v)!r}) + ",", flush=True)\n'
                                for r, v in ((50.0, 50.0), (30.0, 2.0), (40.0, 4.0)))
                      + 'time.sleep(60)\n')
            runner = intel_gpu.IntelGpuTop(Path(directory) / 'out.json', ['python3', '-c', script])
            runner.start()
            deadline = time.monotonic() + 10
            while (Path(directory) / 'out.json').read_text().count('Video') < 3 and time.monotonic() < deadline:
                time.sleep(0.05)
            result = runner.stop()
            self.assertEqual(result['render_3d_busy_percent'], 35.0)
            # sudo без пароля не настроен / утилиты нет: колонка NOT TESTED, а не исключение.
            missing = intel_gpu.IntelGpuTop(Path(directory) / 'missing.json', ['/nonexistent/intel_gpu_top'])
            missing.start()
            self.assertEqual(missing.stop()['status'], 'NOT TESTED')
            denied = intel_gpu.IntelGpuTop(Path(directory) / 'denied.json',
                                           ['sh', '-c', 'echo "sudo: a password is required" >&2; exit 1'])
            denied.start()
            denied.process.wait(timeout=10)  # sudo отказывает сразу, ещё до окна замера
            outcome = denied.stop()
            self.assertEqual(outcome['status'], 'NOT TESTED')
            self.assertIn('password is required', outcome['reason'])


# --- 6. стоп-краны -----------------------------------------------------------------------

class ConditionTests(unittest.TestCase):
    def test_gpu_hang_lines_are_counted(self):
        journal = 'kernel: i915 0000:00:02.0: GPU HANG: ecode 5:1:85dffffb\nkernel: other\nGPU HANG again\n'
        self.assertEqual(conditions.count_gpu_hangs(run=lambda command: journal), 2)
        self.assertEqual(conditions.count_gpu_hangs(run=lambda command: 'clean\n'), 0)

    def test_hang_growth_makes_the_attempt_invalid(self):
        self.assertEqual(conditions.hang_check(1, 1), ({'before': 1, 'after': 1, 'grew': False}, None))
        info, failure = conditions.hang_check(1, 2)
        self.assertTrue(info['grew'])
        self.assertEqual(failure, 'gpu hang during attempt')

    def test_ac_state_from_power_supply_nodes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.assertEqual(conditions.ac_power_state(root), 'not_applicable')  # десктоп без AC-узлов
            (root / 'AC').mkdir()
            (root / 'AC/online').write_text('0\n')
            self.assertEqual(conditions.ac_power_state(root), 'offline')
            (root / 'AC/online').write_text('1\n')
            self.assertEqual(conditions.ac_power_state(root), 'online')

    def test_locked_hint_comes_from_the_active_seat0_session(self):
        calls = []

        def run(command):
            calls.append(command)
            return '2\n' if command[1] == 'show-seat' else 'yes\n'
        self.assertTrue(conditions.session_locked(run=run))
        self.assertEqual(calls[1], ['loginctl', 'show-session', '2', '-p', 'LockedHint', '--value'])
        self.assertFalse(conditions.session_locked(run=lambda c: '2\n' if c[1] == 'show-seat' else 'no\n'))

    def test_runnable_check_stops_on_lock_battery_or_unreadable_state(self):
        ok = conditions.require_runnable(power=lambda: 'online', locked=lambda: False)
        self.assertEqual(ok, {'locked_hint': False, 'ac_power': 'online'})
        for power, locked, text in (('online', True, 'locked'), ('offline', False, 'AC power')):
            with self.assertRaisesRegex(conditions.SeriesStop, text) as caught:
                conditions.require_runnable(power=lambda p=power: p, locked=lambda l=locked: l)
            self.assertEqual(caught.exception.exit_code, 4)

        def broken():
            raise RuntimeError('loginctl missing')
        with self.assertRaisesRegex(conditions.SeriesStop, 'cannot verify') as caught:
            conditions.require_runnable(power=lambda: 'online', locked=broken)
        self.assertEqual(caught.exception.exit_code, 4)

    def test_fullscreen_step_is_retried_exactly_once_and_the_retry_is_recorded(self):
        calls = []

        def flaky(pid):
            calls.append(pid)
            if len(calls) == 1:
                raise RuntimeError('qdbus6 exit 2')
        self.assertEqual(evidence.fullscreen_with_retry(7, flaky), {'retried': True, 'first_error': 'qdbus6 exit 2'})
        self.assertEqual(calls, [7, 7])
        calls.clear()
        self.assertEqual(evidence.fullscreen_with_retry(7, calls.append), {'retried': False, 'first_error': None})
        self.assertEqual(calls, [7])
        attempts = []

        def broken(pid):
            attempts.append(pid)
            raise RuntimeError('still down')
        with self.assertRaisesRegex(RuntimeError, 'still down'):
            evidence.fullscreen_with_retry(7, broken)
        self.assertEqual(len(attempts), 2)


class SeriesStopTests(unittest.TestCase):
    """run_series с настоящим run_attempt: collector подменён, условия среды — тоже."""

    def run_real(self, patches, result_payload=None):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        plan = {'protocol_version': 4, 'diagnostic_repetitions': 1, 'common': {},
                'players': {'fastiplayer': {}, 'mpv': {}}, 'scenarios': [{'scenario': 's'}]}
        (root / 'plan.json').write_text(json.dumps(plan))
        launched = []

        def fake_collector(command, stdout=None, stderr=None):
            launched.append(command)
            attempt = Path(command[-1])
            attempt.mkdir(parents=True)
            payload = fake_attempt('s', 'fastiplayer', 'quiet', 1, 3.0)
            payload.update(result_payload or {})
            (attempt / 'result.json').write_text(json.dumps(payload))
            return mock.Mock(returncode=1 if (result_payload or {}).get('status') == 'INVALID' else 0)

        with contextlib.ExitStack() as stack:
            for patch in patches:
                stack.enter_context(patch)
            stack.enter_context(mock.patch.object(run_series.subprocess, 'run', fake_collector))
            with contextlib.redirect_stdout(io.StringIO()) as output:
                code = run_series.run_series(root / 'plan.json', root / 'series')
        return code, launched, output.getvalue()

    def test_locked_screen_stops_the_series_before_the_first_attempt(self):
        stop = conditions.SeriesStop(conditions.EXIT_CONDITIONS, 'graphical session is locked')
        code, launched, output = self.run_real([mock.patch.object(conditions, 'require_runnable', side_effect=stop)])
        self.assertEqual(code, 4)
        self.assertEqual(launched, [])
        self.assertIn('locked', output)

    def test_gpu_hang_in_an_attempt_stops_the_series_with_its_own_exit_code(self):
        code, launched, output = self.run_real(
            [mock.patch.object(conditions, 'require_runnable', return_value={})],
            {'status': 'INVALID', 'failures': ['gpu hang during attempt'],
             'gpu_hang': {'before': 0, 'after': 1, 'grew': True}})
        self.assertEqual(code, 3)
        self.assertEqual(len(launched), 1)  # дальше первой попытки серия не пошла
        self.assertIn('GPU HANG', output)

    def test_clean_attempt_does_not_trigger_the_hang_stop(self):
        with tempfile.TemporaryDirectory() as directory:
            attempt = Path(directory)
            (attempt / 'result.json').write_text(json.dumps({'gpu_hang': {'grew': False}}))
            run_series.raise_if_gpu_hang(attempt, 'label')  # не бросает


# --- 7. mpv direct rendering -------------------------------------------------------------

class MpvDirectRenderingTests(unittest.TestCase):
    def test_exact_mpv_lines_are_classified(self):
        classify = mpv_direct_rendering.classify
        self.assertEqual(classify('[vd] DR enabled: yes\n', 'no'), 'yes')
        self.assertEqual(classify('[vd] DR enabled: no\n', 'no'), 'no')
        slow = '[vo/gpu] DR path suspected slow/uncached, disabling.\n[vd] DR failed - disabling.\n'
        self.assertEqual(classify(slow, 'no'), 'no')
        self.assertEqual(classify('[vo/gpu] DR path suspected slow/uncached, disabling.\n', 'no'), 'no')
        # Включён и затем отключён — итог «выключен».
        self.assertEqual(classify('[vd] DR enabled: yes\n[vd] DR failed - disabling.\n', 'no'), 'no')
        self.assertEqual(classify('[cplayer] nothing about it\n', 'no'), 'unknown')
        self.assertEqual(classify('[cplayer] nothing about it\n', 'vaapi'), 'not_applicable_hwdec')
        self.assertEqual(classify('[cplayer] nothing about it\n', None), 'unknown')

    def test_diagnostic_mpv_writes_its_log_file_and_quiet_does_not(self):
        diagnostic = collect.mpv_command('/usr/bin/mpv', Path('/r'), 'diagnostic', Path('/m'), 'software',
                                         log_file=Path('/attempt/mpv.log'))
        self.assertIn('-v', diagnostic)
        self.assertIn('--log-file=/attempt/mpv.log', diagnostic)
        quiet = collect.mpv_command('/usr/bin/mpv', Path('/r'), 'quiet', Path('/m'), 'software',
                                    log_file=Path('/attempt/mpv.log'))
        self.assertFalse(any(item.startswith('--log-file') for item in quiet))
        self.assertNotIn('-v', quiet)

    def test_reader_returns_unknown_when_the_log_is_absent(self):
        before = {'details': {'hwdec-current': 'no'}}
        self.assertEqual(collect.read_mpv_direct_rendering(Path('/nonexistent/mpv.log'), before), 'unknown')
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / 'mpv.log'
            log.write_text('[vd] DR failed - disabling.\n')
            self.assertEqual(collect.read_mpv_direct_rendering(log, before), 'no')


# --- 8. лесенка --------------------------------------------------------------------------

class LadderTests(unittest.TestCase):
    def run_plan(self, behaviour, scenario=None, common=None, diagnostic_repetitions=1):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        plan = {'protocol_version': 4, 'diagnostic_repetitions': diagnostic_repetitions, 'common': common or {},
                'players': {'fastiplayer': {}, 'mpv': {}}, 'scenarios': [{'scenario': 's', **(scenario or {})}]}
        (root / 'plan.json').write_text(json.dumps(plan))
        launched = []

        def fake_run_attempt(series_root, tooling, plan_value, scenario_value, player, mode, repetition):
            launched.append((player, mode, repetition))
            attempt = series_root / 'attempts' / f's-{mode}-{repetition:02}-{player}'
            attempt.mkdir(parents=True)
            payload = behaviour(player, mode, repetition)
            payload['spec'].update(plan_v4.attempt_spec(plan_value, scenario_value, player, mode, repetition))
            (attempt / 'result.json').write_text(json.dumps(payload))
            return 0

        with mock.patch.object(run_series, 'run_attempt', fake_run_attempt), \
                contextlib.redirect_stdout(io.StringIO()):
            code = run_series.run_series(root / 'plan.json', root / 'series')
        return code, launched, json.loads((root / 'series/summary.json').read_text())

    @staticmethod
    def noisy(player, mode, repetition, **extra):
        noise = 2.0 if repetition % 2 else -2.0
        return fake_attempt('s', player, mode, repetition, 5.0 + noise if player == 'fastiplayer' else 5.0, **extra)

    def test_default_ladder_extends_to_nine_while_inconclusive(self):
        _, launched, _ = self.run_plan(self.noisy)
        self.assertEqual(len([item for item in launched if item[1] == 'quiet']), 18)

    def test_single_look_runs_exactly_three_pairs_even_when_inconclusive(self):
        code, launched, summary = self.run_plan(self.noisy, scenario={'quiet_looks': [3]})
        self.assertEqual(code, 0)
        self.assertEqual(len([item for item in launched if item[1] == 'quiet']), 6)
        quiet = next(c for c in summary['cohorts'] if c['mode'] == 'quiet')['resource_verdict']
        self.assertEqual((quiet['status'], quiet['n']), ('INCONCLUSIVE', 3))

    def test_two_looks_stop_at_six(self):
        _, launched, _ = self.run_plan(self.noisy, common={'quiet_looks': [3, 6]})
        self.assertEqual(max(item[2] for item in launched if item[1] == 'quiet'), 6)

    def test_empty_quiet_looks_run_diagnostic_only(self):
        code, launched, summary = self.run_plan(self.noisy, scenario={'quiet_looks': []})
        self.assertEqual(code, 0)
        self.assertEqual({item[1] for item in launched}, {'diagnostic'})
        self.assertEqual(len(launched), 2)
        self.assertEqual({c['mode'] for c in summary['cohorts']}, {'diagnostic'})

    def test_invalid_looks_and_policies_are_plan_errors(self):
        for looks in ([4], [6, 3], [3, 3], [3.0], 'x', [True]):
            with self.subTest(looks), self.assertRaises(ValueError):
                ladder.quiet_looks({'common': {}}, {'quiet_looks': looks})
        with self.assertRaises(ValueError):
            ladder.loss_policy({'common': {'losses': 'ignore'}}, {})
        self.assertEqual(ladder.quiet_looks({'common': {}}, {}), (3, 6, 9))
        self.assertEqual(ladder.quiet_looks({'common': {'quiet_looks': [3]}}, {'quiet_looks': [3, 6]}), (3, 6))

    def test_stop_policy_still_stops_on_subject_losses(self):
        def behaviour(player, mode, repetition):
            return fake_attempt('s', player, mode, repetition, 3.0 if player == 'fastiplayer' else 5.0,
                                unique=1100 if player == 'fastiplayer' else None)
        code, launched, summary = self.run_plan(behaviour)
        self.assertEqual(code, 2)
        self.assertEqual(summary['quality_gates']['s']['status'], 'FAIL')

    def test_annotate_all_records_subject_and_reference_losses_and_keeps_going(self):
        def behaviour(player, mode, repetition):
            attempt = fake_attempt('s', player, mode, repetition, 3.0 if player == 'fastiplayer' else 5.0,
                                   unique=1190 if player == 'fastiplayer' else None,
                                   frames_lost=3 if player == 'mpv' else 0)
            attempt['effective_decode'] = 'software' if player == 'fastiplayer' else 'hardware'
            attempt['metrics']['process_cpu_percent'] = 2.0
            return attempt
        code, launched, summary = self.run_plan(behaviour, scenario={'losses': 'annotate_all', 'quiet_looks': [3]})
        self.assertEqual(code, 0)
        self.assertEqual(max(item[2] for item in launched), 3)  # серия дошла до конца лесенки
        self.assertEqual(summary['quality_gates']['s']['status'], 'LOSSES_ANNOTATED')
        scene = summary['scenes']['s']
        self.assertEqual(scene['losses_policy'], 'annotate_all')
        subject, mpv = scene['players']['fastiplayer'], scene['players']['mpv']
        self.assertEqual(subject['losses_per_window_mean'], 10)  # round(60 fps × 20 с) − 1190
        self.assertEqual(mpv['losses_per_window_mean'], 3)
        self.assertEqual(subject['effective_decode'], ['software'])
        self.assertEqual(mpv['effective_decode'], ['hardware'])
        self.assertEqual(subject['process_cpu_percent_mean'], 2.0)
        self.assertEqual(subject['tree_cpu_percent_mean'], 3.0)
        self.assertEqual(subject['rss_mean_mib'], 100_000 / 1024)
        self.assertEqual(subject['quiet_attempts'], 3)

    def test_old_reference_losses_key_still_works(self):
        def behaviour(player, mode, repetition):
            return fake_attempt('s', player, mode, repetition, 3.0 if player == 'fastiplayer' else 5.0,
                                frames_lost=3 if player == 'mpv' else 0)
        code, _, summary = self.run_plan(behaviour, common={'reference_losses': 'annotate'})
        self.assertEqual(code, 0)
        self.assertEqual(summary['quality_gates']['s']['status'], 'REFERENCE_LOSSES')


# --- 9. удалённый запуск -----------------------------------------------------------------

class RemoteTests(unittest.TestCase):
    def test_push_builds_rsync_sources_destinations_and_a_bash_build_step(self):
        target = remote.RemoteTarget('t410', 'bench')
        commands = remote.push_commands(target, Path('/plans/p.json'), [Path('/cfg/a.toml')], Path('/repo/target/release'))
        rsyncs = [c for c in commands if c[0] == 'rsync']
        sources = {c[-2]: c[-1] for c in rsyncs}
        self.assertEqual(sources[f'{remote.LOCAL_TOOLS}/'], 't410:bench/tools/local/')
        self.assertEqual(sources[f'{remote.T480S_TOOLS}/'], 't410:bench/tools/t480s/')
        self.assertEqual(sources['/repo/target/release/fastiplayer'], 't410:bench/release/')
        self.assertEqual(sources['/repo/target/release/lib/'], 't410:bench/release/lib/')
        self.assertEqual(sources['/cfg/a.toml'], 't410:bench/configs/')
        self.assertEqual(sources['/plans/p.json'], 't410:bench/plans/')
        self.assertTrue(all('--exclude=__pycache__' in c for c in rsyncs if remote.LOCAL_TOOLS.as_posix() in c[-2]))
        build = commands[-1]
        self.assertEqual(build[:3], ['ssh', 't410', build[2]])
        self.assertTrue(build[2].startswith('bash -c '))
        self.assertIn('cc -Wall -Wextra -Werror -O2 window_owner.c -lxcb -lxcb-res -o window-owner', build[2])
        self.assertIn('$HOME/bench/tools/local', build[2])

    def test_start_survives_ssh_drop_and_carries_the_session_environment(self):
        [command] = remote.start_commands(remote.RemoteTarget(), 'p.json', 'gles2-t410-1')
        self.assertEqual(command[:2], ['ssh', 't410'])
        script = command[2]
        self.assertTrue(script.startswith('bash -c '))  # логин-оболочка T410 — fish
        self.assertIn('systemd-run --user --unit=fastiplayer-series-gles2-t410-1.service', script)
        for variable in ('XDG_RUNTIME_DIR=/run/user/1000', 'DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus',
                         'WAYLAND_DISPLAY=wayland-1', 'DISPLAY=:0', 'XAUTHORITY="$xauthority"'):
            self.assertIn(f'--setenv={variable}', script)
        # XAUTHORITY ищется при старте по маске, а не прошит в команде.
        self.assertIn('ls /run/user/1000/xauth_*', script)
        self.assertIn('run_series.py', script)
        self.assertIn('plans/p.json', script)

    def test_status_and_fetch_commands(self):
        target = remote.RemoteTarget('lab', '/data/bench')
        [status] = remote.status_commands(target, 's1')
        self.assertIn('systemctl --user is-active fastiplayer-series-s1.service', status[2])
        self.assertIn('/data/bench/series/s1/run/runner.log', status[2])
        [fetch] = remote.fetch_commands(target, 's1', Path('/out/nested/s1'))
        self.assertEqual(fetch[-2:], ['lab:/data/bench/series/s1/', '/out/nested/s1/'])
        # Родительские каталоги назначения rsync создаёт сам (на пилоте их не было).
        self.assertIn('--mkpath', fetch)

    def test_bad_series_names_are_refused_everywhere(self):
        target = remote.RemoteTarget()
        for build in (lambda: remote.start_commands(target, 'p.json', 'a b; rm'),
                      lambda: remote.status_commands(target, 'X'),
                      lambda: remote.fetch_commands(target, '../x', Path('/o'))):
            with self.assertRaises(ValueError):
                build()
        with self.assertRaises(ValueError):
            remote.start_commands(target, 'p;x.json', 's1')

    def test_dry_run_prints_commands_and_runs_nothing(self):
        with mock.patch.object(remote.subprocess, 'run') as run, \
                contextlib.redirect_stdout(io.StringIO()) as output:
            code = remote.main(['--dry-run', '--alias', 'lab', 'start', 'p.json', 's1'])
        self.assertEqual(code, 0)
        run.assert_not_called()
        self.assertIn('ssh lab', output.getvalue())
        self.assertIn('fastiplayer-series-s1.service', output.getvalue())

    def test_real_run_executes_every_command_and_stops_on_the_first_failure(self):
        import subprocess
        outcomes = [None, subprocess.CalledProcessError(1, 'rsync')]
        with mock.patch.object(remote.subprocess, 'run', side_effect=outcomes) as run, \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
            code = remote.main(['push', '/plans/p.json'])
        self.assertEqual(code, 1)
        self.assertEqual(run.call_count, 2)


if __name__ == '__main__':
    unittest.main()

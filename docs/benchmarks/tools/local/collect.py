#!/usr/bin/env python3
"""Один локальный measurement attempt; ошибки сохраняются и никогда не становятся нулём CPU."""

import argparse
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import time
import tempfile

import evidence
from host import (PROTOCOL_VERSION, cpu_percent, gpu_memory_summary, probe_media, sample, sha256, source_fps,
                  summarize_gpu, write_json)
from service import MeasuredService, isolated_environment


def summarize(samples):
    if len(samples) < 2:
        raise ValueError('fewer than two samples')
    first, last = samples[0], samples[-1]
    if first['process']['start_ticks'] != last['process']['start_ticks']:
        raise ValueError('process PID identity changed')
    result = {}
    for key, counter, scale, output in [
        ('process', 'cpu_ticks', os.sysconf('SC_CLK_TCK'), 'process_cpu_percent'),
        ('cgroup_cpu', 'usage_usec', 1_000_000, 'tree_cpu_percent')]:
        time_key = 'cpu_time' if key == 'process' else 'time'
        elapsed = last[key][time_key] - first[key][time_key]
        result[output] = cpu_percent(last[key][counter] - first[key][counter], scale, elapsed)
        result[output + '_window_seconds'] = elapsed
    for metric in ('rss_sum_kib', 'pss_sum_kib'):
        values = [s[metric] for s in samples]
        result[metric] = {'mean': statistics.mean(values), 'sampled_max': max(values), 'minimum': min(values)}
    # Память GPU (GTT/VRAM) рядом с RSS: см. host.gpu_memory_kib. Если DRM-clients
    # не было ни в одном sample, метрики нет вовсе (в отчёте «—»), а не ложный 0.
    result.update(gpu_memory_summary(samples))
    return result


def vlc_command(binary, socket_path, mode, media):
    """Командная строка VLC — эталона протокола v1, без изменений против PR #12.

    В v2 эталон — mpv (у VLC 3.0.24 нет аппаратного AV1 на этой машине). Команда VLC
    сохранена байт-в-байт, чтобы серии v1 можно было воспроизвести и сравнить.
    """
    return [str(binary), '--ignore-config', '--no-one-instance', '--intf', 'dummy',
            '--extraintf', 'oldrc', '--rc-fake-tty', f'--rc-unix={socket_path}',
            '--avcodec-hw=vaapi', '--vout=gl', '--fullscreen', '--no-video-title-show', '--gain=0',
            *(['-vv'] if mode == 'diagnostic' else []), str(media)]


def mpv_command(binary, socket_path, mode, media, decode='hardware'):
    """Командная строка mpv по протоколу v2 (эталон с 2026-10-21, решение владельца).

    ``--no-config`` — аналог ``vlc --ignore-config``: ни пользовательский, ни системный
    mpv.conf не меняют нагрузку. Аппаратное декодирование mpv по умолчанию выключено
    ради надёжности; руководство mpv рекомендует включать его как ``hwdec=auto``, и
    это единственное отличие от поведения «из коробки». Какой hwdec mpv выбрал
    (на 780M — ``vulkan``, Vulkan Video на том же блоке VCN), фиксирует свойство
    ``hwdec-current`` на границах окна. ``--volume=0`` — аналог ``--gain=0``:
    аудиовывод работает. Встроенный интерфейс (OSC) остаётся по умолчанию.
    JSON IPC нужен только для чтения свойств.

    ``decode='software'`` (cpu-parity 08, эталон SW — тоже mpv по решению владельца):
    ``--hwdec=no`` — это поведение mpv «из коробки», так что SW-команда отличается от
    умолчаний mpv только фуллскрином, громкостью и IPC. Остальное (потоки FFmpeg/dav1d,
    ``gpu-next``) mpv выбирает сам.
    """
    hwdec = MPV_HWDEC_BY_DECODE_PATH[decode]
    return [str(binary), '--no-config', f'--hwdec={hwdec}', '--fs', '--volume=0',
            f'--input-ipc-server={socket_path}', *(['-v'] if mode == 'diagnostic' else []), str(media)]


def fastiplayer_command(binary, media):
    """Командная строка Fastiplayer по протоколу v3 (cpu-parity 06b, решение владельца).

    ``--fullscreen`` — аналог ``mpv --fs``: плеер сам входит в фуллскрин тем же путём, что
    кнопка, поэтому автоскрытие панели работает, как у пользователя. В v1/v2 окно делал
    полноэкранным только скрипт KWin, а под XWayland winit такой фуллскрин не видит:
    заголовок и панель оставались на экране (диагноз D2 сессии 06). Скрипт KWin остаётся
    у обоих плееров и в v3 — он же подтверждает, что окно на весь экран.
    """
    return [str(binary), '--fullscreen', str(media)]


PLAYER_COMMANDS = {'vlc': vlc_command, 'mpv': mpv_command}
# Путь декодирования попытки (поле ``decode`` плана). Аппаратный — умолчание: планы
# HW-серий 06/06c его не объявляют и должны давать прежние команды и доказательства.
# Программный путь Fastiplayer задаёт его конфиг (``preferred_backend = "software"``),
# а не командная строка; ``decode`` нужен эталону и проверке доказательств.
DECODE_PATHS = ('hardware', 'software')
MPV_HWDEC_BY_DECODE_PATH = {'hardware': 'auto', 'software': 'no'}


def decode_path(spec):
    """Путь декодирования попытки; неизвестное значение — ошибка плана, а не HW по умолчанию."""
    decode = spec.get('decode', 'hardware')
    if decode not in DECODE_PATHS:
        raise ValueError(f'unsupported decode path {decode!r}')
    if decode == 'software' and spec['player'] == 'vlc':
        # Команда VLC заморожена для воспроизведения серий v1 и умеет только VA-API.
        raise ValueError('VLC v1 reference command supports only hardware decode')
    return decode


def player_command(spec, binary, socket_path, mode, media, decode):
    """Командная строка плеера попытки (Fastiplayer, mpv или VLC v1)."""
    player = spec['player']
    if player == 'fastiplayer':
        return fastiplayer_command(binary, media)
    if player == 'mpv':
        return mpv_command(binary, socket_path, mode, media, decode)
    return vlc_command(binary, socket_path, mode, media)


def collect(spec, directory):
    directory = directory.resolve()
    directory.mkdir(parents=True, exist_ok=False)
    observation = {'schema_version': 1, 'protocol_version': PROTOCOL_VERSION,
                   'status': 'INVALID', 'spec': spec, 'samples': [],
                   'failures': [], 'clk_tck': os.sysconf('SC_CLK_TCK'),
                   'gpu_memory': {'status': 'N/A', 'reason': 'no qualified per-allocation DRM deduplication'},
                   'limitations': ['RSS sum includes shared mappings; PSS apportions them',
                                   'cgroup memory is separate, includes charged cache/kernel, not additive to RSS/PSS',
                                   'sampled maxima miss transient memory between samples',
                                   'physical scanout and speaker output are not measured']}
    service = None
    control_directory = None
    try:
        warmup, duration, interval = (float(spec[k]) for k in ('warmup_seconds', 'duration_seconds', 'interval_seconds'))
        if not all(math.isfinite(v) for v in (warmup, duration, interval)) or warmup < 3 or duration <= 0 or interval <= 0 or interval > duration:
            raise ValueError('invalid measurement timing')
        media, binary = Path(spec['media']), Path(spec['binary'])
        observation['media_probe'] = probe_media(media, spec['media_sha256'], warmup, duration)
        if sha256(binary) != spec['binary_sha256']:
            raise ValueError('binary hash mismatch')
        window_helper = Path(spec['window_helper'])
        if sha256(window_helper) != spec['window_helper_sha256']:
            raise ValueError('window helper hash mismatch')
        mode, player = spec['mode'], spec['player']
        if mode not in {'quiet', 'diagnostic'} or player not in {'fastiplayer', *PLAYER_COMMANDS}:
            raise ValueError('unsupported player or mode')
        decode = decode_path(spec)
        rust_log = 'info' + (',fastiplayer::video_render_acceptance=trace' if mode == 'diagnostic' else '')
        environment = isolated_environment(directory, rust_log)
        config = Path(spec['config'])
        if sha256(config) != spec['config_sha256']:
            raise ValueError('config hash mismatch')
        config_dir = directory / 'config/fastiplayer'
        config_dir.mkdir()
        shutil.copyfile(config, config_dir / 'config.toml')
        # AF_UNIX ограничивает длину адреса независимо от допустимой длины
        # artifact path. Только эфемерный IPC живёт в коротком runtime-каталоге.
        control_directory = tempfile.TemporaryDirectory(prefix='fi-bench-', dir=os.environ['XDG_RUNTIME_DIR'])
        socket_path = Path(control_directory.name) / 'rc'
        observation['control_socket'] = str(socket_path)
        command = player_command(spec, binary, socket_path, mode, media, decode)
        observation['command'] = command
        observation['environment'] = environment
        # Полное чтение одинаково прогревает page cache; вне lifecycle/CPU окна.
        with media.open('rb') as stream:
            while stream.read(1024 * 1024):
                pass
        service = MeasuredService(command, environment, directory)
        service.start()
        observation['unit'] = service.unit
        observation['pid'] = service.pid
        origin = time.monotonic()
        time.sleep(2)
        evidence.fullscreen(service.pid)
        # Одинаковый minimum warmup; actual CPU start идёт сразу после boundary
        # RPC. Его длительность и skew сохраняются, а не выдаются за точное +12s.
        time.sleep(max(0, origin + warmup - time.monotonic()))
        if spec.get('capture', False):
            observation['capture_before'] = evidence.capture(directory, 'before', service.pid, window_helper, spec['display_size'])
        observation['before'] = evidence.boundary(player, service.pid, socket_path, media, window_helper)
        log_path = directory / 'runtime.log'
        start_offset = log_path.stat().st_size
        observation['log_window_started'] = time.monotonic()
        observer_cpu_start = time.process_time()
        window_start = time.monotonic()
        next_sample = window_start
        while True:
            time.sleep(max(0, next_sample - time.monotonic()))
            observation['samples'].append(sample(service.pid, service.cgroup))
            if time.monotonic() >= window_start + duration:
                break
            next_sample = min(window_start + duration, max(next_sample + interval, time.monotonic()))
        observer_cpu_end = time.process_time()
        observer_elapsed = time.monotonic() - window_start
        end_offset = log_path.stat().st_size
        observation['log_window_ended'] = time.monotonic()
        observation['after'] = evidence.boundary(player, service.pid, socket_path, media, window_helper)
        if spec.get('capture', False):
            observation['capture_after'] = evidence.capture(directory, 'after', service.pid, window_helper, spec['display_size'])
        observation['actual_warmup_seconds'] = window_start - origin
        observation['boundary_skew_seconds'] = {
            'before': observation['samples'][0]['cgroup_cpu']['time'] - observation['before']['started'],
            'after': observation['after']['ended'] - observation['samples'][-1]['cgroup_cpu']['time']}
        observation['evidence'] = evidence.validate(player, spec['codec'], mode, decode, log_path,
            observation['before'], observation['after'], start_offset, end_offset)
        observation['failures'].extend(observation['evidence']['failures'])
        observation['metrics'] = summarize(observation['samples'])
        # GPU — отдельная метрика v2 рядом с CPU, никогда не складывается с ним.
        observation['gpu'] = summarize_gpu(observation['samples'][0], observation['samples'][-1])
        observation['source_fps'] = source_fps(observation['media_probe'])
        observation['observer_cpu_percent'] = 100 * (observer_cpu_end - observer_cpu_start) / observer_elapsed
        for boundary_value in (observation['before'], observation['after']):
            window = boundary_value['window']
            if [window['width'], window['height']] != spec['display_size']:
                observation['failures'].append('window geometry differs from frozen display size')
        if sha256(media) != spec['media_sha256'] or sha256(binary) != spec['binary_sha256']:
            raise ValueError('media or binary changed during attempt')
        observation['status'] = 'VALID_RESOURCE_OBSERVATION' if not observation['failures'] else 'INVALID'
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        observation['failures'].append(f'{type(error).__name__}: {error}')
    finally:
        if service is not None:
            try:
                observation['exit'] = service.stop()
                if observation['exit'] and observation['exit']['forced_kill']:
                    observation['failures'].append('forced kill during cleanup')
            except (OSError, RuntimeError) as error:
                observation['failures'].append(f'cleanup: {error}')
        if control_directory is not None:
            try:
                control_directory.cleanup()
            except OSError as error:
                observation['failures'].append(f'IPC cleanup: {error}')
        if observation['failures']:
            observation['status'] = 'INVALID'
        write_json(directory / 'result.json', observation)
    return observation


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('spec', type=Path)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    observation = collect(json.loads(args.spec.read_text()), args.directory)
    print(observation['status'], observation.get('metrics', {}), observation['failures'])
    return 0 if observation['status'] == 'VALID_RESOURCE_OBSERVATION' else 1


if __name__ == '__main__':
    raise SystemExit(main())

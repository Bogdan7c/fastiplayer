#!/usr/bin/env python3
"""Один изолированный runtime attempt; raw JSON не содержит команд или путей media."""

import argparse
import json
import math
import os
from pathlib import Path
import re
import socket
import subprocess
import struct
import sys
import time


class PlaybackProofMissing(Exception):
    """Живой процесс без реального первого кадра не является playback sample."""


class PreparationOverrun(Exception):
    """Подготовка не уложилась в общую отметку начала CPU-окна."""


def power_conditions():
    """Обезличенные доступные power/thermal значения; никаких device IDs."""
    result = {'thermal_zones': []}
    for zone in sorted(Path('/sys/class/thermal').glob('thermal_zone*')):
        result['thermal_zones'].append({'type': (zone / 'type').read_text().strip(),
                                        'millidegrees_celsius': int((zone / 'temp').read_text())})
    for label, path in {
        'governor': '/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor',
        'energy_performance_preference': '/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference',
        'ac_online': '/sys/class/power_supply/AC/online',
    }.items():
        result[label] = Path(path).read_text().strip() if Path(path).exists() else None
    return result


def render_events(log_path, start, end):
    """Allowlist событий handoff; границы byte offsets отдельно от CPU samples."""
    with log_path.open('rb') as runtime:
        runtime.seek(start)
        lines = runtime.read(end - start).decode('utf-8', errors='replace').splitlines()
    events = []
    for line in lines:
        if 'current video frame submitted to surface' not in line:
            continue
        fields = re.search(r'frame_pts_ns=(\d+)\s+render_generation=(\d+)\s+decoded_generation=(\d+)', line)
        if fields:
            event = dict(zip(['pts_ns', 'render_generation', 'decoded_generation'], map(int, fields.groups())))
            timestamp = re.match(r'\d{4}-\d{2}-\d{2}T[\d:.]+Z', line)
            event['log_timestamp_utc'] = timestamp[0] if timestamp else None
            events.append(event)
    return events


def playback_log_proofs(log_path):
    """Публикуем признаки известных этапов, не строки с локальными media URI."""
    runtime = log_path.read_text(errors='replace')
    markers = {
        'rust_demux_opened': 'Symphonia media source открыт',
        'rust_audio_decoder_created': 'Symphonia audio decoder создан',
        'rust_audio_output_created': 'Audio output создан после первого decoded AudioSpec',
        'rust_audio_resumed': 'Startup audio playback resumed',
        'rust_first_surface_presented': 'First startup video frame presented',
        'rust_h264_vaapi_configured': 'configured for stream backend_name="VA-API H.264"',
        'rust_hevc_vaapi_configured': 'configured for stream backend_name="VA-API H.265"',
        'rust_software_pipeline': 'plan="ffmpeg-host-upload-wgpu"',
        'rust_dmabuf_pipeline': 'VA-API Vulkan DMA-BUF',
        'vlc_ihd_hardware_decode': 'for hardware decoding',
        'vlc_dav1d_decoder': 'using video decoder module "dav1d"',
        'video_output_creation_failed': 'video output creation failed',
        'rust_panic': 'panicked at',
    }
    return {name: marker in runtime for name, marker in markers.items()}


def sample_process(pid, origin):
    """CPU включает все threads процесса; RSS читается точным smaps rollup."""
    process_dir = Path('/proc') / str(pid)
    cpu_read_started = time.monotonic()
    fields = (process_dir / 'stat').read_text().rsplit(')', 1)[1].split()
    cpu_read_finished = time.monotonic()
    # После comm первый элемент — stat field 3; utime/stime — fields 14/15.
    cpu_ticks = int(fields[11]) + int(fields[12])
    rollup = (process_dir / 'smaps_rollup').read_text()
    rss_read_finished = time.monotonic()
    rss = int(re.search(r'^Rss:\s+(\d+) kB$', rollup, re.MULTILINE)[1])
    children = set()
    for child_file in (process_dir / 'task').glob('*/children'):
        try:
            children.update(child_file.read_text().split())
        except FileNotFoundError:
            # Thread может штатно завершиться между перечислением и чтением.
            continue
    return {
        'elapsed_seconds': (cpu_read_started + cpu_read_finished) / 2 - origin,
        'cpu_read_duration_seconds': cpu_read_finished - cpu_read_started,
        'rss_read_end_elapsed_seconds': rss_read_finished - origin,
        'cpu_ticks': cpu_ticks,
        'rss_kib': rss,
        'child_process_count': len(children),
        'thread_count': int(fields[17]),
        'ac_online': Path('/sys/class/power_supply/AC/online').read_text().strip()
        if Path('/sys/class/power_supply/AC/online').exists() else None,
    }


def terminate_process(process):
    """Завершаем только запущенный нами процесс и явно отмечаем forced kill."""
    if process.poll() is not None:
        return False
    process.terminate()
    try:
        process.wait(timeout=5)
        return False
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
        return True


def capture_window(arguments, phase, origin):
    """Снимок вне CPU-окна позволяет проверить, что реальное видео продвинулось."""
    if arguments.capture_prefix is None:
        return None
    screenshot = arguments.capture_prefix.with_name(arguments.capture_prefix.name + f'-{phase}.png')
    subprocess.run([
        'spectacle', '-a', '-b', '-n', '-o', str(screenshot),
    ], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
    header = screenshot.read_bytes()[:24]
    if header[:8] != b'\x89PNG\r\n\x1a\n':
        raise ValueError('capture is not a PNG')
    width, height = struct.unpack('>II', header[16:24])
    if arguments.window_mode == 'xwayland-fullscreen' and arguments.expected_capture_size is not None and (width, height) != tuple(arguments.expected_capture_size):
        raise ValueError('fullscreen capture does not match qualified display')
    return {'file': screenshot.name, 'width': width, 'height': height,
            'capture_end_elapsed_seconds': time.monotonic() - origin}


def read_vlc_statistics(socket_path, origin):
    """Сохраняет только allowlisted counters; приветствие с media URI отбрасывается."""
    if socket_path is None:
        return None
    started = time.monotonic() - origin
    response = bytearray()
    with socket.socket(socket.AF_UNIX) as control:
        control.settimeout(1)
        control.connect(str(socket_path))
        control.sendall(b'stats\nget_time\n')
        while len(response) < 65536:
            try:
                chunk = control.recv(8192)
            except TimeoutError:
                break
            if not chunk:
                break
            response.extend(chunk)
            decoded = response.decode('utf-8', errors='replace')
            if 'buffers lost' in decoded and re.search(r'^\s*(\d+)\s*$', decoded, re.MULTILINE):
                break
    text = response.decode('utf-8', errors='replace')
    counters = {}
    for label in ['video decoded', 'frames displayed', 'frames lost', 'audio decoded', 'buffers played', 'buffers lost']:
        match = re.search(r'\|\s*' + label + r'\s*:\s*(\d+)', text)
        if match:
            counters[label.replace(' ', '_')] = int(match[1])
    times = re.findall(r'^\s*(\d+)\s*$', text, re.MULTILINE)
    if times:
        counters['media_time_seconds'] = int(times[-1])
    return {'read_start_elapsed_seconds': started, 'read_end_elapsed_seconds': time.monotonic() - origin, 'counters': counters}


def collect_attempt(arguments):
    observation = {
        'schema_version': 1,
        'attempt': arguments.attempt,
        'phase': arguments.phase,
        'scenario': arguments.scenario,
        'player': arguments.player,
        'window_mode': arguments.window_mode,
        'status': 'failed',
        'clock_ticks_per_second': os.sysconf('SC_CLK_TCK'),
        'settle_seconds': arguments.settle,
        'scheduled_window_start_seconds': arguments.window_start,
        'requested_window_seconds': arguments.duration,
        'sampling_interval_seconds': 1,
        'scope': 'launched process including all threads; children invalidate attempt',
        'samples': [],
        'exclusions': [],
    }
    process = None
    try:
        observation['power_before'] = power_conditions()
        with arguments.log.open('wb') as log_file:
            origin = time.monotonic()
            process = subprocess.Popen(arguments.command, stdin=subprocess.PIPE, stdout=log_file, stderr=log_file)
            if arguments.fullscreen_controller is not None:
                time.sleep(2)
                subprocess.run([
                    sys.executable, str(arguments.fullscreen_controller), str(process.pid),
                ], check=True, timeout=10)
            if arguments.capture_prefix is not None:
                time.sleep(max(0, origin + arguments.settle - time.monotonic()))
                observation['vlc_before_window'] = read_vlc_statistics(arguments.vlc_socket, origin)
                observation['before_window_capture'] = capture_window(arguments, 'before', origin)
            if arguments.require_rust_startup:
                runtime_log = arguments.log.read_text(errors='replace')
                markers = ['First startup video frame presented', 'Startup audio playback resumed']
                observation['startup_proofs_before_window'] = {
                    marker: marker in runtime_log for marker in markers
                }
                if not all(observation['startup_proofs_before_window'].values()):
                    raise PlaybackProofMissing()
            # Фиксированное окно отделяет startup от steady state; playback proof
            # проверяется отдельно, наличие живого процесса не считается playback.
            if time.monotonic() > origin + arguments.window_start:
                raise PreparationOverrun()
            next_sample = origin + arguments.window_start
            window_started = None
            log_start = None
            while True:
                time.sleep(max(0, next_sample - time.monotonic()))
                if process.poll() is not None:
                    observation['exclusions'].append('process_exited_before_window_end')
                    break
                sample = sample_process(process.pid, origin)
                sample['in_measurement_window'] = sample['elapsed_seconds'] >= arguments.settle
                observation['samples'].append(sample)
                if log_start is None:
                    log_start = arguments.log.stat().st_size
                    observation['render_log_start_elapsed_seconds'] = time.monotonic() - origin
                if sample['child_process_count']:
                    observation['exclusions'].append('child_process_outside_collector_scope')
                if arguments.phase == 'measurement' and sample['ac_online'] != '1':
                    observation['exclusions'].append('ac_power_not_confirmed_during_window')
                if sample['in_measurement_window']:
                    if window_started is None:
                        window_started = sample['elapsed_seconds']
                    if sample['elapsed_seconds'] - window_started >= arguments.duration:
                        observation['status'] = 'collected_pending_playback_review'
                        break
                next_sample += 1
                # Не создаём burst samples после задержки collector-а.
                next_sample = max(next_sample, time.monotonic())
                if window_started is not None:
                    # Финальный sample привязан к началу окна, а не к сетке
                    # секунд от spawn: read jitter не должен добавлять секунду.
                    next_sample = min(next_sample, origin + window_started + arguments.duration)
            log_end = arguments.log.stat().st_size
            observation['render_log_end_elapsed_seconds'] = time.monotonic() - origin
            if arguments.require_rust_startup and log_start is not None:
                observation['render_handoff_events'] = render_events(arguments.log, log_start, log_end)
                if not observation['render_handoff_events']:
                    observation['exclusions'].append('no_current_frame_handoffs_in_window')
            observation['vlc_after_window'] = read_vlc_statistics(arguments.vlc_socket, origin)
            observation['after_window_capture'] = capture_window(arguments, 'after', origin)
            observation['power_after'] = power_conditions()
            if arguments.phase == 'measurement' and any(
                observation[key]['ac_online'] != '1' for key in ['power_before', 'power_after']
            ):
                observation['exclusions'].append('ac_power_not_confirmed_at_boundary')
    except (OSError, ValueError, TypeError, subprocess.SubprocessError, PlaybackProofMissing, PreparationOverrun) as error:
        observation['exclusions'].append(type(error).__name__)
    finally:
        if process is not None:
            observation['forced_kill'] = terminate_process(process)
            observation['exit_code_after_cleanup'] = process.returncode
            if process.stdin is not None:
                process.stdin.close()
            if observation['forced_kill']:
                observation['exclusions'].append('forced_kill_after_termination_timeout')
        if arguments.log.exists():
            observation['playback_log_proofs'] = playback_log_proofs(arguments.log)
        if observation['exclusions']:
            observation['status'] = 'excluded'
        samples = [sample for sample in observation['samples'] if sample['in_measurement_window']]
        if len(samples) >= 2:
            first, last = samples[0], samples[-1]
            elapsed = last['elapsed_seconds'] - first['elapsed_seconds']
            observation['window_seconds'] = elapsed
            observation['cpu_percent_one_logical_core'] = (
                (last['cpu_ticks'] - first['cpu_ticks'])
                / observation['clock_ticks_per_second'] / elapsed * 100
            )
            observation['rss_kib_sample_mean'] = sum(sample['rss_kib'] for sample in samples) / len(samples)
            observation['rss_kib_sample_min'] = min(sample['rss_kib'] for sample in samples)
            observation['rss_kib_sample_max'] = max(sample['rss_kib'] for sample in samples)
        arguments.output.write_text(json.dumps(observation, indent=2) + '\n')
    return observation['status'] == 'collected_pending_playback_review'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--attempt', required=True, type=int)
    parser.add_argument('--phase', choices=['warmup', 'measurement', 'validation'], required=True)
    parser.add_argument('--scenario', required=True, choices=['h264-1080p60-hw', 'hevc-4k60-hw', 'av1-4k60-sw'])
    parser.add_argument('--player', choices=['fastiplayer', 'vlc'], required=True)
    parser.add_argument('--settle', type=float, default=15)
    parser.add_argument('--window-start', type=float, default=20)
    parser.add_argument('--duration', type=float, default=60)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--capture-prefix', type=Path)
    parser.add_argument('--fullscreen-controller', type=Path)
    parser.add_argument('--vlc-socket', type=Path)
    parser.add_argument('--window-mode', default='collector-validation', choices=['collector-validation', 'native-window', 'xwayland-window', 'xwayland-fullscreen'])
    parser.add_argument('--require-rust-startup', action='store_true')
    parser.add_argument('--expected-capture-size', type=int, nargs=2)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    arguments = parser.parse_args()
    if arguments.command[:1] == ['--']:
        arguments.command = arguments.command[1:]
    if (not arguments.command or not math.isfinite(arguments.duration)
            or not math.isfinite(arguments.settle) or not math.isfinite(arguments.window_start)
            or arguments.window_start < arguments.settle or arguments.duration <= 0 or arguments.settle < 0):
        parser.error('command and positive duration / nonnegative settle are required')
    if arguments.output.exists() or arguments.log.exists():
        parser.error('attempt output/log already exists; use new attempt paths')
    raise SystemExit(0 if collect_attempt(arguments) else 1)


if __name__ == '__main__':
    main()

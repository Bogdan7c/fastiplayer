"""Измерительные чтения Linux; process CPU и cgroup CPU не смешиваются."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import time


def command_output(command):
    try:
        completed = subprocess.run(command, capture_output=True, text=True, timeout=15)
    except subprocess.TimeoutExpired as error:
        raise RuntimeError(f'{command[0]} timed out') from error
    if completed.returncode:
        raise RuntimeError(f'{command[0]} exit {completed.returncode}: {completed.stderr.strip()}')
    return completed.stdout


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def write_json(path, payload):
    Path(path).write_text(json.dumps(payload, indent=2, ensure_ascii=False) + '\n')


def cpu_percent(delta, units_per_second, elapsed):
    if delta < 0 or units_per_second <= 0 or elapsed <= 0:
        raise ValueError('invalid CPU counter or elapsed interval')
    return 100 * delta / units_per_second / elapsed


def read_process(pid):
    root = Path('/proc') / str(pid)
    started = time.monotonic()
    fields = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    ended = time.monotonic()
    if fields[0] in {'Z', 'X'}:
        raise ProcessLookupError('process exited before window end')
    memory = {}
    for line in (root / 'smaps_rollup').read_text().splitlines():
        name, _, value = line.partition(':')
        if name in {'Rss', 'Pss', 'Private_Clean', 'Private_Dirty', 'SwapPss'}:
            memory[name] = int(value.split()[0])
    if not {'Rss', 'Pss'} <= memory.keys():
        raise ValueError('missing RSS/PSS')
    return {'pid': pid, 'start_ticks': int(fields[19]),
            'cpu_ticks': int(fields[11]) + int(fields[12]),
            'cpu_time': (started + ended) / 2, 'cpu_read_seconds': ended - started,
            'threads': int(fields[17]), 'memory_kib': memory}


def read_cgroup_cpu(root):
    started = time.monotonic()
    counters = dict(line.split() for line in (root / 'cpu.stat').read_text().splitlines())
    ended = time.monotonic()
    return {'time': (started + ended) / 2, 'read_seconds': ended - started,
            'usage_usec': int(counters['usage_usec']),
            'user_usec': int(counters['user_usec']), 'system_usec': int(counters['system_usec'])}


def optional_text(path):
    try:
        return {'value': Path(path).read_text().strip()}
    except OSError as error:
        return {'value': None, 'unavailable': str(error)}


def conditions():
    """Фон и питание регистрируются без изменения governor/EPP/desktop."""
    return {'loadavg': Path('/proc/loadavg').read_text().strip(),
            'system_cpu': Path('/proc/stat').read_text().splitlines()[0],
            'power': {str(path): optional_text(path) for pattern in (
                '/sys/class/power_supply/*/online',
                '/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor',
                '/sys/devices/system/cpu/cpu0/cpufreq/energy_performance_preference',
                '/sys/class/thermal/thermal_zone*/temp')
                for path in Path('/').glob(pattern.lstrip('/'))}}


def sample(pid, cgroup):
    started = time.monotonic()
    group_cpu = read_cgroup_cpu(cgroup)
    primary = read_process(pid)
    members = set()
    for path in [cgroup / 'cgroup.procs', *cgroup.glob('**/cgroup.procs')]:
        members.update(map(int, path.read_text().split()))
    if pid not in members:
        raise RuntimeError('primary process escaped cgroup or exited')
    # Исчезнувший child не теряется из CPU cgroup. Его transient RSS между
    # samples неизвестен: sampled maximum никогда не называется lifetime peak.
    processes, vanished = [primary], []
    for member in sorted(members - {pid}):
        try:
            processes.append(read_process(member))
        except (FileNotFoundError, ProcessLookupError):
            vanished.append(member)
    return {'started': started, 'ended': time.monotonic(), 'process': primary,
            'cgroup_cpu': group_cpu, 'processes': processes, 'vanished_members': vanished,
            'rss_sum_kib': sum(p['memory_kib']['Rss'] for p in processes),
            'pss_sum_kib': sum(p['memory_kib']['Pss'] for p in processes),
            'cgroup_memory_current': optional_text(cgroup / 'memory.current'),
            'cgroup_memory_peak': optional_text(cgroup / 'memory.peak'),
            'cgroup_memory_stat': optional_text(cgroup / 'memory.stat'),
            'conditions': conditions(), 'drm': {str(p['pid']): drm_clients(p['pid']) for p in processes}}


def probe_media(path, expected_hash, warmup, duration):
    """Хеш проверяется до запуска; чужой или слишком короткий файл не получает CPU=0."""
    if sha256(path) != expected_hash:
        raise ValueError('media hash mismatch')
    probe = json.loads(command_output(['ffprobe', '-v', 'error', '-show_streams',
                                      '-show_format', '-of', 'json', str(path)]))
    if float(probe['format']['duration']) <= warmup + duration + 2:
        raise ValueError('media too short for warmup, window and evidence boundary')
    kinds = {s['codec_type'] for s in probe['streams']}
    if not {'video', 'audio'} <= kinds:
        raise ValueError('measurement requires video and audio streams')
    return probe


def drm_clients(pid):
    """FD aliases дедуплицируются по DRM device/client; между clients не суммируем.

    Один DMA-BUF может быть импортирован разными clients. fdinfo не раскрывает
    достаточную allocation topology для корректного общего GPU total.
    """
    clients, errors = {}, []
    try:
        descriptors = list(Path(f'/proc/{pid}/fdinfo').iterdir())
    except (FileNotFoundError, ProcessLookupError) as error:
        return {'clients_not_additive': {}, 'observation_errors': [str(error)],
                'total': None, 'total_status': 'N/A: process exited during observation'}
    for descriptor in descriptors:
        try:
            fields = dict(line.split(':', 1) for line in descriptor.read_text().splitlines() if ':' in line)
            if 'drm-client-id' not in fields:
                continue
            device = Path(f'/proc/{pid}/fd/{descriptor.name}').stat().st_rdev
            key = f'{device}:{fields["drm-client-id"].strip()}'
            counters = {name: value.strip() for name, value in fields.items()
                        if name.startswith(('drm-memory-', 'drm-resident-', 'drm-total-', 'drm-shared-'))}
            if key in clients and clients[key] != counters:
                errors.append(f'client {key}: counters changed between alias reads')
            clients[key] = counters
        except (FileNotFoundError, ProcessLookupError):
            errors.append('fd disappeared during DRM observation')
        except PermissionError as error:
            errors.append(str(error))
    return {'clients_not_additive': clients, 'observation_errors': errors,
            'total': None, 'total_status': 'N/A: cross-client shared allocations are not identifiable'}

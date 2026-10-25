#!/usr/bin/env python3
"""Диагностический семплер: CPU по потокам процесса, RSS и GPU-время по DRM fdinfo.

Не бенчмарк-протокол, а быстрый срез «куда уходит CPU»: читает /proc/<pid>/task/*/stat
в начале и в конце окна, печатает проценты одного логического ядра на поток,
суммарный CPU процесса, RSS/PSS и занятость GPU-движков (drm-engine-*) по fdinfo.
"""

import argparse
import glob
import os
import sys
import time

CLK_TCK = os.sysconf("SC_CLK_TCK")


def read_thread_times(pid):
    """Возвращает {tid: (comm, utime+stime в тиках)} для всех потоков процесса."""
    result = {}
    task_dir = f"/proc/{pid}/task"
    for tid in os.listdir(task_dir):
        try:
            with open(f"{task_dir}/{tid}/stat") as handle:
                stat = handle.read()
            # comm может содержать пробелы и скобки — режем по последней ')'.
            close = stat.rfind(")")
            comm = stat[stat.find("(") + 1:close]
            fields = stat[close + 2:].split()
            utime, stime = int(fields[11]), int(fields[12])
            result[int(tid)] = (comm, utime + stime)
        except (FileNotFoundError, ProcessLookupError):
            continue
    return result


def read_process_times(pid):
    """utime+stime процесса целиком (включая завершившиеся потоки), в тиках."""
    with open(f"/proc/{pid}/stat") as handle:
        stat = handle.read()
    fields = stat[stat.rfind(")") + 2:].split()
    return int(fields[11]) + int(fields[12])


def read_memory(pid):
    """RSS и PSS в МиБ из smaps_rollup."""
    rss = pss = 0
    with open(f"/proc/{pid}/smaps_rollup") as handle:
        for line in handle:
            if line.startswith("Rss:"):
                rss = int(line.split()[1])
            elif line.startswith("Pss:"):
                pss = int(line.split()[1])
    return rss / 1024, pss / 1024


def read_gpu_engines(pid):
    """Сумма drm-engine-* (нс) по уникальным drm-client-id процесса."""
    engines = {}
    seen_clients = set()
    fd_dir = f"/proc/{pid}/fdinfo"
    for fd in os.listdir(fd_dir):
        try:
            with open(f"{fd_dir}/{fd}") as handle:
                text = handle.read()
        except (FileNotFoundError, PermissionError, OSError):
            continue
        if "drm-client-id" not in text:
            continue
        client_id = None
        values = {}
        for line in text.splitlines():
            if line.startswith("drm-client-id:"):
                client_id = line.split(":", 1)[1].strip()
            elif line.startswith("drm-engine-"):
                name, value = line.split(":", 1)
                values[name[len("drm-engine-"):]] = int(value.strip().split()[0])
            elif line.startswith("drm-memory-vram:") or line.startswith("drm-memory-gtt:") or line.startswith("drm-memory-cpu:"):
                name, value = line.split(":", 1)
                values["mem:" + name[len("drm-memory-"):]] = int(value.strip().split()[0])
        if client_id is None or client_id in seen_clients:
            continue
        seen_clients.add(client_id)
        for name, value in values.items():
            engines[name] = engines.get(name, 0) + value
    return engines


COMPOSITOR_NAMES = ("kwin_wayland", "Xwayland")


def find_compositor_pids():
    """PID-ы композитора сессии по имени процесса (comm) — {pid: comm}."""
    found = {}
    for entry in os.listdir("/proc"):
        if not entry.isdigit():
            continue
        try:
            with open(f"/proc/{entry}/comm") as handle:
                comm = handle.read().strip()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        if comm in COMPOSITOR_NAMES:
            found[int(entry)] = comm
    return found


def read_compositor_times(pids):
    """{pid: utime+stime} композитора; исчезнувший процесс — ошибка окна, а не ноль."""
    return {pid: read_process_times(pid) for pid in pids}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int)
    parser.add_argument("--duration", type=float, default=20.0, help="окно измерения, с")
    parser.add_argument("--warmup", type=float, default=15.0, help="ожидание до окна, с")
    parser.add_argument("--label", default="")
    parser.add_argument("--compositor", action="store_true",
                        help="CPU kwin_wayland и Xwayland за то же окно")
    arguments = parser.parse_args()
    pid = arguments.pid

    time.sleep(arguments.warmup)
    start_wall = time.monotonic()
    start_threads = read_thread_times(pid)
    start_process = read_process_times(pid)
    start_gpu = read_gpu_engines(pid)
    compositor_pids = find_compositor_pids() if arguments.compositor else {}
    start_compositor = read_compositor_times(compositor_pids)
    rss_samples = []
    pss_samples = []
    gpu_busy_samples = []
    # Первый DRM-адаптер, который отдаёт gpu_busy_percent (amdgpu); у Intel его нет.
    busy_candidates = sorted(glob.glob("/sys/class/drm/card*/device/gpu_busy_percent"))
    busy_path = busy_candidates[0] if busy_candidates else None
    deadline = start_wall + arguments.duration
    while time.monotonic() < deadline:
        time.sleep(1.0)
        try:
            rss, pss = read_memory(pid)
            rss_samples.append(rss)
            pss_samples.append(pss)
            if busy_path is not None:
                with open(busy_path) as handle:
                    gpu_busy_samples.append(int(handle.read().strip()))
        except (FileNotFoundError, ProcessLookupError):
            print("process exited during window", file=sys.stderr)
            return 1
    end_wall = time.monotonic()
    end_threads = read_thread_times(pid)
    end_process = read_process_times(pid)
    end_gpu = read_gpu_engines(pid)
    end_compositor = read_compositor_times(compositor_pids)
    elapsed = end_wall - start_wall

    print(f"=== {arguments.label} pid={pid} window={elapsed:.1f}s ===")
    total_percent = 100.0 * (end_process - start_process) / CLK_TCK / elapsed
    print(f"process CPU total: {total_percent:.2f}% of one core")
    if compositor_pids:
        compositor_total = 0.0
        for compositor_pid, comm in sorted(compositor_pids.items()):
            percent = 100.0 * (end_compositor[compositor_pid] - start_compositor[compositor_pid]) / CLK_TCK / elapsed
            compositor_total += percent
            print(f"compositor {comm} pid={compositor_pid}: {percent:.2f}%")
        print(f"process + compositor: {total_percent + compositor_total:.2f}% of one core")
    if rss_samples:
        print(f"RSS mean/max MiB: {sum(rss_samples)/len(rss_samples):.1f} / {max(rss_samples):.1f}; "
              f"PSS mean MiB: {sum(pss_samples)/len(pss_samples):.1f}")
    if gpu_busy_samples:
        print(f"whole-GPU busy % (sysfs, all clients): mean {sum(gpu_busy_samples)/len(gpu_busy_samples):.1f} max {max(gpu_busy_samples)}")
    gpu_lines = []
    for name in sorted(set(start_gpu) | set(end_gpu)):
        if name.startswith("mem:"):
            gpu_lines.append(f"  {name}: {end_gpu.get(name, 0)/1024:.1f} MiB")
        else:
            delta_ns = end_gpu.get(name, 0) - start_gpu.get(name, 0)
            gpu_lines.append(f"  engine {name}: {100.0 * delta_ns / 1e9 / elapsed:.2f}% busy")
    if gpu_lines:
        print("GPU per-process (fdinfo):")
        print("\n".join(gpu_lines))
    print("threads (>=0.05%):")
    rows = []
    for tid, (comm, end_ticks) in end_threads.items():
        start_ticks = start_threads.get(tid, (comm, 0))[1]
        percent = 100.0 * (end_ticks - start_ticks) / CLK_TCK / elapsed
        rows.append((percent, tid, comm))
    accounted = 0.0
    for percent, tid, comm in sorted(rows, reverse=True):
        accounted += percent
        if percent >= 0.05:
            print(f"  {percent:6.2f}%  {comm:<20} tid={tid}")
    print(f"  (sum over live threads {accounted:.2f}%; difference to total = exited threads)")
    return 0


if __name__ == "__main__":
    sys.exit(main())

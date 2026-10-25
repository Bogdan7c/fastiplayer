"""CPU композитора (kwin_wayland и Xwayland) за то же окно, что и CPU дерева плеера.

Решение владельца 12 (gles2-renderer): плеер под Wayland отдаёт часть работы композитору,
поэтому CPU считается двумя числами — дерево процесса и «дерево + композитор». Здесь
живёт только композиторная часть: поиск процессов, чтение их счётчиков, проверка
идентичности и перевод в проценты.

Идентичность процесса — пара (pid, starttime из /proc/PID/stat). Если за окно композитор
перезапустился или появился лишний Xwayland, счётчики разных процессов нельзя вычитать
друг из друга: попытка обязана стать невалидной, а не получить «разность» из воздуха.
Все функции принимают ``proc_root``, чтобы тесты работали на поддельном дереве /proc.
"""

import os
from pathlib import Path
import time

from host import cpu_percent

# Процессы композитора по ``comm`` (ядро режет имя до 15 символов; оба короче).
KWIN = 'kwin_wayland'
XWAYLAND = 'Xwayland'
COMPOSITOR_NAMES = (KWIN, XWAYLAND)
# Индексы полей /proc/PID/stat ПОСЛЕ закрывающей скобки comm (как в host.read_process).
STAT_UTIME, STAT_STIME, STAT_STARTTIME = 11, 12, 19


def _read_stat(proc_root, pid):
    """Разбор /proc/PID/stat: (comm процесса, cpu_ticks, start_ticks)."""
    text = (Path(proc_root) / str(pid) / 'stat').read_text()
    comm_end = text.rindex(')')
    comm = text[text.index('(') + 1:comm_end]
    fields = text[comm_end + 1:].split()
    return comm, int(fields[STAT_UTIME]) + int(fields[STAT_STIME]), int(fields[STAT_STARTTIME])


def discover(proc_root='/proc', uid=None):
    """Живые процессы композитора текущего пользователя с их идентичностью.

    Чужие сессии (другой uid) не наша стоимость и в сумму не входят. Процесс, исчезнувший
    во время обхода, пропускается: он гонка обхода, а не композитор окна (его пропажа
    внутри окна ловится сравнением идентичностей на границах).
    """
    uid = os.getuid() if uid is None else uid
    found = []
    for entry in sorted(Path(proc_root).iterdir(), key=lambda path: path.name):
        if not entry.name.isdigit():
            continue
        try:
            if entry.stat().st_uid != uid:
                continue
            comm, _, start_ticks = _read_stat(proc_root, entry.name)
        except (FileNotFoundError, ProcessLookupError):
            continue
        if comm in COMPOSITOR_NAMES:
            found.append({'name': comm, 'pid': int(entry.name), 'start_ticks': start_ticks})
    return found


def require_processes(identities, window_mode):
    """kwin_wayland нужен всегда, Xwayland — когда окно идёт через X11.

    В нативном Wayland Xwayland может быть жив (он часть честной стоимости и тогда
    входит в сумму), а может отсутствовать. Отсутствие обязательного процесса — ошибка:
    нулевой CPU «композитора» нельзя выдавать за измерение.
    """
    present = {identity['name'] for identity in identities}
    required = {KWIN} | ({XWAYLAND} if window_mode == 'xwayland' else set())
    missing = sorted(required - present)
    if missing:
        raise RuntimeError(f'compositor process not found: {", ".join(missing)}')


def read_sample(identities, proc_root='/proc'):
    """Снимок CPU известных процессов композитора с проверкой, что это те же процессы.

    Возвращает ``{'time': monotonic, 'cpu_ticks': {pid: ticks}}``. Исчезнувший или
    подменённый (другой starttime/comm) процесс — RuntimeError: попытка невалидна.
    """
    started = time.monotonic()
    ticks = {}
    for identity in identities:
        try:
            comm, cpu_ticks, start_ticks = _read_stat(proc_root, identity['pid'])
        except (FileNotFoundError, ProcessLookupError) as error:
            raise RuntimeError(f'compositor {identity["name"]} pid {identity["pid"]} vanished') from error
        if comm != identity['name'] or start_ticks != identity['start_ticks']:
            raise RuntimeError(f'compositor {identity["name"]} pid {identity["pid"]} identity changed')
        ticks[str(identity['pid'])] = cpu_ticks
    return {'time': (started + time.monotonic()) / 2, 'cpu_ticks': ticks}


def verify_unchanged(before, after):
    """Набор процессов композитора на обеих границах окна обязан совпасть целиком.

    Лишний Xwayland, поднявшийся посреди окна, или потерянный kwin — смена среды замера.
    """
    def key(identities):
        return sorted((i['name'], i['pid'], i['start_ticks']) for i in identities)
    if key(before) != key(after):
        raise RuntimeError(f'compositor processes changed during the window: {key(before)} -> {key(after)}')


def summarize(identities, samples, clk_tck):
    """Метрики композитора за окно по первому и последнему снимку.

    ``compositor_cpu_percent`` — сумма по всем процессам композитора, разбивка по имени
    отдельно (несколько Xwayland складываются под одним именем).
    """
    first, last = samples[0]['compositor'], samples[-1]['compositor']
    elapsed = last['time'] - first['time']
    by_name = {}
    for identity in identities:
        pid = str(identity['pid'])
        delta = last['cpu_ticks'][pid] - first['cpu_ticks'][pid]
        by_name[identity['name']] = by_name.get(identity['name'], 0.0) + cpu_percent(delta, clk_tck, elapsed)
    return {'compositor_cpu_percent': sum(by_name.values()),
            'compositor_cpu_percent_by_process': by_name,
            'compositor_cpu_percent_window_seconds': elapsed}

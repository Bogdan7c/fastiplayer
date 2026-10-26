"""Стоп-краны и условия каждой попытки (протокол v4): питание, экран, зависание GPU.

На T410 (Intel Gen5) серия идёт без присмотра. Три вещи делают замер бессмысленным или
опасным, и каждую проверяем явно, а не надеемся:

- питание не от сети — частоты и троттлинг другие (как в ``t480s/run_cohorts.py``);
- графическая сессия заблокирована (``LockedHint=yes``) — окна плееров не видны;
- ``GPU HANG`` в журнале ядра — драйвер завис, продолжать нельзя.

Стоп серии — исключение :class:`SeriesStop` с собственным кодом выхода: ``2`` занят
quality-gate-ом, поэтому здесь ``3`` (GPU HANG) и ``4`` (среда не годится).
Чтения через внедряемые функции, чтобы тесты шли без /sys, loginctl и journalctl.
"""

from pathlib import Path

from host import command_output

EXIT_GPU_HANG = 3
EXIT_CONDITIONS = 4
POWER_SUPPLY_ROOT = '/sys/class/power_supply'
GPU_HANG_MARKER = 'GPU HANG'
KERNEL_JOURNAL = ['journalctl', '-k', '-b', '--no-pager']


class SeriesStop(Exception):
    """Серию нужно остановить; ``exit_code`` — код выхода run_series.py, текст — для оператора."""

    def __init__(self, exit_code, message):
        super().__init__(message)
        self.exit_code = exit_code


def count_gpu_hangs(run=command_output):
    """Число строк ``GPU HANG`` в журнале ядра текущей загрузки."""
    return sum(GPU_HANG_MARKER in line for line in run(KERNEL_JOURNAL).splitlines())


def ac_power_state(power_root=POWER_SUPPLY_ROOT):
    """``online`` — питание от сети есть; ``offline`` — нет; ``not_applicable`` — у машины нет AC-узла.

    Десктоп без узлов ``AC*`` питается от сети по определению: останавливать серию там
    нечем. Ноутбук (T410: узел ``AC``) должен показать ``online`` хотя бы на одном AC-узле.
    """
    nodes = sorted(Path(power_root).glob('AC*/online'))
    if not nodes:
        return 'not_applicable'
    return 'online' if any(node.read_text().strip() == '1' for node in nodes) else 'offline'


def session_locked(run=command_output):
    """``LockedHint`` активной графической сессии seat0: True/False.

    Сессию берём у самого seat0 (``ActiveSession``), а не угадываем номер.
    """
    session = run(['loginctl', 'show-seat', 'seat0', '-p', 'ActiveSession', '--value']).strip()
    if not session:
        raise RuntimeError('seat0 has no active session')
    hint = run(['loginctl', 'show-session', session, '-p', 'LockedHint', '--value']).strip()
    if hint not in ('yes', 'no'):
        raise RuntimeError(f'unexpected LockedHint {hint!r}')
    return hint == 'yes'


def require_runnable(*, power=ac_power_state, locked=session_locked):
    """Перед попыткой: питание, экран. Нарушение — :class:`SeriesStop` (код 4).

    Невозможность прочитать условие (нет loginctl и т.п.) — тоже стоп: непроверенное
    условие нельзя считать выполненным.
    """
    try:
        is_locked, power_state = locked(), power()
    except (RuntimeError, OSError) as error:
        raise SeriesStop(EXIT_CONDITIONS, f'cannot verify measurement conditions: {error}') from error
    if is_locked:
        raise SeriesStop(EXIT_CONDITIONS, 'graphical session is locked (LockedHint=yes): windows are not visible')
    if power_state == 'offline':
        raise SeriesStop(EXIT_CONDITIONS, 'AC power required: running on battery')
    return {'locked_hint': False, 'ac_power': power_state}


def hang_check(before, after):
    """Итог GPU HANG за попытку: словарь для результата и причина невалидности (или None)."""
    grew = after > before
    return ({'before': before, 'after': after, 'grew': grew},
            'gpu hang during attempt' if grew else None)

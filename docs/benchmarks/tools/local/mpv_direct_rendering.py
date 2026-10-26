"""Direct rendering (DR) в mpv по его лог-файлу: проверка «равные ли условия» (протокол v4).

DR — mpv декодирует кадры сразу в память, пригодную для загрузки в GPU, без лишней копии.
Если DR включён у mpv, а у Fastiplayer нет эквивалента, сравнение CPU неравное; это знание
нужно отчёту. mpv пишет решение в лог при ``-v`` (в diagnostic-попытке, ``--log-file``).

Строки (проверено живьём на T410 и на 780M):
- ``DR enabled: yes`` (на 780M) — включён; ``DR enabled: no`` — выключен;
- ``[vo/gpu] DR path suspected slow/uncached, disabling.`` и ``[vd] DR failed - disabling.`` —
  mpv отключил DR (программный декод на T410).
При аппаратном декодере (hwdec ≠ no) кадры не проходят через DR, строк может не быть.
"""

import re

DR_YES, DR_NO, DR_UNKNOWN = 'yes', 'no', 'unknown'
DR_NOT_APPLICABLE_HWDEC = 'not_applicable_hwdec'
# Строки, после которых DR выключен.
_DISABLED_MARKERS = ('DR failed', 'DR path suspected slow/uncached')
_ENABLED = re.compile(r'DR enabled:\s*(\w+)')


def classify(log_text, hwdec_current):
    """``yes``/``no``/``unknown``/``not_applicable_hwdec`` по тексту лога mpv.

    Решает ПОСЛЕДНЯЯ значимая строка: DR, включённый и затем отключённый (``DR failed``),
    в итоге выключен. Нет значимых строк: при ``hwdec-current`` ≠ ``no`` (аппаратный путь)
    это ``not_applicable_hwdec``, иначе (в том числе hwdec не прочитан) честное ``unknown``.
    """
    verdict = None
    for line in log_text.splitlines():
        if any(marker in line for marker in _DISABLED_MARKERS):
            verdict = DR_NO
            continue
        enabled = _ENABLED.search(line)
        if enabled:
            verdict = DR_YES if enabled.group(1).lower() == 'yes' else DR_NO
    if verdict is not None:
        return verdict
    return DR_UNKNOWN if hwdec_current in (None, 'no') else DR_NOT_APPLICABLE_HWDEC

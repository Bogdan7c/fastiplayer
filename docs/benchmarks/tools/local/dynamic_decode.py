"""Доказательство эффективного пути декода Fastiplayer в режиме ``dynamic`` (протокол v4).

В ``dynamic`` Fastiplayer стартует на аппаратном декоде и ОДИН раз за файл может перейти
на программный, если железо не успевает (``crates/app-egui/src/state/dynamic_decode.rs``).
Переход пишется INFO-строкой ``Dynamic decode switched to software``. Окно замера обязано
целиком лежать на одном пути, иначе CPU окна — смесь двух путей и ничего не доказывает:

- перехода не было, или он случился ПОСЛЕ окна → путь окна ``hardware``; доказательство —
  обычное аппаратное, но только по части лога ДО перехода (после перехода в логе появляется
  программный план, и целиком лог аппаратным не докажешь);
- переход случился ДО начала окна → путь ``software``; доказательство — программный план
  в части лога ПОСЛЕ перехода (до него в логе законно лежат следы VA-API);
- переход внутри окна → попытка невалидна (``dynamic switch inside measurement window``).

Порядок строк при переходе (``state/video_backend.rs``): сначала установка программного
плана (``Selected video pipeline plan="ffmpeg-host-upload-…"``), потом INFO-строка
``Dynamic decode switched to software``. Поэтому переход — это промежуток от последней
строки ``Selected video pipeline`` перед маркером до самого маркера; программный план
лежит В НЁМ, а не после маркера.

Положение перехода относительно окна определяется байтовыми смещениями лога, а не
временем: смещения окна (``start_offset``/``end_offset``) collector снимает сам, поэтому
сравнение точное. Метка времени строки нужна только для отчёта («переход на N-й секунде»).
"""

from datetime import datetime, timezone
import re

# Точный текст INFO-строки перехода (state/dynamic_decode.rs, ``announce_dynamic_switch_to_software``).
SWITCH_MARKER = b'Dynamic decode switched to software'
# Строка установки плана конвейера: перед маркером перехода это установленный программный план.
PLAN_MARKER = b'Selected video pipeline'
# Метка времени tracing-строки (UTC), как её читает t480s/collect.py.
LOG_TIMESTAMP = re.compile(rb'\d{4}-\d{2}-\d{2}T[\d:.]+Z')

EFFECTIVE_HARDWARE = 'hardware'
EFFECTIVE_SOFTWARE = 'software'
FAILURE_SWITCH_INSIDE_WINDOW = 'dynamic switch inside measurement window'


def find_switches(log_bytes):
    """Все переходы: ``{'line_offset': начало строки маркера, 'plan_offset': начало установки плана,
    'epoch': секунды метки времени маркера | None}``.

    ``plan_offset`` — начало строки последнего ``Selected video pipeline`` перед маркером;
    нет такой строки — равно ``line_offset`` (программный план тогда не будет доказан).
    """
    switches, position = [], log_bytes.find(SWITCH_MARKER)
    while position >= 0:
        line_start = log_bytes.rfind(b'\n', 0, position) + 1  # rfind=-1 даёт 0: строка в начале файла
        line_end = log_bytes.find(b'\n', position)
        line = log_bytes[line_start:line_end if line_end >= 0 else len(log_bytes)]
        stamp = LOG_TIMESTAMP.search(line)
        epoch = None
        if stamp:
            parsed = datetime.fromisoformat(stamp.group().decode().replace('Z', '+00:00'))
            epoch = parsed.replace(tzinfo=parsed.tzinfo or timezone.utc).timestamp()
        plan_position = log_bytes.rfind(PLAN_MARKER, 0, position)
        plan_offset = log_bytes.rfind(b'\n', 0, plan_position) + 1 if plan_position >= 0 else line_start
        switches.append({'line_offset': line_start, 'plan_offset': plan_offset, 'epoch': epoch})
        position = log_bytes.find(SWITCH_MARKER, position + len(SWITCH_MARKER))
    return switches


def assess(log_bytes, window_start_offset, window_end_offset, service_started_epoch):
    """Эффективный путь окна ``dynamic``-попытки и тот кусок лога, по которому его доказывать.

    Возвращает словарь:
    ``effective_decode`` — ``hardware``/``software``/``None`` (None — путь окна неопределён);
    ``proof_log`` — текст лога для проверки пути (:func:`evidence.fastiplayer_backend_proven`);
    ``switch_seconds`` — секунды перехода от старта сервиса (None, если перехода не было или
    у строки нет метки времени); ``switch_position`` — ``none``/``before_window``/
    ``inside_window``/``after_window``; ``failures`` — причины невалидности.
    """
    switches = find_switches(log_bytes)
    if not switches:
        return _result(EFFECTIVE_HARDWARE, log_bytes, 'none', None, [])
    if len(switches) > 1:
        # Один файл — один переход (``switch_already_attempted``); два — сбой самого плеера.
        return _result(None, log_bytes, 'none', None, [f'dynamic decode switched {len(switches)} times'])
    switch = switches[0]
    seconds = (None if switch['epoch'] is None or service_started_epoch is None
               else switch['epoch'] - service_started_epoch)
    plan_offset = switch['plan_offset']
    # Промежуток перехода [plan_offset, line_offset]: целиком до окна, целиком после него
    # или (любое пересечение) внутри — тогда путь окна неопределён.
    if switch['line_offset'] < window_start_offset:
        return _result(EFFECTIVE_SOFTWARE, log_bytes[plan_offset:], 'before_window', seconds, [])
    if plan_offset >= window_end_offset:
        return _result(EFFECTIVE_HARDWARE, log_bytes[:plan_offset], 'after_window', seconds, [])
    return _result(None, log_bytes, 'inside_window', seconds, [FAILURE_SWITCH_INSIDE_WINDOW])


def _result(effective, proof_bytes, position, seconds, failures):
    return {'effective_decode': effective, 'proof_log': proof_bytes.decode(errors='replace'),
            'switch_position': position, 'switch_seconds': seconds, 'failures': failures}

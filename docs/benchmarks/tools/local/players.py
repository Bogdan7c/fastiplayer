"""Модель игроков плана серии: имя, тип (kind), субъект сравнения, порядок и режим окна.

Протокол v3, расширение сессии 10 gles2-renderer. Раньше имя игрока и его тип совпадали
(``fastiplayer``/``mpv``/``vlc``), а планов было два игрока. Теперь:

- запись ``players[name]`` может нести ``kind``; без него тип равен имени (старые планы);
- верхнеуровневый ключ плана ``subject`` называет игрока, чьё качество и CPU мы судим
  (по умолчанию ``fastiplayer``); все остальные — эталоны, судим «субъект против каждого»;
- игроков два или три.

Модуль не знает ни про процессы, ни про файлы: только чистые функции над словарями
плана и спецификации попытки, поэтому он целиком проверяется юнит-тестами.
"""

import re

# Типы игроков: от них зависят командная строка, доказательства и quality-gate.
PLAYER_KINDS = ('fastiplayer', 'mpv', 'vlc')
# Имя игрока — произвольный идентификатор; оно попадает в имена каталогов попыток.
PLAYER_NAME = re.compile(r'[a-z0-9-]+')
DEFAULT_SUBJECT = 'fastiplayer'
MIN_PLAYERS, MAX_PLAYERS = 2, 3
# Режим окна: ``xwayland`` — прежнее поведение (WAYLAND_DISPLAY не передаётся, плеер
# вынужденно идёт через X11); ``wayland`` — нативный клиент Wayland.
WINDOW_MODES = ('xwayland', 'wayland')
DEFAULT_WINDOW_MODE = 'xwayland'
# Рендеры Fastiplayer, которые можно потребовать у записи игрока (поле ``renderer``).
FASTIPLAYER_RENDERERS = ('gles2', 'wgpu')


def player_kind(spec):
    """Тип игрока попытки: явный ``kind`` или, как раньше, само имя игрока."""
    kind = spec.get('kind', spec['player'])
    if kind not in PLAYER_KINDS:
        raise ValueError(f'unsupported player kind {kind!r} for player {spec["player"]!r}')
    return kind


def subject_name(source):
    """Имя субъекта сравнения из плана или спецификации попытки (по умолчанию fastiplayer)."""
    return source.get('subject', DEFAULT_SUBJECT)


def window_mode(spec):
    """Режим окна попытки; неизвестное значение — ошибка плана, а не тихий XWayland."""
    mode = spec.get('window_mode', DEFAULT_WINDOW_MODE)
    if mode not in WINDOW_MODES:
        raise ValueError(f'unsupported window_mode {mode!r}; expected one of {WINDOW_MODES}')
    return mode


def validate_players(plan):
    """Проверяет раздел игроков плана до первой попытки; возвращает имена в порядке плана.

    Ошибка плана должна всплыть сразу, а не на третьем повторе: имена и типы, число
    игроков, наличие субъекта нужного типа и допустимость ``renderer``.
    """
    names = tuple(plan['players'])
    if not MIN_PLAYERS <= len(names) <= MAX_PLAYERS:
        raise ValueError(f'plan must list {MIN_PLAYERS} or {MAX_PLAYERS} players, got {len(names)}')
    for name in names:
        if not PLAYER_NAME.fullmatch(name):
            raise ValueError(f'player name {name!r} must match [a-z0-9-]+')
        entry = plan['players'][name]
        kind = player_kind({'player': name, **entry})
        if 'renderer' in entry:
            if kind != 'fastiplayer' or entry['renderer'] not in FASTIPLAYER_RENDERERS:
                raise ValueError(f'player {name!r}: renderer {entry["renderer"]!r} is valid only for '
                                 f'kind fastiplayer with one of {FASTIPLAYER_RENDERERS}')
    subject = subject_name(plan)
    if subject not in names:
        raise ValueError(f'plan must list subject player {subject!r}')
    if player_kind({'player': subject, **plan['players'][subject]}) != 'fastiplayer':
        raise ValueError(f'subject {subject!r} must be a player of kind fastiplayer')
    return names


def references_of(plan_or_spec, names):
    """Эталоны: все игроки, кроме субъекта (порядок как в ``names``)."""
    subject = subject_name(plan_or_spec)
    return tuple(name for name in names if name != subject)


def repetition_order(names, repetition):
    """Порядок игроков в повторении.

    Два игрока — прежний AB/BA (на чётных повторах порядок разворачивается), чтобы старые
    планы шли тем же порядком. Три — циклический сдвиг по номеру повтора (латинский
    квадрат): за три повтора каждый игрок побывал на каждой позиции, дрейф не копится у одного.
    """
    names = tuple(names)
    if len(names) == 2:
        return names if repetition % 2 else tuple(reversed(names))
    shift = (repetition - 1) % len(names)
    return names[shift:] + names[:shift]

"""Расписание «лесенки» и политика потерь (протокол v4); сводка по сцене для каждого игрока.

Раньше quiet шёл только 3→6→9 с правилом остановки. Лесенка квалификации (T410: HW/SW ×
режимы эталонов × сцены) хочет ФИКСИРОВАННОЕ расписание: ровно N пар без продления и без
остановки серии на потерях кадров — потери там данные, а не авария.

Поля (в ``common`` или в сцене; сцена главнее):
- ``quiet_looks``: точки оценки quiet, подмножество ``[3, 6, 9]`` (на них построен интервал
  ``analyze.comparison``; другое число пар вердикта не даст). ``[3]`` — ровно 3 пары без
  продления, ``[]`` — quiet не запускать (только diagnostic).
- ``losses``: ``stop`` (умолчание) — как раньше (потери субъекта FAIL, эталона — по
  ``reference_losses``); ``annotate_all`` — потери ЛЮБОГО игрока, и субъекта тоже, только
  записываются, серия идёт дальше.

Модуль не импортирует ``analyze``: функции анализа приходят аргументами, иначе цикл импортов.
"""

import statistics

DEFAULT_QUIET_LOOKS = (3, 6, 9)
LOSS_POLICIES = ('stop', 'annotate_all')
DEFAULT_LOSS_POLICY = 'stop'


def _setting(plan, scenario, name, default):
    """Значение поля: сцена главнее ``common``, иначе умолчание."""
    if name in scenario:
        return scenario[name]
    return plan.get('common', {}).get(name, default)


def quiet_looks(plan, scenario):
    """Точки оценки quiet сцены (кортеж по возрастанию); неверное значение — ошибка плана."""
    looks = _setting(plan, scenario, 'quiet_looks', list(DEFAULT_QUIET_LOOKS))
    if (not isinstance(looks, list) or not all(isinstance(look, int) and not isinstance(look, bool) for look in looks)
            or looks != sorted(set(looks)) or not set(looks) <= set(DEFAULT_QUIET_LOOKS)):
        raise ValueError(f'quiet_looks must be an increasing subset of {list(DEFAULT_QUIET_LOOKS)}, got {looks!r}')
    return tuple(looks)


def loss_policy(plan, scenario):
    """Политика потерь сцены: ``stop`` или ``annotate_all``."""
    policy = _setting(plan, scenario, 'losses', DEFAULT_LOSS_POLICY)
    if policy not in LOSS_POLICIES:
        raise ValueError(f'losses must be one of {LOSS_POLICIES}, got {policy!r}')
    return policy


def last_quiet_repetition(looks):
    """Последний повтор quiet; для ``[]`` — 0 (quiet не запускается)."""
    return looks[-1] if looks else 0


def frames_lost(quality):
    """Потери кадров за окно из результата ``analyze.attempt_quality`` или None, если не измерены.

    Fastiplayer: ``round(fps × окно) − уникальные идентичности``; mpv и VLC: их счётчики
    потерь (``frames_lost``). ``NOT TESTED`` — не ноль, а отсутствие числа.
    """
    if not quality or quality['status'] == 'NOT TESTED':
        return None
    if quality['metric'] == 'unique_frame_identities':
        return round(quality['expected']) - quality['unique']
    return quality['frames_lost']


def _mean_or_none(values):
    values = [value for value in values if value is not None]
    return statistics.mean(values) if values else None


def scene_summaries(attempts, valid_status, quality_of, memory_of):
    """Сводка по сцене и игроку: потери за окно, CPU (процесс/+композитор), RSS, GPU-память, декод.

    Потери — из всех валидных попыток, где они измеримы (у Fastiplayer только diagnostic: уникальные
    идентичности кадров пишет лишь trace diagnostic); CPU/RSS/память — по quiet. ``quality_of`` и
    ``memory_of`` — ``analyze.attempt_quality`` и ``analyze.memory_summary``. Ключ ``losses_policy``
    пишет политику сцены, чтобы читатель видел, что потери аннотированы, а не отброшены.
    """
    scenes = {}
    for attempt in attempts:
        if attempt['status'] != valid_status:
            continue
        scenes.setdefault(attempt['spec']['scenario'], {}).setdefault(attempt['spec']['player'], []).append(attempt)
    output = {}
    for scenario, by_player in sorted(scenes.items()):
        players = {}
        for player, group in sorted(by_player.items()):
            quiet = [a for a in group if a['spec']['mode'] == 'quiet']
            losses = [{'mode': a['spec']['mode'], 'repetition': a['spec']['repetition'], 'frames_lost': lost}
                      for a in group for lost in [frames_lost(quality_of(a))] if lost is not None]
            summary = {'losses_per_window': losses,
                       'losses_per_window_mean': _mean_or_none(item['frames_lost'] for item in losses),
                       'effective_decode': sorted({a['effective_decode'] for a in group if a.get('effective_decode')}),
                       'quiet_attempts': len(quiet)}
            if quiet:
                memory = memory_of(quiet)
                summary.update({
                    'process_cpu_percent_mean': _mean_or_none(a['metrics'].get('process_cpu_percent') for a in quiet),
                    'tree_cpu_percent_mean': statistics.mean(a['metrics']['tree_cpu_percent'] for a in quiet),
                    'tree_plus_compositor_cpu_percent_mean': _mean_or_none(
                        a['metrics'].get('tree_plus_compositor_cpu_percent') for a in quiet),
                    'rss_mean_mib': memory['rss_mean_mib'], 'gpu_memory_mean_mib': memory['gtt_mean_mib']})
            players[player] = summary
        output[scenario] = {'players': players,
                            'losses_policy': next(iter(a['spec'].get('losses', DEFAULT_LOSS_POLICY)
                                                       for group in by_player.values() for a in group))}
    return output

"""Парные сравнения одной конфигурации; качество и CPU-решение остаются разными полями.

Протокол v2 (README): решение по CPU принимается только по quiet-парам (3/6/9),
diagnostic — одна пара на сцену для quality-gate и описательного observer-control.
GPU — отдельный парный вывод по занятости gfx-движка, не смешивается с CPU.

Сессия 10 gles2-renderer: у когорты может быть несколько эталонов. Субъект (``subject``
плана, по умолчанию ``fastiplayer``) сравнивается с каждым отдельно — раздел ``pairs``;
верхнеуровневые ``reference_player``/``resource_verdict``/``gpu_gfx_verdict`` остаются
вердиктом первого эталона по имени (для двух игроков — ровно прежний вывод). Второй
CPU-вердикт (решение владельца 12) — по «дереву + композитор» за то же окно.
"""

import argparse
import json
import math
from pathlib import Path
import statistics

from host import PROTOCOL_VERSION, gpu_memory_summary, render_engine_busy_percent, write_json
from players import player_kind, subject_name, window_mode

LOOKS = (3, 6, 9)
# Критерий выхода сессии 06: INCONCLUSIVE засчитывается, только если весь интервал
# разности Fastiplayer − VLC лежит внутри ±0,5 п.п. Это не PARITY (PARITY без допуска
# владельца не выдаётся), а отдельная пометка рядом со статусом.
EXIT_BAND_PERCENTAGE_POINTS = 0.5
# Решение владельца (сессия 06): «кадры не потеряны», если уникальных идентичностей
# кадров за окно не меньше 99 % от частоты источника × длительность окна.
QUALITY_UNIQUE_FRAME_RATIO = 0.99
VALID = 'VALID_RESOURCE_OBSERVATION'
# Политики потерь у эталона (поле плана ``reference_losses``): см. reference_losses_policy.
REFERENCE_LOSSES_POLICIES = ('stop', 'annotate')
# A/V-синхрон эталона mpv: |avsync| на обеих границах окна не больше порога. 45 мс —
# самая строгая граница заметности рассинхрона из ITU-R BT.1359 (звук впереди картинки);
# берём её для обоих знаков, потому что знак avsync mpv в документации не закреплён.
AV_SYNC_LIMIT_SECONDS = 0.045
# Почему A/V у Fastiplayer не проверяется: честного источника разницы часов нет.
FASTIPLAYER_AV_SYNC_REASON = ('Fastiplayer exposes no A/V offset metric (MPRIS Position is the media clock, '
                              'render trace has no audio clock); A/V sync is not measured')


def comparison(differences, quantization_bound, *, unit="percentage_points"):
    """Консервативный Student interval: t=10 для каждого из трёх planned looks.

    Для df>=2 двухсторонняя tail probability меньше 0.01; union bound трёх
    просмотров меньше 0.03 на одну пару scenario/mode. Предполагаются независимые
    approximately-normal paired run errors; дрейф/выбросы требуют отдельного review.
    Нулевая выборочная дисперсия не отменяет разрешение измерительного счётчика.
    """
    if len(differences) not in LOOKS:
        return {'status': 'NOT TESTED', 'n': len(differences)}
    center = statistics.mean(differences)
    deviation = statistics.stdev(differences)
    radius = 10 * deviation / math.sqrt(len(differences)) + quantization_bound
    low, high = center - radius, center + radius
    return {'status': 'WIN' if high < 0 else 'WORSE' if low > 0 else 'INCONCLUSIVE',
            'n': len(differences), 'unit': unit, 'mean_difference': center, 'stdev_difference': deviation,
            'interval': [low, high], 'quantization_bound': quantization_bound,
            'parity': 'exact equality cannot be established by finite noisy samples; no loss margin authorized'}


def exit_band(verdict, band=EXIT_BAND_PERCENTAGE_POINTS):
    """Проходит ли вердикт критерий выхода: WIN или INCONCLUSIVE целиком внутри ±band п.п."""
    if verdict['status'] == 'WIN':
        return True
    if verdict['status'] != 'INCONCLUSIVE':
        return False
    low, high = verdict['interval']
    return -band <= low and high <= band


def plan_exit_band(pairs):
    """Коридор выхода из плана серии (поле ``exit_band_percentage_points`` в ``common``).

    Без поля — коридор HW-сессий 06/06c (±0,5 п.п.). У SW-серии сессии 08 по её плану
    ±1 п.п. Значение одно на серию: разные коридоры внутри когорты — ошибка плана.
    """
    bands = {a['spec'].get('exit_band_percentage_points', EXIT_BAND_PERCENTAGE_POINTS)
             for pair in pairs for a in pair}
    if len(bands) != 1:
        raise ValueError(f'cohort mixes exit bands {sorted(bands)}')
    band = bands.pop()
    if not isinstance(band, (int, float)) or not math.isfinite(band) or band <= 0:
        raise ValueError(f'invalid exit band {band!r}')
    return band


def gfx_percent(attempt):
    """Занятость движка рендера деревом процессов плеера, % (нет записи — None).

    Поле отчёта по-прежнему называется «gfx» (имя amdgpu); у i915 тот же движок
    называется ``render`` — сопоставление в ``host.RENDER_ENGINE_NAMES``.
    """
    return render_engine_busy_percent(attempt.get('gpu', {}).get('engine_busy_percent', {}))


def attempt_quality(attempt):
    """Quality-gate одной попытки; для Fastiplayer осмыслен только diagnostic-режим.

    Fastiplayer: уникальные (PTS, render generation, decoded generation) за окно
    журнала против частоты источника × длительность окна. VLC: прирост
    ``frames_lost`` за окно обязан быть 0 в любом режиме. Попытка без нужных
    данных возвращает ``NOT TESTED`` с причиной, а не PASS.
    """
    spec = attempt['spec']
    quality = attempt.get('evidence', {}).get('quality', {})
    kind = player_kind(spec)
    if kind == 'fastiplayer':
        # Эталон-Fastiplayer (например, wgpu против субъекта GLES2) судится так же, как субъект.
        if spec['mode'] != 'diagnostic':
            return None
        window = attempt.get('log_window_ended', 0) - attempt.get('log_window_started', 0)
        fps = attempt.get('source_fps')
        if 'unique_identities' not in quality or not fps or window <= 0:
            return {'status': 'NOT TESTED', 'reason': 'handoff identities or window missing'}
        expected = fps * window
        ratio = quality['unique_identities'] / expected
        return {'status': 'PASS' if ratio >= QUALITY_UNIQUE_FRAME_RATIO else 'FAIL',
                'metric': 'unique_frame_identities', 'unique': quality['unique_identities'],
                'handoffs': quality.get('handoffs'), 'expected': round(expected, 1), 'ratio': ratio}
    deltas = quality.get('deltas', {})
    if kind == 'mpv':
        # mpv считает потери в VO и в декодере отдельно; quality-gate требует ноль в обоих.
        drops = ('frame-drop-count', 'decoder-frame-drop-count')
        if not all(name in deltas for name in drops):
            return {'status': 'NOT TESTED', 'reason': 'mpv drop counters missing'}
        lost = sum(deltas[name] for name in drops)
        return {'status': 'PASS' if lost == 0 else 'FAIL', 'metric': 'mpv_dropped_frames',
                'frames_lost': lost, 'vo_delayed_frames': deltas.get('vo-delayed-frame-count')}
    if 'frames_lost' not in deltas:
        return {'status': 'NOT TESTED', 'reason': 'VLC counters missing'}
    return {'status': 'PASS' if deltas['frames_lost'] == 0 else 'FAIL', 'metric': 'vlc_frames_lost',
            'frames_lost': deltas['frames_lost'], 'frames_displayed': deltas.get('frames_displayed')}


def reference_losses_policy(attempts):
    """Что делать с потерями кадров у эталона (поле плана ``reference_losses``).

    ``stop`` (умолчание, решение владельца сессии 06): потеря у эталона — FAIL сцены,
    серия останавливается. ``annotate`` (план сессии 08, правило T480s-отчёта):
    сцена получает ``REFERENCE_LOSSES``, серия продолжается, а CPU-таблица
    публикуется с оговоркой «при потерях у эталона» и без заявлений о превосходстве.
    Потери у Fastiplayer остаются FAIL при любой политике.
    """
    policies = {a['spec'].get('reference_losses', 'stop') for a in attempts}
    if not policies <= set(REFERENCE_LOSSES_POLICIES) or len(policies) > 1:
        raise ValueError(f'invalid or mixed reference_losses policy {sorted(policies)}')
    return policies.pop() if policies else 'stop'


def quality_gates(attempts):
    """Итог quality-gate по сцене: FAIL, если провалилась хоть одна попытка.

    NOT TESTED, если у сцены нет ни одной diagnostic-попытки Fastiplayer: без
    подсчёта кадров «не потеряли кадры» не доказано, даже если CPU измерен.
    REFERENCE_LOSSES — Fastiplayer прошёл, а эталон терял кадры при политике
    ``annotate`` (см. :func:`reference_losses_policy`).
    """
    policy = reference_losses_policy([a for a in attempts if a['status'] == VALID])
    scenes = {}
    for attempt in attempts:
        if attempt['status'] != VALID:
            continue
        result = attempt_quality(attempt)
        if result is None:
            continue
        spec = attempt['spec']
        scenes.setdefault(spec['scenario'], []).append(
            {'player': spec['player'], 'mode': spec['mode'], 'repetition': spec['repetition'], **result})
    subjects = scene_subjects(attempts)
    output = {}
    for scenario, results in sorted(scenes.items()):
        subject = subjects[scenario]
        ours = {r['status'] for r in results if r['player'] == subject}
        reference = {r['status'] for r in results if r['player'] != subject}
        has_fastiplayer_count = bool(ours - {'NOT TESTED'})
        if 'FAIL' in ours or ('FAIL' in reference and policy == 'stop'):
            status = 'FAIL'
        elif not has_fastiplayer_count or (ours | reference) - {'PASS', 'FAIL'}:
            status = 'NOT TESTED'
        elif 'FAIL' in reference:
            status = 'REFERENCE_LOSSES'
        else:
            status = 'PASS'
        output[scenario] = {'status': status, 'attempts': results,
                            'av_sync': av_sync_gate([a for a in attempts if a['status'] == VALID
                                                     and a['spec']['scenario'] == scenario], subject)}
    return output


def scene_subjects(attempts):
    """Субъект каждой сцены; разные субъекты внутри сцены — ошибка серии."""
    subjects = {}
    for attempt in attempts:
        spec = attempt['spec']
        subjects.setdefault(spec['scenario'], set()).add(subject_name(spec))
    mixed = sorted(scenario for scenario, names in subjects.items() if len(names) > 1)
    if mixed:
        raise ValueError(f'scenarios mix subject players: {mixed}')
    return {scenario: names.pop() for scenario, names in subjects.items()}


def mpv_av_sync(attempt):
    """A/V одной попытки mpv: максимум |avsync| по границам окна или NOT TESTED без значений."""
    values = attempt.get('evidence', {}).get('quality', {}).get('avsync_seconds')
    if not values or not all(isinstance(value, (int, float)) for value in values):
        return {'status': 'NOT TESTED', 'reason': 'mpv avsync not recorded'}
    worst = max(abs(value) for value in values)
    return {'status': 'PASS' if worst <= AV_SYNC_LIMIT_SECONDS else 'FAIL', 'max_abs_seconds': worst}


def av_sync_gate(scene_attempts, subject):
    """A/V-синхрон сцены отдельным полем: на статус кадрового gate он не влияет.

    Субъект-Fastiplayer честно ``NOT TESTED`` (см. FASTIPLAYER_AV_SYNC_REASON), эталоны mpv —
    по свойству ``avsync``, остальные эталоны — без проверки.
    """
    references = {}
    for attempt in scene_attempts:
        spec = attempt['spec']
        if spec['player'] == subject:
            continue
        if player_kind(spec) == 'mpv':
            result = mpv_av_sync(attempt)
        else:
            result = {'status': 'NOT TESTED', 'reason': f'no A/V offset metric for kind {player_kind(spec)}'}
        references.setdefault(spec['player'], []).append(
            {'mode': spec['mode'], 'repetition': spec['repetition'], **result})
    return {'status': 'NOT TESTED', 'reason': FASTIPLAYER_AV_SYNC_REASON, 'references': references}


def cohort_verdicts(mode, pairs, cohort, subject='fastiplayer'):
    """CPU- и GPU-вердикты одной пары «субъект × эталон» в когорте (scenario, mode)."""
    differences = [a['metrics']['tree_cpu_percent'] - b['metrics']['tree_cpu_percent'] for a, b in pairs]
    if not pairs:
        reason = ('reference player not measured for this scenario'
                  if {a['spec']['player'] for a in cohort} == {subject} else 'no valid pairs')
        return {'status': 'NOT TESTED', 'reason': reason, 'valid_pairs': 0}, {'status': 'NOT TESTED'}
    if mode == 'diagnostic':
        # v2: одна diagnostic-пара — описание, не статистический вывод.
        return ({'status': 'DESCRIPTIVE', 'n': len(pairs), 'differences': differences},
                {'status': 'DESCRIPTIVE', 'differences': [gfx_percent(a) - gfx_percent(b) for a, b in pairs
                                                          if gfx_percent(a) is not None and gfx_percent(b) is not None]})
    # Защитный floor основного /proc счётчика, хотя tree CPU имеет microsecond units.
    resolution = max(200 / min(a['clk_tck'], b['clk_tck']) /
                     min(a['metrics']['tree_cpu_percent_window_seconds'],
                         b['metrics']['tree_cpu_percent_window_seconds']) for a, b in pairs)
    cpu = comparison(differences, resolution)
    if cpu['status'] != 'NOT TESTED':
        band = plan_exit_band(pairs)
        cpu['exit_band_percentage_points'] = band
        cpu['within_exit_band'] = exit_band(cpu, band)
    gfx = [(gfx_percent(a), gfx_percent(b)) for a, b in pairs]
    if any(value is None for pair in gfx for value in pair):
        gpu = {'status': 'NOT TESTED', 'reason': 'gfx engine counter missing'}
    else:
        # Счётчик движка в наносекундах: собственного разрешения, сравнимого с шумом, нет.
        gpu = comparison([a - b for a, b in gfx], 0.0)
    return cpu, gpu


COMPOSITOR_METRIC = 'tree_plus_compositor_cpu_percent'


def compositor_quantization(attempt):
    """Разрешение суммы композитора, п.п.: по одному тику /proc на каждый процесс композитора.

    utime+stime композитора читаются из /proc/PID/stat в тиках ``clk_tck``; ошибка
    округления разности — меньше тика на процесс.
    """
    metrics = attempt['metrics']
    processes = len(metrics.get('compositor_cpu_percent_by_process', {})) or 1
    return processes * 100 / attempt['clk_tck'] / metrics['compositor_cpu_percent_window_seconds']


def compositor_verdict(mode, pairs, tree_verdict):
    """Второй CPU-вердикт пары — по «дерево плеера + композитор» (решение владельца 12).

    Тот же Student-интервал и тот же коридор выхода, что у основного; к разрешению
    основного счётчика добавляется тиковое разрешение композитора обеих попыток пары.
    Старые серии без композитора — NOT TESTED с причиной, а не ноль.
    """
    if not pairs or tree_verdict['status'] == 'NOT TESTED':
        return {'status': 'NOT TESTED', 'reason': tree_verdict.get('reason', 'no valid pairs')}
    if not all(COMPOSITOR_METRIC in attempt['metrics'] for pair in pairs for attempt in pair):
        return {'status': 'NOT TESTED', 'reason': 'compositor CPU not recorded'}
    differences = [a['metrics'][COMPOSITOR_METRIC] - b['metrics'][COMPOSITOR_METRIC] for a, b in pairs]
    if mode == 'diagnostic':
        return {'status': 'DESCRIPTIVE', 'n': len(pairs), 'differences': differences}
    resolution = max(200 / min(a['clk_tck'], b['clk_tck']) /
                     min(a['metrics']['tree_cpu_percent_window_seconds'],
                         b['metrics']['tree_cpu_percent_window_seconds'])
                     + compositor_quantization(a) + compositor_quantization(b) for a, b in pairs)
    verdict = comparison(differences, resolution)
    if verdict['status'] != 'NOT TESTED':
        band = plan_exit_band(pairs)
        verdict['exit_band_percentage_points'] = band
        verdict['within_exit_band'] = exit_band(verdict, band)
    return verdict


def gpu_memory_metrics(attempt):
    """Сводка GTT/VRAM попытки: из сохранённых сырых samples, а без них — из metrics.

    Пересчёт из samples нужен, чтобы исправление разбора fdinfo (i915, сессия 10)
    дошло до уже снятых серий; для amdgpu пересчёт совпадает с сохранённым значением.
    Пересчитываются только попытки, чей collector уже записывал память (есть
    ``gpu_*_kib`` в metrics): старые серии до 08b остаются без этих колонок, как были.
    """
    stored = {key: value for key, value in attempt['metrics'].items() if key.startswith('gpu_')}
    samples = attempt.get('samples')
    if stored and samples and all('drm' in sample_value for sample_value in samples):
        return gpu_memory_summary(samples)
    return stored


def optional_mib(group, metric, field, reduce):
    """Агрегат метрики памяти в МиБ или None, если её нет в сохранённых попытках.

    Старые серии (до сессии 08b) не содержат gpu_gtt_kib/gpu_vram_kib: отсутствие
    даёт None (в отчёте «—»), а не ошибку и не ноль.
    """
    memory = [gpu_memory_metrics(a) for a in group]
    values = [m[metric][field] for m in memory if metric in m]
    if len(values) != len(group):
        return None
    return reduce(values) / 1024


def memory_summary(group):
    """RSS-колонки без изменений плюс GTT/VRAM и RSS+GTT (решение владельца 08b)."""
    mean = statistics.mean
    gtt = optional_mib(group, 'gpu_gtt_kib', 'mean', mean)
    rss = mean(a['metrics']['rss_sum_kib']['mean'] for a in group) / 1024
    return {'rss_mean_mib': rss,
            'pss_mean_mib': mean(a['metrics']['pss_sum_kib']['mean'] for a in group) / 1024,
            'rss_sampled_max_mib': max(a['metrics']['rss_sum_kib']['sampled_max'] for a in group) / 1024,
            'pss_sampled_max_mib': max(a['metrics']['pss_sum_kib']['sampled_max'] for a in group) / 1024,
            'gtt_mean_mib': gtt,
            'gtt_sampled_max_mib': optional_mib(group, 'gpu_gtt_kib', 'sampled_max', max),
            'vram_mean_mib': optional_mib(group, 'gpu_vram_kib', 'mean', mean),
            'vram_sampled_max_mib': optional_mib(group, 'gpu_vram_kib', 'sampled_max', max),
            'rss_plus_gtt_mean_mib': None if gtt is None else rss + gtt}


def pair_analysis(mode, cohort, by_key, repetitions, subject, reference):
    """Вердикты одной пары «субъект × эталон»: CPU, GPU и CPU с композитором.

    Невалидная или непарная попытка любого из двоих делает CPU-вердикт пары NOT TESTED —
    как раньше для единственной пары когорты.
    """
    pairs = [(by_key[(r, subject)], by_key[(r, reference)]) for r in repetitions
             if (r, subject) in by_key and (r, reference) in by_key]
    verdict, gpu_verdict = cohort_verdicts(mode, pairs, cohort, subject)
    members = [a for a in cohort if a['spec']['player'] in (subject, reference)]
    if pairs and (any(a['status'] != VALID for a in members) or len(pairs) * 2 != len(members)):
        verdict = {'status': 'NOT TESTED', 'reason': 'invalid or unpaired attempts retained', 'valid_pairs': len(pairs)}
    return {'resource_verdict': verdict, 'gpu_gfx_verdict': gpu_verdict,
            'compositor_resource_verdict': compositor_verdict(mode, pairs, verdict)}


def optional_mean(group, metric):
    """Среднее необязательной метрики или None, если её нет хоть в одной попытке."""
    values = [a['metrics'][metric] for a in group if metric in a['metrics']]
    return statistics.mean(values) if values and len(values) == len(group) else None


def player_summary(group):
    """Описательная сводка одного плеера когорты (CPU, gfx, память, композитор)."""
    cpu = [a['metrics']['tree_cpu_percent'] for a in group]
    gfx = [value for value in map(gfx_percent, group) if value is not None]
    return {'n': len(cpu), 'cpu_mean': statistics.mean(cpu),
            'cpu_min': min(cpu), 'cpu_max': max(cpu),
            'cpu_stdev': statistics.stdev(cpu) if len(cpu) > 1 else None,
            'gfx_busy_percent_mean': statistics.mean(gfx) if gfx else None,
            **memory_summary(group),
            'compositor_cpu_mean': optional_mean(group, 'compositor_cpu_percent'),
            'tree_plus_compositor_cpu_mean': optional_mean(group, COMPOSITOR_METRIC)}


def cohort_window_mode(cohort):
    """Режим окна когорты; эталоны обязаны мериться в том же режиме, что и субъект."""
    modes = {window_mode(a['spec']) for a in cohort}
    if len(modes) != 1:
        raise ValueError(f'cohort mixes window modes {sorted(modes)}')
    return modes.pop()


def analyze(root):
    attempts = [json.loads(path.read_text()) for path in sorted(root.glob('attempts/*/result.json'))]
    cohorts = {}
    for attempt in attempts:
        spec = attempt['spec']
        cohorts.setdefault((spec['scenario'], spec['mode']), []).append(attempt)
    subjects = scene_subjects(attempts)
    output = []
    for (scenario, mode), cohort in sorted(cohorts.items()):
        subject = subjects[scenario]
        valid = [a for a in cohort if a['status'] == VALID]
        by_key = {(a['spec']['repetition'], a['spec']['player']): a for a in valid}
        if len(by_key) != len(valid):
            raise ValueError('duplicate scenario/mode/repetition/player')
        repetitions = sorted({key[0] for key in by_key})
        # Эталоны — все игроки когорты, кроме субъекта; каждый судится в своей паре.
        references = sorted({a['spec']['player'] for a in cohort} - {subject})
        pairs = {reference: pair_analysis(mode, cohort, by_key, repetitions, subject, reference)
                 for reference in references}
        if references:
            first = pairs[references[0]]
            verdict, gpu_verdict = first['resource_verdict'], first['gpu_gfx_verdict']
        else:
            verdict, gpu_verdict = cohort_verdicts(mode, [], cohort, subject)
        players = {}
        for player in (subject, *references):
            group = [a for a in valid if a['spec']['player'] == player]
            if group:
                players[player] = player_summary(group)
        output.append({'scenario': scenario, 'mode': mode, 'reference_player': references[0] if references else None,
                       'resource_verdict': verdict,
                       'gpu_gfx_verdict': gpu_verdict, 'players': players, 'equal_quality': 'NOT ESTABLISHED',
                       'invalid_attempts': [a['failures'] for a in cohort if a['status'] == 'INVALID'],
                       'subject': subject, 'window_mode': cohort_window_mode(cohort), 'pairs': pairs})
    return {'protocol_version': PROTOCOL_VERSION, 'cohorts': output, 'quality_gates': quality_gates(attempts)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    write_json(args.root / 'summary.json', analyze(args.root))


if __name__ == '__main__':
    main()

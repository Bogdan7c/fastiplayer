"""Парные сравнения одной конфигурации; качество и CPU-решение остаются разными полями.

Протокол v2 (README): решение по CPU принимается только по quiet-парам (3/6/9),
diagnostic — одна пара на сцену для quality-gate и описательного observer-control.
GPU — отдельный парный вывод по занятости gfx-движка, не смешивается с CPU.
"""

import argparse
import json
import math
from pathlib import Path
import statistics

from host import PROTOCOL_VERSION, write_json

LOOKS = (3, 6, 9)
# Критерий выхода сессии 06: INCONCLUSIVE засчитывается, только если весь интервал
# разности Fastiplayer − VLC лежит внутри ±0,5 п.п. Это не PARITY (PARITY без допуска
# владельца не выдаётся), а отдельная пометка рядом со статусом.
EXIT_BAND_PERCENTAGE_POINTS = 0.5
# Решение владельца (сессия 06): «кадры не потеряны», если уникальных идентичностей
# кадров за окно не меньше 99 % от частоты источника × длительность окна.
QUALITY_UNIQUE_FRAME_RATIO = 0.99
VALID = 'VALID_RESOURCE_OBSERVATION'


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


def exit_band(verdict):
    """Проходит ли вердикт критерий выхода: WIN или INCONCLUSIVE целиком внутри ±0,5 п.п."""
    if verdict['status'] == 'WIN':
        return True
    if verdict['status'] != 'INCONCLUSIVE':
        return False
    low, high = verdict['interval']
    return -EXIT_BAND_PERCENTAGE_POINTS <= low and high <= EXIT_BAND_PERCENTAGE_POINTS


def gfx_percent(attempt):
    """Занятость gfx-движка деревом процессов плеера, % (нет записи — None)."""
    return attempt.get('gpu', {}).get('engine_busy_percent', {}).get('gfx')


def attempt_quality(attempt):
    """Quality-gate одной попытки; для Fastiplayer осмыслен только diagnostic-режим.

    Fastiplayer: уникальные (PTS, render generation, decoded generation) за окно
    журнала против частоты источника × длительность окна. VLC: прирост
    ``frames_lost`` за окно обязан быть 0 в любом режиме. Попытка без нужных
    данных возвращает ``NOT TESTED`` с причиной, а не PASS.
    """
    spec = attempt['spec']
    quality = attempt.get('evidence', {}).get('quality', {})
    if spec['player'] == 'fastiplayer':
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
    if spec['player'] == 'mpv':
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


def quality_gates(attempts):
    """Итог quality-gate по сцене: FAIL, если провалилась хоть одна попытка.

    NOT TESTED, если у сцены нет ни одной diagnostic-попытки Fastiplayer: без
    подсчёта кадров «не потеряли кадры» не доказано, даже если CPU измерен.
    """
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
    output = {}
    for scenario, results in sorted(scenes.items()):
        statuses = {r['status'] for r in results}
        has_fastiplayer_count = any(r['player'] == 'fastiplayer' and r['status'] != 'NOT TESTED' for r in results)
        status = ('FAIL' if 'FAIL' in statuses else
                  'PASS' if has_fastiplayer_count and statuses == {'PASS'} else 'NOT TESTED')
        output[scenario] = {'status': status, 'attempts': results}
    return output


def cohort_verdicts(mode, pairs, cohort):
    """CPU- и GPU-вердикты одной когорты (scenario, mode)."""
    differences = [a['metrics']['tree_cpu_percent'] - b['metrics']['tree_cpu_percent'] for a, b in pairs]
    if not pairs:
        reason = ('reference player not measured for this scenario'
                  if {a['spec']['player'] for a in cohort} == {'fastiplayer'} else 'no valid pairs')
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
        cpu['within_exit_band'] = exit_band(cpu)
    gfx = [(gfx_percent(a), gfx_percent(b)) for a, b in pairs]
    if any(value is None for pair in gfx for value in pair):
        gpu = {'status': 'NOT TESTED', 'reason': 'gfx engine counter missing'}
    else:
        # Счётчик движка в наносекундах: собственного разрешения, сравнимого с шумом, нет.
        gpu = comparison([a - b for a, b in gfx], 0.0)
    return cpu, gpu


def analyze(root):
    attempts = [json.loads(path.read_text()) for path in sorted(root.glob('attempts/*/result.json'))]
    cohorts = {}
    for attempt in attempts:
        spec = attempt['spec']
        cohorts.setdefault((spec['scenario'], spec['mode']), []).append(attempt)
    output = []
    for (scenario, mode), cohort in sorted(cohorts.items()):
        valid = [a for a in cohort if a['status'] == VALID]
        by_key = {(a['spec']['repetition'], a['spec']['player']): a for a in valid}
        if len(by_key) != len(valid):
            raise ValueError('duplicate scenario/mode/repetition/player')
        repetitions = sorted({key[0] for key in by_key})
        # Эталон — единственный не-Fastiplayer плеер когорты (mpv в v2, VLC в v1).
        references = sorted({a['spec']['player'] for a in cohort} - {'fastiplayer'})
        if len(references) > 1:
            raise ValueError(f'{scenario}/{mode}: several reference players {references}')
        reference = references[0] if references else None
        pairs = [(by_key[(r, 'fastiplayer')], by_key[(r, reference)]) for r in repetitions
                 if (r, 'fastiplayer') in by_key and (r, reference) in by_key]
        verdict, gpu_verdict = cohort_verdicts(mode, pairs, cohort)
        if pairs and (len(valid) != len(cohort) or len(pairs) * 2 != len(cohort)):
            verdict = {'status': 'NOT TESTED', 'reason': 'invalid or unpaired attempts retained', 'valid_pairs': len(pairs)}
        players = {}
        for player in ('fastiplayer', *references):
            group = [a for a in valid if a['spec']['player'] == player]
            if not group:
                continue
            cpu = [a['metrics']['tree_cpu_percent'] for a in group]
            gfx = [value for value in map(gfx_percent, group) if value is not None]
            players[player] = {'n': len(cpu), 'cpu_mean': statistics.mean(cpu),
                'cpu_min': min(cpu), 'cpu_max': max(cpu),
                'cpu_stdev': statistics.stdev(cpu) if len(cpu) > 1 else None,
                'gfx_busy_percent_mean': statistics.mean(gfx) if gfx else None,
                'rss_mean_mib': statistics.mean(a['metrics']['rss_sum_kib']['mean'] for a in group) / 1024,
                'pss_mean_mib': statistics.mean(a['metrics']['pss_sum_kib']['mean'] for a in group) / 1024,
                'rss_sampled_max_mib': max(a['metrics']['rss_sum_kib']['sampled_max'] for a in group) / 1024,
                'pss_sampled_max_mib': max(a['metrics']['pss_sum_kib']['sampled_max'] for a in group) / 1024}
        output.append({'scenario': scenario, 'mode': mode, 'reference_player': reference,
                       'resource_verdict': verdict,
                       'gpu_gfx_verdict': gpu_verdict, 'players': players, 'equal_quality': 'NOT ESTABLISHED',
                       'invalid_attempts': [a['failures'] for a in cohort if a['status'] == 'INVALID']})
    return {'protocol_version': PROTOCOL_VERSION, 'cohorts': output, 'quality_gates': quality_gates(attempts)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    write_json(args.root / 'summary.json', analyze(args.root))


if __name__ == '__main__':
    main()

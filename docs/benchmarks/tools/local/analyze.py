"""Парные сравнения одной конфигурации; качество и CPU-решение остаются разными полями."""

import argparse
import json
import math
from pathlib import Path
import statistics

from host import write_json

LOOKS = (3, 6, 9)


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


def analyze(root):
    attempts = [json.loads(path.read_text()) for path in sorted(root.glob('attempts/*/result.json'))]
    cohorts = {}
    for attempt in attempts:
        spec = attempt['spec']
        cohorts.setdefault((spec['scenario'], spec['mode']), []).append(attempt)
    output = []
    for (scenario, mode), cohort in sorted(cohorts.items()):
        valid = [a for a in cohort if a['status'] == 'VALID_RESOURCE_OBSERVATION']
        by_key = {(a['spec']['repetition'], a['spec']['player']): a for a in valid}
        if len(by_key) != len(valid):
            raise ValueError('duplicate scenario/mode/repetition/player')
        repetitions = sorted({key[0] for key in by_key})
        pairs = [(by_key[(r, 'fastiplayer')], by_key[(r, 'vlc')]) for r in repetitions
                 if (r, 'fastiplayer') in by_key and (r, 'vlc') in by_key]
        differences = [a['metrics']['tree_cpu_percent'] - b['metrics']['tree_cpu_percent'] for a, b in pairs]
        # Защитный floor основного /proc счётчика, хотя tree CPU имеет microsecond units.
        resolution = max((200 / min(a['clk_tck'], b['clk_tck']) /
                          min(a['metrics']['tree_cpu_percent_window_seconds'],
                              b['metrics']['tree_cpu_percent_window_seconds']) for a, b in pairs), default=0)
        verdict = comparison(differences, resolution)
        if len(valid) != len(cohort) or len(pairs) * 2 != len(cohort):
            verdict = {'status': 'NOT TESTED', 'reason': 'invalid or unpaired attempts retained', 'valid_pairs': len(pairs)}
        players = {}
        for player in ('fastiplayer', 'vlc'):
            group = [a for a in valid if a['spec']['player'] == player]
            if not group:
                continue
            cpu = [a['metrics']['tree_cpu_percent'] for a in group]
            players[player] = {'n': len(cpu), 'cpu_mean': statistics.mean(cpu),
                'cpu_min': min(cpu), 'cpu_max': max(cpu),
                'cpu_stdev': statistics.stdev(cpu) if len(cpu) > 1 else None,
                'rss_mean_mib': statistics.mean(a['metrics']['rss_sum_kib']['mean'] for a in group) / 1024,
                'pss_mean_mib': statistics.mean(a['metrics']['pss_sum_kib']['mean'] for a in group) / 1024,
                'rss_sampled_max_mib': max(a['metrics']['rss_sum_kib']['sampled_max'] for a in group) / 1024,
                'pss_sampled_max_mib': max(a['metrics']['pss_sum_kib']['sampled_max'] for a in group) / 1024}
        output.append({'scenario': scenario, 'mode': mode, 'resource_verdict': verdict,
                       'players': players, 'equal_quality': 'NOT ESTABLISHED',
                       'invalid_attempts': [a['failures'] for a in cohort if a['status'] == 'INVALID']})
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    write_json(args.root / 'summary.json', analyze(args.root))


if __name__ == '__main__':
    main()

"""Пересчитывает описательную статистику проверенных однородных cohorts из JSON."""

import argparse
import json
import math
from pathlib import Path
import statistics


def distribution(values):
    """p95 nearest rank; при N=5 он совпадает с наблюдённым максимумом."""
    ordered = sorted(values)
    return {'n': len(values), 'p50': statistics.median(ordered),
            'p95_nearest_rank': ordered[math.ceil(0.95 * len(ordered)) - 1],
            'minimum': ordered[0], 'maximum': ordered[-1]}


def aggregate(attempts):
    cohorts = {}
    for attempt in attempts:
        if not attempt.get('eligible_for_scored_statistics', False):
            continue
        key = (attempt['scenario'], attempt['player'])
        cohorts.setdefault(key, []).append(attempt)
    result = []
    for (scenario, player), observations in sorted(cohorts.items()):
        if len(observations) != 5:
            raise ValueError('published cohort must contain exactly five validated attempts')
        result.append({
            'scenario': scenario, 'player': player,
            'cpu_percent_one_logical_core': distribution([item['cpu_percent_one_logical_core'] for item in observations]),
            'rss_kib_sample_mean': distribution([item['rss_kib_sample_mean'] for item in observations]),
        })
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('measurements', type=Path)
    arguments = parser.parse_args()
    manifest = json.loads(arguments.measurements.read_text())
    print(json.dumps(aggregate(manifest['scored_attempts']), indent=2))


if __name__ == '__main__':
    main()

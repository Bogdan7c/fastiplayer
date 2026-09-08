#!/usr/bin/env python3
"""Сравнение двух реально существующих Fastiplayer snapshots без подмены historical baseline."""

import argparse
import json
from pathlib import Path

from analyze import comparison
from host import write_json


def compare(before_root, current_root):
    def read_attempts(root):
        attempts = [json.loads(path.read_text()) for path in sorted(root.glob('attempts/*/result.json'))]
        return [a for a in attempts if a['spec']['player'] == 'fastiplayer']

    observers = [json.loads((root / 'tool-provenance.json').read_text())['tool_sha256']
                 for root in (before_root, current_root)]
    if observers[0] != observers[1]:
        raise ValueError('observer tools differ between snapshots')
    before, current = read_attempts(before_root), read_attempts(current_root)
    if not before or not current:
        raise ValueError('both snapshots require actual Fastiplayer attempts')
    output = []
    for scenario, mode in sorted({(a['spec']['scenario'], a['spec']['mode']) for a in before + current}):
        groups = [[a for a in attempts if (a['spec']['scenario'], a['spec']['mode']) == (scenario, mode)]
                  for attempts in (before, current)]
        if not all(groups) or any(a['status'] != 'VALID_RESOURCE_OBSERVATION' for group in groups for a in group):
            output.append({'scenario': scenario, 'mode': mode, 'status': 'NOT TESTED'})
            continue
        indexed = [{a['spec']['repetition']: a for a in group} for group in groups]
        if any(len(index) != len(group) for index, group in zip(indexed, groups)):
            raise ValueError('duplicate snapshot repetition')
        if indexed[0].keys() != indexed[1].keys():
            output.append({'scenario': scenario, 'mode': mode, 'status': 'INCONCLUSIVE',
                           'reason': 'different repetition sets; do not cherry-pick favorable matching runs'})
            continue
        pairs = [(indexed[0][r], indexed[1][r]) for r in sorted(indexed[0])]
        # Ни mode, ни hash media/config, ни окно нельзя незаметно менять между состояниями.
        same_conditions = ('media_sha256', 'config_sha256', 'warmup_seconds', 'duration_seconds',
                           'interval_seconds', 'display_size', 'window_helper_sha256', 'codec')
        for a, b in pairs:
            if any(a['spec'][key] != b['spec'][key] for key in same_conditions):
                raise ValueError(f'incompatible protocol for {scenario}/{mode}')
        first = pairs[0][0]
        resolution = 200 / first['clk_tck'] / first['spec']['duration_seconds']
        cpu = comparison([b['metrics']['tree_cpu_percent'] - a['metrics']['tree_cpu_percent'] for a, b in pairs], resolution)
        memory = {}
        for metric in ('rss_sum_kib', 'pss_sum_kib'):
            for statistic in ('mean', 'sampled_max'):
                memory[f'{metric}_{statistic}'] = comparison([
                    b['metrics'][metric][statistic] - a['metrics'][metric][statistic] for a, b in pairs], 1, unit='KiB')
        output.append({'scenario': scenario, 'mode': mode,
                       'before_snapshot': first['spec']['snapshot'],
                       'current_snapshot': pairs[0][1]['spec']['snapshot'],
                       'cpu': cpu, 'memory_kib_differences': memory,
                       'quality_and_host_equivalence': 'MANUAL REVIEW REQUIRED',
                       'memory_rule': 'positive systematic growth requires owner decision; uncertainty is not approval'})
    return {'before_root': str(before_root.resolve()), 'current_root': str(current_root.resolve()), 'comparisons': output}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('before', type=Path)
    parser.add_argument('current', type=Path)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    write_json(args.output, compare(args.before, args.current))


if __name__ == '__main__':
    main()

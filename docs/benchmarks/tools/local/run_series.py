#!/usr/bin/env python3
"""Последовательная возобновляемая серия; protocol hash фиксируется до первого attempt."""

import argparse
import json
from pathlib import Path
import subprocess
import sys

from analyze import LOOKS, analyze
from host import sha256, write_json


def run_series(plan_path, root):
    plan_path, root = plan_path.resolve(), root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    lock = root / 'plan.json'
    plan = json.loads(plan_path.read_text())
    if lock.exists() and json.loads(lock.read_text()) != plan:
        raise ValueError('plan changed: use a new series directory')
    if not lock.exists():
        write_json(lock, plan)
    tooling = Path(__file__).resolve().parent
    source_hashes = {str(path.relative_to(tooling.parent)): sha256(path)
                     for path in sorted([*tooling.glob('*.py'), tooling / 'window_owner.c',
                                         tooling.parent / 't480s/collect.py', tooling.parent / 't480s/fullscreen.py'])}
    provenance = {'plan_sha256': sha256(lock), 'tool_sha256': source_hashes}
    tool_lock = root / 'tool-provenance.json'
    if tool_lock.exists() and json.loads(tool_lock.read_text()) != provenance:
        raise ValueError('collector changed: use a new series directory')
    write_json(tool_lock, provenance)
    (root / 'attempts').mkdir(exist_ok=True)
    (root / 'specs').mkdir(exist_ok=True)
    stopped = set()
    for repetition in range(1, LOOKS[-1] + 1):
        for scenario in plan['scenarios']:
            # AB/BA чередуется и для player, и для diagnostic/quiet, без параллельного playback.
            for mode in (('quiet', 'diagnostic') if repetition % 2 else ('diagnostic', 'quiet')):
                key = (scenario['scenario'], mode)
                if key in stopped:
                    continue
                for player in (('fastiplayer', 'vlc') if repetition % 2 else ('vlc', 'fastiplayer')):
                    label = f'{scenario["scenario"]}-{mode}-{repetition:02}-{player}'
                    spec = {**plan['common'], **scenario, **plan['players'][player],
                            'player': player, 'mode': mode, 'repetition': repetition}
                    spec_path = root / 'specs' / f'{label}.json'
                    write_json(spec_path, spec)
                    attempt = root / 'attempts' / label
                    if attempt.exists():
                        if not (attempt / 'result.json').exists():
                            raise ValueError(f'interrupted attempt requires review: {label}')
                        continue
                    with (root / 'runner.log').open('a') as log:
                        completed = subprocess.run([sys.executable, str(tooling / 'collect.py'),
                                                    str(spec_path), str(attempt)], stdout=log, stderr=subprocess.STDOUT)
                    print(label, 'exit', completed.returncode, flush=True)
                    if completed.returncode:
                        # Не удаляем/не заменяем неудачный запуск и не тратим полный
                        # corpus на уже доказанно сломанный путь измерения.
                        write_json(root / 'summary.json', analyze(root))
                        return 1
        if repetition in LOOKS:
            summary = analyze(root)
            write_json(root / 'summary.json', summary)
            for cohort in summary:
                if cohort['resource_verdict']['status'] in {'WIN', 'WORSE'}:
                    stopped.add((cohort['scenario'], cohort['mode']))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('plan', type=Path)
    parser.add_argument('root', type=Path)
    args = parser.parse_args()
    return run_series(args.plan, args.root)


if __name__ == '__main__':
    raise SystemExit(main())

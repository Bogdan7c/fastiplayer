#!/usr/bin/env python3
"""Реальный render smoke и отрицательная проверка software вместо требуемого hardware."""

import argparse
import json
from pathlib import Path

from collect import collect
from host import sha256, write_json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('fastiplayer_spec', type=Path)
    parser.add_argument('directory', type=Path)
    args = parser.parse_args()
    args.directory.mkdir(parents=True, exist_ok=False)
    spec = json.loads(args.fastiplayer_spec.read_text())
    spec.update(player='fastiplayer', mode='diagnostic', duration_seconds=4, capture=True)
    positive = collect(spec, args.directory / 'hardware')
    if positive['status'] != 'VALID_RESOURCE_OBSERVATION':
        raise RuntimeError(f'hardware smoke failed: {positive["failures"]}')
    if positive['evidence']['quality']['unique_identities'] < 2:
        raise RuntimeError('render output did not advance')
    if sha256(args.directory / 'hardware/render-before.png') == sha256(args.directory / 'hardware/render-after.png'):
        raise RuntimeError('window images did not change')
    config = Path(spec['config']).read_text()
    expected = 'preferred_backend = "hardware"'
    if config.count(expected) != 1:
        raise ValueError('smoke requires an explicit hardware config')
    wrong_config = args.directory.resolve() / 'software.toml'
    wrong_config.write_text(config.replace(expected, 'preferred_backend = "software"'))
    wrong = {**spec, 'config': str(wrong_config), 'config_sha256': sha256(wrong_config)}
    negative = collect(wrong, args.directory / 'wrong-backend')
    if negative['status'] != 'INVALID' or 'expected hardware backend not proven' not in negative['failures']:
        raise RuntimeError('software playback was not explicitly rejected as wrong backend')
    write_json(args.directory / 'smoke.json', {'status': 'PASS', 'hardware_render_advances': True,
                                              'wrong_backend_rejected': True})
    print('PASS: hardware reaches changing render output; software is explicitly unscored')


if __name__ == '__main__':
    main()

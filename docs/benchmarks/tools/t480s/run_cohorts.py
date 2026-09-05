"""Одинаковые серии с чередованием плееров; существующие attempts сохраняются."""

import argparse
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--phase', choices=['warmup', 'measurement'], required=True)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--fixtures', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    arguments = parser.parse_args()
    count = 3 if arguments.phase == 'warmup' else 5
    runner = Path(__file__).with_name('run_series.py')
    for scenario in ['h264-1080p60-hw', 'hevc-4k60-hw', 'av1-4k60-sw']:
        for attempt in range(1, count + 1):
            players = ['rustiplayer', 'vlc'] if attempt % 2 else ['vlc', 'rustiplayer']
            for player in players:
                # AC проверяется перед каждым scored attempt, чтобы смена
                # питания не становилась неявным условием следующего sample.
                if arguments.phase == 'measurement' and Path('/sys/class/power_supply/AC/online').read_text().strip() != '1':
                    raise RuntimeError('AC power required for qualified measurement cohort')
                subprocess.run([
                    sys.executable, str(runner), '--preload-fixture',
                    '--phase', arguments.phase, '--scenario', scenario,
                    '--player', player, '--count', '1', '--first-attempt', str(attempt),
                    '--directory', str(arguments.directory), '--fixtures', str(arguments.fixtures),
                    '--binary', str(arguments.binary), '--window-mode', 'xwayland-fullscreen',
                ], check=True)


if __name__ == '__main__':
    main()

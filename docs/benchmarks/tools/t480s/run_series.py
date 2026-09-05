"""Последовательные S08 attempts с отдельным состоянием каждого процесса."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def run_series(arguments):
    scenario_files = {
        'h264-1080p60-hw': 'synthetic-h264-1080p60.mp4',
        'hevc-4k60-hw': 'synthetic-hevc-4k60.mp4',
        'av1-4k60-sw': 'big-buck-bunny-av1-4k60.mp4',
    }
    tooling = Path(__file__).resolve().parent
    software = arguments.scenario == 'av1-4k60-sw'
    template = tooling / ('software-template.toml' if software else 'hardware-template.toml')
    for attempt in range(arguments.first_attempt, arguments.first_attempt + arguments.count):
        label = f'{arguments.phase}-{arguments.scenario}-{arguments.player}-{attempt:02}'
        attempt_root = arguments.directory / label
        attempt_root.mkdir(parents=True, exist_ok=False)
        environment = os.environ.copy()
        # Сохраняем desktop session; XWayland выбирается явно. Каждая попытка
        # получает отдельные config/cache, без личного resume и media library.
        if arguments.window_mode.startswith('xwayland'):
            environment.pop('WAYLAND_DISPLAY', None)
            environment.pop('WAYLAND_SOCKET', None)
        environment['XDG_CONFIG_HOME'] = str(attempt_root / 'config')
        environment['XDG_DATA_HOME'] = str(attempt_root / 'data')
        environment['XDG_CACHE_HOME'] = str(attempt_root / 'cache')
        environment['NO_COLOR'] = '1'
        environment['RUST_LOG'] = 'info,rustiplayer::video_render_acceptance=trace'
        fixture = arguments.fixtures / scenario_files[arguments.scenario]
        if arguments.preload_fixture:
            # Одинаковое последовательное чтение задаёт warm page-cache policy
            # до spawn, вне CPU/RSS окна измеряемого player-а.
            with fixture.open('rb') as media:
                while media.read(1024 * 1024):
                    pass
        collector = [
            sys.executable, str(tooling / 'collect.py'),
            '--attempt', str(attempt), '--phase', arguments.phase,
            '--scenario', arguments.scenario, '--player', arguments.player,
            '--window-mode', arguments.window_mode,
            '--settle', '3' if arguments.phase == 'warmup' else '15',
            '--window-start', '10' if arguments.phase == 'warmup' else '20',
            '--duration', '5' if arguments.phase == 'warmup' else '60',
            '--output', str(attempt_root / 'measurement.json'),
            '--log', str(attempt_root / 'runtime.log'),
            '--capture-prefix', str(attempt_root / label),
        ]
        if arguments.window_mode == 'xwayland-fullscreen':
            collector.extend(['--fullscreen-controller', str(tooling / 'fullscreen.py'),
                              '--expected-capture-size', *map(str, arguments.display_size)])
        if arguments.player == 'rustiplayer':
            collector.append('--require-rust-startup')
            config_dir = attempt_root / 'config' / 'rustiplayer'
            config_dir.mkdir(parents=True)
            shutil.copyfile(template, config_dir / 'config.toml')
            command = [str(arguments.binary), str(fixture)]
        else:
            control_socket = attempt_root / 'control.sock'
            collector.extend(['--vlc-socket', str(control_socket)])
            command = [
                'vlc', '--ignore-config', '--no-one-instance', '--intf', 'dummy',
                '--extraintf', 'oldrc', '--rc-fake-tty', f'--rc-unix={control_socket}',
                '--avcodec-hw=none' if software else '--avcodec-hw=vaapi',
                '--vout=gl', '--no-video-title-show', '--gain=0',
                '-vv', str(fixture),
            ]
            if arguments.window_mode == 'xwayland-fullscreen':
                command.insert(-1, '--fullscreen')
            else:
                command[1:1] = ['--width=1280', '--height=720', '--zoom=0.6666667' if arguments.scenario == 'h264-1080p60-hw' else '--zoom=0.3333333']
        completed = subprocess.run(collector + ['--'] + command, env=environment)
        observation = json.loads((attempt_root / 'measurement.json').read_text())
        print(label, observation['status'], flush=True)
        if completed.returncode != 0:
            # Сбой сохраняется; следующая попытка требует выяснения причины.
            raise SystemExit(completed.returncode)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--preload-fixture', action='store_true')
    parser.add_argument('--phase', choices=['warmup', 'measurement'], required=True)
    parser.add_argument('--scenario', choices=['h264-1080p60-hw', 'hevc-4k60-hw', 'av1-4k60-sw'], required=True)
    parser.add_argument('--player', choices=['rustiplayer', 'vlc'], required=True)
    parser.add_argument('--count', type=int, required=True)
    parser.add_argument('--display-size', type=int, nargs=2, default=[1920, 1080])
    parser.add_argument('--first-attempt', type=int, default=1)
    parser.add_argument('--directory', type=Path, required=True)
    parser.add_argument('--fixtures', type=Path, required=True)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--window-mode', choices=['native-window', 'xwayland-window', 'xwayland-fullscreen'], required=True)
    arguments = parser.parse_args()
    arguments.directory = arguments.directory.resolve()
    arguments.fixtures = arguments.fixtures.resolve()
    arguments.binary = arguments.binary.resolve()
    if arguments.count < 1 or arguments.first_attempt < 1 or min(arguments.display_size) < 1:
        parser.error('count, first-attempt and display dimensions must be positive')
    run_series(arguments)


if __name__ == '__main__':
    main()

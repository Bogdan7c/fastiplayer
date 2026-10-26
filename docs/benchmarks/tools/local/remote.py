#!/usr/bin/env python3
"""Запуск серии протокола v4 на T410 с основной машины по ssh: push / start / status / fetch.

Тонкая обёртка: протокол не переписывает, а только доставляет инструменты, бинарь и план
на целевую машину, запускает там ``run_series.py`` и забирает результаты. Сам замер идёт
НА T410 (клиентская ssh-сессия в нём не участвует и может оборваться).

На T410 логин-оболочка — fish, поэтому всё удалённое выполняется через ``bash -c '…'``
(скрипт целиком — один аргумент ssh, закавыченный для fish). Серия стартует юнитом
``systemd-run --user``: он переживает обрыв ssh, а окружение живой графической сессии
(Wayland, X, D-Bus) передаётся явно — из ssh-сессии оно недоступно.

``--dry-run`` печатает команды и ничего не запускает.
"""

import argparse
from pathlib import Path
import re
import shlex
import subprocess
import sys

REPO_ROOT = Path(__file__).resolve().parents[4]
LOCAL_TOOLS = Path(__file__).resolve().parent
T480S_TOOLS = LOCAL_TOOLS.parent / 't480s'

# ssh-алиас целевой машины (~/.ssh/config) и каталог на ней (относительно $HOME, либо абсолютный).
DEFAULT_ALIAS = 't410'
DEFAULT_REMOTE_DIR = 'fastiplayer-bench'
# Окружение живой сессии KDE на T410 (uid 1000). ``XAUTHORITY`` здесь НЕТ: имя файла
# ``xauth_*`` меняется после перезагрузки, его ищет сам скрипт старта.
SESSION_RUNTIME_DIR = '/run/user/1000'
SESSION_ENVIRONMENT = {
    'XDG_RUNTIME_DIR': SESSION_RUNTIME_DIR,
    'DBUS_SESSION_BUS_ADDRESS': f'unix:path={SESSION_RUNTIME_DIR}/bus',
    'WAYLAND_DISPLAY': 'wayland-1',
    'DISPLAY': ':0',
}
XAUTHORITY_GLOB = f'{SESSION_RUNTIME_DIR}/xauth_*'
UNIT_PREFIX = 'fastiplayer-series-'
SERIES_NAME = re.compile(r'[a-z0-9-]+')
# Каталоги внутри удалённого каталога. ``tools/local`` и ``tools/t480s`` — соседи, как в
# репозитории: ``evidence.py`` находит t480s по относительному пути.
REMOTE_LAYOUT = {'local_tools': 'tools/local', 't480s_tools': 'tools/t480s', 'release': 'release',
                 'configs': 'configs', 'plans': 'plans', 'series': 'series'}
RSYNC_EXCLUDES = ('--exclude=__pycache__', '--exclude=*.pyc')


class RemoteTarget:
    """Куда и как: алиас ssh, удалённый каталог, имена юнитов (без размазанных строк)."""

    def __init__(self, alias=DEFAULT_ALIAS, remote_dir=DEFAULT_REMOTE_DIR):
        self.alias = alias
        self.remote_dir = remote_dir.rstrip('/')

    def rsync_path(self, name):
        """Путь для rsync (``alias:путь``): относительный считается от $HOME удалённого пользователя."""
        return f'{self.alias}:{self.remote_dir}/{REMOTE_LAYOUT[name]}/'

    def shell_path(self, *parts):
        """Путь внутри bash-скрипта: ``$HOME/…`` для относительного каталога (раскрывает bash)."""
        base = self.remote_dir if self.remote_dir.startswith('/') else f'$HOME/{self.remote_dir}'
        return '/'.join([base, *parts])

    def series_dir(self, series):
        return self.shell_path(REMOTE_LAYOUT['series'], self.checked_series(series))

    @staticmethod
    def checked_series(series):
        """Имя серии попадает в пути и имя юнита: только ``[a-z0-9-]+`` (никакой оболочки в именах)."""
        if not SERIES_NAME.fullmatch(series):
            raise ValueError(f'series name {series!r} must match [a-z0-9-]+')
        return series

    @classmethod
    def unit(cls, series):
        return f'{UNIT_PREFIX}{cls.checked_series(series)}.service'


def remote_bash(target, script):
    """ssh-команда с исполнением скрипта через ``bash -c`` (логин-оболочка T410 — fish)."""
    return ['ssh', target.alias, 'bash -c ' + shlex.quote(script)]


def rsync(source, destination, *flags):
    return ['rsync', '-a', *RSYNC_EXCLUDES, *flags, source, destination]


def push_commands(target, plan, configs, release_dir):
    """Команды доставки: каталоги, инструменты, бинарь с ``lib/``, конфиги, план, сборка window-owner."""
    release_dir = Path(release_dir)
    layout = [target.shell_path(REMOTE_LAYOUT[name]) for name in REMOTE_LAYOUT]
    commands = [remote_bash(target, 'mkdir -p ' + ' '.join(f'"{path}"' for path in layout)),
                rsync(f'{LOCAL_TOOLS}/', target.rsync_path('local_tools'), '--delete'),
                # collect.py/fullscreen.py из t480s импортирует evidence.py и хеширует run_series.py.
                rsync(f'{T480S_TOOLS}/', target.rsync_path('t480s_tools'), '--delete'),
                rsync(str(release_dir / 'fastiplayer'), target.rsync_path('release')),
                rsync(f'{release_dir / "lib"}/', f'{target.rsync_path("release")}lib/', '--delete')]
    if configs:
        commands.append(rsync_files(configs, target.rsync_path('configs')))
    commands.append(rsync_files([plan], target.rsync_path('plans')))
    build = (f'cd "{target.shell_path(REMOTE_LAYOUT["local_tools"])}" && '
             'cc -Wall -Wextra -Werror -O2 window_owner.c -lxcb -lxcb-res -o window-owner')
    commands.append(remote_bash(target, build))
    return commands


def rsync_files(files, destination):
    return ['rsync', '-a', *map(str, files), destination]


def start_script(target, plan_name, series):
    """Скрипт старта: ищет ``XAUTHORITY``, запускает ``run_series.py`` юнитом, переживающим ssh."""
    unit = target.unit(series)
    series_dir = target.series_dir(series)
    environment = ' '.join(f'--setenv={key}={shlex.quote(value)}' for key, value in SESSION_ENVIRONMENT.items())
    if not re.fullmatch(r'[A-Za-z0-9._-]+', plan_name):
        raise ValueError(f'plan file name {plan_name!r} must be a plain file name')
    plan = target.shell_path(REMOTE_LAYOUT['plans'], plan_name)
    runner = target.shell_path(REMOTE_LAYOUT['local_tools'], 'run_series.py')
    return '\n'.join([
        'set -e',
        f'mkdir -p "{series_dir}"',
        # Имя файла меняется после перезагрузки: ищем при старте, не храним.
        f'xauthority=$(ls {XAUTHORITY_GLOB} 2>/dev/null | head -n 1)',
        '[ -n "$xauthority" ] || { echo "no X authority file found" >&2; exit 1; }',
        f'systemd-run --user --unit={unit} --collect --working-directory="{series_dir}" '
        f'{environment} --setenv=XAUTHORITY="$xauthority" '
        f'--property=StandardOutput=append:"{series_dir}/series.out" '
        f'--property=StandardError=append:"{series_dir}/series.out" '
        f'python3 "{runner}" "{plan}" "{series_dir}/run"'])


def start_commands(target, plan_name, series):
    return [remote_bash(target, start_script(target, plan_name, series))]


def status_commands(target, series):
    unit = target.unit(series)
    series_dir = target.series_dir(series)
    script = (f'systemctl --user is-active {unit} || true; '
              f'tail -n 20 "{series_dir}/run/runner.log" 2>/dev/null || true; '
              f'tail -n 5 "{series_dir}/series.out" 2>/dev/null || true')
    return [remote_bash(target, script)]


def fetch_commands(target, series, destination):
    series = target.checked_series(series)
    # ``--mkpath``: rsync сам создаёт недостающие родительские каталоги назначения.
    return [rsync(f'{target.alias}:{target.remote_dir}/{REMOTE_LAYOUT["series"]}/{series}/', f'{destination}/',
                  '--mkpath')]


def execute(commands, dry_run):
    """Печатает команды; без ``--dry-run`` выполняет по очереди, останавливаясь на первой ошибке."""
    for command in commands:
        print(shlex.join(command), flush=True)
        if not dry_run:
            subprocess.run(command, check=True)


def build_parser():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--alias', default=DEFAULT_ALIAS, help='ssh alias of the target machine')
    parser.add_argument('--remote-dir', default=DEFAULT_REMOTE_DIR,
                        help='directory on the target (relative to $HOME, or absolute)')
    parser.add_argument('--dry-run', action='store_true', help='print the commands, run nothing')
    sub = parser.add_subparsers(dest='action', required=True)
    push = sub.add_parser('push', help='deliver tools, binary, configs and the plan')
    push.add_argument('plan', type=Path)
    push.add_argument('--config', type=Path, action='append', default=[], help='config file (repeatable)')
    push.add_argument('--release-dir', type=Path, default=REPO_ROOT / 'target/release')
    start = sub.add_parser('start', help='start the series on the target, surviving an ssh drop')
    start.add_argument('plan_name', help='plan file name as pushed (inside <remote-dir>/plans)')
    start.add_argument('series')
    status = sub.add_parser('status', help='unit state and the tail of runner.log')
    status.add_argument('series')
    fetch = sub.add_parser('fetch', help='copy the series directory back')
    fetch.add_argument('series')
    fetch.add_argument('destination', type=Path)
    return parser


def commands_for(arguments):
    target = RemoteTarget(arguments.alias, arguments.remote_dir)
    if arguments.action == 'push':
        return push_commands(target, arguments.plan, arguments.config, arguments.release_dir)
    if arguments.action == 'start':
        return start_commands(target, arguments.plan_name, arguments.series)
    if arguments.action == 'status':
        return status_commands(target, arguments.series)
    return fetch_commands(target, arguments.series, arguments.destination)


def main(argv=None):
    arguments = build_parser().parse_args(argv)
    try:
        execute(commands_for(arguments), arguments.dry_run)
    except (ValueError, subprocess.CalledProcessError) as error:
        print(f'remote {arguments.action} failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())

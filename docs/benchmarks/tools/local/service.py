"""Владелец ровно одной transient user service и всех её потомков."""

import os
from pathlib import Path
import uuid
import time

from host import command_output


class MeasuredService:
    def __init__(self, command, environment, directory):
        self.command = command
        self.environment = environment
        self.directory = directory.resolve()
        self.unit = f'fastiplayer-measure-{uuid.uuid4().hex}.service'
        self.started = False
        self.pid = None
        self.cgroup = None

    def state(self):
        output = command_output(['systemctl', '--user', 'show', self.unit,
                                 '--property=MainPID,ControlGroup,Result,ExecMainCode,ExecMainStatus,SubState'])
        return dict(line.split('=', 1) for line in output.splitlines())

    def start(self):
        # env -i предотвращает незаметное наследование другого RUST_LOG/backend
        # из окружения user manager. Параметры не проходят через shell.
        invocation = ['systemd-run', '--user', f'--unit={self.unit}', '--service-type=exec',
                      '--remain-after-exit', '--property=TimeoutStopSec=5s',
                      '--property=KillMode=control-group',
                      f'--property=StandardOutput=append:{self.directory / "runtime.log"}',
                      f'--property=StandardError=append:{self.directory / "runtime.log"}',
                      '--', '/usr/bin/env', '-i',
                      *[f'{key}={value}' for key, value in self.environment.items()], *self.command]
        # Даже неудачный exec может оставить failed unit: cleanup нужен с начала попытки.
        self.started = True
        command_output(invocation)
        state = self.state()
        self.pid = int(state['MainPID'])
        if self.pid <= 0:
            raise RuntimeError(f'early exit: {state}')
        self.cgroup = Path('/sys/fs/cgroup') / state['ControlGroup'].lstrip('/')
        actual = Path(f'/proc/{self.pid}/cgroup').read_text().strip()
        if actual != '0::' + state['ControlGroup']:
            raise RuntimeError('unexpected process cgroup')

    def stop(self):
        if not self.started:
            return None
        before = self.state()
        forced = False
        if int(before['MainPID']) > 0:
            command_output(['systemctl', '--user', 'kill', '--signal=TERM', self.unit])
            deadline = time.monotonic() + 5
            while int(self.state()['MainPID']) > 0 and time.monotonic() < deadline:
                time.sleep(0.05)
            if int(self.state()['MainPID']) > 0:
                forced = True
                command_output(['systemctl', '--user', 'kill', '--signal=KILL', self.unit])
                time.sleep(0.1)
        ended = self.state()
        command_output(['systemctl', '--user', 'stop', self.unit])
        if ended['Result'] != 'success':
            command_output(['systemctl', '--user', 'reset-failed', self.unit])
        self.started = False
        return {'before_cleanup': before, 'after_signal': ended, 'forced_kill': forced}



def isolated_environment(directory, rust_log):
    keys = ('HOME', 'USER', 'LOGNAME', 'PATH', 'DISPLAY', 'XAUTHORITY',
            'XDG_RUNTIME_DIR', 'DBUS_SESSION_BUS_ADDRESS', 'LANG', 'LC_ALL',
            'PULSE_SERVER', 'PIPEWIRE_REMOTE')
    environment = {key: os.environ[key] for key in keys if key in os.environ}
    for name, suffix in [('CONFIG', 'config'), ('DATA', 'data'), ('CACHE', 'cache'), ('STATE', 'state')]:
        path = directory.resolve() / suffix
        path.mkdir(parents=True)
        environment[f'XDG_{name}_HOME'] = str(path)
    environment.update(NO_COLOR='1', RUST_LOG=rust_log)
    return environment

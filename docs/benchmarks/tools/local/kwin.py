"""Окна KWin по PID: доказательство окна нативного Wayland-клиента (сессия 10 gles2-renderer).

Под XWayland окно доказывают xprop и XRes (``evidence.window_proof``). У нативного
Wayland-клиента X11-окна нет, а протокол Wayland не даёт постороннему процессу читать
чужие окна. Источник правды — сам композитор: KWin-скрипт перебирает
``workspace.windowList()`` и печатает окна нужного PID одной JSON-строкой в журнал
KWin; мы находим строку по одноразовой метке. Метка нужна, потому что журнал общий:
старые строки того же PID от прошлых попыток не должны засчитаться.
"""

import json
from pathlib import Path
import subprocess
import tempfile
import time
import uuid

# Пользовательский unit KWin в сессии Plasma 6 (туда идёт ``print`` из KWin-скриптов).
KWIN_JOURNAL_UNIT = 'plasma-kwin_wayland.service'
# Сколько ждать строку скрипта в журнале: journald пишет асинхронно, обычно < 0,2 с.
JOURNAL_WAIT_SECONDS = 3.0
JOURNAL_POLL_SECONDS = 0.1


def run_script(source, plugin):
    """Загружает, запускает и выгружает KWin-скрипт (как ``t480s/fullscreen.py``).

    Ошибка qdbus6 (нет KWin, нет D-Bus) пробрасывается: без композитора доказательства нет.
    """
    with tempfile.TemporaryDirectory(prefix='fastiplayer-kwin-') as directory:
        script = Path(directory) / f'{plugin}.js'
        script.write_text(source)
        script_id = subprocess.check_output(['qdbus6', 'org.kde.KWin', '/Scripting',
                                             'org.kde.kwin.Scripting.loadScript', str(script), plugin],
                                            text=True).strip()
        try:
            subprocess.run(['qdbus6', 'org.kde.KWin', f'/Scripting/Script{int(script_id)}',
                            'org.kde.kwin.Script.run'], check=True, stdout=subprocess.DEVNULL)
        finally:
            subprocess.run(['qdbus6', 'org.kde.KWin', '/Scripting', 'org.kde.kwin.Scripting.unloadScript', plugin],
                           check=True, stdout=subprocess.DEVNULL)


def window_list_script(pid, marker):
    """Текст KWin-скрипта: окна процесса ``pid`` → одна строка ``<marker> <JSON>``."""
    return ('const found = [];'
            ' for (const window of workspace.windowList()) {'
            f'  if (window.pid !== {int(pid)}) continue;'
            '  const geometry = window.frameGeometry;'
            '  found.push({caption: String(window.caption), resource_class: String(window.resourceClass),'
            '   fullscreen: Boolean(window.fullScreen), active: workspace.activeWindow === window,'
            '   x: geometry.x, y: geometry.y, width: geometry.width, height: geometry.height,'
            '   output: window.output ? String(window.output.name) : null});'
            ' }'
            f' print({json.dumps(marker)} + " " + JSON.stringify(found));')


def parse_marker_line(journal_text, marker):
    """JSON-список окон из строки журнала с меткой; нет строки — None (ещё не записана)."""
    for line in journal_text.splitlines():
        # KWin может добавить префикс «js:» — ищем метку в любом месте строки.
        position = line.find(marker + ' ')
        if position >= 0:
            return json.loads(line[position + len(marker) + 1:])
    return None


def read_journal(since_epoch_seconds):
    """Строки журнала KWin начиная с момента запуска скрипта (с запасом в секунду)."""
    return subprocess.check_output(['journalctl', '--user', '-u', KWIN_JOURNAL_UNIT, '-o', 'cat',
                                    f'--since=@{int(since_epoch_seconds) - 1}'], text=True)


def windows_of(pid):
    """Окна KWin процесса ``pid`` (список словарей из :func:`window_list_script`).

    Если строка скрипта так и не появилась в журнале, это ошибка, а не «окон нет»:
    пустой ответ и потерянный ответ — разные состояния.
    """
    marker = f'fastiplayer-window-proof-{uuid.uuid4().hex}'
    started = time.time()
    run_script(window_list_script(pid, marker), marker)
    deadline = time.monotonic() + JOURNAL_WAIT_SECONDS
    while True:
        windows = parse_marker_line(read_journal(started), marker)
        if windows is not None:
            return windows
        if time.monotonic() >= deadline:
            raise RuntimeError(f'KWin script output not found in {KWIN_JOURNAL_UNIT} journal')
        time.sleep(JOURNAL_POLL_SECONDS)

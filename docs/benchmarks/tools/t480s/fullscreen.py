"""Однократно переводит только окно заданного тестового PID в fullscreen."""

from pathlib import Path
import subprocess
import sys
import tempfile

pid = int(sys.argv[1])
plugin = f'rustiplayer-s08-{pid}'
with tempfile.TemporaryDirectory(prefix='rustiplayer-s08-kwin-') as directory:
    script = Path(directory) / 'fullscreen.js'
    # PID ограничивает изменение окном собственного запущенного процесса.
    script.write_text(
        'for (const window of workspace.windowList()) {'
        f' if (window.pid === {pid}) {{'
        ' window.fullScreen = true; workspace.activeWindow = window;'
        ' }}'
    )
    script_id = subprocess.check_output([
        'qdbus6', 'org.kde.KWin', '/Scripting',
        'org.kde.kwin.Scripting.loadScript', str(script), plugin,
    ], text=True).strip()
    try:
        subprocess.run([
            'qdbus6', 'org.kde.KWin', f'/Scripting/Script{int(script_id)}',
            'org.kde.kwin.Script.run',
        ], check=True)
    finally:
        subprocess.run([
            'qdbus6', 'org.kde.KWin', '/Scripting',
            'org.kde.kwin.Scripting.unloadScript', plugin,
        ], check=True, stdout=subprocess.DEVNULL)

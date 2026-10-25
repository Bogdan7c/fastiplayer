"""Доказательства плееров: backend, файл, продвижение и отдельные quality counters."""

import importlib.util
import json
from pathlib import Path
import re
import sys
from types import SimpleNamespace
import time

from host import command_output
import kwin

T480S = Path(__file__).resolve().parents[1] / 't480s'
_spec = importlib.util.spec_from_file_location('t480s_collector', T480S / 'collect.py')
legacy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(legacy)


def fullscreen(pid):
    command_output([sys.executable, str(T480S / 'fullscreen.py'), str(pid)])


def window_proof(pid, window_helper):
    clients = command_output(['xprop', '-root', '_NET_CLIENT_LIST'])
    for identifier in re.findall(r'0x[0-9a-f]+', clients):
        properties = command_output(['xprop', '-id', identifier, '_NET_WM_PID', '_NET_WM_STATE'])
        owner, width, height = map(int, command_output([str(window_helper), identifier]).split())
        if owner == pid:
            if '_NET_WM_STATE_FULLSCREEN' not in properties:
                raise ValueError('measured window is not fullscreen')
            return {'window_id': identifier, 'fullscreen': True, 'width': width, 'height': height,
                    'display': command_output(['xrandr', '--current'])}
    raise ValueError('no XWayland window for measured PID')


def x11_windows_of(pid, window_helper):
    """X11-окна (id), чей владелец по XRes — ``pid``; для нативного Wayland список обязан быть пуст."""
    clients = command_output(['xprop', '-root', '_NET_CLIENT_LIST'])
    owned = []
    for identifier in re.findall(r'0x[0-9a-f]+', clients):
        owner, _, _ = map(int, command_output([str(window_helper), identifier]).split())
        if owner == pid:
            owned.append(identifier)
    return owned


def wayland_window_proof(pid, window_helper, display_size):
    """Окно нативного Wayland-клиента: одно окно KWin этого PID, fullscreen, размер экрана.

    Плюс отрицательная проверка: у PID нет ни одного X11-окна. Иначе «нативный» замер мог
    тихо уйти в XWayland (например, плеер проигнорировал WAYLAND_DISPLAY), и режимы
    окна смешались бы в одной колонке.
    """
    windows = kwin.windows_of(pid)
    if len(windows) != 1:
        raise ValueError(f'expected one KWin window for measured PID, got {len(windows)}')
    window = windows[0]
    if not window['fullscreen']:
        raise ValueError('measured window is not fullscreen')
    width, height = display_size
    if (window['width'], window['height']) != (width, height):
        raise ValueError(f'measured window is {window["width"]}x{window["height"]}, expected {width}x{height}')
    x11 = x11_windows_of(pid, window_helper)
    if x11:
        raise ValueError(f'window_mode wayland but measured PID owns X11 windows {x11}')
    return {'source': 'kwin', 'fullscreen': True, 'width': window['width'], 'height': window['height'],
            'output': window['output'], 'active': window['active'], 'x11_windows': x11}


def window_evidence(pid, window_helper, window_mode, display_size):
    """Доказательство окна по режиму: xprop/XRes для XWayland, KWin для нативного Wayland."""
    if window_mode == 'wayland':
        return wayland_window_proof(pid, window_helper, display_size)
    return window_proof(pid, window_helper)


def audio_sink(pid):
    nodes = json.loads(command_output(['pw-dump']))
    client_ids = {node['id'] for node in nodes if node['type'] == 'PipeWire:Interface:Client'
                  and str(node.get('info', {}).get('props', {}).get('application.process.id')) == str(pid)}
    matching = []
    for node in nodes:
        info = node.get('info', {})
        props = info.get('props', {})
        if (str(props.get('application.process.id')) == str(pid) or props.get('client.id') in client_ids) and props.get('media.class') == 'Stream/Output/Audio':
            matching.append({'id': node['id'], 'state': info.get('state'),
                             'media_class': props['media.class']})
    return matching


def opened_media(pid, media):
    expected = media.stat()
    for descriptor in Path(f'/proc/{pid}/fd').iterdir():
        try:
            actual = descriptor.stat()
        except FileNotFoundError:
            continue
        if (actual.st_dev, actual.st_ino) == (expected.st_dev, expected.st_ino):
            return {'device': actual.st_dev, 'inode': actual.st_ino}
    raise ValueError('measured process does not hold the exact media inode')


# Свойства mpv, читаемые на обеих границах окна. Счётчики потерь — те, что mpv сам
# называет потерянными кадрами: VO (framedrop=vo) и декодер (framedrop=decoder).
MPV_BOUNDARY_PROPERTIES = ('path', 'pause', 'time-pos', 'hwdec-current', 'video-codec', 'current-vo',
                           'current-ao', 'frame-drop-count', 'decoder-frame-drop-count',
                           'vo-delayed-frame-count', 'current-gpu-context', 'avsync')
# Аппаратные hwdec без копирования кадра в память CPU; ``*-copy`` и ``no`` не засчитываются.
MPV_ZERO_COPY_HWDEC = {'vaapi', 'vulkan'}


def mpv_properties(socket_path, names):
    """Читает свойства mpv через JSON IPC; отсутствующее свойство остаётся с ошибкой mpv.

    Каждый запрос несёт свой ``request_id``: асинхронные события mpv в том же сокете
    пропускаются, а не принимаются за ответ.
    """
    import socket
    with socket.socket(socket.AF_UNIX) as control:
        control.settimeout(2)
        control.connect(str(socket_path))
        for index, name in enumerate(names):
            control.sendall((json.dumps({'command': ['get_property', name], 'request_id': index}) + '\n').encode())
        replies, buffer = {}, b''
        while len(replies) < len(names):
            block = control.recv(65536)
            if not block:
                raise ValueError('mpv IPC closed before all replies')
            buffer += block
            *lines, buffer = buffer.split(b'\n')
            for line in lines:
                message = json.loads(line)
                if 'request_id' in message:
                    replies[names[message['request_id']]] = (message.get('data') if message.get('error') == 'success'
                                                             else {'error': message.get('error')})
    return replies


def boundary(player, pid, socket_path, media, window_helper, window_mode='xwayland', display_size=None):
    """Снимок доказательств на границе окна; ``player`` — тип игрока (fastiplayer/mpv/vlc)."""
    started = time.monotonic()
    if player == 'fastiplayer':
        service = 'org.mpris.MediaPlayer2.fastiplayer'
        owner_pid = int(command_output(['qdbus6', 'org.freedesktop.DBus', '/org/freedesktop/DBus',
                                       'org.freedesktop.DBus.GetConnectionUnixProcessID', service]))
        if owner_pid != pid:
            raise ValueError('MPRIS belongs to a different process')
        prefix = ['qdbus6', service, '/org/mpris/MediaPlayer2',
                  'org.freedesktop.DBus.Properties.Get', 'org.mpris.MediaPlayer2.Player']
        position = int(command_output([*prefix, 'Position']).strip())
        metadata = command_output([*prefix, 'Metadata'])
        # MPRIS намеренно не раскрывает URL. Проверяем inode открытого CLI-файла
        # у того же PID; metadata/Position подтверждают installed playback.
        media_matches = opened_media(pid, media)
        details = {'position_us': position, 'metadata': metadata, 'media_matches': media_matches,
                   'playback_status': command_output([*prefix, 'PlaybackStatus']).strip()}
    elif player == 'mpv':
        details = mpv_properties(socket_path, MPV_BOUNDARY_PROPERTIES)
        # path подтверждает текущий вход mpv, inode — что это тот самый файл.
        if details.get('path') != str(media):
            raise ValueError('mpv current input does not match measured file')
        details['media_matches'] = opened_media(pid, media)
    else:
        details = legacy.read_vlc_statistics(socket_path, 0)
        # oldrc status включает current input; argv/open FD сами по себе не доказывают playback.
        import socket
        with socket.socket(socket.AF_UNIX) as control:
            control.settimeout(2)
            control.connect(str(socket_path))
            control.sendall(b'status\n')
            reply = bytearray()
            while len(reply) < 65536:
                try:
                    block = control.recv(8192)
                except TimeoutError:
                    break
                if not block:
                    break
                reply.extend(block)
                if b'status: returned' in reply:
                    break
        details['status'] = reply.decode(errors='replace')
        if media.resolve().as_uri() not in details['status'] and str(media.resolve()) not in details['status']:
            raise ValueError('VLC current input does not match measured file')
    sinks = audio_sink(pid)
    window = window_evidence(pid, window_helper, window_mode, display_size)
    return {'started': started, 'ended': time.monotonic(), 'details': details,
            'audio_sinks': sinks, 'window': window}


# Строки плана видеоконвейера Fastiplayer (`Selected video pipeline plan="…"`, app-egui
# `VideoPipelinePlan::diagnostic_label`): маршрут декода × семейство рендера (план
# user/gles2-renderer/, сессия 08). Строки wgpu не менялись с cpu-parity.
SOFTWARE_PLANS = {
    'wgpu': 'plan="ffmpeg-host-upload-wgpu"',
    'gles2': 'plan="ffmpeg-host-upload-gles2"',
}
HARDWARE_PLANS = {
    'wgpu': 'plan="vaapi-dmabuf-wgpu"',
    'gles2': 'plan="vaapi-dmabuf-gles2"',
}
# Имя контракта пула поверхностей VA-API декодера: какой рендер импортирует его DMA-BUF.
DMA_BUF_IMPORT_BACKENDS = {
    'wgpu': 'VA-API Vulkan DMA-BUF',
    'gles2': 'VA-API EGL DMA-BUF',
}
RENDERER_FAMILIES = ('wgpu', 'gles2')


def fastiplayer_renderer_families(runtime):
    """Семейства рендера, следы которых есть в runtime-логе (строка плана или контракт импорта)."""
    return {family for family in RENDERER_FAMILIES
            if SOFTWARE_PLANS[family] in runtime or HARDWARE_PLANS[family] in runtime
            or DMA_BUF_IMPORT_BACKENDS[family] in runtime}


def fastiplayer_renderer_family(runtime):
    """Чем рисовала попытка: ``'wgpu'``, ``'gles2'`` или ``None`` — следов нет или их два вида
    сразу (смена семейства посреди попытки — не доказательство ни одного)."""
    families = fastiplayer_renderer_families(runtime)
    return next(iter(families)) if len(families) == 1 else None


def fastiplayer_backend_proven(runtime, codec, decode):
    """Доказан ли нужный путь декодирования Fastiplayer по его runtime-логу.

    Аппаратный: VA-API адаптер кодека + импорт DMA-BUF рендером попытки и ни одного SW-плана.
    У GLES2 нужна ещё строка его аппаратного плана (``vaapi-dmabuf-gles2``): по ней видно, что
    кадры импортировал именно GLES2-рендер.
    Программный (cpu-parity 08): план ``ffmpeg-host-upload-<рендер>`` и ни одного следа
    VA-API — иначе «SW-замер» мог бы тихо уйти в аппаратный путь.
    Следы двух семейств рендера сразу — не доказательство.
    """
    family = fastiplayer_renderer_family(runtime)
    if family is None:
        return False
    software_plan = any(plan in runtime for plan in SOFTWARE_PLANS.values())
    vaapi_traces = ('configured for stream backend_name="VA-API' in runtime
                    or any(name in runtime for name in DMA_BUF_IMPORT_BACKENDS.values()))
    if decode == 'software':
        return SOFTWARE_PLANS[family] in runtime and not vaapi_traces
    expected = {'h264': 'VA-API H.264', 'hevc': 'VA-API H.265', 'av1': 'VA-API AV1',
                'vp9': 'VA-API VP9'}[codec]
    renderer_imports = DMA_BUF_IMPORT_BACKENDS[family] in runtime
    if family == 'gles2':
        renderer_imports = renderer_imports and HARDWARE_PLANS['gles2'] in runtime
    return (f'configured for stream backend_name="{expected}"' in runtime
            and renderer_imports
            and not software_plan)


def validate(player, codec, mode, decode, log_path, before, after, start_offset, end_offset, *,
             expect=None, renderer=None):
    """Итог доказательств попытки; ``player`` — тип игрока.

    ``renderer`` (gles2/wgpu, поле записи Fastiplayer в плане) — каким рендером попытка
    обязана рисовать: следы другого семейства в логе делают попытку невалидной, иначе
    подмена конфига тихо подменила бы субъект сравнения. ``expect`` — свойства mpv,
    которые обязаны совпасть на обеих границах (см. :func:`validate_mpv`).
    """
    runtime = log_path.read_text(errors='replace')
    failures = []
    if 'panicked at' in runtime or 'video output creation failed' in runtime:
        failures.append('runtime fatal marker')
    if player == 'fastiplayer':
        backend = fastiplayer_backend_proven(runtime, codec, decode)
        a, b = before['details'], after['details']
        if not a['media_matches'] or not b['media_matches']:
            failures.append('MPRIS media does not match measured file')
        progress = b['position_us'] > a['position_us'] and a['playback_status'] == b['playback_status'] == 'Playing'
        audio = 'Startup audio playback resumed' in runtime and all(
            any(s['state'] == 'running' for s in boundary_value['audio_sinks']) for boundary_value in (before, after))
        events = legacy.render_events(log_path, start_offset, end_offset)
        quality = {'kind': 'surface_handoff_not_scanout', 'handoffs': len(events),
                   'unique_identities': len({(e['pts_ns'], e['render_generation'], e['decoded_generation']) for e in events}),
                   'events': events,
                   'limitation': 'quiet has no cadence proof; MPRIS/audio-sink evidence is not audible PCM validation'}
        if mode == 'diagnostic' and (len(events) < 2 or len({e['pts_ns'] for e in events}) < 2):
            failures.append('no advancing render handoffs')
    elif player == 'mpv':
        backend, progress, audio, quality, extra = validate_mpv(before, after, decode, expect)
        failures.extend(extra)
    else:
        backend = bool(re.search(r'Using .* for hardware decoding', runtime, re.I))
        if mode == 'diagnostic':
            backend = backend and 'using hw decoder module "vaapi"' in runtime and 'using vout display module "gl"' in runtime
        a, b = before['details']['counters'], after['details']['counters']
        required = {'media_time_seconds', 'frames_displayed', 'frames_lost', 'buffers_played', 'buffers_lost', 'audio_decoded'}
        if not required <= a.keys() or not required <= b.keys():
            failures.append('missing VLC counters')
        deltas = {key: b[key] - a[key] for key in a.keys() & b.keys()}
        progress = deltas.get('media_time_seconds', 0) > 0 and deltas.get('frames_displayed', 0) > 0
        audio = deltas.get('buffers_played', 0) > 0 and deltas.get('audio_decoded', 0) > 0
        quality = {'kind': 'vlc_counters_not_handoff_or_scanout', 'deltas': deltas}
        if any(value < 0 for value in deltas.values()):
            failures.append('VLC counters reset')
    if not backend:
        failures.append(f'expected {decode} backend not proven')
    if not progress:
        failures.append('media time/video did not advance')
    if not audio:
        failures.append('audio progression evidence missing')
    renderer_family = fastiplayer_renderer_family(runtime) if player == 'fastiplayer' else None
    if renderer is not None and renderer_family != renderer:
        failures.append(f'fastiplayer renderer {renderer_family!r} does not match plan renderer {renderer!r}')
    return {'backend_confirmed': backend, 'renderer_family': renderer_family,
            'playback_advancing': progress, 'audio_evidence': audio, 'quality': quality,
            'failures': failures}


def validate_mpv(before, after, decode='hardware', expect=None):
    """Доказательства mpv: путь декодирования на обеих границах, ход времени, звук, потери.

    Аппаратный путь — zero-copy hwdec; программный (cpu-parity 08) — ``hwdec-current``
    ровно ``no`` на обеих границах: mpv, тихо включивший hwdec, не эталон SW-сцены.
    Возвращает (backend, progress, audio, quality, failures). Пропавший счётчик потерь —
    это failure, а не ноль: без него quality-gate эталона не доказан.

    ``expect`` (сессия 10) — {свойство: значение}, например ``current-gpu-context: wayland``:
    mpv с ``--gpu-api=opengl`` мог бы молча выбрать другой контекст, и тогда «mpv GL»
    в таблице был бы не GL. Несовпадение на любой границе — failure.
    """
    a, b = before['details'], after['details']
    failures = []
    if decode == 'software':
        backend = a.get('hwdec-current') == 'no' and b.get('hwdec-current') == 'no'
    else:
        backend = a.get('hwdec-current') in MPV_ZERO_COPY_HWDEC and b.get('hwdec-current') == a.get('hwdec-current')
    progress = (isinstance(a.get('time-pos'), (int, float)) and isinstance(b.get('time-pos'), (int, float))
                and b['time-pos'] > a['time-pos'] and a.get('pause') is False and b.get('pause') is False)
    audio = bool(a.get('current-ao')) and all(
        any(s['state'] == 'running' for s in boundary_value['audio_sinks']) for boundary_value in (before, after))
    counters = ('frame-drop-count', 'decoder-frame-drop-count', 'vo-delayed-frame-count')
    deltas = {}
    for name in counters:
        if not isinstance(a.get(name), int) or not isinstance(b.get(name), int):
            failures.append(f'missing mpv counter {name}')
            continue
        deltas[name] = b[name] - a[name]
        if deltas[name] < 0:
            failures.append('mpv counters reset')
    for name, expected in (expect or {}).items():
        observed = (a.get(name), b.get(name))
        if observed != (expected, expected):
            failures.append(f'mpv {name} is {observed[0]!r}/{observed[1]!r}, expected {expected!r}')
    # avsync — разница A−V (секунды), которую mpv сам видит в момент чтения; для
    # quality-gate A/V берётся модуль на обеих границах (analyze.mpv_av_sync).
    quality = {'kind': 'mpv_counters_not_handoff_or_scanout', 'deltas': deltas,
               'hwdec': a.get('hwdec-current'), 'vo': a.get('current-vo'),
               'gpu_context': a.get('current-gpu-context'),
               'avsync_seconds': [a.get('avsync'), b.get('avsync')]}
    return backend, progress, audio, quality, failures


def capture(directory, phase, pid, window_helper, display_size, window_mode='xwayland'):
    """Тестовый screenshot вне CPU-окна; active window должен принадлежать нашему PID.

    Захват идёт через X11 (xprop + legacy capture), поэтому нативный Wayland не поддержан:
    честная ошибка вместо скриншота чужого окна.
    """
    if window_mode == 'wayland':
        raise ValueError('capture unsupported for window_mode wayland')
    window = window_proof(pid, window_helper)
    active = command_output(['xprop', '-root', '_NET_ACTIVE_WINDOW'])
    if window['window_id'] not in re.findall(r'0x[0-9a-f]+', active):
        raise ValueError('capture would target a foreign window')
    arguments = SimpleNamespace(capture_prefix=directory / 'render',
                                window_mode='xwayland-fullscreen', expected_capture_size=display_size)
    return legacy.capture_window(arguments, phase, 0)

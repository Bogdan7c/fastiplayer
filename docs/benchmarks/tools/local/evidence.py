"""Доказательства плееров: backend, файл, продвижение и отдельные quality counters."""

import importlib.util
import json
from pathlib import Path
import re
import sys
from types import SimpleNamespace
import time

from host import command_output

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


def boundary(player, pid, socket_path, media, window_helper):
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
    window = window_proof(pid, window_helper)
    return {'started': started, 'ended': time.monotonic(), 'details': details,
            'audio_sinks': sinks, 'window': window}


def validate(player, codec, mode, log_path, before, after, start_offset, end_offset):
    runtime = log_path.read_text(errors='replace')
    failures = []
    if 'panicked at' in runtime or 'video output creation failed' in runtime:
        failures.append('runtime fatal marker')
    if player == 'fastiplayer':
        expected = {'h264': 'VA-API H.264', 'hevc': 'VA-API H.265', 'av1': 'VA-API AV1'}[codec]
        backend = (f'configured for stream backend_name="{expected}"' in runtime
                   and 'VA-API Vulkan DMA-BUF' in runtime
                   and 'plan="ffmpeg-host-upload-wgpu"' not in runtime)
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
        failures.append('expected hardware backend not proven')
    if not progress:
        failures.append('media time/video did not advance')
    if not audio:
        failures.append('audio progression evidence missing')
    return {'backend_confirmed': backend, 'playback_advancing': progress,
            'audio_evidence': audio, 'quality': quality, 'failures': failures}


def capture(directory, phase, pid, window_helper, display_size):
    """Тестовый screenshot вне CPU-окна; active window должен принадлежать нашему PID."""
    window = window_proof(pid, window_helper)
    active = command_output(['xprop', '-root', '_NET_ACTIVE_WINDOW'])
    if window['window_id'] not in re.findall(r'0x[0-9a-f]+', active):
        raise ValueError('capture would target a foreign window')
    arguments = SimpleNamespace(capture_prefix=directory / 'render',
                                window_mode='xwayland-fullscreen', expected_capture_size=display_size)
    return legacy.capture_window(arguments, phase, 0)

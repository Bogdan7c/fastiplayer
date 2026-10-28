"""Каденс показа новых видеокадров Fastiplayer (gles2-renderer 12a.2, решение владельца 79).

Протокол v4 считает потерей только непоказанный кадр. На 30 fps кадр, показанный на vsync
позже (интервал 50 мс вместо 33), потерей не считается, хотя виден как рывок. Здесь —
описательная метрика: сколько новых кадров пришло позже 1,5 номинального интервала.
Quality-gate она не меняет; у mpv и VLC такого потока событий нет, поэтому колонка только
у Fastiplayer.

Источник — события «current video frame submitted to surface» из ``evidence.quality.events``
(момент после swap рендера, не scanout): новый кадр — первое событие с новой идентичностью
(PTS, render generation, decoded generation).
"""

from datetime import datetime

# Порог опоздания в номинальных интервалах кадра: на 30 fps — 50 мс (лишний vsync на 60 Гц),
# а каденс 3:2 у 24/25 fps (33/50 мс при номинале 41,7/40) опозданием не считается.
LATE_INTERVAL_FACTOR = 1.5


def event_seconds(event):
    """Время события журнала в секундах или None, если метки нет."""
    stamp = event.get('log_timestamp_utc')
    if not stamp:
        return None
    return datetime.fromisoformat(stamp.replace('Z', '+00:00')).timestamp()


def new_frame_moments(events):
    """Моменты первых показов новых кадров по порядку журнала (повторы того же кадра пропускаются)."""
    seen = set()
    moments = []
    for event in events:
        identity = (event['pts_ns'], event['render_generation'], event['decoded_generation'])
        if identity in seen:
            continue
        seen.add(identity)
        moment = event_seconds(event)
        if moment is not None:
            moments.append(moment)
    return moments


def cadence_summary(events, source_fps):
    """Опоздавшие новые кадры за окно или None, если событий или частоты источника нет."""
    if not source_fps or source_fps <= 0:
        return None
    moments = new_frame_moments(events)
    if len(moments) < 2:
        return None
    intervals_ms = sorted((later - earlier) * 1000.0 for earlier, later in zip(moments, moments[1:]))
    nominal_ms = 1000.0 / source_fps
    late_threshold_ms = LATE_INTERVAL_FACTOR * nominal_ms
    late_frames = sum(1 for interval in intervals_ms if interval > late_threshold_ms)
    return {'metric': 'late_new_frames', 'nominal_interval_ms': round(nominal_ms, 3),
            'late_threshold_ms': round(late_threshold_ms, 3), 'intervals': len(intervals_ms),
            'late_frames': late_frames, 'late_ratio': late_frames / len(intervals_ms),
            'p95_interval_ms': round(intervals_ms[int(0.95 * (len(intervals_ms) - 1))], 3),
            'max_interval_ms': round(intervals_ms[-1], 3)}

"""Каденс новых видеокадров Fastiplayer (gles2-renderer 12a.2): описательная колонка анализатора."""

from datetime import datetime, timedelta, timezone
import unittest

import analyze
import cadence
from test_protocol_v2 import fake_attempt

START = datetime(2026, 10, 28, 12, 0, 0, tzinfo=timezone.utc)


def events_at(milliseconds, *, repeat_each=1):
    """События показа новых кадров в заданные моменты (мс от старта), PTS по порядку."""
    events = []
    for index, moment in enumerate(milliseconds):
        stamp = (START + timedelta(milliseconds=moment)).strftime('%Y-%m-%dT%H:%M:%S.%fZ')
        for _ in range(repeat_each):
            events.append({'pts_ns': index * 33_333_000, 'render_generation': 1,
                           'decoded_generation': 0, 'log_timestamp_utc': stamp})
    return events


def steady(count, interval_ms):
    return [index * interval_ms for index in range(count)]


class CadenceSummaryTests(unittest.TestCase):
    def test_steady_30fps_on_60hz_has_no_late_frames(self):
        summary = cadence.cadence_summary(events_at(steady(31, 33.333)), 30.0)

        self.assertEqual(summary['late_frames'], 0)
        self.assertEqual(summary['intervals'], 30)
        self.assertEqual(summary['late_threshold_ms'], 50.0)

    def test_frame_shown_one_vsync_late_is_counted(self):
        moments = steady(10, 33.333)
        # Шестой кадр опоздал на vsync (~50 мс), седьмой пришёл через один vsync — как в трассе T410.
        moments[5] += 17.0
        summary = cadence.cadence_summary(events_at(moments), 30.0)

        self.assertEqual(summary['late_frames'], 1)
        self.assertAlmostEqual(summary["max_interval_ms"], 50.333, places=1)

    def test_repeated_handoffs_of_the_same_frame_are_not_new_frames(self):
        # Перерисовка интерфейса повторяет тот же кадр: интервал считается до следующего НОВОГО.
        summary = cadence.cadence_summary(events_at(steady(11, 33.333), repeat_each=3), 30.0)

        self.assertEqual(summary['intervals'], 10)
        self.assertEqual(summary['late_frames'], 0)

    def test_24fps_three_two_pulldown_is_not_late(self):
        # 24 fps на 60 Гц: кадры по 2 и 3 vsync (33,3 / 50 мс) — норма, а не рывок.
        moments, now = [], 0.0
        for index in range(24):
            moments.append(now)
            now += 33.333 if index % 2 == 0 else 50.0
        summary = cadence.cadence_summary(events_at(moments), 24.0)

        self.assertEqual(summary['late_frames'], 0)

    def test_missing_rate_or_events_give_no_summary(self):
        self.assertIsNone(cadence.cadence_summary(events_at(steady(10, 33.3)), None))
        self.assertIsNone(cadence.cadence_summary(events_at([0.0]), 30.0))


class AnalyzerCadenceColumnTests(unittest.TestCase):
    def test_late_frames_are_reported_without_changing_the_quality_gate(self):
        attempt = fake_attempt('hw-1080p30', 'fastiplayer', 'diagnostic', 1, 10.0, unique=600, fps=30.0)
        moments = steady(601, 33.333)
        for late in (100, 200, 300):
            moments[late] += 17.0
        attempt['evidence']['quality']['events'] = events_at(moments)

        result = analyze.attempt_quality(attempt)

        self.assertEqual(result['status'], 'PASS')
        self.assertEqual(result['cadence']['late_frames'], 3)

    def test_attempt_without_events_has_no_cadence(self):
        attempt = fake_attempt('hw-1080p30', 'fastiplayer', 'diagnostic', 1, 10.0, unique=600, fps=30.0)

        self.assertIsNone(analyze.attempt_quality(attempt)['cadence'])


if __name__ == '__main__':
    unittest.main()

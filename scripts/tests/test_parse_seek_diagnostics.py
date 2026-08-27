#!/usr/bin/env python3
"""Focused regression tests новых и legacy seek diagnostics markers."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from types import ModuleType


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
PARSER_PATH = SCRIPTS_DIRECTORY / "parse-seek-diagnostics.py"
sys.path.insert(0, str(SCRIPTS_DIRECTORY))


def load_parser() -> ModuleType:
    """Загружает CLI script как модуль, сохраняя dataclass namespace."""

    module_spec = importlib.util.spec_from_file_location(
        "parse_seek_diagnostics", PARSER_PATH
    )
    if module_spec is None or module_spec.loader is None:
        raise RuntimeError(f"не удалось загрузить parser `{PARSER_PATH}`")
    parser_module = importlib.util.module_from_spec(module_spec)
    sys.modules[module_spec.name] = parser_module
    module_spec.loader.exec_module(parser_module)
    return parser_module


PARSER = load_parser()


def worker_transaction(
    generation: int,
    latency_ms: int,
    *,
    selected_video: str = "Some(TrackId(1))",
    selected_audio: str = "Some(TrackId(2))",
    include_presented: bool = True,
    include_audio: bool = True,
) -> list[str]:
    """Создаёт один полный worker-receipted marker lifecycle."""

    lines = [
        f"generation={generation} target_milliseconds=355000 "
        "Prepared demux seek request enqueued",
        "elapsed_milliseconds=2 Source HTTP response headers ready",
        "elapsed_milliseconds=3 Source HTTP first non-empty body chunk ready",
        "elapsed_milliseconds=6 Source HTTP validated body complete",
        "candidate_segment_index=35 HLS manifest seek candidate started",
        "elapsed_milliseconds=4 HLS manifest seek candidate demux open completed",
        "inspected_bytes=4096 HLS manifest seek anchor proven",
        "elapsed_milliseconds=5 HLS manifest seek candidate accepted",
        f"generation={generation} elapsed_milliseconds=0 "
        "Prepared demux seek receipt accepted",
        f"generation={generation} target_ms=355000 actual_ms=350000 "
        f"selected_video_track_id={selected_video} "
        f"selected_audio_track_id={selected_audio} Demux seek transaction accepted",
        f"generation={generation} First post-seek video packet observed",
        f"generation={generation} First post-seek decoded frame observed",
    ]
    if include_presented:
        lines.append(
            f"generation={generation} elapsed_ms={latency_ms} "
            "First post-seek presented frame observed"
        )
    if include_audio:
        lines.append(
            f"generation={generation} accepted_after_ms={latency_ms} "
            "Audio play accepted before final seek commit"
        )
    lines.append(
        f"generation={generation} committed_ms=355000 Final seek commit завершён"
    )
    return lines


class SeekDiagnosticsParserTests(unittest.TestCase):
    """Проверяет metrics, gates, supersede и backward compatibility."""

    def test_ten_sequential_values_have_exact_nearest_rank_summary(self):
        parser = PARSER.SeekDiagnosticsParser(scenario="series", media_kind="auto")
        lines = [
            line
            for latency_ms in range(1, 11)
            for line in worker_transaction(latency_ms, latency_ms)
        ]

        parser.parse_lines(lines, "series.log")

        summary = {row["metric"]: row for row in parser.summary_rows()}
        for metric in ("public_to_presented_ms", "public_to_audio_ms"):
            self.assertEqual(summary[metric]["count"], "10")
            self.assertEqual(summary[metric]["p50"], "5")
            self.assertEqual(summary[metric]["p95"], "10")
            self.assertEqual(summary[metric]["max"], "10")
            self.assertEqual(summary[metric]["percentile_method"], "nearest-rank")

        first_transaction = parser.transactions[0]
        self.assertEqual(first_transaction.timing.manifest_candidate_count, 1)
        self.assertTrue(first_transaction.timing.manifest_anchor_proven)
        self.assertTrue(first_transaction.timing.manifest_candidate_accepted)
        self.assertEqual(first_transaction.timing.manifest_candidate_elapsed_ms, 5)
        self.assertEqual(parser.rows()[0]["network_first_byte_ms"], "3")
        self.assertEqual(parser.rows()[0]["network_body_complete_ms"], "6")

    def test_legacy_adaptive_http_markers_remain_accepted(self):
        parser = PARSER.SeekDiagnosticsParser(scenario="legacy-http", media_kind="auto")
        lines = worker_transaction(1, 7)
        lines[1:4] = [
            "elapsed_milliseconds=11 Adaptive streaming HTTP headers ready",
            "elapsed_milliseconds=13 Adaptive streaming HTTP first body chunk ready",
        ]

        parser.parse_lines(lines, "legacy-http.log")

        row = parser.rows()[0]
        self.assertEqual(row["network_first_byte_ms"], "13")
        self.assertEqual(row["network_body_complete_ms"], "")

    def test_superseded_incomplete_transaction_is_excluded_from_summary(self):
        parser = PARSER.SeekDiagnosticsParser(scenario="supersede", media_kind="auto")
        current_transaction = worker_transaction(2, 8)
        lines = [
            "generation=1 target_milliseconds=60000 Prepared demux seek request enqueued",
            current_transaction[0],
            # Запоздалый receipt старой generation не должен отравить новую transaction.
            "generation=1 elapsed_milliseconds=7 Prepared demux seek receipt accepted",
            *current_transaction[1:],
        ]

        parser.parse_lines(lines, "supersede.log")

        self.assertTrue(parser.transactions[0].superseded)
        self.assertEqual(parser.rows()[0]["verdict"], "FAIL")
        self.assertEqual(parser.transactions[1].timing.worker_round_trip_ms, 0)
        summary = {row["metric"]: row for row in parser.summary_rows()}
        self.assertEqual(summary["public_to_presented_ms"]["count"], "1")
        self.assertEqual(summary["public_to_audio_ms"]["count"], "1")
        self.assertEqual(summary["public_to_presented_ms"]["max"], "8")

    def test_selected_audio_and_video_markers_are_strict(self):
        parser = PARSER.SeekDiagnosticsParser(scenario="missing", media_kind="auto")
        parser.parse_lines(
            worker_transaction(1, 4, include_presented=False, include_audio=False),
            "missing.log",
        )

        transaction = parser.transactions[0]
        self.assertEqual(transaction.verdict("auto"), "FAIL")
        self.assertIn("first_presented", transaction.missing_required_markers("auto"))
        self.assertIn(
            "audio_play_accepted", transaction.missing_required_markers("auto")
        )

        video_only = PARSER.SeekDiagnosticsParser(
            scenario="video-only", media_kind="auto"
        )
        video_only.parse_lines(
            worker_transaction(2, 6, selected_audio="None", include_audio=False),
            "video-only.log",
        )
        self.assertEqual(video_only.rows()[0]["verdict"], "PASS")
        self.assertEqual(video_only.rows()[0]["receipt_to_audio_ms"], "")

    def test_audio_soft_fallback_does_not_satisfy_selected_audio_gate(self):
        parser = PARSER.SeekDiagnosticsParser(scenario="fallback", media_kind="auto")
        lines = worker_transaction(3, 9, include_audio=False)
        lines.insert(-1, "generation=3 Final seek commit продолжен через audio gate soft fallback")

        parser.parse_lines(lines, "fallback.log")

        self.assertEqual(parser.rows()[0]["verdict"], "FAIL")
        self.assertEqual(parser.rows()[0]["audio_gate"], "soft_fallback")
        self.assertEqual(parser.summary_rows()[1]["count"], "0")

    def test_legacy_markers_still_parse_without_new_timing_fields(self):
        parser = PARSER.SeekDiagnosticsParser(scenario="legacy", media_kind="auto")
        parser.parse_lines(
            [
                "generation=77 target_ms=8000 selected_video_track_id=Some(TrackId(1)) "
                "selected_audio_track_id=None Starting demux seek transaction",
                "generation=77 actual_ms=5000 Demux seek transaction accepted",
                "generation=77 First post-seek video packet observed",
                "generation=77 First post-seek decoded frame observed",
                "generation=77 First post-seek presented frame observed",
                "generation=77 committed_ms=8000 Final seek commit завершён",
            ],
            "legacy.log",
        )

        row = parser.rows()[0]
        self.assertEqual(row["verdict"], "PASS")
        self.assertEqual(row["worker_round_trip_ms"], "")
        self.assertEqual(row["public_to_presented_ms"], "")
        self.assertEqual(parser.summary_rows()[0]["count"], "0")


if __name__ == "__main__":
    unittest.main()

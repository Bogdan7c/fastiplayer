#!/usr/bin/env python3
"""Functional table/strict regressions offline playback acceptance CLI."""

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from playback_acceptance import Verdict  # noqa: E402
from playback_acceptance_parser import PlaybackAcceptanceAnalyzer  # noqa: E402


CLI_PATH = SCRIPTS_DIRECTORY / "analyze-playback-acceptance.py"
CLI_SPEC = importlib.util.spec_from_file_location(
    "analyze_playback_acceptance_cli",
    CLI_PATH,
)
assert CLI_SPEC is not None and CLI_SPEC.loader is not None
CLI = importlib.util.module_from_spec(CLI_SPEC)
CLI_SPEC.loader.exec_module(CLI)
BASE_TIMESTAMP = datetime(2026, 8, 27, tzinfo=timezone.utc)


def log_line(offset_ms: float, message: str) -> str:
    """Добавляет deterministic timestamp к synthetic tracing marker-у."""

    timestamp = BASE_TIMESTAMP + timedelta(milliseconds=offset_ms)
    return f"{timestamp.isoformat(timespec='microseconds').replace('+00:00', 'Z')} DEBUG {message}"


def passing_startup() -> list[str]:
    """Строит полный startup PASS, чтобы strict outcome зависел от anomaly."""

    target = "Restore { target_position: 355s }"
    return [
        log_line(0, "=== fastiplayer ==="),
        log_line(
            10,
            "startup_attempt_id=7 process_elapsed_ms=10 "
            f"startup_target={target} playback_expectation=Playing "
            "audio_expectation=Unknown Startup media-open/restore accepted",
        ),
        log_line(
            40,
            "startup_attempt_id=7 process_to_presented_ms=40 frame_pts_ms=355040 "
            "First startup video frame presented",
        ),
        log_line(
            42,
            "startup_attempt_id=7 process_to_audio_output_ms=42 "
            "playback_expectation=Playing Startup audio output ready",
        ),
        log_line(
            45,
            "startup_attempt_id=7 process_to_audio_ms=45 "
            "Startup audio playback resumed",
        ),
        log_line(
            46,
            "startup_attempt_id=7 process_to_ready_ms=46 media_to_ready_ms=36 "
            f"startup_target={target} playback_expectation=Playing "
            "audio_expectation=Required Startup presentation and audio gates ready",
        ),
    ]


def hls_marker(*, actual_anchor_ms: int) -> str:
    """Строит exact HLS Display line для CLI integration regression."""

    return (
        "kind=hls_manifest_segment_seek phase=final_receipt component_role=video "
        "manifest_selection_id=81 landing_policy=prefer_post_target_rap "
        "source_generation=17 requested_target_ms=12345 "
        f"actual_anchor_ms={actual_anchor_ms} actual_decode_anchor_ms=12400 "
        "anchor_kind=video_random_access_point media_sequence=91 "
        "discontinuity_sequence=7 manifest_segment_index=5 epoch_index=2 "
        "restart_segment_index=3 segment_start_ms=12000 segment_end_ms=18000"
    )


class PlaybackAcceptanceCliTests(unittest.TestCase):
    """Проверяет user-visible anomaly counts и strict fail-closed exit."""

    def test_table_and_strict_surface_proof_relevant_network_anomaly(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="strict-network-anomaly")
        lines = passing_startup() + [
            log_line(
                50,
                'request_id=request-only operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                55,
                'outcome="cancelled" received_bytes=0 elapsed_milliseconds=5 '
                "Bounded HTTP request terminal",
            ),
        ]
        analyzer.parse_lines(lines, "strict-network-anomaly.log")

        self.assertEqual(analyzer.runs[0].verdict(), Verdict.PASS)
        self.assertTrue(CLI.has_blocking_outcome(analyzer))
        table = io.StringIO()
        CLI.write_table(analyzer, table)
        table_text = table.getvalue()
        self.assertIn("anomalies | proof anomalies", table_text)
        self.assertIn("missing_request_id", table_text)
        self.assertIn("proof_relevant", table_text)

        with tempfile.TemporaryDirectory() as temporary_directory:
            log_path = Path(temporary_directory) / "strict-network-anomaly.log"
            log_path.write_text("\n".join(lines), encoding="utf-8")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                exit_code = CLI.main(
                    [str(log_path), "--format", "json", "--strict"]
                )
        self.assertEqual(exit_code, 1)
        report = json.loads(stdout.getvalue())
        self.assertEqual(report["network_anomaly_summary"]["anomaly_count"], 1)
        self.assertEqual(
            report["network_anomaly_summary"]["proof_relevant_anomaly_count"],
            1,
        )

    def test_unmatched_old_orphan_is_visible_but_not_strict_blocking(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="legacy-orphan")
        analyzer.parse_lines(
            passing_startup()
            + [log_line(55, "Source HTTP request cancelled elapsed_milliseconds=5")],
            "legacy-orphan.log",
        )

        self.assertEqual(analyzer.runs[0].verdict(), Verdict.PASS)
        self.assertEqual(len(analyzer.network_terminal_anomalies), 1)
        self.assertEqual(
            analyzer.network_terminal_anomalies[0].impact,
            "diagnostic_only",
        )
        self.assertFalse(CLI.has_blocking_outcome(analyzer))

    def test_strict_blocks_missing_modern_scrub_pair_and_table_names_it(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="strict-scrub-anomaly")
        analyzer.parse_lines(
            passing_startup()
            + [
                log_line(
                    55,
                    "scrub_schema_version=1 scrub_command_id=1 "
                    "scrub_stage=begin scrub_command_form=dispatch "
                    "scrub_target_kind=none scrub_requested_target_ms=0 "
                    "Player scrub command received",
                )
            ],
            "strict-scrub-anomaly.log",
        )

        self.assertEqual(analyzer.runs[0].verdict(), Verdict.PASS)
        self.assertTrue(CLI.has_blocking_outcome(analyzer))
        table = io.StringIO()
        CLI.write_table(analyzer, table)
        self.assertIn("missing_acceptance_form", table.getvalue())
        self.assertEqual(
            analyzer.to_dict()["scrub_anomaly_summary"],
            {
                "anomaly_count": 1,
                "proof_relevant_anomaly_count": 1,
                "by_kind": {"missing_acceptance_form": 1},
            },
        )

    def test_hls_table_shows_exact_segment_and_strict_blocks_typed_anomaly(self):
        clean_analyzer = PlaybackAcceptanceAnalyzer(scenario="clean-hls-marker")
        clean_analyzer.parse_lines(
            passing_startup() + [log_line(50, hls_marker(actual_anchor_ms=12_600))],
            "clean-hls-marker.log",
        )
        self.assertFalse(CLI.has_blocking_outcome(clean_analyzer))
        clean_table = io.StringIO()
        CLI.write_table(clean_analyzer, clean_table)
        clean_table_text = clean_table.getvalue()
        self.assertIn("HLS MANIFEST SELECTIONS", clean_table_text)
        self.assertIn("warm | final_receipt | video", clean_table_text)
        self.assertIn("[12000,18000)", clean_table_text)
        self.assertIn("12345 | [12000,18000) | 12600 | 12400", clean_table_text)

        invalid_lines = passing_startup() + [
            log_line(50, hls_marker(actual_anchor_ms=18_000))
        ]
        invalid_analyzer = PlaybackAcceptanceAnalyzer(scenario="invalid-hls-marker")
        invalid_analyzer.parse_lines(invalid_lines, "invalid-hls-marker.log")
        self.assertEqual(invalid_analyzer.runs[0].verdict(), Verdict.PASS)
        self.assertTrue(CLI.has_blocking_outcome(invalid_analyzer))
        invalid_table = io.StringIO()
        CLI.write_table(invalid_analyzer, invalid_table)
        invalid_table_text = invalid_table.getvalue()
        self.assertIn("anomalies | proof anomalies | by kind", invalid_table_text)
        self.assertIn("actual_anchor_outside_segment:1", invalid_table_text)
        self.assertIn("actual_anchor_outside_segment", invalid_table_text)

        with tempfile.TemporaryDirectory() as temporary_directory:
            log_path = Path(temporary_directory) / "invalid-hls-marker.log"
            log_path.write_text("\n".join(invalid_lines), encoding="utf-8")
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                exit_code = CLI.main(
                    [str(log_path), "--format", "json", "--strict"]
                )
        self.assertEqual(exit_code, 1)
        report = json.loads(stdout.getvalue())
        self.assertEqual(
            report["hls_manifest_selection_anomaly_summary"],
            {
                "anomaly_count": 1,
                "proof_relevant_anomaly_count": 1,
                "by_kind": {"actual_anchor_outside_segment": 1},
            },
        )


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Deterministic tests offline startup/seek acceptance analyzer."""

from __future__ import annotations

import sys
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from playback_acceptance import Verdict  # noqa: E402
from playback_acceptance_parser import PlaybackAcceptanceAnalyzer  # noqa: E402


BASE_TIMESTAMP = datetime(2026, 8, 25, tzinfo=timezone.utc)


def log_line(offset_ms: float, message: str) -> str:
    """Добавляет стабильный ISO timestamp к synthetic tracing marker-у."""

    timestamp = BASE_TIMESTAMP + timedelta(milliseconds=offset_ms)
    return f"{timestamp.isoformat(timespec='microseconds').replace('+00:00', 'Z')} DEBUG {message}"


def process_prefix() -> list[str]:
    """Создаёт оба обязательных startup origins."""

    return [
        log_line(0, "=== rustiplayer ==="),
        log_line(
            10,
            "process_elapsed_ms=10 Startup media-open/restore accepted",
        ),
    ]


def explicit_startup_readiness() -> list[str]:
    """Даёт parser-у честные app-owned surface/audio endpoints."""

    return [
        log_line(
            40,
            "startup_attempt_id=1 process_to_presented_ms=40 "
            "First startup video frame presented",
        ),
        log_line(
            45,
            "startup_attempt_id=1 process_to_audio_ms=45 "
            "Startup audio playback resumed",
        ),
    ]


def structured_startup_readiness(
    playback: str,
    *,
    include_output_ready: bool,
    include_resumed: bool,
    final_attempt_id: int = 7,
    final_target: str = "Restore { target_position: 355s }",
    final_playback: str | None = None,
) -> list[str]:
    """Строит production-like correlated startup attempt без сетевого I/O."""

    target = "Restore { target_position: 355s }"
    lines = [
        log_line(0, "=== rustiplayer ==="),
        log_line(
            10,
            "startup_attempt_id=7 process_elapsed_ms=10 "
            f"startup_target={target} playback_expectation={playback} "
            "audio_expectation=Unknown Startup media-open/restore accepted",
        ),
        log_line(
            40,
            "startup_attempt_id=7 process_to_presented_ms=40 frame_pts_ms=355040 "
            "First startup video frame presented",
        ),
    ]
    if include_output_ready:
        lines.append(
            log_line(
                42,
                "startup_attempt_id=7 process_to_audio_output_ms=42 "
                f"playback_expectation={playback} Startup audio output ready",
            )
        )
    if include_resumed:
        lines.append(
            log_line(
                45,
                "startup_attempt_id=7 process_to_audio_ms=45 "
                "Startup audio playback resumed",
            )
        )
    lines.append(
        log_line(
            46,
            f"startup_attempt_id={final_attempt_id} process_to_ready_ms=46 "
            f"media_to_ready_ms=36 startup_target={final_target} "
            f"playback_expectation={final_playback or playback} "
            "audio_expectation=Required Startup presentation and audio gates ready",
        )
    )
    return lines


def complete_seek(
    generation: int,
    base_ms: float,
    origin_ms: float,
    target_ms: float,
    public_ready_ms: float,
    *,
    include_public: bool = True,
    include_progress: bool = True,
    include_pre_target_proof: bool = True,
    process_ready_base_ms: float | None = None,
) -> list[str]:
    """Создаёт полный final seek с правильным target frame и selected audio."""

    enqueue_gap_ms = 2.0
    worker_round_trip_ms = 20.0
    receipt_to_ready_ms = public_ready_ms - enqueue_gap_ms - worker_round_trip_ms
    lines: list[str] = []
    if include_public:
        lines.extend(
            [
                log_line(
                    base_ms,
                    "Player command received "
                    f"command=Seek(SeekRequest {{ target: Absolute(MediaTime({target_ms}ms)), "
                    "mode: Accurate }) "
                    f"current_position_ms={origin_ms}",
                ),
                log_line(
                    base_ms + 1,
                    f"generation={generation} target_ms={target_ms} "
                    "Public final seek accepted",
                ),
            ]
        )
    lines.extend(
        [
            log_line(
                base_ms + enqueue_gap_ms,
                f"generation={generation} target_milliseconds={target_ms} "
                f"public_to_enqueue_ms={enqueue_gap_ms} "
                "Prepared demux seek request enqueued",
            ),
            log_line(
                base_ms + enqueue_gap_ms + worker_round_trip_ms,
                f"generation={generation} elapsed_milliseconds={worker_round_trip_ms} "
                f"target_milliseconds={target_ms} actual_milliseconds={target_ms - 5000} "
                "Prepared demux seek receipt accepted",
            ),
            log_line(
                base_ms + 23,
                f"generation={generation} target_ms={target_ms} actual_ms={target_ms - 5000} "
                "selected_video_track_id=Some(TrackId(258)) "
                "selected_audio_track_id=Some(TrackId(257)) "
                "available_audio_track_count=1 "
                "Demux seek transaction accepted",
            ),
            log_line(
                base_ms + public_ready_ms - 1,
                f"generation={generation} frame_pts_ms={target_ms} "
                "First post-seek decoded frame observed",
            ),
            log_line(
                base_ms + public_ready_ms,
                f"generation={generation} target_ms={target_ms} frame_pts_ms={target_ms} "
                "stale_frame=false "
                f"seek_elapsed_ms={public_ready_ms} "
                f"public_to_presented_ms={public_ready_ms} "
                f"receipt_to_presented_ms={receipt_to_ready_ms} "
                + (
                    f"process_to_presented_ms={process_ready_base_ms} "
                    if process_ready_base_ms is not None
                    else ""
                )
                + "First post-seek presented frame observed",
            ),
            log_line(
                base_ms + public_ready_ms + 0.5,
                f"generation={generation} seek_elapsed_ms={public_ready_ms + 0.5} "
                f"public_to_audio_ms={public_ready_ms + 0.5} "
                f"receipt_to_audio_ms={receipt_to_ready_ms + 0.5} "
                + (
                    f"process_to_audio_ms={process_ready_base_ms + 0.5} "
                    if process_ready_base_ms is not None
                    else ""
                )
                + "Audio play accepted before final seek commit",
            ),
            log_line(
                base_ms + public_ready_ms + 1,
                f"generation={generation} target_ms={target_ms} committed_ms={target_ms} "
                f"public_to_commit_ms={public_ready_ms + 1} "
                f"receipt_to_commit_ms={receipt_to_ready_ms + 1} "
                "available_audio_track_count=1 "
                + (
                    "presented_pre_target_frames=0 "
                    if include_pre_target_proof
                    else ""
                )
                + "Final seek commit завершён",
            ),
        ]
    )
    if include_progress:
        lines.append(
            log_line(
                base_ms + public_ready_ms + 20,
                f"generation={generation} position_ms={target_ms + 20} "
                f"public_to_progress_ms={public_ready_ms + 20} "
                f"receipt_to_progress_ms={receipt_to_ready_ms + 20} "
                "commit_to_progress_ms=19 "
                "Post-seek position progress observed",
            )
        )
    return lines


class PlaybackAcceptanceAnalyzerTests(unittest.TestCase):
    """Проверяет A/V gates, percentiles, scrub, supersede и HTTP correlation."""

    def test_colored_tracing_fields_are_normalized_before_parsing(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="colored-pty")
        plain_lines = process_prefix() + complete_seek(
            1,
            100,
            0,
            60_000,
            200,
            process_ready_base_ms=300,
        )
        colored_lines = [
            f"\x1b[2m{line.replace('=', '\x1b[2m=\x1b[0m')}\x1b[0m"
            for line in plain_lines
        ]

        analyzer.parse_lines(colored_lines, "colored-pty.log")

        self.assertEqual(analyzer.runs[0].verdict(), Verdict.PASS)
        self.assertEqual(analyzer.samples[0].verdict(), Verdict.PASS)
        self.assertEqual(analyzer.samples[0].monotonic_public_to_ready_ms(), 200.5)

    def test_ten_seek_series_reports_forward_backward_repeated_and_nearest_rank(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="warm-10")
        targets = [60_000, 180_000, 355_000, 550_000, 355_000, 180_000, 60_000, 60_000, 550_000, 355_000]
        lines = process_prefix()
        origin = 0.0
        for index, target in enumerate(targets, start=1):
            lines.extend(
                complete_seek(
                    index,
                    1000.0 * index,
                    origin,
                    float(target),
                    100.0 + index,
                    process_ready_base_ms=1101.0 if index == 1 else None,
                )
            )
            origin = float(target)

        analyzer.parse_lines(lines, "warm-10.log")

        self.assertEqual(len(analyzer.samples), 10)
        self.assertTrue(all(sample.verdict() == Verdict.PASS for sample in analyzer.samples))
        self.assertIn("forward", {sample.direction for sample in analyzer.samples})
        self.assertIn("backward", {sample.direction for sample in analyzer.samples})
        self.assertTrue(any(sample.repeated for sample in analyzer.samples))
        summary = {row.metric: row for row in analyzer.summary_rows()}
        ready = summary["public_to_ready_ms"]
        self.assertEqual(ready.eligible_count, 10)
        self.assertEqual(ready.p50, 105.5)
        self.assertEqual(ready.p95, 110.5)
        self.assertEqual(ready.maximum, 110.5)
        self.assertEqual(analyzer.runs[0].verdict(), Verdict.PASS)

    def test_missing_progress_and_pre_target_proof_is_incomplete_not_success(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1,
            100,
            0,
            355_000,
            200,
            include_progress=False,
            include_pre_target_proof=False,
            process_ready_base_ms=300,
        )

        analyzer.parse_lines(lines, "incomplete.log")

        sample = analyzer.samples[0]
        self.assertEqual(sample.verdict(), Verdict.INCOMPLETE)
        self.assertIn("position_progressed", sample.missing_gates())
        self.assertIn("no_pre_target_presentation_proof", sample.missing_gates())
        summary = analyzer.summary_rows()[0]
        self.assertEqual(summary.observed_count, 1)
        self.assertEqual(summary.eligible_count, 0)
        self.assertEqual(summary.incomplete_count, 1)

    def test_nonzero_owner_pre_target_counter_is_explicit_failure(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1, 100, 0, 355_000, 200, process_ready_base_ms=300
        )
        lines = [
            line.replace(
                "presented_pre_target_frames=0",
                "presented_pre_target_frames=1",
            )
            for line in lines
        ]

        analyzer.parse_lines(lines, "pre-target-violation.log")

        sample = analyzer.samples[0]
        self.assertEqual(sample.verdict(), Verdict.FAIL)
        self.assertEqual(sample.pre_target_presented_count, 1)
        self.assertIn("pre_target_frame_presented", sample.explicit_failures)

    def test_available_but_unselected_audio_is_not_treated_as_audio_less(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1, 100, 0, 60_000, 200, process_ready_base_ms=300
        )
        lines = [
            line.replace(
                "selected_audio_track_id=Some(TrackId(257))",
                "selected_audio_track_id=None",
            )
            for line in lines
            if "Audio play accepted before final seek commit" not in line
        ]

        analyzer.parse_lines(lines, "audio-unselected.log")

        sample = analyzer.samples[0]
        self.assertEqual(sample.verdict(), Verdict.FAIL)
        self.assertIn("selected_audio_track", sample.missing_gates())
        self.assertIn("audio_resumed", sample.missing_gates())
        self.assertIn("commit_before_audio", sample.order_failures())

    def test_commit_before_frame_and_audio_is_explicit_failure(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1, 100, 0, 60_000, 100, process_ready_base_ms=200
        )
        commit_index = next(
            index for index, line in enumerate(lines) if "Final seek commit завершён" in line
        )
        lines.pop(commit_index)
        commit_line = log_line(
            150,
            "generation=1 target_ms=60000 committed_ms=60000 "
            "presented_pre_target_frames=0 Final seek commit завершён",
        )
        presented_index = next(
            index
            for index, line in enumerate(lines)
            if "First post-seek decoded frame observed" in line
        )
        lines.insert(presented_index, commit_line)

        analyzer.parse_lines(lines, "false-commit.log")

        sample = analyzer.samples[0]
        self.assertEqual(sample.verdict(), Verdict.FAIL)
        self.assertIn("commit_before_target_frame", sample.order_failures())
        self.assertIn("commit_before_audio", sample.order_failures())

    def test_superseded_seek_is_excluded_but_final_seek_passes(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix()
        lines.extend(
            [
                log_line(
                    100,
                    "Player command received command=Seek(SeekRequest { "
                    "target: Absolute(MediaTime(60000ms)), mode: Accurate }) "
                    "current_position_ms=0",
                ),
                log_line(
                    102,
                    "generation=1 target_milliseconds=60000 public_to_enqueue_ms=2 "
                    "Prepared demux seek request enqueued",
                ),
            ]
        )
        lines.extend(
            complete_seek(
                2,
                105,
                0,
                550_000,
                200,
                process_ready_base_ms=305,
            )
        )
        lines.append(
            log_line(
                150,
                "generation=1 elapsed_milliseconds=48 Prepared demux seek receipt accepted",
            )
        )

        analyzer.parse_lines(lines, "supersede.log")

        self.assertEqual(analyzer.samples[0].verdict(), Verdict.SUPERSEDED)
        self.assertEqual(analyzer.samples[1].verdict(), Verdict.PASS)
        summary = analyzer.summary_rows()[0]
        self.assertEqual(summary.eligible_count, 1)
        self.assertEqual(summary.superseded_count, 1)

    def test_superseded_owned_network_request_requires_cancel_or_prior_completion(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + [
            log_line(
                100,
                "Player command received command=Seek(SeekRequest { "
                "target: Absolute(MediaTime(60000ms)), mode: Accurate }) "
                "current_position_ms=0",
            ),
            log_line(
                102,
                "generation=1 target_milliseconds=60000 public_to_enqueue_ms=2 "
                "Prepared demux seek request enqueued",
            ),
            log_line(
                103,
                'http_request_id=req-1 operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                105,
                "supersede_after_ms=5 Player command received "
                "command=Seek(SeekRequest { target: Absolute(MediaTime(550000ms)), "
                "mode: Accurate }) current_position_ms=0",
            ),
            log_line(
                106,
                'http_request_id=req-1 operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=3 Source HTTP request cancelled",
            ),
        ]
        lines.extend(
            complete_seek(
                2,
                105,
                0,
                550_000,
                200,
                include_public=False,
                process_ready_base_ms=305,
            )
        )

        analyzer.parse_lines(lines, "supersede-cancel.log")

        superseded = analyzer.samples[0]
        self.assertEqual(superseded.verdict(), Verdict.SUPERSEDED)
        self.assertEqual(superseded.superseded_after_ms, 5)
        self.assertEqual(
            superseded.supersede_network_status,
            "cancelled_or_completed_before_supersede",
        )
        self.assertEqual(analyzer.network_requests[0].cancelled_ms, 3)

    def test_timeline_drag_requires_begin_preview_end_and_monotonic_span(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + [
            log_line(100, "Player command received command=BeginScrub"),
            log_line(
                120,
                "elapsed_since_begin_ms=20 Player command received command=PreviewScrub",
            ),
            log_line(
                180,
                "begin_to_end_ms=80 Player command received command=EndScrub",
            ),
            log_line(
                181,
                "generation=1 target_ms=355000 Public final seek accepted",
            ),
        ]
        lines.extend(
            complete_seek(
                1,
                180,
                0,
                355_000,
                150,
                include_public=False,
                process_ready_base_ms=330,
            )
        )

        analyzer.parse_lines(lines, "scrub.log")

        sample = analyzer.samples[0]
        self.assertEqual(sample.role, "timeline_final")
        self.assertEqual(sample.verdict(), Verdict.PASS)
        self.assertIsNotNone(sample.scrub)
        self.assertEqual(sample.scrub.begin_to_first_preview_ms, 20)
        self.assertEqual(sample.scrub.begin_to_end_ms, 80)
        self.assertEqual(len(sample.scrub.previews), 1)
        self.assertEqual(len(analyzer.samples), 1)
        summary = {row.metric: row for row in analyzer.summary_rows()}
        self.assertEqual(summary["scrub_begin_to_first_preview_ms"].p50, 20)
        self.assertEqual(summary["scrub_begin_to_end_ms"].p50, 80)

    def test_public_generations_do_not_merge_when_worker_markers_are_filtered(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + [
            log_line(
                100,
                "generation=7 target_ms=60000 Public final seek accepted",
            ),
            log_line(
                120,
                "generation=7 target_ms=60000 actual_ms=60033 frame_pts_ms=60033 "
                "presented_pre_target_frames=0 public_to_presented_ms=20 "
                "First post-seek presented frame observed",
            ),
            log_line(
                121,
                "generation=7 target_ms=60000 actual_ms=60033 audio_ready=true "
                "public_to_audio_ms=21 Audio play accepted before final seek commit",
            ),
            log_line(
                122,
                "generation=7 target_ms=60000 actual_ms=60033 committed_ms=60000 "
                "presented_pre_target_frames=0 public_to_commit_ms=22 "
                "Final seek commit завершён",
            ),
            log_line(
                123,
                "generation=7 target_ms=60000 position_ms=60000 progress_delta_us=250 "
                "public_to_progress_ms=23 Post-seek position progress observed",
            ),
            log_line(
                200,
                "generation=8 target_ms=180000 Public final seek accepted",
            ),
            log_line(
                220,
                "generation=8 target_ms=180000 actual_ms=180033 frame_pts_ms=180033 "
                "presented_pre_target_frames=0 public_to_presented_ms=20 "
                "First post-seek presented frame observed",
            ),
        ]

        analyzer.parse_lines(lines, "info-filtered-worker.log")

        self.assertEqual(len(analyzer.samples), 2)
        self.assertEqual(analyzer.samples[0].generation, "7")
        self.assertEqual(analyzer.samples[0].target_ms, 60_000)
        self.assertEqual(analyzer.samples[0].actual_ms, 60_033)
        self.assertEqual(analyzer.samples[0].progress_delta_us, 250)
        self.assertNotIn("position_did_not_advance", analyzer.samples[0].order_failures())
        self.assertEqual(analyzer.samples[1].generation, "8")
        self.assertEqual(analyzer.samples[1].target_ms, 180_000)
        self.assertEqual(analyzer.samples[1].actual_ms, 180_033)

    def test_network_overlap_correlates_only_unique_elapsed_candidate(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=0 '
                "Source HTTP request started",
            ),
            log_line(
                100,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=0 '
                "Source HTTP request started",
            ),
            log_line(
                50,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=50 '
                "Source HTTP response headers ready",
            ),
            log_line(
                130,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=30 '
                "Source HTTP response headers ready",
            ),
            log_line(
                60,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=60 '
                "Source HTTP first non-empty body chunk ready",
            ),
            log_line(
                140,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=40 '
                "Source HTTP first non-empty body chunk ready",
            ),
        ]

        analyzer.parse_lines(lines, "network-overlap.log")

        self.assertEqual(len(analyzer.network_requests), 2)
        self.assertEqual(
            [request.headers_ms for request in analyzer.network_requests], [50, 30]
        )
        self.assertEqual(
            [request.first_body_ms for request in analyzer.network_requests], [60, 40]
        )
        self.assertFalse(any(request.ambiguous for request in analyzer.network_requests))

    def test_network_equal_candidates_are_ambiguous_instead_of_guessed(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'operation_kind="bounded_streaming_fetch" Source HTTP request started',
            ),
            log_line(
                0,
                'operation_kind="bounded_streaming_fetch" Source HTTP request started',
            ),
            log_line(
                10,
                'operation_kind="bounded_streaming_fetch" elapsed_milliseconds=10 '
                "Source HTTP response headers ready",
            ),
        ]

        analyzer.parse_lines(lines, "network-ambiguous.log")

        self.assertTrue(all(request.ambiguous for request in analyzer.network_requests))
        self.assertTrue(
            all(request.headers_ms is None for request in analyzer.network_requests)
        )
        self.assertEqual(analyzer.network_summary_rows()[0]["ambiguous_count"], 2)

    def test_legacy_wall_timestamps_remain_diagnostic_not_monotonic_success(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1, 100, 0, 355_000, 200, process_ready_base_ms=300
        )
        lines = [line.replace("public_to_presented_ms=200 ", "") for line in lines]
        lines = [line.replace("public_to_audio_ms=200.5 ", "") for line in lines]
        lines = [line.replace("public_to_enqueue_ms=2.0 ", "") for line in lines]

        analyzer.parse_lines(lines, "legacy-wall.log")

        sample = analyzer.samples[0]
        self.assertIsNone(sample.monotonic_public_to_ready_ms())
        self.assertAlmostEqual(sample.wall_public_to_ready_ms(), 200.5)
        self.assertEqual(sample.verdict(), Verdict.INCOMPLETE)
        self.assertIn("public_to_ready_monotonic_span", sample.missing_gates())

    def test_explicit_surface_audio_and_early_acceptance_own_startup_metric(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + explicit_startup_readiness()
        lines.append(log_line(100, "process_elapsed_ms=100 Startup restore Installed"))
        lines.extend(
            complete_seek(1, 200, 0, 355_000, 300, process_ready_base_ms=500)
        )

        analyzer.parse_lines(lines, "explicit-startup.log")

        run = analyzer.runs[0]
        self.assertEqual(run.process_to_ready_ms(), 45)
        self.assertEqual(run.media_open_to_ready_ms(), 35)
        self.assertEqual(run.verdict(), Verdict.PASS)

    def test_structured_paused_startup_uses_output_ready_without_audio_resume(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        analyzer.parse_lines(
            structured_startup_readiness(
                "Paused",
                include_output_ready=True,
                include_resumed=False,
            ),
            "paused-startup.log",
        )

        run = analyzer.runs[0]
        self.assertEqual(run.process_to_ready_ms(), 46)
        self.assertEqual(run.media_open_to_ready_ms(), 36)
        self.assertIsNotNone(run.audio_output_ready)
        self.assertIsNone(run.audio_playback_resumed)
        self.assertEqual(run.verdict(), Verdict.PASS)

    def test_structured_playing_startup_requires_audio_playback_resumed(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        analyzer.parse_lines(
            structured_startup_readiness(
                "Playing",
                include_output_ready=True,
                include_resumed=True,
            ),
            "playing-startup.log",
        )

        run = analyzer.runs[0]
        self.assertIsNotNone(run.audio_output_ready)
        self.assertIsNotNone(run.audio_playback_resumed)
        self.assertEqual(run.process_to_ready_ms(), 46)
        self.assertEqual(run.verdict(), Verdict.PASS)

    def test_output_ready_does_not_falsely_complete_playing_startup(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        analyzer.parse_lines(
            structured_startup_readiness(
                "Playing",
                include_output_ready=True,
                include_resumed=False,
            ),
            "playing-output-only.log",
        )

        run = analyzer.runs[0]
        self.assertIsNotNone(run.audio_output_ready)
        self.assertIsNone(run.audio_playback_resumed)
        self.assertIsNone(run.process_to_ready_ms())
        self.assertIn("audio_playback_resumed", run.missing_gates())
        self.assertEqual(run.verdict(), Verdict.FAIL)

    def test_structured_final_marker_requires_exact_attempt_target_and_playback(self):
        mismatch_cases = (
            {"final_attempt_id": 8},
            {"final_target": "Restore { target_position: 180s }"},
            {"final_playback": "Paused"},
        )
        for mismatch in mismatch_cases:
            with self.subTest(mismatch=mismatch):
                analyzer = PlaybackAcceptanceAnalyzer()
                analyzer.parse_lines(
                    structured_startup_readiness(
                        "Playing",
                        include_output_ready=True,
                        include_resumed=True,
                        **mismatch,
                    ),
                    "mismatched-final.log",
                )

                run = analyzer.runs[0]
                self.assertIsNone(run.process_to_ready_ms())
                self.assertIsNone(run.structured_final_point)
                self.assertEqual(run.verdict(), Verdict.FAIL)

    def test_explicit_dual_origin_fields_do_not_reinterpret_public_elapsed_as_receipt(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1, 100, 0, 355_000, 1_700, process_ready_base_ms=1_800
        )

        analyzer.parse_lines(lines, "dual-origin.log")

        sample = analyzer.samples[0]
        self.assertEqual(sample.public_to_presented_direct_ms, 1_700)
        self.assertEqual(sample.receipt_to_presented_ms, 1_678)
        self.assertEqual(sample.public_to_audio_direct_ms, 1_700.5)
        self.assertEqual(sample.receipt_to_audio_ms, 1_678.5)
        report_row = sample.to_dict()
        self.assertEqual(report_row["public_to_commit_ms"], 1_701)
        self.assertEqual(report_row["receipt_to_commit_ms"], 1_679)
        self.assertEqual(report_row["public_to_progress_ms"], 1_720)
        self.assertEqual(report_row["receipt_to_progress_ms"], 1_698)
        self.assertEqual(report_row["commit_to_progress_ms"], 19)

    def test_legacy_receipt_only_elapsed_remains_subpath_not_public_latency(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + complete_seek(
            1, 100, 0, 355_000, 200, process_ready_base_ms=300
        )
        lines = [
            line.replace("public_to_presented_ms=200 ", "")
            .replace("receipt_to_presented_ms=178.0 ", "")
            .replace("public_to_audio_ms=200.5 ", "")
            .replace("receipt_to_audio_ms=178.5 ", "")
            for line in lines
        ]

        analyzer.parse_lines(lines, "legacy-receipt-only.log")

        sample = analyzer.samples[0]
        self.assertIsNone(sample.public_to_presented_direct_ms)
        self.assertEqual(sample.receipt_to_presented_ms, 200)
        self.assertIsNone(sample.public_to_audio_direct_ms)
        self.assertEqual(sample.receipt_to_audio_ms, 200.5)
        self.assertEqual(sample.monotonic_public_to_ready_ms(), 222.5)

    def test_timeout_is_failure_even_when_readiness_markers_are_missing(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + [
            log_line(
                100,
                "Player command received command=Seek(SeekRequest { "
                "target: Absolute(MediaTime(355000ms)), mode: Accurate }) "
                "current_position_ms=0",
            ),
            log_line(
                102,
                "generation=1 target_milliseconds=355000 public_to_enqueue_ms=2 "
                "Prepared demux seek request enqueued",
            ),
            log_line(
                1100,
                "generation=1 target_ms=355000 actual_ms=350033 "
                "Final seek commit остановлен по timeout",
            ),
        ]

        analyzer.parse_lines(lines, "timeout.log")

        self.assertEqual(analyzer.samples[0].verdict(), Verdict.FAIL)
        self.assertIn("commit_timeout", analyzer.samples[0].explicit_failures)
        self.assertEqual(analyzer.runs[0].verdict(), Verdict.FAIL)


if __name__ == "__main__":
    unittest.main()

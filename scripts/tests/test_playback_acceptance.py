#!/usr/bin/env python3
"""Deterministic tests offline startup/seek acceptance analyzer."""

from __future__ import annotations

import json
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


def info_log_line(offset_ms: float, message: str) -> str:
    """Добавляет INFO level к production-visible correlation marker-у."""

    timestamp = BASE_TIMESTAMP + timedelta(milliseconds=offset_ms)
    return f"{timestamp.isoformat(timespec='microseconds').replace('+00:00', 'Z')} INFO {message}"


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


def paired_scrub_commands(
    base_ms: float,
    target_ms: float,
    *,
    preview_count: int = 1,
    begin_to_preview_ms: float = 20,
    begin_to_end_ms: float = 80,
    first_command_id: int = 1,
) -> list[str]:
    """Строит current schema pair из одного Rust-owned command envelope."""

    begin_fields = (
        f"scrub_schema_version=1 scrub_command_id={first_command_id} "
        "scrub_stage=begin scrub_target_kind=none scrub_requested_target_ms=0"
    )
    lines = [
        info_log_line(
            base_ms,
            f"{begin_fields} scrub_command_form=dispatch "
            "Player scrub command received",
        ),
        info_log_line(
            base_ms + 1,
            f'kind="seek_acceptance" {begin_fields} '
            "scrub_command_form=acceptance scrub_elapsed_ms=0 "
            "current_position_ms=0 Player scrub command received",
        ),
    ]
    for preview_index in range(preview_count):
        preview_offset_ms = begin_to_preview_ms + preview_index * 10
        preview_command_id = first_command_id + preview_index + 1
        preview_fields = (
            f"scrub_schema_version=1 scrub_command_id={preview_command_id} "
            "scrub_stage=preview scrub_target_kind=absolute "
            f"scrub_requested_target_ms={int(target_ms)}"
        )
        lines.extend(
            [
                info_log_line(
                    base_ms + preview_offset_ms,
                    f"{preview_fields} scrub_command_form=dispatch "
                    "Player scrub command received",
                ),
                info_log_line(
                    base_ms + preview_offset_ms + 1,
                    f'kind="seek_acceptance" {preview_fields} '
                    "scrub_command_form=acceptance "
                    f"scrub_elapsed_ms={preview_offset_ms} "
                    "Player scrub command received",
                ),
            ]
        )
    end_command_id = first_command_id + preview_count + 1
    end_fields = (
        f"scrub_schema_version=1 scrub_command_id={end_command_id} "
        "scrub_stage=end scrub_target_kind=none scrub_requested_target_ms=0"
    )
    lines.extend(
        [
            info_log_line(
                base_ms + begin_to_end_ms,
                f"{end_fields} scrub_command_form=dispatch "
                "Player scrub command received",
            ),
            info_log_line(
                base_ms + begin_to_end_ms + 1,
                f'kind="seek_acceptance" {end_fields} '
                "scrub_command_form=acceptance "
                f"scrub_elapsed_ms={begin_to_end_ms} "
                "Player scrub command received",
            ),
        ]
    )
    return lines


def modern_scrub_form(
    offset_ms: float,
    command_id: int,
    stage: str,
    form: str,
    *,
    target_kind: str = "none",
    target_ms: int = 0,
    scrub_elapsed_ms: int = 0,
) -> str:
    """Строит одну current-schema form для corruption/cross-missing tests."""

    kind = 'kind="seek_acceptance" ' if form == "acceptance" else ""
    return info_log_line(
        offset_ms,
        f"{kind}scrub_schema_version=1 scrub_command_id={command_id} "
        f"scrub_stage={stage} scrub_command_form={form} "
        f"scrub_target_kind={target_kind} scrub_requested_target_ms={target_ms} "
        f"scrub_elapsed_ms={scrub_elapsed_ms} Player scrub command received",
    )


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

    def test_bounded_terminal_cancelled_proves_exact_rapid_request(self):
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
                'received_body_bytes=0 operation_kind="bounded_streaming_fetch" '
                "request_id=request-7 elapsed_milliseconds=0 "
                "Source HTTP request started",
            ),
            log_line(
                105,
                "supersede_after_ms=5 Player command received "
                "command=Seek(SeekRequest { target: Absolute(MediaTime(550000ms)), "
                "mode: Accurate }) current_position_ms=0",
            ),
            log_line(
                106,
                'received_bytes=0 outcome="cancelled" request_id=request-7 '
                "elapsed_milliseconds=46 Bounded HTTP request terminal",
            ),
        ]

        analyzer.parse_lines(lines, "rapid-terminal-cancel.log")

        request = analyzer.network_requests[0]
        self.assertEqual(request.safe_request_id, "request-7")
        self.assertEqual(request.cancelled_ms, 46)
        self.assertEqual(request.body_bytes, 0)
        self.assertEqual(request.terminal_outcome, "cancelled")
        self.assertFalse(request.ambiguous)
        self.assertEqual(
            analyzer.samples[0].supersede_network_status,
            "cancelled_or_completed_before_supersede",
        )

    def test_bounded_terminal_without_id_never_uses_unique_candidate_fallback(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'request_id=request-only operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                5,
                'outcome="cancelled" received_bytes=7 elapsed_milliseconds=5 '
                "Bounded HTTP request terminal",
            ),
        ]

        analyzer.parse_lines(lines, "bounded-terminal-missing-id.log")

        request = analyzer.network_requests[0]
        self.assertIsNone(request.cancelled_ms)
        self.assertEqual(request.terminal_outcome, "")
        self.assertTrue(request.ambiguous)
        self.assertEqual(
            [anomaly.kind for anomaly in analyzer.network_terminal_anomalies],
            ["missing_request_id"],
        )
        self.assertTrue(analyzer.network_terminal_anomalies[0].proof_relevant())
        self.assertEqual(
            analyzer.network_anomaly_summary(),
            {
                "anomaly_count": 1,
                "proof_relevant_anomaly_count": 1,
                "by_kind": {"missing_request_id": 1},
            },
        )
        self.assertEqual(analyzer.network_summary_rows()[0]["anomaly_count"], 1)

    def test_old_terminal_string_keeps_legacy_unique_candidate_fallback(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'request_id=request-old operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                5,
                "received_bytes=7 elapsed_milliseconds=5 "
                "Source HTTP request cancelled",
            ),
        ]

        analyzer.parse_lines(lines, "legacy-terminal-fallback.log")

        self.assertEqual(analyzer.network_requests[0].cancelled_ms, 5)
        self.assertEqual(analyzer.network_requests[0].body_bytes, 7)
        self.assertEqual(analyzer.network_terminal_anomalies, [])

    def test_bounded_terminal_outcomes_do_not_alias_error_to_cancellation(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'request_id=request-complete operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                1,
                'operation_kind="bounded_streaming_fetch" request_id=request-error '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                2,
                "received_bytes=128 elapsed_milliseconds=7 "
                'outcome="complete" request_id=request-complete '
                "Bounded HTTP request terminal",
            ),
            log_line(
                3,
                'error_category="timeout" received_bytes=64 request_id=request-error '
                'outcome="error" elapsed_milliseconds=9 '
                "Bounded HTTP request terminal",
            ),
        ]

        analyzer.parse_lines(lines, "typed-terminal-outcomes.log")

        completed, failed = analyzer.network_requests
        self.assertEqual(completed.body_complete_ms, 7)
        self.assertEqual(completed.body_bytes, 128)
        self.assertEqual(completed.terminal_outcome, "complete")
        self.assertIsNone(completed.cancelled_ms)
        self.assertEqual(failed.terminal_ms, 9)
        self.assertEqual(failed.body_bytes, 64)
        self.assertEqual(failed.terminal_outcome, "error")
        self.assertEqual(failed.terminal_error_category, "timeout")
        self.assertIsNone(failed.cancelled_ms)

    def test_explicit_request_id_is_fail_closed_even_without_operation_kind(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'request_id=request-1 operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                0,
                'request_id=request-2 operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                5,
                'outcome="cancelled" received_bytes=17 request_id=request-2 '
                "elapsed_milliseconds=5 Bounded HTTP request terminal",
            ),
            log_line(
                6,
                'outcome="cancelled" request_id=request-missing received_bytes=99 '
                "elapsed_milliseconds=6 Bounded HTTP request terminal",
            ),
        ]

        analyzer.parse_lines(lines, "exact-request-id.log")

        first, second = analyzer.network_requests
        self.assertIsNone(first.cancelled_ms)
        self.assertEqual(second.cancelled_ms, 5)
        self.assertEqual(second.body_bytes, 17)
        self.assertFalse(first.ambiguous)
        self.assertFalse(second.ambiguous)

    def test_unknown_terminal_request_id_is_reported_and_cannot_prove_supersede(self):
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
                'request_id=request-owned operation_kind="bounded_streaming_fetch" '
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
                'outcome="cancelled" request_id=request-unknown received_bytes=0 '
                "elapsed_milliseconds=3 Bounded HTTP request terminal",
            ),
        ]

        analyzer.parse_lines(lines, "unknown-terminal-request.log")

        self.assertIsNone(analyzer.network_requests[0].cancelled_ms)
        self.assertEqual(
            analyzer.samples[0].supersede_network_status,
            "cancellation_unproven",
        )
        self.assertEqual(len(analyzer.network_terminal_anomalies), 1)
        anomaly = analyzer.network_terminal_anomalies[0]
        self.assertEqual(anomaly.kind, "unknown_request_id")
        self.assertEqual(anomaly.safe_request_id, "request-unknown")
        self.assertEqual(anomaly.outcome, "cancelled")

    def test_missing_and_unsupported_terminal_outcomes_are_explicit_anomalies(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                0,
                'request_id=request-missing operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                1,
                'request_id=request-unsupported operation_kind="bounded_streaming_fetch" '
                "elapsed_milliseconds=0 Source HTTP request started",
            ),
            log_line(
                5,
                "request_id=request-missing received_bytes=11 elapsed_milliseconds=5 "
                "Bounded HTTP request terminal",
            ),
            log_line(
                7,
                'outcome="aborted_by_owner" request_id=request-unsupported '
                "received_bytes=13 elapsed_milliseconds=6 "
                "Bounded HTTP request terminal",
            ),
        ]

        analyzer.parse_lines(lines, "invalid-terminal-outcomes.log")

        self.assertEqual(
            [anomaly.kind for anomaly in analyzer.network_terminal_anomalies],
            ["missing_outcome", "unsupported_outcome"],
        )
        self.assertEqual(
            [anomaly.outcome for anomaly in analyzer.network_terminal_anomalies],
            [None, "aborted_by_owner"],
        )
        self.assertTrue(all(request.ambiguous for request in analyzer.network_requests))
        self.assertTrue(
            all(request.cancelled_ms is None for request in analyzer.network_requests)
        )
        self.assertEqual(
            [request.body_bytes for request in analyzer.network_requests],
            [11, 13],
        )
        serialized_anomalies = analyzer.to_dict()["network_terminal_anomalies"]
        self.assertEqual(
            [row["anomaly_kind"] for row in serialized_anomalies],
            ["missing_outcome", "unsupported_outcome"],
        )
        self.assertEqual(serialized_anomalies[1]["terminal_outcome"], "aborted_by_owner")
        self.assertIsInstance(json.dumps(serialized_anomalies), str)

    def test_terminal_anomaly_report_is_additive_json_and_defaults_empty(self):
        legacy_analyzer = PlaybackAcceptanceAnalyzer(scenario="legacy-consumer")
        legacy_analyzer.parse_lines(process_prefix(), "legacy.log")

        legacy_report = legacy_analyzer.to_dict()

        self.assertEqual(legacy_report["network_terminal_anomalies"], [])
        self.assertEqual(legacy_report["scrub_command_anomalies"], [])
        self.assertEqual(
            legacy_report["network_anomaly_summary"],
            {
                "anomaly_count": 0,
                "proof_relevant_anomaly_count": 0,
                "by_kind": {},
            },
        )
        self.assertEqual(
            legacy_report["scrub_anomaly_summary"],
            {
                "anomaly_count": 0,
                "proof_relevant_anomaly_count": 0,
                "by_kind": {},
            },
        )
        self.assertTrue(
            {
                "scenario",
                "startup_runs",
                "seek_samples",
                "network_requests",
                "startup_summary",
                "seek_summary",
                "network_summary",
                "production_marker_requirements",
            }.issubset(legacy_report)
        )
        self.assertIsInstance(json.dumps(legacy_report), str)

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

    def test_info_scrub_dispatch_and_acceptance_count_each_command_once(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = process_prefix() + [
            log_line(
                99,
                "command=BeginScrub { live_scrub: None } "
                "Player scrub command debug received",
            )
        ]
        lines.extend(paired_scrub_commands(100, 355_000))
        lines.append(
            log_line(
                181,
                "generation=1 target_ms=355000 Public final seek accepted",
            )
        )
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

        analyzer.parse_lines(lines, "paired-scrub.log")

        self.assertEqual(len(analyzer.samples), 1)
        sample = analyzer.samples[0]
        self.assertFalse(sample.superseded)
        self.assertEqual(len(sample.scrub.previews), 1)
        self.assertEqual(sample.scrub.begin_to_first_preview_ms, 20)
        self.assertEqual(sample.scrub.begin_to_end_ms, 80)
        self.assertEqual(sample.verdict(), Verdict.PASS)
        self.assertEqual(analyzer.scrub_command_anomalies, [])

    def test_two_identical_real_preview_pairs_remain_two_previews(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = paired_scrub_commands(100, 355_000, preview_count=2)

        analyzer.parse_lines(lines, "identical-preview-pairs.log")

        self.assertEqual(len(analyzer.samples), 1)
        self.assertIsNotNone(analyzer.samples[0].scrub)
        self.assertEqual(len(analyzer.samples[0].scrub.previews), 2)
        self.assertEqual(analyzer.scrub_command_anomalies, [])

    def test_modern_cross_missing_same_target_never_cross_pairs_ids(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            modern_scrub_form(100, 1, "begin", "dispatch"),
            modern_scrub_form(101, 1, "begin", "acceptance"),
            modern_scrub_form(
                120,
                2,
                "preview",
                "dispatch",
                target_kind="absolute",
                target_ms=355_000,
                scrub_elapsed_ms=20,
            ),
            log_line(125, "unrelated diagnostic between exact command forms"),
            modern_scrub_form(
                130,
                3,
                "preview",
                "acceptance",
                target_kind="absolute",
                target_ms=355_000,
                scrub_elapsed_ms=30,
            ),
            modern_scrub_form(180, 4, "end", "dispatch", scrub_elapsed_ms=80),
            modern_scrub_form(
                181,
                4,
                "end",
                "acceptance",
                scrub_elapsed_ms=80,
            ),
        ]

        analyzer.parse_lines(lines, "modern-cross-missing.log")

        self.assertEqual(len(analyzer.samples), 1)
        self.assertEqual(len(analyzer.samples[0].scrub.previews), 2)
        self.assertEqual(
            [anomaly.kind for anomaly in analyzer.scrub_command_anomalies],
            ["missing_acceptance_form", "missing_dispatch_form"],
        )
        self.assertEqual(
            [anomaly.command_id for anomaly in analyzer.scrub_command_anomalies],
            [2, 3],
        )
        self.assertEqual(analyzer.samples[0].verdict(), Verdict.INCOMPLETE)

    def test_modern_duplicate_stage_and_target_mismatch_are_explicit(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            modern_scrub_form(100, 1, "begin", "dispatch"),
            modern_scrub_form(101, 1, "begin", "dispatch"),
            modern_scrub_form(102, 1, "end", "acceptance"),
            modern_scrub_form(
                120,
                2,
                "preview",
                "dispatch",
                target_kind="absolute",
                target_ms=1000,
            ),
            modern_scrub_form(
                121,
                2,
                "preview",
                "acceptance",
                target_kind="absolute",
                target_ms=2000,
            ),
        ]

        analyzer.parse_lines(lines, "modern-corrupt-pairs.log")

        anomaly_kinds = [
            anomaly.kind for anomaly in analyzer.scrub_command_anomalies
        ]
        self.assertIn("duplicate_scrub_command_form", anomaly_kinds)
        self.assertIn("scrub_stage_mismatch", anomaly_kinds)
        self.assertIn("scrub_target_mismatch", anomaly_kinds)
        self.assertIn("missing_acceptance_form", anomaly_kinds)
        serialized = analyzer.to_dict()["scrub_command_anomalies"]
        self.assertTrue(all(row["proof_relevant"] for row in serialized))

    def test_modern_missing_and_non_monotonic_ids_are_explicit_anomalies(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            modern_scrub_form(100, 2, "begin", "dispatch"),
            modern_scrub_form(101, 2, "begin", "acceptance"),
            modern_scrub_form(110, 1, "end", "dispatch"),
            modern_scrub_form(111, 1, "end", "acceptance"),
            log_line(
                120,
                "scrub_schema_version=1 scrub_stage=preview "
                "scrub_command_form=dispatch scrub_target_kind=absolute "
                "scrub_requested_target_ms=355000 Player scrub command received",
            ),
        ]

        analyzer.parse_lines(lines, "modern-invalid-ids.log")

        self.assertEqual(
            [anomaly.kind for anomaly in analyzer.scrub_command_anomalies],
            ["non_monotonic_scrub_command_id", "missing_scrub_command_id"],
        )
        self.assertTrue(analyzer.has_proof_relevant_anomalies())

    def test_adjacent_modern_ids_above_float_precision_remain_distinct(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        first_id = 9_007_199_254_740_992
        second_id = first_id + 1
        lines = [
            modern_scrub_form(100, first_id, "begin", "dispatch"),
            modern_scrub_form(101, first_id, "begin", "acceptance"),
            modern_scrub_form(
                110,
                second_id,
                "preview",
                "dispatch",
                target_kind="absolute",
                target_ms=355_000,
            ),
            modern_scrub_form(
                111,
                second_id,
                "preview",
                "acceptance",
                target_kind="absolute",
                target_ms=355_000,
            ),
            modern_scrub_form(120, second_id + 1, "end", "dispatch"),
            modern_scrub_form(121, second_id + 1, "end", "acceptance"),
        ]

        analyzer.parse_lines(lines, "exact-large-scrub-ids.log")

        self.assertEqual(analyzer.scrub_command_anomalies, [])
        self.assertEqual(len(analyzer.samples), 1)
        self.assertEqual(len(analyzer.samples[0].scrub.previews), 1)
        report = analyzer.to_dict()
        self.assertEqual(report["scrub_anomaly_summary"]["anomaly_count"], 0)

    def test_modern_unsigned_decimal_fields_reject_non_decimal_and_overflow(self):
        invalid_values = ("1.5", "1.9", "nan", "inf", "-1", "1e3", "malformed")
        field_cases = (
            (
                "scrub_schema_version",
                "invalid_scrub_schema_version_integer",
                str(1 << 64),
                "scrub_schema_version_overflow",
            ),
            (
                "scrub_command_id",
                "invalid_scrub_command_id_integer",
                str(1 << 64),
                "scrub_command_id_overflow",
            ),
            (
                "scrub_requested_target_ms",
                "invalid_scrub_target_integer",
                str(1 << 128),
                "scrub_target_overflow",
            ),
        )

        for field_name, invalid_kind, overflow_value, overflow_kind in field_cases:
            for invalid_value in invalid_values:
                with self.subTest(field=field_name, value=invalid_value):
                    fields = {
                        "scrub_schema_version": "1",
                        "scrub_command_id": "1",
                        "scrub_requested_target_ms": "355000",
                    }
                    fields[field_name] = invalid_value
                    analyzer = PlaybackAcceptanceAnalyzer()
                    analyzer.parse_lines(
                        [
                            log_line(
                                100,
                                f"scrub_schema_version={fields['scrub_schema_version']} "
                                f"scrub_command_id={fields['scrub_command_id']} "
                                "scrub_stage=preview scrub_command_form=dispatch "
                                "scrub_target_kind=absolute "
                                "scrub_requested_target_ms="
                                f"{fields['scrub_requested_target_ms']} "
                                "Player scrub command received",
                            )
                        ],
                        f"invalid-{field_name}-{invalid_value}.log",
                    )
                    self.assertEqual(
                        [
                            anomaly.kind
                            for anomaly in analyzer.scrub_command_anomalies
                        ],
                        [invalid_kind],
                    )
                    self.assertTrue(analyzer.has_proof_relevant_anomalies())

            for overflow_case in (overflow_value, "9" * 5_000):
                with self.subTest(field=field_name, value="overflow"):
                    fields = {
                        "scrub_schema_version": "1",
                        "scrub_command_id": "1",
                        "scrub_requested_target_ms": "355000",
                    }
                    fields[field_name] = overflow_case
                    analyzer = PlaybackAcceptanceAnalyzer()
                    analyzer.parse_lines(
                        [
                            log_line(
                                100,
                                f"scrub_schema_version={fields['scrub_schema_version']} "
                                f"scrub_command_id={fields['scrub_command_id']} "
                                "scrub_stage=preview scrub_command_form=dispatch "
                                "scrub_target_kind=absolute "
                                "scrub_requested_target_ms="
                                f"{fields['scrub_requested_target_ms']} "
                                "Player scrub command received",
                            )
                        ],
                        f"overflow-{field_name}.log",
                    )
                    self.assertEqual(
                        [
                            anomaly.kind
                            for anomaly in analyzer.scrub_command_anomalies
                        ],
                        [overflow_kind],
                    )

    def test_typed_only_legacy_scrub_remains_backward_compatible(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                100,
                'kind="seek_acceptance" current_position_ms=0 '
                "Player command received command=BeginScrub",
            ),
            log_line(
                120,
                'kind="seek_acceptance" target_ms=355000 begin_to_preview_ms=20 '
                "Player command received command=PreviewScrub",
            ),
            log_line(
                180,
                'kind="seek_acceptance" begin_to_end_ms=80 '
                "Player command received command=EndScrub",
            ),
        ]

        analyzer.parse_lines(lines, "typed-only-legacy.log")

        self.assertEqual(len(analyzer.samples), 1)
        self.assertEqual(len(analyzer.samples[0].scrub.previews), 1)
        self.assertEqual(analyzer.scrub_command_anomalies, [])

    def test_two_unmatched_legacy_previews_are_not_collapsed(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(100, "Player command received command=BeginScrub"),
            log_line(
                120,
                "Player command received command=PreviewScrub { request: SeekRequest { "
                "target: Absolute(MediaTime(355000ms)), mode: Accurate } }",
            ),
            log_line(
                130,
                "Player command received command=PreviewScrub { request: SeekRequest { "
                "target: Absolute(MediaTime(355000ms)), mode: Accurate } }",
            ),
            log_line(180, "Player command received command=EndScrub"),
        ]

        analyzer.parse_lines(lines, "unmatched-legacy-previews.log")

        self.assertEqual(len(analyzer.samples), 1)
        self.assertEqual(len(analyzer.samples[0].scrub.previews), 2)

    def test_idless_mixed_forms_are_anomalous_and_use_only_first_legacy_family(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                100,
                'kind="seek_acceptance" current_position_ms=0 '
                "Player command received command=BeginScrub",
            ),
            log_line(
                120,
                'kind="seek_acceptance" target_ms=355000 begin_to_preview_ms=20 '
                "Player command received command=PreviewScrub",
            ),
            log_line(
                130,
                "Player command received command=PreviewScrub { request: SeekRequest { "
                "target: Absolute(MediaTime(355000ms)), mode: Accurate } }",
            ),
            log_line(
                180,
                'kind="seek_acceptance" begin_to_end_ms=80 '
                "Player command received command=EndScrub",
            ),
        ]

        analyzer.parse_lines(lines, "typed-then-raw-cross-missing.log")

        self.assertEqual(len(analyzer.samples), 1)
        self.assertEqual(len(analyzer.samples[0].scrub.previews), 1)
        self.assertEqual(
            [anomaly.kind for anomaly in analyzer.scrub_command_anomalies],
            ["legacy_mixed_forms_without_id"],
        )
        self.assertIn(
            "scrub_command_correlation",
            analyzer.samples[0].missing_gates(),
        )
        self.assertEqual(analyzer.samples[0].verdict(), Verdict.INCOMPLETE)

    def test_non_adjacent_idless_mixed_forms_never_use_lifo_pairing(self):
        analyzer = PlaybackAcceptanceAnalyzer()
        lines = [
            log_line(
                100,
                'kind="seek_acceptance" current_position_ms=0 '
                "Player command received command=BeginScrub",
            ),
            log_line(
                110,
                "Player command received command=EndScrub { policy: LatestPreview }",
            ),
            log_line(
                120,
                "Player command received command=EndScrub { policy: LatestPreview }",
            ),
            log_line(125, "Source HTTP response headers ready elapsed_milliseconds=4"),
            log_line(
                130,
                'kind="seek_acceptance" current_position_ms=222 '
                'begin_to_end_ms=30 '
                "Player command received command=EndScrub",
            ),
        ]

        analyzer.parse_lines(lines, "non-adjacent-lifo-end.log")

        self.assertEqual(len(analyzer.samples), 1)
        self.assertEqual(analyzer.samples[0].origin_ms, 222)
        self.assertEqual(analyzer.samples[0].scrub.begin_to_end_ms, 30)
        self.assertEqual(
            [anomaly.kind for anomaly in analyzer.scrub_command_anomalies],
            ["legacy_mixed_forms_without_id"],
        )
        self.assertEqual(analyzer.samples[0].verdict(), Verdict.INCOMPLETE)

    def test_ten_paired_scrubs_keep_exact_warm_final_count(self):
        analyzer = PlaybackAcceptanceAnalyzer(scenario="warm-scrub-10")
        lines = process_prefix()
        previous_target_ms = 0.0
        for index in range(1, 11):
            base_ms = 1_000.0 * index
            target_ms = 50_000.0 * index
            lines.extend(
                paired_scrub_commands(
                    base_ms,
                    target_ms,
                    first_command_id=(index - 1) * 3 + 1,
                )
            )
            lines.append(
                log_line(
                    base_ms + 81,
                    f"generation={index} target_ms={target_ms} "
                    "Public final seek accepted",
                )
            )
            lines.extend(
                complete_seek(
                    index,
                    base_ms + 80,
                    previous_target_ms,
                    target_ms,
                    100 + index,
                    include_public=False,
                )
            )
            previous_target_ms = target_ms

        analyzer.parse_lines(lines, "warm-paired-scrub-10.log")

        self.assertEqual(len(analyzer.samples), 10)
        self.assertTrue(all(not sample.superseded for sample in analyzer.samples))
        self.assertTrue(
            all(len(sample.scrub.previews) == 1 for sample in analyzer.samples)
        )
        self.assertEqual(analyzer.summary_rows()[0].eligible_count, 10)

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

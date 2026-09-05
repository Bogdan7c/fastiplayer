#!/usr/bin/env python3
"""Manual offline runner startup/seek performance acceptance report."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import TextIO

from playback_acceptance import Verdict
from playback_acceptance_parser import PlaybackAcceptanceAnalyzer
from playback_acceptance_summary import format_summary_value


def parse_args(argv: list[str]) -> argparse.Namespace:
    """Создаёт dependency-free CLI, который только читает переданные logs."""

    parser = argparse.ArgumentParser(
        description=(
            "Analyze fastiplayer startup/seek acceptance telemetry without "
            "launching the application."
        )
    )
    parser.add_argument("logs", nargs="+", type=Path, help="Existing tracing log files.")
    parser.add_argument(
        "--scenario",
        default="",
        help="Human-readable scenario label stored in the report.",
    )
    parser.add_argument(
        "--format",
        choices=("json", "table"),
        default="table",
        help="Machine-readable JSON or compact manual table.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help=(
            "Return 1 for FAIL/INCOMPLETE final evidence or proof-relevant "
            "network/scrub/HLS manifest-selection telemetry anomalies."
        ),
    )
    return parser.parse_args(argv)


def write_table(analyzer: PlaybackAcceptanceAnalyzer, output: TextIO) -> None:
    """Печатает компактный отчёт, оставляя полный gate list видимым."""

    output.write("STARTUP\n")
    output.write(
        "source | process_ready_ms | media_open_ready_ms | wall_diagnostic_ms | verdict | missing/failures\n"
    )
    for run in analyzer.runs:
        missing_or_failures = [*run.missing_gates(), *run.explicit_failures]
        output.write(
            " | ".join(
                (
                    run.source,
                    format_summary_value(run.process_to_ready_ms()),
                    format_summary_value(run.media_open_to_ready_ms()),
                    format_summary_value(run.wall_process_to_ready_ms()),
                    run.verdict().value,
                    ",".join(missing_or_failures),
                )
            )
            + "\n"
        )

    output.write("\nSTARTUP SUMMARY\n")
    output.write(
        "metric | observed | eligible | failed | incomplete | p50 | p95 | max | clock\n"
    )
    for row in analyzer.startup_summary_rows():
        output.write(
            " | ".join(
                (
                    str(row["metric"]),
                    str(row["observed_count"]),
                    str(row["eligible_count"]),
                    str(row["failed_count"]),
                    str(row["incomplete_count"]),
                    format_summary_value(row["p50"]),
                    format_summary_value(row["p95"]),
                    format_summary_value(row["max"]),
                    str(row["clock_basis"]),
                )
            )
            + "\n"
        )

    output.write("\nSEEK\n")
    output.write(
        "seq | role | direction | target_ms | public_ready_ms | wall_diagnostic_ms | verdict | missing/failures\n"
    )
    for sample in analyzer.samples:
        missing_or_failures = [
            *sample.missing_gates(),
            *sample.explicit_failures,
            *sample.order_failures(),
        ]
        output.write(
            " | ".join(
                (
                    str(sample.sequence),
                    sample.role,
                    sample.direction,
                    format_summary_value(sample.target_ms),
                    format_summary_value(sample.monotonic_public_to_ready_ms()),
                    format_summary_value(sample.wall_public_to_ready_ms()),
                    sample.verdict().value,
                    ",".join(missing_or_failures),
                )
            )
            + "\n"
        )

    output.write("\nSEEK SUMMARY\n")
    output.write(
        "metric | observed | eligible | failed | incomplete | superseded | p50 | p95 | max | clock\n"
    )
    for row in analyzer.summary_rows():
        output.write(
            " | ".join(
                (
                    row.metric,
                    str(row.observed_count),
                    str(row.eligible_count),
                    str(row.failed_count),
                    str(row.incomplete_count),
                    str(row.superseded_count),
                    format_summary_value(row.p50),
                    format_summary_value(row.p95),
                    format_summary_value(row.maximum),
                    row.clock_basis,
                )
            )
            + "\n"
        )

    output.write("\nHLS MANIFEST SELECTIONS\n")
    output.write(
        "seq | source | class | phase | role | selection id | requested_ms | "
        "selected_segment_ms | actual_anchor_ms | decode_anchor_ms | "
        "media/discontinuity/global/epoch/restart | policy | anchor kind | status\n"
    )
    for selection in analyzer.hls_manifest_selections:
        indexes = "/".join(
            str(value)
            for value in (
                selection.media_sequence,
                selection.discontinuity_sequence,
                selection.manifest_segment_index,
                selection.epoch_index,
                selection.restart_segment_index,
            )
        )
        status = (
            "valid" if selection.valid() else ",".join(selection.anomaly_kinds)
        )
        output.write(
            " | ".join(
                (
                    str(selection.sequence),
                    selection.source,
                    selection.operation_class(),
                    selection.phase,
                    selection.component_role,
                    str(selection.manifest_selection_id),
                    str(selection.requested_target_ms),
                    f"[{selection.segment_start_ms},{selection.segment_end_ms})",
                    str(selection.actual_anchor_ms),
                    str(selection.actual_decode_anchor_ms),
                    indexes,
                    selection.landing_policy,
                    selection.anchor_kind,
                    status,
                )
            )
            + "\n"
        )

    output.write("\nHLS MANIFEST SELECTION SUMMARY\n")
    output.write("class | phase | role | selections | valid | anomalies\n")
    for row in analyzer.hls_manifest_selection_summary_rows():
        output.write(
            " | ".join(
                (
                    str(row["operation_class"]),
                    str(row["phase"]),
                    str(row["component_role"]),
                    str(row["selection_count"]),
                    str(row["valid_count"]),
                    str(row["anomaly_count"]),
                )
            )
            + "\n"
        )

    hls_anomaly_summary = analyzer.hls_manifest_selection_anomaly_summary()
    output.write("\nHLS MANIFEST SELECTION ANOMALY SUMMARY\n")
    output.write("anomalies | proof anomalies | by kind\n")
    output.write(
        " | ".join(
            (
                str(hls_anomaly_summary["anomaly_count"]),
                str(hls_anomaly_summary["proof_relevant_anomaly_count"]),
                ",".join(
                    f"{kind}:{count}"
                    for kind, count in hls_anomaly_summary["by_kind"].items()
                ),
            )
        )
        + "\n"
    )

    output.write("\nHLS MANIFEST SELECTION ANOMALIES\n")
    output.write(
        "source | line | kind | field | record | selection id | role | impact\n"
    )
    for anomaly in analyzer.hls_manifest_selection_anomalies:
        output.write(
            " | ".join(
                (
                    anomaly.source,
                    str(anomaly.line_number),
                    anomaly.kind,
                    anomaly.field or "",
                    (
                        str(anomaly.record_sequence)
                        if anomaly.record_sequence is not None
                        else ""
                    ),
                    (
                        str(anomaly.manifest_selection_id)
                        if anomaly.manifest_selection_id is not None
                        else ""
                    ),
                    anomaly.component_role or "",
                    anomaly.impact,
                )
            )
            + "\n"
        )

    output.write("\nNETWORK SUMMARY\n")
    output.write(
        "metric | observed | requests | ambiguous | anomalies | proof anomalies | "
        "p50 | p95 | max | clock\n"
    )
    for row in analyzer.network_summary_rows():
        output.write(
            " | ".join(
                (
                    str(row["metric"]),
                    str(row["observed_count"]),
                    str(row["request_count"]),
                    str(row["ambiguous_count"]),
                    str(row["anomaly_count"]),
                    str(row["proof_relevant_anomaly_count"]),
                    format_summary_value(row["p50"]),
                    format_summary_value(row["p95"]),
                    format_summary_value(row["max"]),
                    str(row["clock_basis"]),
                )
            )
            + "\n"
        )

    output.write("\nNETWORK ANOMALIES\n")
    output.write("source | line | kind | request | outcome | owner seek | impact\n")
    for anomaly in analyzer.network_terminal_anomalies:
        output.write(
            " | ".join(
                (
                    anomaly.source,
                    str(anomaly.line_number),
                    anomaly.kind,
                    anomaly.safe_request_id or "",
                    anomaly.outcome or "",
                    (
                        str(anomaly.owner_seek_sequence)
                        if anomaly.owner_seek_sequence is not None
                        else ""
                    ),
                    anomaly.impact,
                )
            )
            + "\n"
        )

    output.write("\nSCRUB COMMAND ANOMALIES\n")
    output.write("source | line | kind | command id | form | stage | proof relevant\n")
    for anomaly in analyzer.scrub_command_anomalies:
        output.write(
            " | ".join(
                (
                    anomaly.source,
                    str(anomaly.line_number),
                    anomaly.kind,
                    str(anomaly.command_id) if anomaly.command_id is not None else "",
                    anomaly.form or "",
                    anomaly.stage or "",
                    str(anomaly.proof_relevant).lower(),
                )
            )
            + "\n"
        )


def has_blocking_outcome(analyzer: PlaybackAcceptanceAnalyzer) -> bool:
    """Superseded operations допустимы; final FAIL/INCOMPLETE блокируют acceptance."""

    if analyzer.has_proof_relevant_anomalies():
        return True
    if any(run.verdict() != Verdict.PASS for run in analyzer.runs):
        return True
    return any(
        sample.verdict() in {Verdict.FAIL, Verdict.INCOMPLETE}
        for sample in analyzer.samples
        if sample.role != "preview_scrub"
    )


def main(argv: list[str]) -> int:
    """Разбирает logs, печатает report и возвращает strict acceptance status."""

    args = parse_args(argv)
    analyzer = PlaybackAcceptanceAnalyzer(scenario=args.scenario)
    for path in args.logs:
        analyzer.parse_path(path)

    if args.format == "json":
        json.dump(analyzer.to_dict(), sys.stdout, ensure_ascii=False, indent=2)
        sys.stdout.write("\n")
    else:
        write_table(analyzer, sys.stdout)
    return 1 if args.strict and has_blocking_outcome(analyzer) else 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))

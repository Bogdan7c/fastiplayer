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
            "Analyze rustiplayer startup/seek acceptance telemetry without "
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
        help="Return 1 for FAIL/INCOMPLETE final samples or startup runs.",
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

    output.write("\nNETWORK SUMMARY\n")
    output.write("metric | observed | requests | ambiguous | p50 | p95 | max | clock\n")
    for row in analyzer.network_summary_rows():
        output.write(
            " | ".join(
                (
                    str(row["metric"]),
                    str(row["observed_count"]),
                    str(row["request_count"]),
                    str(row["ambiguous_count"]),
                    format_summary_value(row["p50"]),
                    format_summary_value(row["p95"]),
                    format_summary_value(row["max"]),
                    str(row["clock_basis"]),
                )
            )
            + "\n"
        )


def has_blocking_outcome(analyzer: PlaybackAcceptanceAnalyzer) -> bool:
    """Superseded operations допустимы; final FAIL/INCOMPLETE блокируют acceptance."""

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

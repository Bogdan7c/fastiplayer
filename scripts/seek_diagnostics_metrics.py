"""Typed latency metrics и deterministic summary для seek diagnostics."""

from __future__ import annotations

import csv
import json
import math
from dataclasses import dataclass
from typing import Iterable, TextIO


SUMMARY_FIELDNAMES = ["metric", "count", "p50", "p95", "max", "percentile_method"]


@dataclass
class SeekTimingMetrics:
    """Monotonic интервалы и manifest evidence одной seek transaction."""

    worker_receipted: bool = False
    worker_round_trip_ms: float | None = None
    network_headers_ms: float | None = None
    network_first_body_ms: float | None = None
    network_body_complete_ms: float | None = None
    receipt_to_presented_ms: float | None = None
    receipt_to_audio_ms: float | None = None
    manifest_candidate_count: int = 0
    manifest_candidate_elapsed_ms: float = 0.0
    manifest_anchor_proven: bool = False
    manifest_candidate_accepted: bool = False

    def network_first_byte_ms(self) -> float | None:
        """Предпочитает первый body byte, сохраняя headers-only диагностику."""

        if self.network_first_body_ms is not None:
            return self.network_first_body_ms
        return self.network_headers_ms

    def public_to_presented_ms(self) -> float | None:
        """Суммирует public enqueue→receipt и receipt→presentation интервалы."""

        return sum_optional(self.worker_round_trip_ms, self.receipt_to_presented_ms)

    def public_to_audio_ms(self) -> float | None:
        """Суммирует public enqueue→receipt и receipt→audio интервалы."""

        return sum_optional(self.worker_round_trip_ms, self.receipt_to_audio_ms)


def sum_optional(left: float | None, right: float | None) -> float | None:
    """Суммирует интервалы только когда обе monotonic границы известны."""

    if left is None or right is None:
        return None
    return left + right


def format_milliseconds(value: float | None) -> str:
    """Печатает миллисекунды компактно, не теряя дробную часть."""

    if value is None:
        return ""
    return f"{value:.3f}".rstrip("0").rstrip(".")


def nearest_rank(values: list[float], percentile: float) -> float | None:
    """Возвращает nearest-rank percentile: sorted[ceil(p*n)-1]."""

    if not values:
        return None
    ordered = sorted(values)
    rank = max(1, math.ceil(percentile * len(ordered)))
    return ordered[min(rank, len(ordered)) - 1]


def summarize_metric(metric: str, values: list[float]) -> dict[str, str]:
    """Строит стабильную summary-строку одной latency метрики."""

    return {
        "metric": metric,
        "count": str(len(values)),
        "p50": format_milliseconds(nearest_rank(values, 0.50)),
        "p95": format_milliseconds(nearest_rank(values, 0.95)),
        "max": format_milliseconds(max(values) if values else None),
        "percentile_method": "nearest-rank",
    }


def build_summary(completed: Iterable[SeekTimingMetrics]) -> list[dict[str, str]]:
    """Считает обе public latency метрики только по completed success."""

    completed_metrics = list(completed)
    presented_values = [
        value
        for timing in completed_metrics
        if (value := timing.public_to_presented_ms()) is not None
    ]
    audio_values = [
        value
        for timing in completed_metrics
        if (value := timing.public_to_audio_ms()) is not None
    ]
    return [
        summarize_metric("public_to_presented_ms", presented_values),
        summarize_metric("public_to_audio_ms", audio_values),
    ]


def write_summary(
    rows: list[dict[str, str]], output_format: str, output: TextIO
) -> None:
    """Пишет summary в выбранном CLI format без изменения legacy row schema."""

    if output_format == "json":
        json.dump(rows, output, ensure_ascii=False, indent=2)
        output.write("\n")
        return
    if output_format == "csv":
        writer = csv.DictWriter(output, fieldnames=SUMMARY_FIELDNAMES)
        writer.writeheader()
        writer.writerows(rows)
        return
    widths = {
        field: max(len(field), *(len(row[field]) for row in rows))
        for field in SUMMARY_FIELDNAMES
    }
    output.write(
        " | ".join(field.ljust(widths[field]) for field in SUMMARY_FIELDNAMES) + "\n"
    )
    output.write(
        "-+-".join("-" * widths[field] for field in SUMMARY_FIELDNAMES) + "\n"
    )
    for row in rows:
        output.write(
            " | ".join(row[field].ljust(widths[field]) for field in SUMMARY_FIELDNAMES)
            + "\n"
        )

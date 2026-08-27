"""Summary policy для startup/seek acceptance samples."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Callable, Iterable

from playback_acceptance import SeekSample, Verdict
from seek_diagnostics_metrics import format_milliseconds, nearest_rank


@dataclass(frozen=True)
class MetricSummary:
    """Nearest-rank summary с явным количеством неeligible samples."""

    metric: str
    observed_count: int
    eligible_count: int
    failed_count: int
    incomplete_count: int
    superseded_count: int
    p50: float | None
    p95: float | None
    maximum: float | None
    clock_basis: str

    def to_dict(self) -> dict[str, object]:
        """Сохраняет stable JSON keys для manual acceptance report."""

        return {
            "metric": self.metric,
            "observed_count": self.observed_count,
            "eligible_count": self.eligible_count,
            "failed_count": self.failed_count,
            "incomplete_count": self.incomplete_count,
            "superseded_count": self.superseded_count,
            "p50": self.p50,
            "p95": self.p95,
            "max": self.maximum,
            "clock_basis": self.clock_basis,
            "percentile_method": "nearest-rank",
            "percentile_population": "observed_non_superseded_with_metric",
        }


def summary_for_values(
    metric: str,
    samples: Iterable[SeekSample],
    value_getter: Callable[[SeekSample], float | None],
    *,
    clock_basis: str,
) -> MetricSummary:
    """Считает percentiles по observed и отдельно показывает PASS eligibility."""

    sample_list = list(samples)
    observed_values = [
        value
        for sample in sample_list
        if sample.verdict() != Verdict.SUPERSEDED
        and (value := value_getter(sample)) is not None
    ]
    eligible_values = [
        value
        for sample in sample_list
        if sample.verdict() == Verdict.PASS
        and (value := value_getter(sample)) is not None
    ]
    return MetricSummary(
        metric=metric,
        observed_count=len(observed_values),
        eligible_count=len(eligible_values),
        failed_count=sum(sample.verdict() == Verdict.FAIL for sample in sample_list),
        incomplete_count=sum(
            sample.verdict() == Verdict.INCOMPLETE for sample in sample_list
        ),
        superseded_count=sum(
            sample.verdict() == Verdict.SUPERSEDED for sample in sample_list
        ),
        p50=nearest_rank(observed_values, 0.50),
        p95=nearest_rank(observed_values, 0.95),
        maximum=max(observed_values) if observed_values else None,
        clock_basis=clock_basis,
    )


def format_summary_value(value: float | None) -> str:
    """Переиспользует единый millisecond formatter для table CLI."""

    return format_milliseconds(value)

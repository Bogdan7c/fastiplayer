"""Строгая schema legacy aggregate surface внутри stable artifacts.

Этот модуль изолирует одноразовую v1→v2 миграцию и report-only counters от
blocking source-coordinate модели. Legacy исключения никогда не участвуют в
решении о потере стабильной координаты.
"""

from __future__ import annotations

import datetime as dt
import re
from typing import Any

import coverage_coordinate_model as model


LEGACY_EXCEPTION_COUNT = 8
SHA256_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")


def _counter_pair(value: Any, context: str, *, allow_zero_total: bool) -> dict[str, int]:
    counters = model.require_object(value, context)
    model.require_exact_keys(counters, {"covered", "total"}, context)
    covered = model.require_int(counters["covered"], f"{context}.covered")
    total = model.require_int(counters["total"], f"{context}.total")
    if covered > total or (not allow_zero_total and total == 0):
        raise ValueError(f"{context} содержит invalid coverage counters")
    return {"covered": covered, "total": total}


def _metric_map(
    value: Any, context: str, *, allow_zero_total: bool
) -> dict[str, dict[str, int]]:
    metrics = model.require_object(value, context)
    model.require_exact_keys(metrics, set(model.METRICS), context)
    return {
        metric: _counter_pair(
            metrics[metric], f"{context}.{metric}", allow_zero_total=allow_zero_total
        )
        for metric in model.METRICS
    }


def _sum_metric_maps(
    group: dict[str, dict[str, dict[str, int]]]
) -> dict[str, dict[str, int]]:
    return {
        metric: {
            counter: sum(owner[metric][counter] for owner in group.values())
            for counter in ("covered", "total")
        }
        for metric in model.METRICS
    }


def _lower_envelope_delta(
    aggregate: dict[str, dict[str, int]],
    derived: dict[str, dict[str, int]],
    context: str,
) -> dict[str, dict[str, int]]:
    result: dict[str, dict[str, int]] = {}
    for metric in model.METRICS:
        covered_delta = aggregate[metric]["covered"] - derived[metric]["covered"]
        total_delta = aggregate[metric]["total"] - derived[metric]["total"]
        if total_delta != 0:
            raise ValueError(f"{context}.{metric} total не совпадает с суммой crate rows")
        if covered_delta < 0:
            raise ValueError(f"{context}.{metric} нарушает min(sum) >= sum(min)")
        result[metric] = {
            "covered_delta": covered_delta,
            "total_delta": total_delta,
        }
    return result


def _reject_sensitive_strings(value: Any, context: str) -> None:
    if isinstance(value, dict):
        for key, nested in value.items():
            lowered_key = str(key).lower()
            if any(
                marker in lowered_key
                for marker in ("authorization", "cookie", "secret", "token", "manifest_path")
            ):
                raise ValueError(f"{context} содержит запрещённое поле `{key}`")
            _reject_sensitive_strings(nested, f"{context}.{key}")
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            _reject_sensitive_strings(nested, f"{context}[{index}]")
    elif isinstance(value, str) and (
        value.startswith("/") or re.match(r"^[A-Za-z]:[\\/]", value)
    ):
        raise ValueError(f"{context} содержит абсолютный путь")


def validate_cross_check(value: Any, context: str) -> None:
    cross_check = model.require_object(value, context)
    model.require_exact_keys(cross_check, set(model.METRICS), context)

    lines = model.require_object(cross_check["lines"], f"{context}.lines")
    model.require_exact_keys(
        lines,
        {"category", "derived", "llvm", "covered_delta", "total_delta"},
        f"{context}.lines",
    )
    if lines["category"] != "source-lines-vs-function-summary":
        raise ValueError(f"{context}.lines category неизвестна")
    line_derived = _counter_pair(
        lines["derived"], f"{context}.lines.derived", allow_zero_total=True
    )
    line_llvm = _counter_pair(
        lines["llvm"], f"{context}.lines.llvm", allow_zero_total=True
    )
    for delta_name in ("covered_delta", "total_delta"):
        delta = lines[delta_name]
        if isinstance(delta, bool) or not isinstance(delta, int):
            raise ValueError(f"{context}.lines.{delta_name} должен быть integer")
    if lines["covered_delta"] != line_derived["covered"] - line_llvm["covered"]:
        raise ValueError(f"{context}.lines.covered_delta не совпадает с counters")
    if lines["total_delta"] != line_derived["total"] - line_llvm["total"]:
        raise ValueError(f"{context}.lines.total_delta не совпадает с counters")

    functions = model.require_object(cross_check["functions"], f"{context}.functions")
    model.require_exact_keys(
        functions, {"category", "derived", "llvm"}, f"{context}.functions"
    )
    if functions["category"] != "llvm-instantiation-group-exact":
        raise ValueError(f"{context}.functions category неизвестна")
    function_derived = _counter_pair(
        functions["derived"], f"{context}.functions.derived", allow_zero_total=True
    )
    function_llvm = _counter_pair(
        functions["llvm"], f"{context}.functions.llvm", allow_zero_total=True
    )
    if function_derived != function_llvm:
        raise ValueError(f"{context}.functions exact cross-check не совпадает")

    regions = model.require_object(cross_check["regions"], f"{context}.regions")
    model.require_exact_keys(
        regions,
        {"category", "derived", "llvm", "covered_delta"},
        f"{context}.regions",
    )
    region_derived = _counter_pair(
        regions["derived"], f"{context}.regions.derived", allow_zero_total=True
    )
    region_llvm = _counter_pair(
        regions["llvm"], f"{context}.regions.llvm", allow_zero_total=True
    )
    region_delta = regions["covered_delta"]
    if isinstance(region_delta, bool) or not isinstance(region_delta, int):
        raise ValueError(f"{context}.regions.covered_delta должен быть integer")
    if region_derived["total"] != region_llvm["total"]:
        raise ValueError(f"{context}.regions universe total не совпадает")
    if region_delta != region_derived["covered"] - region_llvm["covered"]:
        raise ValueError(f"{context}.regions.covered_delta не совпадает с counters")
    expected_category = (
        "llvm-code-region-covered-combination-difference"
        if region_delta
        else "llvm-code-region-exact"
    )
    if regions["category"] != expected_category:
        raise ValueError(f"{context}.regions category не соответствует delta")


def validate_run_report(value: Any, context: str, domains: set[str]) -> None:
    report = model.require_object(value, context)
    model.require_exact_keys(report, {"domains", "cross_check"}, context)
    legacy_domains = model.require_object(report["domains"], f"{context}.domains")
    if set(legacy_domains) != domains:
        raise ValueError(f"{context}.domains не совпадает со stable domains")
    for domain_name, domain in legacy_domains.items():
        _metric_map(
            domain, f"{context}.domains.{domain_name}", allow_zero_total=True
        )
    validate_cross_check(report["cross_check"], f"{context}.cross_check")
    _reject_sensitive_strings(report, context)


def validate_cohort_report(
    value: Any, context: str, run_set: list[tuple[str, str]], domains: set[str]
) -> None:
    report = model.require_object(value, context)
    model.require_exact_keys(report, {"runs"}, context)
    runs = model.require_array(report["runs"], f"{context}.runs")
    if len(runs) != 3:
        raise ValueError(f"{context}.runs должен содержать ровно три запуска")
    labels: list[str] = []
    for index, run_document in enumerate(runs):
        run_context = f"{context}.runs[{index}]"
        run = model.require_object(run_document, run_context)
        model.require_exact_keys(run, {"run_label", "domains", "cross_check"}, run_context)
        labels.append(model.require_string(run["run_label"], f"{run_context}.run_label"))
        validate_run_report(
            {"domains": run["domains"], "cross_check": run["cross_check"]},
            run_context,
            domains,
        )
    if labels != [label for label, _ in run_set]:
        raise ValueError(f"{context}.runs labels не совпадают с run_set")


def validate_baseline_v1(
    document: Any, context: str = "legacy baseline v1"
) -> dict[str, Any]:
    baseline = model.require_object(document, context)
    model.require_exact_keys(
        baseline,
        {
            "schema_version",
            "tool",
            "workspace",
            "blocking_group",
            "blocking_crates",
            "informational_crates",
        },
        context,
    )
    if model.require_int(baseline["schema_version"], f"{context}.schema_version") != 1:
        raise ValueError(f"{context} schema_version неизвестна")
    tool = model.require_object(baseline["tool"], f"{context}.tool")
    model.require_exact_keys(tool, {"name", "version"}, f"{context}.tool")
    if tool != {"name": "cargo-llvm-cov", "version": model.CARGO_LLVM_COV_VERSION}:
        raise ValueError(f"{context}.tool неизвестен")
    workspace = _metric_map(
        baseline["workspace"], f"{context}.workspace", allow_zero_total=False
    )
    blocking_group = _metric_map(
        baseline["blocking_group"],
        f"{context}.blocking_group",
        allow_zero_total=False,
    )
    parsed_groups: dict[str, dict[str, dict[str, dict[str, int]]]] = {}
    for group_name in ("blocking_crates", "informational_crates"):
        group = model.require_object(baseline[group_name], f"{context}.{group_name}")
        if group_name == "blocking_crates" and not group:
            raise ValueError(f"{context}.{group_name} не может быть пустым")
        parsed_groups[group_name] = {}
        for owner, metrics in group.items():
            model.canonical_crate_owner(owner, f"{context}.{group_name}.owner")
            parsed_groups[group_name][owner] = _metric_map(
                metrics,
                f"{context}.{group_name}.{owner}",
                allow_zero_total=False,
            )
    if set(parsed_groups["blocking_crates"]) & set(parsed_groups["informational_crates"]):
        raise ValueError(f"{context} crate groups пересекаются")

    derived_blocking = _sum_metric_maps(parsed_groups["blocking_crates"])
    derived_workspace = _sum_metric_maps(
        {**parsed_groups["blocking_crates"], **parsed_groups["informational_crates"]}
    )
    _lower_envelope_delta(
        blocking_group, derived_blocking, f"{context}.blocking_group"
    )
    _lower_envelope_delta(workspace, derived_workspace, f"{context}.workspace")
    _reject_sensitive_strings(baseline, context)
    return baseline


def lower_envelope_diagnostics(document: Any) -> dict[str, Any]:
    baseline = validate_baseline_v1(document)
    blocking_rows = baseline["blocking_crates"]
    all_rows = {**blocking_rows, **baseline["informational_crates"]}
    return {
        "category": "independent-scope-lower-envelope-v1",
        "blocking_group_vs_crate_rows": _lower_envelope_delta(
            baseline["blocking_group"],
            _sum_metric_maps(blocking_rows),
            "legacy baseline v1.blocking_group",
        ),
        "workspace_vs_crate_rows": _lower_envelope_delta(
            baseline["workspace"],
            _sum_metric_maps(all_rows),
            "legacy baseline v1.workspace",
        ),
    }


def validate_exceptions_v1(
    document: Any, context: str = "legacy exceptions v1"
) -> list[dict[str, Any]]:
    manifest = model.require_object(document, context)
    model.require_exact_keys(manifest, {"schema_version", "exceptions"}, context)
    if model.require_int(manifest["schema_version"], f"{context}.schema_version") != 1:
        raise ValueError(f"{context} schema_version неизвестна")
    required = {
        "scope", "metric", "previous", "allowed", "reason", "review_by", "follow_up"
    }
    entries = model.require_array(manifest["exceptions"], f"{context}.exceptions")
    if len(entries) != LEGACY_EXCEPTION_COUNT:
        raise ValueError(
            f"{context} должен сохранять exact inventory из {LEGACY_EXCEPTION_COUNT} записей"
        )
    identities: set[tuple[str, str]] = set()
    parsed: list[dict[str, Any]] = []
    for index, entry_document in enumerate(entries):
        entry_context = f"{context}.exceptions[{index}]"
        entry = model.require_object(entry_document, entry_context)
        model.require_exact_keys(entry, required, entry_context)
        scope = model.require_string(entry["scope"], f"{entry_context}.scope")
        metric = model.require_string(entry["metric"], f"{entry_context}.metric")
        if metric not in model.METRICS or not (
            scope in {"workspace", "blocking-group"}
            or (scope.startswith("crate:") and bool(scope.removeprefix("crate:")))
        ):
            raise ValueError(f"{entry_context} имеет неизвестную identity")
        if scope.startswith("crate:"):
            model.canonical_crate_owner(
                scope.removeprefix("crate:"), f"{entry_context}.scope"
            )
        identity = (scope, metric)
        if identity in identities:
            raise ValueError(f"{context} содержит duplicate {scope}/{metric}")
        identities.add(identity)
        for counter_name in ("previous", "allowed"):
            _counter_pair(
                entry[counter_name], f"{entry_context}.{counter_name}", allow_zero_total=False
            )
        for name in ("reason", "follow_up"):
            model.require_string(entry[name], f"{entry_context}.{name}")
        review_by = dt.date.fromisoformat(
            model.require_string(entry["review_by"], f"{entry_context}.review_by")
        )
        if review_by < dt.date.today():
            raise ValueError(f"{entry_context} просрочено {review_by}")
        parsed.append(entry)
    _reject_sensitive_strings(manifest, context)
    return parsed


def validate_baseline_report(value: Any, context: str) -> None:
    report = model.require_object(value, context)
    model.require_exact_keys(
        report,
        {
            "baseline_v1",
            "exception_identities",
            "baseline_hash",
            "exceptions_hash",
            "lower_envelope_diagnostics",
        },
        context,
    )
    validate_baseline_v1(report["baseline_v1"], f"{context}.baseline_v1")
    expected_baseline_hash = model.require_string(
        report["baseline_hash"], f"{context}.baseline_hash"
    )
    if not SHA256_PATTERN.fullmatch(expected_baseline_hash) or model.content_hash(
        report["baseline_v1"]
    ) != expected_baseline_hash:
        raise ValueError(f"{context}.baseline_hash имеет неверный SHA-256")
    if not SHA256_PATTERN.fullmatch(
        model.require_string(report["exceptions_hash"], f"{context}.exceptions_hash")
    ):
        raise ValueError(f"{context}.exceptions_hash должен быть SHA-256")
    expected_diagnostics = lower_envelope_diagnostics(report["baseline_v1"])
    if report["lower_envelope_diagnostics"] != expected_diagnostics:
        raise ValueError(f"{context}.lower_envelope_diagnostics не совпадает с baseline v1")
    identities = model.require_array(
        report["exception_identities"], f"{context}.exception_identities"
    )
    if len(identities) != LEGACY_EXCEPTION_COUNT:
        raise ValueError(f"{context}.exception_identities имеет неверный inventory")
    normalized: list[tuple[str, str]] = []
    for index, identity_document in enumerate(identities):
        identity_context = f"{context}.exception_identities[{index}]"
        identity = model.require_object(identity_document, identity_context)
        model.require_exact_keys(identity, {"scope", "metric"}, identity_context)
        scope = model.require_string(identity["scope"], f"{identity_context}.scope")
        metric = model.require_string(identity["metric"], f"{identity_context}.metric")
        if metric not in model.METRICS:
            raise ValueError(f"{identity_context} содержит unknown metric")
        normalized.append((scope, metric))
    if normalized != sorted(set(normalized)):
        raise ValueError(f"{context}.exception_identities должен быть sorted/unique")
    _reject_sensitive_strings(report, context)

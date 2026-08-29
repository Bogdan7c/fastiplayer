#!/usr/bin/env python3
"""Трёхзапусковая классификация и stable-coordinate coverage ratchet."""

from __future__ import annotations

import argparse
import copy
import json
import sys
from pathlib import Path
from typing import Any

import coverage_coordinate_model as model
import coverage_legacy_schema as legacy
from coverage_stability_schema import (
    BASELINE_SCHEMA_VERSION,
    COHORT_SCHEMA_VERSION,
    _expand_ranges,
    validate_baseline,
    validate_cohort,
    validate_measurement_exceptions,
    validate_run_state,
)


def _cohort_metric(
    identities: list[str], universe_indices: list[int], covered_sets: list[set[int]]
) -> tuple[dict[str, Any], list[tuple[int, list[bool]]]]:
    stable: list[int] = []
    variable: list[int] = []
    uncovered: list[int] = []
    diagnostics: list[tuple[int, list[bool]]] = []
    for index in universe_indices:
        hits = [index in covered for covered in covered_sets]
        hit_count = sum(hits)
        if hit_count == 3:
            stable.append(index)
        elif hit_count == 0:
            uncovered.append(index)
        else:
            variable.append(index)
            diagnostics.append((index, hits))
    return (
        {
            "universe_ranges": model.ranges(universe_indices),
            "stable_ranges": model.ranges(stable),
            "variable_ranges": model.ranges(variable),
            "uncovered_ranges": model.ranges(uncovered),
            "universe_hash": model.content_hash(
                [identities[index] for index in universe_indices]
            ),
            "stable_hash": model.content_hash([identities[index] for index in stable]),
            "counts": {
                "stable": len(stable),
                "variable": len(variable),
                "uncovered": len(uncovered),
                "total": len(universe_indices),
            },
        },
        diagnostics,
    )


def intersect_runs(
    policy_document: Any, run_documents: list[Any]
) -> tuple[dict[str, Any], dict[str, Any]]:
    policy = model.validate_policy(policy_document)
    if len(run_documents) != 3:
        raise ValueError("intersect требует ровно три run state")
    runs = [validate_run_state(document, policy) for document in run_documents]
    labels = [run["run_label"] for run in runs]
    if len(set(labels)) != 3:
        raise ValueError("intersect требует три уникальных run-label")
    reference = runs[0]
    for run in runs[1:]:
        if run["provenance"] != reference["provenance"]:
            raise ValueError("run provenance не совпадает")
        if run["source_files"] != reference["source_files"]:
            raise ValueError("source file universe отличается между runs")
        if run["stable_source"]["coordinates"] != reference["stable_source"]["coordinates"]:
            raise ValueError("source coordinate universe отличается между runs")
        for domain_name, reference_domain in reference["stable_source"]["domains"].items():
            for metric in model.METRICS:
                if (
                    run["stable_source"]["domains"][domain_name][metric]["universe_ranges"]
                    != reference_domain[metric]["universe_ranges"]
                ):
                    raise ValueError("measurement domain universe отличается между runs")
    universes = {
        metric: reference["stable_source"]["coordinates"][metric]["universe"]
        for metric in model.METRICS
    }
    identities = {
        metric: [
            model.coordinate_identity(metric, coordinate, reference["source_files"]["universe"])
            for coordinate in universes[metric]
        ]
        for metric in model.METRICS
    }
    cohort_domains: dict[str, Any] = {}
    variable_domains: dict[str, Any] = {}
    for domain_name, reference_domain in reference["stable_source"]["domains"].items():
        cohort_domains[domain_name] = {}
        variable_domains[domain_name] = {}
        for metric in model.METRICS:
            universe_indices = _expand_ranges(
                reference_domain[metric]["universe_ranges"],
                len(universes[metric]),
                f"{domain_name}.{metric}.universe",
            )
            covered_sets = [
                set(
                    _expand_ranges(
                        run["stable_source"]["domains"][domain_name][metric]["covered_ranges"],
                        len(universes[metric]),
                        f"{run['run_label']}.{domain_name}.{metric}.covered",
                    )
                )
                for run in runs
            ]
            cohort_metric, variable = _cohort_metric(
                identities[metric], universe_indices, covered_sets
            )
            cohort_domains[domain_name][metric] = cohort_metric
            variable_domains[domain_name][metric] = [
                {
                    "coordinate_index": index,
                    "coordinate": json.loads(identities[metric][index]),
                    "hits": hits,
                }
                for index, hits in variable
            ]
    run_set = sorted(
        ({"run_label": run["run_label"], "state_hash": run["state_hash"]} for run in runs),
        key=lambda entry: entry["run_label"],
    )
    cohort = {
        "schema_version": COHORT_SCHEMA_VERSION,
        "kind": "coverage-stable-cohort",
        "provenance": copy.deepcopy(reference["provenance"]),
        "run_set": run_set,
        "source_files": copy.deepcopy(reference["source_files"]),
        "stable_source": {
            "coordinates": copy.deepcopy(reference["stable_source"]["coordinates"]),
            "domains": cohort_domains,
        },
        "legacy_report_only": {
            "runs": sorted(
                (
                    {
                        "run_label": run["run_label"],
                        **copy.deepcopy(run["legacy_report_only"]),
                    }
                    for run in runs
                ),
                key=lambda entry: entry["run_label"],
            )
        },
    }
    cohort["cohort_hash"] = model.content_hash(cohort)
    diagnostics = {
        "schema_version": 1,
        "kind": "coverage-variable-diagnostics",
        "run_order": labels,
        "variables": variable_domains,
    }
    diagnostics["diagnostics_hash"] = model.content_hash(diagnostics)
    return cohort, diagnostics


def _stable_snapshot(cohort: dict[str, Any]) -> dict[str, Any]:
    domains: dict[str, Any] = {}
    for domain_name, domain in cohort["stable_source"]["domains"].items():
        domains[domain_name] = {}
        for metric, entry in domain.items():
            domains[domain_name][metric] = {
                "universe_ranges": copy.deepcopy(entry["universe_ranges"]),
                "stable_ranges": copy.deepcopy(entry["stable_ranges"]),
                "universe_hash": entry["universe_hash"],
                "stable_hash": entry["stable_hash"],
                "counts": {"stable": entry["counts"]["stable"], "total": entry["counts"]["total"]},
            }
    return {
        "coordinates": copy.deepcopy(cohort["stable_source"]["coordinates"]),
        "domains": domains,
    }


def _validate_legacy_documents(
    cohort: dict[str, Any], baseline: Any, exceptions: Any
) -> list[dict[str, str]]:
    baseline = legacy.validate_baseline_v1(baseline)
    exception_entries = legacy.validate_exceptions_v1(exceptions)
    stable_domains = set(cohort["stable_source"]["domains"])
    stable_blocking = {
        domain.removeprefix("crate:")
        for domain in stable_domains
        if domain.startswith("crate:")
    }
    source_owners = {
        model.crate_name(path) for path in cohort["source_files"]["universe"]
    }
    legacy_blocking = set(baseline["blocking_crates"])
    legacy_all = legacy_blocking | set(baseline["informational_crates"])
    if legacy_blocking != stable_blocking or legacy_all != source_owners:
        raise ValueError("legacy v1 crate inventories не совпадают с cohort source owners")
    identities: list[dict[str, str]] = []
    for entry in exception_entries:
        identity = {
            "scope": entry["scope"],
            "metric": entry["metric"],
        }
        scope = entry["scope"]
        if scope == "workspace":
            current_counters = baseline["workspace"][entry["metric"]]
        elif scope == "blocking-group":
            current_counters = baseline["blocking_group"][entry["metric"]]
        elif scope.startswith("crate:"):
            owner = scope.removeprefix("crate:")
            current_counters = baseline["blocking_crates"].get(owner, {}).get(entry["metric"])
        else:
            current_counters = None
        if current_counters != entry["allowed"]:
            raise ValueError(
                f"legacy exception {scope}/{entry['metric']} allowed не совпадает с baseline"
            )
        identities.append(identity)
    identity_keys = [(entry["scope"], entry["metric"]) for entry in identities]
    if len(identity_keys) != len(set(identity_keys)):
        raise ValueError("legacy exception identities содержат duplicate")
    return sorted(identities, key=lambda entry: (entry["scope"], entry["metric"]))


def _artifact_identity_universes(artifact: dict[str, Any]) -> dict[str, list[str]]:
    source_files = artifact["source_files"]["universe"]
    return {
        metric: [
            model.coordinate_identity(metric, coordinate, source_files)
            for coordinate in artifact["stable_source"]["coordinates"][metric]["universe"]
        ]
        for metric in model.METRICS
    }


def _metric_identities(
    artifact: dict[str, Any],
    identity_universes: dict[str, list[str]],
    domain_name: str,
    metric: str,
    range_name: str,
) -> list[str]:
    coordinates = artifact["stable_source"]["coordinates"][metric]["universe"]
    indices = _expand_ranges(
        artifact["stable_source"]["domains"][domain_name][metric][range_name],
        len(coordinates),
        f"{domain_name}.{metric}.{range_name}",
    )
    return [identity_universes[metric][index] for index in indices]


def bootstrap_baseline(cohort_document: Any, legacy_baseline: Any, legacy_exceptions: Any) -> dict[str, Any]:
    cohort = validate_cohort(cohort_document)
    identities = _validate_legacy_documents(cohort, legacy_baseline, legacy_exceptions)
    baseline = {
        "schema_version": BASELINE_SCHEMA_VERSION,
        "kind": "coverage-stable-baseline",
        "provenance": copy.deepcopy(cohort["provenance"]),
        "source_files": copy.deepcopy(cohort["source_files"]),
        "stable_source": _stable_snapshot(cohort),
        "legacy_report_only": {
            "baseline_v1": copy.deepcopy(legacy_baseline),
            "exception_identities": identities,
            "baseline_hash": model.content_hash(legacy_baseline),
            "exceptions_hash": model.content_hash(legacy_exceptions),
            "lower_envelope_diagnostics": legacy.lower_envelope_diagnostics(
                legacy_baseline
            ),
        },
    }
    baseline["baseline_hash"] = model.content_hash(baseline)
    return baseline


def _ratio_decreased(current: dict[str, int], previous: dict[str, int]) -> bool:
    if current["total"] == 0:
        return previous["total"] != 0
    return current["stable"] * previous["total"] < previous["stable"] * current["total"]


def check_baseline(
    baseline_document: Any,
    cohort_document: Any,
    exception_document: Any,
    *,
    allow_universe_update: bool,
) -> tuple[bool, dict[str, Any]]:
    baseline = validate_baseline(baseline_document)
    cohort = validate_cohort(cohort_document)
    exceptions = validate_measurement_exceptions(exception_document)
    hard_provenance = {
        key for key in baseline["provenance"] if key not in {"policy_hash"}
    }
    if any(baseline["provenance"][key] != cohort["provenance"][key] for key in hard_provenance):
        raise ValueError("baseline/cohort tool, methodology или profile provenance отличаются")
    changes: list[dict[str, Any]] = []
    regressions: list[dict[str, Any]] = []
    files_changed = baseline["source_files"] != cohort["source_files"]
    policy_changed = baseline["provenance"]["policy_hash"] != cohort["provenance"]["policy_hash"]
    baseline_domains = baseline["stable_source"]["domains"]
    current_domains = cohort["stable_source"]["domains"]
    baseline_identities = _artifact_identity_universes(baseline)
    current_identities = _artifact_identity_universes(cohort)
    available_domains = set(baseline_domains) & set(current_domains)
    unknown_exceptions = {
        identity for identity in exceptions if identity[0] not in available_domains
    }
    if unknown_exceptions:
        raise ValueError(
            f"measurement exceptions ссылаются на отсутствующие domains: {sorted(unknown_exceptions)}"
        )
    consumed_exceptions: set[tuple[str, str]] = set()
    if set(baseline_domains) != set(current_domains):
        changes.append({"kind": "measurement-domains", "update_required": True})
    for domain_name in sorted(set(baseline_domains) & set(current_domains)):
        for metric in model.METRICS:
            previous = baseline_domains[domain_name][metric]
            current = current_domains[domain_name][metric]
            previous_universe = _metric_identities(
                baseline, baseline_identities, domain_name, metric, "universe_ranges"
            )
            current_universe = _metric_identities(
                cohort, current_identities, domain_name, metric, "universe_ranges"
            )
            same_universe = (
                previous["universe_hash"] == current["universe_hash"]
                and previous_universe == current_universe
            )
            previous_counts = previous["counts"]
            current_counts = {
                "stable": current["counts"]["stable"], "total": current["counts"]["total"]
            }
            if same_universe:
                previous_stable = set(
                    _metric_identities(
                        baseline,
                        baseline_identities,
                        domain_name,
                        metric,
                        "stable_ranges",
                    )
                )
                current_stable = set(
                    _metric_identities(
                        cohort,
                        current_identities,
                        domain_name,
                        metric,
                        "stable_ranges",
                    )
                )
                lost = sorted(previous_stable - current_stable)
                if lost:
                    regressions.append(
                        {
                            "domain": domain_name,
                            "metric": metric,
                            "kind": "exact-stable-coordinate-loss",
                            "lost_coordinates": [json.loads(identity) for identity in lost],
                        }
                    )
                continue
            changes.append({"kind": "coordinate-universe", "domain": domain_name, "metric": metric})
            if _ratio_decreased(current_counts, previous_counts):
                exception = exceptions.get((domain_name, metric))
                if exception is not None:
                    consumed_exceptions.add((domain_name, metric))
                valid_exception = exception is not None and all(
                    (
                        exception["previous"] == previous_counts,
                        exception["allowed"] == current_counts,
                        exception["previous_universe_hash"] == previous["universe_hash"],
                        exception["current_universe_hash"] == current["universe_hash"],
                    )
                )
                if not valid_exception:
                    regressions.append(
                        {"domain": domain_name, "metric": metric, "kind": "cross-universe-stable-ratio-decrease", "previous": previous_counts, "current": current_counts}
                    )
    unused_exceptions = set(exceptions) - consumed_exceptions
    if unused_exceptions:
        raise ValueError(
            f"measurement exceptions содержит stale/unused entries: {sorted(unused_exceptions)}"
        )
    update_required = bool(changes or files_changed or policy_changed)
    passed = not regressions and (allow_universe_update or not update_required)
    report = {
        "schema_version": 1,
        "kind": "coverage-stable-check",
        "status": "pass" if passed else "fail",
        "allow_universe_update": allow_universe_update,
        "source_files_changed": files_changed,
        "policy_changed": policy_changed,
        "universe_changes": changes,
        "regressions": regressions,
        "legacy_report_only": "legacy counters/exceptions do not authorize stable-source loss",
    }
    report["check_hash"] = model.content_hash(report)
    return passed, report


def parse_args(arguments: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Классифицировать и проверить stable coverage coordinates.")
    commands = parser.add_subparsers(dest="command", required=True)
    intersect = commands.add_parser("intersect", help="пересечь ровно три run state")
    intersect.add_argument("--policy", type=Path, required=True)
    intersect.add_argument("--run", type=Path, action="append", required=True)
    intersect.add_argument("--output", type=Path, required=True, metavar="COHORT_JSON")
    intersect.add_argument("--diagnostics", type=Path, required=True, metavar="VARIABLE_JSON")
    validate = commands.add_parser("validate", help="fail-closed schema/hash validation")
    validate.add_argument("--kind", choices=("run", "cohort", "baseline", "measurement-exceptions"), required=True)
    validate.add_argument("--input", type=Path, required=True)
    bootstrap = commands.add_parser("bootstrap", help="создать v2 baseline из cohort и legacy v1")
    bootstrap.add_argument("--cohort", type=Path, required=True)
    bootstrap.add_argument("--legacy-baseline", type=Path, required=True)
    bootstrap.add_argument("--legacy-exceptions", type=Path, required=True)
    bootstrap.add_argument("--output", type=Path, required=True)
    check = commands.add_parser("check", help="проверить v2 stable-coordinate ratchet")
    check.add_argument("--baseline", type=Path, required=True)
    check.add_argument("--cohort", type=Path, required=True)
    check.add_argument("--measurement-exceptions", type=Path, required=True)
    check.add_argument("--output", type=Path, required=True)
    check.add_argument("--allow-universe-update", action="store_true")
    return parser.parse_args(arguments)


def main(arguments: list[str] | None = None) -> int:
    parsed = parse_args(arguments)
    try:
        if parsed.command == "intersect":
            policy = model.read_json(parsed.policy)
            cohort, diagnostics = intersect_runs(
                policy, [model.read_json(path) for path in parsed.run]
            )
            model.write_json_pair_transactional(
                parsed.diagnostics, diagnostics, parsed.output, cohort
            )
            print(f"Stable cohort записан: {parsed.output}")
            return 0
        if parsed.command == "validate":
            document = model.read_json(parsed.input)
            if parsed.kind == "run":
                validate_run_state(document)
            elif parsed.kind == "cohort":
                validate_cohort(document)
            elif parsed.kind == "baseline":
                validate_baseline(document)
            else:
                validate_measurement_exceptions(document)
            print(f"Stable coverage {parsed.kind} validation: OK")
            return 0
        if parsed.command == "bootstrap":
            baseline = bootstrap_baseline(
                model.read_json(parsed.cohort),
                model.read_json(parsed.legacy_baseline),
                model.read_json(parsed.legacy_exceptions),
            )
            model.write_json_atomic(parsed.output, baseline)
            print(f"Stable coverage baseline v2 записан: {parsed.output}")
            return 0
        if parsed.command == "check":
            passed, report = check_baseline(
                model.read_json(parsed.baseline),
                model.read_json(parsed.cohort),
                model.read_json(parsed.measurement_exceptions),
                allow_universe_update=parsed.allow_universe_update,
            )
            model.write_json_atomic(parsed.output, report)
            print(f"Stable coverage check: {report['status']}")
            return 0 if passed else 1
        raise ValueError(f"неизвестная команда {parsed.command}")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"coverage stability error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

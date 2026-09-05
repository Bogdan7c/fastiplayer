#!/usr/bin/env python3
"""Извлечение стабильных source-coordinate метрик из полного LLVM JSON export."""

from __future__ import annotations

import argparse
import json
import math
import re
import sys
from pathlib import Path
from typing import Any

from coverage_coordinate_model import (
    INT64_MAX,
KNOWN_REGION_KINDS,
    LLVM_EXPORT_TYPE,
    LLVM_EXPORT_VERSION,
    METRICS,
    METHODOLOGY,
    RUN_SCHEMA_VERSION,
    SourcePathNormalizer,
    canonical_json,
    content_hash,
    crate_name,
    ranges as _ranges,
    read_json,
    require_array as _require_array,
    require_bool as _require_bool,
    require_exact_keys as _require_exact_keys,
    require_int as _require_int,
    require_object as _require_object,
    require_string as _require_string,
    validate_policy,
    validate_profile_manifest,
    write_json_atomic,
)


SUMMARY_METRICS = ("lines", "functions", "instantiations", "regions", "branches", "mcdc")


def _coordinate(parts: list[Any]) -> str:
    return json.dumps(parts, ensure_ascii=False, separators=(",", ":"))


def _line_coordinates(
    segments_document: Any, relative_path: str, context: str
) -> tuple[set[str], set[str]]:
    segments = _require_array(segments_document, f"{context}.segments")
    parsed_segments: list[tuple[int, int, int, bool, bool, bool]] = []
    previous_location: tuple[int, int] | None = None
    previous_has_count = True
    for segment_index, segment_document in enumerate(segments):
        segment = _require_array(segment_document, f"{context}.segments[{segment_index}]")
        if len(segment) != 6:
            raise ValueError(f"{context}.segments[{segment_index}] должен иметь 6 полей")
        line = _require_int(segment[0], f"{context}.segments[{segment_index}].line", minimum=1)
        column = _require_int(
            segment[1], f"{context}.segments[{segment_index}].column", minimum=1
        )
        count = _require_int(segment[2], f"{context}.segments[{segment_index}].count")
        has_count = _require_bool(
            segment[3], f"{context}.segments[{segment_index}].has_count"
        )
        is_region_entry = _require_bool(
            segment[4], f"{context}.segments[{segment_index}].is_region_entry"
        )
        is_gap = _require_bool(segment[5], f"{context}.segments[{segment_index}].is_gap")
        location = (line, column)
        if previous_location is not None:
            if location < previous_location or (
                location == previous_location
                and (previous_has_count or not has_count)
            ):
                raise ValueError(f"{context}.segments нарушает LLVM sorted topology")
        previous_location = location
        previous_has_count = has_count
        parsed_segments.append((line, column, count, has_count, is_region_entry, is_gap))

    if not parsed_segments:
        return set(), set()
    universe: set[str] = set()
    covered: set[str] = set()
    next_segment = 0
    wrapped_segment: tuple[int, int, int, bool, bool, bool] | None = None
    current_line = parsed_segments[0][0]
    while next_segment < len(parsed_segments):
        line_segments: list[tuple[int, int, int, bool, bool, bool]] = []
        while (
            next_segment < len(parsed_segments)
            and parsed_segments[next_segment][0] == current_line
        ):
            line_segments.append(parsed_segments[next_segment])
            next_segment += 1
        start_of_skipped = bool(line_segments) and not line_segments[0][3] and line_segments[0][4]
        starting_regions = [
            segment
            for segment in line_segments
            if not segment[5] and segment[3] and segment[4]
        ]
        mapped = not start_of_skipped and (
            (wrapped_segment is not None and wrapped_segment[3]) or bool(starting_regions)
        )
        # LLVM дополнительно считает mapped любой counted region-entry, включая gap-entry.
        mapped = mapped or any(segment[3] and segment[4] for segment in line_segments)
        if mapped:
            coordinate = _coordinate(["L", relative_path, current_line])
            universe.add(coordinate)
            execution_count = wrapped_segment[2] if wrapped_segment is not None else 0
            if starting_regions:
                execution_count = max(
                    execution_count, max(segment[2] for segment in starting_regions)
                )
            if execution_count > 0:
                covered.add(coordinate)
        if line_segments:
            wrapped_segment = line_segments[-1]
        current_line += 1
    return universe, covered


def _parse_region(region_document: Any, filenames: list[str], context: str) -> tuple[Any, ...]:
    region = _require_array(region_document, context)
    if len(region) != 8:
        raise ValueError(f"{context} должен иметь 8 полей")
    line_start = _require_int(region[0], f"{context}.line_start", minimum=1)
    column_start = _require_int(region[1], f"{context}.column_start", minimum=1)
    line_end = _require_int(region[2], f"{context}.line_end", minimum=1)
    column_end = _require_int(region[3], f"{context}.column_end", minimum=1)
    if (line_end, column_end) < (line_start, column_start):
        raise ValueError(f"{context} имеет обратный source range")
    count = _require_int(region[4], f"{context}.count")
    file_id = _require_int(region[5], f"{context}.file_id")
    expanded_file_id = _require_int(region[6], f"{context}.expanded_file_id")
    kind = _require_int(region[7], f"{context}.kind")
    if kind not in KNOWN_REGION_KINDS:
        raise ValueError(f"{context} содержит неизвестный region kind {kind}")
    if file_id >= len(filenames) or expanded_file_id >= len(filenames):
        raise ValueError(f"{context} ссылается на отсутствующий filename id")
    return (
        line_start,
        column_start,
        line_end,
        column_end,
        count,
        file_id,
        expanded_file_id,
        kind,
    )


def _validate_percentage(value: Any, context: str) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError(f"{context} должен быть JSON number")
    if not math.isfinite(value) or not 0 <= value <= 100:
        raise ValueError(f"{context} должен быть конечным percentage 0..100")


def _validate_summary(summary_document: Any, context: str) -> dict[str, dict[str, int]]:
    summary = _require_object(summary_document, context)
    expected_metrics = set(SUMMARY_METRICS)
    _require_exact_keys(summary, expected_metrics, context)
    parsed: dict[str, dict[str, int]] = {}
    for metric in expected_metrics:
        metric_context = f"{context}.{metric}"
        metric_document = _require_object(summary[metric], metric_context)
        expected_keys = {"count", "covered", "percent"}
        if metric in {"regions", "branches", "mcdc"}:
            expected_keys.add("notcovered")
        _require_exact_keys(metric_document, expected_keys, metric_context)
        count = _require_int(metric_document["count"], f"{metric_context}.count")
        covered = _require_int(metric_document["covered"], f"{metric_context}.covered")
        if covered > count:
            raise ValueError(f"{metric_context}.covered превышает count")
        if metric in {"branches", "mcdc"} and count != 0:
            raise ValueError(f"{metric_context} должен быть пуст для frozen methodology")
        if "notcovered" in expected_keys:
            not_covered = _require_int(
                metric_document["notcovered"], f"{metric_context}.notcovered"
            )
            if not_covered != count - covered:
                raise ValueError(f"{metric_context}.notcovered не совпадает с counters")
        _validate_percentage(metric_document["percent"], f"{metric_context}.percent")
        parsed[metric] = {"covered": covered, "total": count}
    return parsed


def _function_coordinates(
    functions_document: Any,
    file_universe: set[str],
    normalizer: SourcePathNormalizer,
) -> tuple[set[str], set[str], set[str], set[str]]:
    functions = _require_array(functions_document, "data[0].functions")
    function_counts: dict[str, int] = {}
    region_counts: dict[str, int] = {}
    for function_index, function_document in enumerate(functions):
        context = f"data[0].functions[{function_index}]"
        function = _require_object(function_document, context)
        required_keys = {"name", "count", "filenames", "regions", "branches", "mcdc_records"}
        _require_exact_keys(function, required_keys, context)
        _require_string(function["name"], f"{context}.name")
        function_count = _require_int(function["count"], f"{context}.count")
        branches = _require_array(function["branches"], f"{context}.branches")
        mcdc_records = _require_array(function["mcdc_records"], f"{context}.mcdc_records")
        if branches or mcdc_records:
            raise ValueError(f"{context} содержит unsupported branch/MCDC profile")
        raw_filenames = _require_array(function["filenames"], f"{context}.filenames")
        if not raw_filenames:
            raise ValueError(f"{context}.filenames не может быть пустым")
        filenames = [
            _require_string(filename, f"{context}.filenames[]") for filename in raw_filenames
        ]
        regions_document = _require_array(function["regions"], f"{context}.regions")
        if not regions_document:
            raise ValueError(f"{context}.regions не может быть пустым")
        regions = [
            _parse_region(region, filenames, f"{context}.regions[{index}]")
            for index, region in enumerate(regions_document)
        ]
        main_candidates = set(range(len(filenames)))
        for region in regions:
            if region[7] == 1:
                main_candidates.discard(region[6])
        if len(main_candidates) != 1:
            raise ValueError(
                f"{context} должен иметь ровно один main view filename, "
                f"получено {sorted(main_candidates)}"
            )
        main_file_id = next(iter(main_candidates))
        main_path = normalizer.optional_repository_path(
            filenames[main_file_id], f"{context}.filenames[{main_file_id}]"
        )
        if main_path not in file_universe:
            continue
        definition_region = regions[0]
        if definition_region[5] != main_file_id or definition_region[7] != 0:
            raise ValueError(f"{context} first region не является main CodeRegion")
        definition_prefix = [
            main_path,
            definition_region[0],
            definition_region[1],
        ]
        function_coordinate = _coordinate(["F", *definition_prefix])
        function_counts[function_coordinate] = max(
            function_counts.get(function_coordinate, 0), function_count
        )
        instance_occurrences: dict[tuple[Any, ...], int] = {}
        for region_index, region in enumerate(regions):
            if region[7] != 0:
                continue
            region_path = normalizer.optional_repository_path(
                filenames[region[5]], f"{context}.regions[{region_index}].filename"
            )
            if region_path not in file_universe:
                continue
            structural_region = (
                region_path,
                region[0],
                region[1],
                region[2],
                region[3],
            )
            duplicate_ordinal = instance_occurrences.get(structural_region, 0)
            instance_occurrences[structural_region] = duplicate_ordinal + 1
            region_coordinate = _coordinate(
                ["R", *definition_prefix, *structural_region, duplicate_ordinal]
            )
            region_counts[region_coordinate] = max(
                region_counts.get(region_coordinate, 0), region[4]
            )
    function_universe = set(function_counts)
    function_covered = {
        coordinate for coordinate, count in function_counts.items() if count > 0
    }
    region_universe = set(region_counts)
    region_covered = {coordinate for coordinate, count in region_counts.items() if count > 0}
    return function_universe, function_covered, region_universe, region_covered


def _domain_entry(
    identity_coordinates: list[str],
    covered_indices: set[int],
    universe_indices: Iterable[int],
) -> dict[str, Any]:
    universe_indices = sorted(set(universe_indices))
    selected_covered_indices = [index for index in universe_indices if index in covered_indices]
    selected_universe = [identity_coordinates[index] for index in universe_indices]
    selected_covered = [identity_coordinates[index] for index in selected_covered_indices]
    return {
        "universe_ranges": _ranges(universe_indices),
        "covered_ranges": _ranges(selected_covered_indices),
        "universe_hash": content_hash(selected_universe),
        "covered_hash": content_hash(selected_covered),
        "counts": {"covered": len(selected_covered_indices), "total": len(universe_indices)},
    }


def _scopes_for_coordinate(coordinate: str) -> str:
    parts = json.loads(coordinate)
    return crate_name(parts[1])


def _encode_coordinate(metric: str, coordinate: str, file_index: dict[str, int]) -> list[int]:
    parts = json.loads(coordinate)
    if metric == "lines":
        return [file_index[parts[1]], parts[2]]
    if metric == "functions":
        return [file_index[parts[1]], parts[2], parts[3]]
    return [
        file_index[parts[1]],
        parts[2],
        parts[3],
        file_index[parts[4]],
        parts[5],
        parts[6],
        parts[7],
        parts[8],
        parts[9],
    ]


def _build_surfaces(
    universes: dict[str, set[str]],
    covered: dict[str, set[str]],
    policy: dict[str, Any],
    relative_files: list[str],
    *,
    source_scope: str = "workspace",
) -> dict[str, Any]:
    # Coordinate tuples сортируются по source path/position, а не по строковой
    # записи чисел (`100` не должен предшествовать `20`).
    sorted_universes = {
        metric: sorted(universes[metric], key=lambda coordinate: tuple(json.loads(coordinate)[1:]))
        for metric in METRICS
    }
    file_index = {path: index for index, path in enumerate(relative_files)}
    encoded_universes = {
        metric: [
            _encode_coordinate(metric, coordinate, file_index)
            for coordinate in sorted_universes[metric]
        ]
        for metric in METRICS
    }
    covered_indices = {
        metric: {
            index
            for index, coordinate in enumerate(sorted_universes[metric])
            if coordinate in covered[metric]
        }
        for metric in METRICS
    }
    expected_crates = set(policy["blocking_crates"]) | set(policy["informational_crates"])
    owner_indices: dict[str, dict[str, list[int]]] = {}
    observed_crates: set[str] = set()
    for metric in METRICS:
        owner_indices[metric] = {owner: [] for owner in expected_crates}
        for index, coordinate in enumerate(sorted_universes[metric]):
            owner = _scopes_for_coordinate(coordinate)
            observed_crates.add(owner)
            owner_indices[metric].setdefault(owner, []).append(index)
    missing_crates = expected_crates - observed_crates
    if missing_crates and source_scope == "workspace":
        raise ValueError(f"coverage coordinate universe не содержит crate-ы: {sorted(missing_crates)}")
    domains: dict[str, dict[str, Any]] = {}
    domain_crates: dict[str, set[str]] = {
        "workspace": expected_crates,
        "blocking-group": set(policy["blocking_crates"]),
    }
    domain_crates.update(
        {f"crate:{owner}": {owner} for owner in policy["blocking_crates"]}
    )
    for domain_name, owners in domain_crates.items():
        domains[domain_name] = {}
        for metric in METRICS:
            selected_indices = [
                index for owner in owners for index in owner_indices[metric].get(owner, [])
            ]
            domains[domain_name][metric] = _domain_entry(
                sorted_universes[metric],
                covered_indices[metric],
                selected_indices,
            )
    return {
        "coordinates": {
            metric: {
                "universe": encoded_universes[metric],
                "universe_hash": content_hash(sorted_universes[metric]),
            }
            for metric in METRICS
        },
        "domains": domains,
    }


def extract_run_state(
    llvm_report: Any,
    policy_document: Any,
    profile_document: Any,
    repo_root: Path,
    run_label: str,
    *,
    source_scope: str = "workspace",
) -> dict[str, Any]:
    if source_scope not in {"workspace", "executable"}:
        raise ValueError("неизвестный source scope")
    policy = validate_policy(policy_document)
    profile = validate_profile_manifest(profile_document, policy)
    if not isinstance(run_label, str) or not re.fullmatch(r"[A-Za-z0-9._-]+", run_label):
        raise ValueError("run-label должен быть ASCII идентификатором [A-Za-z0-9._-]+")
    report = _require_object(llvm_report, "LLVM report")
    _require_exact_keys(report, {"type", "version", "cargo_llvm_cov", "data"}, "LLVM report")
    if report.get("type") != LLVM_EXPORT_TYPE or report.get("version") != LLVM_EXPORT_VERSION:
        raise ValueError("ожидался полный LLVM coverage JSON export 3.1.0")
    cargo_metadata = _require_object(report.get("cargo_llvm_cov"), "cargo_llvm_cov")
    _require_exact_keys(cargo_metadata, {"version", "manifest_path"}, "cargo_llvm_cov")
    if cargo_metadata.get("version") != profile["cargo_llvm_cov_version"]:
        raise ValueError("LLVM report создан другой версией cargo-llvm-cov")
    normalizer = SourcePathNormalizer(repo_root)
    expected_manifest = normalizer.repo_root / "Cargo.toml"
    if Path(_require_string(cargo_metadata.get("manifest_path"), "manifest_path")).resolve(
        strict=False
    ) != expected_manifest:
        raise ValueError("cargo_llvm_cov.manifest_path не совпадает с --repo-root")
    data = _require_array(report.get("data"), "LLVM report.data")
    if len(data) != 1:
        raise ValueError("ожидался ровно один merged LLVM coverage datum")
    datum = _require_object(data[0], "data[0]")
    _require_exact_keys(datum, {"files", "functions", "totals"}, "data[0]")

    expected_crates = set(policy["blocking_crates"]) | set(policy["informational_crates"])
    exclusions = set(policy["excluded_source_paths"])
    files = _require_array(datum["files"], "data[0].files")
    relative_files: list[str] = []
    line_universe: set[str] = set()
    line_covered: set[str] = set()
    legacy_by_crate: dict[str, dict[str, dict[str, int]]] = {}
    file_summary_totals = {
        metric: {"covered": 0, "total": 0} for metric in SUMMARY_METRICS
    }
    for file_index, file_document in enumerate(files):
        context = f"data[0].files[{file_index}]"
        file_entry = _require_object(file_document, context)
        required_file_keys = {
            "filename",
            "segments",
            "branches",
            "mcdc_records",
            "expansions",
            "summary",
        }
        _require_exact_keys(file_entry, required_file_keys, context)
        unsupported_arrays = {
            key: _require_array(file_entry[key], f"{context}.{key}")
            for key in ("branches", "mcdc_records", "expansions")
        }
        if any(unsupported_arrays.values()):
            raise ValueError(f"{context} содержит unsupported branch/MCDC/expansion profile")
        relative_path = normalizer.repository_path(file_entry["filename"], f"{context}.filename")
        if relative_path in exclusions:
            continue
        owner = crate_name(relative_path)
        if owner not in expected_crates:
            raise ValueError(f"crate `{owner}` отсутствует в coverage policy")
        if relative_path in relative_files:
            raise ValueError(f"files[] содержит duplicate normalized path `{relative_path}`")
        relative_files.append(relative_path)
        file_lines, covered_file_lines = _line_coordinates(
            file_entry["segments"], relative_path, context
        )
        line_universe.update(file_lines)
        line_covered.update(covered_file_lines)
        owner_summary = legacy_by_crate.setdefault(
            owner, {metric: {"covered": 0, "total": 0} for metric in METRICS}
        )
        parsed_summary = _validate_summary(file_entry["summary"], f"{context}.summary")
        for metric in SUMMARY_METRICS:
            for counter in ("covered", "total"):
                file_summary_totals[metric][counter] += parsed_summary[metric][counter]
        for metric in METRICS:
            metric_summary = parsed_summary[metric]
            owner_summary[metric]["covered"] += metric_summary["covered"]
            owner_summary[metric]["total"] += metric_summary["total"]

    relative_files.sort()
    if len(relative_files) != len(set(relative_files)):
        raise ValueError("files[] содержит duplicate normalized path")
    observed_crates = {crate_name(path) for path in relative_files}
    missing_crates = expected_crates - observed_crates
    if missing_crates and source_scope == "workspace":
        raise ValueError(f"files[] не содержит policy crate-ы: {sorted(missing_crates)}")
    file_universe = set(relative_files)
    functions, covered_functions, regions, covered_regions = _function_coordinates(
        datum["functions"], file_universe, normalizer
    )
    universes = {"lines": line_universe, "functions": functions, "regions": regions}
    covered = {
        "lines": line_covered,
        "functions": covered_functions,
        "regions": covered_regions,
    }
    stable_source = _build_surfaces(
        universes, covered, policy, relative_files, source_scope=source_scope
    )

    parsed_totals = _validate_summary(datum["totals"], "data[0].totals")
    if file_summary_totals != parsed_totals:
        raise ValueError(
            "сумма files[].summary не совпадает с data[0].totals: "
            f"files={file_summary_totals}, totals={parsed_totals}"
        )
    totals = {metric: parsed_totals[metric] for metric in METRICS}
    legacy_workspace = _sum_metrics(legacy_by_crate.values())
    if legacy_workspace != totals:
        raise ValueError(
            "сумма files[].summary не совпадает с data[0].totals: "
            f"files={legacy_workspace}, totals={totals}"
        )
    derived_totals = {
        metric: {"covered": len(covered[metric]), "total": len(universes[metric])}
        for metric in METRICS
    }
    if derived_totals["functions"] != totals["functions"]:
        raise ValueError(
            "function definition groups не совпадают с LLVM summary: "
            f"derived={derived_totals['functions']}, llvm={totals['functions']}"
        )
    if derived_totals["regions"]["total"] != totals["regions"]["total"]:
        raise ValueError("CodeRegion universe total не совпадает с LLVM summary")
    region_covered_delta = (
        derived_totals["regions"]["covered"] - totals["regions"]["covered"]
    )
    cross_check = {
        "lines": {
            # JSON summary складывает FunctionCoverageSummary; source surface
            # намеренно следует LineCoverageStats по объединённым file segments.
            "category": "source-lines-vs-function-summary",
            "derived": derived_totals["lines"],
            "llvm": totals["lines"],
            "covered_delta": (
                derived_totals["lines"]["covered"] - totals["lines"]["covered"]
            ),
            "total_delta": derived_totals["lines"]["total"] - totals["lines"]["total"],
        },
        "functions": {
            "category": "llvm-instantiation-group-exact",
            "derived": derived_totals["functions"],
            "llvm": totals["functions"],
        },
        "regions": {
            "category": (
                "llvm-code-region-covered-combination-difference"
                if region_covered_delta
                else "llvm-code-region-exact"
            ),
            "derived": derived_totals["regions"],
            "llvm": totals["regions"],
            "covered_delta": region_covered_delta,
        },
    }
    for owner in missing_crates:
        legacy_by_crate[owner] = {metric: {"covered": 0, "total": 0} for metric in METRICS}
    legacy_domains = _legacy_domains(legacy_by_crate, policy)
    state = {
        "schema_version": RUN_SCHEMA_VERSION,
        "kind": "coverage-coordinate-run",
        "run_label": run_label,
        "provenance": {
            "profile": profile["profile"],
            "methodology": profile["methodology"],
            "llvm_cov_version": profile["llvm_cov_version"],
            "llvm_export_version": LLVM_EXPORT_VERSION,
            "cargo_llvm_cov_version": profile["cargo_llvm_cov_version"],
            "policy_hash": content_hash(policy),
            "profile_manifest_hash": content_hash(profile),
        },
        "source_files": {"universe": relative_files, "hash": content_hash(relative_files)},
        "stable_source": stable_source,
        "legacy_report_only": {"domains": legacy_domains, "cross_check": cross_check},
    }
    state["state_hash"] = content_hash(state)
    return state


def _sum_metrics(
    summaries: Iterable[dict[str, dict[str, int]]]
) -> dict[str, dict[str, int]]:
    result = {metric: {"covered": 0, "total": 0} for metric in METRICS}
    for summary in summaries:
        for metric in METRICS:
            result[metric]["covered"] += summary[metric]["covered"]
            result[metric]["total"] += summary[metric]["total"]
    return result


def _legacy_domains(
    by_crate: dict[str, dict[str, dict[str, int]]], policy: dict[str, Any]
) -> dict[str, dict[str, dict[str, int]]]:
    return {
        "workspace": _sum_metrics(by_crate.values()),
        "blocking-group": _sum_metrics(
            by_crate[owner] for owner in policy["blocking_crates"]
        ),
        **{
            f"crate:{owner}": by_crate[owner] for owner in policy["blocking_crates"]
        },
    }


def parse_args(arguments: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Извлечь machine-independent source coordinates из full LLVM JSON3.1."
    )
    subcommands = parser.add_subparsers(dest="command", required=True)
    extract = subcommands.add_parser("extract", help="создать state одного coverage run")
    extract.add_argument("--policy", type=Path, required=True)
    extract.add_argument("--repo-root", type=Path, required=True)
    extract.add_argument("--input", type=Path, required=True, metavar="FULL_JSON")
    extract.add_argument("--profile-manifest", type=Path, required=True)
    extract.add_argument("--run-label", required=True)
    extract.add_argument("--object-reports", type=Path)
    extract.add_argument("--output", type=Path, required=True, metavar="STATE_JSON")
    return parser.parse_args(arguments)


def main(arguments: list[str] | None = None) -> int:
    parsed = parse_args(arguments)
    try:
        if parsed.command == "extract":
            state = extract_run_state(
                read_json(parsed.input),
                read_json(parsed.policy),
                read_json(parsed.profile_manifest),
                parsed.repo_root,
                parsed.run_label,
            )
            if parsed.object_reports is not None:
                from coverage_object_union import combine_object_reports

                state = combine_object_reports(
                    state, parsed.object_reports, read_json(parsed.policy),
                    read_json(parsed.profile_manifest), parsed.repo_root, parsed.run_label,
                )
            write_json_atomic(parsed.output, state)
            print(f"Stable coverage run state записан: {parsed.output}")
            return 0
        raise ValueError(f"неизвестная команда {parsed.command}")
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"coverage coordinate extraction error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

"""Fail-closed schema validation для stable coverage artifacts."""

from __future__ import annotations

import datetime as dt
import re
from pathlib import Path
from typing import Any

import coverage_coordinate_model as model
import coverage_legacy_schema as legacy


COHORT_SCHEMA_VERSION = 1
BASELINE_SCHEMA_VERSION = 2
EXCEPTION_SCHEMA_VERSION = 1
RUN_LABEL_PATTERN = re.compile(r"^[A-Za-z0-9._-]+$")
SHA256_PATTERN = re.compile(r"^sha256:[0-9a-f]{64}$")


def _exact_keys(document: dict[str, Any], expected: set[str], context: str) -> None:
    actual = set(document)
    if actual != expected:
        raise ValueError(
            f"{context} имеет неверные поля; missing={sorted(expected - actual)}, "
            f"unexpected={sorted(actual - expected)}"
        )


def _object(value: Any, context: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{context} должен быть JSON object")
    return value


def _array(value: Any, context: str) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError(f"{context} должен быть JSON array")
    return value


def _string(value: Any, context: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{context} должен быть непустой строкой")
    return value


def _integer(value: Any, context: str, *, minimum: int = 0) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{context} должен быть integer >= {minimum}")
    if value >= model.INT64_MAX:
        raise ValueError(f"{context} содержит INT64_MAX sentinel/overflow")
    return value


def _hash(value: Any, expected_hash: Any, context: str) -> None:
    expected_hash = _string(expected_hash, f"{context}.hash")
    if not SHA256_PATTERN.fullmatch(expected_hash) or model.content_hash(value) != expected_hash:
        raise ValueError(f"{context} имеет неверный SHA-256")


def _reject_sensitive_strings(value: Any, context: str) -> None:
    """Versioned artifacts не должны становиться контейнером machine paths/секретов."""

    if isinstance(value, dict):
        for key, nested in value.items():
            lowered_key = str(key).lower()
            if any(marker in lowered_key for marker in ("authorization", "cookie", "secret", "token", "manifest_path")):
                raise ValueError(f"{context} содержит запрещённое поле `{key}`")
            _reject_sensitive_strings(nested, f"{context}.{key}")
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            _reject_sensitive_strings(nested, f"{context}[{index}]")
    elif isinstance(value, str) and (value.startswith("/") or re.match(r"^[A-Za-z]:[\\/]", value)):
        raise ValueError(f"{context} содержит абсолютный путь")


def _expand_ranges(ranges: Any, limit: int, context: str) -> list[int]:
    ranges = _array(ranges, context)
    indices: list[int] = []
    previous_end = -1
    for range_index, range_document in enumerate(ranges):
        bounds = _array(range_document, f"{context}[{range_index}]")
        if len(bounds) != 2:
            raise ValueError(f"{context}[{range_index}] должен иметь start/end")
        start = _integer(bounds[0], f"{context}[{range_index}].start")
        end = _integer(bounds[1], f"{context}[{range_index}].end")
        if start >= end or end > limit:
            raise ValueError(f"{context}[{range_index}] выходит за universe")
        # Adjacent ranges обязаны быть слиты: иначе одинаковый set имеет много JSON forms.
        if start <= previous_end:
            raise ValueError(f"{context} не является canonical RLE")
        indices.extend(range(start, end))
        previous_end = end
    return indices


def _validate_relative_file(value: Any, context: str) -> str:
    source_path = Path(_string(value, context))
    if source_path.is_absolute() or ".." in source_path.parts:
        raise ValueError(f"{context} должен быть repo-relative и не содержать `..`")
    normalized = source_path.as_posix()
    if normalized != str(value) or len(source_path.parts) < 3 or source_path.parts[0] != "crates":
        raise ValueError(f"{context} имеет неканонический first-party path")
    return normalized


def _validate_source_files(document: Any, context: str) -> list[str]:
    files_document = _object(document, context)
    _exact_keys(files_document, {"universe", "hash"}, context)
    files = [
        _validate_relative_file(path, f"{context}[{index}]")
        for index, path in enumerate(_array(files_document["universe"], f"{context}.universe"))
    ]
    if files != sorted(set(files)):
        raise ValueError(f"{context} должен быть sorted/unique")
    _hash(files, files_document["hash"], context)
    return files


def _coordinate_shape(metric: str) -> int:
    return {"lines": 2, "functions": 3, "regions": 9}[metric]


def _validate_coordinate(
    coordinate: Any, metric: str, file_count: int, context: str
) -> list[int]:
    coordinate = _array(coordinate, context)
    if len(coordinate) != _coordinate_shape(metric):
        raise ValueError(f"{context} имеет неверную coordinate arity")
    values = [_integer(value, f"{context}[]") for value in coordinate]
    file_positions = (0,) if metric != "regions" else (0, 3)
    for position in file_positions:
        if values[position] >= file_count:
            raise ValueError(f"{context} ссылается на отсутствующий source file")
    source_position_indices = (
        {1} if metric == "lines" else {1, 2} if metric == "functions" else {1, 2, 4, 5, 6, 7}
    )
    if any(values[position] == 0 for position in source_position_indices):
        raise ValueError(f"{context} source positions начинаются с 1")
    if metric == "regions" and (values[6], values[7]) < (values[4], values[5]):
        raise ValueError(f"{context} имеет обратный CodeRegion range")
    return values


def _expected_domains(policy: dict[str, Any]) -> set[str]:
    return {"workspace", "blocking-group"} | {
        f"crate:{owner}" for owner in policy["blocking_crates"]
    }


def _validate_provenance(provenance: Any, context: str) -> dict[str, Any]:
    provenance = _object(provenance, context)
    _exact_keys(
        provenance,
        {
            "profile",
            "methodology",
            "llvm_cov_version",
            "llvm_export_version",
            "cargo_llvm_cov_version",
            "policy_hash",
            "profile_manifest_hash",
        },
        context,
    )
    for key, value in provenance.items():
        _string(value, f"{context}.{key}")
    if provenance["profile"] != "workspace":
        raise ValueError(f"{context}.profile должен быть workspace")
    if provenance["methodology"] != model.METHODOLOGY:
        raise ValueError(f"{context}.methodology неизвестна")
    if provenance["llvm_cov_version"] != model.LLVM_COV_VERSION:
        raise ValueError(f"{context}.llvm_cov_version неизвестна")
    if provenance["llvm_export_version"] != model.LLVM_EXPORT_VERSION:
        raise ValueError(f"{context}.llvm_export_version неизвестна")
    if provenance["cargo_llvm_cov_version"] != model.CARGO_LLVM_COV_VERSION:
        raise ValueError(f"{context}.cargo_llvm_cov_version неизвестна")
    for hash_name in ("policy_hash", "profile_manifest_hash"):
        if not SHA256_PATTERN.fullmatch(provenance[hash_name]):
            raise ValueError(f"{context}.{hash_name} должен быть SHA-256")
    return provenance


def _validate_domain_metric(
    entry: Any,
    universe: list[list[int]],
    identities: list[str],
    context: str,
    *,
    mode: str,
) -> dict[str, list[int]]:
    entry = _object(entry, context)
    if mode == "cohort":
        expected = {
            "universe_ranges",
            "stable_ranges",
            "variable_ranges",
            "uncovered_ranges",
            "universe_hash",
            "stable_hash",
            "counts",
        }
    elif mode == "baseline":
        expected = {
            "universe_ranges",
            "stable_ranges",
            "universe_hash",
            "stable_hash",
            "counts",
        }
    else:
        expected = {
            "universe_ranges",
            "covered_ranges",
            "universe_hash",
            "covered_hash",
            "counts",
        }
    _exact_keys(entry, expected, context)
    universe_indices = _expand_ranges(
        entry["universe_ranges"], len(universe), f"{context}.universe_ranges"
    )
    selected_universe = [identities[index] for index in universe_indices]
    _hash(selected_universe, entry["universe_hash"], f"{context}.universe")
    if mode == "cohort":
        classes = {
            name: _expand_ranges(entry[f"{name}_ranges"], len(universe), f"{context}.{name}_ranges")
            for name in ("stable", "variable", "uncovered")
        }
        union = set().union(*(set(indices) for indices in classes.values()))
        if union != set(universe_indices) or sum(len(indices) for indices in classes.values()) != len(union):
            raise ValueError(f"{context} classes не образуют disjoint universe partition")
        _hash(
            [identities[index] for index in classes["stable"]],
            entry["stable_hash"],
            f"{context}.stable",
        )
        counts = _object(entry["counts"], f"{context}.counts")
        _exact_keys(counts, {"stable", "variable", "uncovered", "total"}, f"{context}.counts")
        expected_counts = {name: len(classes[name]) for name in classes}
        expected_counts["total"] = len(universe_indices)
        if any(_integer(counts[name], f"{context}.counts.{name}") != value for name, value in expected_counts.items()):
            raise ValueError(f"{context}.counts не совпадают с ranges")
        return {"universe": universe_indices, **classes}
    covered_name = "stable" if mode == "baseline" else "covered"
    covered_indices = _expand_ranges(
        entry[f"{covered_name}_ranges"], len(universe), f"{context}.{covered_name}_ranges"
    )
    if not set(covered_indices) <= set(universe_indices):
        raise ValueError(f"{context}.covered не является subset domain universe")
    _hash(
        [identities[index] for index in covered_indices],
        entry[f"{covered_name}_hash"],
        f"{context}.{covered_name}",
    )
    counts = _object(entry["counts"], f"{context}.counts")
    _exact_keys(counts, {covered_name, "total"}, f"{context}.counts")
    if _integer(counts[covered_name], f"{context}.counts.{covered_name}") != len(covered_indices):
        raise ValueError(f"{context}.counts.{covered_name} не совпадает с ranges")
    if _integer(counts["total"], f"{context}.counts.total") != len(universe_indices):
        raise ValueError(f"{context}.counts.total не совпадает с ranges")
    return {"universe": universe_indices, covered_name: covered_indices}


def _validate_source_surface(
    surface: Any,
    source_files: list[str],
    expected_domains: set[str] | None,
    context: str,
    *,
    mode: str,
) -> dict[str, Any]:
    surface = _object(surface, context)
    _exact_keys(surface, {"coordinates", "domains"}, context)
    coordinate_documents = _object(surface["coordinates"], f"{context}.coordinates")
    _exact_keys(coordinate_documents, set(model.METRICS), f"{context}.coordinates")
    universes: dict[str, list[list[int]]] = {}
    identities: dict[str, list[str]] = {}
    for metric in model.METRICS:
        coordinate_document = _object(
            coordinate_documents[metric], f"{context}.coordinates.{metric}"
        )
        _exact_keys(
            coordinate_document, {"universe", "universe_hash"}, f"{context}.coordinates.{metric}"
        )
        universe = [
            _validate_coordinate(entry, metric, len(source_files), f"{context}.{metric}[{index}]")
            for index, entry in enumerate(_array(coordinate_document["universe"], f"{context}.{metric}"))
        ]
        if universe != sorted(universe) or len(universe) != len({tuple(entry) for entry in universe}):
            raise ValueError(f"{context}.{metric} universe должен быть sorted/unique")
        metric_identities = [
            model.coordinate_identity(metric, coordinate, source_files)
            for coordinate in universe
        ]
        _hash(
            metric_identities,
            coordinate_document["universe_hash"],
            f"{context}.{metric}.universe",
        )
        universes[metric] = universe
        identities[metric] = metric_identities
    domains = _object(surface["domains"], f"{context}.domains")
    if expected_domains is not None and set(domains) != expected_domains:
        raise ValueError(f"{context}.domains не совпадает с coverage policy")
    domain_names = set(domains)
    if not {"workspace", "blocking-group"} <= domain_names:
        raise ValueError(f"{context}.domains должен содержать workspace/blocking-group")
    source_owners = {model.crate_name(path) for path in source_files}
    crate_domains: dict[str, str] = {}
    for domain_name in domain_names - {"workspace", "blocking-group"}:
        if not domain_name.startswith("crate:") or not domain_name.removeprefix("crate:"):
            raise ValueError(f"{context}.domains содержит неканонический domain `{domain_name}`")
        owner = domain_name.removeprefix("crate:")
        model.canonical_crate_owner(owner, f"{context}.domains.{domain_name}")
        if owner not in source_owners:
            raise ValueError(f"{context}.domains ссылается на отсутствующий crate `{owner}`")
        crate_domains[domain_name] = owner
    parsed_domains: dict[str, Any] = {}
    for domain_name, domain_document in domains.items():
        _string(domain_name, f"{context}.domain name")
        domain = _object(domain_document, f"{context}.domains.{domain_name}")
        _exact_keys(domain, set(model.METRICS), f"{context}.domains.{domain_name}")
        parsed_domains[domain_name] = {
            metric: _validate_domain_metric(
                domain[metric],
                universes[metric],
                identities[metric],
                f"{context}.{domain_name}.{metric}",
                mode=mode,
            )
            for metric in model.METRICS
        }
    blocking_owners = set(crate_domains.values())
    owner_indices: dict[str, dict[str, set[int]]] = {}
    for metric in model.METRICS:
        owner_indices[metric] = {owner: set() for owner in blocking_owners}
        for index, coordinate in enumerate(universes[metric]):
            owner = model.crate_name(source_files[coordinate[0]])
            if owner in blocking_owners:
                owner_indices[metric][owner].add(index)
    for metric in model.METRICS:
        complete_universe = set(range(len(universes[metric])))
        if set(parsed_domains["workspace"][metric]["universe"]) != complete_universe:
            raise ValueError(f"{context}.workspace.{metric} не владеет полным universe")
        blocking_indices: set[int] = set()
        for domain_name, owner in crate_domains.items():
            expected_indices = owner_indices[metric][owner]
            actual_indices = set(parsed_domains[domain_name][metric]["universe"])
            if actual_indices != expected_indices:
                raise ValueError(
                    f"{context}.{domain_name}.{metric} не совпадает с owner coordinates"
                )
            blocking_indices.update(actual_indices)
        if set(parsed_domains["blocking-group"][metric]["universe"]) != blocking_indices:
            raise ValueError(
                f"{context}.blocking-group.{metric} не равен union crate domains"
            )
    return {"universes": universes, "identities": identities, "domains": parsed_domains}


def validate_run_state(
    document: Any, policy: dict[str, Any] | None = None
) -> dict[str, Any]:
    state = _object(document, "run state")
    _exact_keys(
        state,
        {
            "schema_version",
            "kind",
            "run_label",
            "provenance",
            "source_files",
            "stable_source",
            "legacy_report_only",
            "state_hash",
        },
        "run state",
    )
    if _integer(state["schema_version"], "run.schema_version") != model.RUN_SCHEMA_VERSION:
        raise ValueError("run state schema_version неизвестна")
    if state["kind"] != "coverage-coordinate-run":
        raise ValueError("run state kind неизвестен")
    if not RUN_LABEL_PATTERN.fullmatch(_string(state["run_label"], "run.run_label")):
        raise ValueError("run.run_label имеет неканонический формат")
    _validate_provenance(state["provenance"], "run.provenance")
    expected_domains: set[str] | None = None
    if policy is not None:
        policy = model.validate_policy(policy)
        if state["provenance"]["policy_hash"] != model.content_hash(policy):
            raise ValueError("run provenance policy_hash не совпадает с policy")
        expected_domains = _expected_domains(policy)
    files = _validate_source_files(state["source_files"], "run.source_files")
    parsed_surface = _validate_source_surface(
        state["stable_source"], files, expected_domains, "run.stable_source", mode="run"
    )
    legacy.validate_run_report(
        state["legacy_report_only"], "run.legacy_report_only", set(parsed_surface["domains"])
    )
    _reject_sensitive_strings(state, "run state")
    hash_payload = dict(state)
    state_hash = hash_payload.pop("state_hash")
    _hash(hash_payload, state_hash, "run.state")
    return state


def validate_cohort(document: Any) -> dict[str, Any]:
    cohort = _object(document, "cohort")
    _exact_keys(
        cohort,
        {
            "schema_version",
            "kind",
            "provenance",
            "run_set",
            "source_files",
            "stable_source",
            "legacy_report_only",
            "cohort_hash",
        },
        "cohort",
    )
    if _integer(cohort["schema_version"], "cohort.schema_version") != 1:
        raise ValueError("cohort schema_version неизвестна")
    if cohort["kind"] != "coverage-stable-cohort":
        raise ValueError("cohort kind неизвестен")
    _validate_provenance(cohort["provenance"], "cohort.provenance")
    run_set = _array(cohort["run_set"], "cohort.run_set")
    if len(run_set) != 3:
        raise ValueError("cohort run_set должен содержать ровно три запуска")
    normalized_run_set = []
    for index, entry_document in enumerate(run_set):
        entry = _object(entry_document, f"cohort.run_set[{index}]")
        _exact_keys(entry, {"run_label", "state_hash"}, f"cohort.run_set[{index}]")
        label = _string(entry["run_label"], "run_label")
        state_hash = _string(entry["state_hash"], "state_hash")
        if not RUN_LABEL_PATTERN.fullmatch(label):
            raise ValueError("cohort run_set содержит неканонический run_label")
        if not SHA256_PATTERN.fullmatch(state_hash):
            raise ValueError("cohort run_set state_hash должен быть SHA-256")
        normalized_run_set.append((label, state_hash))
    if normalized_run_set != sorted(normalized_run_set) or len({item[0] for item in normalized_run_set}) != 3:
        raise ValueError("cohort run_set должен быть sorted/unique")
    files = _validate_source_files(cohort["source_files"], "cohort.source_files")
    parsed_surface = _validate_source_surface(
        cohort["stable_source"], files, None, "cohort.stable_source", mode="cohort"
    )
    legacy.validate_cohort_report(
        cohort["legacy_report_only"],
        "cohort.legacy_report_only",
        normalized_run_set,
        set(parsed_surface["domains"]),
    )
    _reject_sensitive_strings(cohort, "cohort")
    payload = dict(cohort)
    cohort_hash = payload.pop("cohort_hash")
    _hash(payload, cohort_hash, "cohort")
    return cohort


def validate_baseline(document: Any) -> dict[str, Any]:
    baseline = _object(document, "stable baseline")
    _exact_keys(
        baseline,
        {
            "schema_version",
            "kind",
            "provenance",
            "source_files",
            "stable_source",
            "legacy_report_only",
            "baseline_hash",
        },
        "stable baseline",
    )
    if (
        _integer(baseline.get("schema_version"), "baseline.schema_version")
        != BASELINE_SCHEMA_VERSION
        or baseline.get("kind") != "coverage-stable-baseline"
    ):
        raise ValueError("stable baseline schema/kind неизвестны")
    _validate_provenance(baseline["provenance"], "baseline.provenance")
    # Baseline surface является cohort surface без variable/uncovered classes.
    _validate_baseline_surface(baseline)
    legacy.validate_baseline_report(
        baseline["legacy_report_only"], "baseline.legacy_report_only"
    )
    _reject_sensitive_strings(baseline, "stable baseline")
    payload = dict(baseline)
    baseline_hash = payload.pop("baseline_hash")
    _hash(payload, baseline_hash, "stable baseline")
    return baseline


def _validate_baseline_surface(baseline: dict[str, Any]) -> None:
    files = _validate_source_files(baseline["source_files"], "baseline.source_files")
    _validate_source_surface(
        baseline["stable_source"], files, None, "baseline.stable_source", mode="baseline"
    )


def validate_measurement_exceptions(document: Any) -> dict[tuple[str, str], dict[str, Any]]:
    manifest = _object(document, "measurement exceptions")
    _exact_keys(manifest, {"schema_version", "measurement_exceptions"}, "measurement exceptions")
    if manifest["schema_version"] != EXCEPTION_SCHEMA_VERSION:
        raise ValueError("measurement exceptions schema_version неизвестна")
    index: dict[tuple[str, str], dict[str, Any]] = {}
    required = {
        "domain",
        "metric",
        "previous",
        "allowed",
        "previous_universe_hash",
        "current_universe_hash",
        "reason",
        "review_by",
        "follow_up",
    }
    for entry_index, entry_document in enumerate(_array(manifest["measurement_exceptions"], "measurement exceptions")):
        entry = _object(entry_document, f"measurement exceptions[{entry_index}]")
        _exact_keys(entry, required, f"measurement exceptions[{entry_index}]")
        domain = _string(entry["domain"], "exception.domain")
        metric = _string(entry["metric"], "exception.metric")
        if metric not in model.METRICS or not (
            domain in {"workspace", "blocking-group"}
            or (domain.startswith("crate:") and bool(domain.removeprefix("crate:")))
        ):
            raise ValueError("measurement exception domain/metric неизвестны")
        if domain.startswith("crate:"):
            model.canonical_crate_owner(
                domain.removeprefix("crate:"), "measurement exception domain"
            )
        for text_field in ("reason", "follow_up"):
            _string(entry[text_field], f"exception.{text_field}")
        review_by = dt.date.fromisoformat(_string(entry["review_by"], "exception.review_by"))
        if review_by < dt.date.today():
            raise ValueError(f"measurement exception {domain}/{metric} просрочена")
        for counter_name in ("previous", "allowed"):
            counters = _object(entry[counter_name], f"exception.{counter_name}")
            _exact_keys(counters, {"stable", "total"}, f"exception.{counter_name}")
            stable = _integer(counters["stable"], f"exception.{counter_name}.stable")
            total = _integer(counters["total"], f"exception.{counter_name}.total")
            if stable > total:
                raise ValueError("measurement exception stable превышает total")
        for hash_name in ("previous_universe_hash", "current_universe_hash"):
            if not SHA256_PATTERN.fullmatch(_string(entry[hash_name], f"exception.{hash_name}")):
                raise ValueError("measurement exception требует SHA-256 universe hashes")
        key = (domain, metric)
        if key in index:
            raise ValueError(f"measurement exceptions содержит duplicate {domain}/{metric}")
        index[key] = entry
    _reject_sensitive_strings(manifest, "measurement exceptions")
    return index

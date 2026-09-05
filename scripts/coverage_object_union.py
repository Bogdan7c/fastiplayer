"""Объединение exact coordinates отдельных ELF; execution counts не складываются."""

from __future__ import annotations

import copy
import gzip
import json
from pathlib import Path
from typing import Any

from coverage_coordinate_model import (
    METRICS, content_hash, coordinate_identity, read_json,
    _reject_duplicate_keys, _reject_non_finite_constant,
    require_array, require_exact_keys, require_object,
)
from coverage_coordinates import _build_surfaces, extract_run_state
from coverage_runner_support import sha256_file


def combine_object_reports(
    legacy_state: dict[str, Any],
    manifest_path: Path,
    policy: dict[str, Any],
    profile: dict[str, Any],
    repo_root: Path,
    run_label: str,
) -> dict[str, Any]:
    """Сохраняет legacy diagnostics, заменяя stable surface точным OR внутри run."""
    manifest = require_object(read_json(manifest_path), "object export manifest")
    require_exact_keys(manifest, {"schema_version", "kind", "source", "profile_sha256",
                                  "objects", "exports"}, "object export manifest")
    if manifest.get("schema_version") != 1 or manifest.get("kind") != "coverage-per-executable-exports":
        raise ValueError("неизвестный per-executable export manifest")
    exports = require_array(manifest["exports"], "object exports")
    require_array(manifest["objects"], "export object inventory")
    if not exports or len(exports) != len(manifest["objects"]):
        raise ValueError("неполный object export set")
    if manifest["objects"] != [entry["object"] for entry in exports]:
        raise ValueError("object export order/set не совпадает с manifest")
    if len({entry["object"] for entry in exports}) != len(exports):
        raise ValueError("duplicate object export")
    universes = {metric: set() for metric in METRICS}
    covered = {metric: set() for metric in METRICS}
    source_files: set[str] = set()
    for entry in exports:
        require_exact_keys(require_object(entry, "object export"),
                           {"object", "object_sha256", "report", "sha256"}, "object export")
        name = entry["report"]
        if Path(name).name != name:
            raise ValueError("object report path должен быть локальным basename")
        report_path = manifest_path.parent / name
        if report_path.is_symlink() or sha256_file(report_path) != entry["sha256"]:
            raise ValueError("object report hash mismatch")
        with gzip.open(report_path, "rt") as archive:
            report = json.load(archive, object_pairs_hook=_reject_duplicate_keys,
                               parse_constant=_reject_non_finite_constant)
        # Wrapper/tool provenance остаётся общей: профили индексирует pinned cargo-llvm-cov.
        report["cargo_llvm_cov"] = {
            "version": profile["cargo_llvm_cov_version"],
            "manifest_path": str(repo_root / "Cargo.toml"),
        }
        state = extract_run_state(report, policy, profile, repo_root, run_label,
                                  source_scope="executable")
        files = state["source_files"]["universe"]
        source_files.update(files)
        for metric in METRICS:
            coordinates = state["stable_source"]["coordinates"][metric]["universe"]
            identities = [coordinate_identity(metric, c, files) for c in coordinates]
            universes[metric].update(identities)
            ranges = state["stable_source"]["domains"]["workspace"][metric]["covered_ranges"]
            covered[metric].update(identities[i] for start, end in ranges for i in range(start, end))
    if source_files != set(legacy_state["source_files"]["universe"]):
        raise ValueError("per-object source inventory отличается от полного wrapper export")
    result = copy.deepcopy(legacy_state)
    result["stable_source"] = _build_surfaces(universes, covered, policy, sorted(source_files))
    result.pop("state_hash")
    result["state_hash"] = content_hash(result)
    return result

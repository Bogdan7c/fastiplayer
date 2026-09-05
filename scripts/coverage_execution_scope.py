"""Разделяет ответственность hosted software gate и локального GBM gate.

Измерение и baseline остаются полными. Только exact потери во владельцах
реального GBM ресурса относятся к обязательной локальной qualification.
Смена universe, provenance или ratios никогда не разрешается этим разделением.
"""

from __future__ import annotations

import copy
import hashlib
from pathlib import Path
from typing import Any

from coverage_coordinate_model import read_json
from test_execution_scope import LOCAL_HARDWARE_TESTS, TestExecutionScope


LOCAL_HARDWARE_SOURCES = frozenset({
    "crates/video-vaapi/src/gbm_allocator.rs",
    "crates/video-vaapi/src/linear_gbm_frame.rs",
    "crates/video-vaapi/src/dma_heap.rs",
})


def validate_hosted_cohort_manifest(cohort_path: Path) -> None:
    """Связывает hosted scope с exact проверяемым cohort, а не соседним stale report."""

    manifest = read_json(cohort_path.parent / "cohort-manifest.json")
    if not isinstance(manifest, dict):
        raise ValueError("hosted cohort manifest должен быть object")
    expected = {"name": "hosted", "local_hardware_tests": list(LOCAL_HARDWARE_TESTS)}
    if manifest.get("execution_scope") != expected:
        raise ValueError("hosted check требует cohort с тем же execution scope")
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list) or any(not isinstance(entry, dict) for entry in artifacts):
        raise ValueError("hosted cohort manifest должен содержать artifact objects")
    entries = [entry for entry in artifacts if entry.get("path") == cohort_path.name]
    content = cohort_path.read_bytes()
    if len(entries) != 1 or entries[0] != {
        "path": cohort_path.name, "size": len(content),
        "sha256": hashlib.sha256(content).hexdigest(),
    }:
        raise ValueError("hosted scope manifest не связан с exact cohort bytes")


def split_scope_regressions(
    regressions: list[dict[str, Any]], scope: TestExecutionScope
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Возвращает blocking и local-only потери, не изменяя исходный отчёт."""

    blocking: list[dict[str, Any]] = []
    local_only: list[dict[str, Any]] = []
    if not isinstance(scope, TestExecutionScope):
        raise ValueError("unknown coverage test execution scope")
    for original in regressions:
        entry = copy.deepcopy(original)
        if (
            scope is TestExecutionScope.LOCAL
            or entry["kind"] != "exact-stable-coordinate-loss"
            or entry["domain"] != "workspace"
        ):
            blocking.append(entry)
            continue
        hardware = []
        software = []
        for coordinate in entry["lost_coordinates"]:
            destination = hardware if coordinate[1] in LOCAL_HARDWARE_SOURCES else software
            destination.append(coordinate)
        if software:
            blocking.append({**entry, "lost_coordinates": software})
        if hardware:
            local_only.append({**entry, "lost_coordinates": hardware})
    return blocking, local_only

#!/usr/bin/env python3
"""Строгая versioned policy runtime-generated executable roots coverage-а."""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from coverage_coordinate_model import read_json
from coverage_runner_support import CoverageRunnerError


OWNER_PATTERN = re.compile(r"^[a-z][a-z0-9-]*$")
CARGO_TARGET_PATTERN = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_-]*$")


@dataclass(frozen=True)
class CargoTestMaterializer:
    """Typed Cargo selector, который законно создаёт один runtime build root."""

    package: str
    test: str


@dataclass(frozen=True)
class RuntimeBuildRootPolicy:
    """Один typed owner runtime-generated compiler/cache subtree."""

    owner: str
    relative_root: Path
    materializer: CargoTestMaterializer


@dataclass(frozen=True)
class ExecutableInventoryPolicy:
    """Versioned список единственных допустимых post-build executable roots."""

    schema_version: int
    runtime_build_roots: tuple[RuntimeBuildRootPolicy, ...]


def _exact_integer(value: object, expected: int) -> bool:
    """JSON bool/float не маскируются под version integer."""

    return type(value) is int and value == expected


def _parse_materializer(raw_value: object, context: str) -> CargoTestMaterializer:
    """Принимает только воспроизводимый Cargo test selector без arbitrary argv."""

    if not isinstance(raw_value, dict) or set(raw_value) != {
        "kind",
        "package",
        "test",
    }:
        raise CoverageRunnerError(f"{context}.materializer имеет неверную schema")
    if raw_value["kind"] != "cargo-test":
        raise CoverageRunnerError(
            f"{context}.materializer.kind поддерживает только cargo-test"
        )
    package = raw_value["package"]
    test = raw_value["test"]
    if not isinstance(package, str) or not CARGO_TARGET_PATTERN.fullmatch(package):
        raise CoverageRunnerError(
            f"{context}.materializer.package не является canonical Cargo package"
        )
    if not isinstance(test, str) or not CARGO_TARGET_PATTERN.fullmatch(test):
        raise CoverageRunnerError(
            f"{context}.materializer.test не является canonical Cargo test target"
        )
    return CargoTestMaterializer(package=package, test=test)


def load_executable_inventory_policy(policy_path: Path) -> ExecutableInventoryPolicy:
    """Fail-closed читает отдельную policy и нормализует ошибки JSON/I/O."""

    try:
        document = read_json(policy_path)
    except (OSError, ValueError) as error:
        raise CoverageRunnerError(
            f"не удалось прочитать executable inventory policy: {error}"
        ) from error
    if not isinstance(document, dict) or set(document) != {
        "schema_version",
        "runtime_build_roots",
    }:
        raise CoverageRunnerError("executable inventory policy имеет неверную schema")
    if not _exact_integer(document["schema_version"], 1):
        raise CoverageRunnerError(
            "executable inventory policy поддерживает только integer schema_version 1"
        )
    raw_roots = document["runtime_build_roots"]
    if not isinstance(raw_roots, list):
        raise CoverageRunnerError(
            "executable inventory policy требует array runtime_build_roots"
        )
    roots: list[RuntimeBuildRootPolicy] = []
    for index, raw_root in enumerate(raw_roots):
        context = f"runtime_build_roots[{index}]"
        if not isinstance(raw_root, dict) or set(raw_root) != {
            "owner",
            "relative_root",
            "materializer",
        }:
            raise CoverageRunnerError(f"{context} имеет неверную schema")
        owner = raw_root["owner"]
        relative_root_value = raw_root["relative_root"]
        if not isinstance(owner, str) or not OWNER_PATTERN.fullmatch(owner):
            raise CoverageRunnerError(f"{context}.owner не является canonical owner")
        if not isinstance(relative_root_value, str) or not relative_root_value:
            raise CoverageRunnerError(f"{context}.relative_root должен быть строкой")
        relative_root = Path(relative_root_value)
        if (
            relative_root.is_absolute()
            or relative_root.as_posix() != relative_root_value
            or any(part in {"", ".", ".."} for part in relative_root.parts)
            or len(relative_root.parts) < 2
            or relative_root.parts[0] != "tests"
        ):
            raise CoverageRunnerError(
                f"{context}.relative_root должен быть точным descendant `tests/`"
            )
        roots.append(
            RuntimeBuildRootPolicy(
                owner=owner,
                relative_root=relative_root,
                materializer=_parse_materializer(raw_root["materializer"], context),
            )
        )
    if len({root.owner for root in roots}) != len(roots):
        raise CoverageRunnerError("runtime build root owners должны быть уникальны")
    if len({root.relative_root for root in roots}) != len(roots):
        raise CoverageRunnerError("runtime build root paths должны быть уникальны")
    materializers = [
        (root.materializer.package, root.materializer.test) for root in roots
    ]
    if len(set(materializers)) != len(materializers):
        raise CoverageRunnerError(
            "каждый runtime build root требует уникальный typed materializer"
        )
    for root in roots:
        for other in roots:
            if root == other:
                continue
            if root.relative_root in other.relative_root.parents:
                raise CoverageRunnerError("runtime build roots не могут пересекаться")
    return ExecutableInventoryPolicy(schema_version=1, runtime_build_roots=tuple(roots))

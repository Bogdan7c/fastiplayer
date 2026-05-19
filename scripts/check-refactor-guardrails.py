#!/usr/bin/env python3
"""Проверяет прямые Cargo-зависимости на соответствие refactor guardrails."""

from __future__ import annotations

import json
import subprocess
import sys
from dataclasses import dataclass
from typing import Iterable


CONTRACT_CRATES = frozenset(
    {
        "media-core",
        "codec-core",
        "video-core",
        "render-core",
        "capability-core",
    }
)

APP_SHELL_CRATES = frozenset(
    {
        "app-egui",
        "render-wgpu",
    }
)

CONCRETE_BACKEND_CRATES = frozenset(
    {
        "webm-demux",
        "audio",
        "video-vaapi",
        "render-wgpu",
    }
)

REQUIRED_ROLE_CRATES = CONTRACT_CRATES | APP_SHELL_CRATES | CONCRETE_BACKEND_CRATES

TEMPORARY_ALLOWED_EDGES = frozenset(
    {
        ("player-core", "webm-demux"),
        ("player-core", "audio"),
        ("player-core", "video-vaapi"),
        ("player-core", "wgpu"),
        ("render-wgpu", "egui"),
        ("render-wgpu", "egui-wgpu"),
        ("render-wgpu", "winit"),
        ("render-wgpu", "video-vulkan"),
    }
)

FORBIDDEN_FOR_CONTRACT_CRATES = frozenset(
    {
        "app-egui",
        "player-core",
        "webm-demux",
        "audio",
        "video-vaapi",
        "render-wgpu",
        "video-vulkan",
        "service-youtube",
        "desktop-integration",
        "wgpu",
        "winit",
        "egui",
        "egui-winit",
        "egui-wgpu",
    }
)

PLAYER_CORE_FORBIDDEN_DIRECT_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "render-wgpu",
        "service-youtube",
        "desktop-integration",
        "webm-demux",
        "audio",
        "video-vaapi",
        "video-vulkan",
        "wgpu",
        "winit",
        "egui",
        "egui-winit",
        "egui-wgpu",
    }
)

RENDER_WGPU_FORBIDDEN_DIRECT_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "player-core",
        "service-youtube",
        "desktop-integration",
        "source-core",
        "webm-demux",
        "audio",
        "video-vaapi",
        "egui",
        "egui-wgpu",
        "winit",
        "video-vulkan",
    }
)


@dataclass(frozen=True)
class DependencyEdge:
    """Описывает одну прямую normal-dependency связь из Cargo manifest."""

    package_name: str
    dependency_name: str

    def as_pair(self) -> tuple[str, str]:
        """Возвращает пару, которую удобно сравнивать с allowlist."""

        return (self.package_name, self.dependency_name)

    def format(self) -> str:
        """Форматирует связь так же, как она записана в guardrails документе."""

        return f"{self.package_name} -> {self.dependency_name}"


def run_cargo_metadata() -> dict:
    """Получает Cargo metadata в стабильной версии JSON-формата."""

    command = ["cargo", "metadata", "--no-deps", "--format-version", "1"]

    completed_process = subprocess.run(
        command,
        capture_output=True,
        check=False,
        text=True,
    )

    if completed_process.returncode != 0:
        print("Ошибка: cargo metadata завершился неуспешно.", file=sys.stderr)
        print(completed_process.stderr, file=sys.stderr)
        sys.exit(completed_process.returncode)

    try:
        return json.loads(completed_process.stdout)
    except json.JSONDecodeError as error:
        print(f"Ошибка: cargo metadata вернул невалидный JSON: {error}", file=sys.stderr)
        sys.exit(1)


def workspace_package_names(metadata: dict) -> set[str]:
    """Возвращает имена workspace packages из Cargo metadata."""

    workspace_member_ids = set(metadata.get("workspace_members", []))
    package_names = set()

    for package in metadata.get("packages", []):
        if package.get("id") in workspace_member_ids:
            package_names.add(package["name"])

    return package_names


def normal_dependency_edges(metadata: dict) -> list[DependencyEdge]:
    """Собирает только direct normal-dependencies, не смешивая dev/build edges."""

    edges: list[DependencyEdge] = []

    for package in metadata.get("packages", []):
        package_name = package["name"]

        for dependency in package.get("dependencies", []):
            if dependency.get("kind") is not None:
                continue

            dependency_name = dependency["name"]
            edges.append(DependencyEdge(package_name, dependency_name))

    return edges


def missing_role_crates(package_names: set[str]) -> list[str]:
    """Находит role crates, которые должны быть в workspace по guardrails."""

    return sorted(REQUIRED_ROLE_CRATES - package_names)


def contract_crate_violations(edges: Iterable[DependencyEdge]) -> list[str]:
    """Проверяет, что contract crates не зависят от shell/backend/player crates."""

    violations = []

    for edge in edges:
        if edge.package_name not in CONTRACT_CRATES:
            continue

        if edge.dependency_name in FORBIDDEN_FOR_CONTRACT_CRATES:
            violations.append(
                f"{edge.format()}: contract crate не должен зависеть от shell/backend/player слоя"
            )

    return violations


def player_core_violations(edges: Iterable[DependencyEdge]) -> list[str]:
    """Проверяет новые опасные direct dependencies из player-core."""

    violations = []

    for edge in edges:
        if edge.package_name != "player-core":
            continue

        if edge.as_pair() in TEMPORARY_ALLOWED_EDGES:
            continue

        if edge.dependency_name in PLAYER_CORE_FORBIDDEN_DIRECT_DEPENDENCIES:
            violations.append(
                f"{edge.format()}: player-core может знать этот слой только через documented temporary debt"
            )

    return violations


def render_wgpu_violations(edges: Iterable[DependencyEdge]) -> list[str]:
    """Проверяет, что render-wgpu не получает новые player/demux/audio связи."""

    violations = []

    for edge in edges:
        if edge.package_name != "render-wgpu":
            continue

        if edge.as_pair() in TEMPORARY_ALLOWED_EDGES:
            continue

        if edge.dependency_name in RENDER_WGPU_FORBIDDEN_DIRECT_DEPENDENCIES:
            violations.append(
                f"{edge.format()}: render-wgpu не должен зависеть от этого соседнего слоя"
            )

    return violations


def present_temporary_edges(edges: Iterable[DependencyEdge]) -> list[str]:
    """Возвращает текущие временные нарушения, которые всё ещё есть в manifest."""

    return sorted(edge.format() for edge in edges if edge.as_pair() in TEMPORARY_ALLOWED_EDGES)


def main() -> int:
    """Запускает все guardrail checks и печатает actionable результат."""

    metadata = run_cargo_metadata()
    package_names = workspace_package_names(metadata)
    dependency_edges = normal_dependency_edges(metadata)

    violations = []

    for missing_crate in missing_role_crates(package_names):
        violations.append(f"{missing_crate}: role crate отсутствует в workspace")

    violations.extend(contract_crate_violations(dependency_edges))
    violations.extend(player_core_violations(dependency_edges))
    violations.extend(render_wgpu_violations(dependency_edges))

    if violations:
        print("Refactor guardrails: FAIL", file=sys.stderr)

        for violation in violations:
            print(f"- {violation}", file=sys.stderr)

        return 1

    print("Refactor guardrails: OK")

    temporary_edges = present_temporary_edges(dependency_edges)

    if temporary_edges:
        print("Temporary dependency debt still present:")

        for temporary_edge in temporary_edges:
            print(f"- {temporary_edge}")

    return 0


if __name__ == "__main__":
    sys.exit(main())

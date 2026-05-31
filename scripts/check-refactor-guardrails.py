#!/usr/bin/env python3
"""Проверяет архитектурные dependency guardrails для refactoring PR.

Скрипт намеренно читает только direct normal-dependencies из
`cargo metadata --no-deps --format-version 1`: именно эту границу фиксирует
`docs/rustiplayer/13-refactor-guardrails.md`.
"""

from __future__ import annotations

import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any


METADATA_COMMAND = ("cargo", "metadata", "--no-deps", "--format-version", "1")

GUARDRAILS_DOC = "docs/rustiplayer/13-refactor-guardrails.md"

CONTRACT_CRATES = frozenset(
    {
        "media-core",
        "codec-core",
        "video-core",
        "video-backend-api",
        "render-core",
        "capability-core",
    }
)

REQUIRED_ROLE_CRATES = frozenset(
    {
        "app-egui",
        "audio",
        "capability-core",
        "codec-core",
        "desktop-integration",
        "media-prefetch",
        "media-core",
        "player-core",
        "render-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "rustiplayer-config",
        "service-youtube",
        "source-core",
        "symphonia-demux",
        "video-core",
        "video-backend-api",
        "video-vaapi",
        "webm-demux",
    }
)

# Crates из этого списка были удалены из workspace и не должны возвращаться
# как "reference" backend-ы без отдельного архитектурного решения.
REMOVED_WORKSPACE_CRATES = frozenset({"video-vulkan"})

CONTRACT_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "audio",
        "ash",
        "cros-codecs",
        "cros-libva",
        "desktop-integration",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "player-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "service-youtube",
        "symphonia-demux",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
        "wgpu",
        "wgpu-types",
        "winit",
    }
)

LOW_LEVEL_CRATES = frozenset(
    {
        "audio",
        "codec-core",
        "media-core",
        "symphonia-demux",
        "webm-demux",
    }
)

LOW_LEVEL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ash",
        "render-wgpu-shell",
        "render-wgpu-video",
        "video-vulkan",
        "video-vaapi",
        "wgpu",
        "wgpu-types",
    }
)

PLAYER_CORE_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "desktop-integration",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "render-wgpu-shell",
        "render-wgpu-video",
        "service-youtube",
        "source-core",
        "symphonia-demux",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
        "wgpu",
        "wgpu-types",
        "ash",
        "winit",
    }
)

VIDEO_BACKEND_CRATES = frozenset(
    {
        "video-vaapi",
    }
)

VIDEO_BACKEND_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ash",
        "player-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "wgpu",
        "wgpu-types",
    }
)

RENDER_WGPU_SHELL_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "audio",
        "player-core",
        "service-youtube",
        "source-core",
        "symphonia-demux",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
    }
)

RENDER_WGPU_VIDEO_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "app-egui",
        "audio",
        "desktop-integration",
        "egui",
        "egui-wgpu",
        "egui-winit",
        "player-core",
        "pollster",
        "render-wgpu-shell",
        "service-youtube",
        "source-core",
        "symphonia-demux",
        "video-vaapi",
        "video-vulkan",
        "webm-demux",
        "winit",
    }
)

MEDIA_PREFETCH_CRATES = frozenset({"media-prefetch"})

MEDIA_PREFETCH_ALLOWED_DEPENDENCIES = frozenset(
    {
        "source-core",
        "thiserror",
        "tracing",
    }
)

KNOWN_DEBT_EDGES: dict[tuple[str, str], str] = {}


class GuardrailError(RuntimeError):
    """Ошибка входных данных или запуска Cargo, а не нарушение архитектурной policy."""


@dataclass(frozen=True)
class DependencyViolation:
    """Одно прямое dependency-нарушение с объяснением правила."""

    owner: str
    dependency: str
    rule: str


def repository_root() -> Path:
    """Возвращает корень репозитория относительно текущего скрипта."""

    return Path(__file__).resolve().parents[1]


def load_cargo_metadata(repo_root: Path) -> dict[str, Any]:
    """Запускает Cargo и возвращает разобранный JSON metadata."""

    completed_process = subprocess.run(
        METADATA_COMMAND,
        cwd=repo_root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed_process.returncode != 0:
        raise GuardrailError(format_failed_command(completed_process))

    try:
        metadata = json.loads(completed_process.stdout)
    except json.JSONDecodeError as error:
        raise GuardrailError(f"cargo metadata вернул невалидный JSON: {error}") from error

    if not isinstance(metadata, dict):
        raise GuardrailError("cargo metadata должен вернуть JSON object верхнего уровня")

    if metadata.get("version") != 1:
        raise GuardrailError("cargo metadata вернул format-version, отличный от ожидаемого 1")

    return metadata


def format_failed_command(completed_process: subprocess.CompletedProcess[str]) -> str:
    """Формирует диагностическое сообщение без потери stdout/stderr Cargo."""

    command_text = " ".join(METADATA_COMMAND)
    stdout_text = completed_process.stdout.strip()
    stderr_text = completed_process.stderr.strip()
    details = [f"команда `{command_text}` завершилась с кодом {completed_process.returncode}"]
    if stdout_text:
        details.append(f"stdout:\n{stdout_text}")
    if stderr_text:
        details.append(f"stderr:\n{stderr_text}")
    return "\n".join(details)


def workspace_packages(metadata: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Возвращает workspace packages по package name и проверяет целостность metadata."""

    packages = metadata.get("packages")
    workspace_members = metadata.get("workspace_members")
    if not isinstance(packages, list):
        raise GuardrailError("cargo metadata не содержит массив packages")
    if not isinstance(workspace_members, list):
        raise GuardrailError("cargo metadata не содержит массив workspace_members")

    packages_by_id = collect_packages_by_id(packages)
    package_names: dict[str, dict[str, Any]] = {}
    for package_id in workspace_members:
        if not isinstance(package_id, str):
            raise GuardrailError("workspace_members должен содержать строковые package id")
        package = packages_by_id.get(package_id)
        if package is None:
            raise GuardrailError(f"workspace member `{package_id}` отсутствует в packages")
        package_name = read_string_field(package, "name", f"package `{package_id}`")
        if package_name in package_names:
            raise GuardrailError(f"workspace содержит duplicate package name `{package_name}`")
        package_names[package_name] = package

    return package_names


def collect_packages_by_id(packages: list[Any]) -> dict[str, dict[str, Any]]:
    """Индексирует packages по Cargo package id."""

    packages_by_id: dict[str, dict[str, Any]] = {}
    for package in packages:
        if not isinstance(package, dict):
            raise GuardrailError("каждый элемент packages должен быть JSON object")
        package_id = read_string_field(package, "id", "package")
        if package_id in packages_by_id:
            raise GuardrailError(f"cargo metadata содержит duplicate package id `{package_id}`")
        packages_by_id[package_id] = package
    return packages_by_id


def read_string_field(source: dict[str, Any], field_name: str, context: str) -> str:
    """Читает обязательное строковое поле из JSON object."""

    value = source.get(field_name)
    if not isinstance(value, str):
        raise GuardrailError(f"{context} должен содержать строковое поле `{field_name}`")
    return value


def direct_normal_dependencies(packages: dict[str, dict[str, Any]]) -> dict[str, frozenset[str]]:
    """Строит map direct normal-dependencies для каждого workspace package."""

    dependency_map: dict[str, frozenset[str]] = {}
    for package_name, package in packages.items():
        dependencies = package.get("dependencies")
        if not isinstance(dependencies, list):
            raise GuardrailError(f"package `{package_name}` не содержит массив dependencies")

        normal_dependency_names = {
            read_string_field(dependency, "name", f"dependency package `{package_name}`")
            for dependency in dependencies
            if is_normal_dependency(dependency, package_name)
        }
        dependency_map[package_name] = frozenset(normal_dependency_names)

    return dependency_map


def is_normal_dependency(dependency: Any, package_name: str) -> bool:
    """Отличает normal dependency от dev/build dependency."""

    if not isinstance(dependency, dict):
        raise GuardrailError(f"dependency package `{package_name}` должен быть JSON object")
    return dependency.get("kind") is None


def find_missing_role_crates(packages: dict[str, dict[str, Any]]) -> list[str]:
    """Находит зафиксированные архитектурные роли, пропавшие из workspace."""

    return sorted(REQUIRED_ROLE_CRATES.difference(packages))


def find_reintroduced_workspace_crates(packages: dict[str, dict[str, Any]]) -> list[str]:
    """Находит удалённые workspace crates, которые нельзя вернуть молча."""

    return sorted(REMOVED_WORKSPACE_CRATES.intersection(packages))


def find_dependency_violations(
    dependency_map: dict[str, frozenset[str]],
) -> list[DependencyViolation]:
    """Проверяет dependency rules из документа guardrails."""

    violations: list[DependencyViolation] = []
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            CONTRACT_CRATES,
            CONTRACT_FORBIDDEN_DEPENDENCIES,
            "contract crates не зависят от shell/backend/player/UI crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            LOW_LEVEL_CRATES,
            LOW_LEVEL_FORBIDDEN_DEPENDENCIES,
            "media/codec/audio/demux слой не зависит от GPU/video backend crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"player-core"}),
            PLAYER_CORE_FORBIDDEN_DEPENDENCIES,
            "player-core не добавляет direct dependency на shell/service/demux/video backend/GPU crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            VIDEO_BACKEND_CRATES,
            VIDEO_BACKEND_FORBIDDEN_DEPENDENCIES,
            "concrete video backend crates используют video-backend-api и не владеют renderer/GPU import crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-wgpu-shell"}),
            RENDER_WGPU_SHELL_FORBIDDEN_DEPENDENCIES,
            "render-wgpu-shell не зависит от demux/source/audio/player/service/concrete video backend crates",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            dependency_map,
            frozenset({"render-wgpu-video"}),
            RENDER_WGPU_VIDEO_FORBIDDEN_DEPENDENCIES,
            "render-wgpu-video не зависит от shell/UI/app/player/service/concrete video backend crates",
        )
    )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            MEDIA_PREFETCH_CRATES,
            MEDIA_PREFETCH_ALLOWED_DEPENDENCIES,
            "media-prefetch зависит только от source-core плюс tracing/thiserror",
        )
    )
    return sorted(violations, key=lambda violation: (violation.owner, violation.dependency))


def find_forbidden_dependencies(
    dependency_map: dict[str, frozenset[str]],
    owner_crates: frozenset[str],
    forbidden_dependencies: frozenset[str],
    rule: str,
) -> list[DependencyViolation]:
    """Возвращает прямые зависимости, запрещённые конкретным правилом."""

    violations: list[DependencyViolation] = []
    for owner in sorted(owner_crates):
        dependencies = dependency_map.get(owner, frozenset())
        for dependency in sorted(dependencies.intersection(forbidden_dependencies)):
            violations.append(DependencyViolation(owner=owner, dependency=dependency, rule=rule))
    return violations


def find_disallowed_dependencies(
    dependency_map: dict[str, frozenset[str]],
    owner_crates: frozenset[str],
    allowed_dependencies: frozenset[str],
    rule: str,
) -> list[DependencyViolation]:
    """Возвращает прямые зависимости, которых нет в allowlist роли."""

    violations: list[DependencyViolation] = []
    for owner in sorted(owner_crates):
        dependencies = dependency_map.get(owner, frozenset())
        for dependency in sorted(dependencies.difference(allowed_dependencies)):
            violations.append(DependencyViolation(owner=owner, dependency=dependency, rule=rule))
    return violations


def find_known_debt_edges(dependency_map: dict[str, frozenset[str]]) -> list[tuple[str, str, str]]:
    """Находит текущий temporary debt, который документируется как warning."""

    known_debt_edges: list[tuple[str, str, str]] = []
    for (owner, dependency), explanation in sorted(KNOWN_DEBT_EDGES.items()):
        if dependency in dependency_map.get(owner, frozenset()):
            known_debt_edges.append((owner, dependency, explanation))
    return known_debt_edges


def print_success(known_debt_edges: list[tuple[str, str, str]]) -> None:
    """Печатает успешный результат и текущий зафиксированный долг."""

    print("Refactor guardrails: OK")
    if not known_debt_edges:
        return

    print("Temporary debt, documented and allowed for now:")
    for owner, dependency, explanation in known_debt_edges:
        print(f"  warning: {owner} -> {dependency}: {explanation}")


def print_failures(
    missing_role_crates: list[str],
    reintroduced_workspace_crates: list[str],
    violations: list[DependencyViolation],
) -> None:
    """Печатает все найденные ошибки за один запуск."""

    print("Refactor guardrails: FAILED", file=sys.stderr)
    if missing_role_crates:
        print("Missing required role crates:", file=sys.stderr)
        for crate_name in missing_role_crates:
            print(f"  - {crate_name}", file=sys.stderr)

    if reintroduced_workspace_crates:
        print("Removed workspace crates reintroduced:", file=sys.stderr)
        for crate_name in reintroduced_workspace_crates:
            print(f"  - {crate_name}", file=sys.stderr)

    if violations:
        print("Forbidden direct normal-dependencies:", file=sys.stderr)
        for violation in violations:
            print(
                f"  - {violation.owner} -> {violation.dependency}: {violation.rule}",
                file=sys.stderr,
            )

    print(f"Policy source: {GUARDRAILS_DOC}", file=sys.stderr)


def run() -> int:
    """Запускает проверку и возвращает процессный exit code."""

    repo_root = repository_root()
    metadata = load_cargo_metadata(repo_root)
    packages = workspace_packages(metadata)
    dependency_map = direct_normal_dependencies(packages)

    missing_role_crates = find_missing_role_crates(packages)
    reintroduced_workspace_crates = find_reintroduced_workspace_crates(packages)
    violations = find_dependency_violations(dependency_map)
    if missing_role_crates or reintroduced_workspace_crates or violations:
        print_failures(missing_role_crates, reintroduced_workspace_crates, violations)
        return 1

    print_success(find_known_debt_edges(dependency_map))
    return 0


def main() -> None:
    """Преобразует ожидаемые ошибки в понятный stderr и exit code."""

    try:
        exit_code = run()
    except GuardrailError as error:
        print(f"Refactor guardrails: ERROR: {error}", file=sys.stderr)
        exit_code = 2
    raise SystemExit(exit_code)


if __name__ == "__main__":
    main()

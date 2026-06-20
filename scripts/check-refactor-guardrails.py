#!/usr/bin/env python3
"""Проверяет архитектурные dependency guardrails для refactoring PR.

Скрипт намеренно проверяет direct manifest-dependencies из
`cargo metadata --no-deps --format-version 1`. Boundary rules смотрят normal
dependencies, а explicit non-goals вроде FFmpeg/libav проверяются по всем
direct dependency kinds.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any


METADATA_COMMAND = ("cargo", "metadata", "--no-deps", "--format-version", "1")

CONTRACT_CRATES = frozenset(
    {
        "audio-core",
        "media-core",
        "codec-core",
        "settings-core",
        "video-frame-contract",
        "video-core",
        "video-backend-api",
        "render-core",
        "capability-core",
    }
)

REQUIRED_ROLE_CRATES = frozenset(
    {
        "animation-core",
        "app-egui",
        "audio",
        "audio-core",
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
        "rustiplayer-settings",
        "service-direct-media",
        "service-youtube",
        "settings-core",
        "settings-derive",
        "source-core",
        "symphonia-demux",
        "video-frame-contract",
        "video-core",
        "video-backend-api",
        "video-ffmpeg",
        "video-vaapi",
    }
)

# Crates из этого списка были удалены из workspace и не должны возвращаться
# как "reference" backend-ы без отдельного архитектурного решения.
REMOVED_WORKSPACE_CRATES = frozenset({"video-vulkan"})

VIDEO_FRAME_CONTRACT_ALLOWED_DEPENDENCIES = frozenset({"serde"})

FFMPEG_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ac-ffmpeg",
        "ffmpeg",
        "ffmpeg-next",
        "ffmpeg-sys",
        "ffmpeg-sys-next",
        "ffmpeg-the-third",
        "ffmpeg4-sys",
        "ffmpeg5-sys",
        "ffmpeg6-sys",
        "ffmpeg7-sys",
        "ffmpeg8-sys",
        "libav",
        "libav-sys",
        "libavcodec",
        "libavcodec-sys",
        "libavfilter",
        "libavfilter-sys",
        "libavformat",
        "libavformat-sys",
        "libavutil",
        "libavutil-sys",
        "rsmpeg",
    }
)

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
        "ffmpeg-next",
        "ffmpeg-sys-next",
        "gbm",
        "gbm-sys",
        "player-core",
        "render-wgpu-shell",
        "render-wgpu-video",
        "rustiplayer-config",
        "rustiplayer-settings",
        "service-direct-media",
        "service-youtube",
        "settings-derive",
        "symphonia-demux",
        "video-vaapi",
        "video-ffmpeg",
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
        "video-ffmpeg",
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
        "video-ffmpeg",
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
        "video-ffmpeg",
        "video-vaapi",
    }
)

VIDEO_BACKEND_FORBIDDEN_DEPENDENCIES = frozenset(
    {
        "ash",
        "player-core",
        "render-core",
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
        "video-ffmpeg",
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
        "video-ffmpeg",
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

PUBLIC_CONFIG_SCAN_ROOTS = (
    "crates/app-egui",
    "crates/config",
    "crates/rustiplayer-settings",
    "crates/settings-core",
    "crates/settings-derive",
)

TEXT_SOURCE_SUFFIXES = frozenset(
    {
        ".rs",
        ".toml",
        ".ron",
        ".json",
        ".snap",
    }
)

RUST_SOURCE_SUFFIXES = frozenset({".rs"})

SOURCE_POLICY_SCAN_ROOTS = ("crates",)

DIRECT_FFMPEG_TYPE_ALLOWED_ROOTS = (Path("crates/video-ffmpeg"),)

DIRECT_FFMPEG_TYPE_PATTERNS = (
    (
        re.compile(
            r"\bAV(?:Frame|Packet|Codec|CodecContext|PixelFormat|Rational|Dictionary|BufferRef)\b"
        ),
        "raw FFmpeg/libav types должны оставаться внутри video-ffmpeg",
    ),
    (
        re.compile(r"\bAVERROR\b"),
        "raw FFmpeg/libav error macros должны оставаться внутри video-ffmpeg",
    ),
    (
        re.compile(r"\bffmpeg_sys_next::|\bffmpeg_next::|\brsmpeg::"),
        "raw FFmpeg Rust bindings должны использоваться только внутри video-ffmpeg",
    ),
)

CPU_RGB_CONVERSION_PATTERNS = (
    (
        re.compile(r"\bsws_scale\b|\bsws_getContext\b|\bSwsContext\b"),
        "swscale CPU conversion запрещён в playback/source tree",
    ),
    (
        re.compile(r"\blibswscale\b|\bav_image_convert\b|\bavpicture_"),
        "CPU RGB/YUV conversion helpers запрещены в playback/source tree",
    ),
)

FFMPEG_HARDWARE_DECODE_PATTERNS = (
    (
        re.compile(r"\bav_hwdevice_|\bav_hwframe_|\bAVHW(?:Device|Frames)"),
        "FFmpeg hardware decode/device API запрещён: native hardware path живёт вне FFmpeg",
    ),
    (
        re.compile(r"\bhwaccel\b|\bhw_frames\b"),
        "FFmpeg hwaccel path запрещён: video-ffmpeg остаётся software-decode-only",
    ),
)


class GuardrailError(RuntimeError):
    """Ошибка входных данных или запуска Cargo, а не нарушение архитектурной policy."""


@dataclass(frozen=True)
class DependencyViolation:
    """Одно прямое dependency-нарушение с объяснением правила."""

    owner: str
    dependency: str
    rule: str


@dataclass(frozen=True)
class SourcePolicyViolation:
    """Одно нарушение source/string policy guardrail."""

    path: Path
    line_number: int
    rule: str
    matched_text: str


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

    return direct_dependencies(packages, normal_only=True)


def direct_all_manifest_dependencies(
    packages: dict[str, dict[str, Any]],
) -> dict[str, frozenset[str]]:
    """Строит map всех direct dependencies, включая dev/build dependency."""

    return direct_dependencies(packages, normal_only=False)


def direct_dependencies(
    packages: dict[str, dict[str, Any]],
    *,
    normal_only: bool,
) -> dict[str, frozenset[str]]:
    """Строит map direct dependencies из Cargo manifest metadata."""

    dependency_map: dict[str, frozenset[str]] = {}
    for package_name, package in packages.items():
        dependencies = package.get("dependencies")
        if not isinstance(dependencies, list):
            raise GuardrailError(f"package `{package_name}` не содержит массив dependencies")

        direct_dependency_names = {
            read_string_field(dependency, "name", f"dependency package `{package_name}`")
            for dependency in dependencies
            if not normal_only or is_normal_dependency(dependency, package_name)
        }
        dependency_map[package_name] = frozenset(direct_dependency_names)

    return dependency_map


def workspace_dependency_names(repo_root: Path) -> frozenset[str]:
    """Читает root `[workspace.dependencies]`, потому что Cargo metadata показывает только package deps."""

    manifest_path = repo_root / "Cargo.toml"
    try:
        manifest = tomllib.loads(manifest_path.read_text(encoding="utf-8"))
    except tomllib.TOMLDecodeError as error:
        raise GuardrailError(f"`{manifest_path}` содержит невалидный TOML: {error}") from error
    except OSError as error:
        raise GuardrailError(f"`{manifest_path}` нельзя прочитать: {error}") from error

    workspace = manifest.get("workspace", {})
    if not isinstance(workspace, dict):
        raise GuardrailError("root Cargo.toml `[workspace]` должен быть TOML table")

    dependencies = workspace.get("dependencies", {})
    if not isinstance(dependencies, dict):
        raise GuardrailError("root Cargo.toml `[workspace.dependencies]` должен быть TOML table")

    return frozenset(dependencies)


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
    all_dependency_map: dict[str, frozenset[str]],
    workspace_dependencies: frozenset[str],
) -> list[DependencyViolation]:
    """Проверяет dependency rules из документа guardrails."""

    violations: list[DependencyViolation] = []
    for dependency in sorted(workspace_dependencies.intersection(FFMPEG_FORBIDDEN_DEPENDENCIES)):
        violations.append(
            DependencyViolation(
                owner="workspace.dependencies",
                dependency=dependency,
                rule="FFmpeg/libav crates не должны быть общими workspace dependencies",
            )
        )
    violations.extend(
        find_disallowed_dependencies(
            dependency_map,
            frozenset({"video-frame-contract"}),
            VIDEO_FRAME_CONTRACT_ALLOWED_DEPENDENCIES,
            "video-frame-contract остаётся leaf contract crate и зависит только от serde",
        )
    )
    violations.extend(
        find_forbidden_dependencies(
            all_dependency_map,
            frozenset(all_dependency_map).difference({"video-ffmpeg"}),
            FFMPEG_FORBIDDEN_DEPENDENCIES,
            "FFmpeg/libav crates разрешены только внутри video-ffmpeg",
        )
    )
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


def find_source_policy_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Проверяет source-level guardrails, которые нельзя выразить Cargo graph-ом."""

    violations: list[SourcePolicyViolation] = []
    violations.extend(find_public_video_backend_option_violations(repo_root))
    violations.extend(find_direct_ffmpeg_type_violations(repo_root))
    violations.extend(find_cpu_rgb_conversion_violations(repo_root))
    violations.extend(find_ffmpeg_hardware_decode_violations(repo_root))
    return sorted(
        violations,
        key=lambda violation: (str(violation.path), violation.line_number, violation.rule),
    )


def find_public_video_backend_option_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает public config/UI options для удалённых video decode backend-ов."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_text_files(repo_root, PUBLIC_CONFIG_SCAN_ROOTS):
        text = read_text_lossy(repo_root / relative_path)
        for line_index, line in enumerate(text.splitlines(), start=1):
            stripped_line = line.strip()
            lowered_line = stripped_line.lower()

            if "ffmpeg_sw" in lowered_line or "ffmpeg-sw" in lowered_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="ffmpeg_sw не должен появляться как public config/UI option",
                        matched_text=stripped_line,
                    )
                )

            if is_allowed_removed_vulkan_video_backend_reference(relative_path, stripped_line):
                continue

            if 'preferred_backend = "vulkan"' in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule='video.preferred_backend = "vulkan" не должен быть public config value',
                        matched_text=stripped_line,
                    )
                )

            if "VideoBackendPreference::Vulkan" in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="VideoBackendPreference не должен возвращать Vulkan video backend variant",
                        matched_text=stripped_line,
                    )
                )

            if "settings.video.preferred_backend.vulkan" in stripped_line:
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule="settings registry/UI не должен публиковать Vulkan video backend option",
                        matched_text=stripped_line,
                    )
                )

    return violations


def is_allowed_removed_vulkan_video_backend_reference(
    relative_path: Path,
    stripped_line: str,
) -> bool:
    """Оставляет только rejection diagnostics для старого удалённого значения."""

    if relative_path == Path("crates/config/src/store.rs"):
        return 'preferred_backend = "vulkan"' in stripped_line
    if relative_path != Path("crates/config/src/schema.rs"):
        return False
    return (
        'REMOVED_VULKAN_VIDEO_BACKEND_PREFERENCE: &str = "vulkan"' in stripped_line
        or 'video.preferred_backend = "vulkan" удал' in stripped_line
        or "REMOVED_VULKAN_VIDEO_BACKEND_PREFERENCE => Err" in stripped_line
    )


def find_direct_ffmpeg_type_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает raw FFmpeg identifiers за пределами `video-ffmpeg`."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_files_with_suffixes(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
    ):
        if path_is_under_any(relative_path, DIRECT_FFMPEG_TYPE_ALLOWED_ROOTS):
            continue
        violations.extend(
            find_regex_line_violations(
                repo_root,
                relative_path,
                DIRECT_FFMPEG_TYPE_PATTERNS,
            )
        )
    return violations


def find_cpu_rgb_conversion_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает FFmpeg/swscale-style CPU color conversion artifacts в source tree."""

    return find_regex_violations_in_roots(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
        CPU_RGB_CONVERSION_PATTERNS,
    )


def find_ffmpeg_hardware_decode_violations(repo_root: Path) -> list[SourcePolicyViolation]:
    """Запрещает FFmpeg hardware decode API даже внутри `video-ffmpeg`."""

    return find_regex_violations_in_roots(
        repo_root,
        SOURCE_POLICY_SCAN_ROOTS,
        RUST_SOURCE_SUFFIXES,
        FFMPEG_HARDWARE_DECODE_PATTERNS,
    )


def find_regex_violations_in_roots(
    repo_root: Path,
    relative_roots: tuple[str, ...],
    suffixes: frozenset[str],
    patterns: tuple[tuple[re.Pattern[str], str], ...],
) -> list[SourcePolicyViolation]:
    """Ищет regex guardrails в заданных roots и возвращает нарушения с line numbers."""

    violations: list[SourcePolicyViolation] = []
    for relative_path in iter_files_with_suffixes(repo_root, relative_roots, suffixes):
        violations.extend(find_regex_line_violations(repo_root, relative_path, patterns))
    return violations


def find_regex_line_violations(
    repo_root: Path,
    relative_path: Path,
    patterns: tuple[tuple[re.Pattern[str], str], ...],
) -> list[SourcePolicyViolation]:
    """Проверяет один файл набором regex policy rules."""

    violations: list[SourcePolicyViolation] = []
    text = read_text_lossy(repo_root / relative_path)
    for line_index, line in enumerate(text.splitlines(), start=1):
        stripped_line = line.strip()
        for pattern, rule in patterns:
            if pattern.search(stripped_line):
                violations.append(
                    SourcePolicyViolation(
                        path=relative_path,
                        line_number=line_index,
                        rule=rule,
                        matched_text=stripped_line,
                    )
                )
    return violations


def iter_text_files(repo_root: Path, relative_roots: tuple[str, ...]) -> list[Path]:
    """Возвращает текстовые файлы из ограниченных source roots."""

    return iter_files_with_suffixes(repo_root, relative_roots, TEXT_SOURCE_SUFFIXES)


def iter_files_with_suffixes(
    repo_root: Path,
    relative_roots: tuple[str, ...],
    suffixes: frozenset[str],
) -> list[Path]:
    """Возвращает файлы с нужными suffix-ами из ограниченных source roots."""

    text_files: list[Path] = []
    for relative_root in relative_roots:
        root = repo_root / relative_root
        if not root.exists():
            raise GuardrailError(f"source root `{relative_root}` отсутствует")
        for path in root.rglob("*"):
            if path.is_file() and path.suffix in suffixes:
                text_files.append(path.relative_to(repo_root))
    return sorted(text_files)


def path_is_under_any(relative_path: Path, allowed_roots: tuple[Path, ...]) -> bool:
    """Проверяет, находится ли относительный путь внутри одного из разрешённых roots."""

    return any(
        relative_path == allowed_root or allowed_root in relative_path.parents
        for allowed_root in allowed_roots
    )


def read_text_lossy(path: Path) -> str:
    """Читает UTF-8 source file; ошибки кодировки считаются нарушением guardrail input."""

    try:
        return path.read_text(encoding="utf-8")
    except UnicodeDecodeError as error:
        raise GuardrailError(f"`{path}` не является UTF-8 текстом: {error}") from error


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
    source_policy_violations: list[SourcePolicyViolation],
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
        print("Forbidden direct manifest dependencies:", file=sys.stderr)
        for violation in violations:
            print(
                f"  - {violation.owner} -> {violation.dependency}: {violation.rule}",
                file=sys.stderr,
            )

    if source_policy_violations:
        print("Forbidden source/config policy matches:", file=sys.stderr)
        for violation in source_policy_violations:
            line_suffix = f":{violation.line_number}" if violation.line_number else ""
            print(
                f"  - {violation.path}{line_suffix}: {violation.rule}: "
                f"{violation.matched_text}",
                file=sys.stderr,
            )


def run() -> int:
    """Запускает проверку и возвращает процессный exit code."""

    repo_root = repository_root()
    metadata = load_cargo_metadata(repo_root)
    packages = workspace_packages(metadata)
    dependency_map = direct_normal_dependencies(packages)
    all_dependency_map = direct_all_manifest_dependencies(packages)
    workspace_dependencies = workspace_dependency_names(repo_root)

    missing_role_crates = find_missing_role_crates(packages)
    reintroduced_workspace_crates = find_reintroduced_workspace_crates(packages)
    violations = find_dependency_violations(
        dependency_map,
        all_dependency_map,
        workspace_dependencies,
    )
    source_policy_violations = find_source_policy_violations(repo_root)
    if (
        missing_role_crates
        or reintroduced_workspace_crates
        or violations
        or source_policy_violations
    ):
        print_failures(
            missing_role_crates,
            reintroduced_workspace_crates,
            violations,
            source_policy_violations,
        )
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

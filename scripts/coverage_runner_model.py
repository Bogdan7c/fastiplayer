#!/usr/bin/env python3
"""Чистые value objects stable coverage runner-а без subprocess/filesystem логики."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class RunnerConfig:
    """Все пути и exact tool identities одного воспроизводимого cohort."""

    repo_root: Path
    profile_directory: Path
    artifact_directory: Path
    policy_path: Path
    executable_inventory_policy_path: Path
    coordinate_extractor: Path
    stability_tool: Path
    lcov_validator: Path
    toolchain: str
    cargo_llvm_cov_version: str
    llvm_cov_version: str
    session_id: str
    cargo_command: str
    rustc_command: str
    python_command: str


@dataclass(frozen=True)
class ToolIdentity:
    """Версии compiler, LLVM и wrapper, которыми построен весь cohort."""

    rustc_release: str
    llvm_release: str
    cargo_llvm_cov_release: str

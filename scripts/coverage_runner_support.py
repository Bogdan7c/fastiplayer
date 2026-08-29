#!/usr/bin/env python3
"""Filesystem/hash primitives stable coverage runner-а без command semantics."""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import stat
import subprocess
import tempfile
from pathlib import Path


class CoverageRunnerError(RuntimeError):
    """Ошибка, при которой нельзя публиковать частичный coverage cohort."""


def sha256_file(path: Path) -> str:
    """Считает content identity файла потоково, без пропорционального расхода RAM."""

    digest = hashlib.sha256()
    with path.open("rb") as source_file:
        for chunk in iter(lambda: source_file.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def canonical_json_bytes(value: object) -> bytes:
    """Даёт стабильное представление для manifests и их SHA-256."""

    return (json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n").encode(
        "utf-8"
    )


def atomic_write_json(path: Path, value: object) -> None:
    """Не оставляет правдоподобный, но усечённый JSON после ошибки записи."""

    path.parent.mkdir(parents=True, exist_ok=True)
    file_descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{path.name}.", suffix=".tmp", dir=path.parent
    )
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(file_descriptor, "wb") as temporary_file:
            temporary_file.write(canonical_json_bytes(value))
            temporary_file.flush()
            os.fsync(temporary_file.fileno())
        os.replace(temporary_path, path)
    finally:
        temporary_path.unlink(missing_ok=True)


def git_source_manifest(repo_root: Path) -> dict[str, object]:
    """Хеширует tracked и видимые untracked inputs, исключая ignored target artifacts."""

    try:
        result = subprocess.run(
            [
                "git",
                "-C",
                str(repo_root),
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except subprocess.CalledProcessError as error:
        detail = error.stderr.decode("utf-8", errors="replace").strip()
        raise CoverageRunnerError(f"не удалось получить source inventory: {detail}") from error

    entries: list[dict[str, object]] = []
    for encoded_path in sorted(filter(None, result.stdout.split(b"\0"))):
        relative_path = Path(os.fsdecode(encoded_path))
        source_path = repo_root / relative_path
        if not source_path.is_file():
            raise CoverageRunnerError(
                f"source inventory изменился во время чтения: {relative_path.as_posix()}"
            )
        source_stat = source_path.stat()
        entries.append(
            {
                "path": relative_path.as_posix(),
                "mode": stat.S_IMODE(source_stat.st_mode),
                "size": source_stat.st_size,
                "sha256": sha256_file(source_path),
            }
        )
    if not entries:
        raise CoverageRunnerError("source inventory пуст; runner требует git worktree")
    return {
        "file_count": len(entries),
        "sha256": hashlib.sha256(canonical_json_bytes(entries)).hexdigest(),
    }


def executable_manifest(profile_directory: Path) -> dict[str, object]:
    """Фиксирует instrumented executables без повторного хеширования гигабайтов binaries."""

    entries: list[dict[str, object]] = []
    if profile_directory.exists():
        for candidate in sorted(profile_directory.rglob("*")):
            if not candidate.is_file() or not os.access(candidate, os.X_OK):
                continue
            candidate_stat = candidate.stat()
            entries.append(
                {
                    "path": candidate.relative_to(profile_directory).as_posix(),
                    "size": candidate_stat.st_size,
                    "mtime_ns": candidate_stat.st_mtime_ns,
                }
            )
    if not entries:
        raise CoverageRunnerError("build-once не создал ни одного instrumented executable")
    return {
        "file_count": len(entries),
        "sha256": hashlib.sha256(canonical_json_bytes(entries)).hexdigest(),
    }


def assert_unchanged(label: str, expected: object, actual: object) -> None:
    """Останавливает cohort, если между runs изменился его инструмент или input."""

    if actual != expected:
        raise CoverageRunnerError(f"{label} изменился внутри coverage cohort")


def atomic_artifact_stage(final_directory: Path, session_id: str) -> Path:
    """Создаёт sibling stage, который не виден потребителям до публикации."""

    final_directory.parent.mkdir(parents=True, exist_ok=True)
    stage = final_directory.parent / f".{final_directory.name}.stage-{session_id}"
    if stage.exists():
        raise CoverageRunnerError(f"stale runner stage требует ручного аудита: {stage}")
    stage.mkdir()
    return stage


def publish_artifacts(stage: Path, final_directory: Path, session_id: str) -> None:
    """Меняет целое дерево с rollback; partial stage никогда не становится current."""

    backup = final_directory.parent / f".{final_directory.name}.backup-{session_id}"
    if backup.exists():
        raise CoverageRunnerError(f"stale runner backup требует ручного аудита: {backup}")
    moved_previous = False
    try:
        if final_directory.exists():
            os.replace(final_directory, backup)
            moved_previous = True
        os.replace(stage, final_directory)
    except BaseException:
        if moved_previous and not final_directory.exists() and backup.exists():
            os.replace(backup, final_directory)
        raise
    if backup.exists():
        shutil.rmtree(backup)

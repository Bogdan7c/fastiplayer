#!/usr/bin/env python3
"""Границы immutable и runtime-owned executable inventories coverage cohort-а."""

from __future__ import annotations

import hashlib
import os
import stat
import tempfile
from pathlib import Path

from coverage_executable_policy import (
    ExecutableInventoryPolicy,
    RuntimeBuildRootPolicy,
    load_executable_inventory_policy,
)
from coverage_runner_support import (
    CoverageRunnerError,
    canonical_json_bytes,
)


RUN_COUNT = 3


def _canonical_profile_directory(profile_directory: Path) -> Path:
    """Запрещает symlink profile root до любого рекурсивного обхода."""

    requested_directory = profile_directory.absolute()
    resolved_directory = requested_directory.resolve()
    if requested_directory != resolved_directory:
        raise CoverageRunnerError("profile directory для executable inventory содержит symlink")
    return resolved_directory


def _runtime_root(
    profile_directory: Path,
    root_policy: RuntimeBuildRootPolicy,
) -> Path:
    """Возвращает exact plain configured root без broad glob-классификации."""

    inspected_path = _canonical_profile_directory(profile_directory)
    for component in root_policy.relative_root.parts:
        inspected_path /= component
        if inspected_path.is_symlink():
            raise CoverageRunnerError(
                f"runtime root `{root_policy.owner}` не может содержать symlink"
            )
    return inspected_path


def _is_runtime_owned(
    relative_path: Path,
    policy: ExecutableInventoryPolicy,
) -> bool:
    """Классифицирует только exact versioned runtime-owned subtrees."""

    return any(
        relative_path == root.relative_root
        or root.relative_root in relative_path.parents
        for root in policy.runtime_build_roots
    )


def _manifest(entries: list[dict[str, object]], identity: str) -> dict[str, object]:
    """Формирует versioned deterministic manifest из уже отсортированных entries."""

    return {
        "schema_version": 1,
        "identity": identity,
        "file_count": len(entries),
        "sha256": hashlib.sha256(canonical_json_bytes(entries)).hexdigest(),
        "entries": entries,
    }


def _opened_file_identity(file_stat: os.stat_result) -> tuple[int, ...]:
    """Даёт verification triggers; переносимый manifest их не публикует."""

    return (
        file_stat.st_dev,
        file_stat.st_ino,
        stat.S_IMODE(file_stat.st_mode),
        file_stat.st_size,
        file_stat.st_nlink,
        file_stat.st_ctime_ns,
        file_stat.st_mtime_ns,
    )


def _semantic_sha256(
    candidate: Path,
    candidate_lstat: os.stat_result,
    digest_by_identity: dict[tuple[int, ...], str],
) -> str:
    """Хеширует regular file и дедуплирует hardlinks только внутри snapshot-а."""

    open_flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        file_descriptor = os.open(candidate, open_flags)
    except OSError as error:
        raise CoverageRunnerError(
            f"не удалось безопасно открыть executable {candidate}: {error}"
        ) from error
    try:
        opened_stat = os.fstat(file_descriptor)
        expected_identity = _opened_file_identity(candidate_lstat)
        if (
            not stat.S_ISREG(opened_stat.st_mode)
            or _opened_file_identity(opened_stat) != expected_identity
        ):
            raise CoverageRunnerError(
                f"executable изменился до открытия: {candidate}"
            )
        cached_digest = digest_by_identity.get(expected_identity)
        if cached_digest is None:
            digest = hashlib.sha256()
            while chunk := os.read(file_descriptor, 1024 * 1024):
                digest.update(chunk)
            cached_digest = digest.hexdigest()
            final_stat = os.fstat(file_descriptor)
            if _opened_file_identity(final_stat) != expected_identity:
                raise CoverageRunnerError(
                    f"executable изменился во время хеширования: {candidate}"
                )
            digest_by_identity[expected_identity] = cached_digest
        return cached_digest
    finally:
        os.close(file_descriptor)


def _scan_prebuilt_executables(
    profile_directory: Path,
    policy: ExecutableInventoryPolicy,
) -> dict[str, tuple[Path, os.stat_result]]:
    """Полностью сканирует path/type/mode-set вне exact runtime-owned root."""

    canonical_profile = _canonical_profile_directory(profile_directory)
    if not canonical_profile.is_dir():
        raise CoverageRunnerError("build-once не создал profile directory")
    # Exact root валидируется отдельно, поэтому symlink нельзя спрятать исключением.
    for root_policy in policy.runtime_build_roots:
        _runtime_root(canonical_profile, root_policy)
    executable_files: dict[str, tuple[Path, os.stat_result]] = {}
    try:
        for candidate in sorted(canonical_profile.rglob("*")):
            relative_path = candidate.relative_to(canonical_profile)
            if _is_runtime_owned(relative_path, policy):
                continue
            candidate_lstat = candidate.lstat()
            if candidate.is_symlink():
                raise CoverageRunnerError(
                    "prebuilt executable tree содержит symlink: "
                    f"{relative_path.as_posix()}"
                )
            if not stat.S_ISREG(candidate_lstat.st_mode):
                continue
            if stat.S_IMODE(candidate_lstat.st_mode) & 0o111 == 0:
                continue
            executable_files[relative_path.as_posix()] = (
                candidate,
                candidate_lstat,
            )
    except OSError as error:
        raise CoverageRunnerError(
            f"не удалось прочитать prebuilt executable tree: {error}"
        ) from error
    if not executable_files:
        raise CoverageRunnerError("build-once не создал ни одного instrumented executable")
    return executable_files


def _probe_ctime_capability(profile_directory: Path) -> bool:
    """Доказывает kernel trigger даже для same-size write с restored mtime."""

    file_descriptor, probe_name = tempfile.mkstemp(
        prefix=".stable-coverage-ctime-probe-",
        dir=profile_directory,
    )
    probe_path = Path(probe_name)
    try:
        os.fchmod(file_descriptor, 0o700)
        os.write(file_descriptor, b"A")
        os.fsync(file_descriptor)
        initial_stat = os.fstat(file_descriptor)
        os.lseek(file_descriptor, 0, os.SEEK_SET)
        os.write(file_descriptor, b"B")
        os.fsync(file_descriptor)
        written_stat = os.fstat(file_descriptor)
        os.utime(
            probe_path,
            ns=(initial_stat.st_atime_ns, initial_stat.st_mtime_ns),
            follow_symlinks=False,
        )
        final_stat = os.fstat(file_descriptor)
        write_changed_observable_metadata = (
            written_stat.st_ctime_ns != initial_stat.st_ctime_ns
            or written_stat.st_mtime_ns != initial_stat.st_mtime_ns
        )
        restored_mtime_is_exact = final_stat.st_mtime_ns == initial_stat.st_mtime_ns
        restore_advanced_ctime = final_stat.st_ctime_ns != initial_stat.st_ctime_ns
        return (
            write_changed_observable_metadata
            and restored_mtime_is_exact
            and restore_advanced_ctime
        )
    except OSError:
        # Недоказанная capability включает безопасный full-rehash fallback.
        return False
    finally:
        os.close(file_descriptor)
        probe_path.unlink(missing_ok=True)


def _semantic_entry(
    relative_path: str,
    candidate: Path,
    candidate_lstat: os.stat_result,
    digest_by_identity: dict[tuple[int, ...], str],
) -> dict[str, object]:
    """Проецирует volatile filesystem identity в deterministic content semantics."""

    return {
        "path": relative_path,
        "mode": stat.S_IMODE(candidate_lstat.st_mode),
        "size": candidate_lstat.st_size,
        "sha256": _semantic_sha256(
            candidate,
            candidate_lstat,
            digest_by_identity,
        ),
    }


class PrebuiltExecutableReference:
    """Защищает parent cohort binaries в unprivileged Linux threat model.

    Initial snapshot хеширует content с hardlink-dedup. Каждый последующий check
    полностью перечитывает path/type/mode/size. На доказанном ctime filesystem SHA
    пересчитывается при dev/inode/nlink/ctime/mtime drift; без такой capability
    безопасный fallback пересчитывает все content hashes. `mtime` — trigger, не truth.
    """

    def __init__(
        self,
        profile_directory: Path,
        policy: ExecutableInventoryPolicy,
    ):
        self.profile_directory = _canonical_profile_directory(profile_directory)
        self.policy = policy
        self.ctime_capable = _probe_ctime_capability(self.profile_directory)
        executable_files = _scan_prebuilt_executables(
            self.profile_directory,
            self.policy,
        )
        digest_by_identity: dict[tuple[int, ...], str] = {}
        self.semantic_entries: dict[str, dict[str, object]] = {}
        self.observed_identities: dict[str, tuple[int, ...]] = {}
        for relative_path, (candidate, candidate_lstat) in executable_files.items():
            self.semantic_entries[relative_path] = _semantic_entry(
                relative_path,
                candidate,
                candidate_lstat,
                digest_by_identity,
            )
            self.observed_identities[relative_path] = _opened_file_identity(
                candidate_lstat
            )

    def manifest(self) -> dict[str, object]:
        """Публикует переносимую semantics без inode/ctime/mtime."""

        return _manifest(
            [self.semantic_entries[path] for path in sorted(self.semantic_entries)],
            "path-mode-size-sha256-v1",
        )

    def assert_unchanged(self, label: str = "instrumented build") -> None:
        """Проверяет exact set и selective/full content identity после execution."""

        executable_files = _scan_prebuilt_executables(
            self.profile_directory,
            self.policy,
        )
        expected_paths = set(self.semantic_entries)
        actual_paths = set(executable_files)
        if actual_paths != expected_paths:
            actual_entries = [
                dict(self.semantic_entries[path])
                if path in self.semantic_entries
                else {
                    "path": path,
                    "mode": stat.S_IMODE(candidate_lstat.st_mode),
                    "size": candidate_lstat.st_size,
                    "sha256": "not-hashed-unexpected-path",
                }
                for path, (_candidate, candidate_lstat) in executable_files.items()
            ]
            assert_executable_manifest_unchanged(
                label,
                self.manifest(),
                _manifest(actual_entries, "path-mode-size-sha256-v1"),
            )

        digest_by_identity: dict[tuple[int, ...], str] = {}
        actual_semantics: dict[str, dict[str, object]] = {}
        for relative_path in sorted(actual_paths):
            candidate, candidate_lstat = executable_files[relative_path]
            current_identity = _opened_file_identity(candidate_lstat)
            expected_semantics = self.semantic_entries[relative_path]
            must_rehash = (
                not self.ctime_capable
                or current_identity != self.observed_identities[relative_path]
            )
            if must_rehash:
                actual_semantics[relative_path] = _semantic_entry(
                    relative_path,
                    candidate,
                    candidate_lstat,
                    digest_by_identity,
                )
            else:
                actual_semantics[relative_path] = dict(expected_semantics)

        actual_manifest = _manifest(
            [actual_semantics[path] for path in sorted(actual_semantics)],
            "path-mode-size-sha256-v1",
        )
        assert_executable_manifest_unchanged(label, self.manifest(), actual_manifest)
        # Новые triggers принимаются только после совпадения content semantics.
        self.observed_identities = {
            relative_path: _opened_file_identity(candidate_lstat)
            for relative_path, (_candidate, candidate_lstat) in executable_files.items()
        }


def runtime_executable_manifest(
    profile_directory: Path,
    root_policy: RuntimeBuildRootPolicy,
) -> dict[str, object]:
    """Хеширует runtime-owned executables без inode/mtime в semantics."""

    canonical_profile = _canonical_profile_directory(profile_directory)
    runtime_root = _runtime_root(canonical_profile, root_policy)
    if not runtime_root.exists():
        return _manifest([], "path-mode-size-sha256-v1")
    if not runtime_root.is_dir():
        raise CoverageRunnerError(
            f"runtime root `{root_policy.owner}` не является каталогом"
        )
    entries: list[dict[str, object]] = []
    digest_by_identity: dict[tuple[int, ...], str] = {}
    try:
        for candidate in sorted(runtime_root.rglob("*")):
            relative_path = candidate.relative_to(canonical_profile)
            candidate_lstat = candidate.lstat()
            if candidate.is_symlink():
                raise CoverageRunnerError(
                    f"runtime tree `{root_policy.owner}` содержит symlink: "
                    f"{relative_path.as_posix()}"
                )
            if not stat.S_ISREG(candidate_lstat.st_mode):
                continue
            executable_mode = stat.S_IMODE(candidate_lstat.st_mode)
            if executable_mode & 0o111 == 0:
                continue
            entries.append(
                {
                    "path": relative_path.as_posix(),
                    "mode": executable_mode,
                    "size": candidate_lstat.st_size,
                    "sha256": _semantic_sha256(
                        candidate,
                        candidate_lstat,
                        digest_by_identity,
                    ),
                }
            )
    except OSError as error:
        raise CoverageRunnerError(
            f"не удалось прочитать runtime tree `{root_policy.owner}`: {error}"
        ) from error
    return _manifest(entries, "path-mode-size-sha256-v1")


def assert_executable_manifest_unchanged(
    label: str,
    expected: dict[str, object],
    actual: dict[str, object],
) -> None:
    """Даёт exact bounded path diagnostics вместо одного aggregate mismatch."""

    if actual == expected:
        return
    expected_entries = {
        str(entry["path"]): entry for entry in expected.get("entries", [])
    }
    actual_entries = {
        str(entry["path"]): entry for entry in actual.get("entries", [])
    }
    added = sorted(actual_entries.keys() - expected_entries.keys())
    removed = sorted(expected_entries.keys() - actual_entries.keys())
    changed = sorted(
        path
        for path in expected_entries.keys() & actual_entries.keys()
        if expected_entries[path] != actual_entries[path]
    )

    def bounded(paths: list[str]) -> str:
        visible = paths[:8]
        suffix = f", +{len(paths) - len(visible)} more" if len(paths) > len(visible) else ""
        return "[" + ", ".join(visible) + suffix + "]"

    raise CoverageRunnerError(
        f"{label} изменился внутри coverage cohort: "
        f"added={bounded(added)}, removed={bounded(removed)}, "
        f"changed={bounded(changed)}"
    )


class RuntimeExecutableReference:
    """Владеет typed prewarm reference и exact three-run semantic freeze."""

    def __init__(
        self,
        profile_directory: Path,
        root_policy: RuntimeBuildRootPolicy,
    ):
        self.profile_directory = profile_directory
        self.root_policy = root_policy
        self.reference_manifest: dict[str, object] | None = None
        self.next_run_number = 1

    def freeze_after_materialization(self) -> None:
        """Принимает только непустой результат уже выполненного typed prewarm."""

        if self.reference_manifest is not None or self.next_run_number != 1:
            raise CoverageRunnerError("runtime executable reference уже установлен")
        materialized_manifest = runtime_executable_manifest(
            self.profile_directory,
            self.root_policy,
        )
        if materialized_manifest["file_count"] == 0:
            raise CoverageRunnerError(
                f"typed materializer `{self.root_policy.owner}` не создал executables"
            )
        self.reference_manifest = materialized_manifest

    def assert_ready_before_run(self, run_number: int) -> None:
        """Проверяет sequence и отсутствие mutation между report и следующим run."""

        if run_number != self.next_run_number:
            raise CoverageRunnerError(
                f"runtime executable sequence ожидал run-{self.next_run_number}, "
                f"получил run-{run_number}"
            )
        if self.reference_manifest is None:
            raise CoverageRunnerError("typed prewarm не установил runtime executable reference")
        assert_executable_manifest_unchanged(
            "runtime-owned executables до execution",
            self.reference_manifest,
            runtime_executable_manifest(self.profile_directory, self.root_policy),
        )

    def observe_completed_run(self, run_number: int) -> None:
        """Проверяет semantic equality после каждого измеряемого execution."""

        if run_number != self.next_run_number:
            raise CoverageRunnerError(
                f"runtime executable completion ожидал run-{self.next_run_number}, "
                f"получил run-{run_number}"
            )
        actual_manifest = runtime_executable_manifest(
            self.profile_directory,
            self.root_policy,
        )
        if self.reference_manifest is None:
            raise CoverageRunnerError("typed prewarm runtime executable reference отсутствует")
        assert_executable_manifest_unchanged(
            "runtime-owned executables после execution",
            self.reference_manifest,
            actual_manifest,
        )
        self.next_run_number += 1

    def assert_final(self) -> None:
        """Повторяет semantic check непосредственно перед publication."""

        if self.next_run_number != RUN_COUNT + 1 or self.reference_manifest is None:
            raise CoverageRunnerError("runtime executable reference не завершил три runs")
        assert_executable_manifest_unchanged(
            "runtime-owned executables перед publication",
            self.reference_manifest,
            runtime_executable_manifest(self.profile_directory, self.root_policy),
        )

    def manifest(self) -> dict[str, object]:
        """Возвращает принятую typed-prewarm identity для cohort provenance."""

        if self.reference_manifest is None:
            raise CoverageRunnerError("runtime executable reference ещё не установлен")
        return self.reference_manifest

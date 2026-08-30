#!/usr/bin/env python3
"""Process-transaction quarantine generated runtime roots вокруг coverage cohort-а."""

from __future__ import annotations

import os
import shutil
import stat
from pathlib import Path

from coverage_executable_inventory import _canonical_profile_directory, _runtime_root
from coverage_executable_policy import ExecutableInventoryPolicy
from coverage_runner_support import CoverageRunnerError, atomic_write_json


def _exists_without_following(path: Path) -> bool:
    """Различает отсутствующий path и dangling symlink без resolve."""

    try:
        path.lstat()
    except FileNotFoundError:
        return False
    return True


def _validate_plain_tree(root: Path, label: str) -> None:
    """До quarantine запрещает symlink и non-directory exact root."""

    if not _exists_without_following(root):
        return
    root_stat = root.lstat()
    if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
        raise CoverageRunnerError(f"{label} не является plain directory")
    try:
        for candidate in root.rglob("*"):
            if stat.S_ISLNK(candidate.lstat().st_mode):
                raise CoverageRunnerError(f"{label} содержит symlink: {candidate}")
    except OSError as error:
        raise CoverageRunnerError(f"не удалось проверить {label}: {error}") from error


def _remove_generated_tree(root: Path, label: str) -> None:
    """Удаляет exact generated root без следования по встреченным symlink."""

    if not _exists_without_following(root):
        return
    try:
        root_stat = root.lstat()
        if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
            root.unlink()
            return
        if not shutil.rmtree.avoids_symlink_attacks:
            raise CoverageRunnerError(
                f"платформа не поддерживает symlink-safe cleanup для {label}"
            )
        shutil.rmtree(root)
    except OSError as error:
        raise CoverageRunnerError(f"не удалось удалить {label}: {error}") from error


def _paths_overlap(first: Path, second: Path) -> bool:
    """Запрещает equality и ancestor collision canonical paths."""

    return first == second or first in second.parents or second in first.parents


def _move_path(source: Path, destination: Path) -> None:
    """Единственная injectable atomic-rename boundary transaction-а."""

    os.replace(source, destination)


def _create_quarantine_directory(path: Path) -> None:
    """Injectable boundary для обработки mkdir failure внутри transaction."""

    path.mkdir(mode=0o700)


class RuntimeRootTransaction:
    """All-or-nothing сохраняет stale generated roots до publication.

    Гарантия распространяется на обработанные исключения текущего процесса. Без
    fsync directory protocol power/process crash не угадывается: journaled orphan
    блокирует следующий запуск до явного cleanup, а не подменяется новым cache.
    """

    def __init__(
        self,
        profile_directory: Path,
        artifact_directory: Path,
        policy: ExecutableInventoryPolicy,
    ):
        self.profile_directory = _canonical_profile_directory(profile_directory)
        self.policy = policy
        self.quarantine_directory = (
            self.profile_directory.parent
            / f".{self.profile_directory.name}.stable-runtime-quarantine"
        )
        self.retired_directory = (
            self.profile_directory.parent
            / f".{self.profile_directory.name}.stable-runtime-retired"
        )
        canonical_artifact = artifact_directory.absolute().resolve()
        for generated_directory in (
            self.quarantine_directory,
            self.retired_directory,
        ):
            if _paths_overlap(generated_directory, canonical_artifact):
                raise CoverageRunnerError(
                    "runtime quarantine/retired directory пересекается с artifact root"
                )
        self.journal_path = self.quarantine_directory / "journal.json"
        self.root_records: list[dict[str, object]] = []
        self.active = False
        self.prepared_for_publication = False
        self.retired_for_publication = False

    def _quarantined_root(self, owner: str) -> Path:
        """Возвращает exact policy-owned quarantine path."""

        return self.quarantine_directory / "roots" / owner

    def _prepare_startup_paths(self) -> None:
        """Чистит retired post-publication tree и fail-closed ловит crash orphan."""

        if _exists_without_following(self.retired_directory):
            _remove_generated_tree(self.retired_directory, "retired runtime quarantine")
        if not _exists_without_following(self.quarantine_directory):
            return
        _validate_plain_tree(self.quarantine_directory, "runtime quarantine")
        if any(self.quarantine_directory.iterdir()):
            raise CoverageRunnerError(
                "обнаружен journaled/orphaned runtime quarantine после crash; "
                "автоматическое восстановление без directory-fsync запрещено"
            )
        self.quarantine_directory.rmdir()

    def _describe_roots(self) -> list[dict[str, object]]:
        """Фиксирует owner identity и original presence до первого move."""

        records = []
        for root_policy in self.policy.runtime_build_roots:
            runtime_root = _runtime_root(self.profile_directory, root_policy)
            _validate_plain_tree(runtime_root, f"runtime root `{root_policy.owner}`")
            records.append(
                {
                    "owner": root_policy.owner,
                    "relative_root": root_policy.relative_root.as_posix(),
                    "had_original": _exists_without_following(runtime_root),
                }
            )
        return records

    def _write_journal(self, state: str) -> None:
        """Пишет audit journal; он не объявляется crash-durable recovery log."""

        atomic_write_json(
            self.journal_path,
            {
                "schema_version": 1,
                "profile_directory": str(self.profile_directory),
                "state": state,
                "roots": self.root_records,
            },
        )

    def _restore_in_process(self) -> None:
        """По in-memory moved set восстанавливает exact pre-run layout."""

        if _exists_without_following(self.retired_directory):
            if _exists_without_following(self.quarantine_directory):
                raise CoverageRunnerError(
                    "runtime rollback обнаружил одновременно live и retired quarantine"
                )
            _move_path(self.retired_directory, self.quarantine_directory)
            self.retired_for_publication = False
        rollback_errors = []
        records_by_owner = {str(record["owner"]): record for record in self.root_records}
        for root_policy in reversed(self.policy.runtime_build_roots):
            runtime_root = _runtime_root(self.profile_directory, root_policy)
            quarantined_root = self._quarantined_root(root_policy.owner)
            try:
                had_original = bool(records_by_owner[root_policy.owner]["had_original"])
                if _exists_without_following(quarantined_root):
                    if not had_original:
                        raise CoverageRunnerError(
                            f"quarantine содержит неожиданный root `{root_policy.owner}`"
                        )
                    _remove_generated_tree(
                        runtime_root,
                        f"regenerated runtime root `{root_policy.owner}`",
                    )
                    runtime_root.parent.mkdir(parents=True, exist_ok=True)
                    _move_path(quarantined_root, runtime_root)
                elif had_original and not _exists_without_following(runtime_root):
                    raise CoverageRunnerError(
                        f"quarantine потерял original root `{root_policy.owner}`"
                    )
                elif not had_original:
                    _remove_generated_tree(
                        runtime_root,
                        f"regenerated runtime root `{root_policy.owner}`",
                    )
            except BaseException as error:
                rollback_errors.append(f"{root_policy.owner}: {error}")
        if rollback_errors:
            raise CoverageRunnerError(
                "runtime root rollback failed: " + "; ".join(rollback_errors)
            )
        roots_directory = self.quarantine_directory / "roots"
        if _exists_without_following(roots_directory) and any(roots_directory.iterdir()):
            raise CoverageRunnerError(
                "runtime root rollback оставил unknown quarantine owner"
            )
        _remove_generated_tree(self.quarantine_directory, "runtime quarantine")
        self.active = False
        self.prepared_for_publication = False

    def begin(self) -> None:
        """Карантинит все current roots; partial move немедленно откатывается."""

        if self.active:
            raise CoverageRunnerError("runtime root transaction уже активна")
        self.profile_directory.parent.mkdir(parents=True, exist_ok=True)
        self._prepare_startup_paths()
        self.root_records = self._describe_roots()
        try:
            _create_quarantine_directory(self.quarantine_directory)
            self._write_journal("preparing")
            for root_policy, root_record in zip(
                self.policy.runtime_build_roots,
                self.root_records,
                strict=True,
            ):
                if not root_record["had_original"]:
                    continue
                runtime_root = _runtime_root(self.profile_directory, root_policy)
                quarantined_root = self._quarantined_root(root_policy.owner)
                quarantined_root.parent.mkdir(parents=True, exist_ok=True)
                _move_path(runtime_root, quarantined_root)
            self._write_journal("quarantined")
            self.active = True
            self.prepared_for_publication = False
            self.retired_for_publication = False
        except BaseException as error:
            try:
                self._restore_in_process()
            except BaseException as rollback_error:
                raise CoverageRunnerError(
                    f"runtime quarantine begin завершился ошибкой ({error}); "
                    f"rollback тоже завершился ошибкой: {rollback_error}"
                ) from error
            raise

    def rollback(self) -> None:
        """Удаляет regenerated roots и возвращает exact pre-run cache layout."""

        if not self.active:
            return
        self._restore_in_process()

    def prepare_publication(self) -> None:
        """До artifact swap проверяет, что atomic retirement можно начинать."""

        if not self.active:
            raise CoverageRunnerError("runtime root transaction не активна")
        if _exists_without_following(self.retired_directory):
            _remove_generated_tree(self.retired_directory, "retired runtime quarantine")
        _validate_plain_tree(self.quarantine_directory, "runtime quarantine")
        if not self.journal_path.is_file() or self.journal_path.is_symlink():
            raise CoverageRunnerError("runtime quarantine journal отсутствует перед publication")
        self.prepared_for_publication = True

    def complete_publication(self) -> None:
        """Atomic retire выполняется внутри rollback-capable artifact finalize."""

        if not self.active or not self.prepared_for_publication:
            raise CoverageRunnerError("runtime root transaction не готова к publication")
        _move_path(self.quarantine_directory, self.retired_directory)
        self.retired_for_publication = True
        self.prepared_for_publication = False

    def accept_publication(self) -> str | None:
        """После принятого artifact swap освобождает stale cache best-effort."""

        if not self.active or not self.retired_for_publication:
            raise CoverageRunnerError("runtime root publication не завершила atomic retire")
        self.active = False
        self.retired_for_publication = False
        try:
            _remove_generated_tree(self.retired_directory, "retired runtime quarantine")
        except CoverageRunnerError as error:
            return str(error)
        return None

    def commit(self) -> str | None:
        """Удобная граница unit-теста полного успешного publication lifecycle."""

        self.prepare_publication()
        self.complete_publication()
        return self.accept_publication()

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
from collections.abc import Callable
from pathlib import Path


class CoverageRunnerError(RuntimeError):
    """Ошибка, при которой нельзя публиковать частичный coverage cohort."""


class MergeMetadataTransaction:
    """Сохраняет только cargo-llvm-cov list/profdata вокруг нового cohort."""

    BACKUP_DIRECTORY_NAME = "replaced-merge-metadata"
    METADATA_PATTERNS = ("*-profraw-list", "*.profdata")

    def __init__(
        self,
        repo_root: Path,
        profile_directory: Path,
        artifact_stage: Path,
        published_artifact_directory: Path,
    ):
        self.repo_root = repo_root.resolve()
        self.requested_profile_directory = profile_directory.absolute()
        self.requested_artifact_stage = artifact_stage.absolute()
        self.requested_published_artifact_directory = (
            published_artifact_directory.absolute()
        )
        self.profile_directory = profile_directory.resolve()
        self.artifact_stage = artifact_stage.resolve()
        self.published_artifact_directory = published_artifact_directory.resolve()
        self.backup_directory = self.artifact_stage / self.BACKUP_DIRECTORY_NAME
        self.original_entries: list[dict[str, object]] = []
        self.quarantined_entries: list[dict[str, object]] = []
        self.authoritative_entries: list[dict[str, object]] | None = None
        self.active = False
        self.prepared_for_publication = False

    @staticmethod
    def paths_overlap(first: Path, second: Path) -> bool:
        """Определяет equality/ancestor collision уже canonical путей."""

        return first == second or first in second.parents or second in first.parents

    @staticmethod
    def validate_plain_descendant(
        label: str,
        requested_path: Path,
        target_root: Path,
    ) -> Path:
        """Запрещает symlink-компоненты и broad/outside generated roots."""

        absolute_path = requested_path.absolute()
        try:
            lexical_relative_path = absolute_path.relative_to(target_root)
        except ValueError as error:
            raise CoverageRunnerError(
                f"{label} находится вне repository target/"
            ) from error
        if any(component in {".", ".."} for component in lexical_relative_path.parts):
            raise CoverageRunnerError(f"{label} содержит неоднозначный path component")
        inspected_requested_path = target_root
        for component in lexical_relative_path.parts:
            inspected_requested_path /= component
            if inspected_requested_path.is_symlink():
                raise CoverageRunnerError(f"{label} не может содержать symlink")
        resolved_path = absolute_path.resolve()
        try:
            relative_path = resolved_path.relative_to(target_root)
        except ValueError as error:
            raise CoverageRunnerError(
                f"{label} находится вне repository target/"
            ) from error
        if not relative_path.parts:
            raise CoverageRunnerError(f"{label} не может совпадать с repository target/")
        return resolved_path

    @classmethod
    def validate_configured_roots(
        cls,
        repo_root: Path,
        profile_directory: Path,
        artifact_directory: Path,
    ) -> tuple[Path, Path]:
        """Проверяет layout до создания private stage либо запуска cargo clean."""

        canonical_repo = repo_root.resolve()
        requested_target = canonical_repo / "target"
        if requested_target.is_symlink():
            raise CoverageRunnerError("repository target/ не может быть symlink")
        canonical_target = requested_target.resolve()
        if canonical_target.parent != canonical_repo:
            raise CoverageRunnerError("repository target/ вышел за source root")
        canonical_profile = cls.validate_plain_descendant(
            "profile directory", profile_directory, canonical_target
        )
        canonical_artifact = cls.validate_plain_descendant(
            "artifact directory", artifact_directory, canonical_target
        )
        if cls.paths_overlap(canonical_profile, canonical_artifact):
            raise CoverageRunnerError(
                "profile и artifact directories не могут совпадать либо пересекаться"
            )
        return canonical_profile, canonical_artifact

    def validate_layout(self) -> None:
        """Повторно доказывает final/stage/quarantine containment перед move."""

        canonical_profile, canonical_artifact = self.validate_configured_roots(
            self.repo_root,
            self.requested_profile_directory,
            self.requested_published_artifact_directory,
        )
        canonical_target = (self.repo_root / "target").resolve()
        canonical_stage = self.validate_plain_descendant(
            "artifact stage", self.requested_artifact_stage, canonical_target
        )
        if self.paths_overlap(canonical_profile, canonical_stage):
            raise CoverageRunnerError(
                "profile directory и artifact stage не могут пересекаться"
            )
        if self.paths_overlap(canonical_stage, canonical_artifact):
            raise CoverageRunnerError("artifact stage и final не могут пересекаться")
        if canonical_stage.parent != canonical_artifact.parent:
            raise CoverageRunnerError("artifact stage и final должны быть siblings")
        if self.backup_directory.parent.resolve() != canonical_stage:
            raise CoverageRunnerError("merge metadata quarantine вышла за artifact stage")
        self.profile_directory = canonical_profile
        self.artifact_stage = canonical_stage
        self.published_artifact_directory = canonical_artifact
        self.backup_directory = canonical_stage / self.BACKUP_DIRECTORY_NAME

    def validate_profile_directory(self) -> None:
        """Разрешает merge metadata только в отдельном подкаталоге repo target/."""

        try:
            self.profile_directory.mkdir(parents=True, exist_ok=True)
        except OSError as error:
            raise CoverageRunnerError(
                f"не удалось подготовить isolated profile directory: {error}"
            ) from error
        if not self.profile_directory.is_dir():
            raise CoverageRunnerError("profile directory не является каталогом")

    def metadata_paths(self) -> list[Path]:
        """Находит только direct list/profdata names, которыми владеет wrapper."""

        candidates: dict[str, Path] = {}
        try:
            for pattern in self.METADATA_PATTERNS:
                for candidate in self.profile_directory.glob(pattern):
                    if candidate.name in candidates:
                        continue
                    candidate_stat = candidate.lstat()
                    if candidate.is_symlink() or not stat.S_ISREG(candidate_stat.st_mode):
                        raise CoverageRunnerError(
                            f"merge metadata не является regular file: {candidate.name}"
                        )
                    if candidate.parent.resolve() != self.profile_directory:
                        raise CoverageRunnerError(
                            f"merge metadata вышла за isolated target: {candidate.name}"
                        )
                    candidates[candidate.name] = candidate
        except OSError as error:
            raise CoverageRunnerError(
                f"не удалось перечислить merge metadata: {error}"
            ) from error
        return [candidates[name] for name in sorted(candidates)]

    @staticmethod
    def describe_file(path: Path) -> dict[str, object]:
        """Фиксирует переносимую content identity и разумный metadata минимум."""

        try:
            path_stat = path.lstat()
            if path.is_symlink() or not stat.S_ISREG(path_stat.st_mode):
                raise CoverageRunnerError(
                    f"merge metadata не является regular file: {path.name}"
                )
            return {
                "path": path.name,
                "size": path_stat.st_size,
                "sha256": sha256_file(path),
                "mode": stat.S_IMODE(path_stat.st_mode),
                "mtime_ns": path_stat.st_mtime_ns,
            }
        except OSError as error:
            raise CoverageRunnerError(
                f"не удалось прочитать merge metadata {path.name}: {error}"
            ) from error

    @staticmethod
    def assert_entry(path: Path, expected: dict[str, object]) -> None:
        """Доказывает byte/mode/mtime identity после move либо restore."""

        actual = MergeMetadataTransaction.describe_file(path)
        if actual != expected:
            raise CoverageRunnerError(
                f"merge metadata изменилась во время transaction: {path.name}"
            )

    def locate_backup(self, entry: dict[str, object]) -> Path:
        """Находит exact quarantine copy до либо после artifact publication."""

        name = str(entry["path"])
        backup_candidates = (
            self.backup_directory / name,
            self.published_artifact_directory / self.BACKUP_DIRECTORY_NAME / name,
        )
        for candidate in backup_candidates:
            if candidate.exists() or candidate.is_symlink():
                self.assert_entry(candidate, entry)
                return candidate
        raise CoverageRunnerError(f"quarantine copy отсутствует: {name}")

    def restore_moved_entries(self, moved_entries: list[dict[str, object]]) -> None:
        """Возвращает частично перемещённый preflight без перезаписи чужого файла."""

        restoration_errors: list[str] = []
        for entry in reversed(moved_entries):
            name = str(entry["path"])
            destination = self.profile_directory / name
            try:
                backup_path = self.locate_backup(entry)
                if destination.exists() or destination.is_symlink():
                    raise CoverageRunnerError(
                        f"restore destination уже существует: {name}"
                    )
                os.replace(backup_path, destination)
                self.assert_entry(destination, entry)
                self.quarantined_entries = [
                    quarantined
                    for quarantined in self.quarantined_entries
                    if quarantined["path"] != entry["path"]
                ]
            except (OSError, CoverageRunnerError) as error:
                restoration_errors.append(f"{name}: {error}")
        if restoration_errors:
            raise CoverageRunnerError(
                "не удалось восстановить merge metadata: "
                + "; ".join(restoration_errors)
            )

    def begin(self) -> None:
        """Хеширует и атомарно изолирует pre-existing merge metadata до clean."""

        self.validate_layout()
        self.validate_profile_directory()
        candidates = self.metadata_paths()
        self.original_entries = [self.describe_file(path) for path in candidates]
        if candidates:
            try:
                self.backup_directory.mkdir(parents=True, exist_ok=False)
                if (
                    self.profile_directory.stat().st_dev
                    != self.backup_directory.stat().st_dev
                ):
                    raise CoverageRunnerError(
                        "merge metadata quarantine находится на другом filesystem"
                    )
            except OSError as error:
                raise CoverageRunnerError(
                    f"не удалось создать merge metadata quarantine: {error}"
                ) from error
        self.active = True
        moved_entries: list[dict[str, object]] = []
        try:
            for candidate, entry in zip(candidates, self.original_entries, strict=True):
                os.replace(candidate, self.backup_directory / candidate.name)
                moved_entries.append(entry)
                self.quarantined_entries.append(entry)
                self.assert_entry(self.backup_directory / candidate.name, entry)
        except (OSError, CoverageRunnerError) as error:
            try:
                self.restore_moved_entries(moved_entries)
            except CoverageRunnerError as restore_error:
                raise CoverageRunnerError(
                    f"quarantine move завершился ошибкой ({error}); {restore_error}"
                ) from error
            self.active = False
            raise CoverageRunnerError(
                f"не удалось изолировать pre-existing merge metadata: {error}"
            ) from error

    def manifest(self) -> dict[str, object]:
        """Фиксирует старые и новые hashes перед атомарной публикацией cohort."""

        if not self.active:
            raise CoverageRunnerError("merge metadata transaction не активна")
        self.authoritative_entries = [
            self.describe_file(path) for path in self.metadata_paths()
        ]
        return {
            "schema_version": 1,
            "preexisting": self.original_entries,
            "authoritative": self.authoritative_entries,
            "backup_artifact": (
                self.BACKUP_DIRECTORY_NAME if self.original_entries else None
            ),
            "backup_retention": "replaced atomically by the next complete cohort",
        }

    def rollback(self) -> None:
        """Удаляет только post-quarantine metadata и возвращает originals."""

        if not self.active:
            return
        # Full cargo clean вправе удалить весь isolated target до последующей ошибки.
        # Restore сначала заново доказывает безопасный destination, иначе os.replace
        # либо потеряет понятную причину, либо не сможет вернуть оригиналы.
        self.validate_layout()
        self.validate_profile_directory()
        original_names = {str(entry["path"]) for entry in self.original_entries}
        quarantined_names = {
            str(entry["path"]) for entry in self.quarantined_entries
        }
        # Originals проверяются до удаления любого usable current replacement.
        for entry in self.quarantined_entries:
            self.locate_backup(entry)
        for replacement in self.metadata_paths():
            if (
                replacement.name in original_names
                and replacement.name not in quarantined_names
            ):
                continue
            try:
                replacement.unlink()
            except OSError as error:
                raise CoverageRunnerError(
                    f"не удалось удалить runner-owned metadata {replacement.name}: {error}"
                ) from error
        self.restore_moved_entries(list(self.quarantined_entries))
        self.active = False
        self.prepared_for_publication = False

    def prepare_publication(self) -> None:
        """Выполняет все fallible commit-проверки до atomic artifact swap."""

        if not self.active or self.authoritative_entries is None:
            raise CoverageRunnerError("merge metadata transaction не готова к publication")
        for entry in self.original_entries:
            self.assert_entry(self.backup_directory / str(entry["path"]), entry)
        actual_authoritative = [
            self.describe_file(path) for path in self.metadata_paths()
        ]
        if actual_authoritative != self.authoritative_entries:
            raise CoverageRunnerError(
                "authoritative merge metadata изменилась перед publication"
            )
        self.prepared_for_publication = True

    def complete_publication(self) -> None:
        """Завершает подготовленную transaction без I/O внутри artifact swap."""

        if not self.active or not self.prepared_for_publication:
            raise CoverageRunnerError(
                "merge metadata transaction не подготовлена к завершению publication"
            )
        self.active = False
        self.prepared_for_publication = False


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


def publish_artifacts(
    stage: Path,
    final_directory: Path,
    session_id: str,
    finalize: Callable[[], None] | None = None,
) -> Path | None:
    """Меняет дерево, удерживая previous до fallible transaction finalize."""

    del session_id
    backup = final_directory.parent / f".{final_directory.name}.previous"
    if backup.is_symlink() or (backup.exists() and not backup.is_dir()):
        raise CoverageRunnerError(
            "bounded previous artifact не является обычным каталогом"
        )
    if backup.exists():
        if not final_directory.exists():
            raise CoverageRunnerError(
                "bounded previous artifact является единственным last-known-good; "
                "требуется явное восстановление до нового publication"
            )
        try:
            shutil.rmtree(backup)
        except OSError as error:
            raise CoverageRunnerError(
                f"не удалось удалить bounded previous artifact до publication: {error}"
            ) from error
    moved_previous = False
    published_stage = False
    try:
        if final_directory.exists():
            os.replace(final_directory, backup)
            moved_previous = True
        os.replace(stage, final_directory)
        published_stage = True
        if finalize is not None:
            finalize()
    except BaseException:
        if published_stage and final_directory.exists() and not stage.exists():
            os.replace(final_directory, stage)
        if moved_previous and not final_directory.exists() and backup.exists():
            os.replace(backup, final_directory)
        raise
    return backup if moved_previous else None


def cleanup_retired_artifact(retired_artifact: Path | None) -> str | None:
    """Best-effort удаляет previous tree, не превращая accepted swap в failure."""

    if retired_artifact is None or not retired_artifact.exists():
        return None
    try:
        shutil.rmtree(retired_artifact)
    except OSError as error:
        return (
            "не удалось удалить bounded previous coverage artifact; "
            f"следующий publication повторит cleanup: {error}"
        )
    return None

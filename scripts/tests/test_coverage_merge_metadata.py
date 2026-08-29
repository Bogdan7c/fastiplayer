#!/usr/bin/env python3
"""Focused filesystem tests merge-metadata quarantine transaction."""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from coverage_runner_support import (  # noqa: E402
    CoverageRunnerError,
    MergeMetadataTransaction,
)


class MergeMetadataTransactionTests(unittest.TestCase):
    """Проверяет exact file ownership без запуска Cargo либо удаления target tree."""

    def build_fixture(self):
        """Создаёт isolated repo target, private stage и future final artifact."""

        temporary_directory = tempfile.TemporaryDirectory(prefix="coverage-merge-")
        repo_root = Path(temporary_directory.name)
        profile_directory = repo_root / "target" / "llvm-cov-target"
        profile_directory.mkdir(parents=True)
        artifact_parent = repo_root / "target" / "coverage"
        artifact_parent.mkdir()
        stage = artifact_parent / ".stable.stage-fixture"
        stage.mkdir()
        final = artifact_parent / "stable"
        transaction = MergeMetadataTransaction(
            repo_root,
            profile_directory,
            stage,
            final,
        )
        return temporary_directory, profile_directory, stage, final, transaction

    @staticmethod
    def seed_files(profile_directory: Path, files: dict[str, bytes]) -> None:
        """Фиксирует одинаковые mode/mtime для проверки точного restore."""

        for name, payload in files.items():
            path = profile_directory / name
            path.write_bytes(payload)
            path.chmod(0o640)
            os.utime(path, ns=(1_700_000_000_000_000_000,) * 2)

    def test_empty_list_pair_and_profdata_only_roundtrip_with_foreign_sibling(self):
        """Все неполные старые формы изолируются, а unrelated файл не затрагивается."""

        scenarios = {
            "empty-list": {"rustiplayer-profraw-list": b""},
            "list-and-profdata": {
                "rustiplayer-profraw-list": b"/old/profile.profraw\n",
                "rustiplayer.profdata": b"old-profdata",
            },
            "profdata-only": {"rustiplayer.profdata": b"orphan-profdata"},
        }
        for scenario, originals in scenarios.items():
            with self.subTest(scenario=scenario):
                fixture = self.build_fixture()
                temporary_directory, profile, _stage, _final, transaction = fixture
                try:
                    self.seed_files(profile, originals)
                    foreign = profile / "foreign-sibling.cache"
                    foreign.write_bytes(b"foreign")
                    transaction.begin()
                    self.assertTrue(foreign.is_file())
                    for name in originals:
                        self.assertFalse((profile / name).exists())
                    (profile / "current-profraw-list").write_bytes(b"new-list")
                    (profile / "current.profdata").write_bytes(b"new-profdata")
                    transaction.rollback()
                    self.assertEqual(foreign.read_bytes(), b"foreign")
                    for name, payload in originals.items():
                        restored = profile / name
                        self.assertEqual(restored.read_bytes(), payload)
                        self.assertEqual(restored.stat().st_mode & 0o777, 0o640)
                        self.assertEqual(
                            restored.stat().st_mtime_ns,
                            1_700_000_000_000_000_000,
                        )
                    self.assertFalse((profile / "current-profraw-list").exists())
                    self.assertFalse((profile / "current.profdata").exists())
                finally:
                    temporary_directory.cleanup()

    def test_symlink_directory_and_outside_target_are_rejected_without_mutation(self):
        """Malformed либо outside paths никогда не становятся cleanup targets."""

        for malformed_kind in ("symlink", "directory"):
            with self.subTest(malformed_kind=malformed_kind):
                fixture = self.build_fixture()
                temporary_directory, profile, _stage, _final, transaction = fixture
                try:
                    outside = Path(temporary_directory.name) / "outside.bin"
                    outside.write_bytes(b"outside")
                    malformed = profile / "malformed.profdata"
                    if malformed_kind == "symlink":
                        malformed.symlink_to(outside)
                    else:
                        malformed.mkdir()
                    with self.assertRaisesRegex(CoverageRunnerError, "regular file"):
                        transaction.begin()
                    self.assertEqual(outside.read_bytes(), b"outside")
                    self.assertTrue(malformed.exists() or malformed.is_symlink())
                finally:
                    temporary_directory.cleanup()

        fixture = self.build_fixture()
        temporary_directory, profile, stage, final, _transaction = fixture
        try:
            profile_link = profile.parent / "profile-link"
            profile_link.symlink_to(profile, target_is_directory=True)
            symlink_transaction = MergeMetadataTransaction(
                Path(temporary_directory.name),
                profile_link,
                stage,
                final,
            )
            with self.assertRaisesRegex(CoverageRunnerError, "profile directory.*symlink"):
                symlink_transaction.begin()
            outside_profile = Path(temporary_directory.name) / "outside-profile"
            outside_profile.mkdir()
            transaction = MergeMetadataTransaction(
                Path(temporary_directory.name),
                outside_profile,
                stage,
                final,
            )
            with self.assertRaisesRegex(CoverageRunnerError, "вне repository target"):
                transaction.begin()
        finally:
            temporary_directory.cleanup()

    def test_generated_roots_reject_broad_overlapping_and_nonsibling_layouts(self):
        """Configurable CLI paths не расширяют cleanup/publication ownership."""

        with tempfile.TemporaryDirectory(prefix="coverage-layout-") as temporary_name:
            repo_root = Path(temporary_name)
            target_root = repo_root / "target"
            target_root.mkdir()
            profile = target_root / "llvm-cov-target"
            profile.mkdir()
            safe_artifact = target_root / "coverage" / "stable"
            invalid_artifacts = {
                "equal": profile,
                "descendant": profile / "stable",
                "ancestor": target_root,
                "source": repo_root / "coverage",
            }
            for scenario, artifact in invalid_artifacts.items():
                with self.subTest(scenario=scenario):
                    with self.assertRaises(CoverageRunnerError):
                        MergeMetadataTransaction.validate_configured_roots(
                            repo_root, profile, artifact
                        )

            real_artifact = target_root / "real-artifact"
            real_artifact.mkdir()
            artifact_symlink = target_root / "artifact-link"
            artifact_symlink.symlink_to(real_artifact, target_is_directory=True)
            with self.assertRaisesRegex(CoverageRunnerError, "symlink"):
                MergeMetadataTransaction.validate_configured_roots(
                    repo_root, profile, artifact_symlink
                )
            nested_real_parent = target_root / "nested-real"
            nested_real_parent.mkdir()
            nested_alias = target_root / "nested-alias"
            nested_alias.symlink_to(nested_real_parent, target_is_directory=True)
            with self.assertRaisesRegex(CoverageRunnerError, "symlink"):
                MergeMetadataTransaction.validate_configured_roots(
                    repo_root, profile, nested_alias / "stable"
                )

            unsafe_stage = profile / ".stable.stage-collision"
            unsafe_stage.mkdir()
            transaction = MergeMetadataTransaction(
                repo_root,
                profile,
                unsafe_stage,
                safe_artifact,
            )
            with self.assertRaisesRegex(CoverageRunnerError, "stage.*пересекаться"):
                transaction.begin()

            nonsibling_stage = target_root / "other" / ".stable.stage-wrong-parent"
            nonsibling_stage.mkdir(parents=True)
            transaction = MergeMetadataTransaction(
                repo_root,
                profile,
                nonsibling_stage,
                safe_artifact,
            )
            with self.assertRaisesRegex(CoverageRunnerError, "siblings"):
                transaction.begin()

    def test_second_atomic_move_failure_restores_first_original(self):
        """Permission/move failure не оставляет половину originals в quarantine."""

        fixture = self.build_fixture()
        temporary_directory, profile, _stage, _final, transaction = fixture
        try:
            originals = {
                "a-profraw-list": b"list",
                "b.profdata": b"profdata",
            }
            self.seed_files(profile, originals)
            real_replace = os.replace
            quarantine_move_count = 0

            def fail_second_quarantine_move(source, destination):
                nonlocal quarantine_move_count
                source_path = Path(source)
                if source_path.parent == profile:
                    quarantine_move_count += 1
                    if quarantine_move_count == 2:
                        raise PermissionError("fixture move denied")
                return real_replace(source, destination)

            with mock.patch(
                "coverage_runner_support.os.replace",
                side_effect=fail_second_quarantine_move,
            ):
                with self.assertRaisesRegex(CoverageRunnerError, "изолировать"):
                    transaction.begin()
            for name, payload in originals.items():
                self.assertEqual((profile / name).read_bytes(), payload)
        finally:
            temporary_directory.cleanup()

    def test_rollback_recreates_profile_directory_removed_by_full_clean(self):
        """Ошибка сразу после cargo clean всё равно возвращает старый merge state."""

        fixture = self.build_fixture()
        temporary_directory, profile, _stage, _final, transaction = fixture
        try:
            originals = {
                "rustiplayer-profraw-list": b"old-list",
                "rustiplayer.profdata": b"old-profdata",
            }
            self.seed_files(profile, originals)
            transaction.begin()
            profile.rmdir()
            transaction.rollback()
            for name, payload in originals.items():
                self.assertEqual((profile / name).read_bytes(), payload)
        finally:
            temporary_directory.cleanup()

    def test_rollback_rejects_profile_symlink_inserted_after_full_clean(self):
        """Restore не следует по подменённому parent и сохраняет quarantine."""

        fixture = self.build_fixture()
        temporary_directory, profile, stage, _final, transaction = fixture
        try:
            originals = {
                "rustiplayer-profraw-list": b"old-list",
                "rustiplayer.profdata": b"old-profdata",
            }
            self.seed_files(profile, originals)
            transaction.begin()
            profile.rmdir()
            outside = Path(temporary_directory.name) / "outside-restore"
            outside.mkdir()
            profile.symlink_to(outside, target_is_directory=True)
            with self.assertRaisesRegex(CoverageRunnerError, "symlink"):
                transaction.rollback()
            self.assertEqual(list(outside.iterdir()), [])
            quarantine = stage / "replaced-merge-metadata"
            self.assertEqual(
                {path.name: path.read_bytes() for path in quarantine.iterdir()},
                originals,
            )
            profile.unlink()
            transaction.rollback()
            self.assertEqual(
                {name: (profile / name).read_bytes() for name in originals},
                originals,
            )
        finally:
            temporary_directory.cleanup()

    def test_manifest_has_no_absolute_paths_and_commit_keeps_one_bounded_backup(self):
        """Success сохраняет hashes originals/replacements без environment path leakage."""

        fixture = self.build_fixture()
        temporary_directory, profile, stage, final, transaction = fixture
        try:
            self.seed_files(profile, {"old-profraw-list": b"old"})
            transaction.begin()
            (profile / "current-profraw-list").write_bytes(b"current-list")
            (profile / "current.profdata").write_bytes(b"current-profdata")
            manifest = transaction.manifest()
            self.assertEqual(manifest["schema_version"], 1)
            for entry in manifest["preexisting"] + manifest["authoritative"]:
                self.assertEqual(Path(entry["path"]).name, entry["path"])
                self.assertNotIn(str(temporary_directory.name), entry["path"])
            transaction.prepare_publication()
            os.replace(stage, final)
            transaction.complete_publication()
            self.assertEqual(
                (final / "replaced-merge-metadata" / "old-profraw-list").read_bytes(),
                b"old",
            )
            self.assertEqual(
                len(list(final.rglob("replaced-merge-metadata"))),
                1,
            )
        finally:
            temporary_directory.cleanup()


if __name__ == "__main__":
    unittest.main()

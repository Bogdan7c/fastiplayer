#!/usr/bin/env python3
"""Filesystem vertical tests атомарной coverage publication transaction."""

from __future__ import annotations

import shutil
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
    cleanup_retired_artifact,
    publish_artifacts,
)


class CoveragePublicationTransactionTests(unittest.TestCase):
    """Закрепляет общий commit point artifact tree и merge metadata."""

    def build_transaction(self):
        """Создаёт previous tree, quarantine originals и готовый новый stage."""

        temporary_directory = tempfile.TemporaryDirectory(prefix="coverage-publish-")
        repo_root = Path(temporary_directory.name)
        profile_directory = repo_root / "target" / "llvm-cov-target"
        profile_directory.mkdir(parents=True)
        old_list = profile_directory / "rustiplayer-profraw-list"
        old_profdata = profile_directory / "rustiplayer.profdata"
        old_list.write_bytes(b"old-list")
        old_profdata.write_bytes(b"old-profdata")
        artifact_parent = repo_root / "target" / "coverage"
        artifact_parent.mkdir()
        final = artifact_parent / "stable"
        final.mkdir()
        (final / "accepted.txt").write_bytes(b"previous")
        stage = artifact_parent / ".stable.stage-fixture"
        stage.mkdir()
        (stage / "accepted.txt").write_bytes(b"current")
        transaction = MergeMetadataTransaction(
            repo_root,
            profile_directory,
            stage,
            final,
        )
        transaction.begin()
        current_list = profile_directory / old_list.name
        current_profdata = profile_directory / old_profdata.name
        current_list.write_bytes(b"current-list")
        current_profdata.write_bytes(b"current-profdata")
        transaction.manifest()
        return (
            temporary_directory,
            profile_directory,
            stage,
            final,
            transaction,
        )

    def test_finalize_without_prepare_rolls_back_tree_and_original_metadata(self):
        """Out-of-order finalize после swap не оставляет failed cohort current."""

        fixture = self.build_transaction()
        temporary_directory, profile, stage, final, transaction = fixture
        try:
            with self.assertRaisesRegex(CoverageRunnerError, "не подготовлена"):
                publish_artifacts(
                    stage,
                    final,
                    "fixture",
                    transaction.complete_publication,
                )
            transaction.rollback()
            self.assertEqual((final / "accepted.txt").read_bytes(), b"previous")
            self.assertEqual((stage / "accepted.txt").read_bytes(), b"current")
            self.assertEqual(
                (profile / "rustiplayer-profraw-list").read_bytes(), b"old-list"
            )
            self.assertEqual(
                (profile / "rustiplayer.profdata").read_bytes(), b"old-profdata"
            )
        finally:
            temporary_directory.cleanup()

    def test_rmtree_failure_is_bounded_warning_and_next_swap_fails_before_mutation(self):
        """Cleanup previous tree не откатывает accepted cohort и не множит backups."""

        fixture = self.build_transaction()
        temporary_directory, profile, _stage, final, transaction = fixture
        try:
            transaction.prepare_publication()
            retired = publish_artifacts(
                transaction.artifact_stage,
                final,
                "fixture",
                transaction.complete_publication,
            )
            self.assertIsNotNone(retired)
            retired = Path(retired)
            real_rmtree = shutil.rmtree

            def reject_retired_cleanup(path, *arguments, **keywords):
                if Path(path) == retired:
                    raise PermissionError("fixture retained cleanup denied")
                return real_rmtree(path, *arguments, **keywords)

            with mock.patch(
                "coverage_runner_support.shutil.rmtree",
                side_effect=reject_retired_cleanup,
            ):
                warning = cleanup_retired_artifact(retired)
                self.assertIn("следующий publication", warning)
                self.assertEqual((final / "accepted.txt").read_bytes(), b"current")
                self.assertEqual(
                    (profile / "rustiplayer-profraw-list").read_bytes(), b"current-list"
                )
                self.assertEqual((retired / "accepted.txt").read_bytes(), b"previous")
                next_stage = final.parent / ".stable.stage-next"
                next_stage.mkdir()
                (next_stage / "accepted.txt").write_bytes(b"next")
                with self.assertRaisesRegex(CoverageRunnerError, "bounded previous"):
                    publish_artifacts(next_stage, final, "next")
                self.assertEqual((final / "accepted.txt").read_bytes(), b"current")
                self.assertEqual((next_stage / "accepted.txt").read_bytes(), b"next")
            self.assertEqual(len(list(final.parent.glob(".stable.previous"))), 1)
        finally:
            temporary_directory.cleanup()

    def test_previous_only_crash_state_is_never_deleted_or_overwritten(self):
        """Last-known-good после interrupted swap требует явного recovery."""

        with tempfile.TemporaryDirectory(prefix="coverage-crash-") as temporary_name:
            artifact_parent = Path(temporary_name)
            final = artifact_parent / "stable"
            previous = artifact_parent / ".stable.previous"
            previous.mkdir()
            (previous / "accepted.txt").write_bytes(b"last-good")
            stage = artifact_parent / ".stable.stage-next"
            stage.mkdir()
            (stage / "accepted.txt").write_bytes(b"incoming")
            with self.assertRaisesRegex(CoverageRunnerError, "last-known-good"):
                publish_artifacts(stage, final, "next")
            self.assertFalse(final.exists())
            self.assertEqual(
                (previous / "accepted.txt").read_bytes(), b"last-good"
            )
            self.assertEqual((stage / "accepted.txt").read_bytes(), b"incoming")


if __name__ == "__main__":
    unittest.main()

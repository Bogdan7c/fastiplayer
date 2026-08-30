#!/usr/bin/env python3
"""Focused tests executable policy, inventories и runtime-root transaction."""

from __future__ import annotations

import hashlib
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

import coverage_executable_inventory as inventory  # noqa: E402
import coverage_runtime_quarantine as quarantine  # noqa: E402
from coverage_executable_inventory import (  # noqa: E402
    PrebuiltExecutableReference,
    RuntimeExecutableReference,
    runtime_executable_manifest,
)
from coverage_executable_policy import (  # noqa: E402
    CargoTestMaterializer,
    ExecutableInventoryPolicy,
    RuntimeBuildRootPolicy,
    load_executable_inventory_policy,
)
from coverage_runner_support import CoverageRunnerError  # noqa: E402
from coverage_runtime_quarantine import RuntimeRootTransaction  # noqa: E402


def root_policy(owner: str = "trybuild", relative_root: str = "tests/trybuild"):
    """Возвращает production-shaped owner без arbitrary command string."""

    return RuntimeBuildRootPolicy(
        owner=owner,
        relative_root=Path(relative_root),
        materializer=CargoTestMaterializer("settings-derive", "trybuild"),
    )


def policy(*roots: RuntimeBuildRootPolicy) -> ExecutableInventoryPolicy:
    """Возвращает typed policy; default совпадает с production owner."""

    exact_roots = roots or (root_policy(),)
    return ExecutableInventoryPolicy(1, tuple(exact_roots))


class ExecutablePolicyTests(unittest.TestCase):
    """Проверяет strict JSON schema и отсутствие broad hidden config."""

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.policy_path = Path(self.temporary_directory.name) / "policy.json"

    def tearDown(self):
        self.temporary_directory.cleanup()

    def write_policy(self, document: object) -> None:
        self.policy_path.write_text(json.dumps(document), encoding="utf-8")

    @staticmethod
    def valid_document() -> dict[str, object]:
        return {
            "schema_version": 1,
            "runtime_build_roots": [
                {
                    "owner": "trybuild",
                    "relative_root": "tests/trybuild",
                    "materializer": {
                        "kind": "cargo-test",
                        "package": "settings-derive",
                        "test": "trybuild",
                    },
                }
            ],
        }

    def test_policy_accepts_typed_owner_and_empty_fixture_inventory(self):
        self.write_policy(self.valid_document())
        self.assertEqual(load_executable_inventory_policy(self.policy_path), policy())
        self.write_policy({"schema_version": 1, "runtime_build_roots": []})
        self.assertEqual(
            load_executable_inventory_policy(self.policy_path).runtime_build_roots,
            (),
        )

    def test_policy_rejects_noninteger_broad_overlap_and_arbitrary_materializer(self):
        valid = self.valid_document()
        first_root = valid["runtime_build_roots"][0]
        invalid_documents = {
            "bool-version": {**valid, "schema_version": True},
            "float-version": {**valid, "schema_version": 1.0},
            "broad-root": {
                **valid,
                "runtime_build_roots": [{**first_root, "relative_root": "tests"}],
            },
            "parent-root": {
                **valid,
                "runtime_build_roots": [
                    {**first_root, "relative_root": "tests/../debug"}
                ],
            },
            "overlap": {
                **valid,
                "runtime_build_roots": [
                    first_root,
                    {
                        **first_root,
                        "owner": "nested",
                        "relative_root": "tests/trybuild/nested",
                    },
                ],
            },
            "duplicate-materializer": {
                **valid,
                "runtime_build_roots": [
                    first_root,
                    {
                        **first_root,
                        "owner": "second",
                        "relative_root": "tests/second",
                    },
                ],
            },
            "arbitrary-kind": {
                **valid,
                "runtime_build_roots": [
                    {
                        **first_root,
                        "materializer": {
                            "kind": "shell",
                            "package": "settings-derive",
                            "test": "trybuild",
                        },
                    }
                ],
            },
        }
        for scenario, document in invalid_documents.items():
            with self.subTest(scenario=scenario):
                self.write_policy(document)
                with self.assertRaises(CoverageRunnerError):
                    load_executable_inventory_policy(self.policy_path)

    def test_policy_wraps_malformed_json_as_runner_error(self):
        self.policy_path.write_text("{broken", encoding="utf-8")
        with self.assertRaisesRegex(CoverageRunnerError, "inventory policy"):
            load_executable_inventory_policy(self.policy_path)


class ExecutableInventoryTests(unittest.TestCase):
    """Проверяет parent threat model и prewarmed runtime reference."""

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.profile_directory = Path(self.temporary_directory.name) / "profile"
        self.profile_directory.mkdir()
        self.root_policy = root_policy()
        self.policy = policy()

    def tearDown(self):
        self.temporary_directory.cleanup()

    def executable(self, relative_path: str, content: bytes = b"AAAA") -> Path:
        executable_path = self.profile_directory / relative_path
        executable_path.parent.mkdir(parents=True, exist_ok=True)
        executable_path.write_bytes(content)
        executable_path.chmod(0o755)
        return executable_path

    def materialize_runtime_hardlinks(self, content: bytes = b"runtime") -> None:
        runtime_root = self.profile_directory / self.root_policy.relative_root
        runtime_root.mkdir(parents=True, exist_ok=True)
        executable_path = runtime_root / "settings-derive-tests"
        hardlink_path = runtime_root / "settings-derive-tests-hardlink"
        executable_path.unlink(missing_ok=True)
        hardlink_path.unlink(missing_ok=True)
        executable_path.write_bytes(content)
        executable_path.chmod(0o755)
        os.link(executable_path, hardlink_path)

    def test_parent_rejects_same_size_replacement_with_restored_mtime(self):
        executable_path = self.executable("debug/deps/parent-test")
        with mock.patch.object(inventory, "_probe_ctime_capability", return_value=True):
            reference = PrebuiltExecutableReference(self.profile_directory, self.policy)
        original_stat = executable_path.stat()
        executable_path.write_bytes(b"BBBB")
        os.utime(executable_path, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))
        self.assertEqual(executable_path.stat().st_mtime_ns, original_stat.st_mtime_ns)
        with self.assertRaisesRegex(CoverageRunnerError, "changed=.*parent-test"):
            reference.assert_unchanged()

    def test_ctime_incapable_filesystem_falls_back_to_full_hash(self):
        executable_path = self.executable("debug/deps/parent-test")
        with mock.patch.object(inventory, "_probe_ctime_capability", return_value=False):
            reference = PrebuiltExecutableReference(self.profile_directory, self.policy)
        executable_path.write_bytes(b"BBBB")
        reference.observed_identities["debug/deps/parent-test"] = (
            inventory._opened_file_identity(executable_path.lstat())
        )
        with self.assertRaisesRegex(CoverageRunnerError, "changed=.*parent-test"):
            reference.assert_unchanged()

    def test_parent_rejects_added_removed_and_mode_changed_executables(self):
        executable_path = self.executable("debug/deps/parent-test")
        self.executable("debug/deps/stable-test")
        with mock.patch.object(inventory, "_probe_ctime_capability", return_value=True):
            reference = PrebuiltExecutableReference(self.profile_directory, self.policy)
        self.executable("debug/deps/unplanned-test")
        with self.assertRaisesRegex(CoverageRunnerError, "added=.*unplanned-test"):
            reference.assert_unchanged()
        (self.profile_directory / "debug/deps/unplanned-test").unlink()
        executable_path.chmod(0o700)
        with self.assertRaisesRegex(CoverageRunnerError, "changed=.*parent-test"):
            reference.assert_unchanged()
        executable_path.unlink()
        with self.assertRaisesRegex(CoverageRunnerError, "removed=.*parent-test"):
            reference.assert_unchanged()

    def test_prewarm_freeze_accepts_byte_identical_inode_mtime_churn(self):
        self.materialize_runtime_hardlinks()
        reference = RuntimeExecutableReference(self.profile_directory, self.root_policy)
        reference.freeze_after_materialization()
        accepted_manifest = reference.manifest()
        for run_number in (1, 2, 3):
            reference.assert_ready_before_run(run_number)
            self.materialize_runtime_hardlinks()
            reference.observe_completed_run(run_number)
        reference.assert_final()
        self.assertEqual(reference.manifest(), accepted_manifest)
        self.assertEqual(accepted_manifest["file_count"], 2)

    def test_prewarm_rejects_empty_root_and_later_content_mutation(self):
        runtime_root = self.profile_directory / self.root_policy.relative_root
        runtime_root.mkdir(parents=True)
        reference = RuntimeExecutableReference(self.profile_directory, self.root_policy)
        with self.assertRaisesRegex(CoverageRunnerError, "не создал executables"):
            reference.freeze_after_materialization()
        self.materialize_runtime_hardlinks()
        reference.freeze_after_materialization()
        reference.assert_ready_before_run(1)
        self.materialize_runtime_hardlinks(b"MUTATED")
        with self.assertRaisesRegex(CoverageRunnerError, "changed=.*settings-derive"):
            reference.observe_completed_run(1)

    def test_runtime_manifest_deduplicates_hardlink_reads_and_rejects_symlink(self):
        self.materialize_runtime_hardlinks()
        real_sha256 = hashlib.sha256
        with mock.patch.object(inventory.hashlib, "sha256", wraps=real_sha256) as sha256_spy:
            manifest = runtime_executable_manifest(self.profile_directory, self.root_policy)
        self.assertEqual(sha256_spy.call_count, 2)
        self.assertEqual(manifest["file_count"], 2)
        alias = self.profile_directory / self.root_policy.relative_root / "settings-derive-tests-hardlink"
        alias.unlink()
        alias.symlink_to("settings-derive-tests")
        with self.assertRaisesRegex(CoverageRunnerError, "symlink"):
            runtime_executable_manifest(self.profile_directory, self.root_policy)


class RuntimeRootTransactionTests(unittest.TestCase):
    """Проверяет stale cleanup, rollback и fail-closed orphan handling."""

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.profile_directory = Path(self.temporary_directory.name) / "profile"
        self.artifact_directory = Path(self.temporary_directory.name) / "coverage/stable"
        self.runtime_root = self.profile_directory / "tests" / "trybuild"
        self.runtime_root.mkdir(parents=True)
        self.policy = policy()

    def tearDown(self):
        self.temporary_directory.cleanup()

    def transaction(self, exact_policy: ExecutableInventoryPolicy | None = None):
        return RuntimeRootTransaction(
            self.profile_directory,
            self.artifact_directory,
            exact_policy or self.policy,
        )

    def write_original(self, payload: bytes = b"old-cache") -> Path:
        cache_file = self.runtime_root / "cache.bin"
        cache_file.parent.mkdir(parents=True, exist_ok=True)
        cache_file.write_bytes(payload)
        return cache_file

    def test_failure_restores_original_and_success_discards_only_stale_cache(self):
        self.write_original()
        transaction = self.transaction()
        transaction.begin()
        regenerated = self.runtime_root / "new.bin"
        regenerated.parent.mkdir(parents=True)
        regenerated.write_bytes(b"new-cache")
        transaction.rollback()
        self.assertEqual((self.runtime_root / "cache.bin").read_bytes(), b"old-cache")
        self.assertFalse(regenerated.exists())

        transaction = self.transaction()
        transaction.begin()
        regenerated.parent.mkdir(parents=True)
        regenerated.write_bytes(b"accepted-cache")
        self.assertIsNone(transaction.commit())
        self.assertEqual(regenerated.read_bytes(), b"accepted-cache")
        self.assertFalse(transaction.quarantine_directory.exists())

    def test_empty_aborted_directory_is_cleaned_but_journaled_orphan_fails_closed(self):
        self.write_original()
        transaction = self.transaction()
        transaction.quarantine_directory.mkdir()
        transaction.begin()
        transaction.rollback()
        transaction.quarantine_directory.mkdir()
        transaction.journal_path.write_text('{"state":"preparing"}', encoding="utf-8")
        with self.assertRaisesRegex(CoverageRunnerError, "orphaned runtime quarantine"):
            self.transaction().begin()

    def test_postpublication_retired_cleanup_is_retryable(self):
        self.write_original()
        transaction = self.transaction()
        transaction.begin()
        accepted = self.runtime_root / "accepted.bin"
        accepted.parent.mkdir(parents=True)
        accepted.write_bytes(b"accepted")
        with mock.patch.object(
            quarantine,
            "_remove_generated_tree",
            side_effect=CoverageRunnerError("simulated cleanup failure"),
        ):
            warning = transaction.commit()
        self.assertIn("simulated cleanup failure", warning)
        restarted = self.transaction()
        restarted.begin()
        restarted.rollback()
        self.assertEqual(accepted.read_bytes(), b"accepted")

    def test_mkdir_and_journal_failures_leave_original_without_orphan(self):
        self.write_original()
        for scenario in ("mkdir", "journal"):
            with self.subTest(scenario=scenario):
                transaction = self.transaction()
                patch_target = (
                    mock.patch.object(
                        quarantine,
                        "_create_quarantine_directory",
                        side_effect=OSError("mkdir failed"),
                    )
                    if scenario == "mkdir"
                    else mock.patch.object(
                        transaction,
                        "_write_journal",
                        side_effect=OSError("journal failed"),
                    )
                )
                with patch_target, self.assertRaisesRegex(OSError, "failed"):
                    transaction.begin()
                self.assertEqual(
                    (self.runtime_root / "cache.bin").read_bytes(),
                    b"old-cache",
                )
                self.assertFalse(transaction.quarantine_directory.exists())

    def test_exception_after_atomic_move_is_inferred_and_restored(self):
        self.write_original()
        transaction = self.transaction()
        real_move = quarantine._move_path

        def move_then_fail(source: Path, destination: Path) -> None:
            real_move(source, destination)
            if source == self.runtime_root:
                raise KeyboardInterrupt("after move")

        with mock.patch.object(quarantine, "_move_path", side_effect=move_then_fail):
            with self.assertRaisesRegex(KeyboardInterrupt, "after move"):
                transaction.begin()
        self.assertEqual((self.runtime_root / "cache.bin").read_bytes(), b"old-cache")
        self.assertFalse(transaction.quarantine_directory.exists())

    def test_unknown_quarantine_owner_fails_closed_without_losing_original(self):
        self.write_original()
        transaction = self.transaction()
        transaction.begin()
        unknown_root = transaction.quarantine_directory / "roots" / "unknown"
        unknown_root.mkdir()
        (unknown_root / "cache.bin").write_bytes(b"unknown")
        with self.assertRaisesRegex(CoverageRunnerError, "unknown quarantine owner"):
            transaction.rollback()
        self.assertEqual((self.runtime_root / "cache.bin").read_bytes(), b"old-cache")
        self.assertEqual((unknown_root / "cache.bin").read_bytes(), b"unknown")

    def test_multi_root_second_move_failure_rolls_back_first_exactly(self):
        second_policy = root_policy("second", "tests/second")
        second_root = self.profile_directory / second_policy.relative_root
        second_root.mkdir(parents=True)
        (second_root / "cache.bin").write_bytes(b"second-cache")
        self.write_original()
        transaction = self.transaction(policy(root_policy(), second_policy))
        real_move = quarantine._move_path

        def fail_second_move(source: Path, destination: Path) -> None:
            if source == second_root:
                raise OSError("second move failed")
            real_move(source, destination)

        with mock.patch.object(quarantine, "_move_path", side_effect=fail_second_move):
            with self.assertRaisesRegex(OSError, "second move failed"):
                transaction.begin()
        self.assertEqual((self.runtime_root / "cache.bin").read_bytes(), b"old-cache")
        self.assertEqual((second_root / "cache.bin").read_bytes(), b"second-cache")

    def test_symlink_and_artifact_overlap_fail_closed(self):
        external = Path(self.temporary_directory.name) / "external"
        external.mkdir()
        self.runtime_root.rmdir()
        self.runtime_root.symlink_to(external, target_is_directory=True)
        with self.assertRaisesRegex(CoverageRunnerError, "plain directory|symlink"):
            self.transaction().begin()
        self.runtime_root.unlink()
        overlapping_artifact = (
            self.profile_directory.parent
            / f".{self.profile_directory.name}.stable-runtime-quarantine/artifacts"
        )
        with self.assertRaisesRegex(CoverageRunnerError, "пересекается"):
            RuntimeRootTransaction(
                self.profile_directory,
                overlapping_artifact,
                self.policy,
            )


if __name__ == "__main__":
    unittest.main()

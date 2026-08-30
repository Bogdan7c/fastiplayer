#!/usr/bin/env python3
"""End-to-end runner tests для build/prewarm executable inventory boundary."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from coverage_runner import RunnerConfig, StableCoverageRunner  # noqa: E402
from coverage_runner_support import CoverageRunnerError  # noqa: E402
from coverage_runtime_quarantine import RuntimeRootTransaction  # noqa: E402
from scripts.tests.test_coverage_runner import FakeCoverageExecutor  # noqa: E402


class CoverageRunnerInventoryTests(unittest.TestCase):
    """Проверяет публичный lifecycle нового inventory/prewarm слоя."""

    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.repo_root = Path(self.temporary_directory.name)
        (self.repo_root / "src").mkdir()
        (self.repo_root / "src/lib.rs").write_text(
            "pub fn covered() {}\n",
            encoding="utf-8",
        )
        (self.repo_root / ".gitignore").write_text("/target/\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q", self.repo_root], check=True)
        scripts_directory = self.repo_root / "scripts"
        scripts_directory.mkdir()
        for script_name in (
            "coverage_coordinates.py",
            "coverage_stability.py",
            "coverage_metrics.py",
        ):
            (scripts_directory / script_name).write_text("# fixture\n", encoding="utf-8")
        coverage_directory = self.repo_root / "coverage"
        coverage_directory.mkdir()
        (coverage_directory / "policy.json").write_text("{}\n", encoding="utf-8")
        executable_policy = coverage_directory / "executable-inventory-policy.json"
        executable_policy.write_text(
            json.dumps(
                {
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
            ),
            encoding="utf-8",
        )
        self.config = RunnerConfig(
            repo_root=self.repo_root,
            profile_directory=self.repo_root / "target/llvm-cov-target",
            artifact_directory=self.repo_root / "target/coverage/stable",
            policy_path=coverage_directory / "policy.json",
            executable_inventory_policy_path=executable_policy,
            coordinate_extractor=scripts_directory / "coverage_coordinates.py",
            stability_tool=scripts_directory / "coverage_stability.py",
            lcov_validator=scripts_directory / "coverage_metrics.py",
            toolchain="1.96.0",
            cargo_llvm_cov_version="0.8.7",
            llvm_cov_version="22.1.2",
            session_id="fixture-session",
            cargo_command="fake-cargo",
            rustc_command="fake-rustc",
            python_command="fake-python",
        )

    def tearDown(self):
        self.temporary_directory.cleanup()

    def new_runner(self):
        executor = FakeCoverageExecutor(self.config)
        return StableCoverageRunner(self.config, executor), executor

    def assert_no_private_stage(self):
        self.assertEqual(
            list(self.config.artifact_directory.parent.glob(".stable.stage-*")),
            [],
        )

    def test_success_builds_once_prewarm_once_and_runs_exact_suite_three_times(self):
        runner, executor = self.new_runner()
        with mock.patch.dict(os.environ, {"RUST_TEST_THREADS": "1"}):
            runner.run()
        run_commands = [
            command
            for command in executor.commands
            if "test" in command.arguments
            and "--workspace" in command.arguments
            and "--no-run" not in command.arguments
        ]
        self.assertEqual(executor.build_count, 1)
        self.assertEqual(executor.prewarm_count, 1)
        self.assertEqual(len(run_commands), 3)
        self.assertEqual(len({command.arguments for command in run_commands}), 1)
        self.assertEqual(
            [command.profile_name for command in run_commands],
            [
                "stable-fixture-session-run-1-%p-%16m.profraw",
                "stable-fixture-session-run-2-%p-%16m.profraw",
                "stable-fixture-session-run-3-%p-%16m.profraw",
            ],
        )
        self.assertTrue(all(command.rust_test_threads is None for command in run_commands))
        profile_cleans = [
            command
            for command in executor.commands
            if "clean" in command.arguments and "--profraw-only" in command.arguments
        ]
        self.assertEqual(len(profile_cleans), 5)
        self.assertEqual(executor.report_count, 10)
        self.assert_no_private_stage()

    def test_manifest_records_schema2_typed_prewarm_and_nonempty_runtime_set(self):
        runner, _executor = self.new_runner()
        runner.run()
        cohort_manifest = json.loads(
            (self.config.artifact_directory / "cohort-manifest.json").read_text()
        )
        self.assertEqual(cohort_manifest["schema_version"], 2)
        runtime_root = cohort_manifest["runtime_build_roots"][0]
        self.assertEqual(runtime_root["owner"], "trybuild")
        self.assertEqual(runtime_root["manifest"]["file_count"], 2)
        self.assertEqual(
            runtime_root["materialization"],
            {
                "phase": "prewarm",
                "kind": "cargo-test",
                "package": "settings-derive",
                "test": "trybuild",
            },
        )
        self.assertTrue(cohort_manifest["artifacts"])

    def test_runtime_materialization_is_bounded(self):
        for scenario in ("runtime", "outside", "symlink"):
            with self.subTest(scenario=scenario):
                runner, executor = self.new_runner()
                if scenario == "runtime":
                    executor.mutate_runtime_build_run = 2
                elif scenario == "outside":
                    executor.add_outside_runtime_build_run = 1
                else:
                    executor.runtime_symlink_run = 2
                with self.assertRaises(CoverageRunnerError):
                    runner.run()
                self.assert_no_private_stage()
                shutil.rmtree(self.config.artifact_directory, ignore_errors=True)

    def test_prewarm_discards_build_profiles_and_rejects_empty_or_mixed_output(self):
        runner, executor = self.new_runner()
        executor.emit_build_profile = True
        runner.run()
        self.assertFalse((self.config.profile_directory / "build-script.profraw").exists())
        self.assertFalse(
            any("prewarm" in path.name for path in self.config.profile_directory.glob("*.profraw"))
        )
        for scenario in ("empty", "mixed", "invalid-name", "subprocess-failure"):
            with self.subTest(scenario=scenario):
                shutil.rmtree(self.config.artifact_directory, ignore_errors=True)
                runner, executor = self.new_runner()
                if scenario == "empty":
                    executor.skip_prewarm_executables = True
                elif scenario == "mixed":
                    executor.prewarm_foreign_profile = True
                elif scenario == "invalid-name":
                    executor.prewarm_invalid_profile = True
                else:
                    executor.fail_prewarm_after_profile = True
                with self.assertRaises(CoverageRunnerError):
                    runner.run()
                if scenario == "subprocess-failure":
                    self.assertFalse(
                        any(
                            "prewarm" in path.name
                            for path in self.config.profile_directory.glob("*.profraw")
                        )
                    )
                self.assert_no_private_stage()

    def test_failed_cohort_restores_stale_nested_cache_byte_exactly(self):
        stale_cache = self.config.profile_directory / "tests/trybuild/stale.bin"
        stale_cache.parent.mkdir(parents=True)
        stale_cache.write_bytes(b"pre-run-cache")
        runner, executor = self.new_runner()
        executor.failed_run = 1
        with self.assertRaises(CoverageRunnerError):
            runner.run()
        self.assertEqual(stale_cache.read_bytes(), b"pre-run-cache")
        self.assertFalse(
            self.config.profile_directory.parent.joinpath(
                ".llvm-cov-target.stable-runtime-quarantine"
            ).exists()
        )

    def test_finalize_failure_after_runtime_retire_rolls_back_cache_and_artifact(self):
        stale_cache = self.config.profile_directory / "tests/trybuild/stale.bin"
        stale_cache.parent.mkdir(parents=True)
        stale_cache.write_bytes(b"pre-run-cache")
        self.config.artifact_directory.mkdir(parents=True)
        previous_marker = self.config.artifact_directory / "previous.txt"
        previous_marker.write_text("accepted", encoding="utf-8")
        runner, _executor = self.new_runner()
        real_complete = RuntimeRootTransaction.complete_publication

        def retire_then_fail(transaction: RuntimeRootTransaction) -> None:
            real_complete(transaction)
            raise OSError("fixture finalize failure after runtime retire")

        with mock.patch.object(
            RuntimeRootTransaction,
            "complete_publication",
            autospec=True,
            side_effect=retire_then_fail,
        ):
            with self.assertRaisesRegex(OSError, "finalize failure"):
                runner.run()
        self.assertEqual(stale_cache.read_bytes(), b"pre-run-cache")
        self.assertEqual(previous_marker.read_text(encoding="utf-8"), "accepted")
        self.assert_no_private_stage()


if __name__ == "__main__":
    unittest.main()

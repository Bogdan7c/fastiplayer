#!/usr/bin/env python3
"""Hermetic tests build-once/three-run coverage orchestration."""

from __future__ import annotations

import contextlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from dataclasses import dataclass
from pathlib import Path
from unittest import mock


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from coverage_runner import (  # noqa: E402
    CommandExecutor,
    CoverageRunnerError,
    RunnerConfig,
    StableCoverageRunner,
)
from coverage_runner_support import (  # noqa: E402
    MergeMetadataTransaction,
    publish_artifacts,
    sha256_file,
)


@dataclass(frozen=True)
class RecordedCommand:
    """Exact argv/env одного fake subprocess вызова."""

    arguments: tuple[str, ...]
    profile_name: str | None
    profile_path: str | None
    rust_test_threads: str | None


class FakeCoverageExecutor(CommandExecutor):
    """Имитирует cargo-llvm-cov, сохраняя реальную filesystem handoff-семантику."""

    def __init__(self, config: RunnerConfig):
        self.config = config
        self.commands: list[RecordedCommand] = []
        self.execution_count = 0
        self.prewarm_count = 0
        self.build_count = 0
        self.report_count = 0
        self.empty_run: int | None = None
        self.mixed_run: int | None = None
        self.failed_run: int | None = None
        self.mutate_source_run: int | None = None
        self.mutate_build_run: int | None = None
        self.mutate_runtime_build_run: int | None = None
        self.add_outside_runtime_build_run: int | None = None
        self.runtime_symlink_run: int | None = None
        self.leave_stale_runtime_root_after_build = False
        self.emit_build_profile = False
        self.skip_prewarm_executables = False
        self.prewarm_foreign_profile = False
        self.fail_prewarm_after_profile = False
        self.prewarm_invalid_profile = False
        self.mutate_tool_run: int | None = None
        self.leave_stale_after_clean = False
        self.mutate_profile_during_report = False
        self.omit_intersection_output = False
        self.fail_lcov_validation = False
        self.tool_is_mutated = False

    def materialize_runtime_build(self, run_number: int) -> None:
        """Имитирует trybuild: run-1 создаёт, later runs byte-identically relink-ят."""

        runtime_root = self.config.profile_directory / "tests" / "trybuild"
        runtime_root.mkdir(parents=True, exist_ok=True)
        executable = runtime_root / "settings-derive-tests"
        alias = runtime_root / "settings-derive-tests-hardlink"
        executable.unlink(missing_ok=True)
        alias.unlink(missing_ok=True)
        executable_bytes = (
            b"mutated executable"
            if self.mutate_runtime_build_run == run_number
            else b"runtime executable"
        )
        executable.write_bytes(executable_bytes)
        executable.chmod(0o755)
        os.link(executable, alias)
        if self.runtime_symlink_run == run_number:
            alias.unlink()
            alias.symlink_to(executable.name)
        if self.add_outside_runtime_build_run == run_number:
            leaked_executable = (
                self.config.profile_directory / "debug" / "deps" / "runtime-leak"
            )
            leaked_executable.write_bytes(b"outside configured runtime root")
            leaked_executable.chmod(0o755)

    def completed(
        self, arguments: tuple[str, ...], stdout: str = ""
    ) -> subprocess.CompletedProcess[str]:
        """Возвращает успешный subprocess-compatible result."""

        return subprocess.CompletedProcess(arguments, 0, stdout=stdout, stderr="")

    def write_merge_inputs(self) -> None:
        """Воспроизводит profraw-list/profdata, создаваемые cargo-llvm-cov report."""

        profiles = sorted(self.config.profile_directory.glob("*.profraw"))
        profile_list = self.config.profile_directory / "fixture-profraw-list"
        profile_list.write_text(
            "".join(f"{profile.resolve()}\n" for profile in profiles), encoding="utf-8"
        )
        (self.config.profile_directory / "fixture.profdata").write_bytes(b"profdata")

    def run_cargo(
        self,
        arguments: tuple[str, ...],
        environment: dict[str, str],
    ) -> subprocess.CompletedProcess[str]:
        """Проецирует только cargo lifecycle, который обязан контролировать runner."""

        if arguments[-1] == "--version":
            version = "0.8.8" if self.tool_is_mutated else "0.8.7"
            return self.completed(arguments, f"cargo-llvm-cov {version}\n")

        if "show-env" in arguments:
            profile_directory = self.config.profile_directory
            return self.completed(
                arguments,
                f"export LLVM_PROFILE_FILE='{profile_directory}/fixture-%p-%16m.profraw'\n"
                "export RUSTC_WRAPPER='fake-wrapper'\n"
                "export CARGO_LLVM_COV='1'\n"
                f"export CARGO_LLVM_COV_TARGET_DIR='{profile_directory}'\n"
                f"export CARGO_LLVM_COV_BUILD_DIR='{profile_directory}'\n",
            )

        if "clean" in arguments:
            self.config.profile_directory.mkdir(parents=True, exist_ok=True)
            if "--profraw-only" in arguments:
                if not self.leave_stale_after_clean:
                    for profile in self.config.profile_directory.glob("*.profraw"):
                        profile.unlink()
            else:
                shutil.rmtree(self.config.profile_directory)
                self.config.profile_directory.mkdir(parents=True)
            return self.completed(arguments)

        if "--no-run" in arguments:
            self.build_count += 1
            executable = self.config.profile_directory / "debug" / "deps" / "fixture-test"
            executable.parent.mkdir(parents=True, exist_ok=True)
            executable.write_bytes(b"instrumented executable")
            executable.chmod(0o755)
            if self.leave_stale_runtime_root_after_build:
                stale_root = self.config.profile_directory / "tests" / "trybuild"
                stale_root.mkdir(parents=True)
                (stale_root / "stale-cache").write_bytes(b"stale")
            if self.emit_build_profile:
                (self.config.profile_directory / "build-script.profraw").write_bytes(
                    b"build-profile"
                )
            return self.completed(arguments)

        if "report" in arguments:
            self.report_count += 1
            self.write_merge_inputs()
            if "--output-path" in arguments:
                output_path = Path(arguments[arguments.index("--output-path") + 1])
                output_path.parent.mkdir(parents=True, exist_ok=True)
                if "--lcov" in arguments:
                    output_path.write_text(
                        "TN:\nSF:crates/example/src/lib.rs\nDA:1,1\nend_of_record\n",
                        encoding="utf-8",
                    )
                else:
                    output_path.write_text(
                        '{"type":"llvm.coverage.json.export","version":"3.1.0","data":[]}',
                        encoding="utf-8",
                    )
            if "--output-dir" in arguments:
                html_directory = (
                    Path(arguments[arguments.index("--output-dir") + 1]) / "html"
                )
                html_directory.mkdir(parents=True, exist_ok=True)
                (html_directory / "index.html").write_text("fixture", encoding="utf-8")
            if self.mutate_profile_during_report and self.report_count == 1:
                next(self.config.profile_directory.glob("*.profraw")).write_bytes(b"changed")
            return self.completed(arguments)

        if "test" in arguments and "--no-run" not in arguments:
            if "--package" in arguments:
                self.prewarm_count += 1
                if not self.skip_prewarm_executables:
                    self.materialize_runtime_build(0)
                template = environment["LLVM_PROFILE_FILE_NAME"]
                profile_name = template.replace("%p", "900").replace(
                    "%16m", "123456_0"
                )
                (self.config.profile_directory / profile_name).write_bytes(
                    b"prewarm-profile"
                )
                if self.prewarm_foreign_profile:
                    (self.config.profile_directory / "foreign.profraw").write_bytes(
                        b"foreign"
                    )
                if self.prewarm_invalid_profile:
                    invalid_name = template.replace("%p-%16m", "garbage")
                    (self.config.profile_directory / invalid_name).write_bytes(b"invalid")
                if self.fail_prewarm_after_profile:
                    raise CoverageRunnerError("fixture prewarm failed after profile")
                return self.completed(arguments)
            self.execution_count += 1
            run_number = self.execution_count
            if self.failed_run == run_number:
                raise CoverageRunnerError(f"fixture run {run_number} failed")
            if self.mutate_source_run == run_number:
                (self.config.repo_root / "src" / "lib.rs").write_text(
                    f"pub fn changed_{run_number}() {{}}\n", encoding="utf-8"
                )
            if self.mutate_build_run == run_number:
                executable = (
                    self.config.profile_directory / "debug" / "deps" / "fixture-test"
                )
                executable.write_bytes(executable.read_bytes() + b" changed")
            if self.mutate_tool_run == run_number:
                self.tool_is_mutated = True
            self.materialize_runtime_build(run_number)
            if self.empty_run != run_number:
                template = environment["LLVM_PROFILE_FILE_NAME"]
                profile_name = template.replace("%p", str(1000 + run_number)).replace(
                    "%16m", f"module{run_number}"
                )
                (self.config.profile_directory / profile_name).write_bytes(
                    f"profile-{run_number}".encode()
                )
            if self.mixed_run == run_number:
                (self.config.profile_directory / "foreign.profraw").write_bytes(b"foreign")
            return self.completed(arguments)

        raise AssertionError(f"unexpected cargo command: {arguments}")

    def run_python(self, arguments: tuple[str, ...]) -> subprocess.CompletedProcess[str]:
        """Имитирует LCOV validator и frozen coordinate CLI contracts."""

        if "validate-lcov" in arguments:
            if self.fail_lcov_validation:
                raise CoverageRunnerError("fixture LCOV corruption")
            return self.completed(arguments)
        if "extract" in arguments:
            output = Path(arguments[arguments.index("--output") + 1])
            run_label = arguments[arguments.index("--run-label") + 1]
            output.write_text(json.dumps({"run_label": run_label}), encoding="utf-8")
            return self.completed(arguments)
        if "intersect" in arguments:
            if not self.omit_intersection_output:
                output = Path(arguments[arguments.index("--output") + 1])
                diagnostics = Path(arguments[arguments.index("--diagnostics") + 1])
                output.write_text('{"stable":true}', encoding="utf-8")
                diagnostics.write_text('{"variable":[]}', encoding="utf-8")
            return self.completed(arguments)
        raise AssertionError(f"unexpected Python command: {arguments}")

    def run(
        self,
        arguments,
        *,
        cwd,
        environment,
        capture_output=False,
    ) -> subprocess.CompletedProcess[str]:
        """Записывает вызов и делегирует exact fake tool owner-у."""

        del cwd, capture_output
        exact_arguments = tuple(arguments)
        exact_environment = dict(environment)
        self.commands.append(
            RecordedCommand(
                exact_arguments,
                exact_environment.get("LLVM_PROFILE_FILE_NAME"),
                exact_environment.get("LLVM_PROFILE_FILE"),
                exact_environment.get("RUST_TEST_THREADS"),
            )
        )
        if exact_arguments[0] == self.config.rustc_command:
            llvm_version = "99.0.0" if self.tool_is_mutated else "22.1.2"
            return self.completed(
                exact_arguments,
                f"rustc 1.96.0\nrelease: 1.96.0\nLLVM version: {llvm_version}\n",
            )
        if exact_arguments[0] == self.config.cargo_command:
            return self.run_cargo(exact_arguments, exact_environment)
        if exact_arguments[0] == self.config.python_command:
            return self.run_python(exact_arguments)
        raise AssertionError(f"unexpected executable: {exact_arguments[0]}")


class StableCoverageRunnerTests(unittest.TestCase):
    """Проверяет публичный runner lifecycle через filesystem-visible fake tools."""

    def setUp(self):
        """Создаёт отдельный git worktree и policy paths для каждого scenario."""

        self.temporary_directory = tempfile.TemporaryDirectory()
        self.repo_root = Path(self.temporary_directory.name)
        (self.repo_root / "src").mkdir()
        (self.repo_root / "src" / "lib.rs").write_text(
            "pub fn covered() {}\n", encoding="utf-8"
        )
        (self.repo_root / ".gitignore").write_text("/target/\n", encoding="utf-8")
        subprocess.run(["git", "init", "-q", self.repo_root], check=True)
        scripts = self.repo_root / "scripts"
        scripts.mkdir()
        for script_name in (
            "coverage_coordinates.py",
            "coverage_stability.py",
            "coverage_metrics.py",
        ):
            (scripts / script_name).write_text("# fixture\n", encoding="utf-8")
        coverage_directory = self.repo_root / "coverage"
        coverage_directory.mkdir()
        (coverage_directory / "policy.json").write_text("{}\n", encoding="utf-8")
        executable_inventory_policy = (
            coverage_directory / "executable-inventory-policy.json"
        )
        executable_inventory_policy.write_text(
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
            profile_directory=self.repo_root / "target" / "llvm-cov-target",
            artifact_directory=self.repo_root / "target" / "coverage" / "stable",
            policy_path=coverage_directory / "policy.json",
            executable_inventory_policy_path=executable_inventory_policy,
            coordinate_extractor=scripts / "coverage_coordinates.py",
            stability_tool=scripts / "coverage_stability.py",
            lcov_validator=scripts / "coverage_metrics.py",
            toolchain="1.96.0",
            cargo_llvm_cov_version="0.8.7",
            llvm_cov_version="22.1.2",
            session_id="fixture-session",
            cargo_command="fake-cargo",
            rustc_command="fake-rustc",
            python_command="fake-python",
        )

    def tearDown(self):
        """Удаляет только testcase-owned temporary worktree."""

        self.temporary_directory.cleanup()

    def new_runner(self) -> tuple[StableCoverageRunner, FakeCoverageExecutor]:
        """Возвращает runner и его observable fake subprocess boundary."""

        executor = FakeCoverageExecutor(self.config)
        return StableCoverageRunner(self.config, executor), executor

    def prepare_previous_artifact(self) -> Path:
        """Создаёт last-known-good tree для проверки transaction rollback."""

        self.config.artifact_directory.mkdir(parents=True)
        marker = self.config.artifact_directory / "previous.txt"
        marker.write_text("accepted", encoding="utf-8")
        return marker

    def seed_merge_metadata(self) -> tuple[Path, Path]:
        """Создаёт exact stale names, которые следующий cargo report заменит."""

        self.config.profile_directory.mkdir(parents=True, exist_ok=True)
        profile_list = self.config.profile_directory / "fixture-profraw-list"
        profdata = self.config.profile_directory / "fixture.profdata"
        profile_list.write_bytes(b"stale profile list\n")
        profdata.write_bytes(b"stale profdata\n")
        return profile_list, profdata

    def assert_no_private_stage(self):
        """Failure не должен оставлять private stage похожим на accepted artifact."""

        stages = list(
            self.config.artifact_directory.parent.glob(".stable.stage-*")
        )
        self.assertEqual(stages, [])

    def test_reports_are_sequential_and_each_run_is_extracted_before_next_execution(self):
        """Full/summary/LCOV одного run завершаются до следующего test execution."""

        runner, executor = self.new_runner()
        runner.run()
        command_kinds = []
        for command in executor.commands:
            if (
                "test" in command.arguments
                and "--workspace" in command.arguments
                and "--no-run" not in command.arguments
            ):
                command_kinds.append("run")
            elif "report" in command.arguments:
                command_kinds.append("html" if "--html" in command.arguments else "report")
            elif "extract" in command.arguments:
                command_kinds.append("extract")
            elif "intersect" in command.arguments:
                command_kinds.append("intersect")
        self.assertEqual(
            command_kinds,
            [
                "run", "report", "report", "report", "extract",
                "run", "report", "report", "report", "extract",
                "run", "report", "report", "report", "extract",
                "intersect", "html",
            ],
        )

    def test_empty_mixed_and_stale_profiles_fail_without_replacing_previous_artifact(self):
        """Ни пустой, ни mixed, ни переживший clean profile set не допускается к report."""

        scenarios = ("empty", "mixed", "stale")
        for scenario in scenarios:
            with self.subTest(scenario=scenario):
                marker = self.prepare_previous_artifact()
                runner, executor = self.new_runner()
                if scenario == "empty":
                    executor.empty_run = 1
                elif scenario == "mixed":
                    executor.mixed_run = 1
                else:
                    executor.leave_stale_after_clean = True
                    self.config.profile_directory.mkdir(parents=True, exist_ok=True)
                    (self.config.profile_directory / "stale.profraw").write_bytes(b"stale")
                with self.assertRaises(CoverageRunnerError):
                    runner.run()
                self.assertEqual(marker.read_text(encoding="utf-8"), "accepted")
                self.assert_no_private_stage()
                shutil.rmtree(self.config.artifact_directory)

    def test_failed_run_source_tool_or_build_mutation_aborts_exact_cohort(self):
        """Execution failure и любое изменение inputs/binaries/tools являются terminal."""

        scenarios = ("failed", "source", "tool", "build")
        for scenario in scenarios:
            with self.subTest(scenario=scenario):
                marker = self.prepare_previous_artifact()
                runner, executor = self.new_runner()
                if scenario == "failed":
                    executor.failed_run = 2
                elif scenario == "source":
                    executor.mutate_source_run = 2
                elif scenario == "tool":
                    executor.mutate_tool_run = 2
                else:
                    executor.mutate_build_run = 2
                with self.assertRaises(CoverageRunnerError):
                    runner.run()
                self.assertEqual(marker.read_text(encoding="utf-8"), "accepted")
                self.assert_no_private_stage()
                # Source mutation откатывается только testcase-ом, не production runner-ом.
                (self.repo_root / "src" / "lib.rs").write_text(
                    "pub fn covered() {}\n", encoding="utf-8"
                )
                shutil.rmtree(self.config.artifact_directory)

    def test_profile_mutation_or_missing_intersection_never_publishes_partial_tree(self):
        """Hash mismatch и broken coordinate CLI сохраняют last-known-good artifacts."""

        for scenario in ("profile", "intersection", "lcov"):
            with self.subTest(scenario=scenario):
                marker = self.prepare_previous_artifact()
                runner, executor = self.new_runner()
                if scenario == "profile":
                    executor.mutate_profile_during_report = True
                elif scenario == "intersection":
                    executor.omit_intersection_output = True
                else:
                    executor.fail_lcov_validation = True
                with self.assertRaises(CoverageRunnerError):
                    runner.run()
                self.assertEqual(marker.read_text(encoding="utf-8"), "accepted")
                self.assert_no_private_stage()
                shutil.rmtree(self.config.artifact_directory)

    def test_success_replaces_previous_tree_and_cleanup_does_not_touch_foreign_sibling(self):
        """Commit tree заменяется целиком, а соседний artifact остаётся нетронутым."""

        old_marker = self.prepare_previous_artifact()
        foreign = self.config.artifact_directory.parent / "foreign-artifact.txt"
        foreign.write_text("owned elsewhere", encoding="utf-8")
        runner, _executor = self.new_runner()
        runner.run()
        self.assertFalse(old_marker.exists())
        self.assertEqual(foreign.read_text(encoding="utf-8"), "owned elsewhere")
        self.assert_no_private_stage()

    def test_success_quarantines_stale_merge_metadata_and_records_both_hash_sets(self):
        """Старые list/profdata не блокируют run и остаются bounded diagnostics."""

        profile_list, profdata = self.seed_merge_metadata()
        original_hashes = {
            profile_list.name: sha256_file(profile_list),
            profdata.name: sha256_file(profdata),
        }
        runner, _executor = self.new_runner()
        runner.run()
        backup = self.config.artifact_directory / "replaced-merge-metadata"
        self.assertEqual((backup / profile_list.name).read_bytes(), b"stale profile list\n")
        self.assertEqual((backup / profdata.name).read_bytes(), b"stale profdata\n")
        self.assertNotEqual(profile_list.read_bytes(), b"stale profile list\n")
        self.assertNotEqual(profdata.read_bytes(), b"stale profdata\n")
        manifest = json.loads(
            (self.config.artifact_directory / "cohort-manifest.json").read_text()
        )["merge_metadata"]
        self.assertEqual(manifest["backup_artifact"], "replaced-merge-metadata")
        self.assertEqual(
            {entry["path"]: entry["sha256"] for entry in manifest["preexisting"]},
            original_hashes,
        )
        self.assertEqual(
            {entry["path"] for entry in manifest["authoritative"]},
            {"fixture-profraw-list", "fixture.profdata"},
        )

    def test_failure_after_report_restores_original_merge_metadata(self):
        """LCOV failure удаляет replacements и возвращает originals byte-for-byte."""

        profile_list, profdata = self.seed_merge_metadata()
        list_stat = profile_list.stat()
        profdata_stat = profdata.stat()
        runner, executor = self.new_runner()
        executor.fail_lcov_validation = True
        with self.assertRaises(CoverageRunnerError):
            runner.run()
        self.assertEqual(profile_list.read_bytes(), b"stale profile list\n")
        self.assertEqual(profdata.read_bytes(), b"stale profdata\n")
        self.assertEqual(profile_list.stat().st_mode, list_stat.st_mode)
        self.assertEqual(profdata.stat().st_mode, profdata_stat.st_mode)
        self.assertEqual(
            {path.name for path in self.config.profile_directory.glob("*-profraw-list")},
            {"fixture-profraw-list"},
        )
        self.assertEqual(
            {path.name for path in self.config.profile_directory.glob("*.profdata")},
            {"fixture.profdata"},
        )
        self.assert_no_private_stage()

    def test_run_intersection_and_publication_failures_restore_original_metadata(self):
        """Каждая последующая lifecycle boundary использует один rollback owner."""

        for scenario in ("run", "intersection", "prepare", "publication"):
            with self.subTest(scenario=scenario):
                profile_list, profdata = self.seed_merge_metadata()
                marker = self.prepare_previous_artifact()
                runner, executor = self.new_runner()
                if scenario == "run":
                    executor.failed_run = 2
                elif scenario == "intersection":
                    executor.omit_intersection_output = True
                publication_patch = (
                    mock.patch(
                        "coverage_runner.publish_artifacts",
                        side_effect=OSError("fixture publication failure"),
                    )
                    if scenario == "publication"
                    else contextlib.nullcontext()
                )
                prepare_patch = (
                    mock.patch.object(
                        MergeMetadataTransaction,
                        "prepare_publication",
                        side_effect=CoverageRunnerError(
                            "fixture pre-publication validation failure"
                        ),
                    )
                    if scenario == "prepare"
                    else contextlib.nullcontext()
                )
                with prepare_patch, publication_patch:
                    with self.assertRaises((CoverageRunnerError, OSError)):
                        runner.run()
                self.assertEqual(profile_list.read_bytes(), b"stale profile list\n")
                self.assertEqual(profdata.read_bytes(), b"stale profdata\n")
                self.assertEqual(marker.read_text(encoding="utf-8"), "accepted")
                self.assert_no_private_stage()
                shutil.rmtree(self.config.artifact_directory)

    def test_repeated_session_retains_only_one_replaced_metadata_generation(self):
        """Следующий cohort заменяет diagnostic backup без unbounded growth."""

        profile_list, profdata = self.seed_merge_metadata()
        first_runner, _first_executor = self.new_runner()
        first_runner.run()
        first_current = {
            profile_list.name: profile_list.read_bytes(),
            profdata.name: profdata.read_bytes(),
        }
        second_runner, _second_executor = self.new_runner()
        second_runner.run()
        backup = self.config.artifact_directory / "replaced-merge-metadata"
        self.assertEqual(
            {path.name: path.read_bytes() for path in backup.iterdir()},
            first_current,
        )
        self.assertEqual(
            len(list(self.config.artifact_directory.rglob("replaced-merge-metadata"))),
            1,
        )
        self.assert_no_private_stage()

    def test_publication_rolls_back_when_final_tree_swap_fails(self):
        """Ошибка второго rename возвращает previous artifact и не трогает foreign sibling."""

        marker = self.prepare_previous_artifact()
        stage = self.config.artifact_directory.parent / ".manual-stage"
        stage.mkdir()
        (stage / "new.txt").write_text("new", encoding="utf-8")
        real_replace = os.replace
        replace_count = 0

        def fail_second_replace(source, destination):
            nonlocal replace_count
            replace_count += 1
            if replace_count == 2:
                raise OSError("fixture atomic swap failure")
            return real_replace(source, destination)

        with mock.patch(
            "coverage_runner_support.os.replace", side_effect=fail_second_replace
        ):
            with self.assertRaises(OSError):
                publish_artifacts(stage, self.config.artifact_directory, "rollback")
        self.assertEqual(marker.read_text(encoding="utf-8"), "accepted")
        self.assertTrue(stage.is_dir())


if __name__ == "__main__":
    unittest.main()

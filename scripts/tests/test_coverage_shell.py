#!/usr/bin/env python3
"""Subprocess vertical для stable-coordinate coverage.sh orchestration."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]


FAKE_PYTHON_SOURCE = r'''
import json
import os
import sys
from pathlib import Path


repository_root = Path(os.environ["FAKE_COVERAGE_REPO"])
command_log = Path(os.environ["FAKE_COVERAGE_COMMAND_LOG"])
scenario = os.environ.get("FAKE_COVERAGE_SCENARIO", "pass")
arguments = sys.argv[1:]
script_name = Path(arguments[0]).name


with command_log.open("a", encoding="utf-8") as log_file:
    log_file.write(json.dumps({"script": script_name, "arguments": arguments[1:]}) + "\n")


def option(name):
    return arguments[arguments.index(name) + 1]


def read_json(path):
    try:
        return json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def write_json(path, document):
    destination = Path(path)
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_text(json.dumps(document), encoding="utf-8")


if script_name == "coverage_runner.py":
    if scenario in {"runner_failure", "lcov_corruption"}:
        print("fixture runner rejected LCOV/profile", file=sys.stderr)
        raise SystemExit(2)
    artifact_root = Path(option("--artifact-directory"))
    (artifact_root / "legacy").mkdir(parents=True, exist_ok=True)
    (artifact_root / "html").mkdir(parents=True, exist_ok=True)
    for run_number in (1, 2, 3):
        if scenario == "missing_third" and run_number == 3:
            continue
        write_json(
            artifact_root / f"run-{run_number}.json",
            {"schema_version": 1, "kind": "run", "run": run_number},
        )
    if scenario == "malformed_third":
        write_json(
            artifact_root / "run-3.json",
            {"schema_version": 1, "kind": "malformed-run", "run": 3},
        )
    write_json(artifact_root / "cohort.json", {"schema_version": 1, "kind": "cohort"})
    write_json(artifact_root / "variable.json", {"schema_version": 1, "variables": []})
    write_json(artifact_root / "cohort-manifest.json", {"schema_version": 1, "run_count": 3})
    write_json(artifact_root / "legacy" / "run-3-summary.json", {"fixture": "llvm-summary"})
    (artifact_root / "legacy" / "run-3.lcov").write_text("DA:1,1\n", encoding="utf-8")
    (artifact_root / "html" / "index.html").write_text("fixture", encoding="utf-8")
    raise SystemExit(0)


if script_name == "coverage_stability.py":
    command = arguments[1]
    if command == "validate":
        document = read_json(option("--input"))
        kind = option("--kind")
        valid = False
        if kind == "baseline":
            valid = (
                document is not None
                and document.get("schema_version") == 2
                and document.get("valid", True)
            )
        elif kind == "measurement-exceptions":
            valid = document is not None and document.get("schema_version") == 1
        elif kind == "run":
            valid = document is not None and document.get("kind") == "run"
        elif kind == "cohort":
            valid = document is not None and document.get("kind") == "cohort"
        raise SystemExit(0 if valid else 2)
    if command == "check":
        output = option("--output")
        if scenario == "stable_regression":
            write_json(output, {"status": "fail", "regressions": ["fixture-coordinate"]})
            raise SystemExit(1)
        if scenario == "malformed_check":
            print("fixture malformed stable check", file=sys.stderr)
            raise SystemExit(2)
        write_json(output, {"status": "pass", "regressions": []})
        raise SystemExit(0)
    if command == "bootstrap":
        write_json(option("--output"), {"schema_version": 2, "valid": True, "fixture": "proposal"})
        raise SystemExit(0)
    raise SystemExit(2)


if script_name == "coverage_metrics.py":
    command = arguments[1]
    if command == "validate-baseline":
        baseline = read_json(repository_root / "coverage" / "baseline.json")
        exceptions = read_json(repository_root / "coverage" / "exceptions.json")
        valid = (
            baseline is not None
            and baseline.get("schema_version") == 1
            and exceptions is not None
            and exceptions.get("schema_version") == 1
        )
        raise SystemExit(0 if valid else 2)
    if command == "generate":
        output = Path(option("--output"))
        output.parent.mkdir(parents=True, exist_ok=True)
        if scenario == "legacy_corruption":
            output.write_text("partial", encoding="utf-8")
            raise SystemExit(2)
        marker = "decreased-report-only" if scenario == "legacy_decrease" else "diagnostic"
        write_json(output, {"schema_version": 1, "marker": marker})
        raise SystemExit(0)
    raise SystemExit(2)


raise SystemExit(2)
'''


class CoverageShellTests(unittest.TestCase):
    """Проверяет observable shell exit/artifact/argv contract без LLVM rebuild."""

    def setUp(self):
        # Пробелы в каждом значимом пути превращают quoting в обязательную часть fixture-а.
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="coverage shell ")
        self.temporary_root = Path(self.temporary_directory.name)
        self.repository_root = self.temporary_root / "repository with spaces"
        self.scripts_directory = self.repository_root / "scripts"
        self.coverage_directory = self.repository_root / "coverage"
        self.fake_bin = self.temporary_root / "fake tools"
        self.command_log = self.temporary_root / "command log.jsonl"
        self.scripts_directory.mkdir(parents=True)
        self.coverage_directory.mkdir()
        self.fake_bin.mkdir()
        # Тест исполняет production shell verbatim из изолированного fake worktree.
        shutil.copy2(REPO_ROOT / "scripts" / "coverage.sh", self.scripts_directory / "coverage.sh")
        self.write_json(self.coverage_directory / "policy.json", {"schema_version": 1})
        self.write_json(self.coverage_directory / "exceptions.json", {"schema_version": 1})
        self.write_v2_inputs()
        self.write_fake_tools()

    def tearDown(self):
        self.temporary_directory.cleanup()

    @staticmethod
    def write_json(path: Path, document: object) -> None:
        """Пишет маленький fixture document в уже изолированный test root."""

        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(document), encoding="utf-8")

    def write_v2_inputs(self) -> None:
        """Устанавливает минимальные документы, которые fake validators считают v2."""

        self.write_json(
            self.coverage_directory / "baseline.json",
            {"schema_version": 2, "valid": True},
        )
        self.write_json(
            self.coverage_directory / "measurement-exceptions.json",
            {"schema_version": 1, "exceptions": []},
        )

    def write_v1_inputs(self) -> None:
        """Возвращает migration fixture к legacy v1 без stable exceptions."""

        self.write_json(self.coverage_directory / "baseline.json", {"schema_version": 1})
        (self.coverage_directory / "measurement-exceptions.json").unlink(missing_ok=True)

    def write_executable(self, name: str, source: str) -> None:
        """Создаёт ровно один fake executable с absolute Python interpreter."""

        executable = self.fake_bin / name
        executable.write_text(
            f"#!{sys.executable}\n{textwrap.dedent(source).lstrip()}",
            encoding="utf-8",
        )
        executable.chmod(0o755)

    def write_fake_tools(self) -> None:
        """Подменяет только process boundaries shell-а, сохраняя exact argv."""

        self.write_executable(
            "rustc",
            """
            import sys
            print("rustc 1.96.0 (fixture)")
            """,
        )
        self.write_executable(
            "cargo",
            """
            import sys
            print("cargo-llvm-cov 0.8.7")
            """,
        )
        self.write_executable("python3", FAKE_PYTHON_SOURCE)

    def run_shell(self, command: str, *arguments: str, scenario: str = "pass"):
        """Запускает public launcher и возвращает завершённый observable process."""

        environment = dict(os.environ)
        environment["PATH"] = f"{self.fake_bin}{os.pathsep}{environment['PATH']}"
        environment["FAKE_COVERAGE_REPO"] = str(self.repository_root)
        environment["FAKE_COVERAGE_COMMAND_LOG"] = str(self.command_log)
        environment["FAKE_COVERAGE_SCENARIO"] = scenario
        environment.pop("RUST_TEST_THREADS", None)
        return subprocess.run(
            [str(self.scripts_directory / "coverage.sh"), command, *arguments],
            cwd=self.temporary_root,
            env=environment,
            text=True,
            capture_output=True,
            check=False,
        )

    def recorded_commands(self) -> list[dict[str, object]]:
        """Читает argv records без shell-token reconstruction."""

        if not self.command_log.exists():
            return []
        return [
            json.loads(line)
            for line in self.command_log.read_text(encoding="utf-8").splitlines()
        ]

    def commands_for(self, script_name: str) -> list[list[str]]:
        """Возвращает argument vectors выбранного fake Python owner-а."""

        return [
            entry["arguments"]
            for entry in self.recorded_commands()
            if entry["script"] == script_name
        ]

    def test_v2_check_runs_one_runner_validates_exact_three_states_and_checks_stable(self):
        """Stable cohort является единственным blocking ratchet после report-only summary."""

        result = self.run_shell("check")
        self.assertEqual(result.returncode, 0, result.stderr)
        runner_commands = self.commands_for("coverage_runner.py")
        self.assertEqual(len(runner_commands), 1)
        runner_arguments = runner_commands[0]
        self.assertEqual(
            runner_arguments[runner_arguments.index("--repo-root") + 1],
            str(self.repository_root),
        )
        stability_commands = self.commands_for("coverage_stability.py")
        validated_runs = [
            Path(command[command.index("--input") + 1]).name
            for command in stability_commands
            if command[:3] == ["validate", "--kind", "run"]
        ]
        self.assertEqual(validated_runs, ["run-1.json", "run-2.json", "run-3.json"])
        self.assertEqual(sum(command[0] == "check" for command in stability_commands), 1)
        metrics_commands = self.commands_for("coverage_metrics.py")
        self.assertEqual([command[0] for command in metrics_commands], ["generate"])
        self.assertTrue((self.repository_root / "target/coverage/current-summary.json").is_file())
        self.assertIn("report-only diagnostics", result.stdout)

    def test_stable_regression_preserves_semantic_exit_one_and_writes_report(self):
        """Legacy success не может скрыть потерю stable source coordinate."""

        result = self.run_shell("check", scenario="stable_regression")
        self.assertEqual(result.returncode, 1, result.stderr)
        report = json.loads(
            (self.repository_root / "target/coverage/stable/check.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(report["status"], "fail")
        self.assertIn("report-only diagnostics", result.stdout)

    def test_missing_or_malformed_third_run_fails_before_summary_and_check(self):
        """Shell не допускает two-run fallback даже при ошибочном success runner-а."""

        for scenario in ("missing_third", "malformed_third"):
            with self.subTest(scenario=scenario):
                self.command_log.unlink(missing_ok=True)
                result = self.run_shell("check", scenario=scenario)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(self.commands_for("coverage_metrics.py"))
                self.assertFalse(
                    any(
                        command[0] == "check"
                        for command in self.commands_for("coverage_stability.py")
                    )
                )

    def test_legacy_ratio_decrease_is_published_and_labelled_without_blocking(self):
        """Diagnostic aggregate drift не возвращает старый nondeterministic ratchet."""

        result = self.run_shell("check", scenario="legacy_decrease")
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads(
            (self.repository_root / "target/coverage/current-summary.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(summary["marker"], "decreased-report-only")
        self.assertIn("report-only diagnostics", result.stdout)

    def test_v1_check_fails_before_runner_with_explicit_bootstrap_instruction(self):
        """Migration никогда не происходит как side effect public check."""

        self.write_v1_inputs()
        baseline_before = (self.coverage_directory / "baseline.json").read_bytes()
        result = self.run_shell("check")
        self.assertEqual(result.returncode, 2)
        self.assertIn("coverage.sh bootstrap", result.stderr)
        self.assertFalse(self.commands_for("coverage_runner.py"))
        self.assertEqual((self.coverage_directory / "baseline.json").read_bytes(), baseline_before)

    def test_report_remains_available_during_v1_migration_without_any_ratchet(self):
        """Diagnostic measurement не требует baseline и не принимает blocking решение."""

        self.write_v1_inputs()
        result = self.run_shell("report")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.commands_for("coverage_runner.py")), 1)
        self.assertFalse(
            any(
                command[0] == "check"
                for command in self.commands_for("coverage_stability.py")
            )
        )
        self.assertEqual(
            [command[0] for command in self.commands_for("coverage_metrics.py")],
            ["generate"],
        )

    def test_explicit_bootstrap_quotes_output_and_never_rewrites_tracked_baseline(self):
        """Admin получает reviewable v2 proposal по отдельному exact path."""

        self.write_v1_inputs()
        baseline_before = (self.coverage_directory / "baseline.json").read_bytes()
        proposal = self.temporary_root / "review output" / "baseline proposal.json"
        result = self.run_shell("bootstrap", str(proposal))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(proposal.is_file())
        self.assertEqual((self.coverage_directory / "baseline.json").read_bytes(), baseline_before)
        bootstrap_commands = [
            command
            for command in self.commands_for("coverage_stability.py")
            if command[0] == "bootstrap"
        ]
        self.assertEqual(len(bootstrap_commands), 1)
        self.assertEqual(
            bootstrap_commands[0][bootstrap_commands[0].index("--output") + 1],
            str(proposal),
        )
        self.assertFalse(
            any(
                command[0] == "check"
                for command in self.commands_for("coverage_stability.py")
            )
        )

    def test_bootstrap_rejects_tracked_output_before_runner(self):
        """Явный аргумент всё равно не разрешает shell перезаписать versioned policy."""

        self.write_v1_inputs()
        unsafe_output = self.coverage_directory / "baseline.json"
        result = self.run_shell("bootstrap", str(unsafe_output))
        self.assertEqual(result.returncode, 2)
        self.assertIn("только под target", result.stderr)
        self.assertFalse(self.commands_for("coverage_runner.py"))

    def test_bootstrap_rejects_empty_output_before_runner(self):
        """Пустой argv token не превращается в current working directory."""

        self.write_v1_inputs()
        result = self.run_shell("bootstrap", "")
        self.assertEqual(result.returncode, 2)
        self.assertIn("не может быть пустым", result.stderr)
        self.assertFalse(self.commands_for("coverage_runner.py"))

    def test_lcov_runner_failure_blocks_and_keeps_previous_summary(self):
        """LCOV corruption остаётся blocking integrity failure до stable check."""

        current_summary = self.repository_root / "target/coverage/current-summary.json"
        self.write_json(current_summary, {"marker": "previous-complete"})
        result = self.run_shell("check", scenario="lcov_corruption")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(current_summary.read_text())["marker"], "previous-complete")
        self.assertFalse(
            any(
                command[0] == "check"
                for command in self.commands_for("coverage_stability.py")
            )
        )

    def test_legacy_generation_failure_never_publishes_partial_current_summary(self):
        """Private temp удаляется, а последний complete diagnostic остаётся целым."""

        current_summary = self.repository_root / "target/coverage/current-summary.json"
        self.write_json(current_summary, {"marker": "previous-complete"})
        result = self.run_shell("check", scenario="legacy_corruption")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(current_summary.read_text())["marker"], "previous-complete")
        self.assertFalse(list(current_summary.parent.glob(".current-summary-*.json")))
        self.assertFalse(
            any(
                command[0] == "check"
                for command in self.commands_for("coverage_stability.py")
            )
        )

    def test_malformed_stable_check_returns_two_without_legacy_fallback(self):
        """Malformed gate не вызывает legacy coverage_metrics.py check."""

        result = self.run_shell("check", scenario="malformed_check")
        self.assertEqual(result.returncode, 2)
        self.assertNotIn(
            "check",
            [command[0] for command in self.commands_for("coverage_metrics.py")],
        )


if __name__ == "__main__":
    unittest.main()

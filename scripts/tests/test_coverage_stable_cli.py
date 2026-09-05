"""Subprocess vertical для observable stable coverage CLI lifecycle."""

from __future__ import annotations

import copy
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = REPO_ROOT / "scripts/tests/fixtures/coverage_stable"
sys.path.insert(0, str(FIXTURE_ROOT))

from fixture_factory import build_report  # noqa: E402
sys.path.insert(0, str(REPO_ROOT / "scripts"))
from test_execution_scope import LOCAL_HARDWARE_TESTS


class StableCoverageCliTests(unittest.TestCase):
    def setUp(self):
        # Fixture моделирует локальный CLI независимо от host workflow environment.
        self.cli_environment = dict(os.environ)
        self.cli_environment.pop("GITHUB_ACTIONS", None)
        self.cli_environment["FASTIPLAYER_TEST_SCOPE"] = "local"
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="coverage-stable-cli-")
        self.workspace = Path(self.temporary_directory.name)
        (self.workspace / "Cargo.toml").write_text("[workspace]\nmembers=[]\n", encoding="utf-8")
        self.policy_path = self.copy_fixture("policy.json")
        self.profile_path = self.copy_fixture("profile.json")
        self.legacy_baseline_path = self.copy_fixture("legacy_baseline_v1.json")
        self.legacy_exceptions_path = self.copy_fixture("legacy_exceptions_v1.json")
        self.measurement_exceptions_path = self.copy_fixture(
            "measurement_exceptions_empty.json"
        )

    def tearDown(self):
        self.temporary_directory.cleanup()

    def copy_fixture(self, name: str) -> Path:
        destination = self.workspace / name
        destination.write_text((FIXTURE_ROOT / name).read_text(encoding="utf-8"), encoding="utf-8")
        return destination

    def write_json(self, name: str, document) -> Path:
        destination = self.workspace / name
        destination.write_text(json.dumps(document), encoding="utf-8")
        return destination

    def run_cli(self, script: str, *arguments: str):
        return subprocess.run(
            [sys.executable, str(REPO_ROOT / "scripts" / script), *map(str, arguments)],
            cwd=REPO_ROOT,
            text=True,
            capture_output=True,
            check=False,
            env=self.cli_environment,
        )

    def extract(self, raw_path: Path, label: str, output: Path):
        return self.run_cli(
            "coverage_coordinates.py",
            "extract",
            "--policy",
            self.policy_path,
            "--repo-root",
            self.workspace,
            "--input",
            raw_path,
            "--profile-manifest",
            self.profile_path,
            "--run-label",
            label,
            "--output",
            output,
        )

    def intersect(self, states: list[Path], cohort: Path, diagnostics: Path):
        arguments = [
            "intersect",
            "--policy",
            str(self.policy_path),
        ]
        for state in states:
            arguments.extend(("--run", str(state)))
        arguments.extend(("--output", str(cohort), "--diagnostics", str(diagnostics)))
        return self.run_cli("coverage_stability.py", *arguments)

    def valid_artifacts(self):
        state_paths = []
        for run in (1, 2, 3):
            raw_path = self.write_json(
                f"valid-raw-{run}.json", build_report(self.workspace, run=1)
            )
            state_path = self.workspace / f"valid-state-{run}.json"
            result = self.extract(raw_path, f"run-{run}", state_path)
            self.assertEqual(result.returncode, 0, result.stderr)
            state_paths.append(state_path)
        cohort_path = self.workspace / "valid-cohort.json"
        diagnostics_path = self.workspace / "valid-variable.json"
        result = self.intersect(state_paths, cohort_path, diagnostics_path)
        self.assertEqual(result.returncode, 0, result.stderr)
        baseline_path = self.workspace / "valid-baseline.json"
        result = self.run_cli(
            "coverage_stability.py",
            "bootstrap",
            "--cohort",
            cohort_path,
            "--legacy-baseline",
            self.legacy_baseline_path,
            "--legacy-exceptions",
            self.legacy_exceptions_path,
            "--output",
            baseline_path,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return state_paths, cohort_path, diagnostics_path, baseline_path

    def test_hosted_check_requires_matching_scope_and_exact_cohort_bytes(self):
        _, cohort, _, baseline = self.valid_artifacts()
        self.cli_environment.update(GITHUB_ACTIONS="true", FASTIPLAYER_TEST_SCOPE="hosted")
        arguments = ("check", "--baseline", baseline, "--cohort", cohort,
                     "--measurement-exceptions", self.measurement_exceptions_path,
                     "--output", self.workspace / "hosted-check.json")
        self.assertEqual(self.run_cli("coverage_stability.py", *arguments).returncode, 2)
        content = cohort.read_bytes()
        manifest = {
            "execution_scope": {"name": "hosted", "local_hardware_tests": list(LOCAL_HARDWARE_TESTS)},
            "artifacts": [{"path": cohort.name, "size": len(content), "sha256": hashlib.sha256(content).hexdigest()}],
        }
        self.write_json("cohort-manifest.json", manifest)
        result = self.run_cli("coverage_stability.py", *arguments)
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest["artifacts"][0]["sha256"] = "0" * 64
        self.write_json("cohort-manifest.json", manifest)
        self.assertEqual(self.run_cli("coverage_stability.py", *arguments).returncode, 2)
        manifest["execution_scope"]["name"] = "local"
        self.write_json("cohort-manifest.json", manifest)
        self.assertEqual(self.run_cli("coverage_stability.py", *arguments).returncode, 2)

    def test_vertical_pass_variable_regression_and_corruption_exit_contract(self):
        state_paths = []
        for run in (1, 2, 3):
            raw_path = self.write_json(f"raw-{run}.json", build_report(self.workspace, run=run))
            state_path = self.workspace / f"state-{run}.json"
            result = self.extract(raw_path, f"run-{run}", state_path)
            self.assertEqual(result.returncode, 0, result.stderr)
            state_paths.append(state_path)
        cohort_path = self.workspace / "cohort.json"
        diagnostics_path = self.workspace / "variable.json"
        intersect = self.intersect(state_paths, cohort_path, diagnostics_path)
        self.assertEqual(intersect.returncode, 0, intersect.stderr)
        diagnostics = json.loads(diagnostics_path.read_text(encoding="utf-8"))
        self.assertGreater(len(diagnostics["variables"]["workspace"]["lines"]), 0)

        baseline_path = self.workspace / "baseline-v2.json"
        bootstrap = self.run_cli(
            "coverage_stability.py",
            "bootstrap",
            "--cohort",
            cohort_path,
            "--legacy-baseline",
            self.legacy_baseline_path,
            "--legacy-exceptions",
            self.legacy_exceptions_path,
            "--output",
            baseline_path,
        )
        self.assertEqual(bootstrap.returncode, 0, bootstrap.stderr)
        passing_report = self.workspace / "check-pass.json"
        passing = self.run_cli(
            "coverage_stability.py",
            "check",
            "--baseline",
            baseline_path,
            "--cohort",
            cohort_path,
            "--measurement-exceptions",
            self.measurement_exceptions_path,
            "--output",
            passing_report,
        )
        self.assertEqual(passing.returncode, 0, passing.stderr)
        self.assertEqual(json.loads(passing_report.read_text())["status"], "pass")

        regression_states = []
        for run in (1, 2, 3):
            report = build_report(self.workspace, run=run)
            report["data"][0]["files"][0]["segments"][0][2] = 0
            report["data"][0]["files"][0]["summary"]["lines"]["covered"] -= 1
            report["data"][0]["totals"]["lines"]["covered"] -= 1
            raw_path = self.write_json(f"regression-raw-{run}.json", report)
            state_path = self.workspace / f"regression-state-{run}.json"
            result = self.extract(raw_path, f"regression-{run}", state_path)
            self.assertEqual(result.returncode, 0, result.stderr)
            regression_states.append(state_path)
        regression_cohort = self.workspace / "regression-cohort.json"
        result = self.intersect(
            regression_states,
            regression_cohort,
            self.workspace / "regression-variable.json",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        failing_report = self.workspace / "check-fail.json"
        failing = self.run_cli(
            "coverage_stability.py",
            "check",
            "--baseline",
            baseline_path,
            "--cohort",
            regression_cohort,
            "--measurement-exceptions",
            self.measurement_exceptions_path,
            "--output",
            failing_report,
        )
        self.assertEqual(failing.returncode, 1, failing.stderr)
        self.assertEqual(json.loads(failing_report.read_text())["status"], "fail")

        corrupt_report = build_report(self.workspace)
        corrupt_report["data"][0]["files"][0]["segments"][0][2] = True
        corrupt_raw = self.write_json("corrupt.json", corrupt_report)
        protected_output = self.workspace / "protected-output.json"
        protected_output.write_text("do-not-replace\n", encoding="utf-8")
        corrupt = self.extract(corrupt_raw, "corrupt", protected_output)
        self.assertEqual(corrupt.returncode, 2)
        self.assertIn("должен быть integer", corrupt.stderr)
        self.assertEqual(protected_output.read_text(encoding="utf-8"), "do-not-replace\n")

    def test_help_documents_frozen_subcommands_and_required_inputs(self):
        coordinate_help = self.run_cli("coverage_coordinates.py", "extract", "--help")
        self.assertEqual(coordinate_help.returncode, 0)
        for option in (
            "--policy",
            "--repo-root",
            "--input",
            "--profile-manifest",
            "--run-label",
            "--output",
        ):
            self.assertIn(option, coordinate_help.stdout)
        stability_help = self.run_cli("coverage_stability.py", "--help")
        self.assertEqual(stability_help.returncode, 0)
        for command in ("intersect", "validate", "bootstrap", "check"):
            self.assertIn(command, stability_help.stdout)

    def test_validate_four_frozen_kinds_without_hidden_policy_argument(self):
        states, cohort, _, baseline = self.valid_artifacts()
        documents = {
            "run": states[0],
            "cohort": cohort,
            "baseline": baseline,
            "measurement-exceptions": self.measurement_exceptions_path,
        }
        for kind, input_path in documents.items():
            result = self.run_cli(
                "coverage_stability.py",
                "validate",
                "--kind",
                kind,
                "--input",
                input_path,
            )
            self.assertEqual(result.returncode, 0, result.stderr)

        corrupted_state = json.loads(states[0].read_text())
        corrupted_state["state_hash"] = "sha256:" + "0" * 64
        corrupt_path = self.write_json("bad-state.json", corrupted_state)
        result = self.run_cli(
            "coverage_stability.py",
            "validate",
            "--kind",
            "run",
            "--input",
            corrupt_path,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("SHA-256", result.stderr)

    def test_duplicate_keys_and_nonfinite_json_preserve_extract_output(self):
        protected_output = self.workspace / "protected-state.json"
        protected_output.write_text("old state\n", encoding="utf-8")
        malformed_documents = {
            "duplicate.json": '{"type":"first","type":"second"}',
            "nan.json": '{"type":NaN}',
            "infinity.json": '{"type":Infinity}',
        }
        for filename, rendered in malformed_documents.items():
            raw_path = self.workspace / filename
            raw_path.write_text(rendered, encoding="utf-8")
            result = self.extract(raw_path, "run-1", protected_output)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(
                protected_output.read_text(encoding="utf-8"), "old state\n"
            )

    def test_malformed_admin_inputs_never_replace_existing_outputs(self):
        states, cohort, diagnostics, baseline = self.valid_artifacts()
        protected_cohort = self.workspace / "protected-cohort.json"
        protected_diagnostics = self.workspace / "protected-variable.json"
        protected_cohort.write_text("old cohort\n", encoding="utf-8")
        protected_diagnostics.write_text("old diagnostics\n", encoding="utf-8")
        bad_state = self.workspace / "malformed-state.json"
        bad_state.write_text('{"state_hash":"first","state_hash":"second"}', encoding="utf-8")
        result = self.intersect(
            [states[0], states[1], bad_state],
            protected_cohort,
            protected_diagnostics,
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(protected_cohort.read_text(), "old cohort\n")
        self.assertEqual(protected_diagnostics.read_text(), "old diagnostics\n")

        same_output = self.workspace / "same-output.json"
        same_output.write_text("old shared artifact\n", encoding="utf-8")
        result = self.intersect(states, same_output, same_output)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(same_output.read_text(), "old shared artifact\n")

        protected_baseline = self.workspace / "protected-baseline.json"
        protected_baseline.write_text("old baseline\n", encoding="utf-8")
        malformed_legacy = self.workspace / "malformed-legacy.json"
        malformed_legacy.write_text('{"schema_version":NaN}', encoding="utf-8")
        result = self.run_cli(
            "coverage_stability.py",
            "bootstrap",
            "--cohort",
            cohort,
            "--legacy-baseline",
            malformed_legacy,
            "--legacy-exceptions",
            self.legacy_exceptions_path,
            "--output",
            protected_baseline,
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(protected_baseline.read_text(), "old baseline\n")

        protected_check = self.workspace / "protected-check.json"
        protected_check.write_text("old check\n", encoding="utf-8")
        malformed_exceptions = self.workspace / "malformed-measurement.json"
        malformed_exceptions.write_text('{"schema_version":1,"schema_version":1}', encoding="utf-8")
        result = self.run_cli(
            "coverage_stability.py",
            "check",
            "--baseline",
            baseline,
            "--cohort",
            cohort,
            "--measurement-exceptions",
            malformed_exceptions,
            "--output",
            protected_check,
        )
        self.assertEqual(result.returncode, 2)
        self.assertEqual(protected_check.read_text(), "old check\n")


if __name__ == "__main__":
    unittest.main()

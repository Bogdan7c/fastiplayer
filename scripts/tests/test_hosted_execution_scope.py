"""Проверяет конечный hosted verdict и фактическое исключение hardware tests."""

from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
sys.path.insert(0, str(ROOT / "scripts/tests/fixtures/coverage_stable"))

import coverage_coordinates as coordinates
import coverage_stability as stability
from fixture_factory import build_report
from test_execution_scope import LOCAL_HARDWARE_TESTS, TestExecutionScope, execution_scope, scoped_test_command


class ExecutionScopeTests(unittest.TestCase):
    def fixture(self, name):
        return json.loads((ROOT / "scripts/tests/fixtures/coverage_stable" / name).read_text())

    def qualification(self, root, run, *, lose_software=False):
        policy = self.fixture("policy.json")
        policy["informational_crates"] = ["video-vaapi"]
        report = build_report(root, run=run, add_file=True)
        rendered = json.dumps(report).replace(
            "crates/alpha/src/lib.rs", "crates/video-vaapi/src/gbm_allocator.rs"
        ).replace("crates/shell/src/lib.rs", "crates/alpha/src/lib.rs")
        report = json.loads(rendered)
        if lose_software:
            extra = report["data"][0]["files"][-1]
            extra["segments"][0][2] = 0
            extra["summary"]["lines"]["covered"] = 0
            report["data"][0]["totals"]["lines"]["covered"] -= 1
        states = [coordinates.extract_run_state(
            report, policy, self.fixture("profile.json"), root, f"run-{index}"
        ) for index in range(1, 4)]
        return stability.intersect_runs(policy, states)[0]

    def test_hosted_verdict_retains_hardware_diagnostics_and_blocks_software_loss(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "Cargo.toml").write_text("[workspace]\nmembers=[]\n")
            cohort = self.qualification(root, 1)
            legacy = self.fixture("legacy_baseline_v1.json")
            legacy["informational_crates"]["video-vaapi"] = legacy["informational_crates"].pop("shell")
            baseline = stability.bootstrap_baseline(cohort, legacy, self.fixture("legacy_exceptions_v1.json"))
            exceptions = self.fixture("measurement_exceptions_empty.json")
            current = self.qualification(root, 2)
            original = copy.deepcopy(current)
            local_pass, _ = stability.check_baseline(baseline, current, exceptions, allow_universe_update=False)
            hosted_pass, report = stability.check_baseline(
                baseline, current, exceptions, allow_universe_update=False,
                test_scope=TestExecutionScope.HOSTED,
            )
            self.assertFalse(local_pass)
            self.assertTrue(hosted_pass, report)
            self.assertTrue(report["execution_scope"]["local_hardware_losses"])
            self.assertEqual(current, original)
            software_pass, software_report = stability.check_baseline(
                baseline, self.qualification(root, 2, lose_software=True), exceptions,
                allow_universe_update=False, test_scope=TestExecutionScope.HOSTED,
            )
            self.assertFalse(software_pass)
            self.assertTrue(software_report["regressions"])

    def test_scopes_preserve_commands_and_reject_unknown_configuration(self):
        command = ["cargo", "test", "--workspace", "--locked"]
        self.assertEqual(scoped_test_command(command, TestExecutionScope.LOCAL), command)
        expected = command + ["--"]
        for name in LOCAL_HARDWARE_TESTS:
            expected += ["--skip", name]
        self.assertEqual(scoped_test_command(command, TestExecutionScope.HOSTED), expected)
        with patch.dict(os.environ, {"FASTIPLAYER_TEST_SCOPE": "typo"}):
            with self.assertRaises(ValueError):
                execution_scope()
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "FASTIPLAYER_TEST_SCOPE": "local"}):
            with self.assertRaises(ValueError):
                execution_scope()

    def test_hosted_command_cannot_execute_hardware_bodies(self):
        # Настоящий libtest: аппаратные тела panic, software test обязан выполниться.
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "scope.rs"
            executable = Path(directory) / "scope-tests"
            source.write_text('''
mod gbm_allocator { mod tests {
    #[test] fn test_allocate_gbm_buffer() { panic!("hardware executed"); }
}}
mod linear_gbm_frame { mod safety_tests {
    #[test] fn frame_keeps_owner_device_alive_and_cpu_mapping_fails_closed() {
        panic!("hardware executed");
    }
}}
#[test] fn descriptor_software_contract() { assert_eq!(2 + 2, 4); }
''')
            subprocess.run(["rustc", "--test", str(source), "-o", str(executable)], check=True, capture_output=True)
            cargo_command = scoped_test_command(["cargo", "test"], TestExecutionScope.HOSTED)
            result = subprocess.run([str(executable), *cargo_command[3:]], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn("1 passed", result.stdout)
            self.assertIn("2 filtered out", result.stdout)


if __name__ == "__main__":
    unittest.main()

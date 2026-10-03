"""Проверяет hosted/local scope и фактическое исключение hardware tests."""

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))

from test_execution_scope import LOCAL_HARDWARE_TESTS, TestExecutionScope, execution_scope, scoped_test_command


class ExecutionScopeTests(unittest.TestCase):
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

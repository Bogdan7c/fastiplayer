#!/usr/bin/env python3
"""Opt-in offline vertical с настоящим cargo-llvm-cov и tiny Rust crate."""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

from coverage_runner import (  # noqa: E402
    CommandExecutor,
    RunnerConfig,
    StableCoverageRunner,
)


class RealCargoWithFixtureCoordinates(CommandExecutor):
    """Оставляет Cargo настоящим, подменяя только coordinate fixture output."""

    def run(
        self,
        arguments,
        *,
        cwd,
        environment,
        capture_output=False,
    ):
        """Создаёт минимальные atomic-boundary outputs coordinate fixture-а."""

        if arguments[0] != "fixture-python":
            return super().run(
                arguments,
                cwd=cwd,
                environment=environment,
                capture_output=capture_output,
            )
        if "validate-lcov" in arguments:
            return subprocess.CompletedProcess(arguments, 0, stdout="", stderr="")
        if "extract" in arguments:
            output = Path(arguments[arguments.index("--output") + 1])
            run_label = arguments[arguments.index("--run-label") + 1]
            output.write_text(json.dumps({"run_label": run_label}), encoding="utf-8")
            return subprocess.CompletedProcess(arguments, 0, stdout="", stderr="")
        if "intersect" in arguments:
            output = Path(arguments[arguments.index("--output") + 1])
            diagnostics = Path(arguments[arguments.index("--diagnostics") + 1])
            output.write_text('{"stable":true}', encoding="utf-8")
            diagnostics.write_text('{"variable":[]}', encoding="utf-8")
            return subprocess.CompletedProcess(arguments, 0, stdout="", stderr="")
        raise AssertionError(f"unexpected fixture command: {arguments}")


@unittest.skipUnless(
    os.environ.get("RUSTIPLAYER_RUN_REAL_COVERAGE_RUNNER") == "1",
    "opt-in: требуется pinned cargo-llvm-cov 0.8.7 + Rust 1.96.0",
)
class RealCargoCoverageRunnerTests(unittest.TestCase):
    """Проверяет реальные clean/build/run/report semantics без workspace rebuild."""

    def test_tiny_crate_reuses_one_instrumented_build_for_three_isolated_runs(self):
        """Настоящие show-env/direct Cargo/report создают exact isolated manifests."""

        with tempfile.TemporaryDirectory() as temporary_directory:
            repo_root = Path(temporary_directory)
            (repo_root / "src").mkdir()
            (repo_root / "src" / "lib.rs").write_text(
                "pub fn answer() -> u8 { 42 }\n"
                "#[cfg(test)] mod tests { #[test] fn works() { assert_eq!(super::answer(), 42); } }\n",
                encoding="utf-8",
            )
            (repo_root / "Cargo.toml").write_text(
                '[package]\nname = "coverage-runner-fixture"\nversion = "0.1.0"\n'
                'edition = "2021"\n',
                encoding="utf-8",
            )
            (repo_root / ".gitignore").write_text("/target/\n", encoding="utf-8")
            subprocess.run(["git", "init", "-q", repo_root], check=True)
            subprocess.run(
                ["cargo", "+1.96.0", "generate-lockfile", "--offline"],
                cwd=repo_root,
                check=True,
            )
            placeholder = repo_root / "placeholder.py"
            placeholder.write_text("# intercepted by fixture executor\n", encoding="utf-8")
            policy = repo_root / "policy.json"
            policy.write_text("{}\n", encoding="utf-8")
            executable_inventory_policy = repo_root / "executable-policy.json"
            executable_inventory_policy.write_text(
                '{"schema_version":1,"runtime_build_roots":[]}\n',
                encoding="utf-8",
            )
            config = RunnerConfig(
                repo_root=repo_root,
                profile_directory=repo_root / "target" / "llvm-cov-target",
                artifact_directory=repo_root / "target" / "coverage" / "stable",
                policy_path=policy,
                executable_inventory_policy_path=executable_inventory_policy,
                coordinate_extractor=placeholder,
                stability_tool=placeholder,
                lcov_validator=placeholder,
                toolchain="1.96.0",
                cargo_llvm_cov_version="0.8.7",
                llvm_cov_version="22.1.2",
                session_id="real-fixture",
                cargo_command="cargo",
                rustc_command="rustc",
                python_command="fixture-python",
            )
            # Реальный bootstrap blocker: старые wrapper list/profdata пережили прошлый run.
            config.profile_directory.mkdir(parents=True, exist_ok=True)
            stale_list = config.profile_directory / "rustiplayer-profraw-list"
            stale_profdata = config.profile_directory / "rustiplayer.profdata"
            stale_list.write_bytes(b"stale-real-profile-list\n")
            stale_profdata.write_bytes(b"stale-real-profdata\n")
            StableCoverageRunner(config, RealCargoWithFixtureCoordinates()).run()
            raw_manifests = sorted((config.artifact_directory / "manifests").glob("*.json"))
            self.assertEqual(len(raw_manifests), 3)
            self.assertTrue((config.artifact_directory / "cohort.json").is_file())
            self.assertTrue((config.artifact_directory / "html" / "index.html").is_file())
            replacement_backup = (
                config.artifact_directory / "replaced-merge-metadata"
            )
            self.assertEqual(
                (replacement_backup / stale_list.name).read_bytes(),
                b"stale-real-profile-list\n",
            )
            self.assertEqual(
                (replacement_backup / stale_profdata.name).read_bytes(),
                b"stale-real-profdata\n",
            )
            merge_manifest = json.loads(
                (config.artifact_directory / "cohort-manifest.json").read_text()
            )["merge_metadata"]
            self.assertEqual(len(merge_manifest["preexisting"]), 2)
            self.assertEqual(len(merge_manifest["authoritative"]), 2)
            first_authoritative = {
                str(entry["path"]): (
                    config.profile_directory / str(entry["path"])
                ).read_bytes()
                for entry in merge_manifest["authoritative"]
            }
            StableCoverageRunner(config, RealCargoWithFixtureCoordinates()).run()
            replacement_backup = (
                config.artifact_directory / "replaced-merge-metadata"
            )
            self.assertEqual(
                {path.name: path.read_bytes() for path in replacement_backup.iterdir()},
                first_authoritative,
            )
            second_merge_manifest = json.loads(
                (config.artifact_directory / "cohort-manifest.json").read_text()
            )["merge_metadata"]
            self.assertEqual(
                {
                    str(entry["path"]): str(entry["sha256"])
                    for entry in second_merge_manifest["preexisting"]
                },
                {
                    name: hashlib.sha256(payload).hexdigest()
                    for name, payload in first_authoritative.items()
                },
            )
            self.assertEqual(
                len(list(config.artifact_directory.parent.glob(".stable.previous"))),
                0,
            )


if __name__ == "__main__":
    unittest.main()

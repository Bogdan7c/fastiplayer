#!/usr/bin/env python3
"""Транзакционный трёхпрогонный runner для stable-coordinate coverage."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Mapping, Sequence

from coverage_runner_support import (
    CoverageRunnerError,
    assert_unchanged,
    atomic_artifact_stage,
    atomic_write_json,
    canonical_json_bytes,
    executable_manifest,
    git_source_manifest,
    publish_artifacts,
    sha256_file,
)


RUN_COUNT = 3
PROFILE_NAME = "workspace"
METHODOLOGY = "cargo-llvm-cov-full-json-3run-v1"
SESSION_ID_PATTERN = re.compile(r"^[A-Za-z0-9_-]+$")


@dataclass(frozen=True)
class RunnerConfig:
    """Все пути и exact tool identities одного воспроизводимого cohort."""

    repo_root: Path
    profile_directory: Path
    artifact_directory: Path
    policy_path: Path
    coordinate_extractor: Path
    stability_tool: Path
    lcov_validator: Path
    toolchain: str
    cargo_llvm_cov_version: str
    llvm_cov_version: str
    session_id: str
    cargo_command: str
    rustc_command: str
    python_command: str


@dataclass(frozen=True)
class ToolIdentity:
    """Версии compiler, LLVM и wrapper, которыми построен весь cohort."""

    rustc_release: str
    llvm_release: str
    cargo_llvm_cov_release: str


class CommandExecutor:
    """Единственная subprocess-граница runner-а с единым error contract."""

    def run(
        self,
        arguments: Sequence[str],
        *,
        cwd: Path,
        environment: Mapping[str, str],
        capture_output: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        """Запускает exact argv без shell и сохраняет ненулевой status."""

        try:
            return subprocess.run(
                list(arguments),
                cwd=cwd,
                env=dict(environment),
                check=True,
                text=True,
                stdout=subprocess.PIPE if capture_output else None,
                stderr=subprocess.PIPE if capture_output else None,
            )
        except subprocess.CalledProcessError as error:
            rendered = " ".join(arguments)
            detail = (error.stderr or error.stdout or "").strip()
            suffix = f": {detail}" if detail else ""
            raise CoverageRunnerError(
                f"команда завершилась с кодом {error.returncode}: {rendered}{suffix}"
            ) from error


class StableCoverageRunner:
    """Владеет clean/build/execute/report lifecycle одного exact cohort."""

    def __init__(self, config: RunnerConfig, executor: CommandExecutor | None = None):
        self.config = config
        self.executor = executor or CommandExecutor()
        self.base_environment = dict(os.environ)
        # Ambient serialization нельзя выдавать за normal-concurrency acceptance.
        self.base_environment.pop("RUST_TEST_THREADS", None)
        # Wrapper override фиксирует raw/report root; direct Cargo build ниже получает свой target.
        self.base_environment["CARGO_LLVM_COV_TARGET_DIR"] = str(
            config.profile_directory
        )
        self.cargo_prefix = [
            config.cargo_command,
            f"+{config.toolchain}",
            "llvm-cov",
        ]
        self.generated_merge_artifacts: set[Path] = set()
        self.instrumented_environment: dict[str, str] | None = None

    def execute(self, arguments: Sequence[str], *, capture: bool = False):
        """Выполняет команду от корня exact worktree."""

        return self.executor.run(
            arguments,
            cwd=self.config.repo_root,
            environment=self.base_environment,
            capture_output=capture,
        )

    def tool_identity(self) -> ToolIdentity:
        """Проверяет pinned Rust, LLVM и cargo-llvm-cov до использования counters."""

        rustc_result = self.execute(
            [self.config.rustc_command, f"+{self.config.toolchain}", "-vV"],
            capture=True,
        )
        rustc_lines = dict(
            line.split(": ", 1)
            for line in rustc_result.stdout.splitlines()
            if ": " in line
        )
        rustc_release = rustc_lines.get("release", "")
        llvm_release = rustc_lines.get("LLVM version", "")
        cov_result = self.execute([*self.cargo_prefix, "--version"], capture=True)
        cargo_release = cov_result.stdout.strip()
        expected_cargo = f"cargo-llvm-cov {self.config.cargo_llvm_cov_version}"
        if rustc_release != self.config.toolchain:
            raise CoverageRunnerError(
                f"требуется Rust {self.config.toolchain}, получено {rustc_release or 'unknown'}"
            )
        if llvm_release != self.config.llvm_cov_version:
            raise CoverageRunnerError(
                f"требуется LLVM {self.config.llvm_cov_version}, получено {llvm_release or 'unknown'}"
            )
        if cargo_release != expected_cargo:
            raise CoverageRunnerError(
                f"требуется {expected_cargo}, получено {cargo_release or 'unknown'}"
            )
        return ToolIdentity(rustc_release, llvm_release, cargo_release)

    def clean_all_and_build_once(self) -> dict[str, object]:
        """Один full clean и один no-run build отделяют compilation от трёх executions."""

        self.execute([*self.cargo_prefix, "clean", "--workspace", "--locked"])
        # В 0.8.7 пары --no-run/--no-report и --no-clean/--no-report запрещены самим CLI.
        # Поэтому официальный show-env даёт instrumented env для одного direct Cargo build/run.
        show_env = self.execute([*self.cargo_prefix, "show-env", "--sh"], capture=True)
        build_environment = dict(self.base_environment)
        seen_variables: set[str] = set()
        for line in show_env.stdout.splitlines():
            if not line:
                continue
            if not line.startswith("export "):
                raise CoverageRunnerError(f"show-env вернул не-export строку: {line}")
            line = line.removeprefix("export ")
            if "=" not in line:
                raise CoverageRunnerError(f"show-env вернул строку без значения: {line}")
            variable_name, encoded_value = line.split("=", 1)
            if not re.fullmatch(r"[A-Z_][A-Z0-9_]*", variable_name):
                raise CoverageRunnerError(f"show-env вернул недопустимое имя: {variable_name}")
            if variable_name in seen_variables:
                raise CoverageRunnerError(f"show-env повторил переменную: {variable_name}")
            seen_variables.add(variable_name)
            decoded_values = shlex.split(encoded_value, posix=True)
            if len(decoded_values) != 1:
                raise CoverageRunnerError(f"show-env вернул неоднозначное значение: {variable_name}")
            build_environment[variable_name] = decoded_values[0]
        required_variables = {"RUSTC_WRAPPER", "CARGO_LLVM_COV", "LLVM_PROFILE_FILE"}
        missing_variables = sorted(required_variables - build_environment.keys())
        if missing_variables:
            raise CoverageRunnerError(
                "show-env не вернул обязательные переменные: " + ", ".join(missing_variables)
            )
        if (
            Path(build_environment["LLVM_PROFILE_FILE"]).parent.resolve()
            != self.config.profile_directory
        ):
            raise CoverageRunnerError("show-env вывел LLVM_PROFILE_FILE вне isolated directory")
        # Plain Cargo build использует тот же isolated target, который report знает через wrapper env.
        build_environment["CARGO_TARGET_DIR"] = str(self.config.profile_directory)
        build_environment.pop("RUST_TEST_THREADS", None)
        self.instrumented_environment = build_environment
        self.executor.run(
            [
                self.config.cargo_command,
                f"+{self.config.toolchain}",
                "test",
                "--workspace",
                "--all-features",
                "--locked",
                "--no-fail-fast",
                "--no-run",
            ],
            cwd=self.config.repo_root,
            environment=build_environment,
        )
        return executable_manifest(self.config.profile_directory)

    def clean_profiles_and_prove_empty(self) -> None:
        """Перед каждым run удаляет только raw profiles и затем проверяет реальную пустоту."""

        self.execute(
            [
                *self.cargo_prefix,
                "clean",
                "--locked",
                "--profraw-only",
            ]
        )
        leftovers = sorted(self.config.profile_directory.glob("*.profraw"))
        if leftovers:
            names = ", ".join(path.name for path in leftovers[:5])
            raise CoverageRunnerError(f"profraw-only clean оставил stale profiles: {names}")

    def remove_owned_merge_artifacts(self) -> None:
        """Удаляет только profdata/list, которые runner сам обнаружил после прошлого report."""

        for generated_path in self.generated_merge_artifacts:
            generated_path.unlink(missing_ok=True)
        self.generated_merge_artifacts.clear()

    def assert_no_merge_artifacts(self) -> None:
        """Запрещает cargo-llvm-cov переиспользовать profdata предыдущего run."""

        stale = [
            *self.config.profile_directory.glob("*.profdata"),
            *self.config.profile_directory.glob("*-profraw-list"),
        ]
        if stale:
            names = ", ".join(sorted(path.name for path in stale))
            raise CoverageRunnerError(f"до report остались stale merge artifacts: {names}")

    def collect_profiles(self, prefix: str) -> list[dict[str, object]]:
        """Принимает только ненулевой набор raw profiles текущего уникального run."""

        profiles = sorted(self.config.profile_directory.glob("*.profraw"))
        if not profiles:
            raise CoverageRunnerError("test run не создал ни одного .profraw")
        foreign = [profile.name for profile in profiles if not profile.name.startswith(prefix)]
        if foreign:
            raise CoverageRunnerError(
                "profile directory смешал разные runs: " + ", ".join(foreign[:5])
            )
        manifest = []
        for profile in profiles:
            if profile.is_symlink() or not profile.is_file():
                raise CoverageRunnerError(f"raw profile не является regular file: {profile.name}")
            if profile.stat().st_size == 0:
                raise CoverageRunnerError(f"пустой raw profile: {profile.name}")
            manifest.append(
                {
                    "name": profile.name,
                    "size": profile.stat().st_size,
                    "sha256": sha256_file(profile),
                }
            )
        return manifest

    def assert_profile_hashes(self, expected: Sequence[dict[str, object]]) -> None:
        """Report не может читать профильный набор, изменившийся после manifest."""

        actual = []
        for entry in expected:
            profile = self.config.profile_directory / str(entry["name"])
            if not profile.is_file():
                raise CoverageRunnerError(f"raw profile исчез до завершения report: {profile.name}")
            actual.append(
                {
                    "name": profile.name,
                    "size": profile.stat().st_size,
                    "sha256": sha256_file(profile),
                }
            )
        if actual != list(expected):
            raise CoverageRunnerError("raw profile изменился между execution и reports")

    def verify_merge_inputs(self, expected_profiles: Sequence[dict[str, object]]) -> None:
        """Доказывает, что cargo-llvm-cov merge перечислил именно текущие profiles."""

        expected_paths = {
            str((self.config.profile_directory / str(entry["name"])).resolve())
            for entry in expected_profiles
        }
        profile_lists = sorted(self.config.profile_directory.glob("*-profraw-list"))
        matching_lists = []
        for profile_list in profile_lists:
            listed_paths = {
                line.strip()
                for line in profile_list.read_text(encoding="utf-8").splitlines()
                if line.strip()
            }
            if listed_paths == expected_paths:
                matching_lists.append(profile_list)
        if len(matching_lists) != 1 or len(profile_lists) != 1:
            raise CoverageRunnerError("profraw-list не совпадает с exact текущим profile set")
        profdata_files = sorted(self.config.profile_directory.glob("*.profdata"))
        if len(profdata_files) != 1 or profdata_files[0].stat().st_size == 0:
            raise CoverageRunnerError("report не создал ровно один ненулевой profdata")
        self.generated_merge_artifacts = {matching_lists[0], profdata_files[0]}

    def cargo_report(self, *arguments: str) -> None:
        """Reports вызываются строго синхронно на одном проверенном profile set."""

        self.execute([*self.cargo_prefix, "report", *arguments])

    def run_one(
        self,
        run_number: int,
        stage: Path,
        source_manifest: dict[str, object],
        build_manifest: dict[str, object],
        tool_identity: ToolIdentity,
    ) -> Path:
        """Исполняет suite и публикует raw/report/state только внутри private stage."""

        self.remove_owned_merge_artifacts()
        self.clean_profiles_and_prove_empty()
        self.assert_no_merge_artifacts()
        run_label = f"run-{run_number}"
        prefix = f"stable-{self.config.session_id}-{run_label}-"
        if self.instrumented_environment is None:
            raise CoverageRunnerError("instrumented environment отсутствует после build-once")
        run_environment = dict(self.instrumented_environment)
        profile_file_name = f"{prefix}%p-%16m.profraw"
        run_environment["LLVM_PROFILE_FILE_NAME"] = profile_file_name
        run_environment["LLVM_PROFILE_FILE"] = str(
            self.config.profile_directory / profile_file_name
        )
        run_environment.pop("RUST_TEST_THREADS", None)
        run_arguments = [
            self.config.cargo_command,
            f"+{self.config.toolchain}",
            "test",
            "--workspace",
            "--all-features",
            "--locked",
            "--no-fail-fast",
        ]
        self.executor.run(
            run_arguments,
            cwd=self.config.repo_root,
            environment=run_environment,
        )
        assert_unchanged(
            "source inventory", source_manifest, git_source_manifest(self.config.repo_root)
        )
        assert_unchanged(
            "instrumented build",
            build_manifest,
            executable_manifest(self.config.profile_directory),
        )
        assert_unchanged("tool identity", tool_identity, self.tool_identity())
        profiles = self.collect_profiles(prefix)

        profile_identity_path = stage / "profiles" / f"{run_label}.json"
        atomic_write_json(
            profile_identity_path,
            {
                "schema_version": 1,
                "profile": PROFILE_NAME,
                "methodology": METHODOLOGY,
                "llvm_cov_version": self.config.llvm_cov_version,
                "cargo_llvm_cov_version": self.config.cargo_llvm_cov_version,
            },
        )
        profile_manifest_path = stage / "manifests" / f"{run_label}.json"
        atomic_write_json(
            profile_manifest_path,
            {
                "schema_version": 1,
                "run_label": run_label,
                "profile_file_name": profile_file_name,
                "profiles": profiles,
                "profile_set_sha256": hashlib.sha256(
                    canonical_json_bytes(profiles)
                ).hexdigest(),
            },
        )

        full_json_path = stage / "raw" / f"{run_label}.json"
        summary_path = stage / "legacy" / f"{run_label}-summary.json"
        lcov_path = stage / "legacy" / f"{run_label}.lcov"
        full_json_path.parent.mkdir(parents=True, exist_ok=True)
        summary_path.parent.mkdir(parents=True, exist_ok=True)
        self.cargo_report("--json", "--output-path", str(full_json_path))
        if not full_json_path.is_file() or full_json_path.stat().st_size == 0:
            raise CoverageRunnerError(f"full JSON report пуст для {run_label}")
        self.verify_merge_inputs(profiles)
        self.cargo_report(
            "--json", "--summary-only", "--output-path", str(summary_path)
        )
        if not summary_path.is_file() or summary_path.stat().st_size == 0:
            raise CoverageRunnerError(f"summary JSON report пуст для {run_label}")
        self.verify_merge_inputs(profiles)
        self.cargo_report("--lcov", "--output-path", str(lcov_path))
        if not lcov_path.is_file() or lcov_path.stat().st_size == 0:
            raise CoverageRunnerError(f"LCOV report пуст для {run_label}")
        self.verify_merge_inputs(profiles)
        self.assert_profile_hashes(profiles)
        self.execute(
            [
                self.config.python_command,
                str(self.config.lcov_validator),
                "validate-lcov",
                "--input",
                str(lcov_path),
            ]
        )

        state_path = stage / f"{run_label}.json"
        self.execute(
            [
                self.config.python_command,
                str(self.config.coordinate_extractor),
                "extract",
                "--policy",
                str(self.config.policy_path),
                "--repo-root",
                str(self.config.repo_root),
                "--input",
                str(full_json_path),
                "--profile-manifest",
                str(profile_identity_path),
                "--run-label",
                run_label,
                "--output",
                str(state_path),
            ]
        )
        if not state_path.is_file() or state_path.stat().st_size == 0:
            raise CoverageRunnerError(f"coordinate extractor не создал {run_label} state")
        return state_path

    def write_cohort_manifest(
        self,
        stage: Path,
        source_manifest: dict[str, object],
        build_manifest: dict[str, object],
        tool_identity: ToolIdentity,
    ) -> None:
        """Сохраняет проверяемые hashes всех опубликованных cohort artifacts."""

        artifact_hashes = []
        for artifact in sorted(stage.rglob("*")):
            if artifact.is_file() and artifact.name != "cohort-manifest.json":
                artifact_hashes.append(
                    {
                        "path": artifact.relative_to(stage).as_posix(),
                        "size": artifact.stat().st_size,
                        "sha256": sha256_file(artifact),
                    }
                )
        atomic_write_json(
            stage / "cohort-manifest.json",
            {
                "schema_version": 1,
                "run_count": RUN_COUNT,
                "source": source_manifest,
                "build": build_manifest,
                "tool": {
                    "rustc_release": tool_identity.rustc_release,
                    "llvm_release": tool_identity.llvm_release,
                    "cargo_llvm_cov_release": tool_identity.cargo_llvm_cov_release,
                },
                "artifacts": artifact_hashes,
            },
        )

    def run(self) -> None:
        """Строит exact три runs и транзакционно заменяет только свой artifact tree."""

        stage = atomic_artifact_stage(
            self.config.artifact_directory, self.config.session_id
        )
        lock_path = self.config.artifact_directory.parent / ".stable-coverage.lock"
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        try:
            with lock_path.open("a+b") as lock_file:
                try:
                    fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError as error:
                    raise CoverageRunnerError("уже выполняется другой stable coverage runner") from error
                tool_identity = self.tool_identity()
                source_manifest = git_source_manifest(self.config.repo_root)
                build_manifest = self.clean_all_and_build_once()
                assert_unchanged(
                    "source inventory",
                    source_manifest,
                    git_source_manifest(self.config.repo_root),
                )
                assert_unchanged("tool identity", tool_identity, self.tool_identity())
                states = [
                    self.run_one(
                        run_number,
                        stage,
                        source_manifest,
                        build_manifest,
                        tool_identity,
                    )
                    for run_number in range(1, RUN_COUNT + 1)
                ]
                cohort_path = stage / "cohort.json"
                variable_path = stage / "variable.json"
                intersect_arguments = [
                    self.config.python_command,
                    str(self.config.stability_tool),
                    "intersect",
                    "--policy",
                    str(self.config.policy_path),
                ]
                for state in states:
                    intersect_arguments.extend(["--run", str(state)])
                intersect_arguments.extend(
                    ["--output", str(cohort_path), "--diagnostics", str(variable_path)]
                )
                self.execute(intersect_arguments)
                if (
                    not cohort_path.is_file()
                    or cohort_path.stat().st_size == 0
                    or not variable_path.is_file()
                    or variable_path.stat().st_size == 0
                ):
                    raise CoverageRunnerError("intersection не создал cohort/variable artifacts")
                # HTML нужен один раз как диагностика; blocking contract читает cohort JSON.
                self.cargo_report("--html", "--output-dir", str(stage))
                if not (stage / "html" / "index.html").is_file():
                    raise CoverageRunnerError("HTML diagnostics не создали html/index.html")
                self.verify_merge_inputs(
                    json.loads((stage / "manifests" / "run-3.json").read_text())[
                        "profiles"
                    ]
                )
                assert_unchanged(
                    "source inventory",
                    source_manifest,
                    git_source_manifest(self.config.repo_root),
                )
                assert_unchanged(
                    "instrumented build",
                    build_manifest,
                    executable_manifest(self.config.profile_directory),
                )
                assert_unchanged("tool identity", tool_identity, self.tool_identity())
                self.write_cohort_manifest(
                    stage, source_manifest, build_manifest, tool_identity
                )
                publish_artifacts(
                    stage, self.config.artifact_directory, self.config.session_id
                )
        except BaseException:
            if stage.exists():
                shutil.rmtree(stage)
            # Удаляются только profiles с session-owned prefix и уже обнаруженные merge outputs.
            for profile in self.config.profile_directory.glob(
                f"stable-{self.config.session_id}-run-*.profraw"
            ):
                profile.unlink(missing_ok=True)
            self.remove_owned_merge_artifacts()
            raise


def parse_args(arguments: Sequence[str]) -> RunnerConfig:
    """Парсит explicit paths, чтобы production и fake vertical использовали один CLI."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo-root", type=Path, required=True)
    parser.add_argument("--profile-directory", type=Path, required=True)
    parser.add_argument("--artifact-directory", type=Path, required=True)
    parser.add_argument("--policy", type=Path, required=True)
    parser.add_argument("--coordinate-extractor", type=Path, required=True)
    parser.add_argument("--stability-tool", type=Path, required=True)
    parser.add_argument("--lcov-validator", type=Path, required=True)
    parser.add_argument("--toolchain", default="1.96.0")
    parser.add_argument("--cargo-llvm-cov-version", default="0.8.7")
    parser.add_argument("--llvm-cov-version", default="22.1.2")
    parser.add_argument("--session-id", required=True)
    parser.add_argument("--cargo-command", default="cargo")
    parser.add_argument("--rustc-command", default="rustc")
    parser.add_argument("--python-command", default=sys.executable)
    parsed = parser.parse_args(arguments)
    if not SESSION_ID_PATTERN.fullmatch(parsed.session_id):
        parser.error("--session-id принимает только ASCII letters, digits, '_' и '-'")

    repo_root = parsed.repo_root.resolve()
    return RunnerConfig(
        repo_root=repo_root,
        profile_directory=parsed.profile_directory.resolve(),
        artifact_directory=parsed.artifact_directory.resolve(),
        policy_path=parsed.policy.resolve(),
        coordinate_extractor=parsed.coordinate_extractor.resolve(),
        stability_tool=parsed.stability_tool.resolve(),
        lcov_validator=parsed.lcov_validator.resolve(),
        toolchain=parsed.toolchain,
        cargo_llvm_cov_version=parsed.cargo_llvm_cov_version,
        llvm_cov_version=parsed.llvm_cov_version,
        session_id=parsed.session_id,
        cargo_command=parsed.cargo_command,
        rustc_command=parsed.rustc_command,
        python_command=parsed.python_command,
    )


def main(arguments: Sequence[str] | None = None) -> int:
    """CLI boundary печатает краткую причину и никогда не публикует partial cohort."""

    try:
        StableCoverageRunner(parse_args(arguments or sys.argv[1:])).run()
    except (CoverageRunnerError, OSError) as error:
        print(f"Ошибка stable coverage runner: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

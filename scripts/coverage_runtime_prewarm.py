#!/usr/bin/env python3
"""Typed prewarm runtime compiler roots до immutable coverage inventory freeze."""

from __future__ import annotations

import re
import shlex
from collections.abc import Callable, Mapping, Sequence
from pathlib import Path

from coverage_executable_inventory import (
    PrebuiltExecutableReference,
    RuntimeExecutableReference,
)
from coverage_executable_policy import ExecutableInventoryPolicy
from coverage_runner_model import RunnerConfig
from coverage_runner_support import CoverageRunnerError


InstrumentedRunner = Callable[[Sequence[str], Mapping[str, str]], object]
CommandRunner = Callable[[Sequence[str], bool], object]


def prepare_instrumented_build(
    config: RunnerConfig,
    cargo_prefix: Sequence[str],
    base_environment: Mapping[str, str],
    run_command: CommandRunner,
    run_instrumented: InstrumentedRunner,
) -> dict[str, str]:
    """Выполняет один full clean/no-run и возвращает официальный wrapper env."""

    run_command([*cargo_prefix, "clean", "--workspace", "--locked"], False)
    show_env = run_command([*cargo_prefix, "show-env", "--sh"], True)
    build_environment = dict(base_environment)
    seen_variables: set[str] = set()
    for line in show_env.stdout.splitlines():
        if not line:
            continue
        if not line.startswith("export "):
            raise CoverageRunnerError(f"show-env вернул не-export строку: {line}")
        assignment = line.removeprefix("export ")
        if "=" not in assignment:
            raise CoverageRunnerError(f"show-env вернул строку без значения: {line}")
        variable_name, encoded_value = assignment.split("=", 1)
        if not re.fullmatch(r"[A-Z_][A-Z0-9_]*", variable_name):
            raise CoverageRunnerError(f"show-env вернул недопустимое имя: {variable_name}")
        if variable_name in seen_variables:
            raise CoverageRunnerError(f"show-env повторил переменную: {variable_name}")
        seen_variables.add(variable_name)
        decoded_values = shlex.split(encoded_value, posix=True)
        if len(decoded_values) != 1:
            raise CoverageRunnerError(
                f"show-env вернул неоднозначное значение: {variable_name}"
            )
        build_environment[variable_name] = decoded_values[0]
    required_variables = {"RUSTC_WRAPPER", "CARGO_LLVM_COV", "LLVM_PROFILE_FILE"}
    missing_variables = sorted(required_variables - build_environment.keys())
    if missing_variables:
        raise CoverageRunnerError(
            "show-env не вернул обязательные переменные: " + ", ".join(missing_variables)
        )
    if (
        Path(build_environment["LLVM_PROFILE_FILE"]).parent.resolve()
        != config.profile_directory
    ):
        raise CoverageRunnerError("show-env вывел LLVM_PROFILE_FILE вне isolated directory")
    build_environment["CARGO_TARGET_DIR"] = str(config.profile_directory)
    build_environment.pop("RUST_TEST_THREADS", None)
    run_instrumented(
        [
            config.cargo_command,
            f"+{config.toolchain}",
            "test",
            "--workspace",
            "--all-features",
            "--locked",
            "--no-fail-fast",
            "--no-run",
        ],
        build_environment,
    )
    return build_environment


def _validate_and_discard_prewarm_profiles(
    profile_directory: Path,
    prefix: str,
    owner: str,
    clean_profiles: Callable[[], None],
) -> None:
    """Доказывает unique nonempty prewarm profiles и их полное удаление."""

    profiles = sorted(profile_directory.glob("*.profraw"))
    profile_name_pattern = re.compile(
        rf"^{re.escape(prefix)}[0-9]+-[0-9]+_[0-9]+\.profraw$"
    )
    if not profiles or any(
        not profile_name_pattern.fullmatch(profile.name) for profile in profiles
    ):
        raise CoverageRunnerError(
            f"typed prewarm `{owner}` не создал exact unique profiles"
        )
    for profile in profiles:
        if profile.is_symlink() or not profile.is_file() or profile.stat().st_size == 0:
            raise CoverageRunnerError(
                f"typed prewarm `{owner}` создал неверный profile"
            )
    clean_profiles()


def _cleanup_failed_prewarm_profiles(profile_directory: Path, prefix: str) -> None:
    """После failed subprocess удаляет только exact grammar текущего prewarm."""

    profile_name_pattern = re.compile(
        rf"^{re.escape(prefix)}[0-9]+-[0-9]+_[0-9]+\.profraw$"
    )
    cleanup_errors = []
    for profile in profile_directory.glob("*.profraw"):
        if not profile_name_pattern.fullmatch(profile.name):
            continue
        try:
            profile.unlink()
        except OSError as error:
            cleanup_errors.append(f"{profile.name}: {error}")
    if cleanup_errors:
        raise CoverageRunnerError(
            "не удалось удалить failed prewarm profiles: " + "; ".join(cleanup_errors)
        )


def prewarm_runtime_builds(
    config: RunnerConfig,
    policy: ExecutableInventoryPolicy,
    instrumented_environment: Mapping[str, str],
    run_instrumented: InstrumentedRunner,
    clean_profiles: Callable[[], None],
) -> tuple[PrebuiltExecutableReference, tuple[RuntimeExecutableReference, ...]]:
    """Materialize-ит declared roots и лишь затем фиксирует parent/runtime truth."""

    # Build scripts/proc-macros могли выполнить instrumented code уже при --no-run.
    clean_profiles()
    for root_policy in policy.runtime_build_roots:
        materializer = root_policy.materializer
        prewarm_environment = dict(instrumented_environment)
        prefix = f"stable-{config.session_id}-prewarm-{root_policy.owner}-"
        profile_file_name = f"{prefix}%p-%16m.profraw"
        prewarm_environment["LLVM_PROFILE_FILE_NAME"] = profile_file_name
        prewarm_environment["LLVM_PROFILE_FILE"] = str(
            config.profile_directory / profile_file_name
        )
        try:
            run_instrumented(
                [
                    config.cargo_command,
                    f"+{config.toolchain}",
                    "test",
                    "--package",
                    materializer.package,
                    "--test",
                    materializer.test,
                    "--all-features",
                    "--locked",
                    "--no-fail-fast",
                ],
                prewarm_environment,
            )
            _validate_and_discard_prewarm_profiles(
                config.profile_directory,
                prefix,
                root_policy.owner,
                clean_profiles,
            )
        except BaseException as primary_error:
            try:
                _cleanup_failed_prewarm_profiles(config.profile_directory, prefix)
            except BaseException as cleanup_error:
                raise CoverageRunnerError(
                    f"typed prewarm `{root_policy.owner}` завершился ошибкой "
                    f"({primary_error}); cleanup тоже завершился ошибкой: {cleanup_error}"
                ) from primary_error
            raise

    build_reference = PrebuiltExecutableReference(config.profile_directory, policy)
    runtime_references = tuple(
        RuntimeExecutableReference(config.profile_directory, root_policy)
        for root_policy in policy.runtime_build_roots
    )
    for runtime_reference in runtime_references:
        runtime_reference.freeze_after_materialization()
    return build_reference, runtime_references

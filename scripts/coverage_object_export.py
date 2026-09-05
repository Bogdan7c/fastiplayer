#!/usr/bin/env python3
"""LLVM_COV adapter: отдельные exports без потери mapping при объединении ELF."""

from __future__ import annotations

import gzip
import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any, Mapping, Sequence, TYPE_CHECKING

from coverage_runner_model import RunnerConfig
from coverage_executable_inventory import PrebuiltExecutableReference, RuntimeExecutableReference

if TYPE_CHECKING:
    from coverage_runner import CommandExecutor

from coverage_coordinate_model import read_json, write_json_atomic
from coverage_runner_support import sha256_file


CONFIG_ENV = "FASTIPLAYER_COVERAGE_EXPORT_CONFIG"


def split_objects(arguments: list[str]) -> tuple[list[str], list[str]]:
    """Сохраняет все фильтры wrapper-а, извлекая только exact object argv."""
    remaining: list[str] = []
    objects: list[str] = []
    index = 0
    while index < len(arguments):
        argument = arguments[index]
        if argument == "-object":
            index += 1
            if index == len(arguments):
                raise ValueError("-object без пути")
            objects.append(arguments[index])
        elif argument.startswith("-object="):
            objects.append(argument.removeprefix("-object="))
        else:
            remaining.append(argument)
        index += 1
    if not objects or len(objects) != len(set(objects)):
        raise ValueError("export должен содержать непустой unique object set")
    return remaining, objects


def export_objects(config: dict[str, Any], arguments: list[str]) -> None:
    """Проверяет frozen inputs и сохраняет каждый полный JSON вместе с hash."""
    common, objects = split_objects(arguments)
    root = Path(config["profile_directory"]).resolve()
    output = Path(config["output_directory"])
    output.mkdir(parents=True, exist_ok=False)
    inventory = {entry["path"]: entry for entry in config["executables"]}
    if len(inventory) != len(config["executables"]):
        raise ValueError("duplicate executable inventory")
    profiles = [a.split("=", 1)[1] for a in common if a.startswith("-instr-profile=")]
    if len(profiles) != 1 or Path(profiles[0]).resolve().parent != root:
        raise ValueError("export profile вне frozen profile directory")
    profile = Path(profiles[0])
    profile_hash = sha256_file(profile)
    exports = []
    for number, object_name in enumerate(objects):
        binary = Path(object_name).absolute()
        if binary != binary.resolve():
            raise ValueError("export object содержит symlink")
        relative = binary.relative_to(root).as_posix()
        expected = inventory.get(relative)
        if expected is None or sha256_file(binary) != expected["sha256"]:
            raise ValueError(f"export object отсутствует в frozen inventory: {relative}")
        result = subprocess.run(
            [config["llvm_cov"], *common, "-object", str(binary)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True,
        )
        # LLVM diagnostics сохраняются отдельно; malformed JSON остановит extraction.
        report = json.loads(result.stdout)
        if report.get("type") != "llvm.coverage.json.export" or report.get("version") != "3.1.0":
            raise ValueError("неожиданный LLVM export format")
        filename = f"object-{number:04d}.json.gz"
        with gzip.GzipFile(filename=str(output / filename), mode="wb", mtime=0) as archive:
            archive.write(result.stdout)
        (output / f"object-{number:04d}.stderr").write_bytes(result.stderr)
        if sha256_file(binary) != expected["sha256"]:
            raise ValueError("export object изменился во время чтения")
        exports.append({"object": relative, "object_sha256": expected["sha256"],
                        "report": filename, "sha256": sha256_file(output / filename)})
    if sha256_file(profile) != profile_hash:
        raise ValueError("merged profile изменился во время exports")
    write_json_atomic(output / "manifest.json", {
        "schema_version": 1, "kind": "coverage-per-executable-exports",
        "source": config["source"], "profile_sha256": profile_hash,
        "objects": [entry["object"] for entry in exports], "exports": exports,
    })


def collect_workspace_exports(
    config: RunnerConfig,
    executor: CommandExecutor,
    base_environment: Mapping[str, str],
    stage: Path,
    run_label: str,
    full_json_path: Path,
    source_manifest: dict[str, object],
    build_reference: PrebuiltExecutableReference,
    runtime_references: Sequence[RuntimeExecutableReference],
) -> Path:
    """Настраивает adapter на frozen inventory, сохраняя фильтры cargo wrapper-а."""
    from coverage_runner_support import CoverageRunnerError, atomic_write_json

    def inspect_tool(arguments: list[str]) -> str:
        return executor.run(arguments, cwd=config.repo_root,
                            environment=base_environment, capture_output=True).stdout

    rustc = [config.rustc_command, f"+{config.toolchain}"]
    sysroot = Path(inspect_tool([*rustc, "--print", "sysroot"]).strip())
    host = next((line.removeprefix("host: ") for line in
                 inspect_tool([*rustc, "-vV"]).splitlines() if line.startswith("host: ")), None)
    if not host:
        raise CoverageRunnerError("rustc не сообщил host для LLVM tool path")
    llvm_cov = base_environment.get("LLVM_COV") or str(
        sysroot / "lib" / "rustlib" / host / "bin" / "llvm-cov"
    )
    entries = list(build_reference.manifest()["entries"])
    for reference in runtime_references:
        entries.extend(reference.manifest()["entries"])
    output_directory = stage / "objects" / run_label
    config_path = stage / "profiles" / f"{run_label}-export-config.json"
    atomic_write_json(config_path, {
        "llvm_cov": llvm_cov, "profile_directory": str(config.profile_directory),
        "output_directory": str(output_directory), "executables": entries,
        "source": source_manifest,
    })
    environment = dict(base_environment)
    environment["LLVM_COV"] = str(Path(__file__).resolve())
    environment[CONFIG_ENV] = str(config_path)
    executor.run(
        [config.cargo_command, f"+{config.toolchain}", "llvm-cov", "report",
         "--json", "--output-path", str(full_json_path)],
        cwd=config.repo_root, environment=environment,
    )
    manifest_path = output_directory / "manifest.json"
    if not manifest_path.is_file():
        raise CoverageRunnerError("per-executable export manifest не создан")
    return manifest_path


def main() -> int:
    """Прозрачно обслуживает version probes; JSON export дополняет доказательствами."""
    try:
        config = read_json(Path(os.environ[CONFIG_ENV]))
        arguments = sys.argv[1:]
        subprocess.run([config["llvm_cov"], *arguments], check=True)
        if arguments and arguments[0] == "export":
            export_objects(config, arguments)
        return 0
    except (OSError, ValueError, KeyError, subprocess.CalledProcessError) as error:
        print(f"per-executable coverage export failed: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())

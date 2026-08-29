"""Общие schema primitives для stable source-coordinate coverage artifacts."""

from __future__ import annotations

import hashlib
import json
import os
import tempfile
from pathlib import Path
from typing import Any, Iterable


LLVM_EXPORT_TYPE = "llvm.coverage.json.export"
LLVM_EXPORT_VERSION = "3.1.0"
RUN_SCHEMA_VERSION = 1
METHODOLOGY = "cargo-llvm-cov-full-json-3run-v1"
LLVM_COV_VERSION = "22.1.2"
CARGO_LLVM_COV_VERSION = "0.8.7"
INT64_MAX = (1 << 63) - 1
METRICS = ("lines", "functions", "regions")
KNOWN_REGION_KINDS = frozenset(range(7))


def canonical_json(document: Any) -> str:
    return json.dumps(
        document,
        ensure_ascii=False,
        separators=(",", ":"),
        sort_keys=True,
        allow_nan=False,
    )


def content_hash(document: Any) -> str:
    return f"sha256:{hashlib.sha256(canonical_json(document).encode()).hexdigest()}"


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    document: dict[str, Any] = {}
    for key, value in pairs:
        if key in document:
            raise ValueError(f"JSON содержит duplicate key `{key}`")
        document[key] = value
    return document


def _reject_non_finite_constant(constant: str) -> None:
    raise ValueError(f"JSON содержит запрещённую non-finite константу `{constant}`")


def read_json(document_path: Path) -> Any:
    """Читает строгий RFC-совместимый JSON без неоднозначных object-ов."""

    with document_path.open(encoding="utf-8") as document_file:
        return json.load(
            document_file,
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=_reject_non_finite_constant,
        )


def write_json_atomic(document_path: Path, document: Any) -> None:
    """Публикует только полностью сформированный compact JSON."""

    document_path.parent.mkdir(parents=True, exist_ok=True)
    rendered = canonical_json(document) + "\n"
    temporary_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=document_path.parent,
            prefix=f".{document_path.name}.",
            suffix=".tmp",
            delete=False,
        ) as temporary_file:
            temporary_name = temporary_file.name
            temporary_file.write(rendered)
            temporary_file.flush()
            os.fsync(temporary_file.fileno())
        os.replace(temporary_name, document_path)
    finally:
        if temporary_name is not None:
            Path(temporary_name).unlink(missing_ok=True)


def _stage_json(document_path: Path, document: Any) -> Path:
    document_path.parent.mkdir(parents=True, exist_ok=True)
    rendered = canonical_json(document) + "\n"
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=document_path.parent,
        prefix=f".{document_path.name}.",
        suffix=".tmp",
        delete=False,
    ) as temporary_file:
        temporary_file.write(rendered)
        temporary_file.flush()
        os.fsync(temporary_file.fileno())
        return Path(temporary_file.name)


def write_json_pair_transactional(
    report_only_path: Path,
    report_only_document: Any,
    blocking_path: Path,
    blocking_document: Any,
) -> None:
    """Публикует пару без process-visible half-update при ошибке записи.

    Report-only artifact меняется первым. Если второй rename не удался, прежние
    байты первого файла восстанавливаются атомарно; blocking artifact поэтому
    никогда не опережает сопутствующую diagnostics.
    """

    if report_only_path.resolve(strict=False) == blocking_path.resolve(strict=False):
        raise ValueError("blocking output и report-only diagnostics должны различаться")
    staged_report = _stage_json(report_only_path, report_only_document)
    staged_blocking: Path | None = None
    previous_report: bytes | None = None
    report_published = False
    try:
        previous_report = (
            report_only_path.read_bytes() if report_only_path.exists() else None
        )
        staged_blocking = _stage_json(blocking_path, blocking_document)
        os.replace(staged_report, report_only_path)
        report_published = True
        os.replace(staged_blocking, blocking_path)
    except OSError:
        if report_published:
            if previous_report is None:
                report_only_path.unlink(missing_ok=True)
            else:
                with tempfile.NamedTemporaryFile(
                    mode="wb",
                    dir=report_only_path.parent,
                    prefix=f".{report_only_path.name}.",
                    suffix=".rollback",
                    delete=False,
                ) as rollback_file:
                    rollback_file.write(previous_report)
                    rollback_file.flush()
                    os.fsync(rollback_file.fileno())
                    rollback_path = Path(rollback_file.name)
                os.replace(rollback_path, report_only_path)
        raise
    finally:
        staged_report.unlink(missing_ok=True)
        if staged_blocking is not None:
            staged_blocking.unlink(missing_ok=True)


def require_object(value: Any, context: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"{context} должен быть JSON object")
    return value


def require_array(value: Any, context: str) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError(f"{context} должен быть JSON array")
    return value


def require_exact_keys(document: dict[str, Any], keys: set[str], context: str) -> None:
    actual_keys = set(document)
    if actual_keys != keys:
        raise ValueError(
            f"{context} имеет неверные поля; missing={sorted(keys - actual_keys)}, "
            f"unexpected={sorted(actual_keys - keys)}"
        )


def require_string(value: Any, context: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{context} должен быть непустой строкой")
    return value


def require_int(value: Any, context: str, *, minimum: int = 0) -> int:
    # bool является int в Python, но в LLVM schema это отдельный JSON type.
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{context} должен быть integer")
    if value < minimum:
        raise ValueError(f"{context} не может быть меньше {minimum}")
    if value >= INT64_MAX:
        raise ValueError(f"{context} содержит INT64_MAX sentinel/overflow")
    return value


def require_bool(value: Any, context: str) -> bool:
    if not isinstance(value, bool):
        raise ValueError(f"{context} должен быть boolean")
    return value


def _unique_strings(value: Any, context: str) -> list[str]:
    entries = require_array(value, f"policy.{context}")
    normalized = [require_string(entry, f"policy.{context}[]") for entry in entries]
    if len(set(normalized)) != len(normalized):
        raise ValueError(f"policy.{context} содержит duplicate")
    return normalized


def canonical_repo_relative_path(value: Any, context: str) -> str:
    source_path = Path(require_string(value, context))
    if source_path.is_absolute() or ".." in source_path.parts:
        raise ValueError(f"{context} должен быть repo-relative и не содержать `..`")
    normalized = source_path.as_posix()
    if normalized in {"", "."} or normalized != str(value):
        raise ValueError(f"{context} должен быть canonical repo-relative path")
    return normalized


def canonical_crate_owner(value: Any, context: str) -> str:
    owner = require_string(value, context)
    owner_path = Path(owner)
    if owner_path.is_absolute() or len(owner_path.parts) != 1 or owner in {".", ".."}:
        raise ValueError(f"{context} должен быть canonical crate owner")
    return owner


def validate_policy(policy: Any) -> dict[str, Any]:
    policy = require_object(policy, "coverage policy")
    require_exact_keys(
        policy,
        {
            "schema_version",
            "tool",
            "metrics",
            "blocking_crates",
            "informational_crates",
            "excluded_source_paths",
        },
        "coverage policy",
    )
    if require_int(policy["schema_version"], "policy.schema_version") != 1:
        raise ValueError("coverage policy поддерживается только в schema_version 1")
    tool = require_object(policy["tool"], "policy.tool")
    require_exact_keys(tool, {"name", "version"}, "policy.tool")
    if require_string(tool["name"], "policy.tool.name") != "cargo-llvm-cov":
        raise ValueError("policy.tool.name должен быть cargo-llvm-cov")
    if require_string(tool["version"], "policy.tool.version") != CARGO_LLVM_COV_VERSION:
        raise ValueError(f"policy.tool.version должен быть {CARGO_LLVM_COV_VERSION}")
    metrics = require_array(policy["metrics"], "policy.metrics")
    if metrics != list(METRICS):
        raise ValueError(f"policy.metrics должен быть {list(METRICS)}")
    blocking = _unique_strings(policy["blocking_crates"], "blocking_crates")
    informational = _unique_strings(policy["informational_crates"], "informational_crates")
    for group_name, owners in (
        ("blocking_crates", blocking),
        ("informational_crates", informational),
    ):
        for owner in owners:
            canonical_crate_owner(owner, f"policy.{group_name}[]")
    if set(blocking) & set(informational):
        raise ValueError("blocking и informational crate inventories пересекаются")
    exclusions = _unique_strings(policy["excluded_source_paths"], "excluded_source_paths")
    canonical_exclusions = [
        canonical_repo_relative_path(path, "policy.excluded_source_paths[]")
        for path in exclusions
    ]
    if canonical_exclusions != exclusions:
        raise ValueError("policy.excluded_source_paths должен быть canonical")
    classified_owners = set(blocking) | set(informational)
    for excluded_path in canonical_exclusions:
        if crate_name(excluded_path) not in classified_owners:
            raise ValueError(
                "policy.excluded_source_paths содержит путь вне classified crate inventories"
            )
    return policy


def validate_profile_manifest(profile: Any, policy: dict[str, Any]) -> dict[str, Any]:
    profile = require_object(profile, "profile manifest")
    require_exact_keys(
        profile,
        {
            "schema_version",
            "profile",
            "methodology",
            "llvm_cov_version",
            "cargo_llvm_cov_version",
        },
        "profile manifest",
    )
    if require_int(profile["schema_version"], "profile.schema_version") != 1:
        raise ValueError("profile manifest поддерживается только в schema_version 1")
    if require_string(profile["profile"], "profile.profile") != "workspace":
        raise ValueError("profile.profile должен быть workspace")
    if require_string(profile["methodology"], "profile.methodology") != METHODOLOGY:
        raise ValueError(f"profile.methodology должен быть {METHODOLOGY}")
    if require_string(profile["llvm_cov_version"], "profile.llvm_cov_version") != LLVM_COV_VERSION:
        raise ValueError(f"profile.llvm_cov_version должен быть {LLVM_COV_VERSION}")
    cargo_version = require_string(
        profile["cargo_llvm_cov_version"], "profile.cargo_llvm_cov_version"
    )
    if cargo_version != policy["tool"]["version"]:
        raise ValueError("profile cargo-llvm-cov version не совпадает с policy")
    return profile


class SourcePathNormalizer:
    """Не позволяет machine-specific путям стать частью coordinate identity."""

    def __init__(self, repo_root: Path):
        if not repo_root.is_absolute():
            raise ValueError("--repo-root должен быть абсолютным путём")
        self.repo_root = repo_root.resolve()

    def repository_path(self, raw_path: Any, context: str) -> str:
        source_path = Path(require_string(raw_path, context))
        if not source_path.is_absolute():
            raise ValueError(f"{context} должен быть абсолютным LLVM path")
        try:
            relative_path = source_path.resolve(strict=False).relative_to(self.repo_root)
        except ValueError as error:
            raise ValueError(f"{context} находится вне --repo-root") from error
        normalized = relative_path.as_posix()
        if normalized in {"", "."}:
            raise ValueError(f"{context} не может указывать на repo root")
        return normalized

    def optional_repository_path(self, raw_path: Any, context: str) -> str | None:
        source_path = Path(require_string(raw_path, context))
        if not source_path.is_absolute():
            raise ValueError(f"{context} должен быть абсолютным LLVM path")
        try:
            return source_path.resolve(strict=False).relative_to(self.repo_root).as_posix()
        except ValueError:
            # Dependency functions присутствуют в functions[], но не в files[].
            return None


def crate_name(relative_path: str) -> str:
    path_parts = Path(relative_path).parts
    if len(path_parts) < 3 or path_parts[0] != "crates":
        raise ValueError(f"source path `{relative_path}` не принадлежит crates/<owner>")
    return path_parts[1]


def ranges(indices: Iterable[int]) -> list[list[int]]:
    sorted_indices = sorted(set(indices))
    if not sorted_indices:
        return []
    result: list[list[int]] = []
    range_start = previous = sorted_indices[0]
    for index in sorted_indices[1:]:
        if index == previous + 1:
            previous = index
            continue
        result.append([range_start, previous + 1])
        range_start = previous = index
    result.append([range_start, previous + 1])
    return result


def coordinate_identity(metric: str, coordinate: list[int], source_files: list[str]) -> str:
    """Декодирует run-local file indices в межкоммитную source identity."""

    if metric == "lines":
        identity = ["L", source_files[coordinate[0]], coordinate[1]]
    elif metric == "functions":
        identity = ["F", source_files[coordinate[0]], coordinate[1], coordinate[2]]
    else:
        identity = [
            "R",
            source_files[coordinate[0]],
            coordinate[1],
            coordinate[2],
            source_files[coordinate[3]],
            *coordinate[4:],
        ]
    return json.dumps(identity, ensure_ascii=False, separators=(",", ":"))

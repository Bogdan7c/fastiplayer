#!/usr/bin/env python3
"""Владеет checked-in S42 production module-size snapshot ratchet."""

# Future annotations упрощают type hints для immutable result rows.
from __future__ import annotations

# json читает единственный checked-in baseline artifact.
import json
# dataclass хранит одно неизменяемое расхождение snapshot-а.
from dataclasses import dataclass
# pathlib запрещает absolute/path-traversal baseline targets.
from pathlib import Path
# Any описывает внешний JSON после runtime validation.
from typing import Any


# Checked-in snapshot хранит только legacy production modules выше hard limit.
MODULE_SIZE_BASELINE_PATH = Path("scripts/module-size-baseline.json")


# Ошибка schema/input отличается от найденного line-count delta.
class ModuleSizeInputError(RuntimeError):
    """Module-size baseline нельзя достоверно интерпретировать."""


# Одна запись сохраняет общий S42 diagnostic shape.
@dataclass(frozen=True)
class ModuleSizeViolation:
    """Одно module-size snapshot нарушение."""

    # Location является repository-relative production Rust path.
    location: str
    # Rule объясняет new/growth/stale invariant.
    rule: str
    # Evidence содержит exact baseline/current counters.
    evidence: str


# Функция читает strict module-size baseline schema.
def read_module_size_baseline(repo_root: Path) -> dict[str, Any]:
    """Возвращает validated module-size baseline JSON."""

    # Checked-in path является единственным owner-ом legacy allowlist.
    baseline_path = repo_root / MODULE_SIZE_BASELINE_PATH
    # Missing snapshot сделал бы каждый legacy module неаудируемым.
    if not baseline_path.is_file():
        raise ModuleSizeInputError(
            f"module-size baseline отсутствует: {MODULE_SIZE_BASELINE_PATH}"
        )
    # JSON parse error должен сохранять line/column.
    try:
        # UTF-8 read соответствует остальным repository artifacts.
        baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        # Невалидный snapshot не интерпретируется частично.
        raise ModuleSizeInputError(f"module-size baseline невалиден: {error}") from error
    # Текущая implementation понимает только schema v1.
    if not isinstance(baseline, dict) or baseline.get("schema_version") != 1:
        raise ModuleSizeInputError("module-size baseline имеет неподдерживаемую schema")
    # Hard limit хранится в policy artifact, а не magic constant кода.
    hard_limit = baseline.get("hard_limit_lines")
    # Positive integer защищает от bool/zero и бессмысленного ratchet-а.
    if isinstance(hard_limit, bool) or not isinstance(hard_limit, int) or hard_limit < 1:
        raise ModuleSizeInputError(
            "module-size hard_limit_lines должен быть positive integer"
        )
    # Legacy allowlist является exact path->line-count map.
    legacy_modules = baseline.get("legacy_modules")
    # Неверный тип мог бы молча обнулить ratchet.
    if not isinstance(legacy_modules, dict):
        raise ModuleSizeInputError("module-size legacy_modules должен быть JSON object")
    # Каждая запись обязана быть relative Rust path и count выше limit.
    for relative_path, line_count in legacy_modules.items():
        # Path создаётся только после string validation.
        if not isinstance(relative_path, str):
            raise ModuleSizeInputError("module-size baseline path должен быть string")
        # Parsed path нужен для absolute/traversal/suffix checks.
        parsed_path = Path(relative_path)
        # Exact snapshot принимает только production Rust targets.
        if (
            parsed_path.is_absolute()
            or ".." in parsed_path.parts
            or parsed_path.suffix != ".rs"
            or isinstance(line_count, bool)
            or not isinstance(line_count, int)
            or line_count <= hard_limit
        ):
            raise ModuleSizeInputError(
                f"невалидная module-size baseline запись: {relative_path}={line_count}"
            )
    # Validated object используется pure comparison.
    return baseline


# Функция считает строки каждого production module ровно один раз.
def current_oversized_modules(
    repo_root: Path,
    source_files: list[Path],
    hard_limit: int,
) -> dict[str, int]:
    """Возвращает current relative path->line count только выше hard limit."""

    # Отдельный mutable map избегает двойного чтения больших source files.
    current_counts: dict[str, int] = {}
    # Source inventory уже ограничен workspace production modules.
    for relative_path in source_files:
        # splitlines даёт стабильное логическое число строк с/без final newline.
        line_count = len(
            (repo_root / relative_path).read_text(encoding="utf-8").splitlines()
        )
        # Modules в пределах limit не являются legacy debt.
        if line_count <= hard_limit:
            continue
        # JSON-compatible relative path становится stable identity.
        current_counts[str(relative_path)] = line_count
    # Caller сравнивает map с checked-in snapshot.
    return current_counts


# Функция ratchet-ит legacy line counts и запрещает новый oversized module.
def find_module_size_violations(
    repo_root: Path,
    source_files: list[Path],
    baseline: dict[str, Any],
) -> list[ModuleSizeViolation]:
    """Возвращает exact module-size snapshot расхождения."""

    # Hard limit уже validated read boundary.
    hard_limit = baseline["hard_limit_lines"]
    # String keys соответствуют JSON artifact.
    expected_counts = baseline["legacy_modules"]
    # Current snapshot считается только из production workspace files.
    current_counts = current_oversized_modules(repo_root, source_files, hard_limit)
    # Все deltas агрегируются без fail-fast.
    violations: list[ModuleSizeViolation] = []
    # Новый oversized path не может воспользоваться чужим legacy allowance.
    for relative_path in sorted(set(current_counts) - set(expected_counts)):
        violations.append(
            ModuleSizeViolation(
                location=relative_path,
                rule="новый production module превысил hard line limit",
                evidence=f"{current_counts[relative_path]} > {hard_limit}",
            )
        )
    # Удалённый/уменьшенный legacy path требует понизить checked-in snapshot.
    for relative_path in sorted(set(expected_counts) - set(current_counts)):
        violations.append(
            ModuleSizeViolation(
                location=relative_path,
                rule="module-size baseline stale после уменьшения/удаления module",
                evidence=f"baseline={expected_counts[relative_path]}",
            )
        )
    # Любое изменение oversized count требует explicit snapshot review.
    for relative_path in sorted(set(current_counts) & set(expected_counts)):
        # Exact equality одновременно запрещает рост и ratchet-ит уменьшение.
        if current_counts[relative_path] == expected_counts[relative_path]:
            continue
        # Направление delta видно из обеих цифр.
        violations.append(
            ModuleSizeViolation(
                location=relative_path,
                rule="legacy oversized module line count изменился",
                evidence=(
                    f"baseline={expected_counts[relative_path]}, "
                    f"current={current_counts[relative_path]}"
                ),
            )
        )
    # Stable output упрощает deliberate decomposition review.
    return sorted(violations, key=lambda item: item.location)

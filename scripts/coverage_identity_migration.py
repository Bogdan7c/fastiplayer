"""Строгая миграция identity: только зарегистрированная биекция без новых измерений."""

from __future__ import annotations

import copy
import re
from typing import Any

import coverage_coordinate_model as model
from coverage_stability_schema import _expand_ranges, validate_baseline


def rename_identity(value: Any, previous_name: str, proposed_name: str) -> Any:
    """Нормализует подписи, сохраняя числа, порядок массивов и типы значений."""
    if isinstance(value, str):
        return re.sub(re.escape(previous_name), proposed_name, value)
    if isinstance(value, list):
        return [rename_identity(entry, previous_name, proposed_name) for entry in value]
    if isinstance(value, dict):
        renamed = {
            rename_identity(key, previous_name, proposed_name): rename_identity(
                entry, previous_name, proposed_name
            )
            for key, entry in value.items()
        }
        if len(renamed) != len(value):
            raise ValueError("identity migration: неоднозначное соответствие ключей")
        return renamed
    return value


def infer_previous_name(previous: dict, proposed: dict, proposed_name: str) -> str:
    """Выводит прежний бренд из единственного согласованного изменения owners."""
    if not re.fullmatch(r"[a-z][a-z0-9]*", proposed_name):
        raise ValueError("identity migration: некорректное новое имя")
    old_owners = {model.crate_name(path) for path in previous["source_files"]["universe"]}
    new_owners = {model.crate_name(path) for path in proposed["source_files"]["universe"]}
    removed, added = old_owners - new_owners, new_owners - old_owners
    candidates = set()
    for new_owner in added:
        if not new_owner.startswith(proposed_name + "-"):
            continue
        suffix = new_owner[len(proposed_name):]
        candidates.update(owner[:-len(suffix)] for owner in removed if owner.endswith(suffix))
    valid = {
        candidate for candidate in candidates
        if candidate and {
            owner.replace(candidate, proposed_name) for owner in old_owners
        } == new_owners
    }
    if len(valid) != 1:
        raise ValueError("identity migration: неоднозначное соответствие владельцев файлов")
    return valid.pop()


def migrate_baseline(previous: dict, previous_policy: dict, proposed_name: str,
                     previous_name: str) -> tuple[dict, dict]:
    """Переносит каждый source coordinate и каждый stable membership без потерь.

    Индексы локальны для отчёта: сначала перенумеровываются оба file-id региона,
    затем сортируются координаты и пересобираются ranges всех областей. Счётчики
    намеренно копируются: schema validation обнаружит любую потерю биекции.
    """
    validate_baseline(previous)
    model.validate_policy(previous_policy)
    if previous["provenance"]["policy_hash"] != model.content_hash(previous_policy):
        raise ValueError("identity migration: исходная policy не соответствует baseline")
    proposed_policy = rename_identity(previous_policy, previous_name, proposed_name)
    model.validate_policy(proposed_policy)
    result = copy.deepcopy(previous)
    files = previous["source_files"]["universe"]
    renamed_files = [path.replace(previous_name, proposed_name) for path in files]
    sorted_files = sorted(set(renamed_files))
    if len(sorted_files) != len(files):
        raise ValueError("identity migration: неоднозначное соответствие файлов")
    file_indices = {path: index for index, path in enumerate(sorted_files)}
    remap_files = [file_indices[path] for path in renamed_files]
    result["source_files"] = {"universe": sorted_files, "hash": model.content_hash(sorted_files)}
    surface = result["stable_source"]
    surface["domains"] = rename_identity(surface["domains"], previous_name, proposed_name)
    for metric in model.METRICS:
        old_coordinates = previous["stable_source"]["coordinates"][metric]["universe"]
        coordinates = copy.deepcopy(old_coordinates)
        for coordinate in coordinates:
            for position in ((0, 3) if metric == "regions" else (0,)):
                coordinate[position] = remap_files[coordinate[position]]
        ordered = sorted(coordinates)
        coordinate_indices = {tuple(entry): index for index, entry in enumerate(ordered)}
        if len(coordinate_indices) != len(coordinates):
            raise ValueError("identity migration: неоднозначное соответствие координат")
        remap = [coordinate_indices[tuple(entry)] for entry in coordinates]
        identities = [model.coordinate_identity(metric, entry, sorted_files) for entry in ordered]
        surface["coordinates"][metric] = {
            "universe": ordered, "universe_hash": model.content_hash(identities),
        }
        for domain in surface["domains"].values():
            entry = domain[metric]
            for category in ("universe", "stable"):
                old_indices = _expand_ranges(entry[f"{category}_ranges"], len(remap), category)
                indices = sorted(remap[index] for index in old_indices)
                entry[f"{category}_ranges"] = model.ranges(indices)
                entry[f"{category}_hash"] = model.content_hash([identities[index] for index in indices])
    result["provenance"]["policy_hash"] = model.content_hash(proposed_policy)
    archive = rename_identity(result["legacy_report_only"], previous_name, proposed_name)
    archive["baseline_hash"] = model.content_hash(archive["baseline_v1"])
    result["legacy_report_only"] = archive
    result.pop("baseline_hash")
    result["baseline_hash"] = model.content_hash(result)
    validate_baseline(result)
    return result, proposed_policy


def verify_registered_migration(previous: dict, proposed: dict, previous_ledger: dict,
                                proposed_ledger: dict, previous_policy: dict,
                                proposed_policy: dict, registry: Any) -> bool:
    """Разрешает ровно один hash-pinned переход и независимо проверяет его смысл.

    Реестр не является разрешением на произвольный архив: даже совпадение всех
    хешей не заменяет проверку полного результата детерминированной миграции.
    Нет подходящей записи — вызывающий сохраняет обычную строгую update policy.
    """
    model.require_exact_keys(model.require_object(registry, "identity migrations"),
                             {"schema_version", "migrations"}, "identity migrations")
    if registry["schema_version"] != 1:
        raise ValueError("identity migration: неизвестная schema")
    inputs = {
        "previous_baseline_hash": model.content_hash(previous),
        "proposed_baseline_hash": model.content_hash(proposed),
        "previous_policy_hash": model.content_hash(previous_policy),
        "proposed_policy_hash": model.content_hash(proposed_policy),
        "previous_ledger_hash": model.content_hash(previous_ledger),
        "proposed_ledger_hash": model.content_hash(proposed_ledger),
    }
    matches = []
    for entry in model.require_array(registry["migrations"], "identity migrations.migrations"):
        model.require_exact_keys(model.require_object(entry, "identity migration"),
                                 set(inputs) | {"proposed_name"}, "identity migration")
        for key in inputs:
            if not isinstance(entry[key], str) or not re.fullmatch(r"sha256:[0-9a-f]{64}", entry[key]):
                raise ValueError("identity migration: неверный hash")
        if all(entry[key] == value for key, value in inputs.items()):
            matches.append(entry)
    if not matches:
        return False
    if len(matches) != 1:
        raise ValueError("identity migration: неоднозначная регистрация")
    proposed_name = model.require_string(matches[0]["proposed_name"], "proposed_name")
    previous_name = infer_previous_name(previous, proposed, proposed_name)
    expected, expected_policy = migrate_baseline(previous, previous_policy, proposed_name, previous_name)
    if expected != proposed or expected_policy != proposed_policy:
        raise ValueError("identity migration: изменены измерения, coverage membership или policy")
    if previous_ledger != proposed_ledger:
        raise ValueError("identity migration: журнал исключений должен сохраниться полностью")
    return True

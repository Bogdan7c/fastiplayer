#!/usr/bin/env python3
"""Проверяет зафиксированную policy Rust toolchain и MSRV workspace.

Скрипт намеренно не реализует общий semver parser. Он закрепляет принятые
точные значения manifest/toolchain и читает Cargo locked metadata, тогда как
реальная сборка CI на Rust 1.92 остаётся источником истины для crates без
объявленного `rust_version`.
"""

from __future__ import annotations

import json
import subprocess
import sys
import tomllib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping, Sequence


# Решение владельца: это единственный допустимый MSRV всего workspace.
APPROVED_MSRV = "1.92"
# Решение владельца: это закреплённый основной toolchain разработки и CI.
APPROVED_PRIMARY_TOOLCHAIN = "1.96.0"
# Единая edition должна наследоваться каждым workspace member из root manifest.
APPROVED_EDITION = "2024"
# First-party workspace members наследуют единый SPDX identifier владельца.
APPROVED_LICENSE = "MIT"
# Компоненты нужны существующим format и Clippy quality gates.
REQUIRED_TOOLCHAIN_COMPONENTS = frozenset({"clippy", "rustfmt"})
# --locked запрещает этой read-only проверке незаметно менять Cargo.lock.
METADATA_COMMAND = ("cargo", "metadata", "--locked", "--format-version", "1")


@dataclass(frozen=True)
class PolicyViolation:
    """Хранит одно понятное пользователю нарушение policy."""

    # Место нарушения помогает исправить только нужный manifest или config.
    location: str
    # Описание сохраняет проверку отделённой от terminal formatting.
    message: str

    def render(self) -> str:
        """Форматирует нарушение одинаково для CLI и будущих интеграций."""

        # Формат остаётся коротким, чтобы pre-PR output быстро указывал причину.
        return f"{self.location}: {self.message}"


def load_toml(toml_path: Path) -> Mapping[str, Any]:
    """Читает TOML manifest/config как mapping без неявных fallback-значений."""

    # Контекстный менеджер гарантирует закрытие файла и не скрывает ошибки чтения.
    with toml_path.open("rb") as toml_file:
        # tomllib даёт стандартный parser TOML вместо самописного разбора текста.
        return tomllib.load(toml_file)


def read_cargo_metadata(repository_root: Path) -> Mapping[str, Any]:
    """Запускает Cargo metadata над неизменяемым locked dependency graph."""

    # capture_output сохраняет stdout как JSON, а stderr остаётся в исключении Cargo.
    completed_process = subprocess.run(
        METADATA_COMMAND,
        cwd=repository_root,
        capture_output=True,
        check=True,
        text=True,
    )
    # JSON parser не допускает продолжения с повреждённым или неполным metadata output.
    return json.loads(completed_process.stdout)


def is_workspace_inheritance(package_table: Mapping[str, Any], field_name: str) -> bool:
    """Возвращает true только для явного Cargo workspace inheritance syntax."""

    # Cargo manifest выражает inheritance table со значением `workspace = true`.
    field_value = package_table.get(field_name)
    # Literal edition/rust-version запрещены: member обязан следовать root owner.
    return isinstance(field_value, Mapping) and field_value.get("workspace") is True


def workspace_package_by_id(metadata: Mapping[str, Any]) -> Mapping[str, Mapping[str, Any]]:
    """Индексирует только workspace members, перечисленные самим Cargo."""

    # Cargo IDs надёжнее ручного обхода crates/ и не захватывают standalone patches.
    workspace_member_ids = set(metadata.get("workspace_members", []))
    # packages может содержать registry/path dependencies, поэтому фильтруем их по ID.
    return {
        package["id"]: package
        for package in metadata.get("packages", [])
        if package.get("id") in workspace_member_ids
    }


def package_relative_path(repository_root: Path, manifest_path: Path) -> str:
    """Строит стабильный относительный путь для diagnostics."""

    # relative_to также гарантирует, что Cargo member не указывает вне repository root.
    return str(manifest_path.resolve().relative_to(repository_root.resolve()))


def check_root_manifest(repository_root: Path) -> tuple[PolicyViolation, ...]:
    """Проверяет root owner для workspace MSRV, edition и license."""

    # Корневой Cargo.toml — единственный владелец shared package policy.
    root_manifest = load_toml(repository_root / "Cargo.toml")
    # get с пустыми mapping даёт понятное violation вместо KeyError traceback.
    workspace_table = root_manifest.get("workspace", {})
    # Nested mapping защищает guard от malformed, но syntactically valid TOML.
    workspace_package = (
        workspace_table.get("package", {}) if isinstance(workspace_table, Mapping) else {}
    )
    # Список violations сохраняет diagnostics всех несоответствий за один запуск.
    violations: list[PolicyViolation] = []

    # MSRV должен быть ровно принятой строкой, без range или скрытого downgrade.
    if workspace_package.get("rust-version") != APPROVED_MSRV:
        violations.append(
            PolicyViolation(
                "Cargo.toml [workspace.package]",
                f'`rust-version` должен быть ровно "{APPROVED_MSRV}"',
            )
        )

    # Edition остаётся общим owner value, от которого наследуются все members.
    if workspace_package.get("edition") != APPROVED_EDITION:
        violations.append(
            PolicyViolation(
                "Cargo.toml [workspace.package]",
                f'`edition` должна быть ровно "{APPROVED_EDITION}"',
            )
        )

    # License задаётся только стандартным SPDX identifier без дополнительных условий.
    if workspace_package.get("license") != APPROVED_LICENSE:
        violations.append(
            PolicyViolation(
                "Cargo.toml [workspace.package]",
                f'`license` должна быть ровно "{APPROVED_LICENSE}"',
            )
        )

    # Tuple не позволяет случайно изменить результат проверки после возврата.
    return tuple(violations)


def check_toolchain_file(repository_root: Path) -> tuple[PolicyViolation, ...]:
    """Проверяет pinned primary toolchain и компоненты quality gates."""

    # rustup читает этот файл из root workspace, поэтому проверяем его напрямую.
    toolchain_config = load_toml(repository_root / "rust-toolchain.toml")
    # Некорректная верхняя структура даёт policy failure, а не AttributeError.
    toolchain_table = toolchain_config.get("toolchain", {})
    # Список позволяет сообщить все ошибки toolchain config вместе.
    violations: list[PolicyViolation] = []

    # Pin обязан быть exact: floating `stable` не воспроизводит CI environment.
    if not isinstance(toolchain_table, Mapping) or toolchain_table.get(
        "channel"
    ) != APPROVED_PRIMARY_TOOLCHAIN:
        violations.append(
            PolicyViolation(
                "rust-toolchain.toml [toolchain]",
                f'`channel` должен быть ровно "{APPROVED_PRIMARY_TOOLCHAIN}"',
            )
        )

    # Сверяем компоненты только после подтверждения table type.
    configured_components = (
        toolchain_table.get("components", [])
        if isinstance(toolchain_table, Mapping)
        else []
    )
    # Set удаляет порядок TOML list из смыслового сравнения.
    component_names = (
        set(configured_components)
        if isinstance(configured_components, list)
        else set()
    )
    # Missing component ломает локальный format/Clippy workflow, поэтому это failure.
    missing_components = REQUIRED_TOOLCHAIN_COMPONENTS.difference(component_names)
    if missing_components:
        violations.append(
            PolicyViolation(
                "rust-toolchain.toml [toolchain]",
                "отсутствуют обязательные components: "
                + ", ".join(sorted(missing_components)),
            )
        )

    # Minimal profile сохраняет toolchain declaration компактным и предсказуемым.
    if isinstance(toolchain_table, Mapping) and toolchain_table.get("profile") != "minimal":
        violations.append(
            PolicyViolation(
                "rust-toolchain.toml [toolchain]",
                '`profile` должен быть ровно "minimal"',
            )
        )

    # Tuple не позволяет вызывающему случайно испортить собранные diagnostics.
    return tuple(violations)


def check_workspace_members(
    repository_root: Path,
    metadata: Mapping[str, Any],
) -> tuple[PolicyViolation, ...]:
    """Проверяет manifest inheritance и эффективные значения каждого member."""

    # Индекс из Cargo metadata исключает standalone [replace] patch crates.
    packages_by_id = workspace_package_by_id(metadata)
    # Список violations собирается до возврата, чтобы один запуск показывал весь drift.
    violations: list[PolicyViolation] = []

    # Cargo должен вернуть package для каждого workspace member ID.
    for package_id in metadata.get("workspace_members", []):
        # Missing package указывает на неожиданный metadata format или broken Cargo output.
        package = packages_by_id.get(package_id)
        if package is None:
            violations.append(
                PolicyViolation(
                    "cargo metadata",
                    f"не найден package для workspace ID `{package_id}`",
                )
            )
            continue

        # Cargo metadata даёт canonical manifest path без ручного glob по crates/.
        manifest_path = Path(package["manifest_path"])
        # Путь нужен и как стабильная диагностика, и для точного TOML parse.
        relative_manifest_path = package_relative_path(repository_root, manifest_path)
        # Каждого member проверяем по исходному manifest, не только по resolved metadata.
        manifest = load_toml(manifest_path)
        # Missing [package] — некорректный member и отдельное понятное нарушение.
        package_table = manifest.get("package", {})
        if not isinstance(package_table, Mapping):
            violations.append(
                PolicyViolation(relative_manifest_path, "отсутствует корректная таблица `[package]`")
            )
            continue

        # Все shared поля обязаны наследоваться, а не повторяться literal strings.
        for inherited_field in ("edition", "rust-version", "license"):
            if not is_workspace_inheritance(package_table, inherited_field):
                violations.append(
                    PolicyViolation(
                        relative_manifest_path,
                        f'`package.{inherited_field}` должен быть `{inherited_field}.workspace = true`',
                    )
                )

        # Effective Cargo metadata подтверждает, что resolver увидел owner values.
        if package.get("edition") != APPROVED_EDITION:
            violations.append(
                PolicyViolation(
                    relative_manifest_path,
                    f'Cargo metadata edition должна быть "{APPROVED_EDITION}"',
                )
            )
        # Exact comparison не является semver parser и не допускает MSRV range/downgrade.
        if package.get("rust_version") != APPROVED_MSRV:
            violations.append(
                PolicyViolation(
                    relative_manifest_path,
                    f'Cargo metadata rust_version должен быть "{APPROVED_MSRV}"',
                )
            )

        # Effective metadata не должно ошибочно маркировать first-party crate другой лицензией.
        if package.get("license") != APPROVED_LICENSE:
            violations.append(
                PolicyViolation(
                    relative_manifest_path,
                    f'Cargo metadata license должна быть "{APPROVED_LICENSE}"',
                )
            )

    # Tuple делает policy result immutable для CLI и unit tests.
    return tuple(violations)


def check_metadata_workspace_root(
    repository_root: Path,
    metadata: Mapping[str, Any],
) -> tuple[PolicyViolation, ...]:
    """Проверяет, что metadata получен именно для текущего repository root."""

    # Cargo может быть запущен из неверного cwd, поэтому root входит в contract guard.
    metadata_workspace_root = metadata.get("workspace_root")
    # String path проверяем точно после normalisation симлинков.
    if (
        isinstance(metadata_workspace_root, str)
        and Path(metadata_workspace_root).resolve() == repository_root.resolve()
    ):
        return ()
    # Другая root означает, что member/policy result нельзя считать доказательством.
    return (
        PolicyViolation(
            "cargo metadata",
            f"workspace_root должен быть `{repository_root.resolve()}`",
        ),
    )


def validate_policy(
    repository_root: Path,
    metadata: Mapping[str, Any],
) -> tuple[PolicyViolation, ...]:
    """Собирает статические policy checks без terminal I/O."""

    # Последовательность отражает owners: root policy, pin, Cargo graph, members.
    return (
        *check_root_manifest(repository_root),
        *check_toolchain_file(repository_root),
        *check_metadata_workspace_root(repository_root, metadata),
        *check_workspace_members(repository_root, metadata),
    )


def repository_root_from_script() -> Path:
    """Вычисляет root от расположения scripts/, а не от cwd пользователя."""

    # parents[1] поднимается от scripts/check-toolchain-policy.py к repository root.
    return Path(__file__).resolve().parents[1]


def main(arguments: Sequence[str]) -> int:
    """Запускает CLI guard и возвращает код, пригодный для shell/CI."""

    # Аргументы пока не поддерживаются, чтобы policy не получила неявных режимов.
    if len(arguments) != 0:
        print("Ошибка: check-toolchain-policy.py не принимает аргументы.", file=sys.stderr)
        return 2

    # Root вычисляется один раз и передаётся всем functions явно.
    repository_root = repository_root_from_script()
    try:
        # Full locked metadata нужен, потому что member list является Cargo authority.
        metadata = read_cargo_metadata(repository_root)
        # Pure validator отделяет policy от subprocess и легко покрывается unit tests.
        violations = validate_policy(repository_root, metadata)
    except subprocess.CalledProcessError as error:
        # stderr Cargo содержит первопричину, поэтому показываем его, а не только exit code.
        print(f"Ошибка запуска toolchain policy: {error}", file=sys.stderr)
        if error.stderr:
            print(error.stderr.strip(), file=sys.stderr)
        return 2
    except (
        OSError,
        ValueError,
        json.JSONDecodeError,
        tomllib.TOMLDecodeError,
    ) as error:
        # Ошибка инфраструктуры не маскируется как успешная policy validation.
        print(f"Ошибка запуска toolchain policy: {error}", file=sys.stderr)
        return 2

    # Policy drift печатается после complete scan, чтобы исправления были пакетными.
    if violations:
        print("Toolchain policy нарушена:", file=sys.stderr)
        for violation in violations:
            print(f"- {violation.render()}", file=sys.stderr)
        return 1

    # Успех явно сообщает зафиксированные значения, полезные в CI log.
    print(
        "Toolchain policy OK: "
        f"MSRV {APPROVED_MSRV}, primary toolchain {APPROVED_PRIMARY_TOOLCHAIN}."
    )
    return 0


# CLI entry point отделён от logic, чтобы import в unit tests не запускал Cargo.
if __name__ == "__main__":
    # sys.exit передаёт shell именно возвращаемый policy status.
    raise SystemExit(main(sys.argv[1:]))

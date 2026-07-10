#!/usr/bin/env python3
"""Focused regression tests для Session 01 toolchain/MSRV policy."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType
from unittest.mock import patch


# Скрипт с дефисами загружается через explicit import spec.
POLICY_PATH = Path(__file__).resolve().parents[1] / "check-toolchain-policy.py"


def load_policy_module() -> ModuleType:
    """Загружает policy script как обычный Python module для unit tests."""

    # spec связывает file path с корректным module name для dataclass internals.
    module_spec = importlib.util.spec_from_file_location(
        "check_toolchain_policy",
        POLICY_PATH,
    )
    # Защитительная проверка не скрывает невозможную ошибку загрузки test fixture.
    if module_spec is None or module_spec.loader is None:
        raise RuntimeError(f"не удалось создать import spec для `{POLICY_PATH}`")
    # dataclasses ищет namespace модуля в sys.modules во время decoration.
    policy_module = importlib.util.module_from_spec(module_spec)
    # Регистрация должна произойти до exec_module по контракту dataclasses.
    sys.modules[module_spec.name] = policy_module
    # Выполняем module body; Cargo не вызывается из-за защищённого CLI entry point.
    module_spec.loader.exec_module(policy_module)
    return policy_module


# Один импорт делает проверяемые constants/functions доступными всем test cases.
POLICY = load_policy_module()


def write_policy_fixture(
    repository_root: Path,
    *,
    workspace_msrv: str = "1.92",
    member_edition: str = "edition.workspace = true",
    member_rust_version: str = "rust-version.workspace = true",
    member_license: str = "license.workspace = true",
    metadata_edition: str = "2024",
    metadata_rust_version: str = "1.92",
    metadata_license: str = "MIT",
) -> dict[str, object]:
    """Создаёт минимальный workspace и соответствующий Cargo metadata fixture."""

    # Member directory повторяет реальную workspace layout без зависимости от repository files.
    member_directory = repository_root / "crates" / "policy-fixture"
    member_directory.mkdir(parents=True)
    # Root manifest определяет именно те shared fields, которые владеют policy.
    (repository_root / "Cargo.toml").write_text(
        "[workspace]\nmembers = [\"crates/policy-fixture\"]\n\n"
        "[workspace.package]\n"
        'edition = "2024"\n'
        f'rust-version = "{workspace_msrv}"\n'
        'license = "MIT"\n',
        encoding="utf-8",
    )
    # Toolchain file повторяет expected production pin и required quality components.
    (repository_root / "rust-toolchain.toml").write_text(
        "[toolchain]\n"
        'channel = "1.96.0"\n'
        'profile = "minimal"\n'
        'components = ["rustfmt", "clippy"]\n',
        encoding="utf-8",
    )
    # Member manifest получает параметры, чтобы tests точечно создавали policy drift.
    member_manifest = member_directory / "Cargo.toml"
    member_manifest.write_text(
        "[package]\n"
        'name = "policy-fixture"\n'
        'version = "0.1.0"\n'
        f"{member_edition}\n"
        f"{member_rust_version}\n"
        f"{member_license}\n",
        encoding="utf-8",
    )
    # Cargo package ID достаточно стабилен для проверки member filtering.
    package_id = "path+file:///policy-fixture#0.1.0"
    # Fixture моделирует части cargo metadata, которыми владеет policy script.
    return {
        "workspace_root": str(repository_root),
        "workspace_members": [package_id],
        "packages": [
            {
                "id": package_id,
                "manifest_path": str(member_manifest),
                "edition": metadata_edition,
                "rust_version": metadata_rust_version,
                "license": metadata_license,
            }
        ],
    }


class ToolchainPolicyTests(unittest.TestCase):
    """Закрепляет root owner, member inheritance и locked metadata contract."""

    def test_valid_policy_has_no_violations(self) -> None:
        """Правильный root pin и оба inherited fields принимаются."""

        # Временный каталог не касается production repository и очищается автоматически.
        with tempfile.TemporaryDirectory() as temporary_directory:
            # Path делает test fixture одинаковым на Linux/macOS/Windows.
            repository_root = Path(temporary_directory)
            # Корректная fixture содержит exact policy values во всех owners.
            metadata = write_policy_fixture(repository_root)
            # Пустой tuple — единственный успешный результат pure validator.
            self.assertEqual((), POLICY.validate_policy(repository_root, metadata))

    def test_root_msrv_must_match_exact_owner_decision(self) -> None:
        """Случайный downgrade root manifest блокируется статическим guard."""

        # Новый временный root сохраняет этот негативный случай полностью изолированным.
        with tempfile.TemporaryDirectory() as temporary_directory:
            # Fixture меняет только workspace owner field.
            repository_root = Path(temporary_directory)
            # Старое literal-значение должно быть отклонено до CI compile.
            metadata = write_policy_fixture(repository_root, workspace_msrv="1.85")
            # Rendered diagnostics проверяют user-actionable reason вместо implementation detail.
            messages = [violation.render() for violation in POLICY.validate_policy(repository_root, metadata)]
            self.assertTrue(any('`rust-version` должен быть ровно "1.92"' in message for message in messages))

    def test_member_must_inherit_both_workspace_fields(self) -> None:
        """Literal member edition или rust-version не обходят root owner."""

        # Новый временный root держит намеренно некорректный manifest вне project files.
        with tempfile.TemporaryDirectory() as temporary_directory:
            # Fixture replaces both Cargo inheritance tables with literals.
            repository_root = Path(temporary_directory)
            # Effective metadata stays valid to prove that raw syntax is checked separately.
            metadata = write_policy_fixture(
                repository_root,
                member_edition='edition = "2024"',
                member_rust_version='rust-version = "1.92"',
            )
            # Diagnostics must name both missing inheritance fields.
            messages = [violation.render() for violation in POLICY.validate_policy(repository_root, metadata)]
            self.assertTrue(any("package.edition" in message for message in messages))
            self.assertTrue(any("package.rust-version" in message for message in messages))

    def test_member_must_inherit_mit_license(self) -> None:
        """Literal или отсутствующая license не обходят first-party owner."""

        with tempfile.TemporaryDirectory() as temporary_directory:
            repository_root = Path(temporary_directory)
            metadata = write_policy_fixture(
                repository_root,
                member_license='license = "Apache-2.0"',
                metadata_license="Apache-2.0",
            )

            messages = [
                violation.render()
                for violation in POLICY.validate_policy(repository_root, metadata)
            ]

            self.assertTrue(any("package.license" in message for message in messages))
            self.assertTrue(any("Cargo metadata license" in message for message in messages))

    def test_effective_metadata_must_match_workspace_msrv(self) -> None:
        """Resolved Cargo value drift не маскируется корректным raw TOML syntax."""

        # Новый временный root делает metadata-only failure детерминированным.
        with tempfile.TemporaryDirectory() as temporary_directory:
            # Fixture keeps manifests valid but models unexpected Cargo resolved metadata.
            repository_root = Path(temporary_directory)
            # This catches a resolver/metadata result that is inconsistent with root policy.
            metadata = write_policy_fixture(repository_root, metadata_rust_version="1.85")
            # Diagnostics must identify metadata rather than claim the manifest syntax is wrong.
            messages = [violation.render() for violation in POLICY.validate_policy(repository_root, metadata)]
            self.assertTrue(any("Cargo metadata rust_version" in message for message in messages))

    def test_metadata_reader_uses_locked_cargo_command(self) -> None:
        """Guard never refreshes Cargo.lock while it reads the dependency graph."""

        # Новый временный root делает subprocess cwd assertion явным.
        with tempfile.TemporaryDirectory() as temporary_directory:
            # Fixture root need not contain a real Cargo project because subprocess is mocked.
            repository_root = Path(temporary_directory)
            # Minimal valid JSON is sufficient to verify reader plumbing.
            completed_process = subprocess.CompletedProcess(
                POLICY.METADATA_COMMAND,
                0,
                stdout=json.dumps({"workspace_members": [], "packages": []}),
                stderr="",
            )
            # Mock prevents the focused unit test from running actual Cargo.
            with patch.object(POLICY.subprocess, "run", return_value=completed_process) as run:
                # Reader result remains JSON mapping exposed to pure validator.
                metadata = POLICY.read_cargo_metadata(repository_root)
            # Exact command assertion protects the critical --locked invariant.
            run.assert_called_once_with(
                POLICY.METADATA_COMMAND,
                cwd=repository_root,
                capture_output=True,
                check=True,
                text=True,
            )
            # Loaded JSON proves stdout was parsed instead of merely checking exit status.
            self.assertEqual([], metadata["workspace_members"])


# Standard unittest entry point supports direct focused execution.
if __name__ == "__main__":
    # unittest owns result formatting and exit code for shell/CI callers.
    unittest.main()

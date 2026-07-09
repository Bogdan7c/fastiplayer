#!/usr/bin/env python3
"""Regression tests для dependency policy из check-refactor-guardrails.py."""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from types import ModuleType


GUARDRAIL_PATH = Path(__file__).resolve().parents[1] / "check-refactor-guardrails.py"
EXPECTED_NEUTRAL_TEMPO_OWNERS = frozenset({"audio-core", "player-core"})
EXPECTED_CONCRETE_TEMPO_DEPENDENCIES = frozenset(
    {
        "audio-signalsmith",
        "audio-timestretch",
        "signalsmith-stretch",
        "timestretch",
    }
)


def load_guardrail_module() -> ModuleType:
    """Загружает скрипт с дефисами в имени как обычный Python module."""

    module_spec = importlib.util.spec_from_file_location(
        "check_refactor_guardrails",
        GUARDRAIL_PATH,
    )
    if module_spec is None or module_spec.loader is None:
        raise RuntimeError(f"не удалось создать import spec для `{GUARDRAIL_PATH}`")

    guardrail_module = importlib.util.module_from_spec(module_spec)
    # `dataclasses` ищет module namespace через `sys.modules` во время декорирования.
    sys.modules[module_spec.name] = guardrail_module
    module_spec.loader.exec_module(guardrail_module)
    return guardrail_module


GUARDRAIL = load_guardrail_module()


def package_with_dependencies(
    package_name: str,
    dependencies: tuple[tuple[str, str | None], ...],
) -> dict[str, object]:
    """Строит минимальный cargo-metadata package для focused policy test."""

    return {
        "name": package_name,
        "dependencies": [
            {"name": dependency_name, "kind": dependency_kind}
            for dependency_name, dependency_kind in dependencies
        ],
    }


class TempoDependencyGuardrailTests(unittest.TestCase):
    """Закрепляет нейтральную tempo boundary без запрета composition graph."""

    def test_neutral_crates_reject_every_concrete_tempo_dependency_kind(self) -> None:
        """Normal/dev/build edges одинаково запрещены для обоих neutral owners."""

        self.assertEqual(EXPECTED_NEUTRAL_TEMPO_OWNERS, GUARDRAIL.TEMPO_NEUTRAL_CRATES)
        self.assertEqual(
            EXPECTED_CONCRETE_TEMPO_DEPENDENCIES,
            GUARDRAIL.TEMPO_NEUTRAL_FORBIDDEN_DEPENDENCIES,
        )
        dependency_kinds: tuple[tuple[str, str | None], ...] = (
            ("normal", None),
            ("dev", "dev"),
            ("build", "build"),
        )

        for owner in sorted(EXPECTED_NEUTRAL_TEMPO_OWNERS):
            for dependency_name in sorted(EXPECTED_CONCRETE_TEMPO_DEPENDENCIES):
                for kind_label, dependency_kind in dependency_kinds:
                    with self.subTest(
                        owner=owner,
                        dependency=dependency_name,
                        kind=kind_label,
                    ):
                        packages = {
                            owner: package_with_dependencies(
                                owner,
                                ((dependency_name, dependency_kind),),
                            )
                        }
                        normal_dependencies = GUARDRAIL.direct_normal_dependencies(packages)
                        all_dependencies = GUARDRAIL.direct_all_manifest_dependencies(packages)
                        violations = GUARDRAIL.find_dependency_violations(
                            normal_dependencies,
                            all_dependencies,
                            frozenset(),
                        )

                        self.assertTrue(
                            any(
                                violation.owner == owner
                                and violation.dependency == dependency_name
                                for violation in violations
                            ),
                            msg=(
                                f"ожидался запрет {kind_label} dependency "
                                f"{owner} -> {dependency_name}"
                            ),
                        )

    def test_composition_root_and_concrete_adapter_edges_remain_allowed(self) -> None:
        """Policy не превращается в workspace-global запрет runtime composition."""

        dependency_map = {
            "app-egui": frozenset({"audio-signalsmith"}),
            "audio-signalsmith": frozenset({"audio-core", "signalsmith-stretch"}),
            "audio-timestretch": frozenset({"audio-core", "timestretch"}),
        }

        violations = GUARDRAIL.find_dependency_violations(
            dependency_map,
            dependency_map,
            frozenset({"audio-signalsmith"}),
        )

        self.assertEqual([], violations)


if __name__ == "__main__":
    unittest.main()

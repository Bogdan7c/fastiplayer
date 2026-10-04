#!/usr/bin/env python3
"""Focused static tests для S42 release-runner composition contract."""

# re извлекает exact Bash case branch без исполнения дорогих checks.
import re
# pathlib вычисляет repository paths относительно test-файла.
from pathlib import Path
# unittest предоставляет hermetic stdlib test runner.
import unittest


# Корень репозитория находится на два уровня выше scripts/tests/.
REPO_ROOT = Path(__file__).resolve().parents[2]


# Функция читает versioned script как UTF-8 contract artifact.
def read_script(script_name: str) -> str:
    """Возвращает текст одного scripts/ launcher-а."""

    # Exact path не использует cwd и не запускает shell.
    return (REPO_ROOT / "scripts" / script_name).read_text(encoding="utf-8")


# Тесты закрепляют composition без дублирования Cargo command owners.
class S42ReleaseRunnerTests(unittest.TestCase):
    """Проверяет полный automated S42 entrypoint."""

    # `all` обязан вызывать полный exact owner set в утверждённом порядке.
    def test_ci_all_runs_exact_owner_sequence(self):
        """Удаление, добавление или перестановка blocking owner-а ломает contract."""

        # CI script читается только как text artifact.
        ci_script = read_script("ci-checks.sh")
        # Non-greedy branch заканчивается на ближайшем shell case terminator.
        all_branch_match = re.search(
            r"(?ms)^[ \t]*all\)\n(?P<body>.*?)^[ \t]*;;[ \t]*$",
            ci_script,
        )
        # Исчезновение именованного branch является понятным contract failure.
        self.assertIsNotNone(all_branch_match)
        # Type narrowing следует после assertion.
        assert all_branch_match is not None
        # Anchored whole-line regex не принимает упоминания owner-а в комментариях.
        actual_owner_sequence = tuple(
            re.findall(
                r"(?m)^[ \t]*(run_[a-z0-9_]+)[ \t]*$",
                all_branch_match.group("body"),
            )
        )
        # Exact tuple одновременно закрепляет полный owner set и порядок fail-fast gates.
        expected_owner_sequence = (
            "run_format_guardrails",
            "run_dependencies",
            "run_dependency_patch_direct_tests",
            "run_dependency_patches",
            "run_clippy",
            "run_docs",
            "run_tests",
            "run_app_no_default_features",
            "run_msrv",
        )
        # Sequence comparison даёт читаемый diff при missing, extra или moved owner-е.
        self.assertEqual(
            actual_owner_sequence,
            expected_owner_sequence,
            "ветка `all)` обязана сохранять exact ordered blocking owner sequence",
        )

    # Семь standalone forks обязаны оставаться exact, без glob или пропущенного lockfile.
    def test_ci_runner_lists_every_standalone_dependency_patch_manifest(self):
        """Full local gate запускает direct suite всех versioned patches."""

        # Script является единственным локальным owner-ом порядка команд.
        ci_script = read_script("ci-checks.sh")
        # Exact paths совпадают с checked-in dependency patch inventory.
        expected_manifests = (
            "crates/cros-libva-patch/Cargo.toml",
            "crates/cros-codecs-patch/Cargo.toml",
            "crates/symphonia-format-caf-patch/Cargo.toml",
            "crates/symphonia-format-isomp4-patch/Cargo.toml",
            "crates/symphonia-codec-aac-patch/Cargo.toml",
            "crates/symphonia-format-mkv-patch/Cargo.toml",
            "crates/wayland-scanner-patch/Cargo.toml",
        )
        # Каждая manifest identity проверяется отдельно для точной diagnostics.
        for expected_manifest in expected_manifests:
            # Subtest сразу называет потерянный standalone owner.
            with self.subTest(expected_manifest=expected_manifest):
                # Literal path запрещает случайный filesystem auto-discovery.
                self.assertIn(expected_manifest, ci_script)
        # Direct command обязан использовать собственный manifest и lockfile.
        self.assertIn('--manifest-path "${patch_manifest}"', ci_script)
        self.assertRegex(
            ci_script,
            r'--manifest-path "\$\{patch_manifest\}"\s+\\\s+--locked',
        )

    # Primary и MSRV releases должны оставаться exact.
    def test_ci_runner_pins_primary_and_msrv_releases(self):
        """Toolchain aliases не зависят от ambient rustup override."""

        # CI script хранит оба semver release как readonly constants.
        ci_script = read_script("ci-checks.sh")
        # Primary release соответствует accepted S42 toolchain.
        self.assertIn('readonly PRIMARY_RUST_TOOLCHAIN="1.99.0"', ci_script)
        # MSRV release соответствует workspace rust-version contract.
        self.assertIn('readonly MSRV_RUST_TOOLCHAIN="1.99.0"', ci_script)
        # Primary compile commands обязаны использовать explicit rustup selector.
        self.assertIn('cargo +"${PRIMARY_RUST_TOOLCHAIN}" clippy', ci_script)
        # MSRV compile обязан использовать отдельный selector.
        self.assertIn('cargo +"${MSRV_RUST_TOOLCHAIN}" check --workspace --locked', ci_script)

    # Cargo resolution commands должны быть locked.
    def test_supported_cargo_resolution_commands_are_locked(self):
        """Release gate не может незаметно обновить Cargo.lock."""

        # Основной CI owner содержит все compile/policy commands.
        ci_script = read_script("ci-checks.sh")
        # Exact anchors перечисляют поддерживающие --locked commands.
        required_locked_anchors = (
            'metadata --locked --no-deps',
            'deny --locked check advisories',
            'deny --locked check licenses bans sources',
            'clippy --workspace --all-targets --all-features --locked',
            'doc --workspace --all-features --no-deps --locked',
            'test --workspace --all-features --locked',
            'check -p app-egui --no-default-features --locked',
            'check --workspace --locked',
        )
        # Каждый command contract проверяется отдельно для точной diagnostics.
        for required_anchor in required_locked_anchors:
            # Subtest называет отсутствующий anchor.
            with self.subTest(required_anchor=required_anchor):
                # Anchor обязан присутствовать буквально.
                self.assertIn(required_anchor, ci_script)

    # Final launcher должен переиспользовать owners и не запускать manual inputs.
    def test_final_acceptance_reuses_owners_and_keeps_manual_not_run(self):
        """Automated gate не выдаёт отсутствие URL fixtures за PASS."""

        # Final launcher читается как declarative composition.
        final_script = read_script("final-acceptance.sh")
        # Существующий CI owner запускается целиком.
        self.assertIn('"${SCRIPT_DIRECTORY}/ci-checks.sh" all', final_script)
        # Manual status явно остаётся NOT RUN без user inputs.
        self.assertIn("manual opt-in acceptance: NOT RUN", final_script)
        # Network/manual runner не должен быть вызван автоматическим gate.
        self.assertNotIn('"${SCRIPT_DIRECTORY}/progressive-web-smoke.sh"', final_script)


# Прямой запуск удобен для focused local verification.
if __name__ == "__main__":
    # unittest управляет process status.
    unittest.main()

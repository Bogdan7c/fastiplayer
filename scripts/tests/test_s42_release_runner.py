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

    # Coverage check обязан иметь v2 preflight, общий runner и post-suite stable ratchet.
    def test_coverage_check_composes_stable_preflight_suite_and_ratchet(self):
        """Coverage owner не может вернуть legacy aggregate в blocking execution path."""

        # Coverage script читается как declarative composition без запуска LLVM suite.
        coverage_script = read_script("coverage.sh")
        # Main body ограничивает assertions реальным runtime path, а не help-текстом.
        main_match = re.search(
            r"(?ms)^main\(\) \{\n(?P<body>.*?)^\}$",
            coverage_script,
        )
        # Исчезновение единственного entrypoint является точным contract failure.
        self.assertIsNotNone(
            main_match,
            "coverage.sh обязан содержать bounded `main()` runtime body",
        )
        # Type narrowing следует после assertion.
        assert main_match is not None
        # Дальнейшие matches не могут случайно найти одноимённые строки вне main.
        main_body = main_match.group("body")

        # Exact calls отражают три разных ownership boundary.
        preflight_position = main_body.find("validate_stable_check_inputs || return 2")
        suite_position = main_body.find("run_stable_coverage_suite || return 2")
        diagnostics_position = main_body.find("publish_legacy_diagnostics || return 2")
        ratchet_position = main_body.find("run_stable_check")
        # Check проходит preflight, measurement и diagnostics до blocking decision.
        self.assertTrue(
            0 <= preflight_position < suite_position < diagnostics_position < ratchet_position,
            "stable preflight/suite/report-only diagnostics/ratchet обязаны идти в exact порядке",
        )
        # Legacy aggregator остаётся только generate boundary, но не решает exit check-а.
        self.assertNotIn(
            'coverage_metrics.py" check',
            coverage_script,
            "legacy coverage_metrics.py check запрещён в v2 execution gate",
        )
        # Stable checker обязан получать cohort и отдельный measurement-exceptions manifest.
        self.assertIn('coverage_stability.py" check', coverage_script)
        self.assertIn('--cohort "${STABLE_ARTIFACT_DIRECTORY}/cohort.json"', coverage_script)
        self.assertIn('--measurement-exceptions "${MEASUREMENT_EXCEPTIONS_PATH}"', coverage_script)

    # Raw LCOV обязан валидироваться после export и до публикации HTML/cohort.
    def test_coverage_runner_rejects_counter_underflow_before_publication(self):
        """Повреждённый detached-worker profile не может стать release artifact."""

        # Python runner является владельцем последовательных raw report boundaries.
        runner_script = read_script("coverage_runner.py")
        # LCOV сначала должен быть экспортирован из merged profdata.
        lcov_export_position = runner_script.find('self.cargo_report("--lcov"')
        # Pure parser обязан проверить raw counters, а не compact summary.
        lcov_validation_position = runner_script.find('"validate-lcov"')
        # HTML нельзя публиковать из уже известного повреждённого profile.
        html_export_position = runner_script.find('self.cargo_report("--html"')
        # Все три executable anchors обязательны и идут в exact порядке.
        self.assertTrue(
            0 <= lcov_export_position < lcov_validation_position < html_export_position,
            "LCOV export обязан пройти validate-lcov до HTML/cohort publication",
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
        self.assertIn('readonly PRIMARY_RUST_TOOLCHAIN="1.96.0"', ci_script)
        # MSRV release соответствует workspace rust-version contract.
        self.assertIn('readonly MSRV_RUST_TOOLCHAIN="1.92.0"', ci_script)
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
        # Stable runner теперь владеет direct Cargo execution boundary.
        coverage_runner = read_script("coverage_runner.py")
        # Exact argv sequence закрепляет один locked workspace graph для build и трёх runs.
        self.assertRegex(
            coverage_runner,
            r'(?s)run_arguments = \[.*?"test",.*?"--workspace",.*?'
            r'"--all-features",.*?"--locked",.*?"--no-fail-fast",.*?\]',
        )
        # Ambient serialization запрещена approved normal-concurrency methodology.
        self.assertNotIn('RUST_TEST_THREADS=1', coverage_runner)

    # Final launcher должен переиспользовать owners и не запускать manual inputs.
    def test_final_acceptance_reuses_owners_and_keeps_manual_not_run(self):
        """Automated gate не выдаёт отсутствие URL fixtures за PASS."""

        # Final launcher читается как declarative composition.
        final_script = read_script("final-acceptance.sh")
        # Существующий CI owner запускается целиком.
        self.assertIn('"${SCRIPT_DIRECTORY}/ci-checks.sh" all', final_script)
        # Существующий coverage owner запускает blocking ratchet.
        self.assertIn('"${SCRIPT_DIRECTORY}/coverage.sh" check', final_script)
        # Manual status явно остаётся NOT RUN без user inputs.
        self.assertIn("manual opt-in acceptance: NOT RUN", final_script)
        # Network/manual runner не должен быть вызван автоматическим gate.
        self.assertNotIn('"${SCRIPT_DIRECTORY}/progressive-web-smoke.sh"', final_script)


# Прямой запуск удобен для focused local verification.
if __name__ == "__main__":
    # unittest управляет process status.
    unittest.main()

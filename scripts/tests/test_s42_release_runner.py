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

    # Coverage check обязан иметь preflight, общий suite dispatch и post-suite ratchet.
    def test_coverage_check_composes_preflight_shared_suite_and_ratchet(self):
        """Coverage owner не может пропустить validation, измерение либо blocking check."""

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

        # Check-only preflight обязан вызывать pure baseline validation.
        preflight_match = re.search(
            r'(?ms)^[ \t]*if \[\[ "\$1" == "check" \]\]; then[ \t]*\n'
            r"(?P<body>.*?)^[ \t]*fi[ \t]*$",
            main_body,
        )
        # Missing либо переименованный check preflight ломает fail-fast contract.
        self.assertIsNotNone(
            preflight_match,
            "`coverage.sh check` обязан иметь отдельный validate-baseline preflight",
        )
        # Type narrowing следует после assertion.
        assert preflight_match is not None
        # Command regex anchored на whole line и не совпадает с комментариями.
        preflight_commands = re.findall(
            r'(?m)^[ \t]*python3 "\$\{SCRIPT_DIRECTORY\}/coverage_metrics\.py" '
            r"validate-baseline[ \t]*$",
            preflight_match.group("body"),
        )
        # Ровно один preflight не допускает удаления либо случайного duplicate run.
        self.assertEqual(
            len(preflight_commands),
            1,
            "`coverage.sh check` обязан один раз выполнить validate-baseline до suite",
        )

        # Public check/baseline/report modes обязаны делить один clean measurement owner.
        suite_dispatch_match = re.search(
            r'(?ms)^[ \t]*case "\$1" in[ \t]*\n'
            r".*?"
            r"^[ \t]*check\|baseline\|report\)[ \t]*\n"
            r"(?P<body>.*?)^[ \t]*;;[ \t]*$",
            main_body,
        )
        # Изменение selector-а либо исчезновение shared arm ломает composition contract.
        self.assertIsNotNone(
            suite_dispatch_match,
            "check/baseline/report обязаны делить один case arm coverage suite",
        )
        # Type narrowing следует после assertion.
        assert suite_dispatch_match is not None
        # Preflight обязан завершиться раньше входа в общий dispatch.
        self.assertLess(
            preflight_match.end(),
            suite_dispatch_match.start(),
            "validate-baseline обязан выполняться до coverage suite dispatch",
        )
        # Только реальные whole-line run_* calls считаются dispatch owners.
        shared_suite_calls = tuple(
            re.findall(
                r"(?m)^[ \t]*(run_[a-z0-9_]+)[ \t]*$",
                suite_dispatch_match.group("body"),
            )
        )
        # Shared arm обязан делегировать exact clean suite owner-у.
        self.assertEqual(
            shared_suite_calls,
            ("run_clean_coverage_suite",),
            "shared coverage arm обязан вызывать только run_clean_coverage_suite",
        )

        # Blocking ratchet обязан находиться после полного case dispatch.
        case_end_match = re.search(r"(?m)^[ \t]*esac[ \t]*$", main_body)
        # Без terminator-а нельзя доказать post-dispatch положение ratchet-а.
        self.assertIsNotNone(
            case_end_match,
            "coverage.sh main обязан содержать case terminator до ratchet",
        )
        # Type narrowing следует после assertion.
        assert case_end_match is not None
        # Exact command использует freshly generated LLVM summary.
        ratchet_match = re.search(
            r'(?m)^[ \t]*python3 "\$\{SCRIPT_DIRECTORY\}/coverage_metrics\.py" '
            r'check --input "\$\{LLVM_SUMMARY_PATH\}"[ \t]*$',
            main_body,
        )
        # Missing либо изменённый input path должен дать точную diagnostics.
        self.assertIsNotNone(
            ratchet_match,
            "coverage.sh check обязан применять coverage_metrics.py check к LLVM summary",
        )
        # Type narrowing следует после assertion.
        assert ratchet_match is not None
        # Позиционное сравнение не позволяет перенести ratchet до измерения.
        self.assertGreater(
            ratchet_match.start(),
            case_end_match.end(),
            "coverage ratchet обязан выполняться после shared suite dispatch",
        )

    # Raw LCOV обязан валидироваться после export и до публикации HTML/baseline.
    def test_coverage_suite_rejects_counter_underflow_before_publication(self):
        """Повреждённый detached-worker profile не может стать release artifact."""

        # Coverage script остаётся единственным владельцем report composition.
        coverage_script = read_script("coverage.sh")
        # Whole function extraction запрещает совпадение только в help/comment.
        suite_match = re.search(
            r"(?ms)^run_clean_coverage_suite\(\) \{\n(?P<body>.*?)^\}$",
            coverage_script,
        )
        # Missing function является отдельным wiring failure.
        self.assertIsNotNone(suite_match)
        # Type narrowing следует после assertion.
        assert suite_match is not None
        # Позиции exact executable anchors фиксируют fail-closed порядок.
        suite_body = suite_match.group("body")
        # LCOV сначала должен быть экспортирован из merged profdata.
        lcov_export_position = suite_body.find("llvm-cov report --lcov")
        # Pure parser обязан проверить raw counters, а не compact summary.
        lcov_validation_position = suite_body.find(
            'coverage_metrics.py" validate-lcov'
        )
        # HTML нельзя публиковать из уже известного повреждённого profile.
        html_export_position = suite_body.find("llvm-cov report --html")
        # Все три executable anchors обязательны и идут в exact порядке.
        self.assertTrue(
            0 <= lcov_export_position < lcov_validation_position < html_export_position,
            "LCOV export обязан пройти validate-lcov до HTML/baseline/ratchet",
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
        # Coverage test suite тоже является Cargo resolution boundary.
        coverage_script = read_script("coverage.sh")
        # Instrumented suite обязана использовать locked workspace graph.
        self.assertIn(
            'llvm-cov --workspace --all-features --locked --no-fail-fast',
            coverage_script,
        )

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

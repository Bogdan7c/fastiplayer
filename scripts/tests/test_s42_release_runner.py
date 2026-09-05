#!/usr/bin/env python3
"""Focused static tests для S42 release-runner composition contract."""

# re извлекает exact Bash case branch без исполнения дорогих checks.
import re
# pathlib вычисляет repository paths относительно test-файла.
from pathlib import Path
# sys добавляет sibling test-support module для package/direct discovery modes.
import sys
# unittest предоставляет hermetic stdlib test runner.
import unittest


# Корень репозитория находится на два уровня выше scripts/tests/.
REPO_ROOT = Path(__file__).resolve().parents[2]
# Оба unittest entrypoint-а получают один exact sibling import path.
sys.path.insert(0, str(Path(__file__).resolve().parent))
# Pure helper владеет constrained YAML selected-key/indentation validation.
import coverage_workflow_contract as WORKFLOW_CONTRACT  # noqa: E402
import coverage_split_workflow_contract as SPLIT_CONTRACT  # noqa: E402


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
    def test_manual_coverage_and_required_baseline_policy_are_separate(self):
        """Реальные workflows не возвращают дорогой gate в обычный push/PR."""
        main = (REPO_ROOT / '.github/workflows/ci.yml').read_text()
        manual = (REPO_ROOT / '.github/workflows/coverage.yml').read_text()
        SPLIT_CONTRACT.coverage_validation_document(main, manual)
        mutations = [
            (main + '\n# scripts/coverage.sh check\n', manual),
            (main, manual.replace('  workflow_dispatch:', '  push:')),
            (main, manual.replace('  workflow_dispatch:', '  workflow_dispatch:\n  pull_request:')),
            (main.replace('    name: Coverage baseline policy\n', '    name: Coverage baseline policy\n    if: false\n'), manual),
            (main.replace('    name: Coverage baseline policy\n', '    name: Coverage baseline policy\n    continue-on-error: true\n'), manual),
            (main.replace('    name: Coverage baseline policy\n', '    name: Coverage baseline policy\n    name: Shadow\n'), manual),
            (main.replace('  pull_request:', '  workflow_dispatch:'), manual),
            (main.replace('--previous-measurement-exceptions /tmp/coverage-previous-measurement-exceptions.json', '--previous-measurement-exceptions coverage/measurement-exceptions.json'), manual),
            (main, manual.replace('scripts/coverage.sh check', 'scripts/coverage.sh report')),
            (main, manual.replace('    name: Coverage ratchet\n', '    name: Coverage ratchet\n    continue-on-error: true\n')),
        ]
        schema_header = '      - name: Validate tracked coverage policy\n'
        for override in ['if: false', 'continue-on-error: true', 'shell: bash {0}']:
            mutations.append((main.replace(schema_header, schema_header + f'        {override}\n'), manual))
        for command in [
            'python3 scripts/coverage_stability.py validate --kind baseline --input coverage/baseline.json',
            'python3 scripts/coverage_stability.py validate --kind measurement-exceptions --input coverage/measurement-exceptions.json',
        ]:
            mutations.append((main.replace('          ' + command + '\n', ''), manual))
        mutations.append((main.replace(schema_header, schema_header + schema_header), manual))
        mutations.append((main.replace('    name: Coverage baseline policy\n', '    name: Coverage baseline policy\n    defaults:\n      run:\n        shell: bash {0}\n'), manual))
        for index, (changed_main, changed_manual) in enumerate(mutations):
            with self.subTest(mutation=index):
                with self.assertRaises(WORKFLOW_CONTRACT.CoverageWorkflowContractError):
                    SPLIT_CONTRACT.coverage_validation_document(changed_main, changed_manual)

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

        # Workflow является отдельной границей: он передаёт base/current pair в pure v2 validator.
        coverage_workflow = SPLIT_CONTRACT.coverage_validation_document(
            (REPO_ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8"),
            (REPO_ROOT / ".github/workflows/coverage.yml").read_text(encoding="utf-8"),
        )
        # Production workflow обязан пройти тот же strict validator, что и mutations ниже.
        WORKFLOW_CONTRACT.validate_coverage_workflow_contract(coverage_workflow)
        # Comment-only pull_request body остаётся семантически unfiltered trigger-ом.
        commented_pull_request = coverage_workflow.replace(
            "  pull_request:\n",
            "  pull_request:\n    # Все PR остаются включены без paths/branches filters.\n",
            1,
        )
        # Constrained style oracle не должен отвергать безопасную документацию trigger-а.
        WORKFLOW_CONTRACT.validate_coverage_workflow_contract(commented_pull_request)
        # Соседний canonical job с uppercase/underscore не принадлежит coverage body.
        workflow_with_unrelated_job = (
            coverage_workflow
            + "\n  Z_unrelated:\n"
            "    name: Unrelated contract fixture\n"
            "    runs-on: ubuntu-24.04\n"
            "    steps:\n"
            "      - run: true\n"
        )
        # Job-key grammar helper-а и coverage lookahead обязаны оставаться согласованными.
        WORKFLOW_CONTRACT.validate_coverage_workflow_contract(workflow_with_unrelated_job)

        # Mutation helper требует один exact anchor, чтобы fixture не стал silently stale.
        def replace_workflow_anchor(anchor: str, replacement: str) -> str:
            # Multiple/missing occurrences означают, что adversarial fixture потерял точность.
            self.assertEqual(coverage_workflow.count(anchor), 1)
            # Единственная замена моделирует один конкретный опасный workflow regression.
            return coverage_workflow.replace(anchor, replacement, 1)

        # Missing/wrong PR condition не должен запускать update step на push event-е.
        missing_pr_condition = replace_workflow_anchor(
            "        if: github.event_name == 'pull_request'\n",
            "",
        )
        # `always()` моделирует ошибочное расширение update-policy на event без base pair.
        wrong_pr_condition = replace_workflow_anchor(
            "        if: github.event_name == 'pull_request'\n",
            "        if: always()\n",
        )
        # Root trigger обязан существовать независимо от condition внутри update step-а.
        missing_pull_request_trigger = replace_workflow_anchor("  pull_request:\n", "")
        # Другой event не создаёт PR status и не предоставляет проверенный base ref.
        replaced_pull_request_trigger = replace_workflow_anchor(
            "  pull_request:\n",
            "  workflow_dispatch:\n",
        )
        # Paths filter оставил бы часть PR без blocking baseline-update проверки.
        filtered_pull_request_trigger = replace_workflow_anchor(
            "  pull_request:\n",
            "  pull_request:\n    paths:\n      - 'coverage/**'\n",
        )
        # Canonical duplicate root owner может полностью shadow-ить проверенный mapping.
        duplicate_root_owners = {
            "duplicate-root-on": coverage_workflow + "\non:\n  workflow_dispatch:\n",
            "duplicate-root-jobs": coverage_workflow + "\njobs:\n  shadow:\n    name: Shadow\n",
        }
        # Noncanonical root spellings fail closed вместо попытки реализовать весь YAML parser.
        noncanonical_root_owners = {
            "quoted-root-jobs": coverage_workflow + '\n"jobs":\n  shadow:\n',
            "spaced-root-jobs": coverage_workflow + "\njobs :\n  shadow:\n",
            "explicit-root-jobs": coverage_workflow + "\n? jobs\n:\n  shadow:\n",
            "anchored-root-jobs": coverage_workflow + "\n&shadow jobs:\n  shadow:\n",
            "quoted-root-on": coverage_workflow + '\n"on":\n  workflow_dispatch:\n',
            "spaced-root-on": coverage_workflow + "\non :\n  workflow_dispatch:\n",
        }
        # Alternate YAML spelling ребёнка jobs не может создать второй coverage owner.
        noncanonical_coverage_jobs = {
            "duplicate-canonical-coverage-job": (
                coverage_workflow
                + "\n  coverage:\n"
                "    name: Duplicate coverage status\n"
                "    runs-on: ubuntu-24.04\n"
            ),
            **{
                f"noncanonical-{spelling_name}-coverage-job": (
                    coverage_workflow
                    + f"\n  {key_spelling}\n"
                    "    name: Spaced shadow job\n"
                    "    runs-on: ubuntu-24.04\n"
                )
                for spelling_name, key_spelling in (
                    ("plain-spaced-colon", "coverage :"),
                    ("single-quoted", "'coverage':"),
                    ("double-quoted-unicode", '"cover\\u0061ge":'),
                    ("explicit", "? coverage\n  :"),
                    ("anchored", "&shadow coverage:"),
                )
            },
        }
        # Job-level status controls не могут выключить либо условно пропустить ratchet.
        coverage_job_status_mutations = {
            "coverage-job-continue-on-error": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n    continue-on-error: true\n",
            ),
            "coverage-job-if-false": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n    if: false\n",
            ),
            "coverage-job-needs": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n    needs: vertical-seek-acceptance\n",
            ),
            "coverage-job-empty-strategy": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n"
                "    strategy:\n"
                "      matrix:\n"
                "        shard: []\n",
            ),
            "duplicate-coverage-steps-owner": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n    steps:\n",
            ),
        }
        # Ambient serialization на любом workflow scope ломает normal-concurrency cohort.
        serialized_coverage_mutations = {
            "root-rust-test-threads": replace_workflow_anchor(
                "  CARGO_TERM_COLOR: always\n",
                '  CARGO_TERM_COLOR: always\n  RUST_TEST_THREADS: "1"\n',
            ),
            "root-encoded-rust-test-threads": replace_workflow_anchor(
                "  CARGO_TERM_COLOR: always\n",
                '  CARGO_TERM_COLOR: always\n  "RUST_TEST_\\u0054HREADS": "1"\n',
            ),
            "coverage-job-rust-test-threads": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n"
                "    env:\n"
                '      RUST_TEST_THREADS: "1"\n',
            ),
            "measured-step-rust-test-threads": replace_workflow_anchor(
                "      - name: Run clean coverage suite and ratchet\n",
                "      - name: Run clean coverage suite and ratchet\n"
                "        env:\n"
                '          RUST_TEST_THREADS: "1"\n',
            ),
        }
        # Unique env owner anchor включает first-party explanatory comment.
        update_env_owner = (
            "        env:\n"
            "          # Base ref поступает от GitHub event и используется только для чтения versioned JSON.\n"
        )
        # Missing env mapping оставляет orphan child key и обязан fail-closed отклоняться.
        missing_env_owner = replace_workflow_anchor(
            update_env_owner,
            update_env_owner.removeprefix("        env:\n"),
        )
        # Wrong `with` owner не является environment mapping для shell process-а.
        wrong_env_owner = replace_workflow_anchor(
            update_env_owner,
            update_env_owner.replace("        env:\n", "        with:\n", 1),
        )
        # Duplicate env mapping моделирует YAML override exact base-ref owner-а.
        duplicate_env_owner = replace_workflow_anchor(
            update_env_owner,
            "        env:\n        env:\n"
            "          # Base ref поступает от GitHub event и используется только для чтения versioned JSON.\n",
        )
        # Quoted child key с separation space также override-ит base-ref env value.
        duplicate_spaced_base_ref = replace_workflow_anchor(
            "          COVERAGE_BASE_REF: origin/${{ github.base_ref }}\n",
            "          COVERAGE_BASE_REF: origin/${{ github.base_ref }}\n"
            '          "COVERAGE_BASE_REF" : refs/heads/untrusted\n',
        )
        # Удалённый первый continuation запускает flags как отдельные shell commands.
        missing_command_continuation = replace_workflow_anchor(
            "          python3 scripts/coverage_stability.py check-baseline-update \\\n",
            "          python3 scripts/coverage_stability.py check-baseline-update\n",
        )
        # Blank line после escaped newline разрывает frozen contiguous command contract.
        blank_between_continuations = replace_workflow_anchor(
            "          python3 scripts/coverage_stability.py check-baseline-update \\\n",
            "          python3 scripts/coverage_stability.py check-baseline-update \\\n\n",
        )
        # Comment после escaped newline меняет shell continuation и не является harmless prose.
        comment_between_continuations = replace_workflow_anchor(
            "          python3 scripts/coverage_stability.py check-baseline-update \\\n",
            "          python3 scripts/coverage_stability.py check-baseline-update \\\n"
            "          # unexpected command-continuation comment\n",
        )
        # Пробел после backslash превращает его в escaped space, а не line continuation.
        trailing_space_after_backslash = replace_workflow_anchor(
            "          python3 scripts/coverage_stability.py check-baseline-update \\\n",
            "          python3 scripts/coverage_stability.py check-baseline-update \\  \n",
        )
        # Удалённый middle continuation разрывает command после первого argument-а.
        missing_argument_continuation = replace_workflow_anchor(
            "            --previous-baseline /tmp/coverage-previous-baseline.json \\\n",
            "            --previous-baseline /tmp/coverage-previous-baseline.json\n",
        )
        # Любой success suffix на final line не должен маскировать validator exit 1/2.
        masked_update_variants = {
            suffix_name: replace_workflow_anchor(
                "            --proposed-measurement-exceptions coverage/measurement-exceptions.json\n",
                "            --proposed-measurement-exceptions "
                f"coverage/measurement-exceptions.json {suffix}\n",
            )
            for suffix_name, suffix in (
                ("or-colon", "|| :"),
                ("semicolon-true", "; true"),
                ("or-exit-zero", "|| exit 0"),
                ("semicolon-colon", "; :"),
            )
        }
        # Artifact relocation сохраняет старое global совпадение, но ломает Upload step ownership.
        relocated_artifact_name = replace_workflow_anchor(
            "          name: coverage-report\n",
            "          name: moved-coverage-report\n",
        )
        # Второй replacement переносит прежнюю строку в checkout `with`, где она не artifact name.
        self.assertEqual(relocated_artifact_name.count("          fetch-depth: 0\n"), 1)
        # Valid YAML semantics здесь не нужны: oracle обязан отвергнуть relocation до workflow run.
        relocated_artifact_name = relocated_artifact_name.replace(
            "          fetch-depth: 0\n",
            "          fetch-depth: 0\n          name: coverage-report\n",
            1,
        )
        # Blank line после final argument тоже запрещена exact frozen scalar contract-ом.
        blank_line_after_update = replace_workflow_anchor(
            "            --proposed-measurement-exceptions coverage/measurement-exceptions.json\n",
            "            --proposed-measurement-exceptions coverage/measurement-exceptions.json\n\n",
        )
        # Scanner обязан продолжить scalar после blank line и увидеть extra executable command.
        extra_command_after_blank = replace_workflow_anchor(
            "            --proposed-measurement-exceptions coverage/measurement-exceptions.json\n",
            "            --proposed-measurement-exceptions coverage/measurement-exceptions.json\n"
            "\n          echo unexpected-extra-command\n",
        )
        # Duplicate mapping key `name` не может override-ить public lifecycle step labels.
        duplicate_step_names = {
            f"duplicate-{step_slug}-step-name": replace_workflow_anchor(
                f"      - name: {step_name}\n",
                f"      - name: {step_name}\n        name: Shadow {step_name}\n",
            )
            for step_slug, step_name in (
                ("update", "Validate baseline update policy"),
                ("run", "Run clean coverage suite and ratchet"),
                ("upload", "Upload coverage report"),
            )
        }
        # Spaced quoted mapping key проверяет ту же YAML identity внутри exact update step-а.
        duplicate_spaced_update_step_name = replace_workflow_anchor(
            "      - name: Validate baseline update policy\n",
            "      - name: Validate baseline update policy\n"
            '        "name" : Shadow baseline update policy\n',
        )
        # Stable label без actual measured owner не является coverage ratchet evidence.
        replaced_measured_run = replace_workflow_anchor(
            "        run: scripts/coverage.sh check\n",
            "        run: true\n",
        )
        # Measured step с false condition оставляет job зелёным без запуска ratchet-а.
        suppressed_measured_step = replace_workflow_anchor(
            "      - name: Run clean coverage suite and ratchet\n",
            "      - name: Run clean coverage suite and ratchet\n        if: false\n",
        )
        # Upload обязан выполняться always; missing/conditional-only variants теряют failure artifact.
        missing_upload_always = replace_workflow_anchor("        if: always()\n", "")
        success_only_upload = replace_workflow_anchor(
            "        if: always()\n",
            "        if: success()\n",
        )
        # Selected-key grammar применяется не только к top-level coverage identity.
        spaced_selected_key_overrides = {
            "duplicate-spaced-job-name": replace_workflow_anchor(
                "    name: Coverage ratchet\n",
                "    name: Coverage ratchet\n    'name' : Shadow coverage status\n",
            ),
            "duplicate-spaced-update-if": replace_workflow_anchor(
                "        if: github.event_name == 'pull_request'\n",
                "        if: github.event_name == 'pull_request'\n"
                '        "if" : always()\n',
            ),
            "duplicate-spaced-update-env": replace_workflow_anchor(
                update_env_owner,
                update_env_owner + "        'env' :\n",
            ),
            "duplicate-spaced-update-run": replace_workflow_anchor(
                "        run: |\n"
                '          git show "${COVERAGE_BASE_REF}:coverage/baseline.json"',
                "        run: |\n        'run' : echo shadow\n"
                '          git show "${COVERAGE_BASE_REF}:coverage/baseline.json"',
            ),
            "duplicate-spaced-upload-uses": replace_workflow_anchor(
                "        uses: actions/upload-artifact@v4\n",
                "        uses: actions/upload-artifact@v4\n"
                '        "uses" : actions/checkout@v4\n',
            ),
            "duplicate-spaced-upload-with": replace_workflow_anchor(
                "        with:\n          # Стабильное имя упрощает поиск report-а в любом run.\n",
                "        with:\n        'with' :\n"
                "          # Стабильное имя упрощает поиск report-а в любом run.\n",
            ),
            "duplicate-spaced-artifact-name": replace_workflow_anchor(
                "          name: coverage-report\n",
                "          name: coverage-report\n"
                '          "name" : shadow-coverage-report\n',
            ),
        }

        # Каждая mutation обязана провалить contract helper отдельным AssertionError.
        workflow_mutations = {
            "missing-pr-condition": missing_pr_condition,
            "wrong-pr-condition": wrong_pr_condition,
            "missing-pull-request-trigger": missing_pull_request_trigger,
            "replaced-pull-request-trigger": replaced_pull_request_trigger,
            "filtered-pull-request-trigger": filtered_pull_request_trigger,
            **duplicate_root_owners,
            **noncanonical_root_owners,
            **noncanonical_coverage_jobs,
            **coverage_job_status_mutations,
            **serialized_coverage_mutations,
            "missing-env-owner": missing_env_owner,
            "wrong-env-owner": wrong_env_owner,
            "duplicate-env-owner": duplicate_env_owner,
            "duplicate-spaced-base-ref": duplicate_spaced_base_ref,
            "missing-command-continuation": missing_command_continuation,
            "blank-between-continuations": blank_between_continuations,
            "comment-between-continuations": comment_between_continuations,
            "trailing-space-after-backslash": trailing_space_after_backslash,
            "missing-argument-continuation": missing_argument_continuation,
            "relocated-artifact-name": relocated_artifact_name,
            "blank-line-after-update": blank_line_after_update,
            "extra-command-after-blank": extra_command_after_blank,
            "duplicate-spaced-update-step-name": duplicate_spaced_update_step_name,
            "replaced-measured-run": replaced_measured_run,
            "suppressed-measured-step": suppressed_measured_step,
            "missing-upload-always": missing_upload_always,
            "success-only-upload": success_only_upload,
            **duplicate_step_names,
            **spaced_selected_key_overrides,
            **masked_update_variants,
        }
        # Mutation matrix закрепляет причинность каждого strict assertion-а.
        for mutation_name, mutated_workflow in workflow_mutations.items():
            # Subtest сохраняет actionable имя ошибочно принятого bypass-а.
            with self.subTest(mutation_name=mutation_name):
                # Green helper на mutation означал бы false-positive release evidence.
                with self.assertRaises(AssertionError):
                    WORKFLOW_CONTRACT.validate_coverage_workflow_contract(mutated_workflow)

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

#!/usr/bin/env python3
"""Focused hermetic tests для S42 ownership и module-size guardrails."""

# Future annotations сохраняют compact type hints test fixtures.
from __future__ import annotations

# importlib загружает executable script без превращения scripts/ в package.
import importlib.util
# sys нужен, чтобы sibling module-size owner разрешался при import-е.
import sys
# tempfile изолирует source/baseline fixtures от рабочего дерева.
import tempfile
# unittest предоставляет stdlib hermetic test runner.
import unittest
# pathlib строит стабильные test paths.
from pathlib import Path
# ModuleType описывает динамически загруженный guardrail module.
from types import ModuleType


# Каталог scripts находится на один уровень выше текущего tests/.
SCRIPTS_DIRECTORY = Path(__file__).resolve().parents[1]

# Repository root нужен для чтения exact checked-in F4F adapter fixture.
REPOSITORY_ROOT = SCRIPTS_DIRECTORY.parent

# Sibling module должен быть виден так же, как при прямом executable запуске.
sys.path.insert(0, str(SCRIPTS_DIRECTORY))

# Focused F4F owner загружается через тот же sibling-module boundary.
F4F_GUARDRAIL = importlib.import_module("s42_f4f_guardrail")


# Функция загружает S42 script как обычный Python module.
def load_guardrail_module() -> ModuleType:
    """Возвращает импортированный check_s42_guardrails module."""

    # Spec указывает на exact production script.
    module_spec = importlib.util.spec_from_file_location(
        "check_s42_guardrails",
        SCRIPTS_DIRECTORY / "check_s42_guardrails.py",
    )
    # Loader обязан существовать для checked-in Python file.
    if module_spec is None or module_spec.loader is None:
        raise RuntimeError("не удалось создать import spec S42 guardrail")
    # Новый module object не запускает CLI благодаря __name__ guard.
    guardrail_module = importlib.util.module_from_spec(module_spec)
    # dataclass resolution требует module в sys.modules во время exec.
    sys.modules[module_spec.name] = guardrail_module
    # Production definitions выполняются без repository scan.
    module_spec.loader.exec_module(guardrail_module)
    # Typed helper возвращает готовый module fixture.
    return guardrail_module


# Production module загружается один раз для всех pure tests.
GUARDRAIL = load_guardrail_module()


# Функция создаёт минимальный pass dependency ownership graph.
def passing_dependency_maps():
    """Возвращает normal/all maps с текущими обязательными owners."""

    # Normal graph содержит только exact required reuse edges.
    normal_dependencies = {
        owner: frozenset(required_dependencies)
        for owner, required_dependencies in GUARDRAIL.REQUIRED_NORMAL_DEPENDENCIES.items()
    }
    # All-kind map нужен HTTP isolation rule и совпадает в pass fixture.
    all_dependencies = dict(normal_dependencies)
    # Обе карты возвращаются отдельно для fail mutations.
    return normal_dependencies, all_dependencies


# Функция пишет UTF-8 fixture и возвращает relative path.
def write_fixture(repo_root: Path, relative_path: str, source_text: str) -> Path:
    """Создаёт один source/script fixture."""

    # Relative path разрешается только внутри temporary root.
    fixture_path = repo_root / relative_path
    # Родительские module directories создаются явно.
    fixture_path.parent.mkdir(parents=True, exist_ok=True)
    # UTF-8 соответствует production repository invariant.
    fixture_path.write_text(source_text, encoding="utf-8")
    # Caller передаёт guardrail-у repository-relative identity.
    return Path(relative_path)


# Функция копирует exact checked-in F4F adapter в изолированный repository fixture.
def write_current_f4f_adapter_fixture(repo_root: Path) -> Path:
    """Создаёт текущий owner-approved F4F ISO-envelope adapter fixture."""

    # Production source является authoritative exact symbol inventory input.
    source_text = (
        REPOSITORY_ROOT / F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH
    ).read_text(encoding="utf-8")
    # Existing generic writer сохраняет exact relative owner path.
    return write_fixture(
        repo_root,
        str(F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH),
        source_text,
    )


# Dependency tests закрепляют единственных owners и отсутствие parser crates.
class S42DependencyGuardrailTests(unittest.TestCase):
    """Проверяет direct dependency policies без Cargo subprocess."""

    # Current owner graph должен проходить без violations.
    def test_required_http_cache_prefetch_edges_pass(self):
        """Exact required normal edges являются достаточными."""

        # Pass maps создаются независимо для этого test-а.
        normal_dependencies, all_dependencies = passing_dependency_maps()
        # Пустой список подтверждает отсутствие скрытого extra requirement.
        self.assertEqual(
            [],
            GUARDRAIL.find_dependency_violations(
                normal_dependencies,
                all_dependencies,
            ),
        )

    # Второй HTTP client и второй fMP4 parser должны называться отдельно.
    def test_duplicate_http_and_container_parser_dependencies_fail(self):
        """Rogue dependencies не маскируются общим boolean failure."""

        # Базовый owner graph остаётся валидным.
        normal_dependencies, all_dependencies = passing_dependency_maps()
        # Новый package получает production alternative fMP4 parser.
        normal_dependencies["rogue-runtime"] = frozenset({"mp4parse"})
        # Тот же package получает второй HTTP client даже как dev/build edge.
        all_dependencies["rogue-runtime"] = frozenset({"mp4parse", "ureq"})
        # Все violations собираются за один вызов.
        violations = GUARDRAIL.find_dependency_violations(
            normal_dependencies,
            all_dependencies,
        )
        # Exact evidence доказывает оба независимых rules.
        self.assertEqual(
            {"mp4parse", "ureq"},
            {
                violation.evidence
                for violation in violations
                if violation.location == "rogue-runtime"
            },
        )

    # Missing reuse edge должен блокировать gate без второго implementation.
    def test_missing_prefetch_reuse_edge_fails(self):
        """web-media-http обязан переиспользовать media-prefetch."""

        # Pass graph создаётся заново.
        normal_dependencies, all_dependencies = passing_dependency_maps()
        # Удаляем единственный prefetch owner edge.
        normal_dependencies["web-media-http"] = frozenset({"source-core"})
        # All-kind map синхронизируется с fixture mutation.
        all_dependencies["web-media-http"] = frozenset({"source-core"})
        # Diagnostics обязана назвать missing owner dependency.
        violations = GUARDRAIL.find_dependency_violations(
            normal_dependencies,
            all_dependencies,
        )
        # Exact tuple проверяет owner и required edge.
        self.assertIn(
            ("web-media-http", "media-prefetch"),
            {
                (violation.location, violation.evidence)
                for violation in violations
            },
        )


# Source tests проверяют declarations, а не случайные слова/comment fixtures.
class S42SourceGuardrailTests(unittest.TestCase):
    """Проверяет parser/FFmpeg/cache/prefetch/WebM source policies."""

    # Temporary root создаётся отдельно для каждого test-а.
    def setUp(self):
        """Создаёт изолированный repository fixture."""

        # TemporaryDirectory автоматически удаляется cleanup-ом unittest.
        self.temporary_directory = tempfile.TemporaryDirectory()
        # Cleanup регистрируется даже если assertion упадёт.
        self.addCleanup(self.temporary_directory.cleanup)
        # Path используется всеми helper-ами.
        self.repo_root = Path(self.temporary_directory.name)

    # Exact parser/cache owners должны проходить.
    def test_exact_parser_cache_and_prefetch_owners_pass(self):
        """Owner declarations не считаются дубликатами."""

        # MPEG-TS parser находится в canonical crate.
        ts_path = write_fixture(
            self.repo_root,
            "crates/mpeg-ts-demux/src/psi.rs",
            "fn parse_pat() {}\n",
        )
        # FLV parser находится в canonical crate.
        flv_path = write_fixture(
            self.repo_root,
            "crates/flv-demux/src/framing.rs",
            "fn parse_flv_header() {}\n",
        )
        # HTTP cache находится в source-core owner-е.
        cache_path = write_fixture(
            self.repo_root,
            "crates/source-core/src/cache.rs",
            "pub struct RamByteRangeCache;\n",
        )
        # Byte prefetch находится в media-prefetch owner-е.
        prefetch_path = write_fixture(
            self.repo_root,
            "crates/media-prefetch/src/source.rs",
            "pub struct PrefetchingByteSource;\n",
        )
        # Exact узкий F4F exception обязан присутствовать без symbol drift.
        f4f_adapter_path = write_current_f4f_adapter_fixture(self.repo_root)
        # Все owner fixtures должны пройти одновременно.
        self.assertEqual(
            [],
            GUARDRAIL.find_source_violations(
                self.repo_root,
                [ts_path, flv_path, cache_path, prefetch_path, f4f_adapter_path],
            ),
        )

    # Rogue parser/cache/FFmpeg declarations должны давать exact rules.
    def test_duplicate_parsers_cache_and_ffmpeg_encoder_fail(self):
        """Четыре независимых regressions не сливаются в один bool."""

        # Один rogue module намеренно нарушает несколько boundaries.
        rogue_path = write_fixture(
            self.repo_root,
            "crates/rogue-runtime/src/lib.rs",
            "fn parse_pmt() {}\n"
            "fn parse_flv_tag() {}\n"
            "fn parse_moof() {}\n"
            "struct HttpByteRangeCache;\n"
            "fn encode() { avcodec_find_encoder(0); }\n",
        )
        # Exact F4F path не должен добавлять unrelated missing-path violation.
        f4f_adapter_path = write_current_f4f_adapter_fixture(self.repo_root)
        # Source audit собирает все line-addressable violations.
        violations = GUARDRAIL.find_source_violations(
            self.repo_root,
            [rogue_path, f4f_adapter_path],
        )
        # Rules должны покрыть TS, FLV, fMP4, cache и FFmpeg.
        observed_rule_text = "\n".join(violation.rule for violation in violations)
        # Каждый owner invariant проверяется отдельно.
        for expected_fragment in ("MPEG-TS", "FLV/F4F", "fMP4", "HTTP byte cache", "FFmpeg"):
            # Subtest облегчает diagnostics одного missing rule.
            with self.subTest(expected_fragment=expected_fragment):
                # Fragment обязан присутствовать в агрегированном rule text.
                self.assertIn(expected_fragment, observed_rule_text)

    # Rust function qualifiers не должны обходить rogue parser scan.
    def test_qualified_rogue_f4f_and_fmp4_declarations_fail(self):
        """const/async/unsafe/extern declarations остаются запрещёнными."""

        # Exact F4F owner присутствует и не добавляет missing-path noise.
        f4f_adapter_path = write_current_f4f_adapter_fixture(self.repo_root)
        # Rogue module покрывает все разрешённые Rust qualifier forms.
        rogue_path = write_fixture(
            self.repo_root,
            "crates/rogue-runtime/src/qualified_parser.rs",
            "async fn parse_sidx() {}\n"
            "unsafe fn validate_moof() {}\n"
            "const fn validate_afra() {}\n"
            'extern "C" fn parse_f4f_segment() {}\n'
            "extern fn read_emsg() {}\n",
        )
        # Source audit обязан увидеть каждую qualified declaration.
        violations = GUARDRAIL.find_source_violations(
            self.repo_root,
            [f4f_adapter_path, rogue_path],
        )
        # Exact source lines позволяют доказать отсутствие qualifier bypass-а.
        observed_evidence = {violation.evidence for violation in violations}
        # Все пять syntax variants проверяются независимо.
        for expected_evidence in (
            "async fn parse_sidx() {}",
            "unsafe fn validate_moof() {}",
            "const fn validate_afra() {}",
            'extern "C" fn parse_f4f_segment() {}',
            "extern fn read_emsg() {}",
        ):
            # Subtest показывает exact qualifier при regression.
            with self.subTest(expected_evidence=expected_evidence):
                # Qualified declaration обязана присутствовать в diagnostics.
                self.assertIn(expected_evidence, observed_evidence)
        # F4F-specific и generic fMP4 rules должны сработать одновременно.
        observed_rules = {violation.rule for violation in violations}
        # F4F exact-path boundary ловит validate_afra/parse_f4f_segment.
        self.assertIn(
            "F4F ISO-envelope adapter разрешён только в exact flv-demux path",
            observed_rules,
        )
        # Generic owner boundary ловит sidx/moof/emsg declarations.
        self.assertIn(
            "generic fMP4 parsing принадлежит symphonia-format-isomp4 patch",
            observed_rules,
        )

    # Exact F4F path не является вторым generic fMP4 parser-ом.
    def test_exact_f4f_iso_envelope_adapter_passes(self):
        """Текущий path и symbol inventory являются единственным исключением."""

        # Checked-in source копируется без синтетического ослабления declarations.
        f4f_adapter_path = write_current_f4f_adapter_fixture(self.repo_root)
        # Один exact adapter не должен давать generic fMP4 violation.
        self.assertEqual(
            [],
            GUARDRAIL.find_source_violations(
                self.repo_root,
                [f4f_adapter_path],
            ),
        )

    # Перенос exact symbols в другой module запрещён.
    def test_f4f_iso_envelope_adapter_relocation_fails(self):
        """Исключение привязано к одному exact repository path."""

        # Current source читается как authoritative relocation payload.
        source_text = (
            REPOSITORY_ROOT / F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH
        ).read_text(encoding="utf-8")
        # Тот же parser помещается в незаявленный соседний module.
        moved_path = write_fixture(
            self.repo_root,
            "crates/flv-demux/src/f4f_moved.rs",
            source_text,
        )
        # Audit должен одновременно увидеть missing exact path и rogue declarations.
        violations = GUARDRAIL.find_source_violations(
            self.repo_root,
            [moved_path],
        )
        # Rules подтверждают обе стороны fail-closed path boundary.
        observed_rules = {violation.rule for violation in violations}
        # Missing exact identity нельзя скрыть сохранёнными symbol names.
        self.assertIn(
            "exact F4F ISO-envelope adapter path отсутствует",
            observed_rules,
        )
        # Moved F4F declarations также не получают directory-wide exception.
        self.assertIn(
            "F4F ISO-envelope adapter разрешён только в exact flv-demux path",
            observed_rules,
        )

    # Новый parser helper расширяет exception и требует owner decision.
    def test_f4f_iso_envelope_extra_symbol_fails(self):
        """Любой новый function symbol ломает exact ratchet."""

        # Current source сохраняет все обязательные symbols.
        source_path = (
            REPOSITORY_ROOT / F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH
        )
        # Обычный и qualified helpers моделируют расширение generic fMP4 parsing.
        expanded_source = source_path.read_text(encoding="utf-8") + (
            "\nfn parse_sidx() {}\n"
            "async fn parse_async_sidx() {}\n"
            "struct SidxParser;\n"
        )
        # Expanded source записывается только в temporary fixture.
        f4f_adapter_path = write_fixture(
            self.repo_root,
            str(F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH),
            expanded_source,
        )
        # Exact path больше не должен давать pass.
        violations = GUARDRAIL.find_source_violations(
            self.repo_root,
            [f4f_adapter_path],
        )
        # Diagnostics обязана назвать новый symbol.
        self.assertIn(
            "unexpected function `parse_sidx`",
            {violation.evidence for violation in violations},
        )
        # Qualified function syntax не может обойти declaration inventory.
        self.assertIn(
            "unexpected function `parse_async_sidx`",
            {violation.evidence for violation in violations},
        )
        # Новый parser state type также расширяет exact exception.
        self.assertIn(
            "unexpected struct `SidxParser`",
            {violation.evidence for violation in violations},
        )

    # Rename/remove existing validator не должен ослабить exception молча.
    def test_f4f_iso_envelope_symbol_mutation_fails(self):
        """Missing и replacement symbols видны раздельно."""

        # Current source является базой mutation fixture.
        source_path = (
            REPOSITORY_ROOT / F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH
        )
        # Exact validator переименовывается без изменения остального parser-а.
        mutated_source = source_path.read_text(encoding="utf-8").replace(
            "fn validate_trun(",
            "fn validate_fragment_run(",
            1,
        )
        # Assertion защищает test от stale replacement needle.
        self.assertNotEqual(
            source_path.read_text(encoding="utf-8"),
            mutated_source,
        )
        # Mutated source занимает разрешённый path, но не разрешённый inventory.
        f4f_adapter_path = write_fixture(
            self.repo_root,
            str(F4F_GUARDRAIL.F4F_ISO_ENVELOPE_ADAPTER_PATH),
            mutated_source,
        )
        # Audit агрегирует missing и unexpected evidence.
        violations = GUARDRAIL.find_source_violations(
            self.repo_root,
            [f4f_adapter_path],
        )
        # Exact evidence не сливает rename в общий boolean.
        observed_evidence = {violation.evidence for violation in violations}
        # Старый validator обязан оставаться в ratchet.
        self.assertIn("missing function `validate_trun`", observed_evidence)
        # Новое имя не получает неявное разрешение.
        self.assertIn(
            "unexpected function `validate_fragment_run`",
            observed_evidence,
        )

    # Inline test parser fixture не должен считаться production duplicate.
    def test_inline_test_parser_declaration_is_ignored(self):
        """cfg(test) tail не входит в production parser audit."""

        # Production часть не содержит parser-а, test tail содержит fixture helper.
        source_text = (
            "pub fn safe_runtime() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn parse_moof() {}\n"
            "}\n"
        )
        # Pure splitter должен оставить только safe production prefix.
        production_text = GUARDRAIL.production_source_text(source_text)
        # Test helper удалён из audited text.
        self.assertNotIn("parse_moof", production_text)

    # Legacy WebM opener проверяется и в Rust, и в runtime scripts.
    def test_legacy_webm_opener_in_script_fails(self):
        """Удалённый runtime scenario не может вернуться."""

        # Все audited scripts сначала создаются безопасными.
        for script_path in GUARDRAIL.LEGACY_WEBM_SCRIPT_PATHS:
            # Runtime script fixture не содержит legacy symbol.
            write_fixture(self.repo_root, str(script_path), "# safe runtime\n")
        # Один script получает удалённое scenario name.
        write_fixture(
            self.repo_root,
            "scripts/media-regression.sh",
            "selected_webm_opens_over_range\n",
        )
        # Rust source inventory может быть пустым в focused script test-е.
        violations = GUARDRAIL.find_legacy_webm_violations(
            self.repo_root,
            [],
        )
        # Exact script path остаётся в diagnostics.
        self.assertTrue(
            any(
                violation.location.startswith("scripts/media-regression.sh:")
                for violation in violations
            )
        )


# Module-size tests закрепляют no-growth/new-oversized snapshot semantics.
class S42ModuleSizeGuardrailTests(unittest.TestCase):
    """Проверяет exact legacy snapshot ratchet."""

    # Temporary root создаётся отдельно для каждого line-count scenario.
    def setUp(self):
        """Создаёт изолированный module fixture."""

        # TemporaryDirectory владеет cleanup.
        self.temporary_directory = tempfile.TemporaryDirectory()
        # Cleanup выполняется даже при failure.
        self.addCleanup(self.temporary_directory.cleanup)
        # Root path используется source writer-ом.
        self.repo_root = Path(self.temporary_directory.name)

    # Exact legacy count должен проходить.
    def test_exact_legacy_line_count_passes(self):
        """Snapshot не требует refactor существующего oversized module."""

        # Три строки превышают synthetic hard limit 2.
        module_path = write_fixture(
            self.repo_root,
            "crates/legacy/src/lib.rs",
            "one\ntwo\nthree\n",
        )
        # Baseline фиксирует фактический current count.
        baseline = {
            "hard_limit_lines": 2,
            "legacy_modules": {str(module_path): 3},
        }
        # Exact snapshot не создаёт violations.
        self.assertEqual(
            [],
            GUARDRAIL.find_module_size_violations(
                self.repo_root,
                [module_path],
                baseline,
            ),
        )

    # Рост и новый oversized module должны блокироваться.
    def test_growth_and_new_oversized_module_fail(self):
        """Legacy allowance не разрешает рост или другой path."""

        # Legacy module вырос с трёх до четырёх строк.
        legacy_path = write_fixture(
            self.repo_root,
            "crates/legacy/src/lib.rs",
            "one\ntwo\nthree\nfour\n",
        )
        # Новый module тоже пересёк limit.
        new_path = write_fixture(
            self.repo_root,
            "crates/new-owner/src/lib.rs",
            "one\ntwo\nthree\n",
        )
        # Baseline разрешает только старый count/path.
        baseline = {
            "hard_limit_lines": 2,
            "legacy_modules": {str(legacy_path): 3},
        }
        # Оба независимых нарушения должны быть видимы.
        violations = GUARDRAIL.find_module_size_violations(
            self.repo_root,
            [legacy_path, new_path],
            baseline,
        )
        # Paths точно называют growth и new owner.
        self.assertEqual(
            {str(legacy_path), str(new_path)},
            {violation.location for violation in violations},
        )

    # Уменьшение ratchet-ится обновлением checked-in snapshot.
    def test_shrink_requires_snapshot_tightening(self):
        """Legacy module не может незаметно получить право отрасти обратно."""

        # Module уменьшился до hard limit и больше не является legacy.
        module_path = write_fixture(
            self.repo_root,
            "crates/legacy/src/lib.rs",
            "one\ntwo\n",
        )
        # Старый baseline всё ещё разрешает три строки.
        baseline = {
            "hard_limit_lines": 2,
            "legacy_modules": {str(module_path): 3},
        }
        # Stale allowance должен быть удалён deliberate правкой snapshot-а.
        violations = GUARDRAIL.find_module_size_violations(
            self.repo_root,
            [module_path],
            baseline,
        )
        # Rule прямо объясняет shrink/removal case.
        self.assertIn("stale", violations[0].rule)


# Прямой запуск удобен для focused local verification.
if __name__ == "__main__":
    # unittest управляет process exit status.
    unittest.main()

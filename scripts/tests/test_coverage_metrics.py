"""Focused tests для coverage aggregation и exception boundary."""

# datetime создаёт гарантированно непросроченный review date.
import datetime as dt
# contextlib локально подавляет ожидаемую failure-диагностику negative test-а.
import contextlib
# io предоставляет in-memory stderr только для assertRaises сценария.
import io
# importlib загружает repo script без превращения scripts/ в package.
import importlib.util
# json читает versioned coverage policy без запуска production CLI.
import json
# pathlib вычисляет стабильные пути относительно этого test-файла.
from pathlib import Path
# sys добавляет production scripts directory для v2 schema imports.
import sys
# tempfile создаёт изолированный exception manifest для CLI lifecycle test-а.
import tempfile
# tomllib читает workspace/package manifests стандартной библиотекой Python 3.11+.
import tomllib
# unittest предоставляет hermetic stdlib test runner проекта.
import unittest
# mock точечно подменяет CLI path/argv без изменения versioned manifest-а.
from unittest import mock

# Корень репозитория находится на два уровня выше scripts/tests/.
REPO_ROOT = Path(__file__).resolve().parents[2]
# Stable-coordinate validator является владельцем установленной baseline schema v2.
sys.path.insert(0, str(REPO_ROOT / "scripts"))
import coverage_coordinate_model as COORDINATE_MODEL  # noqa: E402
import coverage_stability as COVERAGE_STABILITY  # noqa: E402
# Spec указывает на production parser, который проверяет CI ratchet.
MODULE_SPEC = importlib.util.spec_from_file_location(
    "coverage_metrics", REPO_ROOT / "scripts" / "coverage_metrics.py"
)
# Модуль создаётся явно, чтобы тестировать pure функции без subprocess.
COVERAGE_METRICS = importlib.util.module_from_spec(MODULE_SPEC)
# Loader гарантирован для spec, созданного из существующего файла.
assert MODULE_SPEC.loader is not None
# Выполнение модуля не запускает main благодаря __name__ guard.
MODULE_SPEC.loader.exec_module(COVERAGE_METRICS)


# Вспомогательная функция создаёт counters одной метрики.
def metric(covered: int, total: int):
    # Имена совпадают с compact baseline schema.
    return {"covered": covered, "total": total}


# Вспомогательная функция создаёт минимальный ratchet summary.
def summary(workspace_metric, crate_metric):
    # Все три production metrics получают одинаковые counters для простого fixture.
    workspace_metrics = {
        metric_name: dict(workspace_metric) for metric_name in ["lines", "functions", "regions"]
    }
    # Crate metrics копируются независимо от workspace словаря.
    crate_metrics = {
        metric_name: dict(crate_metric) for metric_name in ["lines", "functions", "regions"]
    }
    # Informational группа не нужна exact ratchet function.
    return {
        "workspace": workspace_metrics,
        "blocking_group": {
            metric_name: dict(crate_metric)
            for metric_name in ["lines", "functions", "regions"]
        },
        "blocking_crates": {"contract-core": crate_metrics},
        "informational_crates": {},
    }


# Тесты закрепляют точное сравнение и exception lifecycle.
class CoverageRatchetTests(unittest.TestCase):
    # LLVM counter-expression underflow нельзя выдавать за покрытую строку.
    def test_lcov_top_bit_execution_counter_is_rejected(self):
        # Валидные DA/FNDA/BRDA records проходят тот же parser без нормализации.
        COVERAGE_METRICS.validate_lcov_profile(
            "TN:\n"
            "SF:/workspace/crates/example/src/lib.rs\n"
            "FNDA:3,example\n"
            "DA:10,2\n"
            "BRDA:10,0,0,-\n"
            "BRDA:10,0,1,1\n"
            "end_of_record\n"
        )
        # u64::MAX наблюдался при flush до завершения detached refresh worker-а;
        # top-bit threshold также ловит underflow на величину больше единицы.
        for corrupted_counter in (1 << 63, (1 << 64) - 2, (1 << 64) - 1):
            # Каждый raw execution grammar record обязан fail-closed независимо.
            corrupted_records = (
                f"DA:84,{corrupted_counter}",
                f"FNDA:{corrupted_counter},refresh_worker",
                f"BRDA:84,0,0,{corrupted_counter}",
            )
            # Grammar records проверяются отдельно внутри counter case.
            for corrupted_record in corrupted_records:
                # Subtest называет точный LCOV grammar owner и значение.
                with self.subTest(corrupted_record=corrupted_record):
                    # Diagnostics отличает corruption от coverage regression.
                    with self.assertRaisesRegex(ValueError, "повреждён"):
                        COVERAGE_METRICS.validate_lcov_profile(corrupted_record)

    # Равная доля с другими counters не должна считаться regression.
    def test_ratio_comparison_uses_exact_fraction_instead_of_rounded_percent(self):
        # 50/100 и 1/2 математически равны.
        self.assertFalse(COVERAGE_METRICS.ratio_decreased(metric(50, 100), metric(1, 2)))
        # 49/100 действительно меньше 1/2.
        self.assertTrue(COVERAGE_METRICS.ratio_decreased(metric(49, 100), metric(1, 2)))

    # Informational crate не должен блокировать zero-regression ratchet.
    def test_find_regressions_checks_workspace_and_blocking_crates_only(self):
        # Baseline фиксирует 80% workspace и crate.
        baseline = summary(metric(8, 10), metric(8, 10))
        # Current сохраняет workspace, но снижает pure crate до 70%.
        current = summary(metric(8, 10), metric(7, 10))
        # Снижение должно затронуть pure group и точный crate owner.
        regressions = COVERAGE_METRICS.find_regressions(
            current, baseline, ["lines", "functions", "regions"]
        )
        # Ни одна запись не должна быть ошибочно приписана workspace/UI shell.
        self.assertEqual(
            {item["scope"] for item in regressions},
            {"blocking-group", "crate:contract-core"},
        )

    # Legacy CLI больше не должен предоставлять второй baseline-update policy.
    def test_legacy_update_subcommand_is_unavailable_but_report_commands_remain(self):
        # Старое имя обязано завершаться parser error до чтения versioned inputs.
        with mock.patch(
            "sys.argv",
            ["coverage_metrics.py", "check-baseline-update", "--previous", "base.json"],
        ):
            # Argparse использует frozen exit 2 для неизвестного subcommand.
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(
                SystemExit
            ) as rejected_update:
                COVERAGE_METRICS.parse_args()
        # Нельзя случайно принять старый v1 update path как policy success.
        self.assertEqual(rejected_update.exception.code, 2)

        # Report-only и integrity commands остаются доступными после удаления update API.
        command_arguments = {
            "generate": ["--input", "raw.json", "--output", "summary.json"],
            "check": ["--input", "raw.json"],
            "validate-baseline": [],
            "validate-lcov": ["--input", "coverage.lcov"],
        }
        # Каждый сохранившийся command парсится тем же production parser-ом.
        for command, arguments in command_arguments.items():
            # Subtest называет exact legacy/report-only поверхность.
            with self.subTest(command=command), mock.patch(
                "sys.argv", ["coverage_metrics.py", command, *arguments]
            ):
                # Parsed command доказывает, что зачистка не сломала соседний CLI.
                self.assertEqual(COVERAGE_METRICS.parse_args().command, command)

    # Верхний уровень exceptions manifest обязан соблюдать versioned exact schema.
    def test_exception_manifest_schema_is_fail_closed(self):
        # Неизвестная версия не может быть прочитана текущим parser-ом.
        with self.assertRaisesRegex(ValueError, "неподдерживаемую schema_version"):
            # Даже пустой список не делает неизвестную schema безопасной.
            COVERAGE_METRICS.validate_exception_document(
                {"schema_version": 2, "exceptions": []}
            )
        # Корректная первая версия возвращает исходный упорядоченный список.
        exceptions = []
        # Identity списка подтверждает отсутствие скрытой нормализации.
        self.assertIs(
            COVERAGE_METRICS.validate_exception_document(
                {"schema_version": 1, "exceptions": exceptions}
            ),
            exceptions,
        )

    # Always-run validate-baseline обязан блокировать просроченную exception.
    def test_validate_baseline_cli_rejects_expired_exception(self):
        # Полностью валидная запись отличается от production manifest-а только сроком.
        expired_exception = {
            "scope": "crate:contract-core",
            "metric": "lines",
            "previous": metric(8, 10),
            "allowed": metric(7, 10),
            "reason": "Временное измеренное снижение line coverage.",
            "review_by": "2000-01-01",
            "follow_up": "До review date добавить focused line test.",
        }
        # TemporaryDirectory гарантирует удаление fixture после CLI вызова.
        with tempfile.TemporaryDirectory(prefix="coverage-expired-exception-") as directory:
            # Отдельный path не затрагивает checked-in coverage/exceptions.json.
            exception_path = Path(directory) / "exceptions.json"
            # JSON writer формирует тот же UTF-8 shape, что и production manifest.
            exception_path.write_text(
                json.dumps(
                    {"schema_version": 1, "exceptions": [expired_exception]},
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
            # Legacy CLI получает embedded report-only v1, а не blocking v2 document.
            baseline_path = Path(directory) / "baseline-v1.json"
            stable_baseline = COORDINATE_MODEL.read_json(
                REPO_ROOT / "coverage/baseline.json"
            )
            baseline_path.write_text(
                json.dumps(
                    stable_baseline["legacy_report_only"]["baseline_v1"],
                    ensure_ascii=False,
                ),
                encoding="utf-8",
            )
            # CLI читает temporary manifest через точечную подмену module constant.
            with mock.patch.object(
                COVERAGE_METRICS, "EXCEPTIONS_PATH", exception_path
            ), mock.patch.object(
                COVERAGE_METRICS, "BASELINE_PATH", baseline_path
            ):
                # argv запускает тот же cheap preflight, который вызывает coverage.sh check.
                with mock.patch("sys.argv", ["coverage_metrics.py", "validate-baseline"]):
                    # Expired lifecycle обязан остановить release до дорогой LLVM suite.
                    with self.assertRaisesRegex(ValueError, "просрочено 2000-01-01"):
                        # main использует current policy и embedded legacy/report-only baseline.
                        COVERAGE_METRICS.main()


# Тесты инвентаря не позволяют workspace и coverage policy снова разойтись.
class CoveragePolicyInventoryTests(unittest.TestCase):
    # Каждый workspace crate обязан иметь ровно одну осознанную coverage-классификацию.
    def test_every_workspace_crate_is_classified_by_coverage_policy(self):
        # Root manifest является каноническим владельцем workspace membership.
        with (REPO_ROOT / "Cargo.toml").open("rb") as manifest_file:
            # tomllib сохраняет точные относительные пути workspace members.
            workspace_manifest = tomllib.load(manifest_file)
        # Policy читается отдельно, чтобы тест проверял versioned production input.
        with (REPO_ROOT / "coverage" / "policy.json").open(encoding="utf-8") as policy_file:
            # JSON parser возвращает те же группы, которые использует coverage_metrics.py.
            coverage_policy = json.load(policy_file)
        # Aggregator идентифицирует crate по первому каталогу внутри crates/.
        workspace_crates = set()
        # Явный members list не включает standalone patch crates вне workspace.
        for member_path in workspace_manifest["workspace"]["members"]:
            # Path разбирается тем же pathlib vocabulary, что и production parser.
            member_parts = Path(member_path).parts
            # Coverage policy управляет только first-party members внутри crates/.
            self.assertEqual(member_parts[0], "crates")
            # Второй компонент совпадает с crate_name_for_file для LLVM source path.
            workspace_crates.add(member_parts[1])
        # Группы извлекаются отдельно, чтобы дешёво проверить их непересечение.
        blocking_crates = set(coverage_policy["blocking_crates"])
        # Informational inventory имеет ту же identity vocabulary каталогов.
        informational_crates = set(coverage_policy["informational_crates"])
        # Один crate не может одновременно блокировать и только информировать.
        self.assertTrue(blocking_crates.isdisjoint(informational_crates))
        # Объединение групп является полным ожидаемым coverage inventory.
        classified_crates = blocking_crates | informational_crates
        # Exact equality ловит как новый неклассифицированный crate, так и stale policy entry.
        self.assertEqual(classified_crates, workspace_crates)

    # Checked-in baseline обязан ratchet-ить каждый current blocking owner.
    def test_checked_in_baseline_matches_exact_policy_inventory(self):
        # Policy является владельцем ожидаемых blocking/informational групп.
        with (REPO_ROOT / "coverage" / "policy.json").open(encoding="utf-8") as policy_file:
            # Production parser получает тот же JSON shape, что и CLI.
            coverage_policy = json.load(policy_file)
        # Baseline читается отдельно, чтобы missing keys не могли скрыться в fixture-е.
        with (REPO_ROOT / "coverage" / "baseline.json").open(
            encoding="utf-8"
        ) as baseline_file:
            # Counters не пересчитываются и не фабрикуются этим дешёвым тестом.
            coverage_baseline = json.load(baseline_file)
        # Stable schema проверяет hashes/ranges/paths до inventory projection.
        COVERAGE_STABILITY.validate_baseline(coverage_baseline)
        # Blocking domains являются exact typed projection current policy.
        expected_domains = {
            "workspace",
            "blocking-group",
            *{
                f"crate:{crate_name}"
                for crate_name in coverage_policy["blocking_crates"]
            },
        }
        self.assertEqual(
            set(coverage_baseline["stable_source"]["domains"]), expected_domains
        )
        # Source universe содержит ровно все classified workspace owners.
        source_owners = {
            COORDINATE_MODEL.crate_name(source_path)
            for source_path in coverage_baseline["source_files"]["universe"]
        }
        self.assertEqual(
            source_owners,
            set(coverage_policy["blocking_crates"])
            | set(coverage_policy["informational_crates"]),
        )
        # Policy content hash связывает baseline с exact tracked classification.
        self.assertEqual(
            coverage_baseline["provenance"]["policy_hash"],
            COORDINATE_MODEL.content_hash(coverage_policy),
        )

    # Missing blocking owner должен давать actionable fail-closed diagnostics.
    def test_baseline_inventory_rejects_missing_blocking_crate(self):
        # Минимальный policy содержит один blocking owner и одну metric.
        coverage_policy = {
            "schema_version": 1,
            "tool": {"name": "cargo-llvm-cov", "version": "0.8.7"},
            "metrics": ["lines"],
            "blocking_crates": ["contract-core"],
            "informational_crates": [],
        }
        # Baseline намеренно не содержит заявленного blocking owner-а.
        incomplete_baseline = {
            "schema_version": 1,
            "tool": dict(coverage_policy["tool"]),
            "workspace": {"lines": metric(1, 1)},
            "blocking_group": {"lines": metric(1, 1)},
            "blocking_crates": {},
            "informational_crates": {},
        }
        # Диагностика должна прямо назвать missing inventory, а не упасть KeyError-ом позже.
        with self.assertRaisesRegex(ValueError, "missing=.*contract-core"):
            # Pure validator не запускает LLVM/Cargo и не пишет artifacts.
            COVERAGE_METRICS.validate_summary_inventory(
                incomplete_baseline,
                coverage_policy,
                document_name="fixture baseline",
            )


# Прямой запуск файла остаётся удобным вне unittest discovery.
if __name__ == "__main__":
    # unittest управляет exit status для shell/CI.
    unittest.main()

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

    # Baseline нельзя снизить простой правкой JSON без exception.
    def test_baseline_decrease_without_exception_is_rejected(self):
        # Previous baseline имеет 80% во всех scopes.
        previous = summary(metric(8, 10), metric(8, 10))
        # Proposed снижает только crate до 70%.
        proposed = summary(metric(8, 10), metric(7, 10))
        # Ожидаемая stderr-диагностика не должна засорять успешный общий CI log.
        with contextlib.redirect_stderr(io.StringIO()):
            # ValueError является policy failure для CI wrapper-а.
            with self.assertRaisesRegex(ValueError, "требует точного"):
                # Пустой список не разрешает ни одну regression.
                COVERAGE_METRICS.validate_baseline_update(
                    previous, proposed, [], ["lines", "functions", "regions"]
                )

    # Точная bounded exception разрешает только заявленную metric/counters пару.
    def test_exact_non_expired_exception_allows_one_metric_decrease(self):
        # Previous и proposed различаются только line coverage crate-а.
        previous = summary(metric(8, 10), metric(8, 10))
        # Глубокая ручная копия fixture не нужна: создаём новый summary.
        proposed = summary(metric(8, 10), metric(8, 10))
        # Снижаем только lines, чтобы exception оставалась точечной.
        proposed["blocking_crates"]["contract-core"]["lines"] = metric(7, 10)
        # Review date в будущем не зависит от календарного года тестовой среды.
        review_by = (dt.date.today() + dt.timedelta(days=30)).isoformat()
        # Exception фиксирует scope/metric, точные counters, причину и follow-up.
        exception = {
            "scope": "crate:contract-core",
            "metric": "lines",
            "previous": metric(8, 10),
            "allowed": metric(7, 10),
            "reason": "Удалён недетерминированный тест внешнего устройства.",
            "review_by": review_by,
            "follow_up": "issue:coverage-contract-core-restore",
        }
        # Отсутствие исключения подтверждает успешную policy validation.
        COVERAGE_METRICS.validate_baseline_update(
            previous, proposed, [exception], ["lines", "functions", "regions"]
        )

    # Duplicate scope/metric не должен молча затереть одну из reviewed записей.
    def test_duplicate_exception_identity_is_rejected(self):
        # Previous и proposed различаются одной line regression.
        previous = summary(metric(8, 10), metric(8, 10))
        # Новый baseline снижает только crate lines.
        proposed = summary(metric(8, 10), metric(8, 10))
        # Точная regression совпадает с обеими намеренно дублированными записями.
        proposed["blocking_crates"]["contract-core"]["lines"] = metric(7, 10)
        # Будущая дата удерживает тест сфокусированным на duplicate identity.
        review_by = (dt.date.today() + dt.timedelta(days=30)).isoformat()
        # Полная валидная запись используется как duplicate fixture.
        exception = {
            "scope": "crate:contract-core",
            "metric": "lines",
            "previous": metric(8, 10),
            "allowed": metric(7, 10),
            "reason": "Одно измеренное снижение line coverage.",
            "review_by": review_by,
            "follow_up": "До review date добавить focused line test.",
        }
        # Duplicate должен завершиться policy failure до regression matching.
        with self.assertRaisesRegex(ValueError, "duplicate crate:contract-core/lines"):
            # Две независимые копии доказывают проверку identity, а не object identity.
            COVERAGE_METRICS.validate_baseline_update(
                previous,
                proposed,
                [dict(exception), dict(exception)],
                ["lines", "functions", "regions"],
            )

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
            # CLI читает temporary manifest через точечную подмену module constant.
            with mock.patch.object(COVERAGE_METRICS, "EXCEPTIONS_PATH", exception_path):
                # argv запускает тот же cheap preflight, который вызывает coverage.sh check.
                with mock.patch("sys.argv", ["coverage_metrics.py", "validate-baseline"]):
                    # Expired lifecycle обязан остановить release до дорогой LLVM suite.
                    with self.assertRaisesRegex(ValueError, "просрочено 2000-01-01"):
                        # main использует реальные checked-in policy/baseline и fixture exceptions.
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
        # Exact validator проверяет tool/schema/groups/metrics/counter bounds.
        COVERAGE_METRICS.validate_summary_inventory(
            coverage_baseline,
            coverage_policy,
            document_name="checked-in coverage baseline",
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

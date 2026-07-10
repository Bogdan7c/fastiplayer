"""Focused tests для coverage aggregation и exception boundary."""

# datetime создаёт гарантированно непросроченный review date.
import datetime as dt
# contextlib локально подавляет ожидаемую failure-диагностику negative test-а.
import contextlib
# io предоставляет in-memory stderr только для assertRaises сценария.
import io
# importlib загружает repo script без превращения scripts/ в package.
import importlib.util
# pathlib вычисляет стабильные пути относительно этого test-файла.
from pathlib import Path
# unittest предоставляет hermetic stdlib test runner проекта.
import unittest

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


# Прямой запуск файла остаётся удобным вне unittest discovery.
if __name__ == "__main__":
    # unittest управляет exit status для shell/CI.
    unittest.main()

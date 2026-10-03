#!/usr/bin/env python3
"""Функциональные тесты report-only зонда `coverage_focus.py`.

Cohort для тестов строится настоящим pipeline-ом gate-а (`extract_run_state` →
`intersect_runs`) из того же fixture LLVM export-а, поэтому проверяется именно
совместимость identity зонда с реальными cohort/baseline документами.
"""

from __future__ import annotations

import io
import json
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPTS_ROOT = REPO_ROOT / "scripts"
FIXTURE_ROOT = SCRIPTS_ROOT / "tests/fixtures/coverage_stable"
sys.path.insert(0, str(SCRIPTS_ROOT))
sys.path.insert(0, str(FIXTURE_ROOT))

import coverage_coordinates as coordinates  # noqa: E402
import coverage_focus as focus  # noqa: E402
import coverage_stability as stability  # noqa: E402
from coverage_coordinate_model import coordinate_identity  # noqa: E402
from fixture_factory import build_report  # noqa: E402


def load_fixture(name: str):
    with (FIXTURE_ROOT / name).open(encoding="utf-8") as fixture_file:
        return json.load(fixture_file)


def baseline_with_targets(crate: str, targets: dict[str, tuple[int, int]]) -> dict:
    """Минимальный baseline-документ: зонд читает из него только counts crate-а."""

    return {
        "stable_source": {
            "domains": {
                f"crate:{crate}": {
                    metric: {"counts": {"stable": stable, "total": total}}
                    for metric, (stable, total) in targets.items()
                }
            }
        }
    }


class CoverageFocusTests(unittest.TestCase):
    def setUp(self):
        self.temporary_directory = tempfile.TemporaryDirectory(prefix="coverage-focus-")
        self.repo_root = Path(self.temporary_directory.name)
        (self.repo_root / "Cargo.toml").write_text("[workspace]\nmembers=[]\n", encoding="utf-8")
        self.policy = load_fixture("policy.json")
        self.profile = load_fixture("profile.json")
        # Настоящий cohort из трёх fixture-прогонов с переменным покрытием.
        states = [
            coordinates.extract_run_state(
                build_report(self.repo_root, run=run), self.policy, self.profile,
                self.repo_root, f"run-{run}",
            )
            for run in (1, 2, 3)
        ]
        self.cohort = stability.intersect_runs(self.policy, states)[0]

    def tearDown(self):
        self.temporary_directory.cleanup()

    def cohort_counts(self, metric: str) -> tuple[int, int]:
        counts = self.cohort["stable_source"]["domains"]["crate:alpha"][metric]["counts"]
        return counts["stable"], counts["total"]

    def test_probe_identities_match_gate_extractor_for_the_crate(self):
        """Публичный вход даёт ровно те же identity, что и extract_run_state gate-а."""

        report = build_report(self.repo_root, run=2)
        state = coordinates.extract_run_state(
            report, self.policy, self.profile, self.repo_root, "run-2"
        )
        files = state["source_files"]["universe"]
        probe = coordinates.crate_coordinate_sets(report, self.repo_root, "alpha")
        for metric in ("lines", "functions", "regions"):
            gate_universe = {
                coordinate_identity(metric, item, files)
                for item in state["stable_source"]["coordinates"][metric]["universe"]
                if files[item[0]].startswith("crates/alpha/")
            }
            self.assertEqual(set(probe[metric].universe), gate_universe, metric)
            self.assertTrue(probe[metric].covered <= probe[metric].universe, metric)
            # Чужой crate и зависимости в выборку зонда не попадают.
            self.assertFalse(any("crates/shell/" in item for item in probe[metric].universe))

    def test_cohort_states_decode_exact_stable_counts(self):
        """Декодированные множества совпадают со счётчиками домена crate-а в cohort."""

        states = focus.load_cohort_crate_states(self.cohort, "alpha")
        for metric, state in states.items():
            stable, total = self.cohort_counts(metric)
            self.assertEqual((len(state.stable), len(state.universe)), (stable, total), metric)

    def test_probe_coverage_closes_deficit_left_by_variable_cohort_lines(self):
        """Строка, покрытая не во всех прогонах, засчитывается, если её покрыл зонд."""

        stable, total = self.cohort_counts("lines")
        # Цель на одну строку выше текущего stable cohort-а.
        targets = {metric: self.cohort_counts(metric) for metric in ("functions", "regions")}
        targets["lines"] = (stable + 1, total)
        estimates = focus.estimate_crate(
            {name: focus.CoverageTarget(*pair) for name, pair in targets.items()},
            focus.load_cohort_crate_states(self.cohort, "alpha"),
            coordinates.crate_coordinate_sets(
                build_report(self.repo_root, run=2), self.repo_root, "alpha"
            ),
        )
        lines = next(item for item in estimates if item.metric == "lines")
        self.assertGreater(len(lines.estimated_covered), stable)
        self.assertEqual(lines.deficit, 0)
        self.assertTrue(all(item.deficit == 0 for item in estimates))

    def test_unreachable_target_reports_exact_deficit_and_uncovered_lines(self):
        """Недостижимая без новых тестов цель даёт точный дефицит и список строк."""

        _, total = self.cohort_counts("lines")
        targets = {
            "lines": focus.CoverageTarget(total, total),
            "functions": focus.CoverageTarget(*self.cohort_counts("functions")),
            "regions": focus.CoverageTarget(*self.cohort_counts("regions")),
        }
        cohort_states = focus.load_cohort_crate_states(self.cohort, "alpha")
        # Зонд не покрыл ничего сверх cohort-а: прогон 1 без переменных строк.
        probe_sets = coordinates.crate_coordinate_sets(
            build_report(self.repo_root, run=1), self.repo_root, "alpha"
        )
        lines = focus.estimate_crate(targets, cohort_states, probe_sets)[0]
        self.assertEqual(lines.required_stable, total)
        self.assertEqual(lines.deficit, total - len(lines.estimated_covered))
        grouped = focus.uncovered_lines_by_file(lines)
        self.assertEqual(set(grouped), {"crates/alpha/src/lib.rs"})
        reported_lines = sum(end - start + 1 for start, end in grouped["crates/alpha/src/lib.rs"])
        self.assertEqual(reported_lines, len(lines.uncovered))
        rendered = focus.format_crate_report("alpha", [lines], max_files=8)
        self.assertIn(f"не хватает {lines.deficit}", rendered)
        self.assertIn("crates/alpha/src/lib.rs:", rendered)

    def test_function_deficit_lists_uncovered_function_definitions(self):
        """При дефиците функций отчёт называет конкретные непокрытые определения."""

        functions_total = self.cohort_counts("functions")[1]
        targets = {
            "lines": focus.CoverageTarget(*self.cohort_counts("lines")),
            "functions": focus.CoverageTarget(functions_total, functions_total),
            "regions": focus.CoverageTarget(*self.cohort_counts("regions")),
        }
        estimates = focus.estimate_crate(
            targets,
            focus.load_cohort_crate_states(self.cohort, "alpha"),
            coordinates.crate_coordinate_sets(
                build_report(self.repo_root, run=1), self.repo_root, "alpha"
            ),
        )
        functions = next(item for item in estimates if item.metric == "functions")
        self.assertGreater(functions.deficit, 0)
        listed = focus.uncovered_functions(functions)
        self.assertEqual(len(listed), len(functions.uncovered))
        rendered = focus.format_crate_report("alpha", estimates, max_files=8)
        path, line = listed[0]
        self.assertIn(f"непокрытые функции: {path}:{line}", rendered)

    def test_probe_coordinates_outside_cohort_universe_are_not_counted(self):
        """Координаты нового кода нельзя засчитать в цель старого universe."""

        cohort_states = focus.load_cohort_crate_states(self.cohort, "alpha")
        foreign = '["L","crates/alpha/src/lib.rs",9999]'
        probe = coordinates.crate_coordinate_sets(
            build_report(self.repo_root, run=2), self.repo_root, "alpha"
        )
        probe["lines"] = coordinates.CrateCoordinateSets(
            probe["lines"].universe | {foreign}, probe["lines"].covered | {foreign}
        )
        targets = {
            metric: focus.CoverageTarget(*self.cohort_counts(metric))
            for metric in ("lines", "functions", "regions")
        }
        lines = focus.estimate_crate(targets, cohort_states, probe)[0]
        self.assertNotIn(foreign, lines.estimated_covered)
        self.assertEqual(lines.probe_only_count, 1)
        self.assertIn("нет в cohort", focus.format_crate_report("alpha", [lines], 8))

    def test_absent_crate_and_inconsistent_cohort_fail_closed(self):
        """Отсутствующий crate и рассогласованный cohort — ошибка, а не нулевая оценка."""

        with self.assertRaises(focus.FocusError):
            focus.load_cohort_crate_states(self.cohort, "missing-crate")
        with self.assertRaises(focus.FocusError):
            focus.load_baseline_targets(baseline_with_targets("alpha", {}), "beta")
        broken = json.loads(json.dumps(self.cohort))
        broken["stable_source"]["domains"]["crate:alpha"]["lines"]["counts"]["total"] += 1
        with self.assertRaises(focus.FocusError):
            focus.load_cohort_crate_states(broken, "alpha")

    def run_main(self, baseline: dict, probe_report: dict | None) -> tuple[int, str, str]:
        """Запускает CLI целиком; cargo заменён готовым LLVM export-ом или ошибкой."""

        cohort_path = self.repo_root / "cohort.json"
        baseline_path = self.repo_root / "baseline.json"
        cohort_path.write_text(json.dumps(self.cohort), encoding="utf-8")
        baseline_path.write_text(json.dumps(baseline), encoding="utf-8")
        output_directory = self.repo_root / "target/coverage/focus"

        def fake_probe(crate, toolchain, repo_root, output):
            self.assertEqual((crate, toolchain, output), ("alpha", "1.96.0", output_directory))
            if probe_report is None:
                raise focus.FocusError("cargo llvm-cov для `alpha` завершился с кодом 101")
            output.mkdir(parents=True, exist_ok=True)
            report_path = output / f"{crate}.json"
            report_path.write_text(json.dumps(probe_report), encoding="utf-8")
            return report_path

        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(focus, "run_crate_probe", fake_probe):
            with redirect_stdout(stdout), redirect_stderr(stderr):
                status = focus.main([
                    "--repo-root", str(self.repo_root), "--toolchain", "1.96.0",
                    "--cohort", str(cohort_path), "--baseline", str(baseline_path),
                    "--output-directory", str(output_directory), "alpha",
                ])
        return status, stdout.getvalue(), stderr.getvalue()

    def test_cli_exit_status_distinguishes_closed_deficit_remaining_and_failure(self):
        """0 — цель достигнута, 1 — дефицит остался, 2 — сборка/тесты упали."""

        reachable = baseline_with_targets(
            "alpha", {metric: self.cohort_counts(metric) for metric in ("lines", "functions", "regions")}
        )
        status, output, _ = self.run_main(reachable, build_report(self.repo_root, run=2))
        self.assertEqual(status, 0, output)
        self.assertIn("== alpha", output)

        lines_total = self.cohort_counts("lines")[1]
        unreachable = json.loads(json.dumps(reachable))
        unreachable["stable_source"]["domains"]["crate:alpha"]["lines"]["counts"] = {
            "stable": lines_total, "total": lines_total,
        }
        status, output, _ = self.run_main(unreachable, build_report(self.repo_root, run=1))
        self.assertEqual(status, 1, output)
        self.assertIn("не хватает", output)

        status, _, error_output = self.run_main(reachable, None)
        self.assertEqual(status, 2)
        self.assertIn("завершился с кодом 101", error_output)
        # Зонд ничего не пишет вне своего каталога под target/.
        written = {path.name for path in self.repo_root.iterdir()}
        self.assertEqual(written, {"Cargo.toml", "cohort.json", "baseline.json", "target"})


if __name__ == "__main__":
    unittest.main()

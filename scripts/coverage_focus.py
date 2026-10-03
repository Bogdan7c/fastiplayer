#!/usr/bin/env python3
"""Быстрый report-only зонд покрытия отдельных crate-ов.

Зачем: полный stable-coverage gate (`scripts/coverage.sh check`) — это
инструментированная сборка всего workspace и три полных прогона тестов
(~15–20 минут). Пока пишутся тесты, чтобы закрыть снижение покрытия
конкретного crate-а, такой цикл слишком дорогой. Зонд за один прогон тестов
одного package-а (обычно меньше минуты) отвечает на вопрос: «сколько ещё
координат crate-у не хватает до целевой доли из baseline и где они».

Как считает (нижняя оценка, НЕ замена gate-у):
- цель — доля `stable/total` crate-а из `coverage/baseline.json`;
- база — stable-координаты crate-а из последнего полного cohort-а
  (`target/coverage/stable/cohort.json`): их уже покрывают тесты всего workspace;
- прибавка — координаты, которые покрыли собственные тесты crate-а в прогоне
  `cargo llvm-cov --package <crate>`. Координаты извлекаются тем же кодом, что и
  в gate (`coverage_coordinates.crate_coordinate_sets`), поэтому identity совпадают.

Ограничения (поэтому финальное слово всегда за полным `scripts/coverage.sh`):
- один прогон не доказывает стабильность 3/3;
- зонд использует обычный merged export без per-object union gate-а, поэтому
  может недосчитать (оценка остаётся нижней);
- если production-код crate-а изменился после cohort-а, identity расходятся —
  зонд показывает это как дрейф universe.

Зонд пишет только в каталог `--output-directory` (под ignored `target/`) и не
читает/не меняет measurement-exceptions; baseline только читает.

Exit status: 0 — по оценке дефицита нет ни в одной метрике, 1 — дефицит
остался, 2 — ошибка запуска, входных данных или сборки/тестов.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from coverage_coordinate_model import (
    METRICS,
    coordinate_identity,
    crate_name,
    read_json,
    require_array,
    require_int,
    require_object,
)
from coverage_coordinates import CrateCoordinateSets, crate_coordinate_sets


class FocusError(Exception):
    """Ошибка входных данных или запуска зонда (exit status 2)."""


@dataclass(frozen=True)
class CohortCrateState:
    """Координаты одного crate-а и метрики из последнего полного cohort-а."""

    universe: frozenset[str]
    stable: frozenset[str]


@dataclass(frozen=True)
class CoverageTarget:
    """Целевая доля `stable/total` crate-а из tracked baseline."""

    stable: int
    total: int


@dataclass(frozen=True)
class MetricEstimate:
    """Оценка одной метрики crate-а после прогона зонда."""

    metric: str
    target: CoverageTarget
    cohort_stable_count: int
    estimated_covered: frozenset[str]
    universe: frozenset[str]
    # Координаты зонда, которых нет в universe cohort-а (код изменился после cohort-а).
    probe_only_count: int
    # Координаты cohort-а, которых зонд не увидел (например, generic-и из чужих crate-ов).
    cohort_only_count: int

    @property
    def required_stable(self) -> int:
        """Минимальное число покрытых координат, при котором доля не ниже цели."""

        if self.target.total == 0:
            return 0
        # ceil(target.stable * |universe| / target.total) в целых числах, без float.
        return -(-self.target.stable * len(self.universe) // self.target.total)

    @property
    def deficit(self) -> int:
        """Сколько координат ещё нужно покрыть; 0 — цель достигнута по оценке."""

        return max(0, self.required_stable - len(self.estimated_covered))

    @property
    def uncovered(self) -> frozenset[str]:
        """Координаты universe, не покрытые ни cohort-ом, ни зондом."""

        return self.universe - self.estimated_covered


def load_cohort_crate_states(cohort: Any, crate: str) -> dict[str, CohortCrateState]:
    """Декодирует stable-координаты crate-а из cohort в межкоммитные identity."""

    document = require_object(cohort, "cohort")
    source_files = require_array(
        require_object(document.get("source_files"), "cohort.source_files").get("universe"),
        "cohort.source_files.universe",
    )
    stable_source = require_object(document.get("stable_source"), "cohort.stable_source")
    domains = require_object(stable_source.get("domains"), "cohort.stable_source.domains")
    domain_key = f"crate:{crate}"
    if domain_key not in domains:
        raise FocusError(f"crate `{crate}` отсутствует в cohort: нечего сравнивать")
    domain = require_object(domains[domain_key], f"cohort.domains.{domain_key}")
    coordinates = require_object(stable_source.get("coordinates"), "cohort.coordinates")
    states: dict[str, CohortCrateState] = {}
    for metric in METRICS:
        encoded = require_array(
            require_object(coordinates.get(metric), f"coordinates.{metric}").get("universe"),
            f"coordinates.{metric}.universe",
        )
        identities = [coordinate_identity(metric, item, source_files) for item in encoded]
        universe = frozenset(
            identity
            for identity, item in zip(identities, encoded)
            if crate_name(source_files[item[0]]) == crate
        )
        metric_domain = require_object(domain.get(metric), f"{domain_key}.{metric}")
        stable = frozenset(
            identities[index]
            for start, end in require_array(metric_domain.get("stable_ranges"), "stable_ranges")
            for index in range(start, end)
        )
        counts = require_object(metric_domain.get("counts"), f"{domain_key}.{metric}.counts")
        # Fail closed: декодированные множества обязаны совпасть со счётчиками cohort-а.
        if len(universe) != require_int(counts.get("total"), "counts.total") or not (
            stable <= universe
        ):
            raise FocusError(f"cohort {domain_key}.{metric} не согласован со своими счётчиками")
        states[metric] = CohortCrateState(universe=universe, stable=stable)
    return states


def load_baseline_targets(baseline: Any, crate: str) -> dict[str, CoverageTarget]:
    """Читает целевые доли crate-а из tracked baseline (только чтение)."""

    stable_source = require_object(
        require_object(baseline, "baseline").get("stable_source"), "baseline.stable_source"
    )
    domains = require_object(stable_source.get("domains"), "baseline.domains")
    domain_key = f"crate:{crate}"
    if domain_key not in domains:
        raise FocusError(f"crate `{crate}` отсутствует в baseline: цели нет")
    targets: dict[str, CoverageTarget] = {}
    for metric in METRICS:
        counts = require_object(
            require_object(domains[domain_key].get(metric), f"{domain_key}.{metric}").get(
                "counts"
            ),
            f"{domain_key}.{metric}.counts",
        )
        targets[metric] = CoverageTarget(
            stable=require_int(counts.get("stable"), "counts.stable"),
            total=require_int(counts.get("total"), "counts.total"),
        )
    return targets


def estimate_crate(
    targets: dict[str, CoverageTarget],
    cohort_states: dict[str, CohortCrateState],
    probe_sets: dict[str, CrateCoordinateSets],
) -> list[MetricEstimate]:
    """Чистая оценка: stable cohort-а ∪ покрытое зондом внутри universe cohort-а."""

    estimates: list[MetricEstimate] = []
    for metric in METRICS:
        cohort_state = cohort_states[metric]
        probe = probe_sets[metric]
        # Покрытое зондом засчитывается только внутри universe cohort-а:
        # чужие координаты нельзя сравнивать с целью, посчитанной по старому universe.
        estimated = cohort_state.stable | (probe.covered & cohort_state.universe)
        estimates.append(
            MetricEstimate(
                metric=metric,
                target=targets[metric],
                cohort_stable_count=len(cohort_state.stable),
                estimated_covered=frozenset(estimated),
                universe=cohort_state.universe,
                probe_only_count=len(probe.universe - cohort_state.universe),
                cohort_only_count=len(cohort_state.universe - probe.universe),
            )
        )
    return estimates


def uncovered_lines_by_file(estimate: MetricEstimate) -> dict[str, list[tuple[int, int]]]:
    """Группирует непокрытые строки по файлам в сжатые диапазоны `[начало, конец]`."""

    lines_by_file: dict[str, list[int]] = {}
    for identity in estimate.uncovered:
        kind, path, line = json.loads(identity)
        if kind == "L":
            lines_by_file.setdefault(path, []).append(line)
    grouped: dict[str, list[tuple[int, int]]] = {}
    for path, lines in lines_by_file.items():
        spans: list[tuple[int, int]] = []
        for line in sorted(lines):
            if spans and spans[-1][1] + 1 == line:
                spans[-1] = (spans[-1][0], line)
            else:
                spans.append((line, line))
        grouped[path] = spans
    return grouped


def uncovered_functions(estimate: MetricEstimate) -> list[tuple[str, int]]:
    """Непокрытые функции (включая замыкания) как пары «файл, строка определения»."""

    functions: list[tuple[str, int]] = []
    for identity in estimate.uncovered:
        kind, path, line, _column = json.loads(identity)
        if kind == "F":
            functions.append((path, line))
    return sorted(functions)


def format_crate_report(
    crate: str, estimates: list[MetricEstimate], max_files: int
) -> str:
    """Человекочитаемый отчёт по crate-у (форматирование отделено от расчёта)."""

    output = [f"== {crate}"]
    for estimate in estimates:
        status = "OK" if estimate.deficit == 0 else f"не хватает {estimate.deficit}"
        output.append(
            f"  {estimate.metric:9} оценка {len(estimate.estimated_covered)}/"
            f"{len(estimate.universe)} (cohort {estimate.cohort_stable_count} + зонд "
            f"{len(estimate.estimated_covered) - estimate.cohort_stable_count}), цель "
            f"{estimate.target.stable}/{estimate.target.total} → нужно "
            f">= {estimate.required_stable}: {status}"
        )
        if estimate.probe_only_count:
            output.append(
                f"    внимание: {estimate.probe_only_count} координат зонда нет в cohort "
                "(код изменился после cohort-а — нужен полный замер)"
            )
    function_estimate = next(
        (item for item in estimates if item.metric == "functions"), None
    )
    # Список функций печатается только при дефиците: иначе он лишь шумит.
    if function_estimate is not None and function_estimate.deficit:
        rendered_functions = ", ".join(
            f"{path}:{line}" for path, line in uncovered_functions(function_estimate)
        )
        output.append(f"  непокрытые функции: {rendered_functions}")
    line_estimate = next(item for item in estimates if item.metric == "lines")
    grouped = uncovered_lines_by_file(line_estimate)
    if grouped:
        output.append("  непокрытые строки (файлы с наибольшим числом строк):")
        ordered = sorted(
            grouped.items(),
            key=lambda entry: (-sum(end - start + 1 for start, end in entry[1]), entry[0]),
        )
        for path, spans in ordered[:max_files]:
            rendered = ", ".join(
                str(start) if start == end else f"{start}-{end}" for start, end in spans
            )
            output.append(f"    {path}: {rendered}")
        if len(ordered) > max_files:
            output.append(f"    … ещё файлов: {len(ordered) - max_files}")
    return "\n".join(output)


def run_crate_probe(
    crate: str, toolchain: str, repo_root: Path, output_directory: Path
) -> Path:
    """Один прогон тестов package-а под инструментированием; возвращает путь к LLVM JSON."""

    output_directory.mkdir(parents=True, exist_ok=True)
    report_path = output_directory / f"{crate}.json"
    command = [
        "cargo",
        f"+{toolchain}",
        "llvm-cov",
        "--package",
        crate,
        "--all-features",
        "--locked",
        "--json",
        "--output-path",
        str(report_path),
    ]
    environment = dict(os.environ)
    # Отдельный target dir: зонд не трогает profiles/артефакты полного gate-а.
    environment["CARGO_TARGET_DIR"] = str(output_directory / "target")
    completed = subprocess.run(command, cwd=repo_root, env=environment, check=False)
    if completed.returncode != 0:
        raise FocusError(
            f"cargo llvm-cov для `{crate}` завершился с кодом {completed.returncode} "
            "(сборка или тесты упали — см. вывод выше)"
        )
    return report_path


def parse_args(arguments: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Быстрый report-only зонд покрытия отдельных crate-ов."
    )
    parser.add_argument("--repo-root", type=Path, required=True)
    parser.add_argument("--toolchain", required=True)
    parser.add_argument("--cohort", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--output-directory", type=Path, required=True)
    parser.add_argument(
        "--max-files", type=int, default=8, help="сколько файлов с пробелами показывать"
    )
    parser.add_argument("crates", nargs="+", metavar="CRATE")
    return parser.parse_args(arguments)


def main(arguments: list[str] | None = None) -> int:
    parsed = parse_args(arguments)
    repo_root = parsed.repo_root.resolve()
    try:
        cohort = read_json(parsed.cohort)
        baseline = read_json(parsed.baseline)
        any_deficit = False
        for crate in parsed.crates:
            targets = load_baseline_targets(baseline, crate)
            cohort_states = load_cohort_crate_states(cohort, crate)
            report_path = run_crate_probe(
                crate, parsed.toolchain, repo_root, parsed.output_directory
            )
            probe_sets = crate_coordinate_sets(read_json(report_path), repo_root, crate)
            estimates = estimate_crate(targets, cohort_states, probe_sets)
            any_deficit = any_deficit or any(item.deficit for item in estimates)
            print(format_crate_report(crate, estimates, parsed.max_files))
    except (FocusError, OSError, ValueError) as error:
        print(f"Ошибка coverage focus: {error}", file=sys.stderr)
        return 2
    print(
        "Оценка нижняя и не доказывает стабильность 3/3; "
        "финальное решение — только полный scripts/coverage.sh."
    )
    return 1 if any_deficit else 0


if __name__ == "__main__":
    raise SystemExit(main())

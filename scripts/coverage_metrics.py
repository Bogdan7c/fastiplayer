#!/usr/bin/env python3
"""Строит компактную карту покрытия и проверяет coverage ratchet."""

# argparse описывает явный CLI-контракт без ручного разбора строк.
import argparse
# datetime проверяет, что срок пересмотра исключения ещё не истёк.
import datetime as dt
# json читает LLVM export и хранит компактные versioned документы.
import json
# pathlib сохраняет работу с путями независимой от текущего каталога.
from pathlib import Path
# sys нужен для понятного ненулевого завершения при policy failure.
import sys

# Корень репозитория вычисляется относительно этого versioned скрипта.
REPO_ROOT = Path(__file__).resolve().parent.parent
# Политика является единственным владельцем классификации crate-ов и метрик.
POLICY_PATH = REPO_ROOT / "coverage" / "policy.json"
# Baseline содержит только агрегаты, необходимые для ratchet.
BASELINE_PATH = REPO_ROOT / "coverage" / "baseline.json"
# Исключения отделены от baseline, чтобы снижение требовало объяснения.
EXCEPTIONS_PATH = REPO_ROOT / "coverage" / "exceptions.json"


# Функция читает UTF-8 JSON и возвращает типизированную Python-структуру.
def read_json(document_path: Path):
    # with гарантированно закрывает файл и не прячет ошибки чтения/JSON.
    with document_path.open(encoding="utf-8") as document_file:
        # Ошибка формата намеренно выходит наружу с точным путём/строкой.
        return json.load(document_file)


# Функция атомарно для процесса записывает детерминированный JSON.
def write_json(document_path: Path, document) -> None:
    # Родительский каталог создаётся для local artifact, но не хардкодится в CI.
    document_path.parent.mkdir(parents=True, exist_ok=True)
    # Стабильные отступы и sort_keys делают review baseline компактным.
    rendered_document = json.dumps(document, ensure_ascii=False, indent=2, sort_keys=True)
    # Финальный перевод строки сохраняет POSIX-friendly текстовый файл.
    document_path.write_text(f"{rendered_document}\n", encoding="utf-8")


# Функция извлекает имя workspace crate из абсолютного LLVM filename.
def crate_name_for_file(filename: str) -> str | None:
    # resolve не требуется: LLVM уже отдаёт абсолютный canonical-like путь.
    source_path = Path(filename)
    # Файлы вне first-party crates не должны случайно попасть в workspace policy.
    try:
        relative_source_path = source_path.relative_to(REPO_ROOT / "crates")
    # ValueError означает dependency/toolchain или иной внешний исходник.
    except ValueError:
        # None явно обозначает файл вне управляемой first-party области.
        return None
    # Первый компонент после crates/ совпадает с каталогом workspace member-а.
    return relative_source_path.parts[0]


# Функция создаёт пустые целочисленные счётчики для каждой ratchet-метрики.
def empty_metrics(metric_names: list[str]) -> dict[str, dict[str, int]]:
    # В baseline хранятся covered и total, а не округлённые проценты.
    return {metric_name: {"covered": 0, "total": 0} for metric_name in metric_names}


# Функция добавляет summary одного файла к агрегату crate/workspace.
def add_file_summary(aggregate, file_summary, metric_names: list[str]) -> None:
    # Каждая метрика суммируется независимо, чтобы regression называла owner.
    for metric_name in metric_names:
        # LLVM называет знаменатель count; наружу используем понятное total.
        aggregate[metric_name]["covered"] += file_summary[metric_name]["covered"]
        # Суммарный total нужен для точного сравнения дробей без float.
        aggregate[metric_name]["total"] += file_summary[metric_name]["count"]


# Функция преобразует LLVM summary в компактную карту policy scopes.
def build_summary(llvm_report, policy):
    # Raw report обязан быть создан exact release из versioned policy.
    actual_tool_version = llvm_report.get("cargo_llvm_cov", {}).get("version")
    # Несовпадающая версия может изменить instrumentation/report semantics.
    if actual_tool_version != policy["tool"]["version"]:
        # Диагностика показывает обе версии до любого сравнения counters.
        raise ValueError(
            "LLVM report создан cargo-llvm-cov "
            f"{actual_tool_version}, требуется {policy['tool']['version']}"
        )
    # Экспорт cargo-llvm-cov должен содержать ровно один merged report.
    if len(llvm_report.get("data", [])) != 1:
        # Явная ошибка защищает от молчаливого выбора неправильного report-а.
        raise ValueError("ожидался ровно один merged LLVM coverage report")
    # Список метрик versioned, поэтому изменение набора видно в review.
    metric_names = policy["metrics"]
    # Полный список first-party owners запрещает незаметно потерять новый crate.
    expected_crates = set(policy["blocking_crates"]) | set(policy["informational_crates"])
    # Пересечение групп разрушило бы смысл blocking/informational boundary.
    if set(policy["blocking_crates"]) & set(policy["informational_crates"]):
        # Policy failure останавливает baseline generation до неверных чисел.
        raise ValueError("crate не может быть одновременно blocking и informational")
    # Агрегат workspace считается по тем же first-party файлам, что группы.
    workspace_metrics = empty_metrics(metric_names)
    # Отдельный агрегат pure contract/business показывает риск hermetic ядра.
    blocking_group_metrics = empty_metrics(metric_names)
    # Каждый owner получает отдельный агрегат, включая crate с нулевым покрытием.
    crate_metrics = {crate_name: empty_metrics(metric_names) for crate_name in sorted(expected_crates)}
    # Исключения source paths пока пусты; каждое будущее значение проверяется буквально.
    excluded_paths = set(policy["excluded_source_paths"])
    # Множество реально увиденных crate-ов выявляет забытый workspace member.
    observed_crates = set()
    # Summary-only JSON содержит один элемент на instrumented first-party файл.
    for file_entry in llvm_report["data"][0]["files"]:
        # Имя crate выводится только из пути внутри repo/crates.
        crate_name = crate_name_for_file(file_entry["filename"])
        # Внешние файлы не принадлежат workspace coverage contract.
        if crate_name is None:
            # Пропуск внешнего dependency является scope boundary, не exclusion.
            continue
        # Относительный путь стабилен между локальной машиной и CI runner-ом.
        relative_filename = str(Path(file_entry["filename"]).relative_to(REPO_ROOT))
        # Только документированные точные пути могут быть исключены.
        if relative_filename in excluded_paths:
            # Excluded generated/raw/manual source не влияет на агрегаты.
            continue
        # Новый crate обязан сначала получить осознанную классификацию policy.
        if crate_name not in expected_crates:
            # Ошибка называет owner вместо молчаливого изменения workspace числа.
            raise ValueError(f"crate `{crate_name}` отсутствует в coverage policy")
        # Наблюдение подтверждает наличие instrumented исходников crate-а.
        observed_crates.add(crate_name)
        # Crate aggregate используется для focused ratchet и risk map.
        add_file_summary(crate_metrics[crate_name], file_entry["summary"], metric_names)
        # Workspace aggregate отслеживает общий hermetic suite без threshold-процента.
        add_file_summary(workspace_metrics, file_entry["summary"], metric_names)
        # Pure group aggregate не смешивается с hardware/FFI/UI shell paths.
        if crate_name in policy["blocking_crates"]:
            # Та же file summary входит в group ровно один раз.
            add_file_summary(blocking_group_metrics, file_entry["summary"], metric_names)
    # Crate без LLVM files обычно означает ошибку features/instrumentation или stale policy.
    missing_crates = expected_crates - observed_crates
    # Нельзя публиковать baseline, который молча не измерил заявленного owner-а.
    if missing_crates:
        # Сортировка делает диагностику воспроизводимой.
        raise ValueError(f"coverage report не содержит crate-ы: {', '.join(sorted(missing_crates))}")
    # Компактный документ не содержит raw regions, profdata или абсолютные пути.
    return {
        "schema_version": 1,
        "tool": policy["tool"],
        "workspace": workspace_metrics,
        "blocking_group": blocking_group_metrics,
        "blocking_crates": {
            crate_name: crate_metrics[crate_name] for crate_name in policy["blocking_crates"]
        },
        "informational_crates": {
            crate_name: crate_metrics[crate_name] for crate_name in policy["informational_crates"]
        },
    }


# Функция сравнивает две дроби точно, не используя округлённый percent.
def ratio_decreased(current_metric, baseline_metric) -> bool:
    # Нулевая новая область не является измеримой и поэтому считается regression.
    if current_metric["total"] == 0:
        # Baseline с нулём тоже не должен встречаться из-за observed-crate guard.
        return baseline_metric["total"] != 0
    # Cross multiplication сохраняет точность целых LLVM counters.
    return (
        current_metric["covered"] * baseline_metric["total"]
        < baseline_metric["covered"] * current_metric["total"]
    )


# Функция форматирует долю только для человеческой диагностики.
def format_ratio(metric) -> str:
    # Защита от деления на ноль сохраняет понятный вывод для broken report-а.
    if metric["total"] == 0:
        # Строка не маскирует отсутствие измеряемого кода как 100%.
        return "n/a"
    # Процент округляется только в сообщении, но не в решении ratchet.
    percent = metric["covered"] * 100 / metric["total"]
    # Счётчики позволяют вручную проверить любое округление.
    return f"{metric['covered']}/{metric['total']} ({percent:.4f}%)"


# Функция проверяет workspace и каждый pure contract/business crate.
def find_regressions(current_summary, baseline, metric_names: list[str]):
    # Список сохраняет все failures, а не останавливается на первом owner-е.
    regressions = []
    # Workspace aggregate является отдельным blocking scope.
    blocking_scopes = [("workspace", current_summary["workspace"], baseline["workspace"])]
    # Pure contract/business aggregate закрепляет границу групп целиком.
    blocking_scopes.append(
        ("blocking-group", current_summary["blocking_group"], baseline["blocking_group"])
    )
    # Каждый pure crate добавляется как именованный owner риска.
    for crate_name, baseline_metrics in baseline["blocking_crates"].items():
        # Отсутствующий crate уже ловится build_summary, здесь структура симметрична.
        current_metrics = current_summary["blocking_crates"][crate_name]
        # Scope включает crate prefix для однозначного exception key.
        blocking_scopes.append((f"crate:{crate_name}", current_metrics, baseline_metrics))
    # Все выбранные метрики ratchet-ятся независимо.
    for scope_name, current_metrics, baseline_metrics in blocking_scopes:
        # Versioned metric list не позволяет baseline скрыть один показатель.
        for metric_name in metric_names:
            # Снижение точной доли является unrecorded regression.
            if ratio_decreased(current_metrics[metric_name], baseline_metrics[metric_name]):
                # Диагностика сохраняет обе пары counters для review.
                regressions.append(
                    {
                        "scope": scope_name,
                        "metric": metric_name,
                        "baseline": baseline_metrics[metric_name],
                        "current": current_metrics[metric_name],
                    }
                )
    # Пустой список означает прохождение zero-unrecorded-regression ratchet.
    return regressions


# Функция печатает найденные regression единым понятным блоком.
def print_regressions(regressions) -> None:
    # Заголовок объясняет, почему команда возвращает failure.
    print("Coverage ratchet обнаружил уменьшение:", file=sys.stderr)
    # Каждая строка называет metric, owner и точные старые/новые counters.
    for regression in regressions:
        # Формат пригоден человеку и остаётся компактным в CI log.
        print(
            f"- {regression['scope']} / {regression['metric']}: "
            f"{format_ratio(regression['baseline'])} -> {format_ratio(regression['current'])}",
            file=sys.stderr,
        )


# Функция проверяет обязательные поля и срок versioned exception.
def validate_exception(exception) -> None:
    # Набор полей делает снижение bounded и проверяемым в code review.
    required_fields = {
        "scope",
        "metric",
        "previous",
        "allowed",
        "reason",
        "review_by",
        "follow_up",
    }
    # Missing fields нельзя заменить неявными defaults.
    missing_fields = required_fields - set(exception)
    # Ошибка перечисляет все недостающие обязательства.
    if missing_fields:
        # ValueError приводит к ненулевому policy status.
        raise ValueError(f"coverage exception не содержит: {', '.join(sorted(missing_fields))}")
    # ISO date обеспечивает машиночитаемый срок пересмотра.
    review_date = dt.date.fromisoformat(exception["review_by"])
    # Просроченное исключение больше не разрешает baseline regression.
    if review_date < dt.date.today():
        # Scope/metric позволяют сразу найти запись для исправления.
        raise ValueError(
            f"coverage exception {exception['scope']}/{exception['metric']} просрочено {review_date}"
        )
    # Пустая причина превращала бы exception в молчаливое ослабление.
    if not exception["reason"].strip():
        # Явный failure требует объяснить конкретную причину.
        raise ValueError("coverage exception требует непустую reason")
    # Follow-up должен быть bounded проверяемой задачей, а не обещанием без ссылки.
    if not exception["follow_up"].strip():
        # Непустой идентификатор/путь обязателен для последующей работы.
        raise ValueError("coverage exception требует непустой bounded follow_up")


# Функция доказывает, что каждое снижение baseline имеет точное exception.
def validate_baseline_update(previous_baseline, proposed_baseline, exceptions, metric_names) -> None:
    # Переиспользуем ту же точную ratchet-логику для proposed baseline.
    regressions = find_regressions(proposed_baseline, previous_baseline, metric_names)
    # Каждая exception сначала проходит schema/deadline validation.
    for exception in exceptions:
        # Ошибка одной записи блокирует всё policy update.
        validate_exception(exception)
    # Индекс по metric/crate гарантирует точечное, а не глобальное разрешение.
    exception_index = {(item["scope"], item["metric"]): item for item in exceptions}
    # Собираем все снижения без точного разрешения для единой диагностики.
    unrecorded_regressions = []
    # Каждое уменьшение должно совпасть со старыми и новыми counters.
    for regression in regressions:
        # Ключ отражает требование metric/crate из общего плана.
        exception = exception_index.get((regression["scope"], regression["metric"]))
        # Отсутствующая запись не разрешает обновление baseline.
        if exception is None:
            # Regression будет напечатана стандартным formatter-ом.
            unrecorded_regressions.append(regression)
            # Дальнейшая проверка полей невозможна без записи.
            continue
        # Exact previous counters не позволяют переиспользовать старое исключение.
        if exception["previous"] != regression["baseline"]:
            # Несовпадение трактуется как незаписанное текущее снижение.
            unrecorded_regressions.append(regression)
            # allowed всё равно не может сделать запись валидной.
            continue
        # Exact allowed counters ограничивают exception конкретным baseline.
        if exception["allowed"] != regression["current"]:
            # Более широкое или устаревшее разрешение не принимается.
            unrecorded_regressions.append(regression)
    # Любое неразрешённое снижение блокирует команду и CI.
    if unrecorded_regressions:
        # Стандартная диагностика показывает точные необходимые пары.
        print_regressions(unrecorded_regressions)
        # Дополнение объясняет required remediation без программного жаргона.
        raise ValueError("обновление baseline требует точного непросроченного exception")


# Функция создаёт CLI с отдельными командами generate/check/update-check.
def parse_args():
    # Описание появляется в --help и CI diagnostics.
    parser = argparse.ArgumentParser(description=__doc__)
    # Subcommand предотвращает случайную перезапись baseline проверкой.
    subparsers = parser.add_subparsers(dest="command", required=True)
    # generate строит compact summary из raw LLVM JSON.
    generate_parser = subparsers.add_parser("generate", help="создать compact summary")
    # Вход остаётся artifact path и не коммитится.
    generate_parser.add_argument("--input", type=Path, required=True)
    # Выход может быть versioned baseline или informational artifact.
    generate_parser.add_argument("--output", type=Path, required=True)
    # check сравнивает текущий raw report с versioned baseline.
    check_parser = subparsers.add_parser("check", help="проверить ratchet")
    # Raw input нужен для независимого пересчёта, а не доверия artifact summary.
    check_parser.add_argument("--input", type=Path, required=True)
    # update-check сравнивает два baseline и exception-файл.
    update_parser = subparsers.add_parser(
        "check-baseline-update", help="проверить осознанное уменьшение baseline"
    )
    # Previous baseline экспортируется CI из base branch во временный файл.
    update_parser.add_argument("--previous", type=Path, required=True)
    # Proposed по умолчанию является versioned baseline текущей ветки.
    update_parser.add_argument("--proposed", type=Path, default=BASELINE_PATH)
    # Возвращаем разобранные значения без глобального mutable state.
    return parser.parse_args()


# Главная функция связывает IO, pure aggregation и policy decisions.
def main() -> int:
    # Аргументы разбираются один раз в process boundary.
    arguments = parse_args()
    # Policy всегда читается из versioned single source of truth.
    policy = read_json(POLICY_PATH)
    # generate не читает baseline и потому безопасен для первоначального снимка.
    if arguments.command == "generate":
        # Raw LLVM JSON преобразуется в переносимый compact документ.
        summary = build_summary(read_json(arguments.input), policy)
        # Явный output не даёт случайно заменить baseline.
        write_json(arguments.output, summary)
        # Нулевой status подтверждает успешную генерацию.
        return 0
    # check строит current summary заново из raw artifact.
    if arguments.command == "check":
        # Baseline является versioned точкой сравнения.
        baseline = read_json(BASELINE_PATH)
        # Current counters нельзя подменить вручную compact artifact-ом.
        current_summary = build_summary(read_json(arguments.input), policy)
        # Informational summary публикуется рядом с raw report для CI artifact.
        write_json(REPO_ROOT / "target" / "coverage" / "current-summary.json", current_summary)
        # Ratchet применяется только к workspace и blocking crates.
        regressions = find_regressions(current_summary, baseline, policy["metrics"])
        # Любое снижение возвращает failure после полной диагностики.
        if regressions:
            # Печатаем все затронутые owners/error surfaces.
            print_regressions(regressions)
            # Ненулевой status блокирует CI job.
            return 1
        # Успех явно виден в локальном и CI log.
        print("Coverage ratchet пройден: workspace и blocking crates не уменьшились.")
        # Informational crates намеренно не участвуют в status.
        return 0
    # Осталась только команда проверки versioned baseline update.
    previous_baseline = read_json(arguments.previous)
    # Proposed baseline читается из текущей ветки или явного пути теста.
    proposed_baseline = read_json(arguments.proposed)
    # Versioned exceptions валидируются даже при отсутствии снижения.
    exception_document = read_json(EXCEPTIONS_PATH)
    # Точная проверка запрещает молчаливое уменьшение baseline.
    validate_baseline_update(
        previous_baseline,
        proposed_baseline,
        exception_document["exceptions"],
        policy["metrics"],
    )
    # Успешный status означает, что каждое снижение объяснено и bounded.
    print("Обновление coverage baseline соответствует exception policy.")
    # Возвращаем успешный process status.
    return 0


# Guard не запускает CLI при импорте функций unit-тестами.
if __name__ == "__main__":
    # SystemExit сохраняет возвращённый main status для shell/CI.
    raise SystemExit(main())

#!/usr/bin/env python3
"""Проверяет обслуживаемость семи standalone dependency patches."""

# pathlib даёт типизированные пути без зависимости от текущего каталога.
from pathlib import Path
# sys нужен только для явного ненулевого exit status при policy failure.
import sys
# tomllib читает Cargo manifests и inventory стандартной библиотекой Python 3.11+.
import tomllib

# Корень репозитория вычисляется относительно расположения этого checker-а.
REPO_ROOT = Path(__file__).resolve().parent.parent
# Inventory является единственным machine-readable описанием patch ownership.
INVENTORY_PATH = REPO_ROOT / "docs/dependency-patches.toml"
# Обязательные поля не дают превратить inventory в формальный список имён.
REQUIRED_FIELDS = (
    "path", "package", "version", "upstream_repository", "upstream_revision",
    "reason", "owned_diff_areas", "dependent_crates", "focused_automated_tests",
    "manual_media_matrix", "removal_gate",
)

# Функция читает TOML и сохраняет понятный контекст ошибки файла.
def read_toml(path: Path) -> dict:
    # Бинарный режим является контрактом tomllib.load.
    with path.open("rb") as toml_file:
        # Парсер возвращает обычную вложенную структуру Python.
        return tomllib.load(toml_file)

# Главная проверка собирает все ошибки, чтобы один CI run показывал полный diff policy.
def validate() -> list[str]:
    # Список diagnostics не останавливает проверку после первой записи.
    errors: list[str] = []
    # Root manifest задаёт фактические workspace exclude и replace resolution.
    root_manifest = read_toml(REPO_ROOT / "Cargo.toml")
    # Inventory задаёт ожидаемые identity, ownership и removal gates.
    inventory = read_toml(INVENTORY_PATH)
    # Неизвестная схема должна обновляться осознанно вместе с checker-ом.
    if inventory.get("schema_version") != 1:
        errors.append("docs/dependency-patches.toml: ожидается schema_version = 1")
    # Нормализованный set упрощает точное сравнение standalone directories.
    excluded_paths = set(root_manifest.get("workspace", {}).get("exclude", []))
    # Members нужны для отдельной проверки принятого запрета normal workspace membership.
    workspace_members = set(root_manifest.get("workspace", {}).get("members", []))
    # Replace keys имеют форму package:version и обязаны совпасть с inventory.
    replacements = root_manifest.get("replace", {})
    # Уникальность paths не позволяет одной записи формально заменить отсутствующий fork.
    inventoried_paths: set[str] = set()
    # Каждая запись валидируется независимо для полной диагностики.
    for patch in inventory.get("patch", []):
        # Путь используется в каждом последующем сообщении об ошибке.
        patch_path = patch.get("path", "<missing path>")
        # Все обязательные строки/списки должны быть непустыми.
        for field in REQUIRED_FIELDS:
            # False также ловит пустую строку и пустой список.
            if not patch.get(field):
                errors.append(f"{patch_path}: отсутствует непустое поле {field}")
        # Неполную запись дальше нельзя безопасно сопоставить с manifest.
        if not all(patch.get(field) for field in ("path", "package", "version")):
            continue
        # Standalone crate должен быть явно исключён из normal workspace membership.
        if patch_path not in excluded_paths:
            errors.append(f"{patch_path}: путь отсутствует в workspace.exclude")
        # Принятое решение владельца прямо запрещает добавлять fork в members.
        if patch_path in workspace_members:
            errors.append(f"{patch_path}: standalone patch запрещено добавлять в workspace.members")
        # Повторный path является ошибкой inventory независимо от общего числа записей.
        if patch_path in inventoried_paths:
            errors.append(f"{patch_path}: path повторяется в inventory")
        # Первый occurrence запоминается для проверки следующих записей.
        inventoried_paths.add(patch_path)
        # На диске обязаны существовать самостоятельные manifest и lock-файл.
        patch_directory = REPO_ROOT / patch_path
        # Проверяем оба файла отдельно ради actionable diagnostics.
        for required_name in ("Cargo.toml", "Cargo.lock"):
            # Missing lock делает cargo test --locked невыполнимым.
            if not (patch_directory / required_name).is_file():
                errors.append(f"{patch_path}: отсутствует {required_name}")
        # Manifest identity должен совпадать с заменяемым registry package.
        # Missing manifest уже получил actionable ошибку и не должен вызывать traceback.
        if not (patch_directory / "Cargo.toml").is_file():
            continue
        # Существующий standalone manifest можно безопасно разобрать.
        patch_manifest = read_toml(patch_directory / "Cargo.toml")
        # Package table принадлежит standalone upstream crate.
        package = patch_manifest.get("package", {})
        # Проверяем имя и версию без наследования workspace metadata.
        if package.get("name") != patch["package"] or package.get("version") != patch["version"]:
            errors.append(f"{patch_path}: package name/version не совпадают с inventory")
        # Точный root replace остаётся неизменным и указывает на тот же путь.
        replace_key = f'{patch["package"]}:{patch["version"]}'
        # Сравнение path не позволяет inventory описывать неактивный fork.
        if replacements.get(replace_key, {}).get("path") != patch_path:
            errors.append(f"{patch_path}: root [replace] {replace_key} отсутствует или указывает не сюда")
        # Registry checksum служит точной upstream revision опубликованного архива.
        if not patch["upstream_revision"].startswith("crates.io:sha256:"):
            errors.append(f"{patch_path}: upstream_revision должна быть crates.io sha256 identity")
        # Direct locked command обязана быть явно зафиксирована в test inventory.
        expected_command = f"cargo test --manifest-path {patch_path}/Cargo.toml --locked"
        # Точное совпадение предотвращает незаметный запуск через root workspace.
        if expected_command not in patch["focused_automated_tests"]:
            errors.append(f"{patch_path}: отсутствует direct locked test command")
    # Ровно семь записей защищают от удаления replace без обновления policy.
    if len(inventory.get("patch", [])) != 7:
        errors.append("docs/dependency-patches.toml: ожидаются ровно семь patch записей")
    # Возвращаем полный набор нарушений вызывающему runner-у.
    return errors

# CLI boundary печатает diagnostics и возвращает стабильный status.
def main() -> int:
    # Policy errors вычисляются один раз.
    errors = validate()
    # Каждая ошибка получает одинаковый префикс для CI поиска.
    for error in errors:
        # stderr отделяет policy failures от success output.
        print(f"Ошибка dependency patch inventory: {error}", file=sys.stderr)
    # Ненулевой status блокирует CI только при найденных нарушениях.
    if errors:
        return 1
    # Короткое подтверждение показывает число проверенных forks.
    print("Dependency patch inventory: проверены 7 standalone patch crates")
    # Ноль означает полное соответствие inventory и manifests.
    return 0

# Модуль остаётся импортируемым для будущих focused unit tests.
if __name__ == "__main__":
    # SystemExit передаёт стабильный status shell runner-у.
    raise SystemExit(main())

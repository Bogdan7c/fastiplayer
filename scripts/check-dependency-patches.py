#!/usr/bin/env python3
"""Проверяет обслуживаемость десяти standalone dependency patches и патчей исходников
закреплённого FFmpeg (`[[source_patch]]`, crate `ffmpeg-vendor`)."""

# hashlib считает SHA-256 файла патча для сверки с inventory.
import hashlib
# pathlib даёт типизированные пути без зависимости от текущего каталога.
from pathlib import Path
# re вытаскивает закреплённую версию FFmpeg из `sources.rs` без сборки Rust.
import re
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

# Обязательные поля патча исходников: без них запись не объясняет миграцию на новый FFmpeg.
SOURCE_PATCH_REQUIRED_FIELDS = (
    "name", "file", "target_library", "target_version", "sha256", "reason",
    "owned_diff_areas", "focused_automated_tests", "manual_media_matrix",
    "removal_gate", "migration_checklist",
)
# Каталог патчей исходников внутри crate-а, который их накладывает.
SOURCE_PATCHES_DIRECTORY = REPO_ROOT / "crates/ffmpeg-vendor/patches"
# Политика патчей (список и порядок наложения) живёт в этом Rust-модуле.
SOURCE_PATCH_POLICY_PATH = REPO_ROOT / "crates/ffmpeg-vendor/src/source_patches.rs"
# Закреплённые версии исходников живут здесь.
PINNED_SOURCES_PATH = REPO_ROOT / "crates/ffmpeg-vendor/src/sources.rs"

# Функция читает TOML и сохраняет понятный контекст ошибки файла.
def read_toml(path: Path) -> dict:
    # Бинарный режим является контрактом tomllib.load.
    with path.open("rb") as toml_file:
        # Парсер возвращает обычную вложенную структуру Python.
        return tomllib.load(toml_file)

# Закреплённая версия библиотеки по `library_name` из `sources.rs` (None — не найдена).
def pinned_version(library_name: str) -> str | None:
    # Текст модуля читается целиком: он маленький и меняется редко.
    sources_text = PINNED_SOURCES_PATH.read_text(encoding="utf-8")
    # Ищем блок PinnedSource с нужным именем и его поле version.
    match = re.search(
        r'library_name:\s*"' + re.escape(library_name) + r'",\s*slug:\s*"[^"]*",\s*version:\s*"([^"]+)"',
        sources_text,
    )
    # Отсутствие совпадения — отдельная ошибка вызывающего кода.
    return match.group(1) if match else None

# SHA-256 файла в нижнем регистре.
def file_sha256(path: Path) -> str:
    # Патч маленький, читаем целиком.
    return hashlib.sha256(path.read_bytes()).hexdigest()

# Проверка патчей исходников: файл, сумма, версия цели, регистрация и отсутствие сирот.
def validate_source_patches(inventory: dict) -> list[str]:
    # Отдельный список: ошибки патчей исходников не смешиваются с crate-патчами.
    errors: list[str] = []
    # Записи патчей исходников (их может и не быть).
    source_patches = inventory.get("source_patch", [])
    # Текст политики Rust нужен для проверки регистрации каждого файла.
    policy_text = SOURCE_PATCH_POLICY_PATH.read_text(encoding="utf-8")
    # Файлы, описанные в inventory, — для поиска сирот на диске.
    inventoried_files: set[Path] = set()
    # Каждая запись проверяется независимо ради полной диагностики.
    for source_patch in source_patches:
        # Имя используется во всех сообщениях.
        name = source_patch.get("name", "<missing name>")
        # Все обязательные поля должны быть непустыми.
        for field in SOURCE_PATCH_REQUIRED_FIELDS:
            # False ловит и пустую строку, и пустой список.
            if not source_patch.get(field):
                errors.append(f"source patch {name}: missing nonempty field {field}")
        # Без пути к файлу дальнейшие проверки невозможны.
        if not source_patch.get("file"):
            continue
        # Путь в inventory — относительно корня репозитория.
        patch_file = REPO_ROOT / source_patch["file"]
        # Запоминаем для поиска сирот.
        inventoried_files.add(patch_file.resolve())
        # Файл обязан существовать.
        if not patch_file.is_file():
            errors.append(f"source patch {name}: file {source_patch['file']} does not exist")
            continue
        # Сумма в inventory — подпись проверенного содержимого: правка патча требует её обновить.
        if file_sha256(patch_file) != source_patch.get("sha256", "").lower():
            errors.append(
                f"source patch {name}: sha256 does not match {source_patch['file']} "
                f"(actual {file_sha256(patch_file)})"
            )
        # Цель патча должна быть ровно закреплённой версией библиотеки.
        library = source_patch.get("target_library", "")
        # Версия из sources.rs — источник истины для сборки.
        pinned = pinned_version(library)
        # Подняли FFmpeg, а патч не перенесли — это и есть ошибка миграции.
        if pinned != source_patch.get("target_version"):
            errors.append(
                f"source patch {name}: targets {library} {source_patch.get('target_version')}, "
                f"but sources.rs pins {pinned}; follow the migration checklist"
            )
        # Файл лежит в каталоге своей версии (`patches/ffmpeg-<версия>/`).
        expected_directory = f"{library.lower()}-{source_patch.get('target_version')}"
        # Сравниваем имя родительского каталога.
        if patch_file.parent.name != expected_directory:
            errors.append(f"source patch {name}: file must live in patches/{expected_directory}/")
        # Build script накладывает только патчи из политики Rust.
        if patch_file.name not in policy_text:
            errors.append(f"source patch {name}: {patch_file.name} is not registered in source_patches.rs")
    # Файл патча на диске без записи inventory — непроверенная правка исходников.
    if SOURCE_PATCHES_DIRECTORY.is_dir():
        # Рекурсивно: каталоги версий внутри patches/.
        for patch_file in sorted(SOURCE_PATCHES_DIRECTORY.rglob("*.patch")):
            # Сравниваем нормализованные пути.
            if patch_file.resolve() not in inventoried_files:
                errors.append(
                    f"{patch_file.relative_to(REPO_ROOT)}: source patch file has no [[source_patch]] record"
                )
    # Полный список нарушений.
    return errors

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
        errors.append("docs/dependency-patches.toml: expected schema_version = 1")
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
                errors.append(f"{patch_path}: missing nonempty field {field}")
        # Неполную запись дальше нельзя безопасно сопоставить с manifest.
        if not all(patch.get(field) for field in ("path", "package", "version")):
            continue
        # Standalone crate должен быть явно исключён из normal workspace membership.
        if patch_path not in excluded_paths:
            errors.append(f"{patch_path}: path is missing from workspace.exclude")
        # Принятое решение владельца прямо запрещает добавлять fork в members.
        if patch_path in workspace_members:
            errors.append(f"{patch_path}: standalone patch must not be added to workspace.members")
        # Повторный path является ошибкой inventory независимо от общего числа записей.
        if patch_path in inventoried_paths:
            errors.append(f"{patch_path}: duplicate path in inventory")
        # Первый occurrence запоминается для проверки следующих записей.
        inventoried_paths.add(patch_path)
        # На диске обязаны существовать самостоятельные manifest и lock-файл.
        patch_directory = REPO_ROOT / patch_path
        # Проверяем оба файла отдельно ради actionable diagnostics.
        for required_name in ("Cargo.toml", "Cargo.lock"):
            # Missing lock делает cargo test --locked невыполнимым.
            if not (patch_directory / required_name).is_file():
                errors.append(f"{patch_path}: missing {required_name}")
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
            errors.append(f"{patch_path}: package name/version does not match the inventory")
        # Точный root replace остаётся неизменным и указывает на тот же путь.
        replace_key = f'{patch["package"]}:{patch["version"]}'
        # Сравнение path не позволяет inventory описывать неактивный fork.
        if replacements.get(replace_key, {}).get("path") != patch_path:
            errors.append(f"{patch_path}: root [replace] {replace_key} is missing or points elsewhere")
        # Registry checksum служит точной upstream revision опубликованного архива.
        if not patch["upstream_revision"].startswith("crates.io:sha256:"):
            errors.append(f"{patch_path}: upstream_revision must be a crates.io sha256 identity")
        # Direct locked command обязана быть явно зафиксирована в test inventory.
        expected_command = f"cargo test --manifest-path {patch_path}/Cargo.toml --locked"
        # Точное совпадение предотвращает незаметный запуск через root workspace.
        if expected_command not in patch["focused_automated_tests"]:
            errors.append(f"{patch_path}: missing direct locked test command")
    # Ровно десять записей защищают от удаления replace без обновления policy.
    if len(inventory.get("patch", [])) != 10:
        errors.append("docs/dependency-patches.toml: exactly ten patch records are expected")
    # Патчи исходников проверяются своими правилами (они не crate-ы и не [replace]).
    errors.extend(validate_source_patches(inventory))
    # Возвращаем полный набор нарушений вызывающему runner-у.
    return errors

# CLI boundary печатает diagnostics и возвращает стабильный status.
def main() -> int:
    # Policy errors вычисляются один раз.
    errors = validate()
    # Каждая ошибка получает одинаковый префикс для CI поиска.
    for error in errors:
        # stderr отделяет policy failures от success output.
        print(f"Dependency patch inventory error: {error}", file=sys.stderr)
    # Ненулевой status блокирует CI только при найденных нарушениях.
    if errors:
        return 1
    # Короткое подтверждение показывает число проверенных forks.
    print(
        "Dependency patch inventory: 10 standalone patch crates and "
        f"{len(read_toml(INVENTORY_PATH).get('source_patch', []))} source patch(es) checked"
    )
    # Ноль означает полное соответствие inventory и manifests.
    return 0

# Модуль остаётся импортируемым для будущих focused unit tests.
if __name__ == "__main__":
    # SystemExit передаёт стабильный status shell runner-у.
    raise SystemExit(main())

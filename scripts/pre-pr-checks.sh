#!/usr/bin/env bash
# Локальный pre-PR путь для базовой проверки workspace.

# Строгий режим останавливает цепочку на первой ошибке и не даёт терять сбои pipeline.
set -Eeuo pipefail

# Явный успешный код нужен для симметрии с остальными shell-скриптами проекта.
readonly SUCCESS_EXIT_CODE=0

# Каталог скрипта нужен, чтобы запуск из любого cwd всё равно работал из корня repo.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

# Корень репозитория вычисляется от scripts/, а не от текущего каталога shell.
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"

# readonly фиксирует вычисленные пути до запуска проверок.
readonly SCRIPT_DIRECTORY="${script_directory}"
readonly REPO_ROOT="${repo_root}"

# Функция печатает ошибку в stderr с единым префиксом.
print_error() {
    # Сообщение передается первым аргументом, чтобы caller называл конкретную причину.
    local error_message="$1"

    # stderr отделяет диагностику от обычного вывода проверок.
    printf 'Ошибка: %s\n' "${error_message}" >&2
}

# Функция проверяет наличие внешней команды до запуска длинной цепочки.
require_command() {
    # Имя команды передается первым аргументом для переиспользования проверки.
    local required_command="$1"

    # command -v проверяет PATH без запуска самой команды.
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        # Ошибка содержит точное имя отсутствующего инструмента.
        print_error "команда '${required_command}' не найдена в PATH"

        # Продолжать нельзя, потому что один из обязательных шагов точно упадёт.
        exit 1
    fi
}

# Функция печатает название шага и запускает переданную команду.
run_step() {
    # Человекочитаемое имя шага отделено от команды, чтобы вывод оставался понятным.
    local step_name="$1"

    # shift оставляет в "$@" только команду и её аргументы.
    shift

    # Пустая строка визуально разделяет длинные Cargo-выводы.
    printf '\n==> %s\n' "${step_name}" >&2

    # Команда запускается как есть; set -e остановит pre-PR путь при ненулевом коде.
    "$@"
}

# Функция проверяет, что Cargo видит workspace metadata в ожидаемом формате.
run_cargo_metadata_sanity() {
    # JSON большой и здесь нужен только exit code, поэтому stdout подавляется.
    cargo metadata --no-deps --format-version 1 >/dev/null
}

# Функция запускает архитектурные dependency guardrails.
run_refactor_guardrails() {
    # Policy живёт в Python-скрипте, чтобы shell-wrapper не дублировал правила.
    "${SCRIPT_DIRECTORY}/check-refactor-guardrails.py"
}

# Функция проверяет форматирование всего workspace.
run_format_check() {
    # --check гарантирует, что pre-PR путь не меняет файлы сам.
    cargo fmt --all --check
}

# Функция выполняет быстрый compile/type check всего workspace.
run_workspace_check() {
    # --workspace нужен, чтобы локальный PR не проверял только default package.
    cargo check --workspace
}

# Функция запускает Clippy по workspace и test/example/bin targets.
run_workspace_clippy() {
    # --all-targets ловит предупреждения в tests/examples, которые cargo check может не покрыть.
    cargo clippy --workspace --all-targets
}

# Главная функция фиксирует порядок pre-PR шагов в одном месте.
main() {
    # Переходим в корень repo, чтобы Cargo и guardrail policy видели один workspace.
    cd "${REPO_ROOT}"

    # Cargo нужен для всех Rust-шагов и metadata.
    require_command "cargo"

    # Python нужен для check-refactor-guardrails.py через env shebang.
    require_command "python3"

    # Проверяем metadata отдельно, чтобы pre-PR путь явно закреплял Cargo graph sanity.
    run_step "cargo metadata --no-deps --format-version 1" run_cargo_metadata_sanity

    # Проверяем архитектурные границы до долгих compile/clippy шагов.
    run_step "scripts/check-refactor-guardrails.py" run_refactor_guardrails

    # Проверяем rustfmt без изменения файлов.
    run_step "cargo fmt --all --check" run_format_check

    # Проверяем компиляцию всего workspace.
    run_step "cargo check --workspace" run_workspace_check

    # Проверяем Clippy для всех targets workspace.
    run_step "cargo clippy --workspace --all-targets" run_workspace_clippy
}

# Запуск main сохраняет функции пригодными для будущего точечного тестирования.
main "$@"

# Явное успешное завершение делает контракт скрипта очевидным.
exit "${SUCCESS_EXIT_CODE}"

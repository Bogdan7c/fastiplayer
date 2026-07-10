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
    cargo metadata --locked --no-deps --format-version 1 >/dev/null
}

# Функция проверяет единый MSRV, pinned development toolchain и manifest inheritance.
run_toolchain_policy_guard() {
    # Python-guard сам читает полный locked graph через cargo metadata.
    python3 "${SCRIPT_DIRECTORY}/check-toolchain-policy.py"
}

# Функция запускает unit-тесты Python guardrails до проверки реального workspace.
run_guardrail_unit_tests() {
    # Discover автоматически включает каждый versioned test_*.py из единого каталога.
    python3 -m unittest discover -s "${SCRIPT_DIRECTORY}/tests" -p 'test_*.py'
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
    cargo check --workspace --locked
}

# Функция запускает Clippy по workspace и test/example/bin targets.
run_workspace_clippy() {
    # Полная feature/target matrix не позволяет warning-у спрятаться в optional коде или тесте.
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
}

# Функция проверяет rustdoc всего workspace в строгом режиме без документации dependencies.
run_workspace_rustdoc() {
    # RUSTDOCFLAGS превращает broken links и invalid markup в блокирующие ошибки.
    RUSTDOCFLAGS="-Dwarnings" cargo doc --workspace --all-features --no-deps --locked
}

# Функция запускает герметичные workspace tests, подготовленные Сессией 02.
run_workspace_tests() {
    # --no-fail-fast показывает все независимые падения одного полного local gate run-а.
    cargo test --workspace --all-features --locked --no-fail-fast
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
    run_step "cargo metadata --locked --no-deps --format-version 1" run_cargo_metadata_sanity

    # Проверяем policy до compile шагов, чтобы manifest/toolchain drift останавливался быстро.
    run_step "scripts/check-toolchain-policy.py" run_toolchain_policy_guard

    # Unit-тесты защищают сами policy scripts от регрессии до их применения к репозиторию.
    run_step "Python guardrail unit tests" run_guardrail_unit_tests

    # Проверяем архитектурные границы до долгих compile/clippy шагов.
    run_step "scripts/check-refactor-guardrails.py" run_refactor_guardrails

    # Проверяем rustfmt без изменения файлов.
    run_step "cargo fmt --all --check" run_format_check

    # Проверяем компиляцию всего workspace.
    run_step "cargo check --workspace --locked" run_workspace_check

    # Проверяем Clippy для всех targets workspace.
    run_step "cargo clippy strict" run_workspace_clippy

    # Документация является частью production API и не должна накапливать warnings.
    run_step "cargo doc strict" run_workspace_rustdoc

    # Default workspace tests герметичны после Сессии 02 и входят в обязательный gate.
    run_step "cargo test --workspace --all-features --locked --no-fail-fast" run_workspace_tests
}

# Запуск main сохраняет функции пригодными для будущего точечного тестирования.
main "$@"

# Явное успешное завершение делает контракт скрипта очевидным.
exit "${SUCCESS_EXIT_CODE}"

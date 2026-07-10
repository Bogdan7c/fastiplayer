#!/usr/bin/env bash
# Единый repo runner для локальных и GitHub Actions blocking-проверок.

# Строгий режим не позволяет потерять ошибку внутри функции или pipeline.
set -Eeuo pipefail

# Каталог скрипта вычисляется независимо от текущего рабочего каталога.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

# Корень репозитория находится на один уровень выше каталога scripts/.
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"

# readonly защищает вычисленные пути от случайного изменения.
readonly SCRIPT_DIRECTORY="${script_directory}"
readonly REPO_ROOT="${repo_root}"

# Функция печатает поддерживаемые стабильные имена проверок.
print_help() {
    # Текст справки одновременно служит кратким CLI-контрактом runner-а.
    cat <<'EOF'
Usage: scripts/ci-checks.sh CHECK

Checks:
  format-guardrails        Locked metadata, policy tests, guardrails and rustfmt.
  clippy                   Strict workspace/all-targets/all-features Clippy.
  docs                     Strict workspace/all-features rustdoc.
  tests                    Workspace/all-features tests without fail-fast.
  app-no-default-features  Compile app-egui without its default FFmpeg feature.
  msrv                     Compile the workspace with the supported Rust 1.92.0.
  all                      Run every blocking check in the order used locally.
EOF
}

# Функция проверяет наличие внешнего инструмента до длинного прогона.
require_command() {
    # Имя инструмента передаётся первым аргументом.
    local required_command="$1"

    # command -v проверяет PATH, не запуская инструмент.
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        # Диагностика явно называет отсутствующую системную зависимость runner-а.
        printf 'Ошибка: команда `%s` не найдена в PATH\n' "${required_command}" >&2
        # Без обязательного инструмента заявленная проверка недостоверна.
        exit 1
    fi
}

# Функция печатает название шага и запускает команду без строкового eval.
run_step() {
    # Человекочитаемое имя шага передаётся отдельно от команды.
    local step_name="$1"
    # После shift в "$@" остаётся безопасный argv запускаемой команды.
    shift
    # Заголовок делает локальный и CI log одинаково читаемым.
    printf '\n==> %s\n' "${step_name}" >&2
    # set -e остановит runner при первом ненулевом exit code.
    "$@"
}

# Функция объединяет дешёвые policy/format checks в один стабильный gate.
run_format_guardrails() {
    # Locked metadata доказывает согласованность manifest-ов и Cargo.lock.
    run_step "cargo metadata" run_cargo_metadata
    # Policy guard сверяет primary toolchain, MSRV и inheritance manifests.
    run_step "toolchain policy" python3 "${SCRIPT_DIRECTORY}/check-toolchain-policy.py"
    # Unit-тесты не позволяют самим policy scripts незаметно сломаться.
    run_step "guardrail unit tests" python3 -m unittest discover -s "${SCRIPT_DIRECTORY}/tests" -p 'test_*.py'
    # Архитектурные guardrails проверяются до дорогой компиляции.
    run_step "refactor guardrails" "${SCRIPT_DIRECTORY}/check-refactor-guardrails.py"
    # rustfmt работает в read-only check mode для всего workspace.
    run_step "rustfmt" cargo fmt --all --check
}

# Функция сохраняет только exit status большого Cargo metadata JSON.
run_cargo_metadata() {
    # Полный JSON не несёт пользы в CI log, поэтому stdout подавляется.
    cargo metadata --locked --no-deps --format-version 1 >/dev/null
}

# Функция запускает Clippy для всех workspace targets и features.
run_clippy() {
    # -D warnings превращает каждое предупреждение в блокирующую ошибку.
    run_step "strict Clippy" cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
}

# Функция проверяет документацию публичных и внутренних API.
run_docs() {
    # RUSTDOCFLAGS запрещает warnings, а --no-deps не документирует registry crates.
    run_step "strict rustdoc" env RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --all-features --no-deps --locked
}

# Функция запускает герметичную all-features test matrix.
run_tests() {
    # --no-fail-fast сохраняет diagnostics всех независимых failing test binaries.
    run_step "workspace tests" cargo test --workspace --all-features --locked --no-fail-fast
}

# Функция закрепляет поддерживаемую сборку app-egui без FFmpeg default feature.
run_app_no_default_features() {
    # Именованный package не даёт Cargo случайно проверить другой workspace target.
    run_step "app-egui without default features" cargo check -p app-egui --no-default-features --locked
}

# Функция выполняет реальный compile check на принятом MSRV.
run_msrv() {
    # Явный +toolchain не зависит от primary pin в rust-toolchain.toml.
    run_step "MSRV workspace check" cargo +1.92.0 check --workspace --locked
}

# Главная функция валидирует CLI и маршрутизирует только именованные режимы.
main() {
    # Все относительные пути Cargo и Python должны разрешаться от repo root.
    cd "${REPO_ROOT}"
    # Cargo нужен каждому режиму этого runner-а.
    require_command cargo
    # Ровно один аргумент предотвращает случайный частичный CI invocation.
    if (($# != 1)); then
        # Справка объясняет корректный контракт вместо неясной shell-ошибки.
        print_help >&2
        # Некорректный вызов не может считаться успешной проверкой.
        exit 2
    fi
    # Именованный case сохраняет список CI boundaries явным.
    case "$1" in
        format-guardrails)
            # Python требуется только policy/guardrail режиму.
            require_command python3
            # Запускаем объединённый быстрый gate.
            run_format_guardrails
            ;;
        clippy)
            # Запускаем строгий lint gate.
            run_clippy
            ;;
        docs)
            # Запускаем строгий documentation gate.
            run_docs
            ;;
        tests)
            # Запускаем all-features test gate.
            run_tests
            ;;
        app-no-default-features)
            # Запускаем отдельную feature-boundary сборку приложения.
            run_app_no_default_features
            ;;
        msrv)
            # Запускаем compile gate на поддерживаемом минимальном Rust.
            run_msrv
            ;;
        all)
            # Полный локальный путь требует Python для первой группы checks.
            require_command python3
            # Порядок начинает с самых дешёвых и понятных failures.
            run_format_guardrails
            # Основной workspace compile покрывается Clippy all-features.
            run_clippy
            # Документация проверяется независимо от test compilation.
            run_docs
            # Тесты запускаются после статических проверок.
            run_tests
            # Отдельно закрепляем feature-off app boundary.
            run_app_no_default_features
            # MSRV идёт последним как отдельная compatibility гарантия.
            run_msrv
            ;;
        --help|-h)
            # Справка является успешным read-only вызовом.
            print_help
            ;;
        *)
            # Неизвестное имя не должно превращаться в пропущенную проверку.
            printf 'Ошибка: неизвестная проверка `%s`\n' "$1" >&2
            # Справка сразу показывает допустимые значения.
            print_help >&2
            # Exit code 2 обозначает ошибку CLI-контракта.
            exit 2
            ;;
    esac
}

# Единственная точка входа передаёт исходный argv без преобразований.
main "$@"

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
# Exact pins делают локальный и CI policy engine воспроизводимыми.
readonly CARGO_DENY_VERSION="0.20.2"
readonly CARGO_MACHETE_VERSION="0.9.2"

# Функция печатает поддерживаемые стабильные имена проверок.
print_help() {
    # Текст справки одновременно служит кратким CLI-контрактом runner-а.
    cat <<'EOF'
Usage: scripts/ci-checks.sh CHECK

Checks:
  format-guardrails        Locked metadata, policy tests, guardrails and rustfmt.
  dependency-patches       Проверить inventory и integration suite local patches.
  dependencies             Advisories, licenses, sources and unused direct dependencies.
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

# Функция не позволяет незаметно запустить policy другим release инструмента.
require_exact_version() {
    # Человекочитаемое имя инструмента используется в диагностике.
    local tool_name="$1"
    # Ожидаемая полная строка версии исключает неоднозначный substring match.
    local expected_version_output="$2"
    # Остальные аргументы образуют безопасную команду получения версии.
    shift 2
    # Реальный stdout сохраняется, чтобы показать установленную версию при ошибке.
    local actual_version_output
    actual_version_output="$("$@")"
    if [[ "${actual_version_output}" != "${expected_version_output}" ]]; then
        printf 'Ошибка: %s должен иметь версию `%s`, установлена `%s`\n' \
            "${tool_name}" "${expected_version_output}" "${actual_version_output}" >&2
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
    # Inventory guard связывает четыре root replace с standalone manifests и lock-файлами.
    run_step "dependency patch inventory" python3 "${SCRIPT_DIRECTORY}/check-dependency-patches.py"
    # Unit-тесты не позволяют самим policy scripts незаметно сломаться.
    run_step "guardrail unit tests" python3 -m unittest discover -s "${SCRIPT_DIRECTORY}/tests" -p 'test_*.py'
    # Shell syntax gate проверяет runtime runners до их неграфических self-tests.
    run_step "runtime script syntax" bash -n \
        "${SCRIPT_DIRECTORY}/playback-smoke.sh" \
        "${SCRIPT_DIRECTORY}/runtime-acceptance.sh" \
        "${SCRIPT_DIRECTORY}/tests/playback-smoke-self-test.sh"
    # Parser/config generation проверяются production config loader-ом без запуска GUI.
    run_step "playback smoke script self-tests" \
        "${SCRIPT_DIRECTORY}/tests/playback-smoke-self-test.sh"
    # Архитектурные guardrails проверяются до дорогой компиляции.
    run_step "refactor guardrails" "${SCRIPT_DIRECTORY}/check-refactor-guardrails.py"
    # rustfmt работает в read-only check mode для всего workspace.
    run_step "rustfmt" cargo fmt --all --check
}

# Функция проверяет workspace integration contracts всех четырёх local patches.
run_dependency_patches() {
    # Inventory проверяется до compile, чтобы structural failure был понятнее Cargo errors.
    run_step "dependency patch inventory" python3 "${SCRIPT_DIRECTORY}/check-dependency-patches.py"
    # Три dependent crates покрывают VA-API, MP4 demux и AAC audio integration boundaries.
    run_step "dependency patch integration tests" cargo test -p video-vaapi -p symphonia-demux -p audio --locked
}

# Функция запускает единый dependency-health boundary.
run_dependencies() {
    # Оба инструмента должны быть установлены exact версиями из README/CI.
    require_exact_version "cargo-deny" "cargo-deny ${CARGO_DENY_VERSION}" cargo deny --version
    require_exact_version "cargo-machete" "${CARGO_MACHETE_VERSION}" cargo machete --version

    # Blocking advisory status сохраняется, чтобы warnings были видимы даже при failure.
    local blocking_advisory_status=0
    cargo deny check advisories || blocking_advisory_status=$?
    # cargo-deny 0.20 не умеет lint-level warn для unmaintained; второй прогон
    # публикует их diagnostics, но только первый status управляет security gate.
    if ! cargo deny --config deny.warnings.toml check advisories; then
        printf 'Примечание: non-blocking advisory report содержит findings; см. diagnostics выше.\n' >&2
    fi
    # Один policy tool владеет license/source и duplicate visibility.
    local dependency_policy_status=0
    run_step "licenses, sources and duplicate inventory" \
        cargo deny check licenses bans sources || dependency_policy_status=$?
    # Явный список workspace members исключает четыре upstream patch directories.
    local unused_dependencies_status=0
    run_step "unused direct dependencies" cargo machete --with-metadata \
        crates/animation-core crates/app-egui crates/audio-core crates/audio-signalsmith \
        crates/audio-timestretch crates/audio crates/capability-core crates/codec-core \
        crates/config crates/desktop-integration crates/frame-server-core crates/media-core \
        crates/media-prefetch crates/player-core crates/playlist-core crates/playlist-state \
        crates/render-core crates/render-wgpu-shell \
        crates/render-wgpu-video crates/rustiplayer-settings crates/service-direct-media \
        crates/service-youtube crates/settings-core crates/settings-derive crates/source-core \
        crates/symphonia-demux crates/video-backend-api crates/video-core crates/video-ffmpeg \
        crates/video-frame-contract crates/video-present-core crates/video-vaapi crates/vp9-parser \
        || unused_dependencies_status=$?

    # Все независимые diagnostics публикуются за один запуск; любой blocking status
    # возвращает общий failure только после завершения полного dependency audit.
    if ((blocking_advisory_status != 0 || dependency_policy_status != 0 || unused_dependencies_status != 0)); then
        return 1
    fi
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
        dependencies)
            # Оба Cargo plugins валидируются внутри режима.
            run_dependencies
            ;;
        dependency-patches)
            # Python проверяет machine-readable inventory перед focused Cargo suite.
            require_command python3
            # Запускаем отдельный integration boundary dependency patches.
            run_dependency_patches
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
            # Dependency policy выполняется до дорогой компиляции.
            run_dependencies
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

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
# Primary toolchain используется явно, чтобы локальный override не менял release gate.
readonly PRIMARY_RUST_TOOLCHAIN="1.96.0"
# MSRV проверяется отдельным compiler release, а не только полем rust-version.
readonly MSRV_RUST_TOOLCHAIN="1.92.0"
# Явный inventory ограничивает cargo-machete first-party workspace и исключает patch crates.
readonly -a WORKSPACE_CRATE_DIRECTORIES=(
    crates/animation-core
    crates/app-egui
    crates/ui-artwork-egui
    crates/audio-core
    crates/audio-signalsmith
    crates/audio-timestretch
    crates/capability-core
    crates/codec-core
    crates/config
    crates/desktop-integration
    crates/frame-server-core
    crates/source-core
    crates/media-prefetch
    crates/service-direct-media
    crates/settings-core
    crates/settings-derive
    crates/rustiplayer-settings
    crates/web-media-core
    crates/web-media-playback-plan
    crates/web-media-http
    crates/web-media-ftp
    crates/web-media-adaptive
    crates/web-media-dash
    crates/hds-manifest-core
    crates/web-media-hds
    crates/web-media-smooth
    crates/web-media-hls
    crates/web-media-transport-api
    crates/bounded-xml-reader
    crates/dash-mpd-core
    crates/smooth-streaming-manifest-core
    crates/smooth-streaming-fmp4
    crates/player-core
    crates/bounded-work-executor
    crates/atomic-file-store
    crates/natural-sort-key
    crates/playlist-core
    crates/playlist-io
    crates/hls-playlist-core
    crates/playlist-discovery
    crates/playlist-state
    crates/media-core
    crates/demux-api
    crates/flv-demux
    crates/mpeg-ts-demux
    crates/symphonia-demux
    crates/audio
    crates/vp9-parser
    crates/video-frame-contract
    crates/video-core
    crates/video-backend-api
    crates/video-present-core
    crates/video-ffmpeg
    crates/render-core
    crates/render-wgpu-video
    crates/render-wgpu-shell
    crates/video-vaapi
    crates/service-ytdlp
)
# Standalone patch manifests не входят в root workspace и требуют отдельных locked suites.
readonly -a DEPENDENCY_PATCH_MANIFESTS=(
    crates/cros-libva-patch/Cargo.toml
    crates/cros-codecs-patch/Cargo.toml
    crates/symphonia-format-caf-patch/Cargo.toml
    crates/symphonia-format-isomp4-patch/Cargo.toml
    crates/symphonia-codec-aac-patch/Cargo.toml
    crates/symphonia-format-mkv-patch/Cargo.toml
    crates/wayland-scanner-patch/Cargo.toml
)

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

# Функция проверяет, что rustup alias разрешился ровно в ожидаемый Rust release.
require_rust_release() {
    # Человекочитаемая роль делает ошибку primary/MSRV однозначной.
    local toolchain_role="$1"
    # Exact rustup toolchain передаётся отдельно от ожидаемого release.
    local rust_toolchain="$2"
    # Полная строка rustc сохраняется для понятной диагностики.
    local actual_version_output
    actual_version_output="$(rustc +"${rust_toolchain}" --version)"
    # Первый пробел отделяет имя binary от release.
    local release_and_build="${actual_version_output#rustc }"
    # Второй пробел отделяет semver release от commit metadata.
    local actual_release="${release_and_build%% *}"
    # Несовпадение запрещает запуск release gate другим compiler release.
    if [[ "${actual_release}" != "${rust_toolchain}" ]]; then
        printf 'Ошибка: %s должен использовать Rust %s, получено `%s`\n' \
            "${toolchain_role}" "${rust_toolchain}" "${actual_version_output}" >&2
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
    # Дешёвые checks тоже обязаны использовать pinned primary Rust.
    require_rust_release "primary gate" "${PRIMARY_RUST_TOOLCHAIN}"
    # Locked metadata доказывает согласованность manifest-ов и Cargo.lock.
    run_step "cargo metadata" run_cargo_metadata
    # Policy guard сверяет primary toolchain, MSRV и inheritance manifests.
    run_step "toolchain policy" python3 "${SCRIPT_DIRECTORY}/check-toolchain-policy.py"
    # Inventory guard связывает семь root replace с standalone manifests и lock-файлами.
    run_step "dependency patch inventory" python3 "${SCRIPT_DIRECTORY}/check-dependency-patches.py"
    # Unit-тесты не позволяют самим policy scripts незаметно сломаться.
    run_step "guardrail unit tests" python3 -m unittest discover -s "${SCRIPT_DIRECTORY}/tests" -p 'test_*.py'
    # Shell syntax gate проверяет runtime runners до их неграфических self-tests.
    run_step "runtime script syntax" bash -n \
        "${SCRIPT_DIRECTORY}/final-acceptance.sh" \
        "${SCRIPT_DIRECTORY}/media-regression.sh" \
        "${SCRIPT_DIRECTORY}/playback-smoke.sh" \
        "${SCRIPT_DIRECTORY}/progressive-web-smoke.sh" \
        "${SCRIPT_DIRECTORY}/runtime-acceptance.sh" \
        "${SCRIPT_DIRECTORY}/tests/playback-smoke-self-test.sh" \
        "${SCRIPT_DIRECTORY}/tests/progressive-web-smoke-self-test.sh"
    # Parser/config generation проверяются production config loader-ом без запуска GUI.
    run_step "playback smoke script self-tests" \
        "${SCRIPT_DIRECTORY}/tests/playback-smoke-self-test.sh"
    # Explicit-URL parser и report redaction проверяются без network, GUI или real secrets.
    run_step "progressive web smoke script self-tests" \
        "${SCRIPT_DIRECTORY}/tests/progressive-web-smoke-self-test.sh"
    # Архитектурные guardrails проверяются до дорогой компиляции.
    run_step "refactor guardrails" "${SCRIPT_DIRECTORY}/check-refactor-guardrails.py"
    # S42 focused gate отдельно ratchet-ит parser/HTTP/FFmpeg/module-size boundaries.
    run_step "S42 acceptance guardrails" "${SCRIPT_DIRECTORY}/check_s42_guardrails.py"
    # rustfmt работает в read-only check mode для всего workspace.
    run_step "rustfmt" cargo +"${PRIMARY_RUST_TOOLCHAIN}" fmt --all --check
}

# Функция проверяет workspace integration contracts всех семи local patches.
run_dependency_patches() {
    # Integration suite компилируется тем же exact primary Rust, что и workspace.
    require_rust_release "dependency patch integration" "${PRIMARY_RUST_TOOLCHAIN}"
    # Inventory проверяется до compile, чтобы structural failure был понятнее Cargo errors.
    run_step "dependency patch inventory" python3 "${SCRIPT_DIRECTORY}/check-dependency-patches.py"
    # Dependent crates покрывают VA-API, MP4/Matroska demux, AAC audio, Smooth fMP4 adapter и preparation boundary.
    run_step "dependency patch integration tests" cargo +"${PRIMARY_RUST_TOOLCHAIN}" test -p video-vaapi -p symphonia-demux -p audio -p smooth-streaming-fmp4 -p web-media-smooth --locked
}

# Функция запускает direct hermetic suite каждого standalone local patch-а.
run_dependency_patch_direct_tests() {
    # Direct suites компилируются exact primary Rust и не зависят от ambient override.
    require_rust_release "standalone dependency patch suites" "${PRIMARY_RUST_TOOLCHAIN}"
    # Общий status позволяет увидеть failures всех независимых patch owners.
    local direct_patch_status=0
    # Exact manifests являются bounded versioned inventory, а не filesystem glob.
    local patch_manifest
    # Каждый standalone lockfile проверяется собственной Cargo invocation.
    for patch_manifest in "${DEPENDENCY_PATCH_MANIFESTS[@]}"; do
        # Именованный шаг сохраняет exact failing owner в local acceptance log.
        run_step "standalone dependency patch: ${patch_manifest}" \
            cargo +"${PRIMARY_RUST_TOOLCHAIN}" test \
            --manifest-path "${patch_manifest}" \
            --locked \
            || direct_patch_status=$?
    done
    # Любой direct failure блокирует полный локальный release gate.
    if ((direct_patch_status != 0)); then
        return 1
    fi
}

# Функция запускает единый dependency-health boundary.
run_dependencies() {
    # Policy plugins получают тот же Cargo/rustup context, что и compile gates.
    require_rust_release "dependency policy" "${PRIMARY_RUST_TOOLCHAIN}"
    # Оба инструмента должны быть установлены exact версиями из README/CI.
    require_exact_version "cargo-deny" "cargo-deny ${CARGO_DENY_VERSION}" \
        cargo +"${PRIMARY_RUST_TOOLCHAIN}" deny --version
    require_exact_version "cargo-machete" "${CARGO_MACHETE_VERSION}" \
        cargo +"${PRIMARY_RUST_TOOLCHAIN}" machete --version

    # Blocking advisory status сохраняется, чтобы warnings были видимы даже при failure.
    local blocking_advisory_status=0
    cargo +"${PRIMARY_RUST_TOOLCHAIN}" deny --locked check advisories \
        || blocking_advisory_status=$?
    # cargo-deny 0.20 не умеет lint-level warn для unmaintained; второй прогон
    # публикует их diagnostics, но только первый status управляет security gate.
    if ! cargo +"${PRIMARY_RUST_TOOLCHAIN}" deny --locked \
        --config deny.warnings.toml check advisories; then
        printf 'Примечание: non-blocking advisory report содержит findings; см. diagnostics выше.\n' >&2
    fi
    # Один policy tool владеет license/source и duplicate visibility.
    local dependency_policy_status=0
    run_step "licenses, sources and duplicate inventory" \
        cargo +"${PRIMARY_RUST_TOOLCHAIN}" deny --locked check licenses bans sources \
        || dependency_policy_status=$?
    # Versioned inventory исключает семь upstream patch directories и проверяется unit-тестом.
    local unused_dependencies_status=0
    run_step "unused direct dependencies" cargo +"${PRIMARY_RUST_TOOLCHAIN}" machete --with-metadata \
        "${WORKSPACE_CRATE_DIRECTORIES[@]}" \
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
    cargo +"${PRIMARY_RUST_TOOLCHAIN}" metadata --locked --no-deps --format-version 1 >/dev/null
}

# Функция запускает Clippy для всех workspace targets и features.
run_clippy() {
    # Clippy запускается только из exact primary component/toolchain.
    require_rust_release "Clippy gate" "${PRIMARY_RUST_TOOLCHAIN}"
    # -D warnings превращает каждое предупреждение в блокирующую ошибку.
    run_step "strict Clippy" cargo +"${PRIMARY_RUST_TOOLCHAIN}" clippy --workspace --all-targets --all-features --locked -- -D warnings
}

# Функция проверяет документацию публичных и внутренних API.
run_docs() {
    # Rustdoc API и lint semantics фиксируются exact primary release.
    require_rust_release "rustdoc gate" "${PRIMARY_RUST_TOOLCHAIN}"
    # RUSTDOCFLAGS запрещает warnings, а --no-deps не документирует registry crates.
    run_step "strict rustdoc" env RUSTDOCFLAGS=-Dwarnings cargo +"${PRIMARY_RUST_TOOLCHAIN}" doc --workspace --all-features --no-deps --locked
}

# Функция запускает герметичную all-features test matrix.
run_tests() {
    # Hermetic suite не зависит от случайного directory override rustup.
    require_rust_release "workspace test gate" "${PRIMARY_RUST_TOOLCHAIN}"
    # --no-fail-fast сохраняет diagnostics всех независимых failing test binaries.
    run_step "workspace tests" cargo +"${PRIMARY_RUST_TOOLCHAIN}" test --workspace --all-features --locked --no-fail-fast
}

# Функция закрепляет поддерживаемую сборку app-egui без FFmpeg default feature.
run_app_no_default_features() {
    # Feature-off boundary компилируется тем же primary release.
    require_rust_release "app feature gate" "${PRIMARY_RUST_TOOLCHAIN}"
    # Именованный package не даёт Cargo случайно проверить другой workspace target.
    run_step "app-egui without default features" cargo +"${PRIMARY_RUST_TOOLCHAIN}" check -p app-egui --no-default-features --locked
}

# Функция выполняет реальный compile check на принятом MSRV.
run_msrv() {
    # Проверка release до compile отличает отсутствующий toolchain от ошибок кода.
    require_rust_release "MSRV gate" "${MSRV_RUST_TOOLCHAIN}"
    # Явный +toolchain не зависит от primary pin в rust-toolchain.toml.
    run_step "MSRV workspace check" cargo +"${MSRV_RUST_TOOLCHAIN}" check --workspace --locked
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
            # Standalone patch suites не входят в workspace и запускаются отдельно.
            run_dependency_patch_direct_tests
            # Workspace integration local patches также остаётся blocking.
            run_dependency_patches
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

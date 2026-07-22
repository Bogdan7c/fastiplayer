#!/usr/bin/env bash
# Локальный optional acceptance smoke для реального runtime playback.

# Строгий режим останавливает скрипт на первой необработанной ошибке.
set -Eeuo pipefail

# Код успешного завершения держим явным, как в остальных shell-скриптах проекта.
readonly SUCCESS_EXIT_CODE=0

# Default длительность одного playback-сценария: достаточно для startup/swap markers.
readonly DEFAULT_DURATION_SECONDS=20

# Лог-фильтр включает info markers и debug summary с `drops_decoder_starvation`.
readonly DEFAULT_SMOKE_RUST_LOG="info,player_core::worker::runtime_publish=debug"

# Positive playback scenarios не должны встречать эти известные fatal/regression markers.
readonly POSITIVE_FORBIDDEN_REGEX="InvalidData|Error parsing OBU data|No start code|resource table is full|Decoder thread disconnected|panicked at|thread .* panicked|panic in a function that cannot unwind|UnsupportedRenderFormat|missing render resources|PlayerEvent::FatalError"

# Hardware rejection может содержать UnsupportedVideoCodec, но не должен падать иначе.
readonly REJECTION_FORBIDDEN_REGEX="InvalidData|Error parsing OBU data|No start code|resource table is full|Decoder thread disconnected|panicked at|thread .* panicked|panic in a function that cannot unwind|UnsupportedRenderFormat|missing render resources"

# Каталог скрипта нужен, чтобы запуск из любого cwd шёл от корня repo.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"

# Корень repo вычисляется от `scripts/`.
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"

# readonly фиксирует вычисленные пути после bootstrap-а.
readonly SCRIPT_DIRECTORY="${script_directory}"
readonly REPO_ROOT="${repo_root}"

# Release binary, который строит `cargo build --release -p app-egui`.
readonly RUSTIPLAYER_BINARY="${REPO_ROOT}/target/release/rustiplayer"

# Пользовательский override оставлен отдельной переменной, чтобы обычный RUST_LOG не ломал checks.
readonly SMOKE_RUST_LOG="${RUSTIPLAYER_SMOKE_RUST_LOG:-${DEFAULT_SMOKE_RUST_LOG}}"

# Режим запуска задаётся явно, чтобы script не угадывал media matrix пользователя.
smoke_mode=""

# Явно выбранный VP9 Profile 0 local path для hardware и software stress scenarios.
vp9_profile0_path=""

# Явно выбранный AV1 local path для auto fallback и hardware rejection scenarios.
av1_path=""

# Явно выбранный H.264 ISO BMFF local path для software host-upload scenario.
h264_path=""

# Длительность одного playback-сценария в секундах.
duration_seconds="${DEFAULT_DURATION_SECONDS}"

# Dry-run печатает команды и не запускает проверки.
dry_run="false"

# keep_logs сохраняет runtime directory даже при успешном завершении.
keep_logs="false"

# Runtime directory создаётся лениво только для playback-сценариев.
runtime_directory=""

# Последний log path нужен вызывающим scenario-check функциям.
last_scenario_log=""

# Функция печатает ошибку в stderr с единым префиксом.
print_error() {
    # Сообщение передаётся первым аргументом, чтобы caller называл конкретную причину.
    local error_message="$1"

    # stderr отделяет диагностику smoke-runner-а от stdout вызываемых команд.
    printf 'Ошибка: %s\n' "${error_message}" >&2
}

# Функция печатает справку по локальному acceptance runner-у.
print_help() {
    # heredoc удобнее обычных printf для многострочной справки.
    cat <<'EOF'
Usage: scripts/playback-smoke.sh [OPTIONS]

Optional local runtime playback acceptance smoke. This is not a CI/pre-PR gate.

Options:
  --mode full|software-only|hardware-only|probe-only|legacy-migration
      full          Run FFmpeg probe tests, release build, and the full scenario matrix.
      software-only Run FFmpeg probe tests, release build, and FFmpeg software scenarios.
      hardware-only Run release VA-API playback/rejection scenarios without FFmpeg probes.
      probe-only    Run only the focused video-ffmpeg runtime probe tests.
      legacy-migration
                    Run the explicitly named legacy config migration smoke only.

  --duration SECONDS
      Per-scenario playback timeout. Default: 20.

  --vp9 FILE
      VP9 Profile 0 SDR file for full/software-only scenarios; a 4K60 stream is recommended.

  --av1 FILE
      AV1 SDR file for full mode; it must exercise software fallback and hardware rejection.

  --h264 FILE
      H.264 ISO BMFF file for full/software-only software host-upload scenario.

  --dry-run
      Print planned commands and scenario checks without executing them.

  --keep-logs
      Keep the temporary log/config directory after a successful run.

  --help
      Show this help.
EOF
}

# Функция печатает команду в shell-совместимом виде для dry-run.
print_command() {
    # Первый символ делает вывод похожим на обычный shell trace.
    printf '+' >&2

    # Каждый аргумент quote-ится отдельно, чтобы пробелы в путях были видны.
    local command_part
    for command_part in "$@"; do
        printf ' %q' "${command_part}" >&2
    done

    # Завершаем строку команды.
    printf '\n' >&2
}

# Функция печатает обязательный статус, когда runner не получил required selection.
print_not_run_missing_selection() {
    # Контракт отличает отсутствие выбора от failed playback assertion.
    printf 'NOT RUN: missing selection\n' >&2
}

# Функция проверяет наличие внешней команды перед runtime-прогоном.
require_command() {
    # Имя команды передаётся первым аргументом.
    local required_command="$1"

    # command -v проверяет PATH без запуска команды.
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        # Ошибка содержит точное имя отсутствующего инструмента.
        print_error "команда '${required_command}' не найдена в PATH"

        # Продолжать нельзя: acceptance не сможет выполнить заявленный шаг.
        exit 1
    fi
}

# Функция проверяет команды, нужные для Cargo probe/build шагов.
require_cargo_commands() {
    # Cargo нужен для probe tests и release build.
    require_command "cargo"
}

# Функция проверяет команды, нужные только для playback scenarios.
require_playback_commands() {
    # timeout ограничивает долгий GUI playback без интерактивного закрытия окна.
    require_command "timeout"

    # grep выполняет stable marker checks.
    require_command "grep"

    # mktemp создаёт изолированный каталог логов и config-ов.
    require_command "mktemp"

    # tail показывает контекст при падении сценария.
    require_command "tail"
}

# Функция запускает шаг или печатает его в dry-run.
run_step() {
    # Человекочитаемое имя шага передаётся первым аргументом.
    local step_name="$1"

    # shift оставляет в "$@" только команду и её аргументы.
    shift

    # Пустая строка визуально отделяет длинный вывод Cargo.
    printf '\n==> %s\n' "${step_name}" >&2

    # Dry-run не должен запускать ни Cargo, ни playback.
    if [[ "${dry_run}" == "true" ]]; then
        print_command "$@"
        return
    fi

    # Команда запускается как есть; set -e остановит скрипт при ненулевом exit code.
    "$@"
}

# Функция валидирует mode из CLI.
validate_mode() {
    # mode передаётся первым аргументом.
    local candidate_mode="$1"

    # Поддерживаем только явно описанные режимы.
    case "${candidate_mode}" in
        full | software-only | hardware-only | probe-only | legacy-migration)
            return
            ;;
        *)
            print_error "неподдерживаемый --mode '${candidate_mode}'"
            exit 1
            ;;
    esac
}

# Функция валидирует duration из CLI.
validate_duration_seconds() {
    # Значение duration передаётся первым аргументом.
    local candidate_duration="$1"

    # Ноль и отрицательные значения не имеют смысла для timeout-а.
    if [[ ! "${candidate_duration}" =~ ^[1-9][0-9]*$ ]]; then
        print_error "--duration должен быть положительным целым числом секунд"
        exit 1
    fi
}

# Функция разбирает CLI options без внешнего getopt.
parse_arguments() {
    # Цикл идёт по всем аргументам слева направо.
    while (($# > 0)); do
        # case держит поведение каждого option рядом с его именем.
        case "$1" in
            --help)
                print_help
                exit "${SUCCESS_EXIT_CODE}"
                ;;
            --dry-run)
                dry_run="true"
                shift
                ;;
            --keep-logs)
                keep_logs="true"
                shift
                ;;
            --mode)
                if (($# < 2)); then
                    print_error "--mode требует значение"
                    exit 1
                fi
                validate_mode "$2"
                smoke_mode="$2"
                shift 2
                ;;
            --duration)
                if (($# < 2)); then
                    print_error "--duration требует значение"
                    exit 1
                fi
                validate_duration_seconds "$2"
                duration_seconds="$2"
                shift 2
                ;;
            --vp9)
                if (($# < 2)); then
                    print_error "--vp9 требует путь к media file"
                    exit 1
                fi
                vp9_profile0_path="$2"
                shift 2
                ;;
            --av1)
                if (($# < 2)); then
                    print_error "--av1 требует путь к media file"
                    exit 1
                fi
                av1_path="$2"
                shift 2
                ;;
            --h264)
                if (($# < 2)); then
                    print_error "--h264 требует путь к media file"
                    exit 1
                fi
                h264_path="$2"
                shift 2
                ;;
            *)
                print_error "неизвестный аргумент '$1'"
                exit 1
                ;;
        esac
    done
}

# Функция создаёт runtime directory для logs/configs при первом playback-сценарии.
ensure_runtime_directory() {
    # Повторный вызов не создаёт новый каталог, чтобы все логи лежали рядом.
    if [[ -n "${runtime_directory}" ]]; then
        return
    fi

    # mktemp создаёт уникальный каталог в системном temp.
    runtime_directory="$(mktemp -d -t rustiplayer-playback-smoke.XXXXXX)"

    # Подкаталоги разделяют stderr logs и XDG_CONFIG_HOME дерева сценариев.
    mkdir -p -- "${runtime_directory}/logs" "${runtime_directory}/configs"
}

# Функция удаляет временные logs/configs только после успешного run без --keep-logs.
cleanup_runtime_directory() {
    # Сохраняем exit code, потому что cleanup не должен маскировать результат smoke.
    local exit_code=$?

    # Если playback-сценариев не было, удалять нечего.
    if [[ -z "${runtime_directory}" || ! -d "${runtime_directory}" ]]; then
        return
    fi

    # При ошибке логи всегда сохраняются для разбора причины.
    if [[ "${keep_logs}" == "true" || "${exit_code}" -ne 0 ]]; then
        printf 'Smoke logs/configs kept at: %s\n' "${runtime_directory}" >&2
        return
    fi

    # Удаляем только каталог, который этот smoke-runner сам создал через mktemp.
    rm -rf -- "${runtime_directory}"
}

# trap гарантирует cleanup и при обычном выходе, и при ранней ошибке.
trap cleanup_runtime_directory EXIT

# Функция печатает хвост лога и завершает smoke ошибкой.
fail_with_log_tail() {
    # Сообщение объясняет, какой именно acceptance check не прошёл.
    local failure_message="$1"

    # Путь к логу передаётся вторым аргументом.
    local log_path="$2"

    # Сначала печатаем короткую причину.
    print_error "${failure_message}"

    # Затем показываем последнюю часть stderr/stdout приложения.
    if [[ -f "${log_path}" ]]; then
        printf '\n--- tail: %s ---\n' "${log_path}" >&2
        tail -n 80 -- "${log_path}" >&2
        printf '%s\n' '--- end tail ---' >&2
    fi

    # Ненулевой exit code делает сценарий fail-fast.
    exit 1
}

# Функция требует presence marker в log-файле.
require_log_regex() {
    # Путь к логу передаётся первым аргументом.
    local log_path="$1"

    # Regex передаётся вторым аргументом.
    local expected_regex="$2"

    # Описание marker-а передаётся третьим аргументом.
    local marker_description="$3"

    # grep -E проверяет stable marker без привязки к цветам/target-ам tracing.
    if ! grep -Eq -- "${expected_regex}" "${log_path}"; then
        fail_with_log_tail "не найден marker: ${marker_description}" "${log_path}"
    fi
}

# Функция запрещает regression marker в log-файле.
reject_log_regex() {
    # Путь к логу передаётся первым аргументом.
    local log_path="$1"

    # Regex передаётся вторым аргументом.
    local forbidden_regex="$2"

    # Описание marker-а передаётся третьим аргументом.
    local marker_description="$3"

    # Наличие запрещённого marker-а означает regression.
    if grep -Eq -- "${forbidden_regex}" "${log_path}"; then
        fail_with_log_tail "найден запрещённый marker: ${marker_description}" "${log_path}"
    fi
}

# Функция проверяет, что explicit selected asset существует перед реальным playback.
require_asset_file() {
    # Выбранный пользователем путь к asset-у передаётся первым аргументом.
    local asset_path="$1"

    # Нечитаемый или несуществующий selected file остаётся failed local acceptance.
    if [[ ! -f "${asset_path}" ]]; then
        print_error "выбранный media file не найден: ${asset_path}"
        exit 1
    fi
}

# Функция нормализует existing selected path до абсолютного до смены cwd на repo root.
absolute_selected_path() {
    # Пользовательский path передаётся первым аргументом.
    local selected_path="$1"

    # dirname/basename сохраняют filename с пробелами без shell word splitting.
    local selected_directory
    selected_directory="$(cd -- "$(dirname -- "${selected_path}")" && pwd -P)"

    # Возвращаем canonical directory и исходное basename без угадывания media файла.
    printf '%s/%s\n' "${selected_directory}" "$(basename -- "${selected_path}")"
}

# Функция проверяет, что mode получил все required explicit paths до expensive Cargo шагов.
validate_media_selection() {
    # Полное отсутствие выбора означает, что пользователь не запросил scenario.
    if [[ -z "${smoke_mode}" && -z "${vp9_profile0_path}" && -z "${av1_path}" && -z "${h264_path}" ]]; then
        print_not_run_missing_selection
        exit "${SUCCESS_EXIT_CODE}"
    fi

    # Path без mode не даёт script-у права угадывать scenario matrix.
    if [[ -z "${smoke_mode}" ]]; then
        print_not_run_missing_selection
        exit 1
    fi

    # Config-only modes не используют реальные media files.
    if [[ "${smoke_mode}" == "probe-only" || "${smoke_mode}" == "legacy-migration" ]]; then
        return
    fi

    # Full matrix требует один path на каждое distinct media property.
    if [[ "${smoke_mode}" == "full" && ( -z "${vp9_profile0_path}" || -z "${av1_path}" || -z "${h264_path}" ) ]]; then
        print_not_run_missing_selection
        exit 1
    fi

    # Software-only использует H.264 и VP9, но не AV1 hardware rejection matrix.
    if [[ "${smoke_mode}" == "software-only" && ( -z "${vp9_profile0_path}" || -z "${h264_path}" ) ]]; then
        print_not_run_missing_selection
        exit 1
    fi

    # Hardware-only использует VP9 success и AV1 typed rejection scenarios.
    if [[ "${smoke_mode}" == "hardware-only" && ( -z "${vp9_profile0_path}" || -z "${av1_path}" ) ]]; then
        print_not_run_missing_selection
        exit 1
    fi

    # Проверяем selected paths здесь, чтобы invalid input не маскировался Cargo failure-ом.
    if [[ -n "${vp9_profile0_path}" ]]; then
        require_asset_file "${vp9_profile0_path}"
        vp9_profile0_path="$(absolute_selected_path "${vp9_profile0_path}")"
    fi
    if [[ -n "${av1_path}" ]]; then
        require_asset_file "${av1_path}"
        av1_path="$(absolute_selected_path "${av1_path}")"
    fi
    if [[ -n "${h264_path}" ]]; then
        require_asset_file "${h264_path}"
        h264_path="$(absolute_selected_path "${h264_path}")"
    fi
}

# Функция пишет полный current-schema config для одного playback-сценария.
write_scenario_config() {
    # XDG_CONFIG_HOME для сценария передаётся первым аргументом.
    local config_home="$1"

    # video.preferred_backend передаётся вторым аргументом.
    local backend_preference="$2"

    # Rustiplayer ищет config в `$XDG_CONFIG_HOME/rustiplayer/config.toml`.
    local config_file="${config_home}/rustiplayer/config.toml"

    # Создаём только isolated config tree текущего сценария.
    mkdir -p -- "$(dirname -- "${config_file}")"

    # Config-crate остаётся единственным владельцем полного набора schema v7 fields/defaults.
    cargo run --quiet --locked -p rustiplayer-config --example smoke_config -- \
        generate-current "${config_file}" "${backend_preference}"

    # Тем же production loader-ом доказываем strict current parse до запуска GUI.
    cargo run --quiet --locked -p rustiplayer-config --example smoke_config -- \
        parse-current "${config_file}"
}

# Функция запускает один playback-сценарий под timeout и сохраняет общий stdout/stderr log.
run_playback_scenario() {
    # Stable имя сценария используется в путях логов.
    local scenario_name="$1"

    # Public config preference для сценария.
    local backend_preference="$2"

    # Явно выбранный путь к media asset-у.
    local asset_path="$3"

    # Dry-run печатает команду и не требует существования binary/logs.
    if [[ "${dry_run}" == "true" ]]; then
        printf '\n==> playback scenario: %s\n' "${scenario_name}" >&2
        printf 'Would write full current config: schema v7, video.preferred_backend = "%s", player.start_paused = false, yt_dlp.hdr_selection = "sdr_only"\n' "${backend_preference}" >&2
        print_command env \
            "XDG_CONFIG_HOME=<tmp>/configs/${scenario_name}" \
            "RUST_LOG=${SMOKE_RUST_LOG}" \
            "NO_COLOR=1" \
            timeout \
            --kill-after=5s \
            "${duration_seconds}s" \
            "${RUSTIPLAYER_BINARY}" \
            "${asset_path}"
        last_scenario_log="<dry-run>/logs/${scenario_name}.log"
        return
    fi

    # Release binary должен быть результатом предыдущего cargo build шага.
    if [[ ! -x "${RUSTIPLAYER_BINARY}" ]]; then
        print_error "release binary не найден или не executable: ${RUSTIPLAYER_BINARY}"
        exit 1
    fi

    # Создаём общий runtime directory при первом playback-сценарии.
    ensure_runtime_directory

    # Config home отделён по сценарию, чтобы настройки не протекали между прогонами.
    local config_home="${runtime_directory}/configs/${scenario_name}"

    # Log path тоже отделён по сценарию.
    local log_path="${runtime_directory}/logs/${scenario_name}.log"

    # Пишем минимальный config с нужным backend preference.
    write_scenario_config "${config_home}" "${backend_preference}"

    # Сообщаем, какой сценарий стартует.
    printf '\n==> playback scenario: %s\n' "${scenario_name}" >&2
    printf '    selected path: %s\n' "${asset_path}" >&2
    printf '    backend preference: %s\n' "${backend_preference}" >&2
    printf '    log: %s\n' "${log_path}" >&2

    # Временно отключаем set -e, чтобы принять timeout status 124 как штатный outcome.
    set +e
    env \
        "XDG_CONFIG_HOME=${config_home}" \
        "RUST_LOG=${SMOKE_RUST_LOG}" \
        "NO_COLOR=1" \
        timeout \
        --kill-after=5s \
        "${duration_seconds}s" \
        "${RUSTIPLAYER_BINARY}" \
        "${asset_path}" \
        >"${log_path}" 2>&1
    local playback_status=$?
    set -e

    # Долгий playback штатно завершается timeout-ом; early clean exit тоже допустим.
    case "${playback_status}" in
        0 | 124 | 137)
            ;;
        *)
            fail_with_log_tail "playback scenario '${scenario_name}' завершился с кодом ${playback_status}" "${log_path}"
            ;;
    esac

    # Сохраняем log path для следующих marker checks.
    last_scenario_log="${log_path}"
}

# Функция проверяет positive playback scenario с ожидаемым concrete pipeline plan.
run_positive_playback_scenario() {
    # Stable имя сценария.
    local scenario_name="$1"

    # Public backend preference.
    local backend_preference="$2"

    # Явно выбранный media path.
    local selected_media_path="$3"

    # Ожидаемый `VideoPipelinePlan::diagnostic_label`.
    local expected_plan="$4"

    # Нужно ли требовать evidence backend reselection.
    local require_reselection="$5"

    # Нужно ли включить stress-only starvation check.
    local stress_check="$6"

    # Запускаем сценарий или печатаем dry-run команду.
    run_playback_scenario "${scenario_name}" "${backend_preference}" "${selected_media_path}"

    # Dry-run не создаёт log, поэтому marker checks только описываем.
    if [[ "${dry_run}" == "true" ]]; then
        printf 'Would require plan marker: %s\n' "${expected_plan}" >&2
        if [[ "${require_reselection}" == "true" ]]; then
            printf 'Would require backend reselection evidence.\n' >&2
        fi
        printf 'Would reject known positive-scenario regression markers.\n' >&2
        return
    fi

    # Проверяем selected pipeline marker.
    require_log_regex \
        "${last_scenario_log}" \
        "Selected video pipeline.*plan=\"?${expected_plan}\"?" \
        "Selected video pipeline plan=${expected_plan}"

    # Auto AV1 должен доказать, что был backend reselection, а не старт сразу в software.
    if [[ "${require_reselection}" == "true" ]]; then
        require_log_regex \
            "${last_scenario_log}" \
            "backend reselection" \
            "backend reselection evidence"
    fi

    # Positive scenarios не должны содержать известных fatal markers.
    reject_log_regex \
        "${last_scenario_log}" \
        "${POSITIVE_FORBIDDEN_REGEX}" \
        "known fatal playback regression"

    # Stress scenario дополнительно запрещает ненулевую decoder starvation summary.
    if [[ "${stress_check}" == "true" ]]; then
        reject_log_regex \
            "${last_scenario_log}" \
            "drops_decoder_starvation=[1-9][0-9]*" \
            "decoder starvation summary"
    fi
}

# Функция проверяет expected typed rejection для hardware-only AV1.
run_hardware_av1_rejection_scenario() {
    # Сценарий должен использовать public hardware preference.
    run_playback_scenario "hardware-av1-4k60-reject" "hardware" "${av1_path}"

    # Dry-run не создаёт log, поэтому marker checks только описываем.
    if [[ "${dry_run}" == "true" ]]; then
        printf 'Would require PlayerEvent::FatalError kind=UnsupportedVideoCodec.\n' >&2
        printf 'Would require hardware rejection reason to mention unsupported AV1 stream.\n' >&2
        printf 'Would reject old generic hardware-output reason.\n' >&2
        printf 'Would reject FFmpeg software selected pipeline fallback.\n' >&2
        return
    fi

    # Typed rejection должен быть виден на app-level stderr marker-е.
    require_log_regex \
        "${last_scenario_log}" \
        "PlayerEvent::FatalError.*kind=UnsupportedVideoCodec|kind=UnsupportedVideoCodec.*PlayerEvent::FatalError" \
        "PlayerEvent::FatalError kind=UnsupportedVideoCodec"

    # Причина должна быть stream-oriented, а не generic selector/resource detail.
    require_log_regex \
        "${last_scenario_log}" \
        "native hardware output не поддерживает.*AV1|AV1.*native hardware output не поддерживает" \
        "hardware rejection explains unsupported AV1 stream"

    # Старый generic selector reason скрывал настоящий codec-policy отказ.
    reject_log_regex \
        "${last_scenario_log}" \
        "нет playable native hardware output после renderer intersection" \
        "generic hardware output rejection reason"

    # Hardware preference не должен silently fallback-иться в FFmpeg software.
    reject_log_regex \
        "${last_scenario_log}" \
        "Selected video pipeline.*plan=\"?ffmpeg-host-upload-wgpu\"?" \
        "software fallback in hardware-only scenario"

    # Остальные known fatal markers всё равно запрещены.
    reject_log_regex \
        "${last_scenario_log}" \
        "${REJECTION_FORBIDDEN_REGEX}" \
        "non-typed hardware rejection regression"
}

# Функция запускает focused FFmpeg probe tests.
run_probe_steps() {
    # Unit/fake probe tests покрывают missing/too-old FFmpeg без runtime override.
    run_step \
        "cargo test -p video-ffmpeg --features ffmpeg probe::tests" \
        cargo test -p video-ffmpeg --features ffmpeg --locked probe::tests

    # Ignored real-runtime probe проверяет локально установленный FFmpeg runtime.
    run_step \
        "cargo test -p video-ffmpeg --features ffmpeg -- --ignored" \
        cargo test -p video-ffmpeg --features ffmpeg --locked -- --ignored

    # Явный PASS не позволяет перепутать реально выполненный runtime probe с ignored listing.
    printf 'PASS: FFmpeg runtime probe acceptance\n' >&2
}

# Функция запускает legacy migration отдельно от current playback smoke.
run_legacy_migration_smoke() {
    # Названные focused tests закрепляют v2/v3/v4 migration, не участвуя в generated config path.
    run_step \
        "legacy config migration smoke" \
        cargo test -p rustiplayer-config --locked legacy_
    # Отдельный marker делает legacy outcome заметным в acceptance log.
    printf 'PASS: explicitly selected legacy config migration smoke\n' >&2
}

# Функция собирает release app binary для playback acceptance.
run_release_build() {
    # Playback smoke всегда использует release binary, потому что perf/debug профиль не является acceptance.
    run_step \
        "cargo build --release -p app-egui" \
        cargo build --release -p app-egui --locked
}

# Функция запускает full hardware+software сценарии.
run_full_scenarios() {
    # Auto + VP9 Profile 0 должен выбрать VA-API DMA-BUF WGPU.
    run_positive_playback_scenario \
        "auto-vp9-profile0-4k60" \
        "auto" \
        "${vp9_profile0_path}" \
        "vaapi-dmabuf-wgpu" \
        "false" \
        "false"

    # Auto + AV1 должен сначала запросить reselection и затем выбрать FFmpeg host-upload.
    run_positive_playback_scenario \
        "auto-av1-4k60-fallback" \
        "auto" \
        "${av1_path}" \
        "ffmpeg-host-upload-wgpu" \
        "true" \
        "false"

    # Software + H.264 MP4 должен идти через FFmpeg host-upload без start-code ошибок.
    run_positive_playback_scenario \
        "software-h264-mp4-4k60" \
        "software" \
        "${h264_path}" \
        "ffmpeg-host-upload-wgpu" \
        "false" \
        "false"

    # Hardware + AV1 должен дать typed unsupported, а не fallback и не panic.
    run_hardware_av1_rejection_scenario

    # Software + VP9 Profile 0 stress должен идти через FFmpeg без resource/starvation regressions.
    run_positive_playback_scenario \
        "software-vp9-profile0-4k60-stress" \
        "software" \
        "${vp9_profile0_path}" \
        "ffmpeg-host-upload-wgpu" \
        "false" \
        "true"
}

# Функция запускает сценарии, которые не требуют hardware playback path.
run_software_only_scenarios() {
    # H.264 MP4 software path проверяет FFmpeg packetization/start-code regression surface.
    run_positive_playback_scenario \
        "software-h264-mp4-4k60" \
        "software" \
        "${h264_path}" \
        "ffmpeg-host-upload-wgpu" \
        "false" \
        "false"

    # VP9 software stress проверяет host-upload resource accounting и decoder starvation summary.
    run_positive_playback_scenario \
        "software-vp9-profile0-4k60-stress" \
        "software" \
        "${vp9_profile0_path}" \
        "ffmpeg-host-upload-wgpu" \
        "false" \
        "true"
}

# Функция запускает только сценарии, принадлежащие VA-API hardware acceptance.
run_hardware_only_scenarios() {
    # VP9 Profile 0 подтверждает основной VA-API DMA-BUF playback path.
    run_positive_playback_scenario \
        "auto-vp9-profile0-4k60" \
        "auto" \
        "${vp9_profile0_path}" \
        "vaapi-dmabuf-wgpu" \
        "false" \
        "false"

    # AV1 подтверждает typed hardware-only rejection без software fallback.
    run_hardware_av1_rejection_scenario
}

# Главная функция фиксирует acceptance workflow в одном месте.
main() {
    # Разбираем аргументы до любых проверок окружения.
    parse_arguments "$@"

    # Required local paths валидируются до probe/build, чтобы missing selection был однозначным.
    validate_media_selection

    # Все относительные пути сценариев считаются от корня repo.
    cd "${REPO_ROOT}"

    # Печатаем итоговую конфигурацию runner-а.
    printf 'playback smoke mode: %s\n' "${smoke_mode}" >&2
    printf 'per-scenario duration: %ss\n' "${duration_seconds}" >&2
    printf 'RUST_LOG: %s\n' "${SMOKE_RUST_LOG}" >&2

    # Dry-run должен показать команды даже если локальные tools не установлены.
    if [[ "${dry_run}" != "true" ]]; then
        require_cargo_commands
    fi

    # Config-only modes не требуют timeout/mktemp/playback tools.
    if [[ "${dry_run}" != "true" && "${smoke_mode}" != "probe-only" && "${smoke_mode}" != "legacy-migration" ]]; then
        require_playback_commands
    fi

    # Legacy migration является отдельным сценарием и не запускает FFmpeg или GUI.
    if [[ "${smoke_mode}" == "legacy-migration" ]]; then
        run_legacy_migration_smoke
        exit "${SUCCESS_EXIT_CODE}"
    fi

    # FFmpeg probe принадлежит software/full acceptance, но не VA-API-only сценарию.
    if [[ "${smoke_mode}" != "hardware-only" ]]; then
        run_probe_steps
    fi

    # probe-only намеренно не собирает app и не запускает playback.
    if [[ "${smoke_mode}" == "probe-only" ]]; then
        exit "${SUCCESS_EXIT_CODE}"
    fi

    # Playback scenarios используют release build.
    run_release_build

    # Выбираем scenario matrix по mode.
    case "${smoke_mode}" in
        full)
            run_full_scenarios
            ;;
        software-only)
            run_software_only_scenarios
            ;;
        hardware-only)
            run_hardware_only_scenarios
            ;;
    esac
}

# Запускаем main, чтобы функции оставались тестируемыми shellcheck-стилем в будущем.
main "$@"

# Явное успешное завершение делает контракт скрипта очевидным.
exit "${SUCCESS_EXIT_CODE}"

#!/usr/bin/env bash
# Opt-in manual runner progressive web playback; CI и обычные tests его не запускают.

# Строгий режим не позволяет потерять ошибку build, runtime или redaction шага.
set -Eeuo pipefail

# Успех CLI означает только завершённый запуск; UX verdict остаётся ручным.
readonly SUCCESS_EXIT_CODE=0
# Ошибка runner-а или runtime имеет обычный ненулевой status.
readonly FAILURE_EXIT_CODE=1
# Ошибка пользовательского CLI отделена от runtime failure.
readonly USAGE_EXIT_CODE=2
# Две минуты дают время проверить sidebar, pause и candidate switch вручную.
readonly DEFAULT_DURATION_SECONDS=120
# Default log level сохраняет lifecycle evidence без включения verbose extractor output.
readonly DEFAULT_PROGRESSIVE_WEB_RUST_LOG="info"

# Каталог скрипта вычисляется независимо от текущего рабочего каталога.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
# Корень repository находится на один уровень выше scripts/.
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"
# readonly защищает вычисленный repository root от случайной перезаписи.
readonly REPO_ROOT="${repo_root}"
# Default binary соответствует package app-egui и release build output.
readonly DEFAULT_RUSTIPLAYER_BINARY="${REPO_ROOT}/target/release/rustiplayer"
# Отдельный env override не позволяет обычному RUST_LOG незаметно изменить report surface.
readonly PROGRESSIVE_WEB_RUST_LOG="${RUSTIPLAYER_PROGRESSIVE_WEB_RUST_LOG:-${DEFAULT_PROGRESSIVE_WEB_RUST_LOG}}"

# URLs появляются только из повторяемого explicit --url пользователя.
declare -a explicit_urls=()
# Duration начинается с документированного безопасного default-а.
duration_seconds="${DEFAULT_DURATION_SECONDS}"
# Report path обязателен для реального запуска и не выбирается автоматически.
report_path=""
# Explicit binary полезен для self-test и уже собранного локального app binary.
selected_binary=""
# Dry-run проверяет parser и показывает redacted план без network/GUI/build side effects.
dry_run="false"
# Флаг отличает пустой invocation от частично заполненного invalid selection.
received_any_argument="false"
# Raw logs живут только в process-owned temporary directory.
runtime_directory=""

# Cleanup удаляет raw URL/log material даже после runtime failure.
cleanup_runtime_directory() {
    # Пустой path означает, что runner ещё не создавал temporary directory.
    if [[ -z "${runtime_directory}" ]]; then
        return
    fi
    # Удаляется только exact directory, созданный этим process через mktemp.
    if [[ -d "${runtime_directory}" ]]; then
        rm -rf -- "${runtime_directory}"
    fi
}

# EXIT trap централизует удаление raw logs для success и handled failures.
trap cleanup_runtime_directory EXIT

# Функция печатает bounded ошибку без URL и runtime payload.
print_error() {
    # Caller передаёт только заранее сформулированную safe причину.
    local error_message="$1"
    # stderr отделяет runner diagnostics от будущего report content.
    printf 'Ошибка: %s\n' "${error_message}" >&2
}

# Пустой selection является явным NOT RUN, а не ложным acceptance pass.
print_not_run_missing_selection() {
    # Сообщение намеренно не перечисляет какие-либо guessed/default URL.
    printf 'NOT RUN: missing explicit --url/--report selection; acceptance not satisfied\n' >&2
}

# Справка описывает только explicit URL workflow и privacy contract.
print_help() {
    # Heredoc делает manual checklist вызова читаемым.
    cat <<'EOF'
Usage: scripts/progressive-web-smoke.sh --url URL [--url URL ...] --report FILE [OPTIONS]

Runs the release Rustiplayer binary for only the explicit HTTP(S) URLs supplied by
the user. The runner never discovers URLs, fixtures, browser profiles, or cookies.
It preserves the normal system yt-dlp configuration lookup and saves only a
redacted report; raw runtime logs are deleted on exit.

Options:
  --url URL          Explicit user-selected HTTP(S) URL; may be repeated.
  --report FILE      New report file; an existing path is never overwritten.
  --duration SECONDS Timebox for each URL. Default: 120.
  --binary FILE      Use an existing executable instead of building release app.
  --dry-run          Validate selection and print a redacted plan only.
  --help             Show this help.

Outcome contract:
  NOT RUN                Missing selection or dry-run; never an acceptance pass.
  MANUAL REVIEW REQUIRED Runtime launched and redacted evidence was written.
  FAIL                   Build, runtime, parser, or report creation failed.
EOF
}

# Duration принимает только положительные целые секунды.
validate_duration_seconds() {
    # Значение duration передаётся первым аргументом.
    local candidate_duration="$1"
    # Ноль, знак и дробная часть не имеют корректной timeout semantics.
    if [[ ! "${candidate_duration}" =~ ^[1-9][0-9]*$ ]]; then
        print_error "--duration должен быть положительным целым числом секунд"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# URL validation допускает только progressive HTTP(S) input этой session card.
validate_explicit_url() {
    # Exact URL передаётся первым аргументом и никогда не печатается функцией.
    local candidate_url="$1"
    # Control characters могли бы подделать строки report-а или shell diagnostics.
    if [[ "${candidate_url}" == *$'\n'* || "${candidate_url}" == *$'\r'* || "${candidate_url}" == *$'\t'* ]]; then
        print_error "--url не должен содержать управляющие символы"
        exit "${USAGE_EXIT_CODE}"
    fi
    # Runner не принимает path, search term, file URL или future provider schemes.
    if [[ ! "${candidate_url}" =~ ^https?://[^/[:space:]]+(/[^[:space:]]*)?$ ]]; then
        print_error "--url должен быть explicit absolute HTTP(S) URL"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# Parser сохраняет exact URL bytes в массиве и не выполняет normalization.
parse_arguments() {
    # Цикл обрабатывает argv слева направо без eval/getopt/string reconstruction.
    while (($# > 0)); do
        # Любой аргумент делает partial selection ошибкой, а не пустым NOT RUN.
        received_any_argument="true"
        # Case ограничивает публичный CLI перечисленными options.
        case "$1" in
            --help)
                print_help
                exit "${SUCCESS_EXIT_CODE}"
                ;;
            --dry-run)
                dry_run="true"
                shift
                ;;
            --url)
                # Option без значения получает bounded parser error.
                if (($# < 2)); then
                    print_error "--url требует значение"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Проверка выполняется до сохранения selection.
                validate_explicit_url "$2"
                # Bash array сохраняет каждый explicit URL отдельным argv.
                explicit_urls+=("$2")
                shift 2
                ;;
            --report)
                # Report target не может быть неявным или отсутствующим.
                if (($# < 2)); then
                    print_error "--report требует путь"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Повтор option-а мог бы скрыть ошибочно выбранный первый target.
                if [[ -n "${report_path}" ]]; then
                    print_error "--report можно указать только один раз"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Exact path сохраняется до safe validation parent directory.
                report_path="$2"
                shift 2
                ;;
            --duration)
                # Duration без значения является parser failure.
                if (($# < 2)); then
                    print_error "--duration требует значение"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Validation выполняется до mutation текущего duration.
                validate_duration_seconds "$2"
                # После проверки duration становится runtime policy этого run-а.
                duration_seconds="$2"
                shift 2
                ;;
            --binary)
                # Explicit binary path нужен ровно один и не является media input.
                if (($# < 2)); then
                    print_error "--binary требует путь"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Повтор binary option-а запрещён как неоднозначный execution owner.
                if [[ -n "${selected_binary}" ]]; then
                    print_error "--binary можно указать только один раз"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Path валидируется после dry-run selection validation.
                selected_binary="$2"
                shift 2
                ;;
            *)
                # Positional URL тоже запрещён: user intent обязан быть явным --url.
                print_error "неизвестный аргумент; URL передаётся только через --url"
                exit "${USAGE_EXIT_CODE}"
                ;;
        esac
    done
}

# Selection validation запрещает auto-discovery и accidental report overwrite.
validate_selection() {
    # Без URL и report пустой invocation остаётся успешным NOT RUN.
    if ((${#explicit_urls[@]} == 0)) && [[ -z "${report_path}" ]]; then
        print_not_run_missing_selection
        # Частично переданные options не считаются безопасным пустым invocation.
        if [[ "${received_any_argument}" == "true" ]]; then
            exit "${USAGE_EXIT_CODE}"
        fi
        exit "${SUCCESS_EXIT_CODE}"
    fi
    # Реальный или dry-run workflow всегда требует хотя бы один exact URL.
    if ((${#explicit_urls[@]} == 0)); then
        print_not_run_missing_selection
        exit "${USAGE_EXIT_CODE}"
    fi
    # Report path обязателен, чтобы privacy contract не зависел от terminal history.
    if [[ -z "${report_path}" ]]; then
        print_not_run_missing_selection
        exit "${USAGE_EXIT_CODE}"
    fi
}

# Report target разрешается через существующий parent и не перезаписывает данные.
validate_report_target() {
    # Parent path вычисляется без создания directory tree и hidden defaults.
    local report_directory
    # dirname обрабатывает относительный filename как текущий directory.
    report_directory="$(dirname -- "${report_path}")"
    # Не существующий parent требует явного решения пользователя вне runner-а.
    if [[ ! -d "${report_directory}" ]]; then
        print_error "parent directory для --report не существует"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Canonical parent исключает различия cwd в последующих file checks.
    report_directory="$(cd -- "${report_directory}" && pwd -P)"
    # Basename сохраняет явно выбранное имя report-а.
    report_path="${report_directory}/$(basename -- "${report_path}")"
    # Existing file, directory или dangling symlink никогда не перезаписывается.
    if [[ -e "${report_path}" || -L "${report_path}" ]]; then
        print_error "--report уже существует; runner не перезаписывает artifacts"
        exit "${FAILURE_EXIT_CODE}"
    fi
}

# Внешние tools проверяются до build и создания report-а.
require_command() {
    # Имя команды передаётся первым аргументом.
    local required_command="$1"
    # command -v не запускает tool и не меняет внешнее состояние.
    if ! command -v "${required_command}" >/dev/null 2>&1; then
        print_error "обязательная команда недоступна"
        exit "${FAILURE_EXIT_CODE}"
    fi
}

# Выбирает explicit executable либо строит canonical release binary.
prepare_binary() {
    # Explicit binary не запускает Cargo build и удобен для локального повторного прогона.
    if [[ -n "${selected_binary}" ]]; then
        # File должен существовать и иметь execute permission.
        if [[ ! -f "${selected_binary}" || ! -x "${selected_binary}" ]]; then
            print_error "--binary должен указывать на executable regular file"
            exit "${FAILURE_EXIT_CODE}"
        fi
        # Canonical path делает дальнейший exec независимым от cwd.
        local binary_directory
        # Parent executable path разрешается отдельно от basename.
        binary_directory="$(cd -- "$(dirname -- "${selected_binary}")" && pwd -P)"
        # Exact executable сохраняется без отражения в report.
        selected_binary="${binary_directory}/$(basename -- "${selected_binary}")"
        return
    fi
    # Default workflow компилирует ровно production app package на pinned primary Rust.
    cargo +1.96.0 build --release -p app-egui --locked
    # Build обязан создать canonical executable до network/GUI запуска.
    if [[ ! -x "${DEFAULT_RUSTIPLAYER_BINARY}" ]]; then
        print_error "release build не создал executable rustiplayer"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Последующие scenarios используют один и тот же проверенный binary.
    selected_binary="${DEFAULT_RUSTIPLAYER_BINARY}"
}

# Raw runtime directory создаётся только после успешного build/preflight.
create_runtime_directory() {
    # mktemp гарантирует process-unique directory в системном temporary root.
    runtime_directory="$(mktemp -d -t rustiplayer-progressive-web.XXXXXX)"
}

# Redactor удаляет explicit URL, любые HTTP(S) endpoints и строки с transport/extractor material.
redact_runtime_log() {
    # Raw log path принадлежит temporary directory runner-а.
    local raw_log_path="$1"
    # Exact explicit URL нужен literal replacement до heuristic endpoint scan-а.
    local exact_url="$2"
    # AWK пишет только sanitized stdout; raw input file никогда не append-ится в report напрямую.
    awk -v exact_url="${exact_url}" '
        function replace_exact(text, secret, position) {
            if (secret == "") {
                return text
            }
            while ((position = index(text, secret)) > 0) {
                text = substr(text, 1, position - 1) "<redacted-url>" substr(text, position + length(secret))
            }
            return text
        }
        {
            lower_line = tolower($0)
            if (lower_line ~ /authorization|cookie|set-cookie|header|request[_ -]?data|requested[_ -]?formats|extractor|payload/) {
                print "<redacted-secret-line>"
                next
            }
            sanitized_line = replace_exact($0, exact_url)
            gsub(/https?:\/\/[^[:space:]<>\"]+/, "<redacted-url>", sanitized_line)
            print sanitized_line
        }
    ' "${raw_log_path}"
}

# Header report-а содержит только bounded policy metadata и пустой manual checklist.
write_report_header() {
    # Owner-only permissions применяются до первого write.
    umask 077
    # Новый file создаётся только после explicit non-existing target validation.
    {
        printf '# S27 progressive/web manual report\n\n'
        printf 'Outcome: MANUAL REVIEW REQUIRED\n'
        printf 'Explicit URL count: %s\n' "${#explicit_urls[@]}"
        printf 'Per-URL timebox seconds: %s\n' "${duration_seconds}"
        printf 'System yt-dlp config lookup: preserved\n'
        printf 'Raw URLs/headers/cookies/extractor payloads: not retained\n\n'
        printf '## Manual checklist\n\n'
        printf -- '- [ ] candidate normalization/profile exclusions are visible and typed\n'
        printf -- '- [ ] audio capabilities select a playable audio path or typed rejection\n'
        printf -- '- [ ] config v7 preferred height affects BestPlayable selection\n'
        printf -- '- [ ] both supplied Range/non-Range cases behave as expected\n'
        printf -- '- [ ] queue Ready/authorize/Enqueued/Installed barrier preserves old playback on pre-barrier failure\n'
        printf -- '- [ ] URL sidebar is secret-safe and has no second URL input\n'
        printf -- '- [ ] candidate switch works while Playing and Paused\n'
        printf -- '- [ ] CUE/group part switch preserves Item/lineage/window semantics\n'
        printf -- '- [ ] system yt-dlp auth works without app credential persistence\n'
        printf -- '- [ ] restore/settings/shutdown keep exact lifecycle semantics\n'
        printf -- '- [ ] acknowledged exact locator persists separately from transient secrets\n'
        printf -- '- [ ] cancellation/stale completion cannot publish a newer active source\n\n'
        printf '## Sanitized runtime evidence\n'
    } >"${report_path}"
}

# Один scenario запускает app с exact argv URL и сразу sanitizes его raw log.
run_explicit_url() {
    # Stable ordinal не раскрывает host/path/query пользователя.
    local scenario_index="$1"
    # Exact URL передаётся только app process и redactor-у.
    local explicit_url="$2"
    # Raw log filename содержит только ordinal.
    local raw_log_path="${runtime_directory}/scenario-${scenario_index}.raw.log"
    # Runner stderr сообщает прогресс без URL или safe-host guessing.
    printf 'Running explicit URL scenario %s/%s\n' "${scenario_index}" "${#explicit_urls[@]}" >&2
    # Timeout status обрабатывается явно, поэтому set -e временно отключается.
    set +e
    # XDG_CONFIG_HOME намеренно не подменяется: system/user yt-dlp auth должен быть доступен.
    env \
        "RUST_LOG=${PROGRESSIVE_WEB_RUST_LOG}" \
        "NO_COLOR=1" \
        timeout \
        --signal=INT \
        --kill-after=5s \
        "${duration_seconds}s" \
        "${selected_binary}" \
        "${explicit_url}" \
        >"${raw_log_path}" 2>&1
    # Exit status сохраняется до возврата strict mode.
    local runtime_status=$?
    # Следующие file/report операции снова выполняются под set -e.
    set -e
    # Report section не содержит input identity.
    {
        printf '\n### Explicit URL scenario %s\n\n' "${scenario_index}"
        printf 'Runtime exit status: %s\n\n' "${runtime_status}"
        printf '```text\n'
        redact_runtime_log "${raw_log_path}" "${explicit_url}"
        printf '```\n'
    } >>"${report_path}"
    # Normal close и timebox termination являются допустимым manual-runner transport outcome.
    case "${runtime_status}" in
        0 | 124 | 137)
            return
            ;;
        *)
            print_error "progressive web runtime завершился неожиданным status; см. redacted report"
            return "${FAILURE_EXIT_CODE}"
            ;;
    esac
}

# Dry-run никогда не показывает shell-quoted URL и не создаёт report.
run_dry_plan() {
    # Count достаточно, чтобы проверить selected matrix без раскрытия identities.
    printf 'progressive web dry-run: explicit URL count=%s; duration=%ss\n' "${#explicit_urls[@]}" "${duration_seconds}" >&2
    # Каждый scenario получает только redacted placeholder.
    local scenario_index
    # Sequence строится по array indices без чтения URL value.
    for scenario_index in "${!explicit_urls[@]}"; do
        printf 'scenario %s: <redacted-explicit-url>\n' "$((scenario_index + 1))" >&2
    done
    # Dry-run не является manual acceptance evidence.
    printf 'NOT RUN: dry-run only; acceptance not satisfied\n' >&2
}

# Main отделяет parser, preflight, build, runtime и report lifecycle.
main() {
    # Exact argv разбирается до любых side effects.
    parse_arguments "$@"
    # Selection validation гарантирует отсутствие default/discovered URL.
    validate_selection
    # Report path проверяется и в dry-run, но file создаётся только в real mode.
    validate_report_target
    # Dry-run завершается до tool checks, build, temp files и report write.
    if [[ "${dry_run}" == "true" ]]; then
        run_dry_plan
        exit "${SUCCESS_EXIT_CODE}"
    fi
    # Runtime работает от repository root, но пользовательский config environment сохраняется.
    cd "${REPO_ROOT}"
    # Набор tools минимален и не включает downloader/browser automation.
    require_command "awk"
    require_command "mktemp"
    require_command "timeout"
    # Cargo нужен только при отсутствии explicit prebuilt binary.
    if [[ -z "${selected_binary}" ]]; then
        require_command "cargo"
    fi
    # Один binary готовится до report/runtime creation.
    prepare_binary
    # Raw storage создаётся после всех deterministic preflights.
    create_runtime_directory
    # Report header фиксирует manual-only verdict до первого scenario.
    write_report_header
    # Aggregate status позволяет sanitized report получить evidence всех selected URLs.
    local aggregate_status="${SUCCESS_EXIT_CODE}"
    # Bash array iteration сохраняет exact user order.
    local scenario_offset
    # Каждый URL запускается независимо, но ни один raw log не переживает process exit.
    for scenario_offset in "${!explicit_urls[@]}"; do
        if ! run_explicit_url "$((scenario_offset + 1))" "${explicit_urls[scenario_offset]}"; then
            aggregate_status="${FAILURE_EXIT_CODE}"
        fi
    done
    # Final message называет artifact path, но не URL identities.
    printf 'MANUAL REVIEW REQUIRED: redacted report written to %s\n' "${report_path}" >&2
    # Runtime failure остаётся failure даже при успешно sanitized report.
    exit "${aggregate_status}"
}

# Единственная process entrypoint передаёт исходный argv без reconstruction.
main "$@"

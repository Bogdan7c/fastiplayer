#!/usr/bin/env bash
# S42 opt-in manual web-media runner; CI и обычные tests его не запускают.

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
# S42 library path строится только от canonical script directory.
readonly S42_MANUAL_LIBRARY="${script_directory}/lib/progressive-web-smoke-s42.sh"
# Missing/unreadable owner module блокирует runner до любого media input.
if [[ ! -f "${S42_MANUAL_LIBRARY}" || ! -r "${S42_MANUAL_LIBRARY}" ]]; then
    printf 'Ошибка: S42 manual owner module недоступен\n' >&2
    exit "${FAILURE_EXIT_CODE}"
fi
# shellcheck source=lib/progressive-web-smoke-s42.sh
source "${S42_MANUAL_LIBRARY}"
# Default binary соответствует package app-egui и release build output.
readonly DEFAULT_RUSTIPLAYER_BINARY="${REPO_ROOT}/target/release/rustiplayer"
# Отдельный env override не позволяет обычному RUST_LOG незаметно изменить report surface.
readonly PROGRESSIVE_WEB_RUST_LOG="${RUSTIPLAYER_PROGRESSIVE_WEB_RUST_LOG:-${DEFAULT_PROGRESSIVE_WEB_RUST_LOG}}"

# Pending case связывает следующий explicit --url/--fixture с безопасной ролью.
pending_case_id=""
# Duration начинается с документированного безопасного default-а.
duration_seconds="${DEFAULT_DURATION_SECONDS}"
# Report path обязателен для реального запуска и не выбирается автоматически.
report_path=""
# Explicit binary полезен для self-test и уже собранного локального app binary.
selected_binary=""
# Binary origin запрещает приписывать explicit prebuilt текущему workspace.
selected_binary_origin=""
# Exact executable digest является authoritative runtime identity в report.
selected_binary_sha256=""
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
    # Сообщение намеренно не перечисляет какие-либо guessed/default URL или fixtures.
    printf 'NOT RUN: missing explicit --url/--fixture/--report selection; acceptance not satisfied\n' >&2
}

# Справка описывает explicit S42 case workflow и backward-compatible URL-only режим.
print_help() {
    # Heredoc делает длинный safe case contract читаемым без shell reconstruction.
    cat <<'EOF'
Usage:
  scripts/progressive-web-smoke.sh --case CASE_ID --url URL --report FILE [OPTIONS]
  scripts/progressive-web-smoke.sh --case CASE_ID --fixture FILE --report FILE [OPTIONS]
  scripts/progressive-web-smoke.sh --url URL [--url URL ...] --report FILE [OPTIONS]

Runs the release Rustiplayer binary only for explicit HTTP/HTTPS/FTP/FTPS URLs or
local fixtures supplied by the user. Named CASE_ID values are safe report labels;
raw URL/fixture identities are never retained. The real run requires exact system
yt-dlp 2026.07.04, preserves its normal config/plugin/cookie lookup, and records
workspace clean/dirty state, exact Rustiplayer/yt-dlp executable hashes and only
redacted runtime logs.

Options:
  --case CASE_ID     Safe S42 role for the next --url or --fixture.
  --url URL          Explicit approved network URL. Without --case, maps to a
                     backward-compatible legacy-url-N case and cannot complete S42.
  --fixture FILE     Explicit local fixture for a fixture-only named case.
  --report FILE      New report file; an existing path is never overwritten.
  --duration SECONDS Timebox for each case. Default: 120.
  --binary FILE      Use an existing executable instead of building release app.
  --dry-run          Validate selection and print a redacted plan only.
  --help             Show this help.

Status contract:
  Matrix NOT RUN                One or more of the 29 required case IDs is missing.
  Matrix MANUAL REVIEW REQUIRED All 29 case IDs were selected; human checks remain.
  Runner MANUAL REVIEW REQUIRED Selected real runs completed; human checks remain.
  Runner FAIL                   Version, build, runtime, parser, or report lifecycle failed.
  Terminal NOT RUN              Missing selection or dry-run; no report was created.

Required safe CASE_ID values are documented in docs/web-media-s42-final-acceptance.md.
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

# Parser сохраняет exact URL/fixture bytes и связывает их только с safe case ID.
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
            --case)
                # Option без safe ID является bounded parser failure.
                if (($# < 2)); then
                    print_error "--case требует значение"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Новый case нельзя начать до input предыдущего.
                if [[ -n "${pending_case_id}" ]]; then
                    print_error "предыдущий --case не получил --url или --fixture"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Allowlist проверяется до сохранения pending state.
                validate_case_id "$2"
                # Следующий explicit input получает эту safe role.
                pending_case_id="$2"
                shift 2
                ;;
            --url)
                # Option без значения получает bounded parser error.
                if (($# < 2)); then
                    print_error "--url требует значение"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Проверка выполняется до сохранения selection.
                validate_explicit_url "$2"
                # Named case сохраняет strict role; legacy call получает safe generated ID.
                if [[ -n "${pending_case_id}" ]]; then
                    # URL kind и scheme обязаны соответствовать exact case.
                    validate_case_input "${pending_case_id}" "url" "$2"
                    # Scenario сохраняется только после полной validation.
                    add_scenario "${pending_case_id}" "url" "$2"
                    # Pending role consumed ровно один раз.
                    pending_case_id=""
                else
                    # Backward-compatible URL не может притвориться completed S42 row.
                    add_scenario "legacy-url-$(( ${#scenario_case_ids[@]} + 1 ))" "url" "$2"
                fi
                shift 2
                ;;
            --fixture)
                # Fixture без path является bounded parser failure.
                if (($# < 2)); then
                    print_error "--fixture требует путь"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Fixture всегда требует explicit named role.
                if [[ -z "${pending_case_id}" ]]; then
                    print_error "--fixture требует предшествующий --case"
                    exit "${USAGE_EXIT_CODE}"
                fi
                # Safe path validation не читает filesystem в dry-run.
                validate_explicit_fixture "$2"
                # Named role отделяет playlist/import fixture от URL case.
                validate_case_input "${pending_case_id}" "fixture" "$2"
                # Scenario сохраняет raw path только в process memory.
                add_scenario "${pending_case_id}" "fixture" "$2"
                # Pending role consumed ровно один раз.
                pending_case_id=""
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
                # Positional input запрещён: user intent обязан быть explicit option.
                print_error "неизвестный аргумент; input передаётся только через --url/--fixture"
                exit "${USAGE_EXIT_CODE}"
                ;;
        esac
    done
    # Dangling case не является explicit input selection.
    if [[ -n "${pending_case_id}" ]]; then
        print_error "--case требует следующий --url или --fixture"
        exit "${USAGE_EXIT_CODE}"
    fi
}

# Selection validation запрещает auto-discovery и accidental report overwrite.
validate_selection() {
    # Без scenarios и report пустой invocation остаётся успешным NOT RUN.
    if ((${#scenario_case_ids[@]} == 0)) && [[ -z "${report_path}" ]]; then
        print_not_run_missing_selection
        # Частично переданные options не считаются безопасным пустым invocation.
        if [[ "${received_any_argument}" == "true" ]]; then
            exit "${USAGE_EXIT_CODE}"
        fi
        exit "${SUCCESS_EXIT_CODE}"
    fi
    # Реальный или dry-run workflow всегда требует хотя бы один exact input.
    if ((${#scenario_case_ids[@]} == 0)); then
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

# Вычисляет exact digest уже выбранного Rustiplayer executable.
record_selected_binary_provenance() {
    # SHA utility возвращает digest и path; path в report не сохраняется.
    local sha256_output
    # Нечитаемый либо изменившийся executable блокирует неполный provenance.
    if ! sha256_output="$(sha256sum -- "${selected_binary}")"; then
        print_error "не удалось вычислить SHA-256 Rustiplayer binary"
        exit "${FAILURE_EXIT_CODE}"
    fi
    # Первый whitespace-delimited field является exact digest.
    selected_binary_sha256="${sha256_output%% *}"
    # Malformed output нельзя выдавать за executable identity.
    if [[ ! "${selected_binary_sha256}" =~ ^[0-9a-fA-F]{64}$ ]]; then
        print_error "SHA-256 Rustiplayer binary имеет некорректный формат"
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
        # External origin запрещает связывать prebuilt bytes с workspace HEAD.
        selected_binary_origin="explicit-external-prebuilt"
    else
        # Default workflow компилирует ровно production app package на pinned primary Rust.
        cargo +1.96.0 build --release -p app-egui --locked
        # Build обязан создать canonical executable до network/GUI запуска.
        if [[ ! -x "${DEFAULT_RUSTIPLAYER_BINARY}" ]]; then
            print_error "release build не создал executable rustiplayer"
            exit "${FAILURE_EXIT_CODE}"
        fi
        # Последующие scenarios используют один и тот же проверенный binary.
        selected_binary="${DEFAULT_RUSTIPLAYER_BINARY}"
        # Origin честно указывает на текущий worktree, а clean/dirty хранится отдельно.
        selected_binary_origin="runner-built-from-current-worktree"
    fi
    # Любой origin получает один exact digest до создания report/runtime artifacts.
    record_selected_binary_provenance
}

# Raw runtime directory создаётся только после успешного build/preflight.
create_runtime_directory() {
    # mktemp гарантирует process-unique directory в системном temporary root.
    runtime_directory="$(mktemp -d -t rustiplayer-progressive-web.XXXXXX)"
}

# Один scenario запускает app с exact argv input и сразу sanitizes raw log.
run_explicit_scenario() {
    # Stable ordinal не раскрывает host/path/query пользователя.
    local scenario_index="$1"
    # Safe case ID допускается в terminal/report.
    local scenario_case_id="$2"
    # Non-secret input kind допускается в report.
    local scenario_input_kind="$3"
    # Exact URL/fixture передаётся только app process и redactor-у.
    local explicit_input="$4"
    # Raw log filename содержит только ordinal.
    local raw_log_path="${runtime_directory}/scenario-${scenario_index}.raw.log"
    # Runner stderr сообщает safe role без URL/fixture identity.
    printf 'Running safe case %s (%s/%s)\n' \
        "${scenario_case_id}" \
        "${scenario_index}" \
        "${#scenario_case_ids[@]}" >&2
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
        "${explicit_input}" \
        >"${raw_log_path}" 2>&1
    # Exit status сохраняется до возврата strict mode.
    local runtime_status=$?
    # Следующие file/report операции снова выполняются под set -e.
    set -e
    # Report section не содержит input identity.
    {
        printf '\n### Safe case `%s`\n\n' "${scenario_case_id}"
        printf 'Input kind: %s (raw identity not retained)\n\n' "${scenario_input_kind}"
        printf 'Runtime exit status: %s\n\n' "${runtime_status}"
        printf '```text\n'
        redact_runtime_log "${raw_log_path}" "${explicit_input}" "${scenario_input_kind}"
        printf '```\n'
    } >>"${report_path}"
    # Normal close и graceful INT timebox являются допустимым manual-runner outcome.
    case "${runtime_status}" in
        0 | 124)
            return
            ;;
        *)
            # Status 137 означает SIGKILL/kill-after и потому не является bounded shutdown PASS.
            print_error "web-media runtime завершился неожиданным status; см. redacted report"
            return "${FAILURE_EXIT_CODE}"
            ;;
    esac
}

# Dry-run никогда не показывает shell-quoted input и не создаёт report.
run_dry_plan() {
    # Count достаточно, чтобы проверить selected matrix без раскрытия identities.
    printf 'S42 web-media dry-run: selected case count=%s; missing required=%s; duration=%ss\n' \
        "${#scenario_case_ids[@]}" \
        "${#missing_s42_case_ids[@]}" \
        "${duration_seconds}" >&2
    # Каждый scenario показывает только safe label/kind и redacted placeholder.
    local scenario_offset
    # Sequence строится по parallel array indices без чтения raw value.
    for scenario_offset in "${!scenario_case_ids[@]}"; do
        printf 'case %s (%s): <redacted-explicit-input>\n' \
            "${scenario_case_ids[scenario_offset]}" \
            "${scenario_input_kinds[scenario_offset]}" >&2
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
    # Missing matrix rows вычисляются до dry-run/report без raw identities.
    collect_missing_s42_case_ids
    # Report path проверяется и в dry-run, но file создаётся только в real mode.
    validate_report_target
    # Dry-run завершается до tool checks, build, temp files и report write.
    if [[ "${dry_run}" == "true" ]]; then
        run_dry_plan
        exit "${SUCCESS_EXIT_CODE}"
    fi
    # Набор tools минимален и не включает downloader/browser automation.
    require_command "awk"
    require_command "git"
    require_command "mktemp"
    require_command "realpath"
    require_command "sha256sum"
    require_command "timeout"
    require_command "yt-dlp"
    # Fixture нормализуется относительно caller cwd до перехода в repository root.
    validate_real_fixture_inputs
    # Runtime работает от repository root, но пользовательский config environment сохраняется.
    cd "${REPO_ROOT}"
    # Cargo нужен только при отсутствии explicit prebuilt binary.
    if [[ -z "${selected_binary}" ]]; then
        require_command "cargo"
    fi
    # Pinned version/hash/source provenance проверяется до build/report creation.
    verify_ytdlp_provenance
    # Один binary готовится до report/runtime creation.
    prepare_binary
    # Raw storage создаётся после всех deterministic preflights.
    create_runtime_directory
    # Report header фиксирует manual-only verdict до первого scenario.
    write_report_header
    # Aggregate status позволяет sanitized report получить evidence всех selected cases.
    local aggregate_status="${SUCCESS_EXIT_CODE}"
    # Bash array iteration сохраняет exact user order.
    local scenario_offset
    # Каждый case запускается независимо, но ни один raw log не переживает process exit.
    for scenario_offset in "${!scenario_case_ids[@]}"; do
        if ! run_explicit_scenario \
            "$((scenario_offset + 1))" \
            "${scenario_case_ids[scenario_offset]}" \
            "${scenario_input_kinds[scenario_offset]}" \
            "${scenario_inputs[scenario_offset]}"; then
            aggregate_status="${FAILURE_EXIT_CODE}"
        fi
    done
    # Authoritative footer отличает manual review от runtime FAIL.
    write_final_report_outcome "${aggregate_status}"
    # Успешный transport run всё равно требует human review.
    if [[ "${aggregate_status}" == "${SUCCESS_EXIT_CODE}" ]]; then
        printf 'MANUAL REVIEW REQUIRED: redacted report written to %s\n' "${report_path}" >&2
    else
        # Failed runtime получает честный FAIL и сохраняет только sanitized evidence.
        printf 'FAIL: runtime error; redacted report written to %s\n' "${report_path}" >&2
    fi
    # Runtime failure остаётся failure даже при успешно sanitized report.
    exit "${aggregate_status}"
}

# Единственная process entrypoint передаёт исходный argv без reconstruction.
main "$@"

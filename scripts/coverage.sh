#!/usr/bin/env bash
# Единый runner трёхпрогонного stable-coordinate coverage gate и диагностических reports.

# Строгий режим запрещает терять ошибки Cargo, LLVM или policy parser-а.
set -Eeuo pipefail

# Каталог скрипта вычисляется независимо от текущего рабочего каталога.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
# Корень репозитория находится на один уровень выше scripts/.
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"
# readonly защищает вычисленные пути от случайной перезаписи.
readonly SCRIPT_DIRECTORY="${script_directory}"
# Все Cargo и artifact пути разрешаются от единого корня.
readonly REPO_ROOT="${repo_root}"
# Exact version синхронизирован с coverage/policy.json и CI install step.
readonly CARGO_LLVM_COV_VERSION="0.8.7"
# Coverage instrumentation фиксируется тем же primary Rust, что и release gate.
readonly PRIMARY_RUST_TOOLCHAIN="1.96.0"
# Rust 1.96.0 поставляет exact LLVM, которым измерен stable-coordinate baseline.
readonly LLVM_COV_VERSION="22.1.2"
# Raw profiles и instrumented executables изолированы от обычного Cargo target.
readonly PROFILE_DIRECTORY="${REPO_ROOT}/target/llvm-cov-target"
# Общий artifact root остаётся совместимым с существующей CI публикацией target/coverage.
readonly ARTIFACT_DIRECTORY="${REPO_ROOT}/target/coverage"
# Runner транзакционно заменяет только это поддерево после всех трёх runs.
readonly STABLE_ARTIFACT_DIRECTORY="${ARTIFACT_DIRECTORY}/stable"
# Versioned policy определяет first-party source domains для extractor-а.
readonly POLICY_PATH="${REPO_ROOT}/coverage/policy.json"
# Отдельная versioned policy владеет exact runtime-generated build roots.
readonly EXECUTABLE_INVENTORY_POLICY_PATH="${REPO_ROOT}/coverage/executable-inventory-policy.json"
# После явной миграции этот путь содержит blocking stable-coordinate baseline v2.
readonly BASELINE_PATH="${REPO_ROOT}/coverage/baseline.json"
# Measurement exceptions отделены от legacy relocation exceptions.
readonly MEASUREMENT_EXCEPTIONS_PATH="${REPO_ROOT}/coverage/measurement-exceptions.json"
# Legacy v1 exceptions нужны только для явного bootstrap и report-only provenance.
readonly LEGACY_EXCEPTIONS_PATH="${REPO_ROOT}/coverage/exceptions.json"
# Последний compact summary остаётся диагностикой и не принимает blocking решение.
readonly CURRENT_SUMMARY_PATH="${ARTIFACT_DIRECTORY}/current-summary.json"
# Stable check пишет атомарный отчёт рядом с cohort, который он проверил.
readonly STABLE_CHECK_PATH="${STABLE_ARTIFACT_DIRECTORY}/check.json"
# Default bootstrap output намеренно находится вне versioned coverage/.
readonly DEFAULT_BOOTSTRAP_OUTPUT="${ARTIFACT_DIRECTORY}/stable-baseline-v2-proposal.json"

# Функция печатает стабильный и явный CLI-контракт runner-а.
print_help() {
    # Heredoc сохраняет справку читаемой и не выполняет подстановки.
    cat <<'EOF'
Usage: scripts/coverage.sh COMMAND [OUTPUT]

Commands:
  check               Измерить три normal-concurrency run и применить blocking v2 gate.
  report              Измерить тот же cohort без baseline comparison.
  bootstrap [OUTPUT]  Явно создать v2 baseline proposal из текущего v1 и нового cohort.
  baseline            Устаревшее имя; завершится ошибкой с инструкцией migration.

`check` никогда не меняет baseline. Legacy summary, LCOV и HTML являются только
диагностикой; blocking status принадлежит stable-coordinate check.
EOF
}

# Функция проверяет exact release cargo-llvm-cov до дорогой пересборки.
require_coverage_tool() {
    # Полная строка rustc нужна для понятной toolchain diagnostics.
    local actual_rustc_version
    if ! actual_rustc_version="$(rustc +"${PRIMARY_RUST_TOOLCHAIN}" --version)"; then
        # Отсутствующий toolchain является setup/corruption failure, а не regression.
        printf 'Ошибка: не удалось запустить Rust %s для coverage.\n' \
            "${PRIMARY_RUST_TOOLCHAIN}" >&2
        return 2
    fi
    # Удаляем имя binary, сохраняя release и необязательную commit metadata.
    local release_and_build="${actual_rustc_version#rustc }"
    # Первый token после имени rustc является exact semver release.
    local actual_rust_release="${release_and_build%% *}"
    # Неправильный compiler делает coverage coordinates несопоставимыми.
    if [[ "${actual_rust_release}" != "${PRIMARY_RUST_TOOLCHAIN}" ]]; then
        # Diagnostics показывает ожидаемый и фактический compiler.
        printf 'Ошибка: coverage требует Rust %s, получено `%s`.\n' \
            "${PRIMARY_RUST_TOOLCHAIN}" "${actual_rustc_version}" >&2
        return 2
    fi
    # Реальная строка version сохраняется для понятной ошибки.
    local actual_version
    # Отсутствующий subcommand тоже становится явным setup failure.
    if ! actual_version="$(cargo +"${PRIMARY_RUST_TOOLCHAIN}" llvm-cov --version)"; then
        # Команда установки остаётся известной владельцу CI image.
        printf 'Ошибка: cargo-llvm-cov %s недоступен.\n' \
            "${CARGO_LLVM_COV_VERSION}" >&2
        return 2
    fi
    # Полное совпадение защищает baseline от изменений LLVM wrapper semantics.
    if [[ "${actual_version}" != "cargo-llvm-cov ${CARGO_LLVM_COV_VERSION}" ]]; then
        # Ненулевой status запрещает несопоставимый report.
        printf 'Ошибка: требуется cargo-llvm-cov %s, установлено `%s`.\n' \
            "${CARGO_LLVM_COV_VERSION}" "${actual_version}" >&2
        return 2
    fi
}

# Функция проверяет v2 inputs до clean/build и отличает старую v1 от corruption.
validate_stable_check_inputs() {
    # Вывод validator-а перехватывается, чтобы v1 migration не выглядела corruption.
    local stable_validation_output
    if stable_validation_output="$(
        python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" validate \
            --kind baseline \
            --input "${BASELINE_PATH}" 2>&1
    )"; then
        # Валидный v2 baseline допускает следующий preflight.
        :
    elif python3 "${SCRIPT_DIRECTORY}/coverage_metrics.py" validate-baseline \
        >/dev/null 2>&1; then
        # Текущий v1 является ожидаемым migration состоянием, а не поводом переписать файл.
        printf 'Ошибка: coverage/baseline.json ещё использует schema v1.\n' >&2
        printf 'Запустите `scripts/coverage.sh bootstrap`.\n' >&2
        printf 'Проверьте proposal и обновите versioned policy явно.\n' >&2
        return 2
    else
        # Для malformed v2 сохраняется первичная точная diagnostics A validator-а.
        printf '%s\n' "${stable_validation_output}" >&2
        printf 'Ошибка: stable coverage baseline повреждён или несовместим.\n' >&2
        return 2
    fi
    # Measurement exceptions проходят отдельную schema/hash/inventory validation.
    python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" validate \
        --kind measurement-exceptions \
        --input "${MEASUREMENT_EXCEPTIONS_PATH}" || return 2
}

# Функция запускает build-once и ровно три одинаковых normal-concurrency executions.
run_stable_coverage_suite() {
    # BASHPID уникализирует только private stage/profile prefix; policy semantics не меняются.
    local session_id="coverage-shell-${BASHPID}"
    # Runner сам владеет clean, profile isolation, reports, extraction и intersection.
    if python3 "${SCRIPT_DIRECTORY}/coverage_runner.py" \
        --repo-root "${REPO_ROOT}" \
        --profile-directory "${PROFILE_DIRECTORY}" \
        --artifact-directory "${STABLE_ARTIFACT_DIRECTORY}" \
        --policy "${POLICY_PATH}" \
        --executable-inventory-policy "${EXECUTABLE_INVENTORY_POLICY_PATH}" \
        --coordinate-extractor "${SCRIPT_DIRECTORY}/coverage_coordinates.py" \
        --stability-tool "${SCRIPT_DIRECTORY}/coverage_stability.py" \
        --lcov-validator "${SCRIPT_DIRECTORY}/coverage_metrics.py" \
        --toolchain "${PRIMARY_RUST_TOOLCHAIN}" \
        --cargo-llvm-cov-version "${CARGO_LLVM_COV_VERSION}" \
        --llvm-cov-version "${LLVM_COV_VERSION}" \
        --session-id "${session_id}" \
        --python-command python3; then
        # Успех означает атомарную публикацию целого stable artifact tree.
        :
    else
        # Любая execution/orchestration ошибка имеет frozen status 2.
        printf 'Ошибка: stable coverage cohort не опубликован.\n' >&2
        return 2
    fi
    # Shell boundary не доверяет даже успешному дочернему process без exact трех states.
    local run_number
    for run_number in 1 2 3; do
        # Standalone validator проверяет schema, hashes и repo-relative coordinates.
        python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" validate \
            --kind run \
            --input "${STABLE_ARTIFACT_DIRECTORY}/run-${run_number}.json" || return 2
    done
    # Cohort validation запрещает stale/partial intersection до ratchet.
    python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" validate \
        --kind cohort \
        --input "${STABLE_ARTIFACT_DIRECTORY}/cohort.json" || return 2
}

# Функция атомарно публикует legacy compact summary только как диагностику.
publish_legacy_diagnostics() {
    # Run 3 является обычным членом cohort, а не скрытым дополнительным измерением.
    local llvm_summary_path="${STABLE_ARTIFACT_DIRECTORY}/legacy/run-3-summary.json"
    # Hidden sibling не может быть принят потребителем за завершённый current summary.
    local summary_stage="${ARTIFACT_DIRECTORY}/.current-summary-${BASHPID}.json"
    # Генератор всё ещё fail-closed проверяет tool/source inventory и raw counters shape.
    if ! python3 "${SCRIPT_DIRECTORY}/coverage_metrics.py" generate \
        --input "${llvm_summary_path}" \
        --output "${summary_stage}"; then
        # Удаляется только private файл текущего shell process.
        rm -f -- "${summary_stage}"
        printf 'Ошибка: legacy diagnostic summary не прошёл inventory/corruption validation.\n' >&2
        return 2
    fi
    # Rename внутри одного filesystem атомарно заменяет только завершённый JSON.
    if ! mv -f -- "${summary_stage}" "${CURRENT_SUMMARY_PATH}"; then
        # Ошибка публикации не оставляет private stage следующему запуску.
        rm -f -- "${summary_stage}"
        printf 'Ошибка: legacy diagnostic summary не удалось опубликовать атомарно.\n' >&2
        return 2
    fi
    # Явная метка не позволяет принять случайное снижение legacy ratios за blocking status.
    printf 'Legacy summary/LCOV/HTML опубликованы как report-only diagnostics.\n'
    printf 'Blocking решение принимает только stable-coordinate gate.\n'
}

# Функция применяет единственный blocking execution ratchet и сохраняет exact status.
run_stable_check() {
    # A CLI атомарно пишет report как при PASS, так и при semantic FAIL.
    if python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" check \
        --baseline "${BASELINE_PATH}" \
        --cohort "${STABLE_ARTIFACT_DIRECTORY}/cohort.json" \
        --measurement-exceptions "${MEASUREMENT_EXCEPTIONS_PATH}" \
        --output "${STABLE_CHECK_PATH}"; then
        # Stable-coordinate PASS является единственным blocking success.
        return 0
    else
        # Сохраняем различие semantic regression (1) и malformed/corrupt input (2).
        local check_status=$?
        case "${check_status}" in
            1)
                return 1
                ;;
            2)
                return 2
                ;;
            *)
                printf 'Ошибка: stable coverage check вернул неожиданный status %s.\n' \
                    "${check_status}" >&2
                return 2
                ;;
        esac
    fi
}

# Функция проверяет, что bootstrap proposal не меняет tracked repo path.
resolve_safe_bootstrap_output() {
    # Явный либо default путь нормализуется даже до создания файла.
    local requested_output="$1"
    # Пустая строка не должна неожиданно разрешиться в current directory.
    if [[ -z "${requested_output}" ]]; then
        printf 'Ошибка: bootstrap output path не может быть пустым.\n' >&2
        return 2
    fi
    # realpath -m не требует существования leaf и устраняет ../ ambiguity.
    local resolved_output
    if ! resolved_output="$(realpath -m -- "${requested_output}")"; then
        printf 'Ошибка: bootstrap output path не удалось нормализовать.\n' >&2
        return 2
    fi
    # Внутри worktree proposal разрешён только в ignored target/.
    if [[ "${resolved_output}" == "${REPO_ROOT}"/* \
        && "${resolved_output}" != "${REPO_ROOT}/target/"* ]]; then
        printf 'Ошибка: bootstrap proposal внутри worktree разрешён только под target/.\n' >&2
        return 2
    fi
    # Функция возвращает единственный безопасный canonical path вызывающему коду.
    printf '%s\n' "${resolved_output}"
}

# Функция явно строит migration proposal, не заменяя versioned baseline.
run_bootstrap() {
    # Safe canonical output проверяется до дорогого measurement и любых artifact writes.
    local bootstrap_output
    bootstrap_output="$(resolve_safe_bootstrap_output "$1")" || return 2
    # До измерения текущий v1 и восемь relocation exceptions должны быть валидны.
    if ! python3 "${SCRIPT_DIRECTORY}/coverage_metrics.py" validate-baseline; then
        # v2 либо malformed v1 нельзя молча использовать как legacy input.
        printf 'Ошибка: bootstrap требует валидный legacy baseline v1 и exceptions v1.\n' >&2
        return 2
    fi
    # Migration использует тот же exact трехпрогонный cohort, что и будущий check.
    run_stable_coverage_suite || return 2
    # Report-only поверхность публикуется независимо от будущего blocking baseline.
    publish_legacy_diagnostics || return 2
    # Frozen bootstrap сохраняет legacy v1 целиком внутри нового v2 документа.
    python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" bootstrap \
        --cohort "${STABLE_ARTIFACT_DIRECTORY}/cohort.json" \
        --legacy-baseline "${BASELINE_PATH}" \
        --legacy-exceptions "${LEGACY_EXCEPTIONS_PATH}" \
        --output "${bootstrap_output}" || return 2
    # Proposal ещё раз проверяется standalone validator-ом до handoff владельцу policy.
    python3 "${SCRIPT_DIRECTORY}/coverage_stability.py" validate \
        --kind baseline \
        --input "${bootstrap_output}" || return 2
    # Сообщение подчёркивает, что автоматической versioned mutation не было.
    printf 'Baseline proposal готов: %s\n' "${bootstrap_output}"
    printf '%s\n' \
        'Versioned coverage/baseline.json не изменён; proposal требует review и явной интеграции.'
}

# Главная функция валидирует mode/arity и выполняет только выбранный workflow.
main() {
    # Пустой argv является CLI error и не запускает toolchain checks.
    if (($# == 0)); then
        print_help >&2
        return 2
    fi
    # Help является успешной read-only операцией и не зависит от установленного LLVM.
    if [[ "$1" == "--help" || "$1" == "-h" ]]; then
        # Лишние аргументы к help считаются CLI ошибкой.
        if (($# != 1)); then
            print_help >&2
            return 2
        fi
        print_help
        return 0
    fi
    # Устаревшее имя не должно сохранить опасную автоматическую запись baseline.
    if [[ "$1" == "baseline" ]]; then
        printf 'Ошибка: команда `baseline` заменена на явный `bootstrap [OUTPUT]`.\n' >&2
        printf 'Tracked coverage/baseline.json автоматически не изменяется.\n' >&2
        return 2
    fi
    # Check/report не принимают скрытых дополнительных параметров.
    if [[ "$1" == "check" || "$1" == "report" ]]; then
        if (($# != 1)); then
            print_help >&2
            return 2
        fi
    # Bootstrap принимает не более одного явного proposal path.
    elif [[ "$1" == "bootstrap" ]]; then
        if (($# > 2)); then
            print_help >&2
            return 2
        fi
    else
        # Неизвестный mode не запускает clean либо expensive suite.
        printf 'Ошибка: неизвестная coverage-команда `%s`.\n' "$1" >&2
        print_help >&2
        return 2
    fi
    # Относительные пути Cargo должны разрешаться одинаково локально и в CI.
    cd "${REPO_ROOT}"
    # Exact tools проверяются до clean/build во всех измерительных modes.
    require_coverage_tool || return 2
    # Check fail-fast валидирует v2 baseline и exceptions до дорогой suite.
    if [[ "$1" == "check" ]]; then
        validate_stable_check_inputs || return 2
    fi
    # Bootstrap является отдельной записывающей admin операцией.
    if [[ "$1" == "bootstrap" ]]; then
        # Default остаётся ignored proposal; явный путь проходит safety boundary.
        run_bootstrap "${2-${DEFAULT_BOOTSTRAP_OUTPUT}}"
        return $?
    fi
    # Check и report используют один owner build-once/three-run methodology.
    run_stable_coverage_suite || return 2
    # Legacy artifacts остаются обязательными и валидируемыми, но report-only.
    publish_legacy_diagnostics || return 2
    # Report намеренно не читает baseline и не применяет ratchet.
    if [[ "$1" == "report" ]]; then
        return 0
    fi
    # Единственный blocking ratchet возвращает frozen 0/1/2 semantics.
    run_stable_check
}

# Единственная точка входа передаёт исходный argv без преобразований.
main "$@"

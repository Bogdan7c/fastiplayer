#!/usr/bin/env bash
# Hermetic self-test проверяет CLI contract development-only yt-dlp runner-а.

# Strict mode не позволяет failed assertion потеряться между command substitutions.
set -Eeuo pipefail

# Корень repo вычисляется от расположения self-test, а не от caller working directory.
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
# Проверяемый runner всегда вызывается по exact absolute path.
readonly COMPATIBILITY_RUNNER="${REPO_ROOT}/scripts/ytdlp-compatibility.sh"
# Временный каталог изолирует fake executables и invocation evidence от repository tree.
temporary_directory="$(mktemp -d -t fastiplayer-ytdlp-compatibility-self-test.XXXXXX)"
# Fake PATH directory не может случайно перехватить команды вне этого self-test process tree.
readonly FAKE_BIN_DIRECTORY="${temporary_directory}/bin"
# Cargo argv записывается отдельно и затем проверяется как observable runner contract.
readonly CARGO_ARGUMENTS_LOG="${temporary_directory}/cargo-arguments.log"

# Cleanup удаляет только exact каталог, созданный текущим self-test process.
cleanup() {
    # Quoted path запрещает glob expansion и ошибочное удаление соседних файлов.
    rm -rf -- "${temporary_directory}"
}

# Trap выполняет cleanup и после success, и после failed assertion.
trap cleanup EXIT

# Assertion требует присутствия stable marker-а в captured output.
require_output() {
    # Полный captured output остаётся shell data.
    local output="$1"
    # Expected fragment задаётся каждым отдельным scenario.
    local expected_text="$2"
    # Substring comparison не зависит от Cargo formatting или terminal colors.
    if [[ "${output}" != *"${expected_text}"* ]]; then
        # Failure показывает и ожидание, и фактически полученный output.
        printf 'FAIL: ожидалась строка `%s`\n%s\n' "${expected_text}" "${output}" >&2
        # Self-test не продолжает последующие scenarios после нарушенного contract.
        exit 1
    fi
}

# Negative assertion запрещает ложный PASSED после failed Cargo test.
require_absent() {
    # Полный captured output остаётся shell data.
    local output="$1"
    # Forbidden fragment задаётся проверяемым outcome contract.
    local forbidden_text="$2"
    # Совпадение означает ложную положительную диагностику runner-а.
    if [[ "${output}" == *"${forbidden_text}"* ]]; then
        # Failure не печатает лишнее process окружение.
        printf 'FAIL: обнаружена запрещённая строка `%s`\n' "${forbidden_text}" >&2
        # Self-test завершается немедленно.
        exit 1
    fi
}

# Fake directory создаётся до executable scripts.
mkdir -p -- "${FAKE_BIN_DIRECTORY}"

# Fake yt-dlp реализует только diagnostic version command, которым владеет shell runner.
cat >"${FAKE_BIN_DIRECTORY}/yt-dlp" <<'EOF'
#!/usr/bin/env bash
# Неожиданный argv означает расширение runner contract без обновления self-test.
if [[ "$#" -ne 1 || "$1" != "--version" ]]; then
    printf 'unexpected fake yt-dlp arguments\n' >&2
    exit 97
fi
# Специальный env switch проверяет typed shell failure version probe-а.
if [[ "${FASTIPLAYER_FAKE_YTDLP_VERSION_FAILURE:-0}" == "1" ]]; then
    exit 23
fi
# Произвольная future version доказывает отсутствие version allowlist в runner-е.
printf '2099.12.31\n'
EOF

# Fake Cargo проверяет shell orchestration, не компилируя уже отдельно проверяемый Rust test.
cat >"${FAKE_BIN_DIRECTORY}/cargo" <<'EOF'
#!/usr/bin/env bash
# Exact argv сохраняется для assertions вызывающего self-test.
printf '%s\n' "$*" >"${FASTIPLAYER_FAKE_CARGO_ARGUMENTS_LOG}"
# Exit status моделирует success либо нарушенный Rust compatibility contract.
exit "${FASTIPLAYER_FAKE_CARGO_EXIT_CODE:-0}"
EOF

# Executable bits делают fake commands наблюдаемыми через обычный `command -v`.
chmod +x "${FAKE_BIN_DIRECTORY}/yt-dlp" "${FAKE_BIN_DIRECTORY}/cargo"

# Help не требует наличия yt-dlp и не заявляет compatibility PASS.
help_output="$(${COMPATIBILITY_RUNNER} --help 2>&1)"
# Help обязан явно описать development-only scope.
require_output "${help_output}" "development-only"
# Help не является результатом compatibility check.
require_absent "${help_output}" "PASSED: system yt-dlp compatibility"

# Неизвестный argument должен остановить runner до executable discovery.
if unknown_argument_output="$(${COMPATIBILITY_RUNNER} --unknown-option 2>&1)"; then
    # Успех неизвестного argument-а нарушил бы маленький CLI surface.
    printf 'FAIL: неизвестный argument завершился успешно\n' >&2
    # Self-test возвращает failure вызывающей проверке.
    exit 1
fi
# Stable reason позволяет отличить parser failure от Cargo failure.
require_output "${unknown_argument_output}" "unknown argument '--unknown-option'"

# Success scenario подменяет только два внешних executable, сохраняя настоящий runner code.
success_output="$(env \
    "PATH=${FAKE_BIN_DIRECTORY}:${PATH}" \
    "FASTIPLAYER_FAKE_CARGO_ARGUMENTS_LOG=${CARGO_ARGUMENTS_LOG}" \
    "FASTIPLAYER_FAKE_CARGO_EXIT_CODE=0" \
    "${COMPATIBILITY_RUNNER}" 2>&1)"
# Future version не блокируется номером и попадает только в diagnostic marker.
require_output "${success_output}" "version=2099.12.31"
# PASSED появляется только после успешного Cargo status.
require_output "${success_output}" "PASSED: system yt-dlp compatibility"
# Exact Cargo test target доказывает связь shell runner-а с real-system Rust boundary test.
require_output "$(<"${CARGO_ARGUMENTS_LOG}")" \
    "+1.96.0 test -p service-ytdlp --locked --test system_ytdlp_compatibility system_ytdlp_reaches_candidate_and_topology_boundaries -- --ignored --exact --nocapture"

# Cargo failure моделирует несовместимый system runtime либо rejected production output.
if cargo_failure_output="$(env \
    "PATH=${FAKE_BIN_DIRECTORY}:${PATH}" \
    "FASTIPLAYER_FAKE_CARGO_ARGUMENTS_LOG=${CARGO_ARGUMENTS_LOG}" \
    "FASTIPLAYER_FAKE_CARGO_EXIT_CODE=19" \
    "${COMPATIBILITY_RUNNER}" 2>&1)"; then
    # Runner не имеет права превращать failed Rust test в success.
    printf 'FAIL: failed Cargo compatibility test завершился успешно\n' >&2
    # Self-test возвращает failure.
    exit 1
fi
# Failure marker объясняет, какой boundary отверг runtime.
require_output "${cargo_failure_output}" \
    "FAILED: system yt-dlp compatibility; reason=candidate or topology production boundary rejected the executable"
# Failed scenario никогда не публикует итоговый PASSED.
require_absent "${cargo_failure_output}" "PASSED: system yt-dlp compatibility"

# Version command failure останавливает runner до Cargo invocation.
if version_failure_output="$(env \
    "PATH=${FAKE_BIN_DIRECTORY}:${PATH}" \
    "FASTIPLAYER_FAKE_CARGO_ARGUMENTS_LOG=${CARGO_ARGUMENTS_LOG}" \
    "FASTIPLAYER_FAKE_YTDLP_VERSION_FAILURE=1" \
    "${COMPATIBILITY_RUNNER}" 2>&1)"; then
    # Неработающий diagnostic command не должен считаться совместимым executable.
    printf 'FAIL: failed yt-dlp --version завершился успешно\n' >&2
    # Self-test возвращает failure.
    exit 1
fi
# Stable failure reason не зависит от произвольного stderr внешнего executable.
require_output "${version_failure_output}" "FAILED: system yt-dlp compatibility; reason=yt-dlp --version failed"

# Итоговый marker означает прохождение всех shell outcome scenarios.
printf 'PASS: yt-dlp compatibility runner self-tests\n'

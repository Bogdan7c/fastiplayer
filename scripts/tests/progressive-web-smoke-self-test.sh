#!/usr/bin/env bash
# Focused hermetic self-test explicit-URL parser-а и report redaction.

# Строгий режим завершает test на первом несоблюдённом privacy assertion.
set -Eeuo pipefail

# Repository root вычисляется от расположения test script-а.
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
# Тестируемый runner всегда вызывается по абсолютному path.
readonly PROGRESSIVE_WEB_SMOKE="${REPO_ROOT}/scripts/progressive-web-smoke.sh"
# Temporary root принадлежит только этому self-test process.
temporary_directory="$(mktemp -d -t rustiplayer-progressive-web-self-test.XXXXXX)"

# Cleanup удаляет только exact mktemp directory self-test-а.
cleanup() {
    # Directory создаётся до trap и всегда имеет explicit process-owned path.
    rm -rf -- "${temporary_directory}"
}

# EXIT trap сохраняет workspace чистым и при failed assertion.
trap cleanup EXIT

# Assertion проверяет наличие ожидаемого bounded marker-а.
require_output() {
    # Полный captured output передаётся первым аргументом.
    local output="$1"
    # Stable expected fragment передаётся вторым аргументом.
    local expected_text="$2"
    # Missing marker печатает безопасный captured output для диагностики.
    if [[ "${output}" != *"${expected_text}"* ]]; then
        printf 'FAIL: ожидалась строка `%s`\n%s\n' "${expected_text}" "${output}" >&2
        exit 1
    fi
}

# Assertion запрещает exact secret в terminal output или report.
require_absent() {
    # Проверяемый text передаётся первым аргументом.
    local output="$1"
    # Secret marker передаётся вторым аргументом только из hermetic fixture-а.
    local forbidden_text="$2"
    # Exact substring достаточно для доказательства literal redaction.
    if [[ "${output}" == *"${forbidden_text}"* ]]; then
        printf 'FAIL: обнаружен запрещённый secret marker\n' >&2
        exit 1
    fi
}

# Пустой invocation обязан быть NOT RUN без URL discovery.
missing_selection_output="$(${PROGRESSIVE_WEB_SMOKE} 2>&1)"
# Marker отличает пустой запуск от acceptance pass.
require_output "${missing_selection_output}" "NOT RUN: missing explicit"

# Positional URL запрещён, потому что user intent должен быть explicit --url.
if positional_output="$(${PROGRESSIVE_WEB_SMOKE} "https://example.invalid/video" 2>&1)"; then
    printf 'FAIL: positional URL завершился успешно\n' >&2
    exit 1
fi
# Parser diagnostic остаётся bounded и не повторяет входной URL.
require_output "${positional_output}" "URL передаётся только через --url"
# Даже parser failure не должен отражать raw URL.
require_absent "${positional_output}" "https://example.invalid/video"

# Local file scheme не входит в progressive HTTP(S) manual runner.
if invalid_scheme_output="$(${PROGRESSIVE_WEB_SMOKE} --url "file:///tmp/media.webm" --report "${temporary_directory}/invalid.txt" 2>&1)"; then
    printf 'FAIL: file URL завершился успешно\n' >&2
    exit 1
fi
# Typed CLI reason объясняет supported input boundary.
require_output "${invalid_scheme_output}" "explicit absolute HTTP(S) URL"

# Fake binary детерминированно печатает raw locator и representative auth material.
fake_binary="${temporary_directory}/fake-rustiplayer"
# Generated fixture создаётся без heredoc substitution и получает только test argv.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic fake печатает вход только для проверки redactor-а.'
    printf '%s\n' 'printf '\''opening %%s\n'\'' "$1"'
    printf '%s\n' 'printf '\''Authorization: Bearer runtime-secret\n'\'''
    printf '%s\n' 'printf '\''Cookie: session=runtime-cookie\n'\'''
    printf '%s\n' 'printf '\''Set-Cookie: refreshed=runtime-set-cookie\n'\'''
    printf '%s\n' 'printf '\''Header X-Custom-Token: runtime-custom-header\n'\'''
    printf '%s\n' 'printf '\''Extractor payload: runtime-extractor-payload\n'\'''
    printf '%s\n' 'printf '\''redirect=https://cdn.example.invalid/media?token=runtime-endpoint\n'\'''
} >"${fake_binary}"
# Fake должен быть executable для того же preflight-а, что и real binary.
chmod +x -- "${fake_binary}"

# Первый URL содержит userinfo/query/fragment, которые report никогда не сохраняет.
explicit_url_one="https://user:password@example.invalid/watch?token=first-secret#fragment"
# Второй URL доказывает repeatable explicit selection без default corpus-а.
explicit_url_two="http://media.example.invalid/video.webm?signature=second-secret"
# Report target новый и существует только внутри temporary directory.
report_path="${temporary_directory}/progressive-report.txt"
# Real self-test run использует fake binary и не выполняет network/GUI/build.
runner_output="$(${PROGRESSIVE_WEB_SMOKE} \
    --url "${explicit_url_one}" \
    --url "${explicit_url_two}" \
    --duration 1 \
    --binary "${fake_binary}" \
    --report "${report_path}" 2>&1)"
# Terminal output сообщает manual verdict без identities.
require_output "${runner_output}" "MANUAL REVIEW REQUIRED"
# Ни один exact URL не отражается в terminal output.
require_absent "${runner_output}" "${explicit_url_one}"
# Второй exact URL также не отражается в terminal output.
require_absent "${runner_output}" "${explicit_url_two}"

# Report читается только после успешного runner completion.
report_content="$(<"${report_path}")"
# Exact URLs отсутствуют даже при прямой печати fake binary.
require_absent "${report_content}" "${explicit_url_one}"
# Повторный URL получает тот же privacy contract.
require_absent "${report_content}" "${explicit_url_two}"
# Header value не сохраняется в sanitized report.
require_absent "${report_content}" "runtime-secret"
# Initial Cookie value не сохраняется в sanitized report.
require_absent "${report_content}" "runtime-cookie"
# Set-Cookie value не сохраняется в sanitized report.
require_absent "${report_content}" "runtime-set-cookie"
# Произвольный transport header вырезается целой строкой, а не только known auth names.
require_absent "${report_content}" "runtime-custom-header"
# Extractor payload marker не позволяет случайно сохранить structured response.
require_absent "${report_content}" "runtime-extractor-payload"
# Endpoint из runtime log также вырезается, даже если он не совпадает с input URL.
require_absent "${report_content}" "runtime-endpoint"
# Exact URL replacement остаётся видимым safe marker-ом.
require_output "${report_content}" "<redacted-url>"
# Secret-bearing lines заменяются целиком.
require_output "${report_content}" "<redacted-secret-line>"
# Report не выдаёт runtime launch за автоматический UX pass.
require_output "${report_content}" "MANUAL REVIEW REQUIRED"
# Count доказывает обработку обоих и только явно переданных URL.
require_output "${report_content}" "Explicit URL count: 2"

# Existing report target не должен быть молча overwritten вторым run-ом.
if overwrite_output="$(${PROGRESSIVE_WEB_SMOKE} \
    --url "${explicit_url_one}" \
    --binary "${fake_binary}" \
    --report "${report_path}" 2>&1)"; then
    printf 'FAIL: existing report был перезаписан\n' >&2
    exit 1
fi
# Failure точно называет safe overwrite policy.
require_output "${overwrite_output}" "runner не перезаписывает artifacts"
# Overwrite diagnostic не отражает exact URL.
require_absent "${overwrite_output}" "${explicit_url_one}"

# Dry-run target остаётся новым, но file создаваться не должен.
dry_report_path="${temporary_directory}/dry-report.txt"
# Dry-run обрабатывает selection без binary, network или report write.
dry_run_output="$(${PROGRESSIVE_WEB_SMOKE} \
    --url "${explicit_url_one}" \
    --report "${dry_report_path}" \
    --dry-run 2>&1)"
# Plan показывает только redacted placeholder.
require_output "${dry_run_output}" "<redacted-explicit-url>"
# Plan не отражает raw identity.
require_absent "${dry_run_output}" "${explicit_url_one}"
# Dry-run явно не является acceptance pass.
require_output "${dry_run_output}" "NOT RUN: dry-run only"
# Dry-run не создаёт даже пустой report artifact.
if [[ -e "${dry_report_path}" ]]; then
    printf 'FAIL: dry-run создал report artifact\n' >&2
    exit 1
fi

# Итоговый marker означает, что parser/redaction assertions выполнены.
printf 'PASS: progressive web smoke script self-tests\n'

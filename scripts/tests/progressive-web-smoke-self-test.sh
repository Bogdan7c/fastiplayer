#!/usr/bin/env bash
# Hermetic self-test S42 parser-а, provenance gate, matrix status и report redaction.

# Строгий режим завершает test на первом нарушенном invariant-е.
set -Eeuo pipefail

# Repository root вычисляется от расположения test script-а.
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
# Public runner вызывается по absolute path.
readonly PROGRESSIVE_WEB_SMOKE="${REPO_ROOT}/scripts/progressive-web-smoke.sh"
# S42 owner module проверяется отдельно и затем используется как источник allowlist.
readonly S42_LIBRARY="${REPO_ROOT}/scripts/lib/progressive-web-smoke-s42.sh"
# Temporary root принадлежит только этому process.
temporary_directory="$(mktemp -d -t rustiplayer-progressive-web-self-test.XXXXXX)"

# Cleanup удаляет только exact mktemp directory.
cleanup() {
    # Path создан самим self-test до установки trap.
    rm -rf -- "${temporary_directory}"
}

# EXIT trap сохраняет workspace чистым и при failed assertion.
trap cleanup EXIT

# Assertion проверяет наличие bounded marker-а.
require_output() {
    # Captured text передаётся первым аргументом.
    local output="$1"
    # Expected fragment передаётся вторым аргументом.
    local expected_text="$2"
    # Missing marker публикует безопасный captured output.
    if [[ "${output}" != *"${expected_text}"* ]]; then
        printf 'FAIL: ожидалась строка `%s`\n%s\n' "${expected_text}" "${output}" >&2
        exit 1
    fi
}

# Assertion запрещает exact secret в terminal output или report.
require_absent() {
    # Проверяемый text передаётся первым аргументом.
    local output="$1"
    # Forbidden marker приходит только из hermetic fixture-а.
    local forbidden_text="$2"
    # Exact substring доказывает literal leakage.
    if [[ "${output}" == *"${forbidden_text}"* ]]; then
        printf 'FAIL: обнаружен запрещённый secret marker\n' >&2
        exit 1
    fi
}

# Оба shell-модуля обязаны проходить parser до behavioral tests.
bash -n "${PROGRESSIVE_WEB_SMOKE}" "${S42_LIBRARY}"

# Owner module fail-closed при ошибочном direct execution.
if direct_library_output="$(bash "${S42_LIBRARY}" 2>&1)"; then
    printf 'FAIL: S42 owner module разрешил direct execution\n' >&2
    exit 1
fi
# Diagnostic раскрывает только назначение module-а.
require_output "${direct_library_output}" "не является самостоятельной командой"

# Source даёт self-test-у тот же readonly allowlist, что использует runner.
source "${S42_LIBRARY}"

# Пустой invocation обязан быть NOT RUN без input discovery.
missing_selection_output="$("${PROGRESSIVE_WEB_SMOKE}" 2>&1)"
# Marker отличает пустой запуск от acceptance pass.
require_output "${missing_selection_output}" "NOT RUN: missing explicit"

# Positional URL запрещён: intent выражается только named option-ом.
if positional_output="$("${PROGRESSIVE_WEB_SMOKE}" "https://example.invalid/video" 2>&1)"; then
    printf 'FAIL: positional URL завершился успешно\n' >&2
    exit 1
fi
# Parser diagnostic не повторяет входной URL.
require_output "${positional_output}" "input передаётся только через --url/--fixture"
# Даже parser failure не отражает raw identity.
require_absent "${positional_output}" "https://example.invalid/video"

# Local file scheme не входит в approved network boundary.
if invalid_scheme_output="$("${PROGRESSIVE_WEB_SMOKE}" \
    --url "file:///tmp/media.webm" \
    --report "${temporary_directory}/invalid.txt" 2>&1)"; then
    printf 'FAIL: file URL завершился успешно\n' >&2
    exit 1
fi
# Typed CLI reason перечисляет exact admitted schemes.
require_output "${invalid_scheme_output}" "HTTP/HTTPS/FTP/FTPS URL"

# FTP row не может быть закрыта HTTP locator-ом.
if invalid_ftp_case_output="$("${PROGRESSIVE_WEB_SMOKE}" \
    --case ftp-ftps-progressive \
    --url "https://example.invalid/not-ftp" \
    --report "${temporary_directory}/invalid-ftp.txt" 2>&1)"; then
    printf 'FAIL: FTP case принял HTTP URL\n' >&2
    exit 1
fi
# Failure остаётся typed и не повторяет raw URL.
require_output "${invalid_ftp_case_output}" "требует explicit FTP/FTPS URL"
require_absent "${invalid_ftp_case_output}" "https://example.invalid/not-ftp"

# Fake tools directory моделирует exact pinned system yt-dlp.
fake_tools_directory="${temporary_directory}/fake-tools"
# Explicit directory создаётся только под process-owned temporary root.
mkdir -p -- "${fake_tools_directory}"
# Fake yt-dlp возвращает ровно утверждённый release независимо от probe flags.
fake_ytdlp="${fake_tools_directory}/yt-dlp"
# Generated executable не читает config/network.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic provenance fixture.'
    printf '%s\n' 'if [[ -n "${RUSTIPLAYER_SELFTEST_COLLISION_PATH:-}" ]]; then'
    printf '%s\n' '    printf '\''competitor-owned\n'\'' >"${RUSTIPLAYER_SELFTEST_COLLISION_PATH}"'
    printf '%s\n' 'fi'
    printf '%s\n' 'printf '\''2026.07.04\n'\'''
} >"${fake_ytdlp}"
# PATH lookup требует executable bit.
chmod +x -- "${fake_ytdlp}"

# Fake app печатает input и representative secrets только для redactor test-а.
fake_binary="${temporary_directory}/fake-rustiplayer"
# Generated executable получает raw input как первый argv.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic runtime fixture.'
    printf '%s\n' 'printf '\''opening %s\n'\'' "$1"'
    printf '%s\n' 'printf '\''Authorization: Bearer runtime-secret\n'\'''
    printf '%s\n' 'printf '\''Cookie: session=runtime-cookie\n'\'''
    printf '%s\n' 'printf '\''Set-Cookie: refreshed=runtime-set-cookie\n'\'''
    printf '%s\n' 'printf '\''Header X-Custom-Token: runtime-custom-header\n'\'''
    printf '%s\n' 'printf '\''Extractor payload: runtime-extractor-payload\n'\'''
    printf '%s\n' 'printf '\''redirect=https://cdn.example.invalid/media?opaque=runtime-endpoint\n'\'''
    printf '%s\n' 'printf '\''mirror=ftps://ftp-user:ftp-password@ftp.example.invalid/private.bin\n'\'''
} >"${fake_binary}"
# Runner preflight принимает только executable regular file.
chmod +x -- "${fake_binary}"
# Self-test вычисляет independently expected exact runtime executable identity.
fake_binary_sha256_output="$(sha256sum -- "${fake_binary}")"
# Первый field сравнивается с report без раскрытия temporary path.
fake_binary_sha256="${fake_binary_sha256_output%% *}"
# Current repository state нужен для exact clean/dirty report assertion.
if [[ -z "$(git -C "${REPO_ROOT}" status --porcelain=v1 --untracked-files=normal)" ]]; then
    expected_workspace_state="clean"
else
    expected_workspace_state="dirty"
fi

# Backward-compatible URL one содержит userinfo/query/fragment.
explicit_url_one="https://user:password@example.invalid/watch?opaque=first-value#fragment"
# Второй legacy URL проверяет repeatable selection.
explicit_url_two="http://media.example.invalid/video.webm?opaque=second-value"
# Report target новый и process-owned.
legacy_report_path="${temporary_directory}/legacy-report.txt"
# Real hermetic run использует fake binary и pinned fake yt-dlp.
legacy_runner_output="$(PATH="${fake_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
    --url "${explicit_url_one}" \
    --url "${explicit_url_two}" \
    --duration 1 \
    --binary "${fake_binary}" \
    --report "${legacy_report_path}" 2>&1)"
# Terminal сообщает manual verdict без identities.
require_output "${legacy_runner_output}" "MANUAL REVIEW REQUIRED"
# Exact inputs не отражаются в terminal.
require_absent "${legacy_runner_output}" "${explicit_url_one}"
require_absent "${legacy_runner_output}" "${explicit_url_two}"

# Report читается только после successful runner completion.
legacy_report_content="$(<"${legacy_report_path}")"
# Raw identities отсутствуют даже при прямой печати fake app.
require_absent "${legacy_report_content}" "${explicit_url_one}"
require_absent "${legacy_report_content}" "${explicit_url_two}"
# Representative secrets и derived endpoints не сохраняются.
require_absent "${legacy_report_content}" "runtime-secret"
require_absent "${legacy_report_content}" "runtime-cookie"
require_absent "${legacy_report_content}" "runtime-set-cookie"
require_absent "${legacy_report_content}" "runtime-custom-header"
require_absent "${legacy_report_content}" "runtime-extractor-payload"
require_absent "${legacy_report_content}" "runtime-endpoint"
require_absent "${legacy_report_content}" "ftp-password"
# Redaction остаётся visible safe evidence.
require_output "${legacy_report_content}" "<redacted-input>"
require_output "${legacy_report_content}" "<redacted-secret-line>"
require_output "${legacy_report_content}" "<redacted-url>"
# Legacy URL mapping сохраняет обратную совместимость, но не закрывает S42.
require_output "${legacy_report_content}" '`legacy-url-1`'
require_output "${legacy_report_content}" '`legacy-url-2`'
require_output "${legacy_report_content}" "S42 matrix status: NOT RUN"
require_output "${legacy_report_content}" "Selected case count: 2"
require_output "${legacy_report_content}" "Missing required case count: 29"
require_output "${legacy_report_content}" "Outcome: MANUAL REVIEW REQUIRED"
# Provenance фиксирует exact approved release.
require_output "${legacy_report_content}" 'System yt-dlp version: `2026.07.04`'
# Automatic PASS отсутствует при любом runtime launch.
require_absent "${legacy_report_content}" "Outcome: PASS"

# Existing report target нельзя overwrite вторым run-ом.
if overwrite_output="$(PATH="${fake_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
    --url "${explicit_url_one}" \
    --binary "${fake_binary}" \
    --report "${legacy_report_path}" 2>&1)"; then
    printf 'FAIL: existing report был перезаписан\n' >&2
    exit 1
fi
# Failure называет safe overwrite policy и скрывает raw URL.
require_output "${overwrite_output}" "runner не перезаписывает artifacts"
require_absent "${overwrite_output}" "${explicit_url_one}"

# Dry-run target остаётся новым, но file не создаётся.
dry_report_path="${temporary_directory}/dry-report.txt"
# Named dry-run проверяет selection без tools/build/runtime.
dry_run_output="$("${PROGRESSIVE_WEB_SMOKE}" \
    --case public-single \
    --url "${explicit_url_one}" \
    --report "${dry_report_path}" \
    --dry-run 2>&1)"
# Plan показывает только safe ID/kind и placeholder.
require_output "${dry_run_output}" "case public-single (url): <redacted-explicit-input>"
require_absent "${dry_run_output}" "${explicit_url_one}"
require_output "${dry_run_output}" "NOT RUN: dry-run only"
# Dry-run не создаёт artifact.
if [[ -e "${dry_report_path}" ]]; then
    printf 'FAIL: dry-run создал report artifact\n' >&2
    exit 1
fi

# Version mismatch моделируется отдельным PATH prefix.
mismatched_tools_directory="${temporary_directory}/mismatched-tools"
# Directory ограничен temporary root.
mkdir -p -- "${mismatched_tools_directory}"
# Mismatched executable возвращает соседний release.
mismatched_ytdlp="${mismatched_tools_directory}/yt-dlp"
# Fixture не выполняет network/config.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic mismatched provenance fixture.'
    printf '%s\n' 'printf '\''2026.07.03\n'\'''
} >"${mismatched_ytdlp}"
# PATH lookup требует executable bit.
chmod +x -- "${mismatched_ytdlp}"
# Mismatch report не должен появиться даже с valid selected case.
mismatch_report_path="${temporary_directory}/mismatch-report.txt"
# Real acceptance fail-closed до runtime/report creation.
if mismatch_output="$(PATH="${mismatched_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
    --case public-single \
    --url "${explicit_url_one}" \
    --binary "${fake_binary}" \
    --report "${mismatch_report_path}" 2>&1)"; then
    printf 'FAIL: mismatched yt-dlp был принят\n' >&2
    exit 1
fi
# Diagnostic называет approved release, но не input.
require_output "${mismatch_output}" "не совпадает с утверждённым 2026.07.04 profile"
require_absent "${mismatch_output}" "${explicit_url_one}"
# Failed provenance не оставляет misleading report.
if [[ -e "${mismatch_report_path}" ]]; then
    printf 'FAIL: version mismatch создал report artifact\n' >&2
    exit 1
fi

# Competing process моделируется после report preflight внутри yt-dlp provenance probe.
collision_report_path="${temporary_directory}/collision-report.txt"
# Exclusive create обязан завершить runner failure и сохранить чужой artifact.
if collision_output="$(
    RUSTIPLAYER_SELFTEST_COLLISION_PATH="${collision_report_path}" \
        PATH="${fake_tools_directory}:${PATH}" \
        "${PROGRESSIVE_WEB_SMOKE}" \
        --case public-single \
        --url "${explicit_url_one}" \
        --binary "${fake_binary}" \
        --report "${collision_report_path}" 2>&1
)"; then
    printf 'FAIL: report collision завершился успешно\n' >&2
    exit 1
fi
# Failure остаётся secret-safe и объясняет atomic non-overwrite boundary.
require_output "${collision_output}" "atomically создать новый --report"
require_absent "${collision_output}" "${collision_report_path}"
require_absent "${collision_output}" "${explicit_url_one}"
# Exact competing bytes доказывают отсутствие truncate/overwrite.
collision_report_content="$(<"${collision_report_path}")"
if [[ "${collision_report_content}" != "competitor-owned" ]]; then
    printf 'FAIL: competing report artifact был изменён\n' >&2
    exit 1
fi

# Failing app проверяет honest report outcome после successful provenance.
failing_binary="${temporary_directory}/failing-rustiplayer"
# Fixture печатает raw input, затем возвращает unexpected status.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic runtime failure fixture.'
    printf '%s\n' 'printf '\''failed opening %s\n'\'' "$1"'
    printf '%s\n' 'exit 9'
} >"${failing_binary}"
# Runner preflight требует executable bit.
chmod +x -- "${failing_binary}"
# Новый report path позволяет проверить failure artifact.
failure_report_path="${temporary_directory}/failure-report.txt"
# Runtime failure обязан вернуть non-zero.
if failure_output="$(PATH="${fake_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
    --case public-single \
    --url "${explicit_url_one}" \
    --binary "${failing_binary}" \
    --report "${failure_report_path}" 2>&1)"; then
    printf 'FAIL: runtime failure завершился успешно\n' >&2
    exit 1
fi
# Terminal и sanitized report публикуют FAIL без raw input.
require_output "${failure_output}" "FAIL: runtime error"
require_absent "${failure_output}" "${explicit_url_one}"
failure_report_content="$(<"${failure_report_path}")"
require_output "${failure_report_content}" "Outcome: FAIL"
require_output "${failure_report_content}" "S42 matrix status: NOT RUN"
require_absent "${failure_report_content}" "${explicit_url_one}"

# SIGKILL fixture доказывает, что status 137 не выдаётся за graceful timebox.
sigkill_binary="${temporary_directory}/sigkill-rustiplayer"
# Generated process завершает себя SIGKILL без network/GUI ожидания.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic forced-kill fixture.'
    printf '%s\n' 'kill -KILL "$$"'
} >"${sigkill_binary}"
# Runner preflight требует executable bit.
chmod +x -- "${sigkill_binary}"
# Отдельный report не смешивает shutdown failure с предыдущими fixtures.
sigkill_report_path="${temporary_directory}/sigkill-report.txt"
# Status 137 обязан превратить aggregate manual outcome в failure.
if PATH="${fake_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
    --url "https://example.invalid/forced-kill" \
    --duration 1 \
    --binary "${sigkill_binary}" \
    --report "${sigkill_report_path}" >/dev/null 2>&1; then
    printf 'FAIL: runtime status 137 был принят как bounded shutdown\n' >&2
    exit 1
fi
# Sanitized report сохраняет exact process status без raw URL.
sigkill_report_content="$(<"${sigkill_report_path}")"
require_output "${sigkill_report_content}" "Runtime exit status: 137"
require_output "${sigkill_report_content}" "Outcome: FAIL"
require_absent "${sigkill_report_content}" "forced-kill"

# Fixture с relative path и пробелами проверяет canonical/URI-derived redaction.
relative_fixture_directory="${temporary_directory}/relative fixtures"
# Parent создаётся только внутри process-owned temporary directory.
mkdir -p -- "${relative_fixture_directory}"
# Exact fixture name содержит пробел и не должен пережить sanitized report.
relative_fixture_path="${relative_fixture_directory}/private playlist.m3u8"
# Минимальный readable playlist проходит local fixture preflight.
printf '%s\n' '#EXTM3U' >"${relative_fixture_path}"
# Fake app печатает canonical, file-URI и standalone percent-encoded формы.
fixture_echo_binary="${temporary_directory}/fixture-echo-rustiplayer"
# Script fixture не выполняет filesystem/network side effects.
{
    printf '%s\n' '#!/usr/bin/env bash'
    printf '%s\n' '# Hermetic fixture identity echo.'
    printf '%s\n' 'encoded_fixture="${1// /%20}"'
    printf '%s\n' 'fixture_basename="${1##*/}"'
    printf '%s\n' 'encoded_basename="${fixture_basename// /%20}"'
    printf '%s\n' 'printf '\''canonical=%s\nfile-uri=file://%s\nencoded=%s\nbasename=%s\nencoded-basename=%s\n'\'' "$1" "${encoded_fixture}" "${encoded_fixture}" "${fixture_basename}" "${encoded_basename}"'
} >"${fixture_echo_binary}"
# Runner preflight требует executable bit.
chmod +x -- "${fixture_echo_binary}"
# Отдельный report сохраняет только sanitized representations.
relative_fixture_report="${temporary_directory}/relative-fixture-report.txt"
# Caller cwd делает переданный fixture path намеренно relative.
relative_fixture_output="$(
    cd -- "${temporary_directory}"
    PATH="${fake_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
        --case playlist-m3u8 \
        --fixture "relative fixtures/private playlist.m3u8" \
        --binary "${fixture_echo_binary}" \
        --report "${relative_fixture_report}" 2>&1
)"
# Terminal не раскрывает ни relative, ни canonical identity.
require_absent "${relative_fixture_output}" "relative fixtures/private playlist.m3u8"
require_absent "${relative_fixture_output}" "${relative_fixture_path}"
# Report обязан удалить exact canonical и derived percent-encoded формы.
relative_fixture_report_content="$(<"${relative_fixture_report}")"
percent_encoded_fixture_path="${relative_fixture_path// /%20}"
require_output "${relative_fixture_report_content}" "<redacted-fixture"
require_absent "${relative_fixture_report_content}" "${relative_fixture_path}"
require_absent "${relative_fixture_report_content}" "${percent_encoded_fixture_path}"
require_absent "${relative_fixture_report_content}" "relative%20fixtures"
require_absent "${relative_fixture_report_content}" "private playlist.m3u8"
require_absent "${relative_fixture_report_content}" "private%20playlist.m3u8"

# Local playlist fixture проверяет fixture path redaction.
explicit_fixture="${temporary_directory}/private-playlist.m3u8"
# Content не является media corpus и нужен только readable-file preflight-у.
printf '%s\n' '#EXTM3U' >"${explicit_fixture}"
# Complete args строятся из production allowlist без копии matrix в test-е.
complete_matrix_arguments=()
# Каждый safe case получает только explicit hermetic input.
for required_case_id in "${REQUIRED_S42_CASE_IDS[@]}"; do
    # Fixture-only roles получают owner-selected local file.
    if [[ "$(case_input_kind "${required_case_id}")" == "fixture" ]]; then
        complete_matrix_arguments+=(--case "${required_case_id}" --fixture "${explicit_fixture}")
        continue
    fi
    # FTP row получает exact FTP(S) family.
    if [[ "${required_case_id}" == "ftp-ftps-progressive" ]]; then
        complete_matrix_arguments+=(--case "${required_case_id}" --url "ftps://ftp-user:fixture-password@ftp.example.invalid/media.bin")
        continue
    fi
    # Остальные URL rows получают explicit HTTPS locator.
    complete_matrix_arguments+=(--case "${required_case_id}" --url "https://example.invalid/manual-fixture?opaque=matrix-value")
done

# Complete selection report остаётся manual-only.
complete_report_path="${temporary_directory}/complete-report.txt"
# Fake app быстро выполняет все safe roles без network/GUI.
complete_runner_output="$(PATH="${fake_tools_directory}:${PATH}" "${PROGRESSIVE_WEB_SMOKE}" \
    "${complete_matrix_arguments[@]}" \
    --duration 1 \
    --binary "${fake_binary}" \
    --report "${complete_report_path}" 2>&1)"
# Terminal verdict не становится PASS.
require_output "${complete_runner_output}" "MANUAL REVIEW REQUIRED"
require_absent "${complete_runner_output}" "PASS"

# Complete report проверяет matrix/provenance/checklist invariants.
complete_report_content="$(<"${complete_report_path}")"
require_output "${complete_report_content}" "S42 matrix status: MANUAL REVIEW REQUIRED"
require_output "${complete_report_content}" "Selected case count: 29"
require_output "${complete_report_content}" "Missing required case count: 0"
require_output "${complete_report_content}" "Outcome: MANUAL REVIEW REQUIRED"
require_output "${complete_report_content}" 'Compatibility profile ID: `yt-dlp-2026.07.04-serializable-v1`'
require_output "${complete_report_content}" "Workspace state: \`${expected_workspace_state}\`"
require_output "${complete_report_content}" 'Rustiplayer binary origin: `explicit-external-prebuilt`'
require_output "${complete_report_content}" "Rustiplayer binary SHA-256: \`${fake_binary_sha256}\`"
require_output "${complete_report_content}" "workspace HEAD is not asserted as its source"
require_output "${complete_report_content}" 'RTMP family: `ProfileExcluded`'
require_output "${complete_report_content}" 'HDS live/DVR: `NoApprovedRow`'
require_output "${complete_report_content}" "H.264 Baseline 8-bit YUV420/NV12"
require_output "${complete_report_content}" "current manual rerun NOT RUN"
require_output "${complete_report_content}" "owner has no compatible VA-API device"
require_output "${complete_report_content}" "failed pre-barrier quality/component switch preserves current playback"
# Complete selection никогда не автоматизирует human checkboxes.
require_output "${complete_report_content}" "- [ ] M3U8 import"
require_absent "${complete_report_content}" "Outcome: PASS"
# Raw fixture/URL/FTP credential не сохраняются.
require_absent "${complete_report_content}" "${explicit_fixture}"
require_absent "${complete_report_content}" "manual-fixture"
require_absent "${complete_report_content}" "fixture-password"

# Итоговый marker означает, что parser/provenance/privacy/matrix assertions выполнены.
printf 'PASS: progressive web smoke script self-tests\n'

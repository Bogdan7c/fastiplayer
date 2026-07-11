#!/usr/bin/env bash
# Неграфические self-tests CLI parser-а и current config generation.

set -Eeuo pipefail

# Корень repo вычисляется от расположения этого test script-а.
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
# Тестируемый runner всегда вызывается по абсолютному пути.
readonly PLAYBACK_SMOKE="${REPO_ROOT}/scripts/playback-smoke.sh"
# Временные файлы принадлежат только этому self-test run.
temporary_directory="$(mktemp -d -t rustiplayer-smoke-self-test.XXXXXX)"

# Cleanup удаляет только созданный self-test-ом temporary directory.
cleanup() {
    rm -rf -- "${temporary_directory}"
}

# Trap сохраняет чистоту рабочего дерева и при failed assertion.
trap cleanup EXIT

# Простая assertion показывает ожидаемую строку и полный captured output.
require_output() {
    local output="$1"
    local expected_text="$2"
    if [[ "${output}" != *"${expected_text}"* ]]; then
        printf 'FAIL: ожидалась строка `%s`\n%s\n' "${expected_text}" "${output}" >&2
        exit 1
    fi
}

# Пустой invocation является NOT RUN, а не ложным acceptance pass.
missing_selection_output="$(${PLAYBACK_SMOKE} 2>&1)"
require_output "${missing_selection_output}" "NOT RUN: missing selection"

# Неизвестный аргумент обязан завершиться ошибкой parser-а.
if unknown_output="$(${PLAYBACK_SMOKE} --unknown-option 2>&1)"; then
    printf 'FAIL: неизвестный аргумент завершился успешно\n' >&2
    exit 1
fi
require_output "${unknown_output}" "неизвестный аргумент"

# Пустые fixture-файлы достаточны: dry-run не читает media и не запускает GUI.
touch "${temporary_directory}/vp9.mp4" "${temporary_directory}/av1.mp4" "${temporary_directory}/h264.mp4"
# Dry-run проверяет полный parser path и описывает current config contract.
dry_run_output="$(${PLAYBACK_SMOKE} --mode full --dry-run --duration 1 \
    --vp9 "${temporary_directory}/vp9.mp4" \
    --av1 "${temporary_directory}/av1.mp4" \
    --h264 "${temporary_directory}/h264.mp4" 2>&1)"
require_output "${dry_run_output}" "schema v5"
require_output "${dry_run_output}" 'youtube.hdr_selection = "sdr_only"'
require_output "${dry_run_output}" "cargo build --release -p app-egui"

# Config helper создаёт полный current-schema TOML без GUI.
current_config_path="${temporary_directory}/current-config.toml"
cargo run --quiet --locked -p rustiplayer-config --example smoke_config -- \
    generate-current "${current_config_path}" software
# Ключи доказывают current schema, playback overrides и Session 16 HDR default.
grep -Fqx 'schema_version = 5' "${current_config_path}"
grep -Fqx 'start_paused = false' "${current_config_path}"
grep -Fqx 'preferred_backend = "software"' "${current_config_path}"
grep -Fqx 'hdr_selection = "sdr_only"' "${current_config_path}"
# Production loader подтверждает parse без запуска приложения.
cargo run --quiet --locked -p rustiplayer-config --example smoke_config -- \
    parse-current "${current_config_path}"

# Manifest без suite явно сообщает NOT RUN.
manifest_not_run_output="$("${REPO_ROOT}/scripts/runtime-acceptance.sh" 2>&1)"
require_output "${manifest_not_run_output}" "NOT RUN: missing --suite selection"

# Manifest dry-run не требует реальных fixtures/runtime и не заявляет PASS.
manifest_dry_run_output="$("${REPO_ROOT}/scripts/runtime-acceptance.sh" \
    --suite playback-matrix --dry-run 2>&1)"
require_output "${manifest_dry_run_output}" "NOT RUN: dry-run only"

# Выбранная runtime suite без fixture должна дать reasoned SKIP и специальный exit code.
set +e
manifest_skip_output="$("${REPO_ROOT}/scripts/runtime-acceptance.sh" \
    --suite runtime-software 2>&1)"
manifest_skip_status=$?
set -e
if [[ "${manifest_skip_status}" -ne 3 ]]; then
    printf 'FAIL: ожидаемый SKIP exit code 3, получен %s\n' "${manifest_skip_status}" >&2
    exit 1
fi
require_output "${manifest_skip_output}" "SKIP: не указан --vp9 local asset path"

# Итоговый marker означает, что все неграфические assertions выполнены.
printf 'PASS: playback smoke script self-tests\n'

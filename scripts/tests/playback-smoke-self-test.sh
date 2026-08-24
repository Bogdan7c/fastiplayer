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

# Negative assertion защищает policy от запрещённого status/marker-а.
require_absent() {
    local output="$1"
    local forbidden_text="$2"
    if [[ "${output}" == *"${forbidden_text}"* ]]; then
        printf 'FAIL: обнаружена запрещённая строка `%s`\n' "${forbidden_text}" >&2
        exit 1
    fi
}

# Source-level ratchet запрещает снова считать kill-after/SIGKILL штатным timeout-ом.
playback_smoke_source="$(<"${PLAYBACK_SMOKE}")"
require_output "${playback_smoke_source}" "0 | 124)"
require_absent "${playback_smoke_source}" "0 | 124 | 137)"

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
require_output "${dry_run_output}" "schema v8"
require_output "${dry_run_output}" 'yt_dlp.hdr_selection = "sdr_only"'
require_output "${dry_run_output}" "cargo build --release -p app-egui"
# Full dry-run переиспользует probe workflow, но обязан маркировать его только как план.
require_output "${dry_run_output}" "DRY-RUN: WOULD RUN FFmpeg runtime probe acceptance; no checks were executed"
require_absent "${dry_run_output}" "PASS: FFmpeg runtime probe acceptance"

# Прямой probe-only dry-run должен завершиться успешно без запуска обеих Cargo-проверок.
probe_dry_run_output="$(${PLAYBACK_SMOKE} --mode probe-only --dry-run 2>&1)"
# Вывод команды доказывает, что пользователь видит конкретный план probe workflow.
require_output "${probe_dry_run_output}" "cargo test -p video-ffmpeg --features ffmpeg probe::tests"
# Outcome явно отделяет запланированный probe от реально выполненного acceptance.
require_output "${probe_dry_run_output}" "DRY-RUN: WOULD RUN FFmpeg runtime probe acceptance; no checks were executed"
# Главный regression invariant запрещает production PASS в direct dry-run режиме.
require_absent "${probe_dry_run_output}" "PASS: FFmpeg runtime probe acceptance"

# Прямой legacy-migration dry-run также должен завершиться успешно без запуска Cargo.
legacy_dry_run_output="$(${PLAYBACK_SMOKE} --mode legacy-migration --dry-run 2>&1)"
# Вывод команды сохраняет полезность dry-run как проверяемого плана запуска.
require_output "${legacy_dry_run_output}" "cargo test -p rustiplayer-config --locked legacy_"
# Outcome явно сообщает, что legacy migration только была бы запущена.
require_output "${legacy_dry_run_output}" "DRY-RUN: WOULD RUN explicitly selected legacy config migration smoke; no checks were executed"
# Главный regression invariant запрещает тот же PASS, который выдаёт реальный успешный smoke.
require_absent "${legacy_dry_run_output}" "PASS: explicitly selected legacy config migration smoke"

# Временный Cargo shim позволяет функционально проверить success/failure orchestration без тяжёлой сборки.
cargo_shim_directory="${temporary_directory}/cargo-shim"
# Отдельный каталог не вмешивается в настоящий Cargo, используемый ниже для config integration test.
mkdir -p "${cargo_shim_directory}"
# Shim записывает каждый реальный вызов и возвращает управляемый self-test-ом exit code.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'set -Eeuo pipefail' \
    'printf '\''%s\n'\'' "$*" >> "${RUSTIPLAYER_SMOKE_SELF_TEST_CARGO_LOG:?}"' \
    'exit "${RUSTIPLAYER_SMOKE_SELF_TEST_CARGO_EXIT_CODE:-0}"' \
    >"${cargo_shim_directory}/cargo"
# Исполняемый бит делает shim полноценной process boundary заменой Cargo.
chmod +x "${cargo_shim_directory}/cargo"
# Один log принадлежит успешному probe-only workflow.
successful_probe_cargo_log="${temporary_directory}/successful-probe-cargo.log"
# Нулевой exit code shim-а позволяет обеим probe-командам реально завершиться успешно.
successful_probe_output="$(PATH="${cargo_shim_directory}:${PATH}" \
    RUSTIPLAYER_SMOKE_SELF_TEST_CARGO_LOG="${successful_probe_cargo_log}" \
    RUSTIPLAYER_SMOKE_SELF_TEST_CARGO_EXIT_CODE=0 \
    "${PLAYBACK_SMOKE}" --mode probe-only 2>&1)"
# Production PASS допустим только после двух успешных process boundary вызовов.
require_output "${successful_probe_output}" "PASS: FFmpeg runtime probe acceptance"
# Ровно две строки доказывают выполнение unit/fake и installed-runtime probe steps.
if [[ "$(wc -l <"${successful_probe_cargo_log}")" -ne 2 ]]; then
    printf 'FAIL: успешный probe-only должен вызвать Cargo ровно два раза\n' >&2
    exit 1
fi

# Отдельный log принадлежит намеренно падающему probe-only workflow.
failing_probe_cargo_log="${temporary_directory}/failing-probe-cargo.log"
# Errexit тестируемого runner-а должен сохранить ненулевой status первой упавшей команды.
set +e
# Управляемый exit 17 моделирует реальную ошибку Cargo без зависимости от host runtime.
failing_probe_output="$(PATH="${cargo_shim_directory}:${PATH}" \
    RUSTIPLAYER_SMOKE_SELF_TEST_CARGO_LOG="${failing_probe_cargo_log}" \
    RUSTIPLAYER_SMOKE_SELF_TEST_CARGO_EXIT_CODE=17 \
    "${PLAYBACK_SMOKE}" --mode probe-only 2>&1)"
# Код процесса сохраняется до возврата self-test-а в fail-fast режим.
failing_probe_status=$?
# Остальные assertions снова должны завершать self-test немедленно.
set -e
# Runner не имеет права превращать ошибку Cargo в успешный exit code.
if [[ "${failing_probe_status}" -ne 17 ]]; then
    printf 'FAIL: ожидаемый probe failure exit code 17, получен %s\n' "${failing_probe_status}" >&2
    exit 1
fi
# Упавшая реальная команда не должна публиковать acceptance PASS.
require_absent "${failing_probe_output}" "PASS: FFmpeg runtime probe acceptance"
# Только первый probe step должен быть запущен до fail-fast остановки workflow.
if [[ "$(wc -l <"${failing_probe_cargo_log}")" -ne 1 ]]; then
    printf 'FAIL: падающий probe-only должен остановиться после первого Cargo-вызова\n' >&2
    exit 1
fi

# Config helper создаёт полный current-schema TOML без GUI.
current_config_path="${temporary_directory}/current-config.toml"
cargo run --quiet --locked -p rustiplayer-config --example smoke_config -- \
    generate-current "${current_config_path}" software
# Ключи доказывают current schema v8, playback overrides и generic yt-dlp HDR default.
grep -Fqx 'schema_version = 9' "${current_config_path}"
grep -Fqx 'start_paused = false' "${current_config_path}"
grep -Fqx 'preferred_backend = "software"' "${current_config_path}"
grep -Fqx '[yt_dlp]' "${current_config_path}"
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

#!/usr/bin/env bash
# Единый executable manifest hermetic и локальных runtime acceptance suites.

set -Eeuo pipefail

# Код SKIP намеренно ненулевой: пропущенная suite не считается выполненной acceptance.
readonly SKIPPED_EXIT_CODE=3
# Корень repo вычисляется независимо от текущего рабочего каталога.
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# Playback runner является единственным владельцем scenario/config orchestration.
readonly PLAYBACK_SMOKE="${REPO_ROOT}/scripts/playback-smoke.sh"

# Выбранная suite отсутствует до явного --suite.
acceptance_suite=""
# Local asset paths никогда не угадываются из test-assets.
vp9_path=""
av1_path=""
h264_path=""
# Dry-run печатает точную команду, но не является acceptance pass.
dry_run="false"

# Справка одновременно служит человекочитаемым manifest-ом команд.
print_help() {
    cat <<'EOF'
Usage: scripts/runtime-acceptance.sh --suite SUITE [ASSET OPTIONS] [--dry-run]

Suites:
  hermetic-ci       scripts/ci-checks.sh tests; no local fixtures/runtime/hardware.
  runtime-software  FFmpeg runtime probe + software playback; requires --vp9 and --h264.
  vaapi-hardware    VA-API playback/rejection; requires --vp9, --av1 and working vainfo/render node.
  playback-matrix   Full hardware/software matrix; requires --vp9, --av1, --h264 and runtime/hardware.

Outcome contract:
  PASS      Command really ran and all assertions passed (exit 0).
  SKIP      A selected suite lacks a named prerequisite (exit 3; never acceptance pass).
  NOT RUN   No suite was selected or --dry-run was used (exit 0; never acceptance pass).
  FAIL      A command or assertion failed (nonzero exit from the failed command).
EOF
}

# Единый SKIP printer всегда называет недостающую prerequisite.
skip_acceptance() {
    local reason="$1"
    printf 'SKIP: %s; acceptance not satisfied\n' "${reason}" >&2
    exit "${SKIPPED_EXIT_CODE}"
}

# Парсер принимает только manifest fields и не передаёт неизвестные options дальше.
parse_arguments() {
    while (($# > 0)); do
        case "$1" in
            --suite)
                (($# >= 2)) || { printf 'Ошибка: --suite требует значение\n' >&2; exit 2; }
                acceptance_suite="$2"
                shift 2
                ;;
            --vp9)
                (($# >= 2)) || { printf 'Ошибка: --vp9 требует путь\n' >&2; exit 2; }
                vp9_path="$2"
                shift 2
                ;;
            --av1)
                (($# >= 2)) || { printf 'Ошибка: --av1 требует путь\n' >&2; exit 2; }
                av1_path="$2"
                shift 2
                ;;
            --h264)
                (($# >= 2)) || { printf 'Ошибка: --h264 требует путь\n' >&2; exit 2; }
                h264_path="$2"
                shift 2
                ;;
            --dry-run)
                dry_run="true"
                shift
                ;;
            --help|-h)
                print_help
                exit 0
                ;;
            *)
                printf 'Ошибка: неизвестный аргумент `%s`\n' "$1" >&2
                exit 2
                ;;
        esac
    done
}

# Проверяет explicit local file и объясняет SKIP вместо silent ignored outcome.
require_asset() {
    local option_name="$1"
    local asset_path="$2"
    [[ "${dry_run}" == "true" ]] && return
    [[ -n "${asset_path}" ]] || skip_acceptance "не указан ${option_name} local asset path"
    [[ -f "${asset_path}" ]] || skip_acceptance "${option_name} local asset не найден: ${asset_path}"
}

# Проверяет software build/runtime prerequisites до запуска acceptance suite.
require_ffmpeg_runtime() {
    [[ "${dry_run}" == "true" ]] && return
    command -v pkg-config >/dev/null 2>&1 || skip_acceptance "pkg-config недоступен для FFmpeg runtime preflight"
    pkg-config --atleast-version=62 libavcodec >/dev/null 2>&1 || \
        skip_acceptance "libavcodec >= 62 недоступен через pkg-config"
    pkg-config --atleast-version=60 libavutil >/dev/null 2>&1 || \
        skip_acceptance "libavutil >= 60 недоступен через pkg-config"
}

# Проверяет доступность локального VA-API runtime без запуска GUI.
require_vaapi_runtime() {
    [[ "${dry_run}" == "true" ]] && return
    [[ -r /dev/dri/renderD128 ]] || skip_acceptance "нет readable VA-API render node /dev/dri/renderD128"
    command -v vainfo >/dev/null 2>&1 || skip_acceptance "команда vainfo недоступна"
    vainfo >/dev/null 2>&1 || skip_acceptance "vainfo не подтвердил рабочий VA-API runtime"
}

# Печатает argv без eval; dry-run не затрагивает runtime.
print_command() {
    printf '+' >&2
    local command_part
    for command_part in "$@"; do
        printf ' %q' "${command_part}" >&2
    done
    printf '\n' >&2
}

# Запускает suite и печатает PASS только после реального успеха.
run_acceptance_command() {
    local suite_name="$1"
    shift
    if [[ "${dry_run}" == "true" ]]; then
        print_command "$@"
        printf 'NOT RUN: dry-run only for suite %s; acceptance not satisfied\n' "${suite_name}" >&2
        return
    fi
    "$@"
    printf 'PASS: %s acceptance\n' "${suite_name}" >&2
}

# Делает ignored runtime surface видимой рядом с зелёным hermetic result.
report_hermetic_runtime_skips() {
    printf 'SKIP: FFmpeg installed-runtime probe требует runtime-software suite; runtime acceptance not satisfied\n' >&2
    printf 'SKIP: 17 local-media demux regressions требуют explicit scenario/path; fixture acceptance not satisfied\n' >&2
    printf 'SKIP: direct HTTP и yt-dlp network regressions требуют explicit URL/path; network acceptance not satisfied\n' >&2
    printf 'SKIP: VA-API/WGPU playback требует vaapi-hardware или playback-matrix suite; hardware acceptance not satisfied\n' >&2
}

# Маршрутизирует manifest suite к одной точной команде и её prerequisites.
run_selected_suite() {
    case "${acceptance_suite}" in
        hermetic-ci)
            # Cargo помечает runtime tests ignored; причины должны быть видны в том же отчёте.
            report_hermetic_runtime_skips
            run_acceptance_command "hermetic-ci" "${REPO_ROOT}/scripts/ci-checks.sh" tests
            ;;
        runtime-software)
            require_asset "--vp9" "${vp9_path}"
            require_asset "--h264" "${h264_path}"
            require_ffmpeg_runtime
            run_acceptance_command "runtime-software" "${PLAYBACK_SMOKE}" \
                --mode software-only --vp9 "${vp9_path}" --h264 "${h264_path}"
            ;;
        vaapi-hardware)
            require_asset "--vp9" "${vp9_path}"
            require_asset "--av1" "${av1_path}"
            require_vaapi_runtime
            run_acceptance_command "vaapi-hardware" "${PLAYBACK_SMOKE}" \
                --mode hardware-only --vp9 "${vp9_path}" --av1 "${av1_path}"
            ;;
        playback-matrix)
            require_asset "--vp9" "${vp9_path}"
            require_asset "--av1" "${av1_path}"
            require_asset "--h264" "${h264_path}"
            require_ffmpeg_runtime
            require_vaapi_runtime
            run_acceptance_command "playback-matrix" "${PLAYBACK_SMOKE}" \
                --mode full --vp9 "${vp9_path}" --av1 "${av1_path}" --h264 "${h264_path}"
            ;;
        "")
            printf 'NOT RUN: missing --suite selection; acceptance not satisfied\n' >&2
            ;;
        *)
            printf 'Ошибка: неизвестная suite `%s`\n' "${acceptance_suite}" >&2
            exit 2
            ;;
    esac
}

# Main сохраняет parser, routing и process entrypoint раздельными.
main() {
    parse_arguments "$@"
    cd "${REPO_ROOT}"
    run_selected_suite
}

# Единственная точка входа передаёт исходный argv без eval.
main "$@"

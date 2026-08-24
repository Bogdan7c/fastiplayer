#!/usr/bin/env bash
# Единый executable manifest hermetic и локальных runtime acceptance suites.

set -Eeuo pipefail

# Код SKIP намеренно ненулевой: пропущенная suite не считается выполненной acceptance.
readonly SKIPPED_EXIT_CODE=3
# Корень repo вычисляется независимо от текущего рабочего каталога.
readonly REPO_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
# Playback runner является единственным владельцем scenario/config orchestration.
readonly PLAYBACK_SMOKE="${REPO_ROOT}/scripts/playback-smoke.sh"
# Exact AV1 Profile 0 decode entrypoint обязателен для обеих hardware suites.
readonly AV1_VAAPI_PROFILE_REGEX='^[[:space:]]*VAProfileAV1Profile0[[:space:]]*:[[:space:]]*VAEntrypointVLD([[:space:]]|$)'
# Explicit DRM node делает preflight независимым от X11/Wayland; override поддерживает multi-GPU hosts.
readonly VAAPI_RENDER_NODE="${RUSTIPLAYER_SMOKE_VAAPI_RENDER_NODE:-/dev/dri/renderD128}"

# Выбранная suite отсутствует до явного --suite.
acceptance_suite=""
# Local asset paths никогда не угадываются из test-assets.
vp9_path=""
av1_path=""
av1_hdr_path=""
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
  vaapi-hardware    Host-specific VA-API regression smoke; requires --vp9, --av1, --av1-hdr and AV1 Profile 0 VLD.
  playback-matrix   Combined host-specific regression set; requires --vp9, --av1, --av1-hdr, --h264 and runtime/hardware.

Asset options:
  --vp9 FILE        VP9 Profile 0 SDR fixture.
  --av1 FILE        AV1 Main/Profile 0 8-bit YUV420 SDR fixture.
  --av1-hdr FILE    AV1 Main/Profile 0 10-bit YUV420 HDR fixture.
  --h264 FILE       H.264 ISO BMFF software fixture.

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
            --av1-hdr)
                (($# >= 2)) || { printf 'Ошибка: --av1-hdr требует путь\n' >&2; exit 2; }
                av1_hdr_path="$2"
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

# Проверяет локальный VA-API runtime и exact AV1 decode capability без запуска GUI.
require_vaapi_runtime() {
    if [[ "${dry_run}" == "true" ]]; then
        printf 'Would require readable %s and vainfo entry: VAProfileAV1Profile0 : VAEntrypointVLD.\n' "${VAAPI_RENDER_NODE}" >&2
        return
    fi
    [[ -r "${VAAPI_RENDER_NODE}" ]] || skip_acceptance "нет readable VA-API render node ${VAAPI_RENDER_NODE}"
    command -v vainfo >/dev/null 2>&1 || skip_acceptance "команда vainfo недоступна"
    local vaapi_capabilities
    if ! vaapi_capabilities="$(vainfo --display drm --device "${VAAPI_RENDER_NODE}" 2>&1)"; then
        skip_acceptance "vainfo не подтвердил рабочий VA-API runtime"
    fi
    if ! grep -Eq -- "${AV1_VAAPI_PROFILE_REGEX}" <<<"${vaapi_capabilities}"; then
        skip_acceptance "vainfo не содержит exact VAProfileAV1Profile0 : VAEntrypointVLD"
    fi
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

# Делает невыбранные runtime surfaces видимыми, не выдавая их за selected-suite SKIP.
report_hermetic_runtime_not_run() {
    printf 'NOT RUN: FFmpeg installed-runtime probe требует runtime-software suite; runtime acceptance not satisfied\n' >&2
    printf 'NOT RUN: 18 local-media regressions (17 symphonia-demux + 1 direct HTTP Range) требуют explicit scripts/media-regression.sh --scenario/--path; fixture acceptance not satisfied\n' >&2
    printf 'NOT RUN: S42 web-media manual acceptance требует scripts/progressive-web-smoke.sh с полной explicit --case + --url/--fixture matrix и --report; manual acceptance not satisfied\n' >&2
    printf 'NOT RUN: VA-API/WGPU playback требует vaapi-hardware или playback-matrix suite; hardware acceptance not satisfied\n' >&2
}

# Маршрутизирует manifest suite к одной точной команде и её prerequisites.
run_selected_suite() {
    case "${acceptance_suite}" in
        hermetic-ci)
            # Cargo помечает runtime tests ignored; причины должны быть видны в том же отчёте.
            report_hermetic_runtime_not_run
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
            require_asset "--av1-hdr" "${av1_hdr_path}"
            require_vaapi_runtime
            run_acceptance_command "vaapi-hardware" "${PLAYBACK_SMOKE}" \
                --mode hardware-only --vp9 "${vp9_path}" --av1 "${av1_path}" \
                --av1-hdr "${av1_hdr_path}"
            ;;
        playback-matrix)
            require_asset "--vp9" "${vp9_path}"
            require_asset "--av1" "${av1_path}"
            require_asset "--av1-hdr" "${av1_hdr_path}"
            require_asset "--h264" "${h264_path}"
            require_ffmpeg_runtime
            require_vaapi_runtime
            run_acceptance_command "playback-matrix" "${PLAYBACK_SMOKE}" \
                --mode full --vp9 "${vp9_path}" --av1 "${av1_path}" \
                --av1-hdr "${av1_hdr_path}" --h264 "${h264_path}"
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

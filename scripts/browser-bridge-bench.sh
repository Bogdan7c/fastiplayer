#!/usr/bin/env bash
# Тестовый стенд моста «браузер качает — мы играем» (сессия 08): сборка и запуск одной командой.
#   scripts/browser-bridge-bench.sh capture --browser both
#   scripts/browser-bridge-bench.sh all --display real
# Повтор записи потока страницы (*.fprec, отладочный инструмент 09b2):
#   scripts/browser-bridge-bench.sh generate-recording --output /tmp/synthetic.fprec
#   scripts/browser-bridge-bench.sh replay --recording /tmp/synthetic.fprec --browser firefox [--min-frames 50]
# Быстрый набор (16b3c2) и параллельный прогон:
#   scripts/browser-bridge-bench.sh quick
#   scripts/browser-bridge-bench.sh capture --jobs 4
# Плеер и посредник для стенда — оптимизированная сборка (release): под нагрузкой параллельных
# строк судьи по времени не плывут. Отладочная — BRIDGE_BENCH_PLAYER_PROFILE=debug.
# Все аргументы передаются как есть в target/debug/browser-bridge-bench (см. --help); каталог
# плеера (--player-bin-dir) скрипт ставит первым, поэтому свой флаг пользователя главнее.
# Только локально: в CI не запускается (нужны Chromium, Firefox, kwin_wayland, ffmpeg).

# Строгий режим: любая ошибка сборки останавливает прогон, а не тихо ведёт к старому бинарнику.
set -euo pipefail

# Корень репозитория вычисляется от места скрипта, поэтому запуск работает из любого каталога.
script_directory="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" >/dev/null 2>&1 && pwd)"
repo_root="$(cd -- "${script_directory}/.." >/dev/null 2>&1 && pwd)"
readonly REPO_ROOT="${repo_root}"

# Справка не требует сборки.
for argument in "$@"; do
    if [[ "${argument}" == "-h" || "${argument}" == "--help" ]]; then
        if [[ -x "${REPO_ROOT}/target/debug/browser-bridge-bench" ]]; then
            exec "${REPO_ROOT}/target/debug/browser-bridge-bench" --help
        fi
    fi
done

cd "${REPO_ROOT}"

# 1. Плеер и посредник — профилем BRIDGE_BENCH_PLAYER_PROFILE (release по умолчанию: плеер
#    в отладочной сборке под нагрузкой параллельных строк опаздывает), стенд — debug.
player_profile="${BRIDGE_BENCH_PLAYER_PROFILE:-release}"
case "${player_profile}" in
    release) player_profile_flag=(--release) ;;
    debug) player_profile_flag=() ;;
    *)
        printf 'Error: BRIDGE_BENCH_PLAYER_PROFILE must be release or debug, got %s\n' "${player_profile}" >&2
        exit 2
        ;;
esac
readonly PLAYER_BIN_DIR="${REPO_ROOT}/target/${player_profile}"
printf '==> building player and host (%s)\n' "${player_profile}" >&2
cargo build "${player_profile_flag[@]}" -p app-egui -p browser-bridge-host
printf '==> building the bench (debug)\n' >&2
cargo build -p browser-bridge-bench

# 2. Настоящий экстеншен 07: его попап проверяет команда extension-status.
printf '==> building the real browser extension\n' >&2
if ! command -v npm >/dev/null 2>&1; then
    printf 'Error: npm is required to build extensions/browser (install nodejs/npm)\n' >&2
    exit 1
fi
(
    cd extensions/browser
    if [[ ! -d node_modules ]]; then
        printf '    node_modules missing: npm ci\n' >&2
        npm ci --silent
    fi
    npm run build --silent
)

# 3. Сам стенд. exec сохраняет код выхода: 0 успех, 1 провал, 2 ошибка аргументов.
printf '==> running the bench\n' >&2
exec "${REPO_ROOT}/target/debug/browser-bridge-bench" --player-bin-dir "${PLAYER_BIN_DIR}" "$@"

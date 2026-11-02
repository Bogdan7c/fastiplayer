#!/usr/bin/env bash
# Тестовый стенд моста «браузер качает — мы играем» (сессия 08): сборка и запуск одной командой.
#   scripts/browser-bridge-bench.sh capture --browser both
#   scripts/browser-bridge-bench.sh all --display real
# Все аргументы передаются как есть в target/debug/browser-bridge-bench (см. --help).
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

# 1. Плеер, посредник и сам стенд (debug: нужен быстрый цикл, а не скорость плеера).
printf '==> building player, host and bench (debug)\n' >&2
cargo build -p app-egui -p browser-bridge-host -p browser-bridge-bench

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
exec "${REPO_ROOT}/target/debug/browser-bridge-bench" "$@"

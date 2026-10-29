#!/usr/bin/env bash
# Ставит (или убирает) .desktop-файл, иконки и файл D-Bus activation браузерного
# режима Fastiplayer для текущего пользователя.
#
# Зачем: на Wayland панель задач берёт иконку окна только из .desktop-файла,
# имя которого совпадает с app_id окна (`io.github.Bogdan7c.Fastiplayer`).
# Без этого файла KDE/GNOME показывают стандартный значок Wayland.
# Файл `dbus-1/services/…BrowserSession.service` позволяет посреднику экстеншена
# Fastiplayer Bridge разбудить `fastiplayer --browser-session` по звонку на шину
# (мост в браузер, сессия 02). Позже его же поставит кнопка «Connect to browsers» (06).
#
# Использование:
#   scripts/install-desktop-entry.sh                     # бинарь из target/release
#   scripts/install-desktop-entry.sh --binary /путь/к/fastiplayer
#   scripts/install-desktop-entry.sh --uninstall

# Строгий режим останавливает скрипт на первой необработанной ошибке.
set -Eeuo pipefail

# Постоянный ID приложения: тот же, что `desktop_integration::FASTIPLAYER_APPLICATION_ID`.
readonly APPLICATION_ID="io.github.Bogdan7c.Fastiplayer"

# Корень репозитория вычисляем от расположения скрипта, а не от текущей папки.
readonly REPOSITORY_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Исходники ассетов в репозитории.
readonly PACKAGING_DIR="${REPOSITORY_ROOT}/packaging/linux"
readonly SOURCE_DESKTOP_FILE="${PACKAGING_DIR}/${APPLICATION_ID}.desktop"
readonly SOURCE_ICON_THEME_DIR="${PACKAGING_DIR}/icons/hicolor"

# Бинарь по умолчанию — результат `cargo build --release`.
readonly DEFAULT_BINARY_PATH="${REPOSITORY_ROOT}/target/release/fastiplayer"

# Пользовательские каталоги по XDG Base Directory; системные файлы не трогаем.
readonly USER_DATA_DIR="${XDG_DATA_HOME:-${HOME}/.local/share}"
readonly TARGET_APPLICATIONS_DIR="${USER_DATA_DIR}/applications"
readonly TARGET_ICON_THEME_DIR="${USER_DATA_DIR}/icons/hicolor"
readonly TARGET_DESKTOP_FILE="${TARGET_APPLICATIONS_DIR}/${APPLICATION_ID}.desktop"

# Имя службы браузерного режима и ключ процесса: те же, что
# `desktop_integration::{BROWSER_SESSION_BUS_NAME, BROWSER_SESSION_ARGUMENT}`.
readonly BROWSER_SESSION_BUS_NAME="${APPLICATION_ID}.BrowserSession"
readonly BROWSER_SESSION_ARGUMENT="--browser-session"
readonly TARGET_DBUS_SERVICES_DIR="${USER_DATA_DIR}/dbus-1/services"
readonly TARGET_BROWSER_SESSION_SERVICE_FILE="${TARGET_DBUS_SERVICES_DIR}/${BROWSER_SESSION_BUS_NAME}.service"

# Печатает подсказку по аргументам.
print_usage() {
    sed -n '12,16p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

# Сообщает об ошибке в stderr и завершает скрипт.
fail() {
    echo "error: $*" >&2
    exit 1
}

# Экранирует путь для ключа Exec по спецификации Desktop Entry:
# аргумент в двойных кавычках, внутри экранируются `"`, `` ` ``, `$` и `\`.
quote_desktop_exec_argument() {
    local raw_path="$1"
    local escaped_path
    escaped_path="$(printf '%s' "${raw_path}" | sed -e 's/[\\"`$]/\\&/g')"
    printf '"%s"' "${escaped_path}"
}

# Пишет путь для ключа Exec файла D-Bus activation — по тем же правилам, что
# `desktop_integration::browser_session_activation_service_file` (там обоснование):
# путь в одинарных кавычках; апостроф, обратная черта и управляющие символы
# запрещены, потому что dbus-daemon и dbus-broker толкуют их по-разному.
quote_dbus_exec_argument() {
    local raw_path="$1"
    if [[ "${raw_path}" == *"'"* || "${raw_path}" == *"\\"* || "${raw_path}" =~ [[:cntrl:]] ]]; then
        fail "binary path contains an apostrophe, a backslash or a control character, which D-Bus activation cannot start: ${raw_path}"
    fi
    printf "'%s'" "${raw_path}"
}

# Обновляет кэши рабочего стола. Отсутствие утилиты — не ошибка: значит, это
# окружение её не использует. Сбой утилиты — предупреждение, файлы уже на месте.
refresh_desktop_caches() {
    if command -v update-desktop-database >/dev/null 2>&1; then
        update-desktop-database "${TARGET_APPLICATIONS_DIR}" \
            || echo "warning: update-desktop-database failed" >&2
    fi
    if command -v gtk-update-icon-cache >/dev/null 2>&1 && [[ -f "${TARGET_ICON_THEME_DIR}/index.theme" ]]; then
        gtk-update-icon-cache --quiet "${TARGET_ICON_THEME_DIR}" \
            || echo "warning: gtk-update-icon-cache failed" >&2
    fi
    # KDE Plasma читает .desktop-файлы через свой кэш sycoca.
    if command -v kbuildsycoca6 >/dev/null 2>&1; then
        kbuildsycoca6 >/dev/null 2>&1 || echo "warning: kbuildsycoca6 failed" >&2
    fi
}

# Копирует иконки всех размеров из packaging/linux в пользовательскую тему hicolor.
install_icons() {
    local source_icon
    local icon_relative_path
    while IFS= read -r -d '' source_icon; do
        # Путь вида `256x256/apps/<id>.png` сохраняем как есть.
        icon_relative_path="${source_icon#"${SOURCE_ICON_THEME_DIR}/"}"
        install -Dm644 "${source_icon}" "${TARGET_ICON_THEME_DIR}/${icon_relative_path}"
    done < <(find "${SOURCE_ICON_THEME_DIR}" -type f -name "${APPLICATION_ID}.png" -print0)
}

# Ставит .desktop с абсолютным путём к бинарю в Exec.
install_desktop_file() {
    local binary_path="$1"
    local exec_value
    exec_value="$(quote_desktop_exec_argument "${binary_path}") %U"
    mkdir -p "${TARGET_APPLICATIONS_DIR}"
    # Заменяем только строку Exec; остальное содержимое берём из репозитория без изменений.
    awk -v exec_value="${exec_value}" \
        '/^Exec=/ { print "Exec=" exec_value; next } { print }' \
        "${SOURCE_DESKTOP_FILE}" >"${TARGET_DESKTOP_FILE}"
    chmod 644 "${TARGET_DESKTOP_FILE}"
}

# Ставит файл D-Bus activation браузерного режима (абсолютный путь к бинарю).
install_browser_session_service_file() {
    local binary_path="$1"
    local exec_value
    exec_value="$(quote_dbus_exec_argument "${binary_path}") ${BROWSER_SESSION_ARGUMENT}"
    mkdir -p "${TARGET_DBUS_SERVICES_DIR}"
    printf '[D-BUS Service]\nName=%s\nExec=%s\n' \
        "${BROWSER_SESSION_BUS_NAME}" "${exec_value}" >"${TARGET_BROWSER_SESSION_SERVICE_FILE}"
    chmod 644 "${TARGET_BROWSER_SESSION_SERVICE_FILE}"
}

# Удаляет ранее поставленные .desktop, иконки и файл D-Bus activation.
uninstall() {
    rm -f "${TARGET_DESKTOP_FILE}"
    rm -f "${TARGET_BROWSER_SESSION_SERVICE_FILE}"
    find "${TARGET_ICON_THEME_DIR}" -type f -name "${APPLICATION_ID}.png" -delete 2>/dev/null || true
    refresh_desktop_caches
    echo "Removed ${APPLICATION_ID} desktop entry, icons and browser session service from ${USER_DATA_DIR}"
}

main() {
    local binary_path="${DEFAULT_BINARY_PATH}"
    local should_uninstall=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --binary)
                [[ $# -ge 2 ]] || fail "--binary requires a path"
                binary_path="$2"
                shift 2
                ;;
            --uninstall)
                should_uninstall=true
                shift
                ;;
            -h | --help)
                print_usage
                exit 0
                ;;
            *)
                fail "unknown argument: $1 (see --help)"
                ;;
        esac
    done

    if [[ "${should_uninstall}" == true ]]; then
        uninstall
        return
    fi

    [[ -f "${SOURCE_DESKTOP_FILE}" ]] || fail "missing ${SOURCE_DESKTOP_FILE}"
    [[ -x "${binary_path}" ]] || fail "binary not found or not executable: ${binary_path} (run: cargo build --release)"
    # Абсолютный путь: .desktop запускается не из папки репозитория.
    binary_path="$(realpath "${binary_path}")"

    install_icons
    install_desktop_file "${binary_path}"
    install_browser_session_service_file "${binary_path}"
    refresh_desktop_caches
    echo "Installed ${TARGET_DESKTOP_FILE}"
    echo "Installed ${TARGET_BROWSER_SESSION_SERVICE_FILE}"
    echo "Exec: ${binary_path}"
}

main "$@"

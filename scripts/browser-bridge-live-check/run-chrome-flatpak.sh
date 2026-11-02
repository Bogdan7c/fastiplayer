#!/usr/bin/env bash
# Живая проверка посредника в Google Chrome из Flatpak (на ТВОЁМ экране, звук настоящий).
#   ./run-chrome-flatpak.sh
#
# ВАЖНО: скрипт НИЧЕГО не устанавливает (ни файл подключения, ни посредника, ни D-Bus службу).
# Это делает кнопка Connect в плеере (Settings > Browser integration, строка Google Chrome
# (Flatpak)), а кнопка Allow даёт песочнице право звать плеер по шине. Скрипт только проверяет,
# что всё это на месте, и запускает Chrome с временным профилем и мини-расширением 05.
# Плеер, который будит шина, - тот, чей путь записан в службе (кто последним нажал Connect).
source "$(dirname "$(readlink -f "$0")")/common.sh"

APP_ID="com.google.Chrome"
APP_DIR="$HOME/.var/app/$APP_ID"
# Файл подключения, который ставит плеер (путь из каталога целей сессии 06).
PLAYER_MANIFEST="$APP_DIR/config/google-chrome/NativeMessagingHosts/$HOST_NAME.json"
SERVICE_FILE="${XDG_DATA_HOME:-$HOME/.local/share}/dbus-1/services/$BROWSER_SESSION_BUS_NAME.service"

# Только настоящий экран: песочница Flatpak не видит нашу частную Wayland-сессию и частную шину.
DISPLAY_TARGET="${DISPLAY_TARGET:-real}"
[ "$DISPLAY_TARGET" = real ] || { lv_log "run-chrome-flatpak.sh works only with DISPLAY_TARGET=real (the sandbox needs your real display and session bus)"; exit 2; }
command -v flatpak >/dev/null || { lv_log "flatpak is not installed"; exit 2; }
flatpak info "$APP_ID" >/dev/null 2>&1 || { lv_log "$APP_ID is not installed (flatpak install flathub $APP_ID)"; exit 2; }

# 1. То, что должен был поставить плеер.
[ -f "$PLAYER_MANIFEST" ] || { lv_log "no native messaging manifest at $PLAYER_MANIFEST - press Connect for Google Chrome (Flatpak) in Fastiplayer (Settings > Browser integration) first"; exit 2; }
[ -f "$SERVICE_FILE" ] || { lv_log "no D-Bus service file at $SERVICE_FILE - press Connect in Fastiplayer first"; exit 2; }
grep -q "chrome-extension://$CHROMIUM_ID/" "$PLAYER_MANIFEST" || { lv_log "$PLAYER_MANIFEST does not allow chrome-extension://$CHROMIUM_ID/ (not written by Fastiplayer Connect?)"; exit 2; }
MANIFEST_HOST_PATH="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["path"])' "$PLAYER_MANIFEST")" \
  || { lv_log "cannot read the host path from $PLAYER_MANIFEST"; exit 2; }
[ -x "$MANIFEST_HOST_PATH" ] || { lv_log "the host from the manifest is missing or not executable: $MANIFEST_HOST_PATH - press Reconnect in Fastiplayer"; exit 2; }

# 2. Право песочницы звать плеер по шине: полная шина сессии или talk-name с маской .*
PERMISSIONS="$(flatpak info --show-permissions "$APP_ID" 2>/dev/null)"
if ! grep -Eq '^sockets=(.*;)?session-bus(;|$)' <<<"$PERMISSIONS" \
   && ! grep -Eq '^io\.github\.Bogdan7c\.Fastiplayer\.(\*|BrowserSession)=(talk|own)' <<<"$PERMISSIONS"; then
  lv_log "the Chrome sandbox cannot reach $BROWSER_SESSION_BUS_NAME - press Allow in Fastiplayer, or run: flatpak override --user --talk-name='io.github.Bogdan7c.Fastiplayer.*' $APP_ID"
  exit 2
fi

# 3. Свой Chrome из Flatpak не должен уже работать (мы его потом завершаем через flatpak kill).
if flatpak ps --columns=application 2>/dev/null | grep -qx "$APP_ID"; then
  lv_log "$APP_ID is already running - close it first (this script stops its own instance at the end)"
  exit 2
fi

# Рабочая папка скрипта: статус-сервер, журнал расширения, итог. Бинари не нужны.
RUN_NAME="chrome-flatpak"
lv_prepare
WORK="$LIVE_ROOT/run-$RUN_NAME"
rm -rf "$WORK"; mkdir -p "$WORK"
# Песочница видит только свою папку ~/.var/app/<id>: профиль и расширение кладём туда.
SANDBOX_TMP="$APP_DIR/cache/fplive-$$"
mkdir -p "$SANDBOX_TMP/profile/NativeMessagingHosts"
cp -r "$EXT_BUILD" "$SANDBOX_TMP/ext"
# Временный профиль ищет файл подключения у себя: даём ссылку на файл плеера
# (так проверяется его содержимое: путь к хосту и разрешённые расширения).
ln -s "$PLAYER_MANIFEST" "$SANDBOX_TMP/profile/NativeMessagingHosts/$HOST_NAME.json"
LV_EXTRA_CLEANUP='flatpak kill "$APP_ID" 2>/dev/null; sleep 1; rm -rf "$SANDBOX_TMP"'
trap lv_cleanup EXIT INT TERM

node "$LIVE_DIR/status-server.mjs" "$WORK" & LV_PIDS+=($!)
for _ in $(seq 50); do [ -s "$WORK/status.port" ] && break; sleep 0.1; done
[ -s "$WORK/status.port" ] || { lv_log "status server did not start"; exit 6; }
echo "globalThis.LIVE_CONFIG = { statusUrl: \"http://127.0.0.1:$(cat "$WORK/status.port")/\", hostName: \"$HOST_NAME\" };" > "$SANDBOX_TMP/ext/config.js"

DEBUG_PORT=$((20000 + RANDOM % 20000))
lv_log "starting $APP_ID (CDP port $DEBUG_PORT, profile in $SANDBOX_TMP)"
flatpak run "$APP_ID" --user-data-dir="$SANDBOX_TMP/profile" --remote-debugging-port=$DEBUG_PORT \
  --enable-unsafe-extension-debugging --no-first-run --no-default-browser-check about:blank >"$WORK/browser.log" 2>&1 &
BROWSER_PID=$!; LV_PIDS+=($BROWSER_PID)

# Расширение грузим через CDP Extensions.loadUnpacked (--load-extension у брендированного Chrome с 137 не работает).
set -o pipefail   # ошибка node не должна теряться за tee
node "$LIVE_DIR/load-chrome-extension.mjs" $DEBUG_PORT "$SANDBOX_TMP/ext" "$CHROMIUM_ID" | tee "$WORK/install.log" \
  || { lv_log "extension was not loaded"; exit 7; }

lv_wait_final; RESULT=$?
if [ "${AUTO_CLOSE:-0}" = 1 ]; then
  sleep 2
  lv_log "closing $APP_ID (flatpak kill), host must exit on stdin EOF"
  flatpak kill "$APP_ID" 2>/dev/null; wait $BROWSER_PID 2>/dev/null
  sleep 3
  echo "orphan hosts after browser close: $(pgrep -fc "^$MANIFEST_HOST_PATH" || true)"
else
  lv_log "done. Close Chrome yourself (this checks that the host exits); Ctrl+C here to clean up"
  wait $BROWSER_PID
  sleep 3
  echo "orphan hosts after browser close: $(pgrep -fc "^$MANIFEST_HOST_PATH" || true)"
fi
echo "=== final stats ($RUN_NAME) ==="; cat "$WORK/final-stats.json" 2>/dev/null || cat "$WORK/last-stats.json" 2>/dev/null; echo
exit $RESULT

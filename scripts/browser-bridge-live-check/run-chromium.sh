#!/usr/bin/env bash
# Живая проверка посредника в Chromium.
#   ./run-chromium.sh                          # виртуальный дисплей (kwin_wayland --virtual), автоматически
#   DISPLAY_TARGET=real ./run-chromium.sh      # на ТВОЁМ экране: окно Chromium + окно плеера, звук настоящий
# Окно плеера покажет 20-секундный ролик с номером кадра. После конца проверки при DISPLAY_TARGET=real
# Chromium остаётся открытым: закрой его сам (проверяется, что хост выходит) или нажми на иконку
# расширения, чтобы запустить ролик снова.
source "$(dirname "$(readlink -f "$0")")/common.sh"
lv_setup chromium
lv_start_display
lv_start_bus

PROFILE="$WORK/chromium-profile"
mkdir -p "$PROFILE/NativeMessagingHosts"
cat > "$PROFILE/NativeMessagingHosts/$HOST_NAME.json" <<M
{
  "name": "$HOST_NAME",
  "description": "Fastiplayer bridge host (live check)",
  "path": "$WORK/host-wrapper.sh",
  "type": "stdio",
  "allowed_origins": ["chrome-extension://$CHROMIUM_ID/"]
}
M
BROWSER_ENV=("${DISPLAY_ENV[@]}")
[ "$DISPLAY_TARGET" = virtual ] && BROWSER_ENV+=(HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/xdg/bconfig" XDG_CACHE_HOME="$WORK/xdg/bcache")
FLAGS=(--user-data-dir="$PROFILE" --load-extension="$WORK/ext" --no-first-run --no-default-browser-check)
[ "$DISPLAY_TARGET" = virtual ] && FLAGS+=(--ozone-platform=wayland --disable-gpu)
lv_log "starting chromium"
env "${BROWSER_ENV[@]}" DBUS_SESSION_BUS_ADDRESS="$BUS_ADDR" chromium "${FLAGS[@]}" about:blank >"$WORK/browser.log" 2>&1 &
BROWSER_PID=$!; LV_PIDS+=($BROWSER_PID)

lv_wait_final; RESULT=$?
if [ "$AUTO_CLOSE" = 1 ]; then
  sleep 2
  lv_log "closing chromium (SIGTERM), host must exit on stdin EOF"
  kill $BROWSER_PID 2>/dev/null; wait $BROWSER_PID 2>/dev/null
  sleep 3
  echo "orphan hosts after browser close: $(pgrep -fc "^$WORK/bin/fastiplayer-browser-host" || true)"
else
  lv_log "done. Close Chromium yourself; press Ctrl+C here to clean up"
  wait $BROWSER_PID
fi
lv_report
exit $RESULT

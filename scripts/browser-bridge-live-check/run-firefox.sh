#!/usr/bin/env bash
# Живая проверка посредника в Firefox (временная установка через WebDriver BiDi).
#   ./run-firefox.sh                          # виртуальный дисплей
#   DISPLAY_TARGET=real ./run-firefox.sh      # на ТВОЁМ экране
# ВНИМАНИЕ: в режиме real пишет ~/.mozilla/native-messaging-hosts/<имя хоста из фикстуры>.json и
# убирает его при выходе (trap). Если файл уже есть и он не наш — скрипт отказывается работать.
source "$(dirname "$(readlink -f "$0")")/common.sh"
MANIFEST="$HOME/.mozilla/native-messaging-hosts/$HOST_NAME.json"
if [ -e "$MANIFEST" ] && ! grep -q "live check" "$MANIFEST"; then lv_log "refusing to overwrite foreign $MANIFEST"; exit 5; fi
lv_setup firefox
LV_EXTRA_CLEANUP='[ "$DISPLAY_TARGET" = real ] && { rm -f "$MANIFEST"; rmdir --ignore-fail-on-non-empty "$HOME/.mozilla/native-messaging-hosts" 2>/dev/null; }'
lv_start_display
lv_start_bus

MANIFEST_BODY=$(cat <<M
{
  "name": "$HOST_NAME",
  "description": "Fastiplayer bridge host (live check)",
  "path": "$WORK/host-wrapper.sh",
  "type": "stdio",
  "allowed_extensions": ["$GECKO_ID"]
}
M
)
if [ "$DISPLAY_TARGET" = real ]; then   # настоящий браузер ищет файл в настоящем ~
  mkdir -p "$HOME/.mozilla/native-messaging-hosts"; echo "$MANIFEST_BODY" > "$MANIFEST"
fi
PROFILE="$WORK/firefox-profile"; mkdir -p "$PROFILE"
cat > "$PROFILE/user.js" <<'P'
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.startup.homepage_override.mstone", "ignore");
user_pref("datareporting.policy.dataSubmissionEnabled", false);
user_pref("app.update.auto", false);
user_pref("extensions.autoDisableScopes", 0);
user_pref("xpinstall.signatures.required", false);
user_pref("browser.aboutwelcome.enabled", false);
user_pref("devtools.console.stdout.content", true);
user_pref("devtools.console.stdout.chrome", true);
user_pref("toolkit.telemetry.reportingpolicy.firstRun", false);
P
BIDI_PORT=$((20000 + RANDOM % 20000))
BROWSER_ENV=("${DISPLAY_ENV[@]}")
[ "$DISPLAY_TARGET" = virtual ] && BROWSER_ENV+=(HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/xdg/bconfig" XDG_CACHE_HOME="$WORK/xdg/bcache" MOZ_ENABLE_WAYLAND=1 MOZ_DISABLE_RDD_SANDBOX=0)
lv_log "starting firefox (BiDi port $BIDI_PORT)"
# virtual: у браузера свой HOME ($WORK/home) — манифест кладём туда, настоящий ~ не трогаем.
if [ "$DISPLAY_TARGET" = virtual ]; then
  BROWSER_HOME="$WORK/home"
  mkdir -p "$BROWSER_HOME/.mozilla/native-messaging-hosts"; echo "$MANIFEST_BODY" > "$BROWSER_HOME/.mozilla/native-messaging-hosts/$HOST_NAME.json"
fi
env "${BROWSER_ENV[@]}" DBUS_SESSION_BUS_ADDRESS="$BUS_ADDR" firefox --profile "$PROFILE" --no-remote --remote-debugging-port=$BIDI_PORT about:blank >"$WORK/browser.log" 2>&1 &
BROWSER_PID=$!; LV_PIDS+=($BROWSER_PID)
node "$LIVE_DIR/install-firefox-addon.mjs" $BIDI_PORT "$WORK/ext" | tee "$WORK/install.log"

lv_wait_final; RESULT=$?
if [ "$AUTO_CLOSE" = 1 ]; then
  sleep 2
  lv_log "closing firefox (SIGTERM), host must exit on stdin EOF"
  kill $BROWSER_PID 2>/dev/null; wait $BROWSER_PID 2>/dev/null
  sleep 3
  echo "orphan hosts after browser close: $(pgrep -fc "$WORK/bin/fastiplayer-browser-host" || true)"
else
  lv_log "done. Close Firefox yourself"
  wait $BROWSER_PID
fi
lv_report
exit $RESULT

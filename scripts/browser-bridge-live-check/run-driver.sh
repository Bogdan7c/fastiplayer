#!/usr/bin/env bash
# Проверка БЕЗ браузера: node играет роль браузера для хоста (argv Chromium), плеер — без окна.
source "$(dirname "$(readlink -f "$0")")/common.sh"
DISPLAY_TARGET=headless
lv_setup driver
DISPLAY_ENV=(FASTIPLAYER_TEST_BROWSER_VIDEO=headless); PLAYER_AUDIO_ENV=(FASTIPLAYER_TEST_BROWSER_AUDIO=simulated)
lv_start_bus
DBUS_SESSION_BUS_ADDRESS="$BUS_ADDR" node "$LIVE_DIR/drive-host.mjs" "$HOST_BIN" "$EXT_BUILD" "$CHROMIUM_ID" 2>"$WORK/host.log"; RC=$?
sleep 1
lv_report; echo "driver rc=$RC"; exit $RC

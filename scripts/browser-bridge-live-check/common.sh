# Общие функции run-chromium.sh / run-firefox.sh / run-driver.sh (source).
# Ничего не пишет вне $LIVE_ROOT, /tmp/fplive-* и (только Firefox в режиме real)
# ~/.mozilla/native-messaging-hosts/<имя>.json — всё убирается в lv_cleanup.
#
# Переменные окружения:
#   DISPLAY_TARGET=virtual|real   (по умолчанию virtual: частный kwin_wayland --virtual;
#                                  real: окно плеера и браузер на ТВОЁМ экране, звук настоящий)
#   LIVE_ROOT=<папка>             рабочая папка для всего сгенерированного
#                                 (по умолчанию <репозиторий>/target/browser-bridge-live-check)
#   LIVE_BIN_DIR=<папка>          откуда брать fastiplayer и fastiplayer-browser-host
#                                 (по умолчанию <репозиторий>/target/debug)
#   LIVE_NO_SNAPSHOT=1            не копировать бинари в папку прогона
#   AUTO_CLOSE=1|0                закрывать браузер после конца проверки (virtual: 1, real: 0)
#   TIMEOUT_S=120                 потолок ожидания конца проверки
LIVE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Корень репозитория выводим из положения скрипта (scripts/browser-bridge-live-check/ -> ../..).
REPO="$(cd "$LIVE_DIR/../.." && pwd)"
LIVE_ROOT="${LIVE_ROOT:-$REPO/target/browser-bridge-live-check}"
BIN_DIR="${LIVE_BIN_DIR:-$REPO/target/debug}"
HOST_BIN="$BIN_DIR/fastiplayer-browser-host"
PLAYER_BIN="$BIN_DIR/fastiplayer"
# Единый источник правды об «наших» расширениях: имя хоста, ID Chromium, ключ и gecko id Firefox.
IDENTITY_FIXTURE="$REPO/crates/browser-bridge-protocol/fixtures/extension-identity.json"
[ -f "$IDENTITY_FIXTURE" ] || { echo "[live] identity fixture not found: $IDENTITY_FIXTURE" >&2; exit 2; }
# Три строки: имя хоста, ID расширения Chromium, gecko id. Падаем, если поля нет (set -e не нужен: проверяем явно).
IDENTITY_LINES="$(python3 -I - "$IDENTITY_FIXTURE" <<'PY'
import json, sys
identity = json.load(open(sys.argv[1], encoding="utf-8"))
print(identity["native_host_name"])
print(identity["chromium_development"]["extension_id"])
print(identity["firefox"]["gecko_id"])
PY
)" || { echo "[live] cannot read $IDENTITY_FIXTURE" >&2; exit 2; }
{ read -r HOST_NAME; read -r CHROMIUM_ID; read -r GECKO_ID; } <<<"$IDENTITY_LINES"
[ -n "$HOST_NAME" ] && [ -n "$CHROMIUM_ID" ] && [ -n "$GECKO_ID" ] || { echo "[live] empty identity fields in $IDENTITY_FIXTURE" >&2; exit 2; }
DISPLAY_TARGET="${DISPLAY_TARGET:-virtual}"
TIMEOUT_S="${TIMEOUT_S:-120}"
LV_PIDS=()
LV_EXTRA_CLEANUP=""
EXT_BUILD="$LIVE_ROOT/extension-build"   # готовое расширение: исходники + сгенерированные manifest, plan, media

lv_log() { echo "[live] $*" >&2; }

# Готовит $EXT_BUILD: ролик и план создаются при первом запуске (в git их нет), manifest.json
# собирается из шаблона и фикстуры протокола каждый раз (ключ и gecko id не дублируются в репозитории).
lv_prepare() {
  mkdir -p "$LIVE_ROOT/media" "$EXT_BUILD"
  command -v ffmpeg >/dev/null || { lv_log "ffmpeg is required to generate the test clip"; exit 2; }
  if [ ! -s "$LIVE_ROOT/media/video.mp4" ] || [ ! -s "$LIVE_ROOT/media/audio.mp4" ]; then
    lv_log "generating test clip (first run)"
    "$LIVE_DIR/make-media.sh" "$LIVE_ROOT/media" || { lv_log "clip generation failed"; exit 2; }
    rm -f "$LIVE_ROOT/plan.json"   # план привязан к размерам файлов
  fi
  if [ ! -s "$LIVE_ROOT/plan.json" ]; then
    python3 -I "$LIVE_DIR/make-plan.py" "$LIVE_ROOT/media" "$LIVE_ROOT/plan.json" || { lv_log "plan generation failed"; exit 2; }
  fi
  rm -rf "$EXT_BUILD"; mkdir -p "$EXT_BUILD/media"
  cp "$LIVE_DIR"/extension/{background.js,runner.js,config.js} "$EXT_BUILD/"
  python3 -I "$LIVE_DIR/make-manifest.py" "$IDENTITY_FIXTURE" "$LIVE_DIR/extension/manifest.template.json" "$EXT_BUILD/manifest.json" \
    || { lv_log "manifest generation failed"; exit 2; }
  cp "$LIVE_ROOT/plan.json" "$EXT_BUILD/plan.json"
  cp "$LIVE_ROOT"/media/{video,audio}.mp4 "$EXT_BUILD/media/"
}

lv_setup() { # $1 = имя прогона
  RUN_NAME="$1"
  lv_prepare
  WORK="$LIVE_ROOT/run-$RUN_NAME"
  RT="/tmp/fplive-$RUN_NAME-$$"          # короткий путь: сокеты Unix ограничены 108 байтами
  rm -rf "$WORK"; mkdir -p "$WORK/xdg/config" "$WORK/xdg/data" "$WORK/xdg/state" "$WORK/xdg/cache" "$WORK/services" "$WORK/home"
  mkdir -m 700 "$RT"
  [ -x "$HOST_BIN" ] && [ -x "$PLAYER_BIN" ] || { lv_log "build first: cargo build -p browser-bridge-host -p app-egui (looked in $BIN_DIR)"; exit 2; }
  trap lv_cleanup EXIT INT TERM
  # Снимок бинарей: параллельная пересборка cargo не подменит их посреди прогона.
  # LIVE_NO_SNAPSHOT=1 — использовать target/debug как есть.
  if [ -z "$LIVE_NO_SNAPSHOT" ]; then
    mkdir -p "$WORK/bin"; cp "$HOST_BIN" "$WORK/bin/"; cp "$PLAYER_BIN" "$WORK/bin/"; cp -r "$(dirname "$PLAYER_BIN")/lib" "$WORK/bin/lib"
    HOST_BIN="$WORK/bin/fastiplayer-browser-host"; PLAYER_BIN="$WORK/bin/fastiplayer"
  fi
  # Копия расширения с адресом сервера статуса и именем хоста.
  cp -r "$EXT_BUILD" "$WORK/ext"
  node "$LIVE_DIR/status-server.mjs" "$WORK" & LV_PIDS+=($!)
  for _ in $(seq 50); do [ -s "$WORK/status.port" ] && break; sleep 0.1; done
  [ -s "$WORK/status.port" ] || { lv_log "status server did not start"; exit 6; }
  echo "globalThis.LIVE_CONFIG = { statusUrl: \"http://127.0.0.1:$(cat "$WORK/status.port")/\", hostName: \"$HOST_NAME\" };" > "$WORK/ext/config.js"
  # Обёртка хоста: те же argv от браузера, но stderr хоста — в файл (чтобы собрать журнал).
  cat > "$WORK/host-wrapper.sh" <<W
#!/bin/sh
echo "wrapper: start pid=\$\$ argv=\$*" >> "$WORK/host.log"
"$HOST_BIN" "\$@" 2>>"$WORK/host.log"
rc=\$?
echo "wrapper: host exited rc=\$rc" >> "$WORK/host.log"
exit \$rc
W
  chmod +x "$WORK/host-wrapper.sh"
}

lv_start_display() {
  if [ "$DISPLAY_TARGET" = real ]; then
    lv_log "REAL SCREEN mode: using the current session display"
    DISPLAY_ENV=()   # окружение пользователя как есть
    PLAYER_AUDIO_ENV=()
    AUTO_CLOSE="${AUTO_CLOSE:-0}"
    return
  fi
  AUTO_CLOSE="${AUTO_CLOSE:-1}"
  local kwin_conf="$WORK/kwin-bus.conf" kwin_addr="unix:path=$RT/kwin-bus"
  cat > "$kwin_conf" <<C
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>$kwin_addr</listen><auth>EXTERNAL</auth>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
C
  dbus-daemon --config-file="$kwin_conf" --nofork --print-address=1 >/dev/null 2>&1 & LV_PIDS+=($!)
  mkdir -p "$WORK/kwin-home"
  env -i PATH="$PATH" HOME="$WORK/kwin-home" XDG_RUNTIME_DIR="$RT" XDG_CONFIG_HOME="$WORK/kwin-home/config" \
    XDG_DATA_HOME="$WORK/kwin-home/data" XDG_CACHE_HOME="$WORK/kwin-home/cache" XDG_STATE_HOME="$WORK/kwin-home/state" \
    DBUS_SESSION_BUS_ADDRESS="$kwin_addr" \
    kwin_wayland --virtual --no-lockscreen --width 1280 --height 720 --socket=wl-fplive >"$WORK/kwin.log" 2>&1 & LV_PIDS+=($!)
  for _ in $(seq 150); do [ -S "$RT/wl-fplive" ] && break; sleep 0.1; done
  [ -S "$RT/wl-fplive" ] || { lv_log "kwin_wayland did not start"; exit 3; }
  # Окружение для всего, что рисует: браузер и плеер. DISPLAY снят: только Wayland.
  DISPLAY_ENV=(WAYLAND_DISPLAY=wl-fplive XDG_RUNTIME_DIR="$RT")
  PLAYER_AUDIO_ENV=(FASTIPLAYER_TEST_BROWSER_AUDIO=simulated)   # звук в тихую карту
}

lv_start_bus() {
  local addr="unix:path=$RT/bus"
  cat > "$WORK/services/io.github.Bogdan7c.Fastiplayer.BrowserSession.service" <<S
[D-BUS Service]
Name=io.github.Bogdan7c.Fastiplayer.BrowserSession
Exec='$PLAYER_BIN' --browser-session
S
  cat > "$WORK/bus.conf" <<C
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>$addr</listen><auth>EXTERNAL</auth>
<servicedir>$WORK/services</servicedir>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
C
  # Запущенный шиной плеер наследует окружение демона: изолированные XDG, дисплей, журнал.
  # В real-режиме HOME и XDG_RUNTIME_DIR остаются настоящими (звук, Wayland), config — изолирован.
  local home_env=(HOME="$WORK/home")
  [ "$DISPLAY_TARGET" = real ] && home_env=()
  env "${DISPLAY_ENV[@]}" "${PLAYER_AUDIO_ENV[@]}" "${home_env[@]}" \
    DBUS_SESSION_BUS_ADDRESS="$addr" XDG_CONFIG_HOME="$WORK/xdg/config" XDG_DATA_HOME="$WORK/xdg/data" \
    XDG_STATE_HOME="$WORK/xdg/state" XDG_CACHE_HOME="$WORK/xdg/cache" RUST_LOG="${RUST_LOG:-info}" NO_COLOR=1 \
    dbus-daemon --config-file="$WORK/bus.conf" --nofork --print-address=1 >"$WORK/bus-address.txt" 2>"$WORK/player.log" & LV_PIDS+=($!)
  for _ in $(seq 50); do [ -s "$WORK/bus-address.txt" ] && break; sleep 0.1; done
  [ -S "$RT/bus" ] || { lv_log "private bus did not start"; exit 4; }
  BUS_ADDR="$addr"
  lv_log "private bus: $BUS_ADDR (player log: $WORK/player.log)"
}

# Ждёт final-stats.json или потолок. Возвращает 0 если дождался.
lv_wait_final() {
  lv_log "waiting for the extension to finish (up to ${TIMEOUT_S}s)..."
  for _ in $(seq $((TIMEOUT_S * 2))); do
    [ -s "$WORK/final-stats.json" ] && return 0
    sleep 0.5
  done
  return 1
}

lv_report() {
  echo "=== final stats ($RUN_NAME) ==="; cat "$WORK/final-stats.json" 2>/dev/null || cat "$WORK/last-stats.json" 2>/dev/null; echo
  echo "=== host log lines ==="; grep -E "fastiplayer-browser-host|wrapper" "$WORK/host.log" 2>/dev/null | sed 's/title=[^ ]*//' | head -40
  echo "=== player summary ==="; grep -E "Browser (playback summary|playback ended|session (opened|stopped)|window ready)" "$WORK/player.log" 2>/dev/null
}

lv_cleanup() {
  trap - EXIT INT TERM
  eval "$LV_EXTRA_CLEANUP"
  for pid in "${LV_PIDS[@]}"; do kill "$pid" 2>/dev/null; done
  sleep 0.5
  for pid in "${LV_PIDS[@]}"; do kill -9 "$pid" 2>/dev/null; done
  pkill -f "$WORK/" 2>/dev/null
  rm -rf "$RT"
}

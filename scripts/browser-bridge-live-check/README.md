# Живая проверка моста «браузер -> Fastiplayer»

## Что это доказывает

Мини-расширение (один MV3-манифест на Chromium и Firefox) делает ровно то, что будет делать
настоящее расширение: вызывает посредника `fastiplayer-browser-host` (native messaging) ->
`hello` / `open_session` -> получает порт и токен -> подключается по WebSocket к
`fastiplayer --browser-session` и стримит записанный 20-секундный ролик (H.264 + AAC, два
фрагментированных MP4 = два SourceBuffer, куски режутся посреди боксов). Окно плеера
должно это проиграть до конца. То есть проверяется вся цепочка на настоящих браузерах и
настоящих бинарях, а не только юнит-тесты.

Это одноразовый стенд. Постоянный стенд (с регрессиями в CI) - сессия 08.

## Что нужно

- собранные бинари: `cargo build -p browser-bridge-host -p app-egui` (ищутся в `target/debug`);
- `chromium`, `firefox`, `node`, `ffmpeg` (с `drawtext`), `python3`, `dbus-daemon`;
- `kwin_wayland` - для виртуального режима (невидимый частный экран, ваш экран не трогается).

## Команды

Из корня репозитория (пути выводятся от места скрипта, откуда запускать - неважно):

```
scripts/browser-bridge-live-check/run-driver.sh      # без браузера: node изображает браузер
scripts/browser-bridge-live-check/run-chromium.sh    # виртуальный экран
scripts/browser-bridge-live-check/run-firefox.sh     # виртуальный экран
DISPLAY_TARGET=real scripts/browser-bridge-live-check/run-chromium.sh   # на вашем экране, звук настоящий
DISPLAY_TARGET=real scripts/browser-bridge-live-check/run-firefox.sh
```

В режиме `real` браузер остаётся открытым: закройте его сами (так проверяется, что хост
выходит). Firefox в режиме `real` на время прогона пишет
`~/.mozilla/native-messaging-hosts/<имя хоста>.json`, убирает при выходе (trap) и
отказывается работать, если там лежит чужой файл. В виртуальном режиме домашняя папка
браузера своя - настоящий `~` не трогается.

Полезные переменные: `LIVE_ROOT` (рабочая папка), `LIVE_BIN_DIR` (откуда брать бинари),
`LIVE_NO_SNAPSHOT=1`, `AUTO_CLOSE`, `TIMEOUT_S` - см. шапку `common.sh`.

## Что и где создаётся

Всё сгенерированное лежит в `target/browser-bridge-live-check/` (в git не попадает):
`media/` (ролик, создаётся через `make-media.sh` при первом запуске), `plan.json`
(`make-plan.py`: порядок и нарезка рамок), `extension-build/` (исходники расширения +
`manifest.json`, собранный из `extension/manifest.template.json` и
`crates/browser-bridge-protocol/fixtures/extension-identity.json` - ключ Chromium и gecko id
живут только в фикстуре), `run-<имя>/` (снимок бинарей, профили, `host.log`, `player.log`,
`extension-console.log`, `final-stats.json`). Каждый прогон начинает свою папку `run-<имя>/`
заново.

## Что считается PASS

- в сводке плеера `Browser playback ended ... presented_frames=500` (у `run-driver.sh`
  бывает ~497-498 - известная особенность тестового драйвера, не ошибка);
- в журнале хоста строки `caller accepted`, `answered ... reply=player:hello_accepted`,
  `answered ... reply=player:session_granted`, `input closed, exiting`;
- хост завершился с `rc=0` (строка `wrapper: host exited rc=0`; у драйвера - `HOST EXIT {"code":0}`);
- `orphan hosts after browser close: 0`;
- у скрипта код возврата 0 и `final-stats.json` с `"phase":"done"`.

## Режим `MANIFEST_SOURCE=player` (проверка того, что поставил плеер)

По умолчанию `run-chromium.sh` и `run-firefox.sh` сами пишут файл подключения (с обёрткой
хоста, которая ведёт `host.log`) и поднимают частную шину. С `MANIFEST_SOURCE=player` они
**ничего не устанавливают**: берут файл подключения, посредника и D-Bus службу, которые поставила
кнопка Connect в плеере (Settings > Browser integration), и ходят по настоящей шине сессии
(будится настоящий `fastiplayer --browser-session`). Только `DISPLAY_TARGET=real`; без
установленных плеером файлов скрипт останавливается с понятным сообщением. Журнала `host.log` в
этом режиме нет (хост запущен без обёртки).

```
MANIFEST_SOURCE=player DISPLAY_TARGET=real scripts/browser-bridge-live-check/run-chromium.sh
MANIFEST_SOURCE=player DISPLAY_TARGET=real scripts/browser-bridge-live-check/run-firefox.sh
```

## Google Chrome из Flatpak: `run-chrome-flatpak.sh`

```
scripts/browser-bridge-live-check/run-chrome-flatpak.sh     # на вашем экране, звук настоящий
```

Условие: **файлы ставит плеер, не скрипт.** Перед запуском в плеере нажмите Connect для строки
Google Chrome (Flatpak) и, если там есть Allow, нажмите его (это
`flatpak override --user --talk-name='io.github.Bogdan7c.Fastiplayer.*' com.google.Chrome`).
Скрипт проверяет и останавливается, если чего-то нет: файл подключения
`~/.var/app/com.google.Chrome/config/google-chrome/NativeMessagingHosts/<имя хоста>.json` (с
разрешённым ID расширения и исполняемым посредником по пути из него), D-Bus службу в
`~/.local/share/dbus-1/services/`, право песочницы (`flatpak info --show-permissions`: полная
шина или `talk-name` с маской `.*`), а также что Chrome из Flatpak сейчас не запущен.

Что делает: временный профиль и копию мини-расширения кладёт внутрь `~/.var/app/com.google.Chrome/cache/fplive-<pid>/`
(только это песочница видит), даёт профилю ссылку на файл подключения плеера (с нестандартным
`--user-data-dir` Chrome ищет файл в профиле, поэтому проверяется содержимое файла, а не его
расположение), запускает `flatpak run com.google.Chrome --user-data-dir=... --remote-debugging-port=N
--enable-unsafe-extension-debugging`, грузит расширение CDP-командой `Extensions.loadUnpacked`
(`load-chrome-extension.mjs`, проверяет ID), ждёт `final-stats.json`. При выходе (trap)
вызывает `flatpak kill com.google.Chrome` и удаляет временную папку. Только `DISPLAY_TARGET=real`
(песочница не видит частный экран и частную шину). Chrome остаётся открытым, пока вы его не
закроете (`AUTO_CLOSE=1` закрывает сам).

PASS: `final-stats.json` с `"phase":"done"`, окно плеера доиграло ролик до конца
(`presented_frames=500` в журнале плеера из шины сессии), `orphan hosts after browser close: 0`,
код возврата 0. Журнал плеера и хоста здесь не собирается скриптом (запуск идёт через настоящую
шину): смотрите `journalctl --user` или запустите плеер сами.

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

## Чего пока нет

Flatpak Chrome не покрыт (сессия 06). Для него понадобится: переопределение talk-name для
D-Bus (песочница иначе не достучится до сессионной шины), настоящий бинарь хоста внутри
папки песочницы и загрузка расширения через CDP `Extensions.loadUnpacked`.

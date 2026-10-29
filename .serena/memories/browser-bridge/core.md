# Мост в браузер / Fastiplayer Bridge (issue #2) — core

План: `user/browser-bridge/` (README — решения владельца 1–72, порядок сессий 00…24, инварианты; `results/NN.md` — отчёты). Статус: сессия 00 (разведка) DONE 2026-10-29; дальше 00a/00b (режим 3, окна) и 00c (прототип виртуального элемента и канала байтов). Код плеера пока не менялся.

## Главная идея (решение 37)
«Браузер качает — мы играем»: экстеншен перехватывает байты, которые плеер страницы кормит видеоэлементу (MSE `appendBuffer`, для `<video src>` — скачивание сетью браузера), подменяет элемент «виртуальным» (браузер не декодирует) и отдаёт байты плееру. У плеера нет кук/адресов/секретов. Браузерный режим — второй процесс того же бинаря `--browser-session`, ничего о просмотре не пишет.

## Эталонные сайты (решения 61–62)
YouTube (видео, микс, live, Shorts, субтитры, дорожки), Twitch (live, запись, клип), украинские uaserials.com, kinoukr.tv, uafilm.pro/klonua.com (плееры Ashdi, Tortuga — допущены владельцем), 1+1 video, Megogo, Takflix; MDN `<video src>`, archive.org, Vimeo, PeerTube, Суспільне, DW; DRM-негативы Shaka/dash.js; Jitsi. eneyida.tv исключён (HDVB). Российское — никогда.

## Факты зоопарка (проверено вживую в 00)
- MSE-байты — fMP4/WebM всегда (даже если сеть даёт `.ts`). Поток собирать **по SourceBuffer** (YouTube режет куски посреди бокса); бывает 1 SB со звуком и видео (PeerTube, Twitch live).
- Обходы хука: **MSE в Dedicated Worker** (Twitch, `MediaSourceHandle`) → обёртка `Worker` в Chrome; **нативный HLS** в Chrome 156 (`<video src=m3u8>`, DW) → `canPlayType(HLS)=""`; `<video src=mp4>` (archive.org в shadow DOM, клипы, реклама).
- Поколение = новый MediaSource на том же элементе (реклама YouTube, следующий ролик, серия Tortuga, озвучка Ashdi — та ещё и сбрасывает позицию в 0). Реклама Ashdi/Tortuga — отдельный `<video src=mp4>`.
- DRM-признак — `setMediaKeys`/`encrypted`, не опрос `requestMediaKeySystemAccess` (Shaka опрашивает 11 систем всегда).
- Буфер плееров: Twitch live ≤1,6 с, YouTube 30–48 с, Ashdi ~64 с, Tortuga ~120 с; на паузе все перестают качать.
- MAIN-мир `document_start` + `all_frames` + `match_about_blank` + `match_origin_as_fallback` опережает скрипты страницы во всех фреймах (Chromium 156, Firefox 158), в воркеры не попадает.
- Native messaging: от хоста ≤1 048 576 байт; от браузера Chrome ≤64 МиБ (исключение, порт жив), Firefox ≥130 МиБ; base64 браузер→хост 200–475 МиБ/с. После disconnect: Chromium EOF→~2 с SIGKILL, Firefox EOF→~3 с SIGTERM→~3 с SIGKILL; при закрытии браузера — только EOF (хост обязан выходить сам). Открытый порт держит SW/event page >6 мин.
- Firefox 147+ читает файл подключения только из `~/.mozilla/native-messaging-hosts` (XDG-путь — нет, bug 2005167); создание `~/.mozilla` корень профиля не переключает.
- Flatpak (Chrome 156): хост в `~/.var/app/<id>/…` работает внутри песочницы; `talk-name=io.github.Bogdan7c.Fastiplayer.*` + D-Bus activation поднимает плеер снаружи песочницы, fd передаётся (~2 ГиБ/с). Glibc рантайма 25.08 = 2.42. Firefox 157+ умеет `xdg-native-messaging-proxy`, Flatpak-Firefox — ещё нет.
- Брендированный Chrome: `--load-extension` нет, но CDP `Extensions.loadUnpacked` (`--enable-unsafe-extension-debugging`) работает. Firefox: BiDi `webExtension.install`.
- Firefox — MV3, один манифест (`service_worker` + `scripts`).
- Блокировщик рекламы — обязательный случай проверки (решение 72; 00c, 08, 09, 21a, 22).

## Связи
Пересылка второго запуска и D-Bus `org.freedesktop.Application`: `mem:app-egui/instance-forwarding-ux13`. Демуксеры fMP4/WebM: `mem:symphonia-demux/iso-bmff-fmp4-s28a-2026-07-22`, `mem:symphonia-demux/matroska-webm-s28b-2026-07-22`. Швы под Windows — как у рендера (`mem:gles2-renderer/render-shell-contract`).

# Ручная web-media acceptance-очередь

Этот прогон проверяет одну настоящую очередь из крупных заявленных классов
совместимости. Он намеренно не пытается перебрать все сайты из stock `yt-dlp`:
один публичный ресурс представляет один transport/container/layout-класс, а
YouTube HDR-ресурс отдельно нагружает полный каталог вкладки URL.

Плейлист находится в
[`docs/web-media-playlist-acceptance.xspf`](web-media-playlist-acceptance.xspf).
Все URL публичные, не требуют cookies и были повторно проверены через exact
`yt-dlp 2026.07.04` 4 августа 2026 года. Это не делает их вечными: удалённый
ресурс может изменить форматы или исчезнуть.

## Что именно входит в очередь

| № | Safe ID | Публичный владелец | Подтверждённая роль |
| ---: | --- | --- | --- |
| 00 | `url-settings-adaptive` | YouTube | Separate A/V, MP4/WebM, H.264/VP9/AV1, AAC/Opus, 60 FPS, HDR10, несколько разрешений |
| 01 | `hls-vod-ts` | Mux test streams | HLS VOD master, пять H.264/AAC разрешений, MPEG-TS segments |
| 02 | `dash-vod-fmp4` | Google Shaka demo assets | DASH VOD, separate A/V, fMP4 и WebM alternatives, H.264/VP9 + AAC/Opus |
| 03 | `progressive-http-proven-audio` | Wikimedia Commons | Маленький progressive Ogg audio-only resource |
| 04 | `hls-live-dvr` | BBC/Akamai testcard | Живой HLS master с несколькими H.264 representations |
| 05 | `progressive-http-matroska-webm` | Wikimedia Commons | 30-секундный progressive WebM VP9/Opus |
| 06 | `dash-live-dvr` | DASH-IF livesim | Dynamic MPD, fMP4 H.264/AAC, двухсекундные segments, минутный DVR |
| 07 | `ism-mss-base-h264-aac-fmp4` | Unified Streaming demo | Static Smooth Streaming H.264/AAC VOD |
| 08 | `progressive-http-iso-bmff` | Internet Archive | Progressive MP4 candidate через site extractor |
| 09 | `hds-f4m-f4f` | Unified Streaming demo | Static HDS/F4M VOD с FLV-family output |
| 10 | `ftp-ftps-progressive` | GNU FTP | FTP progressive Ogg/Vorbis audio-only resource |
| 11 | `hls-vod-fmp4` | Google Shaka demo assets | HLS VOD с `EXT-X-MAP`, fMP4 media, separate alternate audio |
| 12 | `dash-vod-webm` | Google Shaka demo assets | DASH VOD только с WebM VP9/Opus fragments |

Строка 00 — дополнительная и не заменяет ни одну из двенадцати transport rows.
Она нужна для глубокой проверки зависимых selectors вкладки URL.

## Честные ограничения

- Это ручная runtime acceptance, а не hermetic CI и не автоматический `PASS`.
- Публичный ресурс может дать `SOURCE DRIFT`, geo/network failure или временную
  недоступность. Такой результат нельзя записывать как дефект Rustiplayer.
- HLS live закрывает `hls-live-dvr` только если текущий manifest реально
  публикует доступный для seek диапазон. Один лишь флаг `is_live` недостаточен.
- Строка FTP подтверждает exact `ftp` transport. Direct probe
  `ftps://ftp.gnu.org/...` на pinned `yt-dlp 2026.07.04` завершается
  `Unsupported url scheme: "ftps"`; поэтому FTPS-ветка агрегированной
  `ftp-ftps-progressive` строки остаётся отдельным `NOT RUN`, пока не появится
  реально извлекаемый FTPS source contract.
- YouTube, HLS и DASH могут со временем поменять набор codec/resolution/HDR.
  Проверяется фактическая вкладка URL, а не старые format IDs.
- DRM, RTMP wire, RTSP/RTP/MMS, subtitle playback и private-live extractor state
  остаются исключёнными согласно основной compatibility matrix.

## Подготовка

Проверить exact stock `yt-dlp` без пользовательских config/plugins:

```bash
yt-dlp --ignore-config --no-plugin-dirs --version
```

Ожидаемое значение:

```text
2026.07.04
```

Этот probe доказывает identity официального binary и чистую extraction-команду.
Production Rustiplayer намеренно сохраняет обычное поведение system `yt-dlp`:
system config и установленные plugins всё ещё могут влиять на app run. Для
stock-прогона используйте чистую официальную установку без system config и
plugins; один изолированный `XDG_CONFIG_HOME` удаляет только пользовательский
config root и не переопределяет system-wide файлы.

Собрать release-приложение:

```bash
cargo +1.96.0 build --release -p app-egui --locked
```

Создать отдельный config root, чтобы acceptance не смешалась с личной очередью
и настройками:

```bash
acceptance_config_dir="$(mktemp -d)"
```

Каталог временный, но автоматически не удаляется: это позволяет перезапустить
приложение с той же очередью и другим backend preference.

## Запуск

```bash
env \
  XDG_CONFIG_HOME="${acceptance_config_dir}" \
  RUST_LOG=info \
  target/release/rustiplayer \
  docs/web-media-playlist-acceptance.xspf
```

После появления preview подтвердить импорт всей очереди. Проверить, что видны
ровно 13 top-level элементов в документированном порядке, без silent drops.

Первый обязательный прогон выполняется с `video.preferred_backend = software`.
Это даёт самый широкий software decode → HostPlanar upload → WGPU путь. После
него ту же сохранённую очередь можно выборочно повторить с `auto`; hardware-only
прогон имеет смысл только на машине с подходящим VA-API устройством.

## Базовый сценарий каждого элемента

Для каждой строки очереди:

1. Запустить элемент и дождаться исчезновения pending/open состояния.
2. Подтвердить настоящий движущийся video frame либо слышимый audio output.
3. Проиграть не меньше пяти секунд без fatal/panic/render-resource ошибки.
4. Для конечного VOD сделать seek примерно на 30%, затем назад примерно на 10%.
5. Убедиться, что после seek появились новый кадр/звук и изменилась позиция, а
   старый кадр не выдаётся за успешное приземление.
6. Перейти к следующему элементу штатной кнопкой Next.
7. Один раз вернуться Previous и снова Next, проверив правильный stable item.
8. На коротких строках 03 и 05 отдельно дождаться EOF и автоматического перехода.

Порядок специально создаёт переходы progressive → HLS → DASH → audio-only →
live → progressive → live → Smooth → progressive → HDS → FTP → HLS → DASH.

## Полная проверка вкладки настроек потока URL

Вкладка URL проверяется для каждого активного web-media элемента, но полный
набор переключений обязателен на строке 00 и повторяется на строках 01/02/11/12
для provider-specific HLS/DASH вариантов.

### Общая проекция

1. Открыть вкладку URL после exact Installed, а не во время initial import.
2. Проверить безопасный label источника, область применения и preference.
3. Проверить честные значения `live`, `seekable`, `buffering` и
   `refresh-on-reopen`.
4. Убедиться, что вкладка не показывает raw URL, query, headers, cookies,
   format identity или extractor payload.
5. Для direct-media строки 03 допустимо состояние без fake format choices.
6. Один доступный вариант должен оставаться видимым, но не создавать ложный
   выбор.

### Зависимые selectors

На строке 00 последовательно проверить selectors:

1. Режим потока.
2. Codec.
3. Разрешение.
4. FPS.
5. Динамический диапазон SDR/HDR.

После изменения верхнего selector нижние варианты должны перестроиться под
новый prefix. Нельзя выбирать комбинацию, которой нет в фактическом каталоге.
`Автоматически` допустимо для неизвестных FPS/HDR, но не должно маскировать
известное значение активного варианта.

### Переключение во время Playing

1. Запомнить текущую позицию.
2. Выбрать другое разрешение или codec.
3. Проверить надпись `Переключаем поток...` и блокировку остальных selectors.
4. Дождаться нового Installed и первого нового кадра/звука.
5. Проверить сохранение позиции с разумной поправкой на прошедшее время.
6. Убедиться, что Item ID и место элемента в очереди не изменились.

### Переключение во время Paused

1. Поставить воспроизведение на паузу.
2. Выбрать другую доступную ось, например FPS, HDR или resolution.
3. Дождаться завершения controlled reopen.
4. Убедиться, что состояние осталось Paused и очередь не сдвинулась.
5. Продолжить воспроизведение и подтвердить новый кадр/звук.

### HLS/DASH варианты

- На 01 проверить смену HLS TS rendition между низким и высоким разрешением.
- На 02 проверить separate A/V и переход между H.264/fMP4 и VP9/WebM, если оба
  остаются planner-playable на текущей машине.
- На 11 проверить fMP4 rendition и отсутствие выдуманного Cartesian product
  между video и alternate audio.
- На 12 проверить WebM-only каталог без ложного MP4 fallback.
- Active row/option должна быть inert; повторный выбор не запускает reopen.

Любой safe error должен оставлять текущую очередь и уже установленное
воспроизведение в честном состоянии. Сообщение не должно содержать endpoint или
секретный request material.

## Live/DVR

Для строк 04 и 06:

1. Подтвердить старт у безопасного live edge.
2. Дождаться хотя бы одного обновления timeline/range.
3. Сделать seek назад внутри доступного DVR.
4. Подтвердить воспроизведение из прошлого и продолжение обновления диапазона.
5. Подождать сдвига начала окна и попробовать старую позицию.
6. Ожидать typed rejection/adjustment, а не silent clamp или зависание.
7. Вернуться к актуальному live edge и перейти к следующей строке.

Если строка 04 не предоставляет DVR в момент прогона, записать
`SOURCE DRIFT: live without required DVR`; сам HLS live playback можно отметить
отдельно, но заявленная `hls-live-dvr` строка не закрыта.

## Дополнительные прогоны backend/render path

### Auto

Не обязательно повторять всю очередь. Минимум:

- 00: богатый adaptive catalog и возможный codec/backend fallback;
- 01: HLS TS H.264;
- 02: DASH H.264/VP9 switch;
- 05: progressive VP9 WebM;
- 08: progressive MP4 H.264;
- один переход между hardware-compatible и software-selected элементами.

### Hardware-only

Запускать только при успешном VA-API preflight. Поддерживаемый ресурс обязан
дойти до DMA-BUF/WGPU presentation. Неподдерживаемый codec/profile обязан дать
typed rejection без software fallback, panic и поломки следующего элемента.

WGPU FIFO остаётся полным основным прогоном. Другие present modes достаточно
проверить на строках 00 и 05; умножать на них всю очередь не нужно.

## Как записывать результат

Для каждой строки записать один основной статус:

| Статус | Значение |
| --- | --- |
| `PASS` | Ресурс реально открыт, media дошла до presentation/audio output, обязательные действия выполнены |
| `SOURCE DRIFT` | yt-dlp/manifest больше не предоставляет заявленные свойства |
| `SOURCE UNAVAILABLE` | Удалённый сервер, сеть, geo или auth не дали выполнить проверку |
| `PLAYER FAILURE` | Ресурс предоставляет свойства, но Rustiplayer нарушил open/seek/switch/queue contract |
| `UNSUPPORTED AS EXPECTED` | Получен заранее ожидаемый typed backend/profile rejection |
| `NOT RUN` | Строка или обязательное действие не выполнялись |

Не ставить общий `PASS`, если хотя бы одна из двенадцати transport rows имеет
`SOURCE DRIFT`, `SOURCE UNAVAILABLE`, `PLAYER FAILURE` или `NOT RUN`.

При дефекте сохранить safe ID строки, backend preference, действие, видимый
safe error и небольшой redacted log. Raw resolved media URL, cookies, headers и
query-параметры в отчёт не копировать.

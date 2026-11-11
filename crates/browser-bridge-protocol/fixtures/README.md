# Общие фикстуры протокола моста

Один и тот же набор файлов проверяют две стороны договора:
- Rust — `crates/browser-bridge-protocol/tests/fixtures.rs`;
- TypeScript — тесты экстеншена (сессия 07 плана `user/browser-bridge/`).

Если сторона разбирает фикстуру иначе, чем записано в `manifest.json`, это ошибка этой стороны, а не фикстуры.

## Как устроено

| Путь | Что это |
|---|---|
| `manifest.json` | Список случаев: файл, схема разбора, ожидаемый итог; список записанных потоков |
| `control/` | Участок «вход» (native messaging): `valid/` и `invalid/` |
| `session/` | Текстовые сообщения сеанса WebSocket (`page-*` — от экстеншена, `player-*` — от плеера) |
| `data/` | Двоичные рамки сеанса WebSocket (`.bin`) |
| `streams/<имя>/` | Записанный поток: `stream.json` (шаги по порядку + ожидание) и файлы шагов |
| `media/` | Крошечные синтетические медиа (fMP4 H.264, fMP4 AAC, WebM VP9+Opus) и `generate.sh` |
| `message-versions.json` | В какой версии протокола появилось каждое сообщение сеанса (с протокола 1.1) |
| `value-versions.json` | В какой версии протокола появилось каждое ЗНАЧЕНИЕ старого сообщения, у которого значения добавлялись (с протокола 1.3; с 1.4 — и значения нового сообщения `page_reopen_failed`) |
| `page-subtitle-gate.json` | Случаи сторожа для субтитров страницы (протокол 1.5): шаги на проводе и итог — принято (с готовыми документами) или нарушение на шаге N |
| `resource-addresses.json` | Верные и неверные адреса-заглушки номерков ресурсов HLS (протокол 1.1) |
| `generate_fixtures.py` | Генератор всего, кроме `media/`; независимая от Rust реализация формата |

Схемы (`schema` в манифесте):

| Схема | Rust-тип | Направление |
|---|---|---|
| `extension_control` | `ExtensionControlMessage` | экстеншен → посредник → плеер |
| `player_control` | `PlayerControlMessage` | плеер (или посредник) → экстеншен |
| `page_session` | `PageSessionMessage` | экстеншен → плеер, текстовая рамка WebSocket |
| `player_session` | `PlayerSessionMessage` | плеер → экстеншен, текстовая рамка WebSocket |
| `data_frame` | `decode_data_frame` | экстеншен → плеер, двоичная рамка WebSocket |

## Что должна проверить каждая сторона

- `{"result": "ok"}` у JSON — сообщение разбирается и **записывается обратно байт в байт**: компактный JSON, поля в порядке из фикстуры, не-ASCII как есть.
- `{"result": "ok", "frame": {...}}` у рамки — поля заголовка совпадают, и рамка собирается обратно байт в байт. 64-битное число больше 2⁵³ − 1 записано строкой: в TypeScript его читают через `BigInt`.
- `{"result": "error", "kind": "..."}` — разбор отвергает сообщение **именно с этим видом причины**. Имена видов — `MessageErrorKind` и `DataFrameError` в snake_case.
- Записанный поток — шаги проигрываются по порядку через сторожа поколений (`StreamGate`). Принятые байты каждого SourceBuffer равны склейке указанных диапазонов медиафайла; выброшено ровно `dropped_frames` рамок и `dropped_messages` сообщений.
- `resource_bodies` записанного потока — тела ответов рамками вида 2 по `request_id`: смещения рамок одного запроса идут подряд с 0, склейка груза равна склейке диапазонов `ranges` файла `source` (путь от корня фикстур), а `resource_fetch_finished{completed}` называет ровно длину тела. У потоков без запросов список пуст.
- Шаг `{"await_clock_us": N}` — не сообщение и не рамка, а темп записи: страница ждала, пока часы плеера (`clock`) дойдут до N мкс (смена качества посреди просмотра). Сторож его пропускает; проигрыватель записи, говорящий с настоящим плеером, ждёт такой `clock`.

Поток `streams/quality-switch-new-media-source` (сессия 03e) повторяет живую запись смены качества YouTube: каждая смена — новый `MediaSource` (новое поколение); страница объявляет поколение и SourceBuffer, кладёт init-сегменты, сообщает позицию зрителя (`seek_started{page_seek}`) и только потом кладёт коробки — видео с ключевого кадра до позиции, звук с ещё более раннего фрагмента. Плеер обязан начать новое поколение с этой позиции.

## Файл `<video src>` и правила сторожа

Сообщение `file_source_added` (`{"type":"file_source_added","generation":N}`) говорит: в поколении N вместо `MediaSource` у элемента файл; его байты плеер запросит командой `fetch_range`, размер придёт в `range_fetch_finished`. Плеер, не сумевший получить файл окончательно (файл сменился или повторы после сетевых ошибок исчерпаны), отвечает `stream_rejected` с `reason: "file_unavailable"`.

Причина `stream_rejected` `reason: "playback_failed"` (решение 128, внутри 1.0) значит: байты поколения приемлемы, но плеер не смог его установить или играть (старт декодера, прерванная установка, сбой воспроизведения). Страница вправе забрать видео обратно (это делается позже, сессия 11). Известные причины: `protected_content`, `unsupported_codec`, `unparsable_media`, `file_unavailable`, `playback_failed`; любая другая отвергается как неизвестная.

Содержимое поколения выбирается один раз: «ещё не решено» → MSE (первый `source_buffer_added`) или файл. Сторож (`StreamGate`) считает нарушением протокола (сеанс закрывается):
- `file_source_added` повторно в том же поколении;
- `file_source_added` после `source_buffer_added` того же поколения;
- `source_buffer_added` в поколении-файле;
- в поколении-файле любое сообщение только для MSE: `source_buffer_type_changed`, `timestamp_offset_changed`, `source_buffer_removed`, `source_buffer_aborted`, `duration_changed`, `end_of_stream`, и любая двоичная рамка вида 1. Длительность и конец плеер знает из самого файла.

Разрешены в поколении-файле, как и везде: `seek_started`, `page_playback`, `variant_menu_changed`, `range_fetch_finished`, `tab_gone`. Про устаревшее и будущее поколение `file_source_added` подчиняется общим правилам (выбросить / нарушение). Записанный поток `streams/file-source-generation` показывает допустимую последовательность; отвергаемые последовательности проверяют модульные тесты Rust (в фикстурах нарушения сторожа не записаны).

Случай «сообщение длиннее 128 КиБ» в файлах не хранится (он весил бы 128 КиБ). Каждая сторона собирает его в тесте сама: валидное сообщение, добитое пробелами до лимита, принимается, а на байт длиннее — `too_large`.

## HLS (протокол 1.1, сессия 10a1, решение 228)

Новое в 1.1 — только три сообщения (`message-versions.json`):
- страница → плеер `hls_source_added{generation, root}` — поколение `generation` — нативный HLS, корневой плейлист — номерок `root` (`u32`, весь диапазон);
- плеер → страница `fetch_resource{request_id, resource, byte_range, max_bytes}` — дай ресурс по номерку. `byte_range` — метка, а не необязательное поле: `{"kind":"whole"}` (весь ресурс) или `{"kind":"slice","offset":N,"length":M}` (`EXT-X-BYTERANGE`). Поле обязательно: его отсутствие — `missing_field`, `null` — `wrong_value_type`. `length` и `max_bytes` — 1…128 МиБ (`MAX_RESOURCE_FETCH_BYTES`), `offset` ≤ 2⁵³ − 1, кусок длиннее `max_bytes` — `number_out_of_range`;
- страница → плеер `resource_fetch_finished{request_id, outcome}`: `{"kind":"completed","size_bytes":N}` (0…128 МиБ) или `{"kind":"failed","failure":{...}}`, где `failure` — объект с меткой: `network_error`, `http_error{status}` (100…599, иначе `number_out_of_range`), `refused`, `too_large`.

**Номера запросов** у `fetch_resource` и `fetch_range` — одно пространство: плеер ведёт один счётчик на сеанс. **Одновременно** плеер держит не больше 8 незавершённых `fetch_resource` (`MAX_CONCURRENT_RESOURCE_FETCHES`); на лишний экстеншен вправе ответить `refused`. Тело длиннее `max_bytes` экстеншен не отдаёт, а отвечает `too_large`.

**Байты ответа** идут прежними рамками вида 2 (`RangeBytes{request_id, offset}`), формат рамки не менялся. `offset` — смещение **внутри тела ответа** с 0, даже у `slice` (а не смещение в файле на сервере). Что значит рамка, плеер знает по номеру запроса. Сначала все рамки тела, потом `resource_fetch_finished` того же запроса. Пример — `data/valid/range-bytes-resource-body-start.bin` и поток `streams/hls-source-generation` (три параллельных запроса с перемежающимися рамками, кусок файла `slice` со смещениями тела с 0, отказ 404).

**Версия.** Сообщение новее общей версии сеанса — нарушение отправителя. При общей 1.0 страница не шлёт `hls_source_added` и `resource_fetch_finished` (иначе плеер закрывает сеанс кодом 4003), а плеер не шлёт `fetch_resource`; экстеншен на 1.0-плеере оставляет честное «not supported yet». Версию плеер берёт из `hello` того, кто открыл сеанс; таблица версий сообщений — `message-versions.json`.

**Сторож** (`StreamGate`): содержимое поколения выбирается один раз — MSE, файл или HLS. `hls_source_added` принимается только в нерешённом поколении; нарушение — повтор в том же поколении, после `source_buffer_added`, после `file_source_added`. В HLS-поколении нарушение — `file_source_added`, `source_buffer_added`, любое MSE-сообщение (`source_buffer_type_changed`, `timestamp_offset_changed`, `source_buffer_removed`, `source_buffer_aborted`, `duration_changed`, `end_of_stream`) и любая рамка вида 1. Разрешены, как везде: `seek_started`, `page_playback`, `variant_menu_changed`, `ad_state`, `range_fetch_finished`, `resource_fetch_finished`, `tab_gone`. Рамки вида 2 сторож не проверяет — номер запроса сверяет плеер. Устаревшее и будущее поколение — по общим правилам.

**Адрес-заглушка номерка.** В тексте плейлиста фон пишет каждый адрес (варианты, сегменты, `EXT-X-MAP`, ключи) как `https://resource.fastiplayer-bridge.invalid/<N>` (`<N>` — номерок десятичным числом). Разбор строгий, без нормализации: ровно схема `https` в нижнем регистре, ровно этот хост, без порта, имени пользователя, `?` и `#`, путь — `/` и номерок без ведущих нулей (кроме `0`), без `%`-кодирования, знака, пробелов и не-ASCII цифр, не больше `u32`. Всё остальное транспорт плеера отвергает у себя и в протокол не шлёт. Список верных и неверных адресов — `resource-addresses.json`; корневой плейлист потока `streams/hls-source-generation/root.m3u8` уже переписан так.

## HLS live (протокол 1.2, сессия 10a3, решение 232)

Версия протокола — **1.2**. Новое в 1.2 — ровно одно сообщение плеер → страница (`message-versions.json`):

`live_window{generation, edge_us, seekable}` — окно эфира live HLS: край эфира и честное окно перемотки плеера. Все моменты — во времени плеера (то же, что `clock.position_us`), целые микросекунды ≤ 2⁵³ − 1. `seekable` — метка, а не необязательное поле: `{"kind":"none"}` (эфир без DVR, перемотки нет) или `{"kind":"range","start_us":S,"end_us":E}`. Поле обязательно: отсутствие — `missing_field`, `null` — `wrong_value_type`, незнакомая метка — `unknown_value`.

**Инварианты** `start_us ≤ end_us ≤ edge_us` (края включительно, окно в одну точку допустимо). Вывернутое окно (`start_us > end_us`) — `empty_range`, как у вывернутого промежутка `source_buffer_removed`; конец дальше края — `number_out_of_range`. Обе стороны проверяют это и при разборе, и при записи.

**Версия.** Плеер шлёт `live_window` только собеседнику, у которого общая версия включает 1.2, и только для live-поколения HLS. При общей 1.1 его нет, а экстеншен оставляет для live HLS честный отказ «not supported yet». VOD HLS по-прежнему хватает 1.1.

**Частота.** Не чаще раза в `MIN_LIVE_WINDOW_INTERVAL_MS` (1000 мс) на поколение: частые изменения плеер сводит подряд, последнее окно не теряет, первое окно нового поколения шлёт сразу. Экстеншен частоту не проверяет.

Фикстуры — `session/valid/player-live-window-*`, `session/invalid/player-live-window-*` и `session/invalid/player-shape-live-window-*`.

## Плеер сайта сломался (протокол 1.3, сессия 11a, решения 236–237)

Версия протокола — **1.3**. Новых сообщений нет (`message-versions.json` не менялся). Новое — одно ЗНАЧЕНИЕ старого сообщения страница → плеер:

`tab_gone{reason: "page_player_failed"}` — вкладка на месте, но плеер сайта сломался во время передачи (ошибка элемента не прошла, страница сняла источник после ошибки). Для плеера это тот же конец, что и закрытая вкладка (решение 42): доиграть скачанное и сказать об этом своими словами («The page's player stopped…»).

**Версия значения.** Таблица `value-versions.json` — «сообщение.поле» → значение → версия; она полная по значениям (`closed`, `navigated_away`, `browser_closing` — 1.0, `page_player_failed` — 1.3). Rust — `TabGoneReason::introduced_in`, TypeScript — `tabGoneReasonIntroducedIn`. При общей версии ниже 1.3 экстеншен шлёт вместо нового значения `navigated_away`; плеер, получив `page_player_failed` на сеансе ниже 1.3, закрывает связь как при нарушении (4003): «договорились — значит строго» и для значений (`PageSessionMessage::required_version`, сторож `StreamGate`).

**Метка строгая.** Ровно `page_player_failed`: другой регистр, дефисы, пробелы — `unknown_value`; внешняя форма serde (`{"page_player_failed":null}`) — `wrong_value_type`.

**Код закрытия 4010 `viewer_returned_to_page`** (решение 236): зритель нажал «Return» на заглушке страницы; видео играет на странице дальше. Закрывает экстеншен; плеер закрывает окно сразу и молча — без доигрывания, сообщения и прощания `return_to_page`. Коды закрытия не версионируются (4009 добавлен так же): старый плеер видит незнакомый 4xxx и кончает сеанс молча, как на 1000. Заняты коды 4000–4011 (4011 — протокол 1.9, ниже).

Фикстуры — `session/valid/page-tab-gone-page-player-failed.json`, `session/invalid/page-tab-gone-*` и `session/invalid/page-shape-tab-gone-page-player-failed-as-map.json`.

## Возврат вкладки (протокол 1.4, сессия 11b1, решения 240–241)

Версия протокола — **1.4**. Четыре новых сообщения (все `1.4` в `message-versions.json`):

- страница → плеер `{"type":"page_left","reason":"closed"}` — страница больше не источник, но связь остаётся открытой («сирота»); причины те же, что у `tab_gone`. Сеанс НЕ кончается;
- страница → плеер `{"type":"page_returned"}` — страница вернулась, тот же сеанс продолжается, дальше идёт новое поколение;
- страница → плеер `{"type":"page_reopen_failed","reason":"no_browser_window"}` — вернуть не вышло; значения `no_browser_window`, `page_did_not_load`, `not_the_same_video`, `video_did_not_start` (таблица `value-versions.json`, ключ `page_reopen_failed.reason`); затем экстеншен закрывает соединение кодом 4002;
- плеер → страница `{"type":"reopen_page","position_us":123}` — зритель нажал play, пока страница ушла: вернуть страницу на этой секунде (время плеера).

`tab_gone` не менялся и по-прежнему означает конец сеанса, когда страницу вернуть нельзя. Коды закрытия не менялись. Значения `page_left.reason` — те же `TabGoneReason`, отдельной записи в `value-versions.json` у них нет (их версии — в `tab_gone.reason`).

Правила порядка (сторож плеера, `StreamGate`): после `page_left` разрешены только `page_returned`, `page_reopen_failed` и `tab_gone`; байты и любые другие сообщения — нарушение (`PageIsAway`); `page_returned`/`page_reopen_failed` при странице на месте — нарушение (`PageIsPresent`); поколение, ушедшее со страницей, остаётся устаревшим и после возврата, следующее — ровно на 1 больше. Рамки `<video src>` сторож не разбирает: сервер обязан спрашивать `admit_range_bytes()`.

Фикстуры — `session/valid/page-page-left-*`, `page-page-returned`, `page-page-reopen-failed-*`, `player-reopen-page*`; `session/invalid/page-left-*`, `page-reopen-failed-*`, `page-returned-with-field`, `player-reopen-page-*` и `page-shape-page-left-*`/`page-shape-page-reopen-failed-*`.

## Строгость формы (сессия 07)

У каждого значения ровно одна форма JSON — та, что пишет отправитель. Знакомые данные в чужой форме отвергаются видом `wrong_value_type` (значение не того типа), где бы они ни стояли — в корне или в любом вложенном объекте:
- структура (`protocol`, `page`, `hints`, `menu`, вариант меню, `player` в `hello_accepted`) — только объект; массив полей по порядку (`"protocol":[1,0]`) — `wrong_value_type`;
- перечисление с меткой (`kind` у `duration`, `position`, `end`, `cause`, `request`, `outcome`, `resource_size`, `in_reply_to`, `reason`, а в 1.1 — `byte_range` и `failure` у `resource_fetch_finished`, в 1.2 — `seekable` у `live_window`) — только объект, а метка — только строка. Массив (`"end":["at",5]`) и метка числом (`"kind":0`, `"kind":7`) — `wrong_value_type`; номер вне числа вариантов — тоже `wrong_value_type`, а не `number_out_of_range`;
- перечисление-метка (`intent`, `outcome`, `update`, `error`, `state`, `intake`, `resume`, `reason`, `selection`, `failure`, `kind` меню и `select_variant`) — только строка; число (`"state":0`) и внешняя форма serde (`{"closed":null}`) — `wrong_value_type`.

Сначала проверяется форма, потом содержимое: незнакомая метка-строка по-прежнему `unknown_value`, лишнее/повторное/недостающее поле внутри вложенного объекта — `unknown_field`/`duplicate_field`/`missing_field`. Случаи — файлы `control/invalid/shape-*`, `session/invalid/page-shape-*` и `session/invalid/player-shape-*`. В Rust это обеспечивает `src/strict_shape.rs`: производный разбор serde сам по себе принимает все три чужие формы.

## Как пересобрать

```sh
crates/browser-bridge-protocol/fixtures/media/generate.sh   # только если меняются медиа
crates/browser-bridge-protocol/fixtures/generate_fixtures.py
cargo test -p browser-bridge-protocol
```

Медиа синтетические (`testsrc2` и синус через ffmpeg), чужого контента нет. Перегенерация на той же версии ffmpeg даёт те же байты.

## Кто «наш» экстеншен (`extension-identity.json`, сессия 05)

- `native_host_name` — имя посредника для `runtime.connectNative()` и имя файла подключения.
- `chromium_development.manifest_key` — публичный ключ (DER SPKI в base64) для поля `key` манифеста экстеншена. С ним распакованный экстеншен в Chrome всегда получает `extension_id`: SHA-256 ключа, первые 16 байт, шестнадцатеричные цифры `0`–`f` записаны буквами `a`–`p`. Закрытый ключ не нужен (магазин подписывает сам) и не хранится.
- `firefox.gecko_id` — значение `browser_specific_settings.gecko.id`.

Rust-константы (`extension_identity.rs`) сверяет с этим файлом `tests/extension_identity.rs`, включая вывод ID Chrome из ключа. Посредник пускает только эти ID; ID из Chrome Web Store добавится в сессии 24.

## Субтитры страницы (протокол 1.5, сессия 13b1, решения 270–274)

Версия протокола — **1.5**. Новое — одно сообщение страница → плеер (`message-versions.json`) и один вид двоичной рамки:

- `{"type":"subtitle_track_offered","track":1,"language":{"kind":"known","tag":"en"},"label":{"kind":"text","text":"English"},"format_hint":"web_vtt","origin":"page_download","page_display":"showing","byte_length":1234}` — заголовок документа дорожки субтитров. Поля:
  - `track` — номер дорожки на сеанс, `u32` от 1; тот же номер ещё раз — **замена** документа (в том числе недокачанного);
  - `language` — метка, а не необязательное поле: `{"kind":"unknown"}` или `{"kind":"known","tag":T}`, где `T` — форма BCP-47: 1…35 байт ASCII `[A-Za-z0-9-]`, дефис только между частями (не первым, не последним, не два подряд — иначе `text_forbidden_character`), регистр не нормализуется;
  - `label` — тоже метка: `{"kind":"none"}` или `{"kind":"text","text":L}`, `L` — текст для человека 1…256 байт UTF-8 (правила `text.rs`: без управляющих, разрывов строки и переворота направления);
  - `format_hint` — `web_vtt | srt | ttml | srv3 | json3 | unknown` (подсказка: плеер всё равно узнаёт формат по содержимому);
  - `origin` — `page_download | track_element | text_track_cues`;
  - `page_display` — `showing | not_showing` («скачала — значит включено», решение 271);
  - `byte_length` — точная длина документа, 1…8 МиБ (`MAX_PAGE_SUBTITLE_DOCUMENT_BYTES`).
  Все поля обязательны (отсутствие — `missing_field`, `null` — `wrong_value_type`), лишние поля и внутри меток — `unknown_field`. Сообщение не привязано к поколению потока.
- рамка **вида 3** — байты документа: `@0 kind=3`, `@1 flags=0`, `@2 reserved u16=0`, `@4 track u32 (≥ 1)`, `@8 offset u64 (≤ 2⁵³ − 1)`, `@16..24 reserved = 0`, груз 1 Б…1 МиБ. В манифесте её поля — `{"kind":"page_subtitle_bytes","track":N,"offset":N,"payload_len":N}`. Прежняя фикстура «незнакомый вид» теперь — `unknown-kind-4`.

**Правила сторожа** (`StreamGate`, на сеанс): рамка вида 3 допустима только для дорожки с открытым (недокачанным) предложением, её `offset` — ровно сколько байт этого предложения уже пришло, а `offset + len ≤ byte_length`. Документ готов, когда пришло ровно `byte_length` байт (отдельного «готово» нет); рамка для готового документа — нарушение. Разных `track` за сеанс не больше 64 (`MAX_PAGE_SUBTITLE_TRACKS`), повторное предложение места не занимает. При общей версии ниже 1.5 и сообщение (`message_not_in_agreed_version`), и рамка вида 3 (`data_frame_not_in_agreed_version`) — нарушение. Пока страница ушла (1.4, `page_left`), нельзя ни предлагать, ни слать байты (`page_is_away`). Байты документа не трогают поколений и SourceBuffer-ов. Любое нарушение — закрытие 4003.

**Случаи сторожа** — `page-subtitle-gate.json`: `{"cases":[{"name", "agreed":"1.5", "steps":[{"text":"<JSON на проводе>"} | {"binary_hex":"<рамка>"}], "expect": {"result":"ok","documents":[{"track":N,"utf8":"…"}]} | {"result":"violation","at_step":N,"violation":"<имя>"}}]}`. Имена нарушений — варианты `StreamViolation` в snake_case. У `ok` в `documents` — ровно готовые документы (недокачанные не перечисляются). Сторож есть только у плеера (Rust, `tests/protocol_1_5.rs`); TypeScript проверяет, что каждый шаг разбирается его кодеками.

Фикстуры — `session/valid/page-subtitle-track-offered-*`, `session/invalid/page-subtitle-track-offered-*`, `data/valid/page-subtitle-*`, `data/invalid/page-subtitle-*`.

## Показать страницу (протокол 1.6, сессия 13c, решение 278)

Версия протокола — **1.6**. Новое — одно сообщение плеер → страница `show_page {}` (`message-versions.json`): зритель хочет увидеть страницу (например, сменить там качество). Экстеншен делает вкладку сеанса активной, просит её окно выйти вперёд и подсвечивает окно; ответа нет, сеанс и поколения не меняются. При общей версии ниже 1.6 плеер `show_page` не шлёт. Фикстуры — `session/valid/player-show-page.json`, `session/invalid/player-show-page-unknown-field.json`; договор версий и согласование — `tests/protocol_1_6.rs`.

## Варианты страницы (протокол 1.7, сессия 14a, решения 279–283)

Версия протокола — **1.7**. Новое (`message-versions.json`): страница → плеер `page_navigation {next_episode: "available" | "unavailable"}` и `subtitle_preference {choice}`; плеер → страница `subtitle_choice {choice}`. `choice` — `{"kind":"page_track","track":N}` (N от 1, как в `subtitle_track_offered`) или `{"kind":"off"}`; форма строгая (лишнее поле, чужой вид, номер 0 — отказ). Сообщения 1.0 `variant_menu_changed`, `select_variant`, `next_episode` не менялись. При общей версии ниже 1.7 плеер новых сообщений не шлёт, а сторож отвергает входящие. Фикстуры — `session/valid/page-navigation-*`, `page-subtitle-preference-*`, `player-subtitle-choice-*` и `session/invalid/` с теми же префиксами; договор — `tests/protocol_1_7.rs`.

## Исход выбора на странице (протокол 1.8, сессия 15c2, решения 298, 301)

Версия протокола — **1.8**. Новое — одно сообщение страница → плеер `variant_outcome {request, outcome}` (`message-versions.json`): что страница сделала с выбором зрителя. `request` — `{"kind":"select_variant","menu":"audio","variant_id":"dub-b"}` (те же меню и пункт, что в `select_variant`) или `{"kind":"next_episode"}`; `outcome` — `done`, `no_such_episode` (у новой озвучки нет этой серии, решение 295), `person_only` (сайт принимает только настоящего человека) или `failed`. Шлётся только исход выбора зрителя, не памяти выбора экстеншена. При общей версии ниже 1.8 экстеншен его не шлёт, а сторож отвергает входящий. Фикстуры — `session/valid/page-variant-outcome-*` и `session/invalid/page-variant-outcome-*`; договор — `tests/protocol_1_8.rs`.

## Смена серии на той же странице (протокол 1.9, сессия 16a, решения 303, 306–308)

Версия протокола — **1.9**. Новое (`message-versions.json`) — два сообщения страница → плеер, оба вне поколений:
- `awaiting_next_episode {patience_ms}` — серия у нас доиграла, страница сняла источник, экстеншен ждёт новый не дольше `patience_ms` миллисекунд (любое `u32`; отрицательное и больше `u32` — `number_out_of_range`, дробное, строка и `null` — `wrong_value_type`);
- `page_title_changed {title}` — заголовок вкладки сменился. Правило то же, что у `title` в `open_session`: до 1024 байт UTF-8, без управляющих символов и переворота направления, **пустой допустим** (страница без `<title>`). Чистит и режет заголовок экстеншен (`sanitizePageTitle`) до отправки.

**Код закрытия 4011 `next_episode_not_started`**: новый источник не пришёл за срок ожидания; плеер пишет «The page didn't start the next episode» и закрывает окно без досмотра. Код на проводе не версионируется, но экстеншен закрывает им связь только при общей версии не ниже 1.9, а со старым плеером — прежним 4009. При общей версии ниже 1.9 экстеншен новых сообщений не шлёт, а сторож отвергает входящие; пока страница ушла (`page_left`), они — нарушение. Фикстуры — `session/valid/page-awaiting-next-episode-*`, `page-title-changed-*` и `session/invalid/` с теми же префиксами; договор — `tests/protocol_1_9.rs`.

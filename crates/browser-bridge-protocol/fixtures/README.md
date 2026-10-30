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

## Файл `<video src>` и правила сторожа

Сообщение `file_source_added` (`{"type":"file_source_added","generation":N}`) говорит: в поколении N вместо `MediaSource` у элемента файл; его байты плеер запросит командой `fetch_range`, размер придёт в `range_fetch_finished`. Плеер, не сумевший получить файл окончательно (файл сменился или повторы после сетевых ошибок исчерпаны), отвечает `stream_rejected` с `reason: "file_unavailable"`.

Содержимое поколения выбирается один раз: «ещё не решено» → MSE (первый `source_buffer_added`) или файл. Сторож (`StreamGate`) считает нарушением протокола (сеанс закрывается):
- `file_source_added` повторно в том же поколении;
- `file_source_added` после `source_buffer_added` того же поколения;
- `source_buffer_added` в поколении-файле;
- в поколении-файле любое сообщение только для MSE: `source_buffer_type_changed`, `timestamp_offset_changed`, `source_buffer_removed`, `source_buffer_aborted`, `duration_changed`, `end_of_stream`, и любая двоичная рамка вида 1. Длительность и конец плеер знает из самого файла.

Разрешены в поколении-файле, как и везде: `seek_started`, `page_playback`, `variant_menu_changed`, `range_fetch_finished`, `tab_gone`. Про устаревшее и будущее поколение `file_source_added` подчиняется общим правилам (выбросить / нарушение). Записанный поток `streams/file-source-generation` показывает допустимую последовательность; отвергаемые последовательности проверяют модульные тесты Rust (в фикстурах нарушения сторожа не записаны).

Случай «сообщение длиннее 128 КиБ» в файлах не хранится (он весил бы 128 КиБ). Каждая сторона собирает его в тесте сама: валидное сообщение, добитое пробелами до лимита, принимается, а на байт длиннее — `too_large`.

## Как пересобрать

```sh
crates/browser-bridge-protocol/fixtures/media/generate.sh   # только если меняются медиа
crates/browser-bridge-protocol/fixtures/generate_fixtures.py
cargo test -p browser-bridge-protocol
```

Медиа синтетические (`testsrc2` и синус через ffmpeg), чужого контента нет. Перегенерация на той же версии ffmpeg даёт те же байты.

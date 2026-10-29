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

Случай «сообщение длиннее 128 КиБ» в файлах не хранится (он весил бы 128 КиБ). Каждая сторона собирает его в тесте сама: валидное сообщение, добитое пробелами до лимита, принимается, а на байт длиннее — `too_large`.

## Как пересобрать

```sh
crates/browser-bridge-protocol/fixtures/media/generate.sh   # только если меняются медиа
crates/browser-bridge-protocol/fixtures/generate_fixtures.py
cargo test -p browser-bridge-protocol
```

Медиа синтетические (`testsrc2` и синус через ffmpeg), чужого контента нет. Перегенерация на той же версии ffmpeg даёт те же байты.

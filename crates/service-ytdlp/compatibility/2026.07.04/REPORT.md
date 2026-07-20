# yt-dlp 2026.07.04 compatibility inventory

Статус: S00 inventory завершён как checked-in доказательная граница. Runtime
production playback не переключён на новый профиль и не изменён.

Machine-readable источник истины — `profile.json`. Этот отчёт объясняет решения,
но не заменяет manifest и focused checks.

## Source fingerprint

Профиль привязан к официальному release/tag `2026.07.04`:

- commit: `fdec00e0bf530dc6c3cc7b1dd780e95d9ae460e9`;
- tree: `b14ea6bf92e81a98bdcf652f5e46977c1ee593cc`;
- наблюдённый 2026-07-20 SHA-256 GitHub source archive:
  `7fb7ca0509dd8f21263246d3d749a346e049fa9d3cfdef072e05c7bbd88d6fc0`;
- локально наблюдён `/usr/bin/yt-dlp --version = 2026.07.04`;
- локальный executable SHA-256:
  `ed88c0fafaad8f242357af30d8f33ac0999a8f0b45c1ad8088080015e65f0061`.

Основные upstream anchors: public `InfoExtractor` result/format documentation,
`YoutubeDL._format_fields`, `YoutubeDL.sanitize_info`, `PROTOCOL_MAP` и CLI
parser из pinned source tree. Archive hash является воспроизводимой локальной
проверкой конкретной загрузки; canonical source identity задают tag, commit и
tree.

## Final CLI profiles

Hermetic inventory:

```text
yt-dlp --ignore-config --no-plugin-dirs --quiet --no-warnings --simulate --dump-single-json --no-playlist <URL>
```

Hermetic selected result:

```text
yt-dlp --ignore-config --no-plugin-dirs --quiet --no-warnings --simulate --dump-single-json --no-playlist --format <SELECTOR> <URL>
```

`--ignore-config` изолирует portable/home/user/system config. В этой версии
реальный CLI reset plugin search — `--no-plugin-dirs`; upstream также понимает
непустой `YTDLP_NO_PLUGINS`, но профиль не полагается только на environment.
`--simulate` явно запрещает video download и disk writes. `--dump-single-json`
сам включает simulation, но explicit `--simulate` оставлен safety-critical
аргументом. `--skip-download` намеренно не используется: upstream разрешает ему
писать связанные metadata-файлы.

Rustiplayer-owned argv не содержит и не запрашивает:

- `--no-simulate`, download или output;
- write/print-to-file metadata, subtitle, thumbnail, link или page behavior;
- `--exec`/`--exec-before-download`;
- `--use-postprocessor` или postprocessing presets;
- `--mark-watched`;
- cookie file/browser input.

Это доказательство относится к app-owned arguments hermetic profile. Оно не
является обещанием, что произвольный extractor не выполнит network requests,
необходимые для extraction.

## Manual opt-in trust boundary

Текущий production process owner запускает тот же extraction suffix без
`--ignore-config` и `--no-plugin-dirs`. Этот режим сохранён в manifest отдельно
как `manual_opt_in_inventory`, а не назван hermetic.

Произвольный trusted user config может добавить write/exec/postprocessor,
mark-watched, authentication или другое поведение. Все plugins импортируются как
Python code без upstream safety checks и могут иметь side effects уже при
импорте. Эти side effects находятся вне Rustiplayer guarantee.

`--cookies FILE` означает не только чтение: pinned upstream описывает файл как
источник, в который cookie jar также dump-ится. Поэтому user-owned cookie jar
может быть обновлён самим system `yt-dlp`. Hermetic profile cookie file не
загружает; manual opt-in обязан считать его user-owned mutable state.

## Serialization boundary

`YoutubeDL.extract_info()` сам по себе не гарантирует JSON-serializable обычный
dict. CLI `--dump-single-json` проходит через `YoutubeDL.sanitize_info`.
Sanitizer:

- материализует dict и поддерживаемые list/tuple/set/LazyList;
- превращает `ImpersonateTarget` в строку;
- сохраняет scalar JSON values;
- превращает любой иной Python object в `repr(...)`.

Последний пункт критичен: наличие строки в JSON не доказывает, что request можно
повторить. Generator, WebSocket response, thread-coupled refresh object или
другой `repr` классифицируется `RequiresLiveExtractorState`, а не
`PublicSerializable`.

Классы manifest-а:

- `PublicSerializable` — документированное public поле с воспроизводимой
  JSON-формой в указанном подмножестве;
- `PrivateSerializablePinned` — поле наблюдается в pinned source/JSON, но
  upstream помечает его internal/private; оно не становится публичным API;
- `RequiresLiveExtractorState` — JSON потерял объектную семантику или path
  требует живого Python/extractor state.

`downloader_options` — internal поле и никогда не исполняется Rustiplayer.
`downloader_options.ws`, `http_dash_segments_generator`, `niconico_live`,
`fc2_live`, `websocket_frag`, `_bunnycdn_ping_data` и
`_cookie_refresh_params` исключены. BunnyCDN/Soop downloaders сами названы
upstream private и зависят от background ping/cookie refresh state.

## Result topology

Inventory фиксирует пять public result shapes:

- missing `_type` либо `_type = video`;
- `playlist`;
- `multi_video`;
- `url`;
- `url_transparent`.

`url` и `url_transparent` являются delegation, не collection. Transparent
wrapper имеет отдельную upstream merge semantics. `multi_video` одновременно
должен удовлетворять video fields и иметь bounded concrete `entries`.
Generator/unrecognized iterable в `entries` не принимается.

## `formats` и `requested_formats`

`formats` — extractor inventory. Только он является входом будущего UI/candidate
inventory.

`requested_formats` — результат compound format selection/reconstruction.
Он содержит фактические выбранные A/V components только для merge path.
Он не является inventory комбинаций, не подменяет `formats` и не создаётся
искусственно. `format_id` snapshot-local: новое extraction generation обязано
искать semantic match и повторно проверять attributes.

Fixture `format-inventory.json` содержит тринадцать inventory rows и только два
выбранных components. Focused test проверяет, что requested IDs являются
подмножеством inventory и что списки не совпадают.

## Request material decisions

Поддерживаемая сериализуемая основа:

- `url`, `manifest_url`;
- bounded concrete `fragments[]`, `fragment_base_url`;
- inline `hls_media_playlist_data`;
- secret-scoped `http_headers` и `cookies`;
- HLS/DASH segment/key query additions;
- AES-128-only `hls_aes`;
- public RTMP request fields для будущей S39.

Ограничения:

- `request_data` исключает row, пока отдельное решение не докажет exact byte
  semantics: bytes после generic sanitizer становятся lossy `repr`;
- `impersonate` исключает row: строка после sanitization не доказывает наличие и
  реализацию browser fingerprint;
- `downloader_options` не исполняется;
- private provider request state исключает row и требует отдельной S40P-card.

Secret fixtures содержат только fixed redaction markers. Procedure optional
network captures находится в `CAPTURES.md`.

## Protocol aliases

Manifest хранит exact aliases без схлопывания неизвестных строк:

- HTTP: `http`, `https`;
- FTP: `ftp`, `ftps`;
- HLS: `m3u8`, `m3u8_native`, internal handoff `m3u8_frag_urls`;
- DASH: `http_dash_segments`, internal handoff `dash_frag_urls`;
- RTMP family: `rtmp`, `rtmpe`, `rtmp_ffmpeg`;
- отдельные `ism` и `f4m`.

`http_dash_segments_generator` не является alias обычного serializable DASH
path: pinned downloader умеет re-extract при строковом sanitized generator
marker, а Rustiplayer не сохраняет живое состояние.

## Target rows

Target rows не обещают, что runtime уже реализован. Они задают конечные строки,
для которых указаны future sessions и synthetic fixture identity:

- progressive HTTP(S): ISO BMFF, Matroska/WebM и текущие доказанные audio
  containers/codecs;
- HLS VOD/live: MPEG-TS и fMP4/CMAF;
- DASH VOD/live: fMP4/CMAF и WebM;
- ISM/MSS fMP4;
- FTP/FTPS progressive на уже доказанных container/codec families;
- HDS/F4M/F4F;
- RTMP family с public serialized request fields.

Новые codec families S00 не добавляет. Строка `existing_major_web_video` означает
только VP8/VP9/AV1/H.264/H.265, уже присутствующие в neutral model. Audio —
только текущий proven native set из implementation plan.

Каждая Target row связана с future session(s) и `fixture_id`; focused test
проверяет обе ссылки.

## Explicit exclusions and provisional gaps

Жёстко исключены:

- RTSP, standalone RTP, MMS/MMST/MMSH;
- DRM и encrypted-only paths;
- generator/private live-state paths;
- arbitrary third-party plugin contract;
- `downloader_options` execution;
- provider paths, где нужен upstream private API.

Release `2026.07.04` сам удалил RTSP/MMS support из yt-dlp. Это не превращает
их в unknown target, а подтверждает exclusion.

MPEG-PS, AVI, ASF/WMV/WMA и rare codecs исследованы как provisional gaps.
Checked-in S00 corpus не содержит достаточного evidence, а текущая production
matrix не доказывает end-to-end path. Поэтому именно revision v1 помечает их
`ProfileExcludedProvisional`. Это не вечный запрет: отдельный profile extension
может изменить статус с corpus, capability, demux/decode и fixture evidence.

## Unknown identity

Unknown `protocol`, `ext`, `container`, `vcodec`, `acodec` сохраняется verbatim
до 256 UTF-8 bytes. Превышение bound либо неизвестная комбинация даёт будущий
typed `IncompatibleYtDlpContract`; значение не исчезает, не превращается в
fallback и не мутирует player/queue. Fixture `future-unknown-identity` закрепляет
сохранение raw identity.

## STOP audit

Ни одна Target row в manifest-е не требует generator, live Python object либо
upstream private provider API. Такие paths находятся в exclusions. Поэтому S00
не достигает STOP-condition и не требует расширения production scope.

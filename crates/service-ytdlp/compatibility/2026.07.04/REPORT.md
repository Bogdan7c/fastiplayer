# yt-dlp 2026.07.04 compatibility inventory

Статус: S00 inventory завершён как checked-in доказательная граница. S15
включил bounded topology extraction; candidate/playback runtime продолжает
использовать прежний single-item profile.

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

Hermetic topology:

```text
yt-dlp --ignore-config --no-plugin-dirs --quiet --no-warnings --simulate --dump-json --dump-single-json --flat-playlist --lazy-playlist <URL>
```

Topology profile намеренно сочетает два official dump режима:

- `--dump-json` публикует child entries line-by-line по мере lazy enumeration;
- `--dump-single-json` завершает output authoritative root object-ом;
- `--flat-playlist` не выполняет full extraction formats для child URL results;
- `--lazy-playlist` отключает `n_entries`, поэтому Rustiplayer никогда не
  использует это поле для allocation, progress или completeness.

Production topology suffix не содержит hermetic prefix и продолжает читать
trusted system config/plugins. Hermetic fake/fixture tests изолируют process
environment и проверяют exact ordered argv.

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
Единственное узкое исключение — bounded positive integer
`downloader_options.http_chunk_size`: оно нормализуется в нейтральный предел
одного HTTP Range-запроса, а не передаётся downloader-у как executable config.
Любой иной ключ остаётся fail-closed. `downloader_options.ws`,
`http_dash_segments_generator`, `niconico_live`,
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
- `downloader_options` не исполняется; exact bounded `http_chunk_size`
  переносится только как neutral HTTP Range policy, остальные shapes исключают row;
- private provider request state исключает row. S40 доказал отсутствие
  `PublicSerializable` special-provider target rows, поэтому ни одной
  `S40P-*` card не создано. Будущее расширение сначала обязано добавить в S00
  отдельную public-serializable target row с stable ID и exact fixture, а уже
  затем пройти обсуждаемую owner-specific `S40P-<stable-row-id>` card.

## S40 special-provider gate result

S40 завершён как доказанный no-op. `PublicSerializable` у scalar поля
`protocol` означает только воспроизводимую JSON-форму самой строки и не является
provider admission. Все текущие S00 target rows уже принадлежат конкретным
S22–S39 transport/provider sessions; ни одна row не ссылается на S40 или
`S40P-*`.

Special identities `bunnycdn`, `soopvod`, `niconico_live`, `fc2_live` и
`websocket_frag` остаются в exact alias family
`special_private_state_excluded`. Для них нет отдельной S00 target row и exact
deterministic transport-to-demux fixture. Representative checked-in fixture
дополнительно доказывает lossy WebSocket `repr` и private refresh/ping state,
которые не образуют переносимый descriptor.

Поэтому S40 не добавляет provider owner, descriptor schema,
transport-to-demux mapping, dependency, Python helper или IPC. S41 имеет только
обычную dependency на завершённый S40 gate и не получает дополнительных
`S40P-*` dependencies.

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
- ISM/MSS base/VOD fMP4 только для exact H.264/AAC evidence;
- FTP/FTPS progressive на уже доказанных container/codec families;
- HDS/F4M/F4F;
- RTMP family с public serialized request fields.

Новые codec families S00 не добавляет. Строка `existing_major_web_video` означает
только VP8/VP9/AV1/H.264/H.265, уже присутствующие в neutral model. Audio —
только текущий proven native set из implementation plan.

S30 уточняет один exact codec внутри уже существующей `adpcm` family:
`A_ADPCM_SWF` принадлежит project-owned `audio::SwfAdpcmDecoder`, поддерживает
только mono/stereo и 2/3/4/5-bit codes. Полный block содержит 4096 frames, но
последний block packet-а может быть короче: после channel headers принимаются
только целые interleaved channel code groups и нулевой byte-alignment tail. Это
не wildcard для похожих строк и не разрешение подменять SWF layout на MS/IMA
ADPCM. Поле `cross_packet_state: false` закрепляет reset/seek invariant decoder-а.

Reference arithmetic и partial-final sample counting сверены с primary FFmpeg
implementation: [`libavcodec/adpcm.c`](https://ffmpeg.org/doxygen/trunk/adpcm_8c_source.html).
Delta складывается из отдельно сдвинутых step contributions с integer rounding
на каждом сдвиге; алгебраически сворачивать это в одно умножение нельзя.

Каждая Target row связана с future session(s) и `fixture_id`; focused test
проверяет обе ссылки.

S36A заменяет прежнюю aggregate row `ism-mss-fmp4` на стабильную exact row
`ism-mss-base-h264-aac-fmp4`. Existing fixture `target-ism-fmp4` не изменён:
он доказывает только serialized `ism` manifest identity, fMP4 и codec identities
`avc1.640028`/`mp4a.40.2`. Поэтому Target ссылается на отдельные narrow profiles
`ism-base-video-h264` и `ism-base-audio-aac`, а не на все major web video и
proven native audio families.

Остальные уже известные video families (`vp8`, `vp9`, `av1`, `h265`) и audio
families (`adpcm`, `alac`, `flac`, `mp1`, `mp2`, `mp3`, `pcm`, `vorbis`,
`opus`) перечислены exact provisional sets. Их promotion требует отдельного
profile extension, отдельной implementation card и собственного exact fixture,
а не переиспользования H.264/AAC evidence.

Approved ISM live/DVR Target row в S00 отсутствует. `ism-mss-live-dvr` остаётся
`ProfileExcludedProvisional`; S36A не создаёт `S36L-*` dependency/card.
Будущее promotion live/DVR также требует отдельной card и exact live fixture.

## S39 exact RTMP variant gate

Агрегированная Target row `rtmp-family-flv` остаётся только inventory evidence:
её transport намеренно называется
`rtmp_rtmpe_or_rtmp_ffmpeg_identity_only`. Synthetic format и request-material
fixtures доказывают, что pinned yt-dlp сериализует identity, FLV codec hints и
public RTMP-поля. Они не содержат локального RTMP server-а, handshake/chunk/
message/play exchange или зашифрованного RTMPE payload-а и поэтому не являются
wire approval ни для одного exact variant.

S39 не добавляет provider, dependency, S31L binding или S15A capability.
`rtmp` и `rtmpe` остаются `ProfileExcludedProvisional`: promotion требует
отдельной exact Target row и deterministic local wire fixture, а для `rtmpe`
ещё и настоящего crypto handshake/encrypted payload evidence. `rtmp_ffmpeg`
остаётся жёстким `ProfileExcluded`, потому что это downloader identity, а не
wire protocol; hidden FFmpeg fallback запрещён.

`rtmps`, `rtmpt` и `rtmpte` также записаны отдельными provisional exclusions.
Они не являются aliases обычного `rtmp`: будущая promotion каждого variant
требует собственной S00 row и TLS/tunnel/crypto fixture. Focused S39 test
проверяет, что aggregate identity-only row не повышается до exact Target и что
каждый названный variant остаётся в exclusion namespace.

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

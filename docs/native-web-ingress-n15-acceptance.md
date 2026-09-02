# N15 — public acceptance и performance

Session N15 завершена 2026-09-02 на code commit `c330ba74`. Exact
`user/web-media-playlist-acceptance.xspf` содержит 13 строк и имеет SHA-256
`1daa973aa0f16a3be93e588dd3c83a8432b2917a5b525a05eb278776bb9c6435`.
Locator-ы не заменялись; raw URL, headers, credentials, runtime logs и большие
media artifacts в tracked evidence не попали.

Итог card — **PASS**: все 11 доступных строк дошли до полного startup
presentation/audio gate, player failures отсутствуют. Ещё две exact строки
честно имеют `PROFILE_EXCLUDED`, а не ложный `SOURCE_DRIFT`, `UNAVAILABLE` или
`PASS` через скрытый extractor fallback.

## Что в действительности работает

| Row | Safe role | Software verdict | Ingress | Extractor spawn |
| --- | --- | --- | --- | ---: |
| 00 | URL settings / adaptive HDR selection | PASS | extractor | 1 |
| 01 | HLS VOD MPEG-TS | PASS | native HLS | 0 |
| 02 | DASH VOD fMP4/WebM separate A/V | PASS | native DASH | 0 |
| 03 | HTTP audio-only Ogg | PASS | native HTTP | 0 |
| 04 | HLS live avc3 + HE-AAC/TTML | PROFILE_EXCLUDED | native admission, fail-closed | 0 |
| 05 | HTTP WebM VP9 + Opus | PASS | native HTTP | 0 |
| 06 | DASH live SegmentTimeline/DVR | PASS | native DASH | 0 |
| 07 | Smooth VOD H.264 + AAC | PASS | native Smooth | 0 |
| 08 | HTTP ISO-BMFF MP4 | PASS | extractor | 1 |
| 09 | HDS F4M/F4F | PASS | native HDS | 0 |
| 10 | FTP audio-only Ogg | PASS | native FTP | 0 |
| 11 | HLS CMAF + alternate audio | PASS | native HLS | 0 |
| 12 | DASH WebM VP9 + Opus with unsupported aspect evidence | PROFILE_EXCLUDED | native admission, fail-closed | 0 |

Row 04 остаётся вне принятого profile: exact source сейчас требует `avc3` с
HE-AAC и TTML. Это прямо соответствует подписи строки и не является отказом
доступного player path. Row 12 объявляет picture/sample aspect evidence, которое
нельзя честно протащить через текущий square-pixel display contract. Игнорировать
его означало бы показывать неверную геометрию, поэтому строка исключена явно.

Публичный N15 run использовал release binary SHA-256
`5cfc3c08979a07573a63d1b8d48637a40afaec250d883213bf9e1c1506a38fed`.
Финальная G3 workspace release build после audit-fix цепочки имеет SHA-256
`15db8697bc15d78d13201ff3883086adf0992d1deb4dfa874ab814f88bcc452f`.
Полные sanitized результаты находятся в
[`native-web-ingress-n15-acceptance.json`](native-web-ingress-n15-acceptance.json).

## Process и lifecycle accounting

Process spy дал ровно требуемое множество `{row00, row08}`. На cold open обе
строки сделали по одному spawn; после controlled close/restart — снова по одному.
Ни одна из 11 direct rows при `yt_dlp.enabled=false` extractor не запускала.
Обе process-positive строки вошли в extractor по одной и той же точной продуктовой
причине `PageMediaResolution` и начали с фазы `CandidatePrimary`: row 00 является
страницей YouTube, row 08 — HTML-страницей W3Schools, из которой extractor получает
media locator. Ни native compatibility fallback, ни recovery не выдавались за
первичную причину этих запусков.

Hermetic N14B cohort прошёл 17/17 и реально проверил forward/back seek,
same-item switch, live expiry/recovery, reopen, queue restore, close/restart и
stale-generation fence. Consumer tests доходили до A/V consumers; direct Ogg —
до nonzero PCM/clock, direct WebM — до WGPU submit/readback.

G3 добавил отсутствовавший cross-source regression: одна durable queue проходит
`native HLS → native DASH → native Smooth → native DASH`, сохраняя предыдущий
source живым до consumer success нового. Каждый переход до commit-а достигает
video decode, общего WGPU submit/readback и ненулевого PCM; используется один
renderer harness, а process-spy остаётся 0. Старый N14B queue-сценарий с двумя
строками одного HLS root сам по себе эту границу не доказывал.

## Auto и hardware

Auto profile прошёл representative direct, HLS VOD, DASH VOD и DASH live rows.
Hardware profile после реального preflight прошёл public HLS VOD, DASH VOD и
DASH live. Preflight подтвердил AMD Radeon 780M, Mesa 26.2.1, readable
`renderD128` и exact `VAProfileAV1Profile0:VAEntrypointVLD`.

Локальный hardware-only smoke прошёл VP9 SDR auto, AV1 SDR hardware и AV1 HDR
10-bit P010 hardware. HDR-monitor не требовался: runtime подтвердил P010 10-bit
4:2:0 boundary, активный `BT.2020 PQ limited → SDR BT.709` BT.2446-C shader
path и 1192 renderer submits. Отдельный WGPU integration test дошёл до mapped
readback; 13 BT.2446-C reference/shader tests и focused PQ/host-upload color-path
tests прошли.

## Performance: 30 cold + 30 warm

Все cohorts имеют 30 успешных повторов; p95 рассчитан nearest-rank методом.
Ключевое matched cold сравнение native против legacy extractor fixture:

| Metric | Legacy median / p95 | Native median / p95 | Изменение median / p95 |
| --- | ---: | ---: | ---: |
| Catalog | 29.486 / 30.569 ms | 4.321 / 4.403 ms | −85.35% / −85.60% |
| First consumer | 29.757 / 30.859 ms | 5.324 / 5.559 ms | −82.11% / −81.99% |
| Wall | 73.079 / 74.236 ms | 19.737 / 20.125 ms | −72.99% / −72.89% |
| Combined CPU time | 23.465 / 33.067 ms | 17.214 / 22.834 ms | −26.64% / −30.95% |
| Max RSS | 51,320 / 51,712 KiB | 47,774 / 48,068 KiB | −6.91% / −7.05% |

Native warm Ogg p95: catalog 4.427 ms, first consumer 5.489 ms, seek forward
1.511 ms, seek backward 0.718 ms, refresh 4.315 ms, 6 requests, 146,154 bytes,
0 spawns. Native warm HLS p95: catalog 37.832 ms, first consumer 60.867 ms,
seek forward 18.216 ms, seek backward 20.641 ms, switch 20.439 ms, refresh
21.047 ms, 3 root requests, 576 root bytes, 0 spawns.

N00 не содержал 30-run latency distribution, поэтому ему не приписан выдуманный
p95. С ним сравниваются доказанные structural/process показатели: cold extractor
spawns уменьшились с 11 до 2 (−81.82%), а на direct rows — с 9 до 0 (−100%).
Подробный catalog CPU/RSS/request/byte dataset находится в
[`native-web-ingress-n15-performance.json`](native-web-ingress-n15-performance.json).

Payload bytes legacy и native имеют разный смысл: legacy считает маленький
extractor metadata fixture, native — media transport. Поэтому bytes сохранены,
но не выданы за throughput regression. Аналогично warm legacy counters являются
кумулятивным reopen accounting и не смешиваются со seek/switch/refresh cohort.

## Исправленные причины, а не симптомы

- DASH parser теперь изолирует только доказанные non-playback subtitle rows,
  сохраняет DRM fail-closed, принимает exact XSI schemaLocation и не теряет
  playable siblings из-за одной unsupported SAR representation.
- DASH SegmentBase catalog proof видит только manifest-declared zero-based init
  prefix; full playback по-прежнему владеет всей representation. Это убрало
  сотни бессмысленных catalog Range requests без изменения playback semantics.
- Startup readiness теперь получает authoritative audio/video topology и не ждёт
  несуществующую video surface у audio-only HTTP/FTP.
- HLS alternate-audio admission выбирает единственный provider `DEFAULT` и
  сравнивает общий presentation interval/discontinuity layout, а не требует
  побитно одинаковый `EXTINF` у video и AAC access-unit boundaries.
- Terminal preparation warning публикует только уже sanitized typed error.
- HLS больше не зависит от `codec-core`: neutral decode-start evidence принадлежит
  `media-core`, MPEG-TS публикует typed in-band configuration, а HLS только
  потребляет этот boundary. H.264 classifier разбирает NAL-пакет один раз и
  различает non-keyframe, необходимость track configuration и in-band SPS/PPS.
- Worker wake для installed playback intent теперь закреплён детерминированным
  functional regression: production `select_biased!` применяет exact update,
  завершает typed receipt и публикует consumer-visible snapshot. Coverage не
  опирается на случайность scheduler-а.

## Fallback policy

Native → extractor fallback принадлежит одному app-owned
`NativeWebFallbackOwner` и может быть получен не более одного раза только до
strong `Installed` barrier. Allowlist содержит ровно три typed trigger-а:

- authoritative root оказался provider/HTML document;
- источник требует extractor-owned authorization material;
- parser доказал явно unsupported native profile, для которого extractor
  сохраняет существующую продуктовую семантику.

Cancellation, DNS/timeout/обычная network failure, malformed manifest, expired
endpoint, backpressure, invariant, decoder и renderer failure являются terminal
и не расходуют разрешённую попытку. После `Installed` owner больше не хранит
extractor locator: switch, seek, recovery, suspend/resume и restart используют
reconstructible stable source owner либо возвращают typed terminal error.

## Config v10 и migration

Current schema version — 10. Provider-neutral HDR/SDR selection, preferred video
height и VOD/live recovery attempts/backoff/stable-reset находятся только в
`[web_media]`. В `[yt_dlp]` остались только process controls: `enabled`, resolve
timeout и stdout/stderr/JSON budgets.

Одноразовая migration v9 → v10 переносит семь прежних policy keys из `[yt_dlp]`
в `[web_media]`, после чего current model читает один source of truth. Alias-полей
нет. Если legacy-документ одновременно задаёт target `[web_media]` и старые keys,
strict `deny_unknown_fields` отклоняет конфликт вместо угадывания приоритета.

## Acceptance и release commands

Публичный прогон использовал release binary, exact positional XSPF и отдельный
XDG profile. Конкретный временный profile path, media paths и runtime logs не
сохраняются в tracked evidence. Воспроизводимая форма и blocking проверки:

```bash
sha256sum user/web-media-playlist-acceptance.xspf
env XDG_CONFIG_HOME=/tmp/rustiplayer-native-ingress-g3 \
  target/release/rustiplayer user/web-media-playlist-acceptance.xspf
cargo +1.96.0 test -p app-egui --all-features --locked n14a_consumer -- --nocapture
cargo +1.96.0 test -p app-egui --all-features --locked n14b_lifecycle -- --nocapture
cargo +1.96.0 test -p app-egui --all-features --locked \
  n14b_cross_source_playlist_reaches_consumers_before_each_queue_commit -- --nocapture
cargo +1.96.0 test -p dash-mpd-core -p web-media-adaptive \
  -p web-media-dash -p web-media-hls --locked
scripts/playback-smoke.sh --mode hardware-only \
  --vp9 <VP9_SDR_FILE> --av1 <AV1_SDR_FILE> --av1-hdr <AV1_HDR_FILE>
scripts/pre-pr-checks.sh
scripts/coverage.sh check
cargo +1.96.0 build --workspace --all-features --release --locked
```

`scripts/pre-pr-checks.sh` является только wrapper-ом над
`scripts/ci-checks.sh all`, поэтому в G3 запускается один из них, а не оба.

## Known limitations

- Row 04 остаётся честно `PROFILE_EXCLUDED`: exact public source требует avc3,
  HE-AAC и TTML за пределами принятого native profile.
- Row 12 остаётся `PROFILE_EXCLUDED`: declared picture/sample aspect evidence
  нельзя представить через текущий square-pixel display contract без искажения.
- Реальный HDR display/output mode не проверялся. Доказан существующий
  HDR→SDR путь на SDR output: P010 decode boundary, BT.2020/PQ → BT.709
  BT.2446-C shader, WGPU submit/readback и active color-path telemetry. Это не
  является заявлением о настоящем HDR-monitor presentation.
- Cross-source regression проверяет production media/queue boundary, настоящий
  decode/audio/WGPU consumer и commit после consumer success, но не поднимает
  оконный `AppState` и не нажимает UI-кнопку Next. Для точного воспроизведения
  оставшегося пользовательского UI-сбоя всё ещё нужны конкретная пара строк и
  наблюдаемый симптом; этот gap не выдан за доказанное исправление UI.

## G3 coverage qualification

Финальный source revision coverage-квалификации — `d61a2d87`. Baseline получен
exact пересечением девяти workspace-run из трёх независимых cohort-ов:

- `sha256:404996c890975fe666573751a60c86ec05c019cbf0f42be73ab7da4c3611d0d7`;
- `sha256:8f275aaa75182eba4d42ac12328edd62e4d6a56a9f233d60c974f38b6b07d54f`;
- `sha256:2b1418916b92d1ae0b595d60bb891d4ddd0d9d8aa429b9b0a8ec788869a2ac3e`.

Logical baseline hash —
`sha256:ff51d2799a3562816de9a5f919bedb5594dc96c93b772e6e9d45c9f94b7f9743`,
tracked file SHA-256 —
`8c98f6acb996d9520b58703d29efb3f150bd8ba2cb60813610f1edd4936cf67b`.
Workspace stable intersection: functions `15,696/19,914`, lines
`163,462/211,068`, regions `205,271/268,471`.

Atomic transition от предыдущего G2 baseline прошёл с одной exact bounded
measurement exception: `crate:web-media-adaptive/regions`,
`2890/3239 → 2904/3255`, universe
`sha256:873164a09fe34e2680520748d02273541921a3800931bbed5ddbb293b43e5732 →
sha256:0a61eaa976d554aaeb568a38be11e2dd54fa9c4b41d9480700bea837dad6bac8`.
Причина — 16 новых regions у manifest-declared exposed-prefix boundary, из них
14 стабильны во всех девяти run; review deadline — 2026-12-01. File-local audit
58 изменённых Rust-файлов не нашёл потери стабильного живого кода; единственное
уменьшение `18/18 → 17/17` — удалённая iterator closure в HLS discovery,
заменённая обычным циклом.

Два свежих `scripts/coverage.sh check` после установки tracked pair прошли с
пустыми `regressions` и `universe_changes`; последний cohort hash —
`sha256:0f63da3c8df0e721c28da36f80f74be2c894658dd4e500418e487f945b5d400b`.
Во время qualification gate
fail-closed поймал две реальные scheduler-sensitive test gaps: already-cancelled
preparation executor и stale mismatched progressive seek. Финальный audit также
нашёл timing-dependent playback-intent wake branch; он закреплён exact installed
update → session → published snapshot regression. Все три gap-а проверяются через
production worker/consumer paths; соответствующие coordinates стабильны 9/9.
Непригодные/неполные cohort-ы в baseline не смешивались.

## Verification boundary

PASS: exact 13-row runner; 17 N14B lifecycle tests; cross-source
HLS→DASH→Smooth→DASH consumer regression; full `web-media-hls`; focused
DASH/HLS consumer tests; `dash-mpd-core`, `web-media-adaptive` и
`web-media-dash` suites; hardware-only smoke; WGPU readback; BT.2446-C reference
и active-path tests; 9-run baseline qualification и два fresh coverage checks.

Canonical `scripts/pre-pr-checks.sh` PASS. Wrapper уже выполнил
`scripts/ci-checks.sh all`, поэтому второй бессмысленный полный запуск не
делался. Внутри прошли toolchain policy/MSRV, guardrails, smoke self-tests,
dependency policy, `cargo deny`, `cargo machete`, strict workspace Clippy и
rustdoc, full workspace tests и no-default-features check. Отдельный
`cargo +1.96.0 build --workspace --all-features --release --locked` PASS.
Финальный redaction/reference/Serena diagnostics audit выполняется перед
логическим docs/memory commit и обычным fast-forward push.

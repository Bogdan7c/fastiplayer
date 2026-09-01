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

Финальный release binary имел SHA-256
`5cfc3c08979a07573a63d1b8d48637a40afaec250d883213bf9e1c1506a38fed`.
Полные sanitized результаты находятся в
[`native-web-ingress-n15-acceptance.json`](native-web-ingress-n15-acceptance.json).

## Process и lifecycle accounting

Process spy дал ровно требуемое множество `{row00, row08}`. На cold open обе
строки сделали по одному spawn; после controlled close/restart — снова по одному.
Ни одна из 11 direct rows при `yt_dlp.enabled=false` extractor не запускала.

Hermetic N14B cohort прошёл 17/17 и реально проверил forward/back seek,
same-item switch, live expiry/recovery, reopen, queue restore, close/restart и
stale-generation fence. Consumer tests доходили до A/V consumers; direct Ogg —
до nonzero PCM/clock, direct WebM — до WGPU submit/readback.

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
- Terminal preparation warning публикует только уже sanitizied typed error.

## Verification boundary

PASS: final release build; exact 13-row runner; 17 N14B lifecycle tests; full
`web-media-hls`; focused DASH/HLS consumer tests; `dash-mpd-core`,
`web-media-adaptive` и `web-media-dash` suites; strict Clippy затронутых packages;
workspace all-targets/all-features check; fmt; diff check; hardware-only smoke;
WGPU readback; BT.2446-C reference and active-path tests.

G3 намеренно не запускался: согласно roadmap это следующая отдельная gate-session.

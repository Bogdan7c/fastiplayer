# Web-media compatibility matrix

Этот документ описывает ровно утверждённый serializable profile
`yt-dlp-2026.07.04-serializable-v1`. Он не обещает поддержку любого сайта,
контейнера, кодека или protocol alias, который когда-либо вернёт `yt-dlp`.

Machine-readable источники истины:

- S00 schema и exact target identities:
  `crates/service-ytdlp/compatibility/2026.07.04/profile.json`;
- S41 runtime status и code/test evidence:
  `crates/service-ytdlp/compatibility/2026.07.04/runtime-coverage-s41.json`;
- S42 scoped profile-row audit:
  `crates/service-ytdlp/compatibility/2026.07.04/final-acceptance-s42.json`;
- S42 полный §14 goal→code/tests и release-audit trace:
  `crates/service-ytdlp/compatibility/2026.07.04/roadmap-trace-s42.json`.

`Implemented` означает, что checked-in hermetic provider/demux/runtime path
существует и проходит общий app-owned install barrier. Это не гарантирует
доступность конкретного публичного сервера, extractor-а, учётной записи,
географии или codec build на машине пользователя. Реальные URL проверяются
только manual opt-in runner-ом.

## Утверждённые runtime rows

| Stable row | Точная граница | S41 | Manual case ID |
| --- | --- | --- | --- |
| `progressive-http-iso-bmff` | HTTP(S), MP4/M4A ISO-BMFF, только текущие proven codec families | `Implemented` | `progressive-http-iso-bmff` |
| `progressive-http-matroska-webm` | HTTP(S), Matroska/WebM, VP8/VP9/AV1 и Vorbis/Opus в approved profile | `Implemented` | `progressive-http-matroska-webm` |
| `progressive-http-proven-audio` | HTTP(S), Ogg/FLAC/WAV/AIFF/CAF/MPEG audio из current native decoder set | `Implemented` | `progressive-http-proven-audio` |
| `hls-vod-ts` | HLS VOD с MPEG-TS segments | `Implemented` | `hls-vod-ts` |
| `hls-vod-fmp4` | HLS VOD с fMP4/CMAF segments | `Implemented` | `hls-vod-fmp4` |
| `hls-live-dvr` | HLS live/DVR, dynamic range, expiry, starvation и terminal end contract | `Implemented` | `hls-live-dvr` |
| `dash-vod-fmp4` | serialized DASH VOD с fMP4/CMAF fragments | `Implemented` | `dash-vod-fmp4` |
| `dash-vod-webm` | serialized DASH VOD с WebM fragments | `Implemented` | `dash-vod-webm` |
| `dash-live-dvr` | serialized dynamic DASH live/DVR | `Implemented` | `dash-live-dvr` |
| `ism-mss-base-h264-aac-fmp4` | static ISM/MSS VOD, только H.264 + AAC в fMP4 | `Implemented` | `ism-mss-base-h264-aac-fmp4` |
| `ftp-ftps-progressive` | FTP/FTPS progressive input на уже proven containers/codecs | `Implemented` | `ftp-ftps-progressive` |
| `hds-f4m-f4f` | static HDS/F4M/F4F VOD через FLV-family demux path | `Implemented` | `hds-f4m-f4f` |
| `rtmp-family-flv` | aggregate identity evidence без RTMP wire provider | `ProfileExcluded` | manual provider case отсутствует |

Runtime providers и manual runner принимают exact top-level schemes `http`,
`https`, `ftp` и `ftps`. Pure locator parser дополнительно сохраняет `rtmp` и
`rtmpe` как typed identity, но app classifier возвращает для них
`ProfileExcludedInputScheme`: wire provider не зарегистрирован и не обещан.
HLS, DASH, ISM и HDS обычно приходят как извлечённые format identities за
HTTP(S) locator-ом. Scheme сам по себе не доказывает container/codec/runtime
совместимость: окончательное решение принимает typed candidate/provider path.

## Topology, playlist и playback shapes

| Surface | Поддерживаемая граница | Manual case IDs |
| --- | --- | --- |
| Playlist files | M3U/M3U8, XSPF и CUE import/export; non-UTF input/output rejected | `playlist-m3u8`, `playlist-xspf`, `playlist-cue` |
| Result topology | `video`/missing type, `playlist`, first-class `multi_video`, `url`, `url_transparent`; bounded entries only | `public-single`, `public-playlist`, `public-channel`, `public-search`, `compound-multi-video` |
| Auth opt-in | trusted system `yt-dlp` config/plugins/cookies; app не хранит app-owned browser/cookie credentials, durable остаётся только exact acknowledged locator | `protected-system-cookie` |
| Media layout | muxed, separate A/V, video-only, audio-only through neutral composition | `layout-muxed`, `layout-separate`, `layout-video-only`, `layout-audio-only` |
| Quality | global preferred height и per-item runtime override/switch | `quality-preference-switch` |
| Recovery | failed import/open/switch до Installed barrier сохраняет current playback | `pre-barrier-import`, `pre-barrier-open`, `pre-barrier-switch` |

`multi_video` хранится как одна first-class top-level queue entry с Group ID.
Части участвуют в part-level navigation, но не превращаются в независимые
top-level storage rows. Top-level, retained и visible counts имеют разные
контракты и не должны вычисляться как один flat slice.

## Explicit exclusions

| Scope | Статус | Причина |
| --- | --- | --- |
| RTSP, standalone RTP, MMS | `ProfileExcluded` | Exact roadmap exclusion; pinned `yt-dlp` release также удалил RTSP/MMS support |
| MMST/MMSH aliases | `UnsupportedScheme` | Эти aliases не входят в machine-backed exact `ProfileExcluded` vocabulary |
| DRM и encrypted-only paths | `ProfileExcluded` | Нет decrypt/license architecture; `has_drm` используется как rejection marker |
| Private live extractor state | `ProfileExcluded` | Нужны Python objects, threads, mutable refresh/cookie state |
| RTMP/RTMPE/RTMPS/RTMPT/RTMPTE wire playback | `ProfileExcluded` или provisional exclusion | Identity metadata не является deterministic wire fixture |
| `rtmp_ffmpeg` | `ProfileExcluded` | Downloader identity не является wire protocol; hidden FFmpeg fallback запрещён |
| ISM live/DVR | scoped `ProfileExcluded`; canonical `ProfileExcludedProvisional` | Approved S41/S42 ISM row ограничена static H.264/AAC VOD; canonical S00 status остаётся provisional |
| HDS live/DVR | `NoApprovedRow` | Approved HDS row ограничена VOD |
| BunnyCDN/Soop/Niconico/FC2/WebSocket special state | `NoApprovedRow` | Нет public-serializable target row; fake provider test запрещён |
| MPEG-PS, AVI, ASF/WMV/WMA, rare codecs | `ProfileExcludedProvisional` | Требуются отдельные profile extension, exact fixture и end-to-end evidence |
| Subtitle playback | не заявлено | Descriptor metadata не выдаётся за реализованный subtitle renderer/playback |

Unknown identities не замалчиваются: bounded raw `protocol`, `ext`,
`container`, `vcodec` и `acodec` сохраняются для typed
`IncompatibleYtDlpContract`. Они не превращаются в fallback и не меняют
queue/current playback.

## Архитектурные ограничения

- Direct-media-first routing сохранён; URL service не дублирует direct opener.
- HTTP session, cache, prefetch и refresh принадлежат общему transport owner-у;
  parallel HTTP/cache/prefetch stack отсутствует.
- MPEG-TS и FLV payload разбираются только их общими demux owners, а generic
  fragmented MP4 — только `symphonia-format-isomp4` patch; provider modules не
  содержат вторых TS/FLV/generic-fMP4 parser-ов.
- Единственное точное исключение — bounded F4F ISO-envelope adapter
  `crates/flv-demux/src/f4f.rs`: он проверяет только Adobe
  `afra/abst/moof/mdat` и вложенные `traf/tfhd/trun`, после чего передаёт
  headerless FLV tags тому же `flv-demux`. Это не второй зарегистрированный
  generic fMP4 parser/demuxer; exact path и текущий symbol inventory
  fail-closed закреплены `scripts/s42_f4f_guardrail.py` через общий
  `scripts/check_s42_guardrails.py`.
- Старого parallel WebM-only opener нет.
- FFmpeg остаётся software decode boundary и не используется как hidden
  demux/network/RTMP provider.
- Единственное owner-approved hardware-capability исключение в S42 evidence —
  exact `VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12, capability
  intersection only. Более широкая hardware matrix не принимается; текущий
  hardware manual rerun — `NOT RUN`: у владельца сейчас нет совместимого
  VA-API device для opt-in rerun.

Operational failures и действия пользователя описаны в
[web-media-operational-errors.md](web-media-operational-errors.md). Manual
процедура и полный safe-case allowlist находятся в
[web-media-s42-final-acceptance.md](web-media-s42-final-acceptance.md).

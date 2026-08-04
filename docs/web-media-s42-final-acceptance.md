# S42 — final acceptance

S42 закрывает roadmap только для profile
`yt-dlp-2026.07.04-serializable-v1`. Hardware codec scope не расширяется сверх
явно принятого владельцем исключения S27: точного
`VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12, capability
intersection only. FFmpeg boundary, protocol aliases и private provider scope
не расширяются.

## Фактический итог 2026-07-25

`scripts/final-acceptance.sh` завершён успешно:

```text
S42 automated acceptance: PASS
S42 manual opt-in acceptance: NOT RUN (explicit user URL/fixtures required)
```

Primary Rust 1.96.0, locked MSRV 1.92, full hermetic suites, strict
Clippy/rustdoc/fmt, guardrails и coverage ratchet прошли. Manual matrix из 29
явно переданных URL/fixtures не запускалась. Real VA-API rerun также `NOT RUN`,
потому что у владельца нет совместимого устройства; принятое exact H.264
Baseline hardware exception остаётся в compatibility matrix без более широкого
hardware claim.

RTMP/RTMPE app admission возвращает typed `ProfileExcludedInputScheme` до
provider lookup и не выдаётся за отсутствующий `Implemented` provider.
Cargo-deny сообщает только два явно неблокирующих unmaintained advisory без
safe upgrade: RUSTSEC-2026-0150 (`audiopus_sys`) и RUSTSEC-2026-0192
(`ttf-parser`). XML advisory graph clean.

Acceptance состоит из двух независимых частей:

1. automated gate проверяет code/tests/policies/manifests без пользовательских
   media и credentials; test suites герметичны, а dependency audit отдельно
   использует настроенный advisory database workflow;
2. manual opt-in запускается только с URL/fixtures, которые явно выбрал
   пользователь, и остаётся `NOT RUN`, пока человек не выполнил всю matrix.

Зелёный automated gate не превращает manual часть в `PASS`.

## Automated gate

Единая команда:

```bash
scripts/final-acceptance.sh
```

Она запускает `scripts/ci-checks.sh all` и `scripts/coverage.sh check`. Gate
проверяет, среди прочего:

- goal-to-code/tests traceability и exact S00 → S41 profile coverage;
- отсутствие `Implemented` gaps и `Planned` rows;
- full locked hermetic tests, strict Clippy/rustdoc/fmt;
- primary Rust `1.96.0` и MSRV `1.92`;
- cargo-deny, cargo-machete и coverage inventory;
- dependency/toolchain/refactor/module-size guardrails;
- secret scope, cancellation, stale-generation и shutdown contracts;
- hardware capability без расширения сверх owner-approved exact
  `VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12, capability
  intersection only;
- FFmpeg decode-only boundary;
- отсутствие дублированных TS/FLV/generic-fMP4 parser-ов,
  HTTP/cache/prefetch stack и legacy WebM opener;
- единственное owner-approved parser exception — bounded
  `crates/flv-demux/src/f4f.rs`, который валидирует exact Adobe F4F
  `afra/abst/moof/mdat` envelope и вложенные `traf/tfhd/trun`, затем передаёт
  FLV payload canonical `flv-demux`; это не зарегистрированный generic fMP4
  demuxer, а его exact path и текущий symbol set закреплены fail-closed в
  `scripts/s42_f4f_guardrail.py`;
- XML advisory graph без ignore.

Machine-readable evidence:

- `crates/service-ytdlp/compatibility/2026.07.04/profile.json`;
- `crates/service-ytdlp/compatibility/2026.07.04/runtime-coverage-s41.json`;
- `crates/service-ytdlp/compatibility/2026.07.04/final-acceptance-s42.json`
  — scoped trace exact profile rows;
- `crates/service-ytdlp/compatibility/2026.07.04/roadmap-trace-s42.json`
  — полный executable trace 31 hermetic пунктов §14, обязательных release
  audits и отдельного manual-not-automated contract.

Оба trace-файла проверяет Cargo target `final_acceptance_s42`. Первый не
выдаётся за полное покрытие roadmap, второй не выдаётся за фактически
выполненный real-URL или hardware manual acceptance.

RTSP/RTP/MMS, private live state и DRM остаются explicit exclusions. RTMP
aggregate остаётся `ProfileExcluded`; для ISM live/DVR scoped S41/S42 status —
`ProfileExcluded`, а canonical S00 status — `ProfileExcludedProvisional`. HDS
live/DVR и approved special providers — `NoApprovedRow`. Их отсутствие
доказывается checked-in typed evidence, а не фиктивным provider test.

## Manual runner contract

Для последовательной проверки реального playback, seek, переходов между
transport owners и вкладки настроек потока URL существует дополнительная
[`web-media-playlist-acceptance.xspf`](web-media-playlist-acceptance.xspf) с
пошаговой
[`инструкцией`](web-media-playlist-acceptance.md). Этот удобный ручной прогон
не заменяет safe-case runner ниже: он не закрывает topology/auth/privacy rows
полной 29-case S42 matrix и не публикует автоматический `PASS`.

Runner:

```bash
scripts/progressive-web-smoke.sh --help
```

Каждый зачётный input задаётся парой:

```text
--case SAFE_CASE_ID --url EXPLICIT_URL
--case SAFE_CASE_ID --fixture EXPLICIT_LOCAL_FILE
```

Допустимы только exact HTTP, HTTPS, FTP и FTPS URL. Fixture разрешена только
для playlist/import cases. Runner не ищет corpus, не угадывает URL, не выбирает
browser profile и не подставляет credentials.

Пример неполного прогона:

```bash
scripts/progressive-web-smoke.sh \
  --case playlist-m3u8 --fixture /path/selected-playlist.m3u8 \
  --case public-single --url 'https://user-selected.example/watch' \
  --case ftp-ftps-progressive --url 'ftps://user-selected.example/media.bin' \
  --duration 120 \
  --binary target/release/rustiplayer \
  --report /tmp/rustiplayer-s42-manual.md
```

Без `--binary` runner сам собирает `app-egui` release на Rust `1.96.0` с
`--locked`. `--dry-run` проверяет parser/matrix и не создаёт report.
При real run local fixture сначала разрешается относительно caller cwd в
существующий canonical absolute path; только эта identity передаётся
приложению после перехода runner-а в repository root.

Backward-compatible `--url URL` без `--case` получает safe generated label
`legacy-url-N`. Такой запуск удобен для старого локального smoke workflow, но не
закрывает ни одну S42 row и всегда оставляет matrix `NOT RUN`.

## Required safe case IDs

Все 29 IDs должны быть переданы ровно по одному разу. Один URL разрешено
использовать для нескольких ролей только если он действительно доказывает
каждую из них; runner не выводит media properties из имени case-а.

| Case ID | Input | Что проверяет человек |
| --- | --- | --- |
| `playlist-m3u8` | fixture | M3U8 import, full/selected export и re-import |
| `playlist-xspf` | fixture | XSPF import/export и compound metadata |
| `playlist-cue` | fixture | CUE windows и typed unrepresentable export |
| `compound-multi-video` | HTTP(S) URL | Одна top-level Group entry, disclosure/parts/navigation |
| `public-single` | HTTP(S) URL | Direct-media-first single item |
| `public-playlist` | HTTP(S) URL | Bounded preview/commit без silent drops |
| `public-channel` | HTTP(S) URL | Partial/unavailable/duplicate topology |
| `public-search` | HTTP(S) URL | Bounded order и explicit confirmation |
| `protected-system-cookie` | HTTP(S) URL | Trusted system cookie/config без app credential persistence |
| `progressive-http-iso-bmff` | HTTP(S) URL | MP4/M4A progressive path |
| `progressive-http-matroska-webm` | HTTP(S) URL | Matroska/WebM progressive path |
| `progressive-http-proven-audio` | HTTP(S) URL | Approved native audio containers/codecs |
| `hls-vod-ts` | HTTP(S) URL | HLS VOD MPEG-TS |
| `hls-vod-fmp4` | HTTP(S) URL | HLS VOD fMP4/CMAF |
| `hls-live-dvr` | HTTP(S) URL | HLS live/DVR range, expiry, starvation/end |
| `dash-vod-fmp4` | HTTP(S) URL | DASH VOD fMP4 |
| `dash-vod-webm` | HTTP(S) URL | DASH VOD WebM |
| `dash-live-dvr` | HTTP(S) URL | DASH live/DVR dynamic range, expiry и neutral starvation; terminal end для dynamic MPD не заявлен |
| `ism-mss-base-h264-aac-fmp4` | HTTP(S) URL | Exact static ISM H.264/AAC VOD boundary |
| `ftp-ftps-progressive` | FTP(S) URL | Progressive FTP/FTPS transport |
| `hds-f4m-f4f` | HTTP(S) URL | Static HDS/F4M/F4F VOD boundary |
| `layout-muxed` | HTTP(S) URL | Muxed A/V |
| `layout-separate` | HTTP(S) URL | Neutral separate A/V composition |
| `layout-video-only` | HTTP(S) URL | Video-only |
| `layout-audio-only` | HTTP(S) URL | Audio-only |
| `quality-preference-switch` | HTTP(S) URL | Global preferred height и runtime override/switch |
| `pre-barrier-import` | fixture | Failed import сохраняет queue/current playback |
| `pre-barrier-open` | HTTP(S) URL | Failed open до barrier сохраняет playback |
| `pre-barrier-switch` | HTTP(S) URL | Failed switch до barrier сохраняет playback |

## Provenance и fail-closed behavior

Перед реальным запуском runner разрешает `yt-dlp` через тот же `PATH`, который
унаследует приложение, и отдельным безопасным probe выполняет:

```text
yt-dlp --ignore-config --no-plugin-dirs --version
```

Принимается только exact output `2026.07.04`. Mismatch или ошибка probe
завершают процесс до build, report и media runtime. Report фиксирует:

- profile ID и pinned source commit;
- current workspace HEAD и только его `clean`/`dirty` classification;
- origin и SHA-256 реально запущенного Rustiplayer binary;
- runner-built source association с current worktree либо честный
  `external prebuilt`, для которого workspace HEAD не объявляется source
  provenance;
- exact `yt-dlp` version;
- SHA-256 выбранного `yt-dlp` executable;
- per-case runtime exit status и sanitized log.

Version probe изолирован от user config/plugins, но production app run
намеренно сохраняет обычный system/user config/plugin/cookie lookup. Это
trusted external code: его side effects находятся вне Rustiplayer guarantee.

## Outcome contract

| Поле/состояние | Значение |
| --- | --- |
| Matrix status `NOT RUN` | В selection отсутствует хотя бы один из 29 required case IDs |
| Matrix status `MANUAL REVIEW REQUIRED` | Все 29 IDs выбраны; человек ещё обязан проверить checklist |
| Runner outcome `MANUAL REVIEW REQUIRED` | Выбранные real runs завершились допустимыми status; human checks остаются, даже если matrix неполна |
| Runner outcome `FAIL` | Version, build, runtime, parser или report lifecycle завершились ошибкой |
| Terminal `NOT RUN` | Selection отсутствует либо вызван dry-run; report не создавался |

Полный набор 29 IDs меняет только matrix status с `NOT RUN` на
`MANUAL REVIEW REQUIRED`. Runner никогда не пишет `Outcome: PASS`.
Runtime status `137` означает SIGKILL/неуспешный `--kill-after` shutdown и
всегда даёт `FAIL`; допустимый graceful timebox status — `124`.
Первый write создаёт report exclusive; существующий или появившийся после
preflight artifact не перезаписывается.

## Privacy contract

Raw URL/fixture identities живут только в argv/process memory и временных raw
logs. Перед сохранением report runner:

- буквально заменяет текущий explicit input;
- для fixture также удаляет canonical path, basename, file-URI и типичные
  percent-encoded absolute-path представления;
- заменяет любые HTTP(S)/FTP(S) endpoints;
- удаляет целиком строки с Authorization, Cookie, Set-Cookie, headers, request
  data, extractor payload, token, signature, password или bearer material;
- удаляет process-owned temporary directory на штатном exit/failure/timeout;
- сохраняет только safe case ID и input kind.

Это не очищает history родительского shell и не скрывает argv от process tools.
Для protected URL предпочтительнее locator без embedded credentials и
user-owned system cookie/config. Raw secret нельзя дописывать вручную в report.

## Human checklist после запуска

Generated report содержит unchecked список. Человек обязан подтвердить:

1. M3U8/XSPF/CUE import/export и exact CUE windows.
2. First-class compound UI, structural actions и part-level navigation.
3. Public single/playlist/channel/search topology и partial/duplicate behavior.
4. Protected URL через system cookie/config без app credential persistence.
5. Каждую из двенадцати `Implemented` provider rows в её точной boundary.
6. Checked-in `ProfileExcluded`/`NoApprovedRow` evidence вместо fake extended
   provider tests.
7. Muxed, separate A/V, video-only и audio-only layouts.
8. Global quality preference и runtime switch в Playing и Paused.
9. VOD terminal end; live/DVR range, expiry, starvation и safe live edge.
10. Failed pre-barrier import/open/switch сохраняют current playback.
11. Post-barrier failure не выдаётся за recoverable rollback.
12. URL sidebar не содержит второго input и не показывает transient secrets.
13. Exact acknowledged locator отделён от headers/cookies/resolved targets.
14. Supersede/cancel/stale completion/shutdown не публикуют stale active source.
15. Итоговый report не содержит raw URL, fixture path или credential material.

Единственное hardware-capability утверждение S42 — owner-approved S27
exception: exact `VAProfileH264Baseline` → H.264 Baseline 8-bit YUV420/NV12,
capability intersection only. Более широкое hardware acceptance не заявлено;
current hardware manual rerun считается `NOT RUN`: у владельца сейчас нет
совместимого VA-API device для opt-in rerun.

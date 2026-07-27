# Generic site open: CF + embed recovery + HLS deferred codecs (2026-07-27)

Философия: открыть медиа с любого сайта без hardcoding каталожных доменов; YouTube watch URL не ломать.

## C1 — `StreamLayout::HlsMuxedCodecDeferred`

- Владелец: `web-media-core::layout`. Gate: transport=HLS + оба `vcodec`/`acodec` JSON-null + height present. Progressive с null codecs по-прежнему `Missing`.
- Не подставлять fake h264/aac. Declared YouTube/codec path не менялся.
- S21C: `CandidateRuntimeRequirements::HlsMuxedCodecDeferred` — без static decode checks; demux admission = HLS transport ∩ (MpegTs|IsoBmff|FragmentedIsoBmff). `av_completeness_rank` как Muxed.
- App HLS open: `codecs=None`, main container `ContentProbe`; после open `prove_deferred_hls_codec_evidence` через `AppCatalogCapabilityProbe` на demuxer tracks (fail-closed до Installed).
- Picker: deferred проецируется в `VideoTrackDescriptor` с codec `none` (Absent) → facet Codec = `Automatic`; Resolution по height (width optional → 0).

## A — `generic:impersonate`

- Production/hermetic argv candidate и topology всегда включают `--extractor-args generic:impersonate` (константа `GENERIC_IMPERSONATE_EXTRACTOR_ARGS`).
- Влияет только на generic IE; YouTube IE не читает `generic:` args. Format-level `impersonate` в JSON по-прежнему `ImpersonationRequired`.
- Profile `compatibility/2026.07.04/profile.json` и exact-argv tests обновлены. Нужен system `curl_cffi` у yt-dlp.

## B — cross-host platform embed recovery

- `--use-extractors -youtube` **не работает**: Generic не падает на другие iframe и даёт Unsupported URL. Отвергнуто.
- Актуальный recovery в `service-ytdlp::embed_recovery` + общей process-boundary `recover_playable_document_after_platform_hijack` используется и candidate, и topology путями:
  1. Primary dump-single-json/topology; recovery только при подтверждённом cross-host platform hijack (extractor ∈ youtube/vimeo/dailymotion/twitter/tiktok/instagram/facebook И result host этой семьи, а input host — нет).
  2. Tempdir + yt-dlp `--write-pages --skip-download` + impersonate; bounded scan `*.dump` (лимиты файлов/байт).
  3. Pure HTML: iframe src/data-src и source src; drop platform и login/signin/oauth/accounts URL; prefer path tokens `/vod/|/embed/|/video/|/player/`.
  4. Embed candidates пробуются по порядку до первого успешного dump-single-json, который не является platform hijack относительно исходного input URL. Ошибки кандидата пропускаются, Cancellation не глотается, исчерпание оставляет primary.
  5. Из первого подходящего input-host HTML dump извлекается bounded `<title>`/`og:title`; им дополняется recovered document, только если extractor title отсутствует, пуст или равен `video` без учёта регистра.
  6. Topology после успешного primary parse вызывает ту же recovery boundary для Video/MultiVideo root и повторно парсит recovered Value с теми же budgets; принимает recovered Video. Неуспех recovery fail-open оставляет primary trailer topology.
- Прямые YouTube URL recovery не запускают. Домены каталогов не allowlist-ятся. `--use-extractors -youtube` по-прежнему отвергнут: Generic не продолжает полезный iframe fallback.

Related: `mem:media-services/ytdlp-candidate-normalization-s19-2026-07-21`, `mem:media-services/web-playback-planner-s21c-2026-07-21`, `mem:media-services/hls-vod-s32c-2026-07-23`, `mem:app-egui/web-media-picker-slice-g-2026-07-26`, `mem:media-services/ytdlp-system-auth-s26-2026-07-22`.

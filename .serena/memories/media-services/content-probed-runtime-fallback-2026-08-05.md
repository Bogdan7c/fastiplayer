# ContentProbed runtime proof и bounded BestPlayable fallback (2026-08-05)

См. также `mem:core`, `mem:media-services/ytdlp-candidate-normalization-s19-2026-07-21` и HDS S38 memory.

## Boundary

- `app-egui/web_media_open/content_probe.rs` владеет authoritative post-demux proof для generic progressive `StreamLayout::ContentProbed`: literal `Absent`, presence и codec-family correspondence для `Declared`, actual decoder capabilities и hard `PlaybackSelectionPolicy`.
- Actual container codec IDs нормализуются через `web_media_core::NormalizedCodec`; video decoder identity дополнительно разрешается `codec_core::VideoCodec::from_container_codec_id`.
- Отсутствие color metadata передаётся в policy как `None`; оно не превращается в выдуманный `DynamicRange::Unknown`. Доказанный HDR при `SdrOnly`, explicit unknown dynamic range и codec вне configured order являются typed hard rejection.
- `app-egui/web_media_open/content_probe_fallback.rs` повторяет open только для `BestPlayable` и только после `CandidateOpenError::ContentProbe`. Порядок берётся из `rank_playable_opaque_alternatives` как полный best-first exact+semantic identity stream и строго маппится в canonical service view; duplicate/missing identity fail closed. Network/parser/provider/cancellation ошибки terminal. `Exact` и `Composed` всегда выполняют ровно одну попытку.
- Service/planner pairing валидируется через full service-owned planning projection до BestPlayable/Exact/sidebar/catalog use; одинаковые identities с подменёнными runtime requirements или quality score отклоняются.
- Planner A/V-completeness определяется по validated `CandidateRuntimeRequirements`: `Muxed`, `Separate`, HLS-deferred и `ContentProbed { video: Some(_), audio: Some(_) }` считаются complete A/V и ранжируются раньше single-track alternatives. `ContentProbed` с unknown/partial requirements не получает выдуманную A/V привилегию и остаётся в одном ранге с `VideoOnly`/`AudioOnly` до authoritative runtime proof.
- После fallback `candidate_selection`, stream configuration и catalog attachment строятся из successful canonical candidate, а не из первой planner попытки. Catalog composition пробует audio по deterministic planner-owned fallback rank до первого inventory-composable варианта; selected-only components не создают fake A/V target.

## HDS special case

- HDS worker demuxer после open может ещё не публиковать tracks, поэтому generic post-open proof для HDS запрещён.
- Authoritative HDS proof выполняется eager discovery каждой rendition через `ContentProbedHdsCapabilityProbe`: outer `Absent`/`Declared` correspondence, immutable decode capabilities и actual HDR/codec policy до provider-default/semantic exact selection.
- HDS sibling outcome разделён на `Admitted`, content/profile `Rejected` и infrastructure `Unavailable`. Discovery продолжает bounded pass: недоступная sibling изолируется, если существует `Admitted`; при нуле admitted любая `Unavailable` остаётся fatal. Только полный pass из content/profile rejections выдаёт безопасный zero-payload `web_media_hds::HdsNoPlayableRendition`, который app мапит в retryable parent-content rejection.
- ProviderDefault и Semantic используют один discovery pass и всегда публикуют Installed coupled component catalog. Выбранный eagerly probed `HdsDemuxPlan` + demuxer передаются в receipted playback worker напрямую: initial F4F fragment не скачивается повторно; transactional seek по-прежнему открывает fresh demuxer с target fragment anchor.

## Focused evidence

- `web_media_open::content_probe` tests: declared-vs-actual codec mismatch, actual PQ/HDR rejection, HDS outer correspondence, real Ogg/Opus PCM и planner-ranked fallback.
- `web_media_open::content_probe_fallback` tests: typed retry, fatal no-retry, Exact single attempt; isolated fake yt-dlp tests строят настоящий `selected + formats[]` service snapshot, проверяют canonical dedup, second-candidate fallback, sidebar/catalog alignment rejection и три composition shape-а: selected-only best audio → следующий inventory audio, selected-only video без composed target, обычная inventory A/V pair.
- `web-media-playback-plan::policy` tests фиксируют semantics missing color evidence, HDR и codec exclusions.
- HDS app verticals фиксируют mixed `Unavailable + Rejected + Admitted` success, ровно один initial selected Frag1 fetch, encoded A/V, receipted seek с fresh target-fragment fetch, а также terminal all-fragment-404 и external-bootstrap-404 без ложного parent content fallback. `web-media-hds/tests/vod_runtime.rs` отдельно фиксирует typed all-content-rejected marker.

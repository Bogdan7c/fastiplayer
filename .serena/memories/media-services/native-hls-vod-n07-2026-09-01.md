# N07 native HLS VOD catalog/switch/reopen (2026-09-01)

## Результат

Existing native HLS VOD path расширен до extractor-independent TS/fMP4 catalog flow. Новый parallel data plane не создавался: root fetch, HLS catalog discovery, exact catalog reopen, progressive demux, receipted seek и app strong-open lifecycle остаются существующими owners.

## Владение и boundaries

- `web-media-hls::native_ingress` владеет parsing/profile validation и fresh provider-default. `NativeHlsCatalogAdmission` хранит semantic runtime intent и optional exact master ordinal только текущего parsed snapshot-а. Ordinal не является reopen identity и не переживает refresh.
- `HlsCatalogDiscoveryRequest::provider_default_variant_index` позволяет catalog owner-у однозначно связать ranked default с текущей master row даже при одинаковых `RESOLUTION/CODECS`; generic/extractor paths передают `None`.
- `prepare_hls_catalog_vod_receipted_at_start` переносит caller-owned `HlsVodStartIntent` в exact catalog reopen. Main component единолично решает permissive restore fallback; alternate audio получает strict effective start.
- `app-egui::NativeHlsSourceState` владеет neutral `WebMediaSelection`, full component catalog projection и semantic switch/reopen intent. `WebMediaSourceIntent::selection_switch_request` теперь разрешает native component action через тот же provider-neutral same-item lifecycle.
- `NativeHlsUrl` владеет process-local opaque stable `SourceIdentity`, не выведенным из URL/query. Каждый root refresh получает fresh extraction/catalog generations при той же source lineage.
- Native VOD descriptor теперь несёт `VodEndpointRecoveryAttachment`; app recovery принимает `RefreshRootManifestAndRematch` и строит тот же controlled semantic reopen, что settings/manual reopen.
- Для размера модулей app composition вынесена в `startup_media/native_hls/vod_catalog.rs` и `web_media_hls_open/native_vod.rs`.

## Исправленные первопричины

1. Legacy low-load admission rank-ил одинаковые descriptor rows по bandwidth, затем терял bandwidth в `HlsVariantSelectionIntent` и ошибочно возвращал `ExtractorMaterialRequired` из-за ambiguity. Fresh catalog admission теперь передаёт exact current-snapshot ordinal в catalog build; refresh/switch/reopen используют только semantic component selection.
2. Catalog reopen принимал `start`, но `select_and_load_catalog_master` жёстко передавал `Beginning` обоим components. Теперь main получает исходный start и публикует authoritative prepared position/receipt; separate audio следует strict effective start.

## Инварианты

- Authoritative root загружается ровно один раз на open attempt и передаётся discovery/runtime как `FetchedTop`; второго root GET нет.
- Valid supported finite HLS media/master не вызывает extractor. Initial 401/403 и typed live/event сохраняют прежний fallback scope; malformed/profile/network/cancellation не маскируются fallback-ом.
- TS/fMP4 sibling URLs и current ordinals не сохраняются в durable app state. Reopen/recovery refresh-ят stable root и semantic-rematch-ят selection против fresh catalog.
- Same-item Playing/Paused transaction сохраняет item/lineage/position и commit-ит selection только после Installed candidate; HLS exact reopen выполняет worker-receipted seek.
- Endpoint recovery arm-ится только после окончательной candidate finalization.
- Debug/read-only projections не раскрывают root/child URL, query, headers или extractor material.

## Functional evidence

Главная hermetic vertical: `crates/app-egui/src/media_open/web/tests/native_hls_vertical.rs`.

Она использует repository fixtures с real H.264 baseline + AAC:
- initial master selection: fMP4;
- full coupled catalog: TS + fMP4;
- H.264 packet -> FFmpeg software decoder -> HostPlanar materializer -> WGPU draw/submit/readback/release;
- AAC packet -> production decoder -> nonempty PCM;
- semantic switch fMP4 -> TS с `PreparedInitialPosition::PositionedAt`;
- master rows переставляются между initial/switch/reopen, selection semantic-rematch-ится;
- controlled reopen повторно достигает decoder/render/audio с receipted restore;
- root request count exact 1/2/3 по попыткам;
- injected process spy остаётся 0.

Дополнительно:
- `web-media-hls::native_ingress` tests покрывают ambiguous valid master, media VOD без fake ordinal и typed live fallback;
- existing Playing/Paused same-item lifecycle tests подтверждают сохранение позиции/status/item/lineage;
- VOD endpoint recovery claim-policy suite остаётся зелёной.

## Verification

PASS:
- `cargo fmt --all -- --check`
- `git diff --check`
- full `cargo test -p web-media-hls --all-features --locked`
- app native HLS vertical/unit filter
- Playing и Paused same-item lifecycle filter
- VOD endpoint recovery filter
- vertical repeatability 3/3
- strict Clippy для `web-media-hls` и `app-egui`
- `cargo check --workspace --all-targets --all-features --locked`
- Serena diagnostics/references audit

По §6.3 не запускались full workspace tests, MSRV, dependency/release/coverage gates и public/GUI/hardware acceptance. Следующая session: N08; в N07 она не начиналась.

Связанные memories: `mem:core`, `mem:media-services/hls-vod-manifest-receipted-seek-2026-08-24`, `mem:media-services/hls-vod-seek-index-compaction-aud010-2026-08-23`, `mem:media-services/vod-endpoint-recovery-aud009-2026-08-23`, `mem:testing/hls-ts-vod-runtime-fix-2026-08-04`.
# N09 native static DASH VOD без yt-dlp (2026-09-01)

## Результат

Direct HTTP(S) URL с syntactic `.mpd` hint теперь сначала проходит native content admission. Суффикс выбирает только adapter: authoritative DASH identity, static/dynamic presentation и supported profile определяет существующий `dash-mpd-core` parser. Supported static MPD открывается через существующие `web-media-dash` discovery/open/runtime owners; новый data plane не создавался.

## Владение и boundaries

- `source-core` + `web-media-adaptive::AdaptiveHttpContext` остаются единственными HTTP owners.
- `web_media_dash::DashFetchedManifestInput` переносит effective redirect target, bounded body, source generation и explicit XML/MPD budgets от первого root fetch-а к parser/catalog owner-у. `DashVodInput::FetchedManifest` запрещает второй root GET.
- Fetched handoff fail-closed повторно проверяет current HTTP generation и current `DashVodOpenPolicy::maximum_manifest_bytes`; cross-generation и over-policy body имеют отдельные `DashVodOpenError`.
- `discover_native_dash_vod_catalog` строит existing representation-lane catalog без service-ytdlp provider-default evidence. Native default выбирается только после actual demux/capability proof, учитывает preferred-height policy и использует только опубликованные exact compatible relations.
- Exact separate A/V selection проверяется catalog compatibility edge; muxed rows остаются coupled. Никакие video×audio Cartesian combinations не синтезируются.
- `app-egui::NativeDashUrl` владеет stable opaque `SourceIdentity`; URL/query не участвуют в identity и не попадают в Debug. Каждый refresh получает fresh extraction/catalog generation при той же source lineage.
- `NativeDashSourceState` хранит neutral `WebMediaSelection`, installed-only component catalog projection и semantic switch/reopen intent. Representation child URLs, current XML order и extractor DTO в durable state не сохраняются.
- `prepare_native_dash_attempt` владеет direct root fetch, catalog discovery, semantic rematch, exact open, worker-receipted VOD seek и VOD endpoint-recovery attachment.
- Startup/media-open adapters используют один sequential native attempt. Extractor fallback разрешён только initial pre-Installed attempt и только для parser-owned StrictlyNotDash / supported-auth-required / UnsupportedNativeProfile categories. Semantic switch/reopen, malformed MPD, transport failure, cancellation и runtime failure extractor не запускают. При `yt_dlp.enabled=false` typed fallback requirement становится terminal native-open error до process owner-а.
- Для размера и ownership direct DASH routing, adapter view и prepared startup ownership вынесены в `url_service_adapter/native_dash.rs`, `media_open/web/adapter_view.rs` и `startup_media/orchestration/prepared.rs`.

## Инварианты

- Root MPD GET выполняется ровно один раз на physical open/switch/reopen attempt.
- Static/dynamic никогда не выводится из extension или extractor metadata.
- Capability-rejected siblings изолируются; neutral catalog публикует только actual playable lanes.
- Fresh XML row order/Representation ID/endpoint rotation не являются selection identity; installed semantic request rematch-ится против fresh catalog.
- VOD seek завершается authoritative worker receipt-ом.
- Endpoint recovery refresh-ит stable MPD root и semantic-rematch-ит selection.
- Endpoint recovery attachment arm-ится только после Installed lifecycle.
- Valid supported static MPD не вызывает yt-dlp process.

## Functional evidence

Основная hermetic vertical:
`crates/app-egui/src/media_open/web/tests/native_dash_vertical.rs`.

Loopback MPD переставляет две actual muxed rows между attempts:
- fMP4 H.264 baseline + AAC;
- WebM VP9 profile 0 + Opus.

Тест доказывает:
- root accounting exact 1/2/3 для initial/switch/reopen;
- capability-filtered coupled catalog содержит ровно две physical rows без fake combinations;
- initial H.264 packet -> FFmpeg software decode -> HostPlanar materializer -> WGPU draw/submit/readback/release;
- AAC packet -> production audio decoder -> nonempty PCM;
- worker-receipted accurate VOD seek;
- semantic switch после перестановки XML rows выбирает VP9/Opus;
- VP9 packet снова достигает decoder/render, Opus — production PCM;
- controlled reopen/root refresh сохраняет VP9/Opus semantic selection и stable source lineage;
- injected `YtDlpExtractorAdapter` process spy остаётся 0 при disabled yt-dlp.

Дополнительные focused tests:
- fetched root discovery/open не выполняет второй manifest request;
- fetched handoff отвергает чужую generation;
- fetched handoff повторно применяет current body policy;
- `.mpd` hint строит content-probed native request при disabled extractor;
- secret-safe Debug для native DASH URL/state.

## Verification §6.3

PASS:
- `cargo test -p web-media-dash --all-targets --all-features --locked` (40 unit + 11 integration);
- app `native_dash` filter (secret-safe unit + full vertical);
- app `mpd_hint_builds_content_probed_native_admission_when_extractor_is_disabled`;
- strict Clippy: `web-media-dash` + `app-egui`, all targets/features, `-D warnings`;
- `cargo check --workspace --all-targets --all-features --locked`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- Serena symbol/reference/diagnostics audit. Rust-analyzer у Serena до staging временно не видел новый untracked `discovery/native_vod.rs`; Cargo check/test/Clippy видели файл и были authoritative.

По §6.3 не запускались full workspace tests, public-network, GUI/hardware, MSRV, dependency/release/coverage/pre-PR gates. Следующая session: N10; в N09 она не начиналась.

Связанные memories: `mem:core`, `mem:media-services/dash-vod-s34-2026-07-24`, `mem:media-services/adaptive-transport-s31-2026-07-23`, `mem:media-services/native-web-ingress-n01-2026-08-31`, `mem:media-services/native-hls-vod-n07-2026-09-01`, `mem:testing/native-web-ingress-g1-2026-08-31`.

## Public launch S02 layout update (2026-09-04)

- Provider-owned representation lane proof, track validators, descriptor builders, codec matching и probe-error mapping перенесены без изменения semantics в `discovery/lane_proof.rs`; static VOD orchestration остаётся в `discovery.rs`/`discovery/native_vod.rs`.
- Точные новые tests и verification: `mem:public-launch/s02-module-boundary-split-2026-09-04`.
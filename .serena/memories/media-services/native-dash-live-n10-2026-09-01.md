# N10 native dynamic DASH live/DVR без yt-dlp (2026-09-01)

## Результат

- Direct HTTP(S) `.mpd` ingress теперь authoritative-классифицирует fetched body как static/dynamic и открывает supported dynamic MPD через существующий `web-media-dash` S35 runtime без extractor.
- Session N10 не расширяет approved dynamic profile: direct UTC, `SegmentTemplate + SegmentTimeline`, PTO, publish ordering, A/V availability intersection, live edge, refresh continuity и explicit exclusions остаются прежними. `$Number$ + duration`, LL-DASH и новый multi-Period scope не добавлены.
- Следующая session — N11; N10 перед ней остановлена.

## Ownership и boundaries

- `dash-mpd-core` остаётся единственным schema/profile owner-ом. Unsupported dynamic addressing возвращается как `DashDynamicMpdError::ProfileExcluded(DashDynamicProfileExclusion::UnsupportedAddressing)`.
- `web-media-dash::DashFetchedLiveManifestInput` дополняет N09 fetched handoff exact `Instant` начала root fetch-а и `DashClockFetchObservation`; direct UTC midpoint и first refresh deadline поэтому не теряются при app -> runtime handoff.
- Runtime-private `DashLiveInitialManifest::{Fetch,Fetched}` и `DashLiveRuntimeOpenRequest` живут в `live/runtime/initial_request.rs`. Initial fetched bytes используются только для первого snapshot-а; refresh worker немедленно нормализуется к stable `DashManifestInput::Fetch`, поэтому stale bytes не replay-ятся.
- `discovery/native_live.rs` строит capability-filtered dynamic logical lane catalog с `NativePreferredHeight`, без extractor evidence и без fake Cartesian A/V combinations. Exact/semantic selection затем открывается прежним `prepare_discovered_dash_live`.
- App `startup_media/native_dash/live_runtime.rs` владеет direct live catalog/rematch/composition. `PreparedNativeDashLifecycle::{Vod,Live}` запрещает смешать VOD endpoint attachment и S31L timeline port.
- App `startup_media/native_dash/live_refresh.rs` владеет native stable-root endpoint generation recovery. Он создаёт fresh live-scoped HTTP context и stable MPD request; fetch/parse/semantic continuity/publish commit остаются у S35 runtime. `service-ytdlp` не вызывается.
- `WebMediaSourceIntent::native_dash` теперь получает proven neutral `WebMediaPresentationKind`; generic same-item/Playing/Paused lifecycle поэтому использует уже существующие S31L contracts без DASH vocabulary в `player-core`.

## Ошибки и fallback

- Initial-only fallback разрешён для authorization material и parser-owned deliberate unsupported profile.
- Dynamic profile exclusion сохраняется typed до app fallback gate-а; installed semantic reopen не имеет extractor locator и поэтому fail-closed.
- Network/status failure, malformed dynamic schema, cancellation, semantic rematch failure и runtime failure терминальны и не запускают extractor.
- First root response выполняется одним GET: classification, dynamic discovery и initial runtime используют один fetched body. Periodic/endpoint refresh-и обращаются к stable root по протокольной необходимости.

## Durationless fMP4 RAP evidence

- Hermetic production fMP4 обнаружил старый S35 boundary defect: video packet без duration на exact DVR-window start создавал zero-length RAP point и никогда не мог опубликовать seekable range.
- `DashLiveTimelineCoordinator` теперь принимает point на inclusive manifest start и завершает его только timestamp-ом следующего фактически observed packet-а. Это не угадывает duration: right boundary берётся из реального packet timestamp-а, а continuity допустима только внутри уже approved gap-free S35 `SegmentTimeline`.
- Focused regression: `live::runtime::timeline::tests::durationless_video_rap_becomes_seekable_only_after_the_next_observed_packet`.

## Hermetic functional evidence

Основная vertical:
`crates/app-egui/src/media_open/web/tests/native_dash_live_vertical.rs::native_dynamic_dash_reaches_moving_presentation_audio_and_dvr_without_extractor`.

Она доказывает:
- initial root GET exact 1 и fetched handoff без второго GET;
- direct UTC sample, equal publish snapshots и strictly newer rotated-Representation snapshot;
- H.264/AAC fMP4 packets доходят до FFmpeg software decoder, HostPlanar/WGPU submit+release и production audio decoder с nonzero PCM;
- newer publish сдвигает availability/DVR window;
- retained worker-receipted DVR seek succeeds, expired old target fail-closed без clamp;
- transient fragment 410 вызывает native stable-root endpoint recovery и повтор resource fetch;
- logical semantic rematch переживает Representation id rotation и controlled reopen, сохраняя stable source lineage;
- injected `YtDlpExtractorAdapter` process spy остаётся 0.

Failure vertical `native_dynamic_dash_keeps_profile_network_malformed_and_cancel_failures_distinct` покрывает unsupported addressing, malformed schema, missing/network route, pre-cancel и process spy 0.

## Verification §6.3

PASS:
- `cargo test -p web-media-dash --all-targets --all-features --locked` (41 unit + 4 dynamic + 3 live runtime + 4 catalog);
- N10 app live vertical и failure semantics test;
- fresh N10 live vertical cohort 3/3;
- N09 static DASH H.264/AAC + VP9/Opus switch/seek/reopen regression;
- player paused-expired live recovery, pre-receipt expiry->Paused, app live same-lineage restore и Playing/Paused Installed-barrier tests;
- strict Clippy `web-media-dash` + `app-egui`, all targets/features, `-D warnings`;
- `cargo check --workspace --all-targets --all-features --locked`;
- `cargo fmt --all -- --check`;
- `git diff --check`;
- Serena diagnostics/reference audit.

По §6.3 не запускались full workspace tests, public-network acceptance, GUI/hardware, MSRV, dependency/release/pre-PR/coverage gates.

Связанные memories: `mem:media-services/dash-live-s35-2026-07-24`, `mem:player-core/dynamic-live-timeline-s31l-2026-07-23`, `mem:media-services/native-dash-vod-n09-2026-09-01`.
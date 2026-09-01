# N08 native HLS live/DVR без yt-dlp (2026-09-01)

## Результат и commit

- Session N08 выполнена локальным commit `feat(hls): admit live manifests without yt-dlp`.
- Supported sliding-live и EVENT top/media profiles теперь admitted native и используют существующий `web-media-hls::live` S33 runtime. До N09 работа не продолжалась.
- Blanket extractor vocabulary удалена полностью: `LiveOrEventPlaylist`, `LiveRequiresExtractor` и `admit_native_hls_vod` отсутствуют. Low-load `admit_native_hls` presentation-neutral, полный `admit_native_hls_catalog` возвращает typed `NativeHlsPresentationEvidence`.

## Ownership и границы

- `hls-playlist-core` остаётся владельцем profile validation. Initial live принимает sliding и `EXT-X-PLAYLIST-TYPE:EVENT`; explicit `PLAYLIST-TYPE:VOD` без ENDLIST отклоняется. LL-HLS, private state, unsupported encryption/DRM и прочие прежние exclusions не расширялись.
- `web-media-hls::catalog::detect_hls_catalog_presentation` классифицирует selected master child по authoritative fetched root. Root повторно не GET-ится; child classification/profile check выполняется до app runtime composition.
- `web-media-hls::live::prepare_hls_live_with_catalog` принимает `FetchedTop` только для initial handoff, затем нормализует refresh input к stable selected root `Fetch`; stale fetched bytes никогда не replay-ятся.
- App `startup_media::native_hls::catalog_runtime` (файл `startup_media/native_hls/vod_catalog.rs` через path module) владеет одним native admission/catalog/rematch orchestration для VOD и live. Data plane не дублируется: VOD делегируется прежнему VOD owner-у, live — существующему S33 owner-у.
- `PreparedNativeHlsLifecycle` типизирует взаимоисключающие attachments:
  - VOD: authoritative post-target seek, initial position, armed VOD endpoint recovery;
  - Live: worker-receipted DVR seek и `DynamicMediaTimelinePort`, без VOD recovery.
- `NativeHlsEndpointRefreshPort` принадлежит app native HLS live path: после endpoint expiry увеличивает generation, заново получает stable root с той же bounded HTTP/cancellation policy и возвращает `FetchedTop`. Он не знает yt-dlp и не запускает процессы.
- Durable/provider-neutral source сохраняет stable root lineage + semantic selection; fresh catalog ordinal используется только внутри snapshot. Same-item Playing/Paused switch продолжает использовать общий Installed barrier и app live restore owner.

## Ошибки и fallback

- До admission fallback разрешён только для строго не-HLS body, недостаточных declared master evidence и 401/403 authorization material.
- Malformed HLS, unsupported profile, network failure, cancellation, semantic rematch failure и любые post-admission/runtime failures терминальны; они не переходят в extractor.
- Provider-default alternate HE-AAC можно изолировать как row-local capability rejection для native catalog: proven video row остаётся допустимым video-only и не вызывает extractor. Strict default catalog policy для остальных callers сохранена.
- Native live endpoint recovery различает cancellation и bounded refresh failure; profile/malformed replacement отвергается существующим live runtime без fallback.

## Функциональное доказательство

Hermetic app vertical `native_sliding_hls_live_reaches_moving_frame_audio_and_expires_old_dvr_without_extractor` использует loopback master, sliding MPEG-TS live row и fMP4 alternate row с реальными repo H.264/AAC assets. Он доказывает:

- initial root GET ровно один раз;
- два последовательных H.264 кадра доходят decoder -> WGPU submit/release;
- два AAC packet batch дают непустой PCM;
- refresh публикует сдвинутое DVR окно;
- retained target проходит receipted seek, старый target после shift возвращает typed expiry без clamp;
- transient segment expiry инициирует single stable-root endpoint recovery и поток продолжается;
- semantic same-item switch TS -> fMP4 снова достигает decoder/render/audio;
- root accounting: initial + endpoint recovery + switch = 3;
- injected `YtDlpExtractorAdapter` process spy = 0.

Новый concurrency-sensitive vertical прошёл повторно 3/3 до финального self-review и ещё раз после последних API cleanup. N07 native HLS VOD TS/fMP4 switch/seek/reopen vertical также прошёл, поэтому finite regression не внесена. Playing и Paused lifecycle tests и обе app live same-item restore tests прошли.

## Verification §6.3

PASS:

- `cargo test -p hls-playlist-core -p web-media-hls --all-targets --all-features --locked`
- `cargo test -p app-egui --all-features native_sliding_hls_live_reaches_moving_frame_audio_and_expires_old_dvr_without_extractor -- --nocapture`
- `cargo test -p app-egui --all-features native_hls_master_ts_fmp4_switch_seek_reopen_reaches_consumers_without_extractor -- --nocapture`
- `cargo test -p app-egui --all-features resolved_url_action_keeps_position_and_commits_only_after_installed -- --nocapture`
- focused `live_same_lineage_restore` и `live_restore_outcomes`
- `cargo clippy -p hls-playlist-core -p web-media-hls -p app-egui --all-targets --all-features --locked -- -D warnings`
- `cargo check --workspace --all-targets --all-features --locked`
- `cargo fmt --all -- --check`
- `git diff --check`
- Serena diagnostics и final reference audit.

По §6.3 не запускались full workspace tests, public-network acceptance, GUI/hardware, release/MSRV/pre-PR/coverage gates; это не scope feature session N08.

## Связанные memories

- Предыдущий finite path: `mem:media-services/native-hls-vod-n07-2026-09-01`.
- Live runtime foundation: `mem:media-services/hls-live-s33-2026-07-24`.
- AVC3/fMP4 live support: `mem:media-services/hls-live-avc3-2026-08-10`.

## N14B flush-safe DVR correction (2026-09-02)

- N14B strengthened the former receipt-only retained-seek proof to require post-receipt FFmpeg/WGPU frame and nonzero PCM after the same video/audio reset used by `PlayerSession`.
- This exposed a root defect: HLS live accepted any TS H.264 IDR as a DVR decode anchor even when the retained access unit lacked SPS/PPS, so FFmpeg flush left audio progressing but video unable to restart.
- `web-media-hls::live` now accepts an H.264/TS video anchor only with codec-core proof of ordered in-band SPS -> PPS -> IDR. Incomplete IDR packets remain valid playback packets but do not expand the seekable DVR range; fMP4/other-codec policy and ordinary MPEG-TS index semantics are unchanged.
- Exact tests, dependency/API detail and §6.3 evidence: `mem:testing/native-web-ingress-n14b-2026-09-02`.

# N13A cross-protocol recovery/fallback closure (2026-09-01)

## Архитектурная граница

- `app-egui::media_open::native_fallback::NativeWebFallbackOwner` теперь единственный app-owned owner native -> extractor fallback для HLS, DASH, Smooth и HDS. Protocol owners возвращают только `NativeWebMediaAttempt::RequiresExtractorFallback(WebMediaFallbackTrigger)`; protocol-specific fallback enums и повторная классификация удалены.
- Owner хранит `web_media_core::WebMediaFallbackGate` и initial page `YtDlpMediaLocator`. `before_installed` допускает максимум один allowlisted trigger, `installed` не хранит locator и всегда возвращает `AfterInstalled`.
- Exact mapping сохраняется до subprocess boundary: ProviderDocument -> PageMediaResolution, ExtractorOwnedAuthorizationMaterial -> та же exact reason, UnsupportedNativeProfile -> NativeProfileCompatibilityFallback. Forbidden classes cancellation/network/malformed/expired/backpressure/invariant/decoder/renderer не расходуют legal attempt и никогда не запускают extractor.
- DASH ContentProtection является terminal manifest/profile error и больше не маскируется fallback-ом.

## Extractor reason и page rows

- `WebMediaOpenAdapter::Extractor`, его view и `WebMediaOpenRequest::extractor` несут явный `ExtractorInvocationReason`.
- Initial page resolution получает `PageMediaResolution`; native fallback — exact claimed reason; extractor-backed selection switch, settings reconfigure, controlled reopen и installed page recovery — `ExtractorBackedRecovery`.
- Queue preload сохраняет reason без переклассификации. `prepare_yt_dlp_web_media` валидирует совместимость reason с BestPlayable/Exact/Composed intent; collection topology в media-open path запрещена.
- Page rows сохраняют исходный extractor locator и на refresh/reopen выполняют fresh extraction + semantic rematch, а не endpoint replay.

## Stable-root endpoint recovery

- `WebMediaSourceIntent` выбирает recovery через `WebMediaRecoveryStrategy::for_reconstructible_ingress`.
- Direct HTTP/FTP preparation теперь создаёт и передаёт `VodEndpointRecoveryAttachment`; HTTP/FTP transport request получает expiry observer, attachment вооружается только после успешной demux/candidate подготовки.
- Installed routing исчерпывающий: Direct -> stable resource reopen; Native HLS/DASH/Smooth/HDS -> stable manifest refresh + semantic rematch; Extractor -> fresh extraction + semantic rematch. Все reconstructible owners дают controlled reopen без fallback.
- Temporary endpoint без reconstructible owner завершается typed `VodEndpointRecoverySourceError::TerminalUnreconstructibleEndpoint`; отсутствующий web source/capabilities/reopen также остаются отдельными typed outcomes.

## Проверки и evidence

- Matrix tests покрывают ровно один allowed pre-Installed fallback с exact reason, второй attempt, каждый forbidden trigger и всю post-Installed trigger matrix с extractor admissions = 0.
- Direct functional vertical: HTTP Range Ogg open/seek/reopen даёт nonzero Vorbis PCM и armed/claimable endpoint recovery; HTTP 200 и FTP Ogg дают nonzero PCM; WebM VP9 достигает decode -> WGPU submit/readback/release.
- HLS VOD/live, DASH static/live, Smooth VOD и HDS VOD verticals проходят decoder/render/nonzero PCM и process spy 0. DASH failure vertical покрывает profile/malformed/network/cancel/DRM без fallback.
- PASS: fmt, diff check, strict app-egui all-target/all-feature Clippy, workspace all-target/all-feature check, web-media-core tests, focused fallback/recovery/direct/protocol/same-item/controlled-reopen tests и обычный three-run direct HTTP cohort.
- Первичный параллельный HLS filter дал timing failure live availability при конкуренции с VOD; isolated и затем полный serial HLS filter прошли. Concurrency-sensitive runtime не менялся.
- Дополнительная (не §6.3) no-default-features попытка app test не компилируется из-за прежних N07–N12 test-модулей, которые без cfg импортируют ffmpeg-gated `direct_progressive_webm`; production all-features gates зелёные, unrelated feature-gating refactor в N13A не смешивался.

## Связи и следующий шаг

- Уточняет временные gaps после `mem:media-services/native-progressive-http-ftp-n06-2026-08-31`, `mem:media-services/native-hls-vod-n07-2026-09-01`, `mem:media-services/native-hls-live-n08-2026-09-01`, `mem:media-services/native-dash-vod-n09-2026-09-01`, `mem:media-services/native-dash-live-n10-2026-09-01`, `mem:media-services/native-smooth-vod-n11-2026-09-01`, `mem:media-services/native-hds-vod-n12-2026-09-01`, а также правила `mem:media-services/vod-endpoint-recovery-aud009-2026-08-23` и `mem:media-services/content-probed-runtime-fallback-2026-08-05`.
- Локальный commit создан с сообщением `fix(web-media): enforce source-owned recovery and fallback policy`; точный hash фиксируется в session handoff.
- N13B не начинался; следующий session ID — N13B.

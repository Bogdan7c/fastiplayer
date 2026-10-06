# Понятные причины отказа web-open (UX edge cases, сессия 08, 2026-10-06)

План: `user/ux-edge-cases/08-web-open-error-reasons.md`, итог: `user/ux-edge-cases/results/08.md`.

## Решения владельца
- HTTP 404/410/429/5xx классифицируются отдельно в `web-media-http` (корень «мёртвая ссылка = нет сети»).
- yt-dlp: stderr разбирается внутри `service-ytdlp` по меткам `ERROR:` (текст не хранится и не покидает сервис).
- В тексте только домен: «Не удалось открыть ссылку (youtube.com): …» (ведущий `www.` убирается).
- Объём показа: CLI/восстановление очереди (окно + бейдж) и бейдж строки очереди. Пересборка после настроек (`settings_runtime_adapter`, `"web media rebuild failed: {kind:?}"`) и тексты классификации URL (`StartupUrlUnsupportedReason::safe_error`, «NetworkError: …») — НЕ трогали (бэклог).

## Цепочка владельцев
1. `web-media-transport-api::TransportFailure` + `NotFound`/`Gone`/`RateLimited`/`ServerError`; intent `allows_alternate_candidate()` (true для NetworkUnavailable и 4 новых) — единственный production-потребитель `content_probe_fallback::CandidateOpenError::from` сохраняет прежний выбор alternate.
2. `web-media-http::map_source_open_error` + `http_status_transport_failure(u16)`; прочие не-2xx (400/405/408…) остаются `NetworkUnavailable`.
3. `service-ytdlp`: `YtDlpServiceError::ExecutableNotFound` (spawn `io::ErrorKind::NotFound` в `process.rs`); `ExtractorRejection { stderr_bytes, reason: YtDlpRejectionReason }`. Модуль `rejection_reason.rs`: `StderrRejectionClassifier` (потоково, ≤2 KiB одной строки, первая распознанная `ERROR:`-строка), правила `CLASSIFICATION_RULES` (порядок важен: Private раньше Sign in; HTTP-статус раньше «unavailable»). `process_output::StderrObservation { observed_bytes, rejection_reason }` вместо `usize`. Topology (`topology/process.rs`) не менялась (сессия 09).
4. `media-source-open::web_open_failure`: `WebOpenFailureReason` (26 вариантов, Copy) + `classify_web_open_failure(&anyhow::Error)` — обход `error.chain()` с `downcast_ref` (YtDlpServiceError, TransportOpenError, ProviderOpenError, SourceError, DemuxOpenError [FactoryRejected::Backend → глубже], ContentProbeRejection [стал pub(crate) + re-export в `web_media_open`], ComponentVariantFinalizationError, HdsNoPlayableRendition, DashDynamicMpdError). Первое осмысленное звено побеждает; отмена → пропуск; ничего → `Unclassified`.
5. app-egui:
   - `MediaPreparationFailureKind::{DirectOpen, NativeHlsOpen, NativeDashOpen, NativeHdsOpen, NativeSmoothOpen, ExtractorOpen}(WebOpenFailureReason)`; intent `web_open_failure_reason()`, `user_failure_reason()`. Отказ fallback-gate / composition → `Unclassified`; extractor выключен → `ExtractorDisabled`.
   - `MediaOpenUserFailureReason::WebOpen(_)` (+From) → бейдж строки автоматически через `playlist_target_failure_summary`.
   - Тексты: `web_open_message.rs` (`web_open_failure_message(display_host, reason)`, `web_open_failure_phrase`); общий диспетчер фраз — `local_open_message::failure_reason_phrase` (pub(crate)).
   - Домен: `StartupUrlLocator::display_host()` (`url::Url::host_str`), запоминается в `StartupUrlLocator::start` → `StartupMediaController::remember_web_display_host`.
   - Startup jobs (yt-dlp, direct, native HLS/DASH/HDS/Smooth): `Result<_, StartupWebPreparationFailure { reason, diagnostic }>` (`startup_media/web_failure.rs`) вместо `String`; `drain.rs` → `handle_web_preparation_failure` (лог: reason + diagnostic; окно + бейдж — человеческий текст).
   - Native startup «yt-dlp выключен» — `anyhow::Error::new(YtDlpServiceError::AdapterDisabled).context(..)` вместо строки.
   - `StartupPendingInstall.target: StartupInstallTarget { Local, Web { display_host }, PlaylistEntry }` (было `local_target: Option`): отказ player-а для web-ссылки при старте → web-шаблон; PlaylistEntry сохраняет прежний технический текст.
   - `YtDlpStartupAdapter::validate_config` (CLI, yt-dlp выключен) → тот же web-текст.

## Тесты
- `web-media-http/src/tests.rs`: `http_error_statuses_are_typed_instead_of_network_unavailable`, `unclassified_http_status_keeps_previous_network_category`, `connection_closed_without_response_is_network_unavailable_not_http_status` (loopback `TestServerBehavior::{Status, CloseWithoutResponse}`).
- `content_probe_fallback/tests.rs::best_playable_http_status_failures_keep_one_alternate_and_exact_reason`.
- `service-ytdlp`: `rejection_reason/tests.rs` (реальные строки yt-dlp 2026.08.19), `process/tests.rs::{rejected_process_reports_recognized_reason_without_stderr_text, successful_exit_ignores_error_lines_in_stderr, missing_executable_is_typed_not_found}`.
- `media-source-open/src/web_open_failure/tests.rs`: синтетические цепочки + настоящий `open_direct_media` против loopback (404/403/429/close/не-media) + настоящий adapter с `PathOverrideLauncher` (пустой PATH → ExtractorNotInstalled; скрипт «Private video» → SitePrivateMedia).
- app: `media_open/preparation/web_failure_tests.rs` (prepare_source + loopback → тексты окна/строки, без секретов; `LoopbackServer` переиспользуется), `coordinator/tests/player_failure_tests.rs::web_preparation_failure_reports_reason_and_keeps_old_media_playing` (настоящий PlayerWorker), `web_open_message/tests.rs`, `startup_media/web_failure/tests.rs`, `pending_install` (web/playlist entry), `url_service_adapter/tests.rs` (display_host без секретов, disabled CLI текст), `failure_summary_tests::web_preparation_failure_puts_specific_reason_on_queue_row`.

## Ручной прогон (2026-10-06, агент, PASS)
- Найдено и исправлено: метка YouTube `video is unavailable` (сервер отдаёт текст причины сам); fallback классификатора — если глубже нет типизированной причины, а `DemuxOpenError::FactoryRejected{Backend}` в цепочке → `UnrecognizedFormat` (`fallback_reason_without_typed_cause`).
- Приёмы без вреда системе владельца: `unshare -rn <бинарь>` = «нет сети» только для плеера; `env PATH=<пустой каталог>` = «yt-dlp не установлен»; восстановленная очередь — `playlist-state.json` schema v1 (`items` с `{"kind":"url","reopenable_url":…}`) в изолированном `XDG_CONFIG_HOME`; рабочее web-видео — `python3 -m http.server` (без Range → mp4 нужен `-movflags +faststart`). Приватное видео для проверки: `youtube.com/watch?v=yZIXLfi8CZQ` (из тестов yt-dlp). `.m3u` при старте ждёт клика «Импортировать».
- Осторожно: `pkill -f <шаблон>` убивает собственный шелл агента, если шаблон есть в его командной строке — убивать по PID (`pgrep -f "^…"`).

## Ограничения
- TLS не отличим от «нет соединения» (reqwest одна категория `HttpRequest`).
- Метки yt-dlp могут поменяться в новой версии → честный `SiteRejected` (общая фраза).
- Ручной клик по строке очереди: только бейдж + статусная строка (окно не показывает, как и раньше).
- Строки spawn-ошибок потоков startup («NetworkError: YtDlp error: …») не менялись — редкий OS-сбой.

Связанные: `mem:app-egui/local-open-error-messages-ux02`, `mem:app-egui/notifications`, `mem:media-source-open/core`, `mem:media-services/core`.

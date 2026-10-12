# Английские пользовательские тексты Fastiplayer

Интерфейс, уведомления, пользовательские ошибки, диагностические сообщения и
вывод служебных команд проекта используют английский язык. Комментарии и
документация сохраняются на русском. Имена медиа, пути, пользовательские данные
и Unicode-фикстуры сохраняются буквально.

## Совместимость и границы

- `ui.language` сохраняет default `"ru"`, валидацию, сохранение и применение.
  Сейчас значение зарезервировано для будущей локализации и не выбирает язык
  интерфейса. Это прямо объясняется в описании настройки.
- Имена `label_ru`, `description_ru`, `help_ru`, `fallback_ru`, ключи локализации,
  идентификаторы настроек и виджетов остаются прежними. В исторических полях
  `*_ru` временно находится английский текст.
- Сообщения переводятся в модулях, которые ими владеют. Типизированные ошибки,
  маршруты обработки, Rust API, формат конфигурации, структурированные поля логов
  и схемы JSON/CSV сохраняются. Миграция конфигурации не нужна.
- Анализаторы принимают новый английский текстовый контракт. Основные маркеры:
  `Final seek commit completed`, `Final seek commit stopped on timeout`,
  `Demuxer reported a track list update`,
  `Final seek commit continued with audio gate soft fallback`.
  Последний маркер распознаётся анализаторами; перевод не добавляет новый
  fallback-переход в playback runtime.
- Язык стандартных системных кнопок и вывода сторонних программ определяется
  настройками этих программ.

## Проверка результата

Существующие тесты проверяют текст после отрисовки кнопок, плейлиста,
подтверждений, статусов Settings и ошибок. Проверки понятности пользовательских
ошибок продолжают запрещать внутренние идентификаторы, Debug-представления и
необработанные технические причины.

Дополнительные тесты в
`crates/app-egui/src/settings_runtime/tests/english_texts.rs` сравнивают
нарисованные подписи при `ui.language = "ru"` и `"en"`, проверяют сохранение
самого значения настройки, а также расположение и реальные клики Apply/Cancel
при минимальной допустимой ширине сайдбара. Существующий тест таймлайна
проверяет LIVE/DVR в окне шириной 400.

Тесты Python-анализаторов проверяют английские события завершения seek,
timeout, обновления дорожек, audio fallback и ошибок через итоговые статусы и
метрики. Новые случаи timeout и track update находятся в
`scripts/tests/test_parse_seek_diagnostics.py`.

## Итоговый аудит оставшейся кириллицы

Строковые литералы Rust проверены с исключением комментариев и символьных
литералов; для каждого оставшегося случая ниже указаны файл, строки и назначение.
Пояснения lint в `#[expect(..., reason = ...)]` являются документацией
инвариантов для разработчика и сохраняются на русском вместе с комментариями.
Русский текст внутри документирующих атрибутов, примеров исходного кода и
комментариев генерируемого TOML относится к документации, а не к сообщениям
интерфейса. Входные Unicode-данные и ожидаемые результаты тестов сохраняют
проверки кодировок, экранирования, названий и безопасной обработки ошибок.

- Unicode-фикстура/ввод теста: 195 литералов.
- Пояснение lint/документация: 173 литералов.
- Русская документация (включая комментарии TOML): 100 литералов.

| Файл | Строки | Назначение |
| --- | --- | --- |
| `crates/app-egui/src/app_instance/forwarding/tests.rs` | 70, 78, 90, 91, 92, 213 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/app_shell/escape_dismissal/tests.rs` | 199 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/app_wake.rs` | 262, 284 | Пояснение lint/документация |
| `crates/app-egui/src/external_open/pipeline_tests.rs` | 161, 199 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/external_open/uri/tests.rs` | 10, 147 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/fatal_startup/dialog_utility/tests.rs` | 13, 81, 101 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/fatal_startup/tests.rs` | 299 (×2), 302 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/instance_forwarding/tests.rs` | 283, 287 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/local_open_message/tests.rs` | 69 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/media_open/coordinator.rs` | 220, 385, 543, 563, 731 | Пояснение lint/документация |
| `crates/app-egui/src/media_open/coordinator/player_staging.rs` | 88, 153, 185, 215 | Пояснение lint/документация |
| `crates/app-egui/src/media_open/executor.rs` | 276 | Пояснение lint/документация |
| `crates/app-egui/src/media_open/player_failure/tests.rs` | 71 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/media_open/preparation.rs` | 73, 176 | Пояснение lint/документация |
| `crates/app-egui/src/media_open/preparation/tests.rs` | 71, 76, 82, 83, 89, 94, 95 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/media_open/speculative.rs` | 97 | Пояснение lint/документация |
| `crates/app-egui/src/media_open/types.rs` | 290 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/compound_view.rs` | 473, 589 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller.rs` | 152 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller/automatic_lifecycle.rs` | 733 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller/discovery.rs` | 49 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller/discovery_navigation.rs` | 207 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller/manual_navigation.rs` | 261, 391 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller/removal.rs` | 41, 50, 96, 131, 158, 168, 191 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/controller/startup_restore/open_locator_tests.rs` | 94 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/playlist_runtime/controller/transport.rs` | 563 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/discovery.rs` | 107 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/discovery/action_jobs.rs` | 48, 363 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/discovery/metadata_sort/runtime.rs` | 36, 52, 85, 151, 165 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/discovery/yt_dlp_metadata/tests.rs` | 101, 115, 122, 125, 163, 192, 232 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/playlist_runtime/dropped_collection/tests.rs` | 236, 310, 461 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/playlist_runtime/import_io/materializer.rs` | 257 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/import_path/tests.rs` | 134, 161, 165 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/playlist_runtime/import_path/tests.rs` | 46, 137 | Русская документация (включая комментарии TOML) |
| `crates/app-egui/src/playlist_runtime/persistence.rs` | 172 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/replacement_confirmation/local_files.rs` | 132, 140, 143, 151 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/playlist_runtime/settings.rs` | 332 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/shell_boundary.rs` | 148, 179 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/startup.rs` | 418 | Пояснение lint/документация |
| `crates/app-egui/src/playlist_runtime/url_import/tests.rs` | 40, 59, 474, 569, 600 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/process_shutdown.rs` | 99 | Пояснение lint/документация |
| `crates/app-egui/src/settings_runtime/tests/hardware_decode_hint.rs` | 69 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/settings_ui/field_widget_tests.rs` | 26 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/startup_media/orchestration.rs` | 735 | Пояснение lint/документация |
| `crates/app-egui/src/startup_media/orchestration/web_preparation.rs` | 80 | Пояснение lint/документация |
| `crates/app-egui/src/startup_media/user_stop/tests.rs` | 44 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/startup_readiness.rs` | 712 | Пояснение lint/документация |
| `crates/app-egui/src/state/notifications/buffering/tests.rs` | 182 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/state/notifications/render_tests.rs` | 237 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/state/notifications/tests.rs` | 130, 144, 180, 181, 195 (×3), 202 (×2), 211, 279, 341, 373, 404, 422, 456 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/state/playlist_transport.rs` | 418 | Пояснение lint/документация |
| `crates/app-egui/src/state/same_item_candidate_switch.rs` | 367 | Пояснение lint/документация |
| `crates/app-egui/src/state/strong_media_open.rs` | 195 | Пояснение lint/документация |
| `crates/app-egui/src/state/strong_media_open/pending/resume.rs` | 415 | Пояснение lint/документация |
| `crates/app-egui/src/state/tests.rs` | 557, 798 | Русская документация (включая комментарии TOML) |
| `crates/app-egui/src/state/ui_runtime.rs` | 553, 608 | Пояснение lint/документация |
| `crates/app-egui/src/state/vod_endpoint_recovery.rs` | 218 | Пояснение lint/документация |
| `crates/app-egui/src/state/vod_endpoint_recovery_claim_policy_tests.rs` | 351 | Русская документация (включая комментарии TOML) |
| `crates/app-egui/src/ui/edge_slide/tests.rs` | 17 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/ui/egui_behavior.rs` | 224, 248 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/ui/playlist/import_preview.rs` | 93, 98 | Пояснение lint/документация |
| `crates/app-egui/src/ui/playlist/status/tests.rs` | 195 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/ui/playlist/tests.rs` | 45, 548, 552, 564, 587, 1262, 1273, 1334, 1335, 1350, 1353, 1364, 1372 | Unicode-фикстура/ввод теста |
| `crates/app-egui/src/video_pipeline_candidate.rs` | 32, 329, 428, 447, 468, 512, 556, 566, 684 | Пояснение lint/документация |
| `crates/app-egui/src/video_pipeline_candidate/protocol.rs` | 33 | Пояснение lint/документация |
| `crates/config/src/schema/default_document.rs` | 403 | Пояснение lint/документация |
| `crates/config/src/schema/default_document.rs` | 6, 11, 16, 21, 26, 31, 36, 41, 46, 51, 56, 61, 66, 71, 76, 81, 86, 91, 96, 101, 106, 111, 116, 121, 126, 131, 136, 141, 146, 151, 156, 161, 166, 171, 176, 181, 186, 191, 196, 201, 206, 211, 216, 221, 226, 231, 236, 241, 246, 251, 256, 261, 266, 271, 276, 281, 286, 291, 296, 301, 306, 311, 316, 321, 326, 331, 336, 341, 346, 351, 356, 361, 366, 371, 376, 381, 386, 388, 392, 397 | Русская документация (включая комментарии TOML) |
| `crates/config/src/schema/web_media_settings.rs` | 412 | Пояснение lint/документация |
| `crates/config/src/schema/yt_dlp_settings.rs` | 249 | Пояснение lint/документация |
| `crates/config/src/store/tests/frame_server.rs` | 238, 240, 242, 244 | Русская документация (включая комментарии TOML) |
| `crates/config/src/store/tests/persistence_sections.rs` | 97 | Unicode-фикстура/ввод теста |
| `crates/config/src/store/tests/persistence_sections.rs` | 23, 30, 33, 35, 44, 46, 73 | Русская документация (включая комментарии TOML) |
| `crates/config/src/store/tests/ui_and_legacy.rs` | 57, 97 | Русская документация (включая комментарии TOML) |
| `crates/cros-codecs-patch/src/lib.rs` | 25 | Пояснение lint/документация |
| `crates/cros-libva-patch/src/lib.rs` | 12, 16 | Пояснение lint/документация |
| `crates/demux-api/src/progressive/worker.rs` | 525 | Пояснение lint/документация |
| `crates/demux-api/src/registry.rs` | 566 | Пояснение lint/документация |
| `crates/desktop-integration/src/instance_forwarding/admission/tests.rs` | 18, 27, 125 | Unicode-фикстура/ввод теста |
| `crates/desktop-integration/src/notification/linux/tests.rs` | 165, 177, 228, 229 | Unicode-фикстура/ввод теста |
| `crates/desktop-integration/src/notification/linux/tests.rs` | 105 | Пояснение lint/документация |
| `crates/flv-demux/src/factory.rs` | 61 | Пояснение lint/документация |
| `crates/frame-server-core/src/scheduler.rs` | 387 | Пояснение lint/документация |
| `crates/frame-server-core/src/state_machine.rs` | 460, 487 | Пояснение lint/документация |
| `crates/media-prefetch/src/buffer.rs` | 238, 284 | Пояснение lint/документация |
| `crates/media-prefetch/src/config.rs` | 85, 95 | Пояснение lint/документация |
| `crates/media-source-open/src/direct_progressive_open.rs` | 113 | Пояснение lint/документация |
| `crates/media-source-open/src/local_media.rs` | 133, 147 | Пояснение lint/документация |
| `crates/media-source-open/src/native_startup/hls/vod_catalog.rs` | 276 | Пояснение lint/документация |
| `crates/media-source-open/src/safe_media_label.rs` | 68 | Unicode-фикстура/ввод теста |
| `crates/media-source-open/src/web_media_catalog/attachment.rs` | 76 | Пояснение lint/документация |
| `crates/media-source-open/src/web_media_catalog/model.rs` | 131 | Пояснение lint/документация |
| `crates/media-source-open/src/web_media_open/catalog.rs` | 209, 343 | Пояснение lint/документация |
| `crates/media-source-open/src/web_media_quality.rs` | 30, 40 | Пояснение lint/документация |
| `crates/media-source-open/src/web_media_vod_recovery.rs` | 67, 191 | Пояснение lint/документация |
| `crates/media-source-open/src/web_open_failure/tests.rs` | 320 | Unicode-фикстура/ввод теста |
| `crates/mpeg-ts-demux/src/options.rs` | 53 | Пояснение lint/документация |
| `crates/player-core/src/media_install.rs` | 73 | Пояснение lint/документация |
| `crates/player-core/src/media_install/playback_intent.rs` | 431 | Пояснение lint/документация |
| `crates/player-core/src/pipeline/audio_clock_mapping.rs` | 110, 141, 395 | Пояснение lint/документация |
| `crates/player-core/src/render_lease_bridge.rs` | 46 | Пояснение lint/документация |
| `crates/player-core/src/session.rs` | 705 | Пояснение lint/документация |
| `crates/player-core/src/worker.rs` | 185, 270 | Пояснение lint/документация |
| `crates/playlist-core/src/entry.rs` | 545 | Пояснение lint/документация |
| `crates/playlist-core/src/payload/tests.rs` | 143 | Unicode-фикстура/ввод теста |
| `crates/playlist-core/src/playback_span.rs` | 63 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/automatic.rs` | 287, 387 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/entries.rs` | 265 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/mod.rs` | 444 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/navigation.rs` | 356, 444, 575 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/removal.rs` | 28, 100 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/removal/tests.rs` | 19 | Unicode-фикстура/ввод теста |
| `crates/playlist-core/src/queue/reordering.rs` | 148, 303 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/reservation.rs` | 171, 208, 244 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/shuffle/removal.rs` | 99 | Пояснение lint/документация |
| `crates/playlist-core/src/queue/shuffle/runtime.rs` | 227, 252, 390, 632 | Пояснение lint/документация |
| `crates/playlist-discovery/src/readiness_ack.rs` | 28 | Пояснение lint/документация |
| `crates/playlist-discovery/src/request.rs` | 339 | Пояснение lint/документация |
| `crates/playlist-io/src/cue/parser.rs` | 300 | Пояснение lint/документация |
| `crates/playlist-io/src/export/locator.rs` | 316 | Пояснение lint/документация |
| `crates/playlist-io/src/export/m3u8.rs` | 25 | Пояснение lint/документация |
| `crates/playlist-io/src/export/xspf.rs` | 15, 62 | Пояснение lint/документация |
| `crates/playlist-io/src/hls.rs` | 40 | Пояснение lint/документация |
| `crates/playlist-io/src/issue.rs` | 11 | Пояснение lint/документация |
| `crates/playlist-io/src/locator.rs` | 159 | Пояснение lint/документация |
| `crates/playlist-io/src/m3u.rs` | 202 | Пояснение lint/документация |
| `crates/playlist-io/src/source.rs` | 84 | Пояснение lint/документация |
| `crates/playlist-io/src/text_encoding.rs` | 104, 106, 126 | Unicode-фикстура/ввод теста |
| `crates/playlist-io/src/xspf/limits.rs` | 100 | Пояснение lint/документация |
| `crates/playlist-io/tests/export_core.rs` | 85, 89, 96, 100, 107, 176, 177, 178, 179, 203, 207, 209, 211, 453, 464, 503, 510, 516, 546, 558, 561 | Unicode-фикстура/ввод теста |
| `crates/playlist-io/tests/generic_m3u.rs` | 87, 91 | Unicode-фикстура/ввод теста |
| `crates/playlist-io/tests/generic_m3u.rs` | 74 | Русская документация (включая комментарии TOML) |
| `crates/playlist-io/tests/legacy_encoding.rs` | 72, 81, 104, 111, 112, 113, 114, 121 | Unicode-фикстура/ввод теста |
| `crates/playlist-io/tests/legacy_encoding.rs` | 61 | Русская документация (включая комментарии TOML) |
| `crates/playlist-state/src/tests.rs` | 139, 140, 161 | Unicode-фикстура/ввод теста |
| `crates/render-wgpu-video/src/video/mod.rs` | 527 | Пояснение lint/документация |
| `crates/service-ytdlp/src/candidate/request_material/dash.rs` | 144, 405 | Пояснение lint/документация |
| `crates/service-ytdlp/src/candidate/tests.rs` | 1875, 1890 | Unicode-фикстура/ввод теста |
| `crates/service-ytdlp/src/topology/parser/tests.rs` | 36, 71 | Unicode-фикстура/ввод теста |
| `crates/settings-core/src/controller/pending_apply/tests.rs` | 129, 146, 157, 188, 189, 240, 291, 332, 359 | Unicode-фикстура/ввод теста |
| `crates/settings-core/src/tests.rs` | 29, 390, 400, 404, 516, 591, 597, 598, 617, 667, 698, 707, 708, 718, 728, 745, 782, 796, 828, 856, 971, 1050, 1068, 1104, 1115, 1218, 1223, 1253, 1267, 1448 | Unicode-фикстура/ввод теста |
| `crates/settings-derive/src/codegen.rs` | 491, 578 | Пояснение lint/документация |
| `crates/settings-derive/tests/ui/unknown_editor.rs` | 11 | Unicode-фикстура/ввод теста |
| `crates/smooth-streaming-fmp4/src/reconstruction.rs` | 324 | Пояснение lint/документация |
| `crates/smooth-streaming-manifest-core/src/parser.rs` | 490 | Пояснение lint/документация |
| `crates/smooth-streaming-manifest-core/src/template.rs` | 226 | Пояснение lint/документация |
| `crates/smooth-streaming-manifest-core/src/timeline.rs` | 44, 113, 165, 191, 443 | Пояснение lint/документация |
| `crates/source-core/src/ftp_policy.rs` | 331 | Unicode-фикстура/ввод теста |
| `crates/source-core/src/http_cookie.rs` | 70, 192 | Пояснение lint/документация |
| `crates/source-core/src/http_cookie_seed.rs` | 53 | Пояснение lint/документация |
| `crates/symphonia-demux/src/packet_mapper.rs` | 62 | Пояснение lint/документация |
| `crates/video-ffmpeg/src/lib.rs` | 44 | Пояснение lint/документация |
| `crates/web-media-adaptive/src/completed_resource_cache.rs` | 153, 191, 220 | Пояснение lint/документация |
| `crates/web-media-adaptive/src/range_source.rs` | 171 | Пояснение lint/документация |
| `crates/web-media-adaptive/src/streaming_resource.rs` | 277 | Пояснение lint/документация |
| `crates/web-media-core/src/identity.rs` | 289 | Unicode-фикстура/ввод теста |
| `crates/web-media-core/src/variant/tests_identity.rs` | 65, 67 | Unicode-фикстура/ввод теста |
| `crates/web-media-dash/src/catalog.rs` | 710 | Пояснение lint/документация |
| `crates/web-media-dash/src/open.rs` | 52 | Пояснение lint/документация |
| `crates/web-media-hls/src/catalog/build.rs` | 414, 430 | Пояснение lint/документация |
| `crates/web-media-hls/src/tests.rs` | 512, 516 | Unicode-фикстура/ввод теста |
| `crates/web-media-smooth/src/demux/runtime.rs` | 77 | Пояснение lint/документация |
| `crates/web-media-smooth/src/demux/seek.rs` | 103 | Пояснение lint/документация |
| `crates/web-media-transport-api/src/secret.rs` | 175, 187 | Пояснение lint/документация |
| `crates/web-media-transport-api/src/tests.rs` | 571 | Unicode-фикстура/ввод теста |
| `crates/winit-patch/src/platform/external_drag.rs` | 119, 123, 129 | Unicode-фикстура/ввод теста |
| `crates/winit-patch/src/platform_impl/linux/external_drag/payload.rs` | 246, 274 (×2) | Unicode-фикстура/ввод теста |

В Python остаётся одна строковая фикстура:
`scripts/tests/test_check_refactor_guardrails.py:972`. Она содержит русский
комментарий исходного кода, на котором проверяется, что анализатор границ
игнорирует комментарии. Python docstrings и Shell-комментарии остаются русской
документацией; вывод Shell-команд и части Python f-строк переведены.
В `.github/*.yml` кириллица остаётся только в комментариях.

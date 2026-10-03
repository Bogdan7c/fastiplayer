//! Открытие источников медиа без UI: локальные файлы, прямые HTTP/FTP ссылки и
//! листовые части web-media orchestration (VOD-восстановление endpoint-а,
//! composition демуксеров, проекция каталога extractor-а, качество и adaptive
//! настройки), а также общие доменные типы web-media: composition
//! `PreparedMedia`, stream model с component variants и declared catalog.
//! Из YtDlp orchestration здесь пока только намерение выбора candidate-а,
//! финализация component variants и capability probe каталогов
//! (`web_media_open`).
//!
//! Crate вынесен из `app-egui` как рефакторинг границы: поведение, тексты ошибок,
//! redaction секретов и generation/identity fences не менялись. Он не зависит от
//! `egui`, `winit`, `wgpu`, `render-*` и `app-egui` (проверяет
//! `scripts/check-refactor-guardrails.py`). `app-egui` остаётся composition
//! root-ом и только вызывает эти модули.
//!
//! Модули объявлены в корне под теми же именами, что были в `app-egui`, чтобы
//! пути тестов (`web_media_quality::tests::...`) не изменились при переезде.

pub mod direct_progressive_open;
pub mod local_media;
pub mod prepared_web_media;
pub mod video_codec_mapping;
pub mod web_media_adaptive_config;
pub mod web_media_catalog;
pub mod web_media_demux_registry;
pub mod web_media_extractor_adapter;
pub mod web_media_hls_subtitles;
pub mod web_media_open;
pub mod web_media_quality;
pub mod web_media_stream_model;
pub mod web_media_vod_recovery;

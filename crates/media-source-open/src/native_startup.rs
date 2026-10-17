//! Чистая подготовка native web-источников при старте (HLS/DASH/HDS/Smooth):
//! fetch манифеста, admission вариантов, сборка демуксера и typed решение
//! «готово / нужен единственный откат на extractor».
//!
//! Перенесено из `app-egui/src/startup_media/native_*` (session-07 выноса
//! web-media). Подготовка работает в фоновом потоке и не трогает ни состояние
//! приложения, ни окно, ни wake. Фоновые job-ы (поток, почтовый ящик результата,
//! пробуждение UI, join), выбор стартовой позиции и сам вызов yt-dlp при откате
//! остаются в `app-egui` — это glue приложения. Поведение, тексты ошибок,
//! redaction URL-ов и generation/identity fences не менялись.

pub mod dash;
pub mod hds;
pub mod hls;
pub mod smooth;

/// DASH/HLS fMP4 с E-AC-3: сквозные вертикали до production-декодера (пакет G).
#[cfg(test)]
mod fmp4_dolby_vertical_tests;

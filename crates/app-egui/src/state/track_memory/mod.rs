//! Память выбора дорожек «для каждого видео» на стороне приложения.
//!
//! Что запоминается (решение владельца): выбранная пользователем аудиодорожка, выбор
//! субтитров (встроенная дорожка / файл / «выключено») и сдвиг синхронизации. Только ВЫБОР
//! ПОЛЬЗОВАТЕЛЯ: автовыбор не сохраняется. Приоритет при открытии: память видео > автовыбор
//! по языкам > флаги файла; отсутствующая дорожка молча уступает автовыбору.
//!
//! # Владельцы
//! - `playlist-state::track_memory` — нейтральное хранилище (ключ видео, LRU, файл, запись).
//! - `TrackMemoryOwner` (`owner`) — память процесса + отложенная запись; общий через
//!   [`TrackMemoryHandle`] (`startup`), создаётся в `AppShell`, переживает suspend.
//! - `identity` — источник media -> `VideoKey` (локальный путь / исходная ссылка / окно).
//! - `hints` — описание аудиодорожек и план восстановления звука.
//! - `app_state_api` — склейка с `AppState`: старт media, воронка выбора аудио, захват
//!   действий с субтитрами, ёмкость из настроек, срок записи для event loop-а.
//!
//! # Как восстанавливается
//! - Субтитры и сдвиг: `begin_media_restoring` у `SubtitleOwner` (`state/subtitles`).
//! - Звук: до начала воспроизведения, в `AppState::begin_post_installed_intent`
//!   (`state/strong_media_open/pending/remembered_audio.rs`).

mod app_state_api;
mod bridge;
mod hints;
mod identity;
mod owner;
mod startup;
#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) use bridge::{
    record_subtitle_changes, record_user_audio_choice, subtitle_restore_request,
};
#[cfg(test)]
pub(crate) use hints::plan_audio_restore;
pub(crate) use hints::{RememberedAudioPlan, hint_candidate_from_labels};
#[cfg(test)]
pub(crate) use identity::video_key_for_source;
pub(crate) use owner::TrackMemoryShutdownOutcome;
#[cfg(test)]
pub(crate) use owner::{RecordOutcome, TRACK_MEMORY_SAVE_DEBOUNCE};
pub(crate) use startup::{
    TrackMemoryHandle, TrackMemoryStartOutcome, start_track_memory,
    start_track_memory_in_memory_only,
};

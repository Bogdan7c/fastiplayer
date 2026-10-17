//! Выбор аудиодорожки из UI: меню кнопки «♪», клавиша `B` и единая точка отправки команды.
//!
//! # Владение
//! - Список аудиодорожек и текущий выбор принадлежат player-core (`PlayerSnapshot.tracks`,
//!   `selected_tracks.audio_track`). Здесь своего состояния нет: [`AudioTrackMenuView`] —
//!   read-only снимок, который заново строится из snapshot на каждый кадр.
//! - `labels` — как дорожка называется пользователю (язык, название, кодек).
//! - `web_versions` — web-версии звука того же видео (HLS/DASH audio renditions) из каталога
//!   `media-source-open`: подпись «English (E-AC-3 5.1)» и безопасное component-действие.
//! - `web_version_switch` — запуск переключения web-версии через существующий same-item
//!   component switch и уведомления о старте/отказе.
//! - `menu_view` — чистое построение снимка меню и выбор «следующей» дорожки.
//! - `control_actions` — применение намерений UI. Все выборы, которые сделал пользователь
//!   (клик по меню и клавиша `B`), проходят через `AppState::select_audio_track_by_user`.
//!
//! # Воронка выбора
//! `select_audio_track_by_user` — единственное место, где пользовательский выбор аудио
//! превращается в `PlayerCommand::SelectAudioTrack`. Будущая «память выбора для каждого видео»
//! подключается именно здесь (после успешной отправки), поэтому обходить воронку нельзя.
//! Автоматические выборы (восстановление сессии) идут другим путём и память не пишут.

mod control_actions;
mod labels;
mod menu_view;
#[cfg(test)]
mod tests;
mod web_version_switch;
mod web_versions;
#[cfg(test)]
mod web_versions_tests;

pub(crate) use menu_view::{AudioTrackMenuTarget, AudioTrackMenuView};

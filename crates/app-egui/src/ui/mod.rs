//! UI-слой `app-egui`, отделённый от playback/runtime деталей.
//!
//! Модули внутри `ui` читают snapshots и возвращают намерения пользователя.
//! Отправка `PlayerCommand` остаётся в `AppState`, чтобы визуальный слой не
//! получал доступ к worker/session internals.

pub mod animation;
pub mod assets;
pub mod media_info;
pub mod player_controls;
pub(crate) mod playlist;
pub(crate) mod queue_replacement_confirmation;
pub mod sidebar;
pub mod skin;
pub mod timeline;
pub mod titlebar_icon_area;
pub mod window_chrome;

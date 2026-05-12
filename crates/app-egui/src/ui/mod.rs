//! UI-слой `app-egui`, отделённый от playback/runtime деталей.
//!
//! Модули внутри `ui` читают snapshots и возвращают намерения пользователя.
//! Отправка `PlayerCommand` остаётся в `AppState`, чтобы визуальный слой не
//! получал доступ к worker/session internals.

pub mod animation;
pub mod assets;
pub mod player_controls;
pub mod skin;
pub mod timeline;

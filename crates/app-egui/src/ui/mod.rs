//! UI-слой `app-egui`, отделённый от playback/runtime деталей.
//!
//! Модули внутри `ui` читают snapshots и возвращают намерения пользователя.
//! Отправка `PlayerCommand` остаётся в `AppState`, чтобы визуальный слой не
//! получал доступ к worker/session internals.

pub mod animation;
pub mod assets;
pub(crate) mod buffering_indicator;
pub(crate) mod edge_slide;
pub(crate) mod egui_behavior;
pub(crate) mod external_drop_overlay;
pub(crate) mod fullscreen_chrome_panels;
pub(crate) mod keyboard_focus;
pub mod media_info;
pub(crate) mod notifications;
pub mod player_controls;
pub(crate) mod playlist;
pub(crate) mod queue_replacement_confirmation;
pub mod sidebar;
pub mod skin;
#[cfg(test)]
pub(crate) mod test_frame;
pub mod timeline;
pub mod titlebar_icon_area;
pub(crate) mod url_sidebar;
pub(crate) mod video_surface_input;
pub mod window_chrome;

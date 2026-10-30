//! Центр окна поверх видео: подтверждение замены очереди, предпросмотр импорта,
//! сообщение центра, спиннер ожидания данных, подсказка пустого окна и стопка toast-ов.
//!
//! Вынесено из `ui_runtime.rs` (лимит размера модулей): кадр зовёт эту функцию одним
//! вызовом. Текст подсказки пустого окна задаёт вид окна (`WindowViewProfile`): обычное
//! окно предлагает открыть файл, окно браузерного режима — честно ждёт страницу.

use player_core::PlaybackState;

use super::AppState;
use crate::ui::buffering_indicator::render_buffering_indicator;

impl AppState {
    /// Стартовая подсказка принадлежит только Idle: пауза, seek и EOF сохраняют media.
    /// Спиннер ожидания данных — самое неважное сообщение центра после подсказки Idle.
    /// Toast-ы уведомлений рисуются в углу центральной области всегда, поверх видео.
    #[expect(
        clippy::too_many_arguments,
        reason = "центр собирает независимые снимки разных владельцев (плеер, вид окна, уведомления, очередь); структура-обёртка только добавила бы слой"
    )]
    pub(super) fn render_center_overlay(
        ui: &mut egui::Ui,
        playback_state: PlaybackState,
        idle_center_hint: &str,
        notifications: &super::NotificationsFrame,
        notification_output: &mut crate::ui::notifications::NotificationUiOutput,
        playlist_import_preview: Option<&crate::playlist_runtime::PlaylistImportPreview>,
        playlist_confirmation: Option<&crate::playlist_runtime::PendingPlaylistConfirmation>,
        playlist_output: &mut crate::ui::playlist::PlaylistUiOutput,
    ) -> Option<crate::playlist_runtime::PlaylistConfirmationAction> {
        let mut confirmation_action = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                if let Some(model) = playlist_confirmation {
                    confirmation_action =
                        crate::ui::queue_replacement_confirmation::render(ui, model);
                } else if let Some(preview) = playlist_import_preview {
                    if let Some(action) = crate::ui::playlist::import_preview::render(ui, preview) {
                        playlist_output.push_action(action);
                    }
                } else if let Some(notice) = notifications.center.as_ref() {
                    crate::ui::notifications::render_center_notice(ui, notice, notification_output);
                } else if notifications.shows_buffering_indicator() {
                    render_buffering_indicator(ui, notifications.motion);
                } else if playback_state == PlaybackState::Idle {
                    ui.vertical_centered(|ui| {
                        ui.add_space(40.0);
                        ui.heading(idle_center_hint);
                    });
                }
                let toast_area_rect = ui.max_rect();
                crate::ui::notifications::render_toast_stack(
                    ui,
                    toast_area_rect,
                    notifications,
                    notification_output,
                );
            });
        confirmation_action
    }
}

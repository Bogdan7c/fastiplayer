//! Подсветка области, куда будут брошены перетаскиваемые в окно файлы.
//!
//! Пока над окном тащат данные, выбранная область (панель плейлиста или видео) заливается
//! полупрозрачным цветом с подсказкой по центру. Модуль только рисует готовую модель
//! [`DropOverlay`] от владельца `external_open` и ничего не решает и не хранит.
//!
//! Рисуется стандартными виджетами (`Area`, `Frame`, `Label`); `Area` не перехватывает
//! указатель, поэтому подсветка не мешает остальному интерфейсу.

use crate::external_open::{DropOverlay, DropTarget};

/// Stable egui id области подсветки: одна область на окно.
const OVERLAY_AREA_ID: &str = "fastiplayer_external_drop_overlay";

/// Прозрачность заливки подсветки (0.0 — невидима, 1.0 — сплошная).
const OVERLAY_FILL_OPACITY: f32 = 0.35;

/// Размер шрифта подсказки в точках.
const OVERLAY_HINT_FONT_SIZE: f32 = 20.0;

/// Подсказка для места назначения.
pub(crate) fn drop_hint_text(target: DropTarget) -> &'static str {
    match target {
        DropTarget::Playlist => "Отпустите, чтобы добавить в плейлист",
        DropTarget::Video => "Отпустите, чтобы открыть",
    }
}

/// Рисует подсветку, если над окном что-то тащат (`None` — ничего не рисует).
pub(crate) fn show_external_drop_overlay(ctx: &egui::Context, overlay: Option<DropOverlay>) {
    let Some(overlay) = overlay else {
        return;
    };
    egui::Area::new(egui::Id::new(OVERLAY_AREA_ID))
        .order(egui::Order::Foreground)
        .fixed_pos(overlay.rect.min)
        // Подсветка — только картинка: клики и наведение проходят сквозь неё.
        .interactable(false)
        // Появление мгновенное: встроенный fade игнорирует reduced motion.
        .fade_in(false)
        .show(ctx, |ui| {
            let fill = ui
                .visuals()
                .selection
                .bg_fill
                .gamma_multiply(OVERLAY_FILL_OPACITY);
            egui::Frame::NONE.fill(fill).show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    overlay.rect.size(),
                    egui::Layout::centered_and_justified(egui::Direction::TopDown),
                    |ui| {
                        ui.label(
                            egui::RichText::new(drop_hint_text(overlay.target))
                                .size(OVERLAY_HINT_FONT_SIZE)
                                .strong(),
                        );
                    },
                );
            });
        });
}

#[cfg(test)]
mod tests;

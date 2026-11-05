//! Плашка рекламы поверх видео (сессия 09d, решения 184, 207).
//!
//! Рисует готовое [`AdPlate`] у правого нижнего края центральной области (над панелью
//! управления, как на сайтах) и возвращает клик как [`NotificationAction::AdPlate`].
//! Видна и когда панели спрятаны в фуллскрине: рисуется в центральной области, а не в
//! панелях. Ничего не решает и не хранит: «Ad 1 of 2 · Skip in 5», кнопки «Skip ›» и
//! «Go to page» и защита от повторного клика (кнопка занята, пока команда в пути) идут
//! от владельца состояния. Все подписи — наши, текста страницы здесь нет (инвариант 1).
//!
//! Читаемость на ярком видео: плашка стоит на тёмной полупрозрачной подложке с белым
//! текстом. Визуальная полировка — на финальной доводке интерфейса.

use super::{NotificationAction, NotificationUiOutput};
use crate::ad_plate::{AdActionProgress, AdPlate, AdPlateAction, AdPodLabel, AdSkipOffer};

/// Подпись кнопки пропуска.
pub(crate) const SKIP_BUTTON_LABEL: &str = "Skip ›";

/// Подпись кнопки «вывести страницу вперёд».
pub(crate) const GO_TO_PAGE_BUTTON_LABEL: &str = "Go to page";

/// Подпись без счётчика, когда сайт номер рекламы не показывает.
pub(crate) const AD_LABEL: &str = "Ad";

/// Разделитель частей плашки.
const PLATE_SEPARATOR: &str = "·";

/// Stable egui id области плашки.
const AD_PLATE_AREA_ID: &str = "fastiplayer_ad_plate";

/// Отступ плашки от правого нижнего угла центральной области.
const PLATE_MARGIN: f32 = 16.0;

/// Высота полосы, которую плашка занимает у нижнего края (стопка toast-ов поднимается
/// над ней): подпись или кнопка, внутренние поля и отступ от края.
const PLATE_RESERVED_HEIGHT: f32 = 56.0;

/// Прозрачность подложки: 0 — прозрачная, 255 — непрозрачная. Достаточно тёмная, чтобы
/// белый текст читался на белом кадре, и достаточно прозрачная, чтобы кадр был виден.
const PLATE_BACKDROP_ALPHA: u8 = 200;

/// Скругление подложки, точки.
const PLATE_CORNER_RADIUS: u8 = 6;

/// Внутренние поля подложки, точки.
const PLATE_PADDING_X: i8 = 10;
const PLATE_PADDING_Y: i8 = 6;

/// «Ad 1 of 2» или просто «Ad».
pub(crate) fn pod_caption(pod: AdPodLabel) -> String {
    match pod {
        AdPodLabel::Unknown => AD_LABEL.to_owned(),
        AdPodLabel::Known { index, count } => format!("{AD_LABEL} {index} of {count}"),
    }
}

/// «Skip in 5».
pub(crate) fn countdown_caption(seconds: u16) -> String {
    format!("Skip in {seconds}")
}

/// Область, которую остаётся стопке toast-ов, когда плашка занимает нижний край.
pub(crate) fn area_above_plate(area_rect: egui::Rect) -> egui::Rect {
    let mut rect = area_rect;
    rect.max.y = (rect.max.y - PLATE_RESERVED_HEIGHT).max(rect.min.y);
    rect
}

/// Рисует плашку у правого нижнего края `area_rect` и кладёт клик в `output`.
pub(crate) fn render_ad_plate(
    ui: &egui::Ui,
    area_rect: egui::Rect,
    plate: &AdPlate,
    output: &mut NotificationUiOutput,
) {
    let anchor = area_rect.right_bottom() - egui::vec2(PLATE_MARGIN, PLATE_MARGIN);
    egui::Area::new(egui::Id::new(AD_PLATE_AREA_ID))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(anchor)
        .constrain_to(area_rect)
        .fade_in(false)
        .show(ui.ctx(), |ui| {
            egui::Frame::NONE
                .fill(egui::Color32::from_black_alpha(PLATE_BACKDROP_ALPHA))
                .corner_radius(PLATE_CORNER_RADIUS)
                .inner_margin(egui::Margin::symmetric(PLATE_PADDING_X, PLATE_PADDING_Y))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        if let Some(action) = render_plate_contents(ui, plate) {
                            output.push(NotificationAction::AdPlate(action));
                        }
                    });
                });
        });
}

/// Подпись, отсчёт или кнопка; возвращает действие, если по кнопке кликнули.
fn render_plate_contents(ui: &mut egui::Ui, plate: &AdPlate) -> Option<AdPlateAction> {
    let text_color = egui::Color32::WHITE;
    ui.label(egui::RichText::new(pod_caption(plate.pod)).color(text_color));
    match plate.offer {
        AdSkipOffer::Nothing => None,
        AdSkipOffer::Countdown { seconds } => {
            ui.label(egui::RichText::new(PLATE_SEPARATOR).color(text_color));
            ui.label(egui::RichText::new(countdown_caption(seconds)).color(text_color));
            None
        }
        AdSkipOffer::Skip => {
            ui.label(egui::RichText::new(PLATE_SEPARATOR).color(text_color));
            action_button(ui, SKIP_BUTTON_LABEL, plate.progress).then_some(AdPlateAction::Skip)
        }
        AdSkipOffer::GoToPage => {
            ui.label(egui::RichText::new(PLATE_SEPARATOR).color(text_color));
            action_button(ui, GO_TO_PAGE_BUTTON_LABEL, plate.progress)
                .then_some(AdPlateAction::GoToPage)
        }
    }
}

/// Кнопка плашки: занятая (команда в пути) не нажимается; `true` — кликнули.
fn action_button(ui: &mut egui::Ui, label: &str, progress: AdActionProgress) -> bool {
    let enabled = progress == AdActionProgress::Idle;
    ui.add_enabled(enabled, egui::Button::new(label)).clicked()
}

#[cfg(test)]
#[path = "ad_plate_tests.rs"]
mod tests;

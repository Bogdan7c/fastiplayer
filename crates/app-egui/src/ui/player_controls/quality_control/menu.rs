//! Содержимое меню качества: строки «что играет» и пункт «Change quality on the page».
//!
//! ```text
//! Resolution   1920 × 1080
//! Bitrate      4.3 Mbps
//! Codec        H.264
//! Frame rate   60 fps          (только если известна)
//! ───────────
//! ( ) 1080p                    (качества страницы, 14a — если адаптер сообщил меню;
//! (•) 720p                      отметка — выбранное на странице; в рекламе неактивны)
//! ───────────
//! Change quality on the page   (виден, когда страницу можно попросить;
//!                               выключен с подсказкой, если экстеншен устарел)
//! ```
//!
//! Меню только показывает снимок [`QualityMenuView`] и публикует типизированное действие.

use egui::{Button, Grid, Ui};

use super::super::page_variant_row::{PageVariantRowState, page_variant_row};
use super::{ControlAction, QualityControlAction};
use crate::page_quality::PageViewOffer;
use crate::page_variants::{PageVariantAvailability, PageVariantMenuView, PageVariantSelection};
use crate::state::QualityMenuView;

/// Минимальная ширина меню: числа не должны прыгать от открытия к открытию.
const MENU_MIN_WIDTH: f32 = 240.0;
/// Пункт просьбы показать страницу.
pub(super) const CHANGE_QUALITY_ITEM: &str = "Change quality on the page";
/// Подсказка выключенного пункта (экстеншен старее протокола 1.6).
pub(super) const EXTENSION_TOO_OLD_HINT: &str =
    "Update the Fastiplayer Bridge extension to use this";

/// Рисует строки меню и добавляет действие, если зритель нажал пункт.
pub(super) fn show(ui: &mut Ui, view: &QualityMenuView, actions: &mut Vec<ControlAction>) {
    ui.set_min_width(MENU_MIN_WIDTH);
    let Some(text) = view.text() else {
        return;
    };
    Grid::new("quality_menu_rows")
        .num_columns(2)
        .spacing([16.0, 4.0])
        .show(ui, |ui| {
            row(ui, "Resolution", &text.resolution);
            row(ui, "Bitrate", &text.bitrate);
            row(ui, "Codec", &text.codec);
            if let Some(frame_rate) = &text.frame_rate {
                row(ui, "Frame rate", frame_rate);
            }
        });
    if let Some(choices) = view.page_qualities() {
        ui.separator();
        page_quality_rows(ui, choices.menu(), choices.availability(), actions);
    }
    match view.page_view_offer() {
        PageViewOffer::Available => {
            ui.separator();
            if ui.add(Button::new(CHANGE_QUALITY_ITEM)).clicked() {
                actions.push(ControlAction::Quality(QualityControlAction::ShowPage));
                ui.close();
            }
        }
        PageViewOffer::ExtensionTooOld => {
            ui.separator();
            ui.add_enabled(false, Button::new(CHANGE_QUALITY_ITEM))
                .on_disabled_hover_text(EXTENSION_TOO_OLD_HINT);
        }
        // Страницы нет или она ещё не подключилась: просить некого, пункта нет.
        PageViewOffer::PageAway | PageViewOffer::NotAttached => {}
    }
}

/// Качества плеера страницы радио-списком (общая строка: обрезка подписи, блокировка в
/// рекламе); выбор публикует [`QualityControlAction::SelectPageQuality`] и закрывает меню.
fn page_quality_rows(
    ui: &mut Ui,
    menu: &PageVariantMenuView,
    availability: PageVariantAvailability,
    actions: &mut Vec<ControlAction>,
) {
    for option in menu.options() {
        let state = PageVariantRowState {
            is_selected: option.selection == PageVariantSelection::Selected,
            availability,
        };
        if !page_variant_row(ui, option.label, state).clicked() {
            continue;
        }
        if let Some(choice) = menu.choice(option.index) {
            actions.push(ControlAction::Quality(
                QualityControlAction::SelectPageQuality(choice),
            ));
            ui.close();
        }
    }
}

/// Строка «название — значение».
fn row(ui: &mut Ui, name: &str, value: &str) {
    ui.label(name);
    ui.label(value);
    ui.end_row();
}

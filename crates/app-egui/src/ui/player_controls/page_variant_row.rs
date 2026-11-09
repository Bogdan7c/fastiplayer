//! Строка пункта меню страницы (озвучка в «♪», качество в кнопке качества), сессия 14a.
//!
//! Общая для обоих меню, чтобы пункты страницы выглядели и вели себя одинаково:
//! - радио-кнопка с отметкой пункта, выбранного на странице;
//! - подпись — текст страницы: длинная обрезается многоточием по ширине строки, а не
//!   растягивает меню (полная подпись остаётся в дереве доступности);
//! - во время рекламы выбор заблокирован, как перемотка (решения 206, 283): пункт виден, но
//!   неактивен, а наведение объясняет почему. Тоста нет — его нет и у отказа перемотки.

use egui::{RadioButton, Response, TextWrapMode, Ui};

use crate::page_variants::PageVariantAvailability;

/// Подсказка неактивного пункта во время рекламы.
pub(super) const UNAVAILABLE_DURING_AD_HINT: &str = "Unavailable during an ad";

/// Предельная ширина строки: длинная подпись страницы дальше обрезается.
pub(super) const PAGE_VARIANT_ROW_MAX_WIDTH: f32 = 320.0;

/// Как показать пункт: отмечен ли он и можно ли его нажать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PageVariantRowState {
    /// Пункт выбран на странице (отметка «играет»).
    pub(super) is_selected: bool,
    /// Можно ли нажать (во время рекламы — нельзя).
    pub(super) availability: PageVariantAvailability,
}

/// Рисует пункт; `clicked()` ответа — зритель выбрал его (неактивный пункт не нажимается).
pub(super) fn page_variant_row(ui: &mut Ui, label: &str, state: PageVariantRowState) -> Response {
    ui.scope(|ui| {
        // Обрезка вместо переноса и растяжения: подпись приходит со страницы и может быть
        // любой длины (протокол пускает до 256 байт).
        ui.set_max_width(PAGE_VARIANT_ROW_MAX_WIDTH);
        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
        ui.add_enabled(
            state.availability.is_selectable(),
            RadioButton::new(state.is_selected, label),
        )
        .on_disabled_hover_text(UNAVAILABLE_DURING_AD_HINT)
    })
    .inner
}

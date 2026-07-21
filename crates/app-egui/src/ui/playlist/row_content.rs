//! Общий presentation-owner содержимого обычных и compound playlist rows.

use egui::{Align, Layout, Sense, TextWrapMode};
use playlist_core::PlaylistMediaKind;
use ui_artwork_egui::{ArtworkPainter, MediaKindGlyph};

use crate::playlist_runtime::PlaylistVisibleRow;
use crate::ui::skin::PlaylistRowStyle;

pub(super) const ROW_HEIGHT: f32 = 34.0;
pub(super) const TOOLTIP_MAX_WIDTH: f32 = 320.0;
pub(super) const INDEX_WIDTH: f32 = 38.0;
pub(super) const MEDIA_KIND_WIDTH: f32 = 16.0;
pub(super) const DURATION_WIDTH: f32 = 50.0;
/// Ширина области служебных индикаторов без удалённого декоративного Play-глифа.
pub(super) const BADGES_WIDTH: f32 = 45.0;

/// Обычная строка переиспользует те же фиксированные колонки, что и compound presentation.
pub(super) fn render_row_content(
    ui: &mut egui::Ui,
    row_index: usize,
    row: &PlaylistVisibleRow,
    row_style: PlaylistRowStyle,
) {
    ui.add_sized(
        [INDEX_WIDTH, ROW_HEIGHT],
        egui::Label::new(egui::RichText::new(format!("{}.", row_index + 1)).weak())
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    render_media_kind_icon(ui, row.media_kind());

    let trailing_width = DURATION_WIDTH + BADGES_WIDTH;
    let spacing_width = ui.spacing().item_spacing.x * 2.0;
    let title_width = (ui.available_width() - trailing_width - spacing_width).max(24.0);
    // Обычные строки используют стандартный foreground без прежнего forced strong color.
    let mut title_text = egui::RichText::new(row.display_title());
    // Только подтверждённая active identity получает skin-owned контрастный цвет.
    if row.is_active() {
        title_text = title_text.color(row_style.active_title_color);
    }
    ui.add_sized(
        [title_width, ROW_HEIGHT],
        egui::Label::new(title_text)
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    ui.add_sized(
        [DURATION_WIDTH, ROW_HEIGHT],
        egui::Label::new(format_duration(row.duration()))
            .selectable(false)
            .wrap_mode(TextWrapMode::Truncate),
    );
    render_badges(ui, row);
}

/// Резервирует компактную неинтерактивную ячейку и передаёт рисование artwork-crate.
pub(super) fn render_media_kind_icon(ui: &mut egui::Ui, media_kind: PlaylistMediaKind) {
    // `Sense::hover` не создаёт click/drag-владельца и сохраняет взаимодействие с целой строкой.
    let (response, painter) =
        ui.allocate_painter(egui::vec2(MEDIA_KIND_WIDTH, ROW_HEIGHT), Sense::hover());
    // Цвет и толщина берутся из текущей темы, поэтому glyph наследует состояние интерфейса.
    let stroke = ui.visuals().widgets.noninteractive.fg_stroke;
    // App-level выполняет только типизированное отображение domain-вида в визуальный glyph.
    ArtworkPainter::new(&painter).media_kind_icon(
        response.rect,
        media_kind_glyph(media_kind),
        stroke,
    );
}

/// Переводит playlist-domain тип в нейтральный artwork-контракт.
pub(super) const fn media_kind_glyph(media_kind: PlaylistMediaKind) -> MediaKindGlyph {
    // Полный match не позволит молча забыть новый domain-вариант.
    match media_kind {
        PlaylistMediaKind::Unknown => MediaKindGlyph::Unknown,
        PlaylistMediaKind::Audio => MediaKindGlyph::Audio,
        PlaylistMediaKind::Video => MediaKindGlyph::Video,
    }
}

/// Pending/error indicators занимают стабильную ширину и не вызывают row flicker.
pub(super) fn render_badges(ui: &mut egui::Ui, row: &PlaylistVisibleRow) {
    ui.allocate_ui_with_layout(
        egui::vec2(BADGES_WIDTH, ROW_HEIGHT),
        Layout::left_to_right(Align::Center),
        |ui| {
            if row.is_pending() {
                ui.add_sized([14.0, 14.0], egui::Spinner::new());
            } else {
                ui.add_space(14.0);
            }
            if row.runtime_error().is_some() {
                ui.add(
                    egui::Label::new(egui::RichText::new("!").color(ui.visuals().error_fg_color))
                        .selectable(false),
                );
            } else {
                ui.add_space(8.0);
            }
        },
    );
}

/// Русское accessible-имя типа не зависит от конкретного glyph.
pub(super) const fn media_kind_text(media_kind: PlaylistMediaKind) -> &'static str {
    match media_kind {
        PlaylistMediaKind::Unknown => "Медиа",
        PlaylistMediaKind::Audio => "Аудио",
        PlaylistMediaKind::Video => "Видео",
    }
}

/// Длительность форматируется одинаково во всех вариантах playlist row.
pub(super) fn format_duration(duration: Option<media_core::MediaDuration>) -> String {
    let Some(duration) = duration else {
        return "—".to_owned();
    };
    let total_seconds = duration.as_duration().as_secs();
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// Обычная строка сообщает active/selection/pending/error без зависимости от paint geometry.
pub(super) fn accessibility_text(row_index: usize, row: &PlaylistVisibleRow) -> String {
    let mut text = format!(
        "Элемент {}. {}. Тип: {}. Длительность: {}.",
        row_index + 1,
        row.display_title(),
        media_kind_text(row.media_kind()),
        format_duration(row.duration())
    );
    if row.display_title() != row.fallback_display_name() {
        text.push_str(" Имя файла: ");
        text.push_str(row.fallback_display_name());
        text.push('.');
    }
    if row.is_active() {
        text.push_str(" Сейчас играет.");
    }
    if row.is_selected() {
        text.push_str(" Выбрано.");
    }
    match (row.runtime_error(), row.is_pending()) {
        (Some(error), true) => {
            text.push_str(
                " Предыдущая попытка завершилась ошибкой; выполняется повторная попытка. Ошибка: ",
            );
            text.push_str(error.safe_summary());
            text.push('.');
        }
        (Some(error), false) => {
            text.push_str(" Ошибка: ");
            text.push_str(error.safe_summary());
            text.push('.');
        }
        (None, true) => text.push_str(" Выполняется открытие."),
        (None, false) => {}
    }
    text
}

/// Tooltip показывает только safe presentation metadata и bounded error summary.
pub(super) fn show_safe_tooltip(ui: &mut egui::Ui, row: &PlaylistVisibleRow) {
    ui.add(egui::Label::new(row.display_title()).wrap_mode(TextWrapMode::Wrap));
    if row.display_title() != row.fallback_display_name() {
        ui.add(
            egui::Label::new(egui::RichText::new(row.fallback_display_name()).weak())
                .wrap_mode(TextWrapMode::Wrap),
        );
    }
    if let Some(error) = row.runtime_error() {
        ui.add(
            egui::Label::new(
                egui::RichText::new(error.safe_summary()).color(ui.visuals().error_fg_color),
            )
            .wrap_mode(TextWrapMode::Wrap),
        );
    }
}

/// Ограничивает tooltip шириной строки и не выпускает длинное имя поверх всего видео.
pub(super) fn tooltip_width(row_width: f32) -> f32 {
    row_width.clamp(1.0, TOOLTIP_MAX_WIDTH)
}

#[cfg(test)]
pub(super) fn stable_row_id(
    parent_id: egui::Id,
    entry_id: playlist_core::PlaylistEntryId,
) -> egui::Id {
    parent_id.with(("playlist_row", entry_id))
}

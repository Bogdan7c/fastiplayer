//! Компактный переключатель секций в левой части custom titlebar.

use egui::{Color32, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};
use ui_artwork_egui::{ArtworkPainter, ButtonVisualState, SidebarButtonGlyph};

use crate::state::SidebarSection;

const BUTTON_WIDTH: f32 = 36.0;
const BUTTON_HEIGHT: f32 = 32.0;
const ACTIVE_FILL_ALPHA: u8 = 20;
const ACTIVE_HOVER_FILL_ALPHA: u8 = 42;

const BUTTONS: [(SidebarSection, &str); 4] = [
    (SidebarSection::Playlist, "Плейлист"),
    (SidebarSection::Settings, "Настройки"),
    (SidebarSection::Url, "URL"),
    (SidebarSection::Info, "Информация"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TitlebarIconAreaAction {
    SelectSidebarSection(SidebarSection),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TitlebarIconAreaStyle {
    pub(crate) icon_stroke: Stroke,
    pub(crate) button_hover_fill: Color32,
}

/// Горизонтальная сетка центров левой titlebar-группы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TitlebarIconAreaAlignment {
    /// Абсолютная X-координата центра первой кнопки.
    first_button_center_x: f32,

    /// Расстояние между центрами соседних кнопок.
    button_center_step: f32,
}

impl TitlebarIconAreaAlignment {
    /// Создаёт explicit alignment без скрытого знания о соседних UI-модулях.
    #[must_use]
    pub(crate) fn new(first_button_center_x: f32, button_center_step: f32) -> Self {
        Self {
            first_button_center_x,
            button_center_step,
        }
    }

    /// Возвращает центр кнопки конкретной sidebar-секции.
    #[must_use]
    fn button_center_x(self, section: SidebarSection) -> f32 {
        self.first_button_center_x + self.button_center_step * section_axis_index(section) as f32
    }
}

/// Типизированно связывает sidebar intent со стабильной window-chrome осью.
const fn section_axis_index(section: SidebarSection) -> usize {
    match section {
        SidebarSection::Playlist => 0,
        SidebarSection::Settings => 1,
        SidebarSection::Url => 2,
        SidebarSection::Info => 3,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TitlebarIconAreaOutput {
    pub(crate) reserved_rect: Rect,
    pub(crate) actions: Vec<TitlebarIconAreaAction>,
}

#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    titlebar_rect: Rect,
    alignment: TitlebarIconAreaAlignment,
    style: TitlebarIconAreaStyle,
    active_section: Option<SidebarSection>,
) -> TitlebarIconAreaOutput {
    let mut actions = Vec::new();
    for (section, tooltip) in BUTTONS {
        let button_rect = button_rect(titlebar_rect, alignment, section);
        let response = ui
            .interact(
                button_rect,
                ui.id().with(("sidebar_section", section as u8)),
                Sense::click(),
            )
            .on_hover_text(tooltip);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), tooltip));
        paint_button(
            ui,
            button_rect,
            section,
            active_section == Some(section),
            response.hovered(),
            style,
        );
        if response.clicked() {
            actions.push(TitlebarIconAreaAction::SelectSidebarSection(section));
        }
    }

    TitlebarIconAreaOutput {
        reserved_rect: reserved_rect(titlebar_rect, alignment),
        actions,
    }
}

/// Вычисляет кнопку относительно общей оси первой titlebar-иконки.
///
/// Все последующие кнопки используют skin-owned center step, поэтому titlebar
/// и playlist toolbar могут разделять одну горизонтальную сетку.
#[must_use]
pub(crate) fn button_rect(
    titlebar_rect: Rect,
    alignment: TitlebarIconAreaAlignment,
    section: SidebarSection,
) -> Rect {
    let top = titlebar_rect.center().y - BUTTON_HEIGHT * 0.5;
    let left = alignment.button_center_x(section) - BUTTON_WIDTH * 0.5;
    Rect::from_min_size(pos2(left, top), vec2(BUTTON_WIDTH, BUTTON_HEIGHT))
}

/// Возвращает общий hit-rect всей левой группы, не захватывая край окна.
#[must_use]
pub(crate) fn button_group_rect(titlebar_rect: Rect, alignment: TitlebarIconAreaAlignment) -> Rect {
    let first_button_rect = button_rect(titlebar_rect, alignment, SidebarSection::Playlist);
    let last_button_rect = button_rect(titlebar_rect, alignment, SidebarSection::Info);

    Rect::from_min_max(first_button_rect.min, last_button_rect.max)
}

#[must_use]
pub(crate) fn reserved_rect(titlebar_rect: Rect, alignment: TitlebarIconAreaAlignment) -> Rect {
    let button_group_rect = button_group_rect(titlebar_rect, alignment);

    Rect::from_min_max(
        titlebar_rect.min,
        pos2(button_group_rect.right(), titlebar_rect.bottom()),
    )
}

fn paint_button(
    ui: &Ui,
    rect: Rect,
    section: SidebarSection,
    active: bool,
    hovered: bool,
    style: TitlebarIconAreaStyle,
) {
    let active_fill = Color32::from_rgba_unmultiplied(
        255,
        255,
        255,
        if hovered {
            ACTIVE_HOVER_FILL_ALPHA
        } else {
            ACTIVE_FILL_ALPHA
        },
    );
    let painter = ArtworkPainter::new(ui.painter());
    if active {
        painter.sidebar_button_active_background(rect, active_fill);
    }
    let visual_state = if hovered {
        ButtonVisualState::Hovered
    } else {
        ButtonVisualState::Idle
    };
    match section {
        SidebarSection::Settings => painter.settings_button(
            rect,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
        SidebarSection::Playlist => painter.sidebar_button(
            rect,
            SidebarButtonGlyph::Playlist,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
        SidebarSection::Url => painter.sidebar_button(
            rect,
            SidebarButtonGlyph::Url,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
        SidebarSection::Info => painter.sidebar_button(
            rect,
            SidebarButtonGlyph::Info,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_have_required_order_size_and_spacing() {
        let titlebar = Rect::from_min_size(pos2(10.0, 5.0), vec2(900.0, 64.0));
        let first_button_center_x = titlebar.left() + 39.0;
        let alignment = TitlebarIconAreaAlignment::new(first_button_center_x, 40.0);
        let rects = BUTTONS.map(|(section, _)| button_rect(titlebar, alignment, section));
        assert_eq!(rects[0].center().x, first_button_center_x);
        assert!(rects.iter().all(|rect| rect.size() == vec2(36.0, 32.0)));
        assert!(
            rects
                .windows(2)
                .all(|pair| pair[1].left() - pair[0].right() == 4.0)
        );
        assert_eq!(rects[0].center().y, titlebar.center().y);
    }

    #[test]
    fn reserved_area_covers_inset_gaps_and_every_button() {
        let titlebar = Rect::from_min_size(pos2(10.0, 5.0), vec2(900.0, 32.0));
        let first_button_center_x = titlebar.left() + 39.0;
        let alignment = TitlebarIconAreaAlignment::new(first_button_center_x, 40.0);
        let reserved = reserved_rect(titlebar, alignment);
        let button_group = button_group_rect(titlebar, alignment);
        assert_eq!(reserved.left(), titlebar.left());
        assert_eq!(reserved.right(), button_group.right());
        assert_eq!(
            button_group.left(),
            button_rect(titlebar, alignment, SidebarSection::Playlist).left()
        );
        assert_eq!(
            button_group.right(),
            button_rect(titlebar, alignment, SidebarSection::Info).right()
        );
    }
}

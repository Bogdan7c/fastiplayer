//! Компактный переключатель секций в левой части custom titlebar.

use egui::{Color32, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};
use ui_artwork_egui::{ArtworkPainter, ButtonVisualState, SidebarButtonGlyph};

use crate::state::SidebarSection;

const LEFT_INSET: f32 = 8.0;
const BUTTON_WIDTH: f32 = 36.0;
const BUTTON_HEIGHT: f32 = 32.0;
const BUTTON_GAP: f32 = 4.0;
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

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TitlebarIconAreaOutput {
    pub(crate) reserved_rect: Rect,
    pub(crate) actions: Vec<TitlebarIconAreaAction>,
}

#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    titlebar_rect: Rect,
    style: TitlebarIconAreaStyle,
    active_section: Option<SidebarSection>,
) -> TitlebarIconAreaOutput {
    let mut actions = Vec::new();
    for (index, (section, tooltip)) in BUTTONS.into_iter().enumerate() {
        let button_rect = button_rect(titlebar_rect, index);
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
        reserved_rect: reserved_rect(titlebar_rect),
        actions,
    }
}

#[must_use]
pub(crate) fn button_rect(titlebar_rect: Rect, index: usize) -> Rect {
    let top = titlebar_rect.center().y - BUTTON_HEIGHT * 0.5;
    let left = titlebar_rect.left() + LEFT_INSET + index as f32 * (BUTTON_WIDTH + BUTTON_GAP);
    Rect::from_min_size(pos2(left, top), vec2(BUTTON_WIDTH, BUTTON_HEIGHT))
}

/// Совместимый geometry helper для window-chrome layout tests.
#[must_use]
pub(crate) fn settings_button_rect(titlebar_rect: Rect) -> Rect {
    button_rect(titlebar_rect, 1)
}

#[must_use]
pub(crate) fn reserved_rect(titlebar_rect: Rect) -> Rect {
    Rect::from_min_max(
        titlebar_rect.min,
        pos2(
            button_rect(titlebar_rect, BUTTONS.len() - 1).right(),
            titlebar_rect.bottom(),
        ),
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
        let rects: Vec<_> = (0..4).map(|index| button_rect(titlebar, index)).collect();
        assert_eq!(rects[0].left(), titlebar.left() + 8.0);
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
        let reserved = reserved_rect(titlebar);
        assert_eq!(reserved.left(), titlebar.left());
        assert_eq!(reserved.right(), button_rect(titlebar, 3).right());
    }
}

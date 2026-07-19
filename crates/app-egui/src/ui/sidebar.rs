//! Единый app-level host левого sidebar.
//!
//! Этот модуль создаёт ровно один egui Panel. Playlist, Settings, URL и Info
//! являются только сменяемым содержимым и не владеют шириной или resize-state.

use egui::Ui;
use rustiplayer_config::{MAX_SIDEBAR_WIDTH_POINTS, MIN_SIDEBAR_WIDTH_POINTS};

use crate::settings_ui::{SettingsUiAction, SettingsUiModel, layout};
use crate::state::{ContentSlideDirection, SidebarContentTransition, SidebarSection};
use crate::ui::skin::{PlaylistHeaderUndoStyle, PlaylistRowStyle, PlaylistToolbarStyle};
use crate::ui::window_chrome::WindowChromeEdgeAlignment;
use crate::ui::{media_info, playlist};

const SIDEBAR_FILL: egui::Color32 = egui::Color32::from_rgb(18, 18, 18);

mod header;

/// Типизированная округлённая ширина на settings/persistence boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SidebarWidthPoints(u16);

impl SidebarWidthPoints {
    /// Нормализует значение из committed config или geometry restore boundary.
    #[must_use]
    pub(crate) fn from_committed(width_points: u16) -> Self {
        Self(width_points.clamp(MIN_SIDEBAR_WIDTH_POINTS, MAX_SIDEBAR_WIDTH_POINTS))
    }

    /// Возвращает serializable значение config.
    #[must_use]
    pub(crate) fn value(self) -> u16 {
        self.0
    }

    /// Округляет live egui geometry только на persistence boundary.
    #[must_use]
    fn from_live_width(width_points: f32) -> Self {
        let rounded = width_points.round();
        Self::from_committed(rounded as u16)
    }
}

/// Единственный владелец fully-open ширины общей панели.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SidebarHostState {
    open_width_points: f32,
    last_reported_width: SidebarWidthPoints,
}

impl SidebarHostState {
    /// Восстанавливает host из validated committed config.
    #[must_use]
    pub(crate) fn from_committed(width_points: u16) -> Self {
        let normalized = SidebarWidthPoints::from_committed(width_points);
        Self {
            open_width_points: f32::from(normalized.value()),
            last_reported_width: normalized,
        }
    }

    /// Возвращает текущую live ширину, которой должен подчиняться layout.
    #[must_use]
    pub(crate) fn open_width_points(&self) -> f32 {
        self.open_width_points
    }

    /// Синхронизирует успешный committed Apply либо явный rollback persistence failure.
    pub(crate) fn restore_committed_width(&mut self, width_points: SidebarWidthPoints) {
        self.open_width_points = f32::from(width_points.value());
        self.last_reported_width = width_points;
    }

    /// Принимает только fully-open geometry; animation width сюда никогда не попадает.
    fn accept_fully_open_width(&mut self, width_points: f32) -> Option<SidebarWidthChange> {
        self.open_width_points = width_points.clamp(
            f32::from(MIN_SIDEBAR_WIDTH_POINTS),
            f32::from(MAX_SIDEBAR_WIDTH_POINTS),
        );
        let rounded_width = SidebarWidthPoints::from_live_width(self.open_width_points);
        if rounded_width == self.last_reported_width {
            return None;
        }

        self.last_reported_width = rounded_width;
        Some(SidebarWidthChange {
            width_points: rounded_width,
        })
    }
}

/// Typed событие только реального fully-open drag-resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SidebarWidthChange {
    pub(crate) width_points: SidebarWidthPoints,
}

/// Результат единственного sidebar host-а за кадр.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SidebarOutput {
    pub(crate) rect: egui::Rect,
    pub(crate) open_width_points: f32,
    pub(crate) width_change: Option<SidebarWidthChange>,
}

/// Все borrowed данные сменяемого содержимого sidebar.
pub(crate) struct SidebarRenderContext<'a> {
    pub(crate) model: &'a SettingsUiModel,
    pub(crate) snapshot: &'a player_core::PlayerSnapshot,
    pub(crate) playlist_model: Option<&'a crate::playlist_runtime::PlaylistViewModel>,
    pub(crate) playlist_row_style: PlaylistRowStyle,
    pub(crate) playlist_toolbar_style: PlaylistToolbarStyle,
    pub(crate) playlist_header_undo_style: PlaylistHeaderUndoStyle,
    pub(crate) playlist_interaction: &'a crate::playlist_runtime::PlaylistInteractionModel,
    pub(crate) playlist_undo: &'a crate::playlist_runtime::PlaylistUndoUiSnapshot,
    pub(crate) ui_motion: crate::ui::animation::UiMotion,
    pub(crate) window_chrome_edge_alignment: WindowChromeEdgeAlignment,
    pub(crate) playlist_state: &'a mut playlist::PlaylistUiState,
    pub(crate) playlist_output: &'a mut playlist::PlaylistUiOutput,
    pub(crate) settings_actions: &'a mut Vec<SettingsUiAction>,
    pub(crate) close_requested: &'a mut bool,
}

/// Единый ID: изменение секции не создаёт новый persisted width-state.
fn sidebar_panel_id() -> egui::Id {
    egui::Id::new("app_sidebar")
}

/// Рисует единственный sidebar и возвращает rect, который вытесняет видео.
#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    host_state: &mut SidebarHostState,
    displayed_section: Option<SidebarSection>,
    slide_progress: f32,
    content_transition: Option<SidebarContentTransition>,
    mut context: SidebarRenderContext<'_>,
) -> Option<SidebarOutput> {
    let displayed_section = displayed_section?;
    if slide_progress <= 0.0 {
        return None;
    }

    Some(show_sidebar_host(
        ui,
        host_state,
        slide_progress,
        |ui, panel_rect, target_width, fully_open| match content_transition {
            Some(transition) => {
                render_content_transition(ui, panel_rect, transition, &mut context);
            }
            None => render_open_content(
                ui,
                panel_rect,
                target_width,
                displayed_section,
                fully_open,
                &mut context,
            ),
        },
    ))
}

/// Создаёт geometry host. Сменяемое содержимое получает только готовый rect и не владеет Panel.
fn show_sidebar_host(
    ui: &mut Ui,
    host_state: &mut SidebarHostState,
    slide_progress: f32,
    add_contents: impl FnOnce(&mut Ui, egui::Rect, f32, bool),
) -> SidebarOutput {
    let target_width = host_state.open_width_points();
    let fully_open = slide_progress >= 1.0;
    let visible_width = (target_width * slide_progress).max(1.0);

    // `egui::PanelState` хранит прошлый rect отдельно от app state. Удаляем его
    // перед каждым показом, чтобы default_size ниже всегда начинался от нашего owner-а.
    ui.ctx().data_mut(|data| {
        data.remove::<egui::containers::panel::PanelState>(sidebar_panel_id());
    });
    let panel = egui::Panel::left(sidebar_panel_id())
        .resizable(fully_open)
        .default_size(target_width)
        .size_range(if fully_open {
            f32::from(MIN_SIDEBAR_WIDTH_POINTS)..=f32::from(MAX_SIDEBAR_WIDTH_POINTS)
        } else {
            visible_width..=visible_width
        })
        .frame(egui::Frame::NONE.fill(SIDEBAR_FILL));

    let mut sidebar_rect = egui::Rect::NOTHING;
    let _response = panel.show_inside(ui, |ui| {
        // Содержимое не имеет права задавать minimum width владельцу Panel.
        ui.set_min_width(0.0);
        let panel_rect = ui.max_rect();
        // Только rect самого host является resize geometry. `response.rect` ниже
        // может отражать translated animation children и потому зависит от контента.
        sidebar_rect = panel_rect;
        add_contents(ui, panel_rect, target_width, fully_open);
        // Анимационные child UI могут целиком оказаться за clip-границей и не
        // занять место в parent UI. Явно сохраняем panel rect, чтобы egui не
        // сдвинул cursor соседнего layout до content-dependent minimum width.
        ui.expand_to_include_rect(panel_rect);
    });

    debug_assert!(sidebar_rect.is_positive());
    let width_change = fully_open
        .then(|| host_state.accept_fully_open_width(sidebar_rect.width()))
        .flatten();
    SidebarOutput {
        rect: sidebar_rect,
        open_width_points: host_state.open_width_points(),
        width_change,
    }
}

/// В fully-open состоянии содержимое рисуется прямо в Panel UI, чтобы его
/// текущий max rect не превращался в minimum width и не блокировал resize.
/// Fixed child используется только во время open/close animation.
fn render_open_content(
    ui: &mut Ui,
    panel_rect: egui::Rect,
    target_width: f32,
    section: SidebarSection,
    fully_open: bool,
    context: &mut SidebarRenderContext<'_>,
) {
    let Some(content_rect) = animated_content_rect(panel_rect, target_width, fully_open) else {
        render_section(ui, section, context);
        return;
    };
    let mut child = content_child(ui, panel_rect, content_rect, ("open", section as u8));
    child.disable();
    render_section(&mut child, section, context);
}

/// `None` означает direct rendering в resizable host. Fixed rect разрешён
/// только для промежуточной open/close animation.
fn animated_content_rect(
    panel_rect: egui::Rect,
    target_width: f32,
    fully_open: bool,
) -> Option<egui::Rect> {
    (!fully_open).then(|| {
        egui::Rect::from_min_max(
            egui::pos2(panel_rect.right() - target_width, panel_rect.top()),
            panel_rect.max,
        )
    })
}

/// Рисует outgoing/incoming content внутри того же Panel.
fn render_content_transition(
    ui: &mut Ui,
    panel_rect: egui::Rect,
    transition: SidebarContentTransition,
    context: &mut SidebarRenderContext<'_>,
) {
    let width = panel_rect.width();
    let progress = animation_core::Easing::EaseInOutCubic.apply(transition.progress);
    let travel = width * progress;
    let (outgoing_offset, incoming_offset) = match transition.direction {
        ContentSlideDirection::FromRight => (-travel, width - travel),
        ContentSlideDirection::FromLeft => (travel, -width + travel),
    };

    for (role, section, offset) in [
        ("outgoing", transition.from, outgoing_offset),
        ("incoming", transition.to, incoming_offset),
    ] {
        let content_rect = panel_rect.translate(egui::vec2(offset, 0.0));
        let mut child = content_child(ui, panel_rect, content_rect, (role, section as u8));
        // Переходящие visual-копии не принимают input.
        child.disable();
        render_section(&mut child, section, context);
    }
}

fn content_child(
    ui: &mut Ui,
    panel_rect: egui::Rect,
    content_rect: egui::Rect,
    id_salt: impl std::hash::Hash,
) -> Ui {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id_salt)
            .max_rect(content_rect),
    );
    child.set_clip_rect(panel_rect.intersect(ui.clip_rect()));
    child.set_min_width(0.0);
    child
}

/// Единственная точка выбора содержимого; Panel state здесь отсутствует.
fn render_section(ui: &mut Ui, section: SidebarSection, context: &mut SidebarRenderContext<'_>) {
    ui.push_id(("section", section as u8), |ui| {
        // Header и separator принадлежат host и одинаковы для всех секций.
        header::show(ui, section, context);
        ui.separator();

        match section {
            SidebarSection::Playlist => {
                playlist::show(
                    ui,
                    playlist::PlaylistShowInput {
                        model: context.playlist_model,
                        interaction: context.playlist_interaction,
                        row_style: context.playlist_row_style,
                        toolbar_style: context.playlist_toolbar_style,
                        motion: context.ui_motion,
                    },
                    context.playlist_state,
                    context.playlist_output,
                );
            }
            SidebarSection::Settings => {
                layout::show(ui, context.model, context.settings_actions);
            }
            SidebarSection::Url => {}
            SidebarSection::Info => {
                egui::ScrollArea::vertical()
                    .id_salt("info_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width(0.0);
                        media_info::show(ui, context.snapshot);
                    });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, RawInput};
    use std::path::Path;

    #[test]
    fn sidebar_source_has_exactly_one_panel_creation_site() {
        let source = sidebar_source();
        let panel_constructor = format!("{}{}", "Panel::", "left(");
        let shared_constructor = format!("{}{}", "Panel::left", "(sidebar_panel_id())");
        assert_eq!(source.matches(&panel_constructor).count(), 1);
        assert!(source.contains(&shared_constructor));
    }

    #[test]
    fn all_sections_share_one_width_policy() {
        assert_eq!(rustiplayer_config::DEFAULT_SIDEBAR_WIDTH_POINTS, 420);
        assert_eq!(MIN_SIDEBAR_WIDTH_POINTS, 350);
        assert_eq!(MAX_SIDEBAR_WIDTH_POINTS, 600);
    }

    /// Реальный headless-egui drag должен сразу менять app-owned ширину и выдавать typed event.
    #[test]
    fn fully_open_headless_panel_drag_updates_live_host_width() {
        let egui_ctx = egui::Context::default();
        let mut host =
            SidebarHostState::from_committed(rustiplayer_config::DEFAULT_SIDEBAR_WIDTH_POINTS);
        let resize_handle = egui::pos2(420.0, 200.0);

        let warmup = render_host(
            &egui_ctx,
            &mut host,
            vec![Event::PointerMoved(resize_handle)],
            1.0,
        );
        assert_eq!(warmup.rect.width(), 420.0);
        assert_eq!(warmup.width_change, None);

        let pressed = render_host(
            &egui_ctx,
            &mut host,
            vec![
                Event::PointerMoved(resize_handle),
                pointer_button(resize_handle, true),
            ],
            1.0,
        );
        assert_eq!(pressed.width_change, None);

        let dragged = render_host(
            &egui_ctx,
            &mut host,
            vec![Event::PointerMoved(egui::pos2(500.0, 200.0))],
            1.0,
        );
        assert_eq!(dragged.rect.width(), 500.0);
        assert_eq!(dragged.open_width_points, 500.0);
        assert_eq!(
            dragged.width_change,
            Some(SidebarWidthChange {
                width_points: SidebarWidthPoints::from_committed(500),
            })
        );

        let reopened = render_host(
            &egui_ctx,
            &mut host,
            vec![pointer_button(egui::pos2(500.0, 200.0), false)],
            1.0,
        );
        assert_eq!(reopened.rect.width(), 500.0);
        assert_eq!(reopened.width_change, None);
    }

    /// Open/close animation использует live target, но не записывает сжатую промежуточную ширину.
    #[test]
    fn animation_width_never_replaces_fully_open_host_width() {
        let egui_ctx = egui::Context::default();
        let mut host = SidebarHostState::from_committed(420);
        let initial = render_host(&egui_ctx, &mut host, Vec::new(), 1.0);
        assert_eq!(initial.rect.width(), 420.0);

        host.restore_committed_width(SidebarWidthPoints::from_committed(500));
        let half_closed = render_host(&egui_ctx, &mut host, Vec::new(), 0.5);
        assert_eq!(half_closed.rect.width(), 250.0);
        assert_eq!(half_closed.open_width_points, 500.0);
        assert_eq!(half_closed.width_change, None);

        let reopened = render_host(&egui_ctx, &mut host, Vec::new(), 1.0);
        assert_eq!(reopened.rect.width(), 500.0);
        assert_eq!(reopened.open_width_points, 500.0);
        assert_eq!(reopened.width_change, None);
    }

    /// Выезжающая копия другой секции не должна становиться геометрией resize-host.
    #[test]
    fn content_transition_does_not_replace_resized_host_width() {
        let egui_ctx = egui::Context::default();
        let mut host = SidebarHostState::from_committed(500);

        let (transition_frame, remaining_rect) =
            render_host_with_transition_copy(&egui_ctx, &mut host);

        assert_eq!(transition_frame.rect.width(), 500.0);
        assert_eq!(transition_frame.open_width_points, 500.0);
        assert_eq!(transition_frame.width_change, None);
        assert_eq!(remaining_rect.left(), 500.0);

        let completed_transition = render_host(&egui_ctx, &mut host, Vec::new(), 1.0);
        assert_eq!(completed_transition.rect.width(), 500.0);
        assert_eq!(completed_transition.open_width_points, 500.0);
        assert_eq!(completed_transition.width_change, None);
    }

    #[test]
    fn fully_open_content_does_not_create_width_locking_child_rect() {
        let panel_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(420.0, 700.0));
        assert_eq!(animated_content_rect(panel_rect, 420.0, true), None);
        assert!(animated_content_rect(panel_rect, 420.0, false).is_some());
    }

    #[test]
    fn source_guardrail_sidebar_stays_visual_only() {
        let normalized_source = sidebar_source().to_lowercase();
        let forbidden_patterns = [
            concat!("settings_", "runtime").to_string(),
            format!("{}{}", "app", "config"),
            concat!("render_", "wg", "pu").to_string(),
            concat!("render-", "wg", "pu").to_string(),
            concat!("wg", "pu").to_string(),
        ];
        for forbidden_pattern in forbidden_patterns {
            assert!(
                !normalized_source.contains(&forbidden_pattern),
                "ui/sidebar.rs must not reference {forbidden_pattern}"
            );
        }
    }

    fn sidebar_source() -> String {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let sidebar_path = manifest_dir.join("src/ui/sidebar.rs");
        let header_path = manifest_dir.join("src/ui/sidebar/header.rs");
        let sidebar_source =
            std::fs::read_to_string(sidebar_path).expect("sidebar source is readable");
        let header_source =
            std::fs::read_to_string(header_path).expect("sidebar header source is readable");

        format!("{sidebar_source}\n{header_source}")
    }

    fn render_host(
        egui_ctx: &egui::Context,
        host: &mut SidebarHostState,
        events: Vec<Event>,
        slide_progress: f32,
    ) -> SidebarOutput {
        let input = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: true,
            events,
            ..RawInput::default()
        };
        let mut output = None;
        let _ = egui_ctx.run_ui(input, |ui| {
            output = Some(show_sidebar_host(
                ui,
                host,
                slide_progress,
                |ui, _, _, _| {
                    ui.take_available_space();
                },
            ));
        });
        output.expect("sidebar host should render for positive progress")
    }

    fn render_host_with_transition_copy(
        egui_ctx: &egui::Context,
        host: &mut SidebarHostState,
    ) -> (SidebarOutput, egui::Rect) {
        let input = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            focused: true,
            ..RawInput::default()
        };
        let mut output = None;
        let mut remaining_rect = None;
        let _ = egui_ctx.run_ui(input, |ui| {
            output = Some(show_sidebar_host(ui, host, 1.0, |ui, panel_rect, _, _| {
                let incoming_rect = panel_rect.translate(egui::vec2(panel_rect.width(), 0.0));
                let mut incoming_copy =
                    content_child(ui, panel_rect, incoming_rect, "incoming-test-copy");
                incoming_copy.take_available_space();
            }));
            remaining_rect = Some(ui.available_rect_before_wrap());
        });
        (
            output.expect("sidebar host should render transition copy"),
            remaining_rect.expect("parent UI should expose remaining rect"),
        )
    }

    fn pointer_button(position: egui::Pos2, pressed: bool) -> Event {
        Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        }
    }
}

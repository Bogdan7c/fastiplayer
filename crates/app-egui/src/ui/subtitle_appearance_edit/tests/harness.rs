//! Общий прогон кадров режима правки: одна egui-сессия, события по кадрам, итог кадра.

use egui::epaint::ClippedShape;
use egui::{Context, CursorIcon, Event, Modifiers, PointerButton, Pos2, Rect, Shape, pos2, vec2};
use fastiplayer_config::SubtitleDecorationPreference;
use render_core::PictureRect;

use crate::state::{
    AppearanceEdit, AppearanceEditView, PlacedLayerRect, SubtitleRgb, UserSubtitleStyle,
};
use crate::ui::skin::{MinimalSkin, PlayerSkin};
use crate::ui::subtitle_appearance_edit::{
    SubtitleAppearanceEditAction, SubtitleAppearanceEditInput, show,
};
use crate::ui::test_frame::{app_behavior_context, input_at, run_ui_frame};

/// Свободная область видео в тестах (точки).
pub(super) fn video_rect() -> Rect {
    Rect::from_min_max(pos2(0.0, 40.0), pos2(800.0, 560.0))
}

/// Картинка видео в пикселях: ровно вдвое больше экрана в точках при масштабе 1.0.
pub(super) fn picture_rect_px() -> PictureRect {
    PictureRect {
        x: 0.0,
        y: 0.0,
        width: 1600.0,
        height: 1000.0,
    }
}

/// Слой в пикселях окна (при масштабе 1.0 равен точкам): 200x40 внизу по центру.
pub(super) fn layer_rect_px() -> PlacedLayerRect {
    PlacedLayerRect {
        x: 300,
        y: 450,
        width: 200,
        height: 40,
    }
}

/// Снимок сессии: слой на месте или отсутствует (образец ещё не готов).
pub(super) fn view_with(layer: Option<PlacedLayerRect>) -> AppearanceEditView {
    AppearanceEditView {
        draft: UserSubtitleStyle::default(),
        layer_rect: layer,
        picture_rect: layer.map(|_| picture_rect_px()),
    }
}

/// Итог одного кадра.
pub(super) struct FrameReport {
    pub(super) actions: Vec<SubtitleAppearanceEditAction>,
    pub(super) cursor: CursorIcon,
    pub(super) shapes: Vec<ClippedShape>,
    /// Прокрутка колеса, оставшаяся в кадре после `show` (0 - режим её съел).
    pub(super) scroll_left: f32,
}

/// Сессия egui с режимом правки.
pub(super) struct EditHarness {
    pub(super) ctx: Context,
    pub(super) view: AppearanceEditView,
    time: f64,
}

impl EditHarness {
    pub(super) fn new(view: AppearanceEditView) -> Self {
        let mut harness = Self {
            ctx: app_behavior_context(),
            view,
            time: 0.0,
        };
        // egui узнаёт прямоугольники областей (Area) только после кадра: без прогрева первые
        // события мыши не попали бы в слои, как и в живом приложении на самом первом кадре.
        harness.frame(Vec::new());
        harness.frame(Vec::new());
        harness
    }

    /// Один кадр с событиями; время идёт на 50 мс вперёд (быстрее порога двойного клика).
    pub(super) fn frame(&mut self, events: Vec<Event>) -> FrameReport {
        self.time += 0.05;
        let style = MinimalSkin.subtitle_appearance_edit_style();
        let mut actions = Vec::new();
        let mut scroll_left = 0.0;
        let view = self.view.clone();
        let output = run_ui_frame(&self.ctx, input_at(self.time, events), |ui| {
            actions = show(
                ui,
                SubtitleAppearanceEditInput {
                    view: &view,
                    video_rect: video_rect(),
                    style: &style,
                },
            );
            scroll_left = ui.input(|input| input.smooth_scroll_delta().y);
        });
        FrameReport {
            actions,
            cursor: output.platform_output.cursor_icon,
            shapes: output.shapes,
            scroll_left,
        }
    }

    /// Наведение, нажатие, движение по точкам и отпускание; возвращает отчёты всех кадров.
    pub(super) fn drag(&mut self, path: &[Pos2]) -> Vec<FrameReport> {
        let (first, rest) = path.split_first().expect("path has a start");
        let mut reports = vec![self.frame(vec![Event::PointerMoved(*first)])];
        reports.push(self.frame(vec![button(*first, true)]));
        for point in rest {
            reports.push(self.frame(vec![Event::PointerMoved(*point)]));
        }
        let last = *path.last().expect("path has an end");
        reports.push(self.frame(vec![button(last, false)]));
        reports
    }

    /// Клик: наведение, нажатие, отпускание.
    pub(super) fn click(&mut self, position: Pos2) -> Vec<FrameReport> {
        self.drag(&[position])
    }
}

/// Событие основной кнопки мыши.
pub(super) fn button(position: Pos2, pressed: bool) -> Event {
    Event::PointerButton {
        pos: position,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

/// Колесо: положительное значение - прокрутка вверх (egui `smooth_scroll_delta`).
pub(super) fn wheel(points: f32) -> Vec<Event> {
    let event = |phase, delta_y| Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, delta_y),
        phase,
        modifiers: Modifiers::NONE,
    };
    vec![
        event(egui::TouchPhase::Start, 0.0),
        event(egui::TouchPhase::Move, points),
    ]
}

/// Все правки черновика из отчётов.
pub(super) fn edits(reports: &[FrameReport]) -> Vec<AppearanceEdit> {
    reports
        .iter()
        .flat_map(|report| report.actions.iter())
        .filter_map(|action| match action {
            SubtitleAppearanceEditAction::Edit(edit) => Some(*edit),
            _ => None,
        })
        .collect()
}

/// Сумма долей всех `MoveBy` (горизонталь, вертикаль).
pub(super) fn total_move(edits: &[AppearanceEdit]) -> (f32, f32) {
    edits.iter().fold((0.0, 0.0), |(h, v), edit| match edit {
        AppearanceEdit::MoveBy {
            horizontal_fraction,
            vertical_fraction,
        } => (h + horizontal_fraction, v + vertical_fraction),
        _ => (h, v),
    })
}

/// Произведение всех множителей `ScaleTextBy`.
pub(super) fn total_scale(edits: &[AppearanceEdit]) -> f32 {
    edits
        .iter()
        .filter_map(|edit| match edit {
            AppearanceEdit::ScaleTextBy { factor } => Some(*factor),
            _ => None,
        })
        .product()
}

/// Центр текстовой надписи на экране (для клика по кнопке с этой подписью).
pub(super) fn label_center(shapes: &[ClippedShape], text: &str) -> Option<Rect> {
    fn walk(shape: &Shape, text: &str) -> Option<Rect> {
        match shape {
            Shape::Text(text_shape) if text_shape.galley.text() == text => {
                Some(text_shape.galley.rect.translate(text_shape.pos.to_vec2()))
            }
            Shape::Vec(inner) => inner.iter().find_map(|shape| walk(shape, text)),
            _ => None,
        }
    }
    shapes.iter().find_map(|clipped| walk(&clipped.shape, text))
}

/// Слой с зелёным текстом: удобное непустое значение для проверок панели.
pub(super) fn styled_view() -> AppearanceEditView {
    let mut view = view_with(Some(layer_rect_px()));
    view.draft.text_color = SubtitleRgb {
        red: 10,
        green: 200,
        blue: 30,
    };
    view.draft.decoration = SubtitleDecorationPreference::Box;
    view
}

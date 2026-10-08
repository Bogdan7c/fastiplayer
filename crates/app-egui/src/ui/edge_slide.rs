//! Сдвиг верхней/нижней egui-панели за край экрана без изменения самой панели.
//!
//! Зачем отдельный модуль: у egui `Panel` есть встроенный слайд, но только с собственным
//! таймером egui (`show_collapsible`), а владелец хочет ту же скорость и кривую, что у
//! сайдбара. Поэтому позицию анимации задаёт вызывающий код (`hidden_fraction`), а здесь
//! только геометрия:
//!
//! 1. панель рисуется в дочернем `Ui`, сдвинутом на `высота × hidden_fraction` к своему краю
//!    и обрезанном по видимой области — всё, что уехало, не рисуется и не кликается;
//! 2. её высота измеряется в этом же кадре по курсору дочернего `Ui` (egui `Panel` ставит
//!    курсор на свою внутреннюю границу);
//! 3. в родителе ставится пустая «распорка» — `Panel` той же стороны ровно на видимую часть.
//!    Соседи (сайдбар, центральная область, уведомления) раскладываются так же, как если бы
//!    там стояла сама панель, и плавно занимают освободившееся место.
//!
//! Инвариант: при `hidden_fraction == 0` остаток родителя совпадает с тем, что оставила бы
//! панель, нарисованная прямо в родителе (закреплено тестом). Поэтому вне фуллскрина
//! раскладка окна не меняется.

use egui::{Rect, Ui, UiBuilder, Vec2};

/// К какому краю экрана прижата панель и куда она уезжает.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScreenEdge {
    Top,
    Bottom,
}

/// Параметры сдвига одной панели.
#[derive(Clone, Copy, Debug)]
pub(crate) struct EdgeSlide {
    /// Стабильный id области; от него же строится id «распорки».
    pub(crate) id: egui::Id,
    pub(crate) edge: ScreenEdge,
    /// `0.0` — панель на месте, `1.0` — полностью за краем экрана.
    pub(crate) hidden_fraction: f32,
}

/// Результат отрисовки сдвигаемой панели.
#[derive(Debug)]
pub(crate) struct EdgeSlideOutput<R> {
    /// Что вернул код панели.
    pub(crate) inner: R,
    /// Видимая на экране часть панели; `None`, если панель полностью уехала.
    pub(crate) visible_rect: Option<Rect>,
}

/// Рисует панель `add_panel` со сдвигом к краю `slide.edge`.
///
/// `add_panel` обязан нарисовать ровно одну `egui::Panel` той же стороны, что `slide.edge`
/// (`Panel::top` для `Top`, `Panel::bottom` для `Bottom`), прямо в переданном `Ui`.
pub(crate) fn show<R>(
    ui: &mut Ui,
    slide: EdgeSlide,
    add_panel: impl FnOnce(&mut Ui) -> R,
) -> EdgeSlideOutput<R> {
    let available_rect = ui.available_rect_before_wrap();
    let hidden_fraction = normalized_hidden_fraction(slide.hidden_fraction);
    // Высоту прошлого кадра берём только для сдвига: при fraction 0 она не нужна вовсе,
    // а во время анимации отставание на кадр незаметно.
    let previous_extent = ui
        .ctx()
        .data(|data| data.get_temp::<f32>(slide.id))
        .unwrap_or(0.0);
    let offset = previous_extent * hidden_fraction;
    let shift = match slide.edge {
        ScreenEdge::Top => Vec2::new(0.0, -offset),
        ScreenEdge::Bottom => Vec2::new(0.0, offset),
    };

    let shifted_rect = available_rect.translate(shift);
    let mut panel_host = ui.new_child(UiBuilder::new().id_salt(slide.id).max_rect(shifted_rect));
    // Уехавшая часть не рисуется и не принимает ввод: egui не отдаёт hover/click вне clip.
    panel_host.set_clip_rect(available_rect.intersect(ui.clip_rect()));
    let inner = add_panel(&mut panel_host);

    let extent = measured_panel_extent(&panel_host, shifted_rect, slide.edge);
    ui.ctx().data_mut(|data| data.insert_temp(slide.id, extent));
    let visible_extent = (extent - offset).clamp(0.0, extent);
    reserve_visible_extent(ui, slide, visible_extent);

    EdgeSlideOutput {
        inner,
        visible_rect: visible_panel_rect(available_rect, slide.edge, visible_extent),
    }
}

/// NaN и выход за `0..=1` не должны ломать раскладку.
fn normalized_hidden_fraction(hidden_fraction: f32) -> f32 {
    if hidden_fraction.is_nan() {
        0.0
    } else {
        hidden_fraction.clamp(0.0, 1.0)
    }
}

/// Полная высота панели по курсору дочернего `Ui`: egui `Panel` после отрисовки ставит
/// курсор родителя на свою внутреннюю границу (низ для верхней, верх для нижней).
fn measured_panel_extent(panel_host: &Ui, shifted_rect: Rect, edge: ScreenEdge) -> f32 {
    let cursor = panel_host.cursor();
    let extent = match edge {
        ScreenEdge::Top => cursor.min.y - shifted_rect.top(),
        ScreenEdge::Bottom => shifted_rect.bottom() - cursor.max.y,
    };
    if extent.is_finite() {
        extent.clamp(0.0, shifted_rect.height())
    } else {
        0.0
    }
}

/// Резервирует в родителе видимую часть панели пустой панелью той же стороны.
fn reserve_visible_extent(ui: &mut Ui, slide: EdgeSlide, visible_extent: f32) {
    let spacer_id = slide.id.with("visible_extent_spacer");
    let spacer = match slide.edge {
        ScreenEdge::Top => egui::Panel::top(spacer_id),
        ScreenEdge::Bottom => egui::Panel::bottom(spacer_id),
    };
    spacer
        .exact_size(visible_extent)
        .resizable(false)
        .show_separator_line(false)
        .frame(egui::Frame::NONE)
        .show(ui, |_spacer_ui| {});
}

/// Прямоугольник видимой части панели внутри исходной области.
fn visible_panel_rect(available_rect: Rect, edge: ScreenEdge, visible_extent: f32) -> Option<Rect> {
    if visible_extent <= 0.0 {
        return None;
    }
    let mut visible_rect = available_rect;
    match edge {
        ScreenEdge::Top => visible_rect.max.y = available_rect.top() + visible_extent,
        ScreenEdge::Bottom => visible_rect.min.y = available_rect.bottom() - visible_extent,
    }
    Some(visible_rect)
}

#[cfg(test)]
mod tests;

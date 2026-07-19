//! UI-owned математика плавного акцента подтверждённо играющей строки.

use std::time::Duration;

use animation_core::{Easing, SlideTransition};
use egui::{Pos2, Rect, pos2, vec2};
use playlist_core::PlaylistItemId;

use crate::ui::animation::UiMotion;

/// Длительность перехода между одновременно видимыми строками.
pub(super) const NEARBY_TRANSITION_DURATION: Duration = Duration::from_millis(220);
/// Длительность перехода с необходимым следованием viewport-а.
pub(super) const FOLLOW_TRANSITION_DURATION: Duration = Duration::from_millis(360);
/// Первая четверть follow-пути доводит акцент до ближайшего края.
const EDGE_DEPARTURE_END: f32 = 0.25;
/// До последней четверти акцент остаётся у края во время основной прокрутки.
const EDGE_ARRIVAL_START: f32 = 0.75;

/// Эфемерное состояние не покидает renderer-bound `PlaylistUiState`.
#[derive(Debug, Default)]
pub(super) struct ActiveAccentAnimationState {
    /// Последний подтверждённый Item ID, увиденный UI.
    observed_active_item_id: Option<PlaylistItemId>,
    /// Item ID, от которого начался последний подтверждённый переход.
    previous_item_id: Option<PlaylistItemId>,
    /// Текущий authoritative Item ID, к которому относится декоративный слой.
    target_item_id: Option<PlaylistItemId>,
    /// Активный nearby/follow переход вместе с линейным timeline.
    transition: Option<ActiveAccentTransition>,
    /// Последний экранный rect декоративного акцента для непрерывного retarget.
    current_rect: Option<Rect>,
    /// Последний viewport после всех egui clamp и пользовательских interaction.
    last_authoritative_viewport: Option<AuthoritativeViewport>,
    /// Structural revision хранится как opaque монотонное значение из typed model.
    observed_structural_revision: Option<u64>,
}

/// Именованный input начала кадра не смешивает domain identity и UI policy.
#[derive(Debug, Clone, Copy)]
pub(super) struct BeginFrameInput {
    /// Подтверждённый Item ID; pending target сюда не передаётся.
    pub(super) active_item_id: Option<PlaylistItemId>,
    /// Текущее opaque значение structural revision.
    pub(super) structural_revision: u64,
    /// Индекс authoritative цели разрешается одним O(1) lookup у view model.
    pub(super) target_row_index: Option<usize>,
    /// Число строк нужно только для безопасного clamp follow offset.
    pub(super) item_count: usize,
    /// Fixed-height pitch включает межстрочный spacing egui.
    pub(super) row_pitch: f32,
    /// Общая typed reduced-motion policy.
    pub(super) motion: UiMotion,
    /// Стабильная дельта кадра, ограниченная владельцем egui input.
    pub(super) delta_seconds: f32,
    /// Wheel/drag/focus intent немедленно отдаёт viewport пользователю.
    pub(super) manual_viewport_override: bool,
}

/// Геометрия завершённого ScrollArea кадра нужна для paint и следующего retarget.
#[derive(Debug, Clone, Copy)]
pub(super) struct FinishFrameInput {
    /// Реальный viewport после egui scroll processing.
    pub(super) viewport: Option<AuthoritativeViewport>,
    /// Реальный rect authoritative строки, если она попала в visible range.
    pub(super) target_row_rect: Option<Rect>,
    /// Drag scrollbar/content мог начаться уже внутри текущего render pass.
    pub(super) manual_viewport_override: bool,
}

/// Именованный input строит viewport mapping без позиционных geometry/boolean аргументов.
#[derive(Debug, Clone, Copy)]
pub(super) struct AuthoritativeViewportInput {
    /// Точный clip rect без scrollbar-а.
    pub(super) screen_rect: Rect,
    /// Применённый egui offset после clamp и interaction.
    pub(super) scroll_offset: f32,
    /// Fixed-height pitch включает межстрочный spacing.
    pub(super) row_pitch: f32,
    /// Высота одной row surface без spacing.
    pub(super) row_height: f32,
    /// Текущее число строк ограничивает stable reference index.
    pub(super) item_count: usize,
    /// Canonical индекс одной реально отрисованной visible row.
    pub(super) reference_row_index: usize,
    /// Экранный rect той же reference row.
    pub(super) reference_row_rect: Rect,
    /// Typed источник текущего viewport ownership.
    pub(super) control: ViewportControl,
}

/// Typed источник последнего ScrollArea offset устраняет positional `bool`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ViewportControl {
    /// Offset сформирован обычным layout или auto-follow.
    Automatic,
    /// Wheel/trackpad/scrollbar/content/playlist drag принадлежит пользователю.
    Manual,
}

/// Authoritative viewport связывает content coordinates с экранными rect-ами.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct AuthoritativeViewport {
    /// Точный clip rect без scrollbar-а.
    screen_rect: Rect,
    /// Применённый egui offset после clamp и interaction.
    scroll_offset: f32,
    /// Экранная Y-координата начала первой строки при нулевом offset.
    content_origin_y: f32,
    /// Левая граница всех full-width row surfaces.
    row_left: f32,
    /// Ширина всех full-width row surfaces.
    row_width: f32,
    /// Высота видимой row surface без spacing.
    row_height: f32,
    /// Расстояние между началами соседних fixed-height строк.
    row_pitch: f32,
    /// Текущее число строк для clamp у начала и конца.
    item_count: usize,
    /// Последний кадр принадлежал wheel/drag/kinetic управлению пользователя.
    control: ViewportControl,
}

/// Один переход хранит только геометрию старта и тип пути к authoritative цели.
#[derive(Debug, Clone, Copy)]
struct ActiveAccentTransition {
    /// Экранный rect, из которого transition стартовал или был retargeted.
    source_rect: Rect,
    /// Stable индекс цели допустим, пока structural revision не изменилась.
    target_row_index: usize,
    /// Nearby и follow имеют разные duration и paint path.
    kind: ActiveAccentTransitionKind,
    /// `SlideTransition` хранит линейную позицию без baked-in easing.
    timeline: SlideTransition,
}

/// Тип пути сохраняет различие обычного движения и viewport follow.
#[derive(Debug, Clone, Copy)]
enum ActiveAccentTransitionKind {
    /// Обе строки находятся в authoritative viewport.
    Nearby,
    /// Цель вне viewport, поэтому offset и edge-hold двигаются согласованно.
    Follow {
        /// Край определяется положением target row выше или ниже viewport-а.
        edge: ViewportEdge,
        /// Стартовый offset берётся только из последнего authoritative кадра.
        scroll_start: f32,
        /// Конечный offset центрирует цель с clamp у границ списка.
        scroll_target: f32,
    },
}

/// Вертикальный край, у которого декоративный слой ждёт прокрутку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewportEdge {
    /// Цель расположена выше текущего viewport-а.
    Top,
    /// Цель расположена ниже текущего viewport-а.
    Bottom,
}

impl ActiveAccentAnimationState {
    /// Сбрасывает геометрию для доказанно пустой очереди без persistence side effects.
    pub(super) fn observe_empty(&mut self, structural_revision: u64) {
        // Empty model становится новым authoritative наблюдением.
        self.observed_structural_revision = Some(structural_revision);
        // У пустой очереди подтверждённого row Item ID быть не может.
        self.observed_active_item_id = None;
        // Previous ID не нужен после structural reset.
        self.previous_item_id = None;
        // Декоративная цель исчезает одновременно с authoritative identity.
        self.target_item_id = None;
        // Старый transition не должен пережить удаление rows.
        self.transition = None;
        // Экранный rect больше не относится к существующей строке.
        self.current_rect = None;
        // Пустой ScrollArea не задаёт пригодную row-coordinate систему.
        self.last_authoritative_viewport = None;
    }

    /// Обновляет authoritative identity и возвращает программный scroll offset кадра.
    pub(super) fn begin_frame(&mut self, input: BeginFrameInput) -> Option<f32> {
        // Сначала фиксируется domain-смена, чтобы accessibility и paint target не расходились.
        let transition_started = self.observe_authoritative_identity(input);

        // Явное пользовательское управление всегда сильнее уже начатого auto-follow.
        if input.manual_viewport_override {
            self.cancel_motion();
            return None;
        }

        // Reduced motion мгновенно доводит необходимую прокрутку до конечного offset.
        if input.motion == UiMotion::Reduced {
            let final_scroll_offset = self
                .transition
                .and_then(ActiveAccentTransition::final_scroll_offset);
            self.cancel_motion();
            return final_scroll_offset;
        }

        // Первый paint нового transition остаётся точно в source rect без скачка на один dt.
        if !transition_started && let Some(transition) = self.transition.as_mut() {
            transition.advance(input.delta_seconds);
        }

        // Nearby path не перехватывает viewport и возвращает `None`.
        self.transition
            .and_then(ActiveAccentTransition::requested_scroll_offset)
    }

    /// Завершает кадр по реальной ScrollArea геометрии и возвращает decorative rect.
    pub(super) fn finish_frame(&mut self, input: FinishFrameInput) -> Option<Rect> {
        // Scrollbar/content drag может появиться только после выполнения ScrollArea closure.
        if input.manual_viewport_override {
            self.cancel_motion();
        }

        // Без валидного viewport-а переход нельзя безопасно рисовать или продолжать.
        let Some(viewport) = input.viewport else {
            self.transition = None;
            self.current_rect = None;
            self.last_authoritative_viewport = None;
            return None;
        };

        // Authoritative viewport сохраняется после всех clamp и interaction текущего кадра.
        self.last_authoritative_viewport = Some(viewport);

        // Stop и detached active media мгновенно убирают декоративный слой.
        if self.target_item_id.is_none() {
            self.transition = None;
            self.current_rect = None;
            return None;
        }

        // При отсутствии transition accent просто привязывается к видимой target row.
        let Some(transition) = self.transition else {
            self.current_rect = input.target_row_rect;
            return visible_rect(self.current_rect, viewport.screen_rect);
        };

        // Nearby использует реальный rect цели; follow способен вычислить off-screen rect.
        let resolved_rect = match transition.kind {
            ActiveAccentTransitionKind::Nearby => input
                .target_row_rect
                .map(|target_rect| transition.nearby_rect(target_rect)),
            ActiveAccentTransitionKind::Follow { edge, .. } => viewport
                .row_rect(transition.target_row_index)
                .map(|target_rect| {
                    let edge_rect = viewport.edge_rect(edge);
                    transition.follow_rect(edge_rect, target_rect)
                }),
        };

        // Неожиданная потеря row geometry отменяет старую геометрию вместо stale paint.
        let Some(resolved_rect) = resolved_rect else {
            self.transition = None;
            self.current_rect = None;
            return None;
        };

        // Текущий rect сохраняется для непрерывного быстрого retarget.
        self.current_rect = Some(resolved_rect);

        // Последний кадр transition закрепляет accent за реальной target row.
        if transition.is_complete() {
            self.transition = None;
            self.current_rect = input.target_row_rect.or(Some(resolved_rect));
        }

        // Painter получает только пересекающую viewport геометрию.
        visible_rect(self.current_rect, viewport.screen_rect)
    }

    /// Проверяет wheel/trackpad input только внутри последнего Playlist viewport-а.
    pub(super) fn has_manual_scroll_input(&self, context: &egui::Context) -> bool {
        // До первого authoritative кадра невозможно безопасно атрибутировать scroll событие.
        let Some(viewport) = self.last_authoritative_viewport else {
            return false;
        };
        // Kinetic/drag ownership предыдущего кадра не должно проиграть новой active identity.
        viewport.control == ViewportControl::Manual
            // Pointer должен находиться над очередью, а не над соседним scrollable UI.
            || context.input(|input| {
            input
                .pointer
                .hover_pos()
                .is_some_and(|pointer| viewport.screen_rect.contains(pointer))
                && (input.is_scrolling() || input.smooth_scroll_delta().length_sq() > 0.0)
        })
    }

    /// `true` только пока finite transition действительно требует следующий кадр.
    pub(super) fn needs_repaint(&self) -> bool {
        // Завершённый или отменённый transition не оставляет idle repaint.
        self.transition
            .is_some_and(|transition| transition.timeline.is_animating())
    }

    /// Возвращает текущий decorative rect только для focused headless assertions.
    #[cfg(test)]
    pub(super) const fn current_rect_for_test(&self) -> Option<Rect> {
        // Getter не даёт production-коду читать внутреннюю геометрию напрямую.
        self.current_rect
    }

    /// Возвращает последний фактически применённый scroll offset для headless assertions.
    #[cfg(test)]
    pub(super) const fn scroll_offset_for_test(&self) -> Option<f32> {
        // Test читает уже authoritative post-ScrollArea значение.
        match self.last_authoritative_viewport {
            Some(viewport) => Some(viewport.scroll_offset),
            None => None,
        }
    }

    /// Показывает только typed факт follow без раскрытия внутреннего timeline.
    #[cfg(test)]
    pub(super) fn is_following_for_test(&self) -> bool {
        // Nearby и отсутствие transition остаются отличимыми от viewport follow.
        self.transition.is_some_and(|transition| {
            matches!(transition.kind, ActiveAccentTransitionKind::Follow { .. })
        })
    }

    /// Обрабатывает authoritative identity и при необходимости создаёт новый transition.
    fn observe_authoritative_identity(&mut self, input: BeginFrameInput) -> bool {
        // Первое наблюдение и structural change никогда не используют старую row geometry.
        let first_observation = self.observed_structural_revision.is_none();
        // Любое изменение rows инвалидирует stable index и content-coordinate mapping.
        let structural_change = self
            .observed_structural_revision
            .is_some_and(|revision| revision != input.structural_revision);
        // Старый ID нужен только для различения Some→Some от Stop/первого старта.
        let previous_authoritative_id = self.observed_active_item_id;

        // Новая structural revision становится authoritative до любых ранних выходов.
        self.observed_structural_revision = Some(input.structural_revision);
        // Новый подтверждённый ID сразу становится источником accessibility снаружи state.
        self.observed_active_item_id = input.active_item_id;
        // Paint target всегда совпадает с подтверждённой identity.
        self.target_item_id = input.active_item_id;

        // Первый кадр и structural mutation привязываются мгновенно к актуальной row.
        if first_observation || structural_change {
            self.previous_item_id = previous_authoritative_id;
            self.cancel_motion();
            return false;
        }

        // Неизменившийся active ID не перезапускает timeline.
        if previous_authoritative_id == input.active_item_id {
            return false;
        }

        // Previous ID сохраняется как ephemeral diagnostic/retarget identity.
        self.previous_item_id = previous_authoritative_id;

        // Только подтверждённый Some(old)→Some(new) имеет визуальную траекторию.
        let (Some(_previous_item_id), Some(_target_item_id)) =
            (previous_authoritative_id, input.active_item_id)
        else {
            self.cancel_motion();
            return false;
        };

        // Source должен быть реально видим в последнем authoritative viewport-е.
        let Some(source_rect) = self.visible_current_rect() else {
            self.cancel_motion();
            return false;
        };
        // Target index отсутствует только при несогласованном/stale model snapshot.
        let Some(target_row_index) = input.target_row_index else {
            self.cancel_motion();
            return false;
        };
        // Follow классифицируется по последнему завершённому viewport кадру.
        let Some(viewport) = self.last_authoritative_viewport else {
            self.cancel_motion();
            return false;
        };

        // Фактический target rect определяет nearby либо направление follow.
        let Some(target_rect) = viewport.row_rect(target_row_index) else {
            self.cancel_motion();
            return false;
        };
        // Видимая target row получает короткий 220-ms переход без scroll ownership.
        let kind = if viewport.screen_rect.intersects(target_rect) {
            ActiveAccentTransitionKind::Nearby
        } else {
            // Off-screen цель центрируется с clamp у начала и конца списка.
            let scroll_target = centered_scroll_offset(
                target_row_index,
                input.item_count,
                input.row_pitch,
                viewport.row_height,
                viewport.screen_rect.height(),
            );
            // Положение target относительно viewport выбирает ближайший край.
            let edge = if target_rect.center().y < viewport.screen_rect.top() {
                ViewportEdge::Top
            } else {
                ViewportEdge::Bottom
            };
            // Follow хранит ровно старт/цель offset без управления ScrollArea state.
            ActiveAccentTransitionKind::Follow {
                edge,
                scroll_start: viewport.scroll_offset,
                scroll_target,
            }
        };

        // Новый timeline всегда стартует закрытым и движется к единице.
        let mut timeline = SlideTransition::closed();
        // Typed intent раскрывает переход без прямого доступа к position.
        timeline.set_target_open(true);
        // Retarget source уже является текущим визуальным rect-ом.
        self.transition = Some(ActiveAccentTransition {
            source_rect,
            target_row_index,
            kind,
            timeline,
        });
        // Caller не продвигает только что созданный timeline в этом же кадре.
        true
    }

    /// Возвращает current rect только когда он пересекает последний viewport.
    fn visible_current_rect(&self) -> Option<Rect> {
        // Без viewport экранный rect нельзя считать authoritative видимым.
        let viewport = self.last_authoritative_viewport?;
        // Старый accent вне экрана запрещает автоматическое следование.
        visible_rect(self.current_rect, viewport.screen_rect)
    }

    /// Отменяет только UI motion, не меняя authoritative target Item ID.
    fn cancel_motion(&mut self) {
        // Timeline больше не имеет права запрашивать scroll или repaint.
        self.transition = None;
        // Следующий finish кадр привяжет rect к реально видимой target row.
        self.current_rect = None;
    }
}

impl AuthoritativeViewport {
    /// Строит content-coordinate mapping по одной реально отрисованной visible row.
    pub(super) fn from_rendered_row(input: AuthoritativeViewportInput) -> Option<Self> {
        // Destructure один раз, чтобы validation читалась в терминах geometry contract.
        let AuthoritativeViewportInput {
            screen_rect,
            scroll_offset,
            row_pitch,
            row_height,
            item_count,
            reference_row_index,
            reference_row_rect,
            control,
        } = input;
        // Все входы должны описывать конечную положительную UI геометрию.
        if !screen_rect.is_finite()
            || !screen_rect.is_positive()
            || !reference_row_rect.is_finite()
            || !reference_row_rect.is_positive()
            || !scroll_offset.is_finite()
            || !row_pitch.is_finite()
            || row_pitch <= 0.0
            || !row_height.is_finite()
            || row_height <= 0.0
            || item_count == 0
            || reference_row_index >= item_count
        {
            return None;
        }

        // Reference row восстанавливает экранный origin content-а при offset 0.
        let content_origin_y =
            reference_row_rect.top() + scroll_offset - reference_row_index as f32 * row_pitch;
        // Полный viewport хранит только эфемерную геометрию текущего render pass.
        Some(Self {
            screen_rect,
            scroll_offset,
            content_origin_y,
            row_left: reference_row_rect.left(),
            row_width: reference_row_rect.width(),
            row_height,
            row_pitch,
            item_count,
            control,
        })
    }

    /// Возвращает экранный rect строки без обхода очереди.
    fn row_rect(self, row_index: usize) -> Option<Rect> {
        // Stable index допустим только внутри текущей structural revision.
        if row_index >= self.item_count {
            return None;
        }
        // Content Y переводится в экранную координату вычитанием scroll offset.
        let row_top =
            self.content_origin_y + row_index as f32 * self.row_pitch - self.scroll_offset;
        // Все строки используют одну full-width surface geometry.
        Some(Rect::from_min_size(
            pos2(self.row_left, row_top),
            vec2(self.row_width, self.row_height),
        ))
    }

    /// Строит rect у ближайшего края для средней hold-фазы follow.
    fn edge_rect(self, edge: ViewportEdge) -> Rect {
        // При слишком низком viewport row остаётся привязана к верхней границе.
        let bottom_edge_top =
            (self.screen_rect.bottom() - self.row_height).max(self.screen_rect.top());
        // Typed edge устраняет двусмысленное направление числового sign.
        let row_top = match edge {
            ViewportEdge::Top => self.screen_rect.top(),
            ViewportEdge::Bottom => bottom_edge_top,
        };
        // X/width совпадают с authoritative row surfaces текущего кадра.
        Rect::from_min_size(
            pos2(self.row_left, row_top),
            vec2(self.row_width, self.row_height),
        )
    }
}

impl ActiveAccentTransition {
    /// Продвигает линейный timeline с duration, соответствующим типу пути.
    fn advance(&mut self, delta_seconds: f32) {
        // Duration остаётся константой UX-контракта, а не скрытым callsite literal.
        self.timeline
            .advance(delta_seconds, self.duration().as_secs_f32());
    }

    /// Возвращает duration nearby/follow без смешивания двух семантик.
    const fn duration(self) -> Duration {
        // Match делает разные UX-контракты исчерпывающими.
        match self.kind {
            ActiveAccentTransitionKind::Nearby => NEARBY_TRANSITION_DURATION,
            ActiveAccentTransitionKind::Follow { .. } => FOLLOW_TRANSITION_DURATION,
        }
    }

    /// Интерполирует short-path rect по общей cubic ease-out кривой.
    fn nearby_rect(self, target_rect: Rect) -> Rect {
        // Eased progress влияет только на выбор paint sample, не на timeline storage.
        let eased_progress = self.timeline.eased_progress(Easing::EaseOutCubic);
        // Source и target интерполируются целиком, включая возможную смену ширины layout.
        lerp_rect(self.source_rect, target_rect, eased_progress)
    }

    /// Вычисляет edge-hold path по линейным четвертям времени.
    fn follow_rect(self, edge_rect: Rect, target_rect: Rect) -> Rect {
        // Linear easing здесь намеренно извлекает незапечённый time progress.
        let linear_progress = self.timeline.eased_progress(Easing::Linear);
        // Pure path helper закрепляет 25/50/25 контракт.
        edge_hold_rect(self.source_rect, edge_rect, target_rect, linear_progress)
    }

    /// Возвращает текущий auto-follow offset с глобальным cubic ease-out.
    fn requested_scroll_offset(self) -> Option<f32> {
        // Nearby transition не владеет viewport offset.
        let ActiveAccentTransitionKind::Follow {
            scroll_start,
            scroll_target,
            ..
        } = self.kind
        else {
            return None;
        };
        // Scroll использует ту же smooth ease-out policy, что и nearby accent.
        let eased_progress = self.timeline.eased_progress(Easing::EaseOutCubic);
        // Scalar lerp остаётся зажатым благодаря Easing::apply.
        Some(lerp_scalar(scroll_start, scroll_target, eased_progress))
    }

    /// Возвращает конечный follow offset для reduced-motion переключения.
    const fn final_scroll_offset(self) -> Option<f32> {
        // Только follow требует необходимого viewport reposition.
        match self.kind {
            ActiveAccentTransitionKind::Nearby => None,
            ActiveAccentTransitionKind::Follow { scroll_target, .. } => Some(scroll_target),
        }
    }

    /// Завершённый timeline больше не требует repaint.
    fn is_complete(self) -> bool {
        // Target всегда остаётся open, поэтому отсутствие animation означает progress 1.
        !self.timeline.is_animating()
    }
}

/// Центрирует target row и зажимает offset у начала/конца content-а.
fn centered_scroll_offset(
    row_index: usize,
    item_count: usize,
    row_pitch: f32,
    row_height: f32,
    viewport_height: f32,
) -> f32 {
    // Некорректная геометрия безопасно возвращает начало списка.
    if item_count == 0
        || row_index >= item_count
        || !row_pitch.is_finite()
        || row_pitch <= 0.0
        || !row_height.is_finite()
        || row_height <= 0.0
        || !viewport_height.is_finite()
        || viewport_height <= 0.0
    {
        return 0.0;
    }
    // Content height не включает spacing после последней строки.
    let content_height = (item_count.saturating_sub(1)) as f32 * row_pitch + row_height;
    // Максимальный offset равен невидимому хвосту content-а.
    let max_scroll_offset = (content_height - viewport_height).max(0.0);
    // Центр target row переводится в желаемую верхнюю границу viewport-а.
    let target_center = row_index as f32 * row_pitch + row_height * 0.5;
    // Clamp сохраняет первую и последнюю строку без overscroll.
    (target_center - viewport_height * 0.5).clamp(0.0, max_scroll_offset)
}

/// Реализует departure/hold/arrival path по линейным четвертям времени.
fn edge_hold_rect(source_rect: Rect, edge_rect: Rect, target_rect: Rect, progress: f32) -> Rect {
    // NaN и выход за диапазон не должны попадать в egui geometry.
    let linear_progress = if progress.is_nan() {
        0.0
    } else {
        progress.clamp(0.0, 1.0)
    };

    // Первая четверть мягко доводит accent до края.
    if linear_progress < EDGE_DEPARTURE_END {
        let local_progress = linear_progress / EDGE_DEPARTURE_END;
        let eased_local = Easing::EaseOutCubic.apply(local_progress);
        return lerp_rect(source_rect, edge_rect, eased_local);
    }

    // Средняя половина удерживает декоративный слой у края.
    if linear_progress <= EDGE_ARRIVAL_START {
        return edge_rect;
    }

    // Последняя четверть мягко привязывает accent к уже прибывающей target row.
    let local_progress = (linear_progress - EDGE_ARRIVAL_START) / (1.0 - EDGE_ARRIVAL_START);
    let eased_local = Easing::EaseOutCubic.apply(local_progress);
    // Target rect может двигаться вместе с последними пикселями auto-scroll.
    lerp_rect(edge_rect, target_rect, eased_local)
}

/// Интерполирует весь rect без spring overshoot и нечисловой геометрии.
fn lerp_rect(source: Rect, target: Rect, progress: f32) -> Rect {
    // Core easing уже clamp-ит progress, но helper остаётся безопасным отдельно.
    let clamped_progress = if progress.is_nan() {
        0.0
    } else {
        progress.clamp(0.0, 1.0)
    };
    // Min corner интерполируется как позиция плюс доля направляющего вектора.
    let min = lerp_position(source.min, target.min, clamped_progress);
    // Max corner интерполируется независимо, сохраняя изменение размера.
    let max = lerp_position(source.max, target.max, clamped_progress);
    // Валидные source/target rect-ы сохраняют ordered corners при convex lerp.
    Rect::from_min_max(min, max)
}

/// Интерполирует экранную позицию по нормализованной доле.
fn lerp_position(source: Pos2, target: Pos2, progress: f32) -> Pos2 {
    // Разность позиций является вектором, который можно безопасно масштабировать.
    source + (target - source) * progress
}

/// Интерполирует scroll offset по нормализованной доле.
fn lerp_scalar(source: f32, target: f32, progress: f32) -> f32 {
    // Простая affine interpolation не создаёт overshoot при progress 0..=1.
    source + (target - source) * progress
}

/// Возвращает rect только когда он действительно попадает в clip viewport-а.
fn visible_rect(candidate: Option<Rect>, viewport_rect: Rect) -> Option<Rect> {
    // Частично видимая строка считается допустимым источником перехода.
    candidate.filter(|candidate_rect| viewport_rect.intersects(*candidate_rect))
}

#[cfg(test)]
mod tests {
    use super::{
        ActiveAccentAnimationState, AuthoritativeViewport, AuthoritativeViewportInput,
        BeginFrameInput, EDGE_ARRIVAL_START, EDGE_DEPARTURE_END, FOLLOW_TRANSITION_DURATION,
        FinishFrameInput, NEARBY_TRANSITION_DURATION, UiMotion, ViewportControl,
        centered_scroll_offset, edge_hold_rect, lerp_rect,
    };
    use egui::{Rect, pos2, vec2};
    use playlist_core::PlaylistItemId;

    /// Создаёт стабильный Item ID без allocator side effects.
    fn item_id(value: u64) -> PlaylistItemId {
        // Test fixture использует тот же persistence validation boundary, что и production restore.
        PlaylistItemId::from_persistence_value(value).expect("positive test Item ID")
    }

    /// Возвращает viewport десяти строк с тремя видимыми row surfaces.
    fn viewport() -> AuthoritativeViewport {
        // Reference row index 2 расположен в верхней части viewport-а при offset 68.
        AuthoritativeViewport::from_rendered_row(AuthoritativeViewportInput {
            screen_rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 102.0)),
            scroll_offset: 68.0,
            row_pitch: 34.0,
            row_height: 34.0,
            item_count: 10,
            reference_row_index: 2,
            reference_row_rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 34.0)),
            control: ViewportControl::Automatic,
        })
        .expect("valid viewport")
    }

    #[test]
    fn durations_match_nearby_and_follow_contract() {
        // Короткая траектория обязана занимать ровно 220 ms.
        assert_eq!(NEARBY_TRANSITION_DURATION.as_millis(), 220);
        // Follow с прокруткой обязана занимать ровно 360 ms.
        assert_eq!(FOLLOW_TRANSITION_DURATION.as_millis(), 360);
    }

    #[test]
    fn nearby_rect_uses_cubic_midpoint_and_exact_endpoints() {
        // Source и target отличаются только вертикальной координатой для простого ожидания.
        let source = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 20.0));
        // Target находится на сто points ниже.
        let target = source.translate(vec2(0.0, 100.0));
        // Нулевая доля сохраняет source без смещения.
        assert_eq!(lerp_rect(source, target, 0.0), source);
        // Cubic ease-out в середине равен 0.875.
        let midpoint = lerp_rect(source, target, 0.875);
        // Верхняя граница therefore проходит 87.5 points.
        assert!((midpoint.top() - 87.5).abs() < f32::EPSILON);
        // Единичная доля точно достигает target.
        assert_eq!(lerp_rect(source, target, 1.0), target);
    }

    #[test]
    fn follow_path_departs_holds_and_arrives_in_quarters() {
        // Source начинается в середине viewport-а.
        let source = Rect::from_min_size(pos2(0.0, 34.0), vec2(100.0, 20.0));
        // Edge rect расположен у нижней границы.
        let edge = Rect::from_min_size(pos2(0.0, 80.0), vec2(100.0, 20.0));
        // Target после scroll оказывается выше edge.
        let target = Rect::from_min_size(pos2(0.0, 45.0), vec2(100.0, 20.0));
        // Начало path совпадает с текущим визуальным rect.
        assert_eq!(edge_hold_rect(source, edge, target, 0.0), source);
        // Конец первой четверти достигает края.
        assert_eq!(
            edge_hold_rect(source, edge, target, EDGE_DEPARTURE_END),
            edge
        );
        // Вся средняя половина остаётся у края.
        assert_eq!(edge_hold_rect(source, edge, target, 0.5), edge);
        // Начало финальной четверти всё ещё находится у края.
        assert_eq!(
            edge_hold_rect(source, edge, target, EDGE_ARRIVAL_START),
            edge
        );
        // Конец path точно совпадает с target row.
        assert_eq!(edge_hold_rect(source, edge, target, 1.0), target);
    }

    #[test]
    fn centered_scroll_clamps_first_and_last_rows() {
        // Первая строка не создаёт отрицательный overscroll.
        assert_eq!(centered_scroll_offset(0, 100, 34.0, 34.0, 102.0), 0.0);
        // Последняя строка зажимается по нижней границе content-а.
        assert_eq!(centered_scroll_offset(99, 100, 34.0, 34.0, 102.0), 3_298.0);
        // Средняя строка действительно центрируется.
        assert_eq!(centered_scroll_offset(50, 100, 34.0, 34.0, 102.0), 1_666.0);
    }

    #[test]
    fn reduced_motion_jumps_required_follow_without_repaint() {
        // State уже наблюдал первую активную строку и её видимый accent.
        let mut state = ActiveAccentAnimationState {
            observed_active_item_id: Some(item_id(1)),
            target_item_id: Some(item_id(1)),
            current_rect: viewport().row_rect(2),
            last_authoritative_viewport: Some(viewport()),
            observed_structural_revision: Some(7),
            ..ActiveAccentAnimationState::default()
        };
        // Новая подтверждённая цель находится вне viewport-а.
        let requested_offset = state.begin_frame(BeginFrameInput {
            active_item_id: Some(item_id(2)),
            structural_revision: 7,
            target_row_index: Some(9),
            item_count: 10,
            row_pitch: 34.0,
            motion: UiMotion::Reduced,
            delta_seconds: 1.0 / 60.0,
            manual_viewport_override: false,
        });
        // Reduced motion сразу применяет конечный clamped offset.
        assert_eq!(requested_offset, Some(238.0));
        // Мгновенный режим не оставляет transition repaint.
        assert!(!state.needs_repaint());
    }

    #[test]
    fn mid_flight_retarget_starts_from_current_visual_rect() {
        // State начинает с видимого первого Item ID.
        let initial_viewport = viewport();
        // Старая row находится на верхней видимой позиции.
        let initial_rect = initial_viewport.row_rect(2).expect("visible source");
        // Новый nearby target расположен строкой ниже.
        let second_rect = initial_viewport.row_rect(3).expect("visible second target");
        // Следующий target расположен ещё на строку ниже.
        let third_rect = initial_viewport.row_rect(4).expect("visible third target");
        // Ephemeral state получает только геометрию и stable identities.
        let mut state = ActiveAccentAnimationState {
            observed_active_item_id: Some(item_id(1)),
            target_item_id: Some(item_id(1)),
            current_rect: Some(initial_rect),
            last_authoritative_viewport: Some(initial_viewport),
            observed_structural_revision: Some(9),
            ..ActiveAccentAnimationState::default()
        };

        // Первый Some→Some запускает nearby transition без продвижения в стартовом кадре.
        state.begin_frame(BeginFrameInput {
            active_item_id: Some(item_id(2)),
            structural_revision: 9,
            target_row_index: Some(3),
            item_count: 10,
            row_pitch: 34.0,
            motion: UiMotion::Standard,
            delta_seconds: 0.11,
            manual_viewport_override: false,
        });
        // Стартовый paint остаётся в source rect.
        assert_eq!(
            state.finish_frame(FinishFrameInput {
                viewport: Some(initial_viewport),
                target_row_rect: Some(second_rect),
                manual_viewport_override: false,
            }),
            Some(initial_rect)
        );

        // Половина 220-ms timeline создаёт текущий cubic visual sample.
        state.begin_frame(BeginFrameInput {
            active_item_id: Some(item_id(2)),
            structural_revision: 9,
            target_row_index: Some(3),
            item_count: 10,
            row_pitch: 34.0,
            motion: UiMotion::Standard,
            delta_seconds: 0.11,
            manual_viewport_override: false,
        });
        // Current visual rect сохраняется до нового authoritative retarget.
        let current_visual_rect = state
            .finish_frame(FinishFrameInput {
                viewport: Some(initial_viewport),
                target_row_rect: Some(second_rect),
                manual_viewport_override: false,
            })
            .expect("mid-flight accent");

        // Быстрый следующий Some→Some создаёт новый timeline из текущего sample.
        state.begin_frame(BeginFrameInput {
            active_item_id: Some(item_id(3)),
            structural_revision: 9,
            target_row_index: Some(4),
            item_count: 10,
            row_pitch: 34.0,
            motion: UiMotion::Standard,
            delta_seconds: 0.01,
            manual_viewport_override: false,
        });
        // Первый paint retarget-а не прыгает к старой или новой row.
        assert_eq!(
            state.finish_frame(FinishFrameInput {
                viewport: Some(initial_viewport),
                target_row_rect: Some(third_rect),
                manual_viewport_override: false,
            }),
            Some(current_visual_rect)
        );
    }
}

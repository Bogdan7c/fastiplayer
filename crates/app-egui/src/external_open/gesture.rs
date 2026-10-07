//! Состояние жеста перетаскивания и сбор его в один [`ExternalOpenRequest`].
//!
//! Владелец инварианта «один бросок — ровно один запрос»: серия событий
//! `Entered/Moved/…` не создаёт запросов, а `Dropped` создаёт ровно один, с элементами в
//! порядке источника. `Left` без броска запроса не даёт. Состояние «над окном что-то
//! тащат» (для подсветки) живёт здесь же и сбрасывается и `Dropped`, и `Left`.

use super::event::{DropGestureEvent, PhysicalDropPosition};
use super::request::{DropTarget, ExternalOpenRequest};

/// Состояние жеста: либо ничего не происходит, либо что-то висит над окном.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum GestureState {
    /// Над окном ничего не тащат.
    #[default]
    Idle,
    /// Над окном тащат данные; позиция может быть неизвестна (устаревший источник).
    Hovering {
        /// Последняя известная позиция курсора.
        last_position: Option<PhysicalDropPosition>,
    },
}

/// Отслеживает жест и выдаёт запрос открытия в момент броска.
#[derive(Debug, Default)]
pub(crate) struct DropGestureTracker {
    state: GestureState,
}

impl DropGestureTracker {
    /// Применяет событие жеста.
    ///
    /// `resolve_target` вызывается только при броске и превращает точку броска в место
    /// назначения. Источник цели передаётся снаружи, чтобы жест не знал про геометрию UI.
    pub(crate) fn apply_event(
        &mut self,
        event: DropGestureEvent,
        resolve_target: impl FnOnce(Option<PhysicalDropPosition>) -> DropTarget,
    ) -> Option<ExternalOpenRequest> {
        match event {
            DropGestureEvent::Entered { position } => {
                self.state = GestureState::Hovering {
                    last_position: position,
                };
                None
            }
            DropGestureEvent::Moved { position } => {
                // Потерянный `Entered` не должен ломать жест: движение тоже означает «тащат».
                self.state = GestureState::Hovering {
                    last_position: Some(position),
                };
                None
            }
            DropGestureEvent::Left => {
                self.state = GestureState::Idle;
                None
            }
            DropGestureEvent::Dropped { position, items } => {
                // Бросок без позиции берёт последнюю известную: некоторые источники не
                // повторяют координаты в финальном событии.
                let drop_position = position.or(self.last_known_position());
                self.state = GestureState::Idle;
                Some(ExternalOpenRequest {
                    target: resolve_target(drop_position),
                    items,
                })
            }
        }
    }

    /// `Some(позиция)` пока над окном тащат данные; внутренний `None` — позиция неизвестна.
    pub(crate) fn hovering_position(&self) -> Option<Option<PhysicalDropPosition>> {
        match self.state {
            GestureState::Idle => None,
            GestureState::Hovering { last_position } => Some(last_position),
        }
    }

    /// Последняя известная позиция курсора в текущем жесте.
    fn last_known_position(&self) -> Option<PhysicalDropPosition> {
        self.hovering_position().flatten()
    }
}

#[cfg(test)]
mod tests;

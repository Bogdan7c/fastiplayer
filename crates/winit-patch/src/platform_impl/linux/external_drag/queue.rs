//! Очередь событий внешнего drag & drop между бэкендом и диспетчером приложения.
//!
//! Почему очередь, а не новый вариант `Event`. Публичный `winit::event::Event` не
//! помечен `#[non_exhaustive]`: новый вариант сломал бы чужие исчерпывающие `match`.
//! Поэтому бэкенд кладёт событие сюда, а общий диспетчер
//! (`event_loop::dispatch_event_for_app`) после каждого обычного события забирает
//! накопленное и вызывает `ApplicationHandler::external_drag_event`.
//!
//! Очередь thread-local: и бэкенд (обработчики Wayland / X11), и диспетчер работают
//! в одном потоке event loop. Изолированный модуль с интерфейсом «опубликовать /
//! забрать» не даёт остальному коду зависеть от способа хранения.
//!
//! Доставка гарантирована: каждая итерация цикла завершается `AboutToWait`, поэтому
//! событие, опубликованное при разборе, уходит приложению в той же итерации.

use std::cell::RefCell;
use std::collections::VecDeque;

use crate::platform::external_drag::ExternalDragEvent;
use crate::window::WindowId;

/// Событие жеста вместе с окном-адресатом.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PendingExternalDrag {
    /// Окно, над которым идёт жест.
    pub(crate) window_id: WindowId,
    /// Само событие жеста.
    pub(crate) event: ExternalDragEvent,
}

thread_local! {
    /// FIFO-очередь ещё не доставленных событий этого потока.
    static PENDING_EVENTS: RefCell<VecDeque<PendingExternalDrag>> =
        const { RefCell::new(VecDeque::new()) };
}

/// Ставит событие жеста в конец очереди.
pub(crate) fn publish(window_id: WindowId, event: ExternalDragEvent) {
    PENDING_EVENTS.with(|queue| {
        queue.borrow_mut().push_back(PendingExternalDrag { window_id, event });
    });
}

/// Забирает самое старое недоставленное событие, если оно есть.
///
/// Очередь не удерживается между вызовами, поэтому обработчик приложения может
/// безопасно породить новые события (они уйдут следующими).
pub(crate) fn take_next() -> Option<PendingExternalDrag> {
    PENDING_EVENTS.with(|queue| queue.borrow_mut().pop_front())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dpi::PhysicalPosition;

    /// Окно-пустышка: значение идентификатора не важно, важно только его сравнение.
    fn window(raw_id: u64) -> WindowId {
        WindowId(crate::platform_impl::WindowId::from(raw_id))
    }

    #[test]
    fn events_are_delivered_in_publication_order() {
        // Чистим состояние потока теста на случай соседних тестов.
        while take_next().is_some() {}

        publish(
            window(1),
            ExternalDragEvent::Entered { position: PhysicalPosition::new(1.0, 2.0) },
        );
        publish(window(1), ExternalDragEvent::Left);

        let first = take_next().expect("first event");
        let second = take_next().expect("second event");

        assert_eq!(
            first.event,
            ExternalDragEvent::Entered { position: PhysicalPosition::new(1.0, 2.0) }
        );
        assert_eq!(first.window_id, window(1));
        assert_eq!(second.event, ExternalDragEvent::Left);
        assert!(take_next().is_none(), "очередь должна опустеть");
    }

    #[test]
    fn queue_is_isolated_per_thread() {
        while take_next().is_some() {}
        publish(window(7), ExternalDragEvent::Left);

        let other_thread_sees_event =
            std::thread::spawn(|| take_next().is_some()).join().expect("thread joined");

        assert!(!other_thread_sees_event, "другой поток не должен видеть чужие события");
        assert!(take_next().is_some(), "событие остаётся у владельца потока");
    }
}

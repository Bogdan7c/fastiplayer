//! Переходник winit → нейтральные события жеста. ЕДИНСТВЕННЫЙ файл модуля, который знает winit.
//!
//! Два источника:
//! - **канал `external_drag`** патча `crates/winit-patch` (Linux: X11 и Wayland) — вход,
//!   движение, бросок с позицией и сырыми URI, уход;
//! - **устаревшие события** `HoveredFile`/`DroppedFile`/`HoveredFileCancelled` — источник
//!   только вне Linux (на Linux они тоже приходят, но обрабатывать их нельзя: бросок
//!   был бы выполнен дважды, а позиции у них нет).
//!
//! # Когда выйдет стабильный winit 0.31
//!
//! Патч выбрасывается, а переписать нужно только этот файл: `DragEntered`/`DragMoved`/
//! `DragDropped`/`DragLeft` и `DataTransfer` переводятся в тот же [`DropGestureEvent`].
//! Жест, классификатор, маршрутизация, подсветка и тесты остаются без изменений.

use winit::dpi::PhysicalPosition;
use winit::platform::external_drag::ExternalDragEvent;

use super::event::{DropGestureEvent, PhysicalDropPosition};
use super::uri::items_from_drop_payload;

/// Переводит событие канала `external_drag` в нейтральное; неизвестные варианты игнорирует
/// (`ExternalDragEvent` помечен `#[non_exhaustive]`).
pub(crate) fn gesture_event_from_external_drag(
    event: ExternalDragEvent,
) -> Option<DropGestureEvent> {
    match event {
        ExternalDragEvent::Entered { position } => Some(DropGestureEvent::Entered {
            position: Some(physical_position(position)),
        }),
        ExternalDragEvent::Moved { position } => Some(DropGestureEvent::Moved {
            position: physical_position(position),
        }),
        ExternalDragEvent::Dropped { position, payload } => {
            let (uris, plain_text) = payload.into_parts();
            Some(DropGestureEvent::Dropped {
                position: Some(physical_position(position)),
                items: items_from_drop_payload(&uris, plain_text.as_deref()),
            })
        }
        ExternalDragEvent::Left => Some(DropGestureEvent::Left),
        _ => None,
    }
}

/// Позиция winit → нейтральная позиция в физических пикселях.
fn physical_position(position: PhysicalPosition<f64>) -> PhysicalDropPosition {
    PhysicalDropPosition {
        x: position.x,
        y: position.y,
    }
}

#[cfg(any(not(target_os = "linux"), test))]
pub(crate) use legacy::LegacyFileDropCollector;

/// Сборщик устаревших событий: превращает серию `DroppedFile` в один жест без позиции.
#[cfg(any(not(target_os = "linux"), test))]
mod legacy {
    use std::path::PathBuf;

    use winit::event::WindowEvent;

    use super::DropGestureEvent;
    use crate::external_open::request::ExternalOpenItem;

    /// Собирает `HoveredFile`/`DroppedFile` одного жеста.
    ///
    /// winit присылает по событию на каждый файл, поэтому бросок нельзя исполнять сразу:
    /// пути копятся, а [`Self::take_completed_drop`] вызывается один раз за проход
    /// event loop (`about_to_wait`) и выдаёт весь бросок единым событием.
    #[derive(Debug, Default)]
    pub(crate) struct LegacyFileDropCollector {
        hovering: bool,
        dropped_paths: Vec<PathBuf>,
    }

    impl LegacyFileDropCollector {
        /// Принимает оконное событие; возвращает событие жеста, если оно уже готово.
        pub(crate) fn on_window_event(&mut self, event: &WindowEvent) -> Option<DropGestureEvent> {
            match event {
                WindowEvent::HoveredFile(_) if !self.hovering => {
                    self.hovering = true;
                    Some(DropGestureEvent::Entered { position: None })
                }
                WindowEvent::HoveredFileCancelled if self.hovering => {
                    self.hovering = false;
                    Some(DropGestureEvent::Left)
                }
                WindowEvent::DroppedFile(path) => {
                    self.dropped_paths.push(path.clone());
                    None
                }
                _ => None,
            }
        }

        /// Забирает накопленный бросок целиком (путей нет — `None`) и завершает жест.
        pub(crate) fn take_completed_drop(&mut self) -> Option<DropGestureEvent> {
            if self.dropped_paths.is_empty() {
                return None;
            }
            self.hovering = false;
            let items: Vec<ExternalOpenItem> = std::mem::take(&mut self.dropped_paths)
                .into_iter()
                .map(ExternalOpenItem::LocalPath)
                .collect();
            Some(DropGestureEvent::Dropped {
                position: None,
                items,
            })
        }
    }
}

#[cfg(test)]
mod tests;

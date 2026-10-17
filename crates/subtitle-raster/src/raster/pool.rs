// Rust-часть растеризатора, © Fastiplayer contributors.

//! Пул буферов отрезков растеризатора, по одному на поток.
//!
//! libass держит `RasterizerData` (с буферами `linebuf`) в состоянии рендерера и растит их один
//! раз. У нас растеризатор создаётся на каждую реплику, поэтому без пула каждая реплика заново
//! выделяла бы и наращивала два стека отрезков (десятки тысяч байт при обводке сложного текста).
//! Пул хранит один набор буферов на поток: [`Rasterizer::new`](super::Rasterizer::new) берёт его,
//! `Drop` возвращает. Содержимое при возврате очищается, поэтому состояние между
//! растеризаторами не протекает; слишком раздутый набор не сохраняется, чтобы разовая гигантская
//! фигура не удерживала память навсегда.

use std::cell::RefCell;

use super::fill::LineStacks;

/// Наибольшая суммарная ёмкость набора (в отрезках), которую пул оставляет себе.
/// Около 4 МиБ при 40 байтах на отрезок.
const MAX_RETAINED_SEGMENTS: usize = 100_000;

thread_local! {
    /// Свободный набор буферов потока (`None`, пока никто его не вернул).
    static FREE_STACKS: RefCell<Option<LineStacks>> = const { RefCell::new(None) };
}

/// Берёт набор буферов потока либо создаёт пустой.
pub(super) fn take_stacks() -> LineStacks {
    FREE_STACKS
        .try_with(|slot| slot.try_borrow_mut().ok().and_then(|mut free| free.take()))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Возвращает набор буферов в пул потока (очищенным); слишком большой набор просто удаляется.
pub(super) fn give_back_stacks(mut stacks: LineStacks) {
    stacks.clear();
    if stacks.retained_capacity() > MAX_RETAINED_SEGMENTS {
        return;
    }
    // Поток уже завершается или пул занят - буферы просто освобождаются.
    let _ = FREE_STACKS.try_with(|slot| {
        if let Ok(mut free) = slot.try_borrow_mut() {
            *free = Some(stacks);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Каждый тест идёт в своём потоке теста, а пул потоковый, поэтому пул пуст на старте.
    #[test]
    fn returned_buffers_are_handed_out_once_with_their_capacity() {
        assert_eq!(
            take_stacks().retained_capacity(),
            0,
            "fresh thread: empty pool"
        );
        give_back_stacks(LineStacks::reserved(1000));
        let reused = take_stacks();
        assert!(
            reused.retained_capacity() >= 1000,
            "the pooled capacity is reused, not reallocated: {}",
            reused.retained_capacity()
        );
        // Набор выдан один раз: следующий запрос получает пустой.
        assert_eq!(take_stacks().retained_capacity(), 0);
    }

    #[test]
    fn oversized_buffers_are_dropped_instead_of_kept_forever() {
        give_back_stacks(LineStacks::reserved(MAX_RETAINED_SEGMENTS + 1));
        assert_eq!(take_stacks().retained_capacity(), 0);
        // Набор ровно на пределе ещё сохраняется.
        give_back_stacks(LineStacks::reserved(MAX_RETAINED_SEGMENTS));
        assert!(take_stacks().retained_capacity() >= MAX_RETAINED_SEGMENTS);
    }
}

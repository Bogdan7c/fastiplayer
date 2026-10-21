//! Упорядоченная передача кадров показа из worker-а в UI-поток (cpu-parity, сессия 06a).
//!
//! ## Зачем не «последний кадр побеждает»
//!
//! Worker выбирает кадр в ритме видео (PTS по часам звука), а UI забирает его в ритме
//! дисплея: под XWayland/FIFO главный поток на каждом кадре ждёт свободный буфер swapchain,
//! поэтому заглядывает в слот строго по vsync. На 60 fps при дисплее 59,96 Гц два ритма
//! почти совпадают; если их фазы сошлись, дрожание в доли миллисекунды решает, успел ли
//! кадр к чтению. Слот на одно место тогда то повторяет прежний кадр, то перезаписывает
//! незабранный — пара «повтор + пропуск» (до 2–3 % кадров за прогон, `results/06a.md`).
//!
//! Здесь незабранный кадр не перезаписывается: UI получает кадры одной последовательности
//! строго по порядку. Сброс возможен только когда UI отстал больше чем на
//! [`UNTAKEN_PRESENT_FRAME_CAPACITY`] кадров — это неизбежный пропуск раз в ~26 с, когда
//! видео 60,00 fps идёт быстрее дисплея 59,96 Гц, и он считается отдельным счётчиком.
//!
//! ## Инварианты
//! 1. Кадры одной последовательности (та же render/decoded generation, PTS растёт) уходят
//!    в UI в порядке публикации; ни один не пропадает, пока очередь не переполнена.
//! 2. Кадр другой последовательности (seek, смена поколения, PTS назад) отбрасывает все
//!    незабранные кадры старой: устаревшую картинку после seek UI не увидит.
//! 3. Слот держит не больше `1 + UNTAKEN_PRESENT_FRAME_CAPACITY` lease-ов: забранный UI
//!    кадр (повторное чтение вернёт его же) и незабранные.
//! 4. Lease-ы освобождаются (RAII release ack может ждать канал до 2 мс) только вне mutex-а:
//!    каждая критическая секция — перестановка уже существующих значений без пользовательского
//!    кода, поэтому после poison состояние согласовано и восстанавливается (как у соседнего
//!    `LatestPresentFrameHandoff`, `mem:panic-invariant-policy`).

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, TryLockError};

use tracing::warn;
use video_present_core::VideoFrameLease;

use super::{
    LatestPresentFrameAcquire, PresentFrameIdentityProbe, present_frame_identity_from_lease,
};
use crate::session::PresentFrameIdentity;

/// Сколько опубликованных, но ещё не забранных UI кадров держит передача.
///
/// Два места — минимум, который гасит совпадение фаз: в момент столкновения UI видит
/// «ничего нового», а к следующему чтению worker успевает положить ещё один кадр. С одним
/// местом ранний из этих двух терялся бы (прежнее поведение). Больше двух не нужно:
/// очередь в норме пуста или держит один кадр, а лишнее место — лишняя удержанная
/// поверхность декодера (запрет issue #10 на рост очередей без решения владельца).
pub(crate) const UNTAKEN_PRESENT_FRAME_CAPACITY: usize = 2;

/// Итог публикации кадра worker-ом.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "сброс незабранного кадра обязан попасть в диагностику"]
pub(crate) enum OrderedPresentFramePublish {
    /// Кадр встал в очередь, ни один незабранный кадр не потерян.
    Queued,
    /// Очередь была полна: самый старый незабранный кадр сброшен, так и не дойдя до UI.
    DroppedOldestUntaken,
    /// Кадр начинает новую последовательность (seek, смена поколения, PTS назад);
    /// незабранные кадры прежней отброшены как устаревшие — это не потеря каденции.
    RestartedSequence {
        /// Сколько незабранных кадров старой последовательности отброшено.
        discarded_untaken_frames: usize,
    },
}

/// Содержимое передачи под одним mutex-ом.
struct OrderedPresentFrames {
    /// Последний кадр, который UI уже забрал; повторное чтение без нового кадра вернёт его.
    taken: Option<VideoFrameLease>,
    /// Опубликованные, но ещё не забранные кадры, старший первым.
    untaken: VecDeque<VideoFrameLease>,
}

impl OrderedPresentFrames {
    /// Кадр, который UI получит при следующем взятии: старший незабранный или уже забранный.
    fn next_for_render(&self) -> Option<&VideoFrameLease> {
        self.untaken.front().or(self.taken.as_ref())
    }

    /// Самый свежий опубликованный кадр — с ним worker сверяет «кадр сменился?».
    fn newest_published(&self) -> Option<&VideoFrameLease> {
        self.untaken.back().or(self.taken.as_ref())
    }
}

/// Lease-ы, вынутые из передачи под mutex-ом; освобождаются после его отпускания.
///
/// Массив фиксированного размера вместо `Vec`: публикация на горячем пути без аллокаций.
/// Больше `UNTAKEN_PRESENT_FRAME_CAPACITY + 1` lease-ов за одну операцию не вынимается.
struct RetiredLeases([Option<VideoFrameLease>; UNTAKEN_PRESENT_FRAME_CAPACITY + 1]);

impl RetiredLeases {
    /// Пустой набор.
    const fn new() -> Self {
        Self([const { None }; UNTAKEN_PRESENT_FRAME_CAPACITY + 1])
    }

    /// Запоминает lease для освобождения вне mutex-а.
    ///
    /// Вызывающий код вынимает не больше lease-ов, чем мест; если инвариант всё же нарушен,
    /// lease освобождается сразу (под mutex-ом) — медленнее, но без потери release ack.
    fn retire(&mut self, lease: VideoFrameLease) {
        match self.0.iter_mut().find(|slot| slot.is_none()) {
            Some(free_slot) => *free_slot = Some(lease),
            None => drop(lease),
        }
    }
}

/// Передача кадров показа worker → UI с сохранением порядка (см. документацию модуля).
pub(crate) struct OrderedPresentFrameHandoff {
    /// Забранный и незабранные кадры; worker пишет блокирующе, UI читает только `try_lock`.
    frames: Mutex<OrderedPresentFrames>,
}

impl OrderedPresentFrameHandoff {
    /// Создаёт пустую передачу; место под очередь выделяется сразу, чтобы горячий путь
    /// публикации не аллоцировал.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            frames: Mutex::new(OrderedPresentFrames {
                taken: None,
                untaken: VecDeque::with_capacity(UNTAKEN_PRESENT_FRAME_CAPACITY),
            }),
        }
    }

    /// Публикует новый кадр показа (worker).
    pub(crate) fn publish(&self, frame: VideoFrameLease) -> OrderedPresentFramePublish {
        let mut retired = RetiredLeases::new();
        let publication = {
            let mut frames = self.frames_guard();
            let publication = Self::make_room_for(&mut frames, &frame, &mut retired);
            frames.untaken.push_back(frame);
            publication
        };
        drop(retired);
        publication
    }

    /// Освобождает место под `frame` и решает, чем публикация считается.
    fn make_room_for(
        frames: &mut OrderedPresentFrames,
        frame: &VideoFrameLease,
        retired: &mut RetiredLeases,
    ) -> OrderedPresentFramePublish {
        let continues_sequence = frames.newest_published().is_none_or(|newest| {
            continues_present_sequence(
                &present_frame_identity_from_lease(newest),
                &present_frame_identity_from_lease(frame),
            )
        });
        if !continues_sequence {
            let discarded_untaken_frames = frames.untaken.len();
            while let Some(stale_frame) = frames.untaken.pop_front() {
                retired.retire(stale_frame);
            }
            return OrderedPresentFramePublish::RestartedSequence {
                discarded_untaken_frames,
            };
        }
        if frames.untaken.len() < UNTAKEN_PRESENT_FRAME_CAPACITY {
            return OrderedPresentFramePublish::Queued;
        }
        match frames.untaken.pop_front() {
            Some(dropped_frame) => {
                retired.retire(dropped_frame);
                OrderedPresentFramePublish::DroppedOldestUntaken
            }
            // Ёмкость ненулевая, значит полная очередь не пуста; ветка только для полноты.
            None => OrderedPresentFramePublish::Queued,
        }
    }

    /// Очищает передачу: кадра для показа больше нет (stop, конец, смена источника).
    pub(crate) fn clear(&self) {
        let mut retired = RetiredLeases::new();
        {
            let mut frames = self.frames_guard();
            if let Some(taken_frame) = frames.taken.take() {
                retired.retire(taken_frame);
            }
            while let Some(untaken_frame) = frames.untaken.pop_front() {
                retired.retire(untaken_frame);
            }
        }
        drop(retired);
    }

    /// Identity самого свежего опубликованного кадра (worker: «нужно ли публиковать?»).
    pub(crate) fn newest_identity(&self) -> Option<PresentFrameIdentity> {
        self.frames_guard()
            .newest_published()
            .map(present_frame_identity_from_lease)
    }

    /// Неблокирующе забирает следующий кадр для renderer-а (UI-поток).
    ///
    /// Старший незабранный кадр становится «забранным»; если незабранных нет, повторно
    /// отдаётся уже забранный (UI сам поймёт по identity, что кадр тот же). `Busy` — worker
    /// прямо сейчас публикует: UI показывает кэш, а пробуждение после публикации придёт.
    pub(crate) fn try_take_next(&self) -> LatestPresentFrameAcquire {
        let mut retired = RetiredLeases::new();
        let acquire = match self.frames.try_lock() {
            Ok(mut frames) => Self::take_next_from(&mut frames, &mut retired),
            Err(TryLockError::WouldBlock) => LatestPresentFrameAcquire::Busy,
            Err(TryLockError::Poisoned(poisoned)) => {
                warn!("Ordered present frame handoff mutex was poisoned; recovering slot");
                let mut frames = poisoned.into_inner();
                Self::take_next_from(&mut frames, &mut retired)
            }
        };
        drop(retired);
        acquire
    }

    /// Продвигает очередь на один кадр под уже взятым mutex-ом.
    fn take_next_from(
        frames: &mut OrderedPresentFrames,
        retired: &mut RetiredLeases,
    ) -> LatestPresentFrameAcquire {
        if let Some(next_frame) = frames.untaken.pop_front() {
            // Прежний забранный кадр UI держит в своём кэше, пока показывает; слоту он
            // больше не нужен.
            if let Some(previous_taken) = frames.taken.replace(next_frame) {
                retired.retire(previous_taken);
            }
        }
        frames.taken.as_ref().cloned().map_or(
            LatestPresentFrameAcquire::Empty,
            LatestPresentFrameAcquire::Acquired,
        )
    }

    /// Неблокирующе читает identity кадра, который UI получит при следующем взятии.
    ///
    /// UI по нему решает «картинка изменится?» до любой GPU-работы (сессия 04) и после
    /// показа — «ждёт ли ещё кадр?» (сессия 06a). Очередь не продвигается.
    pub(crate) fn try_next_identity(&self) -> PresentFrameIdentityProbe {
        match self.frames.try_lock() {
            Ok(frames) => PresentFrameIdentityProbe::Known(
                frames
                    .next_for_render()
                    .map(present_frame_identity_from_lease),
            ),
            Err(TryLockError::WouldBlock) => PresentFrameIdentityProbe::Busy,
            Err(TryLockError::Poisoned(poisoned)) => {
                warn!("Ordered present frame handoff mutex was poisoned; recovering slot");
                let frames = poisoned.into_inner();
                PresentFrameIdentityProbe::Known(
                    frames
                        .next_for_render()
                        .map(present_frame_identity_from_lease),
                )
            }
        }
    }

    /// Берёт mutex для worker-side операций и восстанавливается после poison (инвариант 4).
    fn frames_guard(&self) -> MutexGuard<'_, OrderedPresentFrames> {
        match self.frames.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                warn!("Ordered present frame handoff mutex was poisoned; recovering slot");
                poisoned.into_inner()
            }
        }
    }
}

impl Default for OrderedPresentFrameHandoff {
    /// Возвращает пустую передачу.
    fn default() -> Self {
        Self::new()
    }
}

/// `true`, если `next` продолжает ту же последовательность показа, что и `newest`.
///
/// Одна последовательность — то же поколение рендера и декодера и строго растущий PTS.
/// Всё остальное (seek, flush, смена поколения, повтор PTS) начинает новую.
fn continues_present_sequence(newest: &PresentFrameIdentity, next: &PresentFrameIdentity) -> bool {
    newest.render_generation() == next.render_generation()
        && newest.decoded_generation() == next.decoded_generation()
        && next.pts() > newest.pts()
}

#[cfg(test)]
mod tests;

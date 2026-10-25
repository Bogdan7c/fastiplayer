//! Кольцо слотов текстур плоскостей (чистая логика, без GL).
//!
//! Зачем: запись в текстуру, которую GPU ещё читает предыдущим рисованием, останавливает
//! конвейер — драйвер неявно ждёт GPU (замер сессии 00 на T410: «загрузка + проход» 17,4 мс
//! вместо ~9). Поэтому новый кадр пишется в другой набор текстур, а набор становится снова
//! доступен для записи, только когда сработал fence работы, которая его трогала.
//!
//! Правила кольца:
//! - текущий слот (кадр на экране) не перезаписывается никогда: его рисуют повторы кадра;
//! - слот, тронутый загрузкой или рисованием, до следующего [`UploadRing::retire`] — «в
//!   неотданной работе»: fence на неё ещё не поставлен, писать нельзя;
//! - после `retire(fence)` слот свободен, когда fence сработал (проверка без ожидания);
//! - свободного слота нет — [`BusyReason`]: кольцо не растёт, вызывающий показывает прошлый кадр.
//!
//! Fence-ы одного контекста срабатывают по порядку команд, поэтому более поздний fence заменяет
//! более ранний у того же слота.

use std::num::NonZeroUsize;
use std::sync::Arc;

use gles2_egui::{FenceStatus, FenceWaitError, GpuFence};

use crate::planes::VideoFrameKey;

/// Проверка «GPU закончил работу до этой метки» без ожидания.
pub(crate) trait FenceProbe {
    /// Отказ проверки (потеря контекста, ошибка драйвера).
    type Error;

    /// Сработал ли fence.
    fn is_signaled(&self) -> Result<bool, Self::Error>;
}

impl FenceProbe for GpuFence {
    type Error = FenceWaitError;

    fn is_signaled(&self) -> Result<bool, Self::Error> {
        self.poll()
            .map(|status| matches!(status, FenceStatus::Signaled))
    }
}

/// Число слотов кольца: не меньше двух (текущий кадр + слот для следующего).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadSlotCount(NonZeroUsize);

impl UploadSlotCount {
    /// Наименьшее осмысленное кольцо: текущий слот не перезаписывается, нужен хотя бы ещё один.
    pub const MINIMUM: usize = 2;

    /// По умолчанию три слота: кадр на экране, кадр, который GPU ещё может читать, и слот для
    /// записи (план сессии 05).
    pub const DEFAULT: Self = Self(NonZeroUsize::MIN.saturating_add(2));

    /// Кольцо из `slots` слотов; `None`, если слотов меньше [`Self::MINIMUM`].
    #[must_use]
    pub fn new(slots: usize) -> Option<Self> {
        if slots < Self::MINIMUM {
            return None;
        }
        NonZeroUsize::new(slots).map(Self)
    }

    /// Число слотов.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0.get()
    }
}

impl Default for UploadSlotCount {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Почему нет слота для записи нового кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BusyReason {
    /// Хотя бы один слот тронут работой, на которую вызывающий ещё не отдал fence
    /// ([`crate::VideoLayerRenderer::retire_submitted_work`]).
    AwaitingSubmittedWorkFence,
    /// Все остальные слоты ещё читает GPU (fence не сработал).
    GpuStillReading,
}

/// Что делает GPU со слотом.
#[derive(Debug)]
enum GpuUse<F> {
    /// Ни одна неоконченная работа слот не трогает.
    Idle,
    /// Слот тронут работой, на которую fence ещё не поставлен.
    InUnretiredWork,
    /// Слот трогала работа до этого fence.
    UntilFence(Arc<F>),
}

/// Слот кольца: набор текстур (создаётся при первой записи) и ключ кадра в нём.
#[derive(Debug)]
pub(crate) struct RingSlot<T, F> {
    /// Текстуры слота; `None` до первой записи.
    pub(crate) textures: Option<T>,
    key: Option<VideoFrameKey>,
    gpu_use: GpuUse<F>,
}

/// Индекс слота, выданный кольцом для записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WritableSlot(usize);

/// Результат поиска слота для записи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SlotForWrite {
    /// Слот свободен.
    Free(WritableSlot),
    /// Свободного слота нет.
    Busy(BusyReason),
}

/// Кольцо слотов.
#[derive(Debug)]
pub(crate) struct UploadRing<T, F> {
    slots: Vec<RingSlot<T, F>>,
    current: Option<usize>,
}

impl<T, F: FenceProbe> UploadRing<T, F> {
    /// Кольцо из `count` пустых слотов.
    pub(crate) fn new(count: UploadSlotCount) -> Self {
        let slots = (0..count.get())
            .map(|_| RingSlot {
                textures: None,
                key: None,
                gpu_use: GpuUse::Idle,
            })
            .collect();
        Self {
            slots,
            current: None,
        }
    }

    /// Ключ кадра на экране.
    pub(crate) fn current_key(&self) -> Option<VideoFrameKey> {
        self.current
            .and_then(|index| self.slots.get(index))
            .and_then(|slot| slot.key)
    }

    /// Текстуры кадра на экране.
    pub(crate) fn current_textures(&self) -> Option<&T> {
        self.current
            .and_then(|index| self.slots.get(index))
            .and_then(|slot| slot.textures.as_ref())
    }

    /// Ищет слот для записи нового кадра, начиная со слота после текущего (по кругу), чтобы
    /// загрузки чередовались, а только что нарисованный слот шёл последним.
    ///
    /// Сработавшие fence-ы снимаются по пути. Ошибка проверки fence — отказ, а не «свободно».
    pub(crate) fn slot_for_write(&mut self) -> Result<SlotForWrite, F::Error> {
        let slot_count = self.slots.len();
        let start = self.current.map_or(0, |current| current + 1);
        let mut awaiting_fence = false;
        for step in 0..slot_count {
            let index = (start + step) % slot_count;
            if Some(index) == self.current {
                continue;
            }
            let Some(slot) = self.slots.get_mut(index) else {
                continue;
            };
            if let GpuUse::UntilFence(fence) = &slot.gpu_use
                && fence.is_signaled()?
            {
                slot.gpu_use = GpuUse::Idle;
            }
            match slot.gpu_use {
                GpuUse::Idle => return Ok(SlotForWrite::Free(WritableSlot(index))),
                GpuUse::InUnretiredWork => awaiting_fence = true,
                GpuUse::UntilFence(_) => {}
            }
        }
        let reason = if awaiting_fence {
            BusyReason::AwaitingSubmittedWorkFence
        } else {
            BusyReason::GpuStillReading
        };
        Ok(SlotForWrite::Busy(reason))
    }

    /// Слот, выданный [`Self::slot_for_write`].
    pub(crate) fn slot_mut(&mut self, slot: WritableSlot) -> Option<&mut RingSlot<T, F>> {
        self.slots.get_mut(slot.0)
    }

    /// Запись удалась: слот становится текущим кадром.
    pub(crate) fn commit_upload(&mut self, slot: WritableSlot, key: VideoFrameKey) {
        if let Some(target) = self.slots.get_mut(slot.0) {
            target.key = Some(key);
            target.gpu_use = GpuUse::InUnretiredWork;
            self.current = Some(slot.0);
        }
    }

    /// Запись оборвалась на середине: в слоте мусор, часть команд записи уже у драйвера.
    /// Текущий кадр не меняется.
    pub(crate) fn abandon_upload(&mut self, slot: WritableSlot) {
        if let Some(target) = self.slots.get_mut(slot.0) {
            target.key = None;
            target.gpu_use = GpuUse::InUnretiredWork;
        }
    }

    /// Текущий кадр нарисован: слот в неотданной работе до следующего `retire`.
    pub(crate) fn mark_current_drawn(&mut self) {
        if let Some(slot) = self.current.and_then(|index| self.slots.get_mut(index)) {
            slot.gpu_use = GpuUse::InUnretiredWork;
        }
    }

    /// Вызывающий поставил fence после работы: все слоты «в неотданной работе» ждут его.
    pub(crate) fn retire(&mut self, fence: &Arc<F>) {
        for slot in &mut self.slots {
            if matches!(slot.gpu_use, GpuUse::InUnretiredWork) {
                slot.gpu_use = GpuUse::UntilFence(Arc::clone(fence));
            }
        }
    }

    /// Забыть, какие кадры в слотах (ключи вызывающего начались заново). Текстуры и ожидание
    /// GPU остаются: забыть содержимое не значит, что GPU перестал его читать.
    pub(crate) fn forget_keys(&mut self) {
        self.current = None;
        for slot in &mut self.slots {
            slot.key = None;
        }
    }

    /// Кадр кольца больше не показывается (текущим стал импортированный кадр): слот перестаёт
    /// быть «текущим» и не защищён от записи этим званием. Защита от чтения GPU остаётся: слот,
    /// который GPU ещё читает, держит `gpu_use`, а не «текущий».
    pub(crate) fn clear_current(&mut self) {
        if let Some(slot) = self.current.and_then(|index| self.slots.get_mut(index)) {
            slot.key = None;
        }
        self.current = None;
    }

    /// Все текстуры кольца (для удаления вместе с рендером).
    pub(crate) fn take_all_textures(&mut self) -> Vec<T> {
        self.current = None;
        self.slots
            .iter_mut()
            .filter_map(|slot| {
                slot.key = None;
                slot.textures.take()
            })
            .collect()
    }

    /// Сколько слотов уже держат текстуры.
    pub(crate) fn allocated_slots(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.textures.is_some())
            .count()
    }
}

#[cfg(test)]
mod tests;

//! Билет на буфер swapchain (инвариант сессии 06a cpu-parity: «без полученного буфера кадр
//! не рисуется»).
//!
//! # Почему билет, а не буфер
//!
//! Приложение не должно видеть GPU-типы: у wgpu буфер — `SurfaceTexture`, у EGL — «задний
//! буфер окна», у DX — back buffer swap chain-а. Поэтому рендер кладёт свой настоящий буфер
//! в «коробку» билета, а приложение держит только билет:
//! - билет обязателен во входе кадра (`RenderFrameInput::surface`), то есть `render_frame`
//!   без полученного буфера не вызвать;
//! - открыть коробку может только рендер, знающий тип содержимого и свой паспорт
//!   ([`AcquiredSurfaceFrame::into_buffer`]); чужой билет — типизированный отказ;
//! - неиспользованный билет при drop-е роняет буфер внутри коробки, и рендер возвращает его
//!   swapchain-у (у wgpu это discard в `SurfaceTexture::drop`).
//!
//! Цена — одна маленькая аллокация коробки на кадр.

use std::any::Any;
use std::fmt;
use std::time::Duration;

use crate::{RenderFrameDropReason, RendererInstanceId};

/// Буфер swapchain, полученный рендером под текущий кадр.
///
/// Приложение получает его от `RenderShell::acquire_surface_frame` и передаёт в
/// `render_frame`. Открыть билет может только выдавший его рендер.
#[must_use = "неиспользованный билет возвращает буфер swapchain без показа кадра"]
pub struct AcquiredSurfaceFrame {
    /// Паспорт рендера, который получил буфер.
    owner: RendererInstanceId,

    /// Настоящий буфер рендера; тип знает только сам рендер.
    buffer: Box<dyn Any>,

    /// Сколько главный поток ждал буфер (ожидание FIFO входит); идёт в стадию `surface_acquire`.
    acquire_elapsed: Duration,
}

impl AcquiredSurfaceFrame {
    /// Кладёт настоящий буфер рендера в билет. Вызывает только конкретный рендер.
    pub fn new<Buffer: Any>(
        owner: RendererInstanceId,
        buffer: Buffer,
        acquire_elapsed: Duration,
    ) -> Self {
        Self {
            owner,
            buffer: Box::new(buffer),
            acquire_elapsed,
        }
    }

    /// Билет, который не принадлежит ни одному рендеру (проверка «чужой билет не рисуется»).
    #[cfg(any(test, feature = "conformance"))]
    pub(crate) fn unowned_for_conformance() -> Self {
        /// Тип, которого не знает ни один рендер.
        struct ConformanceForeignBuffer;

        Self::new(
            RendererInstanceId::unowned(),
            ConformanceForeignBuffer,
            Duration::ZERO,
        )
    }

    /// Сколько главный поток ждал буфер swapchain.
    #[must_use]
    pub const fn acquire_elapsed(&self) -> Duration {
        self.acquire_elapsed
    }

    /// Каким рендером выдан билет.
    #[must_use]
    pub const fn owner(&self) -> RendererInstanceId {
        self.owner
    }

    /// Открывает билет: рендер забирает свой буфер обратно.
    ///
    /// Отказ, если билет выдан другим экземпляром рендера или внутри лежит буфер другого
    /// типа. При отказе содержимое билета роняется здесь же: чужой буфер возвращается своему
    /// swapchain-у его собственным drop-ом, и этот рендер его не трогает.
    pub fn into_buffer<Buffer: Any>(
        self,
        expected_owner: RendererInstanceId,
    ) -> Result<OpenedSurfaceFrame<Buffer>, ForeignSurfaceFrame> {
        if self.owner != expected_owner {
            return Err(ForeignSurfaceFrame {
                ticket_owner: self.owner,
                expected_owner,
            });
        }
        let acquire_elapsed = self.acquire_elapsed;
        match self.buffer.downcast::<Buffer>() {
            Ok(buffer) => Ok(OpenedSurfaceFrame {
                buffer: *buffer,
                acquire_elapsed,
            }),
            Err(_foreign_buffer) => Err(ForeignSurfaceFrame {
                ticket_owner: self.owner,
                expected_owner,
            }),
        }
    }
}

impl fmt::Debug for AcquiredSurfaceFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Содержимое коробки — GPU-объект рендера; печатаем только нейтральные поля.
        formatter
            .debug_struct("AcquiredSurfaceFrame")
            .field("owner", &self.owner)
            .field("acquire_elapsed", &self.acquire_elapsed)
            .finish_non_exhaustive()
    }
}

/// Буфер, который рендер достал из своего билета.
#[derive(Debug)]
pub struct OpenedSurfaceFrame<Buffer> {
    /// Настоящий буфер рендера.
    pub buffer: Buffer,

    /// Ожидание получения буфера (стадия `surface_acquire`).
    pub acquire_elapsed: Duration,
}

/// Билет выдан не этим рендером (или внутри не тот тип буфера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForeignSurfaceFrame {
    /// Чей паспорт стоит на билете.
    pub ticket_owner: RendererInstanceId,

    /// Паспорт рендера, который пытался открыть билет.
    pub expected_owner: RendererInstanceId,
}

/// Итог получения буфера swapchain.
#[must_use = "без буфера кадр не показан; причину пропуска обязан учесть app"]
#[derive(Debug)]
pub enum SurfaceFrameAcquire {
    /// Буфер получен: можно выбирать видеокадр и вызывать `render_frame`.
    Acquired(AcquiredSurfaceFrame),

    /// Буфер не получен (окно скрыто, surface потерян/устарел, таймаут): кадр пропущен
    /// до выбора видеокадра, кадр плеера остаётся в передаче для следующего показа.
    Dropped(RenderFrameDropReason),
}

#[cfg(test)]
#[path = "tests/surface_ticket.rs"]
mod tests;

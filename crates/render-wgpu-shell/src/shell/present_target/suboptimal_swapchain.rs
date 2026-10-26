//! Пересоздание swapchain-а, который драйвер назвал «неоптимальным» (`Suboptimal`).
//!
//! Зачем это нужно (корневая причина дорогого композитора у wgpu, 2026-10-26):
//! - KWin в fullscreen присылает окну новый dmabuf feedback с «scanout»-траншем — список
//!   модификаторов буфера, который дисплейный контроллер умеет показывать напрямую, без
//!   композиции (direct scanout).
//! - Mesa Vulkan WSI (Wayland) сравнивает этот список с модификаторами, на которых выделен
//!   текущий swapchain, и при различии лишь помечает его `suboptimal`: каждый
//!   `vkAcquireNextImageKHR` возвращает `VK_SUBOPTIMAL_KHR`. Сам он буферы НЕ пересоздаёт —
//!   это обязанность приложения (`vkCreateSwapchainKHR` заново, у wgpu — `Surface::configure`).
//! - Раньше мы считали `Suboptimal` успехом и никогда не пересоздавали swapchain. Буферы так и
//!   оставались на «рендерном» модификаторе (AMD: DCC без displayable-плоскости), KWin не мог
//!   показать их напрямую и композировал каждый кадр. EGL (рендер GLES2) пересоздаёт буферы
//!   сам, поэтому у него direct scanout был, а у wgpu — нет.
//!
//! Правило здесь: увидели `Suboptimal` — кадр всё равно рисуем и показываем (буфер рабочий),
//! а перед получением СЛЕДУЮЩЕГО буфера один раз пересоздаём swapchain той же конфигурацией.
//! Пересоздание идёт в начале acquire, когда живого `SurfaceTexture` нет: это требование wgpu
//! к `Surface::configure`.
//!
//! Защита от зацикливания: если сразу после пересоздания первый же буфер снова `Suboptimal`
//! (драйвер не может выдать «оптимальный» swapchain), это «бесплодная» попытка. После
//! [`MAX_FRUITLESS_REBUILDS`] таких попыток подряд пересоздание прекращается до следующей
//! перенастройки surface по другой причине (resize, смена present mode, возврат окна):
//! пересоздавать swapchain каждый кадр дороже, чем жить с неоптимальным.
//!
//! Модуль не знает wgpu: политика — чистое состояние, а surface скрыта за [`SwapchainPort`].
//! Так оркестрацию можно проверить тестами с поддельным swapchain-ом, ведущим себя как Mesa.

use tracing::{debug, info, warn};

/// Сколько бесплодных пересозданий подряд допускается, прежде чем перестать пробовать.
///
/// Это не пользовательская настройка, а предохранитель от цикла «пересоздали → снова
/// неоптимально»: три попытки покрывают гонку с поздним feedback-ом композитора, но не дают
/// пересоздавать swapchain каждый кадр.
pub(super) const MAX_FRUITLESS_REBUILDS: u8 = 3;

/// Насколько полученный буфер подходит surface (перевод `Success`/`Suboptimal` из wgpu).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcquiredSwapchainFit {
    /// Swapchain соответствует surface.
    Optimal,

    /// Буфер рабочий, но swapchain стоит пересоздать (у Mesa — новые модификаторы от композитора).
    Suboptimal,
}

/// Нужно ли пересоздать swapchain перед получением следующего буфера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SwapchainRebuild {
    /// Swapchain в порядке (или пересоздание отключено предохранителем).
    NotNeeded,

    /// Прошлый буфер был неоптимальным: пересоздать сейчас.
    Due,
}

/// Что политика решила после очередного полученного буфера (для логов и тестов).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SuboptimalVerdict {
    /// Буфер оптимален, делать ничего не нужно.
    Optimal,

    /// Буфер неоптимален: перед следующим acquire swapchain будет пересоздан.
    RebuildScheduled,

    /// Пересоздания подряд не помогли — предохранитель сработал только что.
    RebuildsExhausted,

    /// Предохранитель уже сработал раньше: неоптимальный буфер принимается как есть.
    AcceptedAfterExhaustion,
}

/// Чем закончилось получение буфера через [`SuboptimalSwapchainRecovery::acquire`].
#[derive(Debug)]
pub(super) enum SwapchainAcquireFailure<AcquireError> {
    /// Surface не приняла пересоздание swapchain-а прежней конфигурацией.
    RebuildRejected {
        /// Текст ошибок устройства, перехваченных при `configure`.
        message: String,
    },

    /// Пересоздание (если было) прошло, но буфер получить не удалось.
    Acquire(AcquireError),
}

/// Доступ политики к swapchain-у окна (у production — wgpu surface, в тестах — подделка).
pub(super) trait SwapchainPort {
    /// Полученный буфер кадра.
    type Frame;

    /// Причина, по которой буфер не выдан (у wgpu — `RenderFrameDropReason`).
    type AcquireError;

    /// Пересоздаёт swapchain текущей конфигурацией surface.
    fn rebuild_swapchain(&mut self) -> Result<(), String>;

    /// Получает буфер и сообщает, насколько swapchain подходит surface.
    fn acquire_image(&mut self) -> Result<(Self::Frame, AcquiredSwapchainFit), Self::AcquireError>;
}

/// Где политика находится между кадрами.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryPhase {
    /// Последний буфер был оптимальным (или пересоздание ещё не требовалось).
    Settled,

    /// Последний буфер неоптимален: перед следующим acquire пересоздать swapchain.
    RebuildPending,

    /// Swapchain только что пересоздан; первый буфер покажет, помогло ли это.
    AwaitingFirstImageAfterRebuild,
}

/// Состояние «пересоздать неоптимальный swapchain» одной surface окна.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SuboptimalSwapchainRecovery {
    /// Фаза между кадрами.
    phase: RecoveryPhase,

    /// Сколько пересозданий подряд не дали оптимального swapchain-а.
    fruitless_rebuilds: u8,

    /// Предохранитель сработал: неоптимальность принимается до перенастройки surface извне.
    exhausted: bool,
}

impl SuboptimalSwapchainRecovery {
    /// Состояние только что настроенной surface.
    pub(super) const fn new() -> Self {
        Self {
            phase: RecoveryPhase::Settled,
            fruitless_rebuilds: 0,
            exhausted: false,
        }
    }

    /// Нужно ли пересоздать swapchain сейчас; ответ `Due` выдаётся один раз на неоптимальный кадр.
    pub(super) fn take_rebuild_before_acquire(&mut self) -> SwapchainRebuild {
        if self.phase != RecoveryPhase::RebuildPending {
            return SwapchainRebuild::NotNeeded;
        }
        self.phase = RecoveryPhase::AwaitingFirstImageAfterRebuild;
        SwapchainRebuild::Due
    }

    /// Учитывает пригодность только что полученного буфера.
    pub(super) fn record_acquired_image(&mut self, fit: AcquiredSwapchainFit) -> SuboptimalVerdict {
        let first_image_after_rebuild = self.phase == RecoveryPhase::AwaitingFirstImageAfterRebuild;
        match fit {
            AcquiredSwapchainFit::Optimal => {
                // Swapchain подошёл: счётчик бесплодных попыток больше не нужен.
                self.phase = RecoveryPhase::Settled;
                self.fruitless_rebuilds = 0;
                SuboptimalVerdict::Optimal
            }
            AcquiredSwapchainFit::Suboptimal if self.exhausted => {
                self.phase = RecoveryPhase::Settled;
                SuboptimalVerdict::AcceptedAfterExhaustion
            }
            AcquiredSwapchainFit::Suboptimal => {
                if first_image_after_rebuild {
                    self.fruitless_rebuilds = self.fruitless_rebuilds.saturating_add(1);
                }
                if self.fruitless_rebuilds >= MAX_FRUITLESS_REBUILDS {
                    self.exhausted = true;
                    self.phase = RecoveryPhase::Settled;
                    return SuboptimalVerdict::RebuildsExhausted;
                }
                self.phase = RecoveryPhase::RebuildPending;
                SuboptimalVerdict::RebuildScheduled
            }
        }
    }

    /// Surface только что перенастроена по другой причине (resize, present mode, возврат окна).
    ///
    /// Такая перенастройка сама пересоздаёт swapchain по свежему feedback-у, поэтому ожидающее
    /// пересоздание лишнее, а предохранитель снимается: условия на surface изменились.
    pub(super) fn surface_reconfigured(&mut self) {
        *self = Self::new();
    }

    /// Получает буфер через `port`, при необходимости сначала пересоздав swapchain.
    pub(super) fn acquire<Port: SwapchainPort>(
        &mut self,
        port: &mut Port,
    ) -> Result<Port::Frame, SwapchainAcquireFailure<Port::AcquireError>> {
        if self.take_rebuild_before_acquire() == SwapchainRebuild::Due {
            if let Err(message) = port.rebuild_swapchain() {
                // Surface без рабочей конфигурации: её восстановление — забота вызывающего,
                // а политика начинает с чистого листа после следующей настройки.
                self.surface_reconfigured();
                return Err(SwapchainAcquireFailure::RebuildRejected { message });
            }
            info!("Swapchain rebuilt after a suboptimal acquire");
        }
        let (frame, fit) = port
            .acquire_image()
            .map_err(SwapchainAcquireFailure::Acquire)?;
        log_verdict(self.record_acquired_image(fit));
        Ok(frame)
    }
}

/// Пишет в лог только смену решения, а не каждый кадр.
fn log_verdict(verdict: SuboptimalVerdict) {
    match verdict {
        SuboptimalVerdict::Optimal | SuboptimalVerdict::AcceptedAfterExhaustion => {}
        SuboptimalVerdict::RebuildScheduled => {
            debug!("Swapchain is suboptimal for the surface; rebuilding it before the next frame");
        }
        SuboptimalVerdict::RebuildsExhausted => {
            warn!(
                attempts = MAX_FRUITLESS_REBUILDS,
                "Swapchain stays suboptimal after rebuilding; keeping it until the surface is reconfigured"
            );
        }
    }
}

#[cfg(test)]
mod tests;

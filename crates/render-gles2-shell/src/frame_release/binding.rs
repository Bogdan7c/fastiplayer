//! Привязка освобождения кадров к рендеру GLES2.
//!
//! Привязку держит поток рендера (публикует fence кадра показа и сообщает о потере контекста),
//! а её клоны живут в обёртках провайдера и декодера (запрашивают освобождение). Подготовщик
//! импорта работает в потоке плеера (решение 20 README), поэтому привязка НЕ создаёт GL/EGL
//! объектов: только хранит `Arc<GpuFence>`, которые ей публикует поток рендера.

use std::sync::Arc;
use std::sync::atomic::AtomicU64;

use gles2_egui::{FenceStatus, GpuFence};
use video_backend_api::{
    PresentFrameResourceProviderHandle, StartedVideoBackend, VideoBackendLifetimeGuard,
    share_video_backend_decoder_thread,
};
use video_core::FrameResourceHandle;

use super::decoder::Gles2ReleaseVideoDecoderThreadHandle;
use super::fence::PresentedFrameFence;
use super::provider::Gles2FenceReleaseProvider;
use super::shared::{BindingMode, PendingRelease, ReleaseCore, WaiterLifecycle, WaitingRelease};
use super::stats::FrameReleaseStats;
use super::waiter::spawn_waiter;

/// Как кадр отпускается сразу, минуя поток-ожидатель.
#[derive(Debug, Clone, Copy)]
enum ImmediateReason {
    /// Fence ещё не публиковали: кадр не рисовался.
    NoFencePublished,
    /// Последний fence уже сработал.
    FenceAlreadySignaled,
    /// Контекст потерян.
    ContextLost,
    /// Рендер отвязан от привязки (штатное уничтожение).
    RendererDetached,
    /// Проверка fence отказала.
    FenceCheckFailed,
    /// Поток-ожидатель не запустился.
    WaiterUnavailable,
}

/// Что делать с запросом освобождения после осмотра состояния.
enum ReleaseDecision {
    Immediate(ImmediateReason),
    /// Fence ещё не сработал: нужен поток-ожидатель.
    WaitForFence(Arc<dyn PresentedFrameFence>),
}

/// Владелец общего ядра: когда уничтожен последний клон привязки, закрывает ядро, и поток
/// ожидатель доделывает очередь и выходит.
struct BindingOwner {
    core: Arc<ReleaseCore>,
}

impl Drop for BindingOwner {
    fn drop(&mut self) {
        let last_fence = {
            let mut state = self.core.lock_state();
            state.closed = true;
            state.last_fence.take()
        };
        self.core.wakeup.notify_all();
        // Страховка на случай, если рендер не вызвал `renderer_detaching`: последний fence
        // умирает здесь, в потоке, уничтожающем последний клон привязки, а не в
        // отсоединённом потоке-ожидателе, который мог бы пережить `main`.
        drop(last_fence);
    }
}

/// Привязка «кадр декодера ↔ fence кадра показа». Клонируется дёшево, `Send + Sync`.
#[derive(Clone)]
pub struct Gles2FrameReleaseBinding {
    owner: Arc<BindingOwner>,
}

impl Default for Gles2FrameReleaseBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl Gles2FrameReleaseBinding {
    /// Создаёт привязку без потока: он стартует при первом кадре, который надо ждать.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: Arc::new(BindingOwner {
                core: Arc::new(ReleaseCore::new()),
            }),
        }
    }

    /// Та же ли это привязка (общее состояние), а не просто такая же по типу: клоны одной
    /// привязки равны, независимо созданные — нет.
    #[must_use]
    pub fn is_same_binding(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.owner, &other.owner)
    }

    pub(super) fn core(&self) -> &Arc<ReleaseCore> {
        &self.owner.core
    }

    /// Публикует fence только что показанного кадра. Зовёт поток рендера после каждого кадра
    /// показа (тот же `Arc`, что хранит `VideoLayerRenderer::retire_submitted_work`).
    ///
    /// Последний fence покрывает всю прежнюю работу GPU (команды контекста идут по порядку).
    /// После потери контекста публикации игнорируются до
    /// [`Self::resume_after_context_recreated`].
    pub fn publish_presented_frame_fence(&self, fence: Arc<GpuFence>) {
        self.publish_fence(fence);
    }

    /// Общая часть публикации: принимает любой [`PresentedFrameFence`] (тесты подставляют fake).
    pub(super) fn publish_fence(&self, fence: Arc<dyn PresentedFrameFence>) {
        let mut state = self.core().lock_state();
        if state.mode == BindingMode::Active {
            state.last_fence = Some(fence);
        }
    }

    /// Контекст потерян: отпускает ВСЕ ожидающие кадры немедленно (GPU их уже не дочитает) и
    /// переводит привязку в режим «потерян» — дальнейшие запросы отпускаются сразу.
    ///
    /// Возвращает, сколько кадров отпущено этим вызовом; повторный вызов вернёт 0.
    /// Кадр, который параллельно отпустил поток-ожидатель, не считается и не отпускается снова.
    pub fn release_after_context_lost(&self) -> usize {
        let core = self.core();
        let waiting = {
            let mut state = core.lock_state();
            state.mode = BindingMode::ContextLost;
            state.last_fence = None;
            std::mem::take(&mut state.waiting)
        };
        let loss_counter = &core.counters.released_after_context_loss;
        waiting
            .into_iter()
            .filter(|entry| entry.release.release_once(loss_counter))
            .count()
    }

    /// Рендер пересоздан на новом контексте: привязка снова ждёт fence. Старый fence забыт
    /// (он принадлежал мёртвому контексту). Работает и после [`Self::renderer_detaching`]:
    /// привязка снова принимает fence нового рендера.
    pub fn resume_after_context_recreated(&self) {
        let mut state = self.core().lock_state();
        state.mode = BindingMode::Active;
        state.last_fence = None;
    }

    /// Снимок статистики освобождения.
    #[must_use]
    pub fn stats(&self) -> FrameReleaseStats {
        self.core().stats()
    }

    /// Запрос освобождения кадра: отпустить сейчас или после fence. Зовёт обёртка провайдера.
    pub(super) fn schedule_release(
        &self,
        inner_provider: PresentFrameResourceProviderHandle,
        handle: FrameResourceHandle,
        backend_lifetime: VideoBackendLifetimeGuard,
    ) {
        let core = self.core();
        let release = Arc::new(PendingRelease::new(
            inner_provider,
            handle,
            backend_lifetime,
        ));

        // Осмотр состояния без удержания блокировки на вызове EGL.
        match self.inspect_state() {
            ReleaseDecision::Immediate(reason) => release_immediately(core, &release, reason),
            ReleaseDecision::WaitForFence(fence) => {
                if let Err(reason) = self.enqueue_for_waiter(&release, fence) {
                    release_immediately(core, &release, reason);
                }
            }
        }
    }

    /// Решает по последнему fence: сразу или ждать. `poll` — неблокирующий вызов EGL.
    fn inspect_state(&self) -> ReleaseDecision {
        let last_fence = {
            let state = self.core().lock_state();
            match state.mode {
                BindingMode::Active => {}
                BindingMode::ContextLost => {
                    return ReleaseDecision::Immediate(ImmediateReason::ContextLost);
                }
                BindingMode::RendererDetached => {
                    return ReleaseDecision::Immediate(ImmediateReason::RendererDetached);
                }
            }
            match &state.last_fence {
                Some(fence) => Arc::clone(fence),
                None => return ReleaseDecision::Immediate(ImmediateReason::NoFencePublished),
            }
        };
        match last_fence.poll() {
            Ok(FenceStatus::Signaled) => {
                ReleaseDecision::Immediate(ImmediateReason::FenceAlreadySignaled)
            }
            Ok(FenceStatus::Pending) => ReleaseDecision::WaitForFence(last_fence),
            Err(error) => {
                log::warn!(
                    "проверка fence кадра показа GLES2 отказала ({error}); кадр возвращён сразу"
                );
                ReleaseDecision::Immediate(ImmediateReason::FenceCheckFailed)
            }
        }
    }

    /// Ставит запись в очередь, при необходимости запуская поток. `Err` — отпустить сразу.
    fn enqueue_for_waiter(
        &self,
        release: &Arc<PendingRelease>,
        fence: Arc<dyn PresentedFrameFence>,
    ) -> Result<(), ImmediateReason> {
        let core = self.core();
        let mut state = core.lock_state();
        // Потеря контекста или отвязка рендера могли случиться между осмотром и постановкой
        // в очередь.
        match state.mode {
            BindingMode::Active => {}
            BindingMode::ContextLost => return Err(ImmediateReason::ContextLost),
            BindingMode::RendererDetached => return Err(ImmediateReason::RendererDetached),
        }
        if state.waiter == WaiterLifecycle::Exited {
            // Недостижимо при корректном владении (поток выходит лишь после закрытия привязки,
            // а запрос идёт через её клон), но зависшая запись хуже честного немедленного возврата.
            return Err(ImmediateReason::WaiterUnavailable);
        }
        if state.waiter == WaiterLifecycle::NotStarted {
            if let Err(error) = spawn_waiter(Arc::clone(core)) {
                log::error!("не удалось запустить поток освобождения кадров GLES2: {error}");
                return Err(ImmediateReason::WaiterUnavailable);
            }
            state.waiter = WaiterLifecycle::Running;
        }
        let id = state.next_waiting_id;
        state.next_waiting_id += 1;
        state.waiting.push_back(WaitingRelease {
            id,
            release: Arc::clone(release),
            fence,
        });
        core.wakeup.notify_all();
        Ok(())
    }
}

/// Отпускает кадр в вызывающем потоке и учитывает причину.
fn release_immediately(core: &ReleaseCore, release: &PendingRelease, reason: ImmediateReason) {
    let counter: &AtomicU64 = match reason {
        ImmediateReason::NoFencePublished => &core.counters.released_without_fence,
        ImmediateReason::FenceAlreadySignaled => &core.counters.released_after_fence_signaled_fast,
        ImmediateReason::ContextLost => &core.counters.released_after_context_loss,
        ImmediateReason::RendererDetached => &core.counters.released_after_renderer_detach,
        ImmediateReason::FenceCheckFailed => &core.counters.fence_wait_failures,
        ImmediateReason::WaiterUnavailable => &core.counters.waiter_spawn_failures,
    };
    release.release_once(counter);
}

/// Оборачивает started backend так, чтобы кадр возвращался декодеру только после fence показа.
///
/// Внутренний провайдер остаётся владельцем ресурса; обёртка провайдера форвардит lookup-ы
/// как есть, а `release_frame` отдаёт в `binding`. Обёртка декодера форвардит все методы
/// декодера, кроме `resource_provider` (отдаёт обёрнутый провайдер). `release_frame` декодера
/// идёт напрямую внутрь: так возвращается кадр, который не дошёл до рендера.
#[must_use]
pub fn wrap_video_backend_for_gles2_release(
    started_backend: StartedVideoBackend,
    binding: &Gles2FrameReleaseBinding,
) -> (StartedVideoBackend, PresentFrameResourceProviderHandle) {
    let backend_id = started_backend.backend_id().to_owned();
    let decoder_thread = started_backend.into_decoder_thread();
    let inner_provider = decoder_thread.resource_provider();
    let (decoder_thread, backend_lifetime) = share_video_backend_decoder_thread(decoder_thread);
    let renderer_provider = PresentFrameResourceProviderHandle::new(
        Gles2FenceReleaseProvider::new(inner_provider, binding.clone(), backend_lifetime),
    );
    let wrapped_thread =
        Gles2ReleaseVideoDecoderThreadHandle::new(decoder_thread, renderer_provider.clone());
    let wrapped_backend = StartedVideoBackend::from_decoder_thread(backend_id, wrapped_thread);
    (wrapped_backend, renderer_provider)
}

#[cfg(test)]
impl Gles2FrameReleaseBinding {
    /// Общее ядро для тестов: наблюдать выход потока после уничтожения всех клонов привязки.
    pub(super) fn core_for_tests(&self) -> Arc<ReleaseCore> {
        Arc::clone(self.core())
    }
}

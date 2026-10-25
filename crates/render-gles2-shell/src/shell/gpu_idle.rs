//! Ожидание завершения работы GPU (`RenderShell::wait_for_gpu_idle`) и потеря устройства.
//!
//! Как у wgpu (`device.poll(Wait)` + колбэки завершения): после ожидания GPU закончил всё
//! отправленное, а кадры, которые он дочитал и которые приложение уже отпустило, вернулись
//! декодеру. Потеря контекста GLES2 — это «устройство потеряно» контракта: приложение по ней
//! освобождает кадры принудительно и откатывается.

use std::time::{Duration, Instant};

use gles2_egui::{
    ContextLoss, ContextLossReason, FenceStatus, FenceWaitError, Gles2Context, Gles2Error,
};
use render_shell_api::{RendererDeviceLossReason, RendererDeviceLost, RendererGpuDrainError};

use crate::frame_release::{Gles2FrameReleaseBinding, PendingReleaseDrain, PresentedWorkWait};

/// Потеря контекста в нейтральной форме контракта.
#[must_use]
pub(super) fn device_lost_from(loss: ContextLoss) -> RendererDeviceLost {
    RendererDeviceLost {
        reason: match loss.reason {
            // Сброс GPU / событие питания / зависание: драйвер сам забрал устройство.
            ContextLossReason::GuiltyReset
            | ContextLossReason::InnocentReset
            | ContextLossReason::UnknownReset
            | ContextLossReason::EglContextLost => RendererDeviceLossReason::Unknown,
        },
        message: loss.to_string(),
    }
}

/// Ждёт GPU и возврата отпущенных кадров, не дольше `timeout` на всё вместе.
///
/// `window_attached` — держит ли рендер окно. Без окна текущего GL-контекста может не быть
/// (нет surfaceless-контекста), тогда новый fence поставить нечем и ждётся fence последнего
/// показанного кадра.
pub(super) fn wait_for_gpu_idle(
    context: &mut Gles2Context,
    release: &Gles2FrameReleaseBinding,
    window_attached: bool,
    timeout: Duration,
) -> Result<(), RendererGpuDrainError> {
    let started_at = Instant::now();
    if let Some(loss) = context.context_loss() {
        return Err(RendererGpuDrainError::DeviceLost(device_lost_from(loss)));
    }
    if window_attached {
        // Свежий fence покрывает ВСЮ отправленную работу контекста, в том числе загрузки кадров
        // между кадрами показа, на которые fence ещё не ставился.
        let fence = context.insert_fence().map_err(drain_error_for_context)?;
        match fence.wait(timeout) {
            Ok(FenceStatus::Signaled) => {}
            Ok(FenceStatus::Pending) => return Err(RendererGpuDrainError::Timeout),
            Err(error) => return Err(drain_error_for_wait(error)),
        }
        // Fence — последний держатель EGLSync этого вызова: уничтожаем его здесь, в потоке рендера.
        drop(fence);
    } else {
        presented_work_result(release.wait_for_last_presented_fence(timeout))?;
    }
    drain_returned_frames(release, timeout.saturating_sub(started_at.elapsed()))
}

/// Итог ожидания последнего показанного кадра → результат ожидания GPU.
fn presented_work_result(wait: PresentedWorkWait) -> Result<(), RendererGpuDrainError> {
    match wait {
        PresentedWorkWait::Done | PresentedWorkWait::NothingPublished => Ok(()),
        PresentedWorkWait::Pending => Err(RendererGpuDrainError::Timeout),
        PresentedWorkWait::Failed(error) => Err(drain_error_for_wait(error)),
    }
}

/// Отказ ожидания fence → ошибка ожидания контракта.
fn drain_error_for_wait(error: FenceWaitError) -> RendererGpuDrainError {
    match error {
        FenceWaitError::ContextLost(loss) => {
            RendererGpuDrainError::DeviceLost(device_lost_from(loss))
        }
        FenceWaitError::Driver(error) => {
            log::warn!("GLES2: драйвер отклонил ожидание fence: {error}");
            RendererGpuDrainError::WaitRejected
        }
    }
}

/// Ждёт, пока кадры, которые приложение отпустило и GPU дочитал, вернутся декодеру.
fn drain_returned_frames(
    release: &Gles2FrameReleaseBinding,
    timeout: Duration,
) -> Result<(), RendererGpuDrainError> {
    match release.wait_for_pending_releases(timeout) {
        PendingReleaseDrain::Drained => Ok(()),
        PendingReleaseDrain::TimedOut { still_pending } => {
            log::warn!("GLES2: {still_pending} кадров декодера ещё ждут возврата после fence");
            Err(RendererGpuDrainError::Timeout)
        }
    }
}

/// Ошибка постановки fence → ошибка ожидания контракта.
fn drain_error_for_context(error: Gles2Error) -> RendererGpuDrainError {
    match error {
        Gles2Error::ContextLost(loss) => RendererGpuDrainError::DeviceLost(device_lost_from(loss)),
        other => {
            log::warn!("GLES2: fence для ожидания GPU не поставлен: {other}");
            RendererGpuDrainError::WaitRejected
        }
    }
}

#[cfg(test)]
mod tests;

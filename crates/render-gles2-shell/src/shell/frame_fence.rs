//! Fence работы кадра: единственное место, где рендер прикрывает отправленную GPU работу.
//!
//! Инвариант плана: кадр декодера освобождается только после того, как GPU закончил его читать.
//! Привязка освобождения смотрит на ПОСЛЕДНИЙ опубликованный fence, поэтому любая отправленная
//! работа с видео обязана закончиться опубликованным fence, а не только успешно показанный кадр.
//! Иначе кадр, который начал рисоваться и вернул ошибку, оставил бы в привязке старый
//! (давно сработавший) fence, и кадр декодера вернулся бы декодеру при живом чтении GPU.

use std::sync::Arc;

use gles2_egui::{Gles2Error, UiFrameError};

use super::Gles2Renderer;

/// Был ли в кадре видеослой (только такая работа читает кадры декодера).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrameVideoUse {
    /// Кадр рисовал (или собирался рисовать) картинку декодера.
    WithVideo,
    /// Видео в кадре нет: читать кадры декодера нечему.
    WithoutVideo,
}

/// Чем прикрыта работа кадра.
#[derive(Debug)]
pub(super) enum FrameWorkRetirement {
    /// Fence поставлен и опубликован (видеослой и привязка освобождения).
    FencePublished,
    /// Fence поставить не удалось; `glFinish` дождался GPU (старый fence теперь тоже безопасен).
    FinishedSynchronously,
    /// Контекст потерян: кадры декодера освободит `release_after_device_lost`.
    ContextLost,
    /// Не вышло ни fence, ни `glFinish`; контекст не потерян. Причина — fence.
    Unprotected(Gles2Error),
}

impl Gles2Renderer {
    /// Прикрывает всю отправленную работу контекста fence-ом: ставит один fence и отдаёт его и
    /// видеослою, и привязке освобождения (один `Arc` — кадр декодера вернётся по тому же fence,
    /// что освобождает слоты кольца). Без fence — `glFinish`; исход возвращается явно.
    pub(super) fn retire_submitted_frame_work(&mut self) -> FrameWorkRetirement {
        match self.context.insert_fence() {
            Ok(fence) => {
                let fence = Arc::new(fence);
                if let Some(layers) = self.layers.as_mut() {
                    layers.video.retire_submitted_work(Arc::clone(&fence));
                }
                self.release.binding().publish_presented_frame_fence(fence);
                FrameWorkRetirement::FencePublished
            }
            Err(Gles2Error::ContextLost(_)) => FrameWorkRetirement::ContextLost,
            Err(fence_error) => {
                log::error!("GLES2: fence работы кадра не поставлен ({fence_error}); glFinish");
                match self.context.finish_submitted_commands() {
                    Ok(()) => {
                        self.counters.frame_fence_fallback_finishes += 1;
                        FrameWorkRetirement::FinishedSynchronously
                    }
                    Err(Gles2Error::ContextLost(_)) => FrameWorkRetirement::ContextLost,
                    Err(finish_error) => {
                        log::error!(
                            "GLES2: glFinish после отказа fence тоже не вышел: {finish_error}"
                        );
                        self.counters.frame_fence_failures += 1;
                        FrameWorkRetirement::Unprotected(fence_error)
                    }
                }
            }
        }
    }

    /// Кадр вернул ошибку, не показавшись: GPU мог уже получить команды с видео, поэтому работу
    /// прикрываем fence так же, как у показанного кадра. Потеря контекста — прежний путь
    /// (`release_after_device_lost`), fence не ставится.
    pub(super) fn retire_work_of_failed_frame(
        &mut self,
        error: &UiFrameError,
        video: FrameVideoUse,
    ) {
        if video == FrameVideoUse::WithoutVideo
            || matches!(error, UiFrameError::Context(Gles2Error::ContextLost(_)))
            || self.context.context_loss().is_some()
        {
            return;
        }
        if matches!(
            self.retire_submitted_frame_work(),
            FrameWorkRetirement::FencePublished | FrameWorkRetirement::FinishedSynchronously
        ) {
            self.counters.failed_frames_fenced += 1;
        }
    }
}

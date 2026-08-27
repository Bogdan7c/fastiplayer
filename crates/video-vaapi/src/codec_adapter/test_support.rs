use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use super::*;

/// Test-only результат readiness query без зависимости от реального VA display.
#[derive(Clone, Copy)]
pub(crate) enum FakeSurfaceReadiness {
    /// Surface query вернул обычное bool-состояние.
    Ready(bool),
    /// Surface query вернул ошибку, которую boundary обязан пробросить.
    QueryError(&'static str),
}

/// Fake decoded handle для проверки `VaapiDecodedFrameHandle` без VA hardware.
struct FakeDecodedHandle {
    /// Backing frame descriptor, который требует `DecodedHandle` contract.
    frame: Arc<InternalVaapiFrame>,
    /// Результат, который вернёт fallible readiness boundary.
    readiness: FakeSurfaceReadiness,
    /// Флаг защищает тест от случайного blocking `sync()` в `surface_ready()`.
    sync_called: Rc<Cell<bool>>,
}

impl FakeDecodedHandle {
    /// Создаёт fake handle с минимальным internal VA frame descriptor-ом.
    fn new(readiness: FakeSurfaceReadiness, sync_called: Rc<Cell<bool>>) -> Self {
        let frame = Arc::new(InternalVaapiFrame::new(
            cros_codecs::Resolution {
                width: 16,
                height: 16,
            },
            cros_codecs::libva::VA_RT_FORMAT_YUV420,
        ));

        Self {
            frame,
            readiness,
            sync_called,
        }
    }
}

impl cros_codecs::decoder::DecodedHandle for FakeDecodedHandle {
    type Frame = InternalVaapiFrame;

    /// Возвращает backing frame descriptor для release-path тестов.
    fn video_frame(&self) -> Arc<Self::Frame> {
        self.frame.clone()
    }

    /// Возвращает стабильный timestamp для fake frame-а.
    fn timestamp(&self) -> u64 {
        123
    }

    /// Возвращает coded resolution fake frame-а.
    fn coded_resolution(&self) -> cros_codecs::Resolution {
        cros_codecs::Resolution {
            width: 16,
            height: 16,
        }
    }

    /// Возвращает display resolution fake frame-а.
    fn display_resolution(&self) -> cros_codecs::Resolution {
        cros_codecs::Resolution {
            width: 16,
            height: 16,
        }
    }

    /// Старый bool API намеренно делает ошибку `true`, чтобы тест поймал
    /// случайный fallback с `try_is_ready()` на `is_ready()`.
    fn is_ready(&self) -> bool {
        match self.readiness {
            FakeSurfaceReadiness::Ready(is_ready) => is_ready,
            FakeSurfaceReadiness::QueryError(_) => true,
        }
    }

    /// Проверяет новый fallible readiness boundary.
    fn try_is_ready(&self) -> Result<bool> {
        match self.readiness {
            FakeSurfaceReadiness::Ready(is_ready) => Ok(is_ready),
            FakeSurfaceReadiness::QueryError(message) => Err(anyhow::anyhow!("{message}")),
        }
    }

    /// Отмечает blocking sync, если тестируемый path случайно его вызвал.
    fn sync(&self) -> Result<()> {
        self.sync_called.set(true);
        Ok(())
    }
}

/// Собирает wrapper поверх fake cros handle и флаг вызова `sync()`.
pub(crate) fn fake_decoded_frame_handle(
    readiness: FakeSurfaceReadiness,
) -> (VaapiDecodedFrameHandle, Rc<Cell<bool>>) {
    let sync_called = Rc::new(Cell::new(false));
    let fake_handle = FakeDecodedHandle::new(readiness, sync_called.clone());

    (
        VaapiDecodedFrameHandle::new(Box::new(fake_handle)),
        sync_called,
    )
}

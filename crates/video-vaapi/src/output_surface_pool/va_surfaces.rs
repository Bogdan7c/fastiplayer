//! VA-часть пула: создание surfaces через VA display и связка с cros-codecs.

use std::borrow::Borrow;
use std::rc::Rc;

use cros_codecs::libva::{Display, Surface, UsageHint, VASurfaceStatus};

use super::{
    OutputSurfaceAllocationError, OutputSurfaceAllocator, OutputSurfaceFormat,
    OutputSurfaceIdleError, OutputSurfaceLease, OutputSurfacePool, PooledOutputSurface,
    SurfaceIdleWait,
};

/// Аренда VA surface, которую cros-codecs picture хранит как `NativeHandle`.
pub type VaapiOutputSurfaceLease = OutputSurfaceLease<Surface<()>>;

/// Production-пул выходных surfaces декодера.
pub(crate) type VaapiOutputSurfacePool = OutputSurfacePool<VaOutputSurfaceAllocator>;

/// Создаёт internal VA surfaces (память выделяет сам драйвер).
pub(crate) struct VaOutputSurfaceAllocator {
    display: Rc<Display>,
}

impl VaOutputSurfaceAllocator {
    /// Allocator поверх уже открытого VA display декодера.
    pub(crate) fn new(display: Rc<Display>) -> Self {
        Self { display }
    }
}

impl OutputSurfaceAllocator for VaOutputSurfaceAllocator {
    type Surface = Surface<()>;

    fn allocate(
        &self,
        format: OutputSurfaceFormat,
    ) -> Result<Self::Surface, OutputSurfaceAllocationError> {
        // FourCC не задаём: VA-драйвер сам выбирает внутренний layout поверхности.
        //
        // `USAGE_HINT_EXPORT` важен для P010 zero-copy: без него часть драйверов
        // создаёт decode-only surface, который потом отвергает `vaExportSurfaceHandle`.
        let usage_hint = UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT;
        let allocation_error = |message: String| OutputSurfaceAllocationError { format, message };
        let mut surfaces = self
            .display
            .create_surfaces(
                format.rt_format,
                None,
                format.coded_resolution.width,
                format.coded_resolution.height,
                Some(usage_hint),
                vec![()],
            )
            .map_err(|error| allocation_error(format!("vaCreateSurfaces failed: {error:?}")))?;
        surfaces
            .pop()
            .ok_or_else(|| allocation_error("VA driver returned no surfaces".to_string()))
    }
}

/// Проверка простоя VA surface перед повторной выдачей.
///
/// Сначала неблокирующий `vaQuerySurfaceStatus`: на обычном пути cros-codecs уже
/// синхронизировал картинку, и surface свободна. Только если драйвер ещё работает
/// над ней (аренда вернулась с пути ошибки до sync), ждём `vaSyncSurface`.
impl PooledOutputSurface for Surface<()> {
    fn wait_until_idle(&self) -> Result<SurfaceIdleWait, OutputSurfaceIdleError> {
        let status = self
            .query_status()
            .map_err(|error| OutputSurfaceIdleError {
                message: format!("vaQuerySurfaceStatus failed: {error:?}"),
            })?;
        if status == VASurfaceStatus::VASurfaceReady {
            return Ok(SurfaceIdleWait::AlreadyIdle);
        }
        self.sync().map_err(|error| OutputSurfaceIdleError {
            message: format!("vaSyncSurface failed: {error:?}"),
        })?;
        Ok(SurfaceIdleWait::WaitedForDriver)
    }
}

/// cros-codecs читает surface арендованной picture через `Borrow`.
impl Borrow<Surface<()>> for VaapiOutputSurfaceLease {
    fn borrow(&self) -> &Surface<()> {
        self.surface()
    }
}

/// Контракт cros-codecs `NativeHandle: Into<Surface>` (нужен только encoder import-у).
///
/// Surface навсегда уходит из пула. Если её уже нет (невозможно для живой аренды),
/// возвращать нечего — это нарушение инварианта аренды.
impl From<VaapiOutputSurfaceLease> for Surface<()> {
    #[expect(
        clippy::expect_used,
        reason = "a live lease always holds its surface; only Drop/detach take it"
    )]
    fn from(lease: VaapiOutputSurfaceLease) -> Self {
        lease
            .into_detached_surface()
            .expect("live output surface lease holds its surface")
    }
}

use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

use cros_codecs::libva::{Display, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12};
use cros_codecs::video_frame::{ReadMapping, VideoFrame, WriteMapping};
use cros_codecs::{Fourcc, Resolution};

use crate::output_surface_pool::{OutputSurfaceFormat, VaapiOutputSurfaceLease};

/// Выходной кадр декодера во internal VA surface из [`crate::output_surface_pool`].
///
/// Память выделяет сам VA драйвер (fallback для Intel/Mesa путей, где decode в
/// external DRM PRIME/GBM BO завершается без ошибки, но память остаётся нулевой).
///
/// Кадр несёт арендованную у пула surface. cros-codecs забирает её ровно один раз
/// в [`VideoFrame::to_native_handle`] при создании picture; дальше surface живёт в
/// picture/DPB и сама возвращается в пул, когда отпущен последний handle.
pub struct InternalVaapiFrame {
    /// Coded resolution кадра.
    resolution: Resolution,
    /// VA RT format поверхности, который должен совпадать с VAConfig decoder context.
    rt_format: u32,
    /// Арендованная surface до передачи в picture; после передачи — `None`.
    leased_surface: Cell<Option<VaapiOutputSurfaceLease>>,
}

impl InternalVaapiFrame {
    /// Кадр поверх арендованной у пула surface.
    pub(crate) fn from_leased_surface(lease: VaapiOutputSurfaceLease) -> Self {
        let OutputSurfaceFormat {
            coded_resolution,
            rt_format,
        } = lease.format();
        Self {
            resolution: coded_resolution,
            rt_format,
            leased_surface: Cell::new(Some(lease)),
        }
    }

    /// Descriptor без surface — только для тестовых fake handle-ов.
    #[cfg(test)]
    pub(crate) fn without_surface(resolution: Resolution, rt_format: u32) -> Self {
        Self {
            resolution,
            rt_format,
            leased_surface: Cell::new(None),
        }
    }

    /// Возвращает VA RT format поверхности.
    pub fn rt_format(&self) -> u32 {
        self.rt_format
    }

    /// Возвращает количество байт на один luma sample для текущего RT format.
    fn bytes_per_sample(&self) -> usize {
        match self.rt_format {
            VA_RT_FORMAT_YUV420_10 | VA_RT_FORMAT_YUV420_12 => 2,
            _ => 1,
        }
    }
}

impl fmt::Debug for InternalVaapiFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `Cell<Option<_>>` нельзя прочитать без take(); для Debug важны только формат.
        formatter
            .debug_struct("InternalVaapiFrame")
            .field("resolution", &self.resolution)
            .field("rt_format", &self.rt_format)
            .finish_non_exhaustive()
    }
}

impl VideoFrame for InternalVaapiFrame {
    type MemDescriptor = ();
    type NativeHandle = VaapiOutputSurfaceLease;

    fn fourcc(&self) -> Fourcc {
        match self.rt_format {
            VA_RT_FORMAT_YUV420_10 => Fourcc::from(b"P010"),
            VA_RT_FORMAT_YUV420_12 => Fourcc::from(b"P012"),
            _ => Fourcc::from(b"NV12"),
        }
    }

    fn resolution(&self) -> Resolution {
        self.resolution
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let width = self.resolution.width as usize;
        let height = self.resolution.height as usize;
        let bytes_per_sample = self.bytes_per_sample();
        vec![
            width * height * bytes_per_sample,
            width * height / 2 * bytes_per_sample,
        ]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        let bytes_per_sample = self.bytes_per_sample();
        vec![
            self.resolution.width as usize * bytes_per_sample,
            self.resolution.width as usize * bytes_per_sample,
        ]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        Err("Internal VA surfaces are zero-copy only; export through DMA-BUF".to_string())
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Err("Internal VA surfaces are not CPU-writable through VideoFrame::map_mut()".to_string())
    }

    /// Передаёт арендованную surface в cros-codecs picture.
    ///
    /// Surface уже создана (или переиспользована) пулом в alloc-callback-е, здесь
    /// `vaCreateSurfaces` не вызывается. Второй вызов для того же кадра — ошибка:
    /// одна surface не может стать render target-ом двух разных pictures.
    fn to_native_handle(&self, _display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        self.leased_surface.take().ok_or_else(|| {
            "output frame has no leased VA surface (already handed to a picture)".to_string()
        })
    }
}

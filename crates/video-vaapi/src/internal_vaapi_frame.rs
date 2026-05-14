use std::rc::Rc;

use cros_codecs::libva::{
    Display, Surface, UsageHint, VA_RT_FORMAT_YUV420_10, VA_RT_FORMAT_YUV420_12,
};
use cros_codecs::video_frame::{ReadMapping, VideoFrame, WriteMapping};
use cros_codecs::{Fourcc, Resolution};
use tracing::debug;

/// Лёгкий descriptor кадра для decode в internal VA surface.
///
/// Память выделяет сам VA драйвер. Это fallback для Intel/Mesa путей,
/// где decode в external DRM PRIME/GBM BO завершается без ошибки, но память остаётся нулевой.
#[derive(Clone, Copy, Debug)]
pub struct InternalVaapiFrame {
    /// Coded resolution кадра.
    resolution: Resolution,
    /// VA RT format поверхности, который должен совпадать с VAConfig decoder context.
    rt_format: u32,
}

impl InternalVaapiFrame {
    /// Создаёт descriptor внутреннего VA кадра.
    pub fn new(resolution: Resolution, rt_format: u32) -> Self {
        Self {
            resolution,
            rt_format,
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

impl VideoFrame for InternalVaapiFrame {
    type MemDescriptor = ();
    type NativeHandle = Surface<()>;

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

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        // Для decoder surface не задаём FourCC вручную: VA-драйвер сам выбирает
        // внутренний layout поверхности, а NV12 мы запрашиваем позже на readback.
        //
        // `USAGE_HINT_EXPORT` важен для P010 zero-copy: без него часть драйверов
        // создаёт decode-only surface, который потом отвергает `vaExportSurfaceHandle`.
        let usage_hint = UsageHint::USAGE_HINT_DECODER | UsageHint::USAGE_HINT_EXPORT;
        debug!(
            width = self.resolution.width,
            height = self.resolution.height,
            rt_format = self.rt_format,
            usage_hint = usage_hint.bits(),
            "Creating internal VA decoder/export surface without forced FourCC"
        );

        let mut surfaces = display
            .create_surfaces(
                self.rt_format,
                None,
                self.resolution.width,
                self.resolution.height,
                Some(usage_hint),
                vec![()],
            )
            .map_err(|e| format!("Failed to create internal VA surface: {e:?}"))?;

        surfaces
            .pop()
            .ok_or_else(|| "VA driver returned no internal surfaces".to_string())
    }
}

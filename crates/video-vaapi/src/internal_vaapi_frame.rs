use std::rc::Rc;

use cros_codecs::libva::{Display, Surface, UsageHint, VA_RT_FORMAT_YUV420};
use cros_codecs::video_frame::{ReadMapping, VideoFrame, WriteMapping};
use cros_codecs::{Fourcc, Resolution};

/// Лёгкий descriptor кадра для decode в internal VA surface.
///
/// Память выделяет сам VA драйвер. Это fallback для Intel/Mesa путей,
/// где decode в external DRM PRIME/GBM BO завершается без ошибки, но память остаётся нулевой.
#[derive(Clone, Copy, Debug)]
pub struct InternalVaapiFrame {
    /// Coded resolution кадра.
    resolution: Resolution,
}

impl InternalVaapiFrame {
    /// Создаёт descriptor внутреннего VA кадра.
    pub fn new(resolution: Resolution) -> Self {
        Self { resolution }
    }
}

impl VideoFrame for InternalVaapiFrame {
    type MemDescriptor = ();
    type NativeHandle = Surface<()>;

    fn fourcc(&self) -> Fourcc {
        Fourcc::from(b"NV12")
    }

    fn resolution(&self) -> Resolution {
        self.resolution
    }

    fn get_plane_size(&self) -> Vec<usize> {
        let width = self.resolution.width as usize;
        let height = self.resolution.height as usize;
        vec![width * height, width * height / 2]
    }

    fn get_plane_pitch(&self) -> Vec<usize> {
        vec![
            self.resolution.width as usize,
            self.resolution.width as usize,
        ]
    }

    fn map<'a>(&'a self) -> Result<Box<dyn ReadMapping<'a> + 'a>, String> {
        Err("Internal VA surfaces must be read through DecodedHandle::nv12_image()".to_string())
    }

    fn map_mut<'a>(&'a mut self) -> Result<Box<dyn WriteMapping<'a> + 'a>, String> {
        Err("Internal VA surfaces are not CPU-writable through VideoFrame::map_mut()".to_string())
    }

    fn to_native_handle(&self, display: &Rc<Display>) -> Result<Self::NativeHandle, String> {
        let mut surfaces = display
            .create_surfaces(
                VA_RT_FORMAT_YUV420,
                Some(u32::from(self.fourcc())),
                self.resolution.width,
                self.resolution.height,
                Some(UsageHint::USAGE_HINT_DECODER),
                vec![()],
            )
            .map_err(|e| format!("Failed to create internal VA surface: {e:?}"))?;

        surfaces
            .pop()
            .ok_or_else(|| "VA driver returned no internal surfaces".to_string())
    }
}

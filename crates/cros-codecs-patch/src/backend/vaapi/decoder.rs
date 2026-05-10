// Copyright 2023 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::OnceLock;

use anyhow::anyhow;
use anyhow::Context as AnyhowContext;
use libva::{
    Buffer, Context, Display, Image, Picture, PictureEnd, PictureNew, PictureSync, Surface,
    SurfaceMemoryDescriptor, VaError,
};

use crate::decoder::{
    DecodedDmaBufExportLayout, DecodedDmaBufImage, DecodedDmaBufLayer, DecodedDmaBufObject,
    DecodedNv12Image,
};
use crate::decoder::stateless::StatelessBackendResult;
use crate::decoder::stateless::StatelessCodec;
use crate::decoder::stateless::StatelessDecoderBackend;
use crate::decoder::stateless::StatelessDecoderBackendPicture;
use crate::decoder::DecodedHandle as DecodedHandleTrait;
use crate::decoder::StreamInfo;
use crate::video_frame::VideoFrame;
use crate::DecodedFormat;
use crate::Fourcc;
use crate::Rect;
use crate::Resolution;

/// A decoded frame handle.
pub(crate) type DecodedHandle<V> = Rc<RefCell<VaapiDecodedHandle<V>>>;

/// Gets the VASurfaceID for the given `picture`.
pub(crate) fn va_surface_id<V: VideoFrame>(
    handle: &Option<DecodedHandle<V>>,
) -> libva::VASurfaceID {
    match handle {
        None => libva::VA_INVALID_SURFACE,
        Some(handle) => handle.borrow().surface().id(),
    }
}

/// Maps VA RT format selected from codec headers to the decoded frame format
/// expected from output surfaces.
fn decoded_format_from_rt_format(rt_format: u32) -> anyhow::Result<DecodedFormat> {
    match rt_format {
        libva::VA_RT_FORMAT_YUV420 => Ok(DecodedFormat::NV12),
        libva::VA_RT_FORMAT_YUV420_10 => Ok(DecodedFormat::I010),
        libva::VA_RT_FORMAT_YUV420_12 => Ok(DecodedFormat::I012),
        libva::VA_RT_FORMAT_YUV422 => Ok(DecodedFormat::I422),
        libva::VA_RT_FORMAT_YUV422_10 => Ok(DecodedFormat::I210),
        libva::VA_RT_FORMAT_YUV422_12 => Ok(DecodedFormat::I212),
        libva::VA_RT_FORMAT_YUV444 => Ok(DecodedFormat::I444),
        libva::VA_RT_FORMAT_YUV444_10 => Ok(DecodedFormat::I410),
        libva::VA_RT_FORMAT_YUV444_12 => Ok(DecodedFormat::I412),
        other => Err(anyhow!("Unsupported VA RT format: {:#x}", other)),
    }
}

/// Parses a boolean environment flag used by VA-API diagnostics.
fn env_flag_enabled(env_name: &'static str) -> bool {
    std::env::var(env_name)
        .ok()
        .as_deref()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Returns true when readback should try `vaDeriveImage` before stable `vaGetImage`.
///
/// `vaDeriveImage` can expose driver-native/tiled memory. On Intel UHD 620
/// it produced black/invalid frames for VP9, while `vaGetImage` returned
/// correct NV12 pixels, so the stable path is the default.
fn should_try_derived_image_readback() -> bool {
    static TRY_DERIVED_IMAGE: OnceLock<bool> = OnceLock::new();
    *TRY_DERIVED_IMAGE.get_or_init(|| {
        std::env::var("VIDEOPLAYER_TRY_VA_DERIVE_IMAGE")
            .ok()
            .as_deref()
            .map(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
            .unwrap_or(false)
    })
}

impl<V: VideoFrame> DecodedHandleTrait for DecodedHandle<V> {
    type Frame = V;

    fn video_frame(&self) -> Arc<Self::Frame> {
        self.borrow().backing_frame.clone()
    }

    fn coded_resolution(&self) -> Resolution {
        self.borrow().surface().size().into()
    }

    fn display_resolution(&self) -> Resolution {
        self.borrow().display_resolution
    }

    fn timestamp(&self) -> u64 {
        self.borrow().timestamp()
    }

    fn is_ready(&self) -> bool {
        self.borrow().state.is_ready().unwrap_or(true)
    }

    fn sync(&self) -> anyhow::Result<()> {
        self.borrow_mut().sync().context("while syncing picture")?;

        Ok(())
    }

    fn nv12_image(&self) -> anyhow::Result<Option<DecodedNv12Image>> {
        let mut borrowed_handle = self.borrow_mut();
        borrowed_handle
            .sync()
            .context("while syncing picture before VA image readback")?;
        borrowed_handle.create_nv12_image().map(Some)
    }

    fn dma_buf_image(&self) -> anyhow::Result<Option<DecodedDmaBufImage>> {
        let mut borrowed_handle = self.borrow_mut();
        borrowed_handle
            .sync()
            .context("while syncing picture before VA surface DMA-BUF export")?;
        borrowed_handle.export_dma_buf_image().map(Some)
    }
}

/// A trait for providing the basic information needed to setup libva for decoding.
pub(crate) trait VaStreamInfo {
    /// Returns the VA profile of the stream.
    fn va_profile(&self) -> anyhow::Result<i32>;
    /// Returns the RT format of the stream.
    #[allow(dead_code)]
    fn rt_format(&self) -> anyhow::Result<u32>;
    /// Returns the minimum number of surfaces required to decode the stream.
    fn min_num_surfaces(&self) -> usize;
    /// Returns the coded size of the surfaces required to decode the stream.
    fn coded_size(&self) -> Resolution;
    /// Returns the visible rectangle within the coded size for the stream.
    fn visible_rect(&self) -> Rect;
}

/// Rendering state of a VA picture.
enum PictureState<M: SurfaceMemoryDescriptor> {
    Ready(Picture<PictureSync, Surface<M>>),
    Pending(Picture<PictureEnd, Surface<M>>),
    // Only set in sync when we take ownership of the VA picture.
    Invalid,
}

impl<M: SurfaceMemoryDescriptor> PictureState<M> {
    /// Make sure that all pending operations on the picture have completed.
    fn sync(&mut self) -> Result<(), VaError> {
        let res;

        (*self, res) = match std::mem::replace(self, PictureState::Invalid) {
            state @ PictureState::Ready(_) => (state, Ok(())),
            PictureState::Pending(picture) => match picture.sync() {
                Ok(picture) => (PictureState::Ready(picture), Ok(())),
                Err((e, picture)) => (PictureState::Pending(picture), Err(e)),
            },
            PictureState::Invalid => unreachable!(),
        };

        res
    }

    fn surface(&self) -> &Surface<M> {
        match self {
            PictureState::Ready(picture) => picture.surface(),
            PictureState::Pending(picture) => picture.surface(),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn timestamp(&self) -> u64 {
        match self {
            PictureState::Ready(picture) => picture.timestamp(),
            PictureState::Pending(picture) => picture.timestamp(),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn is_ready(&self) -> Result<bool, VaError> {
        match self {
            PictureState::Ready(_) => Ok(true),
            PictureState::Pending(picture) => picture
                .surface()
                .query_status()
                .map(|s| s == libva::VASurfaceStatus::VASurfaceReady),
            PictureState::Invalid => unreachable!(),
        }
    }

    fn new_from_same_surface(&self, timestamp: u64) -> Picture<PictureNew, Surface<M>> {
        match &self {
            PictureState::Ready(picture) => Picture::new_from_same_surface(timestamp, picture),
            PictureState::Pending(picture) => Picture::new_from_same_surface(timestamp, picture),
            PictureState::Invalid => unreachable!(),
        }
    }
}

/// VA-API backend handle.
///
/// This includes the VA picture which can be pending rendering or complete, as well as useful
/// meta-information.
pub struct VaapiDecodedHandle<V: VideoFrame> {
    backing_frame: Arc<V>,
    state: PictureState<<V as VideoFrame>::MemDescriptor>,
    /// Actual resolution of the visible rectangle in the decoded buffer.
    display_resolution: Resolution,
    display: Rc<Display>,
}

impl<V: VideoFrame> VaapiDecodedHandle<V> {
    /// Creates a new pending handle on `surface_id`.
    fn new(
        picture: VaapiPicture<V>,
        display_resolution: Resolution,
        display: Rc<Display>,
    ) -> anyhow::Result<Self> {
        let backing_frame = picture.backing_frame;
        let picture = picture.picture.begin()?.render()?.end()?;
        Ok(Self {
            backing_frame: backing_frame,
            state: PictureState::Pending(picture),
            display_resolution: display_resolution,
            display,
        })
    }

    fn sync(&mut self) -> Result<(), VaError> {
        self.state.sync()
    }

    /// Creates a new picture from the surface backing the current one. Useful for interlaced
    /// decoding. TODO: Do we need this for other purposes? We don't intend to support interlaced.
    pub(crate) fn new_picture_from_same_surface(&self, timestamp: u64) -> VaapiPicture<V> {
        VaapiPicture {
            picture: self.state.new_from_same_surface(timestamp),
            backing_frame: self.backing_frame.clone(),
        }
    }

    pub(crate) fn surface(&self) -> &Surface<<V as VideoFrame>::MemDescriptor> {
        self.state.surface()
    }

    /// Returns the timestamp of this handle.
    fn timestamp(&self) -> u64 {
        self.state.timestamp()
    }

    fn copy_va_image_plane_rows(
        image_data: &[u8],
        plane_name: &str,
        plane_offset: usize,
        plane_stride: usize,
        plane_rows: usize,
    ) -> anyhow::Result<Vec<u8>> {
        let plane_size = plane_stride.checked_mul(plane_rows).ok_or_else(|| {
            anyhow!(
                "VA image {} plane size overflow: stride={} rows={}",
                plane_name,
                plane_stride,
                plane_rows
            )
        })?;
        let mut plane = vec![0; plane_size];

        for row_index in 0..plane_rows {
            let row_offset = row_index.checked_mul(plane_stride).ok_or_else(|| {
                anyhow!(
                    "VA image {} row offset overflow: row={} stride={}",
                    plane_name,
                    row_index,
                    plane_stride
                )
            })?;
            let source_start = plane_offset.checked_add(row_offset).ok_or_else(|| {
                anyhow!(
                    "VA image {} source offset overflow: offset={} row_offset={}",
                    plane_name,
                    plane_offset,
                    row_offset
                )
            })?;
            let source_end = source_start.checked_add(plane_stride).ok_or_else(|| {
                anyhow!(
                    "VA image {} source end overflow: start={} stride={}",
                    plane_name,
                    source_start,
                    plane_stride
                )
            })?;
            let target_start = row_offset;
            let target_end = target_start + plane_stride;
            let source_row = image_data.get(source_start..source_end).ok_or_else(|| {
                anyhow!(
                    "VA image {} row is out of bounds: start={} end={} len={}",
                    plane_name,
                    source_start,
                    source_end,
                    image_data.len()
                )
            })?;

            plane[target_start..target_end].copy_from_slice(source_row);
        }

        Ok(plane)
    }

    fn copy_nv12_from_image(
        image: &Image<'_>,
        width: usize,
        height: usize,
    ) -> anyhow::Result<DecodedNv12Image> {
        let va_image = *image.image();

        if va_image.format.fourcc != libva::VA_FOURCC_NV12 {
            return Err(anyhow!(
                "VA image format is not NV12: fourcc={:#x}",
                va_image.format.fourcc
            ));
        }
        if va_image.num_planes < 2 {
            return Err(anyhow!(
                "VA image has fewer than 2 planes: num_planes={}",
                va_image.num_planes
            ));
        }

        let y_stride = va_image.pitches[0] as usize;
        let uv_stride = va_image.pitches[1] as usize;
        let y_offset = va_image.offsets[0] as usize;
        let uv_offset = va_image.offsets[1] as usize;
        let image_data = image.as_ref();

        if y_stride < width {
            return Err(anyhow!(
                "VA image Y stride is smaller than width: stride={} width={}",
                y_stride,
                width
            ));
        }
        if uv_stride < width {
            return Err(anyhow!(
                "VA image UV stride is smaller than NV12 row width: stride={} width={}",
                uv_stride,
                width
            ));
        }

        let y_plane = Self::copy_va_image_plane_rows(image_data, "Y", y_offset, y_stride, height)?;
        let uv_height = height / 2;
        let uv_plane =
            Self::copy_va_image_plane_rows(image_data, "UV", uv_offset, uv_stride, uv_height)?;

        Ok(DecodedNv12Image {
            width: width as u32,
            height: height as u32,
            y_plane,
            uv_plane,
            y_stride: y_stride as u32,
            uv_stride: uv_stride as u32,
        })
    }

    /// Copies decoded pixels from the VA surface into CPU-owned NV12 plane buffers.
    fn create_nv12_image(&self) -> anyhow::Result<DecodedNv12Image> {
        let surface = self.surface();
        let coded_resolution = surface.size();
        let width = coded_resolution.0 as usize;
        let height = coded_resolution.1 as usize;

        if should_try_derived_image_readback() {
            match Image::derive_from(surface, coded_resolution) {
                Ok(derived_image) => {
                    log::debug!(
                        "VA readback using derived image: fourcc={:#x} num_planes={}",
                        derived_image.image().format.fourcc,
                        derived_image.image().num_planes
                    );
                    match Self::copy_nv12_from_image(&derived_image, width, height) {
                        Ok(decoded_image) => return Ok(decoded_image),
                        Err(error) => {
                            log::debug!(
                                "Derived VA image is not usable as NV12, falling back to vaGetImage: {}",
                                error
                            );
                        }
                    }
                }
                Err(error) => {
                    log::debug!(
                        "vaDeriveImage failed, falling back to vaGetImage: {:?}",
                        error
                    );
                }
            }
        } else if env_flag_enabled("VIDEOPLAYER_FORCE_VA_GET_IMAGE") {
            log::debug!("VA readback uses stable vaGetImage path");
        }

        let image_format = self
            .display
            .query_image_formats()
            .context("while querying VA image formats")?
            .into_iter()
            .find(|format| format.fourcc == libva::VA_FOURCC_NV12)
            .ok_or_else(|| anyhow!("VA driver does not expose NV12 image format"))?;

        let image = Image::create_from(surface, image_format, coded_resolution, coded_resolution)
            .context("while creating NV12 VA image from decoded surface")?;

        Self::copy_nv12_from_image(&image, width, height)
    }

    /// Возвращает true, когда decoded frame должен идти через VA separate-layer DMA-BUF export.
    ///
    /// P010 сейчас единственный формат, где separate-layer является baseline path. Intel i965
    /// проверенно отдаёт VP9 Profile 2 P010 как отдельные `R16 + GR32` layers и отклоняет
    /// composed P010 export. NV12 оставляем на composed-first path, чтобы не менять уже рабочий
    /// SDR pipeline.
    fn prefers_separate_dma_buf_export(&self) -> bool {
        self.backing_frame.fourcc() == Fourcc::from(b"P010")
    }

    /// Экспортирует P010 через основной separate-layer VA-API path.
    fn export_dma_buf_image_separate_first(
        &self,
    ) -> anyhow::Result<(libva::DrmPrimeSurfaceDescriptor, DecodedDmaBufExportLayout)> {
        match self.surface().export_prime_separate_layers() {
            Ok(separate_surface) => {
                static LOGGED_SEPARATE_LAYER_BASELINE: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if !LOGGED_SEPARATE_LAYER_BASELINE
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    let layer_formats = separate_surface
                        .layers
                        .iter()
                        .map(|layer| layer.drm_format)
                        .collect::<Vec<_>>();
                    let plane_counts = separate_surface
                        .layers
                        .iter()
                        .map(|layer| layer.num_planes)
                        .collect::<Vec<_>>();
                    log::info!(
                        "VA P010 DRM PRIME export uses preferred separate-layer layout: \
                         fourcc={:#x}, width={}, height={}, objects={}, layers={}, \
                         layer_formats={:?}, plane_counts={:?}",
                        separate_surface.fourcc,
                        separate_surface.width,
                        separate_surface.height,
                        separate_surface.objects.len(),
                        separate_surface.layers.len(),
                        layer_formats,
                        plane_counts
                    );
                }
                Ok((separate_surface, DecodedDmaBufExportLayout::SeparateLayers))
            }
            Err(separate_error) => match self.surface().export_prime() {
                Ok(composed_surface) => {
                    static LOGGED_COMPOSED_COMPATIBILITY: std::sync::atomic::AtomicBool =
                        std::sync::atomic::AtomicBool::new(false);
                    if !LOGGED_COMPOSED_COMPATIBILITY
                        .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        log::warn!(
                            "VA P010 separate-layer DRM PRIME export failed, using composed compatibility export: \
                             separate_status={:#x}, separate_error={}, fourcc={:#x}, width={}, height={}, \
                             objects={}, layers={}",
                            separate_error.va_status(),
                            separate_error,
                            composed_surface.fourcc,
                            composed_surface.width,
                            composed_surface.height,
                            composed_surface.objects.len(),
                            composed_surface.layers.len()
                        );
                    }
                    Ok((composed_surface, DecodedDmaBufExportLayout::ComposedLayers))
                }
                Err(composed_error) => Err(anyhow!(
                    "while exporting decoded VA P010 surface as DRM PRIME: separate-layer export failed: {} \
                     (status={:#x}); composed compatibility export failed: {} (status={:#x})",
                    separate_error,
                    separate_error.va_status(),
                    composed_error,
                    composed_error.va_status()
                )),
            },
        }
    }

    /// Экспортирует не-P010 surfaces через прежний composed-first VA-API path.
    fn export_dma_buf_image_composed_first(
        &self,
    ) -> anyhow::Result<(libva::DrmPrimeSurfaceDescriptor, DecodedDmaBufExportLayout)> {
        match self.surface().export_prime() {
            Ok(exported_surface) => Ok((exported_surface, DecodedDmaBufExportLayout::ComposedLayers)),
            Err(composed_error) => match self.surface().export_prime_separate_layers() {
                Ok(separate_surface) => {
                    static LOGGED_SEPARATE_LAYER_COMPATIBILITY:
                        std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                    let layer_formats = separate_surface
                        .layers
                        .iter()
                        .map(|layer| layer.drm_format)
                        .collect::<Vec<_>>();
                    let plane_counts = separate_surface
                        .layers
                        .iter()
                        .map(|layer| layer.num_planes)
                        .collect::<Vec<_>>();
                    if !LOGGED_SEPARATE_LAYER_COMPATIBILITY
                        .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        log::warn!(
                            "VA surface composed DRM PRIME export failed, using separate-layer compatibility export: \
                             composed_status={:#x}, composed_error={}, fourcc={:#x}, width={}, height={}, \
                             objects={}, layers={}, layer_formats={:?}, plane_counts={:?}",
                            composed_error.va_status(),
                            composed_error,
                            separate_surface.fourcc,
                            separate_surface.width,
                            separate_surface.height,
                            separate_surface.objects.len(),
                            separate_surface.layers.len(),
                            layer_formats,
                            plane_counts
                        );
                    }
                    Ok((separate_surface, DecodedDmaBufExportLayout::SeparateLayers))
                }
                Err(separate_error) => Err(anyhow!(
                    "while exporting decoded VA surface as DRM PRIME: composed export failed: {} \
                     (status={:#x}); separate-layer compatibility export failed: {} (status={:#x})",
                    composed_error,
                    composed_error.va_status(),
                    separate_error,
                    separate_error.va_status()
                )),
            },
        }
    }

    /// Экспортирует decoded VA surface как DRM PRIME/DMA-BUF.
    fn export_dma_buf_image(&self) -> anyhow::Result<DecodedDmaBufImage> {
        let (exported_surface, export_layout) = if self.prefers_separate_dma_buf_export() {
            self.export_dma_buf_image_separate_first()?
        } else {
            self.export_dma_buf_image_composed_first()?
        };

        let objects = exported_surface
            .objects
            .into_iter()
            .map(|object| DecodedDmaBufObject {
                fd: object.fd,
                size: object.size,
                drm_format_modifier: object.drm_format_modifier,
            })
            .collect();

        let layers = exported_surface
            .layers
            .into_iter()
            .map(|layer| DecodedDmaBufLayer {
                drm_format: layer.drm_format,
                num_planes: layer.num_planes,
                object_index: layer.object_index,
                offset: layer.offset,
                pitch: layer.pitch,
            })
            .collect();

        Ok(DecodedDmaBufImage {
            surface_id: u64::from(self.surface().id()),
            fourcc: exported_surface.fourcc,
            export_layout,
            width: exported_surface.width,
            height: exported_surface.height,
            objects,
            layers,
        })
    }
}

pub struct VaapiBackend<V: VideoFrame> {
    pub display: Rc<Display>,
    pub context: Rc<Context>,
    stream_info: StreamInfo,
    // TODO: We should try to support context reuse
    _supports_context_reuse: bool,
    _phantom_data: PhantomData<V>,
}

impl<V: VideoFrame> VaapiBackend<V> {
    pub(crate) fn new(display: Rc<libva::Display>, supports_context_reuse: bool) -> Self {
        let init_stream_info = StreamInfo {
            format: DecodedFormat::NV12,
            coded_resolution: Resolution::from((16, 16)),
            display_resolution: Resolution::from((16, 16)),
            min_num_frames: 1,
        };
        let config = display
            .create_config(
                vec![libva::VAConfigAttrib {
                    type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: libva::VA_RT_FORMAT_YUV420,
                }],
                libva::VAProfile::VAProfileH264Main,
                libva::VAEntrypoint::VAEntrypointVLD,
            )
            .expect("Could not create initial VAConfig!");
        let context = display
            .create_context::<<V as VideoFrame>::MemDescriptor>(
                &config,
                init_stream_info.coded_resolution.width,
                init_stream_info.coded_resolution.height,
                None,
                true,
            )
            .expect("Could not create initial VAContext!");
        Self {
            display: display,
            context: context,
            _supports_context_reuse: supports_context_reuse,
            stream_info: init_stream_info,
            _phantom_data: Default::default(),
        }
    }

    pub(crate) fn new_sequence<StreamData>(
        &mut self,
        stream_params: &StreamData,
    ) -> StatelessBackendResult<()>
    where
        for<'a> &'a StreamData: VaStreamInfo,
    {
        self.stream_info.display_resolution = Resolution::from(stream_params.visible_rect());
        self.stream_info.coded_resolution = stream_params.coded_size().clone();
        self.stream_info.min_num_frames = stream_params.min_num_surfaces();

        // TODO: Handle context re-use
        // VAConfig должен соответствовать текущему codec sequence header:
        // VP9 profile0 использует 8-bit YUV420, а HDR/profile2 требует 10/12-bit RT format.
        let rt_format =
            stream_params.rt_format().map_err(|_| anyhow!("Could not get VA RT format!"))?;
        self.stream_info.format = decoded_format_from_rt_format(rt_format)?;

        let config = self
            .display
            .create_config(
                vec![libva::VAConfigAttrib {
                    type_: libva::VAConfigAttribType::VAConfigAttribRTFormat,
                    value: rt_format,
                }],
                stream_params.va_profile().map_err(|_| anyhow!("Could not get VAProfile!"))?,
                libva::VAEntrypoint::VAEntrypointVLD,
            )
            .map_err(|_| anyhow!("Could not create VAConfig!"))?;
        let context = self
            .display
            .create_context::<<V as VideoFrame>::MemDescriptor>(
                &config,
                self.stream_info.coded_resolution.width,
                self.stream_info.coded_resolution.height,
                None,
                true,
            )
            .map_err(|_| anyhow!("Could not create VAContext!"))?;
        self.context = context;

        Ok(())
    }

    pub(crate) fn process_picture<Codec: StatelessCodec>(
        &mut self,
        picture: VaapiPicture<V>,
    ) -> StatelessBackendResult<<Self as StatelessDecoderBackend>::Handle>
    where
        Self: StatelessDecoderBackendPicture<Codec>,
        for<'a> &'a Codec::FormatInfo: VaStreamInfo,
    {
        Ok(Rc::new(RefCell::new(VaapiDecodedHandle::new(
            picture,
            self.stream_info.display_resolution.clone(),
            self.display.clone(),
        )?)))
    }
}

/// Shortcut for pictures used for the VAAPI backend.
pub struct VaapiPicture<V: VideoFrame> {
    picture: Picture<PictureNew, Surface<V::MemDescriptor>>,
    backing_frame: Arc<V>,
}

impl<V: VideoFrame> VaapiPicture<V> {
    pub fn new(timestamp: u64, context: Rc<Context>, backing_frame: V) -> Self {
        let display = context.display();
        let surface = backing_frame
            .to_native_handle(display)
            .expect("Failed to export video frame to vaapi picture!")
            .into();
        Self {
            backing_frame: Arc::new(backing_frame),
            picture: Picture::new(timestamp, context, surface),
        }
    }

    pub fn surface(&self) -> &Surface<V::MemDescriptor> {
        self.picture.surface()
    }

    pub fn add_buffer(&mut self, buffer: Buffer) {
        self.picture.add_buffer(buffer)
    }
}

impl<V: VideoFrame> StatelessDecoderBackend for VaapiBackend<V> {
    type Handle = DecodedHandle<V>;

    fn stream_info(&self) -> Option<&StreamInfo> {
        Some(&self.stream_info)
    }

    fn reset_backend(&mut self) -> anyhow::Result<()> {
        //TODO(bchoobineh): Implement VAAPI DRC
        Ok(())
    }
}

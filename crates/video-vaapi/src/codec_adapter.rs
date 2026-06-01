use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use codec_core::{
    BitDepth, ChromaSubsampling, SupportedVideoDecodeFormat, VideoCodec, VideoProfile,
    VideoSurfaceFormat, Vp9Profile,
};
use cros_codecs::DecodedFormat;
use cros_codecs::decoder::stateless::{DecodeError, StatelessVideoDecoder};
use cros_codecs::decoder::{
    BlockingMode, DecodedDmaBufImage, DecodedHandle, DecoderEvent, DynDecodedHandle, StreamInfo,
};
use cros_codecs::libva::Display;
use video_core::{VideoStreamConfigRejection, VideoStreamDecodeConfig, VideoStreamPacketization};

use crate::frame_pool::DmaFramePool;
use crate::internal_vaapi_frame::InternalVaapiFrame;

/// Codec-neutral формат decoded frame-а внутри VAAPI adapter boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VaapiDecodedFormat {
    /// 8-bit planar 4:2:0.
    I420,

    /// 8-bit semi-planar 4:2:0.
    Nv12,

    /// 8-bit planar 4:2:2.
    I422,

    /// 8-bit planar 4:4:4.
    I444,

    /// 10-bit 4:2:0 в 16-bit storage words.
    I010,

    /// 12-bit 4:2:0 в 16-bit storage words.
    I012,

    /// 10-bit 4:2:2 в 16-bit storage words.
    I210,

    /// 12-bit 4:2:2 в 16-bit storage words.
    I212,

    /// 10-bit 4:4:4 в 16-bit storage words.
    I410,

    /// 12-bit 4:4:4 в 16-bit storage words.
    I412,

    /// Tiled 8-bit 4:2:0 format; production renderer boundary его не принимает.
    Mm21,
}

impl From<DecodedFormat> for VaapiDecodedFormat {
    /// Переводит cros-codecs output format в локальный adapter enum.
    fn from(format: DecodedFormat) -> Self {
        match format {
            DecodedFormat::I420 => Self::I420,
            DecodedFormat::NV12 => Self::Nv12,
            DecodedFormat::I422 => Self::I422,
            DecodedFormat::I444 => Self::I444,
            DecodedFormat::I010 => Self::I010,
            DecodedFormat::I012 => Self::I012,
            DecodedFormat::I210 => Self::I210,
            DecodedFormat::I212 => Self::I212,
            DecodedFormat::I410 => Self::I410,
            DecodedFormat::I412 => Self::I412,
            DecodedFormat::MM21 => Self::Mm21,
        }
    }
}

/// Размер кадра без раскрытия concrete cros-codecs type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaapiResolution {
    /// Width в пикселях.
    pub(crate) width: u32,

    /// Height в пикселях.
    pub(crate) height: u32,
}

impl From<cros_codecs::Resolution> for VaapiResolution {
    /// Копирует resolution в локальную структуру boundary layer-а.
    fn from(resolution: cros_codecs::Resolution) -> Self {
        Self {
            width: resolution.width,
            height: resolution.height,
        }
    }
}

/// Stream info, который нужен decode owner-у после FormatChanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VaapiAdapterStreamInfo {
    /// Decoded output format.
    pub(crate) format: VaapiDecodedFormat,

    /// Coded resolution для output surface pool-а.
    pub(crate) coded_resolution: VaapiResolution,

    /// Display resolution, если codec сообщает crop/display area.
    pub(crate) display_resolution: VaapiResolution,

    /// Минимум output frames, который запросил codec backend.
    pub(crate) min_num_frames: usize,
}

impl From<&StreamInfo> for VaapiAdapterStreamInfo {
    /// Копирует cros stream info в локальный adapter boundary.
    fn from(stream_info: &StreamInfo) -> Self {
        Self {
            format: stream_info.format.into(),
            coded_resolution: stream_info.coded_resolution.into(),
            display_resolution: stream_info.display_resolution.into(),
            min_num_frames: stream_info.min_num_frames,
        }
    }
}

/// Decoded handle, принадлежащий VAAPI adapter-у до renderer release.
pub(crate) struct VaapiDecodedFrameHandle {
    /// Concrete handle остаётся спрятанным внутри adapter module-а.
    inner: DynDecodedHandle<InternalVaapiFrame>,
}

impl VaapiDecodedFrameHandle {
    /// Создаёт wrapper вокруг cros decoded handle.
    fn new(inner: DynDecodedHandle<InternalVaapiFrame>) -> Self {
        Self { inner }
    }

    /// Возвращает coded resolution decoded frame-а.
    pub(crate) fn coded_resolution(&self) -> VaapiResolution {
        self.inner.coded_resolution().into()
    }

    /// Возвращает display resolution decoded frame-а.
    pub(crate) fn display_resolution(&self) -> VaapiResolution {
        self.inner.display_resolution().into()
    }

    /// Возвращает timestamp frame-а в микросекундах.
    pub(crate) fn timestamp(&self) -> u64 {
        self.inner.timestamp()
    }

    /// Блокируется до завершения VA decode work.
    pub(crate) fn sync(&self) -> Result<()> {
        self.inner.sync()
    }

    /// Экспортирует VA surface как DMA-BUF descriptor.
    pub(crate) fn dma_buf_image(&self) -> Result<Option<DecodedDmaBufImage>> {
        self.inner.dma_buf_image()
    }

    /// Достаёт backing frame для возврата в surface pool после release.
    pub(crate) fn video_frame(&self) -> Arc<InternalVaapiFrame> {
        self.inner.video_frame()
    }
}

/// Codec-neutral decoder events, которые нужны владельцу VAAPI decode loop-а.
pub(crate) enum VaapiDecoderEvent {
    /// Backend завершил decode одного кадра.
    FrameReady(VaapiDecodedFrameHandle),

    /// Stream format/resolution изменились.
    FormatChanged,
}

impl From<DecoderEvent<DynDecodedHandle<InternalVaapiFrame>>> for VaapiDecoderEvent {
    /// Скрывает cros event enum за локальным adapter event-ом.
    fn from(event: DecoderEvent<DynDecodedHandle<InternalVaapiFrame>>) -> Self {
        match event {
            DecoderEvent::FrameReady(handle) => {
                Self::FrameReady(VaapiDecodedFrameHandle::new(handle))
            }
            DecoderEvent::FormatChanged => Self::FormatChanged,
        }
    }
}

/// Decode error без cros-codecs enum-а на вызывающей стороне adapter boundary.
#[derive(Debug)]
pub(crate) enum VaapiAdapterDecodeError {
    /// Decoder просит output surfaces перед повторной отправкой того же packet-а.
    NotEnoughOutputBuffers(usize),

    /// Нужно обработать pending events и повторить тот же packet.
    CheckEvents,

    /// Packet невозможно распарсить; caller решает, recoverable это или fatal.
    ParseFrameError(String),

    /// Codec parser/decoder вернул generic ошибку.
    Decoder(String),

    /// Backend VAAPI/cros layer вернул ошибку.
    Backend(String),
}

impl fmt::Display for VaapiAdapterDecodeError {
    /// Форматирует ошибку без потери typed причины.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEnoughOutputBuffers(needed) => {
                write!(
                    formatter,
                    "not enough output buffers available, need {needed} more"
                )
            }
            Self::CheckEvents => formatter.write_str("decoder requested event drain"),
            Self::ParseFrameError(message) => write!(formatter, "parse frame error: {message}"),
            Self::Decoder(message) => write!(formatter, "decoder error: {message}"),
            Self::Backend(message) => write!(formatter, "backend error: {message}"),
        }
    }
}

impl std::error::Error for VaapiAdapterDecodeError {}

impl From<DecodeError> for VaapiAdapterDecodeError {
    /// Переводит cros-codecs error в локальный typed enum.
    fn from(error: DecodeError) -> Self {
        match error {
            DecodeError::NotEnoughOutputBuffers(needed) => Self::NotEnoughOutputBuffers(needed),
            DecodeError::CheckEvents => Self::CheckEvents,
            DecodeError::ParseFrameError(message) => Self::ParseFrameError(message),
            DecodeError::DecoderError(error) => Self::Decoder(format!("{error:#}")),
            DecodeError::BackendError(error) => Self::Backend(format!("{error:?}")),
        }
    }
}

/// Common internal contract всех VAAPI codec adapters.
pub(crate) trait VaapiCodecAdapter {
    /// Codec, которым владеет adapter.
    fn codec(&self) -> VideoCodec;

    /// Имя backend-а для UI и diagnostics.
    fn backend_name(&self) -> &'static str;

    /// Короткое имя codec-а для логов decode loop-а.
    fn codec_label(&self) -> &'static str;

    /// Отправляет один encoded packet в backend decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError>;

    /// Flush-ит adapter-owned codec state.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError>;

    /// Забирает следующий pending decoder event.
    fn next_event(&mut self) -> Option<VaapiDecoderEvent>;

    /// Возвращает последний stream info после codec parser/format change.
    fn stream_info(&self) -> Option<VaapiAdapterStreamInfo>;
}

/// Production VP9 adapter поверх существующего cros-codecs decoder-а.
struct Vp9VaapiCodecAdapter {
    /// cros-codecs stateless decoder спрятан за adapter trait-object.
    inner: cros_codecs::decoder::stateless::DynStatelessVideoDecoder<InternalVaapiFrame>,
}

impl Vp9VaapiCodecAdapter {
    /// Создаёт VP9 decoder для уже открытого VA display.
    fn new(display: Rc<Display>) -> Result<Self> {
        type VaapiVp9Decoder = cros_codecs::decoder::stateless::StatelessDecoder<
            cros_codecs::decoder::stateless::vp9::Vp9,
            cros_codecs::backend::vaapi::decoder::VaapiBackend<InternalVaapiFrame>,
        >;

        let decoder = VaapiVp9Decoder::new_vaapi(display, BlockingMode::Blocking)
            .map_err(|error| anyhow::anyhow!("Failed to create VA-API VP9 decoder: {error:?}"))?;

        Ok(Self {
            inner: decoder.into_trait_object(),
        })
    }
}

impl VaapiCodecAdapter for Vp9VaapiCodecAdapter {
    /// Сообщает codec production adapter-а.
    fn codec(&self) -> VideoCodec {
        VideoCodec::Vp9
    }

    /// Возвращает старое имя backend-а для сохранения UI/log совместимости.
    fn backend_name(&self) -> &'static str {
        "VA-API VP9"
    }

    /// Возвращает codec label для сообщений retry-loop-а.
    fn codec_label(&self) -> &'static str {
        "VP9"
    }

    /// Делегирует packet submit в cros-codecs и управляет output surface allocation.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        let mut alloc_cb = || {
            let frame = frame_pool.alloc_or_allocate();
            if frame.is_none() {
                tracing::warn!("Frame pool exhausted; decoder needs more output buffers");
            }
            frame
        };

        self.inner
            .decode(timestamp_us, packet_data, &mut alloc_cb)
            .map_err(VaapiAdapterDecodeError::from)
    }

    /// Flush-ит текущий VP9 decoder state.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// Возвращает следующий cros event в локальном wrapper-е.
    fn next_event(&mut self) -> Option<VaapiDecoderEvent> {
        self.inner.next_event().map(VaapiDecoderEvent::from)
    }

    /// Возвращает stream info без раскрытия cros type-а наружу module-а.
    fn stream_info(&self) -> Option<VaapiAdapterStreamInfo> {
        self.inner.stream_info().map(VaapiAdapterStreamInfo::from)
    }
}

/// Factory/registry production adapter-ов VAAPI backend-а.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct VaapiCodecAdapterFactory;

impl VaapiCodecAdapterFactory {
    /// Создаёт production adapter для текущего backend default-а.
    pub(crate) fn create_default_adapter(
        display: Rc<Display>,
    ) -> Result<Box<dyn VaapiCodecAdapter>> {
        Ok(Box::new(Vp9VaapiCodecAdapter::new(display)?))
    }

    /// Возвращает typed отказ, если stream config не входит в implemented adapter matrix.
    pub(crate) fn stream_config_rejection(
        config: &VideoStreamDecodeConfig,
    ) -> Option<VideoStreamConfigRejection> {
        match config.codec {
            VideoCodec::Vp9 => reject_unsupported_vp9_config(config),
            VideoCodec::H264 => reject_h264_stub_config(config),
            codec @ (VideoCodec::Av1 | VideoCodec::H265 | VideoCodec::Vp8) => {
                Some(VideoStreamConfigRejection::UnsupportedCodec { codec })
            }
        }
    }

    /// Проверяет, что probed hardware format имеет production adapter в этом crate-е.
    pub(crate) fn supports_decode_format(format: &SupportedVideoDecodeFormat) -> bool {
        match (format.codec, format.profile) {
            (VideoCodec::Vp9, VideoProfile::Vp9(Vp9Profile::Profile0)) => {
                format.bit_depth == BitDepth::Eight && format.chroma == ChromaSubsampling::Yuv420
            }
            (VideoCodec::Vp9, VideoProfile::Vp9(Vp9Profile::Profile2)) => {
                format.bit_depth == BitDepth::Ten && format.chroma == ChromaSubsampling::Yuv420
            }
            _ => false,
        }
    }
}

/// Валидирует VP9 config против production adapter matrix.
fn reject_unsupported_vp9_config(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    if let Some(profile) = config.profile {
        let profile_rejection = match profile {
            VideoProfile::Vp9(Vp9Profile::Profile0) => {
                reject_optional_bit_depth(config.bit_depth, BitDepth::Eight)
                    .or_else(|| reject_optional_chroma(config.chroma, ChromaSubsampling::Yuv420))
                    .or_else(|| {
                        reject_optional_surface(config.surface_format, VideoSurfaceFormat::Nv12)
                    })
            }
            VideoProfile::Vp9(Vp9Profile::Profile2) => {
                reject_optional_bit_depth(config.bit_depth, BitDepth::Ten)
                    .or_else(|| reject_optional_chroma(config.chroma, ChromaSubsampling::Yuv420))
                    .or_else(|| {
                        reject_optional_surface(config.surface_format, VideoSurfaceFormat::P010)
                    })
            }
            VideoProfile::Vp9(_) => {
                Some(VideoStreamConfigRejection::UnsupportedProfile { profile })
            }
            profile => Some(VideoStreamConfigRejection::UnsupportedProfile { profile }),
        };
        if profile_rejection.is_some() {
            return profile_rejection;
        }
    } else if let Some(rejection) = reject_vp9_without_profile(config) {
        return Some(rejection);
    }

    if config.packetization.is_some() {
        return Some(VideoStreamConfigRejection::BackendUnsupported {
            reason: "VP9 VA-API adapter does not accept codec-specific packetization metadata"
                .to_string(),
        });
    }

    None
}

/// Валидирует VP9 config, когда profile ещё не доказан до packet-level refinement.
fn reject_vp9_without_profile(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
    if let Some(bit_depth) = config.bit_depth
        && !matches!(bit_depth, BitDepth::Eight | BitDepth::Ten)
    {
        return Some(VideoStreamConfigRejection::UnsupportedBitDepth { bit_depth });
    }

    if let Some(chroma) = config.chroma
        && chroma != ChromaSubsampling::Yuv420
    {
        return Some(VideoStreamConfigRejection::UnsupportedChroma { chroma });
    }

    if let Some(surface_format) = config.surface_format
        && !matches!(
            surface_format,
            VideoSurfaceFormat::Nv12 | VideoSurfaceFormat::P010
        )
    {
        return Some(VideoStreamConfigRejection::UnsupportedSurfaceFormat { surface_format });
    }

    None
}

/// Валидирует H.264 slot и всегда закрывает production decode до готового adapter-а.
fn reject_h264_stub_config(config: &VideoStreamDecodeConfig) -> Option<VideoStreamConfigRejection> {
    if let Some(profile) = config.profile
        && !matches!(profile, VideoProfile::H264(_))
    {
        return Some(VideoStreamConfigRejection::UnsupportedProfile { profile });
    }

    if let Some(rejection) = reject_optional_bit_depth(config.bit_depth, BitDepth::Eight)
        .or_else(|| reject_optional_chroma(config.chroma, ChromaSubsampling::Yuv420))
        .or_else(|| reject_optional_surface(config.surface_format, VideoSurfaceFormat::Nv12))
    {
        return Some(rejection);
    }

    if !matches!(
        config.packetization,
        Some(VideoStreamPacketization::H264(_))
    ) {
        return Some(VideoStreamConfigRejection::MissingPacketization {
            codec: VideoCodec::H264,
        });
    }

    Some(VideoStreamConfigRejection::BackendUnsupported {
        reason: "H.264 VA-API adapter slot exists, but production decode is not implemented yet"
            .to_string(),
    })
}

/// Проверяет optional bit depth на точное expected значение.
fn reject_optional_bit_depth(
    bit_depth: Option<BitDepth>,
    expected: BitDepth,
) -> Option<VideoStreamConfigRejection> {
    bit_depth
        .filter(|bit_depth| *bit_depth != expected)
        .map(|bit_depth| VideoStreamConfigRejection::UnsupportedBitDepth { bit_depth })
}

/// Проверяет optional chroma на точное expected значение.
fn reject_optional_chroma(
    chroma: Option<ChromaSubsampling>,
    expected: ChromaSubsampling,
) -> Option<VideoStreamConfigRejection> {
    chroma
        .filter(|chroma| *chroma != expected)
        .map(|chroma| VideoStreamConfigRejection::UnsupportedChroma { chroma })
}

/// Проверяет optional decoded surface format на точное expected значение.
fn reject_optional_surface(
    surface_format: Option<VideoSurfaceFormat>,
    expected: VideoSurfaceFormat,
) -> Option<VideoStreamConfigRejection> {
    surface_format
        .filter(|surface_format| *surface_format != expected)
        .map(
            |surface_format| VideoStreamConfigRejection::UnsupportedSurfaceFormat {
                surface_format,
            },
        )
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use codec_core::{H264NalLengthSize, H264Packetization, H264Profile, VideoMemoryContract};
    use media_core::TrackId;

    use super::*;

    /// Собирает stream config с production zero-copy memory contract.
    fn stream_config(codec: VideoCodec) -> VideoStreamDecodeConfig {
        VideoStreamDecodeConfig {
            track_id: TrackId::new(1),
            codec,
            profile: None,
            bit_depth: None,
            chroma: None,
            coded_width: Some(1920),
            coded_height: Some(1080),
            surface_format: None,
            memory_contract: VideoMemoryContract::dma_buf_zero_copy(),
            codec_private: None,
            packetization: None,
        }
    }

    /// Проверяет, что VP9 Profile 0 входит в production adapter matrix.
    #[test]
    fn factory_accepts_vp9_profile0_stream_config() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::Vp9(Vp9Profile::Profile0)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            surface_format: Some(VideoSurfaceFormat::Nv12),
            ..stream_config(VideoCodec::Vp9)
        };

        assert!(VaapiCodecAdapterFactory::stream_config_rejection(&config).is_none());
    }

    /// Проверяет, что VP9 Profile 1 не рекламируется как скрытый production path.
    #[test]
    fn factory_rejects_unimplemented_vp9_profile1() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::Vp9(Vp9Profile::Profile1)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv422),
            ..stream_config(VideoCodec::Vp9)
        };

        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::UnsupportedProfile {
                profile: VideoProfile::Vp9(Vp9Profile::Profile1)
            })
        ));
    }

    /// Проверяет H.264 stub: metadata slot есть, но production packets ещё запрещены.
    #[test]
    fn factory_keeps_h264_slot_unsupported_after_packetization_is_known() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H264(H264Profile::High)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            surface_format: Some(VideoSurfaceFormat::Nv12),
            codec_private: Some(Bytes::from_static(b"avcC")),
            packetization: Some(VideoStreamPacketization::H264(
                H264Packetization::AvccLengthPrefixed {
                    nal_length_size: H264NalLengthSize::FOUR,
                },
            )),
            ..stream_config(VideoCodec::H264)
        };

        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::BackendUnsupported { reason })
                if reason.contains("production decode is not implemented")
        ));
    }

    /// Проверяет typed отказ до H.264 packetization proof.
    #[test]
    fn factory_requires_h264_packetization_before_stub_rejection() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H264(H264Profile::Main)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            surface_format: Some(VideoSurfaceFormat::Nv12),
            ..stream_config(VideoCodec::H264)
        };

        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::MissingPacketization {
                codec: VideoCodec::H264
            })
        ));
    }

    /// Проверяет production capability matrix adapter-а без hardware probe.
    #[test]
    fn implemented_format_matrix_contains_only_vp9_profile0_and_profile2() {
        let supported = SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile2),
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            max_width: None,
            max_height: None,
            max_fps: None,
            hdr_input: true,
            backend: codec_core::DecodeBackendId::vaapi(),
        };
        let rejected = SupportedVideoDecodeFormat {
            profile: VideoProfile::H264(H264Profile::High),
            codec: VideoCodec::H264,
            bit_depth: BitDepth::Eight,
            hdr_input: false,
            ..supported.clone()
        };

        assert!(VaapiCodecAdapterFactory::supports_decode_format(&supported));
        assert!(!VaapiCodecAdapterFactory::supports_decode_format(&rejected));
    }
}

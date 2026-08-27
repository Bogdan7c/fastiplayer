use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use codec_core::{
    Av1Profile, BitDepth, ChromaSubsampling, H264Packetization, H264ParameterSetInjection,
    H264Profile, H265NalUnit, H265Packetization, H265ParameterSetInjection, H265Profile,
    SupportedVideoDecodeFormat, VideoCodec, VideoProfile, Vp9Profile,
    h264_access_unit_to_annex_b_into, h265_access_unit_to_annex_b_into,
    h265_decode_requirement_from_hevc_decoder_configuration_record, h265_nal_units,
    parse_avc_decoder_configuration_record, parse_avc3_decoder_configuration_record,
    parse_hevc_decoder_configuration_record, video_frame_pixel_layout_from_decode_requirement,
};
use cros_codecs::DecodedFormat;
use cros_codecs::backend::vaapi::decoder::VaapiBackend;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::h265::H265;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
use cros_codecs::decoder::{
    BlockingMode, DecodedDmaBufExportLayout, DecodedDmaBufImage, DecodedHandle, DecoderEvent,
    DynDecodedHandle, StreamInfo,
};
use cros_codecs::libva::Display;
use video_core::{VideoStreamConfigRejection, VideoStreamDecodeConfig, VideoStreamPacketization};
use video_frame_contract::{HardwareFrameHandle, VideoFramePixelLayout, VideoFrameTransferPath};

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

    /// Неблокирующе проверяет, готова ли VA surface к безопасному reclaim.
    ///
    /// Наружу adapter boundary возвращает только `Result<bool>`: raw VA status
    /// остаётся внутри cros/libva слоя.
    pub(crate) fn surface_ready(&self) -> Result<bool> {
        self.inner.try_is_ready()
    }

    /// Экспортирует VA surface как DMA-BUF descriptor в layout-е выбранного frame contract-а.
    pub(crate) fn dma_buf_image_with_layout(
        &self,
        preferred_layout: DecodedDmaBufExportLayout,
    ) -> Result<Option<DecodedDmaBufImage>> {
        self.inner.dma_buf_image_with_layout(preferred_layout)
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

/// Packet-local decode hints на internal VAAPI adapter boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VaapiPacketDecodeHints {
    /// AU должен получить codec parameter sets перед payload-ом.
    ///
    /// H.264 трактует это как SPS/PPS; будущий H.265 adapter будет трактовать
    /// тот же intent как VPS/SPS/PPS без переименования boundary.
    pub(crate) inject_parameter_sets: bool,
}

impl<H> From<DecoderEvent<H>> for VaapiDecoderEvent
where
    H: DecodedHandle<Frame = InternalVaapiFrame> + 'static,
{
    /// Скрывает cros event enum за локальным adapter event-ом.
    fn from(event: DecoderEvent<H>) -> Self {
        match event {
            DecoderEvent::FrameReady(handle) => {
                Self::FrameReady(VaapiDecodedFrameHandle::new(Box::new(handle)))
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

    /// Проверяет, можно ли переиспользовать adapter для нового stream config-а.
    ///
    /// Default `false` сохраняет безопасную политику для stateful/config-sensitive
    /// codec-ов, пока конкретный adapter не зафиксировал exact reuse contract.
    fn can_reuse_for_config(&self, _config: &VideoStreamDecodeConfig) -> bool {
        false
    }

    /// Имя backend-а для UI и diagnostics.
    fn backend_name(&self) -> &'static str;

    /// Короткое имя codec-а для логов decode loop-а.
    fn codec_label(&self) -> &'static str;

    /// Отправляет один encoded packet в backend decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError>;

    /// Flush-ит adapter-owned codec state.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError>;

    /// Дожимает codec tail при EOF без передачи этого намерения как seek flush.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError>;

    /// Забирает следующий pending decoder event.
    fn next_event(&mut self) -> Option<VaapiDecoderEvent>;

    /// Возвращает последний stream info после codec parser/format change.
    fn stream_info(&self) -> Option<VaapiAdapterStreamInfo>;
}

mod av1;
mod factory;
mod h264;
mod h265;
mod vp9;

pub(crate) use factory::VaapiCodecAdapterFactory;

#[cfg(test)]
use h264::{H264AccessUnitPreparer, H264PendingAccessUnit, H264VaapiStreamConfig};
#[cfg(test)]
use h265::{
    H265_NAL_UNIT_TYPE_PPS, H265_NAL_UNIT_TYPE_SPS, H265_NAL_UNIT_TYPE_VPS, H265AccessUnitPreparer,
    H265PendingAccessUnit, H265VaapiStreamConfig,
};
#[cfg(test)]
use vp9::vp9_can_reuse_for_config;
#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;

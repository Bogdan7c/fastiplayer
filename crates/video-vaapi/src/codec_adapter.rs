use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use codec_core::{
    BitDepth, ChromaSubsampling, H264Packetization, H264ParameterSetInjection, H265NalUnit,
    H265Packetization, H265ParameterSetInjection, H265Profile, SupportedVideoDecodeFormat,
    VideoCodec, VideoProfile, Vp9Profile, h264_access_unit_to_annex_b_into,
    h265_access_unit_to_annex_b_into,
    h265_decode_requirement_from_hevc_decoder_configuration_record, h265_nal_units,
    parse_avc_decoder_configuration_record, parse_hevc_decoder_configuration_record,
    video_frame_pixel_layout_from_decode_requirement,
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
pub(crate) mod test_support {
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
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use codec_core::{
        H264NalLengthSize, H264Packetization, H264Profile, H265NalLengthSize, H265Profile,
    };
    use media_core::TrackId;
    use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};

    use super::test_support::{FakeSurfaceReadiness, fake_decoded_frame_handle};
    use super::*;

    /// Test-only adapter, который использует default reuse policy из trait-а.
    struct DefaultReusePolicyAdapter;

    impl VaapiCodecAdapter for DefaultReusePolicyAdapter {
        /// Возвращает codec только для полноты тестового adapter contract-а.
        fn codec(&self) -> VideoCodec {
            VideoCodec::H264
        }

        /// Возвращает стабильное имя fake backend-а.
        fn backend_name(&self) -> &'static str {
            "test"
        }

        /// Возвращает стабильный codec label для fake diagnostics.
        fn codec_label(&self) -> &'static str {
            "test"
        }

        /// Имитирует полный consume packet-а без VA-API.
        fn submit_packet(
            &mut self,
            _timestamp_us: u64,
            packet_data: &[u8],
            _decode_hints: VaapiPacketDecodeHints,
            _frame_pool: &mut DmaFramePool,
        ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
            Ok(packet_data.len())
        }

        /// Fake adapter не держит codec state.
        fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
            Ok(())
        }

        /// Fake adapter не держит DPB tail.
        fn begin_end_of_stream_drain(
            &mut self,
        ) -> std::result::Result<(), VaapiAdapterDecodeError> {
            Ok(())
        }

        /// Fake adapter не публикует events.
        fn next_event(&mut self) -> Option<VaapiDecoderEvent> {
            None
        }

        /// Fake adapter не сообщает stream info.
        fn stream_info(&self) -> Option<VaapiAdapterStreamInfo> {
            None
        }
    }

    /// Проверяет, что `surface_ready()` возвращает `true` без blocking sync.
    #[test]
    fn surface_ready_returns_true() {
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(true));

        assert!(
            handle.surface_ready().expect("ready query должен пройти"),
            "ready surface должна вернуться как true"
        );
        assert!(
            !sync_called.get(),
            "surface_ready не должен вызывать sync()"
        );
    }

    /// Проверяет, что busy surface остаётся `false`, а не освобождается как ready.
    #[test]
    fn surface_ready_returns_false() {
        let (handle, sync_called) = fake_decoded_frame_handle(FakeSurfaceReadiness::Ready(false));

        assert!(
            !handle.surface_ready().expect("busy query должен пройти"),
            "busy surface должна вернуться как false"
        );
        assert!(
            !sync_called.get(),
            "surface_ready не должен вызывать sync()"
        );
    }

    /// Проверяет, что query error пробрасывается наружу и не превращается в `true`.
    #[test]
    fn surface_ready_propagates_query_error() {
        let (handle, sync_called) =
            fake_decoded_frame_handle(FakeSurfaceReadiness::QueryError("synthetic query failure"));

        let error = handle
            .surface_ready()
            .expect_err("query error должен остаться Err");

        assert!(
            error.to_string().contains("synthetic query failure"),
            "surface_ready должен сохранить текст ошибки query"
        );
        assert!(
            !sync_called.get(),
            "surface_ready не должен вызывать sync()"
        );
    }

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
            display_orientation: codec_core::VideoDisplayOrientation::Identity,
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
            codec_private: None,
            packetization: None,
        }
    }

    fn dma_buf_contract_for_surface(surface_format: VideoFramePixelLayout) -> VideoFrameContract {
        match surface_format {
            VideoFramePixelLayout::Nv12 => {
                VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers)
            }
            VideoFramePixelLayout::P010 => {
                VideoFrameContract::dma_buf_p010(DmaBufImageLayout::SeparateLayers)
            }
            other => VideoFrameContract {
                pixel_layout: other,
                transfer_path: video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload,
            },
        }
    }

    /// Собирает HEVC NAL unit без Annex B start code и без hvcC length prefix.
    fn h265_nal_unit(nal_unit_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut nal_unit = vec![(nal_unit_type & 0x3f) << 1, 0x01];
        nal_unit.extend_from_slice(payload);
        nal_unit
    }

    /// Минимальный VPS для adapter-level injection tests.
    fn h265_vps() -> Vec<u8> {
        h265_nal_unit(H265_NAL_UNIT_TYPE_VPS, &[0x01, 0x60])
    }

    /// Минимальный SPS payload для adapter-level injection tests.
    fn h265_sps() -> Vec<u8> {
        h265_nal_unit(H265_NAL_UNIT_TYPE_SPS, &[0x01, 0x01])
    }

    /// Минимальный PPS payload для adapter-level injection tests.
    fn h265_pps() -> Vec<u8> {
        h265_nal_unit(H265_NAL_UNIT_TYPE_PPS, &[0xc0])
    }

    /// Минимальный slice NAL для lifecycle tests без попытки software decode.
    fn h265_slice() -> Vec<u8> {
        h265_nal_unit(19, &[0x88])
    }

    /// Один hvcC array с NAL units одинакового типа.
    struct H265HvccArray<'a> {
        /// HEVC `nal_unit_type` array-а.
        nal_unit_type: u8,

        /// NAL units без length-prefix/start-code.
        nal_units: &'a [Vec<u8>],
    }

    /// Собирает minimal hvcC record с optional VPS/SPS/PPS arrays.
    fn h265_hvcc(
        nal_length_size: u8,
        profile_idc: u8,
        chroma_format_idc: u8,
        bit_depth: u8,
        arrays: &[H265HvccArray<'_>],
    ) -> Bytes {
        let mut record_bytes = vec![
            1,
            profile_idc & 0x1f,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            120,
            0xf0,
            0x00,
            0xfc,
            0xfc | (chroma_format_idc & 0x03),
            0xf8 | ((bit_depth - 8) & 0x07),
            0xf8 | ((bit_depth - 8) & 0x07),
            0,
            0,
            0b0000_1100 | ((nal_length_size - 1) & 0x03),
            arrays.len() as u8,
        ];

        set_h265_profile_compatibility_flag(&mut record_bytes, profile_idc);
        for array in arrays {
            record_bytes.push(array.nal_unit_type & 0x3f);
            record_bytes.extend_from_slice(&(array.nal_units.len() as u16).to_be_bytes());
            for nal_unit in array.nal_units {
                record_bytes.extend_from_slice(&(nal_unit.len() as u16).to_be_bytes());
                record_bytes.extend_from_slice(nal_unit);
            }
        }

        Bytes::from(record_bytes)
    }

    /// Включает compatibility bit, который codec-core использует для HEVC profile matching.
    fn set_h265_profile_compatibility_flag(record_bytes: &mut [u8], profile_idc: u8) {
        let flag_index = usize::from(profile_idc);
        let byte_index = 2 + flag_index / 8;
        let bit_index = 7 - (flag_index % 8);
        record_bytes[byte_index] |= 1 << bit_index;
    }

    /// Собирает length-prefixed HEVC access unit с 4-byte NAL lengths.
    fn h265_hvcc_access_unit(nal_units: &[&[u8]]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            let nal_len = u32::try_from(nal_unit.len()).expect("test NAL length fits u32");
            access_unit.extend_from_slice(&nal_len.to_be_bytes());
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    /// Собирает valid H.265 decode config для factory/preparer tests.
    fn h265_stream_decode_config(
        profile: H265Profile,
        bit_depth: BitDepth,
        surface_format: VideoFramePixelLayout,
        codec_private: Option<Bytes>,
    ) -> VideoStreamDecodeConfig {
        VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H265(profile)),
            bit_depth: Some(bit_depth),
            chroma: Some(ChromaSubsampling::Yuv420),
            frame_contract: dma_buf_contract_for_surface(surface_format),
            codec_private,
            packetization: Some(VideoStreamPacketization::H265(
                H265Packetization::HvccLengthPrefixed {
                    nal_length_size: H265NalLengthSize::FOUR,
                },
            )),
            ..stream_config(VideoCodec::H265)
        }
    }

    /// Собирает H.265 stream config с hvcC parameter sets для adapter policy tests.
    fn h265_stream_config_with_parameter_sets() -> H265VaapiStreamConfig {
        let video_parameter_set = h265_vps();
        let sequence_parameter_set = h265_sps();
        let picture_parameter_set = h265_pps();
        let codec_private = h265_hvcc(
            4,
            1,
            1,
            8,
            &[
                H265HvccArray {
                    nal_unit_type: H265_NAL_UNIT_TYPE_VPS,
                    nal_units: std::slice::from_ref(&video_parameter_set),
                },
                H265HvccArray {
                    nal_unit_type: H265_NAL_UNIT_TYPE_SPS,
                    nal_units: std::slice::from_ref(&sequence_parameter_set),
                },
                H265HvccArray {
                    nal_unit_type: H265_NAL_UNIT_TYPE_PPS,
                    nal_units: std::slice::from_ref(&picture_parameter_set),
                },
            ],
        );
        let config = h265_stream_decode_config(
            H265Profile::Main,
            BitDepth::Eight,
            VideoFramePixelLayout::Nv12,
            Some(codec_private),
        );

        H265VaapiStreamConfig::from_decode_config(&config)
            .expect("valid test hvcC должен проходить H.265 configure boundary")
    }

    /// Проверяет безопасную default reuse policy для config-sensitive adapters.
    #[test]
    fn default_adapter_reuse_policy_rejects_every_config() {
        let adapter = DefaultReusePolicyAdapter;

        assert!(!adapter.can_reuse_for_config(&stream_config(VideoCodec::H264)));
        assert!(!adapter.can_reuse_for_config(&stream_config(VideoCodec::H265)));
        assert!(!adapter.can_reuse_for_config(&stream_config(VideoCodec::Vp9)));
    }

    /// Проверяет, что VP9 explicitly сохраняет старый same-codec configure reuse.
    #[test]
    fn vp9_reuse_policy_accepts_only_vp9_configs() {
        assert!(vp9_can_reuse_for_config(&stream_config(VideoCodec::Vp9)));
        assert!(!vp9_can_reuse_for_config(&stream_config(VideoCodec::H264)));
        assert!(!vp9_can_reuse_for_config(&stream_config(VideoCodec::H265)));
    }

    /// Проверяет, что VP9 Profile 0 входит в production adapter matrix.
    #[test]
    fn factory_accepts_vp9_profile0_stream_config() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::Vp9(Vp9Profile::Profile0)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            frame_contract: dma_buf_contract_for_surface(VideoFramePixelLayout::Nv12),
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

    /// Проверяет, что H.265 Main/Main10 config входит в adapter construction matrix.
    #[test]
    fn factory_accepts_h265_main_and_main10_stream_configs() {
        let main_config = h265_stream_decode_config(
            H265Profile::Main,
            BitDepth::Eight,
            VideoFramePixelLayout::Nv12,
            Some(h265_hvcc(4, 1, 1, 8, &[])),
        );
        let main10_config = h265_stream_decode_config(
            H265Profile::Main10,
            BitDepth::Ten,
            VideoFramePixelLayout::P010,
            Some(h265_hvcc(4, 2, 1, 10, &[])),
        );

        assert!(VaapiCodecAdapterFactory::stream_config_rejection(&main_config).is_none());
        assert!(H265VaapiStreamConfig::from_decode_config(&main_config).is_ok());
        assert!(VaapiCodecAdapterFactory::stream_config_rejection(&main10_config).is_none());
        assert!(H265VaapiStreamConfig::from_decode_config(&main10_config).is_ok());
    }

    /// Проверяет, что H.265 capability включается только для validated Main/Main10 matrix.
    #[test]
    fn factory_advertises_validated_h265_main_and_main10_formats() {
        let h265_main = SupportedVideoDecodeFormat {
            codec: VideoCodec::H265,
            profile: VideoProfile::H265(H265Profile::Main),
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv420,
            max_width: Some(3840),
            max_height: Some(2160),
            max_fps: None,
            hdr_input: false,
        };
        let h265_main10 = SupportedVideoDecodeFormat {
            profile: VideoProfile::H265(H265Profile::Main10),
            bit_depth: BitDepth::Ten,
            hdr_input: true,
            ..h265_main.clone()
        };
        let rejected_main_wrong_depth = SupportedVideoDecodeFormat {
            bit_depth: BitDepth::Ten,
            hdr_input: true,
            ..h265_main.clone()
        };
        let rejected_future_profile = SupportedVideoDecodeFormat {
            profile: VideoProfile::H265(H265Profile::Main444),
            chroma: ChromaSubsampling::Yuv444,
            ..h265_main.clone()
        };

        assert!(VaapiCodecAdapterFactory::supports_decode_format(&h265_main));
        assert!(VaapiCodecAdapterFactory::supports_decode_format(
            &h265_main10
        ));
        assert!(!VaapiCodecAdapterFactory::supports_decode_format(
            &rejected_main_wrong_depth
        ));
        assert!(!VaapiCodecAdapterFactory::supports_decode_format(
            &rejected_future_profile
        ));
    }

    /// Собирает минимальный avcC record с SPS/PPS для configure-boundary tests.
    fn valid_h264_avcc_private() -> Bytes {
        Bytes::from_static(&[1, 100, 0, 31, 0xff, 0xe1, 0, 2, 0x67, 0x64, 1, 0, 1, 0x68])
    }

    /// Собирает H.264 stream config с AVCC packetization для adapter policy tests.
    fn h264_stream_config() -> H264VaapiStreamConfig {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H264(H264Profile::High)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            frame_contract: dma_buf_contract_for_surface(VideoFramePixelLayout::Nv12),
            codec_private: Some(valid_h264_avcc_private()),
            packetization: Some(VideoStreamPacketization::H264(
                H264Packetization::AvccLengthPrefixed {
                    nal_length_size: H264NalLengthSize::FOUR,
                },
            )),
            ..stream_config(VideoCodec::H264)
        };

        H264VaapiStreamConfig::from_decode_config(&config)
            .expect("valid test avcC должен проходить H.264 configure boundary")
    }

    /// Собирает AVCC access unit с 4-byte NAL lengths.
    fn avcc_access_unit(nal_units: &[&[u8]]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            let nal_len = u32::try_from(nal_unit.len()).expect("test NAL length fits u32");
            access_unit.extend_from_slice(&nal_len.to_be_bytes());
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    /// Имитирует полный consume AU перед возвратом buffer-а в scratch.
    fn recycle_fully_consumed_access_unit(
        preparer: &mut H264AccessUnitPreparer,
        mut pending_access_unit: H264PendingAccessUnit,
    ) {
        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| Ok(remaining_bytes.len()))
            .expect("test AU должен полностью consume-иться");
        assert_eq!(accepted_len, Some(pending_access_unit.source_packet_len));
        preparer.recycle_completed_access_unit(pending_access_unit);
    }

    /// Имитирует полный consume H.265 AU перед возвратом buffer-а в scratch.
    fn recycle_fully_consumed_h265_access_unit(
        preparer: &mut H265AccessUnitPreparer,
        mut pending_access_unit: H265PendingAccessUnit,
    ) {
        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| Ok(remaining_bytes.len()))
            .expect("test H.265 AU должен полностью consume-иться");
        assert_eq!(accepted_len, Some(pending_access_unit.source_packet_len));
        preparer.recycle_completed_access_unit(pending_access_unit);
    }

    /// Собирает Annex B access unit из NAL units без start code.
    fn annex_b_access_unit(nal_units: &[&[u8]]) -> Vec<u8> {
        let mut access_unit = Vec::new();
        for nal_unit in nal_units {
            access_unit.extend_from_slice(&[0, 0, 0, 1]);
            access_unit.extend_from_slice(nal_unit);
        }
        access_unit
    }

    /// Возвращает размер первого Annex B NAL-а в переданном suffix-е.
    fn first_annex_b_nal_len(bytes: &[u8]) -> usize {
        bytes[4..]
            .windows(4)
            .position(|window| window == [0, 0, 0, 1])
            .map_or(bytes.len(), |position| position + 4)
    }

    /// Проверяет typed отказ, когда H.265 packetization не доказана.
    #[test]
    fn factory_rejects_h265_missing_or_wrong_packetization() {
        let mut config = h265_stream_decode_config(
            H265Profile::Main,
            BitDepth::Eight,
            VideoFramePixelLayout::Nv12,
            Some(h265_hvcc(4, 1, 1, 8, &[])),
        );

        config.packetization = None;
        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::MissingPacketization {
                codec: VideoCodec::H265
            })
        ));

        config.packetization = Some(VideoStreamPacketization::H264(
            H264Packetization::AvccLengthPrefixed {
                nal_length_size: H264NalLengthSize::FOUR,
            },
        ));
        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::MissingPacketization {
                codec: VideoCodec::H265
            })
        ));
    }

    /// Проверяет, что incomplete hvcC не мешает принять in-band VPS/SPS/PPS.
    #[test]
    fn h265_incomplete_hvcc_accepts_in_band_parameter_sets() {
        let config = h265_stream_decode_config(
            H265Profile::Main,
            BitDepth::Eight,
            VideoFramePixelLayout::Nv12,
            Some(h265_hvcc(4, 1, 1, 8, &[])),
        );

        assert!(VaapiCodecAdapterFactory::stream_config_rejection(&config).is_none());
        let stream_config = H265VaapiStreamConfig::from_decode_config(&config)
            .expect("incomplete hvcC должен проходить configure boundary");
        let mut preparer = H265AccessUnitPreparer::new(stream_config);
        let video_parameter_set = h265_vps();
        let sequence_parameter_set = h265_sps();
        let picture_parameter_set = h265_pps();
        let slice = h265_slice();
        let first_access_unit = h265_hvcc_access_unit(&[
            &video_parameter_set,
            &sequence_parameter_set,
            &picture_parameter_set,
            &slice,
        ]);

        let first_pending = preparer
            .prepare_pending_access_unit(&first_access_unit, VaapiPacketDecodeHints::default())
            .expect("first H.265 AU с in-band parameter sets должен собираться");

        assert_eq!(preparer.stream_config.video_parameter_sets.len(), 1);
        assert_eq!(preparer.stream_config.sequence_parameter_sets.len(), 1);
        assert_eq!(preparer.stream_config.picture_parameter_sets.len(), 1);
        assert_eq!(
            first_pending.annex_b_bytes,
            annex_b_access_unit(&[
                &video_parameter_set,
                &sequence_parameter_set,
                &picture_parameter_set,
                &slice,
            ])
        );
        recycle_fully_consumed_h265_access_unit(&mut preparer, first_pending);

        let second_slice = h265_slice();
        let second_access_unit = h265_hvcc_access_unit(&[&second_slice]);
        let second_pending = preparer
            .prepare_pending_access_unit(
                &second_access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("known in-band parameter sets должны inject-иться на следующем AU");

        assert_eq!(
            second_pending.annex_b_bytes,
            annex_b_access_unit(&[
                &video_parameter_set,
                &sequence_parameter_set,
                &picture_parameter_set,
                &second_slice,
            ])
        );
    }

    /// Проверяет hev1-style path: hvcC отсутствует, но length-prefixed AU несёт parameter sets.
    #[test]
    fn h265_hev1_style_in_band_parameter_sets_are_accepted() {
        let config = h265_stream_decode_config(
            H265Profile::Main,
            BitDepth::Eight,
            VideoFramePixelLayout::Nv12,
            None,
        );
        let stream_config = H265VaapiStreamConfig::from_decode_config(&config)
            .expect("hev1-style config без hvcC должен проходить при доказанной packetization");
        let mut preparer = H265AccessUnitPreparer::new(stream_config);
        let video_parameter_set = h265_vps();
        let sequence_parameter_set = h265_sps();
        let picture_parameter_set = h265_pps();
        let slice = h265_slice();
        let access_unit = h265_hvcc_access_unit(&[
            &video_parameter_set,
            &sequence_parameter_set,
            &picture_parameter_set,
            &slice,
        ]);

        assert!(VaapiCodecAdapterFactory::stream_config_rejection(&config).is_none());
        assert!(
            preparer
                .prepare_pending_access_unit(&access_unit, VaapiPacketDecodeHints::default())
                .is_ok()
        );
        assert_eq!(preparer.stream_config.video_parameter_sets.len(), 1);
        assert_eq!(preparer.stream_config.sequence_parameter_sets.len(), 1);
        assert_eq!(preparer.stream_config.picture_parameter_sets.len(), 1);
    }

    /// Проверяет typed отказы для HEVC форматов вне Main/Main10 4:2:0.
    #[test]
    fn h265_rejects_unsupported_chroma_profiles_and_bit_depth() {
        let chroma_422_config = VideoStreamDecodeConfig {
            chroma: Some(ChromaSubsampling::Yuv422),
            ..h265_stream_decode_config(
                H265Profile::Main,
                BitDepth::Eight,
                VideoFramePixelLayout::Nv12,
                Some(h265_hvcc(4, 1, 1, 8, &[])),
            )
        };
        let main444_config = h265_stream_decode_config(
            H265Profile::Main444,
            BitDepth::Eight,
            VideoFramePixelLayout::Nv12,
            Some(h265_hvcc(4, 1, 3, 8, &[])),
        );
        let twelve_bit_config = h265_stream_decode_config(
            H265Profile::Main,
            BitDepth::Twelve,
            VideoFramePixelLayout::Nv12,
            Some(h265_hvcc(4, 1, 1, 12, &[])),
        );

        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&chroma_422_config),
            Some(VideoStreamConfigRejection::UnsupportedChroma {
                chroma: ChromaSubsampling::Yuv422
            })
        ));
        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&main444_config),
            Some(VideoStreamConfigRejection::UnsupportedProfile {
                profile: VideoProfile::H265(H265Profile::Main444)
            })
        ));
        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&twelve_bit_config),
            Some(VideoStreamConfigRejection::UnsupportedBitDepth {
                bit_depth: BitDepth::Twelve
            })
        ));
    }

    /// Проверяет VPS/SPS/PPS injection из hvcC перед H.265 AU payload-ом.
    #[test]
    fn h265_keyframe_access_unit_injects_parameter_sets() {
        let mut preparer = H265AccessUnitPreparer::new(h265_stream_config_with_parameter_sets());
        let video_parameter_set = h265_vps();
        let sequence_parameter_set = h265_sps();
        let picture_parameter_set = h265_pps();
        let slice = h265_slice();
        let access_unit = h265_hvcc_access_unit(&[&slice]);

        let pending_access_unit = preparer
            .prepare_pending_access_unit(
                &access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("H.265 AU должен собираться с VPS/SPS/PPS injection");

        assert_eq!(
            pending_access_unit.annex_b_bytes,
            annex_b_access_unit(&[
                &video_parameter_set,
                &sequence_parameter_set,
                &picture_parameter_set,
                &slice,
            ])
        );
    }

    /// Проверяет, что первый AU после flush снова получает VPS/SPS/PPS injection.
    #[test]
    fn h265_first_access_unit_after_flush_injects_parameter_sets() {
        let mut preparer = H265AccessUnitPreparer::new(h265_stream_config_with_parameter_sets());
        let video_parameter_set = h265_vps();
        let sequence_parameter_set = h265_sps();
        let picture_parameter_set = h265_pps();
        let first_slice = h265_slice();
        let first_access_unit = h265_hvcc_access_unit(&[&first_slice]);
        let first_pending = preparer
            .prepare_pending_access_unit(&first_access_unit, VaapiPacketDecodeHints::default())
            .expect("first H.265 AU должен получить lifecycle injection");
        recycle_fully_consumed_h265_access_unit(&mut preparer, first_pending);

        preparer.reset_after_flush();
        let second_slice = h265_slice();
        let second_access_unit = h265_hvcc_access_unit(&[&second_slice]);
        let second_pending = preparer
            .prepare_pending_access_unit(&second_access_unit, VaapiPacketDecodeHints::default())
            .expect("first H.265 AU after flush должен получить lifecycle injection");

        assert_eq!(
            second_pending.annex_b_bytes,
            annex_b_access_unit(&[
                &video_parameter_set,
                &sequence_parameter_set,
                &picture_parameter_set,
                &second_slice,
            ])
        );
    }

    /// Проверяет, что H.265 backpressure держит bytes в pending AU до полного consume.
    #[test]
    fn h265_output_backpressure_preserves_pending_bytes_until_complete_consume() {
        let mut preparer = H265AccessUnitPreparer::new(h265_stream_config_with_parameter_sets());
        let first_slice = h265_slice();
        let first_access_unit = h265_hvcc_access_unit(&[&first_slice]);
        let first_pending = preparer
            .prepare_pending_access_unit(
                &first_access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("first H.265 AU должен собираться");
        let first_capacity = first_pending.annex_b_bytes.capacity();
        recycle_fully_consumed_h265_access_unit(&mut preparer, first_pending);
        assert_eq!(preparer.annex_b_scratch.capacity(), first_capacity);

        let retry_slice = h265_slice();
        let retry_access_unit = h265_hvcc_access_unit(&[&retry_slice]);
        let mut pending_access_unit = preparer
            .prepare_pending_access_unit(&retry_access_unit, VaapiPacketDecodeHints::default())
            .expect("retry H.265 AU должен собираться");
        let pending_bytes_before_backpressure = pending_access_unit.annex_b_bytes.clone();
        let pending_capacity = pending_access_unit.annex_b_bytes.capacity();
        assert_eq!(preparer.annex_b_scratch.capacity(), 0);

        let backpressure_result = pending_access_unit.feed_until_blocked(|_remaining_bytes| {
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        });

        assert!(matches!(
            backpressure_result,
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        ));
        assert_eq!(
            pending_access_unit.annex_b_bytes,
            pending_bytes_before_backpressure
        );
        assert_eq!(
            pending_access_unit.annex_b_bytes.capacity(),
            pending_capacity
        );
        assert_eq!(preparer.annex_b_scratch.capacity(), 0);

        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| Ok(remaining_bytes.len()))
            .expect("pending H.265 AU должен завершиться после retry");
        assert_eq!(accepted_len, Some(retry_access_unit.len()));

        preparer.recycle_completed_access_unit(pending_access_unit);
        assert_eq!(preparer.annex_b_scratch.capacity(), pending_capacity);
        assert!(preparer.annex_b_scratch.is_empty());
    }

    /// Проверяет, что `CheckEvents` оставляет H.265 offset на том же NAL-е для retry.
    #[test]
    fn h265_check_events_retry_does_not_double_consume_input() {
        let video_parameter_set = h265_vps();
        let slice = h265_slice();
        let annex_b = annex_b_access_unit(&[&video_parameter_set, &slice]);
        let source_packet_len = annex_b.len();
        let mut pending_access_unit = H265PendingAccessUnit::new(annex_b, source_packet_len);
        let mut first_attempts = 0usize;

        let first_result = pending_access_unit.feed_until_blocked(|_remaining_bytes| {
            first_attempts += 1;
            Err(VaapiAdapterDecodeError::CheckEvents)
        });

        assert!(matches!(
            first_result,
            Err(VaapiAdapterDecodeError::CheckEvents)
        ));
        assert_eq!(pending_access_unit.consumed_bytes, 0);
        assert_eq!(first_attempts, 1);

        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| Ok(first_annex_b_nal_len(remaining_bytes)))
            .unwrap();

        assert_eq!(accepted_len, Some(source_packet_len));
    }

    /// Проверяет H.264 adapter matrix: metadata slot теперь production-ready.
    #[test]
    fn factory_accepts_h264_after_packetization_and_avcc_are_known() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H264(H264Profile::High)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            frame_contract: dma_buf_contract_for_surface(VideoFramePixelLayout::Nv12),
            codec_private: Some(valid_h264_avcc_private()),
            packetization: Some(VideoStreamPacketization::H264(
                H264Packetization::AvccLengthPrefixed {
                    nal_length_size: H264NalLengthSize::FOUR,
                },
            )),
            ..stream_config(VideoCodec::H264)
        };

        assert!(VaapiCodecAdapterFactory::stream_config_rejection(&config).is_none());
    }

    /// Проверяет typed отказ до H.264 packetization proof.
    #[test]
    fn factory_requires_h264_packetization_before_stub_rejection() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H264(H264Profile::Main)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            frame_contract: dma_buf_contract_for_surface(VideoFramePixelLayout::Nv12),
            ..stream_config(VideoCodec::H264)
        };

        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::MissingPacketization {
                codec: VideoCodec::H264
            })
        ));
    }

    /// Проверяет, что обычный non-keyframe AU после старта не получает SPS/PPS.
    #[test]
    fn h264_non_keyframe_access_unit_after_start_uses_no_parameter_set_injection() {
        let mut preparer = H264AccessUnitPreparer::new(h264_stream_config());
        let first_access_unit = avcc_access_unit(&[&[0x65, 0x88]]);
        let first_pending = preparer
            .prepare_pending_access_unit(
                &first_access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("first H.264 AU должен собираться");
        recycle_fully_consumed_access_unit(&mut preparer, first_pending);

        let non_keyframe_access_unit = avcc_access_unit(&[&[0x41, 0x9a]]);
        let pending_access_unit = preparer
            .prepare_pending_access_unit(
                &non_keyframe_access_unit,
                VaapiPacketDecodeHints::default(),
            )
            .expect("non-keyframe H.264 AU должен собираться");

        assert_eq!(
            pending_access_unit.annex_b_bytes,
            annex_b_access_unit(&[&[0x41, 0x9a]])
        );
    }

    /// Проверяет, что keyframe AU получает SPS/PPS даже не будучи первым после configure.
    #[test]
    fn h264_keyframe_access_unit_injects_parameter_sets() {
        let mut preparer = H264AccessUnitPreparer::new(h264_stream_config());
        let first_access_unit = avcc_access_unit(&[&[0x41, 0x9a]]);
        let first_pending = preparer
            .prepare_pending_access_unit(&first_access_unit, VaapiPacketDecodeHints::default())
            .expect("first H.264 AU должен собираться");
        recycle_fully_consumed_access_unit(&mut preparer, first_pending);

        let keyframe_access_unit = avcc_access_unit(&[&[0x65, 0x88]]);
        let pending_access_unit = preparer
            .prepare_pending_access_unit(
                &keyframe_access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("keyframe H.264 AU должен собираться");

        assert_eq!(
            pending_access_unit.annex_b_bytes,
            annex_b_access_unit(&[&[0x67, 0x64], &[0x68], &[0x65, 0x88]])
        );
    }

    /// Проверяет, что первый AU после flush снова получает SPS/PPS.
    #[test]
    fn h264_first_access_unit_after_flush_injects_parameter_sets() {
        let mut preparer = H264AccessUnitPreparer::new(h264_stream_config());
        let first_access_unit = avcc_access_unit(&[&[0x65, 0x88]]);
        let first_pending = preparer
            .prepare_pending_access_unit(
                &first_access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("first H.264 AU должен собираться");
        recycle_fully_consumed_access_unit(&mut preparer, first_pending);

        preparer.reset_after_flush();
        let post_flush_access_unit = avcc_access_unit(&[&[0x41, 0x9a]]);
        let pending_access_unit = preparer
            .prepare_pending_access_unit(&post_flush_access_unit, VaapiPacketDecodeHints::default())
            .expect("post-flush H.264 AU должен собираться");

        assert_eq!(
            pending_access_unit.annex_b_bytes,
            annex_b_access_unit(&[&[0x67, 0x64], &[0x68], &[0x41, 0x9a]])
        );
    }

    /// Проверяет, что backpressure держит bytes в pending AU до полного consume.
    #[test]
    fn h264_output_backpressure_preserves_pending_bytes_until_complete_consume() {
        let mut preparer = H264AccessUnitPreparer::new(h264_stream_config());
        let first_access_unit = avcc_access_unit(&[&[0x65, 0x88]]);
        let first_pending = preparer
            .prepare_pending_access_unit(
                &first_access_unit,
                VaapiPacketDecodeHints {
                    inject_parameter_sets: true,
                },
            )
            .expect("first H.264 AU должен собираться");
        let first_capacity = first_pending.annex_b_bytes.capacity();
        recycle_fully_consumed_access_unit(&mut preparer, first_pending);
        assert_eq!(preparer.annex_b_scratch.capacity(), first_capacity);

        let retry_access_unit = avcc_access_unit(&[&[0x41, 0x9a]]);
        let mut pending_access_unit = preparer
            .prepare_pending_access_unit(&retry_access_unit, VaapiPacketDecodeHints::default())
            .expect("retry H.264 AU должен собираться");
        let pending_bytes_before_backpressure = pending_access_unit.annex_b_bytes.clone();
        let pending_capacity = pending_access_unit.annex_b_bytes.capacity();
        assert_eq!(preparer.annex_b_scratch.capacity(), 0);

        let backpressure_result = pending_access_unit.feed_until_blocked(|_remaining_bytes| {
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        });

        assert!(matches!(
            backpressure_result,
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        ));
        assert_eq!(
            pending_access_unit.annex_b_bytes,
            pending_bytes_before_backpressure
        );
        assert_eq!(
            pending_access_unit.annex_b_bytes.capacity(),
            pending_capacity
        );
        assert_eq!(preparer.annex_b_scratch.capacity(), 0);

        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| Ok(remaining_bytes.len()))
            .expect("pending AU должен завершиться после retry");
        assert_eq!(accepted_len, Some(retry_access_unit.len()));

        preparer.recycle_completed_access_unit(pending_access_unit);
        assert_eq!(preparer.annex_b_scratch.capacity(), pending_capacity);
        assert!(preparer.annex_b_scratch.is_empty());
    }

    /// Проверяет H.264 AU feeder: один packet может состоять из нескольких NAL units.
    #[test]
    fn h264_pending_access_unit_consumes_all_nals_before_accepting_packet() {
        let annex_b = annex_b_access_unit(&[&[0x67, 0x64], &[0x68], &[0x65, 0x88]]);
        let mut pending_access_unit = H264PendingAccessUnit::new(annex_b.clone(), 17);
        let mut submitted_nals = Vec::new();

        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| {
                let consumed_len = first_annex_b_nal_len(remaining_bytes);
                submitted_nals.push(remaining_bytes[..consumed_len].to_vec());
                Ok(consumed_len)
            })
            .unwrap();

        assert_eq!(accepted_len, Some(17));
        assert_eq!(pending_access_unit.consumed_bytes, annex_b.len());
        assert_eq!(submitted_nals.len(), 3);
    }

    /// Проверяет, что `CheckEvents` оставляет offset на том же NAL-е для retry.
    #[test]
    fn h264_check_events_retry_does_not_double_consume_input() {
        let annex_b = annex_b_access_unit(&[&[0x67, 0x64], &[0x65, 0x88]]);
        let mut pending_access_unit = H264PendingAccessUnit::new(annex_b, 11);
        let mut first_attempts = 0usize;

        let first_result = pending_access_unit.feed_until_blocked(|_remaining_bytes| {
            first_attempts += 1;
            Err(VaapiAdapterDecodeError::CheckEvents)
        });

        assert!(matches!(
            first_result,
            Err(VaapiAdapterDecodeError::CheckEvents)
        ));
        assert_eq!(pending_access_unit.consumed_bytes, 0);
        assert_eq!(first_attempts, 1);

        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| Ok(first_annex_b_nal_len(remaining_bytes)))
            .unwrap();

        assert_eq!(accepted_len, Some(11));
    }

    /// Проверяет, что output-buffer pressure сохраняет текущий NAL для retry.
    #[test]
    fn h264_not_enough_output_buffers_preserves_pending_access_unit() {
        let annex_b = annex_b_access_unit(&[&[0x65, 0x88]]);
        let mut pending_access_unit = H264PendingAccessUnit::new(annex_b.clone(), 5);

        let first_result = pending_access_unit.feed_until_blocked(|_remaining_bytes| {
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        });

        assert!(matches!(
            first_result,
            Err(VaapiAdapterDecodeError::NotEnoughOutputBuffers(1))
        ));
        assert_eq!(pending_access_unit.consumed_bytes, 0);

        let accepted_len = pending_access_unit
            .feed_until_blocked(|remaining_bytes| {
                assert_eq!(remaining_bytes, annex_b.as_slice());
                Ok(remaining_bytes.len())
            })
            .unwrap();

        assert_eq!(accepted_len, Some(5));
    }

    /// Проверяет production capability matrix adapter-а без hardware probe.
    #[test]
    fn implemented_format_matrix_contains_vp9_h264_and_h265_main_main10() {
        let supported_vp9 = SupportedVideoDecodeFormat {
            codec: VideoCodec::Vp9,
            profile: VideoProfile::Vp9(Vp9Profile::Profile2),
            bit_depth: BitDepth::Ten,
            chroma: ChromaSubsampling::Yuv420,
            max_width: None,
            max_height: None,
            max_fps: None,
            hdr_input: true,
        };
        let supported_h264 = SupportedVideoDecodeFormat {
            profile: VideoProfile::H264(H264Profile::High),
            codec: VideoCodec::H264,
            bit_depth: BitDepth::Eight,
            hdr_input: false,
            ..supported_vp9.clone()
        };
        let rejected_h264_high10 = SupportedVideoDecodeFormat {
            profile: VideoProfile::H264(H264Profile::High),
            codec: VideoCodec::H264,
            bit_depth: BitDepth::Ten,
            hdr_input: true,
            ..supported_vp9.clone()
        };
        let supported_h265_main = SupportedVideoDecodeFormat {
            profile: VideoProfile::H265(H265Profile::Main),
            codec: VideoCodec::H265,
            bit_depth: BitDepth::Eight,
            hdr_input: false,
            ..supported_vp9.clone()
        };
        let supported_h265_main10 = SupportedVideoDecodeFormat {
            profile: VideoProfile::H265(H265Profile::Main10),
            codec: VideoCodec::H265,
            bit_depth: BitDepth::Ten,
            hdr_input: true,
            ..supported_vp9.clone()
        };
        let rejected_h265_main444 = SupportedVideoDecodeFormat {
            profile: VideoProfile::H265(H265Profile::Main444),
            codec: VideoCodec::H265,
            bit_depth: BitDepth::Eight,
            chroma: ChromaSubsampling::Yuv444,
            hdr_input: false,
            ..supported_vp9.clone()
        };

        assert!(VaapiCodecAdapterFactory::supports_decode_format(
            &supported_vp9
        ));
        assert!(VaapiCodecAdapterFactory::supports_decode_format(
            &supported_h264
        ));
        assert!(!VaapiCodecAdapterFactory::supports_decode_format(
            &rejected_h264_high10
        ));
        assert!(VaapiCodecAdapterFactory::supports_decode_format(
            &supported_h265_main
        ));
        assert!(VaapiCodecAdapterFactory::supports_decode_format(
            &supported_h265_main10
        ));
        assert!(!VaapiCodecAdapterFactory::supports_decode_format(
            &rejected_h265_main444
        ));
    }
}

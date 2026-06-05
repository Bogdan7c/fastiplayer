use std::fmt;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::Result;
use codec_core::{
    BitDepth, ChromaSubsampling, H264Packetization, H264ParameterSetInjection,
    SupportedVideoDecodeFormat, VideoCodec, VideoProfile, VideoSurfaceFormat, Vp9Profile,
    h264_access_unit_to_annex_b_into, parse_avc_decoder_configuration_record,
};
use cros_codecs::DecodedFormat;
use cros_codecs::backend::vaapi::decoder::VaapiBackend;
use cros_codecs::decoder::stateless::h264::H264;
use cros_codecs::decoder::stateless::{DecodeError, StatelessDecoder, StatelessVideoDecoder};
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

/// H.264 stream metadata, доказанная на configure boundary.
struct H264VaapiStreamConfig {
    /// Packetization player/demuxer уже подтвердили через `codec-core`.
    packetization: H264Packetization,

    /// SPS NAL units из codec-private без Annex B start code.
    sequence_parameter_sets: Vec<Vec<u8>>,

    /// PPS NAL units из codec-private без Annex B start code.
    picture_parameter_sets: Vec<Vec<u8>>,
}

impl H264VaapiStreamConfig {
    /// Строит backend-local config только из уже принятого neutral stream config-а.
    fn from_decode_config(
        config: &VideoStreamDecodeConfig,
    ) -> std::result::Result<Self, VideoStreamConfigRejection> {
        let packetization = match config.packetization {
            Some(VideoStreamPacketization::H264(packetization)) => packetization,
            _ => {
                return Err(VideoStreamConfigRejection::MissingPacketization {
                    codec: VideoCodec::H264,
                });
            }
        };

        let codec_private = config
            .codec_private
            .as_deref()
            .filter(|bytes| !bytes.is_empty())
            .ok_or_else(|| VideoStreamConfigRejection::InvalidCodecPrivate {
                codec: VideoCodec::H264,
                reason: "H.264 VA-API adapter requires avcC codec_private with SPS/PPS".to_string(),
            })?;

        let decoder_config =
            parse_avc_decoder_configuration_record(codec_private).map_err(|error| {
                VideoStreamConfigRejection::InvalidCodecPrivate {
                    codec: VideoCodec::H264,
                    reason: error.to_string(),
                }
            })?;

        if let H264Packetization::AvccLengthPrefixed { nal_length_size } = packetization
            && decoder_config.nal_length_size != nal_length_size
        {
            return Err(VideoStreamConfigRejection::InvalidCodecPrivate {
                codec: VideoCodec::H264,
                reason: format!(
                    "H.264 packetization length size {nal_length_size:?} does not match avcC {:?}",
                    decoder_config.nal_length_size
                ),
            });
        }

        Ok(Self {
            packetization,
            sequence_parameter_sets: decoder_config.sequence_parameter_sets().to_vec(),
            picture_parameter_sets: decoder_config.picture_parameter_sets().to_vec(),
        })
    }

    /// Конвертирует один access unit в caller-owned Annex B buffer по явному intent.
    fn access_unit_to_annex_b_into(
        &self,
        packet_data: &[u8],
        inject_parameter_sets: bool,
        output: &mut Vec<u8>,
    ) -> std::result::Result<(), VaapiAdapterDecodeError> {
        let parameter_set_injection = if inject_parameter_sets {
            H264ParameterSetInjection::BeforeAccessUnit {
                sequence_parameter_sets: &self.sequence_parameter_sets,
                picture_parameter_sets: &self.picture_parameter_sets,
            }
        } else {
            H264ParameterSetInjection::None
        };

        h264_access_unit_to_annex_b_into(
            packet_data,
            self.packetization,
            parameter_set_injection,
            output,
        )
        .map_err(|error| VaapiAdapterDecodeError::ParseFrameError(error.to_string()))
    }
}

/// Готовит H.264 AU к submit-у и владеет reusable Annex B scratch buffer.
struct H264AccessUnitPreparer {
    /// Configure-time packetization и SPS/PPS state.
    stream_config: H264VaapiStreamConfig,

    /// Reusable output buffer для AVCC/Annex B перепаковки.
    annex_b_scratch: Vec<u8>,

    /// Lifecycle flag: первый AU после configure/flush должен получить SPS/PPS.
    inject_parameter_sets_on_next_au: bool,
}

impl H264AccessUnitPreparer {
    /// Создаёт preparer после configure; первый AU остаётся decode-safe.
    fn new(stream_config: H264VaapiStreamConfig) -> Self {
        Self {
            stream_config,
            annex_b_scratch: Vec::new(),
            inject_parameter_sets_on_next_au: true,
        }
    }

    /// Собирает pending AU, перемещая reusable scratch в ownership pending state-а.
    fn prepare_pending_access_unit(
        &mut self,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
    ) -> std::result::Result<H264PendingAccessUnit, VaapiAdapterDecodeError> {
        let inject_parameter_sets =
            decode_hints.inject_parameter_sets || self.inject_parameter_sets_on_next_au;

        self.stream_config.access_unit_to_annex_b_into(
            packet_data,
            inject_parameter_sets,
            &mut self.annex_b_scratch,
        )?;
        self.inject_parameter_sets_on_next_au = false;

        let annex_b_bytes = std::mem::take(&mut self.annex_b_scratch);
        Ok(H264PendingAccessUnit::new(annex_b_bytes, packet_data.len()))
    }

    /// Возвращает полностью consumed AU buffer в scratch и сохраняет его capacity.
    fn recycle_completed_access_unit(&mut self, pending_access_unit: H264PendingAccessUnit) {
        debug_assert_eq!(
            pending_access_unit.consumed_bytes,
            pending_access_unit.annex_b_bytes.len(),
            "H.264 AU buffer can return to scratch only after full consume"
        );
        self.annex_b_scratch = pending_access_unit.into_reusable_annex_b_bytes();
        self.annex_b_scratch.clear();
    }

    /// Сбрасывает lifecycle policy после seek flush/reconfigure cleanup.
    fn reset_after_flush(&mut self) {
        self.annex_b_scratch.clear();
        self.inject_parameter_sets_on_next_au = true;
    }
}

/// Annex B access unit, который может быть partially consumed cros H.264 decoder-ом.
struct H264PendingAccessUnit {
    /// Полный Annex B payload с injected parameter sets.
    annex_b_bytes: Vec<u8>,

    /// Размер исходного packet-а на external adapter boundary.
    source_packet_len: usize,

    /// Сколько Annex B bytes уже принято cros decoder-ом.
    consumed_bytes: usize,
}

impl H264PendingAccessUnit {
    /// Создаёт pending AU до первого NAL submit-а.
    fn new(annex_b_bytes: Vec<u8>, source_packet_len: usize) -> Self {
        Self {
            annex_b_bytes,
            source_packet_len,
            consumed_bytes: 0,
        }
    }

    /// Возвращает owned bytes adapter-у после полного consume.
    fn into_reusable_annex_b_bytes(self) -> Vec<u8> {
        self.annex_b_bytes
    }

    /// Кормит cros decoder NAL-ами до полного AU или первого backpressure/error.
    fn feed_until_blocked(
        &mut self,
        mut decode_next_nal: impl FnMut(&[u8]) -> std::result::Result<usize, VaapiAdapterDecodeError>,
    ) -> std::result::Result<Option<usize>, VaapiAdapterDecodeError> {
        while self.consumed_bytes < self.annex_b_bytes.len() {
            let remaining_bytes = &self.annex_b_bytes[self.consumed_bytes..];
            let consumed_now = decode_next_nal(remaining_bytes)?;

            if consumed_now == 0 {
                return Err(VaapiAdapterDecodeError::Decoder(
                    "H.264 decoder accepted a NAL but reported 0 consumed bytes".to_string(),
                ));
            }
            if consumed_now > remaining_bytes.len() {
                return Err(VaapiAdapterDecodeError::Decoder(format!(
                    "H.264 decoder reported {consumed_now} consumed bytes for {} available bytes",
                    remaining_bytes.len()
                )));
            }

            self.consumed_bytes += consumed_now;
        }

        Ok(Some(self.source_packet_len))
    }
}

/// Production H.264 adapter поверх cros-codecs VAAPI decoder-а.
struct H264VaapiCodecAdapter {
    /// Concrete cros decoder остаётся private implementation detail adapter-а.
    inner: StatelessDecoder<H264, VaapiBackend<InternalVaapiFrame>>,

    /// Готовит AU, выбирает SPS/PPS injection policy и переиспользует scratch.
    access_unit_preparer: H264AccessUnitPreparer,

    /// Unconsumed AU после `CheckEvents` или output-buffer backpressure.
    pending_access_unit: Option<H264PendingAccessUnit>,
}

impl H264VaapiCodecAdapter {
    /// Создаёт H.264 decoder только для уже валидированного stream config-а.
    fn new(display: Rc<Display>, config: &VideoStreamDecodeConfig) -> Result<Self> {
        let stream_config = H264VaapiStreamConfig::from_decode_config(config)
            .map_err(|rejection| anyhow::anyhow!("Invalid H.264 VA-API config: {rejection}"))?;
        let inner = StatelessDecoder::<H264, VaapiBackend<InternalVaapiFrame>>::new_vaapi(
            display,
            BlockingMode::Blocking,
        )
        .map_err(|error| anyhow::anyhow!("Failed to create VA-API H.264 decoder: {error:?}"))?;

        Ok(Self {
            inner,
            access_unit_preparer: H264AccessUnitPreparer::new(stream_config),
            pending_access_unit: None,
        })
    }
}

impl VaapiCodecAdapter for H264VaapiCodecAdapter {
    /// Сообщает codec production adapter-а.
    fn codec(&self) -> VideoCodec {
        VideoCodec::H264
    }

    /// Возвращает backend label для H.264 diagnostics.
    fn backend_name(&self) -> &'static str {
        "VA-API H.264"
    }

    /// Возвращает codec label для сообщений retry-loop-а.
    fn codec_label(&self) -> &'static str {
        "H.264"
    }

    /// Конвертирует AU в Annex B и отправляет все NAL units в cros decoder.
    fn submit_packet(
        &mut self,
        timestamp_us: u64,
        packet_data: &[u8],
        decode_hints: VaapiPacketDecodeHints,
        frame_pool: &mut DmaFramePool,
    ) -> std::result::Result<usize, VaapiAdapterDecodeError> {
        if self.pending_access_unit.is_none() {
            self.pending_access_unit = Some(
                self.access_unit_preparer
                    .prepare_pending_access_unit(packet_data, decode_hints)?,
            );
        }

        let pending_access_unit = self
            .pending_access_unit
            .as_mut()
            .expect("pending access unit was just created");
        let feed_result = pending_access_unit.feed_until_blocked(|remaining_bytes| {
            let mut alloc_cb = || {
                let frame = frame_pool.alloc_or_allocate();
                if frame.is_none() {
                    tracing::warn!("Frame pool exhausted; H.264 decoder needs output buffers");
                }
                frame
            };

            self.inner
                .decode(timestamp_us, remaining_bytes, &mut alloc_cb)
                .map_err(VaapiAdapterDecodeError::from)
        });

        match feed_result {
            Ok(Some(source_packet_len)) => {
                let completed_access_unit = self
                    .pending_access_unit
                    .take()
                    .expect("completed H.264 access unit must still be pending");
                self.access_unit_preparer
                    .recycle_completed_access_unit(completed_access_unit);
                Ok(source_packet_len)
            }
            Ok(None) => unreachable!("H.264 feed loop always completes or returns an error"),
            Err(error) => Err(error),
        }
    }

    /// Flush-ит H.264 decoder state и забывает partially consumed AU.
    fn flush(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        self.pending_access_unit = None;
        self.access_unit_preparer.reset_after_flush();
        self.inner.flush().map_err(VaapiAdapterDecodeError::from)
    }

    /// Дожимает H.264 DPB tail; cros-codecs отдаёт tail frames через events.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        if self.pending_access_unit.is_some() {
            return Err(VaapiAdapterDecodeError::Decoder(
                "cannot drain H.264 while an access unit is partially submitted".to_string(),
            ));
        }

        self.access_unit_preparer.reset_after_flush();
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

/// Production VP9 adapter поверх существующего cros-codecs decoder-а.
struct Vp9VaapiCodecAdapter {
    /// cros-codecs stateless decoder спрятан за adapter trait-object.
    inner: cros_codecs::decoder::stateless::DynStatelessVideoDecoder<InternalVaapiFrame>,
}

/// VP9 сохраняет старую configure-семантику: same-codec config не пересоздаёт decoder.
fn vp9_can_reuse_for_config(config: &VideoStreamDecodeConfig) -> bool {
    config.codec == VideoCodec::Vp9
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

    /// Явно владеет VP9 reuse intent вместо внешней проверки codec-а в decoder-е.
    fn can_reuse_for_config(&self, config: &VideoStreamDecodeConfig) -> bool {
        vp9_can_reuse_for_config(config)
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
        _decode_hints: VaapiPacketDecodeHints,
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

    /// VP9 stateless path публикует готовые frames во время обычного decode loop-а.
    fn begin_end_of_stream_drain(&mut self) -> std::result::Result<(), VaapiAdapterDecodeError> {
        Ok(())
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

    /// Создаёт adapter, который соответствует уже принятому stream config-у.
    pub(crate) fn create_adapter_for_config(
        display: Rc<Display>,
        config: &VideoStreamDecodeConfig,
    ) -> Result<Box<dyn VaapiCodecAdapter>> {
        if let Some(rejection) = Self::stream_config_rejection(config) {
            return Err(anyhow::anyhow!(
                "Unsupported VA-API stream config: {rejection}"
            ));
        }

        match config.codec {
            VideoCodec::Vp9 => Ok(Box::new(Vp9VaapiCodecAdapter::new(display)?)),
            VideoCodec::H264 => Ok(Box::new(H264VaapiCodecAdapter::new(display, config)?)),
            codec @ (VideoCodec::Av1 | VideoCodec::H265 | VideoCodec::Vp8) => Err(anyhow::anyhow!(
                "VA-API adapter factory has no implemented adapter for {codec}"
            )),
        }
    }

    /// Возвращает typed отказ, если stream config не входит в implemented adapter matrix.
    pub(crate) fn stream_config_rejection(
        config: &VideoStreamDecodeConfig,
    ) -> Option<VideoStreamConfigRejection> {
        match config.codec {
            VideoCodec::Vp9 => reject_unsupported_vp9_config(config),
            VideoCodec::H264 => reject_unsupported_h264_config(config),
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
            (VideoCodec::H264, VideoProfile::H264(_)) => {
                format.bit_depth == BitDepth::Eight && format.chroma == ChromaSubsampling::Yuv420
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

/// Валидирует H.264 stream config против production adapter-а.
fn reject_unsupported_h264_config(
    config: &VideoStreamDecodeConfig,
) -> Option<VideoStreamConfigRejection> {
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

    H264VaapiStreamConfig::from_decode_config(config).err()
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
    use codec_core::{
        H264NalLengthSize, H264Packetization, H264Profile, H265Profile, VideoMemoryContract,
    };
    use media_core::TrackId;

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
            surface_format: None,
            memory_contract: VideoMemoryContract::dma_buf_zero_copy(),
            codec_private: None,
            packetization: None,
        }
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

    /// Проверяет, что H.265 stream config пока отсекается до adapter creation.
    #[test]
    fn factory_rejects_h265_until_vaapi_adapter_exists() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H265(H265Profile::Main)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            surface_format: Some(VideoSurfaceFormat::Nv12),
            ..stream_config(VideoCodec::H265)
        };

        assert!(matches!(
            VaapiCodecAdapterFactory::stream_config_rejection(&config),
            Some(VideoStreamConfigRejection::UnsupportedCodec {
                codec: VideoCodec::H265
            })
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
            surface_format: Some(VideoSurfaceFormat::Nv12),
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

    /// Проверяет H.264 adapter matrix: metadata slot теперь production-ready.
    #[test]
    fn factory_accepts_h264_after_packetization_and_avcc_are_known() {
        let config = VideoStreamDecodeConfig {
            profile: Some(VideoProfile::H264(H264Profile::High)),
            bit_depth: Some(BitDepth::Eight),
            chroma: Some(ChromaSubsampling::Yuv420),
            surface_format: Some(VideoSurfaceFormat::Nv12),
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
    fn implemented_format_matrix_contains_vp9_and_h264_8bit_yuv420() {
        let supported_vp9 = SupportedVideoDecodeFormat {
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
        let rejected_h265_main = SupportedVideoDecodeFormat {
            profile: VideoProfile::H265(H265Profile::Main),
            codec: VideoCodec::H265,
            bit_depth: BitDepth::Eight,
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
        assert!(!VaapiCodecAdapterFactory::supports_decode_format(
            &rejected_h265_main
        ));
    }
}

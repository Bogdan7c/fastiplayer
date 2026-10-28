//! Safe RAII owner for software-only `AVCodecContext`.

#[cfg(feature = "ffmpeg")]
use std::ffi::{CStr, CString};
#[cfg(feature = "ffmpeg")]
use std::ptr::{self, NonNull};

use codec_core::VideoCodec;
use video_core::{HostFrameMemoryKind, SoftwareDecodeThreadBudget};

#[cfg(feature = "ffmpeg")]
use self::callbacks::{
    CodecCallbacks, FormatNegotiator, decoder_supports_custom_frame_buffers,
    get_importable_frame_buffer, select_software_pixel_format,
};
use super::error::{FfiResult, FfmpegError};
use super::frame::OwnedAvFrame;
#[cfg(feature = "ffmpeg")]
use super::host_frame_memory::HostFrameBlockPool;
use super::packet::{OwnedAvPacket, PacketTimeBase};
use super::pixel_format::{SoftwarePixelFormat, SoftwarePixelFormatSet};
use crate::codec_adapter::FfmpegDecoderId;

/// Запрос на открытие FFmpeg codec context-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegCodecContextRequest {
    /// Decoder selection без raw FFmpeg enum в public API.
    decoder: FfmpegDecoderSelection,

    /// Software pixel formats, которые adapter уже разрешил.
    accepted_pixel_formats: SoftwarePixelFormatSet,

    /// Ограничение внутренней задержки decoder-а в кадрах, если backend его поддерживает.
    max_frame_delay: Option<u32>,

    /// Software decoder thread budget без raw FFmpeg `thread_count` sentinel-ов.
    software_decode_thread_budget: SoftwareDecodeThreadBudget,

    /// Codec-private global headers (например MP4 `avcC`/`hvcC`), которые
    /// нужно установить как `AVCodecContext.extradata` до `avcodec_open2`.
    /// Хранятся как нейтральные bytes; raw FFmpeg типы наружу не выносятся.
    extradata: Option<Vec<u8>>,

    /// Куда декодер кладёт кадры: обычное выделение FFmpeg или наши блоки для импорта в GPU.
    frame_memory: SoftwareFrameMemory,
}

/// Где декодер выделяет память кадров (cpu-parity 08b).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SoftwareFrameMemory {
    /// Обычное выделение libavcodec (`avcodec_default_get_buffer2`).
    #[default]
    DecoderDefault,

    /// Свой `get_buffer2`: каждый кадр в одном блоке, выровненном по странице, со stride,
    /// кратным 256, — renderer может импортировать его в GPU без CPU-копии.
    /// Ставится только декодерам с `AV_CODEC_CAP_DR1`; остальные остаются на
    /// `DecoderDefault` (например, libdav1d системного FFmpeg со своим пулом; у
    /// закреплённого FFmpeg DR1 даёт патч `ffmpeg-vendor`).
    ImportableBlocks {
        /// Какой памятью выделять блоки: анонимной (wgpu) или memfd (GLES2 dma-buf).
        memory: HostFrameMemoryKind,
    },
}

/// Project-owned decoder selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegDecoderSelection {
    /// Decoder selected by codec/profile adapter policy.
    DecoderId(FfmpegDecoderId),

    /// Explicit decoder name for diagnostics/tests and compatibility.
    DecoderName(String),
}

impl FfmpegCodecContextRequest {
    /// Создаёт compatibility request по decoder name.
    #[must_use]
    pub fn new(codec_name: impl Into<String>) -> Self {
        Self {
            decoder: FfmpegDecoderSelection::DecoderName(codec_name.into()),
            accepted_pixel_formats: SoftwarePixelFormatSet::v1_host_planar(),
            max_frame_delay: None,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::auto(),
            extradata: None,
            frame_memory: SoftwareFrameMemory::DecoderDefault,
        }
    }

    /// Создаёт production-shaped request по neutral `VideoCodec`.
    #[must_use]
    pub fn for_video_codec(
        video_codec: VideoCodec,
        accepted_pixel_formats: SoftwarePixelFormatSet,
    ) -> Self {
        Self::for_decoder_id(
            decoder_id_from_video_codec(video_codec),
            accepted_pixel_formats,
        )
    }

    /// Создаёт production-shaped request по adapter-approved decoder id.
    #[must_use]
    pub fn for_decoder_id(
        decoder_id: FfmpegDecoderId,
        accepted_pixel_formats: SoftwarePixelFormatSet,
    ) -> Self {
        Self {
            decoder: FfmpegDecoderSelection::DecoderId(decoder_id),
            accepted_pixel_formats,
            max_frame_delay: None,
            software_decode_thread_budget: SoftwareDecodeThreadBudget::auto(),
            extradata: None,
            frame_memory: SoftwareFrameMemory::DecoderDefault,
        }
    }

    /// Просит decoder ограничить внутренний frame delay, если такая AVOption есть.
    #[must_use]
    pub fn with_max_frame_delay(mut self, max_frame_delay: u32) -> Self {
        self.max_frame_delay = Some(max_frame_delay);
        self
    }

    /// Устанавливает software thread budget, уже разрешённый внешним budget layer-ом.
    #[must_use]
    pub fn with_software_decode_thread_budget(
        mut self,
        software_decode_thread_budget: SoftwareDecodeThreadBudget,
    ) -> Self {
        self.software_decode_thread_budget = software_decode_thread_budget;
        self
    }

    /// Прикрепляет codec-private global headers (`avcC`/`hvcC`), которые будут
    /// установлены как `AVCodecContext.extradata` перед `avcodec_open2`.
    #[must_use]
    pub fn with_extradata(mut self, extradata: Vec<u8>) -> Self {
        self.extradata = Some(extradata);
        self
    }

    /// Просит декодер класть кадры в блоки, которые GPU импортирует без CPU-копии.
    #[must_use]
    pub fn with_frame_memory(mut self, frame_memory: SoftwareFrameMemory) -> Self {
        self.frame_memory = frame_memory;
        self
    }

    /// Где декодер выделяет память кадров.
    #[must_use]
    pub const fn frame_memory(&self) -> SoftwareFrameMemory {
        self.frame_memory
    }

    /// Возвращает codec name для error/reporting layers.
    #[must_use]
    pub fn codec_name(&self) -> &str {
        match &self.decoder {
            FfmpegDecoderSelection::DecoderId(decoder_id) => decoder_id.codec_name(),
            FfmpegDecoderSelection::DecoderName(codec_name) => codec_name,
        }
    }

    /// Возвращает adapter-owned software pixel format policy.
    #[must_use]
    pub fn accepted_pixel_formats(&self) -> &SoftwarePixelFormatSet {
        &self.accepted_pixel_formats
    }

    /// Возвращает requested decoder frame-delay limit, если он задан.
    #[must_use]
    pub fn max_frame_delay(&self) -> Option<u32> {
        self.max_frame_delay
    }

    /// Возвращает requested software thread budget без FFmpeg-specific integer semantics.
    #[must_use]
    pub const fn software_decode_thread_budget(&self) -> SoftwareDecodeThreadBudget {
        self.software_decode_thread_budget
    }

    /// Возвращает codec-private global headers, если они заданы.
    #[must_use]
    pub fn extradata(&self) -> Option<&[u8]> {
        self.extradata.as_deref()
    }
}

/// Opaque owner для software-only `AVCodecContext`.
#[derive(Debug)]
pub struct CodecContext {
    /// Raw context живёт только внутри FFI boundary.
    #[cfg(feature = "ffmpeg")]
    raw_context: NonNull<ffmpeg_sys_next::AVCodecContext>,

    /// State pointed to by `AVCodecContext.opaque` for `get_format`/`get_buffer2`.
    #[cfg(feature = "ffmpeg")]
    callbacks: Box<CodecCallbacks>,

    /// Marker, чтобы type существовал в default build-е без FFmpeg headers/libs.
    #[cfg(not(feature = "ffmpeg"))]
    _feature_disabled: (),
}

#[cfg(feature = "ffmpeg")]
struct CodecOpenOptionsDictionary {
    /// Raw FFmpeg dictionary остаётся внутри FFI boundary и освобождается в Drop.
    raw_dictionary: *mut ffmpeg_sys_next::AVDictionary,
}

#[cfg(feature = "ffmpeg")]
impl CodecOpenOptionsDictionary {
    fn from_request(request: &FfmpegCodecContextRequest, decoder_name: &str) -> FfiResult<Self> {
        let mut dictionary = Self {
            raw_dictionary: ptr::null_mut(),
        };

        if decoder_name == "libdav1d"
            && let Some(max_frame_delay) = request.max_frame_delay()
        {
            dictionary.set_option("max_frame_delay", &max_frame_delay.to_string())?;
        }

        Ok(dictionary)
    }

    fn as_mut_ptr(&mut self) -> *mut *mut ffmpeg_sys_next::AVDictionary {
        if self.raw_dictionary.is_null() {
            ptr::null_mut()
        } else {
            &mut self.raw_dictionary
        }
    }

    fn set_option(&mut self, key: &'static str, value: &str) -> FfiResult<()> {
        let key = CString::new(key).map_err(|_| FfmpegError::InvalidInput {
            operation: "av_dict_set",
            details: "option key contains an interior NUL byte".to_owned(),
        })?;
        let value = CString::new(value).map_err(|_| FfmpegError::InvalidInput {
            operation: "av_dict_set",
            details: "option value contains an interior NUL byte".to_owned(),
        })?;

        let status = unsafe {
            // SAFETY: key/value are NUL-terminated and live for the call.
            // FFmpeg copies strings into the dictionary on success.
            ffmpeg_sys_next::av_dict_set(&mut self.raw_dictionary, key.as_ptr(), value.as_ptr(), 0)
        };

        if status < 0 {
            return Err(FfmpegError::from_averror("av_dict_set", status));
        }

        Ok(())
    }

    fn unused_option_count(&self) -> usize {
        unsafe {
            // SAFETY: dictionary pointer либо null, либо owned этим wrapper-ом.
            ffmpeg_sys_next::av_dict_count(self.raw_dictionary) as usize
        }
    }
}

#[cfg(feature = "ffmpeg")]
impl Drop for CodecOpenOptionsDictionary {
    fn drop(&mut self) {
        unsafe {
            // SAFETY: dictionary pointer либо null, либо owned этим wrapper-ом.
            ffmpeg_sys_next::av_dict_free(&mut self.raw_dictionary);
        }
    }
}

/// Backward-compatible alias для старого scaffold имени.
pub type FfmpegCodecContext = CodecContext;

impl CodecContext {
    /// Finds, allocates and opens a software-only decoder context.
    pub fn open(request: &FfmpegCodecContextRequest) -> FfiResult<Self> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _request = request;
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            let decoder = find_decoder(request)?;
            reject_hardware_decoder(decoder)?;
            let selected_decoder_name = decoder_name(decoder);

            // SAFETY: decoder pointer вернул FFmpeg registry и он valid/null-checked.
            // `avcodec_alloc_context3` возвращает owned context или null.
            let raw_context = unsafe { ffmpeg_sys_next::avcodec_alloc_context3(decoder) };
            let raw_context = NonNull::new(raw_context).ok_or(FfmpegError::AllocationFailed {
                operation: "avcodec_alloc_context3",
            })?;

            let frame_block_pool = match request.frame_memory {
                SoftwareFrameMemory::ImportableBlocks { memory }
                    if decoder_supports_custom_frame_buffers(decoder) =>
                {
                    Some(HostFrameBlockPool::new(memory))
                }
                SoftwareFrameMemory::ImportableBlocks { .. } => {
                    // Fail-closed: путь «без копии» просили, но декодер не принимает
                    // память приложения (например, libdav1d системного FFmpeg без патча
                    // `ffmpeg-vendor`). Кадры честно идут копией; причина — в логе один
                    // раз на открытие декодера, число копий — в счётчиках рендера.
                    log::info!(
                        "software decoder {selected_decoder_name} has no AV_CODEC_CAP_DR1: \
                         frames stay in FFmpeg memory and are uploaded with a CPU copy"
                    );
                    None
                }
                SoftwareFrameMemory::DecoderDefault => None,
            };
            let mut context_owner = Self {
                raw_context,
                callbacks: Box::new(CodecCallbacks {
                    format_negotiator: FormatNegotiator::new(
                        request.accepted_pixel_formats.clone(),
                    ),
                    frame_block_pool,
                }),
            };

            #[cfg(test)]
            LIVE_CODEC_CONTEXT_ALLOCATIONS.with(|live| live.set(live.get() + 1));

            // SAFETY: context принадлежит wrapper-у. `callbacks` находится в Box,
            // поэтому address стабилен до Drop. FFmpeg может вызывать callbacks
            // из разных decoder threads, но Context7/FFmpeg docs говорят, что не
            // одновременно; negotiator immutable, пул блоков сам под mutex-ом.
            unsafe {
                let context = raw_context.as_ptr();
                (*context).opaque = context_owner.callbacks.as_mut_ptr();
                (*context).get_format = Some(select_software_pixel_format);
                if context_owner.callbacks.frame_block_pool.is_some() {
                    (*context).get_buffer2 = Some(get_importable_frame_buffer);
                }
            }

            // Codec-private global headers (avcC/hvcC) должны попасть в decoder до
            // open: иначе H.264/H.265 length-prefixed packets парсятся как Annex B
            // и FFmpeg отвечает AVERROR_INVALIDDATA ("No start code is found").
            if let Some(extradata) = request.extradata()
                && let Err(error) =
                    assign_codec_context_extradata(context_owner.raw_context, extradata)
            {
                return Err(error);
            }

            let ffmpeg_thread_count =
                ffmpeg_thread_count_from_budget(request.software_decode_thread_budget())?;

            // Многопоточный software decode. Project-level `Auto` уже
            // резолвится в явное положительное число потоков, чтобы FFmpeg не
            // занимал все ядра и оставлял CPU headroom для render/upload/worker
            // путей. `Fixed(N)` используется только для явного playback/config
            // budget-а и передаёт FFmpeg конкретный `thread_count = N`.
            // thread_type разрешает frame/slice threading, а libavcodec сам
            // маскирует их по AVCodec.capabilities. Должно быть выставлено до
            // avcodec_open2.
            // SAFETY: context owned wrapper-ом и валиден до open; поля
            // thread_count/thread_type читаются FFmpeg только внутри open.
            unsafe {
                let context = context_owner.raw_context.as_ptr();
                (*context).thread_count = ffmpeg_thread_count;
                (*context).thread_type =
                    ffmpeg_sys_next::FF_THREAD_FRAME | ffmpeg_sys_next::FF_THREAD_SLICE;
            }

            let mut open_options =
                CodecOpenOptionsDictionary::from_request(request, &selected_decoder_name)?;

            // SAFETY: context и decoder pointer-ы валидны; options dictionary,
            // если задан, живёт до конца вызова. Context остаётся owned wrapper-ом.
            let status = unsafe {
                ffmpeg_sys_next::avcodec_open2(
                    context_owner.raw_context.as_ptr(),
                    decoder,
                    open_options.as_mut_ptr(),
                )
            };

            if status < 0 {
                return Err(FfmpegError::from_averror("avcodec_open2", status));
            }

            let unused_option_count = open_options.unused_option_count();
            if unused_option_count > 0 {
                return Err(FfmpegError::InvalidInput {
                    operation: "avcodec_open2",
                    details: format!(
                        "decoder `{selected_decoder_name}` did not consume {unused_option_count} codec open option(s)"
                    ),
                });
            }

            Ok(context_owner)
        }
    }

    /// Счётчики пула блоков нашего `get_buffer2`; `None` — кадры выделяет FFmpeg.
    #[cfg(all(test, feature = "ffmpeg"))]
    pub(crate) fn frame_block_pool_stats(
        &self,
    ) -> Option<super::host_frame_memory::HostFrameBlockPoolStats> {
        self.callbacks
            .frame_block_pool
            .as_ref()
            .map(|pool| pool.stats())
    }

    /// Selected pixel format after decoder negotiation, if known.
    #[must_use]
    pub fn selected_software_pixel_format(&self) -> Option<SoftwarePixelFormat> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            None
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain field.
            let pixel_format = unsafe { self.raw_context.as_ref().pix_fmt };

            SoftwarePixelFormat::from_av_pixel_format(pixel_format)
        }
    }

    /// Current context width.
    #[must_use]
    pub fn width(&self) -> i32 {
        #[cfg(not(feature = "ffmpeg"))]
        {
            0
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain field.
            unsafe { self.raw_context.as_ref().width }
        }
    }

    /// Current context height.
    #[must_use]
    pub fn height(&self) -> i32 {
        #[cfg(not(feature = "ffmpeg"))]
        {
            0
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: raw pointer валиден в течение `&self`; читаем plain field.
            unsafe { self.raw_context.as_ref().height }
        }
    }

    /// Передаёт один compressed packet в FFmpeg send/receive decoder.
    pub fn send_packet(&mut self, packet: &OwnedAvPacket) -> FfiResult<()> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _packet = packet;
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: context открыт `avcodec_open2`, packet pointer живёт до
            // конца вызова и остаётся owned caller-ом по контракту FFmpeg.
            let status = unsafe {
                ffmpeg_sys_next::avcodec_send_packet(self.raw_context.as_ptr(), packet.as_ptr())
            };

            if status < 0 {
                return Err(FfmpegError::from_averror("avcodec_send_packet", status));
            }

            Ok(())
        }
    }

    /// Объявляет decoder-у шкалу входных `AVPacket.pts/dts` до их отправки.
    pub fn set_packet_time_base(&mut self, time_base: PacketTimeBase) {
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _time_base = time_base;
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: context принадлежит живому wrapper-у; `pkt_timebase`
            // является caller-owned decode metadata по контракту libavcodec.
            let context = unsafe { self.raw_context.as_mut() };
            context.pkt_timebase = time_base.as_av_rational();
        }
    }

    /// Передаёт NULL packet, который переводит decoder в EOF/DPB drain mode.
    pub fn send_flush_packet(&mut self) -> FfiResult<()> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: context открыт `avcodec_open2`; null packet является
            // documented FFmpeg способом начать draining mode.
            let status = unsafe {
                ffmpeg_sys_next::avcodec_send_packet(self.raw_context.as_ptr(), ptr::null())
            };

            if status < 0 {
                return Err(FfmpegError::from_averror(
                    "avcodec_send_packet(NULL)",
                    status,
                ));
            }

            Ok(())
        }
    }

    /// Забирает следующий decoded frame из FFmpeg receive side.
    pub fn receive_frame(&mut self, frame: &mut OwnedAvFrame) -> FfiResult<()> {
        #[cfg(not(feature = "ffmpeg"))]
        {
            let _frame = frame;
            Err(FfmpegError::FeatureDisabled)
        }

        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: context открыт, frame allocation принадлежит caller-у и
            // передаётся FFmpeg как reusable receive buffer.
            let status = unsafe {
                ffmpeg_sys_next::avcodec_receive_frame(
                    self.raw_context.as_ptr(),
                    frame.as_mut_ptr(),
                )
            };

            if status < 0 {
                return Err(FfmpegError::from_averror("avcodec_receive_frame", status));
            }

            // Кадр из нашего блока запоминает блок: renderer импортирует его в GPU.
            if let Some(pool) = &self.callbacks.frame_block_pool {
                frame.attach_importable_block(pool);
            }

            Ok(())
        }
    }

    /// Сбрасывает decoder buffers после seek/lifecycle reset.
    pub fn flush_buffers(&mut self) {
        #[cfg(feature = "ffmpeg")]
        {
            // SAFETY: context открыт; `avcodec_flush_buffers` не освобождает сам
            // context, а сбрасывает внутренний decode state перед новым input.
            unsafe { ffmpeg_sys_next::avcodec_flush_buffers(self.raw_context.as_ptr()) };
        }
    }
}

#[cfg(feature = "ffmpeg")]
impl Drop for CodecContext {
    fn drop(&mut self) {
        free_context(self.raw_context);
        // Декодер закрыт: свободные блоки больше не нужны. Кадры, которые ещё держат
        // плеер или renderer, остаются валидными и освободят свои блоки сами.
        if let Some(pool) = &self.callbacks.frame_block_pool {
            pool.close();
        }

        #[cfg(test)]
        LIVE_CODEC_CONTEXT_ALLOCATIONS.with(|live| live.set(live.get().saturating_sub(1)));
    }
}

// Счётчик живых `CodecContext` только для тестов. Он thread-local, а не
// process-wide: `cargo test` гоняет тесты бинаря параллельно, и общий счётчик
// видел бы контексты чужих тестов (гонка `before=1, after=0`). Изоляция
// корректна, потому что `CodecContext: !Send + !Sync` (см. assert_not_impl_any
// в tests.rs): контекст создаётся и освобождается на одном и том же потоке,
// так что инкремент и декремент всегда попадают в один и тот же счётчик.
#[cfg(all(test, feature = "ffmpeg"))]
thread_local! {
    static LIVE_CODEC_CONTEXT_ALLOCATIONS: std::cell::Cell<usize> =
        const { std::cell::Cell::new(0) };
}

/// Число живых `CodecContext`, созданных текущим потоком (тестовый счётчик).
#[cfg(all(test, feature = "ffmpeg"))]
fn live_codec_context_allocations_for_test() -> usize {
    LIVE_CODEC_CONTEXT_ALLOCATIONS.with(std::cell::Cell::get)
}

#[cfg(feature = "ffmpeg")]
fn find_decoder(request: &FfmpegCodecContextRequest) -> FfiResult<*const ffmpeg_sys_next::AVCodec> {
    let decoder = match &request.decoder {
        FfmpegDecoderSelection::DecoderId(decoder_id) => {
            let codec_id = codec_id_from_decoder_id(*decoder_id);

            // SAFETY: codec id comes from our closed enum mapping. FFmpeg returns
            // borrowed static decoder pointer or null; null is handled below.
            unsafe { ffmpeg_sys_next::avcodec_find_decoder(codec_id) }
        }
        FfmpegDecoderSelection::DecoderName(codec_name) => {
            let codec_name =
                CString::new(codec_name.as_str()).map_err(|_| FfmpegError::InvalidInput {
                    operation: "avcodec_find_decoder_by_name",
                    details: "decoder name contains an interior NUL byte".to_owned(),
                })?;

            // SAFETY: CString is NUL-terminated and lives for the call. FFmpeg
            // returns borrowed static decoder pointer or null; null handled below.
            unsafe { ffmpeg_sys_next::avcodec_find_decoder_by_name(codec_name.as_ptr()) }
        }
    };

    if decoder.is_null() {
        return Err(FfmpegError::DecoderNotFound {
            codec: request.codec_name().to_owned(),
        });
    }

    Ok(decoder)
}

#[cfg(feature = "ffmpeg")]
fn codec_id_from_decoder_id(decoder_id: FfmpegDecoderId) -> ffmpeg_sys_next::AVCodecID {
    match decoder_id {
        FfmpegDecoderId::Vp9 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_VP9,
        FfmpegDecoderId::Av1 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_AV1,
        FfmpegDecoderId::H264 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_H264,
        FfmpegDecoderId::Hevc => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_HEVC,
        FfmpegDecoderId::Vp8 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_VP8,
    }
}

fn decoder_id_from_video_codec(video_codec: VideoCodec) -> FfmpegDecoderId {
    match video_codec {
        VideoCodec::Vp9 => FfmpegDecoderId::Vp9,
        VideoCodec::Av1 => FfmpegDecoderId::Av1,
        VideoCodec::H264 => FfmpegDecoderId::H264,
        VideoCodec::H265 => FfmpegDecoderId::Hevc,
        VideoCodec::Vp8 => FfmpegDecoderId::Vp8,
    }
}

#[cfg(feature = "ffmpeg")]
fn reject_hardware_decoder(decoder: *const ffmpeg_sys_next::AVCodec) -> FfiResult<()> {
    // SAFETY: caller passes non-null pointer returned by FFmpeg registry.
    let capabilities = unsafe { (*decoder).capabilities };
    let hardware_flags =
        (ffmpeg_sys_next::AV_CODEC_CAP_HARDWARE | ffmpeg_sys_next::AV_CODEC_CAP_HYBRID) as i32;

    if capabilities & hardware_flags == 0 {
        return Ok(());
    }

    Err(FfmpegError::HardwareDecoderRejected {
        codec: decoder_name(decoder),
    })
}

#[cfg(feature = "ffmpeg")]
fn decoder_name(decoder: *const ffmpeg_sys_next::AVCodec) -> String {
    // SAFETY: caller passes non-null pointer returned by FFmpeg registry.
    let name = unsafe { (*decoder).name };

    if name.is_null() {
        return "<unnamed>".to_owned();
    }

    // SAFETY: FFmpeg codec registry stores NUL-terminated static strings.
    unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(feature = "ffmpeg")]
fn free_context(raw_context: NonNull<ffmpeg_sys_next::AVCodecContext>) {
    let mut context_to_free = raw_context.as_ptr();

    // SAFETY: pointer получен из `avcodec_alloc_context3` и ещё не освобождён.
    // FFmpeg closes/frees context-owned state and writes null into local
    // variable; `callbacks` освобождается Rust Drop-ом отдельно.
    unsafe { ffmpeg_sys_next::avcodec_free_context(&mut context_to_free) };
}

/// Устанавливает codec-private bytes (`avcC`/`hvcC`) как `AVCodecContext.extradata`.
///
/// Буфер выделяется FFmpeg allocator-ом с `AV_INPUT_BUFFER_PADDING_SIZE` zero
/// padding (требование bitstream reader-а). После присвоения buffer принадлежит
/// codec-у и освобождается в `avcodec_free_context`, поэтому здесь его освобождать
/// нельзя.
#[cfg(feature = "ffmpeg")]
fn assign_codec_context_extradata(
    raw_context: NonNull<ffmpeg_sys_next::AVCodecContext>,
    extradata: &[u8],
) -> FfiResult<()> {
    if extradata.is_empty() {
        return Err(FfmpegError::InvalidInput {
            operation: "set extradata",
            details: "codec-private extradata is empty".to_string(),
        });
    }

    let extradata_size = i32::try_from(extradata.len()).map_err(|_| FfmpegError::InvalidInput {
        operation: "set extradata",
        details: format!(
            "codec-private extradata is too large: {} bytes",
            extradata.len()
        ),
    })?;

    let padding = ffmpeg_sys_next::AV_INPUT_BUFFER_PADDING_SIZE as usize;
    let allocation_size = extradata.len() + padding;

    // SAFETY: `av_mallocz` возвращает owned zero-initialized buffer или null,
    // что даёт обязательное zero padding в хвосте без ручной очистки.
    let buffer = unsafe { ffmpeg_sys_next::av_mallocz(allocation_size) } as *mut u8;
    let buffer = NonNull::new(buffer).ok_or(FfmpegError::AllocationFailed {
        operation: "av_mallocz(extradata)",
    })?;

    // SAFETY: `buffer` указывает минимум на `extradata.len() + padding` writable
    // bytes; source slice не пересекается с только что выделенным destination.
    // Затем передаём ownership buffer-а в codec context, который освободит его
    // через `avcodec_free_context`.
    unsafe {
        ptr::copy_nonoverlapping(extradata.as_ptr(), buffer.as_ptr(), extradata.len());
        let context = raw_context.as_ptr();
        (*context).extradata = buffer.as_ptr();
        (*context).extradata_size = extradata_size;
    }

    Ok(())
}

#[cfg(feature = "ffmpeg")]
fn ffmpeg_thread_count_from_budget(budget: SoftwareDecodeThreadBudget) -> FfiResult<i32> {
    // Auto резолвится в `video-core` (ядра − 2, мин 2), а не в FFmpeg-овский `0`
    // (= все ядра): полный набор decode worker-ов вытесняет render/upload поток.
    let thread_count = budget.resolved_thread_count();
    i32::try_from(thread_count.get()).map_err(|_| FfmpegError::InvalidInput {
        operation: "set decoder thread_count",
        details: format!(
            "software decoder thread budget {} does not fit FFmpeg AVCodecContext.thread_count",
            thread_count
        ),
    })
}

#[cfg(feature = "ffmpeg")]
mod callbacks;

#[cfg(test)]
mod tests;

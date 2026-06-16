//! Safe RAII owner for software-only `AVCodecContext`.

#[cfg(feature = "ffmpeg")]
use std::ffi::{CStr, CString};
#[cfg(feature = "ffmpeg")]
use std::ptr::{self, NonNull};

use codec_core::VideoCodec;

use super::error::{FfiResult, FfmpegError};
#[cfg(feature = "ffmpeg")]
use super::pixel_format::av_pixel_format_is_software;
use super::pixel_format::{SoftwarePixelFormat, SoftwarePixelFormatSet};

/// Запрос на открытие FFmpeg codec context-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegCodecContextRequest {
    /// Decoder selection без raw FFmpeg enum в public API.
    decoder: FfmpegDecoderSelection,

    /// Software pixel formats, которые adapter уже разрешил.
    accepted_pixel_formats: SoftwarePixelFormatSet,
}

/// Project-owned decoder selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegDecoderSelection {
    /// Decoder selected from neutral codec vocabulary.
    VideoCodec(VideoCodec),

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
        }
    }

    /// Создаёт production-shaped request по neutral `VideoCodec`.
    #[must_use]
    pub fn for_video_codec(
        video_codec: VideoCodec,
        accepted_pixel_formats: SoftwarePixelFormatSet,
    ) -> Self {
        Self {
            decoder: FfmpegDecoderSelection::VideoCodec(video_codec),
            accepted_pixel_formats,
        }
    }

    /// Возвращает codec name для error/reporting layers.
    #[must_use]
    pub fn codec_name(&self) -> &str {
        match &self.decoder {
            FfmpegDecoderSelection::VideoCodec(VideoCodec::Vp9) => "vp9",
            FfmpegDecoderSelection::VideoCodec(VideoCodec::Av1) => "av1",
            FfmpegDecoderSelection::VideoCodec(VideoCodec::H264) => "h264",
            FfmpegDecoderSelection::VideoCodec(VideoCodec::H265) => "hevc",
            FfmpegDecoderSelection::VideoCodec(VideoCodec::Vp8) => "vp8",
            FfmpegDecoderSelection::DecoderName(codec_name) => codec_name,
        }
    }

    /// Возвращает adapter-owned software pixel format policy.
    #[must_use]
    pub fn accepted_pixel_formats(&self) -> &SoftwarePixelFormatSet {
        &self.accepted_pixel_formats
    }
}

/// Opaque owner для software-only `AVCodecContext`.
#[derive(Debug)]
pub struct CodecContext {
    /// Raw context живёт только внутри FFI boundary.
    #[cfg(feature = "ffmpeg")]
    raw_context: NonNull<ffmpeg_sys_next::AVCodecContext>,

    /// State pointed to by `AVCodecContext.opaque` for `get_format`.
    #[cfg(feature = "ffmpeg")]
    _format_negotiator: Box<FormatNegotiator>,

    /// Marker, чтобы type существовал в default build-е без FFmpeg headers/libs.
    #[cfg(not(feature = "ffmpeg"))]
    _feature_disabled: (),
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

            // SAFETY: decoder pointer вернул FFmpeg registry и он valid/null-checked.
            // `avcodec_alloc_context3` возвращает owned context или null.
            let raw_context = unsafe { ffmpeg_sys_next::avcodec_alloc_context3(decoder) };
            let raw_context = NonNull::new(raw_context).ok_or(FfmpegError::AllocationFailed {
                operation: "avcodec_alloc_context3",
            })?;

            let mut format_negotiator = Box::new(FormatNegotiator::new(
                request.accepted_pixel_formats.clone(),
            ));

            // SAFETY: context принадлежит wrapper-у. `format_negotiator` находится
            // в Box, поэтому address стабилен до Drop. FFmpeg может вызывать
            // callback из разных decoder threads, но Context7/FFmpeg docs говорят,
            // что не одновременно; state immutable, дополнительных locks не нужно.
            unsafe {
                let context = raw_context.as_ptr();
                (*context).opaque = format_negotiator.as_mut_ptr();
                (*context).get_format = Some(select_software_pixel_format);
            }

            // SAFETY: context и decoder pointer-ы валидны; options null значит
            // caller не передаёт dictionary. Context остаётся owned wrapper-ом.
            let status = unsafe {
                ffmpeg_sys_next::avcodec_open2(raw_context.as_ptr(), decoder, ptr::null_mut())
            };

            if status < 0 {
                free_context(raw_context);
                return Err(FfmpegError::from_averror("avcodec_open2", status));
            }

            Ok(Self {
                raw_context,
                _format_negotiator: format_negotiator,
            })
        }
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
}

#[cfg(feature = "ffmpeg")]
impl Drop for CodecContext {
    fn drop(&mut self) {
        free_context(self.raw_context);
    }
}

#[cfg(feature = "ffmpeg")]
#[derive(Debug)]
struct FormatNegotiator {
    accepted_pixel_formats: SoftwarePixelFormatSet,
}

#[cfg(feature = "ffmpeg")]
impl FormatNegotiator {
    fn new(accepted_pixel_formats: SoftwarePixelFormatSet) -> Self {
        Self {
            accepted_pixel_formats,
        }
    }

    fn as_mut_ptr(&mut self) -> *mut std::ffi::c_void {
        self as *mut Self as *mut std::ffi::c_void
    }

    fn accepts(&self, pixel_format: ffmpeg_sys_next::AVPixelFormat) -> bool {
        self.accepted_pixel_formats
            .contains_av_pixel_format(pixel_format)
            && av_pixel_format_is_software(pixel_format)
    }
}

#[cfg(feature = "ffmpeg")]
unsafe extern "C" fn select_software_pixel_format(
    context: *mut ffmpeg_sys_next::AVCodecContext,
    offered_formats: *const ffmpeg_sys_next::AVPixelFormat,
) -> ffmpeg_sys_next::AVPixelFormat {
    if context.is_null() || offered_formats.is_null() {
        return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
    }

    // SAFETY: FFmpeg передаёт pointer на текущий `AVCodecContext`.
    // Мы читаем только `opaque`, который сами установили до `avcodec_open2`.
    let opaque = unsafe { (*context).opaque };

    if opaque.is_null() {
        return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
    }

    // SAFETY: `opaque` указывает на `FormatNegotiator` в `CodecContext`.
    // Box не перемещается, живёт не меньше context-а, callback не вызывается
    // после `avcodec_free_context`. State immutable, поэтому thread-safe для
    // documented non-concurrent `get_format` calls.
    let format_negotiator = unsafe { &*(opaque as *const FormatNegotiator) };
    let mut format_index = 0usize;

    loop {
        // SAFETY: FFmpeg передаёт список `AVPixelFormat`, завершённый
        // `AV_PIX_FMT_NONE`. Loop останавливается на sentinel-е.
        let candidate = unsafe { *offered_formats.add(format_index) };

        if candidate == ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE {
            return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
        }

        if format_negotiator.accepts(candidate) {
            return candidate;
        }

        format_index += 1;

        if format_index > 256 {
            return ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE;
        }
    }
}

#[cfg(feature = "ffmpeg")]
fn find_decoder(request: &FfmpegCodecContextRequest) -> FfiResult<*const ffmpeg_sys_next::AVCodec> {
    let decoder = match &request.decoder {
        FfmpegDecoderSelection::VideoCodec(video_codec) => {
            let codec_id = codec_id_from_video_codec(*video_codec);

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
fn codec_id_from_video_codec(video_codec: VideoCodec) -> ffmpeg_sys_next::AVCodecID {
    match video_codec {
        VideoCodec::Vp9 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_VP9,
        VideoCodec::Av1 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_AV1,
        VideoCodec::H264 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_H264,
        VideoCodec::H265 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_HEVC,
        VideoCodec::Vp8 => ffmpeg_sys_next::AVCodecID::AV_CODEC_ID_VP8,
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
    // variable; `_format_negotiator` освобождается Rust Drop-ом отдельно.
    unsafe { ffmpeg_sys_next::avcodec_free_context(&mut context_to_free) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_context_request_preserves_codec_name_for_diagnostics() {
        let request = FfmpegCodecContextRequest::new("h264");

        assert_eq!(request.codec_name(), "h264");
    }

    #[test]
    fn codec_context_reports_feature_disabled_without_ffmpeg() {
        if cfg!(feature = "ffmpeg") {
            return;
        }

        let request = FfmpegCodecContextRequest::new("h264");
        let error = CodecContext::open(&request).expect_err("default build has no FFmpeg FFI");

        assert_eq!(error, FfmpegError::FeatureDisabled);
    }

    #[test]
    fn codec_context_request_preserves_adapter_pixel_format_policy() {
        let formats = SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv420Planar8])
            .expect("single software format is valid");
        let request = FfmpegCodecContextRequest::for_video_codec(VideoCodec::H264, formats.clone());

        assert_eq!(request.codec_name(), "h264");
        assert_eq!(request.accepted_pixel_formats(), &formats);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn get_format_chooses_only_adapter_accepted_software_format() {
        let raw_context = unsafe {
            // SAFETY: null codec is allowed for default context allocation in this
            // isolated negotiation test; context is freed before test exits.
            ffmpeg_sys_next::avcodec_alloc_context3(ptr::null())
        };
        let raw_context = NonNull::new(raw_context).expect("context allocation should succeed");
        let mut negotiator = FormatNegotiator::new(
            SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv420Planar8])
                .expect("allowlist should be valid"),
        );

        // SAFETY: test owns context and negotiator for the whole callback call.
        unsafe {
            (*raw_context.as_ptr()).opaque = negotiator.as_mut_ptr();
        }

        let offered_formats = [
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_VAAPI,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV422P,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE,
        ];

        // SAFETY: context pointer is valid and offered list is sentinel-terminated.
        let selected_format =
            unsafe { select_software_pixel_format(raw_context.as_ptr(), offered_formats.as_ptr()) };

        assert_eq!(
            selected_format,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P
        );

        free_context(raw_context);
    }

    #[cfg(feature = "ffmpeg")]
    #[test]
    fn get_format_returns_none_when_adapter_accepts_no_offered_software_format() {
        let raw_context = unsafe {
            // SAFETY: null codec is allowed for default context allocation in this
            // isolated negotiation test; context is freed before test exits.
            ffmpeg_sys_next::avcodec_alloc_context3(ptr::null())
        };
        let raw_context = NonNull::new(raw_context).expect("context allocation should succeed");
        let mut negotiator = FormatNegotiator::new(
            SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv444Planar10Le])
                .expect("allowlist should be valid"),
        );

        // SAFETY: test owns context and negotiator for the whole callback call.
        unsafe {
            (*raw_context.as_ptr()).opaque = negotiator.as_mut_ptr();
        }

        let offered_formats = [
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_VAAPI,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_YUV420P,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE,
        ];

        // SAFETY: context pointer is valid and offered list is sentinel-terminated.
        let selected_format =
            unsafe { select_software_pixel_format(raw_context.as_ptr(), offered_formats.as_ptr()) };

        assert_eq!(
            selected_format,
            ffmpeg_sys_next::AVPixelFormat::AV_PIX_FMT_NONE
        );

        free_context(raw_context);
    }
}

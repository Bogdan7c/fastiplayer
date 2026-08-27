use super::*;
#[cfg(feature = "ffmpeg")]
use static_assertions::assert_not_impl_any;

// Raw mutable codec state остаётся строго на decoder owner thread.
#[cfg(feature = "ffmpeg")]
assert_not_impl_any!(CodecContext: Send, Sync);

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

#[cfg(feature = "ffmpeg")]
#[test]
fn post_allocation_validation_error_releases_codec_context_and_extradata() {
    use std::num::NonZeroUsize;

    let live_allocations_before = live_codec_context_allocations_for_test();
    let impossible_thread_budget = SoftwareDecodeThreadBudget::Fixed(
        NonZeroUsize::new(usize::MAX).expect("usize::MAX is non-zero"),
    );
    let request = FfmpegCodecContextRequest::new("h264")
        .with_extradata(vec![1, 2, 3, 4])
        .with_software_decode_thread_budget(impossible_thread_budget);

    let error = CodecContext::open(&request)
        .expect_err("thread budget that does not fit c_int must be rejected");

    assert!(matches!(error, FfmpegError::InvalidInput { .. }));
    assert_eq!(
        live_codec_context_allocations_for_test(),
        live_allocations_before,
        "every post-allocation error must release the context-owned extradata and callback state"
    );
}

#[test]
fn codec_context_request_preserves_adapter_pixel_format_policy() {
    let formats = SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv420Planar8])
        .expect("single software format is valid");
    let request = FfmpegCodecContextRequest::for_decoder_id(FfmpegDecoderId::H264, formats.clone());

    assert_eq!(request.codec_name(), "h264");
    assert_eq!(request.accepted_pixel_formats(), &formats);
}

#[test]
fn codec_context_request_preserves_max_frame_delay_option() {
    let formats = SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv420Planar8])
        .expect("single software format is valid");
    let request = FfmpegCodecContextRequest::for_decoder_id(FfmpegDecoderId::Av1, formats)
        .with_max_frame_delay(1);

    assert_eq!(request.max_frame_delay(), Some(1));
}

#[test]
fn codec_context_request_preserves_software_decode_thread_budget() {
    let formats = SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv420Planar8])
        .expect("single software format is valid");
    let thread_count = std::num::NonZeroUsize::new(2).expect("test value is positive");
    let budget = SoftwareDecodeThreadBudget::fixed(thread_count);
    let request = FfmpegCodecContextRequest::for_decoder_id(FfmpegDecoderId::H264, formats)
        .with_software_decode_thread_budget(budget);

    assert_eq!(request.software_decode_thread_budget(), budget);
    assert_eq!(
        FfmpegCodecContextRequest::new("h264").software_decode_thread_budget(),
        SoftwareDecodeThreadBudget::auto()
    );
}

#[cfg(feature = "ffmpeg")]
#[test]
fn software_decode_thread_budget_maps_to_ffmpeg_thread_count_values() {
    let thread_count = std::num::NonZeroUsize::new(3).expect("test value is positive");
    let auto_budget = SoftwareDecodeThreadBudget::auto();
    let expected_auto_thread_count = i32::try_from(auto_budget.resolved_thread_count().get())
        .expect("resolved auto thread count fits FFmpeg thread_count");

    assert_eq!(
        ffmpeg_thread_count_from_budget(auto_budget).unwrap(),
        expected_auto_thread_count
    );
    assert_eq!(
        ffmpeg_thread_count_from_budget(SoftwareDecodeThreadBudget::fixed(thread_count)).unwrap(),
        3
    );
}

#[test]
fn codec_context_request_keeps_codec_only_constructor_as_compatibility_path() {
    let formats = SoftwarePixelFormatSet::new([SoftwarePixelFormat::Yuv420Planar8])
        .expect("single software format is valid");
    let request = FfmpegCodecContextRequest::for_video_codec(VideoCodec::H265, formats);

    assert_eq!(request.codec_name(), "hevc");
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

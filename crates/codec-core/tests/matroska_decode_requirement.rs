use codec_core::{
    BitDepth, ChromaSubsampling, SupportedVideoDecodeFormat, VideoCodec, VideoDecodeRequirement,
    VideoProfile, Vp9Profile,
};

/// Container hints должны влиять на совместимость декодера, а неизвестная
/// комбинация не должна превращаться в разрешённый формат 4:2:0.
#[test]
fn matroska_chroma_controls_decoder_compatibility() {
    let supported = SupportedVideoDecodeFormat {
        codec: VideoCodec::Vp9,
        profile: VideoProfile::Vp9(Vp9Profile::Profile0),
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        max_width: None,
        max_height: None,
        max_fps: None,
        hdr_input: false,
    };
    // Opaque function pointer сохраняет реальный runtime-вызов const fn:
    // оптимизатор не должен подменять проверяемое преобразование константой.
    let parse_chroma = std::hint::black_box(
        ChromaSubsampling::from_matroska_subsampling as fn(u64, u64) -> Option<ChromaSubsampling>,
    );
    for (horizontal, vertical, expected) in [
        (1, 1, Some(ChromaSubsampling::Yuv420)),
        (1, 0, Some(ChromaSubsampling::Yuv422)),
        (0, 0, Some(ChromaSubsampling::Yuv444)),
        (0, 1, None),
        (2, 2, None),
    ] {
        let chroma = parse_chroma(horizontal, vertical);
        assert_eq!(chroma, expected);
        if let Some(chroma) = chroma {
            let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
                .with_bit_depth(BitDepth::Eight)
                .with_chroma(chroma);
            assert_eq!(
                supported.satisfies(&requirement),
                chroma == ChromaSubsampling::Yuv420,
                "container chroma must control decoder eligibility"
            );
        }
    }
}

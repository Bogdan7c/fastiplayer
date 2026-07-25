use super::*;
use std::collections::BTreeSet;

/// Строит owned identity exact row/transport evidence без строковой склейки.
fn row_transport_evidence(row_id: &str, transport_raw: &str) -> (String, String) {
    (row_id.to_owned(), transport_raw.to_owned())
}

/// Все двенадцать Implemented S42 rows и их exact scheme-варианты имеют positive plan.
#[test]
fn approved_s42_rows_have_positive_production_shaped_plans() {
    let (transport, demux) = s42_resource_capabilities();
    let video = video_capabilities(vec![
        supported_video_format(VideoCodec::Av1, false),
        supported_video_format(VideoCodec::Vp9, false),
        supported_video_format(VideoCodec::H264, false),
    ]);
    let audio = AudioDecodeCapabilitySnapshot::empty()
        .with_available_family(AudioDecodeCodecFamily::Aac)
        .with_available_family(AudioDecodeCodecFamily::Opus);
    let capabilities = PlaybackCapabilitySnapshot::new(&transport, &demux, &video, audio);
    let policy = selection_policy(
        HdrSelectionPolicy::SdrOnly,
        PreferredHeightPolicy::NoPreference,
        vec![VideoCodec::Av1, VideoCodec::Vp9, VideoCodec::H264],
        vec![
            ContainerFamily::IsoBmff,
            ContainerFamily::FragmentedIsoBmff,
            ContainerFamily::WebM,
            ContainerFamily::Ogg,
            ContainerFamily::MpegTs,
            ContainerFamily::F4f,
        ],
    );
    let cases = vec![
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "progressive-http-iso-bmff",
            semantic_key: "s42-progressive-http-iso-bmff-http",
            transport_raw: "http",
            container_raw: "mp4",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "progressive-http-iso-bmff",
            semantic_key: "s42-progressive-http-iso-bmff-https",
            transport_raw: "https",
            container_raw: "mp4",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "progressive-http-matroska-webm",
            semantic_key: "s42-progressive-http-matroska-webm-http",
            transport_raw: "http",
            container_raw: "webm",
            video_codec_raw: "vp09.00.41.08",
            video_codec: VideoCodec::Vp9,
            audio_codec_raw: "opus",
            audio_codec_family: AudioDecodeCodecFamily::Opus,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "progressive-http-matroska-webm",
            semantic_key: "s42-progressive-http-matroska-webm-https",
            transport_raw: "https",
            container_raw: "webm",
            video_codec_raw: "vp09.00.41.08",
            video_codec: VideoCodec::Vp9,
            audio_codec_raw: "opus",
            audio_codec_family: AudioDecodeCodecFamily::Opus,
        }),
        audio_only_candidate_for(AudioCandidateSpec {
            format_id: "progressive-http-proven-audio",
            semantic_key: "s42-progressive-http-proven-audio-http",
            transport_raw: "http",
            container_raw: "ogg",
            codec_raw: "opus",
            codec_family: AudioDecodeCodecFamily::Opus,
        }),
        audio_only_candidate_for(AudioCandidateSpec {
            format_id: "progressive-http-proven-audio",
            semantic_key: "s42-progressive-http-proven-audio-https",
            transport_raw: "https",
            container_raw: "ogg",
            codec_raw: "opus",
            codec_family: AudioDecodeCodecFamily::Opus,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "hls-vod-ts",
            semantic_key: "s42-hls-vod-ts",
            transport_raw: "m3u8_native",
            container_raw: "ts",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "hls-vod-fmp4",
            semantic_key: "s42-hls-vod-fmp4",
            transport_raw: "m3u8_native",
            container_raw: "fmp4",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "hls-live-dvr",
            semantic_key: "s42-hls-live-dvr",
            transport_raw: "m3u8_native",
            container_raw: "ts",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "dash-vod-fmp4",
            semantic_key: "s42-dash-vod-fmp4",
            transport_raw: "http_dash_segments",
            container_raw: "fmp4",
            codec_raw: "av01.0.08M.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Av1, 1080),
            quality_score: 10,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "dash-vod-webm",
            semantic_key: "s42-dash-vod-webm",
            transport_raw: "http_dash_segments",
            container_raw: "webm",
            codec_raw: "vp09.00.41.08",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::Vp9, 1080),
            quality_score: 10,
        }),
        video_only_candidate(VideoCandidateSpec {
            format_id: "dash-live-dvr",
            semantic_key: "s42-dash-live-dvr",
            transport_raw: "http_dash_segments",
            container_raw: "fmp4",
            codec_raw: "avc1.640028",
            height: 1080,
            dynamic_range: DynamicRange::Sdr,
            requirement: sdr_requirement(VideoCodec::H264, 1080),
            quality_score: 10,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "ism-mss-base-h264-aac-fmp4",
            semantic_key: "s42-ism-mss-base",
            transport_raw: "ism",
            container_raw: "fmp4",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "ftp-ftps-progressive",
            semantic_key: "s42-ftp-progressive",
            transport_raw: "ftp",
            container_raw: "webm",
            video_codec_raw: "vp09.00.41.08",
            video_codec: VideoCodec::Vp9,
            audio_codec_raw: "opus",
            audio_codec_family: AudioDecodeCodecFamily::Opus,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "ftp-ftps-progressive",
            semantic_key: "s42-ftps-progressive",
            transport_raw: "ftps",
            container_raw: "webm",
            video_codec_raw: "vp09.00.41.08",
            video_codec: VideoCodec::Vp9,
            audio_codec_raw: "opus",
            audio_codec_family: AudioDecodeCodecFamily::Opus,
        }),
        muxed_candidate_for(MuxedCandidateSpec {
            format_id: "hds-f4m-f4f",
            semantic_key: "s42-hds-f4f",
            transport_raw: "f4m",
            container_raw: "f4f",
            video_codec_raw: "avc1.640028",
            video_codec: VideoCodec::H264,
            audio_codec_raw: "mp4a.40.2",
            audio_codec_family: AudioDecodeCodecFamily::Aac,
        }),
    ];
    // Expected set ratchet-ит обе scheme-варианта aggregate HTTP и FTP rows.
    let expected_row_transport_evidence = BTreeSet::from([
        row_transport_evidence("progressive-http-iso-bmff", "http"),
        row_transport_evidence("progressive-http-iso-bmff", "https"),
        row_transport_evidence("progressive-http-matroska-webm", "http"),
        row_transport_evidence("progressive-http-matroska-webm", "https"),
        row_transport_evidence("progressive-http-proven-audio", "http"),
        row_transport_evidence("progressive-http-proven-audio", "https"),
        row_transport_evidence("hls-vod-ts", "m3u8_native"),
        row_transport_evidence("hls-vod-fmp4", "m3u8_native"),
        row_transport_evidence("hls-live-dvr", "m3u8_native"),
        row_transport_evidence("dash-vod-fmp4", "http_dash_segments"),
        row_transport_evidence("dash-vod-webm", "http_dash_segments"),
        row_transport_evidence("dash-live-dvr", "http_dash_segments"),
        row_transport_evidence("ism-mss-base-h264-aac-fmp4", "ism"),
        row_transport_evidence("ftp-ftps-progressive", "ftp"),
        row_transport_evidence("ftp-ftps-progressive", "ftps"),
        row_transport_evidence("hds-f4m-f4f", "f4m"),
    ]);
    // Actual set отдельно запрещает duplicate fixture, который скрыл бы missing variant.
    let mut actual_row_transport_evidence = BTreeSet::new();

    for candidate in cases {
        let row_id = candidate
            .descriptor()
            .identity()
            .format()
            .as_str()
            .to_owned();
        // Каждый current S42 fixture имеет один transport; separate path проверяет согласованность.
        let transport_raw = match candidate.descriptor().layout() {
            StreamLayout::Muxed(component) => component.transport().raw().as_str(),
            StreamLayout::Separate { video, audio } => {
                assert_eq!(
                    video.transport().raw(),
                    audio.transport().raw(),
                    "S42 separate fixture не должен смешивать transport families"
                );
                video.transport().raw().as_str()
            }
            StreamLayout::VideoOnly(component) => component.transport().raw().as_str(),
            StreamLayout::AudioOnly(component) => component.transport().raw().as_str(),
        }
        .to_owned();
        // Duplicate row/transport evidence не увеличивает доказанное покрытие.
        assert!(
            actual_row_transport_evidence.insert((row_id.clone(), transport_raw)),
            "S42 row/transport evidence не должно повторяться"
        );
        let request = exact_request(&candidate);
        let snapshot = candidate_snapshot(vec![candidate]);
        let outcome = plan_playback(&snapshot, capabilities, &request, &policy)
            .unwrap_or_else(|error| panic!("S42 row `{row_id}` должна быть playable: {error:?}"));

        assert_eq!(
            outcome.selected().exact_identity().format().as_str(),
            row_id
        );
    }
    // Exact equality ловит missing HTTP/HTTPS/FTP/FTPS variant или незаявленное расширение.
    assert_eq!(
        actual_row_transport_evidence,
        expected_row_transport_evidence
    );
}

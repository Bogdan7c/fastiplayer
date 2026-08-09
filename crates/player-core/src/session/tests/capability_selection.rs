use super::test_support::*;
use super::*;

#[test]
fn capability_report_updates_snapshot_and_event_queue() {
    let mut session = PlayerSession::new();

    session.set_system_capabilities(capabilities_with_vp9_profile0());

    assert!(session.snapshot().capability_summary.is_some());
    assert!(
        session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::CapabilityScanCompleted(_)))
    );
}

#[test]
fn unsupported_profile_returns_player_error_before_decode() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

    let error = session
        .validate_video_decode_requirement(&requirement)
        .expect_err("VP9 profile2 must be rejected by profile0-only capabilities");

    assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoProfile);
    assert!(error.message.contains("profile VP9 Profile 2"));
    assert!(!session.can_defer_packet_refinement(&requirement));
}

#[test]
fn active_backend_filters_matching_capability_output_contract_for_each_codec() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_hardware_and_ffmpeg_outputs());
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "ffmpeg-sw",
        SharedFakeVideoDecoderThread::new(),
    ));

    for requirement in [
        VideoDecodeRequirement::new(VideoCodec::H264)
            .with_profile(VideoProfile::H264(H264Profile::Main))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420),
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0))
            .with_bit_depth(BitDepth::Eight)
            .with_chroma(ChromaSubsampling::Yuv420),
    ] {
        let matched_output = session
            .validate_video_decode_requirement(&requirement)
            .expect("active FFmpeg backend должен пройти playable software requirement")
            .expect("capability selection должен вернуть concrete output");

        assert_eq!(matched_output.backend.as_str(), "ffmpeg-sw");
        assert_eq!(
            matched_output.frame_contract,
            VideoFrameContract::host_yuv420_planar8()
        );
    }
}

#[test]
fn requested_video_track_selection_sets_matching_requirement() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let _ = session.take_events();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(7);
    let tracks = vec![vp9_track_with_profile(7, Vp9Profile::Profile0)];
    install_tracks_for_capability_selection(&mut session, tracks);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("supported VP9 profile0 track должен быть выбран");

    let active_requirement = session
        .pipeline
        .active_video_requirement()
        .expect("validated selection должен установить active requirement");

    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(video_track_id)
    );
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(video_track_id)
    );
    assert_eq!(
        active_requirement.profile,
        Some(VideoProfile::Vp9(Vp9Profile::Profile0))
    );
    assert_eq!(
        fake_decoder
            .stream_config()
            .expect("VP9 selection должен сконфигурировать fake decoder")
            .codec,
        VideoCodec::Vp9
    );
    assert!(session.take_events().iter().any(|event| {
        matches!(event, PlayerEvent::VideoTrackSelected(track_id) if *track_id == video_track_id)
    }));
}

#[test]
fn requested_video_track_selection_replaces_stale_requirement() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    install_active_test_video_backend(&mut session);
    let video_track_id = TrackId::new(11);
    let stale_track_id = TrackId::new(99);
    let stale_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));
    let tracks = vec![vp9_track_with_profile(11, Vp9Profile::Profile0)];
    install_tracks_for_capability_selection(&mut session, tracks);
    session
        .pipeline
        .select_video_track(stale_track_id, stale_requirement);

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("selection должен пересчитать requirement из выбранного track-а");

    let active_requirement = session
        .pipeline
        .active_video_requirement()
        .expect("fresh selection должен заменить stale requirement");

    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(video_track_id)
    );
    assert_eq!(
        active_requirement.profile,
        Some(VideoProfile::Vp9(Vp9Profile::Profile0))
    );
}

#[test]
fn requested_video_track_selection_rejects_unsupported_requirement_before_mutation() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    // Этот focused validation-тест оставляет backend id нейтральным, но даёт
    // selection boundary реальный decoder, чтобы проверить global typed rejection.
    session
        .pipeline
        .set_video_decoder_thread(SharedFakeVideoDecoderThread::new());
    let supported_track_id = TrackId::new(1);
    let unsupported_track_id = TrackId::new(2);
    let tracks = vec![
        vp9_track_with_profile(1, Vp9Profile::Profile0),
        vp9_track_with_profile(2, Vp9Profile::Profile2),
    ];
    install_tracks_for_capability_selection(&mut session, tracks);
    session
        .select_requested_video_track(supported_track_id)
        .expect("initial supported selection должен пройти validation");
    let accepted_requirement = session
        .pipeline
        .active_video_requirement()
        .expect("initial selection должен установить requirement")
        .clone();

    let error = session
        .dispatch_command(PlayerCommand::SelectVideoTrack(unsupported_track_id))
        .expect_err("unsupported profile2 track должен быть отвергнут до mutation");

    assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoProfile);
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(supported_track_id)
    );
    assert_eq!(
        session.pipeline.active_video_requirement(),
        Some(&accepted_requirement)
    );
}

#[test]
fn h264_stream_config_uses_track_codec_private_and_metadata() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(17);
    install_tracks_for_capability_selection(&mut session, vec![h264_track_with_avcc(17)]);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("fake backend должен принять H.264 config без production VAAPI decode");

    let config = fake_decoder
        .stream_config()
        .expect("H.264 selection должен передать stream config в decoder boundary");

    assert_eq!(config.track_id, video_track_id);
    assert_eq!(config.codec, VideoCodec::H264);
    assert_eq!(
        config.profile,
        Some(VideoProfile::H264(H264Profile::ConstrainedBaseline))
    );
    assert_eq!(config.bit_depth, Some(BitDepth::Eight));
    assert_eq!(config.chroma, Some(ChromaSubsampling::Yuv420));
    assert_eq!(config.coded_width, Some(1280));
    assert_eq!(config.coded_height, Some(720));
    assert_eq!(config.codec_private, Some(h264_avcc_codec_private()));
    match config.packetization {
        Some(video_core::VideoStreamPacketization::H264(
            codec_core::H264Packetization::AvccLengthPrefixed { nal_length_size },
        )) => assert_eq!(nal_length_size.get(), 4),
        unexpected_packetization => {
            panic!("expected AVCC H.264 packetization, got {unexpected_packetization:?}");
        }
    }
}

/// Закрепляет полный `avc3` route: track config, in-band SPS/PPS packet, decoder и presentation.
#[test]
fn hls_avc3_sample_with_in_band_parameter_sets_reaches_presentation() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9());
    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.decode_next_packet_as_frame(Duration::ZERO, 701);
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        DecodeBackendId::vaapi().as_str(),
        decoder.clone(),
    ));

    let video_track_id = TrackId::new(71);
    let tracks = vec![h264_avc3_track(video_track_id.get())];
    install_tracks_for_capability_selection(&mut session, tracks.clone());
    session.set_playback_state(PlaybackState::Playing);
    session
        .select_default_video_track(&tracks, "HLS avc3 track должен выбрать video backend")
        .expect("поддерживаемый avc3 track должен сконфигурировать decoder");

    let source_access_unit = h264_avc3_access_unit_with_in_band_parameter_sets();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::ZERO,
            session.pipeline.seek_generation(),
            source_access_unit.clone(),
            PacketKeyframe::Keyframe,
        ));

    let first_tick = session.tick(PlayerTickContext::new(Instant::now()));
    let second_tick = session.tick(PlayerTickContext::new(Instant::now()));

    let configured_stream = decoder
        .stream_config()
        .expect("avc3 track должен передать stream config в decoder boundary");
    assert_eq!(
        configured_stream.codec_private,
        Some(h264_avc3_codec_private())
    );
    assert!(matches!(
        configured_stream.packetization,
        Some(video_core::VideoStreamPacketization::H264(
            codec_core::H264Packetization::AvccLengthPrefixedWithInBandParameterSets {
                nal_length_size: codec_core::H264NalLengthSize::FOUR,
            }
        ))
    ));
    let sent_packets = decoder.sent_packets();
    assert_eq!(sent_packets.len(), 1);
    assert_eq!(sent_packets[0].encoded_bytes, source_access_unit);
    assert_eq!(
        first_tick.video_frames_presented + second_tick.video_frames_presented,
        1,
        "avc3 packet должен стать предъявленным кадром, а не застрять после track mapping"
    );
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
}

#[test]
fn hls_avc3_track_rejects_missing_and_malformed_configuration_before_decode() {
    for (codec_private, expected_reason) in [
        (None, "не содержит avc3 avcC"),
        (
            Some(Bytes::from_static(&[0x01, 0x4d])),
            "не является поддержанным avc3 avcC",
        ),
    ] {
        let mut session = PlayerSession::new();
        let video_track_id = TrackId::new(72);
        let mut avc3_track = h264_avc3_track(video_track_id.get());
        avc3_track.codec_private = codec_private;
        install_tracks_for_capability_selection(&mut session, vec![avc3_track]);
        session
            .pipeline
            .set_video_decoder_thread(SharedFakeVideoDecoderThread::new());

        let error = session
            .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
            .expect_err("avc3 без валидного avcC должен быть отвергнут до decoder configure");

        assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoCodec);
        assert!(
            error.message.contains(expected_reason),
            "unexpected error: {}",
            error.message
        );
        assert_eq!(session.pipeline.selected_video_track_id(), None);
    }
}

#[test]
fn h264_stream_config_accepts_zeroed_avcc_reserved_bits() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(18);
    let codec_private = h264_avcc_codec_private_with_zeroed_reserved_bits();
    let mut h264_track = h264_track_with_avcc(video_track_id.get());
    h264_track.codec_private = Some(codec_private.clone());
    install_tracks_for_capability_selection(&mut session, vec![h264_track]);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("H.264 avcC с zeroed reserved bits должен сохранять AVCC packetization");

    let config = fake_decoder
        .stream_config()
        .expect("H.264 selection должен сконфигурировать fake decoder");

    assert_eq!(config.codec_private, Some(codec_private));
    match config.packetization {
        Some(video_core::VideoStreamPacketization::H264(
            codec_core::H264Packetization::AvccLengthPrefixed { nal_length_size },
        )) => assert_eq!(nal_length_size.get(), 4),
        unexpected_packetization => {
            panic!("expected AVCC H.264 packetization, got {unexpected_packetization:?}");
        }
    }
}

#[test]
fn h265_stream_config_uses_track_hvcc_packetization_and_metadata() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(19);
    install_tracks_for_capability_selection(&mut session, vec![h265_track_with_hvcc(19)]);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("fake backend должен принять нейтральный H.265 config без VAAPI adapter-а");

    let config = fake_decoder
        .stream_config()
        .expect("H.265 selection должен передать stream config в decoder boundary");

    assert_eq!(config.track_id, video_track_id);
    assert_eq!(config.codec, VideoCodec::H265);
    assert_eq!(
        config.profile,
        Some(VideoProfile::H265(codec_core::H265Profile::Main10))
    );
    assert_eq!(config.bit_depth, Some(BitDepth::Ten));
    assert_eq!(config.chroma, Some(ChromaSubsampling::Yuv420));
    assert_eq!(config.coded_width, Some(3840));
    assert_eq!(config.coded_height, Some(2160));
    assert_eq!(config.codec_private, Some(h265_hvcc_codec_private()));
    match config.packetization {
        Some(video_core::VideoStreamPacketization::H265(
            codec_core::H265Packetization::HvccLengthPrefixed { nal_length_size },
        )) => assert_eq!(nal_length_size.get(), 4),
        unexpected_packetization => {
            panic!("expected hvcC H.265 packetization, got {unexpected_packetization:?}");
        }
    }
}

#[test]
fn h265_stream_config_rejects_missing_hvcc_with_typed_codec_error() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(20);
    let mut track = h265_track_with_hvcc(video_track_id.get());
    track.codec_private = None;
    install_tracks_for_capability_selection(&mut session, vec![track]);
    session.pipeline.set_video_decoder_thread(fake_decoder);

    let error = session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect_err("H.265 без hvcC не должен превращаться в bool/no-op");

    assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoCodec);
    assert!(error.message.contains("hvcC codec_private"));
    assert!(session.pipeline.selected_video_track_id().is_none());
}

#[test]
fn h264_annex_b_evidence_configures_decoder_without_avcc() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(21);
    let mut track = h264_track_with_avcc(video_track_id.get());
    track.codec_private = None;
    track.video.as_mut().expect("video metadata").packet_framing =
        media_core::VideoPacketFraming::AnnexB;
    install_tracks_for_capability_selection(&mut session, vec![track]);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("explicit H.264 Annex-B framing");

    assert!(matches!(
        fake_decoder
            .stream_config()
            .and_then(|config| config.packetization),
        Some(video_core::VideoStreamPacketization::H264(
            codec_core::H264Packetization::AnnexB
        ))
    ));
}

#[test]
fn h265_annex_b_evidence_configures_decoder_without_hvcc() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(22);
    let mut track = h265_track_with_hvcc(video_track_id.get());
    track.codec_private = None;
    track.video.as_mut().expect("video metadata").packet_framing =
        media_core::VideoPacketFraming::AnnexB;
    install_tracks_for_capability_selection(&mut session, vec![track]);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect("explicit H.265 Annex-B framing");

    assert!(matches!(
        fake_decoder
            .stream_config()
            .and_then(|config| config.packetization),
        Some(video_core::VideoStreamPacketization::H265(
            codec_core::H265Packetization::AnnexB
        ))
    ));
}

#[test]
fn explicit_annex_b_conflicting_with_codec_private_is_typed_rejection() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let video_track_id = TrackId::new(23);
    let mut track = h265_track_with_hvcc(video_track_id.get());
    track.video.as_mut().expect("video metadata").packet_framing =
        media_core::VideoPacketFraming::AnnexB;
    install_tracks_for_capability_selection(&mut session, vec![track]);
    session.pipeline.set_video_decoder_thread(fake_decoder);

    let error = session
        .dispatch_command(PlayerCommand::SelectVideoTrack(video_track_id))
        .expect_err("conflicting authoritative framing evidence");

    assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoCodec);
    assert!(error.message.contains("одновременно объявляет Annex-B"));
}

#[test]
fn fake_backend_accepts_vp9_h264_vp9_switch_without_restart() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let first_vp9_track_id = TrackId::new(1);
    let h264_track_id = TrackId::new(2);
    let second_vp9_track_id = TrackId::new(3);
    let tracks = vec![
        vp9_track_with_profile(1, Vp9Profile::Profile0),
        h264_track_with_avcc(2),
        vp9_track_with_profile(3, Vp9Profile::Profile0),
    ];
    install_tracks_for_capability_selection(&mut session, tracks);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(first_vp9_track_id))
        .expect("first VP9 track должен быть выбран");
    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(h264_track_id))
        .expect("H.264 track должен быть выбран fake backend-ом");
    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(second_vp9_track_id))
        .expect("second VP9 track должен быть выбран без restart приложения");

    let codecs = fake_decoder
        .configured_streams()
        .into_iter()
        .map(|config| config.codec)
        .collect::<Vec<_>>();

    assert_eq!(
        codecs,
        vec![VideoCodec::Vp9, VideoCodec::H264, VideoCodec::Vp9]
    );
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(second_vp9_track_id)
    );
}

#[test]
fn stream_config_failure_does_not_mutate_selected_track_or_requirement() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let selected_track_id = TrackId::new(1);
    let rejected_track_id = TrackId::new(2);
    let tracks = vec![
        vp9_track_with_profile(1, Vp9Profile::Profile0),
        vp9_track_with_profile(2, Vp9Profile::Profile0),
    ];
    install_tracks_for_capability_selection(&mut session, tracks);
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());
    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(selected_track_id))
        .expect("initial VP9 selection должен пройти");
    let accepted_requirement = session
        .pipeline
        .active_video_requirement()
        .expect("initial selection должен сохранить requirement")
        .clone();
    fake_decoder.push_configure_result(video_core::VideoStreamConfigResult::Fatal(
        DecodeThreadError::new("configure failed"),
    ));

    let error = session
        .dispatch_command(PlayerCommand::SelectVideoTrack(rejected_track_id))
        .expect_err("configure failure должен остановить selection до mutation");

    assert_eq!(error.kind, PlayerErrorKind::RuntimeError);
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(selected_track_id)
    );
    assert_eq!(
        session.pipeline.active_video_requirement(),
        Some(&accepted_requirement)
    );
}

#[test]
fn active_video_requirement_refinement_preserves_selection_and_rejects_before_mutation() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let video_track_id = TrackId::new(1);
    let initial_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let supported_requirement = initial_requirement
        .clone()
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile0));
    let rejected_requirement = initial_requirement
        .clone()
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2));

    session
        .pipeline
        .select_video_track(video_track_id, initial_requirement);

    session
        .refine_active_video_requirement(supported_requirement.clone())
        .expect("profile0 requirement должен пройти fake capabilities");

    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(video_track_id)
    );
    assert_eq!(
        session.pipeline.active_video_requirement(),
        Some(&supported_requirement)
    );

    let error = session
        .refine_active_video_requirement(rejected_requirement)
        .expect_err("profile2 должен быть отвергнут до изменения active requirement");

    assert_eq!(error.kind, PlayerErrorKind::UnsupportedVideoProfile);
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(video_track_id)
    );
    assert_eq!(
        session.pipeline.active_video_requirement(),
        Some(&supported_requirement)
    );
}

#[test]
fn refinement_reconfigures_decoder_when_output_contract_changes() {
    let mut session = PlayerSession::new();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    let tracks = vec![vp9_track_with_profile(1, Vp9Profile::Profile0)];
    install_tracks_for_capability_selection(&mut session, tracks.clone());

    session
        .select_default_video_track(&tracks, "fake media содержит video track")
        .expect("VP9 track без bit_depth должен выбраться через NV12 fallback");

    assert_eq!(
        session
            .pipeline
            .active_video_frame_contract()
            .map(|contract| contract.pixel_layout),
        Some(video_frame_contract::VideoFramePixelLayout::Nv12)
    );

    let refined_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
        .with_bit_depth(BitDepth::Ten)
        .with_chroma(ChromaSubsampling::Yuv420)
        .with_color(bt2020_pq_limited());

    session
        .refine_active_video_requirement(refined_requirement)
        .expect("10-bit refinement должен переконфигурировать decoder под P010");

    assert_eq!(
        session
            .pipeline
            .active_video_frame_contract()
            .map(|contract| contract.pixel_layout),
        Some(video_frame_contract::VideoFramePixelLayout::P010)
    );

    let configured_contracts: Vec<_> = fake_decoder
        .configured_streams()
        .iter()
        .map(|config| config.frame_contract.pixel_layout)
        .collect();
    assert_eq!(
        configured_contracts,
        vec![
            video_frame_contract::VideoFramePixelLayout::Nv12,
            video_frame_contract::VideoFramePixelLayout::P010,
        ],
        "decoder должен быть переинициализирован под refined P010 contract"
    );
}

#[test]
fn deferred_bitstream_selection_preserves_selected_track() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    install_active_test_video_backend(&mut session);
    let mut video_track = fake_track(7, TrackKind::Video);
    let mut container_metadata = VideoTrackMetadata::empty();
    container_metadata.coded_width = Some(3840);
    container_metadata.coded_height = Some(2160);
    container_metadata.color = Some(bt2020_pq_limited());
    video_track.video = Some(container_metadata);
    let video_tracks = vec![video_track];
    let video_track_id = video_tracks[0].id;

    session
        .select_default_video_track(&video_tracks, "fake media содержит video track")
        .expect("неполный HDR metadata должен выбрать track до packet refinement");

    let active_requirement = session
        .pipeline
        .active_video_requirement()
        .expect("deferred selection должен сохранить active requirement");

    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(video_track_id)
    );
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(video_track_id)
    );
    assert!(video_requirement_needs_packet_refinement(
        active_requirement
    ));
    assert!(session.can_defer_packet_refinement(active_requirement));
}

#[test]
fn incomplete_vp9_hdr_container_metadata_waits_for_packet_refinement() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9)
        .with_resolution(3840, 2160)
        .with_color(bt2020_pq_limited());

    let error = session
        .validate_video_decode_requirement(&requirement)
        .expect_err("container-only VP9 HDR metadata is not strict enough yet");

    assert_eq!(error.kind, PlayerErrorKind::UnsupportedHdrMode);
    assert!(video_requirement_needs_packet_refinement(&requirement));
    assert!(session.can_defer_packet_refinement(&requirement));
}

fn install_tracks_for_capability_selection(session: &mut PlayerSession, tracks: Vec<TrackInfo>) {
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(tracks.clone(), Some(Duration::from_secs(30)), seek_log);

    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks);
}

/// Устанавливает реальный decoder boundary для тестов обычного track selection.
/// Capability report сам по себе больше не изображает существующий decoder thread.
fn install_active_test_video_backend(session: &mut PlayerSession) {
    let backend_id = test_decode_backend_id();
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        backend_id.as_str(),
        SharedFakeVideoDecoderThread::new(),
    ));
}

fn capabilities_with_hardware_and_ffmpeg_outputs() -> capability_core::SystemCapabilities {
    let hardware_backend_id = DecodeBackendId::vaapi();
    let ffmpeg_backend_id =
        DecodeBackendId::new("ffmpeg-sw").expect("test FFmpeg backend id is valid");
    let h264_main = h264_main_yuv420_8bit_format();
    let vp9_profile0 = vp9_profile0_yuv420_8bit_format();
    let hardware_outputs = vec![
        SupportedVideoOutput {
            backend: hardware_backend_id.clone(),
            decode_format: h264_main.clone(),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        },
        SupportedVideoOutput {
            backend: hardware_backend_id.clone(),
            decode_format: vp9_profile0.clone(),
            frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        },
    ];
    let ffmpeg_outputs = vec![
        SupportedVideoOutput {
            backend: ffmpeg_backend_id.clone(),
            decode_format: h264_main,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        },
        SupportedVideoOutput {
            backend: ffmpeg_backend_id.clone(),
            decode_format: vp9_profile0,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        },
    ];
    let mut playable_video_outputs = Vec::new();
    playable_video_outputs.extend(hardware_outputs.iter().cloned());
    playable_video_outputs.extend(ffmpeg_outputs.iter().cloned());

    capability_core::SystemCapabilities {
        schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![
            BackendCapabilities {
                backend_id: hardware_backend_id,
                display_name: "Test VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: hardware_outputs,
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            },
            BackendCapabilities {
                backend_id: ffmpeg_backend_id,
                display_name: "Test FFmpeg software".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: ffmpeg_outputs,
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            },
        ],
        render_backends: vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        playable_video_outputs,
    }
}

fn h264_main_yuv420_8bit_format() -> SupportedVideoDecodeFormat {
    SupportedVideoDecodeFormat {
        codec: VideoCodec::H264,
        profile: VideoProfile::H264(H264Profile::Main),
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        max_width: Some(4096),
        max_height: Some(4096),
        max_fps: None,
        hdr_input: false,
    }
}

fn vp9_profile0_yuv420_8bit_format() -> SupportedVideoDecodeFormat {
    SupportedVideoDecodeFormat {
        codec: VideoCodec::Vp9,
        profile: VideoProfile::Vp9(Vp9Profile::Profile0),
        bit_depth: BitDepth::Eight,
        chroma: ChromaSubsampling::Yuv420,
        max_width: Some(4096),
        max_height: Some(4096),
        max_fps: None,
        hdr_input: false,
    }
}

fn vp9_track_with_profile(track_id: u32, profile: Vp9Profile) -> TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Video);
    let mut metadata = VideoTrackMetadata::empty();
    metadata.profile = Some(VideoProfile::Vp9(profile));
    track.video = Some(metadata);
    track
}

fn h264_track_with_avcc(track_id: u32) -> TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Video);
    let mut metadata = VideoTrackMetadata::empty();
    metadata.profile = Some(VideoProfile::H264(H264Profile::ConstrainedBaseline));
    metadata.bit_depth = Some(BitDepth::Eight);
    metadata.chroma = Some(ChromaSubsampling::Yuv420);
    metadata.coded_width = Some(1280);
    metadata.coded_height = Some(720);
    metadata.packet_framing = media_core::VideoPacketFraming::LengthPrefixedFromCodecConfiguration;
    track.codec_id = "V_MPEG4/ISO/AVC".to_string();
    track.codec_private = Some(h264_avcc_codec_private());
    track.video = Some(metadata);
    track
}

/// Собирает neutral track для `avc3`, у которого parameter sets находятся в media samples.
fn h264_avc3_track(track_id: u32) -> TrackInfo {
    let mut track = h264_track_with_avcc(track_id);
    track.codec_private = Some(h264_avc3_codec_private());
    let video_metadata = track
        .video
        .as_mut()
        .expect("H.264 test track должен иметь video metadata");
    video_metadata.profile = Some(VideoProfile::H264(H264Profile::Main));
    video_metadata.packet_framing =
        media_core::VideoPacketFraming::LengthPrefixedWithInBandParameterSets;
    track
}

fn h264_avcc_codec_private() -> Bytes {
    Bytes::from_static(&[
        1, 0x42, 0xe0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xe0, 0x1f, 0x01, 0x00, 0x01, 0x68,
    ])
}

/// Точный минимальный `avcC` acceptance live-потока: length size есть, SPS/PPS отсутствуют.
fn h264_avc3_codec_private() -> Bytes {
    Bytes::from_static(&[0x01, 0x4d, 0x40, 0x1f, 0xff, 0xe0, 0x00])
}

/// Собирает length-prefixed sample, где SPS/PPS/IDR действительно приходят in-band.
fn h264_avc3_access_unit_with_in_band_parameter_sets() -> Bytes {
    Bytes::from_static(&[
        0x00, 0x00, 0x00, 0x04, 0x67, 0x4d, 0x40, 0x1f, 0x00, 0x00, 0x00, 0x01, 0x68, 0x00, 0x00,
        0x00, 0x02, 0x65, 0x88,
    ])
}

fn h264_avcc_codec_private_with_zeroed_reserved_bits() -> Bytes {
    Bytes::from_static(&[
        1, 0x42, 0xe0, 0x1f, 0x03, 0x01, 0x00, 0x04, 0x67, 0x42, 0xe0, 0x1f, 0x01, 0x00, 0x01, 0x68,
    ])
}

fn h265_track_with_hvcc(track_id: u32) -> TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Video);
    let mut metadata = VideoTrackMetadata::empty();
    metadata.profile = Some(VideoProfile::H265(codec_core::H265Profile::Main10));
    metadata.bit_depth = Some(BitDepth::Ten);
    metadata.chroma = Some(ChromaSubsampling::Yuv420);
    metadata.coded_width = Some(3840);
    metadata.coded_height = Some(2160);
    metadata.packet_framing = media_core::VideoPacketFraming::LengthPrefixedFromCodecConfiguration;
    track.codec_id = "V_MPEGH/ISO/HEVC".to_string();
    track.codec_private = Some(h265_hvcc_codec_private());
    track.video = Some(metadata);
    track
}

fn h265_hvcc_codec_private() -> Bytes {
    let mut record_bytes = vec![
        1,
        2,
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
        0xfd,
        0xfa,
        0xfa,
        0,
        0,
        0b0000_1111,
        0,
    ];
    set_h265_profile_compatibility_flag(&mut record_bytes, 2);
    Bytes::from(record_bytes)
}

fn set_h265_profile_compatibility_flag(record_bytes: &mut [u8], profile_idc: u8) {
    let flag_index = usize::from(profile_idc);
    let byte_index = 2 + flag_index / 8;
    let bit_index = 7 - (flag_index % 8);
    record_bytes[byte_index] |= 1 << bit_index;
}

/// Capabilities, где hardware (VA-API) умеет только H.264, а FFmpeg software — H.264 и VP9.
///
/// Это даёт сценарий `auto`: активный VA-API не тянет VP9, но software backend может.
fn capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9() -> capability_core::SystemCapabilities {
    let hardware_backend_id = DecodeBackendId::vaapi();
    let ffmpeg_backend_id =
        DecodeBackendId::new("ffmpeg-sw").expect("test FFmpeg backend id is valid");
    let h264_main = h264_main_yuv420_8bit_format();
    let vp9_profile0 = vp9_profile0_yuv420_8bit_format();

    let hardware_outputs = vec![SupportedVideoOutput {
        backend: hardware_backend_id.clone(),
        decode_format: h264_main.clone(),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
    }];
    let ffmpeg_outputs = vec![
        SupportedVideoOutput {
            backend: ffmpeg_backend_id.clone(),
            decode_format: h264_main,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        },
        SupportedVideoOutput {
            backend: ffmpeg_backend_id.clone(),
            decode_format: vp9_profile0,
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
        },
    ];

    let mut playable_video_outputs = Vec::new();
    playable_video_outputs.extend(hardware_outputs.iter().cloned());
    playable_video_outputs.extend(ffmpeg_outputs.iter().cloned());

    capability_core::SystemCapabilities {
        schema_version: capability_core::CURRENT_CAPABILITY_SCHEMA_VERSION,
        probed_at_unix_seconds: 1,
        video_backends: vec![
            BackendCapabilities {
                backend_id: hardware_backend_id,
                display_name: "Test VA-API".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: hardware_outputs,
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            },
            BackendCapabilities {
                backend_id: ffmpeg_backend_id,
                display_name: "Test FFmpeg software".to_string(),
                status: BackendProbeStatus::Available,
                driver: BackendDriverInfo::default(),
                raw_supported_outputs: ffmpeg_outputs,
                raw_profiles: Vec::new(),
                raw_entrypoints: Vec::new(),
                raw_rt_formats: Vec::new(),
                quirks: Vec::new(),
                diagnostics: Vec::new(),
            },
        ],
        render_backends: vec![RenderCapabilities::wgpu_nv12(Some(4096))],
        playable_video_outputs,
    }
}

/// VP9 трек с полными metadata, чтобы requirement точно совпал с software output.
fn vp9_full_track(track_id: u32) -> TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Video);
    let mut metadata = VideoTrackMetadata::empty();
    metadata.profile = Some(VideoProfile::Vp9(Vp9Profile::Profile0));
    metadata.bit_depth = Some(BitDepth::Eight);
    metadata.chroma = Some(ChromaSubsampling::Yuv420);
    metadata.coded_width = Some(1920);
    metadata.coded_height = Some(1080);
    track.codec_id = "V_VP9".to_string();
    track.video = Some(metadata);
    track
}

#[test]
fn active_hardware_backend_requests_reselection_when_only_software_can_decode() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9());
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "vaapi",
        SharedFakeVideoDecoderThread::new(),
    ));
    let _ = session.take_events();

    let tracks = vec![vp9_full_track(1)];
    session
        .select_default_video_track(&tracks, "media содержит VP9 video track")
        .expect("VP9 трек должен запросить reselection, а не упасть как unsupported");

    assert!(
        session.has_pending_video_backend_reselection(),
        "видео должно ждать совместимого software backend-а"
    );
    assert!(
        session.take_events().iter().any(|event| matches!(
            event,
            PlayerEvent::VideoBackendSelectionRequested(request)
                if !request.decodable_by_active_backend
        )),
        "shell должен получить запрос на подбор backend-а с decodable_by_active_backend=false"
    );
}

/// Воспроизводит late track discovery у HLS/TS: media уже установлен без video
/// decoder-а, затем demuxer публикует playable video track. Player обязан отложить
/// packets до backend install, а не объявлять отсутствующий decoder совместимым.
#[test]
fn late_video_track_without_decoder_requests_backend_before_selection() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9());
    let _ = session.take_events();
    let late_tracks = vec![vp9_full_track(1)];
    install_tracks_for_capability_selection(&mut session, late_tracks.clone());
    session.set_playback_state(PlaybackState::Playing);

    session
        .select_default_video_track(&late_tracks, "late HLS update содержит VP9 video track")
        .expect("playable late video track должен запросить backend reselection");

    assert!(session.has_pending_video_backend_reselection());
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        None,
        "video track нельзя активировать до появления decoder thread"
    );
    assert!(session.take_events().iter().any(|event| matches!(
        event,
        PlayerEvent::VideoBackendSelectionRequested(request)
            if !request.decodable_by_active_backend
    )));

    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "ffmpeg-sw",
        SharedFakeVideoDecoderThread::new(),
    ));

    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(TrackId::new(1)),
        "late video track должен активироваться после установки decoder-а"
    );
    assert_eq!(
        session.playback_state(),
        PlaybackState::Playing,
        "forward keyframe bootstrap не должен превращать Playing в Paused"
    );
    assert!(
        !session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekRequested(_))),
        "late-track bootstrap не должен требовать ещё не доказанный seek anchor"
    );
}

/// Закрепляет полный runtime route исходной HLS/TS регрессии: late H.264 Annex-B
/// track ждёт backend, сохраняет стартовый IDR, проходит decoder boundary и
/// действительно становится кадром, выбранным presentation scheduler-ом.
#[test]
fn late_hls_h264_track_reaches_presentation_after_backend_install() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9());
    let video_track_id = TrackId::new(1);
    let mut late_h264_track = h264_track_with_avcc(video_track_id.get());
    late_h264_track.codec_private = None;
    let late_h264_metadata = late_h264_track
        .video
        .as_mut()
        .expect("H.264 test track должен иметь video metadata");
    late_h264_metadata.profile = Some(VideoProfile::H264(H264Profile::Main));
    late_h264_metadata.packet_framing = media_core::VideoPacketFraming::AnnexB;
    let late_tracks = vec![late_h264_track];
    install_tracks_for_capability_selection(&mut session, late_tracks.clone());
    session.set_playback_state(PlaybackState::Playing);

    session
        .select_default_video_track(&late_tracks, "late HLS update содержит H.264 Annex-B track")
        .expect("late H.264 track должен запросить совместимый backend");
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(99),
            Duration::ZERO,
            session.pipeline.seek_generation(),
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x77]),
            PacketKeyframe::Keyframe,
        ));
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            video_track_id,
            Duration::ZERO,
            session.pipeline.seek_generation(),
            Bytes::from_static(&[0, 0, 0, 1, 0x65, 0x88]),
            PacketKeyframe::Keyframe,
        ));

    // В реальном app shell устанавливает backend только после отдельного worker tick-а.
    // Startup IDR должен пережить этот round-trip, иначе видео начнётся со следующего GOP.
    let waiting_for_shell_tick = session.tick(PlayerTickContext::new(Instant::now()));
    assert_eq!(waiting_for_shell_tick.video_frames_presented, 0);
    assert_eq!(
        session.pipeline.pending_video_packet_len(),
        1,
        "чужой packet должен быть удалён, а startup HLS keyframe — дождаться backend-а"
    );

    let decoder = SharedFakeVideoDecoderThread::new();
    decoder.decode_next_packet_as_frame(Duration::ZERO, 501);
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        DecodeBackendId::vaapi().as_str(),
        decoder.clone(),
    ));

    let first_tick = session.tick(PlayerTickContext::new(Instant::now()));
    let second_tick = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(decoder.sent_packets().len(), 1);
    assert_eq!(
        first_tick.video_frames_presented + second_tick.video_frames_presented,
        1,
        "сохранённый HLS keyframe должен дойти до presentation, а не только до decoder send"
    );
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
}

#[test]
fn installing_compatible_backend_activates_pending_video_track() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9());
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "vaapi",
        SharedFakeVideoDecoderThread::new(),
    ));
    install_fake_media_with_seekability(
        &mut session,
        vec![vp9_full_track(1)],
        DemuxSeekability::Seekable,
    );
    assert!(session.has_pending_video_backend_reselection());

    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "ffmpeg-sw",
        SharedFakeVideoDecoderThread::new(),
    ));

    assert!(
        !session.has_pending_video_backend_reselection(),
        "после установки software backend-а отложенный выбор должен закрыться"
    );
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(TrackId::new(1)),
        "VP9 трек должен активироваться на совместимом backend-е"
    );
    assert!(
        session.snapshot().last_error.is_none(),
        "штатный reseek после backend swap не должен публиковать recoverable error"
    );
}

#[test]
fn switching_to_hardware_backend_recomputes_active_frame_contract() {
    // SW->HW media switch: ffmpeg-sw активен, новый ролик HW-decodable (VP9 Profile0)
    // выбирается на software backend-е с host-upload контрактом; затем app свапает
    // backend на vaapi. configure_active_video_decoder_stream обязан пересчитать
    // frame contract под vaapi (DMA-BUF NV12), а не реюзать software host-upload —
    // иначе hardware decoder падает с UnsupportedFrameContract.
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_hardware_and_ffmpeg_outputs());

    let ffmpeg_decoder = SharedFakeVideoDecoderThread::new();
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "ffmpeg-sw",
        ffmpeg_decoder.clone(),
    ));

    install_fake_media_with_seekability(
        &mut session,
        vec![vp9_full_track(1)],
        DemuxSeekability::Seekable,
    );
    assert!(
        !session.has_pending_video_backend_reselection(),
        "software backend сам декодит VP9 — отложенного reselection быть не должно"
    );
    let software_contract = ffmpeg_decoder
        .configured_streams()
        .last()
        .expect("software decoder должен получить stream config")
        .frame_contract;
    assert_eq!(
        software_contract,
        VideoFrameContract::host_yuv420_planar8(),
        "на ffmpeg-sw трек должен идти через software host-upload контракт"
    );

    let vaapi_decoder = SharedFakeVideoDecoderThread::new();
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "vaapi",
        vaapi_decoder.clone(),
    ));

    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(TrackId::new(1)),
        "видео должно остаться выбранным после свапа на hardware backend"
    );
    let hardware_contract = vaapi_decoder
        .configured_streams()
        .last()
        .expect("hardware decoder должен получить пересчитанный stream config")
        .frame_contract;
    assert_eq!(
        hardware_contract,
        VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::ComposedLayers),
        "после свапа на vaapi контракт обязан стать DMA-BUF, а не остаться software host-upload"
    );
}

#[test]
fn failed_settings_backend_rebuild_restores_previous_decoder_and_track_selection() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_hardware_and_ffmpeg_outputs());

    let previous_decoder = SharedFakeVideoDecoderThread::new();
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "ffmpeg-sw",
        previous_decoder.clone(),
    ));
    install_fake_media_with_seekability(
        &mut session,
        vec![vp9_full_track(1)],
        DemuxSeekability::Seekable,
    );
    let previous_requirement = session
        .pipeline
        .active_video_requirement()
        .expect("active media должен сохранить video requirement")
        .clone();
    let previous_configure_count = previous_decoder.configured_streams().len();

    let failing_decoder = SharedFakeVideoDecoderThread::new();
    failing_decoder.push_configure_result(video_core::VideoStreamConfigResult::Fatal(
        DecodeThreadError::new("replacement configure failed"),
    ));

    let error = session
        .install_video_backend_with_intent(
            crate::StartedVideoBackend::from_decoder_thread("vaapi", failing_decoder),
            crate::PlayerVideoBackendInstallIntent::SettingsReconfigure,
        )
        .expect_err("невалидный replacement decoder должен запустить rollback");

    assert!(matches!(error, crate::PlayerRuntimeApplyError::Fatal(_)));
    assert_eq!(session.pipeline.video_backend_name(), "Shared fake decoder");
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(TrackId::new(1))
    );
    assert_eq!(
        session.pipeline.active_video_requirement(),
        Some(&previous_requirement)
    );
    assert_eq!(
        previous_decoder.configured_streams().len(),
        previous_configure_count,
        "rollback не должен повторно мутировать уже сконфигурированный прежний decoder"
    );
    assert!(
        previous_decoder.stream_config().is_some(),
        "прежний decoder должен сохранить рабочую stream configuration"
    );
}

#[test]
fn swapping_active_backend_advances_render_generation_but_first_install_does_not() {
    // Живой свап backend-а обязан продвинуть render generation, чтобы удержанные
    // present-кадры/lease старого backend-а (например P010 DMA-BUF от VA-API) отсеклись
    // на renderer boundary и не попали в materializer нового класса. Первый install при
    // старте (активного decoder-а ещё нет) лишний bump делать не должен.
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_hardware_and_ffmpeg_outputs());

    let generation_before_first_install = session.pipeline.render_generation();
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "vaapi",
        SharedFakeVideoDecoderThread::new(),
    ));
    assert_eq!(
        session.pipeline.render_generation(),
        generation_before_first_install,
        "первый install backend-а не должен двигать render generation"
    );

    install_fake_media_with_seekability(
        &mut session,
        vec![vp9_full_track(1)],
        DemuxSeekability::Seekable,
    );

    let generation_before_swap = session.pipeline.render_generation();
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "ffmpeg-sw",
        SharedFakeVideoDecoderThread::new(),
    ));
    assert_eq!(
        session.pipeline.render_generation(),
        generation_before_swap.wrapping_add(1),
        "живой свап активного backend-а обязан продвинуть render generation"
    );
}

#[test]
fn rejecting_pending_video_backend_fails_with_unsupported_error() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_vaapi_h264_only_and_ffmpeg_h264_vp9());
    session.set_video_backend(crate::StartedVideoBackend::from_decoder_thread(
        "vaapi",
        SharedFakeVideoDecoderThread::new(),
    ));
    let tracks = vec![vp9_full_track(1)];
    session
        .select_default_video_track(&tracks, "media содержит VP9 video track")
        .expect("VP9 трек должен запросить reselection");
    assert!(session.has_pending_video_backend_reselection());
    let _ = session.take_events();

    session.reject_pending_video_backend("hardware preference не допускает software".to_string());

    assert!(
        !session.has_pending_video_backend_reselection(),
        "отклонённый выбор не должен оставаться pending"
    );
    assert!(
        session.take_events().iter().any(|event| matches!(
            event,
            PlayerEvent::FatalError(error) if error.kind == PlayerErrorKind::UnsupportedVideoCodec
        )),
        "отказ shell-а должен дать typed unsupported fatal error"
    );
}

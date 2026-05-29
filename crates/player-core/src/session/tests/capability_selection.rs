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
fn requested_video_track_selection_sets_matching_requirement() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
    let _ = session.take_events();
    let video_track_id = TrackId::new(7);
    let tracks = vec![vp9_track_with_profile(7, Vp9Profile::Profile0)];
    install_tracks_for_capability_selection(&mut session, tracks);

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
    assert!(session.take_events().iter().any(|event| {
        matches!(event, PlayerEvent::VideoTrackSelected(track_id) if *track_id == video_track_id)
    }));
}

#[test]
fn requested_video_track_selection_replaces_stale_requirement() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_vp9_profile0());
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
fn deferred_bitstream_selection_preserves_selected_track() {
    let mut session = PlayerSession::new();
    session.set_system_capabilities(capabilities_with_phase10_vp9_profile2_hdr());
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

fn vp9_track_with_profile(track_id: u32, profile: Vp9Profile) -> TrackInfo {
    let mut track = fake_track(track_id, TrackKind::Video);
    let mut metadata = VideoTrackMetadata::empty();
    metadata.profile = Some(VideoProfile::Vp9(profile));
    track.video = Some(metadata);
    track
}

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

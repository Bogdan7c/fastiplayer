//! `SelectVideoTrack` для уже активной дорожки — no-op (backlog #21).
//!
//! Повторный выбор не должен переконфигурировать decoder более бедным config-ом из
//! container metadata и не должен публиковать событие; остальные пути выбора не меняются.

use super::test_support::*;
use super::*;

/// VP9 track без profile/bit depth: точный contract даёт только refinement.
fn vp9_track_without_profile(track_id: u32) -> TrackInfo {
    fake_track(track_id, TrackKind::Video)
}

/// Устанавливает media с двумя VP9 дорожками без выбора video.
fn install_two_vp9_tracks(session: &mut PlayerSession) -> Vec<TrackInfo> {
    let tracks = vec![vp9_track_without_profile(1), vp9_track_without_profile(2)];
    let demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::new(Mutex::new(Vec::new())),
    );
    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    tracks
}

/// Выбирает дорожку 1 и уточняет её до 10-bit P010, как это делает packet preflight.
fn select_and_refine_first_track(session: &mut PlayerSession) {
    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(TrackId::new(1)))
        .expect("the first VP9 track must be selected");
    session
        .refine_active_video_requirement(
            VideoDecodeRequirement::new(VideoCodec::Vp9)
                .with_profile(VideoProfile::Vp9(Vp9Profile::Profile2))
                .with_bit_depth(BitDepth::Ten)
                .with_chroma(ChromaSubsampling::Yuv420)
                .with_color(bt2020_pq_limited()),
        )
        .expect("10-bit refinement must reconfigure the decoder under P010");
    let _ = session.take_events();
}

/// Сколько `VideoTrackSelected` событий накопила session.
fn video_track_selected_events(session: &mut PlayerSession) -> Vec<TrackId> {
    session
        .take_events()
        .into_iter()
        .filter_map(|event| match event {
            PlayerEvent::VideoTrackSelected(track_id) => Some(track_id),
            _ => None,
        })
        .collect()
}

/// Pixel layout активного contract-а pipeline-а.
fn active_pixel_layout(
    session: &PlayerSession,
) -> Option<video_frame_contract::VideoFramePixelLayout> {
    session
        .pipeline
        .active_video_frame_contract()
        .map(|contract| contract.pixel_layout)
}

#[test]
fn reselecting_active_video_track_keeps_refined_decoder_stream_and_emits_nothing() {
    let mut session = PlayerSession::new();
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    install_two_vp9_tracks(&mut session);
    select_and_refine_first_track(&mut session);
    let configured_before = decoder.configured_streams().len();
    let requirement_before = session.pipeline.active_video_requirement().cloned();

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(TrackId::new(1)))
        .expect("re-selecting the active track must succeed");

    assert_eq!(decoder.configured_streams().len(), configured_before);
    assert_eq!(
        active_pixel_layout(&session),
        Some(video_frame_contract::VideoFramePixelLayout::P010)
    );
    assert_eq!(
        session.pipeline.active_video_requirement().cloned(),
        requirement_before
    );
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(TrackId::new(1))
    );
    assert!(video_track_selected_events(&mut session).is_empty());
}

#[test]
fn selecting_other_video_track_still_reconfigures_and_emits_selection() {
    let mut session = PlayerSession::new();
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    install_two_vp9_tracks(&mut session);
    select_and_refine_first_track(&mut session);
    let configured_before = decoder.configured_streams().len();

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(TrackId::new(2)))
        .expect("another track must be selected");

    assert_eq!(decoder.configured_streams().len(), configured_before + 1);
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(TrackId::new(2))
    );
    assert_eq!(
        active_pixel_layout(&session),
        Some(video_frame_contract::VideoFramePixelLayout::Nv12)
    );
    assert_eq!(
        video_track_selected_events(&mut session),
        vec![TrackId::new(2)]
    );
}

#[test]
fn reselecting_track_without_active_decoder_keeps_regular_selection_path() {
    let mut session = PlayerSession::new();
    let tracks = install_two_vp9_tracks(&mut session);
    // Выбор записан в pipeline, но decoder-а нет: no-op здесь спрятал бы отсутствие ресурса.
    session
        .pipeline
        .select_video_track(tracks[0].id, VideoDecodeRequirement::new(VideoCodec::Vp9));

    session
        .dispatch_command(PlayerCommand::SelectVideoTrack(TrackId::new(1)))
        .expect("selection without decoder keeps the regular path");

    assert_eq!(
        video_track_selected_events(&mut session),
        vec![TrackId::new(1)],
        "without an active decoder the command must not be swallowed as a no-op"
    );
}

#[test]
fn unknown_video_track_is_rejected_without_touching_active_selection() {
    let mut session = PlayerSession::new();
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    install_two_vp9_tracks(&mut session);
    select_and_refine_first_track(&mut session);
    let configured_before = decoder.configured_streams().len();

    let error = session
        .dispatch_command(PlayerCommand::SelectVideoTrack(TrackId::new(9)))
        .expect_err("unknown track must be rejected");

    assert_eq!(error.kind, PlayerErrorKind::InvalidCommand);
    assert_eq!(decoder.configured_streams().len(), configured_before);
    assert_eq!(
        session.pipeline.selected_video_track_id(),
        Some(TrackId::new(1))
    );
    assert_eq!(
        active_pixel_layout(&session),
        Some(video_frame_contract::VideoFramePixelLayout::P010)
    );
    assert!(video_track_selected_events(&mut session).is_empty());
}

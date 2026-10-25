//! Сквозной сценарий режима Dynamic на настоящем `PlayerSession`: аппаратный fake-декодер
//! устойчиво отстаёт -> событие -> установка программного бэкенда тем же intent-ом, что
//! использует приложение (`PipelineDemand`) -> позиция сохранена, кадры нового бэкенда
//! доходят до показа.
//!
//! Политика «переключать ли» живёт в app-egui и проверена там; здесь доказывается, что
//! сам player выдерживает такую смену посреди воспроизведения.

use super::decoder_pace::{FRAME_STEP, TICK_STEP};
use super::*;
use crate::{PlaybackState, PlayerEvent};

/// Demuxer бесконечного видео: на каждый запрос отдаёт следующий packet с шагом `FRAME_STEP`.
struct EndlessVideoDemuxer {
    tracks: Vec<media_core::TrackInfo>,
    next_pts: Duration,
}

impl media_core::Demuxer for EndlessVideoDemuxer {
    fn tracks(&self) -> &[media_core::TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        None
    }

    fn next_event(&mut self) -> anyhow::Result<media_core::DemuxReadEvent> {
        let pts = self.next_pts;
        self.next_pts += FRAME_STEP;
        Ok(media_core::DemuxReadEvent::Packet(
            media_core::Packet::new_unbounded(
                TrackId::new(1),
                TrackKind::Video,
                pts,
                None,
                pts.is_zero(),
                Bytes::from_static(b"frame"),
            ),
        ))
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<media_core::DemuxSeekResult> {
        self.next_pts = timestamp;
        Ok(media_core::DemuxSeekResult {
            requested_position: media_core::MediaTime::from_duration(timestamp),
            actual_position: media_core::MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }
}

/// Играющий файл с аппаратным fake-декодером, который принимает packets, но кадров не отдаёт.
fn playing_file_with_lagging_hardware_decoder() -> (PlayerSession, RecordingVideoDecoderThread) {
    let hardware_decoder = RecordingVideoDecoderThread::new().with_stream_configuration();
    let mut track = video_track_for_tests(1);
    let mut metadata = media_core::VideoTrackMetadata::empty();
    metadata.profile = Some(codec_core::VideoProfile::Vp9(
        codec_core::Vp9Profile::Profile0,
    ));
    metadata.bit_depth = Some(codec_core::BitDepth::Eight);
    metadata.chroma = Some(codec_core::ChromaSubsampling::Yuv420);
    metadata.coded_width = Some(1920);
    metadata.coded_height = Some(1080);
    track.video = Some(metadata);
    let tracks = vec![track];

    let mut session = PlayerSession::new();
    session
        .pipeline
        .set_video_decoder_thread(hardware_decoder.clone());
    session.pipeline.install_opened_media(
        Box::new(EndlessVideoDemuxer {
            tracks: tracks.clone(),
            next_pts: Duration::ZERO,
        }),
        None,
        None,
        tracks,
    );
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Vp9)
            .with_bit_depth(codec_core::BitDepth::Eight)
            .with_chroma(codec_core::ChromaSubsampling::Yuv420),
    );
    session.begin_playback_without_media_for_test().unwrap();
    (session, hardware_decoder)
}

/// Тикает, пока не придёт событие отставания; возвращает его доказательство и момент времени.
fn tick_until_falling_behind(
    session: &mut PlayerSession,
    start: Instant,
    first_tick: u32,
    mut before_tick: impl FnMut(u32, &mut PlayerSession),
) -> (crate::DecoderPaceEvidence, u32) {
    for tick_index in first_tick..first_tick + 1_200 {
        before_tick(tick_index, session);
        let _ = session.tick(PlayerTickContext::new(start + TICK_STEP * tick_index));
        for event in session.take_events() {
            if let PlayerEvent::VideoDecoderFallingBehind(evidence) = event {
                return (evidence, tick_index);
            }
        }
    }
    panic!("the falling-behind event never arrived");
}

#[test]
fn hardware_decoder_falling_behind_then_software_swap_keeps_position_and_shows_frames() {
    let (mut session, _hardware_decoder) = playing_file_with_lagging_hardware_decoder();
    let start = Instant::now();

    // Аппаратный декодер молчит -> устойчивое отставание -> событие.
    let (hardware_evidence, event_tick) =
        tick_until_falling_behind(&mut session, start, 0, |_, _| {});
    let generation_before_swap = session.pipeline.render_generation();
    assert_eq!(hardware_evidence.render_generation, generation_before_swap);
    let position_before_swap = session.snapshot().current_position;

    // Приложение ставит программный бэкенд тем же intent-ом, что и обычный фолбэк.
    let software_decoder = RecordingVideoDecoderThread::new().with_stream_configuration();
    session
        .install_video_backend_with_intent(
            StartedVideoBackend::from_decoder_thread("ffmpeg-sw", software_decoder.clone()),
            crate::PlayerVideoBackendInstallIntent::PipelineDemand,
        )
        .expect("software backend must install over the lagging hardware one");

    assert_eq!(
        session.pipeline.render_generation(),
        generation_before_swap + 1,
        "a live swap must advance the render generation so old hardware frames are dropped"
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(
        session.snapshot().current_position >= position_before_swap,
        "the swap must not rewind the playback position"
    );
    // Точный seek в прежнюю позицию: player сам просит seek в позицию на момент смены.
    let restore_request = session
        .take_events()
        .into_iter()
        .find_map(|event| match event {
            PlayerEvent::SeekRequested(request) => Some(request),
            _ => None,
        })
        .expect("the swap must start an exact re-seek to the current position");
    assert_eq!(restore_request.mode, crate::SeekMode::Accurate);
    let crate::SeekTarget::Absolute(restore_target) = restore_request.target else {
        panic!(
            "the restore seek must be absolute, got {:?}",
            restore_request.target
        );
    };
    assert!(
        restore_target.as_duration() >= Duration::from_secs(5)
            && restore_target.as_duration().abs_diff(position_before_swap)
                < Duration::from_millis(200),
        "restore target {restore_target:?} must match the position {position_before_swap:?}"
    );

    // Новый (программный) декодер успевает: кадры доходят до показа с актуальным pts.
    let first_tick_after_swap = event_tick + 1;
    let mut shown_frame_pts = None;
    for tick_index in first_tick_after_swap..first_tick_after_swap + 120 {
        let pts = TICK_STEP * tick_index;
        software_decoder.push_decoded_frame(decoded_frame(pts, 10_000 + u64::from(tick_index)));
        let _ = session.tick(PlayerTickContext::new(start + TICK_STEP * tick_index));
        let _ = session.take_events();
        if let Some(frame) = session.pipeline.present_video_frame() {
            shown_frame_pts = Some(frame.pts);
        }
    }
    let shown_frame_pts = shown_frame_pts.expect("software frames must reach presentation");
    assert!(
        shown_frame_pts > FRAME_STEP,
        "the presented frame must belong to the continued timeline, got {shown_frame_pts:?}"
    );
    assert!(
        session.pipeline.render_generation() == generation_before_swap + 1,
        "no extra generation bumps while the software decoder keeps up"
    );
}

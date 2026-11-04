//! Низкая задержка с B-кадрами «до кадра» (сессия 09b2 моста, находка эфира Twitch).
//!
//! Эфир Twitch в низкой задержке (chunked CMAF) кладёт в SourceBuffer по ОДНОМУ кадру
//! на фрагмент `moof`+`mdat`, несколько фрагментов на `appendBuffer`, а видео — H.264
//! с пирамидой B-кадров. Порядок декодирования кадров GOP: I, P, B, b, b, P, …, а время
//! показа у них идёт иначе: P показывается ПОСЛЕ своих B. Полка, подававшая фрагменты
//! по началу времени показа, переставляла кадры в порядок показа: аппаратный декодер
//! получал b с `frame_num` 3 сразу после IDR («Invalid frame_num»), сбрасывался до
//! следующего ключевого кадра, и картинка стояла на одном кадре. Кадр, пришедший в
//! следующем `appendBuffer` позже «своего» P, вообще оставался на полке навсегда.
//!
//! Фикстура `video-h264-64-bframes-per-frame.mp4` (см. `fixtures/generate.sh`):
//! 40 кадров по 40 мс, закрытый GOP из 8 кадров, один кадр на фрагмент. Проверяется
//! и итог (все кадры показаны по порядку), и то, что пакеты дошли до плеера в
//! порядке декодирования: программный декодер теста прячет пропавшие опорные кадры,
//! аппаратный — нет.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "integration-тест: падение с понятным сообщением и есть результат"
)]

mod support;

use std::time::Duration;

use player_core::{PlaybackState, PreparedMedia};
use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, pushed_stream,
};
use support::media::{FragmentedMp4, own_fixture};
use support::packet_order::{PacketLoggingDemuxer, ReadVideoPacket, VideoPacketLog};
use support::playback::{PlaybackReport, open, play, play_controlled};

/// Фикстура: H.264 Main, пирамида B-кадров, один кадр на фрагмент.
const FIXTURE: &str = "video-h264-64-bframes-per-frame.mp4";
/// Сколько кадров в ролике.
const FRAMES_IN_CLIP: usize = 40;
/// Длительность кадра (25 к/с).
const FRAME: Duration = Duration::from_millis(40);
/// Полоса видео.
const VIDEO: LaneId = LaneId::new(0);

/// Итог игры: отчёт плеера и пакеты видео в порядке чтения.
struct ChunkedPlayback {
    report: PlaybackReport,
    packets: Vec<ReadVideoPacket>,
}

/// Страница шлёт init, потом фрагменты пачками по `frames_per_append` в одном
/// `appendBuffer` (как chunked CMAF), потом `endOfStream`; плеер играет до конца.
fn play_in_appends_of(frames_per_append: usize) -> ChunkedPlayback {
    let clip = FragmentedMp4::split(&own_fixture(FIXTURE));
    assert_eq!(
        clip.fragments.len(),
        FRAMES_IN_CLIP,
        "the fixture has one frame per fragment"
    );
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    writer
        .append(VIDEO, &clip.initialization)
        .expect("initialization");
    for chunk in clip.fragments.chunks(frames_per_append) {
        writer.append(VIDEO, &chunk.concat()).expect("chunk");
    }
    writer.end_of_stream().expect("end of stream");

    let log = VideoPacketLog::default();
    let demuxer = PacketLoggingDemuxer::new(open(&mut opener), log.clone());
    let report = play(demuxer, |_| {});
    ChunkedPlayback {
        report,
        packets: log.packets(),
    }
}

/// Все кадры ролика по одному разу, по порядку, шагом в кадр от первого.
fn assert_every_frame_presented_in_order(report: &PlaybackReport) {
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let presented: Vec<Duration> = report.frames.iter().map(|frame| frame.pts).collect();
    let first = *presented.first().expect("at least one frame presented");
    let expected: Vec<Duration> = (0..FRAMES_IN_CLIP)
        .map(|index| first + FRAME * u32::try_from(index).expect("small"))
        .collect();
    assert_eq!(presented, expected, "every frame, once, in time order");
}

/// Плеер прочитал каждый кадр ровно один раз и строго в порядке декодирования.
fn assert_decode_order(packets: &[ReadVideoPacket]) {
    assert_eq!(
        packets.len(),
        FRAMES_IN_CLIP,
        "every frame reaches the decoder: {packets:?}"
    );
    let out_of_order: Vec<_> = packets
        .windows(2)
        .filter(|pair| pair[1].decode_time <= pair[0].decode_time)
        .collect();
    assert!(
        out_of_order.is_empty(),
        "packets left decode order (a P after its B-frames breaks the reference chain): \
         {out_of_order:?}"
    );
    // Порядок декодирования действительно отличается от порядка показа: иначе
    // фикстура не проверяла бы ничего.
    assert!(
        packets
            .windows(2)
            .any(|pair| pair[1].presentation_time < pair[0].presentation_time),
        "the fixture must reorder frames (B-frames)"
    );
}

/// Как эфир Twitch: несколько однокадровых фрагментов в одном `appendBuffer`, границы
/// пачек режут группы B-кадров (P в одной пачке, его B — в следующей).
#[test]
fn single_frame_fragments_in_multi_frame_appends_reach_the_decoder_in_decode_order() {
    let playback = play_in_appends_of(3);
    assert_decode_order(&playback.packets);
    assert_every_frame_presented_in_order(&playback.report);
}

/// Каждый однокадровый фрагмент — отдельный `appendBuffer`: B-кадры приходят после
/// своего P и раньше его по времени показа — это продолжение, а не «время назад».
#[test]
fn single_frame_fragments_appended_one_by_one_play_every_frame() {
    let playback = play_in_appends_of(1);
    assert_decode_order(&playback.packets);
    assert_every_frame_presented_in_order(&playback.report);
}

/// Перемотка в середину группы кадров у однокадровых фрагментов: сегмент с целью — зависимый
/// кадр, подача с него не дала бы ни одного кадра до следующего ключевого. Цель лежит в
/// ПОСЛЕДНЕЙ группе ролика: следующего ключевого нет вовсе, как у начала записи Twitch, где
/// группа длиннее всего скачанного, и плеер стоял без единого кадра вечно. Кадры обязаны пойти
/// от самой цели: посадка отступает к ключевому кадру, а кадры до цели плеер прячет.
#[test]
fn a_seek_inside_a_group_of_pictures_shows_the_target_frame() {
    let clip = FragmentedMp4::split(&own_fixture(FIXTURE));
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, mut opener) = pushed_stream(policy).expect("stream");
    writer
        .add_lane(VIDEO, PushedContainer::FragmentedMp4)
        .expect("video lane");
    // Перемотка требует известной длительности.
    writer.set_duration(PushedDuration::Finite(
        FRAME * u32::try_from(FRAMES_IN_CLIP + 2).expect("small"),
    ));
    writer
        .append(VIDEO, &clip.initialization)
        .expect("initialization");
    for fragment in &clip.fragments {
        writer.append(VIDEO, fragment).expect("fragment");
    }
    writer.end_of_stream().expect("end of stream");

    // Группы по 8 кадров: последняя начинается с 33-го кадра, цель — через два кадра после него.
    let target = Duration::from_millis(1_500);
    let mut seeked = false;
    let prepared = PreparedMedia::from_external_label(
        "pushed media seek inside a group of pictures",
        Box::new(open(&mut opener)),
    );
    let monitor = writer.monitor();
    let report = play_controlled(prepared, |control| {
        let position = control.observation().position;
        monitor.note_playback_position(position);
        if !seeked && position >= FRAME * 3 {
            seeked = true;
            control.seek(target);
        }
    });

    assert!(seeked, "the clip played long enough to seek");
    assert!(
        report.errors.is_empty(),
        "player errors: {:?}",
        report.errors
    );
    assert_eq!(report.final_state, PlaybackState::Ended);
    let (seek_tick, _) = report.seeks[0];
    let after_seek: Vec<Duration> = report
        .frames
        .iter()
        .filter(|frame| frame.tick > seek_tick)
        .map(|frame| frame.pts)
        .collect();
    let first = *after_seek.first().expect("frames after the seek");
    assert!(
        first.abs_diff(target) <= FRAME,
        "first frame after the seek at {first:?}, target {target:?}: {after_seek:?}"
    );
    assert!(
        after_seek
            .windows(2)
            .all(|pair| pair[1] > pair[0] && pair[1] - pair[0] <= FRAME),
        "frames after the seek go one by one: {after_seek:?}"
    );
}

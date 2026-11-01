//! Клик зрителя в эфире во время смены поколения на НАСТОЯЩЕМ плеере (04d, решение
//! владельца: клик в окне DVR исполняется, когда новое поколение показано).
//!
//! Настоящие: медиа-часть, плеер `BrowserPlayback` (рабочий поток, программный FFmpeg,
//! тихая карта звука, безоконный вывод) и потоки `pushed-media-source` на фикстурах
//! протокола; подделана только страница. Харнесс — общий с живым скрабом
//! (`live_scrub_real_player::ScrubChain`).
//!
//! Эфир не позиционируется при установке (03e): поколение уходит плееру с живого входа,
//! а клик — обычная перемотка плеера после показа, по правилам живого окна плеера
//! (`seekable_range` динамической шкалы), без своих правил медиа-части.

use super::live_scrub_real_player::{ScrubChain, near, seek_targets};
use super::*;
use crate::viewer_transport::ViewerTransportIntent;
use pushed_media_source::TimestampOffset;

/// Живой вход нового поколения: страница положила байты с 20 с.
const LIVE_JOIN: Duration = Duration::from_secs(20);
/// Шаг, с которым страница кладёт куски эфира (ролик фикстуры: видео 0,4 с, звук
/// ~0,427 с). Чуть больше звука: куски не налезают друг на друга (налезание полка
/// честно считает повтором и заменяет), а дыры (13–40 мс) меньше допуска непрерывности
/// полки (100 мс) — кусок эфира сплошной, как у настоящей страницы.
const PIECE: Duration = Duration::from_millis(440);

/// Поток эфира: полосы видео и звука, длительность `Live`, байтов ещё нет.
fn live_generation(session: &SessionRef, number: u32) -> (PushedStreamWriter, IntakeNotice) {
    let (mut writer, notice) = stream_generation(session, number, false);
    writer.set_duration(PushedDuration::Live);
    for lane in [0, 1] {
        writer
            .add_lane(LaneId::new(lane), PushedContainer::FragmentedMp4)
            .expect("lane");
    }
    (writer, notice)
}

/// Страница кладёт ролик фикстуры (0,4 с) со смещением `at` — кусок эфира.
fn deliver_piece_at(writer: &mut PushedStreamWriter, at: Duration) {
    let offset = TimestampOffset::from_micros(i64::try_from(at.as_micros()).expect("micros"));
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer.set_timestamp_offset(lane, offset).expect("offset");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
}

/// Эфир-поколение 1 играет на настоящем плеере; поколение 2 (новый `MediaSource`)
/// объявлено, у него кусок с живого входа. Писатели держит тест.
fn live_switch_in_progress() -> (
    ScrubChain,
    SessionRef,
    PushedStreamWriter,
    PushedStreamWriter,
) {
    let mut chain = ScrubChain::start();
    let current = session(1);
    chain.media.on_session_opened(&current);
    let (mut first, first_notice) = live_generation(&current, 1);
    deliver_piece_at(&mut first, Duration::ZERO);
    chain.media.on_intake(vec![first_notice], None);
    chain.drive_until_frame_near("the first live generation on screen", Duration::ZERO);

    let (mut second, second_notice) = live_generation(&current, 2);
    deliver_piece_at(&mut second, LIVE_JOIN);
    chain.media.on_intake(vec![second_notice], None);
    (chain, current, first, second)
}

/// Клик в окно DVR (раньше живого входа), пока поколение 2 открывается: странице —
/// один `seek`, она докладывает куски от клика до входа, и показанное поколение 2
/// встаёт на клик (кадр у цели на экране); перемотка после показа страницу не просит.
#[test]
fn a_click_into_the_dvr_window_during_a_live_switch_lands_there_once_shown() {
    let (mut chain, _current, _first, mut second) = live_switch_in_progress();
    let click = LIVE_JOIN - 2 * PIECE;
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(click), Instant::now());
    // Страница перемоталась по просьбе и положила куски от клика до живого входа.
    deliver_piece_at(&mut second, click);
    deliver_piece_at(&mut second, click + PIECE);

    chain.drive_until_frame_near("the click on screen", click);
    chain.drive_for(Duration::from_millis(200));
    assert_eq!(
        seek_targets(&chain.seen),
        vec![u64::try_from(click.as_micros()).expect("micros")],
        "exactly one page seek for the click"
    );
}

/// Клик вне окна, которое держит плеер (данных там нет, страница их не прислала):
/// медиа-часть своих правил не придумывает — после показа это обычная перемотка
/// плеера по правилам живого окна (цель вне окна плеер отвергает, эфир играет дальше).
/// Странице — по-прежнему один `seek`, на экран кадр у клика не попадает.
#[test]
fn a_click_outside_the_live_window_follows_the_players_live_seek_rules() {
    let (mut chain, _current, _first, _second) = live_switch_in_progress();
    let click = Duration::from_secs(5);
    chain
        .media
        .on_viewer_transport(ViewerTransportIntent::Seek(click), Instant::now());
    chain.drive_for(Duration::from_secs(2));

    let shown = chain.shown_frame_pts();
    assert!(
        !shown.is_some_and(|pts| near(pts, click)),
        "a frame at a place without data: {shown:?}"
    );
    // Правило живого окна плеера: цель вне `seekable_range` — `SeekTargetExpired`, эфир
    // играет дальше там, где он есть (кадр — внутри куска с живого входа).
    assert!(
        shown.is_some_and(|pts| (LIVE_JOIN..=LIVE_JOIN + PIECE).contains(&pts)),
        "the live generation must keep playing inside its window: {shown:?}"
    );
    assert_eq!(
        seek_targets(&chain.seen),
        vec![u64::try_from(click.as_micros()).expect("micros")],
        "exactly one page seek for the click"
    );
}

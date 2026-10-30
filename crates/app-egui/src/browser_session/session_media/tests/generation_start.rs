//! Старт нового поколения (03e): перемотка страницы, пришедшая, пока поколение
//! открывается, — его стартовая позиция; без неё — там, где есть данные всех полос.
//!
//! Фикстура протокола `video.mp4`: init (733 байта) и два фрагмента — с 0 и с 0,2 с;
//! `audio.mp4` — звук 0,4 с с нуля.

use player_core::{PreparedDemuxSeekLandingPolicy, PreparedInitialPosition};
use pushed_media_source::PushedDuration;

use super::*;

/// Конец init-сегмента видео фикстуры.
const VIDEO_INIT_END: usize = 733;
/// Конец первого фрагмента видео: дальше — фрагмент с 0,2 с.
const VIDEO_FIRST_FRAGMENT_END: usize = 5714;
/// Начало второго фрагмента видео.
const SECOND_VIDEO_FRAGMENT: Duration = Duration::from_millis(200);

/// Время страницы в микросекундах протокола.
fn page_time(position: Duration) -> MediaTime {
    MediaTime::from_micros(u64::try_from(position.as_micros()).expect("micros")).expect("time")
}

/// Поколение, чьё видео начинается со второго фрагмента (0,2 с), а звук — с нуля:
/// как новое поколение YouTube, где коробки лежат не с нуля.
fn generation_with_late_video(
    session: &SessionRef,
    number: u32,
) -> (PushedStreamWriter, IntakeNotice) {
    let (mut writer, notice) = stream_generation(session, number, false);
    writer.set_duration(PushedDuration::Finite(FIXTURE_CLIP));
    let video_file = fixture_media("video.mp4");
    for (lane, bytes) in [
        (
            0,
            [
                &video_file[..VIDEO_INIT_END],
                &video_file[VIDEO_FIRST_FRAGMENT_END..],
            ]
            .concat(),
        ),
        (1, fixture_media("audio.mp4")),
    ] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &bytes).expect("append");
    }
    writer.end_of_stream().expect("end of stream");
    (writer, notice)
}

/// Где стояло медиа `index`-й загрузки.
fn initial_position_of(harness: &Harness, index: usize) -> PreparedInitialPosition {
    harness.player.loaded_media.borrow()[index].initial_position
}

/// Цель стартовой позиции загрузки (`None` — с начала).
fn start_target(position: PreparedInitialPosition) -> Option<Duration> {
    match position {
        PreparedInitialPosition::Beginning => None,
        PreparedInitialPosition::PositionedAt {
            target_position, ..
        } => Some(target_position.as_duration()),
    }
}

/// Главное: перемотка страницы до загрузки нового поколения (живой YouTube: новое
/// поколение, затем `seek_started` на позицию зрителя) — стартовая позиция поколения.
/// Плеер получает медиа уже на ней, без отдельной перемотки и без нуля.
#[test]
fn a_page_seek_while_a_generation_opens_becomes_its_start_position() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let (_second_writer, second) = stream_generation(&current, 2, true);
    let target = Duration::from_millis(300);
    harness.media.on_intake(
        vec![
            second,
            intent(&current, PageIntent::FollowPageSeek(page_time(target))),
        ],
        None,
    );
    let calls = harness.drive_until_loaded();
    assert!(
        !calls.iter().any(|call| matches!(call, PlayerCall::Seek(_))),
        "the start position must not be a separate seek: {calls:?}"
    );
    let PreparedInitialPosition::PositionedAt {
        target_position,
        result,
        landing_policy,
    } = initial_position_of(&harness, 1)
    else {
        panic!("the generation was loaded from its beginning");
    };
    assert_eq!(target_position.as_duration(), target);
    // Полка садится на фрагмент, содержащий цель (0,2 с), плеер декодирует до цели.
    assert_eq!(result.actual_position.as_duration(), SECOND_VIDEO_FRAGMENT);
    assert_eq!(
        landing_policy,
        PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget
    );
}

/// Две перемотки страницы, пока поколение открывается: стартует последняя.
#[test]
fn the_last_page_seek_while_a_generation_opens_wins() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(
        vec![
            second,
            intent(
                &current,
                PageIntent::FollowPageSeek(page_time(Duration::from_millis(100))),
            ),
            intent(
                &current,
                PageIntent::FollowPageSeek(page_time(Duration::from_millis(250))),
            ),
        ],
        None,
    );
    harness.drive_until_loaded();
    assert_eq!(
        start_target(initial_position_of(&harness, 1)),
        Some(Duration::from_millis(250))
    );
}

/// Перемотка страницы к поколению 2 не достаётся поколению 3, сменившему его до
/// открытия: у нового `MediaSource` — своё время (страница назовёт его заново).
#[test]
fn a_superseding_generation_does_not_inherit_the_page_seek_of_the_one_it_replaced() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let (_second_writer, second) = stream_generation(&current, 2, false);
    let (_third_writer, third) = stream_generation(&current, 3, true);
    harness.media.on_intake(
        vec![
            second,
            intent(
                &current,
                PageIntent::FollowPageSeek(page_time(Duration::from_millis(300))),
            ),
            third,
        ],
        None,
    );
    harness.drive_until_loaded();
    assert_eq!(
        initial_position_of(&harness, 1),
        PreparedInitialPosition::Beginning
    );
}

/// Страница позицию не называла (реклама, новая серия), а байты лежат не с нуля:
/// старт — там, где есть и звук, и видео, а не ожидание в пустоте с нуля.
#[test]
fn without_a_page_seek_a_generation_starts_where_all_lanes_have_data() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let (_second_writer, second) = generation_with_late_video(&current, 2);
    harness.media.on_intake(vec![second], None);
    harness.drive_until_loaded();
    assert_eq!(
        start_target(initial_position_of(&harness, 1)),
        Some(SECOND_VIDEO_FRAGMENT)
    );
}

/// Реклама и новая серия с данными с нуля начинаются с начала — без перемотки.
#[test]
fn a_generation_with_data_from_zero_starts_from_its_beginning() {
    let (harness, _current, _first_writer) = loaded_harness();
    assert_eq!(
        initial_position_of(&harness, 0),
        PreparedInitialPosition::Beginning
    );
}

/// Пауза и перемотка страницы, пока поколение открывается: загрузка — сразу на
/// паузе и сразу на позиции страницы.
#[test]
fn a_page_paused_while_a_generation_opens_loads_it_paused_at_the_page_position() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let (_second_writer, second) = stream_generation(&current, 2, true);
    harness.media.on_intake(
        vec![
            second,
            intent(&current, PageIntent::Pause),
            intent(
                &current,
                PageIntent::FollowPageSeek(page_time(Duration::from_millis(200))),
            ),
        ],
        None,
    );
    let calls = harness.drive_until_loaded();
    assert!(
        calls.contains(&PlayerCall::Load(
            BROWSER_STREAM_LABEL.to_owned(),
            LoadStart::Paused
        )),
        "{calls:?}"
    );
    assert_eq!(
        start_target(initial_position_of(&harness, 1)),
        Some(Duration::from_millis(200))
    );
}

/// Перемотка страницы после загрузки поколения — обычная перемотка плеера: стартовая
/// позиция уже отдана, а более позднее «я на T» должно двинуть показываемое видео.
#[test]
fn a_page_seek_after_the_load_moves_the_player_instead_of_the_start() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let target = Duration::from_millis(300);
    harness.media.on_intake(
        vec![intent(
            &current,
            PageIntent::FollowPageSeek(page_time(target)),
        )],
        None,
    );
    harness.media.drive(Instant::now());
    let calls = harness.player.take_calls();
    assert!(calls.contains(&PlayerCall::Seek(target)), "{calls:?}");
    assert_eq!(
        initial_position_of(&harness, 0),
        PreparedInitialPosition::Beginning,
        "the loaded generation is not re-positioned"
    );
}

/// Живой эфир не ставится на позицию страницы: вход в эфир решает плеер (live join).
#[test]
fn a_live_generation_starts_at_the_live_join_not_at_the_page_position() {
    let (mut harness, current, _first_writer) = loaded_harness();
    let (second_writer, second) = stream_generation(&current, 2, true);
    second_writer.set_duration(PushedDuration::Live);
    harness.media.on_intake(
        vec![
            second,
            intent(
                &current,
                PageIntent::FollowPageSeek(page_time(Duration::from_millis(300))),
            ),
        ],
        None,
    );
    harness.drive_until_loaded();
    assert_eq!(
        initial_position_of(&harness, 1),
        PreparedInitialPosition::Beginning
    );
}

/// Страница ещё не сказала длительность: поток неперематываемый, и ядро не поставит
/// его «сразу на позицию» (это был бы отказ плеера). Поколение ставится с начала —
/// известное ограничение 03b, а не отказ.
#[test]
fn a_generation_without_a_known_duration_is_not_positioned() {
    let (mut harness, current, _first_writer) = loaded_harness();
    // Байты есть, а `duration_changed` страница ещё не прислала.
    let (mut second_writer, second) = stream_generation(&current, 2, false);
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        second_writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        second_writer
            .append(lane, &fixture_media(file))
            .expect("append");
    }
    harness.media.on_intake(
        vec![
            second,
            intent(
                &current,
                PageIntent::FollowPageSeek(page_time(Duration::from_millis(300))),
            ),
        ],
        None,
    );
    harness.drive_until_loaded();
    assert_eq!(
        initial_position_of(&harness, 1),
        PreparedInitialPosition::Beginning
    );
}

//! Устаревшие позиции плеера после перемотки (модуль `stream_buffer::position`):
//! пока полоса ждёт цель — без срока; после посадки — до позиции у цели, но не
//! дольше окна успокоения ОТ МОМЕНТА ПОСАДКИ (пакет F 04d, стенд 04e: медленная
//! страница садила перемотку позже окна, считанного от самой перемотки, и
//! «буферизовано» снова мерилось от старого места).
//!
//! Фикстура `video-h264-64.mp4`: init + 4 фрагмента по 200 мс.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::StreamBuffer;
use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::framing::MediaTime;
use crate::intake::IntakeLedger;
use crate::lane::{LaneId, LaneIntake, LaneMailbox};
use crate::policy::PushedSourcePolicy;
use crate::stream::pushed_demux_registry;
use crate::test_fixtures::{AUDIO_AAC, SplitMp4, VIDEO_H264_64, split_mp4};

const LANE: LaneId = LaneId::new(0);

/// Насколько «медленная страница» садит перемотку позже самой перемотки.
const PAGE_DELAY: Duration = Duration::from_millis(60);

/// Позиция старого места, которую плеер сообщает до своего коммита перемотки.
const OLD_PLACE: Duration = Duration::from_secs(5);

fn ms(value: u64) -> Duration {
    Duration::from_millis(value)
}

/// Буфер одной полосы fMP4 с принятым init и первым фрагментом (0–200 мс).
struct OneLane {
    buffer: Arc<StreamBuffer>,
    intake: LaneIntake,
    media: SplitMp4,
    settle_window: Duration,
}

impl OneLane {
    fn new() -> Self {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let factory =
            EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy);
        let rules = policy.stream_buffer_rules();
        let buffer = Arc::new(StreamBuffer::new(
            Arc::new(IntakeLedger::new(1 << 20)),
            factory,
            rules,
        ));
        let mailbox = Arc::new(LaneMailbox::default());
        buffer.add_lane(
            LANE,
            PushedContainer::FragmentedMp4,
            Arc::clone(&mailbox),
            4,
        );
        let mut intake = LaneIntake::new(
            LANE,
            PushedContainer::FragmentedMp4,
            policy.framing_limits(),
            mailbox,
            Arc::clone(&buffer),
        );
        let media = split_mp4(VIDEO_H264_64);
        intake.append(&media.init).expect("init");
        intake.append(&media.fragments[0]).expect("fragment 0");
        Self {
            buffer,
            intake,
            media,
            settle_window: rules.seek_settle_window,
        }
    }

    fn reported(&self) -> Option<MediaTime> {
        self.buffer.lock().reported_position
    }

    fn report_at(&self, position: Duration, now: Instant) {
        self.buffer.note_playback_position_at(position, now);
    }
}

/// Окно успокоения длиннее окна «устаревшая позиция приходит за миллисекунды»:
/// тест ниже опирается на то, что 2 с после посадки — ещё внутри окна.
#[test]
fn the_settle_window_covers_a_slow_commit_after_landing() {
    assert!(OneLane::new().settle_window > Duration::from_secs(2));
}

/// Медленная страница: цель пришла через долгое время после перемотки. Сразу после
/// посадки (и через 2 с — дольше прежнего окна от перемотки) старая позиция всё ещё
/// отвергается, запас считается от цели; позиция у цели снимает недоверие.
#[test]
fn a_slow_page_landing_late_still_rejects_the_old_place_right_after_landing() {
    let mut lane = OneLane::new();
    let seek_at = Instant::now();
    drop(lane.buffer.seek(ms(600)).expect("seek off the shelf").stale);

    // Страница несёт цель «долго»: пока полоса ждёт, старое место не принимается
    // даже через 12 с после перемотки.
    lane.report_at(OLD_PLACE, seek_at + Duration::from_secs(12));
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(600_000)));

    // Страница медленная по-настоящему: посадка заметно позже перемотки (иначе окна
    // «от перемотки» и «от посадки» неразличимы).
    thread::sleep(PAGE_DELAY);
    // Цель пришла — полоса села (момент посадки — сейчас).
    lane.intake
        .append(&lane.media.fragments[3])
        .expect("fragment with the target");
    let landed_at = Instant::now();

    // Плеер ещё декодирует до цели и сообщает старое место: отвергается сразу после
    // посадки и через 2 с после неё, запас считается от цели (полка 600–800 мс).
    lane.report_at(OLD_PLACE, landed_at + ms(10));
    lane.report_at(OLD_PLACE, landed_at + Duration::from_secs(2));
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(600_000)));
    assert_eq!(lane.buffer.buffer_status().buffered_ahead, ms(200));
    // Окно считается от посадки: на его краю (уже после «перемотка + окно») старое
    // место всё ещё не принимается.
    let window_edge = landed_at + lane.settle_window - PAGE_DELAY / 2;
    assert!(window_edge > seek_at + lane.settle_window);
    lane.report_at(OLD_PLACE, window_edge);
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(600_000)));

    // Плеер сел и сообщил цель — недоверие снято: дальше любая позиция принимается.
    lane.report_at(ms(650), window_edge);
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(650_000)));
    lane.report_at(OLD_PLACE, window_edge);
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(5_000_000)));
}

/// Плеер так и не сообщил цель (сдался и остался на старом месте): после окна
/// успокоения от посадки старая позиция снова настоящая — буфер не замерзает.
#[test]
fn the_old_place_is_trusted_again_once_the_settle_window_after_landing_passes() {
    let mut lane = OneLane::new();
    drop(lane.buffer.seek(ms(600)).expect("seek off the shelf").stale);
    lane.intake
        .append(&lane.media.fragments[3])
        .expect("fragment with the target");
    let landed_at = Instant::now();

    lane.report_at(OLD_PLACE, landed_at + lane.settle_window - ms(1));
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(600_000)));
    lane.report_at(OLD_PLACE, landed_at + lane.settle_window + ms(1));
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(5_000_000)));
}

/// Перемотка по полке садится сразу: окно считается от неё же, а позиция у цели
/// снимает недоверие без ожидания окна.
#[test]
fn an_on_shelf_seek_lands_at_once_and_the_target_report_opens_the_gate() {
    let mut lane = OneLane::new();
    lane.intake
        .append(&lane.media.fragments[1])
        .expect("fragment 1");
    let seek_at = Instant::now();
    drop(lane.buffer.seek(ms(100)).expect("seek on the shelf").stale);

    lane.report_at(OLD_PLACE, seek_at + Duration::from_secs(2));
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(100_000)));
    lane.report_at(ms(120), seek_at + Duration::from_secs(2));
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(120_000)));
}

/// Две полосы: видео держит цель на полке и садится ещё ВНУТРИ перемотки, звук ждёт
/// страницу. Посадка всей перемотки — только когда сел и звук: миг «все полосы
/// сели», пока звук ещё не был переставлен, окно не открывает.
#[test]
fn the_landing_moment_is_when_the_last_lane_lands_not_when_the_first_one_does() {
    const AUDIO: LaneId = LaneId::new(1);
    let mut lane = OneLane::new();
    lane.intake
        .append(&lane.media.fragments[1])
        .expect("video 200–400 ms");
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let audio_mailbox = Arc::new(LaneMailbox::default());
    lane.buffer.add_lane(
        AUDIO,
        PushedContainer::FragmentedMp4,
        Arc::clone(&audio_mailbox),
        4,
    );
    let mut audio_intake = LaneIntake::new(
        AUDIO,
        PushedContainer::FragmentedMp4,
        policy.framing_limits(),
        audio_mailbox,
        Arc::clone(&lane.buffer),
    );
    let audio = split_mp4(AUDIO_AAC);
    audio_intake.append(&audio.init).expect("audio init");
    audio_intake
        .append(&audio.fragments[0])
        .expect("audio ~0–213 ms");

    let seek_at = Instant::now();
    drop(
        lane.buffer
            .seek(ms(300))
            .expect("seek: video on the shelf, audio off")
            .stale,
    );
    thread::sleep(PAGE_DELAY);
    audio_intake
        .append(&audio.fragments[1])
        .expect("audio with the target");
    let landed_at = Instant::now();

    let window_edge = landed_at + lane.settle_window - PAGE_DELAY / 2;
    assert!(window_edge > seek_at + lane.settle_window);
    lane.report_at(OLD_PLACE, window_edge);
    assert_eq!(lane.reported(), Some(MediaTime::from_micros(300_000)));
}

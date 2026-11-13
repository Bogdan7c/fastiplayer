//! Что медиа-часть говорит плееру окна о скачанном (живой скраб «по полке»): только
//! поколение, которое плеер показывает и которое уже приведено к странице.

use super::scrub_range::{SegmentsDownloadedRange, SegmentsTimeline, ShownTimeline};
use super::*;
use crate::browser_session::playback::live_window::{LiveWindowObservation, ObservedSeekRange};

/// Объявленная длина потока: данных только ролик фикстуры (400 мс), конца нет.
const DECLARED_DURATION: Duration = Duration::from_secs(10);

/// Поколение с роликом фикстуры (0–400 мс) без конца потока: дальше — «у страницы».
fn partially_delivered(session: &SessionRef, number: u32) -> (PushedStreamWriter, IntakeNotice) {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, opener) = pushed_stream(policy).expect("stream starts");
    for (lane, file) in [(0, "video.mp4"), (1, "audio.mp4")] {
        let lane = LaneId::new(lane);
        writer
            .add_lane(lane, PushedContainer::FragmentedMp4)
            .expect("lane");
        writer.append(lane, &fixture_media(file)).expect("append");
    }
    writer.set_duration(PushedDuration::Finite(DECLARED_DURATION));
    let notice = IntakeNotice::NewGeneration(Box::new(GenerationHandoff {
        session: session.clone(),
        generation: generation(number),
        source: GenerationSource::Segments {
            monitor: writer.monitor(),
            opener,
        },
    }));
    (writer, notice)
}

#[test]
fn only_the_shown_generation_offers_its_shelf_and_an_opening_one_hides_it() {
    let mut harness = Harness::new();
    let current = session(1);
    harness.media.on_session_opened(&current);
    // Медиа нет — о скачанном ничего не известно.
    harness.media.drive(Instant::now());
    assert!(!harness.player.offered_range().is_known());

    let (_first_writer, first) = partially_delivered(&current, 1);
    harness.media.on_intake(vec![first], None);
    harness.drive_until_loaded();
    // Загружено, но плеер ещё показывает прежнее (установка асинхронна): скраб достался
    // бы не этому медиа.
    harness.media.drive(Instant::now());
    assert!(!harness.player.offered_range().is_known());

    let ticket = harness.player.load_ticket(0);
    harness
        .player
        .show_load(ticket, Duration::ZERO, PlaybackState::Playing);
    harness.media.drive(Instant::now());
    let shown = harness.player.offered_range();
    assert!(shown.covers(Duration::from_millis(100)), "{shown:?}");
    assert!(!shown.covers(Duration::from_secs(5)), "{shown:?}");

    // Открывается новое поколение: оно заменит медиа в плеере — скачанное неизвестно.
    let (_second_writer, second) = partially_delivered(&current, 2);
    harness.media.on_intake(vec![second], None);
    harness.media.drive(Instant::now());
    assert!(!harness.player.offered_range().is_known());
}

#[test]
fn a_live_generation_offers_no_downloaded_range() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream starts");
    let lane = LaneId::new(0);
    writer
        .add_lane(lane, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer
        .append(lane, &fixture_media("video.mp4"))
        .expect("append");
    let monitor = writer.monitor();

    // Та же полка: у записи скачанное известно, у эфира — нет (две шкалы не смешиваем).
    let on_demand = SegmentsDownloadedRange::new(&monitor, SegmentsTimeline::OnDemand);
    assert!(
        on_demand
            .while_showing(ShownTimeline::Recording)
            .covers(Duration::from_millis(100))
    );
    let live = SegmentsDownloadedRange::new(&monitor, SegmentsTimeline::Live);
    assert!(!live.while_showing(ShownTimeline::Live).is_known());
}

/// Эфир, ставший записью (страница объявила длину и `endOfStream`, 16d1): шкала плеера —
/// время источника, и скачанное снова известно — живой скраб по полке и досмотр без
/// страницы перематывают по нему. Раньше ответ застывал на «неизвестно» с открытия.
#[test]
fn a_live_generation_that_became_a_recording_offers_its_shelf() {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream starts");
    let lane = LaneId::new(0);
    writer
        .add_lane(lane, PushedContainer::FragmentedMp4)
        .expect("lane");
    writer
        .append(lane, &fixture_media("video.mp4"))
        .expect("append");
    let opened_live = SegmentsDownloadedRange::new(&writer.monitor(), SegmentsTimeline::Live);

    let after_the_end = opened_live.while_showing(ShownTimeline::Recording);
    assert!(
        after_the_end.covers(Duration::from_millis(100)),
        "{after_the_end:?}"
    );
    assert!(
        !after_the_end.covers(Duration::from_secs(5)),
        "{after_the_end:?}"
    );
}

/// Записью шкала считается только по явной записи с длиной. «Окна нет» у поколения,
/// открытого эфиром, — это чужое медиа на экране (или ничего): его полку с целью на другой
/// шкале не сравниваем. Иначе досмотр пустил бы перемотку эфира внутрь «скачанного» по
/// чужим секундам.
#[test]
fn only_an_explicit_recording_counts_as_a_recording_timeline() {
    let recording = LiveWindowObservation::Recording {
        end: Duration::from_secs(6),
        seekable: Some(ObservedSeekRange {
            start: Duration::ZERO,
            end: Duration::from_secs(6),
        }),
    };
    assert_eq!(
        ShownTimeline::of_window(recording),
        ShownTimeline::Recording
    );
    assert_eq!(
        ShownTimeline::of_window(LiveWindowObservation::NotLive),
        ShownTimeline::Live
    );
    assert_eq!(
        ShownTimeline::of_window(LiveWindowObservation::LiveWithoutDvr {
            edge: Duration::from_secs(60),
        }),
        ShownTimeline::Live
    );
    assert_eq!(ShownTimeline::of(None), ShownTimeline::Live);
}

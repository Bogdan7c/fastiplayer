//! Что медиа-часть говорит плееру окна о скачанном (живой скраб «по полке»): только
//! поколение, которое плеер показывает и которое уже приведено к странице.

use super::scrub_range::{SegmentsTimeline, segments_downloaded_range};
use super::*;

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
    let on_demand = segments_downloaded_range(&monitor, SegmentsTimeline::OnDemand);
    assert!(on_demand.covers(Duration::from_millis(100)));
    let live = segments_downloaded_range(&monitor, SegmentsTimeline::Live);
    assert!(!live.is_known());
}

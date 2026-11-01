//! Чистые правила ворот поколения (04d, путь C) на настоящей полке `pushed-media-source`.
//!
//! Фикстура протокола `video.mp4`: init (733 байта) и два фрагмента — с 0 и с 0,2 с;
//! `audio.mp4` — звук 0,4 с с нуля. Длительность потока — 10 с, конца потока нет: цель
//! после 0,4 с полка не держит (страницу попросили бы перемотаться).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use pushed_media_source::{
    LaneId, PushedContainer, PushedDuration, PushedSourcePolicy, PushedStreamWriter, pushed_stream,
};

use super::*;

/// Срок ожидания данных у цели в тестах (короткий, чтобы считать от `now`).
const DATA_WAIT: Duration = Duration::from_secs(10);
/// Цель на полке (во втором фрагменте видео и внутри звука).
const ON_SHELF: Duration = Duration::from_millis(300);
/// Цель вне полки (данные только до 0,4 с).
const OFF_SHELF: Duration = Duration::from_secs(5);

fn fixture_media(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../browser-bridge-protocol/fixtures/media")
        .join(name);
    std::fs::read(path).expect("fixture media")
}

/// Поток с полосами видео (0) и звука (1); `audio` — класть ли звук.
fn stream_with_data(audio: bool) -> PushedStreamWriter {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let (mut writer, _opener) = pushed_stream(policy).expect("stream");
    writer.set_duration(PushedDuration::Finite(Duration::from_secs(10)));
    for lane in [0, 1] {
        writer
            .add_lane(LaneId::new(lane), PushedContainer::FragmentedMp4)
            .expect("lane");
    }
    writer
        .append(LaneId::new(0), &fixture_media("video.mp4"))
        .expect("video");
    if audio {
        writer
            .append(LaneId::new(1), &fixture_media("audio.mp4"))
            .expect("audio");
    } else {
        // Только init звука: полоса объявлена, но данных у неё нет нигде.
        let audio_file = fixture_media("audio.mp4");
        writer
            .append(LaneId::new(1), &audio_file[..init_end(&audio_file)])
            .expect("audio init");
    }
    writer
}

/// Конец init-сегмента fMP4 (`ftyp` + `moov`): верхние коробки до первой `moof`.
fn init_end(file: &[u8]) -> usize {
    let mut offset = 0;
    while offset + 8 <= file.len() {
        let size = u32::from_be_bytes(file[offset..offset + 4].try_into().expect("size")) as usize;
        if &file[offset + 4..offset + 8] == b"moof" {
            return offset;
        }
        offset += size;
    }
    file.len()
}

fn placement() -> GenerationPlacement {
    GenerationPlacement::first_common_data(GenerationPlacementPolicy::new(DATA_WAIT))
}

/// Тот же допуск, что у источника (для `aim_for_page`).
fn same_by(monitor: &PushedStreamMonitor) -> impl Fn(Duration, Duration) -> bool + '_ {
    |current, page| monitor.is_same_seek_target(current, page)
}

#[test]
fn without_a_target_the_generation_is_handed_over_where_all_lanes_have_data() {
    let writer = stream_with_data(true);
    let mut placement = placement();
    assert_eq!(placement.awaited_target(), None);
    assert_eq!(
        placement.decide(
            &writer.monitor(),
            Instant::now(),
            PageAvailability::Listening
        ),
        PlacementVerdict::HandOver(GenerationStart::FirstCommonData)
    );
}

#[test]
fn a_target_on_the_shelf_is_handed_over_at_once() {
    let writer = stream_with_data(true);
    let mut placement = placement();
    placement.aim_for_viewer(ON_SHELF);
    assert_eq!(
        placement.decide(
            &writer.monitor(),
            Instant::now(),
            PageAvailability::Listening
        ),
        PlacementVerdict::HandOver(GenerationStart::PageSeekedTo(ON_SHELF))
    );
}

/// Главное правило: без данных у цели поколение плееру не отдаётся, пока не вышел срок;
/// перепроверка — не позже срока.
#[test]
fn a_target_without_data_waits_until_the_deadline() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    assert_eq!(placement.awaited_target(), Some(OFF_SHELF));

    let just_before = start + DATA_WAIT - Duration::from_millis(1);
    let PlacementVerdict::Wait { recheck_at } =
        placement.decide(&monitor, just_before, PageAvailability::Listening)
    else {
        panic!("a target without data must wait");
    };
    assert!(
        recheck_at <= start + DATA_WAIT,
        "the recheck never misses the deadline"
    );
    assert!(recheck_at > just_before);
}

/// Срок вышел, у цели пусто — туда, где у всех полос есть данные (правило 03e).
#[test]
fn after_the_deadline_the_generation_starts_where_data_exists() {
    let writer = stream_with_data(true);
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    assert_eq!(
        placement.decide(
            &writer.monitor(),
            start + DATA_WAIT,
            PageAvailability::Listening
        ),
        PlacementVerdict::HandOver(GenerationStart::FirstCommonData)
    );
}

/// Срок вышел, а страница стоит там, где данные есть, — старт у страницы.
#[test]
fn after_the_deadline_the_page_position_with_data_wins_over_the_common_start() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    assert_eq!(
        placement.aim_for_page(ON_SHELF, same_by(&monitor)),
        PageAim::Retargeted
    );
    // Зритель кликнул дальше, а страница туда не пошла.
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    assert_eq!(
        placement.decide(&monitor, start + DATA_WAIT, PageAvailability::Listening),
        PlacementVerdict::HandOver(GenerationStart::PageSeekedTo(ON_SHELF))
    );
}

/// Страница ушла — байтов у цели не будет: ждать срок незачем.
#[test]
fn a_gone_page_does_not_make_the_generation_wait() {
    let writer = stream_with_data(true);
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    assert_eq!(
        placement.decide(&writer.monitor(), start, PageAvailability::Gone),
        PlacementVerdict::HandOver(GenerationStart::FirstCommonData)
    );
}

/// Перемотка страницы, совпавшая с целью зрителя (эхо нашей просьбы, допуск источника),
/// — не новая цель: цель зрителя остаётся, срок не продлевается, и повторяющая себя
/// страница не держит ожидание вечно.
#[test]
fn a_page_seek_matching_the_viewer_target_neither_retargets_nor_extends_the_wait() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);

    let echo = OFF_SHELF + Duration::from_millis(100);
    assert_eq!(
        placement.aim_for_page(echo, same_by(&monitor)),
        PageAim::EchoOfViewerTarget
    );
    placement.arm(start + Duration::from_secs(5));
    assert_eq!(
        placement.awaited_target(),
        Some(OFF_SHELF),
        "the viewer target is kept"
    );
    // Срок считается от клика, а не от эха страницы.
    assert!(matches!(
        placement.decide(&monitor, start + DATA_WAIT, PageAvailability::Listening),
        PlacementVerdict::HandOver(_)
    ));
}

/// Две близкие перемотки самой страницы — одна перемотка: побеждает последняя позиция
/// (правило 03e), но срок не продлевается.
#[test]
fn a_repeated_page_seek_refines_its_target_without_extending_the_wait() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    placement.aim_for_page(OFF_SHELF, same_by(&monitor));
    let start = Instant::now();
    placement.arm(start);

    let refined = OFF_SHELF + Duration::from_millis(150);
    assert_eq!(
        placement.aim_for_page(refined, same_by(&monitor)),
        PageAim::RefinedPageTarget
    );
    placement.arm(start + Duration::from_secs(5));
    assert_eq!(placement.awaited_target(), Some(refined));
    assert!(matches!(
        placement.decide(&monitor, start + DATA_WAIT, PageAvailability::Listening),
        PlacementVerdict::HandOver(_)
    ));
}

/// Страница перемоталась в другое место — её цель побеждает, срок заново.
#[test]
fn a_page_seek_elsewhere_retargets_and_restarts_the_wait() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);

    let elsewhere = Duration::from_secs(7);
    assert_eq!(
        placement.aim_for_page(elsewhere, same_by(&monitor)),
        PageAim::Retargeted
    );
    let retargeted_at = start + Duration::from_secs(4);
    placement.arm(retargeted_at);
    assert_eq!(placement.awaited_target(), Some(elsewhere));
    assert!(matches!(
        placement.decide(&monitor, start + DATA_WAIT, PageAvailability::Listening),
        PlacementVerdict::Wait { .. }
    ));
    assert!(matches!(
        placement.decide(
            &monitor,
            retargeted_at + DATA_WAIT,
            PageAvailability::Listening
        ),
        PlacementVerdict::HandOver(GenerationStart::FirstCommonData)
    ));
}

/// Новый клик зрителя — новая перемотка: срок заново (как у плеера).
#[test]
fn every_viewer_click_restarts_the_wait() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    let second_click = start + Duration::from_secs(9);
    placement.aim_for_viewer(Duration::from_secs(6));
    placement.arm(second_click);
    assert!(matches!(
        placement.decide(&monitor, start + DATA_WAIT, PageAvailability::Listening),
        PlacementVerdict::Wait { .. }
    ));
}

/// Данных нет нигде, откуда можно стартовать (звук пуст): поставить поколение без
/// данных — тот же отказ плеера, поэтому ворота ждут дальше и после срока.
#[test]
fn with_no_data_anywhere_the_generation_keeps_waiting_after_the_deadline() {
    let writer = stream_with_data(false);
    let mut placement = placement();
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    let late = start + DATA_WAIT * 3;
    let PlacementVerdict::Wait { recheck_at } =
        placement.decide(&writer.monitor(), late, PageAvailability::Listening)
    else {
        panic!("nowhere to start: the generation must keep waiting");
    };
    assert!(recheck_at > late);
}

/// Файл, эфир и поток без длительности ворот не имеют: их старт — названная цель.
#[test]
fn the_named_start_is_the_last_target() {
    let writer = stream_with_data(true);
    let monitor = writer.monitor();
    let mut placement = placement();
    assert_eq!(placement.named_start(), GenerationStart::FirstCommonData);
    placement.aim_for_viewer(OFF_SHELF);
    placement.aim_for_page(Duration::from_secs(8), same_by(&monitor));
    assert_eq!(
        placement.named_start(),
        GenerationStart::PageSeekedTo(Duration::from_secs(8))
    );
}

/// Ожидание данных по умолчанию — срок коммита перемотки из умолчаний config-а.
#[test]
fn the_default_wait_is_the_seek_commit_timeout_of_the_config() {
    assert_eq!(
        GenerationPlacementPolicy::recommended(),
        GenerationPlacementPolicy::new(Duration::from_millis(
            PlayerSeekConfig::default().commit_timeout_ms
        ))
    );
}

/// Часы странице, пока поколение ждёт: «жду данных» в самой цели.
#[test]
fn the_page_clock_waits_at_the_target_while_the_generation_waits() {
    let playing_old_video = PlaybackObservation {
        position: Duration::from_secs(80),
        duration: Some(Duration::from_secs(600)),
        state: PlaybackState::Ended,
        rate: player_core::PlaybackRate::NORMAL,
        buffering: false,
        ended: true,
        fault: None,
        shown_load: None,
        seek_target: None,
    };
    let clock = clock_while_generation_awaits(&playing_old_video, OFF_SHELF);
    assert_eq!(clock.position, OFF_SHELF);
    assert_eq!(clock.state, PlaybackState::Seeking);
    assert!(
        !clock.ended,
        "the old generation's end is not the page's end"
    );
}

/// Срок из config-а может быть огромным (схема допускает до `i64::MAX` мс): переполнение
/// времени — «срока нет», а не паника; поколение ждёт данных у цели.
#[test]
fn a_huge_configured_wait_never_overflows_and_keeps_waiting() {
    let writer = stream_with_data(true);
    let huge = Duration::from_millis(u64::try_from(i64::MAX).expect("fits"));
    let mut placement =
        GenerationPlacement::first_common_data(GenerationPlacementPolicy::new(huge));
    placement.aim_for_viewer(OFF_SHELF);
    let start = Instant::now();
    placement.arm(start);
    let PlacementVerdict::Wait { recheck_at } = placement.decide(
        &writer.monitor(),
        start + Duration::from_secs(3_600),
        PageAvailability::Listening,
    ) else {
        panic!("a huge wait must keep waiting");
    };
    assert!(recheck_at > start + Duration::from_secs(3_600));
}

//! Тесты приёмной стороны полосы: когда начинается новый эпизод, а когда нет.

use std::sync::Arc;

use super::{LaneId, LaneIntake, LaneMailbox, MailboxTake};
use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::intake::IntakeLedger;
use crate::policy::PushedSourcePolicy;
use crate::stream::pushed_demux_registry;
use crate::stream_buffer::StreamBuffer;
use crate::test_fixtures::{VIDEO_H264_64, VIDEO_H264_96, split_mp4};
use crate::time::TimestampOffset;

const VIDEO_MP4: &[u8] = include_bytes!("../../browser-bridge-protocol/fixtures/media/video.mp4");
const INIT_END: usize = 733;
const FIRST_FRAGMENT_END: usize = 5714;
const LANE: LaneId = LaneId::new(0);

struct Fixture {
    intake: LaneIntake,
    mailbox: Arc<LaneMailbox>,
}

fn fixture() -> Fixture {
    let policy = PushedSourcePolicy::recommended().expect("policy");
    let mailbox = Arc::new(LaneMailbox::default());
    let buffer = Arc::new(StreamBuffer::new(
        Arc::new(IntakeLedger::new(1 << 20)),
        EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy),
        policy.stream_buffer_rules(),
    ));
    buffer.add_lane(
        LANE,
        PushedContainer::FragmentedMp4,
        Arc::clone(&mailbox),
        policy.max_pending_episodes_per_lane(),
    );
    let intake = LaneIntake::new(
        LANE,
        PushedContainer::FragmentedMp4,
        policy.framing_limits(),
        Arc::clone(&mailbox),
        buffer,
    );
    Fixture { intake, mailbox }
}

/// Сколько эпизодов лежит в ящике.
fn posted_episodes(mailbox: &LaneMailbox) -> usize {
    let mut count = 0;
    while let MailboxTake::Episode(_) = mailbox.take_next() {
        count += 1;
    }
    count
}

#[test]
fn identical_repeated_init_keeps_the_running_episode() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END])
        .expect("first");
    // Плеер страницы повторяет тот же init перед следующим фрагментом.
    lane.intake
        .append(&VIDEO_MP4[..INIT_END])
        .expect("same init");
    lane.intake
        .append(&VIDEO_MP4[FIRST_FRAGMENT_END..])
        .expect("second");
    assert_eq!(posted_episodes(&lane.mailbox), 1);
}

#[test]
fn offset_change_and_change_type_start_new_episodes_at_the_next_media_segment() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END])
        .expect("first");
    // Тот же первый фрагмент (0–200 мс) со смещением +200 мс продолжает курсор.
    lane.intake
        .set_timestamp_offset(TimestampOffset::from_micros(200_000))
        .expect("offset");
    // Тот же offset ещё раз — не повод для нового эпизода.
    lane.intake
        .set_timestamp_offset(TimestampOffset::from_micros(200_000))
        .expect("same offset");
    lane.intake
        .append(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END])
        .expect("second");
    assert_eq!(posted_episodes(&lane.mailbox), 2);

    // После changeType медиа без нового init — ошибка, с init — новый эпизод.
    lane.intake
        .change_container(PushedContainer::FragmentedMp4)
        .expect("change type");
    assert!(
        lane.intake
            .append(&VIDEO_MP4[INIT_END..FIRST_FRAGMENT_END])
            .is_err()
    );
}

#[test]
fn abort_keeps_the_episode_and_counts_only_partial_bytes() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END + 50])
        .expect("first");
    assert_eq!(lane.intake.partial_segment_bytes(), 50);
    lane.intake.abort();
    assert_eq!(lane.intake.partial_segment_bytes(), 0);
    lane.intake
        .append(&VIDEO_MP4[FIRST_FRAGMENT_END..])
        .expect("second");
    assert_eq!(posted_episodes(&lane.mailbox), 1);
}

#[test]
fn closed_consumer_discards_new_episodes_and_waiting_ones() {
    let mut lane = fixture();
    lane.intake
        .append(&VIDEO_MP4[..FIRST_FRAGMENT_END])
        .expect("first");
    lane.mailbox.close_consumer();
    assert!(matches!(lane.mailbox.take_next(), MailboxTake::Waiting));
    assert_eq!(
        lane.intake.append(&VIDEO_MP4[FIRST_FRAGMENT_END..]),
        Ok(super::AppendOutcome::DiscardedConsumerClosed)
    );
    lane.intake.end_input().expect("end");
    assert!(matches!(lane.mailbox.take_next(), MailboxTake::InputEnded));
}

#[test]
fn new_init_at_the_cursor_starts_a_new_episode_but_after_a_gap_waits() {
    let low = split_mp4(VIDEO_H264_64);
    let high = split_mp4(VIDEO_H264_96);
    let mut lane = fixture();
    lane.intake.append(&low.init).expect("init 64");
    lane.intake.append(&low.fragments[0]).expect("0–200 ms");
    // Другое качество с курсора (200 мс): новый эпизод сразу.
    lane.intake.append(&high.init).expect("init 96");
    lane.intake.append(&high.fragments[1]).expect("200–400 ms");
    assert_eq!(posted_episodes(&lane.mailbox), 2);
    // Снова 64, но за дырой (600 мс): сегмент ждёт на полке, эпизода нет.
    lane.intake.append(&low.init).expect("init 64 again");
    lane.intake.append(&low.fragments[3]).expect("600–800 ms");
    assert_eq!(posted_episodes(&lane.mailbox), 0);
    // Дыру закрывает 96-й фрагмент 400–600: он и ждавший 600–800 (другой init)
    // подаются по порядку — два новых эпизода.
    lane.intake.append(&high.init).expect("init 96 again");
    lane.intake.append(&high.fragments[2]).expect("400–600 ms");
    assert_eq!(posted_episodes(&lane.mailbox), 2);
}
